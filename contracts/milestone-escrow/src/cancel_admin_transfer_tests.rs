#![cfg(test)]
//! Unit tests for `cancel_admin_transfer_proposal`.
//!
//! The endpoint had no coverage: snapshots for these test names survive in
//! `test_snapshots/`, but the tests themselves were lost when `test.rs` was
//! rewritten. Cancelling is the only escape hatch from a mistaken
//! `propose_admin_transfer` — a pending proposal blocks every later one with
//! `AdminTransferPending` — so it is worth pinning down.
//!
//! Issue #447 - Reduce ledger storage footprint of
//! `cancel_admin_transfer_proposal`.
//!
//! Auditing the endpoint found it already at the minimum footprint achievable
//! for this operation: it writes exactly one persistent key
//! (`DataKey::PendingAdminTransfer`, via `remove`) and reads exactly one other
//! (`DataKey::Admin`, for the caller-authorization check, which cannot be
//! dropped without losing the `Unauthorized` guard). There was no redundant
//! key to eliminate, so no change was made to the endpoint body.
//!
//! Two regression guards pin that footprint down. The first
//! (`test_cancel_admin_transfer_footprint_is_single_key_write`) checks the
//! two keys identified by that audit. The second
//! (`test_cancel_admin_transfer_footprint_full_storage_snapshot`) is
//! strictly stronger: it enumerates *every* key in instance, persistent, and
//! temporary storage via `testutils::storage::{Instance, Persistent,
//! Temporary}::all()` rather than checking hand-picked keys, so it also
//! catches a future change that starts writing some third, unrelated key
//! that the first test wouldn't know to look for.

use super::*;
use crate::test::setup_funded_escrow;
use crate::{AdminTransferCancelledEvent, DataKey, Error};
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Map, Val};

fn admincxl_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("admincxl").into_val(env);
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn test_cancel_admin_transfer_happy_path() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &7u32);
    assert!(client.get_pending_admin_transfer().is_some());

    client.cancel_admin_transfer_proposal(&admin_addr);

    // Read the tally straight after the call: env.events() reports only the
    // most recent invocation, so any later client call would clear it.
    assert_eq!(admincxl_event_count(&env), 1);
    let events = crate::all_event_tuples(&env);
    let ev = AdminTransferCancelledEvent::from_val(&env, &events.last().unwrap().2);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.proposal_id, 7);

    assert!(client.get_pending_admin_transfer().is_none());
}

#[test]
fn test_cancel_admin_transfer_no_pending_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_cancel_admin_transfer_proposal(&admin_addr),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
}

#[test]
fn test_cancel_admin_transfer_unauthorized_no_mutation() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &3u32);

    // The escrow client is not the admin.
    assert_eq!(
        client.try_cancel_admin_transfer_proposal(&client_addr),
        Err(Ok(Error::Unauthorized))
    );

    // A rejected call must leave the proposal exactly as it was.
    let pending = client.get_pending_admin_transfer().unwrap();
    assert_eq!(pending.new_admin, new_admin);
    assert_eq!(pending.proposal_id, 3);
}

#[test]
fn test_cancel_admin_transfer_uninitialized_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    // No admin is stored yet, so the guard must reject before touching
    // the pending-transfer key.
    assert_eq!(
        client.try_cancel_admin_transfer_proposal(&caller),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_cancel_admin_transfer_proposal_clears_lock_and_allows_new_proposal() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let first = Address::generate(&env);
    let second = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &first, &1u32);

    // While one is pending, a second proposal is refused outright.
    assert_eq!(
        client.try_propose_admin_transfer(&admin_addr, &second, &2u32),
        Err(Ok(Error::AdminTransferPending))
    );

    client.cancel_admin_transfer_proposal(&admin_addr);

    // Cancelling releases that block, which is the point of the endpoint.
    client.propose_admin_transfer(&admin_addr, &second, &2u32);
    let pending = client.get_pending_admin_transfer().unwrap();
    assert_eq!(pending.new_admin, second);
    assert_eq!(pending.proposal_id, 2);
}

/// Issue #447 footprint guard: a successful cancel writes exactly one
/// persistent key (`PendingAdminTransfer`, removed) and leaves every other
/// key it touched untouched. `DataKey::Admin` is read for authorization but
/// must come out bit-for-bit identical, since the endpoint has no business
/// writing it. This pins the current minimal footprint so a future edit that
/// accidentally starts writing a second key (e.g. re-persisting the admin,
/// or logging the cancellation to its own storage entry) fails loudly here
/// instead of silently widening the call's ledger footprint.
#[test]
fn test_cancel_admin_transfer_footprint_is_single_key_write() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &9u32);

    let admin_before: Address = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin).unwrap()
    });
    let pending_present_before: bool = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
    });
    assert!(pending_present_before);

    client.cancel_admin_transfer_proposal(&admin_addr);

    // The one key the call is allowed to write is gone.
    let pending_present_after: bool = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
    });
    assert!(!pending_present_after);

    // The only other key it touches (Admin, for auth) must be read-only:
    // still present, still the same value.
    let admin_after: Address = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin).unwrap()
    });
    assert_eq!(admin_after, admin_before);
    assert_eq!(admin_after, admin_addr);
}

/// Full-storage-snapshot version of the footprint guard above. Enumerates
/// every entry in instance, persistent, and temporary storage before and
/// after the call, rather than checking only the two keys the earlier audit
/// identified — so it also catches a future edit that starts writing some
/// unrelated third key that nobody thought to hand-pick.
#[test]
fn test_cancel_admin_transfer_footprint_full_storage_snapshot() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    client.propose_admin_transfer(&admin_addr, &new_admin, &11u32);

    let (instance_before, persistent_before, temporary_before): (
        Map<Val, Val>,
        Map<Val, Val>,
        Map<Val, Val>,
    ) = env.as_contract(&contract_id, || {
        (
            env.storage().instance().all(),
            env.storage().persistent().all(),
            env.storage().temporary().all(),
        )
    });

    // Sanity: the key we expect to disappear is actually present beforehand.
    let pending_key: Val = DataKey::PendingAdminTransfer.into_val(&env);
    assert!(persistent_before.get(pending_key).is_some());

    client.cancel_admin_transfer_proposal(&admin_addr);

    let (instance_after, persistent_after, temporary_after): (
        Map<Val, Val>,
        Map<Val, Val>,
        Map<Val, Val>,
    ) = env.as_contract(&contract_id, || {
        (
            env.storage().instance().all(),
            env.storage().persistent().all(),
            env.storage().temporary().all(),
        )
    });

    // Instance and temporary storage are completely untouched — not one key
    // added, removed, or changed.
    assert_eq!(instance_after, instance_before);
    assert_eq!(temporary_after, temporary_before);

    // Persistent storage lost exactly one entry: PendingAdminTransfer. Every
    // other persistent key/value pair (including Admin) survives unchanged.
    assert_eq!(persistent_after.len(), persistent_before.len() - 1);
    assert!(persistent_after.get(pending_key).is_none());
    let mut expected_persistent = persistent_before.clone();
    expected_persistent.remove(pending_key);
    assert_eq!(persistent_after, expected_persistent);
}
