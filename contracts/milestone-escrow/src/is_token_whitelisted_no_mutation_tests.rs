#![cfg(test)]
//! Regression suite for issue #498: guarantee that `is_token_whitelisted`
//! performs no state mutation.
//!
//! `is_token_whitelisted` is a public read path that callers use to ask whether
//! a token address is accepted for escrow funding. It must never write to
//! instance, persistent, or temporary storage, and must never publish events —
//! a regression here would let a "read" silently rewrite ledger state or
//! affect contract entry TTLs.
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
//! `is_token_whitelisted` (or in `read_whitelist`, the shared helper it calls)
//! will change the snapshot and fail this suite.
//!
//! TTL extension is the most realistic way for this regression to creep in:
//! `extend_ttl` leaves the value bytes unchanged but mutates the live-until
//! sequence, which `to_ledger_snapshot` does capture — so a future refactor
//! that "helpfully" bumps the whitelist entry's TTL on read is caught here too.

use crate::{MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{vec, Address, Env};

/// Capacity cap enforced by `add_whitelisted_token`.
const MAX_WHITELIST_SIZE: u32 = 50;

/// Register an initialized contract. `initialize` whitelists `token`, so the
/// returned client starts with a one-entry whitelist, and `admin` is the
/// address authorized to mutate that whitelist.
fn setup_initialized(env: &Env) -> (MilestoneEscrowClient<'_>, Address, Address) {
    let admin = Address::generate(env);
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize(
        &admin,
        &Address::generate(env),
        &Address::generate(env),
        &Address::generate(env),
        &token,
        &604800,
        &vec![env, 1_000_i128],
    );

    (escrow, admin, token)
}

/// Assert `is_token_whitelisted(token)` returns `expected` without touching
/// the ledger or publishing an event.
fn assert_query_is_read_only(
    env: &Env,
    escrow: &MilestoneEscrowClient<'_>,
    token: &Address,
    expected: bool,
) {
    let before = env.to_ledger_snapshot();

    let actual = escrow.is_token_whitelisted(token);

    // The test env only keeps events from the latest invocation, so this is
    // exactly what this call published.
    assert!(
        crate::all_event_tuples(env).is_empty(),
        "is_token_whitelisted published an event"
    );

    let after = env.to_ledger_snapshot();
    assert_eq!(before, after, "is_token_whitelisted mutated the ledger");
    assert_eq!(actual, expected, "is_token_whitelisted returned the wrong result");
}

#[test]
fn is_token_whitelisted_on_uninitialized_contract_does_not_mutate_ledger() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();

    // The whitelist was never written, so the read must report `false` rather
    // than panicking — and must not create the key in the process.
    assert_query_is_read_only(&env, &escrow, &token, false);
}

#[test]
fn is_token_whitelisted_for_listed_token_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow, _admin, token) = setup_initialized(&env);

    assert!(escrow.get_whitelisted_tokens().contains(&token));
    assert_query_is_read_only(&env, &escrow, &token, true);
}

#[test]
fn is_token_whitelisted_for_unlisted_token_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow, _admin, _token) = setup_initialized(&env);

    // A miss still has to be a pure read.
    let unlisted = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    assert_query_is_read_only(&env, &escrow, &unlisted, false);
}

#[test]
fn is_token_whitelisted_repeated_queries_do_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow, _admin, token) = setup_initialized(&env);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        assert!(escrow.is_token_whitelisted(&token));
        assert!(!escrow.is_token_whitelisted(&Address::generate(&env)));
    }
    let after = env.to_ledger_snapshot();
    assert_eq!(
        before, after,
        "repeated is_token_whitelisted calls mutated the ledger"
    );
}

#[test]
fn is_token_whitelisted_does_not_mutate_ledger_after_whitelist_grows() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow, admin, token) = setup_initialized(&env);

    let added = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    escrow.add_whitelisted_token(&admin, &added);

    assert_query_is_read_only(&env, &escrow, &token, true);
    assert_query_is_read_only(&env, &escrow, &added, true);
}

#[test]
fn is_token_whitelisted_does_not_mutate_ledger_after_token_is_removed() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow, admin, kept) = setup_initialized(&env);

    let removed = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    escrow.add_whitelisted_token(&admin, &removed);
    escrow.remove_whitelisted_token(&admin, &removed);

    assert_query_is_read_only(&env, &escrow, &removed, false);
    assert_query_is_read_only(&env, &escrow, &kept, true);
}

#[test]
fn is_token_whitelisted_does_not_mutate_ledger_on_full_whitelist() {
    let env = Env::default();
    env.mock_all_auths();
    let (escrow, admin, first) = setup_initialized(&env);

    // Fill the whitelist to its cap so the read has to scan a maximal vector.
    let mut tokens = vec![&env, first.clone()];
    while tokens.len() < MAX_WHITELIST_SIZE {
        let next = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        escrow.add_whitelisted_token(&admin, &next);
        tokens.push_back(next);
    }

    for token in tokens.iter() {
        assert_query_is_read_only(&env, &escrow, &token, true);
    }
}

#[test]
fn is_token_whitelisted_does_not_mutate_ledger_for_contract_address() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    escrow.initialize(
        &admin,
        &Address::generate(&env),
        &Address::generate(&env),
        &Address::generate(&env),
        &token,
        &604800,
        &vec![&env, 1_000_i128],
    );

    // The contract's own address can never be whitelisted
    // (`add_whitelisted_token` rejects it), so querying it is still a read.
    assert_query_is_read_only(&env, &escrow, &contract_id, false);
}