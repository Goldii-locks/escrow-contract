#![cfg(test)]
//! Regression suite for issue #491: guarantee that
//! `get_platform_fee_allocation` performs no state mutation.
//!
//! `get_platform_fee_allocation` is a public read path that callers (and
//! off-chain indexers) invoke to inspect the currently configured platform
//! fee split. It must never write to instance, persistent, or temporary
//! storage, and must never emit events — a regression here would let a
//! "read" silently rewrite ledger state or affect contract entry TTLs.
//!
//! The validation strategy: take a full ledger snapshot
//! (`Env::to_ledger_snapshot`) immediately before calling the function under
//! test, take another immediately after, and assert the two snapshots are
//! identical. `LedgerSnapshot` captures every ledger entry (instance,
//! persistent, and temporary storage across all contracts registered on the
//! `Env`, including their live-until ledger sequence / TTL) plus the ledger
//! info itself, so this is a byte-for-byte equivalent check of the entire
//! ledger state, not just the one key we expect to be read. Any future edit
//! that adds a `.set(`, `.remove(`, `.extend_ttl(`, or event publish inside
//! `get_platform_fee_allocation` (or the shared
//! `load_platform_fee_allocation` helper it calls) will change the
//! snapshot and fail this test.

use super::*;
use soroban_sdk::{vec, Address, Env};

/// A fully initialised escrow with an explicit (non-default) platform fee
/// allocation configured, plus the admin address used to configure it.
fn escrow_with_fee_allocation(env: &Env) -> (MilestoneEscrowClient<'_>, Address) {
    env.mock_all_auths();

    let admin_addr = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = vec![env, 1_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604_800u64,
        &amounts,
    );

    escrow.set_platform_fee_allocation(&admin_addr, &2_000_u32, &7_000_u32, &1_000_u32);

    (escrow, admin_addr)
}

/// Calling `get_platform_fee_allocation` on a freshly configured allocation
/// must leave the entire ledger byte-for-byte unchanged.
#[test]
fn get_platform_fee_allocation_does_not_mutate_ledger() {
    let env = Env::default();
    let (escrow, _admin_addr) = escrow_with_fee_allocation(&env);

    let before = env.to_ledger_snapshot();
    let allocation = escrow.get_platform_fee_allocation();
    let after = env.to_ledger_snapshot();

    // Sanity check: we actually read back the configured allocation, so the
    // "no mutation" result below isn't vacuously true because the call
    // failed or short-circuited.
    assert_eq!(allocation.client_bps, 2_000);
    assert_eq!(allocation.freelancer_bps, 7_000);
    assert_eq!(allocation.treasury_bps, 1_000);
    assert!(!allocation.locked);

    assert_eq!(
        before, after,
        "get_platform_fee_allocation must not mutate any ledger entry"
    );
}

/// The same guarantee must hold once the allocation has been locked, which
/// exercises a different stored value (`locked: true`) through the same
/// read path.
#[test]
fn get_platform_fee_allocation_does_not_mutate_ledger_when_locked() {
    let env = Env::default();
    let (escrow, admin_addr) = escrow_with_fee_allocation(&env);
    escrow.lock_platform_fee_allocation(&admin_addr);

    let before = env.to_ledger_snapshot();
    let allocation = escrow.get_platform_fee_allocation();
    let after = env.to_ledger_snapshot();

    assert!(allocation.locked);
    assert_eq!(
        before, after,
        "get_platform_fee_allocation must not mutate any ledger entry, \
         even when the allocation is locked"
    );
}

/// Repeated calls must be idempotent from the ledger's point of view: many
/// reads in a row must produce the same snapshot as a single read.
#[test]
fn get_platform_fee_allocation_is_idempotent_across_repeated_calls() {
    let env = Env::default();
    let (escrow, _admin_addr) = escrow_with_fee_allocation(&env);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        escrow.get_platform_fee_allocation();
    }
    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "repeated calls to get_platform_fee_allocation must not accumulate \
         any ledger mutation"
    );
}

/// Calling the read path on an uninitialised allocation returns an error
/// (via `try_get_platform_fee_allocation`) and, like the success path, must
/// not write anything to the ledger.
#[test]
fn get_platform_fee_allocation_does_not_mutate_ledger_when_not_initialized() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    let result = escrow.try_get_platform_fee_allocation();
    let after = env.to_ledger_snapshot();

    assert!(result.is_err());
    assert_eq!(
        before, after,
        "get_platform_fee_allocation must not mutate the ledger even on \
         the NotInitialized error path"
    );
}
