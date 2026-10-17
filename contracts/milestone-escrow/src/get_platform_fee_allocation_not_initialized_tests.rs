#![cfg(test)]
//! Regression suite for issue #490: `get_platform_fee_allocation` must surface
//! a **typed** error — `Error::NotInitialized` — instead of trapping
//! (panicking) or returning a defaulted allocation when it is called before
//! `initialize`.
//!
//! # Why the distinction is load-bearing
//!
//! A read that traps and a read that returns `Err(NotInitialized)` are *not*
//! interchangeable for callers.  Through the Soroban test client the three
//! distinguishable shapes are:
//!
//! * typed contract error → `Err(Ok(Error::NotInitialized))`
//! * host trap / panic    → `Err(Err(InvokeError))`
//! * defaulted value      → `Ok(Ok(PlatformFeeAllocation { .. }))`
//!
//! Only the first shape carries the stable contract error code (`2`) that
//! off-chain callers — the indexer, the dashboard, the backend — decode and act
//! on.  A trap consumes the caller's whole invocation budget and is
//! indistinguishable from a genuine contract bug.  A defaulted allocation is
//! worse still: it silently bills a fee split the admin never configured.
//! Every assertion below therefore inspects *which* of the three shapes came
//! back; a bare `assert!(result.is_err())` would accept a trap and is exactly
//! the regression this suite exists to prevent.
//!
//! # Test matrix
//!
//! | # | Scenario                                                         | Expected outcome |
//! |---|------------------------------------------------------------------|------------------|
//! | 1 | `get_platform_fee_allocation` on a freshly registered contract    | `Err(Ok(NotInitialized))`, no auth required |
//! | 2 | Raw storage behind the error                                      | allocation and lock keys absent (`None`) |
//! | 3 | `calculate_platform_fee_split` for every amount shape             | `Err(Ok(NotInitialized))`, never a split |
//! | 4 | `set_platform_fee_allocation` on a freshly registered contract    | `Err(Ok(NotInitialized))` for valid *and* invalid BPS |
//! | 5 | `lock_platform_fee_allocation` on a freshly registered contract   | `Err(Ok(NotInitialized))` |
//! | 6 | `pf_alloc_admin_override` on a freshly registered contract        | `Err(Ok(NotInitialized))` |
//! | 7 | 25 repeated reads on a fresh contract                             | typed error each time, no default materialised |
//! | 8 | All five platform-fee entrypoints, ledger snapshot before/after   | snapshots byte-identical |
//! | 9 | A failed `initialize` (rolled back) followed by the read          | `Err(Ok(NotInitialized))` |
//! | 10 | After a successful `initialize`                                  | `Ok(default)` — the guard is not over-broad |
//! | 11 | Error code exposed to callers                                     | `Error::NotInitialized as u32 == 2` |

use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient, PlatformFeeAllocation};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

/// Assert that a `try_*` result is a **typed** `Error::NotInitialized`.
///
/// Implemented as a macro because it has to destructure whatever the SDK
/// client returns (`Result<Result<T, ConversionError>, Result<Error,
/// InvokeError>>`) without naming the per-endpoint success type.  All three
/// shapes are pinned:
///
/// * `Err(Ok(Error::NotInitialized))` — the contract's own typed error: pass.
/// * `Err(Err(..))` — the host trapped; panic with the trap payload attached.
/// * `Ok(..)` — a value (e.g. a defaulted allocation) came back; panic.
macro_rules! assert_typed_not_initialized {
    ($result:expr, $context:expr) => {{
        match $result {
            Err(Ok(error)) => assert_eq!(
                error,
                Error::NotInitialized,
                "{}: contract returned the wrong typed error",
                $context
            ),
            Err(Err(invoke_error)) => panic!(
                "{}: trapped instead of surfacing a typed error: {:?}",
                $context, invoke_error
            ),
            Ok(_) => panic!(
                "{}: returned a value instead of Err(Error::NotInitialized) — an \
                 uninitialized contract must never hand back a defaulted allocation",
                $context
            ),
        }
    }};
}

/// Register the contract **without** ever calling `initialize`, which is the
/// single precondition the issue is about.
fn fresh_contract(env: &Env) -> (Address, MilestoneEscrowClient<'_>) {
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    (contract_id, client)
}

