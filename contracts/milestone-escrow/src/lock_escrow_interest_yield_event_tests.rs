//! Dedicated test suite for the `yldlock` structured event emitted by
//! `lock_escrow_interest_yield`.
//!
//! `lock_escrow_interest_yield` is the point at which the interest/yield share
//! configuration stops being mutable: from here on every write path
//! (`set_escrow_interest_yield`, `set_interest_yield_consent`) and the
//! `interest_yield_split_refund` read path is gated on
//! `Error::EscrowLocked`. A lock that leaves no ledger trace is only
//! reconstructible by replaying storage, so the call publishes a typed event
//! carrying the acting admin and the resulting values.
//!
//! Every success-path test asserts that the emitted
//! `EscrowInterestYieldLockedEvent` fields reconcile exactly with the state the
//! call persisted under `DataKey::InterestYieldState` — checked both through
//! the public getter and through a direct instance-storage read, so the
//! assertion cannot pass on a getter that disagrees with the ledger. Every
//! failure-path test asserts that no `yldlock` event is published and that the
//! persisted state is untouched.
//!
//! The event buffer exposed by the test `Env` reflects the most recent
//! top-level invocation only, so each test tallies events immediately after the
//! call it is asserting on (and re-reads the state, which is durable, whenever
//! a later call would otherwise clear the buffer).

use super::*;
use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, EscrowInterestYieldState};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

const YLDLOCK_TOPIC: &str = "yldlock";

fn yldlock_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("yldlock").into_val(env);
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

/// The most recently published event, asserting it carries the `yldlock`
/// topic so a test can never accidentally read a neighbouring event's payload.
fn last_yldlock_event(env: &Env) -> EscrowInterestYieldLockedEvent {
    let events = crate::all_event_tuples(env);
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, YLDLOCK_TOPIC));
    EscrowInterestYieldLockedEvent::from_val(env, &last.2)
}

/// Direct read of the instance-storage entry the call wrote, bypassing the
/// public getter so the reconciliation assertion is against the ledger.
fn stored_state(env: &Env, contract_id: &Address) -> Option<EscrowInterestYieldState> {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&DataKey::InterestYieldState)
    })
}

/// A funded escrow whose interest/yield config is already set to
/// `client_bps` / `freelancer_bps` and left unlocked.
fn escrow_with_yield_shares(
    env: &Env,
    client_bps: u32,
    freelancer_bps: u32,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, _token, contract_id, client) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin_addr, &client_bps, &freelancer_bps);
    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        admin_addr,
        contract_id,
        client,
    )
}

/// Assert the event describes exactly the state now on the ledger.
fn assert_event_reconciles(
    env: &Env,
    ev: &EscrowInterestYieldLockedEvent,
    contract_id: &Address,
    client: &MilestoneEscrowClient,
) {
    let stored = stored_state(env, contract_id).expect("lock persisted a state");
    assert_eq!(ev.client_share_bps, stored.client_share_bps);
    assert_eq!(ev.freelancer_share_bps, stored.freelancer_share_bps);
    assert_eq!(ev.locked, stored.locked);
    assert!(stored.locked, "lock must leave the state locked");

    let via_getter = client.get_escrow_interest_yield();
    assert_eq!(ev.client_share_bps, via_getter.client_share_bps);
    assert_eq!(ev.freelancer_share_bps, via_getter.freelancer_share_bps);
    assert_eq!(ev.locked, via_getter.locked);
    assert!(client.is_escrow_interest_yield_locked());

    // The lock froze a ratio that still satisfies the share invariant.
    assert_eq!(ev.client_share_bps + ev.freelancer_share_bps, BPS_SCALE);
}

// ── success path: field-by-field reconciliation ─────────────────────────────

/// The canonical 50/50 lock. Every field of the event must equal the state the
/// call persisted, and that state must be locked.
#[test]
fn event_reconciles_with_persisted_state_on_lock() {
    let env = Env::default();
    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, contract_id, client) =
        escrow_with_yield_shares(&env, 5_000, 5_000);
    assert!(!client.is_escrow_interest_yield_locked());

    client.lock_escrow_interest_yield(&admin_addr);

    // Exactly one structured event, and it is the last thing on the ledger.
    assert_eq!(yldlock_event_count(&env), 1);
    let ev = last_yldlock_event(&env);

    // Acting address.
    assert_eq!(ev.admin, admin_addr);

    // Values, and their reconciliation with the persisted state.
    assert_eq!(ev.client_share_bps, 5_000);
    assert_eq!(ev.freelancer_share_bps, 5_000);
    assert!(ev.locked);
    assert_event_reconciles(&env, &ev, &contract_id, &client);
}

