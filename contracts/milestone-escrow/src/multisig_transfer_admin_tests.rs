//! Dedicated unit-test suite for `multisig_transfer_admin` (issue #327).
//!
//! Covers the full workflow matrix: initialisation, caller authorisation,
//! amount/ratio guards, largest-remainder conservation, overflow, and
//! event emission. Failed calls must not emit `msigtrx`.

use super::*;
use crate::{Error, MultiSigTransferAdminEvent};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

fn sum_allocs(allocations: &soroban_sdk::Vec<i128>) -> i128 {
    let mut total = 0_i128;
    for amount in allocations.iter() {
        total += amount;
    }
    total
}

fn msigtrx_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("msigtrx").into_val(env);
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

fn last_msigtrx_event(env: &Env) -> MultiSigTransferAdminEvent {
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, "msigtrx"));
    MultiSigTransferAdminEvent::from_val(env, &last.2)
}

// ── auth / init ──────────────────────────────────────────────────────────────

#[test]
fn matrix_before_initialize_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
    assert_eq!(msigtrx_event_count(&env), 0);
}

#[test]
fn matrix_attacker_client_freelancer_arbiter_are_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, client_addr, freelancer_addr, arbiter_addr, _, _, client) = setup_multisig_env(&env);
    let attacker = Address::generate(&env);
    let ratios = vec![&env, 1_i128, 1_i128];

    for caller in [attacker, client_addr, freelancer_addr, arbiter_addr] {
        let result = client.try_multisig_transfer_admin(&caller, &100_i128, &ratios);
        assert_eq!(result, Err(Ok(Error::Unauthorized)));
    }
    assert_eq!(msigtrx_event_count(&env), 0);
}

// ── amount guards ────────────────────────────────────────────────────────────

#[test]
fn matrix_zero_and_negative_total_return_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let ratios = vec![&env, 1_i128, 1_i128];

    for amount in [0_i128, -1_i128, i128::MIN] {
        let result = client.try_multisig_transfer_admin(&admin_addr, &amount, &ratios);
        assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    }
    assert_eq!(msigtrx_event_count(&env), 0);
}

// ── ratio guards ─────────────────────────────────────────────────────────────

#[test]
fn matrix_empty_ratios_return_invalid_ratio() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let ratios = soroban_sdk::Vec::new(&env);
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(msigtrx_event_count(&env), 0);
}

#[test]
fn matrix_too_many_ratios_return_invalid_amount_capacity_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let mut over_cap = vec![&env];
    for _ in 0..256u32 {
        over_cap.push_back(1_i128);
    }
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &over_cap);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(msigtrx_event_count(&env), 0);

    let mut at_cap = vec![&env];
    for _ in 0..255u32 {
        at_cap.push_back(1_i128);
    }
    let allocs = client.multisig_transfer_admin(&admin_addr, &255_i128, &at_cap);
    assert_eq!(allocs.len(), 255);
    assert_eq!(sum_allocs(&allocs), 255);
    assert_eq!(msigtrx_event_count(&env), 1);
}

#[test]
fn matrix_negative_ratio_and_sum_overflow_and_all_zeros_fail() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    assert_eq!(
        client.try_multisig_transfer_admin(&admin_addr, &100_i128, &vec![&env, 1_i128, -1_i128]),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_multisig_transfer_admin(&admin_addr, &100_i128, &vec![&env, -5_i128, 1_i128]),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_multisig_transfer_admin(&admin_addr, &100_i128, &vec![&env, i128::MAX, 1_i128]),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_multisig_transfer_admin(
            &admin_addr,
            &100_i128,
            &vec![&env, 0_i128, 0_i128, 0_i128]
        ),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(msigtrx_event_count(&env), 0);
}

#[test]
fn matrix_weighted_mul_overflow_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let result =
        client.try_multisig_transfer_admin(&admin_addr, &i128::MAX, &vec![&env, i128::MAX]);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(msigtrx_event_count(&env), 0);
}

// ── conservation / largest-remainder ─────────────────────────────────────────

