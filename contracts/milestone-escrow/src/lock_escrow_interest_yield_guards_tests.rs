#![cfg(test)]
//! Unit-test suite for the authorization and precondition guards on
//! `lock_escrow_interest_yield` (issue #464).
//!
//! `lock_escrow_interest_yield` is the endpoint that freezes the
//! client/freelancer yield-share split: once it takes, `set_escrow_interest_yield`
//! and `set_interest_yield_consent` are both refused with `EscrowLocked` until an
//! admin clears the lock. It must therefore be unreachable except by the stored
//! admin, and it must refuse to act on a source state that is illegal for a
//! lock (a configuration that is already locked) or absent (a configuration that
//! was never written).
//!
//! The guards run in a fixed order, before the function writes its single ledger
//! entry:
//!
//! 1. **Authorization** — a non-admin gets `Unauthorized`.
//! 2. **Illegal source state** — an already-locked configuration gets
//!    `InvalidStatus`; a missing configuration gets `NotInitialized`.
//!
//! ## Verification strategy
//!
//! Every rejection case asserts **two** things, not one:
//!
//! * the call returns its specific typed `Error` (an `Err(Ok(Error::..))` from
//!   `try_lock_escrow_interest_yield`), and
//! * the ledger is unchanged.
//!
//! "Unchanged" is checked with `env.to_ledger_snapshot()`: a full snapshot is
//! taken immediately before the call and again immediately after, and the two
//! are compared for equality. `LedgerSnapshot` captures every ledger entry —
//! instance, persistent, and temporary storage across *all* contracts registered
//! on the `Env` (including the escrow's token contract), each with its
//! live-until ledger sequence / TTL — so this is a whole-ledger equivalence
//! check, not a spot check of the one key we expect to change. Any effect that
//! survives the call on a rejected path — an added `.set(`, a `.remove(`, an
//! `extend_ttl`, a nested sub-invocation, or a stray event — shows up as a
//! snapshot difference and fails these tests.
//!
//! The guarantee these tests actually establish is **no net ledger effect**.
//! Note that a failing top-level invocation is rolled back wholesale by the SDK,
//! so a side effect performed *before* the guard on a rejected path is undone and
//! leaves no residue for a snapshot to catch. That is a property of the rollback,
//! not of this endpoint: the guards are what keep the code honest, and the tests
//! pin down the observable outcome (correct typed error, unchanged ledger) rather
//! than relying on rollback to paper over a badly ordered guard.
//!
//! A spot check of the payload key is kept alongside the snapshot throughout, so
//! a regression reports *which* entry drifted rather than only that something
//! did — in particular that a refused re-lock neither releases nor clears the
//! lock it was refused by.

use super::*;
use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, EscrowInterestYieldState};
// `Address`, `Env`, and the `testutils::{Address as _, Ledger}` traits all
// arrive from `test.rs` via the glob import above; only `EnvTestConfig` needs
// naming explicitly, matching `set_escrow_interest_yield_guards_tests`.
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, Address, Env};

/// The even split every fixture configures before exercising the lock.
const CLIENT_BPS: u32 = 5_000;
const FREELANCER_BPS: u32 = 5_000;

/// `Env` that does not write a `test_snapshots/` file when dropped — this suite
/// compares explicit `to_ledger_snapshot()` values, so the automatic per-test
/// file would be noise in the diff. Same configuration as
/// `set_escrow_interest_yield_guards_tests`.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A funded, fully initialised escrow whose interest/yield share configuration is
/// set to an even split and left unlocked. This is the one source state on which
/// a lock is legal, and the starting point for both the success paths and the
/// already-locked fixture below.
fn escrow_with_unlocked_config(env: &Env) -> (MilestoneEscrowClient<'_>, Address, Address) {
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin, &CLIENT_BPS, &FREELANCER_BPS);
    assert!(
        !client.is_escrow_interest_yield_locked(),
        "fixture must start from an unlocked configuration"
    );
    (client, admin, contract_id)
}

/// The same escrow, but the configuration has already been frozen by a successful
/// `lock_escrow_interest_yield` — the illegal source state this endpoint must
/// refuse.
fn escrow_with_locked_config(env: &Env) -> (MilestoneEscrowClient<'_>, Address, Address) {
    let (client, admin, contract_id) = escrow_with_unlocked_config(env);
    client.lock_escrow_interest_yield(&admin);
    assert!(
        client.is_escrow_interest_yield_locked(),
        "fixture must start from a locked configuration"
    );
    (client, admin, contract_id)
}

/// Direct read of the instance-storage entry `lock_escrow_interest_yield` writes,
/// bypassing the public getter so assertions are against the ledger itself.
fn stored_state(env: &Env, contract_id: &Address) -> Option<EscrowInterestYieldState> {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&DataKey::InterestYieldState)
    })
}

