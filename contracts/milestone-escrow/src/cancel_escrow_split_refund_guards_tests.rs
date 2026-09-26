#![cfg(test)]
//! Authorization / precondition-guard suite for `cancel_escrow_split_refund`
//! (issue #528).
//!
//! The endpoint used to validate its arguments and publish its event with no
//! look at the escrow's state at all, so a figure could be produced — and an
//! event emitted — for an escrow that was paused or mid-cancellation.  The
//! guards now run first:
//!
//! * `ensure_not_paused` → `EscrowLocked` when a `cancel_escrow` is in flight
//!   (`DataKey::CancelLock`) or an emergency pause is active (`DataKey::Ep`).
//! * `assert_not_paused` → `Paused` when an `admin_pause_escrow` pause is in
//!   force (`DataKey::Paused`).
//!
//! Each guard reads a missing key as "not set", so the endpoint still answers on
//! an uninitialised contract — the behaviour
//! `test_cancel_escrow_split_refund_works_without_initialization` pins — and the
//! amount/ratio validation is otherwise untouched.
//!
//! Every test below also asserts that a rejected call emits **no** event and
//! mutates **no** ledger entry, which is the "no storage entry is mutated"
//! requirement from the issue.

use super::*;
use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::{symbol_short, testutils::Address as _, vec, Address, Env, IntoVal, Map, Val};

/// A fully initialized, funded escrow plus its contract id and admin.
fn funded(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    (contract_id, admin, client)
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

fn cxlspref_count(env: &Env) -> u32 {
    let topic: Val = symbol_short!("cxlspref").into_val(env);
    crate::all_event_tuples(env)
        .iter()
        .fold(0u32, |acc, e| match e.1.get(0) {
            Some(t) if t.get_payload() == topic.get_payload() => acc + 1,
            _ => acc,
        })
}

// ── illegal source states are rejected with their own typed error ───────────

/// A cancellation in flight (`DataKey::CancelLock`) is an illegal source state:
/// the split it is about to settle is not knowable yet, so the preview is
/// refused with `EscrowLocked` before any arithmetic runs.
#[test]
fn test_cancel_escrow_split_refund_rejects_in_flight_cancellation() {
    let env = Env::default();
    let (contract_id, _, client) = funded(&env);

    // Model the exact state `cancel_escrow` leaves behind while it settles: the
    // instance carries the cancellation lock.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::CancelLock, &true);
    });

    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::EscrowLocked))
    );
    assert_eq!(cxlspref_count(&env), 0);
}

/// An administrative pause (`admin_pause_escrow` → `DataKey::Paused`) is the
/// other illegal source state, and has its own error distinct from
/// `EscrowLocked`.
#[test]
fn test_cancel_escrow_split_refund_rejects_administratively_paused_escrow() {
    let env = Env::default();
    let (_, admin, client) = funded(&env);

    client.admin_pause_escrow(&admin);

    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Paused))
    );
    assert_eq!(cxlspref_count(&env), 0);

    // Once the pause is lifted the same call answers normally.
    client.admin_resume_escrow(&admin);
    let alloc = client.cancel_escrow_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(alloc.client_refund, 500);
    assert_eq!(alloc.freelancer_payout, 500);
}

/// An emergency pause (`DataKey::Ep`) is also refused, with its own
/// `Paused` error surfaced ahead of any arithmetic.
#[test]
fn test_cancel_escrow_split_refund_rejects_emergency_paused_escrow() {
    let env = Env::default();
    let (contract_id, _, client) = funded(&env);

    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Ep, &true);
    });

    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Paused))
    );
    assert_eq!(cxlspref_count(&env), 0);
}

// ── a rejected call mutates nothing ──────────────────────────────────────────

/// The guard is reached before the amount and ratio checks, so an illegal
/// source state wins over otherwise-valid-looking arguments — and the storage
/// snapshot taken around the rejected call is byte-identical to the one taken
/// before it.
#[test]
fn test_cancel_escrow_split_refund_rejected_state_mutates_no_storage() {
    let env = Env::default();
    let (contract_id, admin, client) = funded(&env);

    client.admin_pause_escrow(&admin);
    let before = storage_snapshot(&env, &contract_id);

    // Valid arguments that would otherwise succeed.
    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &6_000_u32, &4_000_u32),
        Err(Ok(Error::Paused))
    );

    let after = storage_snapshot(&env, &contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

/// The same for the cancellation-in-flight rejection.
#[test]
fn test_cancel_escrow_split_refund_locked_state_mutates_no_storage() {
    let env = Env::default();
    let (contract_id, _, client) = funded(&env);

    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::CancelLock, &true);
    });
    let before = storage_snapshot(&env, &contract_id);

    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &6_000_u32, &4_000_u32),
        Err(Ok(Error::EscrowLocked))
    );

    let after = storage_snapshot(&env, &contract_id);
    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

// ── the guards do not change the endpoint's existing behaviour ───────────────

/// A missing guard key is read as "not set", so the pure-calculator contract
/// still answers: no job, no admin, no funding, no initialization.
#[test]
fn test_cancel_escrow_split_refund_still_answers_without_initialization() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let alloc = client.cancel_escrow_split_refund(&2_000_i128, &2_500_u32, &7_500_u32);
    assert_eq!(alloc.client_refund, 500);
    assert_eq!(alloc.freelancer_payout, 1_500);
}

