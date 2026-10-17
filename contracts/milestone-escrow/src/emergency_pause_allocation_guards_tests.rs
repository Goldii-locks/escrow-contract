#![cfg(test)]
//! Guard suite for `emergency_pause_allocation` (issue #536).
//!
//! # What the endpoint used to do
//! `emergency_pause_allocation` used to accept an arbitrary caller and to
//! ignore the escrow's state entirely: it computed a payout split for *any*
//! address, on a live escrow, on an uninitialized contract, and even in the
//! middle of a half-applied pause transition.  The allocation it published is
//! the authoritative multi-party figure a settlement would pay out against, so
//! an unauthorized caller could have had the contract produce (and emit on the
//! ledger) a payout plan for a frozen balance that was never paused.
//!
//! # Guard order now enforced
//! 1. `caller.require_auth()`, then the caller must be the stored admin, the
//!    job's client or the job's freelancer — `NotInitialized` / `Unauthorized`.
//! 2. No half-applied pause transition (`EpLk` held) →
//!    `EmergencyPauseInProgress`.
//! 3. The escrow must actually be frozen (`Ep` set) → `NotPaused`.
//!
//! Every guard runs before the weight vector is inspected and before any
//! arithmetic, and each one only reads instance storage.
//!
//! # What each test pins
//! * each rejection returns its own typed contract error, never a trap;
//! * the caller's authorization is actually collected;
//! * a rejected call mutates **no** ledger entry in any of the three storage
//!   tiers and publishes **no** `epalloc` event;
//! * the guards do not poison the endpoint — a valid call right after a
//!   rejection still allocates and still conserves the total exactly.

use super::*;
use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{symbol_short, vec, Address, Env, IntoVal, Map, Val};

/// A funded escrow plus every party address.
struct Fixture<'a> {
    contract_id: Address,
    admin: Address,
    client_addr: Address,
    freelancer: Address,
    arbiter: Address,
    escrow: MilestoneEscrowClient<'a>,
}

/// A funded escrow that is *not* emergency-paused.
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

/// A funded escrow frozen by the emergency pause — the only legal source state.
fn paused(env: &Env) -> Fixture<'_> {
    let fx = funded(env);
    fx.escrow.emergency_pause_admin_override(&fx.admin, &true);
    fx
}

/// Every ledger entry the contract holds, across all three storage tiers.
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

fn epalloc_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("epalloc").into_val(env);
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

