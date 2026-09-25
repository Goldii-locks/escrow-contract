#![cfg(test)]
use super::*;
use soroban_sdk::{
    symbol_short, testutils::MockAuth, testutils::MockAuthInvoke, token, vec, Address, Env,
    FromVal, IntoVal, Symbol, TryIntoVal, Val,
};

#[test]
fn positive_times_preserve_amount_across_boundaries() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    for (elapsed, total, expected_freelancer) in [
        (0_i128, 10_i128, 0_i128),
        (1, 3, 34),
        (1, 2, 51),
        (10, 10, 101),
    ] {
        let split = client.milestone_time_extensions(&101, &elapsed, &total);
        assert_eq!(split.first, expected_freelancer);
        assert_eq!(split.first + split.second, 101);
    }
}

#[test]
fn invalid_amount_and_time_inputs_return_specific_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    for amount in [0_i128, -1_i128] {
        assert_eq!(
            client.try_milestone_time_extensions(&amount, &1, &2),
            Err(Ok(Error::InvalidAmount))
        );
    }
    for (elapsed, total) in [(0_i128, 0_i128), (-1, 10), (11, 10), (1, -10)] {
        assert_eq!(
            client.try_milestone_time_extensions(&1, &elapsed, &total),
            Err(Ok(Error::InvalidRatio))
        );
    }
}

#[test]
fn arithmetic_overflow_is_reported_without_panicking() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_milestone_time_extensions(&i128::MAX, &i128::MAX, &i128::MAX),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn successful_split_emits_complete_event_payload() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let split = client.milestone_time_extensions(&101, &1, &2);
    let all_events = crate::all_event_tuples(&env);
    let event = all_events.last().unwrap();
    let topic: Symbol = event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, Symbol::new(&env, "m_ext"));
    assert_eq!(
        MilestoneTimeExtensionEvent::from_val(&env, &event.2),
        MilestoneTimeExtensionEvent {
            amount: 101,
            elapsed_seconds: 1,
            total_seconds: 2,
            freelancer_share: split.first,
            client_refund: split.second,
        }
    );
}

#[test]
fn deadline_extensions_accumulate_and_emit_new_total() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);
    escrow.mark_delivered(&freelancer_addr, &0);
    let initial = escrow.time_until_auto_release(&0);

    escrow.extend_milestone_deadline(&client_addr, &0, &100);
    escrow.extend_milestone_deadline(&client_addr, &0, &250);

    assert_eq!(escrow.time_until_auto_release(&0), initial + 350);
}

#[test]
fn deadline_extension_accepts_partially_released_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.approve_partial(&client_addr, &0, &1_000);

    assert!(escrow
        .try_extend_milestone_deadline(&client_addr, &0, &1)
        .is_ok());
}

#[test]
fn deadline_extension_rejects_invalid_index_and_terminal_status() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    assert_eq!(
        escrow.try_extend_milestone_deadline(&client_addr, &1, &1),
        Err(Ok(Error::InvalidMilestone))
    );
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.approve_milestone(&client_addr, &0);
    assert_eq!(
        escrow.try_extend_milestone_deadline(&client_addr, &0, &1),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn deadline_extension_overflow_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);
    escrow.mark_delivered(&freelancer_addr, &0);

    escrow.extend_milestone_deadline(&client_addr, &0, &u32::MAX);
    assert_eq!(
        escrow.try_extend_milestone_deadline(&client_addr, &0, &1),
        Err(Ok(Error::InvalidExtension))
    );
}

// ── #256: execution lock blocks concurrent modifications ─────────────────────

#[test]
fn test_milestone_time_extensions_lock_blocks_fund() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::TimeExtExecutionLock, &true);
    });

    assert_eq!(
        client.try_fund(&client_addr),
        Err(Ok(Error::TimeExtInProgress))
    );

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .remove(&DataKey::TimeExtExecutionLock);
    });
}

