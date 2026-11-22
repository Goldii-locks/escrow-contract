#![cfg(test)]
//! Tests for batch approval via `approve_milestones` (#741), and the slimmed
//! `ApprovedEvent` payload it shares with the single-milestone paths (#742).

use crate::testutils::{Escrow, EscrowFixture};
use crate::{ApprovedEvent, DataKey, Error, MilestoneStatus};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, Val};

/// Funded escrow with every milestone delivered and all auths mocked.
fn delivered<'a>(env: &'a Env, amounts: &[i128]) -> Escrow<'a> {
    env.mock_all_auths();
    let fx = EscrowFixture::new(env).milestones(amounts).funded().build();
    for i in 0..amounts.len() as u32 {
        fx.escrow.mark_delivered(&fx.freelancer, &i);
    }
    fx
}

fn statuses(fx: &Escrow) -> std::vec::Vec<MilestoneStatus> {
    fx.escrow
        .get_job()
        .milestones
        .iter()
        .map(|m| m.status)
        .collect()
}

/// Events from the last invocation whose first topic is `topic`, emitted by
/// `contract`.
fn events_with_topic(env: &Env, contract: &Address, topic: Symbol) -> std::vec::Vec<Val> {
    let topic: Val = topic.into_val(env);
    crate::all_event_tuples(env)
        .into_iter()
        .filter(|e| {
            e.0 == *contract
                && e.1
                    .get(0)
                    .is_some_and(|t| t.get_payload() == topic.get_payload())
        })
        .map(|e| e.2)
        .collect()
}

#[test]
fn approving_three_milestones_does_one_summed_transfer() {
    let env = Env::default();
    let fx = delivered(&env, &[100, 200, 300, 400]);

    fx.escrow
        .approve_milestones(&fx.client, &vec![&env, 0u32, 2u32, 3u32]);

    // Exactly one token transfer, for the sum of the three milestones.
    let transfers = events_with_topic(&env, &fx.token, symbol_short!("transfer"));
    assert_eq!(transfers.len(), 1);
    assert_eq!(i128::from_val(&env, &transfers[0]), 800);

    let token = fx.token_client();
    assert_eq!(token.balance(&fx.freelancer), 800);
    assert_eq!(token.balance(&fx.contract_id), 200);
    assert_eq!(
        statuses(&fx),
        std::vec![
            MilestoneStatus::Released,
            MilestoneStatus::Delivered,
            MilestoneStatus::Released,
            MilestoneStatus::Released,
        ]
    );
    for i in [0u32, 2, 3] {
        let ms = fx.escrow.get_job().milestones.get(i).unwrap();
        assert_eq!(ms.released_amount, ms.amount);
    }

    // Reputation is credited once per released milestone.
    assert_eq!(fx.escrow.get_reputation(&fx.client), 3);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 3);
}

#[test]
fn emits_one_slim_approve_event_per_milestone_in_order() {
    let env = Env::default();
    let fx = delivered(&env, &[100, 200, 300]);

    fx.escrow
        .approve_milestones(&fx.client, &vec![&env, 2u32, 0u32]);

    let events: std::vec::Vec<ApprovedEvent> =
        events_with_topic(&env, &fx.contract_id, symbol_short!("approve"))
            .iter()
            .map(|v| ApprovedEvent::from_val(&env, v))
            .collect();
    assert_eq!(
        events,
        std::vec![
            ApprovedEvent {
                milestone_index: 2,
                amount: 300,
                released_amount: 300,
                remaining: 0,
                status: MilestoneStatus::Released,
            },
            ApprovedEvent {
                milestone_index: 0,
                amount: 100,
                released_amount: 100,
                remaining: 0,
                status: MilestoneStatus::Released,
            },
        ]
    );
}

#[test]
fn duplicate_index_reverts_everything() {
    let env = Env::default();
    let fx = delivered(&env, &[100, 200, 300]);
    let before = statuses(&fx);

    let result = fx
        .escrow
        .try_approve_milestones(&fx.client, &vec![&env, 0u32, 1u32, 0u32]);

    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
    assert_eq!(statuses(&fx), before);
    assert_eq!(fx.token_client().balance(&fx.freelancer), 0);
    assert_eq!(fx.token_client().balance(&fx.contract_id), 600);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 0);
}

#[test]
fn out_of_range_index_reverts_everything() {
    let env = Env::default();
    let fx = delivered(&env, &[100, 200]);
    let before = statuses(&fx);

    let result = fx
        .escrow
        .try_approve_milestones(&fx.client, &vec![&env, 0u32, 7u32]);

    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
    assert_eq!(statuses(&fx), before);
    assert_eq!(fx.token_client().balance(&fx.freelancer), 0);
}

