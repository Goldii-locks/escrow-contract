#![cfg(test)]
//! Regression suite for the read-only query endpoints `is_emergency_paused`
//! and `is_multisig_approved` (issues #501, #502, #503, #504).
//!
//! # Zero state mutation (#501, #504)
//! Each mutation test takes a full ledger snapshot (`Env::to_ledger_snapshot`)
//! immediately before and after the call and asserts the two are identical.
//! The snapshot captures every ledger entry across instance, persistent, and
//! temporary storage for all registered contracts, including live-until
//! ledgers, so any `.set(`, `.remove(`, or `.extend_ttl(` added to the read
//! path (or its `read_*` helper) fails these tests.
//!
//! # Single read (#502)
//! `is_emergency_paused` and `ensure_not_paused` share one helper,
//! `read_emergency_paused`, which performs exactly one `get` on
//! `DataKey::Ep`.  The tests below pin the returned value across every state
//! so the consolidation cannot change behavior.
//!
//! # Documented return contract (#503)
//!
//! | Documented state                          | Test                                             |
//! |-------------------------------------------|--------------------------------------------------|
//! | Empty (uninitialized) → `false`           | `emergency_paused_is_false_when_uninitialized`   |
//! | Populated `false` after `initialize`      | `emergency_paused_is_false_after_initialize`     |
//! | Populated `true` after a pause            | `emergency_paused_is_true_after_pause`           |
//! | Populated `false` after an unpause        | `emergency_paused_is_false_after_unpause`        |
//! | Boundary: `EpLk` held, `Ep` unset/false   | `emergency_paused_ignores_transition_lock`       |
//! | Boundary: `EpLk` held, `Ep` true          | `emergency_paused_ignores_transition_lock`       |

use super::*;
use crate::{DataKey, Error};
use soroban_sdk::{vec, Address, Env, Vec};

fn initialized_escrow(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);
    token_admin.mint(&client_addr, &1_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    escrow.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604_800u64,
        &vec![env, 1_000_i128],
    );
    escrow.fund(&client_addr);

    (contract_id, admin, escrow)
}

fn multisig_escrow(env: &Env) -> (Address, Vec<Address>, MilestoneEscrowClient<'_>) {
    let (contract_id, admin, escrow) = initialized_escrow(env);

    let signers = vec![
        env,
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ];
    escrow.multisig_approval_init(&admin, &signers, &2u32);

    (contract_id, signers, escrow)
}

// ── #501: is_emergency_paused performs zero state mutation ──────────────────

#[test]
fn is_emergency_paused_does_not_mutate_ledger_when_unpaused() {
    let env = Env::default();
    let (_, _, escrow) = initialized_escrow(&env);

    let before = env.to_ledger_snapshot();
    let paused = escrow.is_emergency_paused();
    let after = env.to_ledger_snapshot();

    assert!(!paused);
    assert_eq!(
        before, after,
        "is_emergency_paused must not mutate the ledger"
    );
}

#[test]
fn is_emergency_paused_does_not_mutate_ledger_when_paused() {
    let env = Env::default();
    let (_, admin, escrow) = initialized_escrow(&env);
    escrow.emergency_pause_admin_override(&admin, &true);

    let before = env.to_ledger_snapshot();
    let paused = escrow.is_emergency_paused();
    let after = env.to_ledger_snapshot();

    assert!(paused);
    assert_eq!(
        before, after,
        "is_emergency_paused must not mutate the ledger"
    );
}

#[test]
fn is_emergency_paused_does_not_mutate_ledger_when_uninitialized() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    let paused = escrow.try_is_emergency_paused();
    let after = env.to_ledger_snapshot();

    assert_eq!(paused, Err(Ok(Error::NotInitialized)));
    assert_eq!(before, after);
    // Nothing may be materialized into storage.
    let stored = env.as_contract(&contract_id, || env.storage().instance().has(&DataKey::Ep));
    assert!(!stored);
}

/// Advance the ledger so any TTL bump would move a live-until ledger, then
/// confirm the snapshot is unchanged.
#[test]
fn is_emergency_paused_does_not_extend_ttl() {
    use soroban_sdk::testutils::Ledger as _;

    let env = Env::default();
    let (_, _, escrow) = initialized_escrow(&env);
    env.ledger().with_mut(|li| li.sequence_number += 100);

    let before = env.to_ledger_snapshot();
    escrow.is_emergency_paused();
    let after = env.to_ledger_snapshot();

    assert_eq!(before, after, "is_emergency_paused must not extend any TTL");
}

#[test]
fn is_emergency_paused_emits_no_events() {
    let env = Env::default();
    let (_, _, escrow) = initialized_escrow(&env);

    escrow.is_emergency_paused();
    // The test env only records events from the latest invocation.
    assert_eq!(crate::all_event_tuples(&env).len(), 0);
}

// ── #502: single read, identical results ─────────────────────────────────────

/// `is_emergency_paused` and the internal `ensure_not_paused` guard now share
/// `read_emergency_paused`; both must agree in every state.
#[test]
fn is_emergency_paused_matches_stored_flag_in_every_state() {
    let env = Env::default();
    let (contract_id, _, escrow) = initialized_escrow(&env);

    for (seed, expected) in [(None, false), (Some(false), false), (Some(true), true)] {
        env.as_contract(&contract_id, || match seed {
            Some(v) => env.storage().instance().set(&DataKey::Ep, &v),
            None => env.storage().instance().remove(&DataKey::Ep),
        });

        // The public read reports a missing flag as NotInitialized (#634);
        // the internal guard helper still treats it as "not paused".
        let public = escrow.try_is_emergency_paused();
        match seed {
            None => assert_eq!(public, Err(Ok(Error::NotInitialized))),
            Some(_) => assert_eq!(public, Ok(Ok(expected))),
        }
        let helper = env.as_contract(&contract_id, || {
            MilestoneEscrow::read_emergency_paused(&env)
        });
        assert_eq!(helper, expected);
        let guard = env.as_contract(&contract_id, || MilestoneEscrow::ensure_not_paused(&env));
        if expected {
            assert_eq!(guard, Err(Error::Paused));
        }
    }
}

