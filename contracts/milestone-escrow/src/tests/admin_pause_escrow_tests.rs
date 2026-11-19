#![cfg(test)]
//! Dedicated unit-test suite for `admin_pause_escrow` authorization and
//! precondition guards (issue #349).
//!
//! The endpoint must reject an uninitialised contract with `NotInitialized`,
//! a non-admin caller with `Unauthorized`, and a mid-flight pause/resume
//! transition with `EmergencyPauseInProgress`, all before any ledger entry
//! is written.  Calling pause on an already-paused escrow must be rejected
//! with `AlreadyPaused` so an operator cannot mistake a redundant call for
//! having taken fresh action.  A first pause must set `DataKey::Paused = true`,
//! emit exactly one `pause` event, and leave the emergency-pause lock clear.

use super::*;
use crate::{DataKey, Error, EscrowPausedEvent};
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

    let amounts = vec![env, 1_000_i128];
    let spec_hashes = crate::test_spec_hashes(env, &amounts);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604_800u64,
        &amounts,
        &spec_hashes,
    );

    (escrow, admin_addr)
}

/// Count events whose single topic matches the `pause` symbol.
fn pause_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("pause").into_val(env);
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

/// Return the body of the most recently emitted `pause` event.
fn last_pause_event(env: &Env) -> EscrowPausedEvent {
    let events = crate::all_event_tuples(env);
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
            .get::<_, bool>(&DataKey::Ep)
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
#[ignore = "storage reentrancy guard removed (#675)"]
fn pause_rejects_when_emergency_lock_held() {
    let env = test_env();
    let (client, admin) = initialised_escrow(&env);
    let contract_id = client.address.clone();

    // Manually set the emergency-pause lock to simulate a mid-flight transition.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Ep, &true);
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

// ── redundant pause rejection ───────────────────────────────────────────────────

#[test]
fn pause_rejects_redundant_call_with_already_paused() {
    let env = test_env();
    let (client, admin) = initialised_escrow(&env);
    let contract_id = client.address.clone();

    // First pause — the happy path. The event tally is read before the
    // storage helpers below: they go through env.as_contract, and
    // crate::all_event_tuples(&env) reports the most recent invocation rather than a
    // running total.
    assert_eq!(client.try_admin_pause_escrow(&admin), Ok(Ok(())));
    assert_eq!(pause_event_count(&env), 1);
    assert!(is_paused(&env, &contract_id));
    assert!(!is_lock_held(&env, &contract_id));

    // Second pause — must be rejected with AlreadyPaused so an operator
    // cannot mistake a redundant call for having taken fresh action.
    assert_eq!(
        client.try_admin_pause_escrow(&admin),
        Err(Ok(Error::AlreadyPaused))
    );
    assert_eq!(pause_event_count(&env), 0);
    // State unchanged.
    assert!(is_paused(&env, &contract_id));
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

    // Exactly one pause event with correct payload — read first, before the
    // env.as_contract helpers below.
    assert_eq!(pause_event_count(&env), 1);
    let event = last_pause_event(&env);
    assert_eq!(event.admin, admin);

    // Paused flag set.
    assert!(is_paused(&env, &contract_id));
    // Lock not left behind.
    assert!(!is_lock_held(&env, &contract_id));
}

#[test]
fn pause_touches_minimal_storage_footprint() {
    let env = test_env();
    let (client, admin) = initialised_escrow(&env);
    let contract_id = client.address.clone();

    // Verify EpLk is not mutated by admin_pause_escrow
    assert!(!is_lock_held(&env, &contract_id));
    assert_eq!(client.try_admin_pause_escrow(&admin), Ok(Ok(())));
    assert!(is_paused(&env, &contract_id));
    assert!(!is_lock_held(&env, &contract_id));
}

struct PauseTestFixture<'a> {
    env: Env,
    escrow: MilestoneEscrowClient<'a>,
    admin: Address,
    client: Address,
    freelancer: Address,
    arbiter: Address,
    token_id: Address,
}

fn pause_test_fixture(env: &Env) -> PauseTestFixture<'_> {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let amounts = vec![env, 1_000_i128];
    let spec_hashes = crate::test_spec_hashes(env, &amounts);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token_id,
        &604_800u64,
        &amounts,
        &spec_hashes,
    );

    let token_admin = soroban_sdk::token::StellarAssetClient::new(env, &token_id);
    token_admin.mint(&client, &100_000_i128);

    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);

    PauseTestFixture {
        env: env.clone(),
        escrow,
        admin,
        client,
        freelancer,
        arbiter,
        token_id,
    }
}

