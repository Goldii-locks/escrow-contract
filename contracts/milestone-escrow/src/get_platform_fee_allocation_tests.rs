//! Validation tests for `get_platform_fee_allocation`.
//!
//! # Purpose
//!
//! `get_platform_fee_allocation` is a **pure read** endpoint that:
//! * returns `Err(Error::NotInitialized)` when `set_platform_fee_allocation`
//!   has never been called (the storage key is absent), and
//! * otherwise returns `Ok(PlatformFeeAllocation)` with the exact BPS values
//!   and lock flag that were last written to instance storage.
//!
//! The three basis-point fields always satisfy
//! `client_bps + freelancer_bps + treasury_bps == 10_000` for any value the
//! function can return; this invariant is enforced by both the writer
//! (`set_platform_fee_allocation`) and the override path
//! (`pf_alloc_admin_override`) before committing to storage.
//!
//! The Soroban SDK test client exposes two call forms:
//! * `client.get_platform_fee_allocation()` — panics on a contract error,
//!   returns `PlatformFeeAllocation` directly on success.
//! * `client.try_get_platform_fee_allocation()` — returns
//!   `Result<Result<PlatformFeeAllocation, ConversionError>,
//!           Result<Error, InvokeError>>`;
//!   a contract-level `Err(E)` surfaces as `Err(Ok(E))`.
//!
//! # Test matrix
//!
//! | # | Scenario                                                       | Expected outcome                                |
//! |---|----------------------------------------------------------------|-------------------------------------------------|
//! | 1 | Contract registered but **not** initialized                    | `Err(Ok(NotInitialized))`                       |
//! | 2 | After `initialize`, before `set_platform_fee_allocation`       | `Ok` with default `{0, 10_000, 0, locked: false}` |
//! | 3 | After `set_platform_fee_allocation`                            | `Ok` with exact BPS, `locked == false`          |
//! | 4 | After `lock_platform_fee_allocation`                           | same BPS, `locked == true`                      |
//! | 5 | After `pf_alloc_admin_override`                                | new BPS, `locked == false`                      |
//! | 6 | Multiple reads return the same value                           | idempotent                                      |
//! | 7 | `client_bps + freelancer_bps + treasury_bps == 10_000`         | BPS invariant holds for all stored values       |
//! | 8 | Read does not mutate storage                                   | instance snapshot identical before and after    |

use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient, PlatformFeeAllocation};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

// ── snapshot helper ───────────────────────────────────────────────────────────

/// Captures the raw `PlatformFeeAllocation` from instance storage (or `None`
/// when the key is absent) so tests can verify that a read leaves storage
/// byte-identical.
fn capture_allocation(env: &Env, contract_id: &Address) -> Option<PlatformFeeAllocation> {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
    })
}

// ── 1 ─ contract registered but NOT initialized ──────────────────────────────

/// `get_platform_fee_allocation` returns `Err(NotInitialized)` when called on
/// a contract that has been registered but whose `initialize` entrypoint has
/// never been invoked.  The storage key has never been written.
#[test]
fn test_returns_not_initialized_before_contract_init() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_get_platform_fee_allocation();
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

// ── 2 ─ initialized: default allocation from initialize ──────────────────────

/// `initialize` writes a default `PlatformFeeAllocation` of
/// `{ client_bps: 0, freelancer_bps: 10_000, treasury_bps: 0, locked: false }`
/// before `set_platform_fee_allocation` is called.
/// `get_platform_fee_allocation` must return that exact default.
#[test]
fn test_returns_default_allocation_after_contract_init_only() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &vec![&env, 1_000_i128],
    );

    // initialize sets a default allocation; the call must succeed with the default values.
    let allocation = client.get_platform_fee_allocation();
    assert_eq!(allocation.client_bps, 0);
    assert_eq!(allocation.freelancer_bps, 10_000);
    assert_eq!(allocation.treasury_bps, 0);
    assert!(!allocation.locked);
}

// ── 3 ─ populated state: after set_platform_fee_allocation ───────────────────

/// After `set_platform_fee_allocation` succeeds the function returns `Ok` with
/// every field equal to what was written: the exact BPS values and
/// `locked == false`.
#[test]
fn test_returns_ok_with_exact_bps_after_set() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &3000_u32, &5000_u32, &2000_u32);

    let allocation = client.get_platform_fee_allocation();

    assert_eq!(allocation.client_bps, 3000);
    assert_eq!(allocation.freelancer_bps, 5000);
    assert_eq!(allocation.treasury_bps, 2000);
    assert!(!allocation.locked);
}

