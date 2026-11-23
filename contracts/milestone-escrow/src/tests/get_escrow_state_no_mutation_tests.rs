#![cfg(test)]
//! `get_escrow_state` bundles everything a UI needs to render an escrow (job,
//! admin, both pause flags, cancellation lock and approvals, per-milestone
//! extensions and deadlines, fee allocation, version) into one read.
//!
//! Like the other `*_no_mutation_tests`, the read-only guarantee is checked
//! by comparing whole-ledger snapshots (`Env::to_ledger_snapshot`, which
//! includes every entry and its TTL) taken before and after the call, and by
//! asserting the call publishes no event.

use crate::test::setup_funded_escrow;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{vec, Env};

fn assert_get_escrow_state_is_read_only(env: &Env, escrow: &MilestoneEscrowClient<'_>) {
    let before = env.to_ledger_snapshot();
    escrow.get_escrow_state();
    assert_eq!(
        crate::all_event_tuples(env).len(),
        0,
        "get_escrow_state publishes no event"
    );
    escrow.get_escrow_state();
    let after = env.to_ledger_snapshot();
    assert_eq!(before, after, "get_escrow_state mutated the ledger");
}

#[test]
fn get_escrow_state_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);

    assert_get_escrow_state_is_read_only(&env, &escrow);
}

#[test]
fn get_escrow_state_does_not_mutate_ledger_with_extensions_and_cancel_approval() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.extend_milestone_deadline(&client, &0, &120);
    escrow.cancel_escrow(&client);

    assert_get_escrow_state_is_read_only(&env, &escrow);
}

#[test]
fn get_escrow_state_bundles_the_full_render_state() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, admin, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.extend_milestone_deadline(&client, &0, &120);
    escrow.cancel_escrow(&client);

    let state = escrow.get_escrow_state();

    // Same job as get_job.
    let job = escrow.get_job();
    assert_eq!(state.job.client, client);
    assert_eq!(state.job.freelancer, freelancer);
    assert_eq!(state.job.arbiter, arbiter);
    assert_eq!(state.job.token, token);
    assert_eq!(state.job.milestones.len(), job.milestones.len());
    assert_eq!(state.job.funded, job.funded);

    assert_eq!(state.admin, admin);
    assert!(!state.paused.emergency);
    assert!(!state.paused.admin);
    // The client approved cancellation (bit 1); the escrow is not locked yet.
    assert_eq!(state.cancel.approval_mask, 1);
    assert!(!state.cancel.locked);

    assert_eq!(state.milestone_timing.len(), 2);
    let delivered = state.milestone_timing.get(0).unwrap();
    assert_eq!(delivered.extension_seconds, 120);
    let m0 = job.milestones.get(0).unwrap();
    assert_eq!(
        delivered.deadline,
        Some(m0.delivered_at + job.auto_release_seconds + 120)
    );
    let pending = state.milestone_timing.get(1).unwrap();
    assert_eq!(pending.extension_seconds, 0);
    assert_eq!(pending.deadline, None, "no deadline before delivery");

    assert_eq!(state.version, escrow.version());
    assert_eq!(
        state.fee_allocation_set,
        escrow.try_get_platform_fee_allocation().is_ok()
    );

    // Assert newly added fields for upfront escrow (issue #970)
    assert_eq!(state.acceptance, 3);
    assert_eq!(state.funding_mode, FundingMode::Upfront);
    assert!(state.funded_at.is_some());
    assert_eq!(state.total_owed, 0);
    assert_eq!(state.arbiter_fee_bps, 0);
    assert_eq!(state.dispute_timeout, 604_800);
    assert_eq!(state.schema_version, 5);

    assert_eq!(delivered.due_at, 0);
    assert_eq!(delivered.revision_count, 0);
    assert_eq!(delivered.has_dispute, false);
}

#[test]
fn get_escrow_state_reports_per_milestone_and_timing_fields() {
    let env = Env::default();
    env.mock_all_auths();

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128, 2_000_i128];
    escrow.initialize_with_funding_mode(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        &FundingMode::PerMilestone,
    );

    escrow.set_due_dates(&client, &vec![&env, 100u64, 200u64]);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);

    let state = escrow.get_escrow_state();
    assert_eq!(state.funding_mode, FundingMode::PerMilestone);
    assert_eq!(state.acceptance, 3);
    assert_eq!(state.funded_at, None);
    assert_eq!(state.milestone_timing.len(), 2);
    assert_eq!(state.milestone_timing.get(0).unwrap().due_at, 100);
    assert_eq!(state.milestone_timing.get(1).unwrap().due_at, 200);
    assert_eq!(state.milestone_timing.get(0).unwrap().revision_count, 0);
    assert_eq!(state.milestone_timing.get(0).unwrap().has_dispute, false);

    assert_get_escrow_state_is_read_only(&env, &escrow);
}

#[test]
fn get_escrow_state_reports_the_emergency_pause() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.emergency_pause(&client, &freelancer);

    let state = escrow.get_escrow_state();
    assert!(state.paused.emergency);
    assert_eq!(state.paused.emergency, escrow.is_emergency_paused());
}

#[test]
fn get_escrow_state_on_uninitialized_contract_returns_not_initialized_without_mutation() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    assert!(matches!(
        escrow.try_get_escrow_state(),
        Err(Ok(Error::NotInitialized))
    ));
    let after = env.to_ledger_snapshot();
    assert_eq!(
        before, after,
        "a rejected get_escrow_state mutated the ledger"
    );
}
