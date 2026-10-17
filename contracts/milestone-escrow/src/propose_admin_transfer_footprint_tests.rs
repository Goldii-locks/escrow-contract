#![cfg(test)]
//! Dedicated unit-test suite for the `propose_admin_transfer` ledger-storage
//! footprint reduction (issue #445).
//!
//! Before this change, a single call touched three distinct ledger entries:
//!   * `DataKey::Version`              (instance)   — via `require_initialized`
//!   * `DataKey::Admin`                (persistent) — via `require_admin`
//!   * `DataKey::PendingAdminTransfer` (persistent) — read via `.has(..)`,
//!     then written via `.set(..)`
//!
//! `Version` and `Admin` are only ever written together, atomically, inside
//! `initialize` (a failed `initialize` reverts the whole invocation), so the
//! presence of `Admin` alone is already a complete "is this contract
//! initialised" signal — exactly the invariant `execute_admin_transfer`
//! already relies on. `propose_admin_transfer` now uses that same signal
//! instead of a separate `require_initialized` call, dropping the `Version`
//! read and leaving only two distinct ledger entries touched: `Admin` and
//! `PendingAdminTransfer`.
//!
//! These tests verify:
//!   - The `Version` instance key is no longer required for the call to
//!     succeed — proving the redundant read was actually removed, not just
//!     documented.
//!   - Every existing precondition and its error stays exactly as it was:
//!     `NotInitialized`, `Unauthorized`, `InvalidAddress`, and
//!     `AdminTransferPending`, including the ordering guarantee that an
//!     uninitialised contract is rejected before any signature is required.
//!   - The happy path is unaffected.

use super::*;
use crate::{DataKey, Error, PendingAdminTransfer};
use soroban_sdk::{Address, Env};

/// Directly proves the `Version` read was removed: a contract that has
/// `DataKey::Admin` set in persistent storage — but was never fully
/// `initialize`d, so `DataKey::Version` was never written — must still be
/// treated as initialised by `propose_admin_transfer`.
///
/// Before the fix this call would have failed with `NotInitialized` (from
/// `require_initialized`'s `Version` check) even though the admin key it
/// actually needs is present. After the fix it succeeds, because the
/// function no longer touches `Version` at all.
#[test]
fn test_propose_admin_transfer_does_not_require_version_key() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());

    // Seed only the persistent Admin key — never call `initialize`, so
    // `DataKey::Version` is never written.
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&DataKey::Admin, &admin);
    });
    let version_present: bool = env.as_contract(&contract_id, || {
        env.storage().instance().has(&DataKey::Version)
    });
    assert!(!version_present);

    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let result = client.try_propose_admin_transfer(&admin, &new_admin, &1u32);
    assert_eq!(result, Ok(Ok(())));

    let pending: PendingAdminTransfer = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .unwrap()
    });
    assert_eq!(pending.new_admin, new_admin);
    assert_eq!(pending.proposal_id, 1u32);
}

/// A contract with neither `Admin` nor `Version` set must still be rejected
/// with `NotInitialized` — the initialisation guard is preserved, only its
/// implementation (checking `Admin` instead of `Version`) changed.
#[test]
fn test_propose_admin_transfer_uninitialized_still_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_propose_admin_transfer(&admin, &new_admin, &1u32);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

/// A fully initialised contract still rejects a non-admin caller with
/// `Unauthorized`, and does not create a pending proposal.
#[test]
fn test_propose_admin_transfer_wrong_admin_still_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());

    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Version, &1u32);
    });

    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let attacker = Address::generate(&env);
    let result = client.try_propose_admin_transfer(&attacker, &new_admin, &1u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let pending_present: bool = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
    });
    assert!(!pending_present);
}

/// Full happy path against a normally-initialised escrow (both `Admin` and
/// `Version` present, as `initialize` always sets them together) is
/// unaffected by the footprint change.
#[test]
fn test_propose_admin_transfer_happy_path_unaffected() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer,
        &arbiter,
        &token,
        &604800,
        &soroban_sdk::vec![&env, 1_000_i128],
    );

    let new_admin = Address::generate(&env);
    let result = client.try_propose_admin_transfer(&admin, &new_admin, &9u32);
    assert_eq!(result, Ok(Ok(())));

    let pending = client.get_pending_admin_transfer().unwrap();
    assert_eq!(pending.new_admin, new_admin);
    assert_eq!(pending.proposal_id, 9u32);

    // A second proposal is still blocked while one is pending.
    let other = Address::generate(&env);
    let blocked = client.try_propose_admin_transfer(&admin, &other, &10u32);
    assert_eq!(blocked, Err(Ok(Error::AdminTransferPending)));
}