/// The payload must reflect the configuration actually frozen, not a constant:
/// lock a 70/30 split and check the event carries those shares.
#[test]
fn event_carries_the_shares_frozen_by_the_lock() {
    let env = Env::default();
    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, contract_id, client) =
        escrow_with_yield_shares(&env, 7_000, 3_000);

    client.lock_escrow_interest_yield(&admin_addr);

    assert_eq!(yldlock_event_count(&env), 1);
    let ev = last_yldlock_event(&env);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.client_share_bps, 7_000);
    assert_eq!(ev.freelancer_share_bps, 3_000);
    assert!(ev.locked);
    assert_event_reconciles(&env, &ev, &contract_id, &client);
}

/// Boundary ratios (all-to-one-party) are valid shares, so they lock and
/// reconcile like any other configuration.
#[test]
fn event_reconciles_on_boundary_shares() {
    let env = Env::default();
    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, contract_id, client) =
        escrow_with_yield_shares(&env, 10_000, 0);

    client.lock_escrow_interest_yield(&admin_addr);

    assert_eq!(yldlock_event_count(&env), 1);
    let ev = last_yldlock_event(&env);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.client_share_bps, 10_000);
    assert_eq!(ev.freelancer_share_bps, 0);
    assert!(ev.locked);
    assert_event_reconciles(&env, &ev, &contract_id, &client);
}

/// The dual-consent path (`set_interest_yield_consent`) writes the same
/// `EscrowInterestYieldState`, so locking after it must emit an event that
/// reconciles with that state too.
#[test]
fn event_reconciles_after_consent_configured_the_shares() {
    let env = Env::default();
    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, contract_id, client) =
        escrow_with_yield_shares(&env, 5_000, 5_000);
    // Reconfigure through the consent path, which publishes its own `yldcons`
    // event under a different topic.
    client.set_interest_yield_consent(&admin_addr, &2_500, &7_500);

    client.lock_escrow_interest_yield(&admin_addr);

    // Only the lock publishes `yldlock`.
    assert_eq!(yldlock_event_count(&env), 1);
    let ev = last_yldlock_event(&env);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.client_share_bps, 2_500);
    assert_eq!(ev.freelancer_share_bps, 7_500);
    assert!(ev.locked);
    assert_event_reconciles(&env, &ev, &contract_id, &client);
}

/// Each lock publishes exactly one event describing the shares it froze, so an
/// indexer replaying `yldlock` reconstructs the lock history rather than only
/// the latest state.
#[test]
fn each_lock_publishes_exactly_one_event_with_the_frozen_shares() {
    let env = Env::default();
    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, contract_id, client) =
        escrow_with_yield_shares(&env, 6_000, 4_000);

    client.lock_escrow_interest_yield(&admin_addr);
    assert_eq!(yldlock_event_count(&env), 1);
    let first = last_yldlock_event(&env);
    assert_eq!(first.admin, admin_addr);
    assert_eq!(first.client_share_bps, 6_000);
    assert_eq!(first.freelancer_share_bps, 4_000);
    assert!(first.locked);
    assert_event_reconciles(&env, &first, &contract_id, &client);

    // Unlocking is a separate endpoint: it must not borrow the lock topic, or a
    // replay that only watched `yldlock` would mistake an unlock for a lock.
    client.unlock_escrow_interest_yield(&admin_addr);
    assert_eq!(
        yldlock_event_count(&env),
        0,
        "unlock_escrow_interest_yield must not publish a yldlock event"
    );
    assert!(!client.is_escrow_interest_yield_locked());

    // Lock a different configuration and check the second event describes it.
    client.set_escrow_interest_yield(&admin_addr, &3_000, &7_000);
    client.lock_escrow_interest_yield(&admin_addr);
    assert_eq!(yldlock_event_count(&env), 1);
    let second = last_yldlock_event(&env);
    assert_eq!(second.admin, admin_addr);
    assert_eq!(second.client_share_bps, 3_000);
    assert_eq!(second.freelancer_share_bps, 7_000);
    assert!(second.locked);
    assert_event_reconciles(&env, &second, &contract_id, &client);

    // The two records differ, so the event is a per-call record and not a
    // repeated copy of one payload.
    assert_ne!(first.client_share_bps, second.client_share_bps);
    assert_ne!(first.freelancer_share_bps, second.freelancer_share_bps);
}

