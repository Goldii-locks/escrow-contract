use super::*;
use soroban_sdk::{symbol_short, FromVal, TryIntoVal};

fn custom_timeout_escrow(
    env: &Env,
    timeout_seconds: u64,
) -> (
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
    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    let amounts = vec![env, 1_000_i128];
    client.initialize_with_dispute_timeout(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &600,
        &timeout_seconds,
        &amounts,
        &crate::spec_hashes_for(&amounts),
    );
    client.accept_job(&freelancer_addr);
    client.accept_arbiter(&arbiter_addr);
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0);
    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        contract_id,
        client,
    )
}

fn last_dispute_raised_event(env: &Env) -> DisputeRaisedEvent {
    let event = crate::all_event_tuples(env)
        .into_iter()
        .find(|event| {
            event.1.get(0).and_then(|topic| {
                let topic: Symbol = topic.try_into_val(env).ok()?;
                Some(topic)
            }) == Some(symbol_short!("dispute"))
        })
        .unwrap();
    DisputeRaisedEvent::from_val(env, &event.2)
}

#[test]
fn escalation_fails_before_timeout_and_restores_delivered_after_timeout() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let (client_addr, freelancer_addr, _, _, client) = custom_timeout_escrow(&env, 60);
    env.ledger().set_timestamp(1_100);
    client.raise_dispute(&client_addr, &0, &dispute_reason_hash(&env));

    let raised = last_dispute_raised_event(&env);
    assert_eq!(raised.disputed_at, 1_100);
    assert_eq!(raised.new_status, MilestoneStatus::Disputed);

    env.ledger().set_timestamp(1_159);
    assert_eq!(
        client.try_escalate_dispute(&freelancer_addr, &0),
        Err(Ok(Error::DisputeTimeoutNotReached))
    );
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );

    env.ledger().set_timestamp(1_160);
    client.escalate_dispute(&freelancer_addr, &0);

    let restored = client.get_job().milestones.get(0).unwrap();
    assert_eq!(restored.status, MilestoneStatus::Delivered);
    assert_eq!(restored.delivered_at, 1_000);
}

#[test]
fn initialize_rejects_zero_dispute_timeout() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_initialize_with_dispute_timeout(
            &admin,
            &client_addr,
            &freelancer,
            &arbiter,
            &token_id,
            &600,
            &0,
            &vec![&env, 1_000_i128],
            &crate::spec_hashes_for(&vec![&env, 1_000_i128]),
        ),
        Err(Ok(Error::InvalidDisputeTimeout))
    );
}

#[test]
fn legacy_initializer_uses_default_dispute_timeout() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer,
        &arbiter,
        &token_id,
        &600,
        &vec![&env, 1_000_i128],
        &crate::spec_hashes_for(&vec![&env, 1_000_i128]),
    );

    assert_eq!(
        env.as_contract(&contract_id, || {
            env.storage()
                .instance()
                .get(&DataKey::DisputeTimeout)
                .unwrap_or(DISPUTE_TIMEOUT_SECONDS)
        }),
        DISPUTE_TIMEOUT_SECONDS
    );
}