#[test]
fn admin_pause_escrow_blocks_fund_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    let token_client = soroban_sdk::token::Client::new(&f.env, &f.token_id);

    f.escrow.admin_pause_escrow(&f.admin);

    let client_before = token_client.balance(&f.client);
    let contract_before = token_client.balance(&f.escrow.address);

    assert_eq!(f.escrow.try_fund(&f.client), Err(Ok(Error::Paused)));

    assert_eq!(token_client.balance(&f.client), client_before);
    assert_eq!(token_client.balance(&f.escrow.address), contract_before);

    f.escrow.admin_resume_escrow(&f.admin);
    assert_eq!(f.escrow.try_fund(&f.client), Ok(Ok(())));
    assert_eq!(token_client.balance(&f.escrow.address), 1_000_i128);
}

#[test]
fn admin_pause_escrow_blocks_mark_delivered_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    f.escrow.fund(&f.client);

    f.escrow.admin_pause_escrow(&f.admin);

    assert_eq!(
        f.escrow.try_mark_delivered(&f.freelancer, &0),
        Err(Ok(Error::Paused))
    );

    f.escrow.admin_resume_escrow(&f.admin);
    assert_eq!(f.escrow.try_mark_delivered(&f.freelancer, &0), Ok(Ok(())));
}

#[test]
fn admin_pause_escrow_blocks_approve_partial_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    let token_client = soroban_sdk::token::Client::new(&f.env, &f.token_id);
    f.escrow.fund(&f.client);
    f.escrow.mark_delivered(&f.freelancer, &0);

    f.escrow.admin_pause_escrow(&f.admin);

    let contract_before = token_client.balance(&f.escrow.address);
    let freelancer_before = token_client.balance(&f.freelancer);

    assert_eq!(
        f.escrow.try_approve_partial(&f.client, &0, &300),
        Err(Ok(Error::Paused))
    );

    assert_eq!(token_client.balance(&f.escrow.address), contract_before);
    assert_eq!(token_client.balance(&f.freelancer), freelancer_before);

    f.escrow.admin_resume_escrow(&f.admin);
    assert_eq!(
        f.escrow.try_approve_partial(&f.client, &0, &300),
        Ok(Ok(()))
    );
    assert_eq!(token_client.balance(&f.freelancer), 300_i128);
}

#[test]
fn admin_pause_escrow_blocks_approve_milestone_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    let token_client = soroban_sdk::token::Client::new(&f.env, &f.token_id);
    f.escrow.fund(&f.client);
    f.escrow.mark_delivered(&f.freelancer, &0);

    f.escrow.admin_pause_escrow(&f.admin);

    let contract_before = token_client.balance(&f.escrow.address);
    let freelancer_before = token_client.balance(&f.freelancer);

    assert_eq!(
        f.escrow.try_approve_milestone(&f.client, &0),
        Err(Ok(Error::Paused))
    );

    assert_eq!(token_client.balance(&f.escrow.address), contract_before);
    assert_eq!(token_client.balance(&f.freelancer), freelancer_before);

    f.escrow.admin_resume_escrow(&f.admin);
    assert_eq!(f.escrow.try_approve_milestone(&f.client, &0), Ok(Ok(())));
    assert_eq!(token_client.balance(&f.freelancer), 1_000_i128);
}

