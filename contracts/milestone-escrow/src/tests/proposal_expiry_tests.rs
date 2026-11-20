#![cfg(test)]
//! Expiry and storage cleanup of admin-transfer and multisig-config proposals
//! (issue #995).
//!
//! A proposal is live through `created_at + PROPOSAL_TTL_SECONDS` inclusive and
//! expired one second later. Finalised proposals (executed, cancelled, expired
//! and cleared) have their `AdminProposal(id)` record deleted; only the events
//! remain as history.

use crate::test::setup_funded_escrow;
use crate::{
    AdminRole, DataKey, Error, MilestoneEscrowClient, ProposalExpiredEvent, PROPOSAL_TTL_SECONDS,
};
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger};
use soroban_sdk::{symbol_short, vec, Address, Env, Symbol, TryFromVal, Vec};

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    env
}

struct Setup<'a> {
    admin: Address,
    signers: Vec<Address>,
    new_set: Vec<Address>,
    escrow: MilestoneEscrowClient<'a>,
}

/// Funded escrow with a 2-of-3 multisig regime.
fn setup(env: &Env) -> Setup<'_> {
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    let signers = vec![
        env,
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ];
    escrow.multisig_approval_init(&admin, &signers, &2);
    let new_set = vec![env, Address::generate(env), Address::generate(env)];
    Setup {
        admin,
        signers,
        new_set,
        escrow,
    }
}

fn has_record(env: &Env, s: &Setup<'_>, id: u32) -> bool {
    env.as_contract(&s.escrow.address, || {
        env.storage().persistent().has(&DataKey::AdminProposal(id))
    })
}

fn has_key(env: &Env, s: &Setup<'_>, key: DataKey) -> bool {
    env.as_contract(&s.escrow.address, || env.storage().persistent().has(&key))
}

fn approve_twice(s: &Setup<'_>, id: u32) {
    s.escrow.multisig_approve(&s.signers.get(0).unwrap(), &id);
    s.escrow.multisig_approve(&s.signers.get(1).unwrap(), &id);
}

fn at(env: &Env, ts: u64) {
    env.ledger().set_timestamp(ts);
}

// ── admin transfer: boundaries ───────────────────────────────────────────────

#[test]
fn admin_transfer_approval_boundary() {
    let env = test_env();
    let s = setup(&env);
    let created = env.ledger().timestamp();
    let id = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));

    at(&env, created + PROPOSAL_TTL_SECONDS - 1);
    s.escrow.multisig_approve(&s.signers.get(0).unwrap(), &id);

    // Exactly at the boundary the proposal is still live.
    at(&env, created + PROPOSAL_TTL_SECONDS);
    s.escrow.multisig_approve(&s.signers.get(1).unwrap(), &id);

    // One second later it is dead.
    at(&env, created + PROPOSAL_TTL_SECONDS + 1);
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.signers.get(2).unwrap(), &id),
        Err(Ok(Error::ProposalExpired))
    );
}

#[test]
fn admin_transfer_execution_boundary() {
    let env = test_env();
    let s = setup(&env);
    let created = env.ledger().timestamp();
    let new_admin = Address::generate(&env);
    let id = s.escrow.propose_admin_transfer(&s.admin, &new_admin);
    approve_twice(&s, id);

    // One second after expiry: rejected, and nothing changed.
    at(&env, created + PROPOSAL_TTL_SECONDS + 1);
    assert_eq!(
        s.escrow.try_execute_admin_transfer(),
        Err(Ok(Error::ProposalExpired))
    );
    assert!(has_record(&env, &s, id));
    assert!(has_key(&env, &s, DataKey::PendingAdminTransfer));

    // Exactly at the boundary it still executes (rewind the clock to prove it).
    at(&env, created + PROPOSAL_TTL_SECONDS);
    s.escrow.execute_admin_transfer();
    assert_eq!(s.escrow.get_admin_role(&AdminRole::Resolver), new_admin);
}

// ── multisig config: boundaries ──────────────────────────────────────────────

#[test]
fn multisig_config_approval_and_execution_boundary() {
    let env = test_env();
    let s = setup(&env);
    let created = env.ledger().timestamp();
    let id = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);

    at(&env, created + PROPOSAL_TTL_SECONDS);
    approve_twice(&s, id);

    at(&env, created + PROPOSAL_TTL_SECONDS + 1);
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.signers.get(2).unwrap(), &id),
        Err(Ok(Error::ProposalExpired))
    );
    assert_eq!(
        s.escrow.try_execute_multisig_config(),
        Err(Ok(Error::ProposalExpired))
    );
    assert!(has_record(&env, &s, id));

    at(&env, created + PROPOSAL_TTL_SECONDS);
    s.escrow.execute_multisig_config();
    assert_eq!(s.escrow.get_pending_multisig_config(), None);
}

// ── permissionless clearing ──────────────────────────────────────────────────

#[test]
fn cannot_clear_a_live_proposal() {
    let env = test_env();
    let s = setup(&env);
    let created = env.ledger().timestamp();
    let id = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));

    let stranger = Address::generate(&env);
    assert_eq!(
        s.escrow.try_clear_expired_proposal(&stranger, &id),
        Err(Ok(Error::InvalidStatus))
    );
    at(&env, created + PROPOSAL_TTL_SECONDS);
    assert_eq!(
        s.escrow.try_clear_expired_proposal(&stranger, &id),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(has_record(&env, &s, id));
    assert!(has_key(&env, &s, DataKey::PendingAdminTransfer));
}

