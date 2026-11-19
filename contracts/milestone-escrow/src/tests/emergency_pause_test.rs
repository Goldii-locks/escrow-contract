#![cfg(test)]
use super::*;
use crate::{
    DataKey, EmergencyPauseAdminOverrideEvent, EmergencyPausedEvent, EmergencyUnpausedEvent, Error,
};
use soroban_sdk::{
    symbol_short, testutils::EnvTestConfig, vec, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val,
};

fn env_without_snapshot() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

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
    let events = crate::all_event_tuples(&env);
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("empause"));
    let parsed_event = EmergencyPausedEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.client, client_addr);
    assert_eq!(parsed_event.freelancer, freelancer_addr);

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
    let events = crate::all_event_tuples(&env);
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("emunpause"));
    let parsed_event = EmergencyUnpausedEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.admin, admin_addr);

    // Verify status
    assert!(!client.is_emergency_paused());
}

/// Number of `emoverrid` events currently visible in the test env.
fn emoverrid_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("emoverrid").into_val(env);
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

/// Parse the most recent event, asserting it is an `emoverrid` event.
fn last_emoverrid_event(env: &Env) -> EmergencyPauseAdminOverrideEvent {
    let events = crate::all_event_tuples(env);
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

    // Override to paused
    client.emergency_pause_admin_override(&admin_addr);

    // Verify event immediately
    let events = crate::all_event_tuples(&env);
    let last_event = events.last().unwrap();
    let topic: Symbol = last_event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("emoverrid"));
    let parsed_event = EmergencyPauseAdminOverrideEvent::from_val(&env, &last_event.2);
    assert_eq!(parsed_event.admin, admin_addr);
    assert!(parsed_event.paused);
    assert!(!parsed_event.previous);

    // Verify status
    assert!(client.is_emergency_paused());
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
    client.emergency_pause_admin_override(&admin_addr);

    let ev = last_emoverrid_event(&env);
    assert_eq!(emoverrid_event_count(&env), 1);
    assert_eq!(ev.admin, admin_addr);
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
    let paused_after: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::Paused)
    });
    assert_eq!(paused_after, Some(ev.paused));
}

#[test]
fn test_emergency_pause_admin_override_no_event_on_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let res = client.try_emergency_pause_admin_override(&client_addr);
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

    // Paused → overriding to `true` is a no-op transition.
    client.emergency_pause(&client_addr, &freelancer_addr);
    let res = client.try_emergency_pause_admin_override(&admin_addr);
    assert_eq!(res, Err(Ok(Error::AlreadyPaused)));
    assert_eq!(emoverrid_event_count(&env), 0);
}

#[test]
fn test_emergency_pause_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);
    let before = env.to_ledger_snapshot();

    // Call pause as admin (should fail, missing signatures from client/freelancer)
    let res = client.try_emergency_pause(&admin_addr, &admin_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));

    // Single-signature attempt: passing only client as both or wrong freelancer
    let res = client.try_emergency_pause(&client_addr, &client_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));

    let res = client.try_emergency_pause(&freelancer_addr, &freelancer_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    assert_eq!(env.to_ledger_snapshot(), before);
}

#[test]
fn test_emergency_pause_rejects_a_held_transition_lock_without_mutation() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let amounts = vec![&env, 1_000_i128];
    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, amounts);
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::EpLk, &true);
    });
    let before = env.to_ledger_snapshot();

    assert_eq!(
        client.try_emergency_pause(&client_addr, &freelancer_addr),
        Err(Ok(Error::EmergencyPauseInProgress))
    );
    assert_eq!(env.to_ledger_snapshot(), before);
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
fn test_emergency_unpause_rejects_a_held_transition_lock_without_mutation() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let amounts = vec![&env, 1_000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, amounts);
    client.emergency_pause(&client_addr, &freelancer_addr);
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::EpLk, &true);
    });
    let before = env.to_ledger_snapshot();

    assert_eq!(
        client.try_emergency_unpause(&admin_addr),
        Err(Ok(Error::EmergencyPauseInProgress))
    );
    assert_eq!(env.to_ledger_snapshot(), before);
}

#[test]
fn test_emergency_pause_admin_override_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Call override as client (should fail)
    let res = client.try_emergency_pause_admin_override(&client_addr);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_emergency_pause_admin_override_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Pause it
    client.emergency_pause(&client_addr, &freelancer_addr);

    // Already paused. Calling override should fail with AlreadyPaused.
    let res = client.try_emergency_pause_admin_override(&admin_addr);
    assert_eq!(res, Err(Ok(Error::AlreadyPaused)));
}

// ── #451: emergency_unpause storage-footprint tests ─────────────────────────

