//! Tests for issue #492: single-read guarantee for `get_platform_fee_allocation`.
//!
//! # What was changed
//!
//! All five inline reads of `DataKey::PlatformFeeAllocation` were replaced with
//! calls to a new private helper `load_platform_fee_allocation`, which contains
//! the single call to `env.storage().instance().get(…)`. This guarantees each
//! function that needs the stored value issues exactly one ledger read.
//!
//! # What these tests verify
//!
//! 1. **Unchanged return values** — every public function that previously read
//!    `DataKey::PlatformFeeAllocation` directly produces the same result after
//!    the refactor.
//! 2. **Single-read semantics** — repeated calls to `get_platform_fee_allocation`
//!    are stable and do not accumulate side-effects (storage is not mutated by a
//!    read).
//! 3. **Callers that write through the helper** — `set_platform_fee_allocation`,
//!    `lock_platform_fee_allocation`, `pf_alloc_admin_override`, and
//!    `calculate_platform_fee_split` all produce correct outputs, confirming the
//!    helper is wired consistently through every call-site.
//!
//! # Test matrix
//!
//! | # | Scenario | Assertion |
//! |---|----------|-----------|
//! | 1 | `get_platform_fee_allocation` returns default after `initialize` | exact field values |
//! | 2 | `get_platform_fee_allocation` returns configured values after `set_platform_fee_allocation` | exact field values |
//! | 3 | `get_platform_fee_allocation` returns locked after `lock_platform_fee_allocation` | `locked == true`, BPS unchanged |
//! | 4 | `get_platform_fee_allocation` returns unlocked after `pf_alloc_admin_override` | new BPS, `locked == false` |
//! | 5 | Repeated reads return identical values (no mutation on read) | all reads equal |
//! | 6 | `calculate_platform_fee_split` result consistent with `get_platform_fee_allocation` | split amounts agree with stored BPS |
//! | 7 | `get_platform_fee_allocation` round-trips through every write path without skew | before/after snapshot comparison |
//! | 8 | `NotInitialized` before `initialize` (pre-init state unchanged) | `Err(NotInitialized)` |

use crate::test::setup_funded_escrow;
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient, PlatformFeeAllocation};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Run `get_platform_fee_allocation` N times and assert every result is equal.
fn assert_reads_are_stable(client: &MilestoneEscrowClient, n: usize) {
    let first = client.get_platform_fee_allocation();
    for _ in 1..n {
        assert_eq!(
            client.get_platform_fee_allocation(),
            first,
            "get_platform_fee_allocation is not stable across repeated reads"
        );
    }
}

// ── 1 ─ default value written by initialize ──────────────────────────────────

/// After `initialize` the helper returns the default allocation written by
/// `initialize` itself: `{0, 10_000, 0, locked: false}`.
#[test]
fn test_load_helper_returns_default_after_initialize() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &vec![&env, 1_000_i128],
    );

    let alloc = client.get_platform_fee_allocation();
    assert_eq!(alloc.client_bps, 0);
    assert_eq!(alloc.freelancer_bps, 10_000);
    assert_eq!(alloc.treasury_bps, 0);
    assert!(!alloc.locked);
}

// ── 2 ─ configured values after set_platform_fee_allocation ──────────────────

/// After `set_platform_fee_allocation` the helper returns the exact BPS values
/// that were written, with `locked == false`.
#[test]
fn test_load_helper_returns_configured_bps_after_set() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);

    let alloc = client.get_platform_fee_allocation();
    assert_eq!(alloc.client_bps, 2000);
    assert_eq!(alloc.freelancer_bps, 7000);
    assert_eq!(alloc.treasury_bps, 1000);
    assert!(!alloc.locked);
}

// ── 3 ─ locked state after lock_platform_fee_allocation ──────────────────────

/// After `lock_platform_fee_allocation` the helper returns the same BPS with
/// `locked == true`.  Confirms that `lock_platform_fee_allocation` reaches the
/// correct storage key through the refactored helper.
#[test]
fn test_load_helper_returns_locked_true_after_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &3000_u32, &6000_u32, &1000_u32);
    client.lock_platform_fee_allocation(&admin_addr);

    let alloc = client.get_platform_fee_allocation();
    assert_eq!(alloc.client_bps, 3000);
    assert_eq!(alloc.freelancer_bps, 6000);
    assert_eq!(alloc.treasury_bps, 1000);
    assert!(alloc.locked);
}

// ── 4 ─ override unlocks and writes new BPS ───────────────────────────────────

/// After `pf_alloc_admin_override` the helper returns the overridden BPS with
/// `locked == false`.  Confirms that `pf_alloc_admin_override` reaches the
/// correct storage key through the refactored helper.
#[test]
fn test_load_helper_returns_new_bps_and_unlocked_after_override() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    client.lock_platform_fee_allocation(&admin_addr);
    client.pf_alloc_admin_override(&admin_addr, &4000_u32, &5000_u32, &1000_u32);

    let alloc = client.get_platform_fee_allocation();
    assert_eq!(alloc.client_bps, 4000);
    assert_eq!(alloc.freelancer_bps, 5000);
    assert_eq!(alloc.treasury_bps, 1000);
    assert!(!alloc.locked);
}

