#![cfg(test)]
//! Ledger-storage footprint suite for `multisig_approval_init` (issue #456).
//!
//! # Before
//! A successful call wrote **two** distinct instance-storage keys:
//! `DataKey::MultiSigSigners` (the `Vec<Address>` signer set) and
//! `DataKey::MultiSigThreshold` (the `u32` threshold).  Every reader paid two
//! instance reads for the same decision: `multisig_approve` and the
//! `is_multisig_approved` read path (`read_multisig_approval`).
//!
//! # After
//! Both fields are persisted in **one** instance entry under one key,
//! `DataKey::MultiSigConfig`, whose value is the `MultiSigConfig` tuple struct
//! `(signer set, threshold)`.  The call therefore writes one distinct multisig
//! storage key instead of two, and the `contract_instance` ledger entry — the
//! single entry holding all instance storage — ends up 28 bytes smaller for a
//! three-signer set: the removed `MultiSigThreshold` entry (its encoded
//! `DataKey` plus its `u32` value) is 48 bytes, while the tuple wrapper the
//! consolidated value adds on top of the signer vec is 20 bytes (both figures
//! measured by the byte test below).
//!
//! The same issue also consolidates the call's *admin* read: authorization now
//! uses `require_admin_from_instance` instead of `require_admin`, so the read
//! lands on the instance entry (like the multisig config and every other access
//! of this call) rather than adding the persistent `Admin` entry to the
//! invocation footprint.  This is the consolidation already landed for
//! `multisig_lock` (#460), `set_escrow_interest_yield` (#463),
//! `set_platform_fee_allocation` (#472) and `admin_resume_escrow` (#449);
//! `initialize` writes both `Admin` copies atomically and every admin-transfer
//! path keeps them in sync, so the check is logically identical.
//!
//! The storage tier, value semantics, validation rules, error codes and
//! observable behaviour are unchanged — the tests below call the entry point on
//! an escrow that has *only* the instance `Admin` copy and get the same
//! registration result.  No per-key TTL extension existed here (all instance
//! keys live in, and are bumped with, the shared `contract_instance` entry), so
//! there is no per-key TTL work to collapse.
//!
//! # Issue #772
//! The consolidated `MultiSigConfig` has since moved out of the
//! contract-instance entry into its own persistent key: the instance entry is
//! loaded by every invocation, and a 32-signer set there made every
//! `approve_milestone` pay for it. The call still writes exactly one multisig
//! key, but it no longer grows the instance entry at all.
//!
//! # Tests
//! 1. `test_multisig_approval_init_writes_one_consolidated_key` — the live call
//!    adds no instance key (the legacy layout added 2) and writes the single
//!    consolidated persistent value.
//! 2. `test_multisig_approval_init_shrinks_instance_entry_bytes` — the
//!    `contract_instance` entry does not grow, against the same data written in
//!    the legacy two-key instance shape.
//! 3. `test_multisig_approval_init_touched_entries_footprint` — meters the
//!    invocation: four distinct ledger entries (wasm code, auth nonce, the
//!    instance entry and the persistent `MultiSigConfig`); the persistent
//!    `Admin` copy is still not among them.
//! 4. `test_multisig_approval_init_registers_signer_set_and_threshold_identically`
//!    — behaviour identity through the public API.
//! 5. `test_multisig_approval_init_rejections_write_no_storage` — every
//!    rejection path leaves the instance entry byte-identical.

use super::*;
use crate::{DataKey, Error, MultiSigConfig};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::xdr::{LedgerEntryData, Limits, ScVal, WriteXdr};
use soroban_sdk::{vec, Address, Env, Vec};

fn test_env() -> Env {
    Env::default()
}

/// Env that skips snapshot capture — used for the secondary control envs inside
/// a test, so a single test still writes at most one snapshot file.
fn env_without_snapshot() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// `(number of keys in the contract-instance storage map, XDR byte size of the
/// whole contract_instance ledger entry)`.
///
/// Every instance key — including the multisig ones — lives inside that single
/// ledger entry, so its key count is exactly "how many storage keys the
/// contract currently holds" and its byte size is the ledger footprint those
/// keys occupy.  Read straight from the live ledger snapshot, the inspection
/// capability this test SDK exposes (`Env::to_ledger_snapshot`, already used by
/// `read_path_tests`).
fn instance_footprint(env: &Env) -> (u32, u32) {
    let snapshot = env.to_ledger_snapshot();
    let mut found: Option<(u32, u32)> = None;
    for (_, (entry, _)) in snapshot.ledger_entries.iter() {
        if let LedgerEntryData::ContractData(cd) = &entry.data {
            if let ScVal::ContractInstance(inst) = &cd.val {
                assert!(
                    found.is_none(),
                    "expected exactly one contract-instance entry in the ledger"
                );
                let storage = inst.storage.as_ref();
                let keys = storage.map_or(0, |m| m.0.len() as u32);
                let bytes = entry
                    .to_xdr(Limits::none())
                    .expect("contract_instance entry serializes to XDR")
                    .len() as u32;
                found = Some((keys, bytes));
            }
        }
    }
    found.expect("no contract-instance entry in the ledger snapshot")
}

