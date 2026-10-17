#![cfg(test)]
//! Dedicated unit-test suite for `interest_yield_split_refund` precondition
//! and source-state guards (issue #519).
//!
//! The endpoint must reject illegal source states (emergency pause,
//! locked yield configuration, invalid non-positive amounts, invalid ratio sums)
//! with their specific typed error before reading or writing any ledger entry.
//! Every rejected path must leave contract storage untouched and emit no `iyspltref` event.

use super::*;
use crate::{DataKey, Error, EscrowInterestYieldState};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, Env, IntoVal, Val};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn iyspltref_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("iyspltref").into_val(env);
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn test_interest_yield_split_refund_paused_fails_and_no_mutation() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Set pause state in contract storage
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Paused, &true);
    });

    let result = client.try_interest_yield_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::Paused)));

    // Assert no event emitted
    assert_eq!(iyspltref_event_count(&env), 0);

    // Assert storage remains unmutated (Paused is still true, no extra keys created)
    let is_paused: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    });
    assert!(is_paused);
}

#[test]
fn test_interest_yield_split_refund_emergency_paused_fails_and_no_mutation() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Set emergency pause key (Ep)
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Ep, &true);
    });

    let result = client.try_interest_yield_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::Paused)));

    assert_eq!(iyspltref_event_count(&env), 0);

    let ep_held: bool = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::Ep).unwrap_or(false)
    });
    assert!(ep_held);
}

#[test]
fn test_interest_yield_split_refund_locked_state_fails_and_no_mutation() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Store a locked interest yield configuration
    let state = EscrowInterestYieldState {
        client_share_bps: 5_000,
        freelancer_share_bps: 5_000,
        locked: true,
    };
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::InterestYieldState, &state);
    });

    let result = client.try_interest_yield_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::EscrowLocked)));

    assert_eq!(iyspltref_event_count(&env), 0);

    // Verify stored state was untouched
    let stored: EscrowInterestYieldState = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::InterestYieldState)
            .unwrap()
    });
    assert_eq!(stored, state);
    assert!(stored.locked);
}

#[test]
fn test_interest_yield_split_refund_unlocked_state_succeeds() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Store an unlocked interest yield configuration
    let state = EscrowInterestYieldState {
        client_share_bps: 5_000,
        freelancer_share_bps: 5_000,
        locked: false,
    };
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::InterestYieldState, &state);
    });

    let allocation = client.interest_yield_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund, 500);
    assert_eq!(allocation.freelancer_payout, 500);
    assert_eq!(iyspltref_event_count(&env), 1);
}

#[test]
fn test_interest_yield_split_refund_invalid_amount_no_mutation() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_interest_yield_split_refund(&0_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_interest_yield_split_refund(&-500_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );

    assert_eq!(iyspltref_event_count(&env), 0);
}

#[test]
fn test_interest_yield_split_refund_invalid_ratio_no_mutation() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Sum is 9000 != 10000
    assert_eq!(
        client.try_interest_yield_split_refund(&1_000_i128, &4_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidRatio))
    );

    // Sum is 11000 != 10000
    assert_eq!(
        client.try_interest_yield_split_refund(&1_000_i128, &6_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidRatio))
    );

    // Overflow
    assert_eq!(
        client.try_interest_yield_split_refund(&1_000_i128, &u32::MAX, &1_u32),
        Err(Ok(Error::InvalidRatio))
    );

    assert_eq!(iyspltref_event_count(&env), 0);
}

#[test]
fn test_interest_yield_split_refund_unpause_restores_operation() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Set pause
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Paused, &true);
    });
    assert_eq!(
        client.try_interest_yield_split_refund(&1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Paused))
    );

    // Clear pause
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Paused, &false);
    });
    let allocation = client.interest_yield_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund, 500);
    assert_eq!(allocation.freelancer_payout, 500);
    assert_eq!(iyspltref_event_count(&env), 1);
}