#[test]
fn test_milestone_time_extensions_lock_blocks_mark_delivered() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (_, freelancer_addr, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::TimeExtExecutionLock, &true);
    });

    assert_eq!(
        escrow.try_mark_delivered(&freelancer_addr, &0),
        Err(Ok(Error::TimeExtInProgress))
    );

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .remove(&DataKey::TimeExtExecutionLock);
    });
}

#[test]
fn test_milestone_time_extensions_lock_blocks_extend_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, amounts);
    escrow.mark_delivered(&freelancer_addr, &0);

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::TimeExtExecutionLock, &true);
    });

    assert_eq!(
        escrow.try_extend_milestone_deadline(&client_addr, &0, &100),
        Err(Ok(Error::TimeExtInProgress))
    );

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .remove(&DataKey::TimeExtExecutionLock);
    });
}

#[test]
fn test_milestone_time_extensions_releases_lock_after_success() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let _ = client.milestone_time_extensions(&1_000, &1, &2);

    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held, "execution lock must be cleared after success");
}

#[test]
fn test_milestone_time_extensions_releases_lock_after_failure() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let _ = client.try_milestone_time_extensions(&0, &1, &2);

    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held, "execution lock must be cleared after failure");
}

// ── #274: multi-party authentication ─────────────────────────────────────────

macro_rules! time_ext_consent_invoke {
    ($env:expr, $contract_id:expr) => {
        MockAuthInvoke {
            contract: $contract_id,
            fn_name: "time_extensions_consent",
            args: (&1_000_i128, &1_i128, &2_i128).into_val($env),
            sub_invokes: &[],
        }
    };
}

#[test]
fn test_time_extensions_consent_succeeds_when_both_parties_sign() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (client_addr, freelancer_addr, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, amounts);

    let invoke = time_ext_consent_invoke!(&env, &contract_id);
    let split = escrow
        .mock_auths(&[
            MockAuth {
                address: &client_addr,
                invoke: &invoke,
            },
            MockAuth {
                address: &freelancer_addr,
                invoke: &invoke,
            },
        ])
        .time_extensions_consent(&1_000, &1, &2);

    assert_eq!(split.first + split.second, 1_000);
}

#[test]
fn test_time_extensions_consent_reverts_when_only_client_signs() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (client_addr, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    let invoke = time_ext_consent_invoke!(&env, &contract_id);
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &client_addr,
            invoke: &invoke,
        }])
        .try_time_extensions_consent(&1_000, &1, &2);

    assert!(matches!(result, Err(Err(_))));
}

#[test]
fn test_time_extensions_consent_reverts_when_only_freelancer_signs() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, freelancer_addr, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    let invoke = time_ext_consent_invoke!(&env, &contract_id);
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &freelancer_addr,
            invoke: &invoke,
        }])
        .try_time_extensions_consent(&1_000, &1, &2);

    assert!(matches!(result, Err(Err(_))));
}

#[test]
fn test_time_extensions_consent_reverts_with_no_signatures() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    // Clear mock_all_auths behaviour by using an empty auth set.
    let result = escrow
        .mock_auths(&[])
        .try_time_extensions_consent(&1_000, &1, &2);
    assert!(matches!(result, Err(Err(_))));
}

#[test]
fn test_time_extensions_consent_emits_event_naming_both_signers() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    let split = escrow.time_extensions_consent(&1_000, &1, &2);

    let topic: Val = symbol_short!("m_extcns").into_val(&env);
    let mut found = false;
    for e in crate::all_event_tuples(&env).iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                found = true;
                let data = TimeExtConsentEvent::from_val(&env, &e.2);
                assert_eq!(data.client, client_addr);
                assert_eq!(data.freelancer, freelancer_addr);
                assert_eq!(data.freelancer_share, split.first);
                assert_eq!(data.client_refund, split.second);
            }
        }
    }
    assert!(found, "expected a m_extcns event naming both signers");
}