/// The lock is enforced before the event exists: once `yldlock` is on the
/// ledger the configuration must already reject mutation, so an indexer
/// replaying the event sees the same state the contract enforces.
#[test]
fn lock_is_enforced_after_the_event_is_published() {
    let env = Env::default();
    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _contract_id, client) =
        escrow_with_yield_shares(&env, 5_000, 5_000);

    client.lock_escrow_interest_yield(&admin_addr);

    assert_eq!(yldlock_event_count(&env), 1);
    let ev = last_yldlock_event(&env);
    assert!(ev.locked, "event must report the lock as held");

    // Both share-mutation paths are gated on the flag the event reported.
    assert_eq!(
        client.try_set_escrow_interest_yield(&admin_addr, &9_000, &1_000),
        Err(Ok(Error::EscrowLocked))
    );
    assert_eq!(
        client.try_set_interest_yield_consent(&admin_addr, &9_000, &1_000),
        Err(Ok(Error::EscrowLocked))
    );

    let stored = client.get_escrow_interest_yield();
    assert_eq!(stored.client_share_bps, ev.client_share_bps);
    assert_eq!(stored.freelancer_share_bps, ev.freelancer_share_bps);
    assert_eq!(stored.locked, ev.locked);
}

// ── failure paths: no event emitted, no state change ────────────────────────

/// A non-admin caller is rejected before anything is written, so no `yldlock`
/// event is published and the stored state stays unlocked with the same shares.
#[test]
fn no_event_when_caller_is_not_the_admin() {
    let env = Env::default();
    let (client_addr, freelancer_addr, arbiter_addr, _admin_addr, contract_id, client) =
        escrow_with_yield_shares(&env, 5_000, 5_000);

    for caller in [&client_addr, &freelancer_addr, &arbiter_addr] {
        assert_eq!(
            client.try_lock_escrow_interest_yield(caller),
            Err(Ok(Error::Unauthorized)),
            "only the admin may take the lock"
        );
        assert_eq!(yldlock_event_count(&env), 0);
    }

    assert!(!client.is_escrow_interest_yield_locked());

    let stored = stored_state(&env, &contract_id).expect("configured state is still present");
    assert_eq!(stored.client_share_bps, 5_000);
    assert_eq!(stored.freelancer_share_bps, 5_000);
    assert!(!stored.locked, "rejected lock must not set the flag");
}

/// With no interest/yield state configured the call reverts with
/// `NotInitialized`; nothing is stored and no event is published.
#[test]
fn no_event_when_state_is_uninitialized() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    assert_eq!(
        client.try_lock_escrow_interest_yield(&caller),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(yldlock_event_count(&env), 0);
    assert_eq!(stored_state(&env, &contract_id), None);
}

/// A rejected re-lock after a successful one adds no second event: the ledger
/// keeps exactly the record of the lock that actually took, and the state from
/// that call is untouched.
#[test]
fn no_event_on_failed_relock_by_non_admin() {
    let env = Env::default();
    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, contract_id, client) =
        escrow_with_yield_shares(&env, 5_000, 5_000);

    client.lock_escrow_interest_yield(&admin_addr);
    assert_eq!(yldlock_event_count(&env), 1);
    let ev = last_yldlock_event(&env);
    assert_event_reconciles(&env, &ev, &contract_id, &client);

    let attacker = Address::generate(&env);
    assert_eq!(
        client.try_lock_escrow_interest_yield(&attacker),
        Err(Ok(Error::Unauthorized))
    );

    // A reverted invocation publishes no events, so the failure path
    // contributes no `yldlock` event of its own.
    assert_eq!(yldlock_event_count(&env), 0);

    // The state from the successful lock survives unchanged.
    let stored = stored_state(&env, &contract_id).unwrap();
    assert_eq!(stored.client_share_bps, ev.client_share_bps);
    assert_eq!(stored.freelancer_share_bps, ev.freelancer_share_bps);
    assert_eq!(stored.locked, ev.locked);
    assert!(stored.locked);
}
