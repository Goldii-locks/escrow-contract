//! Tests for the dispute reason hash and evidence trail added for #712.
//!
//! `raise_dispute` now takes a 32-byte `reason_hash`, `submit_evidence` pins
//! further hashes to an open dispute, and `get_dispute_info` exposes the
//! resulting record. The suite covers the happy path for both parties, the
//! event payloads, the rejection paths and the evidence cap.

use super::*;
use soroban_sdk::{symbol_short, TryFromVal, TryIntoVal, Val};

fn evidence_hash(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

/// Collect every event payload published under `topic` and decode it as `T`.
fn payloads<T>(env: &Env, topic: Symbol) -> std::vec::Vec<T>
where
    T: TryFromVal<Env, Val>,
{
    crate::all_event_tuples(env)
        .into_iter()
        .filter(|(_, topics, _)| {
            topics
                .get(0)
                .and_then(|value| {
                    let emitted: Option<Symbol> = value.try_into_val(env).ok();
                    emitted
                })
                .map(|emitted| emitted == topic)
                .unwrap_or(false)
        })
        .map(|(_, _, data)| T::try_from_val(env, &data).expect("event payload decodes"))
        .collect()
}

/// Funded escrow with one delivered milestone and an open dispute.
fn disputed_escrow(env: &Env) -> (Address, Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(env));
    (client_addr, freelancer_addr, arbiter_addr, escrow)
}

#[test]
fn raise_dispute_records_reason_hash_and_exposes_it() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);

    let reason_hash = evidence_hash(&env, 0xAA);
    escrow.raise_dispute(&client_addr, &0, &reason_hash);

    let raised: std::vec::Vec<DisputeRaisedEvent> = payloads(&env, symbol_short!("dispute"));
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].milestone_index, 0);
    assert_eq!(raised[0].caller, client_addr);
    assert_eq!(raised[0].milestone_amount, 1_000);
    assert_eq!(raised[0].reason_hash, reason_hash);
    assert_eq!(raised[0].disputed_at, 1_000);
    assert_eq!(raised[0].new_status, MilestoneStatus::Disputed);

    let info = escrow.get_dispute_info(&0);
    assert_eq!(info.disputed_at, 1_000);
    assert_eq!(info.previous_status, MilestoneStatus::Delivered);
    assert_eq!(info.reason_hash, reason_hash);
    assert_eq!(info.evidence.len(), 0);
}

#[test]
fn freelancer_can_raise_dispute_with_its_own_reason_hash() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 500_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);

    let reason_hash = evidence_hash(&env, 0xBB);
    escrow.raise_dispute(&freelancer_addr, &0, &reason_hash);

    let raised: std::vec::Vec<DisputeRaisedEvent> = payloads(&env, symbol_short!("dispute"));
    assert_eq!(raised[0].caller, freelancer_addr);

    let info = escrow.get_dispute_info(&0);
    assert_eq!(info.reason_hash, reason_hash);
}
#[test]
fn both_parties_can_submit_evidence_in_order() {
    let env = Env::default();
    let (client_addr, freelancer_addr, _, escrow) = disputed_escrow(&env);

    env.ledger().set_timestamp(1_010);
    let client_hash = evidence_hash(&env, 0x01);
    escrow.submit_evidence(&client_addr, &0, &client_hash);

    let first_events: std::vec::Vec<EvidenceSubmittedEvent> =
        payloads(&env, symbol_short!("evidence"));
    assert_eq!(first_events.len(), 1);
    assert_eq!(first_events[0].milestone_index, 0);
    assert_eq!(first_events[0].caller, client_addr);
    assert_eq!(first_events[0].evidence_hash, client_hash);
    assert_eq!(first_events[0].submitted_at, 1_010);

    env.ledger().set_timestamp(1_020);
    let freelancer_hash = evidence_hash(&env, 0x02);
    escrow.submit_evidence(&freelancer_addr, &0, &freelancer_hash);

    let second_events: std::vec::Vec<EvidenceSubmittedEvent> =
        payloads(&env, symbol_short!("evidence"));
    assert_eq!(second_events.len(), 1);
    assert_eq!(second_events[0].caller, freelancer_addr);
    assert_eq!(second_events[0].evidence_hash, freelancer_hash);
    assert_eq!(second_events[0].submitted_at, 1_020);

    let info = escrow.get_dispute_info(&0);
    assert_eq!(info.evidence.len(), 2);

    let first = info.evidence.get(0).expect("first evidence record");
    assert_eq!(first.submitter, client_addr);
    assert_eq!(first.evidence_hash, client_hash);
    assert_eq!(first.submitted_at, 1_010);

    let second = info.evidence.get(1).expect("second evidence record");
    assert_eq!(second.submitter, freelancer_addr);
    assert_eq!(second.evidence_hash, freelancer_hash);
    assert_eq!(second.submitted_at, 1_020);

    // The reason hash survives evidence submissions unchanged.
    assert_eq!(info.reason_hash, dispute_reason_hash(&env));
}