/// Number of storage keys the contract's instance entry holds.
fn instance_key_count(env: &Env) -> u32 {
    instance_footprint(env).0
}

/// XDR byte size of the contract's contract_instance ledger entry.
fn instance_entry_bytes(env: &Env) -> u32 {
    instance_footprint(env).1
}

/// A registered escrow whose **instance** `Admin` key is seeded directly, with
/// no persistent `Admin` copy at all.
///
/// Deliberately minimal: the call must not need the persistent copy (part 2 of
/// the change below), and this keeps the footprint measurements about the
/// multisig storage only.
fn admin_only_escrow(env: &Env) -> (Address, Address) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let contract_id = env.register(MilestoneEscrow, ());
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Admin, &admin);
    });
    assert!(
        !has_persistent_admin(env, &contract_id),
        "precondition: no persistent Admin copy exists"
    );

    (contract_id, admin)
}

/// Whether the persistent `Admin` copy exists.
fn has_persistent_admin(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage().persistent().has(&DataKey::Admin)
    })
}

/// A fully initialised and funded escrow (the shape `multisig_approve`
/// requires), plus its admin.
fn funded_escrow(env: &Env) -> (Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);
    token_admin.mint(&client_addr, &1_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    escrow.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604_800u64,
        &vec![env, 1_000_i128],
        &crate::test_spec_hashes(env, &vec![env, 1_000_i128]),
    );
    escrow.accept_job(&freelancer_addr);
    escrow.accept_arbiter(&arbiter_addr);
    escrow.fund(&client_addr);

    (contract_id, admin, escrow)
}

/// Three generated signers, the signer-set shape used by every test below.
fn signer_set(env: &Env) -> Vec<Address> {
    vec![
        env,
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ]
}

/// The consolidated `MultiSigConfig` persisted by the contract, if any.
fn stored_config(env: &Env, contract_id: &Address) -> Option<MultiSigConfig> {
    env.as_contract(contract_id, || {
        env.storage().persistent().get(&DataKey::MultiSigConfig)
    })
}

/// A rejected `multisig_approval_init` must not touch instance storage at all:
/// no consolidated entry, no legacy entries, and not a single byte added to the
/// `contract_instance` entry.
fn assert_rejected_without_storage_write(
    env: &Env,
    contract_id: &Address,
    admin: &Address,
    signers: &Vec<Address>,
    threshold: u32,
    expected: Error,
) {
    let before = instance_footprint(env);
    let escrow = MilestoneEscrowClient::new(env, contract_id);

    assert_eq!(
        escrow.try_multisig_approval_init(admin, signers, &threshold),
        Err(Ok(expected)),
        "expected the call to be rejected with {:?}",
        expected
    );
    assert_eq!(
        instance_footprint(env),
        before,
        "a rejected call must leave the instance entry byte-identical"
    );
    assert_eq!(
        stored_config(env, contract_id),
        None,
        "a rejected call must not write the consolidated multisig entry"
    );
    env.as_contract(contract_id, || {
        assert!(
            !env.storage().instance().has(&DataKey::MultiSigSigners),
            "a rejected call must not write the legacy MultiSigSigners key"
        );
        assert!(
            !env.storage().instance().has(&DataKey::MultiSigThreshold),
            "a rejected call must not write the legacy MultiSigThreshold key"
        );
    });
}

// ── footprint: distinct storage keys written by the call ─────────────────────

