#![cfg(test)]
use super::*;
use soroban_sdk::{
    testutils::Address as _, testutils::Events, testutils::Ledger, vec, Address, Env, TryIntoVal,
};

fn build_milestone_amounts(env: &Env, count: u32, amount_per: i128) -> soroban_sdk::Vec<i128> {
    let mut amounts = vec![env];
    for _ in 0..count {
        amounts.push_back(amount_per);
    }
    amounts
}

fn setup_funded_escrow(
    env: &Env,
    milestone_amounts: soroban_sdk::Vec<i128>,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    soroban_sdk::Address,
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
    let total: i128 = milestone_amounts.iter().sum();
    token_admin.mint(&client_addr, &total);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &milestone_amounts,
    );
    client.fund(&client_addr);

    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        admin_addr,
        token_contract_id,
        contract_id,
        client,
    )
}

#[test]
fn test_full_happy_path() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 3_000_i128, 7_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    assert_eq!(token.balance(&client_addr), 10_000);

    client.fund(&client_addr);
    assert_eq!(token.balance(&client_addr), 0);
    assert_eq!(token.balance(&contract_id), 10_000);

    client.mark_delivered(&freelancer_addr, &0u32);

    client.approve_milestone(&client_addr, &0u32);
    assert_eq!(token.balance(&freelancer_addr), 3_000);
    assert_eq!(token.balance(&contract_id), 7_000);

    client.mark_delivered(&freelancer_addr, &1u32);
    client.approve_milestone(&client_addr, &1u32);
    assert_eq!(token.balance(&freelancer_addr), 10_000);
    assert_eq!(token.balance(&contract_id), 0);
}

#[test]
fn test_dispute_release_to_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &true);

    assert_eq!(token.balance(&freelancer_addr), 5_000);
}

#[test]
fn test_dispute_refund_to_client() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &false);

    assert_eq!(token.balance(&client_addr), 5_000);
}

#[test]
fn test_double_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert!(result.is_err());
}

#[test]
fn test_unauthorized_fund_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_fund(&bad_actor);
    assert!(result.is_err());
}

#[test]
fn test_invalid_milestone_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_mark_delivered(&freelancer_addr, &1u32);
    assert!(result.is_err());
}

#[test]
fn test_mark_delivered_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_approve_milestone_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_approve_milestone(&client_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_raise_dispute_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_raise_dispute(&bad_actor, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_raise_dispute_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let result = client.try_raise_dispute(&client_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_resolve_dispute_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.raise_dispute(&client_addr, &0u32);

    let result = client.try_resolve_dispute(&bad_actor, &0u32, &true);
    assert!(result.is_err());
}

#[test]
fn test_resolve_dispute_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert!(result.is_err());
}

#[test]
fn test_fund_before_initialized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_fund(&client_addr);
    assert!(result.is_err());
}

#[test]
fn test_double_fund_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &2_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_fund(&client_addr);
    assert!(result.is_err());
}

#[test]
fn test_mark_delivered_before_funded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_admin_add_token() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    assert!(client.is_token_whitelisted(&token1));
    assert!(!client.is_token_whitelisted(&token2));

    client.add_whitelisted_token(&admin_addr, &token2);
    assert!(client.is_token_whitelisted(&token2));

    let whitelist = client.get_whitelisted_tokens();
    assert_eq!(whitelist.len(), 2);
}

#[test]
fn test_non_admin_add_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let result = client.try_add_whitelisted_token(&bad_actor, &token2);
    assert!(result.is_err());
}

#[test]
fn test_admin_remove_token() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );
    client.add_whitelisted_token(&admin_addr, &token2);

    assert!(client.is_token_whitelisted(&token2));

    client.remove_whitelisted_token(&admin_addr, &token2);
    assert!(!client.is_token_whitelisted(&token2));
}

#[test]
fn test_non_admin_remove_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );
    client.add_whitelisted_token(&admin_addr, &token2);

    let result = client.try_remove_whitelisted_token(&bad_actor, &token2);
    assert!(result.is_err());
}

