//! Tests for `get_pending_admin_transfer`.
//!
//! # Purpose
//!
//! `get_pending_admin_transfer` is a **pure read** endpoint that:
//! * returns `Err(Error::NotInitialized)` when called on a contract that has
//!   not been initialized yet, and
//! * otherwise returns `Ok(Some(…))` when a proposal is pending, or `Ok(None)`
//!   when no proposal exists — without writing to any storage tier.
//!
//! The Soroban SDK test client exposes two variants:
//! * `client.get_pending_admin_transfer()` — panics on contract error, returns
//!   `Option<PendingAdminTransfer>` on success.
//! * `client.try_get_pending_admin_transfer()` — returns
//!   `Result<Result<Option<PendingAdminTransfer>, ConversionError>,
//!           Result<Error, InvokeError>>`;
//!   a contract-level `Err(E)` surfaces as `Err(Ok(E))`.
//!
//! # Test matrix
//!
//! | # | Scenario                                    | try_ form                        | Storage effect     |
//! |---|---------------------------------------------|----------------------------------|--------------------|
//! | 1 | Contract not yet initialized                | `Err(Ok(NotInitialized))`        | zero writes        |
//! | 2 | No proposal has ever been created           | `Ok(Ok(None))`                   | zero writes        |
//! | 3 | After a successful `propose_admin_transfer` | `Ok(Ok(Some(…)))`                | zero writes        |
//! | 4 | After `cancel_admin_transfer_proposal`      | `Ok(Ok(None))`                   | zero writes        |
//! | 5 | After `execute_admin_transfer`              | `Ok(Ok(None))`                   | zero writes        |
//! | 6 | Called multiple times in a row              | same value each time             | zero writes        |
//! | 7 | Snapshot before == snapshot after           | —                                | **byte-identical** |
//! | 8 | Payload returned verbatim                   | `new_admin`, `proposal_id` exact | zero writes        |
//! | 9 | Unaffected by pause state                   | `Ok(Ok(Some(…)))` while paused   | zero writes        |

use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient, PendingAdminTransfer};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

// ── ledger snapshot helpers ───────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct LedgerSnapshot {
    pending: Option<PendingAdminTransfer>,
    admin: Option<Address>,
    /// Emergency-pause flag lives under `DataKey::Ep`.
    paused: bool,
    /// Distinguishes "key absent" from a hypothetical serialised `None`.
    pending_key_exists: bool,
}

fn capture_ledger(env: &Env, contract_id: &Address) -> LedgerSnapshot {
    env.as_contract(contract_id, || {
        let pending_key_exists = env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer);
        let pending: Option<PendingAdminTransfer> = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer);
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        let paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        LedgerSnapshot {
            pending,
            admin,
            paused,
            pending_key_exists,
        }
    })
}

fn assert_storage_unchanged(before: &LedgerSnapshot, after: &LedgerSnapshot) {
    assert_eq!(
        after.pending, before.pending,
        "DataKey::PendingAdminTransfer must not be mutated"
    );
    assert_eq!(
        after.pending_key_exists, before.pending_key_exists,
        "presence of DataKey::PendingAdminTransfer must not change"
    );
    assert_eq!(
        after.admin, before.admin,
        "DataKey::Admin must not be mutated"
    );
    assert_eq!(
        after.paused, before.paused,
        "DataKey::Ep must not be mutated"
    );
}

// ── scenario 1: uninitialized contract ───────────────────────────────────────

/// Calling `get_pending_admin_transfer` before `initialize` must surface a
/// typed `NotInitialized` error and leave the ledger completely untouched.
#[test]
fn test_get_pending_admin_transfer_uninitialized_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let before = capture_ledger(&env, &contract_id);
    let result = client.try_get_pending_admin_transfer();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
    assert_storage_unchanged(&before, &after);
}

/// `NotInitialized` is returned consistently on every call, not just the first.
#[test]
fn test_get_pending_admin_transfer_uninitialized_is_stable() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    for _ in 0..3 {
        let before = capture_ledger(&env, &contract_id);
        let result = client.try_get_pending_admin_transfer();
        let after = capture_ledger(&env, &contract_id);

        assert_eq!(result, Err(Ok(Error::NotInitialized)));
        assert_storage_unchanged(&before, &after);
    }
}

// ── scenario 2: initialized, no proposal ─────────────────────────────────────

#[test]
fn test_get_pending_admin_transfer_returns_none_when_no_proposal_exists() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = capture_ledger(&env, &contract_id);
    assert!(!before.pending_key_exists);

    let result = client.get_pending_admin_transfer();

    let after = capture_ledger(&env, &contract_id);
    assert_eq!(result, None);
    assert_storage_unchanged(&before, &after);
}

// ── scenario 3: after a successful proposal ───────────────────────────────────

