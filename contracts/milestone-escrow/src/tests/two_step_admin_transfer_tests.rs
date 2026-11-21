#![cfg(test)]
//! Two-step admin transfer (issue #953).
//!
//! `transfer_admin` and the multisig `execute_admin_transfer` only record a
//! `PendingAdminAcceptance`; the stored admin changes when the proposed admin
//! signs `accept_admin` within `ADMIN_ACCEPTANCE_WINDOW_SECONDS`. The current
//! admin can withdraw it with `cancel_admin_transfer`, and a lapsed one is
//! cleared by `expire_admin_transfer` (or by the next proposal). Events:
//! `adm_prop` (proposed), `adm_acc` (accepted), `adm_exp` (expired),
//! `adm_cncl` (cancelled).

use crate::test::setup_funded_escrow;
use crate::{
    AdminAcceptanceCancelledEvent, AdminAcceptanceExpiredEvent, AdminAcceptanceProposedEvent,
    AdminAcceptedEvent, DataKey, Error, MilestoneEscrowClient, PendingAdminAcceptance,
    TransferAdminEvent, ADMIN_ACCEPTANCE_WINDOW_SECONDS,
};
use soroban_sdk::testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

const START: u64 = 1_700_000_000;

fn env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    env
}

/// Funded escrow with a single admin (no multisig regime).
fn single_admin(env: &Env) -> (Address, MilestoneEscrowClient<'_>) {
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    (admin, escrow)
}

fn stored_admin(env: &Env, escrow: &MilestoneEscrowClient<'_>) -> Address {
    env.as_contract(&escrow.address, || {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    })
}

/// `(topic, data)` of every event from the most recent invocation.
fn events(env: &Env) -> std::vec::Vec<(Symbol, Val)> {
    crate::all_event_tuples(env)
        .into_iter()
        .map(|(_, topics, data)| {
            let topic: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
            (topic, data)
        })
        .collect()
}

fn topics(env: &Env) -> std::vec::Vec<Symbol> {
    events(env).into_iter().map(|(topic, _)| topic).collect()
}

fn event_data(env: &Env, topic: Symbol) -> Val {
    events(env)
        .into_iter()
        .find(|(t, _)| *t == topic)
        .map(|(_, data)| data)
        .expect("event not emitted")
}

fn set_time(env: &Env, timestamp: u64) {
    env.ledger().set_timestamp(timestamp);
}

// ── transfer_admin: proposal, acceptance ─────────────────────────────────────

#[test]
fn transfer_admin_only_proposes_and_emits_adm_prop() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);

    escrow.transfer_admin(&admin, &new_admin);

    let expires_at = START + ADMIN_ACCEPTANCE_WINDOW_SECONDS;
    assert_eq!(
        AdminAcceptanceProposedEvent::from_val(&env, &event_data(&env, symbol_short!("adm_prop"))),
        AdminAcceptanceProposedEvent {
            old_admin: admin.clone(),
            new_admin: new_admin.clone(),
            expires_at,
            proposal_id: 0,
        }
    );
    assert!(!topics(&env).contains(&symbol_short!("admin")));

    assert_eq!(stored_admin(&env, &escrow), admin);
    assert_eq!(
        escrow.get_pending_admin_acceptance(),
        Some(PendingAdminAcceptance {
            old_admin: admin,
            new_admin,
            expires_at,
            proposal_id: 0,
        })
    );
}

#[test]
fn accept_admin_completes_the_transfer() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    escrow.accept_admin(&new_admin);

    assert_eq!(
        topics(&env),
        std::vec![symbol_short!("adm_acc"), symbol_short!("admin")]
    );
    assert_eq!(
        AdminAcceptedEvent::from_val(&env, &event_data(&env, symbol_short!("adm_acc"))),
        AdminAcceptedEvent {
            old_admin: admin.clone(),
            new_admin: new_admin.clone(),
            proposal_id: 0,
        }
    );
    assert_eq!(
        TransferAdminEvent::from_val(&env, &event_data(&env, symbol_short!("admin"))),
        TransferAdminEvent {
            old_admin: admin.clone(),
            new_admin: new_admin.clone(),
        }
    );
    assert_eq!(stored_admin(&env, &escrow), new_admin);
    assert_eq!(escrow.get_pending_admin_acceptance(), None);

    // Authority moved with the entry.
    assert_eq!(
        escrow.try_admin_pause_escrow(&admin),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(escrow.try_admin_pause_escrow(&new_admin), Ok(Ok(())));
}

