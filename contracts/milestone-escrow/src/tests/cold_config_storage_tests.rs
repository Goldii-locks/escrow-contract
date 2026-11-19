#![cfg(test)]
//! Cold configuration out of instance storage (issue #772).
//!
//! The contract-instance ledger entry is loaded in full on every invocation.
//! `WhitelistedTokens` (up to 50 addresses), `MultiSigConfig` (up to 32
//! signers), `InterestYieldState` and `PlatformFeeAllocation` now live under
//! persistent keys read only by the functions that use them, and `migrate`
//! moves them out of instance storage on a deployment upgraded from the old
//! layout.
//!
//! The cost test compares `approve_milestone` — which needs only `Job` and one
//! `Milestone` — on two identically configured escrows (50-token whitelist,
//! 32-signer multisig): one still in the legacy all-instance layout, one
//! migrated. The bytes the call has to load for the instance entry, and the
//! metered work, must both go down.

use crate::test::setup_funded_escrow;
use crate::{
    DataKey, Error, EscrowInterestYieldState, MilestoneEscrowClient, MultiSigConfig,
    PlatformFeeAllocation, SCHEMA_VERSION,
};
use soroban_sdk::testutils::{Address as _, EnvTestConfig};
use soroban_sdk::xdr::{LedgerEntryData, Limits, ScAddress, ScVal, WriteXdr};
use soroban_sdk::{vec, Address, Env, Vec};

const WHITELIST_LEN: u32 = 50;
const SIGNERS_LEN: u32 = 32;

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

struct Escrow<'a> {
    contract_id: Address,
    admin: Address,
    client: Address,
    freelancer: Address,
    escrow: MilestoneEscrowClient<'a>,
}

/// Funded two-milestone escrow with a full 50-token whitelist and a 32-signer
/// multisig regime, in the current (persistent) layout.
fn setup(env: &Env) -> Escrow<'_> {
    env.mock_all_auths();
    let (client, freelancer, _, admin, token, contract_id, escrow) =
        setup_funded_escrow(env, vec![env, 500_i128, 500_i128]);

    let mut whitelist = vec![env, token];
    while whitelist.len() < WHITELIST_LEN {
        whitelist.push_back(Address::generate(env));
    }
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedTokens, &whitelist);
    });

    let mut signers = Vec::new(env);
    for _ in 0..SIGNERS_LEN {
        signers.push_back(Address::generate(env));
    }
    escrow.multisig_approval_init(&admin, &signers, &2u32);

    Escrow {
        contract_id,
        admin,
        client,
        freelancer,
        escrow,
    }
}

/// Rewrite `contract_id`'s cold configuration into the pre-#772 layout: every
/// entry back inside the contract-instance entry, none persistent.
fn revert_to_legacy_layout(env: &Env, contract_id: &Address) {
    env.as_contract(contract_id, || {
        let persistent = env.storage().persistent();
        let instance = env.storage().instance();
        let whitelist: Vec<Address> = persistent.get(&DataKey::WhitelistedTokens).unwrap();
        let multisig: MultiSigConfig = persistent.get(&DataKey::MultiSigConfig).unwrap();
        let fee: PlatformFeeAllocation = persistent.get(&DataKey::PlatformFeeAllocation).unwrap();
        persistent.remove(&DataKey::WhitelistedTokens);
        persistent.remove(&DataKey::MultiSigConfig);
        persistent.remove(&DataKey::PlatformFeeAllocation);
        instance.set(&DataKey::WhitelistedTokens, &whitelist);
        instance.set(&DataKey::MultiSigConfig, &multisig);
        instance.set(&DataKey::PlatformFeeAllocation, &fee);
    });
}

/// Drop the `SchemaVersion` marker, as a deployment from before #772 has none,
/// so `migrate` runs the `1 → 2` cold-config move.
fn revert_to_schema_1(env: &Env, contract_id: &Address) {
    env.as_contract(contract_id, || {
        env.storage().instance().remove(&DataKey::SchemaVersion);
    });
}