#[test]
fn test_get_pending_admin_transfer_returns_some_after_proposal() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &42u32);

    let before = capture_ledger(&env, &contract_id);
    assert!(before.pending_key_exists);

    let result = client.get_pending_admin_transfer();

    let after = capture_ledger(&env, &contract_id);

    let proposal = result.expect("must return Some after a pending proposal");
    assert_eq!(proposal.new_admin, new_admin);
    assert_eq!(proposal.proposal_id, 42u32);
    assert_storage_unchanged(&before, &after);
}

// ── scenario 4: after cancel ─────────────────────────────────────────────────

#[test]
fn test_get_pending_admin_transfer_returns_none_after_cancel() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &7u32);
    client.cancel_admin_transfer_proposal(&admin_addr);

    let before = capture_ledger(&env, &contract_id);
    assert!(!before.pending_key_exists);

    let result = client.get_pending_admin_transfer();

    let after = capture_ledger(&env, &contract_id);
    assert_eq!(result, None);
    assert_storage_unchanged(&before, &after);
}

// ── scenario 5: after execute ─────────────────────────────────────────────────

#[test]
fn test_get_pending_admin_transfer_returns_none_after_execute() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    let signers = vec![&env, new_admin.clone()];
    client.multisig_approval_init(&admin_addr, &signers, &1u32);
    client.propose_admin_transfer(&admin_addr, &new_admin, &1u32);
    client.multisig_approve(&new_admin, &1u32);
    client.execute_admin_transfer();

    let before = capture_ledger(&env, &contract_id);
    assert!(!before.pending_key_exists);

    let result = client.get_pending_admin_transfer();

    let after = capture_ledger(&env, &contract_id);
    assert_eq!(result, None);
    assert_storage_unchanged(&before, &after);
}

// ── scenario 6: idempotent reads ─────────────────────────────────────────────

#[test]
fn test_get_pending_admin_transfer_is_idempotent() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &99u32);

    for _ in 0..3 {
        let before = capture_ledger(&env, &contract_id);
        let result = client.get_pending_admin_transfer();
        let after = capture_ledger(&env, &contract_id);

        let proposal = result.expect("must return Some on every read");
        assert_eq!(proposal.new_admin, new_admin);
        assert_eq!(proposal.proposal_id, 99u32);
        assert_storage_unchanged(&before, &after);
    }
}

// ── scenario 7: snapshot byte-identity ───────────────────────────────────────

/// Core invariant: ledger snapshot is byte-identical before and after — proposal present.
#[test]
fn test_get_pending_admin_transfer_snapshot_is_byte_identical_before_and_after() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &5u32);

    let before = capture_ledger(&env, &contract_id);
    let _ = client.get_pending_admin_transfer();
    let after = capture_ledger(&env, &contract_id);

    assert_storage_unchanged(&before, &after);
}

/// Byte-identical when no proposal exists.
#[test]
fn test_get_pending_admin_transfer_snapshot_byte_identical_when_no_proposal() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = capture_ledger(&env, &contract_id);
    let _ = client.get_pending_admin_transfer();
    let after = capture_ledger(&env, &contract_id);

    assert_storage_unchanged(&before, &after);
}

/// The `NotInitialized` guard path must not write anything.
#[test]
fn test_get_pending_admin_transfer_snapshot_byte_identical_when_uninitialized() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let before = capture_ledger(&env, &contract_id);
    let _ = client.try_get_pending_admin_transfer();
    let after = capture_ledger(&env, &contract_id);

    assert_storage_unchanged(&before, &after);
}

// ── scenario 8: payload integrity ────────────────────────────────────────────

#[test]
fn test_get_pending_admin_transfer_returns_exact_proposal_payload() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);
    let proposal_id: u32 = 12_345;

    client.propose_admin_transfer(&admin_addr, &new_admin, &proposal_id);

    let proposal = client
        .get_pending_admin_transfer()
        .expect("proposal must be present");

    assert_eq!(proposal.new_admin, new_admin, "new_admin must be verbatim");
    assert_eq!(
        proposal.proposal_id, proposal_id,
        "proposal_id must be verbatim"
    );
}

// ── scenario 9: unaffected by pause state ────────────────────────────────────

#[test]
fn test_get_pending_admin_transfer_unaffected_by_pause_state() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &3u32);
    client.emergency_pause(&client_addr, &freelancer_addr);

    let before = capture_ledger(&env, &contract_id);
    assert!(before.paused, "contract must be paused for this test");

    let result = client.get_pending_admin_transfer();

    let after = capture_ledger(&env, &contract_id);
    let proposal = result.expect("proposal must still be readable while paused");
    assert_eq!(proposal.new_admin, new_admin);
    assert_eq!(proposal.proposal_id, 3u32);
    assert_storage_unchanged(&before, &after);
}