#[test]
fn admin_pause_escrow_blocks_claim_auto_release_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    let token_client = soroban_sdk::token::Client::new(&f.env, &f.token_id);
    f.escrow.fund(&f.client);
    f.escrow.mark_delivered(&f.freelancer, &0);

    let current_time = env.ledger().timestamp();
    env.ledger().set_timestamp(current_time + 604_801);

    f.escrow.admin_pause_escrow(&f.admin);

    let contract_before = token_client.balance(&f.escrow.address);
    let freelancer_before = token_client.balance(&f.freelancer);

    assert_eq!(
        f.escrow.try_claim_auto_release(&f.freelancer, &0),
        Err(Ok(Error::Paused))
    );

    assert_eq!(token_client.balance(&f.escrow.address), contract_before);
    assert_eq!(token_client.balance(&f.freelancer), freelancer_before);

    f.escrow.admin_resume_escrow(&f.admin);
    assert_eq!(
        f.escrow.try_claim_auto_release(&f.freelancer, &0),
        Ok(Ok(()))
    );
    assert_eq!(token_client.balance(&f.freelancer), 1_000_i128);
}

#[test]
fn admin_pause_escrow_blocks_raise_dispute_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    f.escrow.fund(&f.client);
    f.escrow.mark_delivered(&f.freelancer, &0);

    f.escrow.admin_pause_escrow(&f.admin);

    assert_eq!(
        f.escrow
            .try_raise_dispute(&f.client, &0, &dispute_reason_hash(&env)),
        Err(Ok(Error::Paused))
    );

    f.escrow.admin_resume_escrow(&f.admin);
    assert_eq!(
        f.escrow
            .try_raise_dispute(&f.client, &0, &dispute_reason_hash(&env)),
        Ok(Ok(()))
    );
}

#[test]
fn admin_pause_escrow_blocks_resolve_dispute_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    let token_client = soroban_sdk::token::Client::new(&f.env, &f.token_id);
    f.escrow.fund(&f.client);
    f.escrow.mark_delivered(&f.freelancer, &0);
    f.escrow
        .raise_dispute(&f.client, &0, &dispute_reason_hash(&env));

    f.escrow.admin_pause_escrow(&f.admin);

    let contract_before = token_client.balance(&f.escrow.address);
    let freelancer_before = token_client.balance(&f.freelancer);

    assert_eq!(
        f.escrow.try_resolve_dispute(&f.arbiter, &0, &true),
        Err(Ok(Error::Paused))
    );

    assert_eq!(token_client.balance(&f.escrow.address), contract_before);
    assert_eq!(token_client.balance(&f.freelancer), freelancer_before);

    f.escrow.admin_resume_escrow(&f.admin);
    assert_eq!(
        f.escrow.try_resolve_dispute(&f.arbiter, &0, &true),
        Ok(Ok(()))
    );
    assert_eq!(token_client.balance(&f.freelancer), 1_000_i128);
}

#[test]
fn admin_pause_escrow_blocks_apply_dispute_arbitration_split_and_resumes() {
    let env = test_env();
    let f = pause_test_fixture(&env);
    let token_client = soroban_sdk::token::Client::new(&f.env, &f.token_id);
    f.escrow.fund(&f.client);
    f.escrow.mark_delivered(&f.freelancer, &0);
    f.escrow
        .raise_dispute(&f.client, &0, &dispute_reason_hash(&env));

    f.escrow.admin_pause_escrow(&f.admin);

    let contract_before = token_client.balance(&f.escrow.address);
    let client_before = token_client.balance(&f.client);
    let freelancer_before = token_client.balance(&f.freelancer);

    assert_eq!(
        f.escrow
            .try_apply_dispute_arbitration_split(&f.arbiter, &0, &5000),
        Err(Ok(Error::Paused))
    );

    assert_eq!(token_client.balance(&f.escrow.address), contract_before);
    assert_eq!(token_client.balance(&f.client), client_before);
    assert_eq!(token_client.balance(&f.freelancer), freelancer_before);

    f.escrow.admin_resume_escrow(&f.admin);
    assert!(f
        .escrow
        .try_apply_dispute_arbitration_split(&f.arbiter, &0, &5000)
        .is_ok());
    assert_eq!(token_client.balance(&f.client), client_before + 500_i128);
    assert_eq!(
        token_client.balance(&f.freelancer),
        freelancer_before + 500_i128
    );
}
