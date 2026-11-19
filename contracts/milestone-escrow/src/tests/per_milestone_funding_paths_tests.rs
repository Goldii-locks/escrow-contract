#![cfg(test)]
//! Issue #921: per-milestone jobs must check the milestone's funding flag,
//! not the job-level `funded` flag, in evidence / escalation / revision.

use crate::{FundingMode, MilestoneEscrow, MilestoneEscrowClient, MilestoneStatus, DISPUTE_TIMEOUT_SECONDS};
use soroban_sdk::{testutils::{Address as _, Ledger}, token, vec, Address, BytesN, Env};

fn setup(env: &Env) -> (Address, Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let admin = Address::generate(env);
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    token::StellarAssetClient::new(env, &token).mint(&client, &1_000);
    let amounts = vec![env, 400_i128, 600];
    let escrow = MilestoneEscrowClient::new(env, &env.register(MilestoneEscrow, ()));
    escrow.initialize_with_funding_mode(
        &admin, &client, &freelancer, &arbiter, &token, &604_800u64,
        &amounts, &crate::spec_hashes_for(&amounts), &FundingMode::PerMilestone,
    );
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund_milestone(&client, &0);
    assert!(!escrow.get_job().funded);
    (client, freelancer, arbiter, escrow)
}

fn hash(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

#[test]
fn evidence_submission_works_on_partially_funded_job() {
    let env = Env::default();
    let (client, freelancer, _, escrow) = setup(&env);
    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &hash(&env, 1));
    escrow.submit_evidence(&client, &0, &hash(&env, 2));
    assert_eq!(escrow.get_dispute_info(&0).evidence.len(), 1);
}

#[test]
fn escalation_works_on_partially_funded_job() {
    let env = Env::default();
    let (client, freelancer, _, escrow) = setup(&env);
    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &hash(&env, 1));
    env.ledger().with_mut(|l| l.timestamp += DISPUTE_TIMEOUT_SECONDS + 1);
    escrow.escalate_dispute(&client, &0);
    assert_eq!(escrow.get_milestone(&0).status, MilestoneStatus::Delivered);
}

#[test]
fn revision_request_works_on_partially_funded_job() {
    let env = Env::default();
    let (client, freelancer, _, escrow) = setup(&env);
    escrow.mark_delivered(&freelancer, &0);
    escrow.request_revision(&client, &0, &hash(&env, 3));
    assert_eq!(escrow.get_milestone(&0).status, MilestoneStatus::Pending);
    assert_eq!(escrow.get_revision_count(&0), 1);
}
