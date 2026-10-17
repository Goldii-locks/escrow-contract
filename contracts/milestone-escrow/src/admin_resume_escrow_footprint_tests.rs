#![cfg(test)]
//! Dedicated unit-test suite for the `admin_resume_escrow` ledger-storage
//! footprint reduction (issue #449).
//!
//! Before this change a single call touched two distinct ledger entries:
//!   1. `DataKey::Admin` (persistent) — probed twice: first via `.has(..)`
//!      for the initialisation guard, then again via `load_admin`'s
//!      `.get(..)` for the authorization comparison.
//!   2. The contract's single **instance** entry — the instance copy
//!      of `DataKey::Admin`, `DataKey::Paused` and `DataKey::EpLk` all live
//!      inside the `contract_instance` ledger entry.
//!
//! `initialize` writes both copies of the admin address atomically, and every
//! admin-transfer path (`execute_admin_transfer`, the multisig override)
//! keeps them in sync, so the function now authorizes via the existing
//! `require_admin_from_instance` helper — the same consolidation already
//! landed for `set_platform_fee_allocation` (#472),
//! `set_escrow_interest_yield` (#463) and `multisig_lock` (#460).  Every
//! storage access of the call now lands on the single instance entry:
//! **2 distinct ledger entries per call → 1**, and the persistent entry is
//! read zero times instead of twice.
//!
//! These tests prove the reduction directly rather than only documenting it:
//!   - The measured per-invocation resource metering pins the exact number
//!     of distinct ledger entries the call touches: 3 after this change (wasm
//!     code + auth nonce + the single instance entry; the latter is the only
//!     *storage* entry), with a control call that still reads the persistent
//!     copy measuring one entry more (i.e. 4 before the change).
//!   - The call succeeds when no persistent `Admin` entry exists at all
//!     (previously `NotInitialized`) and creates no persistent entry.
//!   - The authorization comparison reads the *instance* copy, pinning the
//!     exact key set of the call to instance storage.
//!   - External behavior is unchanged: the pause flag clears, the `resume`
//!     event fires with the expected payload, the emergency lock is released,
//!     a pause-gated endpoint unblocks, and a second resume still fails with
//!     `NotPaused`.

use super::*;
use crate::{DataKey, Error, EscrowResumedEvent};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, Address, Env, FromVal, IntoVal, Val};

fn test_env() -> Env {
    Env::default()
}

/// Env that skips snapshot capture — used for the secondary control env in
/// a test, so a single test writes at most one snapshot file.
fn env_without_snapshot() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn is_paused(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::Paused)
            .unwrap_or(false)
    })
}

fn is_lock_held(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::EpLk)
            .unwrap_or(false)
    })
}

fn has_persistent_admin(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage().persistent().has(&DataKey::Admin)
    })
}

/// A fully initialised escrow (both `Admin` copies written) plus its admin.
fn initialised_escrow(env: &Env) -> (MilestoneEscrowClient<'_>, Address) {
    env.mock_all_auths();

    let admin_addr = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = soroban_sdk::vec![env, 1_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604_800u64,
        &amounts,
    );

    (escrow, admin_addr)
}

/// Count events whose single topic matches the `resume` symbol in the most
/// recent invocation.
fn resume_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("resume").into_val(env);
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

// ── footprint: distinct ledger entries touched per invocation ────────────────