/// Call with otherwise-valid arguments, assert the typed error, and assert that
/// nothing was written in any storage tier and no event was published.
fn assert_rejected(env: &Env, fx: &Fixture<'_>, caller: &Address, expected: Error) {
    let before = storage_snapshot(env, &fx.contract_id);

    assert_eq!(
        fx.escrow.try_emergency_pause_allocation(
            caller,
            &1_000_i128,
            &vec![env, 6_000_i128, 4_000_i128]
        ),
        Err(Ok(expected))
    );
    assert_eq!(
        epalloc_count(env),
        0,
        "a rejected call must publish no event"
    );

    let after = storage_snapshot(env, &fx.contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

// ── #536: unauthorized callers ───────────────────────────────────────────────

/// An address with no relationship to the escrow cannot have the contract
/// compute — or publish — a payout split for it.
#[test]
fn test_allocation_rejects_stranger_with_unauthorized() {
    let env = Env::default();
    let fx = paused(&env);
    let stranger = Address::generate(&env);
    assert_rejected(&env, &fx, &stranger, Error::Unauthorized);
}

/// The arbiter is a participant but receives no share of a paused balance, so
/// it is refused as well.
#[test]
fn test_allocation_rejects_arbiter_with_unauthorized() {
    let env = Env::default();
    let fx = paused(&env);
    assert_rejected(&env, &fx, &fx.arbiter, Error::Unauthorized);
}

/// An uninitialized contract has no job and no parties, so there is nothing to
/// authorize against.
#[test]
fn test_allocation_uninitialized_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    let before = storage_snapshot(&env, &contract_id);
    assert_eq!(
        escrow.try_emergency_pause_allocation(&caller, &1_000_i128, &vec![&env, 1_i128, 1_i128]),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(storage_snapshot(&env, &contract_id), before);
}

/// Admin, client and freelancer are all accepted, and each one must sign.
#[test]
fn test_allocation_accepts_every_party_and_requires_their_auth() {
    let env = Env::default();
    let fx = paused(&env);
    let weights = vec![&env, 6_000_i128, 4_000_i128];

    for caller in [&fx.admin, &fx.client_addr, &fx.freelancer] {
        let out = fx
            .escrow
            .emergency_pause_allocation(caller, &1_000_i128, &weights);
        assert!(
            env.auths().iter().any(|(addr, _)| addr == caller),
            "the caller's authorization must be required"
        );
        assert_eq!(out.get(0).unwrap(), 600);
        assert_eq!(out.get(1).unwrap(), 400);
    }
}

/// Without a signature the host rejects the call outright, before the contract
/// body runs, so no ledger entry can be touched.
#[test]
fn test_allocation_without_signature_is_rejected() {
    let env = Env::default();
    let fx = paused(&env);
    env.set_auths(&[]);

    let before = storage_snapshot(&env, &fx.contract_id);
    let result = fx.escrow.try_emergency_pause_allocation(
        &fx.admin,
        &1_000_i128,
        &vec![&env, 6_000_i128, 4_000_i128],
    );
    assert!(result.is_err(), "an unsigned call must not succeed");
    assert_eq!(storage_snapshot(&env, &fx.contract_id), before);
}

/// Authorization is checked before the source-state guards, so a stranger
/// learns nothing about whether the escrow is paused.
#[test]
fn test_allocation_authorizes_before_state_guards() {
    let env = Env::default();
    let fx = funded(&env);
    let stranger = Address::generate(&env);
    assert_rejected(&env, &fx, &stranger, Error::Unauthorized);
}

// ── #536: illegal source states ──────────────────────────────────────────────

/// A running escrow has no frozen balance to divide, so producing a split for
/// it is an illegal source state.
#[test]
fn test_allocation_rejects_running_escrow_with_not_paused() {
    let env = Env::default();
    let fx = funded(&env);
    assert_rejected(&env, &fx, &fx.client_addr, Error::NotPaused);
}

/// After the freeze is lifted the escrow is running again and the preview is
/// refused.
#[test]
fn test_allocation_rejects_after_unpause() {
    let env = Env::default();
    let fx = paused(&env);
    fx.escrow.emergency_pause_admin_override(&fx.admin, &false);
    assert_rejected(&env, &fx, &fx.freelancer, Error::NotPaused);
}

/// A half-applied pause transition is refused with its own error, whether or
/// not the `Ep` flag has been committed yet.
#[test]
fn test_allocation_rejects_pause_transition_in_progress() {
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

/// The authorization guard is the *first* statement, so a stranger calling
/// into an uninitialized contract gets `NotInitialized` — never a lower-level
/// panic from a missing job record.
#[test]
fn test_allocation_guards_run_before_any_ledger_read() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let stranger = Address::generate(&env);

    // Matches on the outer `Ok`: a trap (rather than a typed error) would
    // surface as `Err(Err(..))` and fail this assertion.
    assert_eq!(
        escrow.try_emergency_pause_allocation(
            &stranger,
            &i128::MAX,
            &vec![&env, i128::MAX, 1_i128]
        ),
        Err(Ok(Error::NotInitialized))
    );
}

// ── #536: argument validation still runs after the guards, still mutates
// nothing ───────────────────────────────────────────────────────────────────

/// Once every guard passes, the weight-vector guards keep their own typed
/// errors and still write nothing.
#[test]
fn test_allocation_argument_validation_after_guards() {
    let env = Env::default();
    let fx = paused(&env);
    let before = storage_snapshot(&env, &fx.contract_id);

    let mut over_cap: Vec<i128> = Vec::new(&env);
    for _ in 0..(MAX_EMERGENCY_ALLOCATION_PARTIES + 1) {
        over_cap.push_back(1_i128);
    }
    let empty: Vec<i128> = Vec::new(&env);

    let rejected = [
        (
            fx.escrow.try_emergency_pause_allocation(
                &fx.admin,
                &0_i128,
                &vec![&env, 1_i128, 1_i128],
            ),
            Error::InvalidAmount,
        ),
        (
            fx.escrow
                .try_emergency_pause_allocation(&fx.admin, &-1_i128, &vec![&env, 1_i128]),
            Error::InvalidAmount,
        ),
        (
            fx.escrow
                .try_emergency_pause_allocation(&fx.admin, &1_000_i128, &empty),
            Error::InvalidAllocationWeights,
        ),
        (
            fx.escrow
                .try_emergency_pause_allocation(&fx.admin, &1_000_i128, &over_cap),
            Error::InvalidAllocationWeights,
        ),
        (
            fx.escrow.try_emergency_pause_allocation(
                &fx.admin,
                &1_000_i128,
                &vec![&env, 3_i128, -1_i128],
            ),
            Error::InvalidAllocationWeights,
        ),
        (
            fx.escrow.try_emergency_pause_allocation(
                &fx.admin,
                &1_000_i128,
                &vec![&env, 0_i128, 0_i128, 0_i128],
            ),
            Error::InvalidAllocationWeights,
        ),
        (
            fx.escrow.try_emergency_pause_allocation(
                &fx.admin,
                &1_000_i128,
                &vec![&env, i128::MAX, 1_i128],
            ),
            Error::InvalidAllocationWeights,
        ),
        (
            fx.escrow.try_emergency_pause_allocation(
                &fx.admin,
                &i128::MAX,
                &vec![&env, 2_i128, 1_i128],
            ),
            Error::InvalidAmount,
        ),
    ];

    for (result, expected) in rejected.iter() {
        assert_eq!(*result, Err(Ok(*expected)));
    }
    assert_eq!(epalloc_count(&env), 0);
    assert_eq!(storage_snapshot(&env, &fx.contract_id), before);
}

/// A successful call writes nothing to the contract either — the endpoint is a
/// pure preview — and publishes exactly one `epalloc` event describing the
/// vector it returned.
#[test]
fn test_allocation_success_mutates_no_contract_storage() {
    let env = Env::default();
    let fx = paused(&env);
    let before = storage_snapshot(&env, &fx.contract_id);

    let out = fx.escrow.emergency_pause_allocation(
        &fx.freelancer,
        &1_001_i128,
        &vec![&env, 2_500_i128, 7_500_i128],
    );

    assert_eq!(epalloc_count(&env), 1);
    assert_eq!(storage_snapshot(&env, &fx.contract_id), before);

    let sum: i128 = out.iter().sum();
    assert_eq!(sum, 1_001, "the preview must still conserve the total");
}

/// The guards are pure reads, so they must not poison the endpoint: a valid
/// call in the same env, immediately after every kind of rejection, still
/// allocates and still conserves the total exactly.
#[test]
fn test_allocation_still_allocates_after_every_rejection() {
    let env = Env::default();
    let fx = paused(&env);
    let stranger = Address::generate(&env);

    // Unauthorized, not-initialized, not-paused-equivalent and malformed-input
    // rejections, all in one env.
    assert!(fx
        .escrow
        .try_emergency_pause_allocation(&stranger, &100_i128, &vec![&env, 1_i128, 1_i128])
        .is_err());

    let running = funded(&env);
    assert!(running
        .escrow
        .try_emergency_pause_allocation(
            &running.client_addr,
            &100_i128,
            &vec![&env, 1_i128, 1_i128]
        )
        .is_err());

    assert!(fx
        .escrow
        .try_emergency_pause_allocation(&fx.admin, &100_i128, &Vec::new(&env))
        .is_err());
    assert!(fx
        .escrow
        .try_emergency_pause_allocation(&fx.admin, &100_i128, &vec![&env, 0_i128, 0_i128])
        .is_err());
    assert!(fx
        .escrow
        .try_emergency_pause_allocation(&fx.admin, &100_i128, &vec![&env, 1_i128, -2_i128])
        .is_err());

    let out = fx.escrow.emergency_pause_allocation(
        &fx.admin,
        &100_i128,
        &vec![&env, 1_i128, 1_i128, 2_i128],
    );
    assert_eq!(out.len(), 3);
    let sum: i128 = out.iter().sum();
    assert_eq!(sum, 100, "the happy path must be unaffected by the guards");
}

/// The guard order is a security property, not a convenience: it must not
/// regress to "state first, then authorization", which would let any address
/// probe the escrow's pause status.
#[test]
fn test_allocation_rejection_order_is_authorization_then_state() {
    let env = Env::default();
    let fx = funded(&env);
    let stranger = Address::generate(&env);

    // A stranger on a *running* escrow: `Unauthorized` (not `NotPaused`).
    assert_eq!(
        fx.escrow
            .try_emergency_pause_allocation(&stranger, &100_i128, &vec![&env, 1_i128, 1_i128]),
        Err(Ok(Error::Unauthorized))
    );

    // The same stranger on an uninitialized contract: `NotInitialized`.
    let bare = Env::default();
    bare.mock_all_auths();
    let bare_id = bare.register(MilestoneEscrow, ());
    let bare_escrow = MilestoneEscrowClient::new(&bare, &bare_id);
    let bare_stranger = Address::generate(&bare);
    assert_eq!(
        bare_escrow.try_emergency_pause_allocation(
            &bare_stranger,
            &100_i128,
            &vec![&bare, 1_i128, 1_i128]
        ),
        Err(Ok(Error::NotInitialized))
    );
}
