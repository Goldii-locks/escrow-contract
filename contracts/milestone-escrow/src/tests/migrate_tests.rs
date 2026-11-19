#![cfg(test)]
//! Upgrade-then-migrate: `migrate` and the `MigrationRequired` guard.
//!
//! The full path is deploy vN, build up state, upgrade to vN+1 (whose
//! `Milestone` gained a field), migrate, carry on. `upgrade` cannot swap code
//! in the test host (`update_current_contract_wasm` needs an uploaded wasm),
//! so these tests run the vN+1 code from the start and write the vN state
//! directly: a funded escrow whose milestones are stored as `MilestoneV1` (no
//! `spec_hash`), whose whitelist sits in instance storage, and which carries
//! no `SchemaVersion` marker, exactly what a schema-1 deployment leaves
//! behind after `upgrade`.

use crate::test::setup_funded_escrow;
use crate::{
    DataKey, Error, MigratedEvent, Milestone, MilestoneEscrow, MilestoneEscrowClient,
    MilestoneStatus, MilestoneV1, SCHEMA_VERSION,
};
use soroban_sdk::testutils::{Address as _, EnvTestConfig};
use soroban_sdk::{symbol_short, vec, Address, BytesN, Env, Symbol, TryFromVal, Vec};

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

struct Escrow<'a> {
    contract_id: Address,
    admin: Address,
    client: Address,
    freelancer: Address,
    token: Address,
    escrow: MilestoneEscrowClient<'a>,
}

fn setup(env: &Env) -> Escrow<'_> {
    let (client, freelancer, _, admin, token, contract_id, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128, 2_000]);
    Escrow {
        contract_id,
        admin,
        client,
        freelancer,
        token,
        escrow,
    }
}

/// Rewrite the escrow's storage into the schema-1 layout.
fn revert_to_schema_1(env: &Env, contract_id: &Address) {
    env.as_contract(contract_id, || {
        let persistent = env.storage().persistent();
        let instance = env.storage().instance();
        for index in 0..2u32 {
            let key = DataKey::Milestone(index);
            let current: Milestone = persistent.get(&key).unwrap();
            persistent.set(
                &key,
                &MilestoneV1 {
                    amount: current.amount,
                    released_amount: current.released_amount,
                    status: current.status,
                    delivered_at: current.delivered_at,
                    funded: current.funded,
                },
            );
        }
        let whitelist: Vec<Address> = persistent.get(&DataKey::WhitelistedTokens).unwrap();
        persistent.remove(&DataKey::WhitelistedTokens);
        instance.set(&DataKey::WhitelistedTokens, &whitelist);
        instance.remove(&DataKey::SchemaVersion);
    });
}

fn migrate_events(env: &Env) -> std::vec::Vec<MigratedEvent> {
    let topic = symbol_short!("migrate");
    crate::all_event_tuples(env)
        .into_iter()
        .filter(|(_, topics, _)| {
            topics.len() == 1
                && Symbol::try_from_val(env, &topics.get(0).unwrap()).ok() == Some(topic.clone())
        })
        .map(|(_, _, data)| MigratedEvent::try_from_val(env, &data).unwrap())
        .collect()
}

