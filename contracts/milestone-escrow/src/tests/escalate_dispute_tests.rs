#![cfg(test)]
//! Branch and lifecycle coverage for `escalate_dispute` (#979).

use crate::{
    all_event_tuples, DataKey, DisputeEscalatedEvent, Error, FundingMode, MilestoneEscrow,
    MilestoneEscrowClient, MilestoneStatus, DISPUTE_TIMEOUT_SECONDS, MIN_DISPUTE_TIMEOUT_SECONDS,
};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{symbol_short, token, vec, Address, Env, FromVal, Symbol, TryFromVal};

const RAISED_AT: u64 = 1_000;

struct Setup<'a> {
    admin: Address,
    client: Address,
    freelancer: Address,
    arbiter: Address,
    contract_id: Address,
    escrow: MilestoneEscrowClient<'a>,
}

fn setup_with_timeout(env: &Env, timeout: Option<u64>) -> Setup<'_> {
    env.mock_all_auths();
    env.ledger().set_timestamp(RAISED_AT);
    let admin = Address::generate(env);
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(env, &token_id).mint(&client, &1_000);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    let amounts = vec![env, 1_000_i128];
    if let Some(timeout) = timeout {
        escrow.initialize_with_dispute_timeout(
            &admin,
            &client,
            &freelancer,
            &arbiter,
            &token_id,
            &604_800,
            &timeout,
            &amounts,
            &crate::spec_hashes_for(&amounts),
        );
    } else {
        escrow.initialize(
            &admin,
            &client,
            &freelancer,
            &arbiter,
            &token_id,
            &604_800,
            &amounts,
            &crate::spec_hashes_for(&amounts),
        );
    }
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);
    escrow.mark_delivered(&freelancer, &0);
    Setup {
        admin,
        client,
        freelancer,
        arbiter,
        contract_id,
        escrow,
    }
}

fn setup_with_per_milestone_funding(env: &Env) -> Setup<'_> {
    env.mock_all_auths();
    env.ledger().set_timestamp(RAISED_AT);
    let admin = Address::generate(env);
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(env, &token_id).mint(&client, &2_000);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    let amounts = vec![env, 1_000_i128, 1_000_i128];
    escrow.initialize_with_funding_mode(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token_id,
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        &FundingMode::PerMilestone,
    );
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund_milestone(&client, &0);
    escrow.mark_delivered(&freelancer, &0);
    Setup {
        admin,
        client,
        freelancer,
        arbiter,
        contract_id,
        escrow,
    }
}

fn raise_dispute(env: &Env, setup: &Setup, caller: &Address) {
    setup
        .escrow
        .raise_dispute(caller, &0, &crate::tests::dispute_reason_hash(env));
}

fn escalation_event(env: &Env) -> DisputeEscalatedEvent {
    let (_, _, data) = all_event_tuples(env)
        .into_iter()
        .find(|(_, topics, _)| {
            topics.len() == 1
                && Symbol::try_from_val(env, &topics.get(0).unwrap()).ok()
                    == Some(symbol_short!("dispescal"))
        })
        .expect("dispute escalation event should be published");
    DisputeEscalatedEvent::from_val(env, &data)
}

fn dispute_info_exists(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage().persistent().has(&DataKey::DisputeInfo(0))
    })
}

