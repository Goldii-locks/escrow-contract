#![cfg(test)]
//! Unit tests for `set_interest_yield_consent`.
//!
//! This is the dual-consent path for configuring how interest/yield is split
//! between client and freelancer: the admin submits, but both parties must
//! have signed the same transaction. It had no coverage on main.

use super::*;
use crate::test::setup_funded_escrow;
use crate::{Error, EscrowInterestYieldConsentSetEvent};
use soroban_sdk::testutils::{Address as _, Events as _};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Val};

fn yldcons_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("yldcons").into_val(env);
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

#[test]
fn test_escrow_interest_yield_consent_both_sign_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_interest_yield_consent(&admin_addr, &6_000u32, &4_000u32);

    // Tally immediately -- a later client call would reset the event buffer.
    assert_eq!(yldcons_event_count(&env), 1);
    let events = env.events().all();
    let ev = EscrowInterestYieldConsentSetEvent::from_val(&env, &events.last().unwrap().2);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.client, client_addr);
    assert_eq!(ev.freelancer, freelancer_addr);
    assert_eq!(ev.client_share_bps, 6_000);
    assert_eq!(ev.freelancer_share_bps, 4_000);

    let state = client.get_escrow_interest_yield();
    assert_eq!(state.client_share_bps, 6_000);
    assert_eq!(state.freelancer_share_bps, 4_000);
    assert!(!state.locked, "consent must leave the config unlocked");
}

#[test]
fn test_escrow_interest_yield_consent_invalid_ratio_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Shares must sum to exactly BPS_SCALE (10_000).
    assert_eq!(
        client.try_set_interest_yield_consent(&admin_addr, &6_000u32, &3_000u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_set_interest_yield_consent(&admin_addr, &6_000u32, &5_000u32),
        Err(Ok(Error::InvalidRatio))
    );

    // Nothing was stored by the rejected calls.
    assert!(client.try_get_escrow_interest_yield().is_err());
}

#[test]
fn test_escrow_interest_yield_consent_boundary_shares_succeed() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // All-to-one-party is a valid ratio; only the sum is constrained.
    client.set_interest_yield_consent(&admin_addr, &10_000u32, &0u32);
    let state = client.get_escrow_interest_yield();
    assert_eq!(state.client_share_bps, 10_000);
    assert_eq!(state.freelancer_share_bps, 0);
}

#[test]
fn test_escrow_interest_yield_consent_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for caller in [&client_addr, &freelancer_addr, &arbiter_addr] {
        assert_eq!(
            client.try_set_interest_yield_consent(caller, &5_000u32, &5_000u32),
            Err(Ok(Error::Unauthorized)),
        );
    }
}

#[test]
fn test_escrow_interest_yield_consent_respects_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_interest_yield_consent(&admin_addr, &5_000u32, &5_000u32);
    client.lock_escrow_interest_yield(&admin_addr);

    // A locked config must not be replaceable, even with both signatures.
    assert_eq!(
        client.try_set_interest_yield_consent(&admin_addr, &7_000u32, &3_000u32),
        Err(Ok(Error::EscrowLocked))
    );

    let state = client.get_escrow_interest_yield();
    assert_eq!(state.client_share_bps, 5_000);
    assert_eq!(state.freelancer_share_bps, 5_000);
}

#[test]
fn test_escrow_interest_yield_consent_unlock_allows_update() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_interest_yield_consent(&admin_addr, &5_000u32, &5_000u32);
    client.lock_escrow_interest_yield(&admin_addr);
    client.unlock_escrow_interest_yield(&admin_addr);

    client.set_interest_yield_consent(&admin_addr, &2_500u32, &7_500u32);
    let state = client.get_escrow_interest_yield();
    assert_eq!(state.client_share_bps, 2_500);
    assert_eq!(state.freelancer_share_bps, 7_500);
}

#[test]
fn test_escrow_interest_yield_consent_uninitialized_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    assert_eq!(
        client.try_set_interest_yield_consent(&caller, &5_000u32, &5_000u32),
        Err(Ok(Error::NotInitialized))
    );
}