#[test]
fn test_add_existing_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let result = client.try_add_whitelisted_token(&admin_addr, &token1);
    assert!(result.is_err());
}

#[test]
fn test_remove_nonexistent_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let result = client.try_remove_whitelisted_token(&admin_addr, &token2);
    assert!(result.is_err());
}

#[test]
fn test_partial_release_remaining_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &4_000_i128);

    assert_eq!(token.balance(&freelancer_addr), 4_000);
    assert_eq!(token.balance(&contract_id), 6_000);

    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    assert_eq!(milestone.released_amount, 4_000);
    assert_eq!(milestone.status, MilestoneStatus::PartiallyReleased);
}

#[test]
fn test_multiple_partial_releases_sum_full() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &3_000_i128);
    client.approve_partial(&client_addr, &0u32, &3_000_i128);
    client.approve_partial(&client_addr, &0u32, &4_000_i128);

    assert_eq!(token.balance(&freelancer_addr), 10_000);
    assert_eq!(token.balance(&contract_id), 0);

    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    assert_eq!(milestone.released_amount, 10_000);
    assert_eq!(milestone.status, MilestoneStatus::Released);
}

#[test]
fn test_over_release_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_approve_partial(&client_addr, &0u32, &11_000_i128);
    assert!(result.is_err());
}

#[test]
fn test_negative_or_zero_release_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result1 = client.try_approve_partial(&client_addr, &0u32, &0_i128);
    assert!(result1.is_err());

    let result2 = client.try_approve_partial(&client_addr, &0u32, &-1000_i128);
    assert!(result2.is_err());
}

#[test]
fn test_release_on_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);

    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert!(result.is_err());
}

#[test]
fn test_unauthorized_partial_release_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_approve_partial(&bad_actor, &0u32, &4000_i128);
    assert!(result.is_err());
}

#[test]
fn test_claim_auto_release_before_deadline_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_claim_auto_release(&freelancer_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_claim_auto_release_after_deadline_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    env.ledger().with_mut(|li| {
        li.timestamp += 200;
    });

    client.claim_auto_release(&freelancer_addr, &0u32);

    assert_eq!(token.balance(&freelancer_addr), 10_000);
    assert_eq!(token.balance(&contract_id), 0);

    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Released);
}

