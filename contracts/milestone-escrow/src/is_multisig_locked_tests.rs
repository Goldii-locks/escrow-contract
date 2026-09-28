#![cfg(test)]
//! `is_multisig_locked` reports the multisig deadlock flag and distinguishes
//! an uninitialized contract (`NotInitialized`) from an initialized escrow
//! that was never locked (`false`).

use crate::test::setup_funded_escrow;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{vec, Env};

#[test]
fn uninitialized_contract_returns_not_initialized() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_is_multisig_locked(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn initialized_escrow_that_was_never_locked_returns_false() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(client.try_is_multisig_locked(), Ok(Ok(false)));
}

#[test]
fn locked_escrow_returns_true() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.multisig_lock(&admin);
    assert_eq!(client.try_is_multisig_locked(), Ok(Ok(true)));
}

#[test]
fn read_mutates_no_ledger_entry_and_publishes_no_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin);

    let before = env.to_ledger_snapshot();
    assert!(client.is_multisig_locked());
    assert_eq!(
        crate::all_event_tuples(&env).len(),
        0,
        "a read publishes no event"
    );
    assert!(client.is_multisig_locked());
    let after = env.to_ledger_snapshot();

    assert_eq!(before, after, "a read mutates no ledger entry");
}