/// Direct measurement of the reduction demanded by issue #449, using the
/// test SDK's per-invocation resource metering
/// (`env.cost_estimate().resources()`): the host resets the recording
/// footprint at the start of every top-level contract invocation and reports
/// how many distinct ledger entries that invocation touched.
///
/// Measured footprint of `admin_resume_escrow` **after** this change —
/// exactly three distinct ledger entries:
///
///   1. `contract_code` — wasm instantiation for the call; a constant of
///      every invocation, independent of the contract's storage layout.
///   2. the temporary nonce entry consumed by `admin.require_auth()`; also a
///      constant of every authenticated call.
///   3. the single `contract_instance` entry that holds *all* instance
///      storage (`Admin`, `Paused`, `EpLk`) — the entry issue #449
///      consolidates the call onto.
///
/// **Before** this change there was a fourth entry: the persistent
/// `DataKey::Admin` storage entry, probed via `has` + `get` — and both
/// envs below seed that persistent copy exactly as `initialize` writes it,
/// so the entry *exists* in the measured env yet stays outside the
/// footprint.  The claim is validated by the control: in a second,
/// identically-shaped fresh environment, `admin_pause_escrow` (unchanged
/// by #449 — it still authorizes against the persistent copy) measures
/// exactly one distinct entry more than the consolidated resume call,
/// i.e. the per-persistent-key delta this issue removes.
#[test]
fn test_admin_resume_escrow_touched_entries_footprint() {
    // Setup A: both copies of `Admin` exist, exactly as `initialize`
    // writes them, and the escrow is paused.  The resume call must touch
    // only the contract-instance entry — the persistent copy's mere
    // existence must not put it into the invocation footprint.
    let env = test_env();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Paused, &true);
        env.storage().persistent().set(&DataKey::Admin, &admin);
    });
    assert!(
        has_persistent_admin(&env, &contract_id),
        "precondition: persistent::Admin exists, as initialize writes it"
    );

    escrow.admin_resume_escrow(&admin);
    let resume = env.cost_estimate().resources();

    // Setup B: a second fresh environment with the same first-invocation
    // shape, plus the persistent `Admin` copy that `admin_pause_escrow`
    // still reads.  Running each measurement as the first invocation of its
    // own fresh env keeps the two footprints directly comparable.  (No
    // snapshot capture here — this env is only a measurement control.)
    let env_b = env_without_snapshot();
    env_b.mock_all_auths();
    let admin_b = Address::generate(&env_b);
    let contract_b = env_b.register(MilestoneEscrow, ());
    let escrow_b = MilestoneEscrowClient::new(&env_b, &contract_b);
    env_b.as_contract(&contract_b, || {
        env_b.storage().instance().set(&DataKey::Admin, &admin_b);
        env_b.storage().persistent().set(&DataKey::Admin, &admin_b);
    });

    escrow_b.admin_pause_escrow(&admin_b);
    let pause = env_b.cost_estimate().resources();

    // The core assertion of issue #449: the consolidated resume call
    // touches exactly three distinct ledger entries — the wasm code entry,
    // the auth nonce entry, and the single contract-instance entry that
    // holds all of its storage state.  The fourth entry the call touched
    // before this change (persistent `DataKey::Admin`) is gone.
    assert_eq!(
        resume.memory_read_entries, 3,
        "admin_resume_escrow must touch exactly three distinct ledger \
         entries (contract_code + auth nonce + contract_instance); the \
         persistent Admin entry must no longer be among them; measured {:?}",
        resume
    );
    assert_eq!(
        resume.write_entries, 2,
        "the only ledger entries written are the contract-instance entry \
         (Paused/EpLk) and the auth nonce; measured {:?}",
        resume
    );
    assert_eq!(
        resume.disk_read_entries, 0,
        "everything the call reads is live in-memory Soroban state"
    );
    assert!(resume.instructions > 0, "sanity: the call was metered");

    // Control: the metric distinguishes 1 from 2.  `admin_pause_escrow`
    // still reads the persistent `Admin` copy (its own storage layout is
    // outside #449's scope), so its footprint is exactly one distinct
    // entry larger than the consolidated resume call.
    assert_eq!(
        pause.memory_read_entries,
        resume.memory_read_entries + 1,
        "control: admin_pause_escrow additionally reads the persistent \
         Admin copy — one distinct ledger entry more than admin_resume_escrow; \
         measured pause {:?}, resume {:?}",
        pause,
        resume
    );
}

// ── footprint: the persistent Admin entry is no longer part of the call ──────

/// The core footprint proof for issue #449.
///
/// Seed *only* the instance keys the call is designed to use — never call
/// `initialize`, so `persistent::Admin` does not exist at all.  Before this
/// change the very first storage access of `admin_resume_escrow` was
/// `persistent().has(&DataKey::Admin)`, which returned `false` and the call
/// failed with `NotInitialized`.  If any read of the persistent entry
/// survives (or is reintroduced), this test fails again.
///
/// After the call: the pause flag is cleared, the lock is released, and the
/// persistent storage is still completely empty — the call neither read nor
/// created a persistent entry.
#[test]
fn test_admin_resume_escrow_does_not_require_persistent_admin() {
    let env = test_env();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    // Only instance storage is seeded; persistent storage stays empty.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Paused, &true);
    });
    assert!(
        !has_persistent_admin(&env, &contract_id),
        "precondition: persistent::Admin must not exist"
    );

    assert_eq!(escrow.try_admin_resume_escrow(&admin), Ok(Ok(())));

    // Read the event before any env.as_contract helper: the event log is
    // scoped to the most recent invocation, and the as_contract reads below
    // would hide it.
    assert_eq!(resume_event_count(&env), 1);

    // The pause state cleared exactly as before.
    assert!(!is_paused(&env, &contract_id), "pause flag must be cleared");
    assert!(!is_lock_held(&env, &contract_id), "EpLk must be released");

    // The call never reached into persistent storage — not even to create
    // the entry it used to read.
    assert!(
        !has_persistent_admin(&env, &contract_id),
        "admin_resume_escrow must not read or write persistent storage"
    );
}

