#![cfg(test)]
//! Checked-arithmetic contract for `payment_streaming_consent` (issue #547).
//!
//! The endpoint multiplies `total_amount × numerator` to scale the ratio before
//! dividing.  On `i128` inputs at the extremes that product does not fit, so
//! the test matrix here pins three things:
//!
//! 1. `i128::MAX` / `i128::MIN` inputs return a **typed** contract error
//!    (`InvalidAmount` / `InvalidRatio`) instead of panicking or wrapping.
//! 2. The largest *representable* product still succeeds — the check is on the
//!    product, not on the magnitude of `total_amount`, so an `i128::MAX` total
//!    paired with a zero numerator is legal and must not be rejected.
//! 3. A rejected call leaves **no** partial write behind.  Validation runs
//!    before `PaymentStreamingExecutionLock` is taken, so the lock is never
//!    written at all and the escrow's storage is unchanged.

use super::*;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::{
    testutils::Address as _, testutils::EnvTestConfig, vec, Address, Env, FromVal, IntoVal, Map,
    Val,
};

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
    escrow: MilestoneEscrowClient<'a>,
}

/// An initialised escrow, so `payment_streaming_consent` has a
/// client/freelancer pair to collect signatures from.
fn initialised_escrow(env: &Env) -> Consent<'_> {
    env.mock_all_auths();

    let admin_addr = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

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
    );

    Consent {
        contract_id,
        client_addr,
        freelancer_addr,
        escrow,
    }
}

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
            .get::<_, bool>(&DataKey::PaymentStreamingExecutionLock)
            .unwrap_or(false)
    })
}

// ── the extremes must return typed errors, never panic or wrap ───────────────

