#![cfg(test)]
//! Tests for `multisig_lock` authorization and precondition guards (#458)

use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{testutils::Address as _, Address, Env};

fn setup(env: &Env) -> (Address, MilestoneEscrowClient<'_>, Address) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize(&admin, &client, &freelancer, &arbiter, &token, &604800u64, &soroban_sdk::vec![env, 1_000_i128]);
    (contract_id, escrow, admin)
}

#[test]
fn unauthorized_caller_returns_unauthorized_and_does_not_mutate() {
    let env = Env::default();
    let (contract_id, escrow, _admin) = setup(&env);
    let attacker = Address::generate(&env);
    let before: bool = env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::MultisigLocked).unwrap_or(false));
    assert!(!before);
    let res = escrow.try_multisig_lock(&attacker);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    let after: bool = env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::MultisigLocked).unwrap_or(false));
    assert_eq!(before, after, "no storage mutated on unauthorized");
    assert!(!after);
}

#[test]
fn already_locked_returns_invalid_status_and_does_not_mutate() {
    let env = Env::default();
    let (contract_id, escrow, admin) = setup(&env);
    // first lock succeeds
    let res1 = escrow.try_multisig_lock(&admin);
    assert_eq!(res1, Ok(Ok(())));
    let before: bool = env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::MultisigLocked).unwrap_or(false));
    assert!(before, "should be locked after first call");
    // second lock should fail with InvalidStatus and not mutate
    let res2 = escrow.try_multisig_lock(&admin);
    assert_eq!(res2, Err(Ok(Error::InvalidStatus)));
    let after: bool = env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::MultisigLocked).unwrap_or(false));
    assert_eq!(before, after);
    assert!(after);
}

#[test]
fn valid_lock_succeeds() {
    let env = Env::default();
    let (contract_id, escrow, admin) = setup(&env);
    let res = escrow.try_multisig_lock(&admin);
    assert_eq!(res, Ok(Ok(())));
    let locked: bool = env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::MultisigLocked).unwrap_or(false));
    assert!(locked);
}

#[test]
fn uninitialized_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let attacker = Address::generate(&env);
    let res = escrow.try_multisig_lock(&attacker);
    // NotInitialized takes precedence after auth? require_admin_from_instance returns NotInitialized if admin key missing
    // But attacker not being admin would be Unauthorized if contract were initialized.
    // For uninitialized, we expect NotInitialized regardless of caller
    assert_eq!(res, Err(Ok(Error::NotInitialized)));
}

#[test]
fn emergency_pause_in_progress_returns_error_and_does_not_mutate() {
    let env = Env::default();
    let (contract_id, escrow, admin) = setup(&env);
    // Simulate emergency-pause transition in progress (EpLk guard).
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::EpLk, &true);
    });
    let before: bool = env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::MultisigLocked).unwrap_or(false));
    assert!(!before);
    let res = escrow.try_multisig_lock(&admin);
    assert_eq!(res, Err(Ok(Error::EmergencyPauseInProgress)));
    let after: bool = env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::MultisigLocked).unwrap_or(false));
    assert_eq!(before, after, "no storage mutated on emergency-pause guard");
    assert!(!after);
}
