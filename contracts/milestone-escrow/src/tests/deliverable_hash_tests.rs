#![cfg(test)]
//! `mark_delivered_with_hash`: the deliverable hash is stored on the
//! milestone, returned by `get_job` and emitted in `DeliveredEvent`, while the
//! legacy `mark_delivered` path records an all-zero hash.

use crate::test::setup_funded_escrow;
use crate::{DataKey, DeliveredEvent, Error, MilestoneStatus, MilestoneV2, SCHEMA_VERSION};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, vec, BytesN, Env, FromVal, Symbol, TryIntoVal};

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

fn last_delivered_event(env: &Env) -> DeliveredEvent {
    let events = crate::all_event_tuples(env);
    let (_, topics, data) = events.last().unwrap();
    let topic: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, symbol_short!("deliver"));
    DeliveredEvent::from_val(env, data)
}

#[test]
fn mark_delivered_with_hash_stores_hash_and_emits_event() {
    let env = test_env();
    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);

    let hash = BytesN::from_array(&env, &[0xAB; 32]);
    client.mark_delivered_with_hash(&freelancer_addr, &1u32, &hash);

    // Read the event before any further call: the test env keeps only the
    // events of the most recent invocation.
    let event = last_delivered_event(&env);
    assert_eq!(event.deliverable, hash);
    assert_eq!(event.milestone_index, 1);
    assert_eq!(event.freelancer, freelancer_addr);
    assert_eq!(event.client, client_addr);

    let job = client.get_job();
    let delivered = job.milestones.get(1).unwrap();
    assert_eq!(delivered.status, MilestoneStatus::Delivered);
    assert_eq!(delivered.deliverable, hash);
    // The untouched milestone keeps the zero hash.
    assert_eq!(
        job.milestones.get(0).unwrap().deliverable,
        BytesN::from_array(&env, &[0u8; 32])
    );
}

#[test]
fn legacy_mark_delivered_records_zero_hash() {
    let env = test_env();
    let (_, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);

    let zero = BytesN::from_array(&env, &[0u8; 32]);
    assert_eq!(last_delivered_event(&env).deliverable, zero);
    let milestone = client.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Delivered);
    assert_eq!(milestone.deliverable, zero);
}

#[test]
fn mark_delivered_with_hash_rejects_non_pending() {
    let env = test_env();
    let (_, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let first = BytesN::from_array(&env, &[1u8; 32]);
    let second = BytesN::from_array(&env, &[2u8; 32]);
    client.mark_delivered_with_hash(&freelancer_addr, &0u32, &first);

    // A second delivery cannot overwrite the recorded hash.
    assert_eq!(
        client.try_mark_delivered_with_hash(&freelancer_addr, &0u32, &second),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().deliverable,
        first
    );
}

#[test]
fn mark_delivered_with_hash_rejects_non_freelancer() {
    let env = test_env();
    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let hash = BytesN::from_array(&env, &[3u8; 32]);
    assert_eq!(
        client.try_mark_delivered_with_hash(&client_addr, &0u32, &hash),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn migrate_from_schema_2_adds_zero_deliverable() {
    let env = test_env();
    let (_, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let original = client.get_job().milestones.get(0).unwrap();

    // Rewrite storage into the schema-2 layout (no `deliverable`).
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Milestone(0),
            &MilestoneV2 {
                amount: original.amount,
                released_amount: original.released_amount,
                status: original.status.clone(),
                delivered_at: original.delivered_at,
                funded: original.funded,
                spec_hash: original.spec_hash.clone(),
            },
        );
        env.storage().instance().set(&DataKey::SchemaVersion, &2u32);
    });
    assert_eq!(
        client.try_mark_delivered(&freelancer_addr, &0u32),
        Err(Ok(Error::MigrationRequired))
    );

    assert_eq!(client.migrate(&admin_addr), SCHEMA_VERSION);

    let migrated = client.get_job().milestones.get(0).unwrap();
    assert_eq!(migrated.spec_hash, original.spec_hash);
    assert_eq!(migrated.deliverable, BytesN::from_array(&env, &[0u8; 32]));

    let hash = BytesN::from_array(&env, &[4u8; 32]);
    client.mark_delivered_with_hash(&freelancer_addr, &0u32, &hash);
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().deliverable,
        hash
    );
}
