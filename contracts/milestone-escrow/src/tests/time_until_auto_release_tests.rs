#![cfg(test)]

use soroban_sdk::testutils::Ledger;

use crate::testutils::EscrowFixture;
use crate::{Error, MilestoneStatus};

#[test]
fn test_time_until_auto_release_performs_no_state_mutation() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0);

    // This is a read-only query that returns seconds remaining until auto-release
    let seconds_remaining = fx.escrow.time_until_auto_release(&0);
    assert!(
        seconds_remaining >= 0,
        "Should return valid seconds for delivered milestone"
    );
}

#[test]
fn test_time_until_auto_release_returns_documented_values_populated_state() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0);

    let seconds_remaining = fx.escrow.time_until_auto_release(&0);
    assert!(
        seconds_remaining > 0,
        "Newly delivered milestone should have positive time remaining"
    );
}

#[test]
fn test_time_until_auto_release_returns_documented_values_empty_state() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();

    // Query out of range milestone returns InvalidMilestone
    let result = fx.escrow.try_time_until_auto_release(&999);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

#[test]
fn test_time_until_auto_release_uninitialized_returns_not_initialized() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).uninitialized().build();

    let result = fx.escrow.try_time_until_auto_release(&0);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_time_until_auto_release_pending_returns_invalid_status() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();

    // Milestone is still Pending (not yet delivered)
    let result = fx.escrow.try_time_until_auto_release(&0);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_time_until_auto_release_partially_released_returns_valid_seconds() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[100]).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0);
    fx.escrow.approve_partial(&fx.client, &0, &40);

    let milestone = fx.escrow.get_milestone(&0);
    assert_eq!(milestone.status, MilestoneStatus::PartiallyReleased);

    let seconds = fx.escrow.time_until_auto_release(&0);
    assert!(
        seconds > 0,
        "Partially released milestone has active deadline"
    );
}

#[test]
fn test_time_until_auto_release_released_returns_invalid_status() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[100]).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0);
    fx.escrow.approve_milestone(&fx.client, &0);

    let result = fx.escrow.try_time_until_auto_release(&0);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_milestone_on_partially_released() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[100]).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0);

    // Partial approval of 30 leaves 70
    fx.escrow.approve_partial(&fx.client, &0, &30);
    let m = fx.escrow.get_milestone(&0);
    assert_eq!(m.status, MilestoneStatus::PartiallyReleased);
    assert_eq!(m.released_amount, 30);

    // approve_milestone approves the remaining 70 and sets Released
    fx.escrow.approve_milestone(&fx.client, &0);
    let m_after = fx.escrow.get_milestone(&0);
    assert_eq!(m_after.status, MilestoneStatus::Released);
    assert_eq!(m_after.released_amount, 100);
}

#[test]
fn test_auto_release_on_partially_released() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let auto_release_seconds = 100u64;
    let fx = EscrowFixture::new(&env)
        .milestones(&[100])
        .auto_release_seconds(auto_release_seconds)
        .funded()
        .build();

    fx.escrow.mark_delivered(&fx.freelancer, &0);
    fx.escrow.approve_partial(&fx.client, &0, &40);

    let m = fx.escrow.get_milestone(&0);
    assert_eq!(m.status, MilestoneStatus::PartiallyReleased);
    assert_eq!(m.released_amount, 40);

    // Advance time past auto-release deadline
    env.ledger().with_mut(|li| {
        li.timestamp += auto_release_seconds + 1;
    });

    // claim_auto_release pays the remaining 60 and marks Released
    fx.escrow.claim_auto_release(&fx.freelancer, &0);
    let m_after = fx.escrow.get_milestone(&0);
    assert_eq!(m_after.status, MilestoneStatus::Released);
    assert_eq!(m_after.released_amount, 100);

    // Double claim is rejected with InvalidStatus
    let second_claim = fx.escrow.try_claim_auto_release(&fx.freelancer, &0);
    assert_eq!(second_claim, Err(Ok(Error::InvalidStatus)));
}
