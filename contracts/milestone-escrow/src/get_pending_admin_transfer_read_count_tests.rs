#![cfg(test)]
//! Storage-read suite for `get_pending_admin_transfer` (issue #496).
//!
//! Before this change every call read **two** storage keys: `DataKey::Admin` —
//! pulled through `load_admin`, which deserializes an admin address the getter
//! never uses — and `DataKey::PendingAdminTransfer`.
//!
//! `read_pending_admin_transfer` now resolves `DataKey::PendingAdminTransfer`
//! exactly once and consults `DataKey::Admin` only when no proposal is stored,
//! and then with a non-deserializing `has` presence probe.  A call that finds a
//! pending proposal therefore never puts the `Admin` entry in the invocation
//! footprint at all.
//!
//! The tests pin that two ways:
//!   - The per-invocation resource metering (`env.cost_estimate().resources()`)
//!     reports the proposal-present path as exactly one entry fewer than the
//!     no-proposal path — the `Admin` guard entry this change removes.  On both
//!     paths the count is the invocation's constant single contract-instance
//!     entry plus one entry per storage key actually touched.  Nothing is ever
//!     written on any path.
//!
//! The one path that does *not* get cheaper is the `NotInitialized` error path:
//! it used to bail out after a single `Admin` read and now resolves the payload
//! key first (2 -> 3 entries).  That is the deliberate trade for skipping the
//! guard on the proposal path, and it costs nothing on either success path.
//!   - The returned value is exactly what a direct read of the same key yields,
//!     in every state: `Err(NotInitialized)`, `Ok(None)` and `Ok(Some(..))`.
//!
//! These tests run without snapshot capture (like the other footprint suites),
//! because they assert on metered resources rather than on ledger bytes; the
//! byte-identical snapshot guarantees live in
//! `get_pending_admin_transfer_tests`.

use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient, PendingAdminTransfer};
use soroban_sdk::testutils::{Address as _, EnvTestConfig};
use soroban_sdk::{vec, Address, Env};

fn env_without_snapshot() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// Register a contract whose persistent `Admin` is seeded exactly as
/// `initialize` writes it, plus the given pending proposal (if any).
fn seeded<'a>(
    env: &'a Env,
    pending: &Option<PendingAdminTransfer>,
) -> (Address, MilestoneEscrowClient<'a>) {
    let contract_id = env.register(MilestoneEscrow, ());
    let admin = Address::generate(env);
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&DataKey::Admin, &admin);
        if let Some(proposal) = pending {
            env.storage()
                .persistent()
                .set(&DataKey::PendingAdminTransfer, proposal);
        }
    });
    let client = MilestoneEscrowClient::new(env, &contract_id);
    (contract_id, client)
}

fn proposal(env: &Env, proposal_id: u32) -> PendingAdminTransfer {
    PendingAdminTransfer {
        new_admin: Address::generate(env),
        proposal_id,
    }
}

// ── storage reads per call ───────────────────────────────────────────────────

/// Issue #496: with a proposal stored, the call never touches the `Admin`
/// storage entry.
///
/// The contract is registered natively here, so there is no separate
/// contract-code entry to count, and the getter takes no authorization, so
/// there is no auth-nonce entry either.  The measured count of `2` is therefore
/// the invocation's constant single contract-instance entry plus the one
/// persistent `PendingAdminTransfer` entry — and **not** the persistent `Admin`
/// entry this suite rules out (which would make it `3`).
#[test]
fn test_get_pending_admin_transfer_proposal_reads_only_the_payload_entry() {
    let env = env_without_snapshot();
    let stored = proposal(&env, 42);
    let (_, escrow) = seeded(&env, &Some(stored.clone()));

    assert_eq!(escrow.get_pending_admin_transfer(), Some(stored));
    let resources = env.cost_estimate().resources();

    assert_eq!(
        resources.memory_read_entries, 2,
        "with a proposal stored the call must touch only the contract-instance \
         entry and the PendingAdminTransfer entry; the persistent Admin entry \
         must not be among them; measured {resources:?}"
    );
    assert_eq!(resources.write_entries, 0, "a read path writes nothing");
    assert_eq!(resources.disk_read_entries, 0);
}

