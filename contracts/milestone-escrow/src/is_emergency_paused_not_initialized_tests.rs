//! Validation tests for `is_emergency_paused`.
//!
//! # Purpose
//!
//! `is_emergency_paused` is a **pure read** endpoint that:
//! * returns `Err(Error::NotInitialized)` when it is invoked on a contract
//!   whose `initialize` has never completed (the `DataKey::Ep` pause flag was
//!   never written), and
//! * otherwise returns `Ok(flag)` carrying exactly the value last written to
//!   `DataKey::Ep` by `emergency_pause`, `emergency_unpause`, or
//!   `emergency_pause_admin_override`.
//!
//! Before the fix under test, a missing flag was defaulted to `false`, so a
//! caller could not tell "the escrow is running" apart from "this contract was
//! never set up", and a monitoring client polling the flag on a mistyped or
//! un-deployed contract id would silently conclude the escrow was healthy.
//!
//! The Soroban SDK test client exposes two call forms:
//! * `client.is_emergency_paused()` — panics on a contract error, returns `bool`
//!   directly on success.
//! * `client.try_is_emergency_paused()` — returns
//!   `Result<Result<bool, ConversionError>, Result<Error, InvokeError>>`;
//!   a contract-level `Err(E)` surfaces as `Err(Ok(E))`.
//!
//! # Test matrix
//!
//! | #  | Scenario                                          | try_ form                | Storage effect     |
//! |----|---------------------------------------------------|--------------------------|--------------------|
//! | 1  | Freshly registered contract, never initialized     | `Err(Ok(NotInitialized))`| zero writes        |
//! | 2  | Error is stable across repeated calls              | `Err(Ok(NotInitialized))`| zero writes        |
//! | 3  | `initialize` rejected → contract still uninitialized | `Err(Ok(NotInitialized))`| zero writes      |
//! | 4  | Guard is per-instance, not global                  | `Err(Ok(NotInitialized))`| zero writes        |
//! | 5  | After `initialize`, before funding                 | `Ok(Ok(false))`          | zero writes        |
//! | 6  | After `initialize` + `fund`                        | `Ok(Ok(false))`          | zero writes        |
//! | 7  | After `emergency_pause`                           | `Ok(Ok(true))`           | zero writes        |
//! | 8  | While paused, the read stays available             | `Ok(Ok(true))`           | zero writes        |
//! | 9  | After `emergency_unpause`                         | `Ok(Ok(false))`          | zero writes        |
//! | 10 | After `emergency_pause_admin_override(_, true)`    | `Ok(Ok(true))`           | zero writes        |
//! | 11 | After `emergency_pause_admin_override(_, false)`   | `Ok(Ok(false))`          | zero writes        |
//! | 12 | Repeated reads are idempotent                      | same value each time     | zero writes        |
//! | 13 | Reported flag equals the stored `DataKey::Ep`      | `Ok`                     | zero writes        |
//! | 14 | Internal guards keep their typed errors            | `NotInitialized`         | zero writes        |

use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

// ── setup helpers ─────────────────────────────────────────────────────────────

/// Register + `initialize` an escrow **without** funding it, so tests can
/// observe the flag as soon as initialization has committed.
fn setup_initialized_escrow(env: &Env) -> (Address, Address, Address, Address, Address) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &vec![env, 1_000_i128],
    );

    (
        client_addr,
        freelancer_addr,
        admin_addr,
        contract_id,
        token_contract_id,
    )
}

// ── ledger snapshot helper ───────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct LedgerSnapshot {
    /// Raw pause flag, `None` when the key is absent (never initialized).
    ep: Option<bool>,
    /// Presence of the pause flag itself, so "absent" cannot be confused with
    /// a hypothetical stored `false`.
    ep_exists: bool,
    job_exists: bool,
    admin_instance: Option<Address>,
    admin_persistent: Option<Address>,
    /// Re-entrancy sentinel for a pause transition; must never be left set by
    /// a read.
    ep_lock: Option<bool>,
}

fn capture_ledger(env: &Env, contract_id: &Address) -> LedgerSnapshot {
    env.as_contract(contract_id, || {
        let ep: Option<bool> = env.storage().instance().get(&DataKey::Ep);
        LedgerSnapshot {
            ep,
            ep_exists: env.storage().instance().has(&DataKey::Ep),
            job_exists: env.storage().instance().has(&DataKey::Job),
            admin_instance: env.storage().instance().get(&DataKey::Admin),
            admin_persistent: env.storage().persistent().get(&DataKey::Admin),
            ep_lock: env.storage().instance().get(&DataKey::EpLk),
        }
    })
}

