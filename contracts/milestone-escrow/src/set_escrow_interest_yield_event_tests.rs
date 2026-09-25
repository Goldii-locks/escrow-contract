#![cfg(test)]
//! Tests for `set_escrow_interest_yield` structured event (#462)

use crate::test::setup_funded_escrow;
use crate::{Error, EscrowInterestYieldSetEvent, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Val};

fn event_count(env: &Env) -> u32 {
    let topic: Val = symbol_short!("yldset").into_val(env);
    let mut count = 0u32;
    for e in crate::all_event_tuples(env).iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn set_yield_emits_event_and_reconciles() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let before = event_count(&env);
    client.set_escrow_interest_yield(&admin_addr, &3500_u32, &6500_u32);
    let after = event_count(&env);
    assert_eq!(after, before + 1);
    let events = crate::all_event_tuples(&env);
    let ev = EscrowInterestYieldSetEvent::from_val(&env, &events.last().unwrap().2);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.client_share_bps, 3500);
    assert_eq!(ev.freelancer_share_bps, 6500);
    assert!(!ev.locked);
    let state = client.get_escrow_interest_yield().unwrap();
    assert_eq!(state.client_share_bps, ev.client_share_bps);
    assert_eq!(state.freelancer_share_bps, ev.freelancer_share_bps);
    assert_eq!(state.locked, ev.locked);
}

#[test]
fn set_yield_failure_emits_no_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin_addr, &4000_u32, &6000_u32);
    client.lock_escrow_interest_yield(&admin_addr);
    let before = event_count(&env);
    // attempt to set while locked should fail with EscrowLocked
    let res = client.try_set_escrow_interest_yield(&admin_addr, &3000_u32, &7000_u32);
    assert_eq!(res, Err(Ok(Error::EscrowLocked)));
    let after = event_count(&env);
    assert_eq!(before, after, "no event on failure");
    let state = client.get_escrow_interest_yield().unwrap();
    assert_eq!(state.client_share_bps, 4000);
    assert_eq!(state.freelancer_share_bps, 6000);
}

#[test]
fn set_yield_unauthorized_no_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin_addr, &5000_u32, &5000_u32);
    let attacker = Address::generate(&env);
    let before = event_count(&env);
    let res = client.try_set_escrow_interest_yield(&attacker, &3000_u32, &7000_u32);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    let after = event_count(&env);
    assert_eq!(before, after);
}

#[test]
fn set_yield_invalid_ratio_no_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let before = event_count(&env);
    let res = client.try_set_escrow_interest_yield(&admin_addr, &6000_u32, &3000_u32);
    assert_eq!(res, Err(Ok(Error::InvalidRatio)));
    let after = event_count(&env);
    assert_eq!(before, after);
}