#![cfg(test)]
use super::*;
use soroban_sdk::{
    testutils::Address as _, testutils::Ledger, vec, Address, Env,
};

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
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
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
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
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
    assert_eq!(result, Err(Ok(Error::AlreadyFunded)));
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
fn test_approve_partial_wrong_status_fails() {
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

    // Try to approve partial on Pending status
    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Mark delivered and approve fully
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    // Try to approve partial on Released status
    let result = client.try_approve_partial(&client_addr, &0u32, &1000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_partial_invalid_milestone_fails() {
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

    // Try to approve partial on non-existent milestone
    let result = client.try_approve_partial(&client_addr, &1u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

#[test]
fn test_approve_partial_before_funded_fails() {
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

    // Try to approve partial on Pending status (before mark_delivered)
    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_partial_unauthorized_partial_release_fails() {
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
fn test_approve_partial_state_transitions() {
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

    // Test 1: Pending → InvalidStatus (should fail)
    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Test 2: Delivered → PartiallyReleased (should pass)
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &4000_i128);
    let job = client.get_job();
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::PartiallyReleased);
    assert_eq!(job.milestones.get(0).unwrap().released_amount, 4000);

    // Test 3: PartiallyReleased → PartiallyReleased (should pass)
    client.approve_partial(&client_addr, &0u32, &3000_i128);
    let job = client.get_job();
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::PartiallyReleased);
    assert_eq!(job.milestones.get(0).unwrap().released_amount, 7000);

    // Test 4: PartiallyReleased → Released (should pass)
    client.approve_partial(&client_addr, &0u32, &3000_i128);
    let job = client.get_job();
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::Released);
    assert_eq!(job.milestones.get(0).unwrap().released_amount, 10000);

    // Test 5: Released → InvalidStatus (should fail)
    let result = client.try_approve_partial(&client_addr, &0u32, &1000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Verify token balances
    assert_eq!(token.balance(&freelancer_addr), 10000);
    assert_eq!(token.balance(&contract_id), 0);
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
