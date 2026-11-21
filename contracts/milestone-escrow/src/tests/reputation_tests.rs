#![cfg(test)]
//! Unit tests for `get_reputation` and the `increment_reputation` bookkeeping
//! behind it.
//!
//! Semantics (issue #952, see "Reputation" in `SPEC.md`): the freelancer is
//! credited every time a milestone reaches `Released` with the whole amount
//! paid to them, through whichever path got it there. The client is credited
//! only when the client explicitly approved that release (`approve_milestone`,
//! `approve_milestones`, the final `approve_partial`, or a co-signed
//! `payment_streaming_consent`), never for an overdue auto-release, a dispute
//! the arbiter decided for the freelancer, or an admin override.

use crate::test::{decision_hash, dispute_reason_hash};
use crate::testutils::EscrowFixture;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{vec, Address, Env};

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

    // Issue #952: an auto-release pays the freelancer because the client did
    // *not* act, so only the freelancer is credited.
    assert_eq!(client.get_reputation(&client_addr), 0);
    assert_eq!(client.get_reputation(&freelancer_addr), 1);
}

#[test]
fn trigger_auto_release_credits_only_the_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0u32);
    env.ledger().with_mut(|l| {
        l.timestamp += 604_800 + 1;
    });

    // Anyone may trigger it; the caller is recorded, not authenticated.
    fx.escrow
        .trigger_auto_release(&Address::generate(&env), &0u32);

    assert_eq!(fx.escrow.get_reputation(&fx.client), 0);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 1);
}

#[test]
fn lost_dispute_credits_only_the_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0u32);
    fx.escrow
        .raise_dispute(&fx.client, &0u32, &dispute_reason_hash(&env));
    // The arbiter releases to the freelancer: the client lost the dispute.
    fx.escrow
        .resolve_dispute(&fx.arbiter, &0u32, &true, &decision_hash(&env));

    assert_eq!(fx.escrow.get_reputation(&fx.client), 0);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 1);
}

#[test]
fn dispute_refunded_to_the_client_credits_nobody() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0u32);
    fx.escrow
        .raise_dispute(&fx.client, &0u32, &dispute_reason_hash(&env));
    fx.escrow
        .resolve_dispute(&fx.arbiter, &0u32, &false, &decision_hash(&env));

    assert_eq!(fx.escrow.get_reputation(&fx.client), 0);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 0);
}

#[test]
fn final_partial_approval_credits_the_client() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0u32);

    // A partial approval releases money but not the milestone: no credit yet.
    fx.escrow.approve_partial(&fx.client, &0u32, &400_i128);
    assert_eq!(fx.escrow.get_reputation(&fx.client), 0);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 0);

    // The approval that releases the rest is the explicit final approval.
    fx.escrow.approve_partial(&fx.client, &0u32, &600_i128);
    assert_eq!(fx.escrow.get_reputation(&fx.client), 1);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 1);
}

#[test]
fn batch_approval_credits_the_client_per_milestone() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env)
        .milestones(&[1_000_i128, 500_i128])
        .funded()
        .build();
    fx.escrow.mark_delivered(&fx.freelancer, &0u32);
    fx.escrow.mark_delivered(&fx.freelancer, &1u32);

    fx.escrow
        .approve_milestones(&fx.client, &vec![&env, 0u32, 1u32]);

    assert_eq!(fx.escrow.get_reputation(&fx.client), 2);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 2);
}

/// An approval after an auto-release on another milestone still credits the
/// client for the approval alone.
#[test]
fn client_score_counts_only_the_approvals() {
    let env = Env::default();
    env.mock_all_auths();

    let fx = EscrowFixture::new(&env)
        .milestones(&[1_000_i128, 500_i128])
        .funded()
        .build();
    fx.escrow.mark_delivered(&fx.freelancer, &0u32);
    env.ledger().with_mut(|l| {
        l.timestamp += 604_800 + 1;
    });
    fx.escrow.claim_auto_release(&fx.freelancer, &0u32);

    fx.escrow.mark_delivered(&fx.freelancer, &1u32);
    fx.escrow.approve_milestone(&fx.client, &1u32);

    assert_eq!(fx.escrow.get_reputation(&fx.client), 1);
    assert_eq!(fx.escrow.get_reputation(&fx.freelancer), 2);
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
