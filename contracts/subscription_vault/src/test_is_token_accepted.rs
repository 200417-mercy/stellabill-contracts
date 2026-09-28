//! Focused tests for `admin::is_token_accepted`.
//!
//! `is_token_accepted` is the contract-internal predicate that guards every
//! multi-token entrypoint (`create_subscription_with_token`,
//! `withdraw_merchant_token_funds`, etc.).  The function reads
//! `DataKey::TokenDecimals(token)` from instance storage and returns `true`
//! iff that key is present — no admin auth is required because it is a pure
//! read.
//!
//! ## Coverage matrix
//!
//! | Scenario | Expected return |
//! |---|---|
//! | Default token registered at `init` | `true` |
//! | Extra token added via `add_accepted_token` | `true` |
//! | Address never registered | `false` |
//! | Contract's own address | `false` |
//! | Token removed via `remove_accepted_token` | `false` |
//! | Random address before contract is initialized | `false` |
//! | State after a *rejected* operation is unchanged | verified |

#![cfg(test)]

use crate::{SubscriptionVault, SubscriptionVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

/// Advance past the admin-config cooldown so consecutive `add_accepted_token`
/// / `remove_accepted_token` calls don't hit `Error::CooldownActive`.
const COOLDOWN_JUMP: u64 = 6 * 60 * 60 + 1; // CONFIG_COOLDOWN_SECS + 1

// ── helpers ──────────────────────────────────────────────────────────────────

/// Minimal initialised environment: one admin, one default token, no real
/// token minting (tests here only exercise the accepted-token registry).
fn setup() -> (Env, SubscriptionVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let default_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    client.init(&default_token, &6, &admin, &1_000_000i128, &(7 * 24 * 60 * 60));

    (env, client, admin, default_token)
}

/// Thin wrapper that calls `is_token_accepted` from inside the contract's
/// execution context so it reads the correct storage.
fn is_accepted(env: &Env, contract_id: &Address, token: &Address) -> bool {
    env.as_contract(contract_id, || crate::admin::is_token_accepted(env, token))
}

// ── success paths ─────────────────────────────────────────────────────────────

/// The default token registered during `init` must immediately be accepted.
#[test]
fn default_token_is_accepted_after_init() {
    let (env, client, _admin, default_token) = setup();
    assert!(
        is_accepted(&env, &client.address, &default_token),
        "default token must be accepted right after init"
    );
}

/// A secondary token added by the admin via `add_accepted_token` must be
/// accepted from that point forward.
#[test]
fn newly_added_token_is_accepted() {
    let (env, client, admin, _default_token) = setup();

    let new_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &new_token, &7u32);

    assert!(
        is_accepted(&env, &client.address, &new_token),
        "token must be accepted immediately after add_accepted_token"
    );
}

/// Multiple extra tokens can be added; all must be accepted independently.
#[test]
fn multiple_added_tokens_are_all_accepted() {
    let (env, client, admin, _default_token) = setup();

    let token_b = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_c = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &token_b, &6u32);

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &token_c, &8u32);

    assert!(
        is_accepted(&env, &client.address, &token_b),
        "token_b must be accepted after add"
    );
    assert!(
        is_accepted(&env, &client.address, &token_c),
        "token_c must be accepted after add"
    );
}

// ── failure / negative paths ──────────────────────────────────────────────────

/// An address that was never registered must not be accepted.
#[test]
fn random_unregistered_address_is_not_accepted() {
    let (env, client, _admin, _default_token) = setup();

    let stranger = Address::generate(&env);

    assert!(
        !is_accepted(&env, &client.address, &stranger),
        "unregistered address must not be accepted"
    );
}

/// The contract's own address must never be accepted (it cannot hold or
/// transfer token balances as a subscriber/merchant).
#[test]
fn contract_self_address_is_not_accepted() {
    let (env, client, _admin, _default_token) = setup();

    assert!(
        !is_accepted(&env, &client.address, &client.address),
        "the contract's own address must never appear as an accepted token"
    );
}

/// A token removed via `remove_accepted_token` must no longer be accepted.
#[test]
fn removed_token_is_no_longer_accepted() {
    let (env, client, admin, _default_token) = setup();

    let extra_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &extra_token, &6u32);

    // Confirm it's accepted before removal.
    assert!(
        is_accepted(&env, &client.address, &extra_token),
        "token must be accepted before removal"
    );

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.remove_accepted_token(&admin, &extra_token);

    assert!(
        !is_accepted(&env, &client.address, &extra_token),
        "token must NOT be accepted after remove_accepted_token"
    );
}