#[test]
fn upgrade_then_migrate_then_normal_flow() {
    let env = test_env();
    let e = setup(&env);
    revert_to_schema_1(&env, &e.contract_id);

    // After the upgrade, before migrate: state changes are refused.
    assert_eq!(e.escrow.schema_version(), 1);
    assert_eq!(
        e.escrow.try_mark_delivered(&e.freelancer, &0),
        Err(Ok(Error::MigrationRequired))
    );
    assert_eq!(
        e.escrow.try_approve_milestone(&e.client, &0),
        Err(Ok(Error::MigrationRequired))
    );

    // Only the admin can migrate.
    let stranger = Address::generate(&env);
    assert_eq!(
        e.escrow.try_migrate(&stranger),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(e.escrow.schema_version(), 1);

    assert_eq!(e.escrow.migrate(&e.admin), SCHEMA_VERSION);
    assert_eq!(
        migrate_events(&env),
        std::vec![MigratedEvent {
            admin: e.admin.clone(),
            from_version: 1,
            to_version: SCHEMA_VERSION,
        }]
    );
    assert_eq!(e.escrow.schema_version(), SCHEMA_VERSION);

    // Milestones were rewritten in the new shape with their values intact.
    let job = e.escrow.get_job();
    assert_eq!(job.milestones.len(), 2);
    let zero = BytesN::from_array(&env, &[0u8; 32]);
    for (index, amount) in [(0u32, 1_000_i128), (1, 2_000)] {
        let m = job.milestones.get(index).unwrap();
        assert_eq!(m.amount, amount);
        assert_eq!(m.released_amount, 0);
        assert_eq!(m.status, MilestoneStatus::Pending);
        assert_eq!(m.spec_hash, zero);
    }
    // The whitelist moved back out of instance storage.
    env.as_contract(&e.contract_id, || {
        assert!(!env.storage().instance().has(&DataKey::WhitelistedTokens));
        assert!(env.storage().persistent().has(&DataKey::WhitelistedTokens));
    });
    assert!(e.escrow.is_token_whitelisted(&e.token));

    // Normal operation resumes.
    e.escrow.mark_delivered(&e.freelancer, &0);
    e.escrow.approve_milestone(&e.client, &0);
    let job = e.escrow.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
    assert_eq!(
        job.milestones.get(1).unwrap().status,
        MilestoneStatus::Pending
    );
}

#[test]
fn migrate_keeps_spec_hashes_already_present() {
    // A deployment created after #767 but before the schema marker: milestones
    // already have `spec_hash`, only the marker is missing.
    let env = test_env();
    let e = setup(&env);
    let original = e.escrow.get_job();
    env.as_contract(&e.contract_id, || {
        env.storage().instance().remove(&DataKey::SchemaVersion);
    });
    assert_eq!(
        e.escrow.try_mark_delivered(&e.freelancer, &0),
        Err(Ok(Error::MigrationRequired))
    );

    e.escrow.migrate(&e.admin);

    let job = e.escrow.get_job();
    for index in 0..2u32 {
        assert_eq!(
            job.milestones.get(index).unwrap().spec_hash,
            original.milestones.get(index).unwrap().spec_hash
        );
    }
    e.escrow.mark_delivered(&e.freelancer, &0);
}

#[test]
fn migrate_on_current_storage_is_already_migrated() {
    let env = test_env();
    let e = setup(&env);
    // `initialize` writes the current schema version.
    assert_eq!(e.escrow.schema_version(), SCHEMA_VERSION);
    assert_eq!(
        e.escrow.try_migrate(&e.admin),
        Err(Ok(Error::AlreadyMigrated))
    );

    revert_to_schema_1(&env, &e.contract_id);
    e.escrow.migrate(&e.admin);
    assert_eq!(
        e.escrow.try_migrate(&e.admin),
        Err(Ok(Error::AlreadyMigrated))
    );
}

#[test]
fn reads_and_pause_controls_work_before_migrate() {
    let env = test_env();
    let e = setup(&env);
    revert_to_schema_1(&env, &e.contract_id);

    assert_eq!(
        e.escrow.version(),
        soroban_sdk::String::from_str(&env, crate::CONTRACT_VERSION)
    );
    assert!(!e.escrow.is_emergency_paused());
    // An incident during the upgrade window can still be contained.
    e.escrow.emergency_pause(&e.client, &e.freelancer);
    assert!(e.escrow.is_emergency_paused());
}

#[test]
fn uninitialized_contract_reports_not_initialized() {
    let env = test_env();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let someone = Address::generate(&env);

    assert_eq!(escrow.schema_version(), 1);
    assert_eq!(escrow.try_migrate(&someone), Err(Ok(Error::NotInitialized)));
    assert_eq!(
        escrow.try_accept_job(&someone),
        Err(Ok(Error::NotInitialized))
    );
}
