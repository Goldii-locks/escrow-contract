#![cfg(test)]

use crate::test::setup_funded_escrow;
use crate::{Error, MilestoneDueDateChangedEvent, MilestoneStatus};
use soroban_sdk::testutils::{Ledger, MockAuth, MockAuthInvoke};
use soroban_sdk::{symbol_short, vec, Env, FromVal, IntoVal, Symbol, TryIntoVal};

#[test]
fn client_can_extend_or_clear_a_deadline_alone() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.set_milestone_due_date(&client, &freelancer, &0, &5_000);
    let extended = 6_000;
    escrow
        .mock_auths(&[MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "set_milestone_due_date",
                args: (client.clone(), freelancer.clone(), 0u32, extended).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .set_milestone_due_date(&client, &freelancer, &0, &extended);
    assert_eq!(escrow.get_milestone_due_at(&0), 6_000);

    let cleared = 0;
    escrow
        .mock_auths(&[MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "set_milestone_due_date",
                args: (client.clone(), freelancer.clone(), 0u32, cleared).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .set_milestone_due_date(&client, &freelancer, &0, &cleared);
    assert_eq!(escrow.get_milestone_due_at(&0), 0);
}

#[test]
fn shortening_requires_both_parties() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let shortened = 4_000;
    let client_only = MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_milestone_due_date",
            args: (client.clone(), freelancer.clone(), 0u32, shortened).into_val(&env),
            sub_invokes: &[],
        },
    };
    assert!(escrow
        .mock_auths(&[client_only])
        .try_set_milestone_due_date(&client, &freelancer, &0, &shortened)
        .is_err());
    assert_eq!(escrow.get_milestone_due_at(&0), 0);

    escrow.set_milestone_due_date(&client, &freelancer, &0, &5_000);
    let client_only = MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_milestone_due_date",
            args: (client.clone(), freelancer.clone(), 0u32, shortened).into_val(&env),
            sub_invokes: &[],
        },
    };
    assert!(escrow
        .mock_auths(&[client_only])
        .try_set_milestone_due_date(&client, &freelancer, &0, &shortened)
        .is_err());
    assert_eq!(escrow.get_milestone_due_at(&0), 5_000);

    let client_auth = MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_milestone_due_date",
            args: (client.clone(), freelancer.clone(), 0u32, shortened).into_val(&env),
            sub_invokes: &[],
        },
    };
    let freelancer_auth = MockAuth {
        address: &freelancer,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_milestone_due_date",
            args: (client.clone(), freelancer.clone(), 0u32, shortened).into_val(&env),
            sub_invokes: &[],
        },
    };
    escrow
        .mock_auths(&[client_auth, freelancer_auth])
        .set_milestone_due_date(&client, &freelancer, &0, &shortened);
    let events = crate::all_event_tuples(&env);
    let event = events.last().unwrap();
    let topic: Symbol = event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("duedate"));
    let payload = MilestoneDueDateChangedEvent::from_val(&env, &event.2);
    assert_eq!(payload.old_due_at, 5_000);
    assert_eq!(payload.new_due_at, shortened);
    assert_eq!(escrow.get_milestone_due_at(&0), shortened);
}

#[test]
fn past_dates_and_terminal_milestones_are_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    env.ledger().set_timestamp(10_000);

    assert_eq!(
        escrow.try_set_milestone_due_date(
            &client,
            &freelancer,
            &0,
            &(env.ledger().timestamp() - 1)
        ),
        Err(Ok(Error::InvalidDeadline))
    );

    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);
    assert_eq!(
        escrow.try_set_milestone_due_date(
            &client,
            &freelancer,
            &0,
            &(env.ledger().timestamp() + 1)
        ),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
}

#[test]
fn paused_escrow_rejects_deadline_changes() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.emergency_pause(&client, &freelancer);
    assert_eq!(
        escrow.try_set_milestone_due_date(&client, &freelancer, &0, &5_000),
        Err(Ok(Error::Paused))
    );
}