/// The allocation can be updated by calling `set_platform_fee_allocation`
/// again before it is locked; the returned value reflects the latest write.
#[test]
fn test_returns_latest_allocation_after_update() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    // Overwrite with different values before locking.
    client.set_platform_fee_allocation(&admin_addr, &1000_u32, &8000_u32, &1000_u32);

    let allocation = client.get_platform_fee_allocation();
    assert_eq!(allocation.client_bps, 1000);
    assert_eq!(allocation.freelancer_bps, 8000);
    assert_eq!(allocation.treasury_bps, 1000);
    assert!(!allocation.locked);
}

// ── 4 ─ boundary state: after lock_platform_fee_allocation ───────────────────

/// After `lock_platform_fee_allocation` the stored BPS values must be
/// unchanged and `locked` must be `true`.
#[test]
fn test_returns_locked_true_after_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &4000_u32, &5000_u32, &1000_u32);
    client.lock_platform_fee_allocation(&admin_addr);

    let allocation = client.get_platform_fee_allocation();

    assert_eq!(allocation.client_bps, 4000);
    assert_eq!(allocation.freelancer_bps, 5000);
    assert_eq!(allocation.treasury_bps, 1000);
    assert!(allocation.locked);
}

// ── 5 ─ after pf_alloc_admin_override ────────────────────────────────────────

/// `pf_alloc_admin_override` replaces a locked allocation with new BPS and
/// resets the lock to `false`.  `get_platform_fee_allocation` must reflect
/// those new values immediately.
#[test]
fn test_returns_new_bps_and_unlocked_after_admin_override() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    client.lock_platform_fee_allocation(&admin_addr);
    client.pf_alloc_admin_override(&admin_addr, &5000_u32, &4000_u32, &1000_u32);

    let allocation = client.get_platform_fee_allocation();

    assert_eq!(allocation.client_bps, 5000);
    assert_eq!(allocation.freelancer_bps, 4000);
    assert_eq!(allocation.treasury_bps, 1000);
    assert!(!allocation.locked);
}

// ── 6 ─ multiple reads are idempotent ────────────────────────────────────────

/// Calling `get_platform_fee_allocation` multiple times in a row returns the
/// same value each time (it is a pure read with no side-effects that could
/// change the result between calls).
#[test]
fn test_multiple_reads_are_idempotent() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &3000_u32, &6000_u32, &1000_u32);

    let first = client.get_platform_fee_allocation();
    let second = client.get_platform_fee_allocation();
    let third = client.get_platform_fee_allocation();

    assert_eq!(first, second);
    assert_eq!(second, third);
}

// ── 7 ─ BPS invariant: three fields always sum to 10_000 ─────────────────────

/// For every `Ok` return the three BPS fields must sum exactly to 10_000.
/// This is tested across all three possible stored states: freshly set,
/// locked, and overridden.
#[test]
fn test_bps_invariant_holds_for_all_stored_states() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // State A: freshly set
    client.set_platform_fee_allocation(&admin_addr, &2500_u32, &5500_u32, &2000_u32);
    let a = client.get_platform_fee_allocation();
    assert_eq!(
        a.client_bps + a.freelancer_bps + a.treasury_bps,
        10_000,
        "BPS invariant violated in freshly-set state"
    );

    // State B: locked
    client.lock_platform_fee_allocation(&admin_addr);
    let b = client.get_platform_fee_allocation();
    assert_eq!(
        b.client_bps + b.freelancer_bps + b.treasury_bps,
        10_000,
        "BPS invariant violated after lock"
    );

    // State C: after admin override
    client.pf_alloc_admin_override(&admin_addr, &1000_u32, &8000_u32, &1000_u32);
    let c = client.get_platform_fee_allocation();
    assert_eq!(
        c.client_bps + c.freelancer_bps + c.treasury_bps,
        10_000,
        "BPS invariant violated after admin override"
    );
}

// ── 8 ─ read does NOT mutate storage ─────────────────────────────────────────

/// Capture the raw instance storage value before and after the call; they
/// must be byte-identical, confirming that `get_platform_fee_allocation` is a
/// true read-only operation.
#[test]
fn test_read_does_not_mutate_storage() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &3000_u32, &5000_u32, &2000_u32);

    let before = capture_allocation(&env, &contract_id);
    let _ = client.get_platform_fee_allocation();
    let after = capture_allocation(&env, &contract_id);

    assert_eq!(
        before, after,
        "get_platform_fee_allocation must not mutate storage"
    );
}

/// Same read-only check in the *empty* state (contract registered but `initialize`
/// never called — storage key is absent).
#[test]
fn test_read_does_not_mutate_storage_when_uninitialized() {
    let env = Env::default();
    env.mock_all_auths();

    // Register the contract but do NOT call initialize — the allocation key is absent.
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let before = capture_allocation(&env, &contract_id);
    let _ = client.try_get_platform_fee_allocation();
    let after = capture_allocation(&env, &contract_id);

    assert_eq!(before, None);
    assert_eq!(
        after, None,
        "get_platform_fee_allocation must not create the storage key on a miss"
    );
}