#[test]
fn anyone_can_clear_expired_transfer_and_replace_it() {
    let env = test_env();
    let s = setup(&env);
    let created = env.ledger().timestamp();
    let id = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    approve_twice(&s, id);

    // While expired-but-uncleared, the lock blocks a replacement.
    at(&env, created + PROPOSAL_TTL_SECONDS + 1);
    assert_eq!(
        s.escrow
            .try_propose_admin_transfer(&s.admin, &Address::generate(&env)),
        Err(Ok(Error::AdminTransferPending))
    );

    let stranger = Address::generate(&env);
    s.escrow.clear_expired_proposal(&stranger, &id);

    // History: the cleanup event survives the storage delete.
    let evt = crate::all_event_tuples(&env)
        .into_iter()
        .find(|(_, t, _)| {
            t.len() == 1
                && Symbol::try_from_val(&env, &t.get(0).unwrap()).ok()
                    == Some(symbol_short!("propexpd"))
        })
        .expect("propexpd event");
    let evt = ProposalExpiredEvent::try_from_val(&env, &evt.2).unwrap();
    assert_eq!(evt.proposal_id, id);
    assert_eq!(evt.created_at, created);
    assert_eq!(evt.cleared_by, stranger);

    assert!(!has_record(&env, &s, id));
    assert!(!has_key(&env, &s, DataKey::PendingAdminTransfer));
    assert_eq!(s.escrow.get_pending_admin_transfer(), None);

    // Replacement starts clean: no inherited approvals, a fresh ID.
    let next = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    assert_eq!(next, id + 1);
    assert_eq!(s.escrow.is_multisig_approved(&next).approvals, 0);
    // The cleared ID cannot be revived.
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.signers.get(0).unwrap(), &id),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
}

#[test]
fn anyone_can_clear_expired_multisig_config_and_replace_it() {
    let env = test_env();
    let s = setup(&env);
    let created = env.ledger().timestamp();
    let id = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);

    at(&env, created + PROPOSAL_TTL_SECONDS + 1);
    s.escrow
        .clear_expired_proposal(&Address::generate(&env), &id);
    assert!(!has_record(&env, &s, id));
    assert_eq!(s.escrow.get_pending_multisig_config(), None);

    let next = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);
    assert_eq!(next, id + 1);
}

#[test]
fn clearing_an_id_never_touches_a_newer_or_unrelated_proposal() {
    let env = test_env();
    let s = setup(&env);
    let t0 = env.ledger().timestamp();

    // Old transfer (#1), later a config proposal (#2).
    let transfer = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    at(&env, t0 + 10);
    let config = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);

    // The transfer expires first; the config proposal is still live.
    at(&env, t0 + PROPOSAL_TTL_SECONDS + 1);
    s.escrow
        .clear_expired_proposal(&Address::generate(&env), &transfer);
    assert!(!has_record(&env, &s, transfer));
    assert!(has_record(&env, &s, config));
    assert!(s.escrow.get_pending_multisig_config().is_some());

    // Re-clearing the stale ID fails and cannot reach the unexpired config.
    assert_eq!(
        s.escrow
            .try_clear_expired_proposal(&Address::generate(&env), &transfer),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
    // A replacement transfer is not removed by clearing the old ID either.
    let replacement = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    assert_eq!(
        s.escrow
            .try_clear_expired_proposal(&Address::generate(&env), &transfer),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
    assert_eq!(
        s.escrow
            .try_clear_expired_proposal(&Address::generate(&env), &replacement),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(has_record(&env, &s, replacement));
    assert!(has_key(&env, &s, DataKey::PendingAdminTransfer));
    assert!(has_record(&env, &s, config));

    // Unknown ID.
    assert_eq!(
        s.escrow
            .try_clear_expired_proposal(&Address::generate(&env), &999),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
}

// ── cleanup on every finalisation path ───────────────────────────────────────

#[test]
fn executed_and_cancelled_proposals_delete_only_their_own_record() {
    let env = test_env();
    let s = setup(&env);

    let transfer = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    let config = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);
    assert!(has_record(&env, &s, transfer) && has_record(&env, &s, config));

    // Execute the config: only its record goes; the transfer's survives
    // (with approvals reset by the rotation).
    approve_twice(&s, config);
    s.escrow.execute_multisig_config();
    assert!(!has_record(&env, &s, config));
    assert!(has_record(&env, &s, transfer));

    // Cancel the transfer: its record goes.
    s.escrow.cancel_admin_transfer_proposal(&s.admin);
    assert!(!has_record(&env, &s, transfer));
    assert!(!has_key(&env, &s, DataKey::PendingAdminTransfer));

    // Execute a transfer: record deleted.
    let t2 = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    s.escrow.multisig_approve(&s.new_set.get(0).unwrap(), &t2);
    s.escrow.multisig_approve(&s.new_set.get(1).unwrap(), &t2);
    s.escrow.execute_admin_transfer();
    assert!(!has_record(&env, &s, t2));

    // Cancel a config proposal: record deleted.
    let new_admin = s.escrow.get_admin_role(&AdminRole::Resolver);
    let c2 = s.escrow.propose_multisig_config(&new_admin, &s.new_set, &1);
    assert!(has_record(&env, &s, c2));
    s.escrow.cancel_multisig_config_proposal(&new_admin);
    assert!(!has_record(&env, &s, c2));
}

#[test]
fn legacy_terminal_record_can_be_cleared_without_waiting() {
    let env = test_env();
    let s = setup(&env);
    let id = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    // Simulate a pre-#995 tombstone: cancelled status left behind.
    env.as_contract(&s.escrow.address, || {
        let mut r: crate::AdminTransferProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AdminProposal(id))
            .unwrap();
        r.status = crate::AdminProposalStatus::Cancelled;
        env.storage()
            .persistent()
            .set(&DataKey::AdminProposal(id), &r);
    });
    s.escrow
        .clear_expired_proposal(&Address::generate(&env), &id);
    assert!(!has_record(&env, &s, id));
}
