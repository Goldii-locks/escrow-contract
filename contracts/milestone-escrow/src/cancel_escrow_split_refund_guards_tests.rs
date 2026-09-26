#![cfg(test)]
//! Source-state guards for `cancel_escrow_split_refund` (issue #528).
//!
//! The endpoint is a pure split calculator, so it used to be callable with no
//! state validation at all — including while the contract was frozen or a
//! cancel was already in flight. It now runs the two source-state guards
//! (`ensure_not_paused` → `assert_not_paused`) as its first operations, before
//! any amount or ratio validation and before any arithmetic.
//!
//! The matrix below pins three properties:
//! 1. Each illegal source state maps to its own typed error.
//! 2. Guard ordering is observable: a paused escrow beats an invalid amount and
//!    an invalid ratio, so callers see the reason the call was refused.
//! 3. A refused call publishes no event and mutates no ledger entry.
//!
//! The guards read only instance storage, which is what makes property 3 hold:
//! nothing is written before the rejection.

use super::*;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::{
    testutils::Address as _, testutils::EnvTestConfig, vec, Address, Env, FromVal, IntoVal, Map,
    Symbol, TryIntoVal, Val,
};

/// Snapshot capture is disabled; no assertion here reads a JSON snapshot.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

struct Split {
    contract_id: Address,
    escrow: MilestoneEscrowClient<'static>,
}

/// A registered, initialised escrow. The guards are keyed on instance state, so
/// an initialised contract is what lets each guard be triggered in isolation.
fn initialised_escrow(env: &Env) -> Split {
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

    Split {
        contract_id,
        escrow,
    }
}

fn set_instance_flag(env: &Env, contract_id: &Address, key: &DataKey, value: bool) {
    env.as_contract(contract_id, || {
        env.storage().instance().set(key, &value);
    });
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

/// Count events carrying `topic` as their first topic.
///
/// The SDK's event log reflects only the most recent invocation, so this counts
/// the log rather than diffing it: an assertion that a rejected call publishes
/// nothing is then valid whether or not the failed frame cleared the log.
fn count_topic(env: &Env, topic: soroban_sdk::Symbol) -> u32 {
    let topic_val: Val = topic.into_val(env);
    crate::all_event_tuples(env)
        .iter()
        .fold(0u32, |acc, event| {
            if let Some(first) = event.1.get(0) {
                if first.get_payload() == topic_val.get_payload() {
                    return acc + 1;
                }
            }
            acc
        })
}

// ── each illegal source state gets its own typed error ───────────────────────

/// Emergency pause in force: the split must not be computed against a frozen
/// contract.
#[test]
fn test_cancel_escrow_split_refund_emergency_pause_rejected() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Ep, true);

    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::Paused))
    );
}

/// Admin pause in force: same outcome as the emergency pause.
#[test]
fn test_cancel_escrow_split_refund_admin_pause_rejected() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Paused, true);

    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::Paused))
    );
}

/// A cancel already in flight: the calculator must not run against a
/// half-applied cancellation, and reports the lock rather than a generic error.
#[test]
fn test_cancel_escrow_split_refund_cancel_in_flight_rejected() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::CancelLock, true);

    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::EscrowLocked))
    );
}

/// The emergency pause is checked first, so it wins over a held cancel lock
/// rather than the two errors being interchangeable.
#[test]
fn test_cancel_escrow_split_refund_emergency_pause_takes_precedence_over_cancel_lock() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Ep, true);
    set_instance_flag(&env, &s.contract_id, &DataKey::CancelLock, true);

    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::Paused))
    );
}

/// Clearing both flags restores the call: the guards are the only thing that
/// changed, and they are not a permanent latch.
#[test]
fn test_cancel_escrow_split_refund_succeeds_once_every_flag_is_cleared() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Ep, true);
    set_instance_flag(&env, &s.contract_id, &DataKey::CancelLock, true);
    set_instance_flag(&env, &s.contract_id, &DataKey::Paused, true);

    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::Paused))
    );

    set_instance_flag(&env, &s.contract_id, &DataKey::Ep, false);
    set_instance_flag(&env, &s.contract_id, &DataKey::CancelLock, false);
    set_instance_flag(&env, &s.contract_id, &DataKey::Paused, false);

    let allocation = s
        .escrow
        .cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32);
    assert_eq!(allocation.client_refund, 500);
    assert_eq!(allocation.freelancer_payout, 500);
}

// ── guard ordering is observable ─────────────────────────────────────────────

/// A paused escrow reports `Paused`, not `InvalidAmount`, even when the amount
/// is also invalid — the source state is the reason the call was refused.
#[test]
fn test_cancel_escrow_split_refund_pause_guard_precedes_amount_validation() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Ep, true);

    for total in [0_i128, -1_i128, i128::MIN] {
        assert_eq!(
            s.escrow
                .try_cancel_escrow_split_refund(&total, &5_000u32, &5_000u32),
            Err(Ok(Error::Paused))
        );
    }
}