#[test]
fn the_old_admin_keeps_its_powers_until_acceptance() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    assert_eq!(
        escrow.try_admin_pause_escrow(&new_admin),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(escrow.try_admin_pause_escrow(&admin), Ok(Ok(())));
}

// ── wrong signers ────────────────────────────────────────────────────────────

#[test]
fn accept_admin_rejects_any_other_address() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    for wrong in [Address::generate(&env), admin.clone()] {
        assert_eq!(
            escrow.try_accept_admin(&wrong),
            Err(Ok(Error::Unauthorized))
        );
    }
    assert_eq!(stored_admin(&env, &escrow), admin);
    assert!(escrow.get_pending_admin_acceptance().is_some());
}

#[test]
fn accept_admin_requires_the_new_admins_own_signature() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    // Only the old admin authorizes a call naming `new_admin`: refused.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &escrow.address,
            fn_name: "accept_admin",
            args: (new_admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(escrow.try_accept_admin(&new_admin).is_err());
    assert_eq!(stored_admin(&env, &escrow), admin);

    // With `new_admin`'s own authorization it succeeds.
    env.mock_auths(&[MockAuth {
        address: &new_admin,
        invoke: &MockAuthInvoke {
            contract: &escrow.address,
            fn_name: "accept_admin",
            args: (new_admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.accept_admin(&new_admin);
    assert_eq!(stored_admin(&env, &escrow), new_admin);
}

#[test]
fn accept_admin_without_a_pending_transfer_is_rejected() {
    let env = env();
    let (_, escrow) = single_admin(&env);

    assert_eq!(
        escrow.try_accept_admin(&Address::generate(&env)),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
}

// ── expiry ───────────────────────────────────────────────────────────────────

#[test]
fn accept_admin_works_until_the_window_closes() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    // The last second of the window is still inside it.
    set_time(&env, START + ADMIN_ACCEPTANCE_WINDOW_SECONDS);
    escrow.accept_admin(&new_admin);
    assert_eq!(stored_admin(&env, &escrow), new_admin);
}

#[test]
fn an_expired_transfer_cannot_be_accepted_and_is_cleared_with_adm_exp() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);
    let expires_at = START + ADMIN_ACCEPTANCE_WINDOW_SECONDS;

    // Too early to expire.
    assert_eq!(
        escrow.try_expire_admin_transfer(),
        Err(Ok(Error::DeadlineNotPassed))
    );

    set_time(&env, expires_at + 1);
    assert_eq!(
        escrow.try_accept_admin(&new_admin),
        Err(Ok(Error::AdminTransferExpired))
    );
    assert_eq!(stored_admin(&env, &escrow), admin);

    // Anyone may clear it.
    escrow.expire_admin_transfer();
    assert_eq!(
        AdminAcceptanceExpiredEvent::from_val(&env, &event_data(&env, symbol_short!("adm_exp"))),
        AdminAcceptanceExpiredEvent {
            old_admin: admin.clone(),
            new_admin: new_admin.clone(),
            expires_at,
            proposal_id: 0,
        }
    );
    assert_eq!(escrow.get_pending_admin_acceptance(), None);
    assert_eq!(
        escrow.try_accept_admin(&new_admin),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
    assert_eq!(stored_admin(&env, &escrow), admin);
}

#[test]
fn a_new_proposal_replaces_a_lapsed_one() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let first = Address::generate(&env);
    escrow.transfer_admin(&admin, &first);

    set_time(&env, START + ADMIN_ACCEPTANCE_WINDOW_SECONDS + 1);
    let second = Address::generate(&env);
    escrow.transfer_admin(&admin, &second);

    // The lapsed proposal is reported expired before the new one is proposed.
    assert_eq!(
        topics(&env),
        std::vec![symbol_short!("adm_exp"), symbol_short!("adm_prop")]
    );
    assert_eq!(
        escrow.get_pending_admin_acceptance().unwrap().new_admin,
        second
    );
    assert_eq!(
        escrow.try_accept_admin(&first),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn a_live_proposal_blocks_another() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    escrow.transfer_admin(&admin, &Address::generate(&env));

    assert_eq!(
        escrow.try_transfer_admin(&admin, &Address::generate(&env)),
        Err(Ok(Error::AdminTransferPending))
    );
}

// ── cancellation ─────────────────────────────────────────────────────────────

#[test]
fn the_current_admin_can_cancel_before_acceptance() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    escrow.cancel_admin_transfer(&admin);

    assert_eq!(
        AdminAcceptanceCancelledEvent::from_val(&env, &event_data(&env, symbol_short!("adm_cncl"))),
        AdminAcceptanceCancelledEvent {
            admin: admin.clone(),
            new_admin: new_admin.clone(),
            proposal_id: 0,
        }
    );
    assert_eq!(escrow.get_pending_admin_acceptance(), None);
    assert_eq!(
        escrow.try_accept_admin(&new_admin),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
    assert_eq!(stored_admin(&env, &escrow), admin);

    // A fresh proposal can follow at once.
    escrow.transfer_admin(&admin, &Address::generate(&env));
}

#[test]
fn only_the_current_admin_can_cancel() {
    let env = env();
    let (admin, escrow) = single_admin(&env);
    let new_admin = Address::generate(&env);
    escrow.transfer_admin(&admin, &new_admin);

    for other in [new_admin.clone(), Address::generate(&env)] {
        assert_eq!(
            escrow.try_cancel_admin_transfer(&other),
            Err(Ok(Error::Unauthorized))
        );
    }
    assert!(escrow.get_pending_admin_acceptance().is_some());
}

#[test]
fn cancel_without_a_pending_transfer_is_rejected() {
    let env = env();
    let (admin, escrow) = single_admin(&env);

    assert_eq!(
        escrow.try_cancel_admin_transfer(&admin),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
}

// ── multisig: execute_admin_transfer also needs accept_admin ─────────────────

/// Funded escrow with a 2-of-2 signer set and a transfer to a fresh admin
/// approved by both signers. Returns `(old_admin, new_admin, escrow)`.
fn approved_multisig_transfer(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    let s1 = Address::generate(env);
    let s2 = Address::generate(env);
    escrow.multisig_approval_init(&admin, &vec![env, s1.clone(), s2.clone()], &2u32);
    let new_admin = Address::generate(env);
    let id = escrow.propose_admin_transfer(&admin, &new_admin);
    escrow.multisig_approve(&s1, &id);
    escrow.multisig_approve(&s2, &id);
    (admin, new_admin, escrow)
}

#[test]
fn execute_admin_transfer_opens_an_acceptance_instead_of_swapping() {
    let env = env();
    let (admin, new_admin, escrow) = approved_multisig_transfer(&env);

    escrow.execute_admin_transfer();

    // `adm_prop` precedes `adminexc`, which stays the last event.
    assert_eq!(
        topics(&env),
        std::vec![symbol_short!("adm_prop"), symbol_short!("adminexc")]
    );
    assert_eq!(stored_admin(&env, &escrow), admin);
    assert_eq!(escrow.get_pending_admin_transfer(), None);
    let pending = escrow.get_pending_admin_acceptance().unwrap();
    assert_eq!(pending.new_admin, new_admin);
    assert_eq!(pending.proposal_id, 1);
    assert_eq!(pending.expires_at, START + ADMIN_ACCEPTANCE_WINDOW_SECONDS);

    escrow.accept_admin(&new_admin);
    assert_eq!(
        AdminAcceptedEvent::from_val(&env, &event_data(&env, symbol_short!("adm_acc"))),
        AdminAcceptedEvent {
            old_admin: admin,
            new_admin: new_admin.clone(),
            proposal_id: 1,
        }
    );
    assert_eq!(stored_admin(&env, &escrow), new_admin);
}

#[test]
fn an_approved_multisig_transfer_still_expires() {
    let env = env();
    let (admin, new_admin, escrow) = approved_multisig_transfer(&env);
    escrow.execute_admin_transfer();

    set_time(&env, START + ADMIN_ACCEPTANCE_WINDOW_SECONDS + 1);
    assert_eq!(
        escrow.try_accept_admin(&new_admin),
        Err(Ok(Error::AdminTransferExpired))
    );
    escrow.expire_admin_transfer();
    assert_eq!(stored_admin(&env, &escrow), admin);
    assert_eq!(escrow.get_pending_admin_acceptance(), None);
}

#[test]
fn a_new_multisig_proposal_waits_for_a_live_acceptance() {
    let env = env();
    let (admin, _, escrow) = approved_multisig_transfer(&env);
    escrow.execute_admin_transfer();

    assert_eq!(
        escrow.try_propose_admin_transfer(&admin, &Address::generate(&env)),
        Err(Ok(Error::AdminTransferPending))
    );

    // Cancelling the hand-over reopens proposals.
    escrow.cancel_admin_transfer(&admin);
    assert_eq!(
        escrow.try_propose_admin_transfer(&admin, &Address::generate(&env)),
        Ok(Ok(2))
    );
}
