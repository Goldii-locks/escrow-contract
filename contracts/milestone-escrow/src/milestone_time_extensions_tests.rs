use super::*;

#[test]
fn positive_ratios_preserve_amount_across_boundaries() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    for (elapsed, total, expected_freelancer) in
        [(0_i128, 10_i128, 0_i128), (1, 3, 34), (1, 2, 51), (10, 10, 101)]
    {
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
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, amounts);
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
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, amounts);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.approve_partial(&client_addr, &0, &1_000);

    assert!(escrow.try_extend_milestone_deadline(&client_addr, &0, &1).is_ok());
}

#[test]
fn deadline_extension_rejects_invalid_index_and_terminal_status() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, amounts);

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
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, amounts);
    escrow.mark_delivered(&freelancer_addr, &0);

    escrow.extend_milestone_deadline(&client_addr, &0, &u64::MAX);
    assert_eq!(
        escrow.try_extend_milestone_deadline(&client_addr, &0, &1),
        Err(Ok(Error::InvalidExtension))
    );
}