/// Assert the whole ledger is equivalent across a rejected call.
///
/// Generic over the snapshot type because `Env::to_ledger_snapshot` returns
/// `soroban_ledger_snapshot::LedgerSnapshot`, which `soroban_sdk` does not
/// re-export and this crate does not depend on directly.
fn assert_ledger_untouched<T: PartialEq + core::fmt::Debug>(before: &T, after: &T, context: &str) {
    assert_eq!(
        before, after,
        "lock_escrow_interest_yield mutated the ledger on a rejected call: {context}"
    );
}

/// Assert the configuration on the ledger still holds the fixture's shares and
/// carries the `locked` flag the case under test expects.
fn assert_state(env: &Env, contract_id: &Address, expected_locked: bool, context: &str) {
    let state = stored_state(env, contract_id)
        .unwrap_or_else(|| panic!("{context}: the interest/yield configuration must still exist"));
    assert_eq!(
        state.client_share_bps, CLIENT_BPS,
        "{context}: client share"
    );
    assert_eq!(
        state.freelancer_share_bps, FREELANCER_BPS,
        "{context}: freelancer share"
    );
    assert_eq!(state.locked, expected_locked, "{context}: lock flag");
}

// ── Guard 1: authorization ───────────────────────────────────────────────────

/// A caller that is not the stored admin gets `Unauthorized` and changes nothing.
/// The table covers the three parties whose payout the frozen split governs
/// (client, freelancer, arbiter — none of whom hold share-lock authority merely
/// by being party to the escrow) plus an unrelated address.
#[test]
fn unauthorized_caller_returns_unauthorized_and_mutates_no_ledger_entry() {
    let env = test_env();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin, &CLIENT_BPS, &FREELANCER_BPS);

    let outsider = Address::generate(&env);
    for caller in [&client_addr, &freelancer_addr, &arbiter_addr, &outsider] {
        let before = env.to_ledger_snapshot();
        assert_eq!(
            client.try_lock_escrow_interest_yield(caller),
            Err(Ok(Error::Unauthorized)),
            "a non-admin must be rejected with Unauthorized"
        );
        let after = env.to_ledger_snapshot();
        assert_ledger_untouched(&before, &after, "unauthorized caller");
    }

    assert_state(&env, &contract_id, false, "unauthorized caller");
    assert!(
        !client.is_escrow_interest_yield_locked(),
        "a rejected lock must not take the freeze"
    );
}

/// An unauthorized call must not *create* the configuration either. On a contract
/// where no interest/yield state was ever written, a rejected call must not be
/// the thing that adds the entry.
#[test]
fn unauthorized_caller_does_not_create_the_configuration() {
    let env = test_env();
    env.mock_all_auths();
    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        stored_state(&env, &contract_id),
        None,
        "fixture must start with no interest/yield configuration"
    );

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_lock_escrow_interest_yield(&client_addr),
        Err(Ok(Error::Unauthorized))
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "unauthorized caller on a fresh config");

    assert_eq!(
        stored_state(&env, &contract_id),
        None,
        "a rejected call must not create DataKey::InterestYieldState"
    );
}

/// Authorization is the *first* guard, so a non-admin probing a locked
/// configuration learns `Unauthorized`, not `InvalidStatus`. If the order ever
/// inverted, an outsider could distinguish "locked" from "unlocked" and map the
/// execution state of the escrow without being the admin.
#[test]
fn unauthorized_caller_cannot_probe_a_locked_configuration() {
    let env = test_env();
    let (client, _admin, contract_id) = escrow_with_locked_config(&env);
    let outsider = Address::generate(&env);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_lock_escrow_interest_yield(&outsider),
        Err(Ok(Error::Unauthorized)),
        "authorization must be evaluated before the source-state guard"
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "unauthorized probe of a locked config");

    assert_state(
        &env,
        &contract_id,
        true,
        "unauthorized probe of a locked config",
    );
    assert!(client.is_escrow_interest_yield_locked());
}

// ── Guard 2: illegal source state ───────────────────────────────────────────

/// The already-locked case: the admin is correctly authorized, but the
/// configuration is frozen, so re-locking is refused with `InvalidStatus` and
/// the ledger — shares and lock flag alike — is left exactly as it was.
#[test]
fn already_locked_configuration_returns_invalid_status_and_mutates_no_ledger_entry() {
    let env = test_env();
    let (client, admin, contract_id) = escrow_with_locked_config(&env);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_lock_escrow_interest_yield(&admin),
        Err(Ok(Error::InvalidStatus)),
        "a locked configuration must be refused with InvalidStatus"
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "already-locked configuration");

    assert_state(&env, &contract_id, true, "already-locked configuration");
    assert!(client.is_escrow_interest_yield_locked());
}

