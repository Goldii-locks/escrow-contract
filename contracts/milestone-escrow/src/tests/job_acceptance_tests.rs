#![cfg(test)]
//! Tests for freelancer and arbiter acceptance of a job.
//!
//! `initialize` only needs the admin's signature, so the freelancer and the
//! arbiter must each accept the job on-chain (`accept_job`,
//! `accept_arbiter`) before delivery or dispute endpoints run. If they never
//! do, the client can reclaim the escrow with `withdraw_unaccepted` once the
//! acceptance window has passed.

use crate::test::dispute_reason_hash;
use crate::{Error, JobDeclinedEvent, MilestoneEscrow, MilestoneEscrowClient, MilestoneStatus};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{
    symbol_short, token, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val,
};

/// Seven days, matching `ACCEPTANCE_WINDOW_SECONDS`.
const WINDOW: u64 = 7 * 24 * 60 * 60;

/// A funded escrow that neither the freelancer nor the arbiter has accepted.
fn setup_unaccepted_escrow(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    token::Client<'_>,
    MilestoneEscrowClient<'_>,
    Address,
) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    token::StellarAssetClient::new(env, &token_id).mint(&client_addr, &1_500);
    let token_client = token::Client::new(env, &token_id);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &vec![env, 1_000_i128, 500_i128],
        &crate::spec_hashes_for(&vec![env, 1_000_i128, 500_i128]),
    );
    client.fund(&client_addr);

    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        token_client,
        client,
        contract_id,
    )
}

#[test]
fn test_mark_delivered_requires_both_acceptances() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, arbiter_addr, _, client, _) = setup_unaccepted_escrow(&env);

    assert_eq!(
        client.try_mark_delivered(&freelancer_addr, &0u32),
        Err(Ok(Error::NotAccepted))
    );

    client.accept_job(&freelancer_addr);
    assert_eq!(
        client.try_mark_delivered(&freelancer_addr, &0u32),
        Err(Ok(Error::NotAccepted))
    );

    client.accept_arbiter(&arbiter_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Delivered
    );
}

#[test]
fn test_raise_dispute_requires_both_acceptances() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, client, _) = setup_unaccepted_escrow(&env);

    client.accept_job(&freelancer_addr);
    assert_eq!(
        client.try_raise_dispute(&client_addr, &0u32, &dispute_reason_hash(&env)),
        Err(Ok(Error::NotAccepted))
    );

    client.accept_arbiter(&arbiter_addr);
    // Disputes need delivered work (#784).
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(&env));
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
}

#[test]
fn test_accept_job_only_by_freelancer_and_once() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, client, _) = setup_unaccepted_escrow(&env);

    assert_eq!(
        client.try_accept_job(&client_addr),
        Err(Ok(Error::Unauthorized))
    );
    client.accept_job(&freelancer_addr);
    assert_eq!(
        client.try_accept_job(&freelancer_addr),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn test_accept_arbiter_only_by_arbiter_and_once() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, arbiter_addr, _, client, _) = setup_unaccepted_escrow(&env);

    assert_eq!(
        client.try_accept_arbiter(&freelancer_addr),
        Err(Ok(Error::Unauthorized))
    );
    client.accept_arbiter(&arbiter_addr);
    assert_eq!(
        client.try_accept_arbiter(&arbiter_addr),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn test_accept_job_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, _, client, contract_id) = setup_unaccepted_escrow(&env);
    client.accept_job(&freelancer_addr);

    let topic: Val = symbol_short!("acceptjob").into_val(&env);
    let found = crate::all_event_tuples(&env)
        .iter()
        .any(|(addr, topics, _)| {
            *addr == contract_id
                && topics.len() == 3
                && topics.get(0).unwrap().get_payload() == topic.get_payload()
        });
    assert!(found, "accept_job must publish an `acceptjob` event");
}

