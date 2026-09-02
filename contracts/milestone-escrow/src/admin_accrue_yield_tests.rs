#![cfg(test)]
//! Unit tests for `admin_accrue_yield`.
//!
//! The endpoint credits an off-chain-computed yield amount to the on-chain
//! `YieldAccrued` accumulator. It had no coverage at all, which matters here
//! because the running total it maintains is what later settlement reads: a
//! guard that silently stopped rejecting bad input would inflate payouts.

use super::*;
use crate::test::setup_funded_escrow;
use crate::{Error, YieldAccruedEvent};
use soroban_sdk::testutils::{Address as _, Events as _};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Val};

/// Running `YieldAccrued` total, via the public getter.
fn accrued(client: &MilestoneEscrowClient<'_>) -> i128 {
    client.get_yield_info().1
}

fn yldacc_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("yldacc").into_val(env);
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
fn test_admin_accrue_yield_happy_path() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(accrued(&client), 0);

    client.admin_accrue_yield(&admin_addr, &0u32, &250_i128);

    // Tally the event immediately: env.events() only reflects the most recent
    // invocation, so calling get_yield_info first would clear it.
    assert_eq!(yldacc_event_count(&env), 1);
    let events = env.events().all();
    let ev = YieldAccruedEvent::from_val(&env, &events.last().unwrap().2);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.milestone_index, 0);
    assert_eq!(ev.accrued_amount, 250);
    assert_eq!(ev.total_accrued, 250);

    assert_eq!(accrued(&client), 250);
}

#[test]
fn test_admin_accrue_yield_accumulates_across_calls() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);

    client.admin_accrue_yield(&admin_addr, &0u32, &100_i128);
    client.admin_accrue_yield(&admin_addr, &1u32, &50_i128);

    // The accumulator is contract-wide, not per milestone.
    assert_eq!(accrued(&client), 150);
}

#[test]
fn test_admin_accrue_yield_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_accrue_yield(&client_addr, &0u32, &100_i128),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        client.try_admin_accrue_yield(&freelancer_addr, &0u32, &100_i128),
        Err(Ok(Error::Unauthorized))
    );

    assert_eq!(accrued(&client), 0);
}

#[test]
fn test_admin_accrue_yield_invalid_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Only index 0 exists.
    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &1u32, &100_i128),
        Err(Ok(Error::InvalidMilestone))
    );
    assert_eq!(accrued(&client), 0);
}

#[test]
fn test_admin_accrue_yield_rejects_non_positive_amounts() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &0_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &-1_i128),
        Err(Ok(Error::InvalidAmount))
    );

    // A rejected accrual must not move the total.
    assert_eq!(accrued(&client), 0);
}

#[test]
fn test_admin_accrue_yield_overflow_is_rejected_not_wrapped() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.admin_accrue_yield(&admin_addr, &0u32, &i128::MAX);
    assert_eq!(accrued(&client), i128::MAX);

    // The checked_add must surface as an error rather than wrapping the
    // accumulator around to a negative total.
    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &1_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(accrued(&client), i128::MAX);
}

#[test]
fn test_admin_accrue_yield_uninitialized_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    assert_eq!(
        client.try_admin_accrue_yield(&caller, &0u32, &100_i128),
        Err(Ok(Error::NotInitialized))
    );
}