/// Read the two raw instance-storage entries the platform-fee read path
/// depends on: the allocation itself and the re-entrancy lock flag.  Used to
/// prove that a rejected call materialised neither key.
fn raw_allocation_keys(
    env: &Env,
    contract_id: &Address,
) -> (Option<PlatformFeeAllocation>, Option<bool>) {
    env.as_contract(contract_id, || {
        (
            env.storage()
                .instance()
                .get::<_, PlatformFeeAllocation>(&DataKey::PlatformFeeAllocation),
            env.storage()
                .instance()
                .get::<_, bool>(&DataKey::PlatformFeeAllocationLock),
        )
    })
}

/// Build a fully initialised escrow and hand back the pieces the positive
/// control needs.
fn initialized_escrow(env: &Env) -> (Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let (contract_id, client) = fresh_contract(env);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer,
        &arbiter,
        &token_id,
        &604_800u64,
        &vec![env, 1_000_i128],
    );

    (contract_id, client)
}

// ── 1 ─ the exact check the issue asks for ───────────────────────────────────

/// Invoking `get_platform_fee_allocation` on a freshly registered contract —
/// one whose `initialize` entrypoint has never been called — returns the typed
/// contract error `Error::NotInitialized`, not a trap and not a default.
///
/// No authorization is mocked in this test on purpose: the documented contract
/// is that the read is open to any caller, so an auth-gated or panic-shaped
/// failure would both be caught here (`Err(Err(..))`).
#[test]
fn get_platform_fee_allocation_returns_typed_not_initialized_on_fresh_contract() {
    let env = Env::default();
    let (_contract_id, client) = fresh_contract(&env);

    assert_typed_not_initialized!(
        client.try_get_platform_fee_allocation(),
        "get_platform_fee_allocation on a freshly registered contract"
    );
}

// ── 2 ─ the error is derived from an absent key, not decoration ──────────────

/// The typed error must come from the storage miss it claims to: both
/// `DataKey::PlatformFeeAllocation` and `DataKey::PlatformFeeAllocationLock`
/// are still absent after the read, and the read did not quietly supply a
/// default `{0, 10_000, 0, locked: false}` allocation in their place.
#[test]
fn get_platform_fee_allocation_error_comes_from_an_absent_storage_key() {
    let env = Env::default();
    let (contract_id, client) = fresh_contract(&env);

    assert_typed_not_initialized!(
        client.try_get_platform_fee_allocation(),
        "get_platform_fee_allocation on a freshly registered contract"
    );

    let (allocation, lock) = raw_allocation_keys(&env, &contract_id);
    assert_eq!(
        allocation, None,
        "the read must not materialize a defaulted allocation on a fresh contract"
    );
    assert_eq!(
        lock, None,
        "the read must not touch the platform-fee lock flag"
    );
}

// ── 3 ─ `calculate_platform_fee_split` never splits an uninitialized contract ─

/// `calculate_platform_fee_split` reads the same allocation, so it must fail
/// with the same typed error for *every* amount shape — including the inputs
/// that are individually invalid (`0`, negative) or extreme (`i128::MAX`,
/// `i128::MIN`, which would otherwise be reported as an arithmetic overflow).
///
/// Asserting the whole matrix proves the initialization guard runs before any
/// argument validation or arithmetic, so callers are told the contract is not
/// initialized rather than being handed a misleading amount error — and never
/// handed a split computed from a defaulted allocation.
#[test]
fn calculate_platform_fee_split_returns_typed_not_initialized_for_every_amount_shape() {
    let env = Env::default();
    let (_contract_id, client) = fresh_contract(&env);

    let amounts: [i128; 6] = [0, 1, 10_000, i128::MAX, i128::MIN, -1];

    for amount in amounts {
        assert_typed_not_initialized!(
            client.try_calculate_platform_fee_split(&amount),
            "calculate_platform_fee_split on a freshly registered contract"
        );
    }
}

// ── 4 ─ the writer is rejected before it can seed a configuration ────────────