/// Removing one token must not affect the acceptance state of other tokens.
#[test]
fn removing_one_token_does_not_affect_others() {
    let (env, client, admin, default_token) = setup();

    let token_b = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_c = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &token_b, &6u32);

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &token_c, &6u32);

    // Remove only token_b.
    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.remove_accepted_token(&admin, &token_b);

    assert!(
        !is_accepted(&env, &client.address, &token_b),
        "token_b must be removed"
    );
    assert!(
        is_accepted(&env, &client.address, &default_token),
        "default token must still be accepted"
    );
    assert!(
        is_accepted(&env, &client.address, &token_c),
        "token_c must still be accepted"
    );
}

// ── state-unchanged after rejected operations ─────────────────────────────────

/// Attempting `add_accepted_token` with a non-admin caller must fail and must
/// leave the token unregistered (storage state unchanged).
#[test]
fn failed_add_by_non_admin_leaves_state_unchanged() {
    let (env, client, _admin, _default_token) = setup();

    let stranger = Address::generate(&env);
    let new_token = env
        .register_stellar_asset_contract_v2(stranger.clone())
        .address();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    let result = client.try_add_accepted_token(&stranger, &new_token, &6u32);

    assert!(
        result.is_err(),
        "non-admin add_accepted_token must be rejected"
    );
    assert!(
        !is_accepted(&env, &client.address, &new_token),
        "token must NOT be accepted after a rejected add operation"
    );
}

/// Attempting `remove_accepted_token` with a non-admin caller must fail and
/// must leave the token still registered.
#[test]
fn failed_remove_by_non_admin_leaves_state_unchanged() {
    let (env, client, admin, _default_token) = setup();

    let extra_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &extra_token, &6u32);

    let stranger = Address::generate(&env);
    let result = client.try_remove_accepted_token(&stranger, &extra_token);

    assert!(
        result.is_err(),
        "non-admin remove_accepted_token must be rejected"
    );
    assert!(
        is_accepted(&env, &client.address, &extra_token),
        "token must still be accepted after a rejected remove operation"
    );
}

/// The default token cannot be removed (it is the primary settlement token).
/// Attempting to do so must fail, and the token must remain accepted.
#[test]
fn removing_default_token_is_rejected_and_state_unchanged() {
    let (env, client, admin, default_token) = setup();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    let result = client.try_remove_accepted_token(&admin, &default_token);

    assert!(
        result.is_err(),
        "removing the default token must be rejected"
    );
    assert!(
        is_accepted(&env, &client.address, &default_token),
        "default token must still be accepted after rejected removal"
    );
}

// ── boundary: uninitialized contract ─────────────────────────────────────────

/// Before `init` is called, no token is registered, so `is_token_accepted`
/// must return `false` for any address.
#[test]
fn any_token_is_not_accepted_before_init() {
    let env = Env::default();
    env.mock_all_auths();

    // Register the contract but deliberately skip `init`.
    let contract_id = env.register(SubscriptionVault, ());

    let random_token = Address::generate(&env);

    assert!(
        !is_accepted(&env, &contract_id, &random_token),
        "no token must be accepted before init is called"
    );
}

// ── idempotency ───────────────────────────────────────────────────────────────

/// Adding the same token twice (allowed by `add_accepted_token` — the second
/// call is a no-op for the list but overwrites decimals) must leave the token
/// accepted exactly once (no duplicates), and `is_token_accepted` must still
/// return `true`.
#[test]
fn adding_same_token_twice_is_idempotent() {
    let (env, client, admin, _default_token) = setup();

    let extra_token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    env.ledger().with_mut(|l| l.timestamp += COOLDOWN_JUMP);
    client.add_accepted_token(&admin, &extra_token, &6u32);

    // Second add — no cooldown is enforced when the key already exists
    // (add_accepted_token skips the cooldown guard if the token is already
    // in the list).
    client.add_accepted_token(&admin, &extra_token, &8u32);

    assert!(
        is_accepted(&env, &client.address, &extra_token),
        "token must remain accepted after being added a second time"
    );

    // The list must not contain the token more than once.
    let listed = client.list_accepted_tokens();
    let count = listed.iter().filter(|t| t.token == extra_token).count();
    assert_eq!(
        count, 1,
        "token must appear exactly once in list_accepted_tokens even after duplicate add"
    );
}
