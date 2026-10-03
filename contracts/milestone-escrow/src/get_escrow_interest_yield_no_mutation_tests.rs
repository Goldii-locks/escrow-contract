#![cfg(test)]
//! Regression suite for issue #488: guarantee that
//! `get_escrow_interest_yield` performs no state mutation.
//!
//! `get_escrow_interest_yield` is the public read path that callers (and
//! off-chain indexers) invoke to inspect the stored interest/yield share
//! configuration: the client share, the freelancer share, and the lock flag.
//! Nothing pinned that it is actually read-only, so a future edit could let a
//! "read" silently `.set`, `.remove`, or `.extend_ttl` an entry, or publish an
//! event, without any test noticing.
//!
//! The validation strategy: take a full ledger snapshot
//! (`Env::to_ledger_snapshot`) immediately before calling the function under
//! test, take another immediately after, and assert the two are identical.
//! `LedgerSnapshot` captures every ledger entry (instance, persistent, and
//! temporary storage across all contracts registered on the `Env`, including
//! their live-until ledger sequence / TTL) plus the ledger info itself, so this
//! is a byte-for-byte equivalent check of the entire ledger state, not just the
//! one key we expect to be read.
//!
//! Because the issue names all three durability tiers explicitly, the suite
//! also fingerprints each tier on its own (`env.as_contract(..)` plus the
//! testutils `all()` accessors), so a write that somehow evaded the aggregate
//! snapshot is still caught, and a failure message names the tier that moved.
//!
//! Any future edit that adds a `.set(`, `.remove(`, `.extend_ttl(`, or event
//! publish inside `get_escrow_interest_yield` (or the shared
//! `load_interest_yield_state` helper it calls) changes the snapshot and fails
//! this suite.

use crate::test::setup_funded_escrow;
use crate::{Error, EscrowInterestYieldState, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::storage::{Instance, Persistent, Temporary};
use soroban_sdk::{vec, Address, Env, Map, Val};

/// A funded escrow carrying an explicit interest/yield share configuration,
/// plus the admin that configured it and the escrow's contract address (the
/// per-tier fingerprint reads the tier of the currently executing contract, so
/// it needs that id).
fn escrow_with_interest_yield(
    env: &Env,
    client_share_bps: u32,
    freelancer_share_bps: u32,
) -> (MilestoneEscrowClient<'_>, Address, Address) {
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, escrow) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    escrow.set_escrow_interest_yield(&admin, &client_share_bps, &freelancer_share_bps);
    (escrow, admin, contract_id)
}

/// The configuration the write path was asked to store, so each test pins what
/// the read reported back and a "no mutation" result can never be vacuous
/// because the call short-circuited.
fn expected_state(
    client_share_bps: u32,
    freelancer_share_bps: u32,
    locked: bool,
) -> EscrowInterestYieldState {
    EscrowInterestYieldState {
        client_share_bps,
        freelancer_share_bps,
        locked,
    }
}

/// Take a whole-ledger snapshot, call `get_escrow_interest_yield`, assert it
/// published no event and left the ledger byte-for-byte unchanged, then return
/// the state it reported.
fn read_only(env: &Env, escrow: &MilestoneEscrowClient<'_>) -> EscrowInterestYieldState {
    let before = env.to_ledger_snapshot();
    let state = escrow.get_escrow_interest_yield();
    // The test env only keeps events from the latest invocation, so this is
    // exactly what `get_escrow_interest_yield` published.
    assert_eq!(
        crate::all_event_tuples(env).len(),
        0,
        "get_escrow_interest_yield publishes no event"
    );
    let after = env.to_ledger_snapshot();
    assert_eq!(
        before, after,
        "get_escrow_interest_yield mutated the ledger"
    );
    state
}

/// A per-tier fingerprint of the escrow's storage: instance, persistent, then
/// temporary.
///
/// `all()` reads the tier of the *currently executing contract*, so the
/// fingerprint has to be taken inside `env.as_contract(&contract_id, ..)` or it
/// would describe the wrong contract.
type TierFingerprint = (Map<Val, Val>, Map<Val, Val>, Map<Val, Val>);

fn storage_fingerprint(env: &Env, contract_id: &Address) -> TierFingerprint {
    env.as_contract(contract_id, || {
        (
            env.storage().instance().all(),
            env.storage().persistent().all(),
            env.storage().temporary().all(),
        )
    })
}

