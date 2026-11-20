#![cfg(test)]
//! Ledger-storage footprint suite for `multisig_approve` (issue #457).
//!
//! # What this call touches
//! `multisig_approve` reads the registered signer set and the approval
//! threshold (the consolidated `MultiSigConfig`) and the proposal record
//! `Proposal(proposal_id)` (**persistent**, issues #785/#996), which holds the
//! approval bitmap — one bit per signer index.  Since issue #787 it no longer
//! reads the job metadata or the token balance.
//!
//! The history below describes the #457 change when the bitmap still lived in
//! a temporary `MultiSigApproval(proposal_id)` key; the guarantee it set up —
//! a duplicate approval writes no contract storage key — carries over to the
//! proposal record unchanged.
//!
//! # Before
//! The bitmap write was unconditional: every call re-wrote the same `u32`,
//! including a duplicate approval whose signer bit was already set and which
//! therefore could not change a stored byte.  Measured with the test SDK's
//! per-invocation metering, a duplicate approval performed **2** ledger entry
//! writes / **180** write bytes, one of which (108 bytes) was that redundant
//! rewriting of the bitmap.
//!
//! # After
//! The bitmap is written only when the signer's bit actually changes, so a
//! duplicate approval writes **no contract storage key at all**: 2 entry writes
//! → 1 and 180 write bytes → 72, the write that remains being the auth nonce
//! entry every authenticated call consumes.  The first approval from each signer
//! is unchanged — one bitmap write, the floor for recording an approval — and so
//! are the returned `MultiSigApprovalState`, the `msigappr` event, every error
//! path, and the bytes held for the bitmap (value and TTL identical).
//!
//! # What was deliberately *not* consolidated
//! The hypothesis behind the issue is that a proposal's metadata and its bitmap
//! are read and written as two separate keys that could be merged into one.
//! They are not: the signer set/threshold live in a single **instance** key
//! inside the `contract_instance` entry the call must read regardless, and no
//! approval writes it, while the bitmap is the only per-proposal key.  Both
//! possible merges are regressions, which is why neither is performed here:
//! * folding the signer set into the per-proposal bitmap entry would duplicate
//!   the signer vec into every proposal and move the shared config out of
//!   instance storage, losing the automatic eviction of the temporary tier;
//! * folding the bitmap into the always-live instance entry would make every
//!   approval re-write the whole instance entry — job metadata included — and
//!   would keep per-proposal state alive forever instead of letting it expire.
//!
//! The metadata consolidation that *does* exist — the former `MultiSigSigners` +
//! `MultiSigThreshold` pair into the single `MultiSigConfig` instance key — is
//! issue #456's and is not duplicated here; the key-shape tests below pin the
//! current layout so that any further change is a deliberate, reviewed edit
//! rather than a silent one.
//!
//! # Tests
//! 1. `test_multisig_approve_duplicate_writes_no_contract_storage_key` — the
//!    reduction demanded by issue #457, with a control proving the metric
//!    distinguishes a real bitmap write from the elided one and a second control
//!    proving the stored bitmap's value and TTL are untouched either way.
//! 2. `test_multisig_approve_writes_one_bitmap_key_per_proposal` — the key-level
//!    shape: approving N proposals adds exactly N temporary keys, and no
//!    instance key at all.
//! 3. `test_multisig_approve_duplicate_is_idempotent_and_emits_the_same_event`
//!    — behaviour identity of the duplicate path (state, event, stored bytes).
//! 4. `test_multisig_approve_sets_the_bit_for_each_signer_index` — which bit is
//!    set, for every signer index, is unchanged.
//! 5. `test_multisig_approve_threshold_detection_unchanged` — threshold-reached
//!    detection is unchanged, including across duplicate approvals.
//! 6. `test_multisig_approve_rejections_write_no_bitmap` — every rejection path
//!    still creates no bitmap key.

use super::*;
use crate::{DataKey, Error, MultiSigApprovedEvent};
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _, Temporary as _};
use soroban_sdk::{vec, Address, Env, Vec};

