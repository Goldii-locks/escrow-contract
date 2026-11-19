#![cfg(test)]
//! Issue #1015: the arbiter commits a tamper-evident hash of their written
//! decision when a dispute is closed.
//!
//! The decision document itself stays off-chain (escrow-backend storage,
//! keyed by `decision_hash`); the contract only ever sees the 32-byte
//! commitment. These tests pin the contract side of that arrangement: both
//! resolution paths take a `BytesN<32>` and echo it verbatim in their
//! events, while the outcome, fund distribution and the (deleted)
//! `DisputeInfo` record stay exactly as they were.

use super::*;
use crate::{ArbitrationSplitAppliedEvent, DisputeResolvedEvent, Error, MilestoneStatus};
use soroban_sdk::{symbol_short, token, BytesN, Env, Symbol, TryFromVal, Val};

/// Commitment used only by this suite: distinct from the dispute's
/// `reason_hash` (`0x21…`) and the shared `decision_hash` helper (`0xD1…`),
/// so an assertion failure shows the value was echoed verbatim rather than
/// coincidentally equal.
fn written_decision(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0x5A; 32])
}

/// Collect every event payload published under `topic`, decoded as `T`.
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

/// Drive milestone 0 into `Disputed` on a funded escrow.
fn disputed_escrow(env: &Env) -> (Address, Address, Address, MilestoneEscrowClient<'_>) {
    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(env));
    (client_addr, freelancer_addr, arbiter_addr, escrow)
}

// ── resolve_dispute ─────────────────────────────────────────────────────────

#[test]
fn resolve_dispute_emits_the_decision_hash_and_keeps_the_outcome() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, escrow) = disputed_escrow(&env);

    let decision = written_decision(&env);
    escrow.resolve_dispute(&arbiter_addr, &0, &true, &decision);

    // Exactly one `resolve` event, echoing the commitment verbatim.
    let events = payloads::<DisputeResolvedEvent>(&env, symbol_short!("resolve"));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].decision_hash, decision);
    // The decision commitment is independent of the dispute's reason hash.
    assert_ne!(events[0].decision_hash, dispute_reason_hash(&env));

    // Outcome and fund distribution are untouched by the extra argument.
    assert_eq!(events[0].status, MilestoneStatus::Released);
    assert!(events[0].released_to_freelancer);
    assert_eq!(events[0].amount, 1_000);
    assert_eq!(events[0].paid_amount, 1_000);
    assert_eq!(events[0].arbiter_fee_amount, 0);
    let job = escrow.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
    let token = token::Client::new(&env, &job.token);
    assert_eq!(token.balance(&freelancer_addr), 1_000);
    assert_eq!(token.balance(&client_addr), 0);
}

#[test]
fn resolve_dispute_refund_path_emits_the_same_decision_hash() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, arbiter_addr, escrow) = disputed_escrow(&env);

    let decision = written_decision(&env);
    escrow.resolve_dispute(&arbiter_addr, &0, &false, &decision);

    let events = payloads::<DisputeResolvedEvent>(&env, symbol_short!("resolve"));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].decision_hash, decision);
    assert_eq!(events[0].status, MilestoneStatus::Refunded);
    assert!(!events[0].released_to_freelancer);
    let token = token::Client::new(&env, &escrow.get_job().token);
    assert_eq!(token.balance(&client_addr), 1_000);
}

// ── apply_dispute_arbitration_split ─────────────────────────────────────────

#[test]
fn arbitration_split_emits_the_decision_hash_in_both_events() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, escrow) = disputed_escrow(&env);

    let decision = written_decision(&env);
    escrow.apply_dispute_arbitration_split(&arbiter_addr, &0, &7_000u32, &decision);

    // The `resolve` event that closes the dispute carries the commitment…
    let resolve_events = payloads::<DisputeResolvedEvent>(&env, symbol_short!("resolve"));
    assert_eq!(resolve_events.len(), 1);
    assert_eq!(resolve_events[0].decision_hash, decision);

    // …and so does the dedicated `arbsplit` record of the split.
    let split_events = payloads::<ArbitrationSplitAppliedEvent>(&env, symbol_short!("arbsplit"));
    assert_eq!(split_events.len(), 1);
    assert_eq!(split_events[0].decision_hash, decision);

    // Outcome and fund distribution are untouched: 70/30 split of the
    // 1_000-token milestone → `Split` status.
    assert_eq!(split_events[0].status, MilestoneStatus::Split);
    assert_eq!(split_events[0].client_refund, 700);
    assert_eq!(split_events[0].freelancer_payout, 300);
    let token = token::Client::new(&env, &escrow.get_job().token);
    assert_eq!(token.balance(&client_addr), 700);
    assert_eq!(token.balance(&freelancer_addr), 300);
}

// ── persistence: the event is the anchor, not storage ───────────────────────

#[test]
fn the_dispute_record_is_deleted_so_the_event_anchors_the_commitment() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, arbiter_addr, escrow) = disputed_escrow(&env);

    // The record exists while the dispute is open…
    assert_eq!(
        escrow.get_dispute_info(&0).reason_hash,
        dispute_reason_hash(&env)
    );

    let decision = written_decision(&env);
    escrow.resolve_dispute(&arbiter_addr, &0, &true, &decision);

    // The commitment survives in the emitted event.
    let events = payloads::<DisputeResolvedEvent>(&env, symbol_short!("resolve"));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].decision_hash, decision);
    assert_eq!(events[0].milestone_index, 0);
    assert_eq!(events[0].arbiter, arbiter_addr);

    // …and is deleted by the resolution (pre-existing behaviour), so there
    // is no dispute-storage slot left to persist the decision hash into.
    assert_eq!(
        escrow.try_get_dispute_info(&0),
        Err(Ok(Error::InvalidStatus))
    );

    let token = token::Client::new(&env, &escrow.get_job().token);
    assert_eq!(token.balance(&client_addr), 0);
}