#[test]
fn ineligible_milestone_reverts_everything() {
    let env = Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env)
        .milestones(&[100, 200])
        .funded()
        .build();
    // Only milestone 0 is delivered; 1 is still Pending.
    fx.escrow.mark_delivered(&fx.freelancer, &0u32);

    let result = fx
        .escrow
        .try_approve_milestones(&fx.client, &vec![&env, 0u32, 1u32]);

    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        statuses(&fx),
        std::vec![MilestoneStatus::Delivered, MilestoneStatus::Pending]
    );
    assert_eq!(fx.token_client().balance(&fx.freelancer), 0);
}

#[test]
fn already_released_milestone_reverts_everything() {
    let env = Env::default();
    let fx = delivered(&env, &[100, 200]);
    fx.escrow.approve_milestone(&fx.client, &0u32);

    let result = fx
        .escrow
        .try_approve_milestones(&fx.client, &vec![&env, 1u32, 0u32]);

    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        statuses(&fx),
        std::vec![MilestoneStatus::Released, MilestoneStatus::Delivered]
    );
    assert_eq!(fx.token_client().balance(&fx.freelancer), 100);
}

#[test]
fn partially_released_milestone_releases_its_remainder() {
    let env = Env::default();
    let fx = delivered(&env, &[1_000, 500]);
    fx.escrow.approve_partial(&fx.client, &0u32, &400);

    fx.escrow
        .approve_milestones(&fx.client, &vec![&env, 0u32, 1u32]);

    let transfers = events_with_topic(&env, &fx.token, symbol_short!("transfer"));
    assert_eq!(transfers.len(), 1);
    assert_eq!(i128::from_val(&env, &transfers[0]), 600 + 500);
    assert_eq!(fx.token_client().balance(&fx.freelancer), 1_500);
    assert_eq!(
        statuses(&fx),
        std::vec![MilestoneStatus::Released, MilestoneStatus::Released]
    );
}

#[test]
fn empty_and_oversized_batches_are_rejected() {
    let env = Env::default();
    let fx = delivered(&env, &[1; 11]);

    assert_eq!(
        fx.escrow
            .try_approve_milestones(&fx.client, &soroban_sdk::Vec::new(&env)),
        Err(Ok(Error::InvalidMilestone))
    );

    let mut eleven = soroban_sdk::Vec::new(&env);
    for i in 0..11u32 {
        eleven.push_back(i);
    }
    assert_eq!(
        fx.escrow.try_approve_milestones(&fx.client, &eleven),
        Err(Ok(Error::InvalidMilestone))
    );

    // Exactly at the cap is fine.
    eleven.pop_back();
    fx.escrow.approve_milestones(&fx.client, &eleven);
    assert_eq!(fx.token_client().balance(&fx.freelancer), 10);
}

#[test]
fn non_client_caller_is_unauthorized() {
    let env = Env::default();
    let fx = delivered(&env, &[100, 200]);
    let stranger = Address::generate(&env);

    assert_eq!(
        fx.escrow
            .try_approve_milestones(&stranger, &vec![&env, 0u32, 1u32]),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(fx.token_client().balance(&fx.freelancer), 0);
}

#[test]
fn unfunded_escrow_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[100]).build();

    assert_eq!(
        fx.escrow
            .try_approve_milestones(&fx.client, &vec![&env, 0u32]),
        Err(Ok(Error::NotFunded))
    );
}

#[test]
fn approve_milestone_emits_slim_event() {
    let env = Env::default();
    let fx = delivered(&env, &[250]);

    fx.escrow.approve_milestone(&fx.client, &0u32);

    let events = events_with_topic(&env, &fx.contract_id, symbol_short!("approve"));
    assert_eq!(events.len(), 1);
    assert_eq!(
        ApprovedEvent::from_val(&env, &events[0]),
        ApprovedEvent {
            milestone_index: 0,
            amount: 250,
            released_amount: 250,
            remaining: 0,
            status: MilestoneStatus::Released,
        }
    );
#[test]
fn batch_approval_clears_time_extension() {
    let env = Env::default();
    let fx = delivered(&env, &[100, 200]);
    fx.escrow.extend_milestone_deadline(&fx.client, &0, &1_000);

    assert!(env.as_contract(&fx.contract_id, || env
        .storage()
        .persistent()
        .has(&DataKey::TimeExt(0))));

    fx.escrow.approve_milestones(&fx.client, &vec![&env, 0u32]);

    assert!(!env.as_contract(&fx.contract_id, || env
        .storage()
        .persistent()
        .has(&DataKey::TimeExt(0))));
}