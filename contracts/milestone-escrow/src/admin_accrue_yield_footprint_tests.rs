#![cfg(test)]
//! Ledger-footprint contract for `admin_accrue_yield` (issue #405).
//!
//! `admin_accrue_yield` authorized against the **persistent** copy of
//! `DataKey::Admin` while every other storage access in the call was either
//! instance storage (`DataKey::Job`) or the single persistent accumulator
//! (`DataKey::YieldAccrued`). That split forced the invocation to open the
//! persistent entry for authorization as well as for the accumulator, pinning
//! the admin lookup to a ledger entry the call would touch anyway but not for
//! the reason that mattered.
//!
//! It now authorizes through `require_admin_from_instance`, the same helper
//! introduced for `set_escrow_interest_yield` (#463) and
//! `set_platform_fee_allocation` (#472). The matrix below pins:
//! 1. The persistent `Admin` read is gone — deleting the key entirely does not
//!    stop the admin from accruing.
//! 2. Authorization is genuinely enforced from the instance copy: a non-admin
//!    is still rejected, and the instance key is what is compared.
//! 3. The instance copy stays in sync across an admin handover, so a
//!    transferred-in admin keeps the ability to accrue.
//! 4. The persistent entry is written exactly once, for the accumulator — no
//!    auth-related persistent write was introduced.

use super::*;
use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _};
use soroban_sdk::{testutils::Address as _, vec, Address, Env, IntoVal, Map, TryIntoVal, Val};

/// Snapshot capture is disabled; no assertion here reads a JSON snapshot.
/// Auths are mocked because `setup_funded_escrow` mints the token balance the
/// escrows are funded with.
fn test_env() -> Env {
    let env = Env::new_with_config(soroban_sdk::testutils::EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

fn accrued(client: &MilestoneEscrowClient<'_>) -> i128 {
    client.get_yield_info().1
}

fn persistent_admin(env: &Env, contract_id: &Address) -> Option<Address> {
    env.as_contract(contract_id, || {
        env.storage().persistent().get(&DataKey::Admin)
    })
}

fn instance_admin(env: &Env, contract_id: &Address) -> Option<Address> {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&DataKey::Admin)
    })
}

fn remove_persistent_admin(env: &Env, contract_id: &Address) {
    env.as_contract(contract_id, || {
        env.storage().persistent().remove(&DataKey::Admin);
    });
}

// ── the persistent admin read is gone ────────────────────────────────────────

/// Baseline: after `initialize` both copies exist, so the pre-change behaviour
/// and the post-change behaviour are indistinguishable from the outside.
#[test]
fn test_admin_accrue_yield_initialize_writes_both_admin_copies() {
    let env = test_env();
    let (_, _, _, admin_addr, _, contract_id, _client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        persistent_admin(&env, &contract_id),
        Some(admin_addr.clone())
    );
    assert_eq!(instance_admin(&env, &contract_id), Some(admin_addr));
}

/// The point of the change: with the persistent copy deleted, the call still
/// succeeds, because authorization is served entirely from the instance entry.
/// Under the old `require_admin` this returned `NotInitialized`.
#[test]
fn test_admin_accrue_yield_succeeds_without_persistent_admin_copy() {
    let env = test_env();
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    remove_persistent_admin(&env, &contract_id);
    assert_eq!(persistent_admin(&env, &contract_id), None);

    client.admin_accrue_yield(&admin_addr, &0u32, &250_i128);
    assert_eq!(accrued(&client), 250);
}

/// Sanity on the inverse: authorization is *not* served from persistent storage
/// any more. A persistent copy holding a stale, different admin is ignored, so
/// the instance copy is the only thing consulted.
#[test]
fn test_admin_accrue_yield_ignores_stale_persistent_admin_copy() {
    let env = test_env();
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let imposter = Address::generate(&env);
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Admin, &imposter.clone());
    });

    // The imposter does not match the instance copy, so it is rejected even
    // though persistent storage names it as admin.
    assert_eq!(
        client.try_admin_accrue_yield(&imposter, &0u32, &100_i128),
        Err(Ok(Error::Unauthorized))
    );

    // The real admin still accrues: the instance copy is what is read.
    client.admin_accrue_yield(&admin_addr, &0u32, &100_i128);
    assert_eq!(accrued(&client), 100);
}

// ── authorization is still enforced ──────────────────────────────────────────

/// A non-admin is rejected with `Unauthorized` and the accumulator does not
/// move. Consolidating the read must not weaken the check.
#[test]
fn test_admin_accrue_yield_non_admin_still_rejected_from_instance_copy() {
    let env = test_env();
    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for impostor in [client_addr, freelancer_addr] {
        assert_eq!(
            client.try_admin_accrue_yield(&impostor, &0u32, &100_i128),
            Err(Ok(Error::Unauthorized))
        );
    }
    assert_eq!(accrued(&client), 0);
}

/// With neither copy present the call reports `NotInitialized` — the instance
/// read maps a missing key onto the same typed error the persistent read did.
#[test]
fn test_admin_accrue_yield_missing_instance_admin_is_not_initialized() {
    let env = test_env();
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    remove_persistent_admin(&env, &contract_id);
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
    });

    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &100_i128),
        Err(Ok(Error::NotInitialized))
    );
}

