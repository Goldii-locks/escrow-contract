#![cfg(test)]
use super::*;
use crate::{CancelEscrowInitiatedEvent, DataKey, Error};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::testutils::Events as _;
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

#[test]
fn test_cancel_escrow_sets_lock_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Initial state: not cancel-locked
    let is_locked_before = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false)
    });
    assert!(!is_locked_before);

    // Call cancel_escrow by client
    client.cancel_escrow(&client_addr);

    // Verify event immediately
    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("cancel"));
    let parsed_event = CancelEscrowInitiatedEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.caller, client_addr);
    assert_eq!(parsed_event.contract_id, client.address);

    // Verify CancelLock is set to true
    let is_locked_after = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false)
    });
    assert!(is_locked_after);
}

#[test]
fn test_cancel_escrow_by_freelancer_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, freelancer_addr, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Call cancel_escrow by freelancer
    client.cancel_escrow(&freelancer_addr);

    // Verify CancelLock is set to true
    let is_locked = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false)
    });
    assert!(is_locked);
}

#[test]
fn test_cancel_escrow_invalid_address() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let zero_contract = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );

    // Call cancel_escrow with zero account
    let res = client.try_cancel_escrow(&zero_account);
    assert_eq!(res, Err(Ok(Error::InvalidAddress)));

    // Call cancel_escrow with zero contract
    let res2 = client.try_cancel_escrow(&zero_contract);
    assert_eq!(res2, Err(Ok(Error::InvalidAddress)));
}

#[test]
fn test_cancel_escrow_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, arbiter_addr, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Call cancel_escrow by arbiter (should fail)
    let res = client.try_cancel_escrow(&arbiter_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));

    // Call cancel_escrow by random address (should fail)
    let random_addr = Address::generate(&env);
    let res2 = client.try_cancel_escrow(&random_addr);
    assert_eq!(res2, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_cancel_escrow_not_funded() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let milestone_amounts = vec![&env, 1000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &milestone_amounts,
    );

    // Escrow not funded. Call cancel_escrow by client (should fail).
    let res = client.try_cancel_escrow(&client_addr);
    assert_eq!(res, Err(Ok(Error::NotFunded)));
}
