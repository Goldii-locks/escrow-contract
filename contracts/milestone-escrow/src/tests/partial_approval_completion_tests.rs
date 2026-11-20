#![cfg(test)]
//! #673: approve_milestone must settle the remainder of a partially released
//! milestone in a single call.

use crate::testutils::EscrowFixture;
use crate::{Error, MilestoneStatus};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::Address;

#[test]
fn approve_milestone_completes_partially_released_milestone() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env).milestones(&[100]).funded().build();
    fx.escrow
        .mock_all_auths()
        .mark_delivered(&fx.freelancer, &0);
    fx.escrow
        .mock_all_auths()
        .approve_partial(&fx.client, &0, &30);
    assert_eq!(
        fx.escrow.get_milestone(&0).status,
        MilestoneStatus::PartiallyReleased
    );

    fx.escrow.mock_all_auths().approve_milestone(&fx.client, &0);

    let milestone = fx.escrow.get_milestone(&0);
    assert_eq!(milestone.status, MilestoneStatus::Released);
    assert_eq!(milestone.released_amount, 100);
    let token = fx.token_client();
    assert_eq!(token.balance(&fx.freelancer), 100);
    assert_eq!(token.balance(&fx.contract_id), 0);
}

#[test]
fn approve_milestone_still_rejects_non_deliverable_statuses() {
    for status in [MilestoneStatus::Pending, MilestoneStatus::Released] {
        let env = soroban_sdk::Env::default();
        let fx = EscrowFixture::new(&env).milestones(&[100]).funded().build();
        if status == MilestoneStatus::Released {
            fx.escrow
                .mock_all_auths()
                .mark_delivered(&fx.freelancer, &0);
            fx.escrow.mock_all_auths().approve_milestone(&fx.client, &0);
        }
        assert_eq!(
            fx.escrow
                .mock_all_auths()
                .try_approve_milestone(&fx.client, &0),
            Err(Ok(Error::InvalidStatus)),
            "status {status:?} must remain rejected"
        );
    }
}

#[test]
fn approve_milestone_never_called_by_a_stranger() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env).milestones(&[100]).funded().build();
    fx.escrow
        .mock_all_auths()
        .mark_delivered(&fx.freelancer, &0);
    let stranger = Address::generate(&env);
    assert_eq!(
        fx.escrow
            .mock_all_auths()
            .try_approve_milestone(&stranger, &0),
        Err(Ok(Error::Unauthorized))
    );
}