/// With no proposal stored the guard still has to consult `Admin` to tell
/// "initialized, nothing pending" from "never initialized" — one entry more
/// than the proposal-present path, and still no writes.
///
/// The measured count of `3` is the constant contract-instance entry plus the
/// two storage keys this path reads: the absent `PendingAdminTransfer` key and
/// the present `Admin` key.  That extra `Admin` read is the price of the typed
/// `NotInitialized` error, and it is paid only when no proposal is stored.
#[test]
fn test_get_pending_admin_transfer_no_proposal_reads_the_admin_guard() {
    let env = env_without_snapshot();
    let (_, escrow) = seeded(&env, &None);

    assert_eq!(escrow.get_pending_admin_transfer(), None);
    let resources = env.cost_estimate().resources();

    assert_eq!(
        resources.memory_read_entries, 3,
        "an absent proposal needs the Admin presence guard as well: \
         contract-instance + PendingAdminTransfer + Admin; measured {resources:?}"
    );
    assert_eq!(resources.write_entries, 0, "a read path writes nothing");
    assert_eq!(resources.disk_read_entries, 0);
}

/// The uninitialized path is read-only and reports the same typed error as
/// before, now from the presence guard rather than from `load_admin`.
///
/// It is also the one path this change makes slightly more expensive (2 -> 3
/// entries): the payload key is resolved before the guard, so a never-initialized
/// contract pays for that extra absent-key probe.  Pinning the number here keeps
/// the trade-off visible rather than accidental.
#[test]
fn test_get_pending_admin_transfer_uninitialized_reads_only_the_guard() {
    let env = env_without_snapshot();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        escrow.try_get_pending_admin_transfer(),
        Err(Ok(Error::NotInitialized))
    );
    let resources = env.cost_estimate().resources();

    assert_eq!(
        resources.memory_read_entries, 3,
        "the NotInitialized path touches the contract-instance entry, the \
         absent PendingAdminTransfer key and the absent Admin key; \
         measured {resources:?}"
    );
    assert_eq!(
        resources.write_entries, 0,
        "the NotInitialized path writes nothing; measured {resources:?}"
    );
    assert_eq!(resources.disk_read_entries, 0);
}

/// The core assertion of issue #496: finding a pending proposal costs strictly
/// fewer storage reads than finding none.
///
/// Both measurements are the first invocation of a freshly-seeded env with the
/// same shape, so the two footprints are directly comparable; the `Admin`
/// entry exists in both, yet only the no-proposal call puts it in the
/// footprint.
#[test]
fn test_get_pending_admin_transfer_proposal_is_cheaper_than_no_proposal() {
    let env_with = env_without_snapshot();
    let stored = proposal(&env_with, 7);
    let (_, escrow_with) = seeded(&env_with, &Some(stored.clone()));
    assert_eq!(escrow_with.get_pending_admin_transfer(), Some(stored));
    let with_proposal = env_with.cost_estimate().resources();

    let env_without = env_without_snapshot();
    let (_, escrow_without) = seeded(&env_without, &None);
    assert_eq!(escrow_without.get_pending_admin_transfer(), None);
    let without_proposal = env_without.cost_estimate().resources();

    assert!(
        with_proposal.memory_read_entries < without_proposal.memory_read_entries,
        "the proposal-present path must read fewer entries than the \
         no-proposal path; measured {with_proposal:?} vs {without_proposal:?}"
    );
    assert_eq!(
        without_proposal.memory_read_entries - with_proposal.memory_read_entries,
        1,
        "exactly one entry — the persistent Admin guard — is saved; \
         measured {with_proposal:?} vs {without_proposal:?}"
    );
}

