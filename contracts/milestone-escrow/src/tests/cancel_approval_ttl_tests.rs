#![cfg(test)]
//! Regression suite for issue #713: a one-sided cancel approval must expire.
//!
//! `cancel_escrow` is a two-party handshake.  Before #713 the first party's
//! approval stayed in `DataKey::CancelApproval` forever, so an approval given in
//! week one during a disagreement could be completed months later — after the
//! parties made up and kept working — freezing the escrow on stale intent.
//! Approvals now carry `approved_at` and expire after
//! `CANCEL_APPROVAL_TTL_SECONDS`: a call made at or past that age records a
//! fresh first approval instead of completing the lock.
//!
//! Issue #1026 extends the same rule to `revoke_cancel_approval`: revoking an
//! approval that has already expired must fail with `InvalidStatus` and emit
//! nothing, exactly as for a missing approval.

use super::*;
use crate::{CancelApprovalRecord, CancelApprovalRecordedEvent, DataKey, Error};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

/// The TTL under test, referenced through the contract constant so these tests
/// keep tracking the production value if it ever changes.
const TTL: u64 = crate::CANCEL_APPROVAL_TTL_SECONDS;

/// Ledger timestamp of the first approval in every test below.
const START: u64 = 1_000;

fn approval_record(env: &Env, contract_id: &Address) -> Option<CancelApprovalRecord> {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&DataKey::CancelApproval)
    })
}

fn stored_mask(env: &Env, contract_id: &Address) -> Option<u32> {
    approval_record(env, contract_id).map(|record| record.mask)
}

fn is_cancel_locked(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false)
    })
}

/// Number of `cancel` (lock finalised) events in the most recent contract
/// invocation's event log.
fn lock_event_count(env: &Env) -> u32 {
    let cancel_topic: Val = symbol_short!("cancel").into_val(env);
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == cancel_topic.get_payload() {
                count += 1;
            }
        }
    }
    count
}

/// Number of `cancelrev` (approval revoked) events in the most recent contract
/// invocation's event log.
fn revoke_event_count(env: &Env) -> u32 {
    let revoke_topic: Val = symbol_short!("cancelrev").into_val(env);
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == revoke_topic.get_payload() {
                count += 1;
            }
        }
    }
    count
}

/// Acceptance criterion: an approval inside the TTL still locks the escrow.
#[test]
fn second_approval_within_ttl_locks_the_escrow() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.cancel_escrow(&client_addr);
    assert_eq!(stored_mask(&env, &contract_id), Some(1));

    // One second before the approval expires the counter-party still locks.
    env.ledger().set_timestamp(START + TTL - 1);
    client.cancel_escrow(&freelancer_addr);

    // Read the event log first: `all_event_tuples` reflects the most recent
    // contract invocation, and the `as_contract` storage reads below start a new
    // one.
    assert_eq!(lock_event_count(&env), 1);

    assert!(
        is_cancel_locked(&env, &contract_id),
        "an unexpired approval must still complete the lock"
    );
    assert_eq!(
        stored_mask(&env, &contract_id),
        None,
        "the lock clears the approval record"
    );
}

/// Acceptance criterion: an approval older than the TTL never locks; the
/// second caller starts a fresh approval round instead.
#[test]
fn second_approval_after_ttl_does_not_lock() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Week one: the client approves cancellation during a disagreement.
    client.cancel_escrow(&client_addr);

    // Months later — here exactly at the TTL boundary — the freelancer tries to
    // capitalise on that forgotten approval.
    env.ledger().set_timestamp(START + TTL);
    client.cancel_escrow(&freelancer_addr);

    // No `cancel` event: the call must not finalise the lock.  Read the event
    // log before the `as_contract` storage reads below start a new invocation.
    assert_eq!(lock_event_count(&env), 0);

    assert!(
        !is_cancel_locked(&env, &contract_id),
        "a stale approval must not freeze the escrow"
    );

    let record = approval_record(&env, &contract_id).expect("a fresh approval is recorded");
    assert_eq!(record.mask, 2, "only the fresh approver's bit survives");
    assert_eq!(
        record.approved_at,
        START + TTL,
        "the TTL window restarts at the fresh approval"
    );
    // The stale bit is gone, so the original approver cannot withdraw it.
    assert_eq!(
        client.try_revoke_cancel_approval(&client_addr),
        Err(Ok(Error::InvalidStatus))
    );
}