/// Shared setup: register an escrow, optionally fund it, and register a
/// 2-of-3 multisig regime on it.
fn multisig_escrow(
    env: &Env,
    fund: bool,
) -> (Address, Address, MilestoneEscrowClient<'_>, Vec<Address>) {
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
    if fund {
        escrow.accept_job(&freelancer_addr);
        escrow.accept_arbiter(&arbiter_addr);
        escrow.fund(&client_addr);
    }

    let signers = vec![
        env,
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ];
    escrow.multisig_approval_init(&admin, &signers, &2u32);

    (contract_id, admin, escrow, signers)
}

/// A funded escrow with a 2-of-3 multisig regime registered.
fn funded_multisig_escrow(env: &Env) -> (Address, Vec<Address>, MilestoneEscrowClient<'_>) {
    let (contract_id, _, escrow, signers) = multisig_escrow(env, true);
    (contract_id, signers, escrow)
}

/// An escrow with a 2-of-3 multisig regime registered but **no** token balance,
/// which multisig approvals accept since issue #787.
fn unfunded_multisig_escrow(env: &Env) -> (Address, Vec<Address>, MilestoneEscrowClient<'_>) {
    let (contract_id, _, escrow, signers) = multisig_escrow(env, false);
    (contract_id, signers, escrow)
}

/// The approval bitmap stored in `proposal_id`'s record (0 when no record
/// exists).
fn stored_bitmap(env: &Env, contract_id: &Address, proposal_id: u32) -> u32 {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get::<_, crate::Proposal>(&DataKey::Proposal(proposal_id))
            .map(|record| record.approvals_bitmap)
            .unwrap_or(0)
    })
}

/// Remaining TTL, in ledgers, of the proposal record that holds the bitmap.
fn bitmap_ttl(env: &Env, contract_id: &Address, proposal_id: u32) -> u32 {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Proposal(proposal_id))
    })
}

/// Number of keys this contract currently holds in persistent storage.
fn persistent_key_count(env: &Env, contract_id: &Address) -> u32 {
    env.as_contract(contract_id, || env.storage().persistent().all().len())
}

/// Number of keys this contract currently holds in temporary storage.
fn temporary_key_count(env: &Env, contract_id: &Address) -> u32 {
    env.as_contract(contract_id, || env.storage().temporary().all().len())
}

/// Number of keys this contract currently holds in instance storage.
fn instance_key_count(env: &Env, contract_id: &Address) -> u32 {
    env.as_contract(contract_id, || env.storage().instance().all().len())
}

/// Every `msigappr` event for `proposal_id` emitted by the most recent
/// invocation, deserialized.
fn approve_events(env: &Env, proposal_id: u32) -> Vec<MultiSigApprovedEvent> {
    let topic_val: Val = symbol_short!("msigappr").into_val(env);
    let mut found = Vec::new(env);
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                let payload = MultiSigApprovedEvent::from_val(env, &event.2);
                if payload.proposal_id == proposal_id {
                    found.push_back(payload);
                }
            }
        }
    }
    found
}

// ── the #457 reduction: distinct storage keys written by the call ────────────