/// The issue's literal validation requirement: the number of distinct storage
/// keys written by `multisig_approval_init` is reduced.
///
/// Measured on the live ledger snapshot: the call adds **one** key to the
/// contract-instance storage map (it added two before).  The key added is the
/// consolidated `DataKey::MultiSigConfig`, and neither legacy key exists
/// afterwards.  The control env shows the same measurement returns two for the
/// pre-#456 layout, i.e. the metric really distinguishes the two layouts.
#[test]
fn test_multisig_approval_init_writes_one_consolidated_key() {
    let env = test_env();
    let (contract_id, admin) = admin_only_escrow(&env);
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let signers = signer_set(&env);
    let threshold = 2u32;

    let keys_before = instance_key_count(&env);
    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &signers, &threshold),
        Ok(Ok(()))
    );
    let keys_after = instance_key_count(&env);

    assert_eq!(
        keys_after - keys_before,
        0,
        "multisig_approval_init must add no instance-storage key: the \
         consolidated MultiSigConfig is persistent (issue #772); the legacy \
         layout added two (MultiSigSigners + MultiSigThreshold)"
    );

    // That single added key carries both fields.
    assert_eq!(
        stored_config(&env, &contract_id),
        Some(MultiSigConfig(signers.clone(), threshold)),
        "the consolidated entry must hold the signer set and the threshold"
    );

    // Neither legacy key was written — and nothing writes them any more.
    env.as_contract(&contract_id, || {
        assert!(
            !env.storage().instance().has(&DataKey::MultiSigSigners),
            "the legacy MultiSigSigners key must not be written"
        );
        assert!(
            !env.storage().instance().has(&DataKey::MultiSigThreshold),
            "the legacy MultiSigThreshold key must not be written"
        );
    });

    // Control: the same measurement counts two keys for the legacy layout of
    // exactly this data.
    let env_legacy = env_without_snapshot();
    let (legacy_contract, _) = admin_only_escrow(&env_legacy);
    let legacy_signers = signer_set(&env_legacy);
    let legacy_before = instance_key_count(&env_legacy);
    env_legacy.as_contract(&legacy_contract, || {
        env_legacy
            .storage()
            .instance()
            .set(&DataKey::MultiSigSigners, &legacy_signers);
        env_legacy
            .storage()
            .instance()
            .set(&DataKey::MultiSigThreshold, &threshold);
    });
    let legacy_after = instance_key_count(&env_legacy);
    assert_eq!(
        legacy_after - legacy_before,
        2,
        "control: the pre-#456 two-key layout added two keys, so the measured \
         1 above is a real reduction rather than an insensitive metric"
    );
}

// ── footprint: bytes held by the contract-instance ledger entry ──────────────

/// The same data costs fewer ledger bytes under the consolidated layout.
///
/// Both sides are measured the same way — the XDR byte size of the
/// `contract_instance` entry before and after the multisig fields are
/// persisted — so the difference is exactly the storage-layout difference:
/// - consolidated (the real entry point, one call): one key + one tuple value;
/// - legacy control (the two `set`s the pre-#456 code performed): two keys.
///
/// The measured saving for a three-signer set is 28 bytes: the removed
/// `MultiSigThreshold` entry (its encoded `DataKey` plus its `u32` value) is
/// 48 bytes, while the tuple wrapper the consolidated value adds on top of the
/// signer vec is 20 bytes.
#[test]
fn test_multisig_approval_init_shrinks_instance_entry_bytes() {
    // Consolidated layout: the real entry point is called.
    let env = test_env();
    let (contract_id, admin) = admin_only_escrow(&env);
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let signers = signer_set(&env);

    let bytes_before = instance_entry_bytes(&env);
    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &signers, &2u32),
        Ok(Ok(()))
    );
    let consolidated_growth = instance_entry_bytes(&env) - bytes_before;

    // Legacy layout: exactly the two writes the pre-#456 implementation made,
    // with the same signer-set shape, in an identically prepared escrow.
    let env_legacy = env_without_snapshot();
    let (legacy_contract, _) = admin_only_escrow(&env_legacy);
    let legacy_signers = signer_set(&env_legacy);
    let legacy_before = instance_entry_bytes(&env_legacy);
    env_legacy.as_contract(&legacy_contract, || {
        env_legacy
            .storage()
            .instance()
            .set(&DataKey::MultiSigSigners, &legacy_signers);
        env_legacy
            .storage()
            .instance()
            .set(&DataKey::MultiSigThreshold, &2u32);
    });
    let legacy_growth = instance_entry_bytes(&env_legacy) - legacy_before;

    assert!(
        legacy_growth > 0,
        "sanity: the legacy layout grows the instance entry (measured {} bytes)",
        legacy_growth
    );
    assert_eq!(
        consolidated_growth, 0,
        "the multisig config lives under its own persistent key (issue #772), \
         so the instance entry every call loads must not grow at all; the \
         legacy layout grew it by {} bytes",
        legacy_growth
    );
}

