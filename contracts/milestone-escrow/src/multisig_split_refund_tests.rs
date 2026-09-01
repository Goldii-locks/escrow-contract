//! Boundary and extreme-value tests for `multisig_split_refund`.
//!
//! Every arithmetic path inside `multisig_split_refund` (and its shared
//! `split_round_nearest` helper) uses checked operations.  This suite
//! verifies that no combination of `i128::MAX`, `i128::MIN`, or negative
//! `total_amount` can cause a wrap or panic: all such inputs must return
//! `Err(Error::InvalidAmount)`.  Happy-path cases with valid extreme amounts
//! must produce results that are identical to those expected by the checked
//! arithmetic.
//!
//! Layout
//! ──────
//! 1. Auth / source-state guards — must reject before any arithmetic runs.
//! 2. `total_amount` boundary guards — negative, zero, `i128::MIN`.
//! 3. Overflow cases — inputs that would wrap under unchecked arithmetic.
//! 4. Edge cases that succeed — `total_amount = i128::MAX` with a 0-bps
//!    numerator (avoids the `checked_mul` overflow path).
//! 5. Valid-amount regression — confirms existing happy-path outputs are
//!    unchanged after the checked-arithmetic conversion.
//! 6. Event emission — a successful call emits exactly one `"splitref"` event
//!    with matching payload.

use super::*;
use crate::{Error, SplitRefundCalculatedEvent};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Count how many events in the environment carry the `"splitref"` topic.
fn splitref_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("splitref").into_val(env);
    let mut count = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

/// Extract the payload of the last `"splitref"` event.
fn last_splitref_event(env: &Env) -> SplitRefundCalculatedEvent {
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, "splitref"));
    SplitRefundCalculatedEvent::from_val(env, &last.2)
}

// ── 1. Auth / source-state guards ────────────────────────────────────────────

/// An attacker (non-admin address) must receive `Unauthorized` before any
/// arithmetic is attempted, even with an otherwise valid payload.
#[test]
fn split_refund_unauthorized_caller_returns_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let attacker = Address::generate(&env);
    let result =
        client.try_multisig_split_refund(&attacker, &i128::MAX, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    assert_eq!(splitref_event_count(&env), 0);
}

/// When the multisig lock is not active the call must be rejected with
/// `InvalidStatus`, regardless of the `total_amount` supplied.
#[test]
fn split_refund_unlocked_source_state_returns_invalid_status() {
    let env = Env::default();
    env.mock_all_auths();

    // Escrow is funded but NOT locked.
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let result =
        client.try_multisig_split_refund(&admin_addr, &i128::MAX, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(splitref_event_count(&env), 0);
}

// ── 2. `total_amount` boundary guards ────────────────────────────────────────

/// `i128::MIN` is negative, so the `total_amount <= 0` guard fires first,
/// returning `InvalidAmount` without any multiplication being attempted.
#[test]
fn split_refund_i128_min_returns_invalid_amount_without_panic() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let result =
        client.try_multisig_split_refund(&admin_addr, &i128::MIN, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(splitref_event_count(&env), 0);
}

/// Any negative `total_amount` is rejected with `InvalidAmount`.
#[test]
fn split_refund_negative_amount_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    for amount in [-1_i128, -100_i128, i128::MIN + 1, i128::MIN] {
        let result =
            client.try_multisig_split_refund(&admin_addr, &amount, &5_000_u32, &5_000_u32);
        assert_eq!(result, Err(Ok(Error::InvalidAmount)), "amount = {amount}");
    }
    assert_eq!(splitref_event_count(&env), 0);
}

/// Zero `total_amount` is rejected with `InvalidAmount`.
#[test]
fn split_refund_zero_amount_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let result =
        client.try_multisig_split_refund(&admin_addr, &0_i128, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(splitref_event_count(&env), 0);
}

// ── 3. Overflow cases ─────────────────────────────────────────────────────────