/// The issue's literal validation requirement: the number of distinct storage
/// keys written by `multisig_approve` is reduced.
///
/// Measured per invocation with the test SDK's resource metering
/// (`env.cost_estimate().resources()`, the same capability used by
/// `admin_resume_escrow_footprint_tests`):
/// * a first approval writes the bitmap: 2 entry writes / 180 write bytes (the
///   temporary bitmap entry plus the auth nonce entry);
/// * a **duplicate** approval no longer writes any contract storage key: 2 → 1
///   entry writes, 180 → 72 write bytes, i.e. exactly the 108 bytes of the
///   bitmap entry this issue removes — the one write left is the auth nonce;
/// * the control below shows the same call still writes the bitmap when the
///   signer's bit really does change, so the metric distinguishes the two paths
///   rather than the write having been dropped unconditionally.
#[test]
fn test_multisig_approve_duplicate_writes_no_contract_storage_key() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let s0 = signers.get(0).unwrap();

    // Reference call: signer 0 approves proposal 1 for the first time.
    crate::tests::seed_admin_proposal(&env, &escrow.address, 1);
    crate::tests::seed_admin_proposal(&env, &escrow.address, 2);
    escrow.multisig_approve(&s0, &1u32);
    let first = env.cost_estimate().resources();
    let bitmap_after_first = stored_bitmap(&env, &contract_id, 1u32);
    assert_eq!(bitmap_after_first, 1u32, "signer 0 must set bit 0");

    // Duplicate approval of the same proposal by the same signer, ten ledgers
    // later so that any TTL bump would be visible.
    env.ledger().with_mut(|li| li.sequence_number += 10);
    let ttl_before_duplicate = bitmap_ttl(&env, &contract_id, 1u32);
    let duplicate_state = escrow.multisig_approve(&s0, &1u32);
    let duplicate = env.cost_estimate().resources();
    let ttl_after_duplicate = bitmap_ttl(&env, &contract_id, 1u32);

    // Control: the identical call against a *fresh* proposal still writes the
    // bitmap, because there the bit does change.
    escrow.multisig_approve(&s0, &2u32);
    let fresh = env.cost_estimate().resources();

    assert_eq!(
        first.write_entries, 2,
        "a first approval writes the proposal record and the auth nonce entry; \
         measured {:?}",
        first
    );
    assert_eq!(
        duplicate.write_entries, 1,
        "a duplicate approval must write one entry fewer (the auth nonce only, \
         no contract storage key); measured {:?}",
        duplicate
    );
    assert_eq!(
        duplicate.write_bytes, 72,
        "a duplicate approval writes only the 72-byte auth nonce; measured {:?}",
        duplicate
    );
    assert!(
        first.write_bytes > duplicate.write_bytes,
        "the first approval pays for the proposal record write"
    );
    assert_eq!(
        fresh.write_entries, first.write_entries,
        "control: a call that changes the bit still writes the bitmap; measured \
         {:?}",
        fresh
    );
    assert_eq!(
        fresh.write_bytes, first.write_bytes,
        "control: a call that changes the bit still pays for the bitmap; \
         measured {:?}",
        fresh
    );

    // Nothing about the bitmap itself changed on the duplicate path: same
    // stored value, same remaining TTL, same returned state.
    assert_eq!(
        stored_bitmap(&env, &contract_id, 1u32),
        bitmap_after_first,
        "the duplicate approval must not alter the stored bitmap"
    );
    assert_eq!(
        ttl_after_duplicate, ttl_before_duplicate,
        "the duplicate approval must not extend the bitmap entry's TTL"
    );
    assert_eq!(
        duplicate_state,
        MultiSigApprovalState {
            approved: false,
            approvals: 1,
            threshold: 2,
            bitmap: 1,
        },
        "the duplicate approval must return the same state as the first call"
    );
}

// ── the key shape the call writes ────────────────────────────────────────────

/// Key-level shape: an approval updates its proposal's record in place.  It
/// creates no temporary key, no instance key and no new persistent key, and a
/// duplicate leaves the record as it was.
#[test]
fn test_multisig_approve_writes_one_bitmap_key_per_proposal() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let s0 = signers.get(0).unwrap();
    let s1 = signers.get(1).unwrap();
    crate::tests::seed_admin_proposal(&env, &escrow.address, 10);
    crate::tests::seed_admin_proposal(&env, &escrow.address, 11);

    let instance_keys_before = instance_key_count(&env, &contract_id);
    let persistent_keys_before = persistent_key_count(&env, &contract_id);

    let state = escrow.multisig_approve(&s0, &10u32);
    escrow.multisig_approve(&s1, &10u32);
    escrow.multisig_approve(&s0, &11u32);
    // A duplicate must not add a key either.
    let duplicate = escrow.multisig_approve(&s0, &11u32);

    assert_eq!(
        temporary_key_count(&env, &contract_id),
        0,
        "approvals no longer use temporary storage"
    );
    assert_eq!(
        persistent_key_count(&env, &contract_id),
        persistent_keys_before,
        "an approval updates the existing proposal record and adds no key"
    );
    assert_eq!(
        instance_key_count(&env, &contract_id),
        instance_keys_before,
        "an approval must not add any instance storage key"
    );
    assert_eq!(state.bitmap, 0b001);
    assert_eq!(duplicate.bitmap, 0b001);
    assert_eq!(stored_bitmap(&env, &contract_id, 10u32), 0b011);
    assert_eq!(stored_bitmap(&env, &contract_id, 11u32), 0b001);
}

// ── behaviour identity: the duplicate path ───────────────────────────────────

