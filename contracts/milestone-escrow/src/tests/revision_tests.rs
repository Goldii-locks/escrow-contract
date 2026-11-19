#![cfg(test)]
use super::*;
use soroban_sdk::{symbol_short, testutils::Ledger, token, vec, BytesN, Env, FromVal, TryIntoVal};

fn note(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0x11; 32])
}

#[test]
fn valid_revision_request() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.request_revision(&client_addr, &0u32, &note(&env));
    let job = escrow.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::Pending);
    assert_eq!(ms.delivered_at, 0);
    assert_eq!(escrow.get_revision_count(&0u32), 1);
}

#[test]
fn delivery_clock_reset() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    env.ledger().with_mut(|l| l.timestamp += 604800 - 1);
    escrow.request_revision(&client_addr, &0u32, &note(&env));
    env.ledger().with_mut(|l| l.timestamp += 2);
    assert_eq!(
        escrow.try_claim_auto_release(&freelancer_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn new_delivery_restarts_clock() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    let first = escrow.get_job().milestones.get(0).unwrap().delivered_at;
    escrow.request_revision(&client_addr, &0u32, &note(&env));
    env.ledger().with_mut(|l| l.timestamp += 50);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    let second = escrow.get_job().milestones.get(0).unwrap().delivered_at;
    assert!(second > first);
    assert_eq!(second, env.ledger().timestamp());
}

#[test]
fn revision_cap() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    for _ in 0..3 {
        escrow.mark_delivered(&freelancer_addr, &0u32);
        escrow.request_revision(&client_addr, &0u32, &note(&env));
    }
    escrow.mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(
        escrow.try_request_revision(&client_addr, &0u32, &note(&env)),
        Err(Ok(Error::MaxRevisions))
    );
    assert_eq!(escrow.get_revision_count(&0u32), 3);
}

#[test]
fn authorization_and_state_validation() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    assert_eq!(
        escrow.try_request_revision(&client_addr, &0u32, &note(&env)),
        Err(Ok(Error::InvalidStatus))
    );
    escrow.mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(
        escrow.try_request_revision(&freelancer_addr, &0u32, &note(&env)),
        Err(Ok(Error::Unauthorized))
    );
    escrow.approve_milestone(&client_addr, &0u32);
    assert_eq!(
        escrow.try_request_revision(&client_addr, &0u32, &note(&env)),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn event_correctness() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    let hash = note(&env);
    escrow.request_revision(&client_addr, &0u32, &hash);
    let events = crate::all_event_tuples(&env);
    let last = events.last().unwrap();
    let topic: soroban_sdk::Symbol = last.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("msrev"));
    let parsed = RevisionRequestedEvent::from_val(&env, &last.2);
    assert_eq!(parsed.milestone_index, 0);
    assert_eq!(parsed.note_hash, hash);
    assert_eq!(parsed.revision_count, 1);
}

#[test]
fn accounting_preservation() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    let before = token::Client::new(&env, &token).balance(&escrow.address);
    let funded = escrow.get_job().funded;
    escrow.request_revision(&client_addr, &0u32, &note(&env));
    let after = token::Client::new(&env, &token).balance(&escrow.address);
    let job = escrow.get_job();
    assert_eq!(before, after);
    assert_eq!(job.funded, funded);
    assert_eq!(job.milestones.get(0).unwrap().released_amount, 0);
    assert_eq!(job.milestones.get(0).unwrap().amount, 100);
}
