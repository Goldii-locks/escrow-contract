#![cfg(test)]

use crate::test::setup_funded_escrow;
use crate::{Error, FreelancerRotatedEvent};
use soroban_sdk::testutils::{Address as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{symbol_short, token, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal};

#[test]
fn rotation_redirects_payout_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, old_freelancer, _, _, token_id, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_freelancer = Address::generate(&env);

    escrow.rotate_freelancer(&new_freelancer);

    let events = crate::all_event_tuples(&env);
    let event = events.last().expect("rotation emits an event");
    let topic: Symbol = event.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("freerot"));
    let data = FreelancerRotatedEvent::from_val(&env, &event.2);
    assert_eq!(data.client, client);
    assert_eq!(data.previous_freelancer, old_freelancer);
    assert_eq!(data.new_freelancer, new_freelancer);
    escrow.accept_job(&new_freelancer);

    escrow.mark_delivered(&new_freelancer, &0);
    escrow.approve_milestone(&client, &0);

    let token = token::Client::new(&env, &token_id);
    assert_eq!(token.balance(&new_freelancer), 1_000);
    assert_eq!(token.balance(&old_freelancer), 0);
}

#[test]
fn old_freelancer_cannot_call_freelancer_methods_after_rotation() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, old_freelancer, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_freelancer = Address::generate(&env);
    escrow.rotate_freelancer(&new_freelancer);

    assert_eq!(
        escrow.try_mark_delivered(&old_freelancer, &0),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn freelancer_cannot_rotate_without_client_authorization() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_freelancer = Address::generate(&env);

    let result = escrow
        .mock_auths(&[MockAuth {
            address: &freelancer,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "rotate_freelancer",
                args: (new_freelancer.clone(),).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_rotate_freelancer(&new_freelancer);

    assert!(matches!(result, Err(Err(_))));
    assert_eq!(escrow.get_job().freelancer, freelancer);
    assert_eq!(client, escrow.get_job().client);

    let client_only = escrow
        .mock_auths(&[MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "rotate_freelancer",
                args: (new_freelancer.clone(),).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_rotate_freelancer(&new_freelancer);
    assert!(matches!(client_only, Err(Err(_))));
    assert_eq!(escrow.get_job().freelancer, freelancer);
}

#[test]
fn rotation_rejects_existing_party_addresses() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _, arbiter, admin, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        escrow.try_rotate_freelancer(&client),
        Err(Ok(Error::InvalidParties))
    );
    assert_eq!(
        escrow.try_rotate_freelancer(&arbiter),
        Err(Ok(Error::InvalidParties))
    );
    assert_eq!(
        escrow.try_rotate_freelancer(&admin),
        Err(Ok(Error::InvalidParties))
    );
}

#[test]
fn rotated_freelancer_must_accept_before_delivery() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_freelancer = Address::generate(&env);
    escrow.rotate_freelancer(&new_freelancer);

    assert_eq!(
        escrow.try_mark_delivered(&new_freelancer, &0),
        Err(Ok(Error::NotAccepted))
    );
    escrow.accept_job(&new_freelancer);
    escrow.mark_delivered(&new_freelancer, &0);
    escrow.raise_dispute(&client, &0, &soroban_sdk::BytesN::from_array(&env, &[2; 32]));
    assert_eq!(escrow.get_milestone(&0).status, crate::MilestoneStatus::Disputed);
}
