//! Tests for `admin_set_yield_rate` authorization and precondition guards.
//!
//! This module verifies that `admin_set_yield_rate` enforces its two
//! precondition checks — contract initialisation and pause state — before it
//! performs any auth check or touches any ledger entry.
//!
//! Validation matrix
//! ─────────────────
//! 1. Unauthorized caller            → `Error::Unauthorized`; no storage mutation.
//! 2. Paused contract                → `Error::Paused`; no storage mutation.
//! 3. Uninitialized contract         → `Error::NotInitialized`; no storage mutation.
//! 4. Happy path (various rates)     → `Ok(())`; `YieldRateBps` written correctly.
//! 5. Rate exceeds BPS_SCALE (10000) → `Error::InvalidRatio`; no storage mutation.
//! 6. Rate == 0 (disable accrual)    → `Ok(())`; `YieldRateBps` written as 0.
//! 7. Rate == 10000 (max / 100 %)    → `Ok(())`; `YieldRateBps` written as 10000.
//! 8. Repeated calls update the rate correctly.

#[cfg(test)]
use super::*;
use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient, YieldConfig};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Set up a fully initialised (and funded) escrow and return the key addresses
/// plus a client handle.
///
/// Return tuple: (client_addr, freelancer_addr, arbiter_addr, admin_addr,
///                token_id, contract_id, escrow_client)
fn setup(env: &Env) -> (Address, Address, Address, Address, Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let amounts = vec![env, 1_000_i128];
    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, token_id, contract_id, escrow) =
        setup_funded_escrow(env, amounts);
    (client_addr, freelancer_addr, arbiter_addr, admin_addr, token_id, contract_id, escrow)
}

/// Read the persisted yield rate from inside the contract.
///
/// The rate is not stored under a key of its own -- `admin_set_yield_rate`
/// writes it as the `yield_rate` field of the `YieldConfig` entry under
/// `DataKey::YieldConfig` -- so `None` here means the config was never
/// written, which is what the no-mutation cases assert.
fn read_yield_rate(env: &Env, contract_id: &Address) -> Option<u32> {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::YieldConfig)
            .map(|config: YieldConfig| config.yield_rate)
    })
}

// ── Unauthorized caller ───────────────────────────────────────────────────────

/// A caller whose address does not match `DataKey::Admin` must receive
/// `Error::Unauthorized` and must not mutate `YieldRateBps`.
#[test]
fn unauthorized_caller_returns_unauthorized_and_does_not_mutate_storage() {
    let env = Env::default();
    let (_, _, _, _, _, contract_id, escrow) = setup(&env);

    let attacker = Address::generate(&env);
    let rate_before = read_yield_rate(&env, &contract_id);

    let result = escrow.try_admin_set_yield_rate(&attacker, &500u32);

    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    // Storage must be exactly as it was before the call.
    assert_eq!(read_yield_rate(&env, &contract_id), rate_before);
}

/// Even when the attacker passes a plausible rate (0), the call must still be
/// rejected with `Unauthorized` and no storage entry may be written.
#[test]
fn unauthorized_caller_with_zero_rate_returns_unauthorized_and_does_not_mutate_storage() {
    let env = Env::default();
    let (_, _, _, _, _, contract_id, escrow) = setup(&env);

    let attacker = Address::generate(&env);

    let result = escrow.try_admin_set_yield_rate(&attacker, &0u32);

    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    assert_eq!(read_yield_rate(&env, &contract_id), None);
}

// ── Paused contract ───────────────────────────────────────────────────────────

/// When the contract is paused, `admin_set_yield_rate` must return
/// `Error::Paused` regardless of caller identity, and `YieldRateBps` must
/// not be touched.
#[test]
fn paused_contract_returns_paused_and_does_not_mutate_storage() {
    let env = Env::default();
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, escrow) = setup(&env);

    // Pause the contract via the emergency-pause endpoint.
    escrow.emergency_pause(&client_addr, &freelancer_addr);

    let rate_before = read_yield_rate(&env, &contract_id);

    let result = escrow.try_admin_set_yield_rate(&admin_addr, &300u32);

    assert_eq!(result, Err(Ok(Error::Paused)));
    assert_eq!(read_yield_rate(&env, &contract_id), rate_before);
}

/// Verify that after unpausing, the admin can set the yield rate again, which
/// confirms the pause guard is lifted correctly.
#[test]
fn after_unpause_admin_can_set_yield_rate() {
    let env = Env::default();
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.emergency_pause(&client_addr, &freelancer_addr);

    // Confirm paused state blocks the call.
    let paused_result = escrow.try_admin_set_yield_rate(&admin_addr, &200u32);
    assert_eq!(paused_result, Err(Ok(Error::Paused)));

    // Unpause and retry.
    escrow.emergency_unpause(&admin_addr);

    let result = escrow.try_admin_set_yield_rate(&admin_addr, &200u32);
    assert!(result.is_ok());
    assert_eq!(read_yield_rate(&env, &contract_id), Some(200u32));
}

