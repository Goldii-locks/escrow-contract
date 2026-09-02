#![cfg(test)]
//! Unit tests for `cancel_admin_transfer_proposal`.
//!
//! The endpoint had no coverage: snapshots for these test names survive in
//! `test_snapshots/`, but the tests themselves were lost when `test.rs` was
//! rewritten. Cancelling is the only escape hatch from a mistaken
//! `propose_admin_transfer` — a pending proposal blocks every later one with
//! `AdminTransferPending` — so it is worth pinning down.

use super::*;
use crate::test::setup_funded_escrow;
use crate::{AdminTransferCancelledEvent, Error};
use soroban_sdk::testutils::{Address as _, Events as _};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Val};

fn admincxl_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("admincxl").into_val(env);
    let mut count = 0u32;
    for event in env.events().all().iter() {
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
    let events = env.events().all();
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