/// Once a stale approval has been superseded, the escrow can still be cancelled
/// — the round that started after the expiry completes normally.
#[test]
fn fresh_round_started_by_stale_approval_can_still_lock() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.cancel_escrow(&client_addr);
    env.ledger().set_timestamp(START + TTL);
    client.cancel_escrow(&freelancer_addr);

    // The client must opt in again; its expired approval no longer counts.
    env.ledger().set_timestamp(START + TTL + 10);
    client.cancel_escrow(&client_addr);

    assert!(is_cancel_locked(&env, &contract_id));
    assert_eq!(stored_mask(&env, &contract_id), None);
}

/// A party whose own approval expired may re-approve immediately instead of
/// being rejected as a duplicate.
#[test]
fn same_party_reapproval_after_own_ttl_is_allowed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.cancel_escrow(&client_addr);

    // Inside the window the duplicate is still rejected...
    env.ledger().set_timestamp(START + TTL - 1);
    assert_eq!(
        client.try_cancel_escrow(&client_addr),
        Err(Ok(Error::InvalidStatus))
    );

    // ...but past it the approval is gone and the call is a fresh first one.
    env.ledger().set_timestamp(START + TTL + 1);
    client.cancel_escrow(&client_addr);

    let record = approval_record(&env, &contract_id).expect("re-approval is recorded");
    assert_eq!(record.mask, 1);
    assert_eq!(record.approved_at, START + TTL + 1);
}

/// Acceptance criterion for #1026: revoking an approval that has already
/// expired fails with `InvalidStatus`, emits no `cancelrev` event, and clears
/// the stale record rather than leaving it behind.
#[test]
fn revoke_after_ttl_fails_and_emits_nothing() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.cancel_escrow(&client_addr);

    // Well past the TTL the approval is treated as absent everywhere else, so
    // revoking it must fail the same way as revoking a missing approval.
    env.ledger().set_timestamp(START + TTL + 100);
    assert_eq!(
        client.try_revoke_cancel_approval(&client_addr),
        Err(Ok(Error::InvalidStatus))
    );

    // No `cancelrev` event: indexers must not see a revocation of something
    // that, per the other paths, did not exist.  Read the event log before the
    // `as_contract` storage reads below start a new invocation.
    assert_eq!(revoke_event_count(&env), 0);

    // The expired record is removed, not rewritten with a stale `approved_at`.
    assert_eq!(approval_record(&env, &contract_id), None);

    // With the stale record cleared, the counter-party is a first approver.
    client.cancel_escrow(&freelancer_addr);
    assert_eq!(stored_mask(&env, &contract_id), Some(2));
    assert!(!is_cancel_locked(&env, &contract_id));
}

/// `CancelApprovalRecordedEvent` carries `approved_at`, so indexers can tell a
/// live approval from one that will expire — including when a stale approval is
/// superseded and the window restarts.
#[test]
fn cancel_approval_recorded_event_carries_approved_at() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.cancel_escrow(&client_addr);

    let events = crate::all_event_tuples(&env);
    let recorded_topic: Val = symbol_short!("cancelappr").into_val(&env);
    let mut found = false;
    for event in events.iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == recorded_topic.get_payload() {
                let data: CancelApprovalRecordedEvent =
                    event.2.try_into_val(&env).expect("event data decodes");
                assert_eq!(data.approver, client_addr);
                assert_eq!(data.approved_at, START);
                found = true;
            }
        }
    }
    assert!(found, "a cancel approval event is emitted");

    // A superseding approval restarts the window and reports the new timestamp.
    env.ledger().set_timestamp(START + TTL);
    client.cancel_escrow(&freelancer_addr);

    let events = crate::all_event_tuples(&env);
    let mut found = false;
    for event in events.iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == recorded_topic.get_payload() {
                let data: CancelApprovalRecordedEvent =
                    event.2.try_into_val(&env).expect("event data decodes");
                assert_eq!(data.approver, freelancer_addr);
                assert_eq!(data.approved_at, START + TTL);
                found = true;
            }
        }
    }
    assert!(found, "the fresh approval event is emitted");
}
