#![cfg(test)]
use super::*;
use soroban_sdk::{testutils::Address as _, testutils::Ledger, vec, Address, Env};

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
