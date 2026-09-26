#![cfg(test)]
//! Ledger-storage footprint suite for `multisig_approve` (issue #457).
//!
//! # What this call touches
//! `multisig_approve` reads the registered signer set and the approval
//! threshold (`MultiSigSigners` + `MultiSigThreshold`, both **instance** keys of
//! the contract's single `contract_instance` ledger entry, which the call needs
//! anyway for the `DataKey::Job` metadata holding the token address), the
//! contract's token balance, and the approval bitmap
//! `MultiSigApproval(proposal_id)` (**temporary**) — one key of key type `u32`
//! holding one `u32`, one bit per signer index.
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
//! The metadata consolidation that *does* exist — `MultiSigSigners` +
//! `MultiSigThreshold` into a single instance key — belongs to issue #456 and is
//! not duplicated here; the key-shape tests below pin the current layout so that
//! change is a deliberate, reviewed edit rather than a silent one.
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
use soroban_sdk::testutils::storage::{Instance as _, Temporary as _};
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
    );
    if fund {
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
/// for the `MultiSigEmptyBalance` guard.
fn unfunded_multisig_escrow(env: &Env) -> (Address, Vec<Address>, MilestoneEscrowClient<'_>) {
    let (contract_id, _, escrow, signers) = multisig_escrow(env, false);
    (contract_id, signers, escrow)
}

/// The stored approval bitmap for `proposal_id` (0 when no entry exists).
fn stored_bitmap(env: &Env, contract_id: &Address, proposal_id: u32) -> u32 {
    env.as_contract(contract_id, || {
        env.storage()
            .temporary()
            .get(&DataKey::MultiSigApproval(proposal_id))
            .unwrap_or(0)
    })
}

/// Remaining TTL, in ledgers, of the temporary bitmap entry.
fn bitmap_ttl(env: &Env, contract_id: &Address, proposal_id: u32) -> u32 {
    env.as_contract(contract_id, || {
        env.storage()
            .temporary()
            .get_ttl(&DataKey::MultiSigApproval(proposal_id))
    })
}

/// Number of keys this contract currently holds in temporary storage — for
/// `multisig_approve` that is exactly one key per proposal with a bitmap.
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
        "a first approval writes the bitmap entry and the auth nonce entry; \
         measured {:?}",
        first
    );
    assert_eq!(
        first.write_bytes, 180,
        "a first approval writes 108 bytes for the bitmap plus 72 for the auth \
         nonce; measured {:?}",
        first
    );
    assert_eq!(
        duplicate.write_entries, 1,
        "a duplicate approval must write one entry fewer (the auth nonce only, \
         no contract storage key); it wrote 2 entries / 180 bytes before this \
         change; measured {:?}",
        duplicate
    );
    assert_eq!(
        duplicate.write_bytes, 72,
        "a duplicate approval must write 108 fewer bytes (the bitmap entry it \
         used to re-write); measured {:?}",
        duplicate
    );
    assert_eq!(
        first.write_bytes - duplicate.write_bytes,
        108,
        "the removed redundant write is exactly the bitmap entry's 108 bytes"
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

/// Key-level shape: each approved proposal owns exactly one temporary key
/// (`MultiSigApproval(proposal_id)`), duplicates add none, and an approval never
/// writes an instance key — not the signer set, not the threshold, not the
/// bitmap.  This pins the layout the issue asks about (metadata and bitmap in
/// separate tiers) so that any future consolidation is a deliberate edit.
#[test]
fn test_multisig_approve_writes_one_bitmap_key_per_proposal() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let s0 = signers.get(0).unwrap();
    let s1 = signers.get(1).unwrap();

    let instance_keys_before = instance_key_count(&env, &contract_id);
    assert_eq!(
        temporary_key_count(&env, &contract_id),
        0,
        "precondition: no proposal bitmap exists yet"
    );

    let state = escrow.multisig_approve(&s0, &10u32);
    escrow.multisig_approve(&s1, &10u32);
    escrow.multisig_approve(&s0, &11u32);
    // A duplicate must not add a key either.
    let duplicate = escrow.multisig_approve(&s0, &11u32);

    assert_eq!(
        temporary_key_count(&env, &contract_id),
        2,
        "two approved proposals must own exactly two temporary storage keys"
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

    // Both temporary keys are proposal bitmaps under the u32-only
    // `MultiSigApproval` key, holding the u32 bitmap as value.
    env.as_contract(&contract_id, || {
        let all = env.storage().temporary().all();
        assert_eq!(all.len(), 2);
        assert!(
            all.contains_key(DataKey::MultiSigApproval(10u32).into_val(&env)),
            "proposal 10's bitmap is stored under its own key"
        );
        assert!(
            all.contains_key(DataKey::MultiSigApproval(11u32).into_val(&env)),
            "proposal 11's bitmap is stored under its own key"
        );
    });
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

/// Every rejection path still creates no bitmap key — the guard order (auth and
/// source-state checks before any storage write) is untouched by the elided
/// write, so a rejected call leaves no storage behind.
#[test]
fn test_multisig_approve_rejections_write_no_bitmap() {
    let env = Env::default();
    let (contract_id, signers, escrow) = funded_multisig_escrow(&env);
    let impostor = Address::generate(&env);

    // Not a registered signer.
    assert_eq!(
        escrow.try_multisig_approve(&impostor, &40u32),
        Err(Ok(Error::Unauthorized))
    );

    // Registered signer, but the escrow holds no token balance.
    let (empty_id, empty_signers, empty_escrow) = unfunded_multisig_escrow(&env);
    assert_eq!(
        empty_escrow.try_multisig_approve(&empty_signers.get(0).unwrap(), &40u32),
        Err(Ok(Error::MultiSigEmptyBalance))
    );

    // Registered signer and funded escrow, but no signature at all.
    env.set_auths(&[]);
    assert!(escrow
        .try_multisig_approve(&signers.get(0).unwrap(), &40u32)
        .is_err());

    assert_eq!(
        temporary_key_count(&env, &contract_id),
        0,
        "no rejection may create the bitmap key"
    );
    assert_eq!(
        temporary_key_count(&env, &empty_id),
        0,
        "the balance guard must reject before the bitmap is touched"
    );
}
