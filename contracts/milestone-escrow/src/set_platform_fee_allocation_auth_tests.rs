#![cfg(test)]
//! Tests for `set_platform_fee_allocation` authorization and precondition guards (#470)
//!
//! Validates that unauthorized caller and illegal source state (locked allocation)
//! each return their specific typed error and that no storage entry is mutated.

use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient, PlatformFeeAllocation};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

fn setup(env: &Env) -> (Address, MilestoneEscrowClient<''_>, Address) {
    env.mock_all_auths();
    let amounts = vec![env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup_funded_escrow(env, amounts);
    (contract_id, escrow, admin_addr)
}

fn read_allocation(env: &Env, contract_id: &Address) -> PlatformFeeAllocation {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .unwrap()
    })
}

#[test]
fn unauthorized_caller_returns_unauthorized_and_does_not_mutate_storage() {
    let env = Env::default();
    let (contract_id, escrow, admin_addr) = setup(&env);
    // ensure initial allocation is set to something known
    escrow.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    let before = read_allocation(&env, &contract_id);
    let attacker = Address::generate(&env);
    let res = escrow.try_set_platform_fee_allocation(&attacker, &1000_u32, &8000_u32, &1000_u32);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    let after = read_allocation(&env, &contract_id);
    assert_eq!(before, after, "storage must not be mutated on unauthorized caller");
}

#[test]
fn locked_allocation_returns_invalid_status_and_does_not_mutate_storage() {
    let env = Env::default();
    let (contract_id, escrow, admin_addr) = setup(&env);
    escrow.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    escrow.lock_platform_fee_allocation(&admin_addr);
    let before = read_allocation(&env, &contract_id);
    assert!(before.locked, "precondition: allocation should be locked");
    let res = escrow.try_set_platform_fee_allocation(&admin_addr, &1000_u32, &8000_u32, &1000_u32);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));
    let after = read_allocation(&env, &contract_id);
    assert_eq!(before, after, "storage must not be mutated on illegal state");
    // also ensure no new allocation was written and lock still held
    assert!(after.locked);
}

#[test]
fn valid_call_succeeds_and_updates_allocation() {
    let env = Env::default();
    let (contract_id, escrow, admin_addr) = setup(&env);
    let res = escrow.try_set_platform_fee_allocation(&admin_addr, &1500_u32, &7000_u32, &1500_u32);
    assert_eq!(res, Ok(Ok(())));
    let after = read_allocation(&env, &contract_id);
    assert_eq!(after.client_bps, 1500);
    assert_eq!(after.freelancer_bps, 7000);
    assert_eq!(after.treasury_bps, 1500);
    assert!(!after.locked);
}

#[test]
fn invalid_ratio_returns_invalid_ratio_and_does_not_mutate_locked_check() {
    let env = Env::default();
    let (contract_id, escrow, admin_addr) = setup(&env);
    escrow.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    let before = read_allocation(&env, &contract_id);
    // sum != 10000
    let res = escrow.try_set_platform_fee_allocation(&admin_addr, &1000_u32, &1000_u32, &1000_u32);
    assert_eq!(res, Err(Ok(Error::InvalidRatio)));
    let after = read_allocation(&env, &contract_id);
    assert_eq!(before, after);
}