#![cfg(test)]
use super::*;
use soroban_sdk::{testutils::Ledger, vec, BytesN, Env};

// `setup_funded_escrow` uses a 604_800 s auto-release window; delivering at
// 1_000 puts the deadline at 605_800.
const DELIVERED_AT: u64 = 1_000;
const DEADLINE: u64 = DELIVERED_AT + 604_800;

fn note(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0x11; 32])
}

fn delivered_escrow(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    env.ledger().set_timestamp(DELIVERED_AT);
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    (client_addr, freelancer_addr, escrow)
}

#[test]
fn revision_succeeds_one_second_before_deadline() {
    let env = Env::default();
    let (client_addr, _, escrow) = delivered_escrow(&env);
    env.ledger().set_timestamp(DEADLINE - 1);
    escrow.request_revision(&client_addr, &0, &note(&env));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
    );
}

#[test]
fn revision_fails_one_second_after_deadline() {
    let env = Env::default();
    let (client_addr, _, escrow) = delivered_escrow(&env);
    env.ledger().set_timestamp(DEADLINE + 1);
    assert_eq!(
        escrow.try_request_revision(&client_addr, &0, &note(&env)),
        Err(Ok(Error::ReviewWindowClosed))
    );
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Delivered
    );
}

#[test]
fn dispute_succeeds_one_second_before_deadline() {
    let env = Env::default();
    let (client_addr, _, escrow) = delivered_escrow(&env);
    env.ledger().set_timestamp(DEADLINE - 1);
    escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(&env));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
}

#[test]
fn dispute_fails_one_second_after_deadline() {
    let env = Env::default();
    let (client_addr, _, escrow) = delivered_escrow(&env);
    env.ledger().set_timestamp(DEADLINE + 1);
    assert_eq!(
        escrow.try_raise_dispute(&client_addr, &0, &dispute_reason_hash(&env)),
        Err(Ok(Error::ReviewWindowClosed))
    );
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Delivered
    );
}

#[test]
fn dispute_escalate_dispute_loop_cannot_extend_deadline() {
    let env = Env::default();
    let (client_addr, freelancer_addr, escrow) = delivered_escrow(&env);

    env.ledger().set_timestamp(DEADLINE - 1);
    escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(&env));

    env.ledger()
        .set_timestamp(DEADLINE - 1 + DISPUTE_TIMEOUT_SECONDS);
    escrow.escalate_dispute(&client_addr, &0);

    let restored = escrow.get_job().milestones.get(0).unwrap();
    assert_eq!(restored.status, MilestoneStatus::Delivered);
    assert_eq!(restored.delivered_at, DELIVERED_AT);

    assert_eq!(
        escrow.try_raise_dispute(&client_addr, &0, &dispute_reason_hash(&env)),
        Err(Ok(Error::ReviewWindowClosed))
    );
    assert_eq!(
        escrow.try_request_revision(&client_addr, &0, &note(&env)),
        Err(Ok(Error::ReviewWindowClosed))
    );

    escrow.claim_auto_release(&freelancer_addr, &0);
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
}
