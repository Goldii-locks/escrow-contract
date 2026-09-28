#![cfg(test)]
//! Checked-arithmetic suite for `multisig_approve` (issue #583).
//!
//! `multisig_approve` turns a signer's position in `MultiSigSigners` into a bit
//! in the proposal's `u32` approval bitmap. The audit found that step performed
//! with bare arithmetic (`1u32 << idx`, `bitmap |= mask`), which cannot report
//! an unrepresentable result: the release profile used for deployment enables
//! `overflow-checks` together with `panic = "abort"`, so a trapping operator
//! aborts the whole transaction and the caller gets no typed error to act on.
//!
//! The production path now performs every step with a checked operation
//! (`usize -> u32` `try_into`, `checked_shl`, `checked_add`), each mapping its
//! failure to `Error::ArithmeticOverflow`, and does so **before** the bitmap is
//! written so a rejected call leaves no partial state. These tests pin the
//! boundaries of that arithmetic:
//!
//! * `idx >= 32` — a signer set longer than `MAX_MULTISIG_SIGNERS`, reachable
//!   only through corrupt or upgraded storage, because
//!   `multisig_approval_init` rejects it with `MultiSigTooManySigners` — must
//!   return `ArithmeticOverflow` instead of trapping or wrapping onto bit 0
//!   (which would hand signer 33 the approval of signer 1), and must neither
//!   create the temporary bitmap entry nor publish an event;
//! * `idx == 31`, the largest index that fits, must set exactly the top bit
//!   (`0x8000_0000`), i.e. the shift is not silently truncated as `1 << 32`
//!   would be in C-like semantics;
//! * 32 approvals must land on exactly `u32::MAX`, the saturation point of the
//!   accumulator, and a repeat approval at saturation must stay a successful
//!   no-op rather than evaluating `u32::MAX + mask`.
//!
//! Every test here builds 32- or 33-entry signer sets, so they run without
//! ledger snapshots — the same choice the existing 33-signer
//! `test_multisig_approval_init_too_many_signers_fails` makes. What those
//! snapshots would have shown is asserted directly instead: the public view
//! (`is_multisig_approved`), the raw temporary entry, and the `msigappr` event
//! count.

use super::*;
use soroban_sdk::testutils::storage::Temporary as _;
use soroban_sdk::testutils::{Address as _, EnvTestConfig};
use soroban_sdk::{symbol_short, vec, Address, Env, IntoVal, Map, Val};

/// Signer-set size the initialiser accepts (`MAX_MULTISIG_SIGNERS`); one more
/// is needed to put a signer past the end of a `u32` bitmap.
const MAX_SIGNERS: u32 = 32;

/// Bit 31 — the only bit `idx == 31` may set.
const TOP_BIT: u32 = 0x8000_0000;

fn env_without_snapshot() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// A funded escrow with `signer_count` registered signers and `threshold`
/// approvals required. `multisig_approve` refuses to run against a zero token
/// balance, so the escrow is funded before the multisig regime is installed.
fn funded_multisig_escrow(
    env: &Env,
    signer_count: u32,
    threshold: u32,
) -> (Address, MilestoneEscrowClient<'_>, Vec<Address>) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    escrow.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800u64,
        &vec![env, 1_000_i128],
    );

    token_admin.mint(&client_addr, &1_000_i128);
    escrow.fund(&client_addr);

    let mut signers = Vec::new(env);
    for _ in 0..signer_count {
        signers.push_back(Address::generate(env));
    }
    escrow.multisig_approval_init(&admin, &signers, &threshold);

    (contract_id, escrow, signers)
}

/// Overwrite the stored signer set, bypassing `multisig_approval_init`'s
/// `MultiSigTooManySigners` cap, to model the corrupt/upgraded storage state in
/// which a signer's index does not fit the bitmap.
fn overwrite_signers(env: &Env, contract_id: &Address, signers: &Vec<Address>) {
    env.as_contract(contract_id, || {
        let storage = env.storage().instance();
        let MultiSigConfig(_, threshold) = storage
            .get(&DataKey::MultiSigConfig)
            .expect("multisig_approval_init wrote the config");
        storage.set(
            &DataKey::MultiSigConfig,
            &MultiSigConfig(signers.clone(), threshold),
        );
    });
}

/// The bitmap as stored for `proposal_id`, or `None` when no temporary entry
/// exists yet.
fn stored_bitmap(env: &Env, contract_id: &Address, proposal_id: u32) -> Option<u32> {
    env.as_contract(contract_id, || {
        env.storage()
            .temporary()
            .get::<_, u32>(&DataKey::MultiSigApproval(proposal_id))
    })
}