/// `set_platform_fee_allocation` on a freshly registered contract is rejected
/// with the typed `NotInitialized` — for a *valid* BPS triple as well as for an
/// invalid one.  The invalid triple matters: `InvalidRatio` / `FeeTooHigh` must
/// not mask the initialization failure, otherwise the first thing an operator
/// is told about a mis-parameterised call on an uninitialized contract is a
/// confusing parameter error.
///
/// Authorization is mocked so that the only possible outcome is the storage
/// miss; without it an auth failure would mask the property under test.
#[test]
fn set_platform_fee_allocation_returns_typed_not_initialized_on_fresh_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract_id, client) = fresh_contract(&env);
    let admin = Address::generate(&env);

    // Sums to 10_000 and respects both caps — a triple the initialized contract
    // would accept.
    assert_typed_not_initialized!(
        client.try_set_platform_fee_allocation(&admin, &2_000_u32, &7_000_u32, &1_000_u32),
        "set_platform_fee_allocation with a valid BPS triple"
    );

    // Sums to 3_000 — invalid, but must not be reported as such.
    assert_typed_not_initialized!(
        client.try_set_platform_fee_allocation(&admin, &1_000_u32, &1_000_u32, &1_000_u32),
        "set_platform_fee_allocation with an invalid BPS triple"
    );

    let (allocation, lock) = raw_allocation_keys(&env, &contract_id);
    assert_eq!(
        allocation, None,
        "a rejected configuration must not seed an allocation"
    );
    assert_eq!(
        lock, None,
        "a rejected configuration must not leave the re-entrancy lock behind"
    );
}

// ── 5 ─ `lock_platform_fee_allocation` ───────────────────────────────────────

/// Locking an allocation that does not exist is rejected with the typed
/// `NotInitialized` rather than trapping on the missing entry.  Authorization
/// is mocked so the storage miss is what decides the outcome.
#[test]
fn lock_platform_fee_allocation_returns_typed_not_initialized_on_fresh_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let (_contract_id, client) = fresh_contract(&env);
    let admin = Address::generate(&env);

    assert_typed_not_initialized!(
        client.try_lock_platform_fee_allocation(&admin),
        "lock_platform_fee_allocation on a freshly registered contract"
    );
}

// ── 6 ─ `pf_alloc_admin_override` ────────────────────────────────────────────

/// The admin override exists to replace a *locked* allocation; on a fresh
/// contract there is nothing to override, and the call must say so with the
/// typed error instead of panicking on the absent key.
#[test]
fn pf_alloc_admin_override_returns_typed_not_initialized_on_fresh_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let (_contract_id, client) = fresh_contract(&env);
    let admin = Address::generate(&env);

    assert_typed_not_initialized!(
        client.try_pf_alloc_admin_override(&admin, &2_000_u32, &7_000_u32, &1_000_u32),
        "pf_alloc_admin_override on a freshly registered contract"
    );
}

// ── 7 ─ repetition must not drift into a default ─────────────────────────────

/// The rejection is stable: 25 consecutive reads each return the typed error
/// and never leave an allocation behind.  This guards against a future
/// "lazy default" change that would seed the entry on the first miss and start
/// returning `Ok` from the second call onwards.
#[test]
fn repeated_reads_on_a_fresh_contract_never_materialize_an_allocation() {
    let env = Env::default();
    let (contract_id, client) = fresh_contract(&env);

    for _ in 0..25 {
        assert_typed_not_initialized!(
            client.try_get_platform_fee_allocation(),
            "repeated get_platform_fee_allocation on a freshly registered contract"
        );
    }

    let (allocation, lock) = raw_allocation_keys(&env, &contract_id);
    assert_eq!(
        allocation, None,
        "25 rejected reads must not materialize an allocation"
    );
    assert_eq!(lock, None, "25 rejected reads must not touch the lock flag");
}

// ── 8 ─ nothing is written on any rejection path ─────────────────────────────

