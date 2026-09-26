//! Dedicated test suite for the `msiginit` structured event emitted by
//! `multisig_approval_init` (issue #455).
//!
//! `multisig_approval_init` is the one-time registration of the multisig signer
//! set and approval threshold: it is the *only* write path for
//! `DataKey::MultiSigSigners` / `DataKey::MultiSigThreshold`, and every later
//! endpoint (`multisig_approve`, `execute_admin_transfer`,
//! `multisig_split_refund`, ...) is gated on the values it stores.  A
//! registration that leaves no ledger trace is only reconstructible by
//! replaying storage, so the call publishes a typed event carrying the acting
//! admin and the values that were actually persisted.
//!
//! Every success-path test asserts that the emitted
//! `MultisigApprovalInitEvent` fields reconcile exactly with the state the call
//! persisted under `DataKey::MultiSigSigners` / `DataKey::MultiSigThreshold` —
//! read straight out of instance storage, bypassing the public accessor, so the
//! assertion cannot pass on a getter that disagrees with the ledger.  Every
//! failure-path test asserts that no `msiginit` event is published and that
//! neither storage key was written.
//!
//! The event buffer exposed by the test `Env` reflects the most recent
//! top-level invocation only, so each test tallies events immediately after the
//! call it is asserting on (and re-reads the durable state whenever a later
//! call would otherwise clear the buffer).

use super::*;
use crate::{DataKey, Error, MultisigApprovalInitEvent};
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

const MSIGINIT_TOPIC: &str = "msiginit";

/// Tally of the `msiginit` events in the current event buffer.
fn msiginit_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("msiginit").into_val(env);
    let mut count = 0u32;
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

/// The most recently published event, asserting it carries the `msiginit`
/// topic so a test can never accidentally read a neighbouring event's payload.
fn last_msiginit_event(env: &Env) -> MultisigApprovalInitEvent {
    let events = crate::all_event_tuples(env);
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, MSIGINIT_TOPIC));
    MultisigApprovalInitEvent::from_val(env, &last.2)
}

/// Direct read of the instance-storage entry the call wrote, bypassing the
/// public accessor so the reconciliation assertion is against the ledger.
fn stored_signers(env: &Env, contract_id: &Address) -> Option<Vec<Address>> {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&DataKey::MultiSigSigners)
    })
}

/// Direct read of the persisted approval threshold.
fn stored_threshold(env: &Env, contract_id: &Address) -> Option<u32> {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&DataKey::MultiSigThreshold)
    })
}

/// Assert both payload fields equal the ledger entries the call wrote, and that
/// the acting address is the admin the contract has on file.
///
/// Deliberately storage-only: reading the entries directly through
/// `Env::as_contract` proves the event describes what the call persisted, and
/// cannot pass on a public accessor that disagrees with the ledger.
fn assert_event_reconciles(env: &Env, ev: &MultisigApprovalInitEvent, contract_id: &Address) {
    let signers = stored_signers(env, contract_id).expect("init persisted the signer set");
    let threshold = stored_threshold(env, contract_id).expect("init persisted the threshold");

    assert_eq!(
        ev.signers, signers,
        "event must report the persisted signer set"
    );
    assert_eq!(
        ev.threshold, threshold,
        "event must report the persisted threshold"
    );

    let stored_admin: Address = env
        .as_contract(contract_id, || {
            env.storage().instance().get(&DataKey::Admin)
        })
        .expect("initialize stored an admin");
    assert_eq!(ev.admin, stored_admin, "event must name the acting admin");
}

// ── success path: field-by-field reconciliation ─────────────────────────────