/// Likewise for a held cancel lock against an invalid amount.
#[test]
fn test_cancel_escrow_split_refund_lock_guard_precedes_amount_validation() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::CancelLock, true);

    for total in [0_i128, -1_i128, i128::MIN] {
        assert_eq!(
            s.escrow
                .try_cancel_escrow_split_refund(&total, &5_000u32, &5_000u32),
            Err(Ok(Error::EscrowLocked))
        );
    }
}

/// And for an invalid ratio: the guard fires first, so the caller is told the
/// contract is paused rather than that their basis points are wrong.
#[test]
fn test_cancel_escrow_split_refund_pause_guard_precedes_ratio_validation() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Paused, true);

    let bad_ratios = [
        (1_000_u32, 1_000_u32),
        (0_u32, 0_u32),
        (u32::MAX, u32::MAX),
        (0_u32, 10_000_u32),
        (10_000_u32, 0_u32),
    ];
    for (client_bps, freelancer_bps) in bad_ratios {
        assert_eq!(
            s.escrow
                .try_cancel_escrow_split_refund(&1_000_i128, &client_bps, &freelancer_bps),
            Err(Ok(Error::Paused))
        );
    }
}

/// With no illegal source state, the original argument validation is unchanged
/// and still reports its own errors — the guards did not swallow them.
#[test]
fn test_cancel_escrow_split_refund_argument_errors_survive_the_guards() {
    let env = test_env();
    let s = initialised_escrow(&env);

    for total in [0_i128, -1_i128, i128::MIN] {
        assert_eq!(
            s.escrow
                .try_cancel_escrow_split_refund(&total, &5_000u32, &5_000u32),
            Err(Ok(Error::InvalidAmount))
        );
    }
    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &1_000u32, &1_000u32),
        Err(Ok(Error::InvalidRatio))
    );
}

// ── a refused call leaves no trace ───────────────────────────────────────────

/// A guard rejection publishes no `cxlspref` event, so an indexer never sees a
/// split for a contract that was in fact frozen.
#[test]
fn test_cancel_escrow_split_refund_paused_call_publishes_no_event() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Ep, true);

    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::Paused))
    );
    assert_eq!(
        count_topic(&env, soroban_sdk::symbol_short!("cxlspref")),
        0,
        "no split event on a paused call"
    );
}

/// No ledger entry is added, removed, or changed by a guard rejection. The
/// guards only read the instance entry, so the whole storage tree is identical
/// before and after.
#[test]
fn test_cancel_escrow_split_refund_paused_call_mutates_no_storage() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::Ep, true);

    let before = storage_snapshot(&env, &s.contract_id);
    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::Paused))
    );
    let after = storage_snapshot(&env, &s.contract_id);

    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

/// The same for the cancel-in-flight guard.
#[test]
fn test_cancel_escrow_split_refund_locked_call_mutates_no_storage() {
    let env = test_env();
    let s = initialised_escrow(&env);
    set_instance_flag(&env, &s.contract_id, &DataKey::CancelLock, true);

    let before = storage_snapshot(&env, &s.contract_id);
    assert_eq!(
        s.escrow
            .try_cancel_escrow_split_refund(&1_000_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::EscrowLocked))
    );
    let after = storage_snapshot(&env, &s.contract_id);

    assert_eq!(after.0, before.0, "instance storage must be untouched");
    assert_eq!(after.1, before.1, "persistent storage must be untouched");
    assert_eq!(after.2, before.2, "temporary storage must be untouched");
}

/// The guards are keyless with respect to initialisation: an uninitialised
/// contract has neither flag set, so the calculator still works. This is the
/// existing uninitialised-calculator behaviour, now stated explicitly so a
/// future guard added before `load_job_meta` cannot silently break it.
#[test]
fn test_cancel_escrow_split_refund_uninitialized_calculator_still_works() {
    let env = test_env();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    assert!(matches!(
        escrow.try_get_job(),
        Err(Ok(Error::NotInitialized))
    ));

    let allocation = escrow.cancel_escrow_split_refund(&1_000_i128, &7_000u32, &3_000u32);
    assert_eq!(allocation.client_refund, 700);
    assert_eq!(allocation.freelancer_payout, 300);
}

/// The success path still publishes exactly one `cxlspref` event with the
/// allocation, so adding the guards did not disturb the event stream.
#[test]
fn test_cancel_escrow_split_refund_success_path_still_emits_one_event() {
    let env = test_env();
    let s = initialised_escrow(&env);

    s.escrow
        .cancel_escrow_split_refund(&1_000_i128, &7_000u32, &3_000u32);
    assert_eq!(
        count_topic(&env, soroban_sdk::symbol_short!("cxlspref")),
        1,
        "expected exactly one cxlspref event"
    );

    let events = crate::all_event_tuples(&env);
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(
        topic,
        soroban_sdk::symbol_short!("cxlspref"),
        "last event must be the split-refund calculation"
    );
    let data = CancelSplitRefundCalculatedEvent::from_val(&env, &last.2);
    assert_eq!(data.client_refund, 700);
    assert_eq!(data.freelancer_payout, 300);
    assert_eq!(data.client_refund_bps, 7_000);
    assert_eq!(data.freelancer_payout_bps, 3_000);
}