/// An attacker calling on a paused contract must receive `Error::Paused`
/// (precondition fires before auth check), and no storage entry is written.
#[test]
fn paused_contract_unauthorized_caller_returns_paused_not_unauthorized() {
    let env = Env::default();
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.emergency_pause(&client_addr, &freelancer_addr);

    let attacker = Address::generate(&env);
    let result = escrow.try_admin_set_yield_rate(&attacker, &500u32);

    // The Paused precondition fires before the auth check.
    assert_eq!(result, Err(Ok(Error::Paused)));
    assert_eq!(read_yield_rate(&env, &contract_id), None);
}

// ── Uninitialized contract ────────────────────────────────────────────────────

/// Calling `admin_set_yield_rate` on a contract where `initialize` was never
/// called must return `Error::NotInitialized` immediately; no storage entry
/// may be written.
#[test]
fn uninitialized_contract_returns_not_initialized_and_does_not_mutate_storage() {
    let env = Env::default();
    env.mock_all_auths();

    // Register a fresh contract without calling `initialize`.
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let result = escrow.try_admin_set_yield_rate(&admin, &500u32);

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
    assert_eq!(read_yield_rate(&env, &contract_id), None);
}

// ── Rate validation ───────────────────────────────────────────────────────────

/// A rate that exceeds 10 000 bps (100 %) must be rejected with
/// `Error::InvalidRatio`; `YieldRateBps` must remain unmodified.
#[test]
fn rate_above_bps_scale_returns_invalid_ratio_and_does_not_mutate_storage() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let rate_before = read_yield_rate(&env, &contract_id);

    let result = escrow.try_admin_set_yield_rate(&admin_addr, &10_001u32);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_yield_rate(&env, &contract_id), rate_before);
}

/// Rate of `u32::MAX` (far above 10 000) must also be rejected cleanly.
#[test]
fn rate_u32_max_returns_invalid_ratio_and_does_not_mutate_storage() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    let result = escrow.try_admin_set_yield_rate(&admin_addr, &u32::MAX);

    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
    assert_eq!(read_yield_rate(&env, &contract_id), None);
}

// ── Happy path ────────────────────────────────────────────────────────────────

/// The admin can set a mid-range rate; `YieldRateBps` must be persisted
/// with the exact supplied value.
#[test]
fn admin_can_set_valid_rate() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.admin_set_yield_rate(&admin_addr, &500u32);

    assert_eq!(read_yield_rate(&env, &contract_id), Some(500u32));
}

/// Rate of `0` (disables accrual) is accepted and stored correctly.
#[test]
fn admin_can_set_zero_rate_to_disable_accrual() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.admin_set_yield_rate(&admin_addr, &0u32);

    assert_eq!(read_yield_rate(&env, &contract_id), Some(0u32));
}

/// Rate of exactly `10 000` (100 %) is at the boundary and must be accepted.
#[test]
fn admin_can_set_max_rate_10000() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.admin_set_yield_rate(&admin_addr, &10_000u32);

    assert_eq!(read_yield_rate(&env, &contract_id), Some(10_000u32));
}

/// Successive calls update the rate; the last written value wins.
#[test]
fn repeated_calls_update_rate_correctly() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.admin_set_yield_rate(&admin_addr, &100u32);
    assert_eq!(read_yield_rate(&env, &contract_id), Some(100u32));

    escrow.admin_set_yield_rate(&admin_addr, &750u32);
    assert_eq!(read_yield_rate(&env, &contract_id), Some(750u32));

    escrow.admin_set_yield_rate(&admin_addr, &0u32);
    assert_eq!(read_yield_rate(&env, &contract_id), Some(0u32));
}

/// A mid-range boundary value (1 bp) is accepted and persisted.
#[test]
fn admin_can_set_minimum_nonzero_rate_1_bps() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.admin_set_yield_rate(&admin_addr, &1u32);

    assert_eq!(read_yield_rate(&env, &contract_id), Some(1u32));
}

/// A rate of 9 999 (just below the cap) is accepted.
#[test]
fn admin_can_set_rate_just_below_cap_9999() {
    let env = Env::default();
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup(&env);

    escrow.admin_set_yield_rate(&admin_addr, &9_999u32);

    assert_eq!(read_yield_rate(&env, &contract_id), Some(9_999u32));
}
