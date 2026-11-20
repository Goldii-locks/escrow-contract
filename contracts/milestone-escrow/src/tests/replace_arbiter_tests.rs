#![cfg(test)]
//! `replace_arbiter`: the client and the freelancer jointly appoint a new
//! arbiter when the sitting one is unavailable (issue #731).

use crate::test::{decision_hash, setup_funded_escrow};
use crate::{ArbiterReplacedEvent, ArbiterResignedEvent, Error, MilestoneStatus};
use soroban_sdk::testutils::{Address as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{
    symbol_short, token, vec, Address, BytesN, Env, FromVal, IntoVal, Symbol, TryIntoVal,
};

#[test]
fn replacement_moves_arbitration_to_the_new_arbiter_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, old_arbiter, _, token_id, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_arbiter = Address::generate(&env);

    escrow.replace_arbiter(&new_arbiter);

    let events = crate::all_event_tuples(&env);
    let event = events.last().expect("replacement emits an event");
    let topic: Symbol = event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("arbchg"));
    assert_eq!(
        ArbiterReplacedEvent::from_val(&env, &event.2),
        ArbiterReplacedEvent {
            client: client.clone(),
            freelancer: freelancer.clone(),
            old: old_arbiter.clone(),
            new: new_arbiter.clone(),
        }
    );
    assert_eq!(escrow.get_job().arbiter, new_arbiter);

    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &BytesN::from_array(&env, &[3; 32]));
    assert_eq!(
        escrow.try_resolve_dispute(&old_arbiter, &0, &true, &decision_hash(&env)),
        Err(Ok(Error::Unauthorized))
    );
    escrow.resolve_dispute(&new_arbiter, &0, &true, &decision_hash(&env));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&freelancer),
        1_000
    );
}

#[test]
fn replacement_needs_both_client_and_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_arbiter = Address::generate(&env);

    for signer in [&client, &freelancer] {
        let result = escrow
            .mock_auths(&[MockAuth {
                address: signer,
                invoke: &MockAuthInvoke {
                    contract: &contract_id,
                    fn_name: "replace_arbiter",
                    args: (new_arbiter.clone(),).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .try_replace_arbiter(&new_arbiter);
        assert!(matches!(result, Err(Err(_))));
        assert_eq!(escrow.get_job().arbiter, arbiter);
    }
}

#[test]
fn replacement_rejects_existing_parties() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for party in [&client, &freelancer, &arbiter] {
        assert_eq!(
            escrow.try_replace_arbiter(party),
            Err(Ok(Error::InvalidParties))
        );
    }
    assert_eq!(escrow.get_job().arbiter, arbiter);
}

#[test]
fn replacement_is_refused_while_a_dispute_is_open() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000]);
    escrow.mark_delivered(&freelancer, &1);
    escrow.raise_dispute(&client, &1, &BytesN::from_array(&env, &[4; 32]));

    assert_eq!(
        escrow.try_replace_arbiter(&Address::generate(&env)),
        Err(Ok(Error::DisputePending))
    );
    assert_eq!(escrow.get_job().arbiter, arbiter);

    // Once the dispute is resolved the parties may swap the arbiter.
    escrow.resolve_dispute(&arbiter, &1, &false, &decision_hash(&env));
    let new_arbiter = Address::generate(&env);
    escrow.replace_arbiter(&new_arbiter);
    assert_eq!(escrow.get_job().arbiter, new_arbiter);
}

#[test]
fn arbiter_can_resign_when_no_dispute_is_open_and_replacement_restores_flow() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer, &0);
    escrow.resign_arbiter(&arbiter);
    let event = crate::all_event_tuples(&env)
        .last()
        .expect("resignation emits an event")
        .clone();
    let topic: Symbol = event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("arbresig"));
    assert_eq!(
        ArbiterResignedEvent::from_val(&env, &event.2),
        ArbiterResignedEvent {
            client: client.clone(),
            arbiter: arbiter.clone(),
        }
    );
    assert_eq!(
        escrow.try_raise_dispute(&client, &0, &BytesN::from_array(&env, &[5; 32])),
        Err(Ok(Error::ArbiterVacant))
    );

    let replacement = Address::generate(&env);
    escrow.replace_arbiter(&replacement);
    escrow.accept_arbiter(&replacement);
    escrow.raise_dispute(&client, &0, &BytesN::from_array(&env, &[5; 32]));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
}

#[test]
fn arbiter_cannot_resign_with_an_open_dispute() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &BytesN::from_array(&env, &[6; 32]));

    assert_eq!(
        escrow.try_resign_arbiter(&arbiter),
        Err(Ok(Error::DisputePending))
    );
    assert_eq!(escrow.get_job().arbiter, arbiter);
}
