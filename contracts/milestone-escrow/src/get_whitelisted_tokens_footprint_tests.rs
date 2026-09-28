#![cfg(test)]
//! Storage-read suite for `get_whitelisted_tokens` (issue #484).
//!
//! `get_whitelisted_tokens` and `is_token_whitelisted` both go through the
//! single-`get` helper `read_whitelist`: no `has` probe precedes the `get`, and
//! `DataKey::WhitelistedTokens` is resolved once per invocation.
//!
//! The tests pin that two ways:
//!   - The per-invocation resource metering (`env.cost_estimate().resources()`)
//!     reports exactly two distinct ledger entries: the contract code and the
//!     single contract-instance entry that holds `WhitelistedTokens`.  No
//!     persistent or temporary entry is touched and nothing is written.
//!   - The returned value is exactly the stored list, in every state, and
//!     `is_token_whitelisted` agrees with it.

use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{testutils::Address as _, vec, Address, Env, Vec};

fn env_without_snapshot() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A contract whose instance holds `whitelist` and nothing else.
fn seeded<'a>(env: &'a Env, whitelist: &Vec<Address>) -> MilestoneEscrowClient<'a> {
    let contract_id = env.register(MilestoneEscrow, ());
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, whitelist);
    });
    MilestoneEscrowClient::new(env, &contract_id)
}

fn generate(env: &Env, n: u32) -> Vec<Address> {
    let mut out = Vec::new(env);
    for _ in 0..n {
        out.push_back(Address::generate(env));
    }
    out
}

// ── storage reads per call ───────────────────────────────────────────────────

/// Exactly two distinct ledger entries are read — the wasm code and the
/// contract-instance entry — and none are written.  The measurement is the
/// same for a one-entry list and a full fifty-entry list, so the read count
/// does not grow with the list.
#[test]
fn test_get_whitelisted_tokens_reads_one_storage_entry() {
    for n in [1_u32, 50] {
        let env = env_without_snapshot();
        let whitelist = generate(&env, n);
        let escrow = seeded(&env, &whitelist);

        assert_eq!(escrow.get_whitelisted_tokens(), whitelist);
        let resources = env.cost_estimate().resources();

        assert_eq!(
            resources.memory_read_entries, 2,
            "get_whitelisted_tokens must touch only contract_code + the \
             instance entry (n={n}); measured {resources:?}"
        );
        assert_eq!(resources.write_entries, 0, "a read path writes nothing");
        assert_eq!(resources.disk_read_entries, 0);
    }
}

/// The uninitialized path reads the same single entry before returning its
/// typed error.
#[test]
fn test_get_whitelisted_tokens_uninitialized_reads_one_storage_entry() {
    let env = env_without_snapshot();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        escrow.try_get_whitelisted_tokens(),
        Err(Ok(Error::NotInitialized))
    );
    let resources = env.cost_estimate().resources();
    assert_eq!(resources.memory_read_entries, 2);
    assert_eq!(resources.write_entries, 0);
}

/// `is_token_whitelisted` shares the helper and has the same footprint.
#[test]
fn test_is_token_whitelisted_reads_one_storage_entry() {
    let env = env_without_snapshot();
    let whitelist = generate(&env, 3);
    let escrow = seeded(&env, &whitelist);

    assert!(escrow.is_token_whitelisted(&whitelist.get(1).unwrap()));
    let resources = env.cost_estimate().resources();
    assert_eq!(resources.memory_read_entries, 2);
    assert_eq!(resources.write_entries, 0);
}

// ── the returned value is unchanged ──────────────────────────────────────────

/// The list comes back exactly as stored, including order and duplicates.
#[test]
fn test_get_whitelisted_tokens_returns_stored_list_verbatim() {
    let env = env_without_snapshot();
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);

    for stored in [
        Vec::<Address>::new(&env),
        vec![&env, a.clone()],
        vec![&env, c.clone(), a.clone(), b.clone()],
        vec![&env, a.clone(), a.clone()],
    ] {
        let escrow = seeded(&env, &stored);
        assert_eq!(escrow.get_whitelisted_tokens(), stored);
        // Repeated calls are stable.
        assert_eq!(escrow.get_whitelisted_tokens(), stored);
    }
}

/// After `initialize` the list holds exactly the configured token, and
/// `is_token_whitelisted` agrees with the list for members and non-members.
#[test]
fn test_get_whitelisted_tokens_after_initialize_matches_membership_query() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, token, _, escrow) =
        crate::test::setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let whitelist = escrow.get_whitelisted_tokens();
    assert_eq!(whitelist, vec![&env, token.clone()]);
    for member in whitelist.iter() {
        assert!(escrow.is_token_whitelisted(&member));
    }
    assert!(!escrow.is_token_whitelisted(&Address::generate(&env)));
}

/// With nothing stored, the membership query is `false` rather than an error.
#[test]
fn test_is_token_whitelisted_false_when_uninitialized() {
    let env = env_without_snapshot();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    assert!(!escrow.is_token_whitelisted(&Address::generate(&env)));
}
