#![cfg(test)]
//! Milestone count cap: whole-set iteration must stay inside the tx budget.

use super::*;
use crate::testutils::EscrowFixture;

#[test]
fn initialize_rejects_more_than_max_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).uninitialized().build();
    let repeated = std::vec![1_i128; MAX_MILESTONES as usize + 1];
    let amounts = Vec::from_slice(&env, &repeated);
    let hashes = crate::spec_hashes_for(&amounts);

    assert_eq!(
        fx.escrow.try_initialize(
            &fx.admin,
            &fx.client,
            &fx.freelancer,
            &fx.arbiter,
            &fx.token,
            &604_800u64,
            &amounts,
            &hashes,
        ),
        Err(Ok(Error::InvalidMilestone))
    );
}

#[test]
fn get_job_and_cancel_override_at_cap_fit_budget() {
    let env = Env::default();
    env.mock_all_auths();
    let repeated = std::vec![1_i128; MAX_MILESTONES as usize];
    let amounts = &repeated;
    let fx = EscrowFixture::new(&env).milestones(&amounts).funded().build();

    assert_eq!(fx.escrow.get_job().milestones.len(), MAX_MILESTONES);

    fx.escrow.cancel_escrow(&fx.client);
    fx.escrow.cancel_escrow(&fx.freelancer);
    fx.escrow.admin_override_cancel_refund(&fx.admin);
}
