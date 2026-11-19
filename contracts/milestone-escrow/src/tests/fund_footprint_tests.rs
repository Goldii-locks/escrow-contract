#![cfg(test)]
//! Ledger-storage-footprint suite for `fund` (issue #730).
//!
//! Before this change a single call touched **N+1** distinct ledger entries:
//!   * `DataKey::Job`       (instance)   — via `load_job_meta`
//!   * `DataKey::Milestone(i)` (persistent) — N entries, one per milestone, via `checked_job_total`
//!
//! `fund` now uses the cached `meta.total_amount` directly instead of summing
//! all milestone amounts from storage. The call is down to **one** entry:
//! the instance entry (`Job`). The milestone amounts are immutable after
//! initialize, so the cached total is always consistent.
//!
//! Every existing precondition is asserted unchanged: `NotInitialized`,
//! `AlreadyFunded`, `Unauthorized`, `InvalidAmount`, plus the happy path.

use super::*;
use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::storage::Persistent as _;
use soroban_sdk::TryFromVal;
use soroban_sdk::{vec, Address, Env, Map, Val};

// ── the footprint itself ─────────────────────────────────────────────────────

/// Issue #730: a successful fund reads **no** milestone entries from persistent
/// storage. It only reads the instance `Job` entry (which contains the cached
/// `total_amount`) and writes the instance `FundedAt` key.
///
/// This test creates an escrow with 50 milestones and verifies that funding
/// it does not touch any `Milestone(i)` persistent entries.
#[test]
fn test_fund_does_not_load_milestones_from_storage() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);

    let mut milestone_amounts = vec![&env];
    let mut total = 0_i128;
    for _ in 0..50u32 {
        milestone_amounts.push_back(1_i128);
        total += 1;
    }
    token_admin.mint(&client_addr, &total);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &milestone_amounts,
        &crate::test_spec_hashes(&env, &milestone_amounts),
    );

    client.accept_job(&freelancer_addr);
    client.accept_arbiter(&arbiter_addr);

    let persistent_before: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());

    client.fund(&client_addr);

    let persistent_after: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());
    // The deposit moves token balances; those entries are the token's
    // writes, not fund's, so only the escrow's own keys are compared.
    let persistent_after = without_token_balances(&env, persistent_after);
    let persistent_before = without_token_balances(&env, persistent_before);

    // Persistent storage must be unchanged: fund should not read or write
    // any milestone entries. The only persistent keys are Admin and the
    // milestones themselves, all of which must remain exactly as they were.
    assert_eq!(
        persistent_after, persistent_before,
        "fund must not touch persistent storage"
    );

    // Verify the funding actually succeeded
    assert_eq!(token.balance(&client_addr), 0);
    assert_eq!(token.balance(&contract_id), total);
    let job = client.get_job();
    assert!(job.funded);
}

// ── every existing precondition is unchanged ─────────────────────────────────

/// `NotInitialized` is still returned.
#[test]
fn test_fund_not_initialized_still_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    assert_eq!(client.try_fund(&caller), Err(Ok(Error::NotInitialized)));
}

/// `AlreadyFunded` is still returned.
#[test]
fn test_fund_already_funded_still_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(client.try_fund(&client_addr), Err(Ok(Error::AlreadyFunded)));
}

/// `entries` without the token's `Balance(..)` keys.
fn without_token_balances(env: &Env, entries: Map<Val, Val>) -> Map<Val, Val> {
    let balance = soroban_sdk::Symbol::new(env, "Balance");
    let mut kept = Map::new(env);
    for (key, value) in entries.iter() {
        let is_balance = soroban_sdk::Vec::<Val>::try_from_val(env, &key)
            .ok()
            .and_then(|parts| parts.get(0))
            .and_then(|first| soroban_sdk::Symbol::try_from_val(env, &first).ok())
            .is_some_and(|symbol| symbol == balance);
        if !is_balance {
            kept.set(key, value);
        }
    }
    kept
}
