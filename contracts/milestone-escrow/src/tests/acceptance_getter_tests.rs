#![cfg(test)]

//! Issue #971: read-only getters for acceptance state, the acceptance
//! deadline and the dispute-escalation timeout.
//!
//! The no-mutation checks take a full ledger snapshot around each call and
//! assert no event was published, following the established getter suites.

use super::*;
use crate::DataKey;
use soroban_sdk::Env;

const ACCEPTANCE_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;

fn set_acceptance_mask(env: &Env, contract_id: &Address, mask: u32) {
    env.as_contract(contract_id, || {
        env.storage().instance().set(&DataKey::Acceptance, &mask);
    });
}

fn set_dispute_timeout(env: &Env, contract_id: &Address, timeout: u64) {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::DisputeTimeout, &timeout);
    });
}

fn assert_pure<T>(env: &Env, call: impl FnOnce() -> T) -> T {
    let before = env.to_ledger_snapshot();
    let value = call();
    assert_eq!(
        crate::all_event_tuples(env).len(),
        0,
        "getter published an event"
    );
    let after = env.to_ledger_snapshot();
    assert_eq!(before, after, "getter mutated the ledger");
    value
}

#[test]
fn acceptance_getter_reports_unfunded_unaccepted_state() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_unfunded_escrow(&env, vec![&env, 1_000_i128]);
    set_acceptance_mask(&env, &contract_id, 0);

    assert_eq!(
        assert_pure(&env, || escrow.get_acceptance()),
        (false, false)
    );
    assert_eq!(
        assert_pure(&env, || escrow.get_acceptance_deadline()),
        None,
        "an unfunded escrow has no acceptance deadline"
    );
}

#[test]
fn acceptance_getter_reports_partially_accepted_states() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    set_acceptance_mask(&env, &contract_id, 1);
    assert_eq!(assert_pure(&env, || escrow.get_acceptance()), (true, false));

    set_acceptance_mask(&env, &contract_id, 2);
    assert_eq!(assert_pure(&env, || escrow.get_acceptance()), (false, true));
}

#[test]
fn acceptance_getter_reports_fully_accepted_state() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    set_acceptance_mask(&env, &contract_id, 3);

    assert_eq!(assert_pure(&env, || escrow.get_acceptance()), (true, true));
}

#[test]
fn acceptance_deadline_matches_withdraw_unaccepted_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let funded_at: u64 = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::FundedAt).unwrap()
    });
    assert_eq!(
        assert_pure(&env, || escrow.get_acceptance_deadline()),
        Some(funded_at + ACCEPTANCE_WINDOW_SECONDS)
    );
}

#[test]
fn dispute_timeout_getter_returns_default_and_configured_values() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        assert_pure(&env, || escrow.get_dispute_timeout()),
        crate::DISPUTE_TIMEOUT_SECONDS
    );

    set_dispute_timeout(&env, &contract_id, 123_456);
    assert_eq!(assert_pure(&env, || escrow.get_dispute_timeout()), 123_456);
}

#[test]
fn getters_do_not_mutate_ledger_across_repeated_calls() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    set_acceptance_mask(&env, &contract_id, 3);
    set_dispute_timeout(&env, &contract_id, 86_400);
    env.ledger().with_mut(|li| li.sequence_number += 10);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        escrow.get_acceptance();
        escrow.get_acceptance_deadline();
        escrow.get_dispute_timeout();
    }
    let after = env.to_ledger_snapshot();

    assert_eq!(before, after, "repeated getter calls must not extend TTLs");
}

#[test]
fn getters_on_uninitialized_contract_return_defaults_without_mutation() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    assert_eq!(escrow.get_acceptance(), (false, false));
    assert_eq!(escrow.get_acceptance_deadline(), None);
    assert_eq!(escrow.get_dispute_timeout(), crate::DISPUTE_TIMEOUT_SECONDS);
    assert_eq!(crate::all_event_tuples(&env).len(), 0);
    let after = env.to_ledger_snapshot();

    assert_eq!(before, after, "uninitialized getters mutated the ledger");
}
