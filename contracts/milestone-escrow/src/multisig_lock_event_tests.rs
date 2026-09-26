//! Dedicated test suite for the `mslock` structured event emitted by
//! `multisig_lock` (issue #459).
//!
//! `multisig_lock` is the point at which the multisig approval workflow freezes:
//! the flag it sets is the precondition for the two admin overrides
//! (`multisig_admin_override_release` / `multisig_admin_override_refund`) and
//! for the `multisig_split_refund` calculation, and each of those endpoints
//! clears it again.  The call itself left no ledger trace, so once a lock had
//! been released the *only* difference between "never locked" and "locked,
//! then released" was the current value of a single boolean.  It now publishes
//! a typed event carrying the acting address and the resulting flag.
//!
//! Every success-path test asserts that the emitted `MultisigLockedEvent`
//! fields reconcile exactly with the state the call persisted under
//! `DataKey::MultisigLocked` — checked both through the public
//! `is_multisig_locked` accessor and through a direct instance-storage read, so
//! the assertion cannot pass on a getter that disagrees with the ledger.  Every
//! failure-path test asserts that no `mslock` event is published and that the
//! persisted flag is untouched.
//!
//! The event buffer exposed by the test `Env` reflects the most recent
//! top-level invocation only, so each test tallies events immediately after the
//! call it is asserting on (and re-reads the durable state whenever a later
//! call would otherwise clear the buffer).

use super::*;
use crate::{DataKey, Error, MultisigLockedEvent};
use soroban_sdk::{
    symbol_short, token, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val,
};

const MSLOCK_TOPIC: &str = "mslock";

fn mslock_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("mslock").into_val(env);
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

/// Tally of the release override's own topic, used to prove the two lock
/// transitions are never conflated under a single topic.
fn msadmrel_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("msadmrel").into_val(env);
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

/// Tally of the refund override's own topic.
fn msadmref_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("msadmref").into_val(env);
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

/// The most recently published event, asserting it carries the `mslock` topic
/// so a test can never accidentally read a neighbouring event's payload.
fn last_mslock_event(env: &Env) -> MultisigLockedEvent {
    let events = crate::all_event_tuples(env);
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, MSLOCK_TOPIC));
    MultisigLockedEvent::from_val(env, &last.2)
}

/// Direct read of the instance-storage entry the call wrote, bypassing the
/// public accessor so the reconciliation assertion is against the ledger.
fn stored_lock_flag(env: &Env, contract_id: &Address) -> Option<bool> {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&DataKey::MultisigLocked)
    })
}

/// Assert the event describes exactly the flag now on the ledger, and that the
/// public accessor agrees with both the event and the raw storage read.
fn assert_event_reconciles(
    env: &Env,
    ev: &MultisigLockedEvent,
    contract_id: &Address,
    client: &MilestoneEscrowClient,
) {
    let stored = stored_lock_flag(env, contract_id).expect("multisig_lock persisted a flag");
    assert_eq!(ev.locked, stored, "event must report the persisted flag");
    assert!(stored, "the flag persisted by multisig_lock must be true");

    // The acting address must be the admin recorded on the ledger.
    let stored_admin: Address = env
        .as_contract(contract_id, || {
            env.storage().instance().get(&DataKey::Admin)
        })
        .expect("initialize stored an admin");
    assert_eq!(ev.admin, stored_admin, "event must name the acting admin");

    let via_getter = client.is_multisig_locked();
    assert_eq!(ev.locked, via_getter, "accessor must agree with the event");
    assert!(via_getter);
}

// ── success path: field-by-field reconciliation ─────────────────────────────

/// The canonical lock. Every field of the event must equal the state the call
/// persisted, that state must be locked, and the event must be the last thing
/// on the ledger — nothing fallible happens after it is published.
#[test]
fn event_reconciles_with_persisted_lock_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    assert!(!client.is_multisig_locked());
    assert_eq!(
        stored_lock_flag(&env, &contract_id),
        None,
        "MultisigLocked must be absent before the call"
    );

    client.multisig_lock(&admin_addr);

    // Exactly one structured event.
    assert_eq!(mslock_event_count(&env), 1);
    let ev = last_mslock_event(&env);

    // Acting address.
    assert_eq!(ev.admin, admin_addr);

    // Resulting value, and its reconciliation with the persisted state.
    assert!(ev.locked, "the event must report the lock as taken");
    assert_event_reconciles(&env, &ev, &contract_id, &client);
}

