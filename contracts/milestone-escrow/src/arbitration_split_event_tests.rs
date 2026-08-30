//! Dedicated test suite for the `arbsplit` structured event emitted by
//! `apply_dispute_arbitration_split` (issue #400).
//!
//! Every test asserts that the emitted `ArbitrationSplitAppliedEvent` fields
//! reconcile exactly with the state the call persisted (token balances, the
//! stored `ArbitrationSplitBps` entry, the milestone's cumulative release, and
//! its terminal status), and that no `arbsplit` event is emitted on any
//! failure path.

use super::*;
use crate::{ArbitrationSplitAppliedEvent, DataKey, Error, MilestoneStatus};
use soroban_sdk::{
    symbol_short, testutils::Events, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val,
};

const ARBSPLIT_TOPIC: &str = "arbsplit";

fn arbsplit_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("arbsplit").into_val(env);
    let mut count = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

fn last_arbsplit_event(env: &Env) -> ArbitrationSplitAppliedEvent {
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, ARBSPLIT_TOPIC));
    ArbitrationSplitAppliedEvent::from_val(env, &last.2)
}

fn stored_split_bps(env: &Env, contract_id: &Address, index: u32) -> Option<u32> {
    env.as_contract(contract_id, || {
        env.storage()
            .temporary()
            .get(&DataKey::ArbitrationSplitBps(index))
    })
}

/// Drive a funded escrow's milestone 0 into `Disputed` and apply the split.
/// Returns the `(contract_id, arbiter, client, freelancer)` addresses.
fn disputed_escrow(
    env: &Env,
    milestone_amounts: soroban_sdk::Vec<i128>,
) -> (
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let (client_addr, freelancer_addr, arbiter_addr, _admin, _token, contract_id, client) =
        setup_funded_escrow(env, milestone_amounts);
    client.raise_dispute(&client_addr, &0u32);
    (
        contract_id,
        arbiter_addr,
        client_addr,
        freelancer_addr,
        client,
    )
}

// ── success path: field-by-field reconciliation ─────────────────────────────

#[test]
fn event_reconciles_with_persisted_state_on_partial_split() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 10_000_i128];
    let (contract_id, arbiter_addr, client_addr, freelancer_addr, client) =
        disputed_escrow(&env, amounts);
    let token = token::Client::new(&env, &client.get_job().token);

    // 40% to the client, 60% to the freelancer.
    let alloc: RefundAllocation =
        client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &4_000u32);

    // Exactly one structured event.
    assert_eq!(arbsplit_event_count(&env), 1);
    let ev = last_arbsplit_event(&env);

    // Acting address + identities.
    assert_eq!(ev.contract_id, contract_id);
    assert_eq!(ev.arbiter, arbiter_addr);
    assert_eq!(ev.milestone_index, 0u32);
    assert_eq!(ev.client, client_addr);
    assert_eq!(ev.freelancer, freelancer_addr);
    assert_eq!(ev.token, client.get_job().token);

    // Transferred amounts reconcile with the return value and the ledger.
    assert_eq!(ev.client_refund, alloc.client_refund);
    assert_eq!(ev.freelancer_payout, alloc.freelancer_payout);
    assert_eq!(ev.client_refund, 4_000);
    assert_eq!(ev.freelancer_payout, 6_000);
    assert_eq!(token.balance(&client_addr), ev.client_refund);
    assert_eq!(token.balance(&freelancer_addr), ev.freelancer_payout);

    // BPS reconcile with the value persisted under ArbitrationSplitBps(0).
    assert_eq!(ev.client_refund_bps, 4_000);
    assert_eq!(ev.freelancer_payout_bps, 6_000);
    assert_eq!(
        stored_split_bps(&env, &contract_id, 0),
        Some(ev.client_refund_bps)
    );

    // Milestone state reconciles with the persisted milestone.
    let ms = client.get_job().milestones.get(0).unwrap();
    assert_eq!(ev.released_amount, ms.released_amount);
    assert_eq!(ev.released_amount, 6_000);
    assert_eq!(ev.status, ms.status);
    assert_eq!(ev.status, MilestoneStatus::Released);
}

#[test]
fn event_reports_refunded_status_on_full_client_refund() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 7_500_i128];
    let (contract_id, arbiter_addr, client_addr, freelancer_addr, client) =
        disputed_escrow(&env, amounts);
    let token = token::Client::new(&env, &client.get_job().token);

    client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &10_000u32);

    assert_eq!(arbsplit_event_count(&env), 1);
    let ev = last_arbsplit_event(&env);

    assert_eq!(ev.client_refund, 7_500);
    assert_eq!(ev.freelancer_payout, 0);
    assert_eq!(ev.client_refund_bps, 10_000);
    assert_eq!(ev.freelancer_payout_bps, 0);
    assert_eq!(ev.released_amount, 0);
    assert_eq!(ev.status, MilestoneStatus::Refunded);

    let ms = client.get_job().milestones.get(0).unwrap();
    assert_eq!(ev.released_amount, ms.released_amount);
    assert_eq!(ev.status, ms.status);
    assert_eq!(token.balance(&client_addr), 7_500);
    assert_eq!(token.balance(&freelancer_addr), 0);
    assert_eq!(stored_split_bps(&env, &contract_id, 0), Some(10_000));
}

