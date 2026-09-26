#![cfg(test)]
//! Tests for `unlock_escrow_interest_yield` structured event (#467)
//!
//! Validates that the emitted event fields reconcile exactly with persisted state
//! and that no event is emitted on the failure path.

use crate::test::setup_funded_escrow;
use crate::{Error, EscrowInterestYieldUnlockedEvent, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Val};
use soroban_sdk::testutils::Address as _;

fn event_count(env: &Env, topic: &str) -> u32 {
    let topic_val: Val = match topic {
        "yldunlock" => symbol_short!("yldunlock").into_val(env),
        _ => panic!("unknown topic"),
    };
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(t) = event.1.get(0) {
            if t.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn unlock_emits_event_and_reconciles_with_persisted_state() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    // set initial yield config
    client.set_escrow_interest_yield(&admin_addr, &3000_u32, &7000_u32);
    client.lock_escrow_interest_yield(&admin_addr);
    assert!(client.is_escrow_interest_yield_locked().unwrap());

    // clear events from setup calls
    // Soroban env events are buffered; we count before unlock and assert delta =1
    let before_count = event_count(&env, "yldunlock");
    client.unlock_escrow_interest_yield(&admin_addr);
    let after_count = event_count(&env, "yldunlock");
    assert_eq!(after_count, before_count + 1, "exactly one yldunlock event should be emitted");

    let events = crate::all_event_tuples(&env);
    let last = events.last().unwrap();
    let ev = EscrowInterestYieldUnlockedEvent::from_val(&env, &last.2);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.client_share_bps, 3000);
    assert_eq!(ev.freelancer_share_bps, 7000);
    assert!(!ev.locked, "event locked should be false after unlock");

    // Reconcile with persisted state
    let state = client.get_escrow_interest_yield().unwrap();
    assert_eq!(state.client_share_bps, ev.client_share_bps);
    assert_eq!(state.freelancer_share_bps, ev.freelancer_share_bps);
    assert_eq!(state.locked, ev.locked);
}

#[test]
fn unlock_failure_emits_no_event_and_does_not_mutate() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin_addr, &4000_u32, &6000_u32);
    // not locked, so unlock should fail
    assert!(!client.is_escrow_interest_yield_locked().unwrap());
    let before_count = event_count(&env, "yldunlock");
    let res = client.try_unlock_escrow_interest_yield(&admin_addr);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));
    let after_count = event_count(&env, "yldunlock");
    assert_eq!(before_count, after_count, "no event should be emitted on failure");

    let state = client.get_escrow_interest_yield().unwrap();
    assert_eq!(state.client_share_bps, 4000);
    assert_eq!(state.freelancer_share_bps, 6000);
    assert!(!state.locked);
}

#[test]
fn unlock_unauthorized_emits_no_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin_addr, &5000_u32, &5000_u32);
    client.lock_escrow_interest_yield(&admin_addr);
    let attacker = Address::generate(&env);
    let before = event_count(&env, "yldunlock");
    let res = client.try_unlock_escrow_interest_yield(&attacker);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    let after = event_count(&env, "yldunlock");
    assert_eq!(before, after);
    assert!(client.is_escrow_interest_yield_locked().unwrap());
}