/// The flag the event reports is the flag the contract enforces: every
/// endpoint gated on it moves from rejecting the state to accepting it across
/// the lock, so an indexer replaying `mslock` sees the same behaviour the
/// contract itself allows.
#[test]
fn lock_is_enforced_after_the_event_is_published() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, freelancer_addr, _arbiter_addr, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token = token::Client::new(&env, &token_id);

    // Unlocked: both multisig endpoints reject the state, and neither
    // rejection publishes the lock event.
    assert_eq!(
        client.try_multisig_split_refund(&admin_addr, &1_000, &5_000, &5_000),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(mslock_event_count(&env), 0);
    assert_eq!(
        client.try_multisig_admin_override_release(&admin_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(mslock_event_count(&env), 0);
    assert!(!client.is_multisig_locked());

    client.multisig_lock(&admin_addr);
    assert_eq!(mslock_event_count(&env), 1);
    let ev = last_mslock_event(&env);
    assert!(ev.locked, "event must report the lock as taken");
    assert_event_reconciles(&env, &ev, &contract_id, &client);

    // Locked: the same calls now succeed, which is the state the event
    // reported. The calculation helper is a pure read of the flag.
    let allocation = client.multisig_split_refund(&admin_addr, &1_000, &4_000, &6_000);
    assert_eq!(allocation.client_refund, 400);
    assert_eq!(allocation.freelancer_payout, 600);
    assert!(
        client.is_multisig_locked(),
        "multisig_split_refund must not clear the lock"
    );

    // The release override is unblocked too, and it settles the milestone.
    client.multisig_admin_override_release(&admin_addr, &0u32);
    assert_eq!(token.balance(&freelancer_addr), 1_000);
    let milestone = client.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Released);
    assert_eq!(milestone.released_amount, 1_000);
}

/// The call is idempotent, and every successful invocation is a lock *attempt*
/// worth recording: a second call publishes its own event with the same
/// payload, so a replay of `mslock` sees one record per call rather than a
/// single de-duplicated entry.
#[test]
fn each_lock_call_publishes_exactly_one_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.multisig_lock(&admin_addr);
    assert_eq!(mslock_event_count(&env), 1);
    let first = last_mslock_event(&env);
    assert_eq!(first.admin, admin_addr);
    assert!(first.locked);
    assert_event_reconciles(&env, &first, &contract_id, &client);

    // Locking an already-locked contract is a no-op for the flag, but it is
    // still a call that took the lock, so it publishes exactly one event.
    client.multisig_lock(&admin_addr);
    assert_eq!(
        mslock_event_count(&env),
        1,
        "a successful call publishes exactly one mslock event"
    );
    let second = last_mslock_event(&env);
    assert_eq!(second.admin, first.admin);
    assert_eq!(second.locked, first.locked);
    assert!(second.locked);
    assert_event_reconciles(&env, &second, &contract_id, &client);
}

/// Clearing the lock is a different transition and must never borrow the lock
/// topic: the override records itself under `msadmrel` and publishes no
/// `mslock`, so a consumer watching only `mslock` cannot mistake a release for
/// a lock.  The lock remains on the record even though the flag is gone.
#[test]
fn release_override_clears_the_lock_without_publishing_mslock() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token_id = client.get_job().token;
    let token = token::Client::new(&env, &token_id);

    client.multisig_lock(&admin_addr);
    assert_eq!(mslock_event_count(&env), 1);
    let ev = last_mslock_event(&env);
    assert_event_reconciles(&env, &ev, &contract_id, &client);

    client.multisig_admin_override_release(&admin_addr, &0u32);

    // The clearing invocation publishes its own topic and no `mslock`.
    assert_eq!(msadmrel_event_count(&env), 1);
    assert_eq!(
        mslock_event_count(&env),
        0,
        "clearing the lock must not publish a mslock event"
    );

    // The flag really is cleared, so the `mslock` record captured above is
    // the only durable evidence that a lock was ever taken.
    assert_eq!(stored_lock_flag(&env, &contract_id), Some(false));
    assert!(!client.is_multisig_locked());
    assert_eq!(token.balance(&freelancer_addr), 1_000);

    // Re-locking after the release is a fresh, observable transition.
    client.multisig_lock(&admin_addr);
    assert_eq!(mslock_event_count(&env), 1);
    let relock = last_mslock_event(&env);
    assert_eq!(relock.admin, admin_addr);
    assert!(relock.locked);
    assert_event_reconciles(&env, &relock, &contract_id, &client);
}

