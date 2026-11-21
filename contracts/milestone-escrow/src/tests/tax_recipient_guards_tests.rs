#![cfg(test)]
//! Guards for `set_tax_recipient` (issue #957).
//!
//! The recipient must be disinterested: never the contract itself, a job
//! party (client, freelancer, arbiter) or the acting admin. Once the escrow
//! is funded the recipient is part of the deal's economics, so changing it
//! needs the client and the freelancer to co-sign the transaction.

use crate::test::{setup_funded_escrow, setup_unfunded_escrow};
use crate::Error;
use soroban_sdk::testutils::{Address as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{vec, Address, Env, IntoVal};

/// Zero-account sentinel rejected by `validate_address`.
fn zero_account(env: &Env) -> Address {
    Address::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    )
}

fn unfunded(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    crate::MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();
    let (client, freelancer, arbiter, admin, _token, _contract, escrow) =
        setup_unfunded_escrow(env, vec![env, 1_000_i128]);
    (client, freelancer, arbiter, admin, escrow)
}

#[test]
fn unfunded_admin_can_set_a_disinterested_recipient() {
    let env = Env::default();
    let (client, freelancer, arbiter, admin, escrow) = unfunded(&env);
    let recipient = Address::generate(&env);

    escrow.set_tax_recipient(&admin, &recipient);
    assert_eq!(escrow.get_tax_recipient(), Some(recipient));

    // Sanity: none of the guards below reject a fresh address even when the
    // other parties exist.
    let _ = (client, freelancer, arbiter);
}

#[test]
fn zero_and_contract_addresses_are_rejected() {
    let env = Env::default();
    let (_client, _freelancer, _arbiter, admin, escrow) = unfunded(&env);
    let before = escrow.get_tax_recipient();

    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &zero_account(&env)),
        Err(Ok(Error::InvalidAddress))
    );
    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &escrow.address),
        Err(Ok(Error::InvalidAddress))
    );
    assert_eq!(escrow.get_tax_recipient(), before);
}

#[test]
fn job_parties_and_the_acting_admin_are_rejected() {
    let env = Env::default();
    let (client, freelancer, arbiter, admin, escrow) = unfunded(&env);

    let before = escrow.get_tax_recipient();
    for bad in [&client, &freelancer, &arbiter, &admin] {
        assert_eq!(
            escrow.try_set_tax_recipient(&admin, bad),
            Err(Ok(Error::InvalidAddress)),
            "a conflicted party must not become the tax recipient"
        );
    }
    assert_eq!(escrow.get_tax_recipient(), before);
}

/// Funded escrow: the admin alone can no longer move the tax recipient.
#[test]
fn funded_change_without_party_signatures_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _freelancer, _arbiter, admin, _token, contract, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let before = escrow.get_tax_recipient();
    let recipient = Address::generate(&env);

    // Only the admin signs: the missing client/freelancer signatures must
    // fail the whole invocation.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract,
            fn_name: "set_tax_recipient",
            args: (admin.clone(), recipient.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_set_tax_recipient(&admin, &recipient);
    assert!(
        res.is_err(),
        "a funded escrow must not change the tax recipient without the \
         client and the freelancer co-signing"
    );
    assert_eq!(escrow.get_tax_recipient(), before);
}

/// Funded escrow: with all three signatures the change succeeds.
#[test]
fn funded_change_with_full_consent_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _arbiter, admin, _token, contract, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let recipient = Address::generate(&env);

    let invoke = |address: &Address| MockAuth {
        address,
        invoke: &MockAuthInvoke {
            contract: &contract,
            fn_name: "set_tax_recipient",
            args: (admin.clone(), recipient.clone()).into_val(&env),
            sub_invokes: &[],
        },
    };
    env.mock_auths(&[invoke(&admin), invoke(&client), invoke(&freelancer)]);

    escrow.set_tax_recipient(&admin, &recipient);
    assert_eq!(escrow.get_tax_recipient(), Some(recipient));
}

/// A party co-signing is not enough either: both the client and the
/// freelancer must sign.
#[test]
fn funded_change_with_only_one_party_signature_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _freelancer, _arbiter, admin, _token, contract, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let recipient = Address::generate(&env);
    let before = escrow.get_tax_recipient();

    let invoke = |address: &Address| MockAuth {
        address,
        invoke: &MockAuthInvoke {
            contract: &contract,
            fn_name: "set_tax_recipient",
            args: (admin.clone(), recipient.clone()).into_val(&env),
            sub_invokes: &[],
        },
    };
    env.mock_auths(&[invoke(&admin), invoke(&client)]);

    assert!(escrow.try_set_tax_recipient(&admin, &recipient).is_err());
    assert_eq!(escrow.get_tax_recipient(), before);
}

/// The funded-freeze guard still rejects conflicted parties even when the
/// parties would co-sign: validation runs before the consent check.
#[test]
fn funded_change_to_a_party_is_rejected_even_with_consent() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _arbiter, admin, _token, _contract, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // mock_all_auths supplies the client/freelancer signatures; the party
    // check must still fire.
    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &client),
        Err(Ok(Error::InvalidAddress))
    );
    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &freelancer),
        Err(Ok(Error::InvalidAddress))
    );
}
