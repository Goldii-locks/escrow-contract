#![cfg(test)]
//! Delegated admin roles across admin transfers (issue #955).
//!
//! Policy: an admin transfer ends the outgoing admin's delegations. Both
//! `transfer_admin` and `execute_admin_transfer` remove every explicitly
//! assigned `DataKey::AdminRole(role)`, so each role falls back to the
//! incoming admin, and emit one `rolechg` event per removed assignment
//! before the transfer's own `admin` / `adminexc` event.

use crate::tests::{seed_admin_proposal, setup_funded_escrow};
use crate::{
    AdminRole, AdminRoleChangedEvent, AdminTransferExecutedEvent, DataKey, Error,
    MilestoneEscrowClient, TransferAdminEvent,
};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{symbol_short, vec, Address, BytesN, Env, FromVal, Symbol, TryIntoVal};

fn env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

/// `(topic, data)` of every event from the most recent invocation.
fn events(env: &Env) -> std::vec::Vec<(Symbol, soroban_sdk::Val)> {
    crate::all_event_tuples(env)
        .into_iter()
        .map(|(_, topics, data)| {
            let topic: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
            (topic, data)
        })
        .collect()
}

fn role_changes(env: &Env) -> std::vec::Vec<AdminRoleChangedEvent> {
    events(env)
        .into_iter()
        .filter(|(topic, _)| *topic == symbol_short!("rolechg"))
        .map(|(_, data)| AdminRoleChangedEvent::from_val(env, &data))
        .collect()
}

fn stored_role(env: &Env, escrow: &MilestoneEscrowClient<'_>, role: AdminRole) -> Option<Address> {
    env.as_contract(&escrow.address, || {
        env.storage().instance().get(&DataKey::AdminRole(role))
    })
}

/// Delegates Upgrader and Resolver (the two roles whose survival matters
/// most after a compromise) and Operator; FeeManager stays with the admin.
fn delegate_three(
    env: &Env,
    escrow: &MilestoneEscrowClient<'_>,
    admin: &Address,
) -> (Address, Address, Address) {
    let upgrader = Address::generate(env);
    let operator = Address::generate(env);
    let resolver = Address::generate(env);
    escrow.set_admin_role(admin, &AdminRole::Upgrader, &upgrader);
    escrow.set_admin_role(admin, &AdminRole::Operator, &operator);
    escrow.set_admin_role(admin, &AdminRole::Resolver, &resolver);
    (upgrader, operator, resolver)
}

fn expected_changes(
    old_admin: &Address,
    new_admin: &Address,
    upgrader: &Address,
    operator: &Address,
    resolver: &Address,
) -> std::vec::Vec<AdminRoleChangedEvent> {
    // Declaration order of `AdminRole`; FeeManager was never assigned.
    [
        (AdminRole::Upgrader, upgrader),
        (AdminRole::Operator, operator),
        (AdminRole::Resolver, resolver),
    ]
    .into_iter()
    .map(|(role, previous)| AdminRoleChangedEvent {
        admin: old_admin.clone(),
        role,
        previous: previous.clone(),
        new_address: new_admin.clone(),
    })
    .collect()
}

/// After a transfer every role resolves to `new_admin`, and the former
/// delegates hold nothing.
fn assert_roles_reset(
    env: &Env,
    escrow: &MilestoneEscrowClient<'_>,
    new_admin: &Address,
    former: &[&Address],
) {
    for role in [
        AdminRole::Upgrader,
        AdminRole::Operator,
        AdminRole::FeeManager,
        AdminRole::Resolver,
    ] {
        assert_eq!(stored_role(env, escrow, role), None);
        assert_eq!(escrow.get_admin_role(&role), *new_admin);
    }
    for delegate in former {
        assert_eq!(
            escrow.try_admin_pause_escrow(delegate),
            Err(Ok(Error::Unauthorized))
        );
        assert_eq!(
            escrow.try_upgrade(delegate, &BytesN::from_array(env, &[0; 32])),
            Err(Ok(Error::Unauthorized))
        );
    }
    // The incoming admin holds the defaulted roles itself.
    assert_eq!(escrow.try_admin_pause_escrow(new_admin), Ok(Ok(())));
}

// ── transfer_admin ───────────────────────────────────────────────────────────

#[test]
fn transfer_admin_clears_delegations_and_emits_rolechg_for_each() {
    let env = env();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let (upgrader, operator, resolver) = delegate_three(&env, &escrow, &admin);
    let new_admin = Address::generate(&env);

    escrow.transfer_admin(&admin, &new_admin);

    assert_eq!(
        role_changes(&env),
        expected_changes(&admin, &new_admin, &upgrader, &operator, &resolver)
    );
    // The transfer event is still the last one emitted.
    let (topic, data) = events(&env).last().cloned().unwrap();
    assert_eq!(topic, symbol_short!("admin"));
    assert_eq!(
        TransferAdminEvent::from_val(&env, &data),
        TransferAdminEvent {
            old_admin: admin.clone(),
            new_admin: new_admin.clone(),
        }
    );

    assert_roles_reset(&env, &escrow, &new_admin, &[&upgrader, &operator, &resolver]);
}