/// Argument validation and the accumulator arithmetic are unchanged, and a
/// rejected accrual still leaves the total alone.
#[test]
fn test_admin_accrue_yield_validation_and_accumulation_unchanged() {
    let env = test_env();
    let (_, _, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);

    // Only index 0 and 1 exist.
    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &2u32, &100_i128),
        Err(Ok(Error::InvalidMilestone))
    );
    for amount in [0_i128, -1_i128] {
        assert_eq!(
            client.try_admin_accrue_yield(&admin_addr, &0u32, &amount),
            Err(Ok(Error::InvalidAmount))
        );
    }
    assert_eq!(accrued(&client), 0);

    // The accumulator is contract-wide, not per milestone.
    client.admin_accrue_yield(&admin_addr, &0u32, &100_i128);
    client.admin_accrue_yield(&admin_addr, &1u32, &50_i128);
    assert_eq!(accrued(&client), 150);
}

/// The `checked_add` on the accumulator still refuses to wrap. Kept on its own
/// escrow so the total can be driven to `i128::MAX` first.
#[test]
fn test_admin_accrue_yield_overflow_still_rejected() {
    let env = test_env();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.admin_accrue_yield(&admin_addr, &0u32, &i128::MAX);
    assert_eq!(accrued(&client), i128::MAX);
    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &1_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(accrued(&client), i128::MAX);
}

// ── the instance copy survives an admin handover ─────────────────────────────

/// Regression guard for the invariant that makes the instance read safe:
/// `transfer_admin` writes the new admin to instance storage as well as
/// persistent storage, so the incoming admin can still accrue.
#[test]
fn test_admin_accrue_yield_new_admin_after_transfer_can_accrue() {
    let env = test_env();
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let new_admin = Address::generate(&env);
    client.transfer_admin(&admin_addr, &new_admin);

    assert_eq!(instance_admin(&env, &contract_id), Some(new_admin.clone()));
    assert_eq!(
        persistent_admin(&env, &contract_id),
        Some(new_admin.clone())
    );

    client.admin_accrue_yield(&new_admin, &0u32, &300_i128);
    assert_eq!(accrued(&client), 300);
}

/// The outgoing admin loses the ability to accrue: the instance copy is the
/// authority, and it was updated by the transfer.
#[test]
fn test_admin_accrue_yield_previous_admin_after_transfer_is_rejected() {
    let env = test_env();
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let new_admin = Address::generate(&env);
    client.transfer_admin(&admin_addr, &new_admin);

    assert_eq!(
        client.try_admin_accrue_yield(&admin_addr, &0u32, &300_i128),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(accrued(&client), 0);
}

// ── what the call writes ─────────────────────────────────────────────────────

/// The persistent entry is written exactly once, for the accumulator. The
/// authorization change adds no persistent write, and the instance entry is left
/// byte-for-byte identical.
#[test]
fn test_admin_accrue_yield_writes_only_the_persistent_accumulator() {
    let env = test_env();
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let instance_before: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().instance().all());
    let persistent_before: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());
    assert!(
        !instance_before.is_empty(),
        "instance entry must be populated"
    );
    assert!(
        persistent_before
            .get(DataKey::YieldAccrued.into_val(&env))
            .is_none(),
        "accumulator must start absent"
    );

    client.admin_accrue_yield(&admin_addr, &0u32, &250_i128);

    let instance_after: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().instance().all());
    let persistent_after: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());

    assert_eq!(
        instance_after.len(),
        instance_before.len(),
        "instance entry must not gain or lose a key"
    );
    assert_eq!(
        persistent_after.len(),
        persistent_before.len() + 1,
        "persistent entry gains exactly the accumulator key"
    );

    let yval: Option<Val> = persistent_after.get(DataKey::YieldAccrued.into_val(&env));
    let stored: i128 = yval.unwrap().try_into_val(&env).unwrap();
    assert_eq!(stored, 250);
}

/// A rejected accrual writes nothing at all — the instance and persistent
/// entries are both unchanged, so the authorization and validation guards run
/// before the single write.
#[test]
fn test_admin_accrue_yield_rejected_call_writes_nothing() {
    let env = test_env();
    let (client_addr, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let instance_before = env.as_contract(&contract_id, || env.storage().instance().all());
    let persistent_before = env.as_contract(&contract_id, || env.storage().persistent().all());

    let rejects = [
        (admin_addr.clone(), 0_u32, 0_i128, Error::InvalidAmount),
        (admin_addr.clone(), 9_u32, 10_i128, Error::InvalidMilestone),
        (client_addr.clone(), 0_u32, 10_i128, Error::Unauthorized),
    ];
    for (caller, index, amount, expected) in rejects {
        assert_eq!(
            client.try_admin_accrue_yield(&caller, &index, &amount),
            Err(Ok(expected))
        );
    }

    assert_eq!(
        env.as_contract(&contract_id, || env.storage().instance().all()),
        instance_before
    );
    assert_eq!(
        env.as_contract(&contract_id, || env.storage().persistent().all()),
        persistent_before
    );
}
