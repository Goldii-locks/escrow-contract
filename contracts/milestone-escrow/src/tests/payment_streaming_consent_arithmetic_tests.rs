#![cfg(test)]
//! Checked-arithmetic contract for `payment_streaming_consent` (issue #695).
//!
//! `payment_streaming_consent` now transfers tokens: `numerator/denominator`
//! of a milestone's remaining balance is released to the freelancer with both
//! parties' consent.  This suite pins:
//!
//! 1. Invalid ratio inputs return typed errors (`InvalidRatio`).
//! 2. A rejected call leaves no partial write behind.
//! 3. The success path transfers tokens and emits exactly one event.

use super::*;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::{testutils::EnvTestConfig, token, vec, Address, Env, FromVal, IntoVal, Map, Val};

/// Snapshot capture is disabled so this suite does not write a JSON file per
/// test into `test_snapshots/`; none of these assertions read snapshots.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

struct Consent<'a> {
    contract_id: Address,
    client_addr: Address,
    freelancer_addr: Address,
    token: Address,
    escrow: MilestoneEscrowClient<'a>,
}

/// A funded escrow with a delivered milestone so
/// `payment_streaming_consent` has a milestone to stream from.
fn delivered_escrow(env: &Env) -> Consent<'_> {
    env.mock_all_auths();

    let admin_addr = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token);
    token_admin.mint(&client_addr, &1_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token,
        &604_800u64,
        &vec![env, 1_000_i128],
        &crate::test_spec_hashes(env, &vec![env, 1_000_i128]),
    );
    escrow.accept_job(&freelancer_addr);
    escrow.accept_arbiter(&arbiter_addr);
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer_addr, &0);

    Consent {
        contract_id,
        client_addr,
        freelancer_addr,
        token,
        escrow,
    }
}

/// Every ledger entry the contract holds, in all three storage tiers.
#[allow(clippy::type_complexity)]
fn storage_snapshot(
    env: &Env,
    contract_id: &Address,
) -> (Map<Val, Val>, Map<Val, Val>, Map<Val, Val>) {
    env.as_contract(contract_id, || {
        (
            env.storage().instance().all(),
            env.storage().persistent().all(),
            env.storage().temporary().all(),
        )
    })
}

fn execution_lock_held(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::Paused)
            .unwrap_or(false)
    })
}

// ── ratio validation must return typed errors ───────────────────────────────

/// `i128::MIN` numerator is rejected by the ratio guard with `InvalidRatio`
/// rather than being multiplied into the product.
#[test]
fn test_payment_streaming_consent_min_numerator_returns_typed_error() {
    let env = test_env();
    let c = delivered_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&0u32, &i128::MIN, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

/// `i128::MIN` denominator is rejected by the ratio guard with `InvalidRatio`.
#[test]
fn test_payment_streaming_consent_min_denominator_returns_typed_error() {
    let env = test_env();
    let c = delivered_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&0u32, &0_i128, &i128::MIN),
        Err(Ok(Error::InvalidRatio))
    );
}