/// A duplicate approval must behave exactly as before the change — same returned
/// state, same stored bitmap, and the same `msigappr` event still emitted (this
/// change removes a redundant *write*, not an observable effect).
#[test]
fn test_multisig_approve_duplicate_is_idempotent_and_emits_the_same_event() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let s0 = signers.get(0).unwrap();
    crate::tests::seed_admin_proposal(&env, &escrow.address, 5);

    let first = escrow.multisig_approve(&s0, &5u32);
    let first_events = approve_events(&env, 5u32);
    assert_eq!(first_events.len(), 1, "the first approval emits msigappr");

    env.ledger().with_mut(|li| li.sequence_number += 10);
    let duplicate = escrow.multisig_approve(&s0, &5u32);
    let duplicate_events = approve_events(&env, 5u32);

    assert_eq!(
        duplicate, first,
        "the duplicate must return exactly the state of the first approval"
    );
    assert_eq!(duplicate.approvals, 1, "a duplicate still counts once");
    assert_eq!(stored_bitmap(&env, &contract_id, 5u32), 1u32);
    assert_eq!(
        duplicate_events.len(),
        1,
        "the duplicate approval still emits exactly one msigappr event"
    );
    assert_eq!(
        duplicate_events.get(0).unwrap(),
        first_events.get(0).unwrap(),
        "the duplicate approval's event payload is unchanged"
    );
}

// ── behaviour identity: which bit is set, and threshold detection ────────────

/// Which bit an approval sets is unchanged: the bit for the signer's index in
/// the registered signer vec, and only that bit.  Checked for every index in the
/// 3-signer set, each on its own proposal, plus a duplicate that must not move a
/// bit.
#[test]
fn test_multisig_approve_sets_the_bit_for_each_signer_index() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    crate::tests::seed_admin_proposal(&env, &escrow.address, 20);
    crate::tests::seed_admin_proposal(&env, &escrow.address, 21);
    crate::tests::seed_admin_proposal(&env, &escrow.address, 22);

    let state0 = escrow.multisig_approve(&signers.get(0).unwrap(), &20u32);
    assert_eq!(state0.bitmap, 1u32 << 0);
    assert_eq!(state0.approvals, 1);

    let state1 = escrow.multisig_approve(&signers.get(1).unwrap(), &21u32);
    assert_eq!(state1.bitmap, 1u32 << 1);
    assert_eq!(state1.approvals, 1);

    let state2 = escrow.multisig_approve(&signers.get(2).unwrap(), &22u32);
    assert_eq!(state2.bitmap, 1u32 << 2);
    assert_eq!(state2.approvals, 1);

    assert_eq!(stored_bitmap(&env, &contract_id, 20u32), 1u32 << 0);
    assert_eq!(stored_bitmap(&env, &contract_id, 21u32), 1u32 << 1);
    assert_eq!(stored_bitmap(&env, &contract_id, 22u32), 1u32 << 2);

    // A duplicate from signer 0 leaves proposal 20's single bit where it was.
    let duplicate = escrow.multisig_approve(&signers.get(0).unwrap(), &20u32);
    assert_eq!(duplicate.bitmap, 1u32 << 0);
    assert_eq!(duplicate.approvals, 1);
    assert_eq!(stored_bitmap(&env, &contract_id, 20u32), 1u32 << 0);
}

