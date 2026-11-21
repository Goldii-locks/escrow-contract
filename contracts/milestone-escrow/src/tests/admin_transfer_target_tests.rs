#![cfg(test)]
//! Target validation for `propose_admin_transfer` and `transfer_admin`
//! (issue #771).
//!
//! Both admin-transfer paths must refuse, with `InvalidAddress`:
//!   * the escrow contract's own address — the contract cannot sign an
//!     external transaction, so becoming its own admin would brick every
//!     admin endpoint permanently;
//!   * the current admin — a no-op that would still cost a full multisig
//!     round and emit misleading transfer events.
//!
//! A refused call must leave the admin key, the pending-transfer slot and the
//! event stream untouched.

use super::*;
use crate::{DataKey, Error};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, vec, Address, Env, IntoVal, Val};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// Funded escrow plus a 2-of-2 multisig regime, returning
/// `(admin, contract_id, client)`.
fn setup(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    let signers = vec![env, Address::generate(env), Address::generate(env)];
    client.multisig_approval_init(&admin, &signers, &2u32);
    (admin, contract_id, client)
}

/// Funded escrow with no multisig regime: `transfer_admin` is closed once
/// signers are registered (#786), so its target checks run without one.
fn setup_single_admin(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, client) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    (admin, contract_id, client)
}

fn admin_event_count(env: &Env) -> u32 {
    let topics: [Val; 2] = [
        symbol_short!("adminprp").into_val(env),
        symbol_short!("admin").into_val(env),
    ];
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topics
                .iter()
                .any(|t| t.get_payload() == topic.get_payload())
            {
                count += 1;
            }
        }
    }
    count
}

fn assert_admin_unchanged(env: &Env, contract_id: &Address, admin: &Address) {
    let (persistent, instance, pending) = env.as_contract(contract_id, || {
        let persistent: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let instance: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let pending = env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer);
        (persistent, instance, pending)
    });
    assert_eq!(&persistent, admin);
    assert_eq!(&instance, admin);
    assert!(!pending);
    assert_eq!(admin_event_count(env), 0);
}

// ── propose_admin_transfer ───────────────────────────────────────────────────

#[test]
fn propose_rejects_contract_address_as_new_admin() {
    let env = test_env();
    let (admin, contract_id, client) = setup(&env);

    assert_eq!(
        client.try_propose_admin_transfer(&admin, &contract_id),
        Err(Ok(Error::InvalidAddress))
    );
    assert_admin_unchanged(&env, &contract_id, &admin);
}

#[test]
fn propose_rejects_current_admin_as_new_admin() {
    let env = test_env();
    let (admin, contract_id, client) = setup(&env);

    assert_eq!(
        client.try_propose_admin_transfer(&admin, &admin),
        Err(Ok(Error::InvalidAddress))
    );
    assert_admin_unchanged(&env, &contract_id, &admin);
}

#[test]
fn propose_still_accepts_a_distinct_new_admin() {
    let env = test_env();
    let (admin, _, client) = setup(&env);

    let new_admin = Address::generate(&env);
    assert_eq!(
        client.try_propose_admin_transfer(&admin, &new_admin),
        Ok(Ok(1))
    );
    assert_eq!(
        client.get_pending_admin_transfer().unwrap().new_admin,
        new_admin
    );
}

// ── transfer_admin ───────────────────────────────────────────────────────────

#[test]
fn transfer_rejects_contract_address_as_new_admin() {
    let env = test_env();
    let (admin, contract_id, client) = setup_single_admin(&env);

    assert_eq!(
        client.try_transfer_admin(&admin, &contract_id),
        Err(Ok(Error::InvalidAddress))
    );
    assert_admin_unchanged(&env, &contract_id, &admin);
}

#[test]
fn transfer_rejects_current_admin_as_new_admin() {
    let env = test_env();
    let (admin, contract_id, client) = setup_single_admin(&env);

    assert_eq!(
        client.try_transfer_admin(&admin, &admin),
        Err(Ok(Error::InvalidAddress))
    );
    assert_admin_unchanged(&env, &contract_id, &admin);
}

#[test]
fn transfer_still_accepts_a_distinct_new_admin() {
    let env = test_env();
    let (admin, contract_id, client) = setup_single_admin(&env);

    let new_admin = Address::generate(&env);
    assert_eq!(client.try_transfer_admin(&admin, &new_admin), Ok(Ok(())));
    // Two-step (issue #953): unchanged until the new admin accepts.
    let stored_admin = || -> Address {
        env.as_contract(&contract_id, || {
            env.storage().instance().get(&DataKey::Admin).unwrap()
        })
    };
    assert_eq!(stored_admin(), admin);
    client.accept_admin(&new_admin);
    assert_eq!(stored_admin(), new_admin);
}

#[test]
fn transfer_is_closed_once_a_multisig_regime_exists() {
    let env = test_env();
    let (admin, contract_id, client) = setup(&env);

    let new_admin = Address::generate(&env);
    assert_eq!(
        client.try_transfer_admin(&admin, &new_admin),
        Err(Ok(Error::Unauthorized))
    );
    assert_admin_unchanged(&env, &contract_id, &admin);
}