/// A numerator greater than the denominator is rejected with `InvalidRatio`.
#[test]
fn test_payment_streaming_consent_numerator_exceeds_denominator_returns_error() {
    let env = test_env();
    let c = delivered_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&0u32, &3_i128, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

// ── a rejected call must leave no partial write ──────────────────────────────

/// An invalid ratio call is rejected *before* the execution lock is taken, so
/// it never writes a single ledger entry.
#[test]
fn test_payment_streaming_consent_rejected_ratio_writes_no_ledger_entry() {
    let env = test_env();
    let c = delivered_escrow(&env);

    let before = storage_snapshot(&env, &c.contract_id);
    assert!(!execution_lock_held(&env, &c.contract_id));

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&0u32, &3_i128, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );

    assert!(!execution_lock_held(&env, &c.contract_id));
    let after = storage_snapshot(&env, &c.contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

/// The same guarantee for multiple rejected inputs.
#[test]
fn test_payment_streaming_consent_rejected_inputs_write_no_ledger_entry() {
    let env = test_env();
    let c = delivered_escrow(&env);

    let before = storage_snapshot(&env, &c.contract_id);

    let cases = [
        (3_i128, 2_i128, Error::InvalidRatio),
        (1_i128, 0_i128, Error::InvalidRatio),
        (-1_i128, 2_i128, Error::InvalidRatio),
    ];
    for (num, den, expected) in cases {
        assert_eq!(
            c.escrow.try_payment_streaming_consent(&0u32, &num, &den),
            Err(Ok(expected))
        );
    }

    let after = storage_snapshot(&env, &c.contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

// ── streaming transfers tokens and emits an event ───────────────────────────

/// A half-stream (num=1, den=2) releases 500 to the freelancer and keeps 500
/// in escrow.  The milestone transitions to `PartiallyReleased`.
#[test]
fn test_payment_streaming_consent_streams_half_and_transfers_tokens() {
    let env = test_env();
    let c = delivered_escrow(&env);

    let token_client = soroban_sdk::token::Client::new(&env, &c.token);
    let contract_balance_before = token_client.balance(&c.contract_id);
    let freelancer_balance_before = token_client.balance(&c.freelancer_addr);

    let split = c.escrow.payment_streaming_consent(&0u32, &1_i128, &2_i128);

    assert_eq!(split.first, 500);
    assert_eq!(split.second, 500);

    let contract_balance_after = token_client.balance(&c.contract_id);
    let freelancer_balance_after = token_client.balance(&c.freelancer_addr);

    assert_eq!(contract_balance_after, contract_balance_before - 500);
    assert_eq!(freelancer_balance_after, freelancer_balance_before + 500);

    let milestone = c.escrow.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.released_amount, 500);
}

/// A full stream (num=den) releases the entire remaining balance and
/// transitions the milestone to `Released`.
#[test]
fn test_payment_streaming_consent_full_stream_releases_milestone() {
    let env = test_env();
    let c = delivered_escrow(&env);

    let split = c.escrow.payment_streaming_consent(&0u32, &1_i128, &1_i128);

    assert_eq!(split.first, 1_000);
    assert_eq!(split.second, 0);

    let milestone = c.escrow.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.released_amount, 1_000);
    assert_eq!(milestone.status, MilestoneStatus::Released);
}

/// A zero-numerator stream releases nothing and the milestone stays
/// `Delivered`.
#[test]
fn test_payment_streaming_consent_zero_numerator_streams_nothing() {
    let env = test_env();
    let c = delivered_escrow(&env);

    let split = c.escrow.payment_streaming_consent(&0u32, &0_i128, &1_i128);

    assert_eq!(split.first, 0);
    assert_eq!(split.second, 1_000);

    let milestone = c.escrow.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.released_amount, 0);
}

/// The success path publishes exactly one consent event naming both signers.
#[test]
fn test_payment_streaming_consent_success_path_emits_one_event() {
    let env = test_env();
    let c = delivered_escrow(&env);

    c.escrow
        .payment_streaming_consent(&0u32, &300_i128, &600_i128);

    let topic: Val = soroban_sdk::symbol_short!("p_strcns").into_val(&env);
    let mut matches = 0;
    for e in crate::all_event_tuples(&env).iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                matches += 1;
                let data = PaymentStreamingConsentEvent::from_val(&env, &e.2);
                assert_eq!(data.client, c.client_addr);
                assert_eq!(data.freelancer, c.freelancer_addr);
                assert_eq!(data.milestone_index, 0);
                assert_eq!(data.total_amount, 1_000);
                assert_eq!(data.streamed_payout, 500);
                assert_eq!(data.client_refund, 500);
            }
        }
    }
    assert_eq!(matches, 1, "expected exactly one p_strcns event");
}

/// Calling on an undelivered milestone is rejected with `InvalidStatus`.
#[test]
fn test_payment_streaming_consent_rejects_pending_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token);
    token_admin.mint(&client_addr, &1_000_i128);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token,
        &604_800u64,
        &vec![&env, 1_000_i128],
        &crate::spec_hashes_for(&vec![&env, 1_000_i128]),
    );
    escrow.accept_job(&freelancer_addr);
    escrow.accept_arbiter(&arbiter_addr);
    escrow.fund(&client_addr);

    assert_eq!(
        escrow.try_payment_streaming_consent(&0u32, &1_i128, &2_i128),
        Err(Ok(Error::InvalidStatus))
    );
}

/// An out-of-range milestone index is rejected with `InvalidMilestone`.
#[test]
fn test_payment_streaming_consent_rejects_invalid_milestone() {
    let env = test_env();
    let c = delivered_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&1u32, &1_i128, &2_i128),
        Err(Ok(Error::InvalidMilestone))
    );
}