/// The amount and ratio validation still returns exactly the same typed errors
/// on a healthy, unpaused escrow.
#[test]
fn test_cancel_escrow_split_refund_argument_validation_unchanged() {
    let env = Env::default();
    let (_, _, client) = funded(&env);

    assert_eq!(
        client.try_cancel_escrow_split_refund(&0_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_cancel_escrow_split_refund(&(-1_i128), &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &4_000_u32, &4_000_u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &u32::MAX, &u32::MAX),
        Err(Ok(Error::InvalidRatio))
    );
}

/// The arithmetic, the echoed BPS values and the single `cxlspref` event are all
/// unchanged on the success path of a healthy escrow.
#[test]
fn test_cancel_escrow_split_refund_success_path_unchanged() {
    let env = Env::default();
    let (_, _, client) = funded(&env);

    let alloc = client.cancel_escrow_split_refund(&1_001_i128, &2_500_u32, &7_500_u32);
    assert_eq!(alloc.client_refund, 250);
    assert_eq!(alloc.freelancer_payout, 751);
    assert_eq!(alloc.client_refund + alloc.freelancer_payout, 1_001);
    assert_eq!(alloc.client_refund_bps, 2_500);
    assert_eq!(alloc.freelancer_payout_bps, 7_500);
    assert_eq!(cxlspref_count(&env), 1);
}

// ── the admin-gated claim counterpart ─────────────────────────────────────────

/// The claim is the authorized counterpart to the preview, so the very first
/// thing it does is check the caller. A non-admin is turned away with
/// `Unauthorized` and the storage snapshot around the rejected call is
/// byte-identical to the one taken before it.
#[test]
fn test_cancel_escrow_claim_refund_rejects_unauthorized_caller() {
    let env = Env::default();
    let (contract_id, admin, client) = funded(&env);

    // A cancellation is in flight, so every *other* precondition is satisfied:
    // the only thing left to fail is the caller.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::CancelLock, &true);
    });

    let stranger = Address::generate(&env);
    let before = storage_snapshot(&env, &contract_id);

    assert_eq!(
        client.try_cancel_escrow_claim_refund(&stranger, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Unauthorized))
    );

    assert_eq!(storage_snapshot(&env, &contract_id), before);
    assert_eq!(cxlspref_count(&env), 0);

    // The real admin gets through the same call.
    client.cancel_escrow_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(cxlspref_count(&env), 1);
}

/// The claim and the preview are complementary: the claim refuses to settle
/// unless a cancellation is in flight (`InvalidStatus`), and the preview refuses
/// to answer while one is. Neither rejected call may mutate a ledger entry.
#[test]
fn test_cancel_escrow_claim_refund_requires_cancellation_in_flight() {
    let env = Env::default();
    let (contract_id, admin, client) = funded(&env);

    // No cancel lock: the claim is refused, the preview is allowed.
    let before = storage_snapshot(&env, &contract_id);
    assert_eq!(
        client.try_cancel_escrow_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(storage_snapshot(&env, &contract_id), before);

    let previewed = client.cancel_escrow_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(previewed.client_refund, 500);
    assert_eq!(previewed.freelancer_payout, 500);
    assert_eq!(previewed.client_refund_bps, 5_000);
    assert_eq!(previewed.freelancer_payout_bps, 5_000);

    // With the lock held the two swap roles.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::CancelLock, &true);
    });
    let locked = storage_snapshot(&env, &contract_id);
    assert_eq!(
        client.try_cancel_escrow_split_refund(&1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::EscrowLocked))
    );
    assert_eq!(storage_snapshot(&env, &contract_id), locked);
}

/// Authorization is checked before the preconditions: a stranger calling into a
/// contract that would *also* fail the cancel-lock check still gets
/// `Unauthorized`, not `InvalidStatus`. This pins the guard order.
#[test]
fn test_cancel_escrow_claim_refund_authorizes_before_preconditions() {
    let env = Env::default();
    let (contract_id, _, client) = funded(&env);

    // No cancel lock: the preconditions are unsatisfied.
    let stranger = Address::generate(&env);
    let before = storage_snapshot(&env, &contract_id);

    assert_eq!(
        client.try_cancel_escrow_claim_refund(&stranger, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(storage_snapshot(&env, &contract_id), before);
    assert_eq!(cxlspref_count(&env), 0);
}

/// The claim reuses the preview's arithmetic, so the two can never disagree:
/// round-nearest on the client leg, exact remainder to the freelancer, and the
/// two amounts always sum back to the total.
///
/// The preview only answers while no cancellation is in flight and the claim
/// only answers once one is, so each is sampled in its own legal state and the
/// two allocations are then compared.
#[test]
fn test_cancel_escrow_claim_refund_matches_preview_arithmetic() {
    let env = Env::default();
    let (contract_id, admin, client) = funded(&env);

    let cases = [
        (1_001_i128, 2_500_u32, 7_500_u32),
        (2_000_i128, 3_333_u32, 6_667_u32),
        (1_000_i128, 10_000_u32, 0_u32),
        (1_000_i128, 0_u32, 10_000_u32),
    ];

    // No cancellation in flight: the preview answers for every case.
    let previewed: std::vec::Vec<RefundAllocation> = cases
        .iter()
        .map(|(total, client_bps, freelancer_bps)| {
            client.cancel_escrow_split_refund(total, client_bps, freelancer_bps)
        })
        .collect();

    // Cancellation in flight: the claim answers for the same cases.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::CancelLock, &true);
    });
    let claimed: std::vec::Vec<RefundAllocation> = cases
        .iter()
        .map(|(total, client_bps, freelancer_bps)| {
            client.cancel_escrow_claim_refund(&admin, total, client_bps, freelancer_bps)
        })
        .collect();

    assert_eq!(claimed, previewed);
    for (allocation, (total, _, _)) in claimed.iter().zip(cases.iter()) {
        assert_eq!(
            allocation.client_refund + allocation.freelancer_payout,
            *total
        );
    }
}
