#![cfg(test)]
//! Regression suite for `revoke_cancel_approval`, covering issues #556 and
//! #557.
//!
//! # #556 — storage footprint
//! A revoke must not leave a zero-mask `DataKey::CancelApproval` entry behind:
//! the key is removed, so the contract's instance storage returns to exactly
//! the key count and byte size it had before the approval was recorded.  The
//! footprint is measured from the contract-instance ledger entry in a full
//! ledger snapshot, so any stray key or oversized value shows up as a diff.
//!
//! # #557 — uninitialized contract
//! Calling `revoke_cancel_approval` before `initialize` must return the typed
//! `Error::NotInitialized` (the crate's canonical "uninitialized" variant)
//! rather than panicking, must not demand a signature first, and must leave
//! the ledger untouched.

use super::*;
use crate::{DataKey, Error};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{vec, Address, Env};

/// Key count and XDR byte size of `contract_id`'s instance storage entry.
fn instance_footprint(env: &Env, contract_id: &Address) -> (usize, usize) {
    use soroban_sdk::xdr::{LedgerEntryData, Limits, ScVal, WriteXdr};
    use soroban_sdk::TryFromVal;

    let snapshot = env.to_ledger_snapshot();
    for (_key, (entry, _live_until)) in snapshot.ledger_entries.iter() {
        let LedgerEntryData::ContractData(data) = &entry.data else {
            continue;
        };
        if data.key != ScVal::LedgerKeyContractInstance {
            continue;
        }
        let owner = Address::try_from_val(env, &ScVal::Address(data.contract.clone()))
            .expect("instance entry owner is not a valid address");
        if &owner != contract_id {
            continue;
        }
        let ScVal::ContractInstance(instance) = &data.val else {
            continue;
        };
        let keys = instance.storage.as_ref().map(|m| m.0.len()).unwrap_or(0);
        let bytes = entry
            .to_xdr(Limits::none())
            .expect("instance entry is not serializable")
            .len();
        return (keys, bytes);
    }
    panic!("no instance entry found for contract");
}

fn has_cancel_approval(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage().instance().has(&DataKey::CancelApproval)
    })
}

// ── #556: storage footprint ──────────────────────────────────────────────────

/// Recording an approval grows instance storage by exactly one key; revoking
/// it shrinks storage back to the pre-approval baseline, both in key count
/// and in serialized bytes.
#[test]
fn revoke_restores_instance_footprint_to_baseline() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);

    let (baseline_keys, baseline_bytes) = instance_footprint(&env, &contract_id);

    client.cancel_escrow(&client_addr);
    let (approved_keys, approved_bytes) = instance_footprint(&env, &contract_id);
    assert_eq!(approved_keys, baseline_keys + 1);
    assert!(approved_bytes > baseline_bytes);

    client.revoke_cancel_approval(&client_addr);
    let (revoked_keys, revoked_bytes) = instance_footprint(&env, &contract_id);

    assert!(!has_cancel_approval(&env, &contract_id));
    assert_eq!(revoked_keys, baseline_keys, "revoke must not leave a key behind");
    assert_eq!(
        revoked_bytes, baseline_bytes,
        "revoke must not leave any bytes behind"
    );
    assert!(revoked_keys < approved_keys);
    assert!(revoked_bytes < approved_bytes);
}

/// Same guarantee from the freelancer side (bit `2`).
#[test]
fn freelancer_revoke_restores_instance_footprint_to_baseline() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);

    let baseline = instance_footprint(&env, &contract_id);

    client.cancel_escrow(&freelancer_addr);
    client.revoke_cancel_approval(&freelancer_addr);

    assert!(!has_cancel_approval(&env, &contract_id));
    assert_eq!(instance_footprint(&env, &contract_id), baseline);
}

/// Repeated approve / revoke cycles never accumulate storage.
#[test]
fn repeated_approve_revoke_cycles_do_not_grow_footprint() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1000_i128]);

    let baseline = instance_footprint(&env, &contract_id);

    for _ in 0..3 {
        client.cancel_escrow(&client_addr);
        client.revoke_cancel_approval(&client_addr);
        client.cancel_escrow(&freelancer_addr);
        client.revoke_cancel_approval(&freelancer_addr);
    }

    assert_eq!(instance_footprint(&env, &contract_id), baseline);
}

/// Rejected revokes write nothing: the whole ledger is unchanged.
#[test]
fn rejected_revoke_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1000_i128]);

    let before = env.to_ledger_snapshot();
    let res = client.try_revoke_cancel_approval(&client_addr);
    let after = env.to_ledger_snapshot();

    assert_eq!(res, Err(Ok(Error::InvalidStatus)));
    assert_eq!(before, after);
}

// ── #557: uninitialized contract ─────────────────────────────────────────────

/// On an uninitialized contract the call returns the typed error without any
/// mocked auth — proving the guard runs before `require_auth` and nothing
/// panics.
#[test]
fn revoke_on_uninitialized_contract_returns_not_initialized() {
    let env = Env::default();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    let res = client.try_revoke_cancel_approval(&caller);
    assert_eq!(res, Err(Ok(Error::NotInitialized)));
}

/// The uninitialized path mutates zero storage entries.
#[test]
fn revoke_on_uninitialized_contract_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    let before = env.to_ledger_snapshot();
    let res = client.try_revoke_cancel_approval(&caller);
    let after = env.to_ledger_snapshot();

    assert_eq!(res, Err(Ok(Error::NotInitialized)));
    assert_eq!(before, after, "uninitialized revoke must not write storage");
    assert!(!has_cancel_approval(&env, &contract_id));
}

/// The error is stable across repeated calls.
#[test]
fn revoke_on_uninitialized_contract_is_consistent() {
    let env = Env::default();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let caller = Address::generate(&env);

    for _ in 0..3 {
        assert_eq!(
            client.try_revoke_cancel_approval(&caller),
            Err(Ok(Error::NotInitialized))
        );
    }
}

/// The zero-address check still takes precedence over the init guard.
#[test]
fn revoke_zero_address_on_uninitialized_contract_is_invalid_address() {
    let env = Env::default();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let zero = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );

    assert_eq!(
        client.try_revoke_cancel_approval(&zero),
        Err(Ok(Error::InvalidAddress))
    );
}
