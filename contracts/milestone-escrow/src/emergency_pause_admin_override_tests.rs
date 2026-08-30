
use super::*;
use crate::{DataKey, EmergencyPauseAdminOverrideEvent, Error};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Count events matching the given 8-char topic symbol.
fn event_count_for(env: &Env, topic: &Val) -> u32 {
    let mut count = 0u32;
    for event in env.events().all().iter() {
        if let Some(t) = event.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                count += 1;
            }
        }
    }
    count
}
fn last_override_event(env: &Env) -> EmergencyPauseAdminOverrideEvent {
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, "emoverrid"));
    EmergencyPauseAdminOverrideEvent::from_val(env, &last.2)
}

fn read_pause_lock(env: &Env, contract: &MilestoneEscrowClient<'_>) -> bool {
    env.as_contract(&contract.address, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::EmergencyPauseLock)
            .unwrap_or(false)
    })
}

#[test]
fn override_false_to_true_sets_paused_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert!(!client.is_emergency_paused(), "contract must start unpaused");

    client.emergency_pause_admin_override(&admin_addr, &true);

    assert!(client.is_emergency_paused(), "flag must be set after override");
    assert!(
        !read_pause_lock(&env, &client),
        "EmergencyPauseLock must remain false — override must not touch it"
    );
}

#[test]
fn override_true_to_false_clears_paused_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.emergency_pause(&admin_addr);

    assert!(client.is_emergency_paused(), "contract must be paused before override");

    client.emergency_pause_admin_override(&admin_addr, &false);

    assert!(!client.is_emergency_paused(), "flag must be cleared after override");
    assert!(
        !read_pause_lock(&env, &client),
        "EmergencyPauseLock must remain false — override must not touch it"
    );
}

#[test]
fn override_round_trip_is_correct() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.emergency_pause_admin_override(&admin_addr, &true);
    assert!(client.is_emergency_paused());
    assert!(!read_pause_lock(&env, &client));

    client.emergency_pause_admin_override(&admin_addr, &false);
    assert!(!client.is_emergency_paused());
    assert!(!read_pause_lock(&env, &client));
}

#[test]
fn unauthorized_caller_returns_unauthorized_and_no_state_change() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let attacker = Address::generate(&env);

    // Neither the client address nor an arbitrary attacker may override.
    for caller in [client_addr, attacker] {
        let before = escrow.is_emergency_paused();
        let result = escrow.try_emergency_pause_admin_override(&caller, &true);
        assert_eq!(result, Err(Ok(Error::Unauthorized)));
        assert_eq!(
            escrow.is_emergency_paused(),
            before,
            "unauthorized call must not change pause state"
        );
        assert!(
            !read_pause_lock(&env, &escrow),
            "unauthorized call must not write EmergencyPauseLock"
        );
    }

    // Confirm admin can still succeed — no lock was left dirty.
    escrow.emergency_pause_admin_override(&admin_addr, &true);
    assert!(escrow.is_emergency_paused());
}

#[test]
fn no_op_unpaused_to_false_returns_invalid_status() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert!(!client.is_emergency_paused());
    let result = client.try_emergency_pause_admin_override(&admin_addr, &false);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert!(!client.is_emergency_paused(), "state must not change on no-op rejection");
    assert!(
        !read_pause_lock(&env, &client),
        "no-op rejection must not write EmergencyPauseLock"
    );
}

#[test]
fn no_op_paused_to_true_returns_invalid_status() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.emergency_pause(&admin_addr);

    assert!(client.is_emergency_paused());
    let result = client.try_emergency_pause_admin_override(&admin_addr, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert!(client.is_emergency_paused(), "state must not change on no-op rejection");
    assert!(
        !read_pause_lock(&env, &client),
        "no-op rejection must not write EmergencyPauseLock"
    );
}

#[test]
fn emergency_pause_lock_is_never_written_by_override() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Perform several back-to-back overrides; the lock must stay false.
    for desired in [true, false, true, false] {
        client.emergency_pause_admin_override(&admin_addr, &desired);
        assert!(
            !read_pause_lock(&env, &client),
            "EmergencyPauseLock must be false after override to {desired}"
        );
    }
}

#[test]
fn override_does_not_leave_lock_that_blocks_emergency_pause() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Override to paused, then unpause via emergency_unpause — if the lock
    // leaked, emergency_unpause would return EmergencyPauseInProgress.
    client.emergency_pause_admin_override(&admin_addr, &true);
    // emergency_unpause checks assert_emergency_pause_not_locked internally.
    client.emergency_unpause(&admin_addr); // must not panic or return error
    assert!(!client.is_emergency_paused());
}

#[test]
fn emergency_pause_does_not_block_subsequent_override() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.emergency_pause(&admin_addr);
    assert!(client.is_emergency_paused());

    // Override to unpaused — must succeed regardless of what emergency_pause did.
    client.emergency_pause_admin_override(&admin_addr, &false);
    assert!(!client.is_emergency_paused());
}

#[test]
fn override_works_after_admin_pause_escrow_cycle() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // admin_pause_escrow acquires and releases EmergencyPauseLock.
    client.admin_pause_escrow(&admin_addr);
    client.admin_resume_escrow(&admin_addr);

    // Lock must be released; override must proceed without EmergencyPauseInProgress.
    assert!(!read_pause_lock(&env, &client));
    client.emergency_pause_admin_override(&admin_addr, &true);
    assert!(client.is_emergency_paused());
    assert!(!read_pause_lock(&env, &client));
}

#[test]
fn interleaved_sequence_produces_correct_final_state() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.emergency_pause_admin_override(&admin_addr, &true);   // override → paused
    assert!(client.is_emergency_paused());

    client.emergency_unpause(&admin_addr);                        // standard unpause
    assert!(!client.is_emergency_paused());

    client.emergency_pause(&admin_addr);                          // standard pause
    assert!(client.is_emergency_paused());

    client.emergency_pause_admin_override(&admin_addr, &false);  // override → unpaused
    assert!(!client.is_emergency_paused());

    // Lock must be clean throughout.
    assert!(!read_pause_lock(&env, &client));
}

#[test]
fn success_emits_exactly_one_emoverrid_event_with_correct_payload() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let topic: Val = symbol_short!("emoverrid").into_val(&env);

    // Override to paused.
    client.emergency_pause_admin_override(&admin_addr, &true);
    assert_eq!(event_count_for(&env, &topic), 1);
    let ev = last_override_event(&env);
    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.contract_id, client.address);
    assert_eq!(ev.paused, true);

    // Override back to unpaused: second event accumulates.
    client.emergency_pause_admin_override(&admin_addr, &false);
    assert_eq!(event_count_for(&env, &topic), 2);
    let ev2 = last_override_event(&env);
    assert_eq!(ev2.admin, admin_addr);
    assert_eq!(ev2.paused, false);
}

#[test]
fn failed_calls_emit_no_emoverrid_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let topic: Val = symbol_short!("emoverrid").into_val(&env);

    // Unauthorized caller.
    let attacker = Address::generate(&env);
    let _ = client.try_emergency_pause_admin_override(&attacker, &true);

    // No-op: unpaused → false.
    let _ = client.try_emergency_pause_admin_override(&admin_addr, &false);

    // No-op: pause first, then try paused → true.
    client.emergency_pause(&admin_addr);
    let _ = client.try_emergency_pause_admin_override(&admin_addr, &true);

    assert_eq!(
        event_count_for(&env, &topic),
        0,
        "no emoverrid event must be emitted by any failed call"
    );
}
