#![cfg(test)]
use super::*;
use soroban_sdk::{
    symbol_short, testutils::Events, testutils::MockAuth, testutils::MockAuthInvoke, token, vec,
    Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val,
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
    let event = env.events().all().last().unwrap();
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
    for e in env.events().all().iter() {
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
