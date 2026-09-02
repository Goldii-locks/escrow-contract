#![cfg(test)]
use super::*;
use crate::{
    DataKey, EmergencyPauseAdminOverrideEvent, EmergencyPausedEvent, EmergencyUnpausedEvent, Error,
};
use soroban_sdk::testutils::Events as _;
use soroban_sdk::{symbol_short, vec, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

#[test]
fn test_emergency_pause_happy_path_and_event() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, _admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Initial state: not paused
    assert!(!client.is_emergency_paused());

    // Call pause
    client.emergency_pause(&client_addr, &freelancer_addr);

    // Verify event immediately
    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("empause"));
    let parsed_event = EmergencyPausedEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.client, client_addr);
    assert_eq!(parsed_event.freelancer, freelancer_addr);
    assert_eq!(parsed_event.contract_id, client.address);

    // Verify status
    assert!(client.is_emergency_paused());
}

#[test]
fn test_emergency_unpause_happy_path_and_event() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Pause first
    client.emergency_pause(&client_addr, &freelancer_addr);
    assert!(client.is_emergency_paused());

    // Call unpause
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

/// Number of `emoverrid` events currently visible in the test env.
fn emoverrid_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("emoverrid").into_val(env);
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

/// Parse the most recent event, asserting it is an `emoverrid` event.
fn last_emoverrid_event(env: &Env) -> EmergencyPauseAdminOverrideEvent {
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, symbol_short!("emoverrid"));
    EmergencyPauseAdminOverrideEvent::from_val(env, &last.2)
}

#[test]
fn test_emergency_pause_admin_override_happy_path_and_event() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_client_addr, _freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

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
    assert!(parsed_event.paused);
    assert!(!parsed_event.previous);

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
    assert!(!parsed_event2.paused);
    assert!(parsed_event2.previous);

    // Verify status
    assert!(!client.is_emergency_paused());
}

// ── issue #398: structured event reconciles with persisted state ────────────

#[test]
fn test_emergency_pause_admin_override_event_reconciles_with_persisted_state() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // false → true
    let before = client.is_emergency_paused();
    client.emergency_pause_admin_override(&admin_addr, &true);

    let ev = last_emoverrid_event(&env);
    assert_eq!(emoverrid_event_count(&env), 1);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.contract_id, client.address);
    // `paused` equals the flag the call actually persisted.
    assert_eq!(ev.paused, client.is_emergency_paused());
    assert!(ev.paused);
    // `previous` equals the flag in effect before the call.
    assert_eq!(ev.previous, before);
    // The call takes no re-entrancy lock at all (#397): it performs a single
    // write with no external call, so the guard key is never written.
    let lock_after: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::EpLk)
    });
    assert_eq!(lock_after, None);
    let paused_after: Option<bool> =
        env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::Ep));
    assert_eq!(paused_after, Some(ev.paused));

    // true → false
    let before2 = client.is_emergency_paused();
    client.emergency_pause_admin_override(&admin_addr, &false);

    let ev2 = last_emoverrid_event(&env);
    assert_eq!(ev2.paused, client.is_emergency_paused());
    assert!(!ev2.paused);
    assert_eq!(ev2.previous, before2);
    assert!(ev2.previous);
    let paused_after2: Option<bool> =
        env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::Ep));
    assert_eq!(paused_after2, Some(false));
}

#[test]
fn test_emergency_pause_admin_override_no_event_on_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let res = client.try_emergency_pause_admin_override(&client_addr, &true);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    assert_eq!(emoverrid_event_count(&env), 0);
}

#[test]
fn test_emergency_pause_admin_override_no_event_on_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Not paused → overriding to `false` is a no-op transition.
    let res = client.try_emergency_pause_admin_override(&admin_addr, &false);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));
    assert_eq!(emoverrid_event_count(&env), 0);

    // Paused → overriding to `true` is a no-op transition.
    client.emergency_pause(&client_addr, &freelancer_addr);
    let res = client.try_emergency_pause_admin_override(&admin_addr, &true);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));
    assert_eq!(emoverrid_event_count(&env), 0);
}

#[test]
fn test_emergency_pause_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Call pause as admin (should fail, missing signatures from client/freelancer)
    let res = client.try_emergency_pause(&admin_addr, &admin_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));

    // Single-signature attempt: passing only client as both or wrong freelancer
    let res = client.try_emergency_pause(&client_addr, &client_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));

    let res = client.try_emergency_pause(&freelancer_addr, &freelancer_addr);
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
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Initially not paused. Calling override to false should fail.
    let res = client.try_emergency_pause_admin_override(&admin_addr, &false);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));

    // Pause it
    client.emergency_pause(&client_addr, &freelancer_addr);

    // Already paused. Calling override to true should fail.
    let res = client.try_emergency_pause_admin_override(&admin_addr, &true);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));
}
