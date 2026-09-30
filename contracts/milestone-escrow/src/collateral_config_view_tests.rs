//! Tests for `set_collateral_config` and `get_collateral_config` —
//! the collateral configuration view endpoints.
//!
//! # Scope
//! Issue #1351 — define and enforce validation boundaries for
//! `escrow/src/tests/collateral_config_view.rs`.
//!
//! # Validation matrix
//! ────────────────────────────────────────────────────────────────────────────
//! ## Authorization
//! 1. Unauthorized caller (attacker)      → `Error::Unauthorized`; no storage written.
//! 2. Uninitialized contract              → `Error::NotInitialized`; no storage written.
//!
//! ## `ltv_bps` constraints  (must be in `[1, 10_000]`)
//! 3. `ltv_bps == 0`                      → `Error::InvalidRatio`; no storage written.
//! 4. `ltv_bps > 10_000`                  → `Error::InvalidRatio`; no storage written.
//! 5. `ltv_bps == u32::MAX`               → `Error::InvalidRatio`; no storage written.
//! 6. `ltv_bps == 1`  (lower boundary)    → `Ok(())`; persisted correctly.
//! 7. `ltv_bps == 10_000` (upper boundary) → `Ok(())`; persisted correctly.
//!
//! ## `liquidation_threshold_bps` constraints  (must be in `[ltv_bps, 10_000]`)
//! 8. `liquidation_threshold_bps < ltv_bps` → `Error::InvalidRatio`; no storage written.
//! 9. `liquidation_threshold_bps > 10_000`  → `Error::InvalidRatio`; no storage written.
//! 10. `liquidation_threshold_bps == ltv_bps` (lower boundary) → `Ok(())`; persisted.
//! 11. `liquidation_threshold_bps == 10_000` (upper boundary)  → `Ok(())`; persisted.
//!
//! ## `penalty_bps` constraints  (must be in `[0, 10_000]`)
//! 12. `penalty_bps > 10_000`             → `Error::InvalidRatio`; no storage written.
//! 13. `penalty_bps == u32::MAX`          → `Error::InvalidRatio`; no storage written.
//! 14. `penalty_bps == 0`  (grace mode)   → `Ok(())`; persisted correctly.
//! 15. `penalty_bps == 10_000`            → `Ok(())`; persisted correctly.
//!
//! ## `get_collateral_config` view
//! 16. Before any `set_collateral_config` call  → `Error::NotInitialized`.
//! 17. After valid `set_collateral_config`       → returns the persisted struct exactly.
//!
//! ## Duplicate / overwrite (idempotency and update semantics)
//! 18. Calling `set_collateral_config` twice with different valid params
//!     replaces the previous value (last write wins).
//! 19. Calling `set_collateral_config` twice with the same params is idempotent
//!     (storage holds the same value, no error).
//!
//! ## Regression / no side-effects on failure
//! 20. A failed `set_collateral_config` must not mutate any storage that was
//!     present before the call.
//! 21. A failed `set_collateral_config` must not emit any events.

use crate::test::setup_funded_escrow;
use crate::{CollateralConfig, DataKey, Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Create a fully initialised (and funded) single-milestone escrow, mock
/// all auths, and return the key addresses plus a client handle.
///
/// Return tuple:
/// `(client_addr, freelancer_addr, arbiter_addr, admin_addr, token_id, contract_id, escrow_client)`
fn setup(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();
    let amounts = vec![env, 1_000_i128];
    setup_funded_escrow(env, amounts)
}

/// Read `DataKey::CollateralConfig` directly from inside the contract,
/// bypassing the public API, so tests can assert no-mutation on failure paths.
fn read_collateral_config_raw(env: &Env, contract_id: &Address) -> Option<CollateralConfig> {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::CollateralConfig)
    })
}

// ── 1. Authorization: unauthorized caller ─────────────────────────────────────

/// An attacker whose address does not match `DataKey::Admin` must receive
/// `Error::Unauthorized`.  No storage entry may be created or modified.
#[test]
fn unauthorized_caller_returns_unauthorized_and_does_not_mutate_storage() {
    let env = Env::default();
    let (_, _, _, _, _, contract_id, escrow) = setup(&env);

    let attacker = Address::generate(&env);
    let before = read_collateral_config_raw(&env, &contract_id);

    let result = escrow.try_set_collateral_config(&attacker, &5_000u32, &7_500u32, &500u32);

    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), before);
}

