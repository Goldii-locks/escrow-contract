#![cfg(test)]
//! Issue #923: `extend_milestone_deadline` and `rotate_freelancer` must honour
//! the full pause guard (emergency pause and mutual cancel lock).

use crate::test::setup_funded_escrow;
use crate::Error;
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

fn setup(env: &Env) -> (Address, Address, Address, crate::MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let (client, freelancer, _, admin, _, _, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    (client, freelancer, admin, escrow)
}

#[test]
fn deadline_extension_is_blocked_by_emergency_pause() {
    let env = Env::default();
    let (client, _, admin, escrow) = setup(&env);
    escrow.emergency_pause_admin_override(&admin);
    assert_eq!(
        escrow.try_extend_milestone_deadline(&client, &0, &60),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn deadline_extension_is_blocked_by_cancel_lock() {
    let env = Env::default();
    let (client, freelancer, _, escrow) = setup(&env);
    escrow.cancel_escrow(&client);
    escrow.cancel_escrow(&freelancer);
    assert_eq!(
        escrow.try_extend_milestone_deadline(&client, &0, &60),
        Err(Ok(Error::EscrowLocked))
    );
}

#[test]
fn freelancer_rotation_is_blocked_by_emergency_pause() {
    let env = Env::default();
    let (_, _, admin, escrow) = setup(&env);
    escrow.emergency_pause_admin_override(&admin);
    assert_eq!(
        escrow.try_rotate_freelancer(&Address::generate(&env)),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn freelancer_rotation_is_blocked_by_cancel_lock() {
    let env = Env::default();
    let (client, freelancer, _, escrow) = setup(&env);
    escrow.cancel_escrow(&client);
    escrow.cancel_escrow(&freelancer);
    assert_eq!(
        escrow.try_rotate_freelancer(&Address::generate(&env)),
        Err(Ok(Error::EscrowLocked))
    );
}