#[test]
fn test_claim_auto_release_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let result = client.try_claim_auto_release(&freelancer_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_claim_auto_release_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    env.ledger().with_mut(|li| {
        li.timestamp += 200;
    });

    let result = client.try_claim_auto_release(&bad_actor, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_time_until_auto_release() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let time_remaining = client.time_until_auto_release(&0u32);
    assert!(time_remaining > 0);
    assert_eq!(time_remaining, 100);

    env.ledger().with_mut(|li| {
        li.timestamp += 50;
    });
    let time_remaining2 = client.time_until_auto_release(&0u32);
    assert_eq!(time_remaining2, 50);

    env.ledger().with_mut(|li| {
        li.timestamp += 100;
    });
    let time_remaining3 = client.time_until_auto_release(&0u32);
    assert!(time_remaining3 < 0);
}

#[test]
fn test_mark_delivered_unauthorized_client_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_mark_delivered(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_mark_delivered_unauthorized_arbiter_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_mark_delivered(&arbiter_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_mark_delivered_unauthorized_freelancer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let impostor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_mark_delivered(&impostor, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_mark_delivered_state_transitions() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &2_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // Test 1: Pending → Delivered (should pass)
    client.mark_delivered(&freelancer_addr, &0u32);
    let job = client.get_job();
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::Delivered);

    // Test 2: Delivered → Delivered (should fail)
    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Setup milestone 1 for PartiallyReleased
    client.mark_delivered(&freelancer_addr, &1u32);
    client.approve_partial(&client_addr, &1u32, &500_i128);
    let result = client.try_mark_delivered(&freelancer_addr, &1u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Reset with new environment for remaining states
    let env2 = Env::default();
    env2.mock_all_auths();
    let client_addr2 = Address::generate(&env2);
    let freelancer_addr2 = Address::generate(&env2);
    let arbiter_addr2 = Address::generate(&env2);
    let admin_addr2 = Address::generate(&env2);
    let token_contract_id2 = env2
        .register_stellar_asset_contract_v2(admin_addr2.clone())
        .address();
    let token_admin2 = token::StellarAssetClient::new(&env2, &token_contract_id2);
    token_admin2.mint(&client_addr2, &4_000);
    let contract_id2 = env2.register(MilestoneEscrow, ());
    let client2 = MilestoneEscrowClient::new(&env2, &contract_id2);
    let amounts2 = vec![&env2, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128];
    client2.initialize(
        &admin_addr2,
        &client_addr2,
        &freelancer_addr2,
        &arbiter_addr2,
        &token_contract_id2,
        &604800,
        &amounts2,
    );
    client2.fund(&client_addr2);

    // Released
    client2.mark_delivered(&freelancer_addr2, &0u32);
    client2.approve_milestone(&client_addr2, &0u32);
    let result = client2.try_mark_delivered(&freelancer_addr2, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Disputed
    client2.mark_delivered(&freelancer_addr2, &1u32);
    client2.raise_dispute(&client_addr2, &1u32);
    let result = client2.try_mark_delivered(&freelancer_addr2, &1u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Refunded
    client2.mark_delivered(&freelancer_addr2, &2u32);
    client2.raise_dispute(&client_addr2, &2u32);
    client2.resolve_dispute(&arbiter_addr2, &2u32, &false);
    let result = client2.try_mark_delivered(&freelancer_addr2, &2u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

// ============================================================================
// approve_partial — hardened test suite
// ============================================================================

/// Helper: set up a funded escrow with a single milestone of `amount` tokens,
/// mark it delivered, and return all relevant handles in one call.
fn setup_delivered_single(
    env: &Env,
    amount: i128,
) -> (
    Address, // client
    Address, // freelancer
    Address, // arbiter
    Address, // token contract
    Address, // escrow contract
    MilestoneEscrowClient<'_>,
) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);
    token_admin.mint(&client_addr, &amount);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = vec![env, amount];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &amounts,
    );
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer_addr, &0u32);

    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        token_id,
        contract_id,
        escrow,
    )
}

/// Test 1 — AUTHORIZATION: The freelancer (a known but non-client party) cannot
/// call `approve_partial`.  We assert the precise `Error::Unauthorized` variant
/// rather than just `is_err()`.
#[test]
fn test_approve_partial_freelancer_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, _, _, escrow) = setup_delivered_single(&env, 10_000);

    let result = escrow.try_approve_partial(&freelancer_addr, &0u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Test 2 — AUTHORIZATION: The arbiter (also a known but non-client party)
/// cannot call `approve_partial`.  Asserts `Error::Unauthorized` precisely.
#[test]
fn test_approve_partial_arbiter_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, arbiter_addr, _, _, escrow) = setup_delivered_single(&env, 10_000);

    let result = escrow.try_approve_partial(&arbiter_addr, &0u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Test 3 — INVALID STATE (Disputed): A milestone in `Disputed` status is not
/// approvable.  Raises a dispute first, then attempts `approve_partial` and
/// asserts `Error::InvalidStatus`.
#[test]
fn test_approve_partial_on_disputed_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, escrow) =
        setup_delivered_single(&env, 10_000);

    // Move the milestone into Disputed state (client may raise after delivery).
    escrow.raise_dispute(&client_addr, &0u32);

    let result = escrow.try_approve_partial(&client_addr, &0u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Test 4 — INVALID STATE (Refunded): A milestone that has been refunded to the
/// client is a terminal state; `approve_partial` must be rejected with
/// `Error::InvalidStatus`.
#[test]
fn test_approve_partial_on_refunded_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, arbiter_addr, _, _, escrow) =
        setup_delivered_single(&env, 10_000);

    // Dispute then resolve in favour of the client → status = Refunded.
    escrow.raise_dispute(&client_addr, &0u32);
    escrow.resolve_dispute(&arbiter_addr, &0u32, &false);

    let result = escrow.try_approve_partial(&client_addr, &0u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Test 5 — NON-EXISTENT MILESTONE ID: Supplying a milestone index that is
/// strictly out of the initialised range must return `Error::InvalidMilestone`
/// rather than panicking or silently succeeding.
#[test]
fn test_approve_partial_nonexistent_milestone_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, escrow) = setup_delivered_single(&env, 10_000);

    // The contract was initialised with exactly 1 milestone (index 0).
    // Index 1 does not exist.
    let result = escrow.try_approve_partial(&client_addr, &1u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

/// Test 6 — EXACT REMAINING BALANCE ON A PARTIALLY-RELEASED MILESTONE:
/// After one partial release the milestone is `PartiallyReleased`.  Approving
/// exactly the residual balance must flip the status to `Released` and leave
/// `released_amount == milestone.amount`.  No tokens should remain in escrow.
#[test]
fn test_approve_partial_exact_remaining_balance_transitions_to_released() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, token_id, contract_id, escrow) =
        setup_delivered_single(&env, 12_000);

    let token = token::Client::new(&env, &token_id);

    // First installment — leaves 7 000 remaining.
    escrow.approve_partial(&client_addr, &0u32, &5_000_i128);
    assert_eq!(token.balance(&freelancer_addr), 5_000);

    let job = escrow.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::PartiallyReleased);
    assert_eq!(ms.released_amount, 5_000);

    // Second installment — exactly the remainder.
    escrow.approve_partial(&client_addr, &0u32, &7_000_i128);

    let job2 = escrow.get_job();
    let ms2 = job2.milestones.get(0).unwrap();
    assert_eq!(ms2.status, MilestoneStatus::Released);
    assert_eq!(ms2.released_amount, 12_000);
    assert_eq!(token.balance(&freelancer_addr), 12_000);
    assert_eq!(token.balance(&contract_id), 0);
}

