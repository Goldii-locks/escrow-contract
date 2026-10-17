#![cfg(test)]
//! Upgrade compatibility for the consolidated multisig configuration
//! (issue #456).
//!
//! Before #456, `multisig_approval_init` stored the signer set and threshold
//! under two instance keys, `MultiSigSigners` and `MultiSigThreshold`; it now
//! writes one `MultiSigConfig` entry. A contract initialised before the change
//! and then upgraded in place still holds the legacy pair, so these tests seed
//! exactly that layout and check that the multisig regime keeps working and
//! cannot be re-initialised.

use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MultiSigConfig};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{vec, Address, Env, Vec};

/// A funded escrow whose multisig regime is stored in the pre-#456 layout.
fn escrow_with_legacy_multisig(
    env: &Env,
) -> (Address, crate::MilestoneEscrowClient<'_>, Vec<Address>) {
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    let signers = vec![env, Address::generate(env), Address::generate(env)];
    env.as_contract(&contract_id, || {
        let storage = env.storage().instance();
        storage.set(&DataKey::MultiSigSigners, &signers);
        storage.set(&DataKey::MultiSigThreshold, &2u32);
    });
    (contract_id, escrow, signers)
}

#[test]
fn legacy_layout_still_counts_approvals_to_the_threshold() {
    let env = Env::default();
    let (_, escrow, signers) = escrow_with_legacy_multisig(&env);

    let first = escrow.multisig_approve(&signers.get(0).unwrap(), &7);
    assert_eq!(
        (first.approvals, first.threshold, first.approved),
        (1, 2, false)
    );

    let second = escrow.multisig_approve(&signers.get(1).unwrap(), &7);
    assert_eq!(
        (second.approvals, second.threshold, second.approved),
        (2, 2, true)
    );

    let read = escrow.is_multisig_approved(&7);
    assert_eq!(
        (read.approvals, read.threshold, read.approved),
        (2, 2, true)
    );
}

#[test]
fn legacy_layout_still_rejects_non_signers() {
    let env = Env::default();
    let (_, escrow, _) = escrow_with_legacy_multisig(&env);

    let outsider = Address::generate(&env);
    assert_eq!(
        escrow.try_multisig_approve(&outsider, &7),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn legacy_layout_cannot_be_reinitialised() {
    let env = Env::default();
    let (contract_id, escrow, _) = escrow_with_legacy_multisig(&env);
    let admin: Address = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::Admin).unwrap()
    });

    let replacement = vec![&env, Address::generate(&env)];
    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &replacement, &1),
        Err(Ok(Error::AlreadyInitialized))
    );
    let config: Option<MultiSigConfig> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::MultiSigConfig)
    });
    assert_eq!(
        config, None,
        "a rejected re-init writes no consolidated entry"
    );
}
