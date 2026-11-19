#![cfg(test)]
//! Unit tests for `get_reputation` and the `increment_reputation` bookkeeping
//! behind it.
//!
//! Reputation is bumped for *both* parties every time a milestone reaches
//! `Released`, through whichever path got it there. The endpoint had no
//! coverage at all: the two tests that exercised it were removed when
//! `test.rs` was rewritten, and nothing replaced them, leaving a public
//! contract function untested while its snapshots stayed behind.

use crate::testutils::EscrowFixture;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{Address, Env};

#[test]
fn test_reputation_tracking() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env)
        .milestones(&[1_000_i128, 500_i128])
        .funded()
        .build();
    let (client_addr, freelancer_addr, client) = (fx.client, fx.freelancer, fx.escrow);

    // Nobody has a reputation before a milestone is released.
    assert_eq!(client.get_reputation(&client_addr), 0);
    assert_eq!(client.get_reputation(&freelancer_addr), 0);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    // A release credits both sides of the contract, not just the freelancer.
    assert_eq!(client.get_reputation(&client_addr), 1);
    assert_eq!(client.get_reputation(&freelancer_addr), 1);

    // A second release accumulates rather than overwriting.
    client.mark_delivered(&freelancer_addr, &1u32);
    client.approve_milestone(&client_addr, &1u32);

    assert_eq!(client.get_reputation(&client_addr), 2);
    assert_eq!(client.get_reputation(&freelancer_addr), 2);
}

#[test]
fn test_reputation_unknown_address_is_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    let (client_addr, freelancer_addr, client) = (fx.client, fx.freelancer, fx.escrow);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    // An address that never took part reads as 0, not as a missing-key panic.
    let stranger = Address::generate(&env);
    assert_eq!(client.get_reputation(&stranger), 0);

    // ...and the release did not leak into it.
    assert_eq!(client.get_reputation(&client_addr), 1);
}

#[test]
fn test_reputation_auto_release() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    let (client_addr, freelancer_addr, client) = (fx.client, fx.freelancer, fx.escrow);

    client.mark_delivered(&freelancer_addr, &0u32);

    // Push past the 604800s auto-release window configured by the helper.
    env.ledger().with_mut(|l| {
        l.timestamp += 604_800 + 1;
    });

    client.claim_auto_release(&freelancer_addr, &0u32);

    // Auto-release is still a release, so it must credit both parties the
    // same way an explicit approve_milestone does.
    assert_eq!(client.get_reputation(&client_addr), 1);
    assert_eq!(client.get_reputation(&freelancer_addr), 1);
}

#[test]
fn test_reputation_not_incremented_on_failed_release() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    let (client_addr, freelancer_addr, client) = (fx.client, fx.freelancer, fx.escrow);

    // Approving a milestone that was never delivered must fail...
    assert!(client.try_approve_milestone(&client_addr, &0u32).is_err());

    // ...and must not credit anyone on the way out.
    assert_eq!(client.get_reputation(&client_addr), 0);
    assert_eq!(client.get_reputation(&freelancer_addr), 0);
}

#[test]
fn test_get_reputation_uninitialized_returns_error() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_get_reputation(&stranger),
        Err(Ok(Error::NotInitialized))
    );
}

/// Issue #736: every reputation increment publishes a `rep` event carrying
/// the address, the new score and the milestone responsible for the change.
#[test]
fn reputation_change_emits_event_for_both_parties() {
    use crate::ReputationChangedEvent;
    use soroban_sdk::{symbol_short, FromVal, IntoVal, Val};

    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    let (client_addr, freelancer_addr, client, contract_id) = (
        fx.client.clone(),
        fx.freelancer.clone(),
        fx.escrow,
        fx.contract_id,
    );

    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let topic_val: Val = symbol_short!("rep").into_val(&env);
    let events: std::vec::Vec<ReputationChangedEvent> = crate::all_event_tuples(&env)
        .iter()
        .filter(|event| {
            event.0 == contract_id
                && event
                    .1
                    .get(0)
                    .is_some_and(|t| t.get_payload() == topic_val.get_payload())
        })
        .map(|event| ReputationChangedEvent::from_val(&env, &event.2))
        .collect();

    assert_eq!(events.len(), 2, "both parties must emit a rep event");
    for address in [&client_addr, &freelancer_addr] {
        let emitted = events
            .iter()
            .find(|event| &event.address == address)
            .expect("missing reputation event for party");
        assert_eq!(emitted.new_score, 1);
        assert_eq!(emitted.milestone_index, 0);
    }
}