#[test]
fn test_client_reclaims_unaccepted_job_after_window() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, token_client, client, contract_id) =
        setup_unaccepted_escrow(&env);
    let funded_at = env.ledger().timestamp();

    // Only one acceptance is not enough to keep the job.
    client.accept_job(&freelancer_addr);

    env.ledger()
        .with_mut(|l| l.timestamp = funded_at + WINDOW - 1);
    assert_eq!(
        client.try_withdraw_unaccepted(&client_addr),
        Err(Ok(Error::DeadlineNotPassed))
    );

    env.ledger().with_mut(|l| l.timestamp = funded_at + WINDOW);
    assert_eq!(
        client.try_withdraw_unaccepted(&freelancer_addr),
        Err(Ok(Error::Unauthorized))
    );
    client.withdraw_unaccepted(&client_addr);

    assert_eq!(token_client.balance(&client_addr), 1_500);
    assert_eq!(token_client.balance(&contract_id), 0);
    for milestone in client.get_job().milestones.iter() {
        assert_eq!(milestone.status, MilestoneStatus::Refunded);
    }

    // Nothing is left to reclaim a second time.
    assert_eq!(
        client.try_withdraw_unaccepted(&client_addr),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_withdraw_unaccepted_rejected_once_accepted() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, client, _) = setup_unaccepted_escrow(&env);
    client.accept_job(&freelancer_addr);
    client.accept_arbiter(&arbiter_addr);

    env.ledger().with_mut(|l| l.timestamp += WINDOW);
    assert_eq!(
        client.try_withdraw_unaccepted(&client_addr),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn declined_party_allows_immediate_client_refund() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, token_client, escrow, contract_id) =
        setup_unaccepted_escrow(&env);
    assert_eq!(
        escrow.try_withdraw_unaccepted(&client_addr),
        Err(Ok(Error::DeadlineNotPassed))
    );

    escrow.decline_job(&freelancer_addr);
    let event = crate::all_event_tuples(&env)
        .last()
        .expect("declining emits an event")
        .clone();
    let topic: Symbol = event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("decljob"));
    assert_eq!(
        JobDeclinedEvent::from_val(&env, &event.2),
        JobDeclinedEvent {
            party: freelancer_addr.clone(),
            declined_mask: 1,
        }
    );
    escrow.withdraw_unaccepted(&client_addr);

    assert_eq!(token_client.balance(&client_addr), 1_500);
    assert_eq!(token_client.balance(&contract_id), 0);
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Refunded
    );
}

#[test]
fn arbiter_can_decline_and_declines_are_party_scoped() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, escrow, _) = setup_unaccepted_escrow(&env);
    assert_eq!(
        escrow.try_decline_job(&client_addr),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        escrow.try_decline_arbiter(&freelancer_addr),
        Err(Ok(Error::Unauthorized))
    );
    escrow.decline_arbiter(&arbiter_addr);
    let event = crate::all_event_tuples(&env)
        .last()
        .expect("declining emits an event")
        .clone();
    let topic: Symbol = event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("declarb"));
    assert_eq!(
        JobDeclinedEvent::from_val(&env, &event.2),
        JobDeclinedEvent {
            party: arbiter_addr.clone(),
            declined_mask: 2,
        }
    );
    escrow.withdraw_unaccepted(&client_addr);
}

#[test]
fn declining_after_acceptance_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, arbiter_addr, _, escrow, _) = setup_unaccepted_escrow(&env);
    escrow.accept_job(&freelancer_addr);
    escrow.accept_arbiter(&arbiter_addr);
    assert_eq!(
        escrow.try_decline_job(&freelancer_addr),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(
        escrow.try_decline_arbiter(&arbiter_addr),
        Err(Ok(Error::InvalidStatus))
    );
}

/// Under PerMilestone funding, `withdraw_unaccepted` works once any milestone
/// is funded and refunds only what was deposited; unfunded milestones are left
/// untouched instead of being "refunded" with money the escrow never held.
#[test]
fn test_withdraw_unaccepted_refunds_only_funded_milestones() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&client_addr, &1_500);
    let token_client = token::Client::new(&env, &token_id);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize_with_funding_mode(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &vec![&env, 1_000_i128, 500_i128],
        &crate::spec_hashes_for(&vec![&env, 1_000_i128, 500_i128]),
        &crate::FundingMode::PerMilestone,
    );
    client.fund_milestone(&client_addr, &0u32);
    assert_eq!(token_client.balance(&client_addr), 500);

    // The first funded milestone starts the acceptance window.
    assert_eq!(
        client.try_withdraw_unaccepted(&client_addr),
        Err(Ok(Error::DeadlineNotPassed))
    );
    env.ledger().with_mut(|l| l.timestamp += WINDOW);
    client.withdraw_unaccepted(&client_addr);

    assert_eq!(token_client.balance(&client_addr), 1_500);
    assert_eq!(token_client.balance(&contract_id), 0);
    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Refunded
    );
    assert_eq!(
        job.milestones.get(1).unwrap().status,
        MilestoneStatus::Pending
    );
}
