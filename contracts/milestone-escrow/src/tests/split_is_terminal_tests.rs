#![cfg(test)]
//! A milestone settled by `apply_dispute_arbitration_split` is terminal:
//! its funds have left the contract, so no override may pay it out again.
//!
//! The admin overrides used to treat only `Released` and `Refunded` as
//! settled, so overriding a `Split` milestone paid its full remaining amount
//! a second time, out of the other milestones' funds. Found by
//! `conservation_proptests` (deliver, dispute, split, admin override release).

use super::*;
use crate::{Error, MilestoneStatus};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, BytesN, Env};

fn split_milestone_zero(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    let (client_addr, freelancer, arbiter, admin, token_id, _, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128, 1_000]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client_addr, &0, &BytesN::from_array(env, &[1; 32]));
    escrow.apply_dispute_arbitration_split(&arbiter, &0, &4_000, &decision_hash(env));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Split
    );
    (admin, token_id, escrow)
}

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

#[test]
fn admin_override_release_rejects_a_split_milestone() {
    let env = test_env();
    let (admin, token_id, escrow) = split_milestone_zero(&env);
    let held = token::Client::new(&env, &token_id).balance(&escrow.address);

    assert_eq!(
        escrow.try_admin_override_release(&admin, &0),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&escrow.address),
        held
    );
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Split
    );
}

#[test]
fn admin_override_refund_rejects_a_split_milestone() {
    let env = test_env();
    let (admin, token_id, escrow) = split_milestone_zero(&env);
    let held = token::Client::new(&env, &token_id).balance(&escrow.address);

    assert_eq!(
        escrow.try_admin_override_refund(&admin, &0),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&escrow.address),
        held
    );
}