/// Every temporary-storage entry the contract owns, for "nothing was written"
/// comparisons.
fn temporary_entries(env: &Env, contract_id: &Address) -> Map<Val, Val> {
    env.as_contract(contract_id, || env.storage().temporary().all())
}

/// Number of `msigappr` events in the **most recent** invocation. The SDK's
/// event log is scoped to a single invocation, so this has to be read before the
/// next client call and before any `env.as_contract` helper (which is itself an
/// invocation, and would hide what is being measured).
fn msigappr_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("msigappr").into_val(env);
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

/// Grow the stored signer set past the bitmap width, keeping the first
/// `MAX_SIGNERS` entries so earlier approvals stay valid.
fn oversized_signers(env: &Env, mut signers: Vec<Address>) -> Vec<Address> {
    while signers.len() <= MAX_SIGNERS {
        signers.push_back(Address::generate(env));
    }
    signers
}

/// Approve once, then immediately record how many `msigappr` events that single
/// call published — see `msigappr_event_count` for why the count cannot be
/// deferred until after the storage assertions.
fn approve_and_count_events(
    env: &Env,
    escrow: &MilestoneEscrowClient<'_>,
    signer: &Address,
    proposal_id: u32,
) -> (MultiSigApprovalState, u32) {
    let state = escrow.multisig_approve(signer, &proposal_id);
    (state, msigappr_event_count(env))
}

// ── idx out of range: idx == 32 needs 33 signers in storage ──────────────────

/// The audited hazard: an index equal to the bitmap width must be answered with
/// `ArithmeticOverflow`. A bare `1u32 << 32` traps on the release profile's
/// overflow checks (aborting the transaction, with no typed error for the
/// caller); on a wrapping platform it would collide with bit 0 instead.
#[test]
fn test_multisig_approve_index_at_bitmap_width_returns_arithmetic_overflow() {
    let env = env_without_snapshot();
    let (contract_id, escrow, signers) = funded_multisig_escrow(&env, 3, 2);

    // 33 signers cannot be installed through the public initialiser, so model
    // the state directly.
    let oversized = oversized_signers(&env, signers);
    overwrite_signers(&env, &contract_id, &oversized);

    let out_of_range = escrow.try_multisig_approve(&oversized.get(MAX_SIGNERS).unwrap(), &1u32);

    assert_eq!(out_of_range, Err(Ok(Error::ArithmeticOverflow)));
}

/// The rejected call must leave the ledger exactly as it found it: no temporary
/// bitmap entry, no event, and the public view still reporting zero approvals.
#[test]
fn test_multisig_approve_out_of_range_index_writes_nothing() {
    let env = env_without_snapshot();
    let (contract_id, escrow, signers) = funded_multisig_escrow(&env, 3, 2);

    let oversized = oversized_signers(&env, signers);
    overwrite_signers(&env, &contract_id, &oversized);

    let before = temporary_entries(&env, &contract_id);

    let out_of_range = escrow.try_multisig_approve(&oversized.get(MAX_SIGNERS).unwrap(), &1u32);
    assert_eq!(out_of_range, Err(Ok(Error::ArithmeticOverflow)));
    // Read the event log before the `env.as_contract` helpers below hide it:
    // the rejected call was silent.
    assert_eq!(msigappr_event_count(&env), 0);

    assert_eq!(stored_bitmap(&env, &contract_id, 1u32), None);
    assert_eq!(temporary_entries(&env, &contract_id), before);

    let state = escrow.is_multisig_approved(&1u32);
    assert!(!state.approved);
    assert_eq!(state.approvals, 0);
    assert_eq!(state.bitmap, 0);
}

/// A rejected out-of-range approval must not disturb approvals already recorded
/// for the same proposal.
#[test]
fn test_multisig_approve_out_of_range_index_keeps_existing_bitmap() {
    let env = env_without_snapshot();
    let (contract_id, escrow, signers) = funded_multisig_escrow(&env, 3, 2);

    // A genuine approval from signer 0, setting bit 0 and announcing itself.
    let (recorded, announced) =
        approve_and_count_events(&env, &escrow, &signers.get(0).unwrap(), 9u32);
    assert_eq!(recorded.bitmap, 1);
    assert_eq!(recorded.approvals, 1);
    assert!(!recorded.approved);
    assert_eq!(announced, 1);

    // Keep signer 0 at index 0 and grow the set so that index 32 exists.
    let oversized = oversized_signers(&env, signers);
    overwrite_signers(&env, &contract_id, &oversized);

    let before = temporary_entries(&env, &contract_id);
    let out_of_range = escrow.try_multisig_approve(&oversized.get(MAX_SIGNERS).unwrap(), &9u32);
    assert_eq!(out_of_range, Err(Ok(Error::ArithmeticOverflow)));
    assert_eq!(msigappr_event_count(&env), 0);

    assert_eq!(stored_bitmap(&env, &contract_id, 9u32), Some(1u32));
    assert_eq!(temporary_entries(&env, &contract_id), before);

    let state = escrow.is_multisig_approved(&9u32);
    assert_eq!(state.bitmap, 1);
    assert_eq!(state.approvals, 1);
}

