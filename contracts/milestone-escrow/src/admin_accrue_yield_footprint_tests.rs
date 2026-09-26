#![cfg(test)]
//! Ledger-storage-footprint suite for `admin_accrue_yield` (issue #405).
//!
//! Before this change a single call touched **three** distinct ledger entries:
//!   * `DataKey::Admin`     (persistent) — via `require_admin`
//!   * `DataKey::Job`       (instance)   — via `load_job_meta`
//!   * `DataKey::YieldAccrued` (persistent) — read and then written
//!
//! `admin_accrue_yield` now authorizes through `require_admin_from_instance`,
//! so the admin read and the job-metadata read share the **single** instance
//! ledger entry.  The call is down to **two** entries: the instance entry
//! (`Admin` + `Job`) and the persistent `YieldAccrued` accumulator.
//!
//! The instance copy of `DataKey::Admin` is a complete mirror of the persistent
//! one — `initialize` writes both, and both `transfer_admin` and
//! `execute_admin_transfer` keep them in sync — so these tests also pin that a
//! post-transfer accrual is authorized against the *new* admin and rejected for
//! the old one, which is what makes the instance read safe to rely on.
//!
//! Every existing precondition is asserted unchanged: `NotInitialized`,
//! `Unauthorized`, `InvalidMilestone`, `InvalidAmount` (both the non-positive
//! input and the `i128` overflow), plus the happy path and accumulation.

use super::*;
use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _};
use soroban_sdk::{testutils::Address as _, vec, Address, Env, Map, Val};

/// Running `YieldAccrued` total, via the public getter.
fn accrued(client: &MilestoneEscrowClient<'_>) -> i128 {
    client.get_yield_info().1
}

fn admin_in(client_tier_instance: bool, contract_id: &Address, env: &Env) -> Option<Address> {
    env.as_contract(contract_id, || {
        if client_tier_instance {
            env.storage().instance().get(&DataKey::Admin)
        } else {
            env.storage().persistent().get(&DataKey::Admin)
        }
    })
}

// ── the footprint itself ─────────────────────────────────────────────────────

/// Issue #405: a successful accrual writes **one** distinct persistent key
/// (`YieldAccrued`) and no new instance key at all.
///
/// The instance tier is read twice before the call (once for `Admin`, once for
/// `Job`) but both reads hit the same ledger entry, so nothing is added there —
/// pinning that the reduction is a consolidation of reads, not a relocation of
/// the accumulator, and that a future edit cannot quietly start writing a third
/// entry.
#[test]
fn test_admin_accrue_yield_writes_a_single_distinct_key() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let instance_before: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().instance().all());
    let persistent_before: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());

    client.admin_accrue_yield(&admin_addr, &0u32, &250_i128);

    let instance_after: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().instance().all());
    let persistent_after: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());

    // Instance: every key present before is still present and unchanged — the
    // call only *reads* the instance entry, it never writes one.
    assert_eq!(instance_after, instance_before);

    // Persistent: the very first accrual has to create the `YieldAccrued`
    // accumulator, so the entry count grows by exactly one — the call writes a
    // single distinct key and touches nothing else.
    assert_eq!(
        persistent_after.len(),
        persistent_before.len() + 1,
        "only the accumulator may be created"
    );
    let stored_after: Option<i128> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::YieldAccrued)
    });
    assert_eq!(stored_after, Some(250));

    // A second accrual updates that same entry in place, so from here on the
    // footprint is a pure read-modify-write of one existing key.
    client.admin_accrue_yield(&admin_addr, &0u32, &250_i128);
    let persistent_after_second: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());
    assert_eq!(
        persistent_after_second.len(),
        persistent_before.len() + 1,
        "an update must not create another entry"
    );
    let stored_second: Option<i128> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::YieldAccrued)
    });
    assert_eq!(stored_second, Some(500));
}

/// The two `DataKey::Admin` copies are both present and agree after a normal
/// `initialize`, which is the invariant `require_admin_from_instance` depends
/// on. Documented here as a precondition of the footprint reduction.
#[test]
fn test_admin_accrue_yield_admin_mirrors_are_in_sync_after_initialize() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, _) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(admin_in(true, &contract_id, &env), Some(admin_addr.clone()));
    assert_eq!(admin_in(false, &contract_id, &env), Some(admin_addr));
}

