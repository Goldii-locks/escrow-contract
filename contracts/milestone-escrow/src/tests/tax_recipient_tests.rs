#![cfg(test)]
//! Dedicated tests for `get_tax_recipient` and `set_tax_recipient` (issue #984).
//!
//! Asserts:
//! - getter before set (`None`) and after set (`Some`);
//! - rejection of zero account and zero contract addresses with `InvalidAddress`;
//! - rejection of caller's own contract address with `InvalidAddress`;
//! - caller authorization: only `FeeManager` role (or admin fallback) can set;
//!   callers with other roles or unauthorised callers are rejected with `Unauthorized`;
//! - role delegation: after `set_admin_role(FeeManager, x)`, `x` can set and `admin` cannot;
//! - overwriting an existing recipient;
//! - `taxrcpt` structured event emission with exact payload.

use crate::test::setup_funded_escrow;
use crate::{AdminRole, Error, TaxRecipientSetEvent};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, Symbol, TryIntoVal};

const ZERO_ACCOUNT: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
const ZERO_CONTRACT: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

#[test]
fn test_get_tax_recipient_before_and_after_set_and_overwrite() {
    let env = Env::default();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Initial state: None
    assert_eq!(escrow.get_tax_recipient(), None);

    // After setting recipient
    let recipient1 = Address::generate(&env);
    escrow.set_tax_recipient(&admin, &recipient1);
    assert_eq!(escrow.get_tax_recipient(), Some(recipient1.clone()));

    // Overwriting existing recipient
    let recipient2 = Address::generate(&env);
    escrow.set_tax_recipient(&admin, &recipient2);
    assert_eq!(escrow.get_tax_recipient(), Some(recipient2));
}

#[test]
fn test_set_tax_recipient_rejects_zero_addresses_and_contract_self() {
    let env = Env::default();
    let (contract_id, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let zero_acc = Address::from_str(&env, ZERO_ACCOUNT);
    let zero_ctr = Address::from_str(&env, ZERO_CONTRACT);

    // Zero account rejected
    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &zero_acc),
        Err(Ok(Error::InvalidAddress))
    );

    // Zero contract rejected
    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &zero_ctr),
        Err(Ok(Error::InvalidAddress))
    );

    // Current contract address rejected
    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &contract_id),
        Err(Ok(Error::InvalidAddress))
    );
}

#[test]
fn test_set_tax_recipient_caller_role_and_delegation() {
    let env = Env::default();
    let (client, freelancer, arbiter, admin, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let recipient = Address::generate(&env);
    let unauthorized_caller = Address::generate(&env);

    // Client, freelancer, arbiter, or arbitrary caller cannot set tax recipient
    assert_eq!(
        escrow.try_set_tax_recipient(&client, &recipient),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        escrow.try_set_tax_recipient(&freelancer, &recipient),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        escrow.try_set_tax_recipient(&arbiter, &recipient),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        escrow.try_set_tax_recipient(&unauthorized_caller, &recipient),
        Err(Ok(Error::Unauthorized))
    );

    // Delegated non-FeeManager roles (Operator, Resolver, Upgrader) cannot set tax recipient
    let operator = Address::generate(&env);
    let resolver = Address::generate(&env);
    let upgrader = Address::generate(&env);
    let fee_manager = Address::generate(&env);

    escrow.set_admin_role(&admin, &AdminRole::Operator, &operator);
    escrow.set_admin_role(&admin, &AdminRole::Resolver, &resolver);
    escrow.set_admin_role(&admin, &AdminRole::Upgrader, &upgrader);

    assert_eq!(
        escrow.try_set_tax_recipient(&operator, &recipient),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        escrow.try_set_tax_recipient(&resolver, &recipient),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        escrow.try_set_tax_recipient(&upgrader, &recipient),
        Err(Ok(Error::Unauthorized))
    );

    // Delegate FeeManager to fee_manager: now fee_manager can set and root admin cannot
    escrow.set_admin_role(&admin, &AdminRole::FeeManager, &fee_manager);

    assert_eq!(
        escrow.try_set_tax_recipient(&admin, &recipient),
        Err(Ok(Error::Unauthorized))
    );

    escrow.set_tax_recipient(&fee_manager, &recipient);
    assert_eq!(escrow.get_tax_recipient(), Some(recipient));
}

#[test]
fn test_set_tax_recipient_event_payload() {
    let env = Env::default();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let recipient = Address::generate(&env);
    escrow.set_tax_recipient(&admin, &recipient);

    let events = crate::all_event_tuples(&env);
    let mut found_taxrcpt = false;
    for (_, topics, data) in events.iter() {
        if let Some(topic_val) = topics.get(0) {
            let topic: Symbol = topic_val.try_into_val(&env).unwrap();
            if topic == symbol_short!("taxrcpt") {
                let event = TaxRecipientSetEvent::from_val(&env, data);
                assert_eq!(event.admin, admin);
                assert_eq!(event.recipient, recipient);
                found_taxrcpt = true;
            }
        }
    }
    assert!(found_taxrcpt, "taxrcpt event must be emitted");
}
