use super::*;
use soroban_sdk::testutils::IssuerFlags;
use soroban_sdk::{symbol_short, Symbol, TryIntoVal};

/// An initialized escrow that nobody has accepted or funded yet, over an
/// auth-revocable asset so a payout can be forced into the `withdraw` credit.
fn unaccepted_escrow(
    env: &Env,
    mode: FundingMode,
) -> (
    Address,
    Address,
    Address,
    token::StellarAssetClient<'_>,
    MilestoneEscrowClient<'_>,
) {
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    sac.issuer().set_flag(IssuerFlags::RevocableFlag);
    let token_admin = token::StellarAssetClient::new(env, &sac.address());
    token_admin.mint(&client, &1_000);
    let escrow = MilestoneEscrowClient::new(env, &env.register(MilestoneEscrow, ()));
    let amounts = vec![env, 1_000_i128];
    escrow.initialize_with_funding_mode(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &sac.address(),
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        &mode,
    );
    (client, freelancer, arbiter, token_admin, escrow)
}

fn assert_topics(env: &Env, name: Symbol, addresses: &[Address]) {
    let events = crate::all_event_tuples(env);
    let topics = &events.last().expect("event was emitted").1;
    assert_eq!(topics.len(), addresses.len() as u32 + 1);
    let first: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(first, name);
    for (index, expected_address) in addresses.iter().enumerate() {
        let address: Address = topics
            .get(index as u32 + 1)
            .unwrap()
            .try_into_val(env)
            .unwrap();
        assert_eq!(&address, expected_address);
    }
}

#[test]
fn init_topics_keep_event_name_and_index_all_parties() {
    let env = Env::default();
    env.mock_all_auths();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    escrow.initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
    );

    assert_topics(&env, symbol_short!("init"), &[client, freelancer, arbiter]);
}

#[test]
fn fund_topics_keep_event_name_and_index_all_parties() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, _, _, _) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_topics(&env, symbol_short!("fund"), &[client, freelancer, arbiter]);
}

#[test]
fn approve_topics_keep_event_name_and_index_client() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    assert_topics(&env, symbol_short!("approve"), &[client]);
}

#[test]
fn dispute_topics_keep_event_name_and_index_caller() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &dispute_reason_hash(&env));

    assert_topics(&env, symbol_short!("dispute"), &[client]);
}

#[test]
fn acceptance_topics_index_acceptor_then_client() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, escrow) = unaccepted_escrow(&env, FundingMode::Upfront);

    escrow.accept_job(&freelancer);
    assert_topics(
        &env,
        symbol_short!("acceptjob"),
        &[freelancer, client.clone()],
    );

    escrow.accept_arbiter(&arbiter);
    assert_topics(&env, symbol_short!("acceptarb"), &[arbiter, client]);
}

#[test]
fn msfund_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, escrow) =
        unaccepted_escrow(&env, FundingMode::PerMilestone);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);

    escrow.fund_milestone(&client, &0);

    assert_topics(&env, symbol_short!("msfund"), &[client, freelancer]);
}

#[test]
fn withdraw_topics_index_recipient_then_counterparty() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, token_admin, escrow) =
        unaccepted_escrow(&env, FundingMode::Upfront);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);
    escrow.mark_delivered(&freelancer, &0);
    token_admin.set_authorized(&freelancer, &false);
    escrow.approve_milestone(&client, &0);
    token_admin.set_authorized(&freelancer, &true);

    escrow.withdraw(&freelancer);

    assert_topics(&env, symbol_short!("withdraw"), &[freelancer, client]);
}

#[test]
fn sweep_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, token, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    token::StellarAssetClient::new(&env, &token).mint(&contract_id, &50);

    escrow.sweep_excess(&client, &token);

    assert_topics(&env, symbol_short!("sweep"), &[client, freelancer]);
}

#[test]
fn overdue_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, escrow) = unaccepted_escrow(&env, FundingMode::Upfront);
    escrow.set_milestone_due_dates(&client, &vec![&env, 10_000]);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);
    env.ledger()
        .set_timestamp(10_000 + crate::OVERDUE_GRACE_SECONDS + 1);

    escrow.reclaim_overdue(&client, &0);

    assert_topics(&env, symbol_short!("overdue"), &[client, freelancer]);
}

#[test]
fn unaccref_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, escrow) = unaccepted_escrow(&env, FundingMode::Upfront);
    escrow.fund(&client);
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + crate::ACCEPTANCE_WINDOW_SECONDS);

    escrow.withdraw_unaccepted(&client);

    assert_topics(&env, symbol_short!("unaccref"), &[client, freelancer]);
}

#[test]
fn msrev_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);

    escrow.request_revision(&client, &0, &BytesN::from_array(&env, &[3; 32]));

    assert_topics(&env, symbol_short!("msrev"), &[client, freelancer]);
}

#[test]
fn extend_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);

    escrow.extend_milestone_deadline(&client, &0, &3_600);

    assert_topics(&env, symbol_short!("extend"), &[client, freelancer]);
}

#[test]
fn ms_add_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    token::StellarAssetClient::new(&env, &token).mint(&client, &500);

    escrow.add_milestone(&client, &freelancer, &500);

    assert_topics(&env, symbol_short!("ms_add"), &[client, freelancer]);
}

#[test]
fn ms_reduce_topics_index_client_then_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.reduce_milestone(&client, &freelancer, &0, &400);

    assert_topics(&env, symbol_short!("ms_reduce"), &[client, freelancer]);
}

#[test]
fn cancel_topics_index_caller_then_counterparty() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.cancel_escrow(&client);
    assert_topics(
        &env,
        symbol_short!("cxlappr"),
        &[client.clone(), freelancer.clone()],
    );

    escrow.cancel_escrow(&freelancer);
    assert_topics(&env, symbol_short!("cancel"), &[freelancer, client]);
}

#[test]
fn cancelrev_topics_index_caller_then_counterparty() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.cancel_escrow(&freelancer);

    escrow.revoke_cancel_approval(&freelancer);

    assert_topics(&env, symbol_short!("cancelrev"), &[freelancer, client]);
}