fn assert_storage_unchanged(before: &LedgerSnapshot, after: &LedgerSnapshot) {
    assert_eq!(
        after.ep, before.ep,
        "DataKey::Ep must not be mutated by a read"
    );
    assert_eq!(
        after.ep_exists, before.ep_exists,
        "presence of DataKey::Ep must not change"
    );
    assert_eq!(
        after.job_exists, before.job_exists,
        "DataKey::Job must not be mutated by a read"
    );
    assert_eq!(
        after.admin_instance, before.admin_instance,
        "instance DataKey::Admin must not be mutated"
    );
    assert_eq!(
        after.admin_persistent, before.admin_persistent,
        "persistent DataKey::Admin must not be mutated"
    );
    assert_eq!(
        after.ep_lock, before.ep_lock,
        "DataKey::EpLk must not be mutated by a read"
    );
}

// ── 1 ─ freshly registered contract ──────────────────────────────────────────

/// Core requirement of the issue: invoking `is_emergency_paused` on a contract
/// that was registered but never initialized must surface a typed
/// `NotInitialized` error rather than a defaulted `false`.
#[test]
fn test_is_emergency_paused_returns_not_initialized_on_freshly_registered_contract() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // The pause flag was never written, so the read must not report a value.
    let before = capture_ledger(&env, &contract_id);
    assert_eq!(before.ep, None, "test setup: flag must start absent");

    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
    assert_storage_unchanged(&before, &after);
}

/// The typed error is deterministic: repeated reads on an uninitialized
/// contract keep reporting `NotInitialized` rather than flipping to `false`
/// once some other key happens to exist.
#[test]
fn test_is_emergency_paused_not_initialized_is_stable_across_repeated_calls() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    for _ in 0..3 {
        let before = capture_ledger(&env, &contract_id);
        let result = client.try_is_emergency_paused();
        let after = capture_ledger(&env, &contract_id);

        assert_eq!(result, Err(Ok(Error::NotInitialized)));
        assert_storage_unchanged(&before, &after);
    }
}

// ── 2 ─ a rejected initialize must not look initialized ──────────────────────

/// `initialize` writes its `DataKey::Job` re-entrancy sentinel before
/// validating the milestone list.  When a later check rejects the call the host
/// rolls that write back, so the read must still report `NotInitialized` —
/// proving the guard keys off the pause flag (committed by a *successful*
/// `initialize`) rather than the sentinel.
#[test]
fn test_is_emergency_paused_still_not_initialized_after_rejected_initialize() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Empty milestone list: rejected with InvalidAmount after the sentinel write.
    let rejected = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &vec![&env],
    );
    assert_eq!(rejected, Err(Ok(Error::InvalidAmount)));

    let before = capture_ledger(&env, &contract_id);
    assert!(!before.job_exists, "sentinel write must be rolled back");

    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Err(Ok(Error::NotInitialized)));
    assert_storage_unchanged(&before, &after);
}

// ── 3 ─ the guard is per-instance ────────────────────────────────────────────

/// Initializing one contract instance must not make an unrelated, uninitialized
/// instance answer the read: the guard inspects this contract's own storage.
#[test]
fn test_is_emergency_paused_not_initialized_is_scoped_per_contract_instance() {
    let env = Env::default();
    env.mock_all_auths();

    let initialized_id = env.register(MilestoneEscrow, ());
    let initialized = MilestoneEscrowClient::new(&env, &initialized_id);
    let admin_addr = Address::generate(&env);
    initialized.initialize(
        &admin_addr,
        &Address::generate(&env),
        &Address::generate(&env),
        &Address::generate(&env),
        &env.register_stellar_asset_contract_v2(admin_addr.clone())
            .address(),
        &604800,
        &vec![&env, 1_000_i128],
    );
    assert_eq!(initialized.try_is_emergency_paused(), Ok(Ok(false)));

    let pristine_id = env.register(MilestoneEscrow, ());
    let pristine = MilestoneEscrowClient::new(&env, &pristine_id);

    assert_eq!(
        pristine.try_is_emergency_paused(),
        Err(Ok(Error::NotInitialized))
    );
}

// ── 4 ─ initialized contract reports the stored flag ─────────────────────────

/// `initialize` commits `DataKey::Ep = false`, so the very first read after
/// initialization succeeds and reports "not paused".
#[test]
fn test_is_emergency_paused_returns_false_after_initialize_only() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, contract_id, _) = setup_initialized_escrow(&env);
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let before = capture_ledger(&env, &contract_id);
    assert_eq!(before.ep, Some(false), "initialize must seed the flag");

    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Ok(Ok(false)));
    assert_storage_unchanged(&before, &after);
}