/// Repeated calls return a stable value and never accumulate state.
#[test]
fn repeated_is_emergency_paused_calls_are_stable() {
    let env = Env::default();
    let (_, admin, escrow) = initialized_escrow(&env);
    escrow.emergency_pause_admin_override(&admin, &true);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        assert!(escrow.is_emergency_paused());
    }
    assert_eq!(before, env.to_ledger_snapshot());
}

// ── #503: documented return contract ─────────────────────────────────────────

#[test]
fn emergency_paused_uninitialized_returns_not_initialized() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    // #634: an uninitialized contract is reported as NotInitialized rather
    // than a defaulted `false`.
    assert_eq!(
        escrow.try_is_emergency_paused(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn emergency_paused_is_false_after_initialize() {
    let env = Env::default();
    let (contract_id, _, escrow) = initialized_escrow(&env);

    let stored: Option<bool> =
        env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::Ep));
    assert_eq!(stored, Some(false));
    assert!(!escrow.is_emergency_paused());
}

#[test]
fn emergency_paused_is_true_after_pause() {
    let env = Env::default();
    let (_, admin, escrow) = initialized_escrow(&env);

    escrow.emergency_pause_admin_override(&admin, &true);
    assert!(escrow.is_emergency_paused());
}

#[test]
fn emergency_paused_is_false_after_unpause() {
    let env = Env::default();
    let (_, admin, escrow) = initialized_escrow(&env);

    escrow.emergency_pause_admin_override(&admin, &true);
    escrow.emergency_pause_admin_override(&admin, &false);
    assert!(!escrow.is_emergency_paused());
}

/// Only the committed `Ep` flag is reported; a held transition lock does not
/// change the result either way.
#[test]
fn emergency_paused_ignores_transition_lock() {
    let env = Env::default();
    let (contract_id, _, escrow) = initialized_escrow(&env);

    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::EpLk, &true);
    });
    assert!(!escrow.is_emergency_paused());

    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Ep, &true);
    });
    assert!(escrow.is_emergency_paused());
}

// ── #504: is_multisig_approved performs zero state mutation ──────────────────

#[test]
fn is_multisig_approved_does_not_mutate_ledger_with_no_approvals() {
    let env = Env::default();
    let (_, _, escrow) = multisig_escrow(&env);

    let before = env.to_ledger_snapshot();
    let state = escrow.is_multisig_approved(&7u32);
    let after = env.to_ledger_snapshot();

    assert!(!state.approved);
    assert_eq!(state.approvals, 0);
    assert_eq!(state.bitmap, 0);
    assert_eq!(
        before, after,
        "is_multisig_approved must not mutate the ledger"
    );
}

#[test]
fn is_multisig_approved_does_not_mutate_ledger_below_threshold() {
    let env = Env::default();
    let (_, signers, escrow) = multisig_escrow(&env);
    escrow.multisig_approve(&signers.get(0).unwrap(), &1u32);

    let before = env.to_ledger_snapshot();
    let state = escrow.is_multisig_approved(&1u32);
    let after = env.to_ledger_snapshot();

    assert!(!state.approved);
    assert_eq!(state.approvals, 1);
    assert_eq!(
        before, after,
        "is_multisig_approved must not mutate the ledger"
    );
}

#[test]
fn is_multisig_approved_does_not_mutate_ledger_when_approved() {
    let env = Env::default();
    let (_, signers, escrow) = multisig_escrow(&env);
    escrow.multisig_approve(&signers.get(0).unwrap(), &1u32);
    escrow.multisig_approve(&signers.get(1).unwrap(), &1u32);

    let before = env.to_ledger_snapshot();
    let state = escrow.is_multisig_approved(&1u32);
    let after = env.to_ledger_snapshot();

    assert!(state.approved);
    assert_eq!(state.approvals, 2);
    assert_eq!(state.threshold, 2);
    assert_eq!(
        before, after,
        "is_multisig_approved must not mutate the ledger"
    );
}

/// The temporary approval bitmap's TTL must not be bumped by a read.
#[test]
fn is_multisig_approved_does_not_extend_ttl() {
    use soroban_sdk::testutils::Ledger as _;

    let env = Env::default();
    let (_, signers, escrow) = multisig_escrow(&env);
    escrow.multisig_approve(&signers.get(0).unwrap(), &1u32);
    env.ledger().with_mut(|li| li.sequence_number += 10);

    let before = env.to_ledger_snapshot();
    escrow.is_multisig_approved(&1u32);
    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "is_multisig_approved must not extend any TTL"
    );
}

#[test]
fn is_multisig_approved_emits_no_events() {
    let env = Env::default();
    let (_, signers, escrow) = multisig_escrow(&env);
    escrow.multisig_approve(&signers.get(0).unwrap(), &1u32);

    escrow.is_multisig_approved(&1u32);
    // The test env only records events from the latest invocation.
    assert_eq!(crate::all_event_tuples(&env).len(), 0);
}

/// The error path is read-only too.
#[test]
fn is_multisig_approved_uninitialized_does_not_mutate_ledger() {
    let env = Env::default();
    let (_, _, escrow) = initialized_escrow(&env);

    let before = env.to_ledger_snapshot();
    let res = escrow.try_is_multisig_approved(&1u32);
    let after = env.to_ledger_snapshot();

    assert_eq!(res, Err(Ok(Error::NotInitialized)));
    assert_eq!(before, after);
}
