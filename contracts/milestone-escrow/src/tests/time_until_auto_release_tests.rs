#![cfg(test)]

use super::*;
use crate::testutils::EscrowFixture;

#[test]
fn uninitialised_returns_not_initialized() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env).uninitialized().build();
    assert_eq!(
        fx.escrow.try_time_until_auto_release(&0),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn out_of_range_index_returns_invalid_milestone() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();
    assert_eq!(
        fx.escrow.try_time_until_auto_release(&999),
        Err(Ok(Error::InvalidMilestone))
    );
}

#[test]
fn pending_milestone_returns_invalid_status() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();
    assert_eq!(
        fx.escrow.try_time_until_auto_release(&0),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn delivered_milestone_returns_positive_seconds() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0);

    let seconds_remaining = fx.escrow.time_until_auto_release(&0);
    assert!(seconds_remaining > 0, "expected positive seconds remaining");
}

#[test]
fn test_time_until_auto_release_performs_no_state_mutation() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).milestones(&[101]).funded().build();
    fx.escrow.mark_delivered(&fx.freelancer, &0);

    let before = env.to_ledger_snapshot();
    let _ = fx.escrow.time_until_auto_release(&0);
    assert_eq!(before, env.to_ledger_snapshot());
}