/// The canonical registration: every field of the event must equal the state
/// the call persisted, so an indexer replaying `msiginit` reconstructs exactly
/// what the contract stored.
#[test]
fn event_reconciles_with_persisted_multisig_setup() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Nothing is registered yet, so nothing has been announced.
    assert_eq!(msiginit_event_count(&env), 0);
    assert_eq!(stored_signers(&env, &contract_id), None);
    assert_eq!(stored_threshold(&env, &contract_id), None);

    // The public accessor reads the same ledger the event will reconcile
    // against, and reports the pre-registration state.
    assert!(
        matches!(
            client.try_is_multisig_approved(&0u32),
            Err(Ok(Error::NotInitialized))
        ),
        "the accessor must report the pre-registration state"
    );

    let signer_a = Address::generate(&env);
    let signer_b = Address::generate(&env);
    let signer_c = Address::generate(&env);
    let signers = vec![&env, signer_a.clone(), signer_b.clone(), signer_c.clone()];
    let threshold = 2u32;

    client.multisig_approval_init(&admin_addr, &signers, &threshold);

    // Exactly one structured event.
    assert_eq!(msiginit_event_count(&env), 1);
    let ev = last_msiginit_event(&env);

    // Acting address: the admin that signed — the address `require_admin`
    // verified against the stored admin key.
    assert_eq!(ev.admin, admin_addr);

    // Resulting values, field by field.
    assert_eq!(
        ev.signers, signers,
        "event must carry the registered signers"
    );
    assert_eq!(
        ev.threshold, threshold,
        "event must carry the registered threshold"
    );

    // ... and their reconciliation with what was actually persisted.
    assert_event_reconciles(&env, &ev, &contract_id);

    // The public accessor agrees with the event about the registered
    // configuration, and no proposal has been approved yet.
    let state = client.try_is_multisig_approved(&0u32).unwrap().unwrap();
    assert_eq!(state.threshold, ev.threshold);
    assert_eq!(state.approvals, 0);
    assert!(!state.approved);
}

/// The event carries the *whole* signer set, in the order it was persisted —
/// not a prefix or a summary — so every registered signer is auditable from the
/// event alone.
#[test]
fn event_carries_the_full_signer_set() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let signer_a = Address::generate(&env);
    let signer_b = Address::generate(&env);
    let signer_c = Address::generate(&env);
    let signer_d = Address::generate(&env);
    let signers = vec![
        &env,
        signer_a.clone(),
        signer_b.clone(),
        signer_c.clone(),
        signer_d.clone(),
    ];

    client.multisig_approval_init(&admin_addr, &signers, &3u32);

    assert_eq!(msiginit_event_count(&env), 1);
    let ev = last_msiginit_event(&env);

    assert_eq!(
        ev.signers.len(),
        4,
        "no signer may be dropped from the event"
    );
    assert_eq!(ev.signers.get(0).unwrap(), signer_a);
    assert_eq!(ev.signers.get(1).unwrap(), signer_b);
    assert_eq!(ev.signers.get(2).unwrap(), signer_c);
    assert_eq!(ev.signers.get(3).unwrap(), signer_d);

    assert_event_reconciles(&env, &ev, &contract_id);
}

// ── failure paths: no event emitted, no state written ───────────────────────

/// A threshold above the signer count is rejected by `validate_multisig_setup`
/// before either storage write, so no `msiginit` event is published and the
/// multisig configuration stays unregistered.
#[test]
fn no_event_when_threshold_is_invalid() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let signer_a = Address::generate(&env);
    let signer_b = Address::generate(&env);
    let signers = vec![&env, signer_a, signer_b];

    // 3 > 2 signers.
    assert_eq!(
        client.try_multisig_approval_init(&admin_addr, &signers, &3u32),
        Err(Ok(Error::MultiSigInvalidThreshold))
    );

    // The reverted invocation contributes no event of its own.
    assert_eq!(msiginit_event_count(&env), 0);
    assert_eq!(stored_signers(&env, &contract_id), None);
    assert_eq!(stored_threshold(&env, &contract_id), None);

    // A zero threshold is rejected the same way.
    assert_eq!(
        client.try_multisig_approval_init(&admin_addr, &signers, &0u32),
        Err(Ok(Error::MultiSigInvalidThreshold))
    );
    assert_eq!(msiginit_event_count(&env), 0);
    assert_eq!(stored_signers(&env, &contract_id), None);
    assert_eq!(stored_threshold(&env, &contract_id), None);
}