/// Even with a zero LTV (which would also be invalid on its own), an attacker
/// receives `Unauthorized` first — the auth check precedes the param check.
#[test]
fn unauthorized_caller_with_zero_ltv_still_returns_unauthorized() {
    let env = Env::default();
    let (_, _, _, _, _, contract_id, escrow) = setup(&env);

    let attacker = Address::generate(&env);

    let result = escrow.try_set_collateral_config(&attacker, &0u32, &0u32, &0u32);

    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 2. Authorization: uninitialized contract ──────────────────────────────────

/// Calling `set_collateral_config` on a contract where `initialize` was never
/// called must return `Error::NotInitialized`; no storage entry may be written.
#[test]
fn uninitialized_contract_returns_not_initialized_and_does_not_mutate_storage() {
    let env = Env::default();
    env.mock_all_auths();

    // Register without calling `initialize`.
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let result = escrow.try_set_collateral_config(&admin, &5_000u32, &7_500u32, &500u32);

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 3. `ltv_bps == 0` ────────────────────────────────────────────────────────

/// A zero LTV is degenerate (nothing can ever be collateralised).  The call
/// must be rejected with `Error::InvalidRatio`; no entry may be written.
#[test]
fn ltv_bps_zero_returns_invalid_ratio_and_no_storage_written() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &0u32, &5_000u32, &500u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 4. `ltv_bps > 10_000` ────────────────────────────────────────────────────

/// An LTV above 10 000 bps (> 100 %) exceeds the maximum allowed ratio.
#[test]
fn ltv_bps_above_max_returns_invalid_ratio_and_no_storage_written() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &10_001u32, &10_001u32, &0u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 5. `ltv_bps == u32::MAX` ─────────────────────────────────────────────────

/// `u32::MAX` is far above 10 000; the call must be rejected cleanly without
/// overflow or panic.
#[test]
fn ltv_bps_u32_max_returns_invalid_ratio_without_panic() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &u32::MAX, &u32::MAX, &0u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 6. `ltv_bps == 1` (lower boundary, accepted) ────────────────────────────

/// The minimum valid LTV is 1 bps (0.01 %).  The call must succeed and
/// the value must be persisted exactly.
#[test]
fn ltv_bps_one_is_minimum_valid_and_persisted() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &1u32, &1u32, &0u32);

    assert!(result.is_ok());
    let stored = read_collateral_config_raw(&env, &contract_id).expect("config must be written");
    assert_eq!(stored.ltv_bps, 1u32);
    assert_eq!(stored.liquidation_threshold_bps, 1u32);
    assert_eq!(stored.penalty_bps, 0u32);
}

// ── 7. `ltv_bps == 10_000` (upper boundary, accepted) ───────────────────────

/// An LTV of exactly 10 000 bps (100 %) is the upper boundary and must be
/// accepted.
#[test]
fn ltv_bps_ten_thousand_is_maximum_valid_and_persisted() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &10_000u32, &10_000u32, &500u32);

    assert!(result.is_ok());
    let stored = read_collateral_config_raw(&env, &contract_id).expect("config must be written");
    assert_eq!(stored.ltv_bps, 10_000u32);
}

// ── 8. `liquidation_threshold_bps < ltv_bps` ────────────────────────────────