/// The absent-state case: a contract that is initialised and correctly
/// authorized, but whose interest/yield configuration was never written. `lock`
/// has no create path — it can only freeze shares that already exist — so the
/// missing entry is `NotInitialized` and nothing is written.
#[test]
fn missing_configuration_returns_not_initialized_and_mutates_no_ledger_entry() {
    let env = test_env();
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(stored_state(&env, &contract_id), None);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_lock_escrow_interest_yield(&admin),
        Err(Ok(Error::NotInitialized)),
        "lock must not create the configuration it was asked to freeze"
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "missing configuration");

    assert_eq!(
        stored_state(&env, &contract_id),
        None,
        "a rejected call must not create DataKey::InterestYieldState"
    );
}

/// Repeated attempts against a locked configuration must stay refused and stay
/// side-effect free, so a retry loop cannot erode the freeze.
#[test]
fn repeated_attempts_against_a_locked_configuration_are_all_rejected() {
    let env = test_env();
    let (client, admin, contract_id) = escrow_with_locked_config(&env);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        assert_eq!(
            client.try_lock_escrow_interest_yield(&admin),
            Err(Ok(Error::InvalidStatus))
        );
    }
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "repeated attempts on a locked config");

    assert_state(
        &env,
        &contract_id,
        true,
        "repeated attempts on a locked config",
    );
}

// ── Initialisation guard and event hygiene ───────────────────────────────────

/// Before `initialize` there is no instance `Admin` key, so the authorization
/// guard reports `NotInitialized` and writes nothing.
#[test]
fn uninitialized_contract_returns_not_initialized_and_mutates_no_ledger_entry() {
    let env = test_env();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_lock_escrow_interest_yield(&caller),
        Err(Ok(Error::NotInitialized))
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "uninitialized contract");

    assert_eq!(stored_state(&env, &contract_id), None);
}

/// No rejected path may publish an event. An event is an off-ledger side channel:
/// a caller that could make the contract emit a `yldlock` record on a rejected
/// call would feed an indexer a lock that never happened — and, on a re-lock
/// attempt, a second record for a transition the ledger did not make.
#[test]
fn rejected_calls_publish_no_event() {
    let env = test_env();
    let (client, admin, _) = escrow_with_locked_config(&env);
    let outsider = Address::generate(&env);

    let events_before = crate::all_event_tuples(&env).len();

    assert_eq!(
        client.try_lock_escrow_interest_yield(&outsider),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        client.try_lock_escrow_interest_yield(&admin),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(
        client.try_lock_escrow_interest_yield(&outsider),
        Err(Ok(Error::Unauthorized))
    );

    assert_eq!(
        crate::all_event_tuples(&env).len(),
        events_before,
        "a rejected call must not publish an event"
    );
}

// ── Success paths (so the rejection assertions above are not vacuous) ───────

/// The one legal source state: an authenticated admin with a configured but
/// unlocked configuration locks it, the write is observable on the ledger, and
/// the freeze it installs actually gates the share-mutation paths.
#[test]
fn admin_can_lock_an_unlocked_configuration() {
    let env = test_env();
    let (client, admin, contract_id) = escrow_with_unlocked_config(&env);

    assert_eq!(
        client.try_lock_escrow_interest_yield(&admin),
        Ok(Ok(())),
        "an unlocked configuration must be lockable by the stored admin"
    );

    assert_state(&env, &contract_id, true, "successful lock");
    assert!(client.is_escrow_interest_yield_locked());

    // The lock is not just a flag: it must actually gate both share-mutation
    // paths, which is the whole point of the endpoint.
    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &6_000_u32, &4_000_u32),
        Err(Ok(Error::EscrowLocked))
    );
    assert_eq!(
        client.try_set_interest_yield_consent(&admin, &6_000_u32, &4_000_u32),
        Err(Ok(Error::EscrowLocked))
    );
}

/// A rejected re-lock must not have cleared the lock, so clearing it through the
/// dedicated unlock endpoint and locking again succeeds — the full lifecycle,
/// which also pins down that the refused call did not disturb the frozen shares.
#[test]
fn unlock_then_relock_succeeds_and_preserves_the_shares() {
    let env = test_env();
    let (client, admin, contract_id) = escrow_with_locked_config(&env);

    assert_eq!(
        client.try_lock_escrow_interest_yield(&admin),
        Err(Ok(Error::InvalidStatus))
    );
    assert_state(&env, &contract_id, true, "refused re-lock");

    client.unlock_escrow_interest_yield(&admin);
    assert_state(&env, &contract_id, false, "after unlock");

    assert_eq!(client.try_lock_escrow_interest_yield(&admin), Ok(Ok(())));
    assert_state(&env, &contract_id, true, "re-lock after unlock");
}