#[test]
fn transfer_admin_without_delegations_emits_no_rolechg() {
    let env = env();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    escrow.transfer_admin(&admin, &new_admin);

    assert!(role_changes(&env).is_empty());
    assert_eq!(escrow.get_admin_role(&AdminRole::Upgrader), new_admin);
}

#[test]
fn rejected_transfer_admin_keeps_delegations() {
    let env = env();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let (upgrader, _, _) = delegate_three(&env, &escrow, &admin);
    let stranger = Address::generate(&env);

    assert_eq!(
        escrow.try_transfer_admin(&stranger, &Address::generate(&env)),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(escrow.get_admin_role(&AdminRole::Upgrader), upgrader);
}

#[test]
fn new_admin_can_delegate_again_after_transfer_admin() {
    let env = env();
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    delegate_three(&env, &escrow, &admin);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    // The outgoing admin can no longer assign roles; the incoming one can.
    let operator = Address::generate(&env);
    assert_eq!(
        escrow.try_set_admin_role(&admin, &AdminRole::Operator, &operator),
        Err(Ok(Error::Unauthorized))
    );
    escrow.set_admin_role(&new_admin, &AdminRole::Operator, &operator);
    assert_eq!(escrow.get_admin_role(&AdminRole::Operator), operator);
}

// ── execute_admin_transfer ───────────────────────────────────────────────────

/// Funded escrow with a 2-of-2 signer set and a transfer to a fresh admin
/// proposed (id 1). Returns `(old_admin, s1, s2, new_admin, escrow)`.
fn setup_multisig_transfer(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    let s1 = Address::generate(env);
    let s2 = Address::generate(env);
    let new_admin = Address::generate(env);
    escrow.multisig_approval_init(&admin, &vec![env, s1.clone(), s2.clone()], &2u32);
    escrow.propose_admin_transfer(&admin, &new_admin);
    (admin, s1, s2, new_admin, escrow)
}

fn approve_and_execute(env: &Env, escrow: &MilestoneEscrowClient<'_>, s1: &Address, s2: &Address) {
    // Same approval sequence as execute_admin_transfer_tests.
    seed_admin_proposal(env, &escrow.address, 1);
    escrow.multisig_approve(s1, &1u32);
    escrow.multisig_approve(s2, &1u32);
    escrow.execute_admin_transfer();
}

#[test]
fn execute_admin_transfer_clears_delegations_and_emits_rolechg_for_each() {
    let env = env();
    let (admin, s1, s2, new_admin, escrow) = setup_multisig_transfer(&env);
    // Delegated after the signer set exists: the Resolver role gates
    // multisig setup, so delegating it first would lock the admin out.
    let (upgrader, operator, resolver) = delegate_three(&env, &escrow, &admin);

    approve_and_execute(&env, &escrow, &s1, &s2);

    assert_eq!(
        role_changes(&env),
        expected_changes(&admin, &new_admin, &upgrader, &operator, &resolver)
    );
    let (topic, data) = events(&env).last().cloned().unwrap();
    assert_eq!(topic, symbol_short!("adminexc"));
    let executed = AdminTransferExecutedEvent::from_val(&env, &data);
    assert_eq!(executed.old_admin, admin);
    assert_eq!(executed.new_admin, new_admin);

    assert_roles_reset(&env, &escrow, &new_admin, &[&upgrader, &operator, &resolver]);
}

#[test]
fn execute_admin_transfer_without_delegations_emits_no_rolechg() {
    let env = env();
    let (_, s1, s2, new_admin, escrow) = setup_multisig_transfer(&env);

    approve_and_execute(&env, &escrow, &s1, &s2);

    assert!(role_changes(&env).is_empty());
    assert_eq!(escrow.get_admin_role(&AdminRole::Resolver), new_admin);
}

#[test]
fn execute_below_threshold_keeps_delegations() {
    let env = env();
    let (admin, s1, _, _, escrow) = setup_multisig_transfer(&env);
    let (upgrader, _, _) = delegate_three(&env, &escrow, &admin);

    seed_admin_proposal(&env, &escrow.address, 1);
    escrow.multisig_approve(&s1, &1u32);
    assert_eq!(
        escrow.try_execute_admin_transfer(),
        Err(Ok(Error::MultiSigThresholdNotMet))
    );
    assert_eq!(escrow.get_admin_role(&AdminRole::Upgrader), upgrader);
}