/// Every platform-fee entrypoint is exercised on one freshly registered
/// contract, and the *entire* ledger is compared before and after.
///
/// `LedgerSnapshot` covers all contracts registered on the `Env` — instance,
/// persistent and temporary storage plus TTLs and the ledger info — so this is a
/// byte-for-byte comparison of the whole ledger, not just of the two keys the
/// read path touches.  A rejection that wrote anything (a sentinel, a lock
/// flag, a TTL bump) would change the snapshot even though the call failed.
#[test]
fn not_initialized_rejections_do_not_mutate_the_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract_id, client) = fresh_contract(&env);
    let admin = Address::generate(&env);

    let before = env.to_ledger_snapshot();

    assert_typed_not_initialized!(
        client.try_get_platform_fee_allocation(),
        "get_platform_fee_allocation"
    );
    assert_typed_not_initialized!(
        client.try_calculate_platform_fee_split(&10_000_i128),
        "calculate_platform_fee_split"
    );
    assert_typed_not_initialized!(
        client.try_set_platform_fee_allocation(&admin, &2_000_u32, &7_000_u32, &1_000_u32),
        "set_platform_fee_allocation"
    );
    assert_typed_not_initialized!(
        client.try_lock_platform_fee_allocation(&admin),
        "lock_platform_fee_allocation"
    );
    assert_typed_not_initialized!(
        client.try_pf_alloc_admin_override(&admin, &2_000_u32, &7_000_u32, &1_000_u32),
        "pf_alloc_admin_override"
    );

    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "rejected platform-fee calls on an uninitialized contract must not \
         mutate any ledger entry"
    );

    let (allocation, lock) = raw_allocation_keys(&env, &contract_id);
    assert_eq!(allocation, None, "no allocation may survive the rejections");
    assert_eq!(lock, None, "no lock flag may survive the rejections");
}

// ── 9 ─ a failed `initialize` must not leave a default behind ────────────────

/// `initialize` writes the default allocation before it finishes validating
/// its remaining arguments, so a rejected `initialize` is the one case where a
/// defaulted allocation really is written and then has to disappear again.
///
/// The Soroban host rolls back every storage write made during a failed
/// invocation, so after the rejection the contract must be indistinguishable
/// from one that was never initialized: no allocation, no lock flag, and the
/// read still reporting the typed error.  If the rollback ever stopped
/// covering the allocation write, callers would start observing a fee split
/// (`Ok(default)`) for an escrow that does not exist — the exact failure mode
/// the issue calls out.
#[test]
fn failed_initialize_leaves_no_defaulted_allocation_behind() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let (contract_id, client) = fresh_contract(&env);

    // `auto_release_seconds == 0` is rejected *after* the allocation write.
    let result = client.try_initialize(
        &admin,
        &client_addr,
        &freelancer,
        &arbiter,
        &token_id,
        &0u64,
        &vec![&env, 1_000_i128],
    );
    assert!(
        matches!(&result, Err(Ok(Error::InvalidAmount))),
        "initialize must fail with the typed InvalidAmount here, got {result:?}"
    );

    assert_typed_not_initialized!(
        client.try_get_platform_fee_allocation(),
        "read after a rolled-back initialize"
    );

    let (allocation, lock) = raw_allocation_keys(&env, &contract_id);
    assert_eq!(
        allocation, None,
        "the default allocation written by the failed initialize must be rolled back"
    );
    assert_eq!(
        lock, None,
        "the failed initialize must not leave the platform-fee lock flag behind"
    );
}

// ── 10 ─ positive control: the guard is not over-broad ───────────────────────

/// After a successful `initialize` the same read returns `Ok` with the
/// documented default allocation, and the split endpoint produces a
/// distribution.  This pins the *scope* of the rejection: it is keyed on the
/// absence of the allocation (i.e. "before initialize"), not on the absence of
/// an argument, not on the caller, and not on a pause flag.
#[test]
fn initialized_contract_returns_ok_and_never_not_initialized() {
    let env = Env::default();
    let (_contract_id, client) = initialized_escrow(&env);

    let allocation = client.get_platform_fee_allocation();
    assert_eq!(
        allocation,
        PlatformFeeAllocation {
            client_bps: 0,
            freelancer_bps: 10_000,
            treasury_bps: 0,
            locked: false,
        },
        "initialize must write the documented default allocation"
    );

    // Panics with the contract's typed error if this regressed to
    // `NotInitialized` on an initialized contract.
    let _distribution = client.calculate_platform_fee_split(&10_000_i128);
}

// ── 11 ─ the typed error carries a stable code ───────────────────────────────

/// Off-chain callers branch on the stable contract error code, which is the
/// concrete reason "surfaces a typed error" is the requirement rather than
/// "fails somehow": a trap carries no decodable code at all, and a defaulted
/// allocation carries no error.  `NotInitialized` is code `2`.
#[test]
fn not_initialized_is_reported_as_contract_error_code_two() {
    assert_eq!(Error::NotInitialized as u32, 2);
}