/// A funded escrow is not paused either, and the read is available to anyone
/// (no authorisation is required).
#[test]
fn test_is_emergency_paused_returns_false_for_funded_escrow() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = capture_ledger(&env, &contract_id);
    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Ok(Ok(false)));
    assert_storage_unchanged(&before, &after);
}

/// After a party-authorised freeze the flag flips to `true`; a monitor polling
/// the read sees the freeze immediately.
#[test]
fn test_is_emergency_paused_returns_true_after_emergency_pause() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.emergency_pause(&client_addr, &freelancer_addr);

    let before = capture_ledger(&env, &contract_id);
    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Ok(Ok(true)));
    assert_storage_unchanged(&before, &after);
}

/// The read stays available while the contract is frozen — the pause gates
/// money-moving endpoints, not observability.
#[test]
fn test_is_emergency_paused_is_readable_while_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);

    let before = capture_ledger(&env, &contract_id);
    assert_eq!(before.ep, Some(true), "contract must be paused");

    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Ok(Ok(true)));
    assert_storage_unchanged(&before, &after);
}

/// Lifting the freeze flips the reported value back to `false`.
#[test]
fn test_is_emergency_paused_returns_false_after_emergency_unpause() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);
    client.emergency_unpause(&admin_addr);

    let before = capture_ledger(&env, &contract_id);
    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Ok(Ok(false)));
    assert_storage_unchanged(&before, &after);
}

// ── 5 ─ admin override path ──────────────────────────────────────────────────

/// `emergency_pause_admin_override(admin, true)` is reflected by the read even
/// though neither party signed, so an operator's unilateral freeze is visible
/// to monitoring.
#[test]
fn test_is_emergency_paused_returns_true_after_admin_override_pause() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.emergency_pause_admin_override(&admin_addr, &true);

    let before = capture_ledger(&env, &contract_id);
    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Ok(Ok(true)));
    assert_storage_unchanged(&before, &after);
}

/// The override can also release a freeze, and the read follows it back to
/// `false`.
#[test]
fn test_is_emergency_paused_returns_false_after_admin_override_unpause() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);
    client.emergency_pause_admin_override(&admin_addr, &false);

    let before = capture_ledger(&env, &contract_id);
    let result = client.try_is_emergency_paused();
    let after = capture_ledger(&env, &contract_id);

    assert_eq!(result, Ok(Ok(false)));
    assert_storage_unchanged(&before, &after);
}

// ── 6 ─ idempotence and agreement with stored state ──────────────────────────

/// Polling the flag repeatedly is stable and never mutates the ledger, so a
/// monitor can poll it as often as it likes.
#[test]
fn test_is_emergency_paused_repeated_reads_are_idempotent_and_read_only() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.emergency_pause(&client_addr, &freelancer_addr);

    for _ in 0..3 {
        let before = capture_ledger(&env, &contract_id);
        let result = client.try_is_emergency_paused();
        let after = capture_ledger(&env, &contract_id);

        assert_eq!(result, Ok(Ok(true)));
        assert_storage_unchanged(&before, &after);
    }
}

/// The endpoint is a read of `DataKey::Ep` and nothing else: whatever value is
/// stored under that key is exactly what the endpoint reports, at every point
/// of the pause lifecycle.
#[test]
fn test_is_emergency_paused_always_matches_stored_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let assert_matches_stored = |contract_id: &Address, client: &MilestoneEscrowClient| {
        let stored = capture_ledger(&env, contract_id).ep;
        let reported = client.try_is_emergency_paused();
        assert_eq!(
            reported,
            Ok(Ok(stored.expect("flag must exist once initialized")))
        );
    };

    assert_matches_stored(&contract_id, &client);
    client.emergency_pause(&client_addr, &freelancer_addr);
    assert_matches_stored(&contract_id, &client);
    client.emergency_pause_admin_override(&admin_addr, &false);
    assert_matches_stored(&contract_id, &client);
    client.emergency_pause_admin_override(&admin_addr, &true);
    assert_matches_stored(&contract_id, &client);
    client.emergency_unpause(&admin_addr);
    assert_matches_stored(&contract_id, &client);
}

// ── 7 ─ internal guards keep their documented error precedence ──────────────

/// The pause-gated endpoints read the flag through the same helper.  On a
/// contract that was never initialized they must still fail with their
/// documented `NotInitialized` — i.e. the new typed read did not turn their
/// pre-existing initialization guard into a panic or a defaulted value.
#[test]
fn test_pause_gated_endpoints_still_report_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_emergency_pause(&stranger, &stranger),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_emergency_unpause(&stranger),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_emergency_pause_claim_refund(&stranger, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::NotInitialized))
    );
}