#[test]
fn test_time_extensions_consent_matches_unauthenticated_calculator() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    let calc = escrow.milestone_time_extensions(&1_000, &3, &7);
    let gated = escrow.time_extensions_consent(&1_000, &3, &7);
    assert_eq!(calc.first, gated.first);
    assert_eq!(calc.second, gated.second);
}

// ── #581: checked arithmetic in time_until_auto_release ──────────────────────

/// `delivered_at + auto_release_seconds` overflows u64: the first checked_add
/// inside `time_until_auto_release` must return `InvalidAmount` rather than
/// wrapping or panicking.
#[test]
fn time_until_auto_release_deadline_overflow_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 5_000_i128];
    let (_, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    // Place the ledger at u64::MAX so delivered_at = u64::MAX; any positive
    // auto_release_seconds will overflow the first checked_add.
    env.ledger().with_mut(|li| {
        li.timestamp = u64::MAX;
    });
    escrow.mark_delivered(&freelancer_addr, &0);

    assert_eq!(
        escrow.try_time_until_auto_release(&0),
        Err(Ok(Error::InvalidAmount))
    );
}

/// `deadline - current` overflows i64: when `deadline as i64` wraps to
/// `i64::MIN` the checked_sub must return `InvalidAmount` rather than wrapping.
///
/// `deadline as i64 == i64::MIN` when `deadline == 2^63`.  We arrange that by
/// setting `delivered_at = 2^63 - auto_release_seconds` so that
/// `delivered_at + auto_release_seconds = 2^63 exactly`, then set current = 1
/// so `i64::MIN.checked_sub(1)` overflows.
#[test]
fn time_until_auto_release_subtraction_overflow_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    // setup_funded_escrow uses auto_release_seconds = 604_800.
    // Set delivered_at = 2^63 - 604_800 so deadline = 2^63 exactly.
    // 2^63 as i64 = i64::MIN; i64::MIN.checked_sub(1) overflows.
    let amounts = vec![&env, 5_000_i128];
    let (_, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    const AUTO_RELEASE: u64 = 604_800;
    let delivered_at: u64 = (i64::MIN as u64).wrapping_sub(AUTO_RELEASE); // 2^63 - 604_800
    env.ledger().with_mut(|li| {
        li.timestamp = delivered_at;
    });
    escrow.mark_delivered(&freelancer_addr, &0);

    // current = 1; deadline as i64 = i64::MIN; i64::MIN - 1 overflows.
    env.ledger().with_mut(|li| {
        li.timestamp = 1;
    });

    assert_eq!(
        escrow.try_time_until_auto_release(&0),
        Err(Ok(Error::InvalidAmount))
    );
}

/// Valid inputs: `try_time_until_auto_release` returns `Ok(expected)`,
/// identical to the result produced by the non-try variant before the change.
/// Confirms the checked rewrite is a no-op for all non-overflowing inputs.
#[test]
fn time_until_auto_release_valid_inputs_match_plain_arithmetic() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 5_000_i128];
    let (_, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    // Ledger starts at 0; delivered_at = 0; auto_release_seconds = 604_800.
    // deadline = 0 + 604_800 = 604_800; current = 0; result = 604_800.
    escrow.mark_delivered(&freelancer_addr, &0);

    assert_eq!(escrow.try_time_until_auto_release(&0), Ok(Ok(604_800_i64)));

    // Advance by 100 seconds; result must be 604_700.
    env.ledger().with_mut(|li| {
        li.timestamp += 100;
    });
    assert_eq!(escrow.try_time_until_auto_release(&0), Ok(Ok(604_700_i64)));

    // Advance past the deadline; result is negative (time already elapsed).
    env.ledger().with_mut(|li| {
        li.timestamp += 604_800;
    });
    assert_eq!(escrow.try_time_until_auto_release(&0), Ok(Ok(-100_i64)));
}

// ── #553: typed error when uninitialized ─────────────────────────────────

