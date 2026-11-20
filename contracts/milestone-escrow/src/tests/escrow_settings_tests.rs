#![cfg(test)]

use crate::{
    Error, EscrowSettings, FundingMode, MilestoneEscrow, MilestoneEscrowClient,
    ACCEPTANCE_WINDOW_SECONDS, DISPUTE_TIMEOUT_SECONDS, OVERDUE_GRACE_SECONDS,
};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, vec, Address, BytesN, Env};

fn settings(
    max_revisions: u32,
    acceptance_window_seconds: u64,
    overdue_grace_seconds: u64,
) -> EscrowSettings {
    EscrowSettings {
        funding_mode: FundingMode::Upfront,
        dispute_timeout_seconds: DISPUTE_TIMEOUT_SECONDS,
        arbiter_fee_bps: 0,
        max_revisions,
        acceptance_window_seconds,
        overdue_grace_seconds,
    }
}

fn create_unaccepted_escrow<'a>(
    env: &'a Env,
    config: &EscrowSettings,
) -> (Address, Address, MilestoneEscrowClient<'a>) {
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let admin = Address::generate(env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(env, &token_id).mint(&client, &1_000);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    let amounts = vec![env, 1_000_i128];
    escrow.initialize_with_settings_test(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token_id,
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        config,
    );
    (client, token_id, escrow)
}

#[test]
fn configured_revision_limit_is_used() {
    let env = Env::default();
    env.mock_all_auths();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&client, &1_000);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    escrow.initialize_with_settings_test(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token_id,
        &604_800,
        &vec![&env, 1_000_i128],
        &crate::spec_hashes_for(&vec![&env, 1_000_i128]),
        &settings(1, ACCEPTANCE_WINDOW_SECONDS, OVERDUE_GRACE_SECONDS),
    );
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);

    escrow.mark_delivered(&freelancer, &0);
    escrow.request_revision(&client, &0, &BytesN::from_array(&env, &[1; 32]));
    escrow.mark_delivered(&freelancer, &0);
    assert_eq!(
        escrow.try_request_revision(&client, &0, &BytesN::from_array(&env, &[2; 32])),
        Err(Ok(Error::MaxRevisions))
    );
}

#[test]
fn zero_revision_limit_disables_revisions() {
    let env = Env::default();
    env.mock_all_auths();
    let config = settings(0, ACCEPTANCE_WINDOW_SECONDS, OVERDUE_GRACE_SECONDS);
    let (client, _, escrow) = create_unaccepted_escrow(&env, &config);
    let job = escrow.get_job();

    escrow.accept_job(&job.freelancer);
    escrow.accept_arbiter(&job.arbiter);
    escrow.fund(&client);
    escrow.mark_delivered(&job.freelancer, &0);

    assert_eq!(
        escrow.try_request_revision(&client, &0, &BytesN::from_array(&env, &[1; 32])),
        Err(Ok(Error::MaxRevisions))
    );
}

#[test]
fn invalid_setting_bounds_are_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    let hashes = crate::spec_hashes_for(&amounts);

    for invalid in [
        settings(101, ACCEPTANCE_WINDOW_SECONDS, OVERDUE_GRACE_SECONDS),
        settings(3, 3_599, OVERDUE_GRACE_SECONDS),
        settings(3, ACCEPTANCE_WINDOW_SECONDS, 90 * 24 * 60 * 60 + 1),
    ] {
        assert_eq!(
            escrow.try_initialize_with_settings_test(
                &admin,
                &client,
                &freelancer,
                &arbiter,
                &token_id,
                &604_800,
                &amounts,
                &hashes,
                &invalid,
            ),
            Err(Ok(Error::InvalidEscrowSettings))
        );
    }

    escrow.initialize_with_settings_test(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token_id,
        &604_800,
        &amounts,
        &hashes,
        &settings(100, 90 * 24 * 60 * 60, 90 * 24 * 60 * 60),
    );
}

#[test]
fn configured_acceptance_and_overdue_windows_are_used() {
    let env = Env::default();
    env.mock_all_auths();
    let config = settings(3, 3_600, 0);

    let (client, token_id, escrow) = create_unaccepted_escrow(&env, &config);
    escrow.fund(&client);
    let funded_at = env.ledger().timestamp();
    env.ledger().set_timestamp(funded_at + 3_599);
    assert_eq!(
        escrow.try_withdraw_unaccepted(&client),
        Err(Ok(Error::DeadlineNotPassed))
    );
    env.ledger().set_timestamp(funded_at + 3_600);
    escrow.withdraw_unaccepted(&client);
    assert_eq!(token::Client::new(&env, &token_id).balance(&client), 1_000);

    let (client, token_id, escrow) = create_unaccepted_escrow(&env, &config);
    let due_at = env.ledger().timestamp() + 100;
    escrow.set_milestone_due_dates(&client, &vec![&env, due_at]);
    escrow.fund(&client);
    env.ledger().set_timestamp(due_at + 1);
    escrow.reclaim_overdue(&client, &0);
    assert_eq!(token::Client::new(&env, &token_id).balance(&client), 1_000);
}