// ── 1 ─ the headline requirement from the issue ──────────────────────────────

/// The acceptance check named by the issue: a full ledger snapshot taken
/// before and after `get_escrow_interest_yield` is byte-identical, while the
/// call still reports the configured shares.
#[test]
fn get_escrow_interest_yield_does_not_mutate_ledger() {
    let env = Env::default();
    let (escrow, _admin, _contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);

    let state = read_only(&env, &escrow);

    assert_eq!(state, expected_state(6_000, 4_000, false));
}

// ── 2 ─ the three durability tiers, checked individually ─────────────────────

/// The issue names instance, persistent and temporary storage separately, so
/// fingerprint each tier on its own and report which one moved.
#[test]
fn get_escrow_interest_yield_writes_to_no_durability_tier() {
    let env = Env::default();
    let (escrow, _admin, contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);

    let before = storage_fingerprint(&env, &contract_id);
    let state = escrow.get_escrow_interest_yield();
    let after = storage_fingerprint(&env, &contract_id);

    assert_eq!(state, expected_state(6_000, 4_000, false));
    assert_eq!(
        before.0, after.0,
        "get_escrow_interest_yield wrote to instance storage"
    );
    assert_eq!(
        before.1, after.1,
        "get_escrow_interest_yield wrote to persistent storage"
    );
    assert_eq!(
        before.2, after.2,
        "get_escrow_interest_yield wrote to temporary storage"
    );
}

// ── 3 ─ the locked branch ────────────────────────────────────────────────────

/// The guarantee must hold once the configuration is frozen, which exercises a
/// different stored payload (`locked: true`) through the same read path.
#[test]
fn get_escrow_interest_yield_does_not_mutate_ledger_when_locked() {
    let env = Env::default();
    let (escrow, admin, _contract_id) = escrow_with_interest_yield(&env, 5_000, 5_000);
    escrow.lock_escrow_interest_yield(&admin);

    let state = read_only(&env, &escrow);

    assert_eq!(state, expected_state(5_000, 5_000, true));
}

// ── 4 ─ every storable share shape ───────────────────────────────────────────

/// The guarantee must not depend on which shares are stored.
///
/// `set_escrow_interest_yield` only requires the two shares to sum to
/// `BPS_SCALE` (10_000), so all of these are configurations the read path can
/// be handed — including the degenerate all-to-one-party shapes.
#[test]
fn ledger_unchanged_for_every_share_shape() {
    let shapes: [(u32, u32); 5] = [
        (0, 10_000),
        (10_000, 0),
        (1, 9_999),
        (5_000, 5_000),
        (9_999, 1),
    ];

    for (client_bps, freelancer_bps) in shapes {
        let env = Env::default();
        let (escrow, _admin, _contract_id) =
            escrow_with_interest_yield(&env, client_bps, freelancer_bps);

        let state = read_only(&env, &escrow);

        assert_eq!(
            state,
            expected_state(client_bps, freelancer_bps, false),
            "share shape ({client_bps}, {freelancer_bps}) must read back unchanged"
        );
    }
}

// ── 5 ─ idempotence across repeated reads ────────────────────────────────────

/// Repeated calls must be idempotent from the ledger's point of view: many
/// reads in a row must produce the same snapshot as a single read.
#[test]
fn get_escrow_interest_yield_does_not_mutate_ledger_across_repeated_calls() {
    let env = Env::default();
    let (escrow, _admin, _contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        escrow.get_escrow_interest_yield();
    }
    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "repeated calls to get_escrow_interest_yield must not accumulate any ledger mutation"
    );
}

// ── 6 ─ no TTL extension ─────────────────────────────────────────────────────

/// A read must not bump the TTL of any entry it touches (`set`, `remove`, and
/// `extend_ttl` all change the snapshot; this isolates the `extend_ttl` case
/// explicitly).
///
/// The ledger is advanced *before* the "before" snapshot, so an `extend_ttl`
/// inside the read would move the entry's live-until sequence past the value
/// captured before the call.
#[test]
fn get_escrow_interest_yield_does_not_extend_ttl() {
    use soroban_sdk::testutils::Ledger as _;

    let env = Env::default();
    let (escrow, _admin, _contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);
    env.ledger().with_mut(|li| li.sequence_number += 10);

    let before = env.to_ledger_snapshot();
    escrow.get_escrow_interest_yield();
    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "get_escrow_interest_yield must not extend any TTL"
    );
}