/// `i128::MAX` with a non-zero `client_refund_bps` triggers `checked_mul`
/// overflow inside `split_round_nearest` and must return `InvalidAmount`
/// rather than wrapping or panicking.
///
/// Tested across all non-trivial BPS pairs that sum to `BPS_SCALE`:
///   • 5_000 / 5_000  — balanced split
///   • 9_999 / 1      — extreme client bias
///   • 1 / 9_999      — extreme freelancer bias (numerator = 1, scaled =
///                       i128::MAX; adding half = 5_000 then overflows)
///   • 10_000 / 0     — 100 % client: scaled = i128::MAX × 10_000 overflows
#[test]
fn split_refund_i128_max_with_nonzero_client_bps_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    // 50 / 50 — scaled = i128::MAX × 5_000 overflows
    let r = client.try_multisig_split_refund(&admin_addr, &i128::MAX, &5_000_u32, &5_000_u32);
    assert_eq!(r, Err(Ok(Error::InvalidAmount)));

    // 9_999 / 1 — scaled = i128::MAX × 9_999 overflows
    let r = client.try_multisig_split_refund(&admin_addr, &i128::MAX, &9_999_u32, &1_u32);
    assert_eq!(r, Err(Ok(Error::InvalidAmount)));

    // 1 / 9_999 — scaled = i128::MAX × 1 = i128::MAX; adding half (5_000) overflows
    let r = client.try_multisig_split_refund(&admin_addr, &i128::MAX, &1_u32, &9_999_u32);
    assert_eq!(r, Err(Ok(Error::InvalidAmount)));

    // 10_000 / 0 — scaled = i128::MAX × 10_000 overflows
    let r = client.try_multisig_split_refund(&admin_addr, &i128::MAX, &10_000_u32, &0_u32);
    assert_eq!(r, Err(Ok(Error::InvalidAmount)));

    assert_eq!(splitref_event_count(&env), 0);
}

/// `u32` BPS values that individually fit in u32 but sum above `u32::MAX`
/// are rejected by the `checked_add` on the BPS pair before any i128 math
/// runs.  Because the sum overflows u32, the result cannot equal `BPS_SCALE`
/// (10_000) even if it were to wrap, so the checked variant correctly returns
/// `InvalidRatio`.
///
/// Note: u32::MAX = 4_294_967_295; u32::MAX + u32::MAX wraps to
/// 4_294_967_294 under unchecked arithmetic, which is not BPS_SCALE.
#[test]
fn split_refund_bps_u32_overflow_returns_invalid_ratio() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let result = client.try_multisig_split_refund(
        &admin_addr,
        &1_000_i128,
        &u32::MAX,
        &u32::MAX,
    );
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(splitref_event_count(&env), 0);
}

// ── 4. Extreme amounts that succeed ──────────────────────────────────────────

/// When `client_refund_bps = 0` the numerator passed to `split_round_nearest`
/// is 0, so `checked_mul(0) = 0` never overflows.  Even with `i128::MAX` as
/// `total_amount` this path completes successfully: the client receives 0 and
/// the freelancer receives `i128::MAX`.
#[test]
fn split_refund_i128_max_with_zero_client_bps_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let allocation = client.multisig_split_refund(
        &admin_addr,
        &i128::MAX,
        &0_u32,
        &10_000_u32,
    );
    assert_eq!(allocation.client_refund, 0);
    assert_eq!(allocation.freelancer_payout, i128::MAX);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        i128::MAX
    );
    assert_eq!(splitref_event_count(&env), 1);
}

/// Large but non-overflowing amount: `i64::MAX` as `total_amount` with a
/// 50/50 split.  `i64::MAX × 5_000` fits in i128, so the call succeeds and
/// the two halves sum exactly to the input.
#[test]
fn split_refund_i64_max_even_split_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let amount = i64::MAX as i128;
    let allocation =
        client.multisig_split_refund(&admin_addr, &amount, &5_000_u32, &5_000_u32);

    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        amount,
        "halves must sum to the original amount"
    );
    assert!(
        (allocation.client_refund - allocation.freelancer_payout).abs() <= 1,
        "halves must differ by at most 1 (rounding)"
    );
    assert_eq!(splitref_event_count(&env), 1);
}

