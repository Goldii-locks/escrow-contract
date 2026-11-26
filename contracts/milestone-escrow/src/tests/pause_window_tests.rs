#![cfg(test)]

//! Issue #972: time spent paused must not consume a party's response window.
//! Every clock below is driven by the same pause interval, so a pause that
//! overlaps a window moves that window's deadline by exactly its duration.

use super::*;
use soroban_sdk::{testutils::Ledger, vec, BytesN, Env};

const DELIVERED_AT: u64 = 1_000;
const AUTO_RELEASE_SECONDS: u64 = 604_800;
const DEADLINE: u64 = DELIVERED_AT + AUTO_RELEASE_SECONDS;
const PAUSE_SECONDS: u64 = 700_000;
const OVERDUE_GRACE_SECONDS: u64 = 86_400;

fn note(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0x31; 32])
}

fn delivered_escrow(env: &Env) -> (Address, Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    env.ledger().set_timestamp(DELIVERED_AT);
    let (client, freelancer, _, admin, _, _, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    (client, freelancer, admin, escrow)
}

#[test]
fn pause_longer_than_review_window_keeps_auto_release_claim_waiting() {
    let env = Env::default();
    let (client, freelancer, admin, escrow) = delivered_escrow(&env);

    // Only 100 seconds of the review window elapsed before the admin froze it.
    env.ledger().set_timestamp(DELIVERED_AT + 100);
    escrow.admin_pause_escrow(&admin);
    env.ledger()
        .set_timestamp(DELIVERED_AT + 100 + PAUSE_SECONDS);
    escrow.admin_resume_escrow(&admin);

    // The nominal deadline is long gone, but the window still has 604_700 s.
    assert!(escrow.time_until_auto_release(&0) > 0);
    assert_eq!(
        escrow.try_claim_auto_release(&freelancer, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
    // The client's review actions are available again as soon as the pause ends.
    escrow.request_revision(&client, &0, &note(&env));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
    );
}

#[test]
fn auto_release_waits_for_the_full_pause_adjusted_deadline() {
    let env = Env::default();
    let (_, freelancer, admin, escrow) = delivered_escrow(&env);

    env.ledger().set_timestamp(DELIVERED_AT + 100);
    escrow.admin_pause_escrow(&admin);
    env.ledger()
        .set_timestamp(DELIVERED_AT + 100 + PAUSE_SECONDS);
    escrow.admin_resume_escrow(&admin);

    env.ledger().set_timestamp(DEADLINE + PAUSE_SECONDS - 1);
    assert_eq!(
        escrow.try_claim_auto_release(&freelancer, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
    env.ledger().set_timestamp(DEADLINE + PAUSE_SECONDS);
    escrow.claim_auto_release(&freelancer, &0);
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
}

#[test]
fn pause_before_delivery_does_not_extend_the_review_window() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(DELIVERED_AT);
    let (_, freelancer, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.admin_pause_escrow(&admin);
    env.ledger().set_timestamp(DELIVERED_AT + PAUSE_SECONDS);
    escrow.admin_resume_escrow(&admin);
    escrow.mark_delivered(&freelancer, &0);

    env.ledger()
        .set_timestamp(env.ledger().timestamp() + AUTO_RELEASE_SECONDS);
    escrow.claim_auto_release(&freelancer, &0);
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
}

#[test]
fn separate_pauses_accumulate_into_the_window_deadline() {
    let env = Env::default();
    let (_, freelancer, admin, escrow) = delivered_escrow(&env);

    env.ledger().set_timestamp(DELIVERED_AT + 10);
    escrow.admin_pause_escrow(&admin);
    env.ledger().set_timestamp(DELIVERED_AT + 110);
    escrow.admin_resume_escrow(&admin);
    env.ledger().set_timestamp(DELIVERED_AT + 210);
    escrow.admin_pause_escrow(&admin);
    env.ledger().set_timestamp(DELIVERED_AT + 410);
    escrow.admin_resume_escrow(&admin);

    env.ledger().set_timestamp(DEADLINE + 300 - 1);
    assert_eq!(
        escrow.try_claim_auto_release(&freelancer, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
    env.ledger().set_timestamp(DEADLINE + 300);
    escrow.claim_auto_release(&freelancer, &0);
}

#[test]
fn overlapping_admin_and_party_pauses_are_counted_once() {
    let env = Env::default();
    let (client, freelancer, admin, escrow) = delivered_escrow(&env);

    env.ledger().set_timestamp(DELIVERED_AT + 50);
    escrow.emergency_pause(&client, &freelancer);
    // admin_pause_escrow only checks Paused, so the two flags can overlap.
    escrow.admin_pause_escrow(&admin);
    env.ledger()
        .set_timestamp(DELIVERED_AT + 50 + PAUSE_SECONDS);
    escrow.emergency_unpause(&admin);

    env.ledger().set_timestamp(DEADLINE + PAUSE_SECONDS - 1);
    assert_eq!(
        escrow.try_claim_auto_release(&freelancer, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
    env.ledger().set_timestamp(DEADLINE + PAUSE_SECONDS);
    escrow.claim_auto_release(&freelancer, &0);
}

#[test]
fn dispute_escalation_timeout_is_extended_by_pause_time() {
    let env = Env::default();
    let (client, _, admin, escrow) = delivered_escrow(&env);

    env.ledger().set_timestamp(DEADLINE - 1);
    escrow.raise_dispute(&client, &0, &dispute_reason_hash(&env));
    let disputed_at = env.ledger().timestamp();

    escrow.admin_pause_escrow(&admin);
    env.ledger().set_timestamp(disputed_at + PAUSE_SECONDS);
    escrow.admin_resume_escrow(&admin);

    let timeout = DISPUTE_TIMEOUT_SECONDS;
    env.ledger()
        .set_timestamp(disputed_at + timeout + PAUSE_SECONDS - 1);
    assert_eq!(
        escrow.try_escalate_dispute(&client, &0),
        Err(Ok(Error::DisputeTimeoutNotReached))
    );
    env.ledger()
        .set_timestamp(disputed_at + timeout + PAUSE_SECONDS);
    escrow.escalate_dispute(&client, &0);
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Delivered
    );
}

#[test]
fn reclaim_overdue_grace_is_extended_by_pause_time() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(DELIVERED_AT);
    let (client, _, _, admin, _, contract_id, escrow) =
        setup_unfunded_escrow(&env, vec![&env, 1_000_i128]);
    // The shared helper accepts the job; deadlines must be set before that.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&crate::DataKey::Acceptance);
    });
    escrow.set_milestone_due_dates(&client, &vec![&env, 10_000_u64]);
    escrow.fund(&client);

    env.ledger().set_timestamp(10_000);
    escrow.admin_pause_escrow(&admin);
    env.ledger().set_timestamp(10_000 + PAUSE_SECONDS);
    escrow.admin_resume_escrow(&admin);

    let nominal_deadline = 10_000 + OVERDUE_GRACE_SECONDS;
    env.ledger().set_timestamp(nominal_deadline + 1);
    assert_eq!(
        escrow.try_reclaim_overdue(&client, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
    env.ledger()
        .set_timestamp(nominal_deadline + PAUSE_SECONDS + 1);
    escrow.reclaim_overdue(&client, &0);
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Refunded
    );
}