/// A successful emergency_unpause must write exactly one instance key (Ep) and
/// must never write EpLk — the reentrancy guard is not needed because there is
/// no external call inside the transition body.
#[test]
fn test_emergency_unpause_does_not_write_eplk_on_success() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    client.emergency_pause(&client_addr, &freelancer_addr);
    assert!(client.is_emergency_paused());

    // Capture EpLk state before unpause — pause leaves it false.
    let lock_before: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::EpLk)
    });

    client.emergency_unpause(&admin_addr);

    // EpLk must be exactly the same value after the call as before.
    let lock_after: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::EpLk)
    });
    assert_eq!(
        lock_after, lock_before,
        "emergency_unpause must not write EpLk"
    );

    // Ep must be false — the single write that emergency_unpause does perform.
    let ep_after: Option<bool> =
        env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::Ep));
    assert_eq!(ep_after, Some(false));
}

/// A failed emergency_unpause (NotPaused) must not write any storage key —
/// neither EpLk nor Ep.
#[test]
fn test_emergency_unpause_does_not_write_eplk_on_failure() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Contract is not paused: unpause must return NotPaused.
    let lock_before: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::EpLk)
    });
    let ep_before: Option<bool> =
        env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::Ep));

    let res = client.try_emergency_unpause(&admin_addr);
    assert_eq!(res, Err(Ok(Error::NotPaused)));

    let lock_after: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::EpLk)
    });
    let ep_after: Option<bool> =
        env.as_contract(&contract_id, || env.storage().instance().get(&DataKey::Ep));

    assert_eq!(
        lock_after, lock_before,
        "EpLk must not be written on failure"
    );
    assert_eq!(ep_after, ep_before, "Ep must not be written on failure");
}

/// After emergency_pause acquires and releases EpLk, emergency_unpause must
/// proceed without acquiring EpLk itself, leaving the lock clean for the next
/// pause call.
#[test]
fn test_emergency_unpause_leaves_lock_clean_for_subsequent_pause() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    client.emergency_pause(&client_addr, &freelancer_addr);
    client.emergency_unpause(&admin_addr);

    // EpLk must be false/absent so the next pause can acquire it.
    let lock: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::EpLk)
    });
    assert!(
        !lock.unwrap_or(false),
        "EpLk must not be held after emergency_unpause"
    );

    // The next pause must succeed (not return EmergencyPauseInProgress).
    client.emergency_pause(&client_addr, &freelancer_addr);
    assert!(client.is_emergency_paused());
}

#[test]
fn joint_unpause() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);
    client.emergency_unpause_parties(&client_addr, &freelancer_addr);
    let events = crate::all_event_tuples(&env);
    assert!(
        events.iter().any(|event| {
            event
                .1
                .get(0)
                .map(|topic| {
                    let t: Symbol = topic.try_into_val(&env).unwrap();
                    t == symbol_short!("emunpausp")
                })
                .unwrap_or(false)
        }),
        "expected emunpausp event"
    );
    assert!(!client.is_emergency_paused());
}

#[test]
fn single_party_rejection() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);
    assert_eq!(
        client.try_emergency_unpause_parties(&client_addr, &client_addr),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        client.try_emergency_unpause_parties(&freelancer_addr, &freelancer_addr),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        client.try_emergency_unpause_parties(&arbiter_addr, &freelancer_addr),
        Err(Ok(Error::Unauthorized))
    );
    assert!(client.is_emergency_paused());
}

#[test]
fn admin_recovery_still_works() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);
    client.emergency_unpause(&admin_addr);
    assert!(!client.is_emergency_paused());
}

#[test]
fn pause_state_isolation() {
    let env = Env::default();
    env.mock_all_auths();
    let (c1, f1, _, _, _, _, e1) = setup_funded_escrow(&env, vec![&env, 1000_i128]);
    let (c2, f2, _, _, _, _, e2) = setup_funded_escrow(&env, vec![&env, 1000_i128]);
    e1.emergency_pause(&c1, &f1);
    e2.emergency_pause(&c2, &f2);
    e1.emergency_unpause_parties(&c1, &f1);
    assert!(!e1.is_emergency_paused());
    assert!(e2.is_emergency_paused());
}

#[test]
fn parties_cannot_lift_admin_pause() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);
    client.emergency_pause_admin_override(&admin_addr);
    assert_eq!(
        client.try_emergency_unpause_parties(&client_addr, &freelancer_addr),
        Err(Ok(Error::Unauthorized))
    );
    assert!(client.is_emergency_paused());
}

#[test]
fn unpause_when_not_paused() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);
    assert_eq!(
        client.try_emergency_unpause_parties(&client_addr, &freelancer_addr),
        Err(Ok(Error::NotPaused))
    );
}