#[test]
fn event_reports_released_status_on_full_freelancer_award() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 9_000_i128];
    let (contract_id, arbiter_addr, _client_addr, freelancer_addr, client) =
        disputed_escrow(&env, amounts);
    let token = token::Client::new(&env, &client.get_job().token);

    client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &0u32);

    assert_eq!(arbsplit_event_count(&env), 1);
    let ev = last_arbsplit_event(&env);

    assert_eq!(ev.client_refund, 0);
    assert_eq!(ev.freelancer_payout, 9_000);
    assert_eq!(ev.client_refund_bps, 0);
    assert_eq!(ev.freelancer_payout_bps, 10_000);
    assert_eq!(ev.released_amount, 9_000);
    assert_eq!(ev.status, MilestoneStatus::Released);

    let ms = client.get_job().milestones.get(0).unwrap();
    assert_eq!(ev.released_amount, ms.released_amount);
    assert_eq!(ev.status, ms.status);
    assert_eq!(token.balance(&freelancer_addr), 9_000);
    assert_eq!(stored_split_bps(&env, &contract_id, 0), Some(0));
}

#[test]
fn event_amounts_sum_to_disputed_balance_with_odd_rounding() {
    let env = Env::default();
    env.mock_all_auths();

    // 101 with a 50/50 split forces integer rounding.
    let amounts = vec![&env, 101_i128];
    let (contract_id, arbiter_addr, client_addr, freelancer_addr, client) =
        disputed_escrow(&env, amounts);

    client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &5_000u32);

    let ev = last_arbsplit_event(&env);
    assert_eq!(arbsplit_event_count(&env), 1);

    // No value created or destroyed: the two payouts must sum to the balance.
    assert_eq!(ev.client_refund + ev.freelancer_payout, 101);
    let token = token::Client::new(&env, &client.get_job().token);
    assert_eq!(token.balance(&client_addr), ev.client_refund);
    assert_eq!(token.balance(&freelancer_addr), ev.freelancer_payout);
    assert_eq!(ev.released_amount, ev.freelancer_payout);
    assert_eq!(stored_split_bps(&env, &contract_id, 0), Some(5_000));
}

// ── failure paths: no event emitted ────────────────────────────────────────

#[test]
fn no_event_when_caller_is_not_the_arbiter() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 10_000_i128];
    let (_contract_id, _arbiter_addr, _client_addr, _freelancer_addr, client) =
        disputed_escrow(&env, amounts);

    let stranger = Address::generate(&env);
    let result = client.try_apply_dispute_arbitration_split(&stranger, &0u32, &5_000u32);

    assert!(result.is_err());
    assert_eq!(arbsplit_event_count(&env), 0);
}

#[test]
fn no_event_when_milestone_is_not_disputed() {
    let env = Env::default();
    env.mock_all_auths();

    // Funded but never disputed → InvalidStatus.
    let (_client, _freelancer, arbiter_addr, _admin, _token, _contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 10_000_i128]);

    let result = client.try_apply_dispute_arbitration_split(&arbiter_addr, &0u32, &5_000u32);

    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(arbsplit_event_count(&env), 0);
}

#[test]
fn no_event_when_bps_exceeds_scale() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 10_000_i128];
    let (contract_id, arbiter_addr, _client_addr, _freelancer_addr, client) =
        disputed_escrow(&env, amounts);

    let result = client.try_apply_dispute_arbitration_split(&arbiter_addr, &0u32, &10_001u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(arbsplit_event_count(&env), 0);
    // Nothing persisted on the failure path.
    assert_eq!(stored_split_bps(&env, &contract_id, 0), None);
}

#[test]
fn no_event_on_double_execution_failure() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 10_000_i128];
    let (contract_id, arbiter_addr, _client_addr, _freelancer_addr, client) =
        disputed_escrow(&env, amounts);

    // First application succeeds and emits exactly one structured event.
    client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &3_000u32);
    assert_eq!(arbsplit_event_count(&env), 1);
    let stored = stored_split_bps(&env, &contract_id, 0);
    assert_eq!(stored, Some(3_000));

    // Milestone is now terminal; a repeat call reverts with InvalidStatus.
    // A reverted invocation rolls back its events, so the failure path
    // contributes no `arbsplit` event of its own.
    let result = client.try_apply_dispute_arbitration_split(&arbiter_addr, &0u32, &3_000u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(arbsplit_event_count(&env), 0);

    // The persisted split from the successful call is untouched.
    assert_eq!(stored_split_bps(&env, &contract_id, 0), stored);
}