/// Pin the *exact* key set: the authorization comparison must be made
/// against the instance copy of `DataKey::Admin`.
///
/// Seed a persistent copy that differs from the instance copy.  The old
/// implementation compared against the persistent value (and would have
/// rejected this caller); the consolidated implementation compares against
/// the instance value and accepts.  Both copies are always written together
/// by `initialize` and kept in sync by every admin-transfer path, so this
/// divergent state is unreachable through the public API — it exists here
/// only to prove which key the call actually reads.
#[test]
fn test_admin_resume_escrow_authorizes_against_instance_admin_copy() {
    let env = test_env();
    env.mock_all_auths();

    let instance_admin = Address::generate(&env);
    let stale_persistent_admin = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::Admin, &instance_admin);
        env.storage()
            .persistent()
            .set(&DataKey::Admin, &stale_persistent_admin);
        env.storage().instance().set(&DataKey::Paused, &true);
    });

    // Caller matches the instance copy → accepted, proving the instance key
    // is the one being compared (the old code compared the persistent copy
    // and would have returned Unauthorized here).
    assert_eq!(escrow.try_admin_resume_escrow(&instance_admin), Ok(Ok(())));
    assert!(!is_paused(&env, &contract_id));

    // The persistent copy was left untouched by the call.
    let persistent_after: Option<Address> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin)
    });
    assert_eq!(
        persistent_after,
        Some(stale_persistent_admin),
        "the persistent copy must not be rewritten by admin_resume_escrow"
    );
}

/// The initialisation guard is preserved: a contract that has never been
/// initialised (neither copy of `Admin` exists) still fails with
/// `NotInitialized`, now sourced from the instance key.
#[test]
fn test_admin_resume_escrow_uninitialized_still_rejected() {
    let env = test_env();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        escrow.try_admin_resume_escrow(&admin),
        Err(Ok(Error::NotInitialized))
    );
    assert!(!is_paused(&env, &contract_id));
    assert!(!is_lock_held(&env, &contract_id));
}

// ── behavior: identical to the documented pre-existing behavior ──────────────

/// Full pause → resume cycle against a normally initialised escrow:
/// while paused a pause-gated endpoint is rejected with `Paused`; after
/// `admin_resume_escrow` the flag is cleared, exactly one `resume` event with
/// the correct payload is emitted, the emergency lock is released, the same
/// endpoint works again, and a second resume still fails with `NotPaused`.
#[test]
fn test_admin_resume_escrow_clears_pause_and_resumes_normal_operation() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);
    let contract_id = escrow.address.clone();

    // Pause via the regular admin endpoint.
    escrow.admin_pause_escrow(&admin);
    assert!(is_paused(&env, &contract_id));
    assert_eq!(resume_event_count(&env), 0);

    // A pause-gated endpoint is rejected while paused.
    assert_eq!(
        escrow.try_admin_set_yield_rate(&admin, &500u32),
        Err(Ok(Error::Paused))
    );

    // Resume — the behavior under test.
    assert_eq!(escrow.try_admin_resume_escrow(&admin), Ok(Ok(())));

    // Read the event first: crate::all_event_tuples reports the most recent
    // invocation, so later client calls would hide it.
    assert_eq!(resume_event_count(&env), 1);
    let events = crate::all_event_tuples(&env);
    let last = events.last().unwrap();
    let topic: Val = last.1.get(0).unwrap();
    let resume_topic: Val = symbol_short!("resume").into_val(&env);
    assert_eq!(
        topic.get_payload(),
        resume_topic.get_payload(),
        "the single event topic must be \"resume\""
    );
    let payload = EscrowResumedEvent::from_val(&env, &last.2);
    assert_eq!(payload.admin, admin);
    assert_eq!(payload.contract_id, contract_id);

    // Pause state cleared, lock released, persisted admin untouched.
    assert!(!is_paused(&env, &contract_id), "pause flag must be cleared");
    assert!(!is_lock_held(&env, &contract_id), "EpLk must be released");
    let (_, _, paused) = escrow.get_yield_info();
    assert!(!paused, "get_yield_info must report the escrow as running");
    let persistent_admin: Option<Address> = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&DataKey::Admin)
    });
    assert_eq!(
        persistent_admin,
        Some(admin.clone()),
        "persistent::Admin must be unchanged by the resume"
    );

    // Normal operation resumed: the same endpoint that was rejected above
    // now succeeds.
    assert_eq!(escrow.try_admin_set_yield_rate(&admin, &500u32), Ok(Ok(())));

    // The NotPaused guard on a second resume is unchanged.
    assert_eq!(
        escrow.try_admin_resume_escrow(&admin),
        Err(Ok(Error::NotPaused))
    );

    // Unauthorized callers are still rejected without mutating anything.
    let attacker = Address::generate(&env);
    assert_eq!(
        escrow.try_admin_resume_escrow(&attacker),
        Err(Ok(Error::Unauthorized))
    );
}