/// A liquidation threshold below the LTV is invalid — the escrow would be
/// permanently under-collateralised.
#[test]
fn liquidation_threshold_below_ltv_returns_invalid_ratio_and_no_storage_written() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    // ltv = 7000, liquidation = 6999 (one below ltv).
    let result = escrow.try_set_collateral_config(&admin_addr, &7_000u32, &6_999u32, &500u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

/// Liquidation threshold of zero with a non-zero LTV must be rejected.
#[test]
fn liquidation_threshold_zero_with_nonzero_ltv_returns_invalid_ratio() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &0u32, &0u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 9. `liquidation_threshold_bps > 10_000` ─────────────────────────────────

/// A liquidation threshold above 10 000 bps exceeds the BPS scale.
#[test]
fn liquidation_threshold_above_max_returns_invalid_ratio_and_no_storage_written() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &10_001u32, &0u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 10. `liquidation_threshold_bps == ltv_bps` (lower boundary, accepted) ───

/// When `liquidation_threshold_bps` equals `ltv_bps` the constraint
/// `threshold ≥ ltv` is satisfied at its lower bound.
#[test]
fn liquidation_threshold_equal_to_ltv_is_valid_and_persisted() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &6_000u32, &6_000u32, &200u32);

    assert!(result.is_ok());
    let stored = read_collateral_config_raw(&env, &contract_id).expect("config must be written");
    assert_eq!(stored.ltv_bps, 6_000u32);
    assert_eq!(stored.liquidation_threshold_bps, 6_000u32);
}

// ── 11. `liquidation_threshold_bps == 10_000` (upper boundary, accepted) ────

/// A liquidation threshold of exactly 10 000 bps is the upper boundary.
#[test]
fn liquidation_threshold_ten_thousand_is_valid_and_persisted() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &10_000u32, &0u32);

    assert!(result.is_ok());
    let stored = read_collateral_config_raw(&env, &contract_id).expect("config must be written");
    assert_eq!(stored.liquidation_threshold_bps, 10_000u32);
}

// ── 12. `penalty_bps > 10_000` ───────────────────────────────────────────────

/// A penalty above 10 000 bps (> 100 %) is invalid.
#[test]
fn penalty_bps_above_max_returns_invalid_ratio_and_no_storage_written() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &10_001u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 13. `penalty_bps == u32::MAX` ────────────────────────────────────────────

/// `u32::MAX` penalty must be rejected cleanly without overflow or panic.
#[test]
fn penalty_bps_u32_max_returns_invalid_ratio_without_panic() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &u32::MAX);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_collateral_config_raw(&env, &contract_id), None);
}

// ── 14. `penalty_bps == 0` (grace mode, accepted) ───────────────────────────

/// A penalty of zero (grace mode — no liquidation penalty) is explicitly
/// valid and must be stored correctly.
#[test]
fn penalty_bps_zero_grace_mode_is_valid_and_persisted() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &0u32);

    assert!(result.is_ok());
    let stored = read_collateral_config_raw(&env, &contract_id).expect("config must be written");
    assert_eq!(stored.penalty_bps, 0u32);
}

// ── 15. `penalty_bps == 10_000` (maximum, accepted) ─────────────────────────

/// A penalty of exactly 10 000 bps (100 %) is the upper boundary and must
/// be accepted.
#[test]
fn penalty_bps_ten_thousand_is_maximum_valid_and_persisted() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &10_000u32);

    assert!(result.is_ok());
    let stored = read_collateral_config_raw(&env, &contract_id).expect("config must be written");
    assert_eq!(stored.penalty_bps, 10_000u32);
}

// ── 16. `get_collateral_config` before any set ───────────────────────────────

/// Calling `get_collateral_config` before `set_collateral_config` has ever
/// succeeded must return `Error::NotInitialized`.
#[test]
fn get_collateral_config_before_set_returns_not_initialized() {
    let env = Env::default();
    let (_, _, _, _, _, _, escrow) = setup(&env);

    let result = escrow.try_get_collateral_config();

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

/// `get_collateral_config` on a completely uninitialized contract must also
/// return `Error::NotInitialized`.
#[test]
fn get_collateral_config_on_uninitialized_contract_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let result = escrow.try_get_collateral_config();

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

// ── 17. `get_collateral_config` happy path ────────────────────────────────────

/// After a successful `set_collateral_config`, `get_collateral_config` must
/// return the exact values that were written.
#[test]
fn get_collateral_config_returns_persisted_values_exactly() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &6_500u32, &8_000u32, &300u32);

    let config = escrow.get_collateral_config();

    assert_eq!(config.ltv_bps, 6_500u32);
    assert_eq!(config.liquidation_threshold_bps, 8_000u32);
    assert_eq!(config.penalty_bps, 300u32);
}