/// Every malformed signer set — empty, duplicated, or larger than the
/// 32-signer cap — is rejected before any write, so none of them announces a
/// registration.
#[test]
fn no_event_when_signer_set_is_malformed() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // No signers at all.
    let empty: Vec<Address> = Vec::new(&env);
    assert_eq!(
        client.try_multisig_approval_init(&admin_addr, &empty, &1u32),
        Err(Ok(Error::MultiSigNoSigners))
    );

    // The same address twice.
    let duplicate = Address::generate(&env);
    let with_duplicate = vec![&env, duplicate.clone(), duplicate];
    assert_eq!(
        client.try_multisig_approval_init(&admin_addr, &with_duplicate, &1u32),
        Err(Ok(Error::MultiSigDuplicateSigner))
    );

    // One signer over the cap.
    let mut oversized = vec![&env];
    for _ in 0..33 {
        oversized.push_back(Address::generate(&env));
    }
    assert_eq!(
        client.try_multisig_approval_init(&admin_addr, &oversized, &1u32),
        Err(Ok(Error::MultiSigTooManySigners))
    );

    // None of the three rejections published an event or wrote a key.
    assert_eq!(msiginit_event_count(&env), 0);
    assert_eq!(stored_signers(&env, &contract_id), None);
    assert_eq!(stored_threshold(&env, &contract_id), None);
}

/// Only the stored admin may register the signer set: `require_admin` runs
/// first, so every other caller is rejected before the writes and no `msiginit`
/// event is published.
#[test]
fn no_event_when_caller_is_not_the_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let stranger = Address::generate(&env);

    let signer = Address::generate(&env);
    let signers = vec![&env, signer];

    for caller in [&client_addr, &freelancer_addr, &arbiter_addr, &stranger] {
        assert_eq!(
            client.try_multisig_approval_init(caller, &signers, &1u32),
            Err(Ok(Error::Unauthorized)),
            "only the admin may register the multisig signer set"
        );
        assert_eq!(msiginit_event_count(&env), 0);
    }

    // The rejected calls left nothing behind.
    assert_eq!(stored_signers(&env, &contract_id), None);
    assert_eq!(stored_threshold(&env, &contract_id), None);

    // The admin's own registration still works afterwards.
    client.multisig_approval_init(&admin_addr, &signers, &1u32);
    assert_eq!(msiginit_event_count(&env), 1);
    let ev = last_msiginit_event(&env);
    assert_eq!(ev.admin, admin_addr);
    assert_event_reconciles(&env, &ev, &contract_id);
}

/// `require_admin` starts with `admin.require_auth()`, so a call carrying no
/// admin signature is rejected by the host before any contract logic runs — and
/// therefore before any event can be published.
#[test]
fn no_event_when_the_admin_signature_is_missing() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Drop every mocked auth entry so the host enforces real authorization.
    env.set_auths(&[]);

    let signer = Address::generate(&env);
    let signers = vec![&env, signer];

    let result = client.try_multisig_approval_init(&admin_addr, &signers, &1u32);
    assert!(
        matches!(result, Err(Err(_))),
        "a call without the admin signature must fail at the host level"
    );

    assert_eq!(msiginit_event_count(&env), 0);
    assert_eq!(stored_signers(&env, &contract_id), None);
    assert_eq!(stored_threshold(&env, &contract_id), None);
}

/// A rejected re-registration adds no second event: the ledger keeps exactly
/// the record of the registration that actually took, and the state from that
/// call is untouched.
#[test]
fn no_event_on_rejected_reinitialization() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, _arbiter_addr, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let signer_a = Address::generate(&env);
    let signer_b = Address::generate(&env);
    let signers = vec![&env, signer_a.clone(), signer_b.clone()];
    client.multisig_approval_init(&admin_addr, &signers, &2u32);

    assert_eq!(msiginit_event_count(&env), 1);
    let ev = last_msiginit_event(&env);
    assert_event_reconciles(&env, &ev, &contract_id);

    // A second registration is illegal: the setup is one-time.
    let extra = Address::generate(&env);
    let new_signers = vec![&env, extra];
    assert_eq!(
        client.try_multisig_approval_init(&admin_addr, &new_signers, &1u32),
        Err(Ok(Error::AlreadyInitialized))
    );

    // The reverted invocation publishes no events of its own...
    assert_eq!(msiginit_event_count(&env), 0);

    // ... and the state from the successful registration survives, still
    // matching the event that announced it.
    assert_event_reconciles(&env, &ev, &contract_id);
    let stored = stored_signers(&env, &contract_id).unwrap();
    assert_eq!(
        stored.len(),
        2,
        "the rejected call must not overwrite the signer set"
    );
    assert_eq!(stored.get(0).unwrap(), signer_a);
    assert_eq!(stored.get(1).unwrap(), signer_b);
}