// ── 7 ─ the shared helper stays pure through both read entry points ──────────

/// `get_escrow_interest_yield` and `is_escrow_interest_yield_locked` both
/// delegate to the same `load_interest_yield_state` helper, so both read
/// entry points must be pure with respect to the ledger.
#[test]
fn both_interest_yield_read_paths_leave_the_ledger_byte_identical() {
    let env = Env::default();
    let (escrow, _admin, _contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);

    let before = env.to_ledger_snapshot();
    let state = escrow.get_escrow_interest_yield();
    let locked = escrow.is_escrow_interest_yield_locked();
    let after = env.to_ledger_snapshot();

    assert_eq!(state, expected_state(6_000, 4_000, false));
    assert!(!locked);
    assert_eq!(
        before, after,
        "the shared load_interest_yield_state helper must stay read-only"
    );
}

// ── 8 ─ no pause gate ────────────────────────────────────────────────────────

/// The getter performs no pause check (documented in its `# Read-only`
/// section), so it must both succeed and leave the ledger alone while the
/// escrow is frozen.
#[test]
fn get_escrow_interest_yield_stays_read_only_while_paused() {
    let env = Env::default();
    let (escrow, admin, _contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);
    escrow.admin_pause_escrow(&admin);

    let state = read_only(&env, &escrow);

    assert_eq!(state, expected_state(6_000, 4_000, false));
}

// ── 9 ─ no authorization is required ─────────────────────────────────────────

/// The getter takes no `Address`, so it must succeed with every auth mock
/// cleared (an unsigned, read-only query) and still mutate nothing.
#[test]
fn get_escrow_interest_yield_needs_no_authorization_and_mutates_nothing() {
    let env = Env::default();
    let (escrow, _admin, _contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);
    // `set_auths(&[])` clears all mocks without installing any new entries.
    env.set_auths(&[]);

    let state = read_only(&env, &escrow);

    assert_eq!(state, expected_state(6_000, 4_000, false));
}

// ── 10 ─ the rejected paths ──────────────────────────────────────────────────

/// An initialized escrow that has never had a configuration written fails with
/// `NotInitialized` and leaves the ledger untouched. This is a different state
/// from the uninitialized case below: the instance `Admin` entry exists and
/// only `DataKey::InterestYieldState` is missing.
#[test]
fn get_escrow_interest_yield_without_configuration_returns_not_initialized_without_mutation() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = env.to_ledger_snapshot();
    let result = escrow.try_get_escrow_interest_yield();
    let after = env.to_ledger_snapshot();

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
    assert_eq!(
        before, after,
        "a rejected get_escrow_interest_yield mutated the ledger"
    );
}

/// A contract that was never initialized also fails with `NotInitialized`, and
/// that rejection is read-only too.
#[test]
fn get_escrow_interest_yield_on_uninitialized_contract_returns_not_initialized_without_mutation() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    let result = escrow.try_get_escrow_interest_yield();
    let after = env.to_ledger_snapshot();

    assert!(
        matches!(result, Err(Ok(Error::NotInitialized))),
        "an uninitialized get_escrow_interest_yield must return NotInitialized"
    );
    assert_eq!(
        before, after,
        "a rejected get_escrow_interest_yield mutated the ledger"
    );
}

// ── 11 ─ the stored payload is never rewritten ───────────────────────────────

/// Belt and braces: the getter must report the same configuration after a
/// lock/unlock cycle, i.e. the reads never rewrote the stored payload.
#[test]
fn get_escrow_interest_yield_never_rewrites_the_stored_configuration() {
    let env = Env::default();
    let (escrow, admin, _contract_id) = escrow_with_interest_yield(&env, 6_000, 4_000);

    let before = escrow.get_escrow_interest_yield();
    escrow.lock_escrow_interest_yield(&admin);
    escrow.unlock_escrow_interest_yield(&admin);
    let after = escrow.get_escrow_interest_yield();

    assert_eq!(
        before, after,
        "reads must not rewrite the stored interest/yield configuration"
    );
    assert_eq!(after, expected_state(6_000, 4_000, false));
}