/// `get_collateral_config` is a pure view: calling it multiple times returns
/// the same result without side effects.
#[test]
fn get_collateral_config_is_idempotent_view() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &4_000u32, &6_000u32, &100u32);

    let first = escrow.get_collateral_config();
    let second = escrow.get_collateral_config();

    assert_eq!(first, second);
}

// ── 18. Duplicate call with different params (last-write-wins) ────────────────

/// Calling `set_collateral_config` a second time with new valid params must
/// replace the previous config.  The view must return the new values.
#[test]
fn second_set_with_different_params_replaces_previous_config() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    // First write.
    escrow.set_collateral_config(&admin_addr, &5_000u32, &7_000u32, &200u32);
    let first = read_collateral_config_raw(&env, &contract_id).unwrap();
    assert_eq!(first.ltv_bps, 5_000u32);

    // Second write with different values.
    escrow.set_collateral_config(&admin_addr, &3_000u32, &4_500u32, &50u32);
    let second = read_collateral_config_raw(&env, &contract_id).unwrap();

    assert_eq!(second.ltv_bps, 3_000u32);
    assert_eq!(second.liquidation_threshold_bps, 4_500u32);
    assert_eq!(second.penalty_bps, 50u32);

    // Public view reflects the latest write.
    let config = escrow.get_collateral_config();
    assert_eq!(config.ltv_bps, 3_000u32);
    assert_eq!(config.liquidation_threshold_bps, 4_500u32);
    assert_eq!(config.penalty_bps, 50u32);
}

/// Multiple successive overwrites: the last write always wins and all
/// intermediate values are gone.
#[test]
fn multiple_overwrites_each_write_replaces_previous() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    let writes: &[(u32, u32, u32)] = &[
        (1_000, 2_000, 100),
        (2_000, 3_000, 200),
        (8_000, 9_000, 500),
        (500, 500, 0),
    ];

    for &(ltv, threshold, penalty) in writes {
        escrow.set_collateral_config(&admin_addr, &ltv, &threshold, &penalty);
        let config = escrow.get_collateral_config();
        assert_eq!(config.ltv_bps, ltv);
        assert_eq!(config.liquidation_threshold_bps, threshold);
        assert_eq!(config.penalty_bps, penalty);
    }
}

// ── 19. Duplicate call with same params (idempotent) ─────────────────────────

/// Calling `set_collateral_config` twice with identical valid params must
/// succeed both times and leave storage unchanged.
#[test]
fn duplicate_set_with_same_params_is_idempotent() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &250u32);
    let after_first = read_collateral_config_raw(&env, &contract_id).unwrap();

    // Same call again.
    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &250u32);
    assert!(result.is_ok(), "second identical call must not fail");

    let after_second = read_collateral_config_raw(&env, &contract_id).unwrap();
    assert_eq!(after_first, after_second);
}

// ── 20. No storage mutation on failure ───────────────────────────────────────

/// If a valid config already exists and a subsequent invalid call is made,
/// the pre-existing config must be preserved exactly.
#[test]
fn failed_set_does_not_overwrite_existing_valid_config() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    // Write a valid config.
    escrow.set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &300u32);
    let before = read_collateral_config_raw(&env, &contract_id).unwrap();

    // Attempt an invalid update (ltv == 0).
    let result = escrow.try_set_collateral_config(&admin_addr, &0u32, &7_500u32, &300u32);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));

    // Storage must still hold the original valid config.
    assert_eq!(read_collateral_config_raw(&env, &contract_id), Some(before));
}

/// Unauthorized call must not touch any pre-existing config.
#[test]
fn unauthorized_set_does_not_overwrite_existing_valid_config() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &5_000u32, &7_500u32, &100u32);
    let before = read_collateral_config_raw(&env, &contract_id).unwrap();

    let attacker = Address::generate(&env);
    let result = escrow.try_set_collateral_config(&attacker, &1_000u32, &2_000u32, &50u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    assert_eq!(read_collateral_config_raw(&env, &contract_id), Some(before));
}