/// Threshold-reached detection is unchanged, including across the duplicate path
/// whose write this issue elides: approval below the threshold reads as not
/// approved, exactly two approvals in a 2-of-3 setup satisfy it, a duplicate
/// after that changes neither the bitmap nor the decision, and a third signer
/// pushes the count past the threshold.
#[test]
fn test_multisig_approve_threshold_detection_unchanged() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let s0 = signers.get(0).unwrap();
    let s1 = signers.get(1).unwrap();
    let s2 = signers.get(2).unwrap();
    crate::tests::seed_admin_proposal(&env, &escrow.address, 30);

    let below = escrow.multisig_approve(&s0, &30u32);
    assert!(!below.approved, "one of two approvals is not approved");
    assert_eq!(below.threshold, 2);
    assert_eq!(below.approvals, 1);
    assert!(!escrow.is_multisig_approved(&30u32).approved);

    let reached = escrow.multisig_approve(&s1, &30u32);
    assert!(
        reached.approved,
        "2-of-3 must be met by exactly two signers"
    );
    assert_eq!(reached.approvals, 2);
    assert_eq!(reached.bitmap, 0b011);
    assert!(escrow.is_multisig_approved(&30u32).approved);

    let duplicate = escrow.multisig_approve(&s1, &30u32);
    assert!(duplicate.approved);
    assert_eq!(duplicate.bitmap, reached.bitmap);
    assert_eq!(duplicate.approvals, 2);
    assert_eq!(stored_bitmap(&env, &contract_id, 30u32), 0b011);

    let over = escrow.multisig_approve(&s2, &30u32);
    assert!(
        over.approved,
        "a third approval still keeps the proposal approved"
    );
    assert_eq!(over.approvals, 3);
    assert_eq!(over.bitmap, 0b111);
    assert_eq!(stored_bitmap(&env, &contract_id, 30u32), 0b111);
    assert_eq!(escrow.is_multisig_approved(&30u32).approvals, 3);
}

// ── behaviour identity: the rejection paths write nothing ────────────────────

/// Every rejection path leaves the proposal record's bitmap untouched, an
/// approval for an ID with no record is refused, and an unfunded escrow now
/// accepts approvals (issue #787: recording intent moves no tokens).
#[test]
fn test_multisig_approve_rejections_write_no_bitmap() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let impostor = Address::generate(&env);
    crate::tests::seed_admin_proposal(&env, &escrow.address, 40);

    // Not a registered signer.
    assert_eq!(
        escrow.try_multisig_approve(&impostor, &40u32),
        Err(Ok(Error::Unauthorized))
    );

    // Registered signer, but no proposal record for the ID.
    assert_eq!(
        escrow.try_multisig_approve(&signers.get(0).unwrap(), &41u32),
        Err(Ok(Error::NoPendingAdminTransfer))
    );

    // Registered signer and funded escrow, but no signature at all.
    env.set_auths(&[]);
    assert!(escrow
        .try_multisig_approve(&signers.get(0).unwrap(), &40u32)
        .is_err());

    assert_eq!(
        stored_bitmap(&env, &contract_id, 40u32),
        0,
        "no rejection may set a bit"
    );
    assert_eq!(
        stored_bitmap(&env, &contract_id, 41u32),
        0,
        "no rejection may create a proposal record"
    );

    // Registered signer on an escrow with no token balance: accepted.
    let (empty_id, empty_signers, empty_escrow) = unfunded_multisig_escrow(&env);
    crate::tests::seed_admin_proposal(&env, &empty_escrow.address, 40);
    let state = empty_escrow.multisig_approve(&empty_signers.get(0).unwrap(), &40u32);
    assert_eq!(state.bitmap, 0b001);
    assert_eq!(stored_bitmap(&env, &empty_id, 40u32), 0b001);
}

// ── TTL persistence test ───────────────────────────────────────────────────────

/// Approvals stored in persistent storage survive TTL expiry for multi-day
/// governance workflows. This test verifies that advancing the ledger past
/// the default TTL does not cause approvals to disappear.
#[test]
fn test_multisig_approve_persists_across_ledger_advancement() {
    use soroban_sdk::testutils::Ledger as _;

    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let s0 = signers.get(0).unwrap();
    let s1 = signers.get(1).unwrap();

    crate::tests::seed_admin_proposal(&env, &escrow.address, 1);

    // First signer approves proposal 1
    let state1 = escrow.multisig_approve(&s0, &1u32);
    assert_eq!(state1.approvals, 1);
    assert_eq!(state1.bitmap, 0b001);
    assert!(!state1.approved);

    // Advance ledger significantly (simulating multiple days)
    env.ledger().with_mut(|li| li.sequence_number += 10000);

    // Second signer approves the same proposal - both approvals should count
    let state2 = escrow.multisig_approve(&s1, &1u32);
    assert_eq!(state2.approvals, 2);
    assert_eq!(state2.bitmap, 0b011);
    assert!(state2.approved);

    // The bitmap lives in the persistent proposal record (#785).
    assert_eq!(stored_bitmap(&env, &contract_id, 1u32), 0b011);
}
