#![cfg(test)]
//! Guard and conservation suite for `emergency_pause_split_refund`
//! (issues #524 and #526).
//!
//! # Authorization and preconditions (#524)
//! The endpoint used to validate its arguments and publish its event without
//! looking at the caller or the escrow's state.  It now runs, in order and
//! before any arithmetic:
//!
//! 1. `caller.require_auth()`; the caller must be the stored admin, client or
//!    freelancer → `NotInitialized` / `Unauthorized`.
//! 2. `EpLk` held (half-applied pause transition) → `EmergencyPauseInProgress`.
//! 3. `Ep` not set (escrow running) → `NotPaused`.
//!
//! Every rejection is asserted to leave all three storage tiers of the
//! contract identical and to publish no `epspltref` event.
//!
//! # Conservation (#526)
//! `client_refund + freelancer_payout == total_amount` across a sweep of
//! amounts (including `1`, the `BPS_SCALE` boundaries and `i128::MAX`) and
//! every basis-point weight from `0` to `10_000`.

use super::*;
use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::{symbol_short, vec, Address, Env, IntoVal, Map, Val};

struct Fixture<'a> {
    contract_id: Address,
    admin: Address,
    client_addr: Address,
    freelancer: Address,
    arbiter: Address,
    escrow: MilestoneEscrowClient<'a>,
}

/// A funded escrow that is not paused.
fn funded(env: &Env) -> Fixture<'_> {
    env.mock_all_auths();
    let (client_addr, freelancer, arbiter, admin, _, contract_id, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    Fixture {
        contract_id,
        admin,
        client_addr,
        freelancer,
        arbiter,
        escrow,
    }
}

/// A funded escrow frozen by the emergency pause.
fn paused(env: &Env) -> Fixture<'_> {
    let fx = funded(env);
    fx.escrow.emergency_pause_admin_override(&fx.admin, &true);
    fx
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

fn epspltref_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("epspltref").into_val(env);
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