#[test]
fn time_extensions_consent_on_uninitialized_returns_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // No initialize call — Job metadata is absent.
    assert_eq!(
        client.try_time_extensions_consent(&1_000, &1, &2),
        Err(Ok(Error::NotInitialized))
    );

    // No storage entry must have been mutated; the execution lock must not be set.
    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held, "uninitialized call must not set execution lock");
    let has_job: bool =
        env.as_contract(&contract_id, || env.storage().instance().has(&DataKey::Job));
    assert!(!has_job, "uninitialized call must not create Job");
}

// ── #550: harden auth & precondition guards ────────────────────────────────

#[test]
fn time_extensions_consent_illegal_source_state_no_mutation_when_locked() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    // Simulate an in-progress execution by manually setting the lock.
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::TimeExtExecutionLock, &true);
    });

    // Must be rejected with the specific illegal-source-state error before any
    // further ledger access, and must not mutate storage (lock stays set, no
    // event, no partial write).
    assert_eq!(
        escrow.try_time_extensions_consent(&1_000, &1, &2),
        Err(Ok(Error::TimeExtInProgress))
    );

    let still_locked: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(still_locked, "lock must remain set after rejected call");

    // No event for the rejected call.
    let topic: Val = symbol_short!("m_extcns").into_val(&env);
    let count_before = crate::all_event_tuples(&env)
        .iter()
        .filter(|e| {
            e.1.get(0)
                .map(|t| t.get_payload() == topic.get_payload())
                .unwrap_or(false)
        })
        .count();
    assert_eq!(count_before, 0, "rejected call must not emit m_extcns");

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .remove(&DataKey::TimeExtExecutionLock);
    });
}

#[test]
fn time_extensions_consent_unauthorized_no_mutation_single_signature() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (client_addr, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    let invoke = time_ext_consent_invoke!(&env, &contract_id);
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &client_addr,
            invoke: &invoke,
        }])
        .try_time_extensions_consent(&1_000, &1, &2);

    // Missing freelancer signature panics at host level (Err(Err(_))), not a
    // contract Error, and must not mutate storage.
    assert!(
        matches!(result, Err(Err(_))),
        "single signature must revert at host level"
    );

    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held, "unauthorized call must not set execution lock");
}

#[test]
fn time_extensions_consent_illegal_source_state_no_mutation_when_paused() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);
    escrow.admin_pause_escrow(&admin_addr);

    assert_eq!(
        escrow.try_time_extensions_consent(&1_000, &1, &2),
        Err(Ok(Error::Paused))
    );

    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held, "paused call must not set execution lock");
    // Unpause for cleanup.
    escrow.admin_resume_escrow(&admin_addr);
}

// ── #551: checked arithmetic ───────────────────────────────────────────────

#[test]
fn time_extensions_consent_overflow_i128_max_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    // amount * elapsed_seconds overflows i128 inside split_round_nearest.
    assert_eq!(
        escrow.try_time_extensions_consent(&i128::MAX, &i128::MAX, &i128::MAX),
        Err(Ok(Error::InvalidAmount))
    );

    // No partial write must survive the failure.
    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held, "overflow must not leave lock set");
}

#[test]
fn time_extensions_consent_overflow_i128_min_returns_invalid_ratio() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    // elapsed_seconds = i128::MIN is <0, so InvalidRatio before arithmetic.
    assert_eq!(
        escrow.try_time_extensions_consent(&1_000, &i128::MIN, &2),
        Err(Ok(Error::InvalidRatio))
    );

    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held);
}

#[test]
fn time_extensions_consent_overflow_no_partial_write_and_lock_cleared() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, amounts);

    let before_events = crate::all_event_tuples(&env).len();
    let result = escrow.try_time_extensions_consent(&i128::MAX, &1, &2);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(
        crate::all_event_tuples(&env).len(),
        before_events,
        "overflow must not emit event"
    );
    let lock_held: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false)
    });
    assert!(!lock_held);
}
