#![cfg(test)]
//! `replace_arbiter`: the client and the freelancer jointly appoint a new
//! arbiter when the sitting one is unavailable (issue #731).

use crate::test::setup_funded_escrow;
use crate::{ArbiterReplacedEvent, Error, MilestoneStatus};
use soroban_sdk::testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke};
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
    escrow.accept_arbiter(&new_arbiter);

    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &BytesN::from_array(&env, &[3; 32]));
    assert_eq!(
        escrow.try_resolve_dispute(&old_arbiter, &0, &true),
        Err(Ok(Error::Unauthorized))
    );
    escrow.resolve_dispute(&new_arbiter, &0, &true);
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
    escrow.resolve_dispute(&arbiter, &1, &false);
    let new_arbiter = Address::generate(&env);
    escrow.replace_arbiter(&new_arbiter);
    assert_eq!(escrow.get_job().arbiter, new_arbiter);
}

#[test]
fn new_arbiter_must_accept_after_replacement() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_arbiter = Address::generate(&env);
    escrow.mark_delivered(&freelancer, &0);
    escrow.replace_arbiter(&new_arbiter);

    assert_eq!(escrow.try_raise_dispute(&client, &0, &BytesN::from_array(&env, &[1; 32])), Err(Ok(Error::NotAccepted)));
    escrow.accept_arbiter(&new_arbiter);
    escrow.raise_dispute(&client, &0, &BytesN::from_array(&env, &[1; 32]));
    assert_eq!(escrow.get_milestone(&0).status, MilestoneStatus::Disputed);
}

#[test]
fn replacement_does_not_reopen_unaccepted_refund_window() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.replace_arbiter(&Address::generate(&env));
    env.ledger().with_mut(|l| l.timestamp += 14 * 24 * 60 * 60);
    assert_eq!(
        escrow.try_withdraw_unaccepted(&client),
        Err(Ok(Error::InvalidStatus))
    );
    let _ = admin;
}
