#![cfg(test)]
//! `is_escrow_interest_yield_locked` reports whether the interest/yield share
//! configuration is frozen, returning `NotInitialized` until a configuration
//! has been written.

use crate::test::setup_funded_escrow;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{vec, Env};

#[test]
fn uninitialized_contract_returns_not_initialized() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_is_escrow_interest_yield_locked(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn missing_configuration_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_is_escrow_interest_yield_locked(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn reports_the_lock_flag_through_lock_and_unlock() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_escrow_interest_yield(&admin, &5_000, &5_000);
    assert!(!client.is_escrow_interest_yield_locked());

    client.lock_escrow_interest_yield(&admin);
    assert!(client.is_escrow_interest_yield_locked());

    client.unlock_escrow_interest_yield(&admin);
    assert!(!client.is_escrow_interest_yield_locked());
}

#[test]
fn read_mutates_no_ledger_entry_and_publishes_no_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin, &5_000, &5_000);
    client.lock_escrow_interest_yield(&admin);

    let before = env.to_ledger_snapshot();
    assert!(client.is_escrow_interest_yield_locked());
    assert_eq!(
        crate::all_event_tuples(&env).len(),
        0,
        "a read publishes no event"
    );
    assert!(client.is_escrow_interest_yield_locked());
    let after = env.to_ledger_snapshot();

    assert_eq!(before, after, "a read mutates no ledger entry");
}
