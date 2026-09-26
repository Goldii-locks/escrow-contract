#![cfg(test)]
//! Unit-test suite for the authorization and precondition guards on
//! `set_escrow_interest_yield` (issue #461).
//!
//! `set_escrow_interest_yield` is the endpoint that writes the client/freelancer
//! yield-share split. It is the one place where the platform admin can move the
//! whole interest/yield allocation between the two parties, so it must be
//! unreachable except by the stored admin, and it must be unreachable while the
//! configuration is frozen for a pending execution.
//!
//! The guards run in a fixed order, before the function reads the payload ledger
//! entry or performs its single write:
//!
//! 1. **Authorization** — a non-admin gets `Unauthorized`.
//! 2. **Illegal source state** — a locked configuration gets `EscrowLocked`.
//! 3. **Argument validation** — a ratio that does not sum to `BPS_SCALE` gets
//!    `InvalidRatio`.
//!
//! ## Verification strategy
//!
//! Every rejection case asserts **two** things, not one:
//!
//! * the call returns its specific typed `Error` (an `Err(Ok(Error::..))` from
//!   `try_set_escrow_interest_yield`), and
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
//! A spot check of the payload key is kept alongside the snapshot in the tests
//! where the failure mode is subtle (creating a config that was absent, or
//! leaving a locked config locked), so a regression reports *which* entry drifted
//! rather than only that something did.

use super::*;
use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, EscrowInterestYieldState};
// `Address`, `Env`, and the `testutils::{Address as _, Ledger}` traits all
// arrive from `test.rs` via the glob import above; only `EnvTestConfig` needs
// naming explicitly, matching `interest_yield_split_refund_guards_tests`.
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, Address, Env};

/// `Env` that does not write a `test_snapshots/` file when dropped — this suite
/// compares explicit `to_ledger_snapshot()` values, so the automatic per-test
/// file would be noise in the diff. Same configuration as
/// `interest_yield_split_refund_guards_tests`.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A funded, fully initialised escrow whose interest/yield share configuration is
/// set to an even split and then frozen by `lock_escrow_interest_yield`.
/// Returns the client, the stored admin, and the contract id (so tests can read
/// instance storage directly).
fn escrow_with_locked_config(env: &Env) -> (MilestoneEscrowClient<'_>, Address, Address) {
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin, &5_000_u32, &5_000_u32);
    client.lock_escrow_interest_yield(&admin);
    assert!(
        client.is_escrow_interest_yield_locked(),
        "fixture must start from a locked configuration"
    );
    (client, admin, contract_id)
}

/// Direct read of the instance-storage entry `set_escrow_interest_yield` writes,
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
        "set_escrow_interest_yield mutated the ledger on a rejected call: {context}"
    );
}

// ── Guard 1: authorization ───────────────────────────────────────────────────

/// A caller that is not the stored admin gets `Unauthorized` and changes nothing
/// — both for the client and the freelancer, who are the two parties the
/// yield-share split exists to protect.
#[test]
fn unauthorized_caller_returns_unauthorized_and_mutates_no_ledger_entry() {
    let env = test_env();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.set_escrow_interest_yield(&admin, &5_000_u32, &5_000_u32);

    for outsider in [&client_addr, &freelancer_addr] {
        let before = env.to_ledger_snapshot();
        assert_eq!(
            client.try_set_escrow_interest_yield(outsider, &1_u32, &9_999_u32),
            Err(Ok(Error::Unauthorized)),
            "a non-admin must be rejected with Unauthorized"
        );
        let after = env.to_ledger_snapshot();
        assert_ledger_untouched(&before, &after, "unauthorized caller");
    }

    // The even split the admin configured is still the one on the ledger.
    let state = stored_state(&env, &contract_id).expect("config was set before the attempts");
    assert_eq!(state.client_share_bps, 5_000);
    assert_eq!(state.freelancer_share_bps, 5_000);
}

/// An unauthorized call must not *create* the configuration either. Before any
/// write exists there is no `InterestYieldState` entry, and a rejected call must
/// not be the thing that adds it.
#[test]
fn unauthorized_caller_does_not_create_the_configuration() {
    let env = test_env();
    env.mock_all_auths();
    let (client_addr, _, _, _admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        stored_state(&env, &contract_id),
        None,
        "fixture must start with no interest/yield configuration"
    );

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_set_escrow_interest_yield(&client_addr, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Unauthorized))
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "unauthorized caller on a fresh contract");

    assert_eq!(
        stored_state(&env, &contract_id),
        None,
        "a rejected call must not create DataKey::InterestYieldState"
    );
}