/// After `transfer_admin` the **instance** mirror is what the accrual endpoint
/// authorizes against, so the new admin must be accepted and the old one
/// rejected. This is the test that would fail if the two mirrors ever drifted.
#[test]
fn test_admin_accrue_yield_authorizes_against_synced_instance_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, old_admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&old_admin, &new_admin);

    assert_eq!(admin_in(true, &contract_id, &env), Some(new_admin.clone()));
    assert_eq!(admin_in(false, &contract_id, &env), Some(new_admin.clone()));

    assert_eq!(
        client.try_admin_accrue_yield(&old_admin, &0u32, &100_i128),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(accrued(&client), 0);

    client.admin_accrue_yield(&new_admin, &0u32, &100_i128);
    assert_eq!(accrued(&client), 100);
}

/// The same invariant across the two-step handover
/// (`propose_admin_transfer` → `execute_admin_transfer`).
#[test]
fn test_admin_accrue_yield_authorizes_against_synced_admin_after_transfer_flow() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, old_admin, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    // The two-step handover runs through the multisig approval regime, so it
    // has to be initialised and satisfied before `execute_admin_transfer` will
    // swap the admin.
    let signer = Address::generate(&env);
    client.multisig_approval_init(&old_admin, &vec![&env, signer.clone()], &1u32);

    client.propose_admin_transfer(&old_admin, &new_admin, &7u32);
    client.multisig_approve(&signer, &7u32);
    client.execute_admin_transfer();

    assert_eq!(admin_in(true, &contract_id, &env), Some(new_admin.clone()));
    assert_eq!(
        client.try_admin_accrue_yield(&old_admin, &0u32, &100_i128),
        Err(Ok(Error::Unauthorized))
    );

    client.admin_accrue_yield(&new_admin, &0u32, &100_i128);
    assert_eq!(accrued(&client), 100);
}

// ── every existing precondition is unchanged ─────────────────────────────────

/// The happy path and multi-call accumulation still behave exactly as before:
/// the accumulator is contract-wide, not per milestone, and grows by exactly
/// the accrued amount each call.
#[test]
fn test_admin_accrue_yield_accumulation_unchanged_by_footprint_change() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);

    assert_eq!(accrued(&client), 0);
    client.admin_accrue_yield(&admin_addr, &0u32, &100_i128);
    client.admin_accrue_yield(&admin_addr, &1u32, &50_i128);
    client.admin_accrue_yield(&admin_addr, &0u32, &25_i128);
    assert_eq!(accrued(&client), 175);
}

/// `NotInitialized` is still returned — now by the instance `Admin` read rather
/// than the persistent one — for a contract that was never initialized.
#[test]
fn test_admin_accrue_yield_uninitialized_still_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    assert_eq!(
        client.try_admin_accrue_yield(&caller, &0u32, &100_i128),
        Err(Ok(Error::NotInitialized))
    );
}

/// `Unauthorized` is still returned for a signed non-admin caller, and the
/// accumulator is left untouched.
#[test]
fn test_admin_accrue_yield_non_admin_still_rejected_without_mutation() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_accrue_yield(&client_addr, &0u32, &100_i128),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        client.try_admin_accrue_yield(&freelancer_addr, &0u32, &100_i128),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(accrued(&client), 0);
}

/// `InvalidMilestone` for an out-of-range index, with no mutation.
#[test]
fn test_admin_accrue_yield_invalid_milestone_still_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &1u32, &100_i128),
        Err(Ok(Error::InvalidMilestone))
    );
    assert_eq!(accrued(&client), 0);
}

/// `InvalidAmount` for a non-positive input, with no mutation.
#[test]
fn test_admin_accrue_yield_non_positive_amount_still_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &0_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &i128::MIN),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(accrued(&client), 0);
}

/// The `i128` overflow guard is untouched: the running total is checked, not the
/// magnitude of the incoming amount, and a rejected accrual does not wrap the
/// accumulator.
#[test]
fn test_admin_accrue_yield_overflow_guard_untouched() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.admin_accrue_yield(&admin_addr, &0u32, &i128::MAX);
    assert_eq!(accrued(&client), i128::MAX);

    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &i128::MAX);
    });
    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &1_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(accrued(&client), i128::MAX);
}