/// Call with otherwise-valid arguments, assert the typed error, and assert
/// that nothing was written and no event was published.
fn assert_rejected(env: &Env, fx: &Fixture<'_>, caller: &Address, expected: Error) {
    let escrow = &fx.escrow;
    let before = storage_snapshot(env, &fx.contract_id);

    assert_eq!(
        escrow.try_emergency_pause_split_refund(caller, &1_000_i128, &6_000_u32, &4_000_u32),
        Err(Ok(expected))
    );
    assert_eq!(epspltref_count(env), 0);

    let after = storage_snapshot(env, &fx.contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

// ── #524: unauthorized callers ───────────────────────────────────────────────

#[test]
fn test_split_refund_rejects_stranger_with_unauthorized() {
    let env = Env::default();
    let fx = paused(&env);
    let stranger = Address::generate(&env);
    assert_rejected(&env, &fx, &stranger, Error::Unauthorized);
}

/// The arbiter is a participant but not a refund party; it is refused too.
#[test]
fn test_split_refund_rejects_arbiter_with_unauthorized() {
    let env = Env::default();
    let fx = paused(&env);
    assert_rejected(&env, &fx, &fx.arbiter, Error::Unauthorized);
}

/// Authorization is checked before the source state: a stranger calling into
/// a *running* escrow gets `Unauthorized`, not `NotPaused`.
#[test]
fn test_split_refund_authorizes_before_state_guards() {
    let env = Env::default();
    let fx = funded(&env);
    let stranger = Address::generate(&env);
    assert_rejected(&env, &fx, &stranger, Error::Unauthorized);
}

/// An uninitialized contract has no parties to authorize against.
#[test]
fn test_split_refund_uninitialized_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    let before = storage_snapshot(&env, &contract_id);
    assert_eq!(
        escrow.try_emergency_pause_split_refund(&caller, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(storage_snapshot(&env, &contract_id), before);
}

/// Admin, client and freelancer are all accepted, and each must sign.
#[test]
fn test_split_refund_accepts_every_party_and_requires_their_auth() {
    let env = Env::default();
    let fx = paused(&env);
    let escrow = &fx.escrow;

    for caller in [&fx.admin, &fx.client_addr, &fx.freelancer] {
        let allocation =
            escrow.emergency_pause_split_refund(caller, &1_000_i128, &6_000_u32, &4_000_u32);
        assert!(
            env.auths().iter().any(|(addr, _)| addr == caller),
            "the caller's authorization must be required"
        );
        assert_eq!(allocation.client_refund, 600);
        assert_eq!(allocation.freelancer_payout, 400);
    }
}

/// Without a signature the host rejects the call outright.
#[test]
fn test_split_refund_without_signature_is_rejected() {
    let env = Env::default();
    let fx = paused(&env);
    let escrow = &fx.escrow;
    env.set_auths(&[]);

    let before = storage_snapshot(&env, &fx.contract_id);
    let result =
        escrow.try_emergency_pause_split_refund(&fx.admin, &1_000_i128, &5_000_u32, &5_000_u32);
    assert!(result.is_err(), "an unsigned call must not succeed");
    assert_eq!(storage_snapshot(&env, &fx.contract_id), before);
}

// ── #524: illegal source states ─────────────────────────────────────────────

/// A running (not emergency-paused) escrow is an illegal source state.
#[test]
fn test_split_refund_rejects_running_escrow_with_not_paused() {
    let env = Env::default();
    let fx = funded(&env);
    assert_rejected(&env, &fx, &fx.admin, Error::NotPaused);
}

/// After an unpause the escrow is running again and the preview is refused.
#[test]
fn test_split_refund_rejects_after_unpause() {
    let env = Env::default();
    let fx = paused(&env);
    fx.escrow.emergency_pause_admin_override(&fx.admin, &false);
    assert_rejected(&env, &fx, &fx.client_addr, Error::NotPaused);
}

/// A half-applied pause transition (`EpLk` held) is refused with its own
/// error, whether or not the `Ep` flag has been committed yet.
#[test]
fn test_split_refund_rejects_pause_transition_in_progress() {
    for committed in [false, true] {
        let env = Env::default();
        let fx = funded(&env);
        env.as_contract(&fx.contract_id, || {
            env.storage().instance().set(&DataKey::EpLk, &true);
            env.storage().instance().set(&DataKey::Ep, &committed);
        });
        assert_rejected(&env, &fx, &fx.admin, Error::EmergencyPauseInProgress);
    }
}

/// Argument validation still yields its own typed errors once every guard
/// passes, and still mutates nothing.
#[test]
fn test_split_refund_argument_validation_after_guards() {
    let env = Env::default();
    let fx = paused(&env);
    let escrow = &fx.escrow;
    let before = storage_snapshot(&env, &fx.contract_id);

    for (total, c, f, expected) in [
        (0_i128, 5_000_u32, 5_000_u32, Error::InvalidAmount),
        (-1_i128, 5_000_u32, 5_000_u32, Error::InvalidAmount),
        (i128::MIN, 5_000_u32, 5_000_u32, Error::InvalidAmount),
        (1_000_i128, 4_000_u32, 4_000_u32, Error::InvalidRatio),
        (1_000_i128, 10_001_u32, 0_u32, Error::InvalidRatio),
        (1_000_i128, u32::MAX, u32::MAX, Error::InvalidRatio),
    ] {
        assert_eq!(
            escrow.try_emergency_pause_split_refund(&fx.admin, &total, &c, &f),
            Err(Ok(expected))
        );
    }
    assert_eq!(storage_snapshot(&env, &fx.contract_id), before);
}

/// A successful preview writes nothing to the contract either, and publishes
/// exactly one event.
#[test]
fn test_split_refund_success_mutates_no_contract_storage() {
    let env = Env::default();
    let fx = paused(&env);
    let escrow = &fx.escrow;
    let before = storage_snapshot(&env, &fx.contract_id);

    escrow.emergency_pause_split_refund(&fx.freelancer, &1_001_i128, &2_500_u32, &7_500_u32);
    assert_eq!(epspltref_count(&env), 1);
    assert_eq!(storage_snapshot(&env, &fx.contract_id), before);
}

// ── #526: the outputs sum exactly to the input ──────────────────────────────

const SWEEP_AMOUNTS: [i128; 24] = [
    1,
    2,
    3,
    7,
    99,
    100,
    101,
    4_999,
    5_000,
    5_001,
    9_999,
    10_000,
    10_001,
    19_999,
    1_000_000_007,
    i64::MAX as i128,
    // Largest total for which the direct `total × bps` product still fits.
    i128::MAX / 10_000,
    i128::MAX / 10_000 + 1,
    i128::MAX / 2,
    i128::MAX - 10_000,
    i128::MAX - 9_999,
    i128::MAX - 2,
    i128::MAX - 1,
    i128::MAX,
];

fn split(total: i128, client_bps: u32) -> RefundAllocation {
    MilestoneEscrow::emergency_split_exact(total, client_bps, BPS_SCALE - client_bps)
        .expect("a positive total with a valid ratio must split")
}

/// Every amount in the sweep against every weight from 0 to 10 000 bps: the
/// two legs sum exactly to the input, neither is negative, and the bps are
/// echoed.
#[test]
fn test_split_conserves_total_across_full_bps_sweep() {
    for &total in SWEEP_AMOUNTS.iter() {
        for client_bps in 0..=BPS_SCALE {
            let a = split(total, client_bps);
            assert_eq!(
                a.client_refund.checked_add(a.freelancer_payout),
                Some(total),
                "drift at total={total} client_bps={client_bps}"
            );
            assert!(a.client_refund >= 0 && a.freelancer_payout >= 0);
            assert_eq!(a.client_refund_bps, client_bps);
            assert_eq!(a.freelancer_payout_bps, BPS_SCALE - client_bps);
        }
    }
}

/// Where the direct round-nearest formula does not overflow, the conserving
/// split returns exactly what it would — the rounding is unchanged.
#[test]
fn test_split_matches_direct_round_nearest_formula() {
    let scale = BPS_SCALE as i128;
    for &total in SWEEP_AMOUNTS.iter().filter(|&&t| t <= i128::MAX / scale) {
        for client_bps in 0..=BPS_SCALE {
            let expected = (total * client_bps as i128 + scale / 2) / scale;
            assert_eq!(
                split(total, client_bps).client_refund,
                expected,
                "rounding changed at total={total} client_bps={client_bps}"
            );
        }
    }
}

/// Exact expected values at the boundaries, including `i128::MAX`, where the
/// old `total × bps` product overflowed and the call failed.
#[test]
fn test_split_boundary_weights_are_exact() {
    for &total in SWEEP_AMOUNTS.iter() {
        // 0 bps: everything to the freelancer.
        let a = split(total, 0);
        assert_eq!((a.client_refund, a.freelancer_payout), (0, total));

        // 10 000 bps: everything to the client.
        let a = split(total, BPS_SCALE);
        assert_eq!((a.client_refund, a.freelancer_payout), (total, 0));

        // 5 000 bps: round-half-up gives the client the odd unit.
        let a = split(total, 5_000);
        assert_eq!(a.client_refund, total / 2 + total % 2);
        assert_eq!(a.freelancer_payout, total / 2);
    }
}

/// The client leg never decreases as its weight grows, so no weight step can
/// move a unit the wrong way.
#[test]
fn test_split_is_monotonic_in_client_weight() {
    for &total in SWEEP_AMOUNTS.iter() {
        let mut previous = 0_i128;
        for client_bps in 0..=BPS_SCALE {
            let refund = split(total, client_bps).client_refund;
            assert!(
                refund >= previous,
                "non-monotonic at total={total} client_bps={client_bps}"
            );
            previous = refund;
        }
    }
}

/// End to end through the contract: the returned allocation conserves the
/// total and matches the helper, across a sample of amounts and weights.
#[test]
fn test_split_refund_endpoint_conserves_total() {
    let env = Env::default();
    let fx = paused(&env);
    let escrow = &fx.escrow;

    for &total in SWEEP_AMOUNTS.iter() {
        for client_bps in [0_u32, 1, 3_333, 5_000, 6_667, 9_999, 10_000] {
            let a = escrow.emergency_pause_split_refund(
                &fx.admin,
                &total,
                &client_bps,
                &(BPS_SCALE - client_bps),
            );
            assert_eq!(
                a.client_refund.checked_add(a.freelancer_payout),
                Some(total)
            );
            assert_eq!(a, split(total, client_bps));
        }
    }
}
