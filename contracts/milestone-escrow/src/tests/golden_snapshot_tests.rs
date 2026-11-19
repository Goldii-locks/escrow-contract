#![cfg(test)]
//! Golden snapshots: the full ledger, auth and event record of three
//! end-to-end flows, committed under `golden_snapshots/`.
//!
//! Soroban's per-test `test_snapshots/` are gitignored. They were ~1,000
//! files that every storage or event change rewrote by the hundred, so
//! reviewers skipped them and they caught nothing. These three files cover
//! the main lifecycles instead. Each test rewrites its file on every run,
//! and CI fails when a file differs from the committed one, so any change to
//! what these flows store or emit shows up as a small, reviewable diff.
//! After an intended change, run `cargo test golden_snapshot_tests` and
//! commit the result.

use crate::test::{decision_hash, setup_funded_escrow};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, BytesN, Env};

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
