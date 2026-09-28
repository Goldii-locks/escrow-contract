#![cfg(test)]
//! Regression suite for issue #476: guarantee that
//! `get_job` performs no state mutation.
//!
//! `get_job` is a public read path that callers invoke to inspect the currently
//! stored job metadata. It must never write to instance, persistent, or
//! temporary storage, and must never emit events — a regression here would let a
//! "read" silently rewrite ledger state or affect contract entry TTLs.
//!
//! The validation strategy: take a full ledger snapshot
//! (`Env::to_ledger_snapshot`) immediately before calling the function under
//! test, take another immediately after, and assert the two snapshots are
//! identical. `LedgerSnapshot` captures every ledger entry (instance,
//! persistent, and temporary storage across all contracts registered on the
//! `Env`, including their live-until ledger sequence / TTL) plus the ledger
//! info itself, so this is a byte-for-byte equivalent check of the entire
//! ledger state, not just the one key we expect to be read. Any future edit
//! that adds a `.set(`, `.remove(`, `.extend_ttl(`, or event publish inside
//! `get_job` (or the shared helpers it calls) will change the snapshot and fail
//! this test.

use crate::test::setup_funded_escrow;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{vec, Env};

/// Call `get_job` twice between two whole-ledger snapshots and assert that
/// nothing changed and no event was published.
fn assert_get_job_is_read_only(env: &Env, escrow: &MilestoneEscrowClient<'_>) {
    let before = env.to_ledger_snapshot();
    escrow.get_job();
    // The test env only keeps events from the latest invocation, so this is
    // exactly what `get_job` published.
    assert_eq!(
        crate::all_event_tuples(env).len(),
        0,
        "get_job publishes no event"
    );
    escrow.get_job();
    let after = env.to_ledger_snapshot();
    assert_eq!(before, after, "get_job mutated the ledger");
}

#[test]
fn get_job_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);

    assert_get_job_is_read_only(&env, &escrow);
}

#[test]
fn get_job_does_not_mutate_ledger_after_milestone_progression() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    assert_get_job_is_read_only(&env, &escrow);
}

#[test]
fn get_job_on_uninitialized_contract_returns_not_initialized_without_mutation() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    assert!(matches!(
        escrow.try_get_job(),
        Err(Ok(Error::NotInitialized))
    ));
    let after = env.to_ledger_snapshot();
    assert_eq!(before, after, "a rejected get_job mutated the ledger");
}