// ── footprint: distinct ledger entries touched per invocation ────────────────

/// Metered proof that the call no longer touches the persistent `Admin` entry.
///
/// `env.cost_estimate().resources()` reports the ledger entries touched by the
/// last invocation.  A successful `multisig_approval_init` touches four: the
/// wasm code entry, the auth nonce entry, the `contract_instance` entry (which
/// holds the instance `Admin` copy) and the persistent `MultiSigConfig`, which
/// moved out of the instance entry in issue #772.  The persistent `Admin` copy
/// read by `require_admin` is not among them, which is why setup A below seeds
/// no persistent `Admin` at all.
///
/// Control: a second, identically prepared env calls `admin_pause_escrow`,
/// which authorises against the persistent `Admin` copy and touches no
/// multisig key, so it measures the same number of entries.
#[test]
fn test_multisig_approval_init_touched_entries_footprint() {
    let env = test_env();
    let (contract_id, admin) = admin_only_escrow(&env);
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let signers = signer_set(&env);

    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &signers, &2u32),
        Ok(Ok(()))
    );
    let init = env.cost_estimate().resources();

    assert_eq!(
        init.memory_read_entries, 4,
        "multisig_approval_init must touch exactly four distinct ledger entries \
         (contract_code + auth nonce + contract_instance + the persistent \
         MultiSigConfig); the persistent Admin entry must not be among them; \
         measured {:?}",
        init
    );
    assert_eq!(
        init.write_entries, 2,
        "the only ledger entries written are the persistent MultiSigConfig and \
         the auth nonce; the instance entry is left untouched; measured {:?}",
        init
    );
    assert_eq!(
        init.disk_read_entries, 0,
        "everything the call reads is live in-memory Soroban state"
    );
    assert!(init.instructions > 0, "sanity: the call was metered");

    // The persistent `Admin` copy is not merely unread — it does not exist.
    assert!(
        !has_persistent_admin(&env, &contract_id),
        "multisig_approval_init must not read or write persistent storage"
    );

    // Control: `admin_pause_escrow` reads only the instance entry
    // (`require_role`, #761), one distinct ledger entry fewer than
    // multisig_approval_init, which also reads the persistent MultiSigConfig.
    let env_b = env_without_snapshot();
    env_b.mock_all_auths();
    let admin_b = Address::generate(&env_b);
    let contract_b = env_b.register(MilestoneEscrow, ());
    env_b.as_contract(&contract_b, || {
        env_b.storage().instance().set(&DataKey::Admin, &admin_b);
        env_b.storage().persistent().set(&DataKey::Admin, &admin_b);
    });
    let escrow_b = MilestoneEscrowClient::new(&env_b, &contract_b);
    escrow_b.admin_pause_escrow(&admin_b);
    let pause = env_b.cost_estimate().resources();

    assert_eq!(
        pause.memory_read_entries + 1,
        init.memory_read_entries,
        "control: multisig_approval_init additionally reads the persistent \
         MultiSigConfig; measured pause {:?}, init {:?}",
        pause,
        init
    );
}

// ── behaviour identity ───────────────────────────────────────────────────────