/// The refund override *removes* the flag key rather than writing `false`, so
/// it is the strongest case that the event is needed: once it returns, the
/// ledger holds no trace of the lock at all and only `mslock` does.
#[test]
fn refund_override_removing_the_flag_still_records_its_own_topic() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _freelancer_addr, _arbiter_addr, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token = token::Client::new(&env, &token_id);
    let client_balance_before = token.balance(&client_addr);

    client.multisig_lock(&admin_addr);
    assert_eq!(mslock_event_count(&env), 1);
    let ev = last_mslock_event(&env);
    assert_event_reconciles(&env, &ev, &contract_id, &client);

    client.multisig_admin_override_refund(&admin_addr, &0u32);

    assert_eq!(msadmref_event_count(&env), 1);
    assert_eq!(
        mslock_event_count(&env),
        0,
        "clearing the lock must not publish a mslock event"
    );

    // The key is gone entirely — the public accessor falls back to `false`.
    assert_eq!(
        stored_lock_flag(&env, &contract_id),
        None,
        "the refund override removes the flag key"
    );
    assert!(!client.is_multisig_locked());
    assert_eq!(token.balance(&client_addr), client_balance_before + 1_000);
}

/// Publishing the event must not cost a ledger entry: the call still touches
/// exactly the one instance entry it did before, leaving persistent storage
/// (including `Admin`) untouched.
#[test]
fn event_publish_adds_no_ledger_entry() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let persistent_admin_before: Option<Address> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin)
    });

    client.multisig_lock(&admin_addr);

    // Tally first: later `client.*` calls would clear the buffer.
    assert_eq!(mslock_event_count(&env), 1);

    // Events are contract logs, not ledger entries, so the only write the call
    // performed is still the instance flag.
    assert_eq!(stored_lock_flag(&env, &contract_id), Some(true));
    let persistent_locked: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::MultisigLocked)
    });
    assert_eq!(
        persistent_locked, None,
        "multisig_lock must not write MultisigLocked to persistent storage"
    );
    let persistent_admin_after: Option<Address> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin)
    });
    assert_eq!(
        persistent_admin_before, persistent_admin_after,
        "multisig_lock must not alter persistent::Admin"
    );
}

// ── failure paths: no event emitted, no state change ────────────────────────

/// A non-admin caller is rejected before anything is written, so no `mslock`
/// event is published and the flag stays absent from instance storage.
#[test]
fn no_event_when_caller_is_not_the_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let stranger = Address::generate(&env);

    for caller in [&client_addr, &freelancer_addr, &arbiter_addr, &stranger] {
        assert_eq!(
            client.try_multisig_lock(caller),
            Err(Ok(Error::Unauthorized)),
            "only the admin may take the lock"
        );
        assert_eq!(mslock_event_count(&env), 0);
        assert_eq!(
            stored_lock_flag(&env, &contract_id),
            None,
            "a rejected lock must not write the flag"
        );
        assert!(!client.is_multisig_locked());
    }
}

/// `require_auth()` is the outermost guard, so a call carrying no admin
/// signature is rejected by the host before any contract logic runs.  The
/// event is therefore only ever published by a fully authorised call, which
/// is what makes the acting address in the payload auditable.
#[test]
fn no_event_when_the_admin_signature_is_missing() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Clear every mocked auth entry so the host enforces real authorization.
    env.set_auths(&[]);

    let result = client.try_multisig_lock(&admin_addr);
    assert!(
        matches!(result, Err(Err(_))),
        "a call without the admin signature must fail at the host level"
    );
    assert_eq!(mslock_event_count(&env), 0);
    assert_eq!(stored_lock_flag(&env, &contract_id), None);
    assert!(!client.is_multisig_locked());
}

/// With no admin ever stored the call reverts with `NotInitialized`; nothing is
/// written and no event is published.
#[test]
fn no_event_when_contract_is_uninitialized() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let admin = Address::generate(&env);

    assert_eq!(
        client.try_multisig_lock(&admin),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(mslock_event_count(&env), 0);
    assert_eq!(stored_lock_flag(&env, &contract_id), None);
}

/// A rejected re-lock after a successful one adds no second event: the ledger
/// keeps exactly the record of the lock that actually took, and the state from
/// that call is untouched.
#[test]
fn no_event_on_failed_relock_by_non_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.multisig_lock(&admin_addr);
    assert_eq!(mslock_event_count(&env), 1);
    let ev = last_mslock_event(&env);
    assert_event_reconciles(&env, &ev, &contract_id, &client);

    let attacker = Address::generate(&env);
    assert_eq!(
        client.try_multisig_lock(&attacker),
        Err(Ok(Error::Unauthorized))
    );

    // A reverted invocation publishes no events, so the failure path
    // contributes no `mslock` event of its own.
    assert_eq!(mslock_event_count(&env), 0);

    // The state from the successful lock survives unchanged.
    assert_event_reconciles(&env, &ev, &contract_id, &client);
}