/// XDR size of the escrow's contract-instance ledger entry: what every
/// invocation of the escrow has to load.
fn instance_entry_bytes(env: &Env, contract_id: &Address) -> u32 {
    let contract = ScAddress::from(contract_id);
    let snapshot = env.to_ledger_snapshot();
    for (_, (entry, _)) in snapshot.ledger_entries.iter() {
        if let LedgerEntryData::ContractData(cd) = &entry.data {
            if cd.contract == contract && matches!(cd.val, ScVal::ContractInstance(_)) {
                return entry.to_xdr(Limits::none()).unwrap().len() as u32;
            }
        }
    }
    panic!("no contract-instance entry for the escrow");
}

fn instance_has(env: &Env, contract_id: &Address, key: &DataKey) -> bool {
    env.as_contract(contract_id, || env.storage().instance().has(key))
}

fn persistent_has(env: &Env, contract_id: &Address, key: &DataKey) -> bool {
    env.as_contract(contract_id, || env.storage().persistent().has(key))
}

// ── approve_milestone cost ───────────────────────────────────────────────────

#[test]
fn approve_milestone_costs_less_once_cold_config_leaves_instance_storage() {
    // Legacy layout: whitelist and multisig config inside the instance entry.
    let env_legacy = test_env();
    let legacy = setup(&env_legacy);
    revert_to_legacy_layout(&env_legacy, &legacy.contract_id);
    legacy.escrow.mark_delivered(&legacy.freelancer, &0);
    let legacy_instance_bytes = instance_entry_bytes(&env_legacy, &legacy.contract_id);
    legacy.escrow.approve_milestone(&legacy.client, &0);
    let legacy_cost = env_legacy.cost_estimate().resources();

    // Same configuration, persistent layout.
    let env_new = test_env();
    let new = setup(&env_new);
    new.escrow.mark_delivered(&new.freelancer, &0);
    let new_instance_bytes = instance_entry_bytes(&env_new, &new.contract_id);
    new.escrow.approve_milestone(&new.client, &0);
    let new_cost = env_new.cost_estimate().resources();

    // 50 + 32 addresses no longer ride along in the entry every call loads.
    assert!(
        legacy_instance_bytes - new_instance_bytes > (WHITELIST_LEN + SIGNERS_LEN) * 32,
        "instance entry read by approve_milestone: legacy {legacy_instance_bytes} \
         bytes, migrated {new_instance_bytes} bytes"
    );
    // The call reads no extra entry for it: it never touches the cold keys.
    assert_eq!(
        new_cost.memory_read_entries, legacy_cost.memory_read_entries,
        "legacy {legacy_cost:?}\nmigrated {new_cost:?}"
    );
    // Less to deserialize means less metered work.
    assert!(
        new_cost.instructions < legacy_cost.instructions,
        "legacy {legacy_cost:?}\nmigrated {new_cost:?}"
    );
    assert!(
        new_cost.mem_bytes < legacy_cost.mem_bytes,
        "legacy {legacy_cost:?}\nmigrated {new_cost:?}"
    );
}

// ── layout written by the current code ──────────────────────────────────────

#[test]
fn cold_config_is_written_to_persistent_storage_only() {
    let env = test_env();
    let e = setup(&env);
    e.escrow
        .set_platform_fee_allocation(&e.admin, &0u32, &9_000u32, &1_000u32);

    for (name, key) in [
        ("WhitelistedTokens", DataKey::WhitelistedTokens),
        ("MultiSigConfig", DataKey::MultiSigConfig),
        ("PlatformFeeAllocation", DataKey::PlatformFeeAllocation),
    ] {
        assert!(persistent_has(&env, &e.contract_id, &key), "{name}");
        assert!(!instance_has(&env, &e.contract_id, &key), "{name}");
    }
    // The hot keys stay in instance storage.
    for (name, key) in [
        ("Job", DataKey::Job),
        ("Admin", DataKey::Admin),
        ("Ep", DataKey::Ep),
    ] {
        assert!(instance_has(&env, &e.contract_id, &key), "{name}");
    }
}

// ── migrate: cold-config move ────────────────────────────────────────────────