#[test]
fn custom_timeout_uses_the_configured_boundary_and_restores_delivered_state() {
    let env = Env::default();
    let s = setup_with_timeout(&env, Some(MIN_DISPUTE_TIMEOUT_SECONDS));
    raise_dispute(&env, &s, &s.client);

    env.ledger()
        .set_timestamp(RAISED_AT + MIN_DISPUTE_TIMEOUT_SECONDS - 1);
    assert_eq!(
        s.escrow.try_escalate_dispute(&s.freelancer, &0),
        Err(Ok(Error::DisputeTimeoutNotReached))
    );
    assert_eq!(s.escrow.get_milestone(&0).status, MilestoneStatus::Disputed);
    assert!(dispute_info_exists(&env, &s.contract_id));

    let escalated_at = RAISED_AT + MIN_DISPUTE_TIMEOUT_SECONDS;
    env.ledger().set_timestamp(escalated_at);
    s.escrow.escalate_dispute(&s.freelancer, &0);
    let event = escalation_event(&env);
    let restored = s.escrow.get_milestone(&0);
    assert_eq!(restored.status, MilestoneStatus::Delivered);
    assert_eq!(restored.delivered_at, RAISED_AT);
    assert!(!dispute_info_exists(&env, &s.contract_id));
    assert!(!env.as_contract(&s.contract_id, || {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::DisputeFlag(0))
            .unwrap_or(false)
    }));
    assert_eq!(
        event,
        DisputeEscalatedEvent {
            milestone_index: 0,
            caller: s.freelancer.clone(),
            disputed_at: RAISED_AT,
            escalated_at,
            restored_status: MilestoneStatus::Delivered,
        }
    );
    assert_eq!(
        s.escrow.try_escalate_dispute(&s.client, &0),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn default_timeout_remains_in_effect_for_the_legacy_initializer() {
    let env = Env::default();
    let s = setup_with_timeout(&env, None);
    raise_dispute(&env, &s, &s.client);

    env.ledger()
        .set_timestamp(RAISED_AT + MIN_DISPUTE_TIMEOUT_SECONDS);
    assert_eq!(
        s.escrow.try_escalate_dispute(&s.client, &0),
        Err(Ok(Error::DisputeTimeoutNotReached))
    );
    env.ledger()
        .set_timestamp(RAISED_AT + DISPUTE_TIMEOUT_SECONDS);
    s.escrow.escalate_dispute(&s.client, &0);
}

#[test]
fn escalation_restores_partially_released_milestone() {
    let env = Env::default();
    let s = setup_with_timeout(&env, Some(MIN_DISPUTE_TIMEOUT_SECONDS));
    s.escrow.approve_partial(&s.client, &0, &250);
    raise_dispute(&env, &s, &s.client);

    env.ledger()
        .set_timestamp(RAISED_AT + MIN_DISPUTE_TIMEOUT_SECONDS);
    s.escrow.escalate_dispute(&s.freelancer, &0);
    let event = escalation_event(&env);
    let restored = s.escrow.get_milestone(&0);
    assert_eq!(restored.status, MilestoneStatus::PartiallyReleased);
    assert_eq!(restored.released_amount, 250);
    assert_eq!(event.restored_status, MilestoneStatus::PartiallyReleased);
}

#[test]
fn escalation_rejects_non_parties_and_already_resolved_disputes() {
    let env = Env::default();
    let s = setup_with_timeout(&env, Some(MIN_DISPUTE_TIMEOUT_SECONDS));
    raise_dispute(&env, &s, &s.client);
    let outsider = Address::generate(&env);
    assert_eq!(
        s.escrow.try_escalate_dispute(&outsider, &0),
        Err(Ok(Error::Unauthorized))
    );

    s.escrow
        .resolve_dispute(&s.arbiter, &0, &true, &crate::tests::decision_hash(&env));
    assert_eq!(
        s.escrow.try_escalate_dispute(&s.client, &0),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn escalation_is_blocked_while_paused_or_cancel_locked() {
    let env = Env::default();
    let s = setup_with_timeout(&env, Some(MIN_DISPUTE_TIMEOUT_SECONDS));
    raise_dispute(&env, &s, &s.client);
    s.escrow.admin_pause_escrow(&s.admin);
    assert_eq!(
        s.escrow.try_escalate_dispute(&s.client, &0),
        Err(Ok(Error::Paused))
    );

    let env = Env::default();
    let s = setup_with_timeout(&env, Some(MIN_DISPUTE_TIMEOUT_SECONDS));
    raise_dispute(&env, &s, &s.client);
    s.escrow.cancel_escrow(&s.client);
    s.escrow.cancel_escrow(&s.freelancer);
    assert_eq!(
        s.escrow.try_escalate_dispute(&s.client, &0),
        Err(Ok(Error::EscrowLocked))
    );
}

#[test]
fn per_milestone_funding_does_not_satisfy_the_job_funded_guard() {
    let env = Env::default();
    let s = setup_with_per_milestone_funding(&env);
    raise_dispute(&env, &s, &s.client);

    env.ledger()
        .set_timestamp(RAISED_AT + DISPUTE_TIMEOUT_SECONDS);
    assert_eq!(
        s.escrow.try_escalate_dispute(&s.client, &0),
        Err(Ok(Error::NotFunded))
    );
}
