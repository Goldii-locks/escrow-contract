#![cfg(test)]
//! Regression suite for issue #486: guarantee that
//! `get_yield_info` performs no state mutation.
//!
//! `get_yield_info` is a public read path that callers (and off-chain
//! indexers) invoke to inspect the currently configured yield rate, the
//! running accrued total, and the pause flag. It must never write to instance,
//! persistent, or temporary storage, and must never emit events — a regression
//! here would let a "read" silently rewrite ledger state or affect contract
//! entry TTLs.
//!
//! The validation strategy: take a full ledger snapshot
//! (`Env::to_ledger_snapshot`) immediately before calling the function under
//! test, take another immediately after, and assert the two snapshots are
//! identical. `LedgerSnapshot` captures every ledger entry (instance,
//! persistent, and temporary storage across all contracts registered on the
//! `Env`, including their live-until ledger sequence / TTL) plus the ledger
//! info itself, so this is a byte-for-byte equivalent check of the entire
//! ledger state, not just the keys we expect to be read. Any future edit that
//! adds a `.set(`, `.remove(`, `.extend_ttl(`, or event publish inside
//! `get_yield_info` (or the shared `load_job_meta` helper it calls) will change
//! the snapshot and fail these tests.

use crate::test::setup_funded_escrow;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{vec, Env};

/// Take a whole-ledger snapshot, call `get_yield_info`, assert it published no
/// event and left the ledger byte-for-byte unchanged, then return the value it
/// reported so each caller can pin the read itself (guarding against a test
/// that passes vacuously because the call short-circuited).
fn read_only(env: &Env, escrow: &MilestoneEscrowClient<'_>) -> (u32, i128, bool) {
    let before = env.to_ledger_snapshot();
    let info = escrow.get_yield_info();
    // The test env only keeps events from the latest invocation, so this is
    // exactly what `get_yield_info` published.
    assert_eq!(
        crate::all_event_tuples(env).len(),
        0,
        "get_yield_info publishes no event"
    );
    let after = env.to_ledger_snapshot();
    assert_eq!(before, after, "get_yield_info mutated the ledger");
    info
}

/// On a freshly funded escrow, before any admin has configured a rate or
/// accrued any yield, the read reports the documented defaults and mutates
/// nothing.
#[test]
fn get_yield_info_returns_defaults_without_mutation() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let (rate_bps, total_accrued, is_paused) = read_only(&env, &escrow);

    assert_eq!(rate_bps, 0);
    assert_eq!(total_accrued, 0);
    assert!(!is_paused);
}

/// Reading the rate after `admin_set_yield_rate` must return the stored value
/// without touching the ledger.
#[test]
fn get_yield_info_does_not_mutate_ledger_after_yield_rate_set() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.admin_set_yield_rate(&admin, &500_u32);

    let (rate_bps, total_accrued, is_paused) = read_only(&env, &escrow);

    assert_eq!(rate_bps, 500);
    assert_eq!(total_accrued, 0);
    assert!(!is_paused);
}

/// Reading the accrued total after `admin_accrue_yield` must return the stored
/// value without touching the ledger.
#[test]
fn get_yield_info_does_not_mutate_ledger_after_accrual() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.admin_accrue_yield(&admin, &0_u32, &250_i128);

    let (rate_bps, total_accrued, is_paused) = read_only(&env, &escrow);

    assert_eq!(rate_bps, 0);
    assert_eq!(total_accrued, 250);
    assert!(!is_paused);
}

/// The read path also stays read-only once the escrow has been paused, which
/// exercises the instance-tier `DataKey::Paused` branch.
#[test]
fn get_yield_info_does_not_mutate_ledger_when_paused() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.admin_pause_escrow(&admin);

    let (rate_bps, total_accrued, is_paused) = read_only(&env, &escrow);

    assert_eq!(rate_bps, 0);
    assert_eq!(total_accrued, 0);
    assert!(is_paused);
}

/// All three stored branches populated at once — persistent `YieldConfig`,
/// persistent `YieldAccrued`, and instance `Paused` — must still be a pure
/// read.
#[test]
fn get_yield_info_reports_full_state_without_mutation() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.admin_set_yield_rate(&admin, &750_u32);
    escrow.admin_accrue_yield(&admin, &0_u32, &325_i128);
    escrow.admin_pause_escrow(&admin);

    let (rate_bps, total_accrued, is_paused) = read_only(&env, &escrow);

    assert_eq!(rate_bps, 750);
    assert_eq!(total_accrued, 325);
    assert!(is_paused);
}

/// Repeated calls must be idempotent from the ledger's point of view: many
/// reads in a row must produce the same snapshot as a single read.
#[test]
fn get_yield_info_does_not_mutate_ledger_across_repeated_calls() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.admin_set_yield_rate(&admin, &1_200_u32);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        escrow.get_yield_info();
    }
    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "repeated calls to get_yield_info must not accumulate any ledger mutation"
    );
}

/// A read must not bump the TTL of any entry it touches (`set`, `remove`, and
/// `extend_ttl` all change the snapshot; this isolates the `extend_ttl` case
/// explicitly).
#[test]
fn get_yield_info_does_not_extend_ttl() {
    use soroban_sdk::testutils::Ledger as _;

    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.admin_set_yield_rate(&admin, &300_u32);
    env.ledger().with_mut(|li| li.sequence_number += 10);

    let before = env.to_ledger_snapshot();
    escrow.get_yield_info();
    let after = env.to_ledger_snapshot();

    assert_eq!(before, after, "get_yield_info must not extend any TTL");
}

/// The error path is read-only too: an uninitialized contract fails with
/// `NotInitialized` and leaves the ledger untouched.
#[test]
fn get_yield_info_on_uninitialized_contract_returns_not_initialized_without_mutation() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    let result = escrow.try_get_yield_info();
    let after = env.to_ledger_snapshot();

    assert!(
        matches!(result, Err(Ok(Error::NotInitialized))),
        "uninitialized get_yield_info must return NotInitialized"
    );
    assert_eq!(
        before, after,
        "a rejected get_yield_info mutated the ledger"
    );
}
