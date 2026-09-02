#![cfg(test)]
//! Unit tests for `admin_override_streaming_release`.
//!
//! The admin settles a *disputed* milestone by streaming it pro-rata over
//! elapsed time: the freelancer is paid for the fraction worked and the client
//! is refunded the rest. It moves funds and had no coverage on main, though
//! snapshots for these test names survive from a suite that was lost.

use super::*;
use crate::test::setup_funded_escrow;
use crate::{AdminOverrideStreamingReleaseEvent, Error, MilestoneStatus};
use soroban_sdk::testutils::Events as _;
use soroban_sdk::{symbol_short, token, vec, Address, Env, FromVal, IntoVal, Val};

fn admstrm_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("admstrm").into_val(env);
    let mut count = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

/// Funded escrow with milestone 0 already moved into `Disputed`.
fn setup_disputed(
    env: &Env,
    amount: i128,
) -> (
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let (client_addr, freelancer_addr, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(env, vec![env, amount]);
    client.raise_dispute(&client_addr, &0u32);
    (client_addr, freelancer_addr, admin_addr, token_id, client)
}

#[test]
fn test_admin_override_streaming_release_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, admin_addr, token_id, client) =
        setup_disputed(&env, 1_000_i128);

    // Half the window elapsed -> half to the freelancer, half back.
    let split = client.admin_override_streaming_release(&admin_addr, &0u32, &50_i128, &100_i128);

    assert_eq!(admstrm_event_count(&env), 1);
    assert_eq!(split.first + split.second, 1_000);
    assert_eq!(split.first, 500, "freelancer payout");
    assert_eq!(split.second, 500, "client refund");

    let token_client = token::Client::new(&env, &token_id);
    assert_eq!(token_client.balance(&freelancer_addr), 500);
    assert_eq!(token_client.balance(&client_addr), 500);
}

#[test]
fn test_admin_override_streaming_release_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, admin_addr, token_id, client) =
        setup_disputed(&env, 1_000_i128);

    client.admin_override_streaming_release(&admin_addr, &0u32, &25_i128, &100_i128);

    let events = env.events().all();
    let ev = AdminOverrideStreamingReleaseEvent::from_val(&env, &events.last().unwrap().2);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.milestone_index, 0);
    assert_eq!(ev.client, client_addr);
    assert_eq!(ev.freelancer, freelancer_addr);
    assert_eq!(ev.token, token_id);
    // The event must reconcile with the amounts actually transferred.
    assert_eq!(ev.freelancer_payout, 250);
    assert_eq!(ev.client_refund, 750);
}

#[test]
fn test_admin_override_streaming_release_zero_elapsed_refunds_client() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, admin_addr, token_id, client) =
        setup_disputed(&env, 1_000_i128);

    let split = client.admin_override_streaming_release(&admin_addr, &0u32, &0_i128, &100_i128);

    assert_eq!(split.first, 0, "no time elapsed, no payout");
    assert_eq!(split.second, 1_000);

    let token_client = token::Client::new(&env, &token_id);
    assert_eq!(token_client.balance(&client_addr), 1_000);
    assert_eq!(token_client.balance(&freelancer_addr), 0);

    // With nothing paid out the milestone is Refunded, not Released.
    let milestone = client.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Refunded);
}

#[test]
fn test_admin_override_streaming_release_full_elapsed_pays_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, admin_addr, token_id, client) =
        setup_disputed(&env, 1_000_i128);

    let split = client.admin_override_streaming_release(&admin_addr, &0u32, &100_i128, &100_i128);

    assert_eq!(split.first, 1_000);
    assert_eq!(split.second, 0);

    let token_client = token::Client::new(&env, &token_id);
    assert_eq!(token_client.balance(&freelancer_addr), 1_000);
    assert_eq!(token_client.balance(&client_addr), 0);

    let milestone = client.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Released);
}

#[test]
fn test_admin_override_streaming_release_requires_disputed_status() {
    let env = Env::default();
    env.mock_all_auths();

    // Milestone left Pending -- no dispute raised.
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_override_streaming_release(&admin_addr, &0u32, &50_i128, &100_i128),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn test_admin_override_streaming_release_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, admin_addr, token_id, client) =
        setup_disputed(&env, 1_000_i128);
    let _ = admin_addr;

    for caller in [&client_addr, &freelancer_addr] {
        assert_eq!(
            client.try_admin_override_streaming_release(caller, &0u32, &50_i128, &100_i128),
            Err(Ok(Error::Unauthorized)),
        );
    }

    // No funds moved on the rejected calls.
    let token_client = token::Client::new(&env, &token_id);
    assert_eq!(token_client.balance(&client_addr), 0);
    assert_eq!(token_client.balance(&freelancer_addr), 0);
}

#[test]
fn test_admin_override_streaming_release_invalid_time_params_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, admin_addr, _, client) = setup_disputed(&env, 1_000_i128);

    // Zero / negative window.
    assert!(client
        .try_admin_override_streaming_release(&admin_addr, &0u32, &10_i128, &0_i128)
        .is_err());
    // Elapsed beyond the window is not a valid ratio.
    assert!(client
        .try_admin_override_streaming_release(&admin_addr, &0u32, &200_i128, &100_i128)
        .is_err());
    // Negative elapsed.
    assert!(client
        .try_admin_override_streaming_release(&admin_addr, &0u32, &-1_i128, &100_i128)
        .is_err());
}

#[test]
fn test_admin_override_streaming_release_invalid_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, admin_addr, _, client) = setup_disputed(&env, 1_000_i128);

    assert_eq!(
        client.try_admin_override_streaming_release(&admin_addr, &5u32, &50_i128, &100_i128),
        Err(Ok(Error::InvalidMilestone))
    );
}