/// Authorization is the *first* guard, so a non-admin probing a locked
/// configuration learns `Unauthorized`, not `EscrowLocked`. If the order ever
/// inverted, an outsider could distinguish "locked" from "unlocked" and map the
/// execution state of the escrow without being the admin.
#[test]
fn unauthorized_caller_cannot_probe_a_locked_configuration() {
    let env = test_env();
    let (client, _admin, contract_id) = escrow_with_locked_config(&env);
    let outsider = Address::generate(&env);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_set_escrow_interest_yield(&outsider, &1_u32, &9_999_u32),
        Err(Ok(Error::Unauthorized)),
        "authorization must be evaluated before the source-state guard"
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "unauthorized probe of a locked config");

    let state = stored_state(&env, &contract_id).expect("fixture wrote a config");
    assert!(
        state.locked,
        "a rejected call must not clear the lock it was refused by"
    );
}

// ── Guard 2: illegal source state (locked configuration) ─────────────────────

/// The frozen-configuration case: the admin is correctly authorized and the
/// ratio is valid, but the configuration is locked for a pending execution, so
/// the call is refused with `EscrowLocked` and the ledger — shares and lock flag
/// alike — is left exactly as it was.
#[test]
fn locked_configuration_returns_escrow_locked_and_mutates_no_ledger_entry() {
    let env = test_env();
    let (client, admin, contract_id) = escrow_with_locked_config(&env);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &1_u32, &9_999_u32),
        Err(Ok(Error::EscrowLocked)),
        "a locked configuration must be refused with EscrowLocked"
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "locked configuration");

    let state = stored_state(&env, &contract_id).expect("lock persisted a config");
    assert_eq!(
        state.client_share_bps, 5_000,
        "the frozen shares must survive a refused overwrite"
    );
    assert_eq!(state.freelancer_share_bps, 5_000);
    assert!(state.locked, "the configuration must stay locked");
    assert!(client.is_escrow_interest_yield_locked());
}

/// The state guard is authoritative over the argument validator: a locked
/// configuration plus a malformed ratio reports `EscrowLocked`, not
/// `InvalidRatio`. This is what stops a caller from using the returned error to
/// distinguish "frozen" from "just needs a better ratio", and it keeps the
/// rejection attributable to the state the caller cannot change rather than to
/// arguments they control.
#[test]
fn locked_configuration_takes_precedence_over_invalid_ratio() {
    let env = test_env();
    let (client, admin, contract_id) = escrow_with_locked_config(&env);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &6_000_u32, &5_000_u32),
        Err(Ok(Error::EscrowLocked)),
        "the source-state guard must be evaluated before argument validation"
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "locked config with an invalid ratio");

    let state = stored_state(&env, &contract_id).expect("lock persisted a config");
    assert!(state.locked);
    assert_eq!(state.client_share_bps, 5_000);
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
            client.try_set_escrow_interest_yield(&admin, &7_000_u32, &3_000_u32),
            Err(Ok(Error::EscrowLocked))
        );
    }
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "repeated attempts on a locked config");

    let state = stored_state(&env, &contract_id).expect("lock persisted a config");
    assert_eq!(state.client_share_bps, 5_000);
    assert!(state.locked);
}

// ── Guard 3: contract initialisation and argument validation ─────────────────

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
        client.try_set_escrow_interest_yield(&caller, &5_000_u32, &5_000_u32),
        Err(Ok(Error::NotInitialized))
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "uninitialized contract");

    assert_eq!(stored_state(&env, &contract_id), None);
}

/// A well-authorized admin on an unlocked (here: absent) configuration still
/// cannot write a ratio that does not sum to `BPS_SCALE`, and the rejection is
/// side-effect free. Covers the under-shoot, the over-shoot, and the
/// `u32` addition overflow that `checked_add` has to catch.
#[test]
fn invalid_ratio_returns_invalid_ratio_and_mutates_no_ledger_entry() {
    let env = test_env();
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for (client_bps, freelancer_bps) in [
        (6_000_u32, 5_000_u32), // 11_000: over-shoot
        (3_000_u32, 3_000_u32), //  6_000: under-shoot
        (0_u32, 0_u32),         //      0: nothing allocated
        (u32::MAX, 1_u32),      //      addition overflows u32
    ] {
        let before = env.to_ledger_snapshot();
        assert_eq!(
            client.try_set_escrow_interest_yield(&admin, &client_bps, &freelancer_bps),
            Err(Ok(Error::InvalidRatio)),
            "shares {client_bps} + {freelancer_bps} must not be accepted"
        );
        let after = env.to_ledger_snapshot();
        assert_ledger_untouched(&before, &after, "invalid ratio");
    }

    assert_eq!(
        stored_state(&env, &contract_id),
        None,
        "no rejected call may have written a configuration"
    );
}

