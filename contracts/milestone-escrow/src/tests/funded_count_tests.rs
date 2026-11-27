#![cfg(test)]
//! `DataKey::FundedCount` (#999): `fund_milestone` decides whether the job is
//! fully funded by comparing a counter with `milestone_count` instead of
//! rescanning every milestone.

use super::*;
use soroban_sdk::{token, vec, Address, Env};

fn setup(
    env: &Env,
    amounts: soroban_sdk::Vec<i128>,
    mode: FundingMode,
) -> (Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let total: i128 = amounts.iter().sum();
    token::StellarAssetClient::new(env, &token_contract_id).mint(&client_addr, &(total + 1_000));

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize_with_funding_mode(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        &mode,
    );
    escrow.accept_job(&freelancer_addr);
    escrow.accept_arbiter(&arbiter_addr);
    (client_addr, freelancer_addr, escrow)
}

fn funded_count(env: &Env, escrow: &MilestoneEscrowClient<'_>) -> Option<u32> {
    env.as_contract(&escrow.address, || {
        env.storage()
            .instance()
            .get::<_, u32>(&crate::DataKey::FundedCount)
    })
}

#[test]
fn counter_starts_at_zero_and_tracks_each_milestone() {
    let env = Env::default();
    let (client_addr, _, escrow) = setup(
        &env,
        vec![&env, 100_i128, 200_i128, 300_i128],
        FundingMode::PerMilestone,
    );
    assert_eq!(funded_count(&env, &escrow), Some(0));

    escrow.fund_milestone(&client_addr, &2);
    assert_eq!(funded_count(&env, &escrow), Some(1));
    assert!(!escrow.get_job().funded);

    escrow.fund_milestone(&client_addr, &0);
    assert_eq!(funded_count(&env, &escrow), Some(2));
    assert!(!escrow.get_job().funded);

    // The last milestone completes the job, whatever order they arrive in.
    escrow.fund_milestone(&client_addr, &1);
    assert_eq!(funded_count(&env, &escrow), Some(3));
    assert!(escrow.get_job().funded);
}

#[test]
fn funding_a_milestone_twice_does_not_advance_the_counter() {
    let env = Env::default();
    let (client_addr, _, escrow) = setup(
        &env,
        vec![&env, 100_i128, 200_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0);
    assert_eq!(
        escrow.try_fund_milestone(&client_addr, &0),
        Err(Ok(Error::AlreadyFunded))
    );
    assert_eq!(funded_count(&env, &escrow), Some(1));
    assert!(!escrow.get_job().funded);
}

#[test]
fn upfront_fund_sets_the_counter_to_the_milestone_count() {
    let env = Env::default();
    let (client_addr, _, escrow) =
        setup(&env, vec![&env, 100_i128, 200_i128], FundingMode::Upfront);
    assert_eq!(funded_count(&env, &escrow), Some(0));
    escrow.fund(&client_addr);
    assert_eq!(funded_count(&env, &escrow), Some(2));
}

#[test]
fn add_milestone_keeps_the_counter_equal_to_the_count() {
    let env = Env::default();
    let (client_addr, freelancer_addr, escrow) = setup(
        &env,
        vec![&env, 100_i128, 200_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0);
    escrow.fund_milestone(&client_addr, &1);
    assert_eq!(funded_count(&env, &escrow), Some(2));

    escrow.add_milestone(&client_addr, &freelancer_addr, &300);
    assert_eq!(funded_count(&env, &escrow), Some(3));
    assert_eq!(escrow.get_job().milestones.len(), 3);

    // Reducing a milestone refunds the difference but it stays funded.
    escrow.reduce_milestone(&client_addr, &freelancer_addr, &2, &250);
    assert_eq!(funded_count(&env, &escrow), Some(3));
}

#[test]
fn escrow_without_a_counter_falls_back_to_counting_once() {
    let env = Env::default();
    let (client_addr, _, escrow) = setup(
        &env,
        vec![&env, 100_i128, 200_i128, 300_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0);
    escrow.fund_milestone(&client_addr, &1);

    // An escrow created before the counter existed has no entry.
    env.as_contract(&escrow.address, || {
        env.storage()
            .instance()
            .remove(&crate::DataKey::FundedCount);
    });
    assert_eq!(funded_count(&env, &escrow), None);

    escrow.fund_milestone(&client_addr, &2);
    assert_eq!(funded_count(&env, &escrow), Some(3));
    assert!(escrow.get_job().funded);
}
