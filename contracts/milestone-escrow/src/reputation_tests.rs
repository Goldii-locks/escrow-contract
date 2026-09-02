#![cfg(test)]
//! Unit tests for `get_reputation` and the `increment_reputation` bookkeeping
//! behind it.
//!
//! Reputation is bumped for *both* parties every time a milestone reaches
//! `Released`, through whichever path got it there. The endpoint had no
//! coverage at all: the two tests that exercised it were removed when
//! `test.rs` was rewritten, and nothing replaced them, leaving a public
//! contract function untested while its snapshots stayed behind.

use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{vec, Address, Env};

#[test]
fn test_reputation_tracking() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 500_i128]);

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

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

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

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

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

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Approving a milestone that was never delivered must fail...
    assert!(client.try_approve_milestone(&client_addr, &0u32).is_err());

    // ...and must not credit anyone on the way out.
    assert_eq!(client.get_reputation(&client_addr), 0);
    assert_eq!(client.get_reputation(&freelancer_addr), 0);
}
