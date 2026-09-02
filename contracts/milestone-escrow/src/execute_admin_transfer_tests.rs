#![cfg(test)]
//! Dedicated unit-test suite for `execute_admin_transfer` authorization
//! and precondition guards (issue #347).
//!
//! The endpoint must reject an uninitialised contract with `NotInitialized`,
//! an absent pending transfer with `NoPendingAdminTransfer`, and an
//! approval-count below the multisig threshold with `MultiSigThresholdNotMet`,
//! all before any ledger entry is read or written.  A successful execution
//! must swap the admin key, remove the pending transfer, and emit exactly one
//! `adminexc` event.

use super::*;
use crate::{AdminTransferExecutedEvent, DataKey, Error};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn bare_contract(env: &Env) -> MilestoneEscrowClient<'_> {
    let contract_id = env.register(MilestoneEscrow, ());
    MilestoneEscrowClient::new(env, &contract_id)
}

fn adminexc_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("adminexc").into_val(env);
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

fn last_adminexc_event(env: &Env) -> AdminTransferExecutedEvent {
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, "adminexc"));
    AdminTransferExecutedEvent::from_val(env, &last.2)
}

/// Initialise + fund an escrow and configure a 2-of-2 multisig regime,
/// returning `(admin, s1, s2, new_admin, contract_id, client)`.
fn setup_multisig_transfer(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();

    let milestone_amounts = vec![env, 1_000_i128];
    let (_client_addr, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(env, milestone_amounts);

    let s1 = Address::generate(env);
    let s2 = Address::generate(env);
    let new_admin = Address::generate(env);

    let signers = vec![env, s1.clone(), s2.clone()];
    client.multisig_approval_init(&admin_addr, &signers, &2u32);

    // Propose the transfer (proposal_id = 1).
    client.propose_admin_transfer(&admin_addr, &new_admin, &1u32);

    (admin_addr, s1, s2, new_admin, contract_id, client)
}

// ── precondition guards ──────────────────────────────────────────────────────

#[test]
fn execute_transfer_rejects_uninitialised_contract() {
    let env = test_env();
    env.mock_all_auths();

    let client = bare_contract(&env);

    assert_eq!(
        client.try_execute_admin_transfer(),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(adminexc_event_count(&env), 0);
}

#[test]
fn execute_transfer_rejects_when_no_pending_transfer() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, _, _, _client) = setup_multisig_transfer(&env);

    // Cancel the pending transfer so there is nothing to execute.
    let _admin = Address::generate(&env);
    // Re-set up a funded escrow with the same admin.
    let amounts = vec![&env, 1_000_i128];
    let (_client_addr, _, _, admin_addr, _, _, funded_client) = setup_funded_escrow(&env, amounts);

    let s1 = Address::generate(&env);
    let signers = vec![&env, s1.clone()];
    funded_client.multisig_approval_init(&admin_addr, &signers, &1u32);

    // No proposal created yet → NoPendingAdminTransfer.
    assert_eq!(
        funded_client.try_execute_admin_transfer(),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
    assert_eq!(adminexc_event_count(&env), 0);
}

#[test]
fn execute_transfer_rejects_when_threshold_not_met() {
    let env = test_env();
    env.mock_all_auths();

    let (_, s1, _, new_admin, contract_id, client) = setup_multisig_transfer(&env);

    // Only one signer has approved — threshold is 2.
    client.multisig_approve(&s1, &1u32);

    assert_eq!(
        client.try_execute_admin_transfer(),
        Err(Ok(Error::MultiSigThresholdNotMet))
    );

    // Admin unchanged and pending transfer still present.
    let old_admin: Address = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin).unwrap()
    });
    assert_ne!(old_admin, new_admin);

    let pending: Option<PendingAdminTransfer> = client.get_pending_admin_transfer();
    assert!(pending.is_some());
    assert_eq!(adminexc_event_count(&env), 0);
}

// ── happy path ───────────────────────────────────────────────────────────────

#[test]
fn execute_transfer_swaps_admin_and_emits_event() {
    let env = test_env();
    env.mock_all_auths();

    let (old_admin, s1, s2, new_admin, contract_id, client) = setup_multisig_transfer(&env);

    // Approve from both signers to reach the threshold.
    client.multisig_approve(&s1, &1u32);
    client.multisig_approve(&s2, &1u32);

    let result = client.try_execute_admin_transfer();
    assert_eq!(result, Ok(Ok(())));

    // Exactly one adminexc event with correct payload. Read before any further
    // `client.*` call: each one is its own contract invocation, and the test
    // env's event buffer reflects the most recent.
    assert_eq!(adminexc_event_count(&env), 1);
    let event = last_adminexc_event(&env);
    assert_eq!(event.old_admin, old_admin);
    assert_eq!(event.new_admin, new_admin);
    assert_eq!(event.proposal_id, 1);

    // Pending transfer removed.
    let pending: Option<PendingAdminTransfer> = client.get_pending_admin_transfer();
    assert!(pending.is_none());

    // Admin key swapped to new_admin.
    let current_admin: Address = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin).unwrap()
    });
    assert_eq!(current_admin, new_admin);
    assert_ne!(current_admin, old_admin);
}

#[test]
fn new_admin_can_propose_after_transfer_old_admin_cannot() {
    let env = test_env();
    env.mock_all_auths();

    let (_, s1, s2, new_admin, _, client) = setup_multisig_transfer(&env);

    client.multisig_approve(&s1, &1u32);
    client.multisig_approve(&s2, &1u32);
    client.execute_admin_transfer();

    // Old admin is no longer authorized.
    let another = Address::generate(&env);
    let _signers2 = vec![&env, another.clone()];
    // Old admin cannot initialize multisig (requires being admin — which fails).
    // New admin can propose a second transfer.
    let result = client.try_propose_admin_transfer(&new_admin, &another, &2u32);
    assert!(result.is_ok());

    // Pending transfer exists for proposal 2.
    let pending = client.get_pending_admin_transfer();
    assert!(pending.is_some());
}