/// The metering is stable across repeated calls: every call costs the same two
/// entries (contract-instance + `PendingAdminTransfer`), the `Admin` entry is
/// never touched, and none of the calls writes anything.
#[test]
fn test_get_pending_admin_transfer_reads_do_not_accumulate() {
    let env = env_without_snapshot();
    let stored = proposal(&env, 3);
    let (_, escrow) = seeded(&env, &Some(stored.clone()));

    for _ in 0..3 {
        assert_eq!(escrow.get_pending_admin_transfer(), Some(stored.clone()));
        let resources = env.cost_estimate().resources();
        assert_eq!(
            resources.memory_read_entries, 2,
            "every call must cost the same two entries; measured {resources:?}"
        );
        assert_eq!(resources.write_entries, 0);
    }
}

// ── the returned value is unchanged ──────────────────────────────────────────

/// The getter's result always equals a direct read of the same key, for every
/// state the contract can be in — initialized with a proposal, initialized
/// without one, and never initialized.
#[test]
fn test_get_pending_admin_transfer_matches_direct_storage_read() {
    let env = env_without_snapshot();

    // Initialized, proposal present.
    let stored = proposal(&env, 11);
    let (contract_id, escrow) = seeded(&env, &Some(stored.clone()));
    let direct: Option<PendingAdminTransfer> = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
    });
    assert_eq!(escrow.get_pending_admin_transfer(), direct);
    assert_eq!(direct, Some(stored));

    // Initialized, no proposal.
    let (contract_id, escrow) = seeded(&env, &None);
    let direct: Option<PendingAdminTransfer> = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
    });
    assert_eq!(escrow.get_pending_admin_transfer(), direct);
    assert_eq!(direct, None);

    // Never initialized.
    let fresh_id = env.register(MilestoneEscrow, ());
    let fresh = MilestoneEscrowClient::new(&env, &fresh_id);
    assert_eq!(
        fresh.try_get_pending_admin_transfer(),
        Err(Ok(Error::NotInitialized))
    );
}

/// The payload comes back verbatim — both fields, including a zero
/// `proposal_id` and the `u32` maximum — and the stored value is unchanged
/// after the getter has run.
#[test]
fn test_get_pending_admin_transfer_returns_payload_verbatim() {
    let env = env_without_snapshot();

    for proposal_id in [0u32, 1, u32::MAX] {
        let stored = proposal(&env, proposal_id);
        let (contract_id, escrow) = seeded(&env, &Some(stored.clone()));

        let before: Option<PendingAdminTransfer> = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get(&DataKey::PendingAdminTransfer)
        });

        let read = escrow.get_pending_admin_transfer();
        assert_eq!(read, Some(stored.clone()));
        let read = read.expect("proposal must be present");
        assert_eq!(
            read.new_admin, stored.new_admin,
            "new_admin must be verbatim"
        );
        assert_eq!(
            read.proposal_id, proposal_id,
            "proposal_id must be verbatim"
        );

        let after: Option<PendingAdminTransfer> = env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .get(&DataKey::PendingAdminTransfer)
        });
        assert_eq!(before, after, "the getter must not mutate the stored value");
    }
}

/// End-to-end through the real API: the value observed through
/// `initialize` -> `propose_admin_transfer` -> `cancel_admin_transfer_proposal`
/// matches the stored key at every step.
#[test]
fn test_get_pending_admin_transfer_matches_api_lifecycle() {
    let env = env_without_snapshot();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_admin = Address::generate(&env);

    assert_eq!(escrow.get_pending_admin_transfer(), None);

    escrow.propose_admin_transfer(&admin_addr, &new_admin, &5u32);
    let stored: Option<PendingAdminTransfer> = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
    });
    assert_eq!(escrow.get_pending_admin_transfer(), stored);
    let stored = stored.expect("proposal must be stored");
    assert_eq!(stored.new_admin, new_admin);
    assert_eq!(stored.proposal_id, 5u32);

    escrow.cancel_admin_transfer_proposal(&admin_addr);
    assert_eq!(escrow.get_pending_admin_transfer(), None);
}