#[test]
fn matrix_single_party_receives_entire_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let allocations = client.multisig_transfer_admin(&admin_addr, &42_i128, &vec![&env, 1_i128]);
    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations.get(0).unwrap(), 42);
    assert_eq!(sum_allocs(&allocations), 42);
}

#[test]
fn matrix_three_equal_ratios_use_largest_remainder() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let allocations =
        client.multisig_transfer_admin(&admin_addr, &100_i128, &vec![&env, 1_i128, 1_i128, 1_i128]);
    assert_eq!(allocations.get(0).unwrap(), 34);
    assert_eq!(allocations.get(1).unwrap(), 33);
    assert_eq!(allocations.get(2).unwrap(), 33);
    assert_eq!(sum_allocs(&allocations), 100);
}

#[test]
fn matrix_equal_two_party_odd_amount_differs_by_at_most_one() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let allocations =
        client.multisig_transfer_admin(&admin_addr, &101_i128, &vec![&env, 1_i128, 1_i128]);
    let a = allocations.get(0).unwrap();
    let b = allocations.get(1).unwrap();
    assert_eq!(a + b, 101);
    assert!((a - b).abs() <= 1);
}

#[test]
fn matrix_disparate_and_sparse_zero_ratios_preserve_total() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let disparate =
        client.multisig_transfer_admin(&admin_addr, &100_i128, &vec![&env, 7_i128, 1_i128, 2_i128]);
    assert_eq!(sum_allocs(&disparate), 100);
    assert_eq!(disparate.get(0).unwrap(), 70);
    assert_eq!(disparate.get(1).unwrap(), 10);
    assert_eq!(disparate.get(2).unwrap(), 20);

    let sparse = client.multisig_transfer_admin(
        &admin_addr,
        &100_i128,
        &vec![&env, 0_i128, 1_i128, 0_i128, 3_i128],
    );
    assert_eq!(sparse.len(), 4);
    assert_eq!(sparse.get(0).unwrap(), 0);
    assert_eq!(sparse.get(2).unwrap(), 0);
    assert_eq!(sum_allocs(&sparse), 100);
}

#[test]
fn matrix_one_unit_and_high_precision_preserve_every_stroop() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let unit = client.multisig_transfer_admin(&admin_addr, &1_i128, &vec![&env, 5_i128, 5_i128]);
    assert_eq!(sum_allocs(&unit), 1);

    let amount = 1_000_000_000_000_000_001_i128;
    let ratios = vec![&env, 2_i128, 3_i128, 5_i128];
    let high = client.multisig_transfer_admin(&admin_addr, &amount, &ratios);
    assert_eq!(sum_allocs(&high), amount);
    let ratio_sum = 10_i128;
    for i in 0..ratios.len() {
        let floor = (amount * ratios.get(i).unwrap()) / ratio_sum;
        let got = high.get(i).unwrap();
        assert!(got == floor || got == floor + 1);
    }
}

// ── events ───────────────────────────────────────────────────────────────────

#[test]
fn matrix_success_emits_event_with_matching_payload() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let allocations =
        client.multisig_transfer_admin(&admin_addr, &1_000_i128, &vec![&env, 3_i128, 1_i128]);

    assert_eq!(allocations.get(0).unwrap(), 750);
    assert_eq!(allocations.get(1).unwrap(), 250);
    assert_eq!(msigtrx_event_count(&env), 1);

    let event = last_msigtrx_event(&env);
    assert_eq!(event.total_amount, 1_000);
    assert_eq!(event.num_parties, 2);
    assert_eq!(event.allocations, allocations);
}

#[test]
fn matrix_single_party_success_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let allocations = client.multisig_transfer_admin(&admin_addr, &500_i128, &vec![&env, 1_i128]);
    assert_eq!(allocations.get(0).unwrap(), 500);

    let event = last_msigtrx_event(&env);
    assert_eq!(event.total_amount, 500);
    assert_eq!(event.num_parties, 1);
    assert_eq!(event.allocations, allocations);
}

#[test]
fn matrix_rejected_call_emits_no_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let result =
        client.try_multisig_transfer_admin(&admin_addr, &0_i128, &vec![&env, 1_i128, 1_i128]);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(msigtrx_event_count(&env), 0);
}
