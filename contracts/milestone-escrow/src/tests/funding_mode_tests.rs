#![cfg(test)]
use super::*;
use soroban_sdk::{token, vec, Address, Env};

pub(crate) fn setup_unfunded(
    env: &Env,
    amounts: soroban_sdk::Vec<i128>,
    mode: FundingMode,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_contract_id);
    let total: i128 = amounts.iter().sum();
    token_admin.mint(&client_addr, &total);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    client.initialize_with_funding_mode(
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
    client.accept_job(&freelancer_addr);
    client.accept_arbiter(&arbiter_addr);
    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        admin_addr,
        token_contract_id,
        client,
    )
}

#[test]
fn upfront_compatibility() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token, escrow) =
        setup_unfunded(&env, vec![&env, 100_i128, 200_i128], FundingMode::Upfront);
    assert_eq!(escrow.get_funding_mode(), FundingMode::Upfront);
    escrow.fund(&client_addr);
    let job = escrow.get_job();
    assert!(job.funded);
    assert!(job.milestones.get(0).unwrap().funded);
    assert!(job.milestones.get(1).unwrap().funded);
    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&escrow.address), 300);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);
    assert_eq!(
        escrow.try_fund_milestone(&client_addr, &0u32),
        Err(Ok(Error::InvalidFundingMode))
    );
}

#[test]
fn per_milestone_funding_transfers_only_that_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, _, token, escrow) = setup_unfunded(
        &env,
        vec![&env, 100_i128, 250_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0u32);
    let job = escrow.get_job();
    assert!(!job.funded);
    assert!(job.milestones.get(0).unwrap().funded);
    assert!(!job.milestones.get(1).unwrap().funded);
    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&escrow.address), 100);
}

#[test]
fn unfunded_milestone_cannot_be_delivered() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, escrow) = setup_unfunded(
        &env,
        vec![&env, 100_i128, 200_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0u32);
    assert_eq!(
        escrow.try_mark_delivered(&freelancer_addr, &1u32),
        Err(Ok(Error::NotFunded))
    );
}

#[test]
fn unfunded_milestone_cannot_be_settled() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, _, _, escrow) = setup_unfunded(
        &env,
        vec![&env, 100_i128, 200_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0u32);
    assert_eq!(
        escrow.try_approve_milestone(&client_addr, &1u32),
        Err(Ok(Error::NotFunded))
    );
    assert_eq!(
        escrow.try_approve_partial(&client_addr, &1u32, &10_i128),
        Err(Ok(Error::NotFunded))
    );
    assert_eq!(
        escrow.try_raise_dispute(&client_addr, &1u32, &dispute_reason_hash(&env)),
        Err(Ok(Error::NotFunded))
    );
    assert_eq!(
        escrow.try_resolve_dispute(&arbiter_addr, &1u32, &true, &decision_hash(&env)),
        Err(Ok(Error::NotFunded))
    );
    assert_eq!(
        escrow.try_claim_auto_release(&freelancer_addr, &1u32),
        Err(Ok(Error::NotFunded))
    );
}

#[test]
fn duplicate_funding_prevention() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, _, _, escrow) =
        setup_unfunded(&env, vec![&env, 100_i128], FundingMode::PerMilestone);
    escrow.fund_milestone(&client_addr, &0u32);
    assert_eq!(
        escrow.try_fund_milestone(&client_addr, &0u32),
        Err(Ok(Error::AlreadyFunded))
    );
    assert_eq!(
        escrow.try_fund(&client_addr),
        Err(Ok(Error::InvalidFundingMode))
    );
}

#[test]
fn partial_funding_independence() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token, escrow) = setup_unfunded(
        &env,
        vec![&env, 100_i128, 200_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0u32);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);
    let job = escrow.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
    assert!(!job.milestones.get(1).unwrap().funded);
    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&escrow.address), 0);
}

#[test]
fn conservation_invariant() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, _, token, escrow) = setup_unfunded(
        &env,
        vec![&env, 100_i128, 50_i128],
        FundingMode::PerMilestone,
    );
    escrow.fund_milestone(&client_addr, &0u32);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_partial(&client_addr, &0u32, &40_i128);
    escrow.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(&env));
    escrow.resolve_dispute(&arbiter_addr, &0u32, &false, &decision_hash(&env));
    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&escrow.address), 0);
    let job = escrow.get_job();
    let ms0 = job.milestones.get(0).unwrap();
    assert!(ms0.released_amount <= ms0.amount);
}

#[test]
fn failed_transfer_atomicity() {
    let env = Env::default();
    env.mock_all_auths();
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env.register(super::mock_token::MockToken, ());
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    escrow.initialize_with_funding_mode(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &vec![&env, 100_i128],
        &crate::spec_hashes_for(&vec![&env, 100_i128]),
        &FundingMode::PerMilestone,
    );
    assert_eq!(
        escrow.try_fund_milestone(&client_addr, &0u32),
        Err(Ok(Error::InsufficientBalance))
    );
    assert!(!escrow.get_job().milestones.get(0).unwrap().funded);
}

#[test]
fn authorization_and_bounds() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, escrow) =
        setup_unfunded(&env, vec![&env, 100_i128], FundingMode::PerMilestone);
    assert_eq!(
        escrow.try_fund_milestone(&freelancer_addr, &0u32),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        escrow.try_fund_milestone(&client_addr, &9u32),
        Err(Ok(Error::InvalidMilestone))
    );
}

#[test]
fn default_initialize_is_upfront() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 10_i128]);
    assert_eq!(escrow.get_funding_mode(), FundingMode::Upfront);
}