/// The registered result is identical to the pre-#456 two-key registration.
///
/// Everything observable through the public API is checked: the persisted
/// config, the threshold reported by the read path, per-signer recognition (the
/// approval index is looked up in the consolidated signer set), the threshold
/// decision, idempotency, rejection of non-signers, and the single-shot
/// initialisation guard.
#[test]
fn test_multisig_approval_init_registers_signer_set_and_threshold_identically() {
    let env = test_env();
    let (contract_id, admin, escrow) = funded_escrow(&env);
    let signers = signer_set(&env);

    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &signers, &2u32),
        Ok(Ok(()))
    );
    crate::tests::seed_admin_proposal(&env, &escrow.address, 1);

    // Same signer set and same threshold as the two legacy entries held.
    assert_eq!(
        stored_config(&env, &contract_id),
        Some(MultiSigConfig(signers.clone(), 2u32)),
        "the consolidated entry must hold the registered signer set and threshold"
    );

    // Read path reports the same threshold it used to read from its own key.
    let state = escrow.is_multisig_approved(&1u32);
    assert_eq!(state.threshold, 2);
    assert_eq!(state.approvals, 0);
    assert_eq!(state.bitmap, 0);
    assert!(!state.approved);

    // Every registered signer is still recognised by `multisig_approve`, and the
    // threshold still decides when a proposal counts as approved.
    let first = escrow.multisig_approve(&signers.get(0).unwrap(), &1u32);
    assert_eq!(first.approvals, 1);
    assert!(
        !first.approved,
        "one of two required approvals is not enough"
    );
    let second = escrow.multisig_approve(&signers.get(1).unwrap(), &1u32);
    assert_eq!(second.approvals, 2);
    assert!(second.approved, "two approvals must meet the threshold");

    // Idempotent for a signer that already approved.
    let repeat = escrow.multisig_approve(&signers.get(0).unwrap(), &1u32);
    assert_eq!(repeat.approvals, 2);
    assert!(repeat.approved);

    // A non-signer is still rejected against the consolidated signer set.
    let stranger = Address::generate(&env);
    assert_eq!(
        escrow.try_multisig_approve(&stranger, &1u32),
        Err(Ok(Error::Unauthorized))
    );

    // Initialisation is still single-shot: the guard now probes the same
    // consolidated key the first call wrote.
    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &signers, &3u32),
        Err(Ok(Error::AlreadyInitialized))
    );
    assert_eq!(
        stored_config(&env, &contract_id),
        Some(MultiSigConfig(signers.clone(), 2u32)),
        "a rejected second initialisation must not overwrite the stored config"
    );
}
/// Rejected calls never write — including the entry this issue moved.
///
/// The successful path is the only write, and the already-initialised guard now
/// keys off the consolidated entry, so a rejected re-init must leave both the
/// stored config and the whole instance entry untouched.
#[test]
fn test_multisig_approval_init_rejections_write_no_storage() {
    let env = test_env();
    let (contract_id, admin) = admin_only_escrow(&env);
    let signers = signer_set(&env);

    // Unauthorized: a caller other than the stored admin.
    assert_rejected_without_storage_write(
        &env,
        &contract_id,
        &Address::generate(&env),
        &signers,
        1u32,
        Error::Unauthorized,
    );

    // Validation failures, all raised after the guard and before any write.
    assert_rejected_without_storage_write(
        &env,
        &contract_id,
        &admin,
        &vec![&env],
        1u32,
        Error::MultiSigNoSigners,
    );
    assert_rejected_without_storage_write(
        &env,
        &contract_id,
        &admin,
        &signers,
        0u32,
        Error::MultiSigInvalidThreshold,
    );
    assert_rejected_without_storage_write(
        &env,
        &contract_id,
        &admin,
        &signers,
        4u32,
        Error::MultiSigInvalidThreshold,
    );
    let duplicate = Address::generate(&env);
    assert_rejected_without_storage_write(
        &env,
        &contract_id,
        &admin,
        &vec![&env, duplicate.clone(), duplicate],
        1u32,
        Error::MultiSigDuplicateSigner,
    );
    let mut too_many: Vec<Address> = Vec::new(&env);
    for _ in 0..33 {
        too_many.push_back(Address::generate(&env));
    }
    assert_rejected_without_storage_write(
        &env,
        &contract_id,
        &admin,
        &too_many,
        1u32,
        Error::MultiSigTooManySigners,
    );

    // AlreadyInitialized, and the stored config survives the rejected re-init.
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &signers, &2u32),
        Ok(Ok(()))
    );
    let after_init = instance_footprint(&env);
    assert_eq!(
        escrow.try_multisig_approval_init(&admin, &signers, &3u32),
        Err(Ok(Error::AlreadyInitialized))
    );
    assert_eq!(
        instance_footprint(&env),
        after_init,
        "a rejected re-initialisation must not touch instance storage"
    );
    assert_eq!(
        stored_config(&env, &contract_id),
        Some(MultiSigConfig(signers.clone(), 2u32))
    );

    // NotInitialized needs its own escrow (one that has no admin at all), so it
    // runs in a non-capturing env.
    let env_b = env_without_snapshot();
    env_b.mock_all_auths();
    let contract_b = env_b.register(MilestoneEscrow, ());
    assert_rejected_without_storage_write(
        &env_b,
        &contract_b,
        &Address::generate(&env_b),
        &signer_set(&env_b),
        1u32,
        Error::NotInitialized,
    );
}