#[test]
fn legacy_layout_keeps_working_before_migration() {
    let env = test_env();
    let e = setup(&env);
    let whitelist = e.escrow.get_whitelisted_tokens();
    revert_to_legacy_layout(&env, &e.contract_id);

    // Reads fall back to the instance copy until the admin migrates.
    assert_eq!(e.escrow.get_whitelisted_tokens(), whitelist);
    assert_eq!(
        e.escrow.get_platform_fee_allocation().freelancer_bps,
        10_000
    );
    assert_eq!(
        e.escrow
            .try_multisig_approval_init(&e.admin, &whitelist, &1u32),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn migrate_moves_cold_config_out_of_instance_storage() {
    let env = test_env();
    let e = setup(&env);
    let whitelist = e.escrow.get_whitelisted_tokens();
    let fee = e.escrow.get_platform_fee_allocation();
    revert_to_legacy_layout(&env, &e.contract_id);
    env.as_contract(&e.contract_id, || {
        env.storage().instance().set(
            &DataKey::InterestYieldState,
            &EscrowInterestYieldState {
                client_share_bps: 4_000,
                freelancer_share_bps: 6_000,
                locked: false,
            },
        );
    });
    revert_to_schema_1(&env, &e.contract_id);
    let bytes_before = instance_entry_bytes(&env, &e.contract_id);

    assert_eq!(e.escrow.migrate(&e.admin), SCHEMA_VERSION);

    for (name, key) in [
        ("WhitelistedTokens", DataKey::WhitelistedTokens),
        ("MultiSigConfig", DataKey::MultiSigConfig),
        ("InterestYieldState", DataKey::InterestYieldState),
        ("PlatformFeeAllocation", DataKey::PlatformFeeAllocation),
    ] {
        assert!(persistent_has(&env, &e.contract_id, &key), "{name}");
        assert!(!instance_has(&env, &e.contract_id, &key), "{name}");
    }
    assert!(instance_entry_bytes(&env, &e.contract_id) < bytes_before);

    // Values survive the move.
    assert_eq!(e.escrow.get_whitelisted_tokens(), whitelist);
    assert_eq!(e.escrow.get_platform_fee_allocation(), fee);
    assert_eq!(e.escrow.get_escrow_interest_yield().client_share_bps, 4_000);

    // A second call is refused: storage is already at `SCHEMA_VERSION`.
    assert_eq!(
        e.escrow.try_migrate(&e.admin),
        Err(Ok(Error::AlreadyMigrated))
    );
}

#[test]
fn migrate_keeps_a_newer_persistent_value() {
    let env = test_env();
    let e = setup(&env);
    let current = e.escrow.get_whitelisted_tokens();
    let stale = vec![&env, Address::generate(&env)];
    env.as_contract(&e.contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &stale);
    });
    revert_to_schema_1(&env, &e.contract_id);

    assert_eq!(e.escrow.migrate(&e.admin), SCHEMA_VERSION);
    assert_eq!(e.escrow.get_whitelisted_tokens(), current);
    assert!(!instance_has(
        &env,
        &e.contract_id,
        &DataKey::WhitelistedTokens
    ));
}

#[test]
fn migrate_folds_the_pre_456_multisig_keys() {
    let env = test_env();
    env.mock_all_auths();
    let (_, _, _, admin, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let signers = vec![&env, Address::generate(&env), Address::generate(&env)];
    env.as_contract(&contract_id, || {
        let instance = env.storage().instance();
        instance.set(&DataKey::MultiSigSigners, &signers);
        instance.set(&DataKey::MultiSigThreshold, &2u32);
    });
    revert_to_schema_1(&env, &contract_id);

    assert_eq!(escrow.migrate(&admin), SCHEMA_VERSION);

    let config: Option<MultiSigConfig> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::MultiSigConfig)
    });
    assert_eq!(config, Some(MultiSigConfig(signers.clone(), 2)));
    assert!(!instance_has(&env, &contract_id, &DataKey::MultiSigSigners));
    assert!(!instance_has(
        &env,
        &contract_id,
        &DataKey::MultiSigThreshold
    ));
    crate::tests::seed_admin_proposal(&env, &escrow.address, 1);

    let state = escrow.multisig_approve(&signers.get(0).unwrap(), &1);
    assert_eq!((state.approvals, state.threshold), (1, 2));
}

#[test]
fn migrate_is_admin_only() {
    let env = test_env();
    let e = setup(&env);
    revert_to_legacy_layout(&env, &e.contract_id);
    revert_to_schema_1(&env, &e.contract_id);

    let outsider = Address::generate(&env);
    assert_eq!(
        e.escrow.try_migrate(&outsider),
        Err(Ok(Error::Unauthorized))
    );
    assert!(instance_has(
        &env,
        &e.contract_id,
        &DataKey::WhitelistedTokens
    ));
}