// ── 5 ─ repeated reads are stable (no mutation on read) ──────────────────────

/// Calling `get_platform_fee_allocation` multiple times in a row returns the
/// same value every time.  This confirms that the single-read path through the
/// helper performs no writes and cannot accumulate state across calls.
#[test]
fn test_repeated_reads_are_stable() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Test stability in the default state.
    assert_reads_are_stable(&client, 5);

    // Test stability after a write.
    client.set_platform_fee_allocation(&admin_addr, &1000_u32, &8000_u32, &1000_u32);
    assert_reads_are_stable(&client, 5);

    // Test stability after a lock.
    client.lock_platform_fee_allocation(&admin_addr);
    assert_reads_are_stable(&client, 5);
}

// ── 6 ─ calculate_platform_fee_split agrees with get_platform_fee_allocation ─

/// `calculate_platform_fee_split` must produce amounts that are consistent with
/// the allocation returned by `get_platform_fee_allocation`.  Both functions
/// now read through the same `load_platform_fee_allocation` helper, so this
/// test confirms they are reading the same data.
#[test]
fn test_split_consistent_with_get_allocation() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 10_000_i128]);

    // 30 % client / 60 % freelancer / 10 % treasury
    client.set_platform_fee_allocation(&admin_addr, &3000_u32, &6000_u32, &1000_u32);

    let alloc = client.get_platform_fee_allocation();
    let split = client.calculate_platform_fee_split(&10_000_i128);

    // Manual check: amounts must agree with the stored BPS.
    let expected_client = 10_000_i128 * alloc.client_bps as i128 / 10_000;
    let expected_freelancer = 10_000_i128 * alloc.freelancer_bps as i128 / 10_000;
    let expected_treasury = 10_000_i128 * alloc.treasury_bps as i128 / 10_000;

    // The split uses largest-remainder rounding so just confirm total is preserved
    // and each individual amount is within 1 stroop of the simple ratio.
    assert_eq!(
        split.client_amount + split.freelancer_amount + split.treasury_amount,
        10_000_i128,
        "split total must equal the input amount"
    );
    assert!(
        (split.client_amount - expected_client).abs() <= 1,
        "client split off by more than 1 stroop"
    );
    assert!(
        (split.freelancer_amount - expected_freelancer).abs() <= 1,
        "freelancer split off by more than 1 stroop"
    );
    assert!(
        (split.treasury_amount - expected_treasury).abs() <= 1,
        "treasury split off by more than 1 stroop"
    );
}

// ── 7 ─ round-trip through every write path ──────────────────────────────────

/// Walk through the full lifecycle of the allocation:
/// default → set → lock → override → set again.
/// After each write, assert that `get_platform_fee_allocation` returns exactly
/// what was written.  This confirms the helper wiring is correct across the
/// entire mutation chain.
#[test]
fn test_round_trip_through_all_write_paths() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Step 1: default
    let default_alloc = client.get_platform_fee_allocation();
    assert_eq!(
        default_alloc,
        PlatformFeeAllocation {
            client_bps: 0,
            freelancer_bps: 10_000,
            treasury_bps: 0,
            locked: false,
        }
    );

    // Step 2: set
    client.set_platform_fee_allocation(&admin_addr, &2500_u32, &5500_u32, &2000_u32);
    let set_alloc = client.get_platform_fee_allocation();
    assert_eq!(set_alloc.client_bps, 2500);
    assert_eq!(set_alloc.freelancer_bps, 5500);
    assert_eq!(set_alloc.treasury_bps, 2000);
    assert!(!set_alloc.locked);

    // Step 3: lock
    client.lock_platform_fee_allocation(&admin_addr);
    let locked_alloc = client.get_platform_fee_allocation();
    assert_eq!(locked_alloc.client_bps, 2500); // BPS unchanged
    assert_eq!(locked_alloc.freelancer_bps, 5500);
    assert_eq!(locked_alloc.treasury_bps, 2000);
    assert!(locked_alloc.locked);

    // Step 4: admin override
    client.pf_alloc_admin_override(&admin_addr, &1000_u32, &8000_u32, &1000_u32);
    let overridden_alloc = client.get_platform_fee_allocation();
    assert_eq!(overridden_alloc.client_bps, 1000);
    assert_eq!(overridden_alloc.freelancer_bps, 8000);
    assert_eq!(overridden_alloc.treasury_bps, 1000);
    assert!(!overridden_alloc.locked);

    // Step 5: set again (now unlocked)
    client.set_platform_fee_allocation(&admin_addr, &5000_u32, &4000_u32, &1000_u32);
    let final_alloc = client.get_platform_fee_allocation();
    assert_eq!(final_alloc.client_bps, 5000);
    assert_eq!(final_alloc.freelancer_bps, 4000);
    assert_eq!(final_alloc.treasury_bps, 1000);
    assert!(!final_alloc.locked);
}

// ── 8 ─ NotInitialized before initialize ─────────────────────────────────────

/// `get_platform_fee_allocation` must return `Err(NotInitialized)` on a
/// contract that has been registered but never initialized.  This confirms
/// that the helper correctly propagates the missing-key case.
#[test]
fn test_load_helper_returns_not_initialized_before_init() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_get_platform_fee_allocation();
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}
