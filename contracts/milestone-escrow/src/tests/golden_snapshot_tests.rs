#![cfg(test)]
//! Golden snapshots: the full ledger, auth and event record of
//! end-to-end flows, committed under `golden_snapshots/`.
//!
//! Soroban's per-test `test_snapshots/` are gitignored. They were ~1,000
//! files that every storage or event change rewrote by the hundred, so
//! reviewers skipped them and they caught nothing. These files cover the
//! main lifecycles instead. Each test rewrites its file on every run,
//! and CI fails when a file differs from the committed one, so any change to
//! what these flows store or emit shows up as a small, reviewable diff.
//! After an intended change, run `cargo test golden_snapshot_tests` and
//! commit the result.

use crate::test::{decision_hash, setup_funded_escrow};
use crate::{FundingMode, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::{Address as _, EnvTestConfig, IssuerFlags, Ledger};
use soroban_sdk::{token, vec, Address, BytesN, Env};

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        // Written explicitly below, to golden_snapshots/ instead.
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

fn write_golden(env: &Env, name: &str) {
    let path = std::format!(
        "{}/golden_snapshots/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    env.to_snapshot_file(path);
}

#[test]
fn golden_fund_deliver_approve() {
    let env = test_env();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);

    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    write_golden(&env, "golden_fund_deliver_approve");
}

#[test]
fn golden_dispute_resolve() {
    let env = test_env();
    let (client, freelancer, arbiter, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);

    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &BytesN::from_array(&env, &[7; 32]));
    escrow.resolve_dispute(&arbiter, &0, &true, &decision_hash(&env));

    write_golden(&env, "golden_dispute_resolve");
}

#[test]
fn golden_cancel_override() {
    let env = test_env();
    let (client, freelancer, _, admin, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);

    escrow.cancel_escrow(&freelancer);
    escrow.cancel_escrow(&client);
    escrow.admin_override_cancel_refund(&admin);

    write_golden(&env, "golden_cancel_override");
}

/// A one-milestone escrow of 1_000 that nobody has accepted or funded yet,
/// over an auth-revocable asset so a payout can be forced into a credit.
/// Returns (client, freelancer, arbiter, token admin, escrow).
fn unaccepted_escrow(
    env: &Env,
    mode: FundingMode,
) -> (
    Address,
    Address,
    Address,
    token::StellarAssetClient<'_>,
    MilestoneEscrowClient<'_>,
) {
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let admin = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    sac.issuer().set_flag(IssuerFlags::RevocableFlag);
    let token_admin = token::StellarAssetClient::new(env, &sac.address());
    token_admin.mint(&client, &1_000);
    let escrow = MilestoneEscrowClient::new(env, &env.register(MilestoneEscrow, ()));
    let amounts = vec![env, 1_000_i128];
    escrow.initialize_with_funding_mode(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &sac.address(),
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        &mode,
    );
    (client, freelancer, arbiter, token_admin, escrow)
}

#[test]
fn golden_revision() {
    let env = test_env();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer, &0);
    escrow.request_revision(&client, &0, &BytesN::from_array(&env, &[5; 32]));
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    write_golden(&env, "golden_revision");
}

#[test]
fn golden_per_milestone_funding() {
    let env = test_env();
    let (client, freelancer, arbiter, _, escrow) =
        unaccepted_escrow(&env, FundingMode::PerMilestone);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);

    escrow.fund_milestone(&client, &0);
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    write_golden(&env, "golden_per_milestone_funding");
}

#[test]
fn golden_overdue_reclaim() {
    let env = test_env();
    let (client, freelancer, arbiter, _, escrow) = unaccepted_escrow(&env, FundingMode::Upfront);
    escrow.set_milestone_due_dates(&client, &vec![&env, 10_000]);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);

    env.ledger()
        .set_timestamp(10_000 + crate::OVERDUE_GRACE_SECONDS + 1);
    escrow.reclaim_overdue(&client, &0);

    write_golden(&env, "golden_overdue_reclaim");
}

#[test]
fn golden_credit_withdraw() {
    let env = test_env();
    let (client, freelancer, arbiter, token_admin, escrow) =
        unaccepted_escrow(&env, FundingMode::Upfront);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);
    escrow.mark_delivered(&freelancer, &0);

    // The payout transfer fails, so it is credited to the freelancer.
    token_admin.set_authorized(&freelancer, &false);
    escrow.approve_milestone(&client, &0);
    token_admin.set_authorized(&freelancer, &true);
    escrow.withdraw(&freelancer);

    write_golden(&env, "golden_credit_withdraw");
}

#[test]
fn golden_withdraw_unaccepted() {
    let env = test_env();
    let (client, _, _, _, escrow) = unaccepted_escrow(&env, FundingMode::Upfront);
    escrow.fund(&client);

    env.ledger()
        .set_timestamp(env.ledger().timestamp() + crate::ACCEPTANCE_WINDOW_SECONDS);
    escrow.withdraw_unaccepted(&client);

    write_golden(&env, "golden_withdraw_unaccepted");
}

#[test]
fn golden_escalate_after_timeout() {
    let env = test_env();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &BytesN::from_array(&env, &[7; 32]));
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + crate::DISPUTE_TIMEOUT_SECONDS);
    escrow.escalate_dispute(&client, &0);

    write_golden(&env, "golden_escalate_after_timeout");
}
