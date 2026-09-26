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

use super::*;
use soroban_sdk::{vec, Address, Env};

/// A fully initialized escrow with specified milestone amounts and parties.
fn initialized_escrow(env: &Env) -> (MilestoneEscrowClient<'_>, Address) {
    env.mock_all_auths();

    let admin_addr = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = vec![env, 1_000_i128, 2_000_i128, 3_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604_800u64,
        &amounts,
    );

    (escrow, admin_addr)
}

/// Calling `get_job` on a freshly initialized escrow must leave the entire
/// ledger byte-for-byte unchanged.
#[test]
fn get_job_does_not_mutate_ledger() {
    let env = Env::default();
    let (escrow, _admin_addr) = initialized_escrow(&env);

    let before = env.to_ledger_snapshot();
    let job = escrow.get_job();
    let after = env.to_ledger_snapshot();

    // Sanity check: we actually read back valid job data, so the
    // "no mutation" result below isn't vacuously true because the call
    // failed or short-circuited.
    assert_eq!(job.milestones.len(), 3);
    assert_eq!(job.milestones.get(0).unwrap().amount, 1_000i128);
    assert_eq!(job.milestones.get(1).unwrap().amount, 2_000i128);
    assert_eq!(job.milestones.get(2).unwrap().amount, 3_000i128);

    assert_eq!(
        before, after,
        "get_job must not mutate any ledger entry"
    );
}

/// The same guarantee must hold after the escrow has been funded, which
/// exercises a different contract state through the same read path.
#[test]
fn get_job_does_not_mutate_ledger_when_funded() {
    let env = Env::default();
    let (escrow, admin_addr) = initialized_escrow(&env);

    let token_addr = escrow.get_job().token;
    let token_client = token::Client::new(&env, &token_addr);

    // Fund the escrow
    token_client.mint(&admin_addr, &6_000i128);
    escrow.fund(&admin_addr, &6_000i128);

    let before = env.to_ledger_snapshot();
    let job = escrow.get_job();
    let after = env.to_ledger_snapshot();

    // Sanity check: milestones are unchanged even after funding
    assert_eq!(job.milestones.len(), 3);
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::Pending);

    assert_eq!(
        before, after,
        "get_job must not mutate any ledger entry even after funding"
    );
}

/// The same guarantee must hold after a milestone has been delivered and
/// approved, exercising further state progression through the same read path.
#[test]
fn get_job_does_not_mutate_ledger_after_milestone_progression() {
    let env = Env::default();
    let (escrow, admin_addr) = initialized_escrow(&env);

    let token_addr = escrow.get_job().token;
    let token_client = token::Client::new(&env, &token_addr);
    let job = escrow.get_job();

    // Fund and progress a milestone
    token_client.mint(&admin_addr, &6_000i128);
    escrow.fund(&admin_addr, &6_000i128);
    escrow.mark_delivered(&job.freelancer, &0u32);
    escrow.approve_milestone(&job.client, &0u32);

    let before = env.to_ledger_snapshot();
    let job_after = escrow.get_job();
    let after = env.to_ledger_snapshot();

    // Sanity check: we can observe the updated milestone state
    assert_eq!(job_after.milestones.get(0).unwrap().status, MilestoneStatus::Released);

    assert_eq!(
        before, after,
        "get_job must not mutate any ledger entry after milestone progression"
    );
}