/// Argument validation sits behind both other guards, so a malformed ratio from
/// an unauthorized caller is still reported as `Unauthorized` — the argument is
/// never inspected for a caller that has not cleared authorization.
#[test]
fn invalid_ratio_from_an_unauthorized_caller_is_reported_as_unauthorized() {
    let env = test_env();
    env.mock_all_auths();
    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        client.try_set_escrow_interest_yield(&client_addr, &0_u32, &0_u32),
        Err(Ok(Error::Unauthorized)),
        "an unauthorized caller must not reach argument validation"
    );
    let after = env.to_ledger_snapshot();
    assert_ledger_untouched(&before, &after, "unauthorized caller with an invalid ratio");

    assert_eq!(stored_state(&env, &contract_id), None);
}

/// No rejected path may publish an event. An event is an off-ledger side
/// channel: a caller that could make the contract emit one on a rejected call
/// could feed an indexer a "configuration changed" record that never happened.
#[test]
fn rejected_calls_publish_no_event() {
    let env = test_env();
    let (client, admin, _) = escrow_with_locked_config(&env);
    let outsider = Address::generate(&env);

    let events_before = crate::all_event_tuples(&env).len();

    assert_eq!(
        client.try_set_escrow_interest_yield(&outsider, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &5_000_u32, &5_000_u32),
        Err(Ok(Error::EscrowLocked))
    );
    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &6_000_u32, &5_000_u32),
        Err(Ok(Error::EscrowLocked))
    );
    assert_eq!(
        client.try_set_escrow_interest_yield(&outsider, &1_u32, &9_999_u32),
        Err(Ok(Error::Unauthorized))
    );

    assert_eq!(
        crate::all_event_tuples(&env).len(),
        events_before,
        "a rejected call must not publish an event"
    );
}

// ── Success paths (so the rejection assertions above are not vacuous) ───────

/// The first write happens when no configuration exists yet. An absent state is
/// not an illegal source state, so the admin may create it — this is the case
/// that `ensure_interest_yield_writable` treats as "not locked" and the reason
/// it cannot reuse `ensure_interest_yield_unlocked` (which would answer
/// `NotInitialized`).
#[test]
fn admin_can_create_the_configuration_when_none_exists() {
    let env = test_env();
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(stored_state(&env, &contract_id), None);
    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &6_000_u32, &4_000_u32),
        Ok(Ok(()))
    );

    let state = stored_state(&env, &contract_id).expect("a successful call must write the config");
    assert_eq!(state.client_share_bps, 6_000);
    assert_eq!(state.freelancer_share_bps, 4_000);
    assert!(!state.locked, "a freshly created configuration is unlocked");

    let via_getter = client.get_escrow_interest_yield();
    assert_eq!(via_getter.client_share_bps, 6_000);
    assert_eq!(via_getter.freelancer_share_bps, 4_000);
}

/// An unlocked configuration is replaceable, including the two boundary splits
/// where one party receives everything. Only the sum is constrained, so these
/// must not be caught by the guards.
#[test]
fn admin_can_replace_an_unlocked_configuration() {
    let env = test_env();
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for (client_bps, freelancer_bps) in [
        (5_000_u32, 5_000_u32),
        (0_u32, 10_000_u32),
        (10_000_u32, 0_u32),
    ] {
        assert_eq!(
            client.try_set_escrow_interest_yield(&admin, &client_bps, &freelancer_bps),
            Ok(Ok(())),
            "an unlocked configuration must accept {client_bps} + {freelancer_bps}"
        );
        let state = stored_state(&env, &contract_id).expect("config present");
        assert_eq!(state.client_share_bps, client_bps);
        assert_eq!(state.freelancer_share_bps, freelancer_bps);
        assert!(!state.locked);
    }
}

/// Full lock/unlock lifecycle around the write path: a locked configuration
/// refuses the admin, and clearing the lock through the dedicated unlock
/// endpoint restores it. This also pins down that the refused call did not
/// release the lock on its way out.
#[test]
fn unlock_restores_the_write_path() {
    let env = test_env();
    let (client, admin, contract_id) = escrow_with_locked_config(&env);

    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &7_000_u32, &3_000_u32),
        Err(Ok(Error::EscrowLocked))
    );
    assert!(
        stored_state(&env, &contract_id)
            .expect("config present")
            .locked
    );

    client.unlock_escrow_interest_yield(&admin);
    assert!(!client.is_escrow_interest_yield_locked());

    assert_eq!(
        client.try_set_escrow_interest_yield(&admin, &7_000_u32, &3_000_u32),
        Ok(Ok(()))
    );
    let state = stored_state(&env, &contract_id).expect("config present");
    assert_eq!(state.client_share_bps, 7_000);
    assert_eq!(state.freelancer_share_bps, 3_000);
    assert!(
        !state.locked,
        "a write after unlock leaves the config unlocked"
    );
}