/// A call with `liquidation_threshold < ltv` must leave any existing config
/// intact.
#[test]
fn invalid_threshold_does_not_overwrite_existing_config() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &5_000u32, &7_000u32, &200u32);
    let before = read_collateral_config_raw(&env, &contract_id).unwrap();

    // threshold (4_999) < ltv (5_000) → invalid.
    let result = escrow.try_set_collateral_config(&admin_addr, &5_000u32, &4_999u32, &200u32);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));

    assert_eq!(read_collateral_config_raw(&env, &contract_id), Some(before));
}

// ── 21. No events emitted on failure ─────────────────────────────────────────

/// A failed `set_collateral_config` call must not emit any events.
/// We verify this by counting all contract events after the failed call and
/// asserting that no collateral-config topic appears.
#[test]
fn failed_set_does_not_emit_any_events() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    // Trigger a validation failure (zero ltv).
    let result = escrow.try_set_collateral_config(&admin_addr, &0u32, &0u32, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));

    // No collateral-config events must have been emitted.  The `set_collateral_config`
    // function does not publish any events (there is nothing to match), so the
    // total count for any topic originating from this call must be zero.
    let total_events = crate::all_event_tuples(&env).len();
    assert_eq!(total_events, 0, "no events should be emitted on a failed set_collateral_config call");
}

// ── Boundary combination matrix ───────────────────────────────────────────────

/// Exhaustive boundary probe: all three fields at their minimum valid values.
#[test]
fn all_fields_at_minimum_valid_values_succeed() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    // ltv = 1, threshold = 1 (== ltv), penalty = 0.
    escrow.set_collateral_config(&admin_addr, &1u32, &1u32, &0u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.ltv_bps, 1u32);
    assert_eq!(config.liquidation_threshold_bps, 1u32);
    assert_eq!(config.penalty_bps, 0u32);
}

/// All three fields at their maximum valid values.
#[test]
fn all_fields_at_maximum_valid_values_succeed() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    // ltv = 10_000, threshold = 10_000, penalty = 10_000.
    escrow.set_collateral_config(&admin_addr, &10_000u32, &10_000u32, &10_000u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.ltv_bps, 10_000u32);
    assert_eq!(config.liquidation_threshold_bps, 10_000u32);
    assert_eq!(config.penalty_bps, 10_000u32);
}

/// Typical mid-range configuration (80 % LTV, 85 % liquidation, 5 % penalty).
#[test]
fn typical_mid_range_config_round_trips_correctly() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &8_000u32, &8_500u32, &500u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.ltv_bps, 8_000u32);
    assert_eq!(config.liquidation_threshold_bps, 8_500u32);
    assert_eq!(config.penalty_bps, 500u32);
}

/// `ltv_bps` one above its minimum (2) should be valid.
#[test]
fn ltv_bps_one_above_minimum_is_valid() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &2u32, &2u32, &0u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.ltv_bps, 2u32);
}

/// `ltv_bps` one below its maximum (9_999) should be valid.
#[test]
fn ltv_bps_one_below_maximum_is_valid() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &9_999u32, &10_000u32, &0u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.ltv_bps, 9_999u32);
}

/// `liquidation_threshold_bps` one above `ltv_bps` (threshold = ltv + 1)
/// must be valid.
#[test]
fn liquidation_threshold_one_above_ltv_is_valid() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &5_000u32, &5_001u32, &0u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.ltv_bps, 5_000u32);
    assert_eq!(config.liquidation_threshold_bps, 5_001u32);
}

/// `liquidation_threshold_bps` of 9_999 (one below max) must be valid.
#[test]
fn liquidation_threshold_9999_is_valid() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &5_000u32, &9_999u32, &0u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.liquidation_threshold_bps, 9_999u32);
}

/// `penalty_bps` of 1 (minimum non-zero) must be accepted.
#[test]
fn penalty_bps_one_is_minimum_nonzero_valid() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &5_000u32, &7_000u32, &1u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.penalty_bps, 1u32);
}

/// `penalty_bps` of 9_999 (one below max) must be accepted.
#[test]
fn penalty_bps_9999_is_valid() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, _, escrow) = setup(&env);

    escrow.set_collateral_config(&admin_addr, &5_000u32, &7_000u32, &9_999u32);

    let config = escrow.get_collateral_config();
    assert_eq!(config.penalty_bps, 9_999u32);
}
