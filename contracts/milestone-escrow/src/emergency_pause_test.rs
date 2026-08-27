#![cfg(test)]
use super::*;
use crate::{
    DataKey, EmergencyPauseAdminOverrideEvent, EmergencyPausedEvent, EmergencyUnpausedEvent, Error,
};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::testutils::Events as _;
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

#[test]
fn test_emergency_pause_happy_path_and_event() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Initial state: not paused
    assert!(!client.is_emergency_paused());

    // Call pause
    client.emergency_pause(&admin_addr);

    // Verify event immediately
    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("empause"));
    let parsed_event = EmergencyPausedEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.admin, admin_addr);
    assert_eq!(parsed_event.contract_id, client.address);

    // Verify status
    assert!(client.is_emergency_paused());
}

#[test]
fn test_emergency_unpause_happy_path_and_event() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Pause first
    client.emergency_pause(&admin_addr);

    // Unpause
    client.emergency_unpause(&admin_addr);

    // Verify event immediately
    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("emunpause"));
    let parsed_event = EmergencyUnpausedEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.admin, admin_addr);
    assert_eq!(parsed_event.contract_id, client.address);

    // Verify status
    assert!(!client.is_emergency_paused());
}

#[test]
fn test_emergency_pause_admin_override_happy_path_and_event() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Override to paused (true)
    client.emergency_pause_admin_override(&admin_addr, &true);

    // Verify event immediately
    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("emoverrid"));
    let parsed_event = EmergencyPauseAdminOverrideEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.admin, admin_addr);
    assert_eq!(parsed_event.contract_id, client.address);
    assert_eq!(parsed_event.paused, true);

    // Verify status
    assert!(client.is_emergency_paused());

    // Override to unpaused (false)
    client.emergency_pause_admin_override(&admin_addr, &false);

    // Verify event immediately
    let events2 = env.events().all();
    let last_event2 = events2.last().unwrap();
    let topic2: Symbol = last_event2.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic2, symbol_short!("emoverrid"));
    let parsed_event2 = EmergencyPauseAdminOverrideEvent::from_val(&env, &last_event2.2);
    assert_eq!(parsed_event2.admin, admin_addr);
    assert_eq!(parsed_event2.contract_id, client.address);
    assert_eq!(parsed_event2.paused, false);

    // Verify status
    assert!(!client.is_emergency_paused());
}

#[test]
fn test_emergency_pause_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Call pause as client (should fail)
    let res = client.try_emergency_pause(&client_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_emergency_unpause_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Call unpause as client (should fail)
    let res = client.try_emergency_unpause(&client_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_emergency_pause_admin_override_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Call override as client (should fail)
    let res = client.try_emergency_pause_admin_override(&client_addr, &true);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_emergency_pause_admin_override_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Initially not paused. Calling override to false should fail.
    let res = client.try_emergency_pause_admin_override(&admin_addr, &false);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));

    // Pause it
    client.emergency_pause(&admin_addr);

    // Already paused. Calling override to true should fail.
    let res = client.try_emergency_pause_admin_override(&admin_addr, &true);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));
}