#[test]
fn submit_evidence_rejects_non_party_without_mutating_state() {
    let env = Env::default();
    let (_, _, _, escrow) = disputed_escrow(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        escrow.try_submit_evidence(&stranger, &0, &evidence_hash(&env, 0x03)),
        Err(Ok(Error::Unauthorized))
    );
    assert!(payloads::<EvidenceSubmittedEvent>(&env, symbol_short!("evidence")).is_empty());
    assert_eq!(escrow.get_dispute_info(&0).evidence.len(), 0);
}

#[test]
fn submit_evidence_rejects_milestones_that_are_not_disputed() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);

    // Delivered, not yet disputed.
    assert_eq!(
        escrow.try_submit_evidence(&client_addr, &0, &evidence_hash(&env, 0x04)),
        Err(Ok(Error::InvalidStatus))
    );

    escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(&env));
    escrow.resolve_dispute(&arbiter_addr, &0, &true, &decision_hash(&env));

    // Resolved milestones no longer accept evidence.
    assert_eq!(
        escrow.try_submit_evidence(&client_addr, &0, &evidence_hash(&env, 0x05)),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn submit_evidence_rejects_out_of_range_milestone() {
    let env = Env::default();
    let (client_addr, _, _, escrow) = disputed_escrow(&env);

    assert_eq!(
        escrow.try_submit_evidence(&client_addr, &7, &evidence_hash(&env, 0x06)),
        Err(Ok(Error::InvalidMilestone))
    );
}

#[test]
fn submit_evidence_enforces_the_per_dispute_cap() {
    let env = Env::default();
    let (client_addr, freelancer_addr, _, escrow) = disputed_escrow(&env);

    for i in 0..MAX_DISPUTE_EVIDENCE_ENTRIES {
        let submitter = if i % 2 == 0 {
            &client_addr
        } else {
            &freelancer_addr
        };
        escrow.submit_evidence(submitter, &0, &evidence_hash(&env, i as u8 + 1));
    }

    let info = escrow.get_dispute_info(&0);
    assert_eq!(info.evidence.len(), MAX_DISPUTE_EVIDENCE_ENTRIES);

    assert_eq!(
        escrow.try_submit_evidence(&client_addr, &0, &evidence_hash(&env, 0xFF)),
        Err(Ok(Error::EvidenceLimitReached))
    );
    // The rejected submission is not recorded.
    assert_eq!(
        escrow.get_dispute_info(&0).evidence.len(),
        MAX_DISPUTE_EVIDENCE_ENTRIES
    );
}

#[test]
fn dispute_info_is_only_available_while_the_dispute_is_open() {
    let env = Env::default();
    let (_, _, arbiter_addr, escrow) = disputed_escrow(&env);

    escrow.resolve_dispute(&arbiter_addr, &0, &true, &decision_hash(&env));

    assert_eq!(
        escrow.try_get_dispute_info(&0),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn get_dispute_info_without_an_open_dispute_returns_invalid_status() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);

    assert_eq!(
        escrow.try_get_dispute_info(&0),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn dispute_record_is_available_after_resolution() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0);
    let reason_hash = evidence_hash(&env, 0x11);
    escrow.raise_dispute(&client_addr, &0, &reason_hash);
    escrow.submit_evidence(&freelancer_addr, &0, &evidence_hash(&env, 0x12));

    // Initially, no dispute record exists while disputed
    assert_eq!(
        escrow.try_get_dispute_record(&0),
        Err(Ok(Error::InvalidStatus))
    );

    // Resolve dispute
    escrow.resolve_dispute(&arbiter_addr, &0, &true, &decision_hash(&env));

    // Verify DisputeRecord is preserved
    let rec = escrow.get_dispute_record(&0);
    assert_eq!(rec.reason_hash, reason_hash);
    assert_eq!(rec.evidence.len(), 1);
    assert_eq!(rec.outcome, MilestoneStatus::Released);

    // DisputeInfo is gone
    assert_eq!(
        escrow.try_get_dispute_info(&0),
        Err(Ok(Error::InvalidStatus))
    );
}