// ── 5. Valid-amount regression ────────────────────────────────────────────────

/// 50 / 50 split of 1_000 produces 500 / 500 — identical to the pre-existing
/// `test_multisig_split_refund_even_split` expectation.
#[test]
fn split_refund_regression_even_split_unchanged() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let allocation =
        client.multisig_split_refund(&admin_addr, &1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund, 500);
    assert_eq!(allocation.freelancer_payout, 500);
    assert_eq!(allocation.client_refund_bps, 5_000);
    assert_eq!(allocation.freelancer_payout_bps, 5_000);
    assert_eq!(allocation.client_refund + allocation.freelancer_payout, 1_000);
}

/// 70 / 30 split of 1_000 produces 700 / 300.
#[test]
fn split_refund_regression_uneven_split_unchanged() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let allocation =
        client.multisig_split_refund(&admin_addr, &1_000_i128, &7_000_u32, &3_000_u32);
    assert_eq!(allocation.client_refund, 700);
    assert_eq!(allocation.freelancer_payout, 300);
    assert_eq!(allocation.client_refund + allocation.freelancer_payout, 1_000);
}

/// Odd amount 101 with 50/50 split: halves sum to 101 (rounding preserves
/// total).
#[test]
fn split_refund_regression_odd_amount_rounding_unchanged() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let allocation =
        client.multisig_split_refund(&admin_addr, &101_i128, &5_000_u32, &5_000_u32);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        101,
        "total must be preserved across rounding"
    );
}

/// Extreme basis-point split (1 / 9_999) of 10_000 preserves total.
#[test]
fn split_refund_regression_extreme_bps_preserves_total_unchanged() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 10_000_i128]);
    client.multisig_lock(&admin_addr);

    let allocation =
        client.multisig_split_refund(&admin_addr, &10_000_i128, &1_u32, &9_999_u32);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        10_000
    );
    assert_eq!(allocation.client_refund_bps, 1);
    assert_eq!(allocation.freelancer_payout_bps, 9_999);
}

// ── 6. Event emission ─────────────────────────────────────────────────────────

/// A successful call emits exactly one `"splitref"` event whose payload
/// matches the returned `RefundAllocation`.
#[test]
fn split_refund_success_emits_exactly_one_splitref_event_with_matching_payload() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let allocation =
        client.multisig_split_refund(&admin_addr, &1_000_i128, &6_000_u32, &4_000_u32);

    assert_eq!(splitref_event_count(&env), 1);

    let event = last_splitref_event(&env);
    assert_eq!(event.client_refund, allocation.client_refund);
    assert_eq!(event.freelancer_payout, allocation.freelancer_payout);
    assert_eq!(event.client_refund_bps, 6_000);
    assert_eq!(event.freelancer_payout_bps, 4_000);
}

/// Failed calls (overflow, invalid amount, invalid ratio) must not emit any
/// `"splitref"` event.
#[test]
fn split_refund_failed_calls_emit_no_splitref_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    // i128::MAX with non-zero client bps → overflow → InvalidAmount
    let _ =
        client.try_multisig_split_refund(&admin_addr, &i128::MAX, &5_000_u32, &5_000_u32);
    // i128::MIN → InvalidAmount (negative guard)
    let _ =
        client.try_multisig_split_refund(&admin_addr, &i128::MIN, &5_000_u32, &5_000_u32);
    // zero total → InvalidAmount
    let _ =
        client.try_multisig_split_refund(&admin_addr, &0_i128, &5_000_u32, &5_000_u32);
    // mismatched ratio → InvalidRatio
    let _ =
        client.try_multisig_split_refund(&admin_addr, &1_000_i128, &5_000_u32, &3_000_u32);

    assert_eq!(
        splitref_event_count(&env),
        0,
        "no event must be emitted for any failed call"
    );
}