/// `i128::MAX` total with a full ratio: `i128::MAX * 10_000` does not fit, so
/// the scaled product must surface as `InvalidAmount`.
#[test]
fn test_payment_streaming_consent_i128_max_total_returns_typed_overflow_error() {
    let env = test_env();
    let c = initialised_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&i128::MAX, &10_000_i128, &10_000_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

/// `i128::MIN` total is rejected by the sign guard with `InvalidAmount` — the
/// same typed error an overflow produces, never a wrap to a positive value.
#[test]
fn test_payment_streaming_consent_i128_min_total_returns_typed_error() {
    let env = test_env();
    let c = initialised_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&i128::MIN, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

/// `i128::MIN` numerator is rejected by the ratio guard with `InvalidRatio`
/// rather than being multiplied into the product.
#[test]
fn test_payment_streaming_consent_i128_min_numerator_returns_typed_error() {
    let env = test_env();
    let c = initialised_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&1_000_i128, &i128::MIN, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

/// `i128::MIN` denominator is rejected by the ratio guard with `InvalidRatio`.
#[test]
fn test_payment_streaming_consent_i128_min_denominator_returns_typed_error() {
    let env = test_env();
    let c = initialised_escrow(&env);

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&1_000_i128, &0_i128, &i128::MIN),
        Err(Ok(Error::InvalidRatio))
    );
}

// ── the boundary is the product, not the magnitude of the total ──────────────

/// The largest `total_amount` whose product by 10 000 still fits in `i128`.
/// It must succeed and hand the whole total to the streamed leg.
#[test]
fn test_payment_streaming_consent_largest_representable_product_succeeds() {
    let env = test_env();
    let c = initialised_escrow(&env);

    // i128::MAX / 10_000, so * 10_000 is guaranteed <= i128::MAX.
    let total = i128::MAX / 10_000;
    assert!(total.checked_mul(10_000).is_some());

    let split = c
        .escrow
        .payment_streaming_consent(&total, &10_000_i128, &10_000_i128);
    assert_eq!(split.first, total);
    assert_eq!(split.second, 0);
    assert_eq!(split.first + split.second, total);
}

/// One stroop past the boundary the product no longer fits: the typed overflow
/// error is returned at exactly the edge, so the guard is not off by one.
#[test]
fn test_payment_streaming_consent_one_stroop_past_the_boundary_is_rejected() {
    let env = test_env();
    let c = initialised_escrow(&env);

    let total = i128::MAX / 10_000 + 1;
    assert!(total.checked_mul(10_000).is_none());

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&total, &10_000_i128, &10_000_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

/// A zero numerator makes the product zero, so even an `i128::MAX` total is
/// legal. This pins that the check is on the product rather than on the size of
/// `total_amount`, which a naive `i128::MAX` rejection would get wrong.
#[test]
fn test_payment_streaming_consent_i128_max_total_with_zero_numerator_succeeds() {
    let env = test_env();
    let c = initialised_escrow(&env);

    let split = c
        .escrow
        .payment_streaming_consent(&i128::MAX, &0_i128, &10_000_i128);
    assert_eq!(split.first, 0);
    assert_eq!(split.second, i128::MAX);
    assert_eq!(split.first + split.second, i128::MAX);
}

// ── a rejected call must leave no partial write ──────────────────────────────

/// An overflowing call is rejected by `validate_streaming_ratio` *before* the
/// execution lock is taken, so it never writes a single ledger entry — not even
/// the lock it would otherwise have set and cleared.
#[test]
fn test_payment_streaming_consent_overflow_writes_no_ledger_entry() {
    let env = test_env();
    let c = initialised_escrow(&env);

    let before = storage_snapshot(&env, &c.contract_id);
    assert!(!execution_lock_held(&env, &c.contract_id));

    assert_eq!(
        c.escrow
            .try_payment_streaming_consent(&i128::MAX, &10_000_i128, &10_000_i128),
        Err(Ok(Error::InvalidAmount))
    );

    assert!(!execution_lock_held(&env, &c.contract_id));
    let after = storage_snapshot(&env, &c.contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

/// The same guarantee for the sign and ratio rejections, which run through the
/// same pre-lock validation.
#[test]
fn test_payment_streaming_consent_rejected_inputs_write_no_ledger_entry() {
    let env = test_env();
    let c = initialised_escrow(&env);

    let before = storage_snapshot(&env, &c.contract_id);

    let cases = [
        (0_i128, 1_i128, 2_i128, Error::InvalidAmount),
        (-1_i128, 1_i128, 2_i128, Error::InvalidAmount),
        (1_000_i128, 3_i128, 2_i128, Error::InvalidRatio),
        (1_000_i128, 1_i128, 0_i128, Error::InvalidRatio),
    ];
    for (total, num, den, expected) in cases {
        assert_eq!(
            c.escrow.try_payment_streaming_consent(&total, &num, &den),
            Err(Ok(expected))
        );
    }

    let after = storage_snapshot(&env, &c.contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

// ── the gate still never changes the arithmetic ──────────────────────────────

/// The pre-flight overflow probe must not perturb the result: for every input
/// the gated endpoint accepts, it agrees exactly with the unauthenticated
/// calculator, including at the `i128::MAX` boundary.
#[test]
fn test_payment_streaming_consent_matches_calculator_across_boundary_inputs() {
    let env = test_env();
    let c = initialised_escrow(&env);

    let cases = [
        (i128::MAX, 0_i128, 1_i128),
        (i128::MAX / 10_000, 10_000_i128, 10_000_i128),
        (1_000_i128, 300_i128, 600_i128),
        (7_i128, 1_i128, 2_i128),
        (999_i128, 333_i128, 1_000_i128),
    ];

    for (total, num, den) in cases {
        let gated = c.escrow.payment_streaming_consent(&total, &num, &den);
        let plain = c.escrow.payment_streaming_milestones(&total, &num, &den);
        assert_eq!(gated.first, plain.first, "total={total} n={num} d={den}");
        assert_eq!(gated.second, plain.second, "total={total} n={num} d={den}");
        assert_eq!(gated.first + gated.second, total);
    }
}

/// The success path still publishes exactly one consent event naming both
/// signers, so adding the pre-flight probe did not disturb the event stream.
#[test]
fn test_payment_streaming_consent_success_path_still_emits_one_event() {
    let env = test_env();
    let c = initialised_escrow(&env);

    c.escrow
        .payment_streaming_consent(&1_000_i128, &300_i128, &600_i128);

    let topic: Val = soroban_sdk::symbol_short!("p_strcns").into_val(&env);
    let mut matches = 0;
    for e in crate::all_event_tuples(&env).iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                matches += 1;
                let data = PaymentStreamingConsentEvent::from_val(&env, &e.2);
                assert_eq!(data.client, c.client_addr);
                assert_eq!(data.freelancer, c.freelancer_addr);
                assert_eq!(data.total_amount, 1_000);
                assert_eq!(data.streamed_payout, 500);
                assert_eq!(data.client_refund, 500);
            }
        }
    }
    assert_eq!(matches, 1, "expected exactly one p_strcns event");
}
