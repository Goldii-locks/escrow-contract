#![cfg(test)]
//! Atomic deployment through `__constructor` (issue #665).
//!
//! Unit-test builds of the escrow use a no-argument constructor so the rest
//! of the suite can register the contract and call the test-only
//! `initialize`. These tests deploy `testdata/escrow_v2.wasm` instead: it is
//! a release build that links the whole escrow, so it carries the real
//! constructor.

use super::*;
use crate::{EscrowSettings, FundingMode};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, Address, BytesN, Env, Vec};

const RELEASE_WASM: &[u8] = include_bytes!("../../testdata/escrow_v2.wasm");

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    // Uploading a whole escrow wasm exceeds the default per-invocation write
    // limit of the test env.
    env.cost_estimate().disable_resource_limits();
    env
}

fn default_settings() -> EscrowSettings {
    EscrowSettings::default()
}

struct Parties {
    admin: Address,
    client: Address,
    freelancer: Address,
    arbiter: Address,
    token: Address,
}

fn parties(env: &Env) -> Parties {
    let admin = Address::generate(env);
    Parties {
        client: Address::generate(env),
        freelancer: Address::generate(env),
        arbiter: Address::generate(env),
        token: env
            .register_stellar_asset_contract_v2(admin.clone())
            .address(),
        admin,
    }
}

fn deploy(env: &Env, p: &Parties, amounts: &Vec<i128>, settings: EscrowSettings) -> Address {
    env.register(
        RELEASE_WASM,
        (
            p.admin.clone(),
            p.client.clone(),
            p.freelancer.clone(),
            p.arbiter.clone(),
            p.token.clone(),
            604_800u64,
            amounts.clone(),
            crate::test_spec_hashes(env, amounts),
            settings,
        ),
    )
}

#[test]
fn deploy_with_constructor_args_succeeds_and_asserts_stored_state() {
    let env = test_env();
    env.mock_all_auths_allowing_non_root_auth();
    let p = parties(&env);
    let amounts: Vec<i128> = vec![&env, 400_i128, 600_i128];
    let spec_hashes: Vec<BytesN<32>> = crate::test_spec_hashes(&env, &amounts);

    let escrow = MilestoneEscrowClient::new(&env, &deploy(&env, &p, &amounts, default_settings()));
    let job = escrow.get_job();

    assert_eq!(job.client, p.client);
    assert_eq!(job.freelancer, p.freelancer);
    assert_eq!(job.arbiter, p.arbiter);
    assert_eq!(job.token, p.token);
    assert_eq!(job.auto_release_seconds, 604_800);
    assert_eq!(job.milestones.len(), 2);
    assert_eq!(job.milestones.get(0).unwrap().amount, 400_i128);
    assert_eq!(job.milestones.get(1).unwrap().amount, 600_i128);
    assert!(!job.funded);

    // The checked-in release WASM still exposes the legacy u32 `version`.
    let legacy_version = env.try_invoke_contract::<u32, soroban_sdk::Error>(
        &escrow.address,
        &soroban_sdk::Symbol::new(&env, "version"),
        vec![&env],
    );
    assert_eq!(legacy_version, Ok(Ok(1)));
    assert_eq!(escrow.get_funding_mode(), FundingMode::Upfront);
    assert_eq!(escrow.get_arbiter_fee_bps(), 0);
    assert!(escrow.get_whitelisted_tokens().contains(&p.token));
    assert_eq!(
        escrow.get_milestone_spec_hash(&0),
        spec_hashes.get(0).unwrap()
    );
    assert_eq!(
        escrow.get_milestone_spec_hash(&1),
        spec_hashes.get(1).unwrap()
    );
}

#[test]
fn constructor_applies_settings() {
    let env = test_env();
    env.mock_all_auths_allowing_non_root_auth();
    let p = parties(&env);
    let amounts: Vec<i128> = vec![&env, 1_000_i128];

    let escrow = MilestoneEscrowClient::new(
        &env,
        &deploy(
            &env,
            &p,
            &amounts,
            EscrowSettings {
                funding_mode: FundingMode::PerMilestone,
                dispute_timeout_seconds: 2 * 24 * 60 * 60,
                arbiter_fee_bps: 500,
                ..default_settings()
            },
        ),
    );

    assert_eq!(escrow.get_funding_mode(), FundingMode::PerMilestone);
    assert_eq!(escrow.get_arbiter_fee_bps(), 500);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn constructor_rejects_an_arbiter_fee_above_the_cap() {
    let env = test_env();
    env.mock_all_auths_allowing_non_root_auth();
    let p = parties(&env);
    let amounts: Vec<i128> = vec![&env, 1_000_i128];
    deploy(
        &env,
        &p,
        &amounts,
        EscrowSettings {
            arbiter_fee_bps: 1_001,
            ..default_settings()
        },
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #44)")]
fn constructor_rejects_invalid_parties() {
    let env = test_env();
    env.mock_all_auths_allowing_non_root_auth();
    let mut p = parties(&env);
    p.arbiter = p.client.clone();
    let amounts: Vec<i128> = vec![&env, 1_000_i128];
    deploy(&env, &p, &amounts, default_settings());
}

#[test]
#[should_panic(expected = "constructor invocation has failed")]
fn deploy_without_constructor_args_fails() {
    let env = test_env();
    env.register(RELEASE_WASM, ());
}
