#![cfg(test)]
//! Admin roles stay independent of the job's parties (issue #954).
//!
//! A client, freelancer or arbiter holding `Operator`, `FeeManager`,
//! `Resolver` or `Upgrader` could pause, re-fee, override or upgrade the very
//! escrow it is a party to. `set_admin_role` refuses every party, and
//! `rotate_freelancer` / `replace_arbiter` refuse an incoming party that
//! currently holds a delegated role, so the two sets can never overlap.

use crate::test::setup_funded_escrow;
use crate::{AdminRole, Error};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{vec, Address, Env};

const ROLES: [AdminRole; 4] = [
    AdminRole::Upgrader,
    AdminRole::Operator,
    AdminRole::FeeManager,
    AdminRole::Resolver,
];

fn env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

// ── set_admin_role ───────────────────────────────────────────────────────────

#[test]
fn no_role_can_be_delegated_to_any_job_party() {
    let env = env();
    let (client, freelancer, arbiter, admin, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for role in ROLES {
        for party in [&client, &freelancer, &arbiter] {
            assert_eq!(
                escrow.try_set_admin_role(&admin, &role, party),
                Err(Ok(Error::InvalidParties)),
                "{role:?} must not be delegated to a job party"
            );
            // The rejected call left the role with the admin.
            assert_eq!(escrow.get_admin_role(&role), admin);
        }
    }
}

#[test]
fn roles_still_go_to_independent_addresses_and_back_to_the_admin() {
    let env = env();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for role in ROLES {
        let delegate = Address::generate(&env);
        escrow.set_admin_role(&admin, &role, &delegate);
        assert_eq!(escrow.get_admin_role(&role), delegate);

        // Returning a role to the admin is always allowed.
        escrow.set_admin_role(&admin, &role, &admin);
        assert_eq!(escrow.get_admin_role(&role), admin);
    }
}

// ── rotate_freelancer ────────────────────────────────────────────────────────

#[test]
fn freelancer_cannot_be_rotated_to_a_role_holder() {
    for role in ROLES {
        let env = env();
        let (_, freelancer, _, admin, _, _, escrow) =
            setup_funded_escrow(&env, vec![&env, 1_000_i128]);
        let delegate = Address::generate(&env);
        escrow.set_admin_role(&admin, &role, &delegate);

        assert_eq!(
            escrow.try_rotate_freelancer(&delegate),
            Err(Ok(Error::InvalidParties)),
            "the {role:?} holder must not become the freelancer"
        );
        assert_eq!(escrow.get_job().freelancer, freelancer);
    }
}

#[test]
fn freelancer_rotation_works_once_the_role_is_returned() {
    let env = env();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let delegate = Address::generate(&env);
    escrow.set_admin_role(&admin, &AdminRole::Operator, &delegate);
    escrow.set_admin_role(&admin, &AdminRole::Operator, &admin);

    escrow.rotate_freelancer(&delegate);
    assert_eq!(escrow.get_job().freelancer, delegate);
}

// ── replace_arbiter ──────────────────────────────────────────────────────────

#[test]
fn arbiter_cannot_be_replaced_by_a_role_holder() {
    for role in ROLES {
        let env = env();
        let (_, _, arbiter, admin, _, _, escrow) =
            setup_funded_escrow(&env, vec![&env, 1_000_i128]);
        let delegate = Address::generate(&env);
        escrow.set_admin_role(&admin, &role, &delegate);

        assert_eq!(
            escrow.try_replace_arbiter(&delegate),
            Err(Ok(Error::InvalidParties)),
            "the {role:?} holder must not become the arbiter"
        );
        assert_eq!(escrow.get_job().arbiter, arbiter);
    }
}

#[test]
fn arbiter_replacement_still_accepts_an_independent_address() {
    let env = env();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.set_admin_role(&admin, &AdminRole::Resolver, &Address::generate(&env));

    let new_arbiter = Address::generate(&env);
    escrow.replace_arbiter(&new_arbiter);
    assert_eq!(escrow.get_job().arbiter, new_arbiter);
}

/// A former party can be delegated once it is no longer a party.
#[test]
fn a_rotated_out_freelancer_can_be_delegated() {
    let env = env();
    let (_, freelancer, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.rotate_freelancer(&Address::generate(&env));

    escrow.set_admin_role(&admin, &AdminRole::Operator, &freelancer);
    assert_eq!(escrow.get_admin_role(&AdminRole::Operator), freelancer);
}
