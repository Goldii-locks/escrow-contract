#![cfg(test)]
//! Dedicated unit-test suite for `admin_pause_escrow` authorization and
//! precondition guards (issue #349).
//!
//! The endpoint must reject an uninitialised contract with `NotInitialized`,
//! a non-admin caller with `Unauthorized`, and a mid-flight pause/resume
//! transition with `EmergencyPauseInProgress`, all before any ledger entry
//! is written.  Calling pause on an already-paused escrow must be an idempotent
//! no-op: `Ok(())` with zero storage mutation and no event.  A first pause
//! must set `DataKey::Paused = true`, emit exactly one `pause` event, and
//! leave the emergency-pause lock clear.

use super::*;
use crate::{DataKey, Error, EscrowPausedEvent};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, Address, Env, FromVal, IntoVal, Val};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn bare_contract(env: &Env) -> MilestoneEscrowClient<'_> {
    let contract_id = env.register(MilestoneEscrow, ());
    MilestoneEscrowClient::new(env, &contract_id)
}

/// A fully initialised, unpaused escrow plus its admin address.
fn initialised_escrow(env: &Env) -> (MilestoneEscrowClient<'_>, Address) {
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

    (escrow, admin_addr)
}

/// Count events whose single topic matches the `pause` symbol.
fn pause_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("pause").into_val(env);
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

/// Return the body of the most recently emitted `pause` event.
fn last_pause_event(env: &Env) -> EscrowPausedEvent {
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: soroban_sdk::Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, soroban_sdk::Symbol::new(env, "pause"));
    EscrowPausedEvent::from_val(env, &last.2)
}

fn is_paused(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::Paused)
            .unwrap_or(false)
    })
}

fn is_lock_held(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::EmergencyPauseLock)
            .unwrap_or(false)
    })
}

// ── precondition guards ──────────────────────────────────────────────────────

#[test]
fn pause_rejects_uninitialised_contract() {
    let env = test_env();
    env.mock_all_auths();

    let client = bare_contract(&env);
    let admin = Address::generate(&env);

    assert_eq!(
        client.try_admin_pause_escrow(&admin),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(pause_event_count(&env), 0);
}

#[test]
fn pause_rejects_non_admin_caller() {
    let env = test_env();
    let (client, _admin) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    assert_eq!(
        client.try_admin_pause_escrow(&attacker),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(pause_event_count(&env), 0);
}

#[test]
fn pause_rejects_when_emergency_lock_held() {
    let env = test_env();
    let (client, admin) = initialised_escrow(&env);
    let contract_id = client.address.clone();

    // Manually set the emergency-pause lock to simulate a mid-flight transition.
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &true);
    });

    assert_eq!(
        client.try_admin_pause_escrow(&admin),
        Err(Ok(Error::EmergencyPauseInProgress))
    );
    // No mutation: paused flag remains false, lock still held.
    assert!(!is_paused(&env, &contract_id));
    assert!(is_lock_held(&env, &contract_id));
    assert_eq!(pause_event_count(&env), 0);
}

// ── idempotent no-op ─────────────────────────────────────────────────────────

#[test]
fn pause_is_idempotent_when_already_paused() {
    let env = test_env();
    let (client, admin) = initialised_escrow(&env);
    let contract_id = client.address.clone();

    // First pause — the happy path.
    assert_eq!(client.try_admin_pause_escrow(&admin), Ok(Ok(())));
    assert!(is_paused(&env, &contract_id));
    assert!(!is_lock_held(&env, &contract_id));
    assert_eq!(pause_event_count(&env), 1);

    // Second pause — must be a no-op.
    assert_eq!(client.try_admin_pause_escrow(&admin), Ok(Ok(())));
    // Still only one pause event (the second call emits nothing).
    assert_eq!(pause_event_count(&env), 1);
    // Lock not left behind.
    assert!(!is_lock_held(&env, &contract_id));
}

// ── happy path ───────────────────────────────────────────────────────────────

#[test]
fn pause_sets_flag_emits_event_and_clears_lock() {
    let env = test_env();
    let (client, admin) = initialised_escrow(&env);
    let contract_id = client.address.clone();

    assert!(!is_paused(&env, &contract_id));
    assert_eq!(pause_event_count(&env), 0);

    assert_eq!(client.try_admin_pause_escrow(&admin), Ok(Ok(())));

    // Paused flag set.
    assert!(is_paused(&env, &contract_id));
    // Lock not left behind.
    assert!(!is_lock_held(&env, &contract_id));
    // Exactly one pause event with correct payload.
    assert_eq!(pause_event_count(&env), 1);
    let event = last_pause_event(&env);
    assert_eq!(event.admin, admin);
    assert_eq!(event.contract_id, contract_id);
}