// ── the top bit: idx == 31 is the last index that fits ───────────────────────

/// Index 31 is the highest position the `u32` bitmap can represent: it must set
/// exactly bit 31 — not wrap to bit 0, and not drop the approval. This is the
/// `checked_shl` boundary, since `checked_shl(32)` is the first `None`.
#[test]
fn test_multisig_approve_last_index_sets_the_top_bit() {
    let env = env_without_snapshot();
    let (contract_id, escrow, signers) = funded_multisig_escrow(&env, MAX_SIGNERS, MAX_SIGNERS);

    let last = signers.get(MAX_SIGNERS - 1).unwrap();
    let (state, announced) = approve_and_count_events(&env, &escrow, &last, 3u32);

    assert_eq!(state.bitmap, TOP_BIT);
    assert_eq!(state.bitmap.count_ones(), 1);
    assert_eq!(state.approvals, 1);
    assert_eq!(state.threshold, MAX_SIGNERS);
    assert!(!state.approved);
    assert_eq!(announced, 1);
    assert_eq!(stored_bitmap(&env, &contract_id, 3u32), Some(TOP_BIT));
}

/// All 32 signers approving must land on exactly `u32::MAX`: the accumulator
/// stops one bit short of overflowing on the final (bit-31) approval, and the
/// threshold is reported as reached.
#[test]
fn test_multisig_approve_all_signers_saturate_bitmap_without_overflow() {
    let env = env_without_snapshot();
    let (contract_id, escrow, signers) = funded_multisig_escrow(&env, MAX_SIGNERS, MAX_SIGNERS);

    let (mut state, mut announced) =
        approve_and_count_events(&env, &escrow, &signers.get(0).unwrap(), 4u32);
    assert_eq!(state.bitmap, 1);
    assert_eq!(announced, 1);
    for index in 1..MAX_SIGNERS {
        let (next, events) =
            approve_and_count_events(&env, &escrow, &signers.get(index).unwrap(), 4u32);
        state = next;
        announced += events;
    }

    assert_eq!(state.bitmap, u32::MAX);
    assert_eq!(state.approvals, MAX_SIGNERS);
    assert_eq!(state.threshold, MAX_SIGNERS);
    assert!(state.approved);
    // One announcement per accepted approval: because the event log only ever
    // holds one invocation, the per-call counts are accumulated instead.
    assert_eq!(announced, MAX_SIGNERS);
    assert_eq!(stored_bitmap(&env, &contract_id, 4u32), Some(u32::MAX));
}

/// A repeat approval at saturation is a documented no-op, and it is also what
/// keeps the accumulator sound: the bit is already set, so the addition is
/// skipped and `u32::MAX + mask` — which would overflow, and abort the
/// transaction on the release profile — is never evaluated.
#[test]
fn test_multisig_approve_duplicate_at_saturated_bitmap_is_a_no_op() {
    let env = env_without_snapshot();
    let (contract_id, escrow, signers) = funded_multisig_escrow(&env, MAX_SIGNERS, MAX_SIGNERS);

    let mut announced = 0u32;
    for index in 0..MAX_SIGNERS {
        let (_, events) =
            approve_and_count_events(&env, &escrow, &signers.get(index).unwrap(), 5u32);
        announced += events;
    }
    assert_eq!(announced, MAX_SIGNERS);
    assert_eq!(stored_bitmap(&env, &contract_id, 5u32), Some(u32::MAX));

    // The signer holding the top bit re-approves, as does the one holding bit 0.
    let (top_bit, top_bit_announced) =
        approve_and_count_events(&env, &escrow, &signers.get(MAX_SIGNERS - 1).unwrap(), 5u32);
    let (bit_zero, bit_zero_announced) =
        approve_and_count_events(&env, &escrow, &signers.get(0).unwrap(), 5u32);

    // Both no-ops are still successful calls, and both are still announced.
    assert_eq!(top_bit_announced, 1);
    assert_eq!(bit_zero_announced, 1);
    for state in [top_bit, bit_zero] {
        assert_eq!(state.bitmap, u32::MAX);
        assert_eq!(state.approvals, MAX_SIGNERS);
        assert_eq!(state.threshold, MAX_SIGNERS);
        assert!(state.approved);
    }
    assert_eq!(stored_bitmap(&env, &contract_id, 5u32), Some(u32::MAX));
}
