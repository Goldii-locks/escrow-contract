use super::*;
use crate::{DataKey, EmergencyPauseAdminOverrideEvent, Error};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Count events matching the given 8-char topic symbol.
fn event_count_for(env: &Env, topic: &Val) -> u32 {
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(t) = event.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                count += 1;
            }
        }
    }
    count
}
fn last_override_event(env: &Env) -> EmergencyPauseAdminOverrideEvent {
    let events = crate::all_event_tuples(env);
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, "emoverrid"));
    EmergencyPauseAdminOverrideEvent::from_val(env, &last.2)
}

fn read_pause_lock(env: &Env, contract: &MilestoneEscrowClient<'_>) -> bool {
    env.as_contract(&contract.address, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::Ep)
            .unwrap_or(false)
    })
}

#[test]
#[ignore = "storage reentrancy guard removed (#675)"]
fn override_false_to_true_sets_paused_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert!(
        !client.is_emergency_paused(),
        "contract must start unpaused"
    );

    client.emergency_pause_admin_override(&admin_addr);

    assert!(
        client.is_emergency_paused(),
        "flag must be set after override"
    );
    assert!(
        !read_pause_lock(&env, &client),
        "EmergencyPauseLock must remain false — override must not touch it"
    );
}

#[test]
#[ignore = "storage reentrancy guard removed (#675)"]
fn override_round_trip_is_correct() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.emergency_pause_admin_override(&admin_addr);
    assert!(client.is_emergency_paused());
    assert!(!read_pause_lock(&env, &client));

    client.emergency_unpause(&admin_addr);
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
        let result = escrow.try_emergency_pause_admin_override(&caller);
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
    escrow.emergency_pause_admin_override(&admin_addr);
    assert!(escrow.is_emergency_paused());
}

#[test]
#[ignore = "storage reentrancy guard removed (#675)"]
fn no_op_paused_returns_already_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);

    assert!(client.is_emergency_paused());
    let result = client.try_emergency_pause_admin_override(&admin_addr);
    assert_eq!(result, Err(Ok(Error::AlreadyPaused)));
    assert!(
        client.is_emergency_paused(),
        "state must not change on no-op rejection"
    );
    assert!(
        !read_pause_lock(&env, &client),
        "no-op rejection must not write EmergencyPauseLock"
    );
}

#[test]
#[ignore = "storage reentrancy guard removed (#675)"]
fn emergency_pause_lock_is_never_written_by_override() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Perform several back-to-back overrides; the lock must stay false.
    for _ in 0..4 {
        client.emergency_pause(&client_addr, &freelancer_addr);
        client.emergency_unpause(&admin_addr);
        client.emergency_pause_admin_override(&admin_addr);
        assert!(
            !read_pause_lock(&env, &client),
            "EmergencyPauseLock must be false after override"
        ); // The override always pauses (#726); unpause for the next round.
        client.emergency_unpause(&admin_addr);
    }
}

#[test]
fn override_does_not_leave_lock_that_blocks_emergency_pause() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Override to paused, then unpause via emergency_unpause — if the lock
    // leaked, emergency_unpause would return EmergencyPauseInProgress.
    client.emergency_pause_admin_override(&admin_addr);
    // emergency_unpause checks assert_emergency_pause_not_locked internally.
    client.emergency_unpause(&admin_addr); // must not panic or return error
    assert!(!client.is_emergency_paused());
}

#[test]
#[ignore = "storage reentrancy guard removed (#675)"]
fn override_works_after_admin_pause_escrow_cycle() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // admin_pause_escrow acquires and releases EmergencyPauseLock.
    client.admin_pause_escrow(&admin_addr);
    client.admin_resume_escrow(&admin_addr);

    // Lock must be released; override must proceed without EmergencyPauseInProgress.
    assert!(!read_pause_lock(&env, &client));
    client.emergency_pause_admin_override(&admin_addr);
    assert!(client.is_emergency_paused());
    assert!(!read_pause_lock(&env, &client));
}

#[test]
fn interleaved_sequence_produces_correct_final_state() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.emergency_pause_admin_override(&admin_addr); // override → paused
    assert!(client.is_emergency_paused());

    client.emergency_unpause(&admin_addr); // standard unpause
    assert!(!client.is_emergency_paused());

    client.emergency_pause(&client_addr, &freelancer_addr); // standard pause
    assert!(client.is_emergency_paused());

    client.emergency_unpause(&admin_addr); // standard unpause
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
    client.emergency_pause_admin_override(&admin_addr);
    assert_eq!(event_count_for(&env, &topic), 1);
    let ev = last_override_event(&env);
    assert_eq!(ev.admin, admin_addr);
    assert!(ev.paused);
}

#[test]
fn failed_calls_emit_no_emoverrid_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let topic: Val = symbol_short!("emoverrid").into_val(&env);

    // Unauthorized caller.
    let attacker = Address::generate(&env);
    let _ = client.try_emergency_pause_admin_override(&attacker);

    // No-op: pause first, then try paused → true.
    client.emergency_pause(&client_addr, &freelancer_addr);
    let _ = client.try_emergency_pause_admin_override(&admin_addr);

    assert_eq!(
        event_count_for(&env, &topic),
        0,
        "no emoverrid event must be emitted by any failed call"
    );
}

#[test]
fn not_initialized_returns_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(crate::MilestoneEscrow, ());
    let client = crate::MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    // Call without having initialized the contract with an admin key
    let result = client.try_emergency_pause_admin_override(&caller);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}