/// Test 7 — OVER-ALLOCATION ON A PARTIALLY-RELEASED MILESTONE:
/// After one partial release the remaining balance is smaller than the
/// original amount.  Requesting more than *that residual* must be rejected
/// with `Error::InvalidAmount` even though the requested value is individually
/// less than the milestone's total amount.
#[test]
fn test_approve_partial_over_release_after_prior_partial_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, escrow) = setup_delivered_single(&env, 10_000);

    // Release 6 000; only 4 000 remains.
    escrow.approve_partial(&client_addr, &0u32, &6_000_i128);

    // Attempt to release 5 000 — valid against the original total but exceeds
    // the 4 000 residual balance.
    let result = escrow.try_approve_partial(&client_addr, &0u32, &5_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Test 8 — STATE ISOLATION ACROSS MILESTONES:
/// A partial release on milestone 0 must not alter the stored state of
/// milestone 1.  `released_amount` and `status` of the untouched milestone
/// must remain exactly as initialised.
#[test]
fn test_approve_partial_does_not_mutate_sibling_milestone() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    token_admin.mint(&client_addr, &20_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    // Two milestones of equal size.
    let amounts = vec![&env, 10_000_i128, 10_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &amounts,
    );
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer_addr, &0u32);

    // Partially release milestone 0.
    escrow.approve_partial(&client_addr, &0u32, &3_000_i128);

    // Milestone 1 must remain completely untouched.
    let job = escrow.get_job();
    let ms1 = job.milestones.get(1).unwrap();
    assert_eq!(ms1.status, MilestoneStatus::Pending);
    assert_eq!(ms1.released_amount, 0);
    assert_eq!(ms1.amount, 10_000);
}

/// Test 9 — PRE-INITIALIZATION GUARD:
/// Calling `approve_partial` before the contract has been initialised at all
/// must return `Error::NotInitialized` — the function should not panic
/// unexpectedly or return a misleading error variant.
#[test]
fn test_approve_partial_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let caller = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let result = escrow.try_approve_partial(&caller, &0u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}
