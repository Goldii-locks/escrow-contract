#![cfg(test)]
//! Optional arbiter compensation on dispute resolution (issue #738).

use super::*;
use crate::Error;
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, Address, Env};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

struct Fixture<'a> {
    client_addr: Address,
    freelancer: Address,
    arbiter: Address,
    token: token::Client<'a>,
    escrow: MilestoneEscrowClient<'a>,
}

/// A funded two-milestone escrow (1,000 each) with milestone 0 delivered
/// and disputed.
fn disputed_escrow(env: &Env, arbiter_fee_bps: u32) -> Fixture<'_> {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(env, &token_id).mint(&client_addr, &2_000_i128);

    let escrow = MilestoneEscrowClient::new(env, &env.register(MilestoneEscrow, ()));
    let amounts = vec![env, 1_000_i128, 1_000_i128];
    escrow.initialize_with_arbiter_fee(
        &admin,
        &client_addr,
        &freelancer,
        &arbiter,
        &token_id,
        &604_800u64,
        &amounts,
        &crate::test_spec_hashes(env, &amounts),
        &arbiter_fee_bps,
    );
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(env));

    Fixture {
        client_addr,
        freelancer,
        arbiter,
        token: token::Client::new(env, &token_id),
        escrow,
    }
}

#[test]
fn resolve_dispute_pays_the_fee_and_the_rest_to_the_winner() {
    let env = test_env();
    let f = disputed_escrow(&env, 500);

    f.escrow
        .resolve_dispute(&f.arbiter, &0u32, &true, &decision_hash(&env));

    assert_eq!(f.token.balance(&f.arbiter), 50);
    assert_eq!(f.token.balance(&f.freelancer), 950);
    // Milestone 1's funds are untouched.
    assert_eq!(f.token.balance(&f.escrow.address), 1_000);
    let milestone = f.escrow.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Released);
    assert_eq!(milestone.released_amount, 950);
}

#[test]
fn resolve_dispute_refund_also_pays_the_fee() {
    let env = test_env();
    let f = disputed_escrow(&env, 500);

    f.escrow
        .resolve_dispute(&f.arbiter, &0u32, &false, &decision_hash(&env));

    assert_eq!(f.token.balance(&f.arbiter), 50);
    assert_eq!(f.token.balance(&f.client_addr), 950);
    assert_eq!(f.token.balance(&f.escrow.address), 1_000);
}

#[test]
fn arbitration_split_divides_what_is_left_after_the_fee() {
    let env = test_env();
    let f = disputed_escrow(&env, 500);

    let allocation = f.escrow.apply_dispute_arbitration_split(
        &f.arbiter,
        &0u32,
        &6_000u32,
        &decision_hash(&env),
    );

    assert_eq!(f.token.balance(&f.arbiter), 50);
    assert_eq!(allocation.client_refund, 570);
    assert_eq!(allocation.freelancer_payout, 380);
    assert_eq!(f.token.balance(&f.client_addr), 570);
    assert_eq!(f.token.balance(&f.freelancer), 380);
    assert_eq!(f.token.balance(&f.escrow.address), 1_000);
}

#[test]
fn zero_fee_leaves_resolution_unchanged() {
    let env = test_env();
    let f = disputed_escrow(&env, 0);

    f.escrow
        .resolve_dispute(&f.arbiter, &0u32, &true, &decision_hash(&env));

    assert_eq!(f.token.balance(&f.arbiter), 0);
    assert_eq!(f.token.balance(&f.freelancer), 1_000);
    assert_eq!(f.escrow.get_arbiter_fee_bps(), 0);
}

#[test]
fn milestones_that_never_go_to_dispute_pay_no_fee() {
    let env = test_env();
    let f = disputed_escrow(&env, 1_000);

    f.escrow.mark_delivered(&f.freelancer, &1u32);
    f.escrow.approve_milestone(&f.client_addr, &1u32);

    assert_eq!(f.token.balance(&f.arbiter), 0);
    assert_eq!(f.token.balance(&f.freelancer), 1_000);
}

#[test]
fn fee_above_the_cap_is_rejected() {
    let env = test_env();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let escrow = MilestoneEscrowClient::new(&env, &env.register(MilestoneEscrow, ()));
    let amounts = vec![&env, 1_000_i128];

    assert_eq!(
        escrow.try_initialize_with_arbiter_fee(
            &admin,
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &token_id,
            &604_800u64,
            &amounts,
            &crate::test_spec_hashes(&env, &amounts),
            &1_001u32,
        ),
        Err(Ok(Error::InvalidRatio))
    );
    assert!(escrow.try_get_job().is_err());
}

#[test]
fn fee_is_readable_after_construction() {
    let env = test_env();
    let f = disputed_escrow(&env, 1_000);
    assert_eq!(f.escrow.get_arbiter_fee_bps(), 1_000);
}

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]
    #[test]
    fn property_test_released_milestone_invariant_with_arbiter_fee(
        amount in 100_i128..1_000_000_i128,
        fee_bps in 0_u32..=1_000_u32,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let client_addr = Address::generate(&env);
        let freelancer_addr = Address::generate(&env);
        let arbiter_addr = Address::generate(&env);
        let admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let token_admin = token::StellarAssetClient::new(&env, &sac.address());
        token_admin.mint(&client_addr, &amount);

        let escrow = MilestoneEscrowClient::new(&env, &env.register(MilestoneEscrow, ()));
        let amounts = vec![&env, amount];
        escrow.initialize_with_arbiter_fee(
            &admin,
            &client_addr,
            &freelancer_addr,
            &arbiter_addr,
            &sac.address(),
            &604_800u64,
            &amounts,
            &crate::spec_hashes_for(&amounts),
            &fee_bps,
        );
        escrow.accept_job(&freelancer_addr);
        escrow.accept_arbiter(&arbiter_addr);
        escrow.fund(&client_addr);
        escrow.mark_delivered(&freelancer_addr, &0);
        escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(&env));

        escrow.resolve_dispute(&arbiter_addr, &0, &true, &decision_hash(&env));

        let ms = escrow.get_job().milestones.get(0).unwrap();
        prop_assert_eq!(ms.status, MilestoneStatus::Released);
        let expected_fee = (amount * fee_bps as i128) / 10_000;
        let expected_payout = amount - expected_fee;
        prop_assert_eq!(ms.released_amount, expected_payout);
        prop_assert_eq!(ms.released_amount + expected_fee, ms.amount);
    }
}
