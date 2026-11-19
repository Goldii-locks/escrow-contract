#![cfg(test)]
//! `DisputeInfo::raised_by` (#997): who raised the dispute is recorded, and
//! disputes stored by schema 3 are migrated to schema 4.

use super::*;
use soroban_sdk::{vec, Env};

use crate::{DataKey, DisputeInfoV3, Error, SCHEMA_VERSION};

#[test]
fn raised_by_is_the_client_when_the_client_raises() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(&env));

    assert_eq!(escrow.get_dispute_info(&0).raised_by, client_addr);
}

#[test]
fn raised_by_is_the_freelancer_when_the_freelancer_raises() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.raise_dispute(&freelancer_addr, &0, &dispute_reason_hash(&env));

    assert_eq!(escrow.get_dispute_info(&0).raised_by, freelancer_addr);
}

#[test]
fn evidence_does_not_change_who_raised_the_dispute() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.raise_dispute(&client_addr, &0, &dispute_reason_hash(&env));
    escrow.submit_evidence(
        &freelancer_addr,
        &0,
        &soroban_sdk::BytesN::from_array(&env, &[7u8; 32]),
    );

    let info = escrow.get_dispute_info(&0);
    assert_eq!(info.raised_by, client_addr);
    assert_eq!(info.evidence.len(), 1);
}

#[test]
fn migrate_from_schema_3_stamps_open_disputes_with_the_client() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.raise_dispute(&freelancer_addr, &0, &dispute_reason_hash(&env));
    let current = escrow.get_dispute_info(&0);

    // Rewrite storage into the schema-3 layout (no `raised_by`).
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::DisputeInfo(0),
            &DisputeInfoV3 {
                disputed_at: current.disputed_at,
                previous_status: current.previous_status.clone(),
                reason_hash: current.reason_hash.clone(),
                evidence: current.evidence.clone(),
            },
        );
        env.storage().instance().set(&DataKey::SchemaVersion, &3u32);
    });

    // State changes are refused until `migrate`, but the getter still reads
    // the old record.
    assert_eq!(
        escrow.try_submit_evidence(
            &client_addr,
            &0,
            &soroban_sdk::BytesN::from_array(&env, &[1u8; 32])
        ),
        Err(Ok(Error::MigrationRequired))
    );
    assert_eq!(escrow.get_dispute_info(&0).raised_by, client_addr);

    assert_eq!(escrow.migrate(&admin_addr), SCHEMA_VERSION);

    let migrated = escrow.get_dispute_info(&0);
    assert_eq!(migrated.raised_by, client_addr);
    assert_eq!(migrated.disputed_at, current.disputed_at);
    assert_eq!(migrated.reason_hash, current.reason_hash);
    assert_eq!(migrated.previous_status, current.previous_status);
}

#[test]
fn migrate_leaves_a_record_that_already_has_raised_by_alone() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, freelancer_addr, _, admin_addr, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);
    escrow.raise_dispute(&freelancer_addr, &0, &dispute_reason_hash(&env));

    // Only the schema marker is lowered; the dispute is already in the v4 shape.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::SchemaVersion, &3u32);
    });
    assert_eq!(escrow.migrate(&admin_addr), SCHEMA_VERSION);

    assert_eq!(escrow.get_dispute_info(&0).raised_by, freelancer_addr);
}
