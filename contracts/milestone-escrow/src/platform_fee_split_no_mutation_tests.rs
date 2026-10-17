#![cfg(test)]
//! Regression suite for issue #541: guarantee that
//! `calculate_platform_fee_split` performs no state mutation.
//!
//! `calculate_platform_fee_split` is presented to callers as a pure
//! calculation: it reads the stored `PlatformFeeAllocation`, returns a
//! `PlatformFeeDistribution`, and is called from quote-style code paths (fee
//! previews, off-chain indexers, dispute calculators) that may run it
//! speculatively and repeatedly. It must never write to instance, persistent,
//! or temporary storage, and it must never extend a TTL: a caller that merely
//! *asks what the fee would be* must not be able to mutate the escrow.
//!
//! # Why the event does not contradict "no state mutation"
//!
//! On success the function publishes one diagnostic `pf_split` event so
//! downstream indexers can audit the split. Events are appended to the
//! transaction's diagnostic log; they are not ledger storage, they are not
//! readable by the contract, and they carry no TTL. So "publishes an event" and
//! "writes no state" are compatible, and the tests below assert both halves
//! explicitly: the ledger must be byte-identical, *and* exactly one
//! `pf_split` event must still be published. Asserting the event count is
//! deliberate — it stops a future edit from "fixing" a storage write by making
//! the function silently return, which would break every consumer of the audit
//! trail while still satisfying a naive no-write check.
//!
//! # The validation strategy
//!
//! Take a full ledger snapshot (`Env::to_ledger_snapshot`) immediately before
//! the call, take another immediately after, and assert the two are identical.
//! `LedgerSnapshot` captures every ledger entry (instance, persistent, and
//! temporary storage for all contracts registered on the `Env`, including
//! their live-until ledger sequence / TTL) plus the ledger info itself, so
//! this is a byte-for-byte check of the entire ledger, not just the keys we
//! expect to be read.
//!
//! Because the issue names all three durability tiers explicitly, the suite
//! also fingerprints each tier on its own (via
//! `env.as_contract(..)` + the testutils `all()` accessors) so a write that
//! somehow evaded the aggregate snapshot is still caught, and so a failure
//! message names the tier that moved.
//!
//! Any future edit that introduces a `.set(`, `.remove(`, `.extend_ttl(`, or a
//! second event publish into `calculate_platform_fee_split` — or into the
//! shared helpers it calls (`load_platform_fee_allocation`,
//! `allocate_platform_fee`) — will change the snapshot and fail this suite.

use soroban_sdk::testutils::storage::{Instance, Persistent, Temporary};
use soroban_sdk::{
    symbol_short, testutils::Address as _, token, vec, Address, Env, IntoVal, Map, Symbol, Val,
};

use crate::{Error, MilestoneEscrow, MilestoneEscrowClient, PlatformFeeDistribution};

/// Topic emitted by `calculate_platform_fee_split` on success.
const PF_SPLIT: Symbol = symbol_short!("pf_split");

/// A fully initialised escrow carrying an explicit platform-fee allocation.
///
/// The funders are retained so tests that need to advance the escrow's state
/// call `mark_delivered` / `approve_milestone` as the *right* party —
/// `fixture` deliberately takes weights only, not addresses, so every caller
/// builds its escrow the same way.
struct Fixture<'a> {
    client: MilestoneEscrowClient<'a>,
    contract_id: Address,
    admin: Address,
    client_addr: Address,
    freelancer_addr: Address,
}

/// Build an initialised, funded escrow carrying the given platform-fee
/// allocation. The three weights must sum to `BPS_SCALE` (10_000), and
/// `set_platform_fee_allocation` additionally enforces
/// `client_bps <= MAX_CLIENT_FEE_BPS` (5_000) and
/// `treasury_bps <= MAX_TREASURY_FEE_BPS` (2_000) — so a shape that violates
/// either cap never reaches the read path under test.
fn fixture(env: &Env, client_bps: u32, freelancer_bps: u32, treasury_bps: u32) -> Fixture<'_> {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    // Mint the exact milestone total to the client and fund the escrow, so
    // tests can advance state (delivery, approval) as a real escrow would.
    // Everything this writes happens *before* the snapshots each test takes,
    // so it never masks a mutation in the function under test.
    let milestones = vec![env, 1_000_i128];
    let fund_total: i128 = milestones.iter().sum();
    token::StellarAssetClient::new(env, &token_id).mint(&client_addr, &fund_total);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    escrow.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604_800u64,
        &milestones,
    );
    escrow.fund(&client_addr);
    escrow.set_platform_fee_allocation(&admin, &client_bps, &freelancer_bps, &treasury_bps);

    Fixture {
        client: escrow,
        contract_id,
        admin,
        client_addr,
        freelancer_addr,
    }
}

/// Count `pf_split` events published by the **last** contract invocation.
///
/// The test env only retains events from the most recent invocation, so this
/// reports exactly what the call under test produced.
fn pf_split_event_count(env: &Env) -> u32 {
    let topic: Val = PF_SPLIT.into_val(env);
    crate::all_event_tuples(env)
        .iter()
        .filter(|event| {
            event
                .1
                .get(0)
                .is_some_and(|t| t.get_payload() == topic.get_payload())
        })
        .count() as u32
}

/// A per-tier fingerprint of the escrow's storage: instance, persistent, then
/// temporary.
///
/// `all()` reads the tier of the *currently executing contract*, so the
/// fingerprint has to be taken inside `env.as_contract(&contract_id, ..)` or it
/// would describe the wrong contract.
type TierFingerprint = (Map<Val, Val>, Map<Val, Val>, Map<Val, Val>);

fn storage_fingerprint(env: &Env, contract_id: &Address) -> TierFingerprint {
    env.as_contract(contract_id, || {
        (
            env.storage().instance().all(),
            env.storage().persistent().all(),
            env.storage().temporary().all(),
        )
    })
}

// ── 1 ─ the core acceptance check ────────────────────────────────────────────

/// The headline requirement from the issue: a full ledger snapshot taken
/// before and after `calculate_platform_fee_split` is byte-identical.
#[test]
fn successful_call_leaves_full_ledger_byte_identical() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    let before = env.to_ledger_snapshot();
    let distribution = f.client.calculate_platform_fee_split(&10_000_i128);
    let after = env.to_ledger_snapshot();

    assert_eq!(
        distribution,
        PlatformFeeDistribution {
            client_amount: 2_000,
            freelancer_amount: 7_000,
            treasury_amount: 1_000,
        },
        "the split must still be computed correctly"
    );
    assert_eq!(
        before, after,
        "a successful calculate_platform_fee_split must leave the ledger \
         byte-for-byte identical: storage, TTLs and ledger info are unchanged"
    );
}

// ── 2 ─ the three durability tiers, checked individually ─────────────────────

/// The issue names instance, persistent and temporary storage separately, so
/// fingerprint each tier on its own and report which one moved.
#[test]
fn successful_call_writes_to_no_durability_tier() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    let before = storage_fingerprint(&env, &f.contract_id);
    f.client.calculate_platform_fee_split(&123_456_i128);
    let after = storage_fingerprint(&env, &f.contract_id);

    assert_eq!(
        before.0, after.0,
        "calculate_platform_fee_split wrote to instance storage"
    );
    assert_eq!(
        before.1, after.1,
        "calculate_platform_fee_split wrote to persistent storage"
    );
    assert_eq!(
        before.2, after.2,
        "calculate_platform_fee_split wrote to temporary storage"
    );
}

// ── 3 ─ the event must survive ───────────────────────────────────────────────

/// A storage write must never be "fixed" by deleting the audit trail: exactly
/// one `pf_split` event is still required on success.
#[test]
fn successful_call_still_publishes_exactly_one_pf_split_event() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    f.client.calculate_platform_fee_split(&10_000_i128);

    assert_eq!(
        pf_split_event_count(&env),
        1,
        "a successful call publishes exactly one pf_split event"
    );
}

// ── 4 ─ idempotence across repeated speculative calls ────────────────────────

/// Fee previews are called repeatedly and speculatively; a hundred calls must
/// leave the ledger exactly as one call did.
#[test]
fn repeated_successful_calls_are_idempotent_on_the_ledger() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    f.client.calculate_platform_fee_split(&1_000_i128);
    let before = env.to_ledger_snapshot();

    for total in 1..=100_i128 {
        f.client.calculate_platform_fee_split(&(total * 1_000_i128));
    }

    let after = env.to_ledger_snapshot();
    assert_eq!(
        before, after,
        "100 speculative fee previews must leave the ledger byte-for-byte identical"
    );
}

// ── 5 ─ every allocation shape ───────────────────────────────────────────────

/// The guarantee must not depend on which ratio is stored, including the
/// degenerate all-to-one-party shapes.
///
/// Every shape respects the caps `set_platform_fee_allocation` enforces
/// (`client <= 5_000`, `treasury <= 2_000`, `client + freelancer + treasury =
/// 10_000`), because a shape that the *setter* rejects never becomes the stored
/// allocation the read path then reads.
#[test]
fn ledger_unchanged_for_every_allocation_shape() {
    let shapes: [(u32, u32, u32); 6] = [
        (0, 10_000, 0),        // default: all to the freelancer
        (5_000, 5_000, 0),     // client cap hit, treasury unused
        (5_000, 3_000, 2_000), // both caps hit
        (2_000, 7_000, 1_000), // even three-way
        (1, 9_999, 0),         // tiny client weight, largest-remainder path
        (1, 7_999, 2_000),     // not divisible: largest remainders stay unsettled
    ];

    for (client_bps, freelancer_bps, treasury_bps) in shapes {
        let env = Env::default();
        let f = fixture(&env, client_bps, freelancer_bps, treasury_bps);

        let before = env.to_ledger_snapshot();
        let result = f.client.calculate_platform_fee_split(&999_999_i128);
        let after = env.to_ledger_snapshot();

        // The direct call panics on failure, so reaching here proves it
        // succeeded; assert the stronger property that the three shares still
        // reconstruct the total exactly, then confirm nothing moved.
        assert_eq!(
            result.client_amount + result.freelancer_amount + result.treasury_amount,
            999_999_i128,
            "allocation ({client_bps}, {freelancer_bps}, {treasury_bps}) must conserve the total"
        );
        assert_eq!(
            before, after,
            "allocation ({client_bps}, {freelancer_bps}, {treasury_bps}) must not mutate the ledger"
        );
    }
}

// ── 6 ─ a locked allocation is still only read ───────────────────────────────

/// Locking the allocation changes who may write it; it must not change that
/// the read path performs no write.
#[test]
fn ledger_unchanged_with_locked_allocation() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);
    f.client.lock_platform_fee_allocation(&f.admin);

    let before = env.to_ledger_snapshot();
    f.client.calculate_platform_fee_split(&10_000_i128);
    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "reading a locked allocation must not mutate the ledger"
    );
}

// ── 7 ─ purity survives contract state evolving ──────────────────────────────

/// The read path must stay pure once the escrow has moved on: after delivery
/// and approval, previewing the next fee must still write nothing.
#[test]
fn ledger_unchanged_after_milestone_progression() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);
    // Advance the escrow as the parties recorded on the job, so the read path
    // runs against genuinely evolved state rather than a freshly-initialized
    // contract.
    f.client.mark_delivered(&f.freelancer_addr, &0);
    f.client.approve_milestone(&f.client_addr, &0);

    let before = env.to_ledger_snapshot();
    f.client.calculate_platform_fee_split(&10_000_i128);
    let after = env.to_ledger_snapshot();

    assert_eq!(
        before, after,
        "the read path must stay pure after milestone progression"
    );
}

// ── 8 ─ boundary and degenerate amounts on the success path ──────────────────

/// Zero, one, and amounts too small to give every party a whole unit all take
/// the success path and must leave the ledger alone.
///
/// The allocation is deliberately lopsided (`client = 1`, i.e. 0.01 %) so these
/// tiny totals route through the largest-remainder branch where some party's
/// floor share is necessarily zero.
#[test]
fn ledger_unchanged_for_degenerate_success_amounts() {
    let env = Env::default();
    let f = fixture(&env, 1, 9_999, 0);

    for total in [0_i128, 1, 2, 3, 7, 1_234_567] {
        let before = env.to_ledger_snapshot();
        let result = f.client.try_calculate_platform_fee_split(&total);
        let after = env.to_ledger_snapshot();

        let distribution = result
            .expect("every total on this allocation is representable and must succeed")
            .expect("success must be a typed Ok, not a host error");

        assert_eq!(
            distribution.client_amount
                + distribution.freelancer_amount
                + distribution.treasury_amount,
            total,
            "total_amount = {total} must be conserved exactly"
        );
        assert!(
            distribution.client_amount >= 0
                && distribution.freelancer_amount >= 0
                && distribution.treasury_amount >= 0,
            "total_amount = {total} must never yield a negative share"
        );
        assert_eq!(
            before, after,
            "total_amount = {total} must not mutate the ledger"
        );
    }
}

// ── 9 ─ the stored allocation is never rewritten ─────────────────────────────

/// Belt and braces: read the allocation back after the call and confirm the
/// three weights and the lock flag are exactly what was stored.
#[test]
fn read_path_never_rewrites_the_stored_allocation() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);
    f.client.lock_platform_fee_allocation(&f.admin);

    let before = f.client.get_platform_fee_allocation();
    f.client.calculate_platform_fee_split(&10_000_i128);
    let after = f.client.get_platform_fee_allocation();

    assert_eq!(
        before, after,
        "calculate_platform_fee_split must not rewrite the stored allocation"
    );
    assert_eq!(after.client_bps, 2_000);
    assert_eq!(after.freelancer_bps, 7_000);
    assert_eq!(after.treasury_bps, 1_000);
    assert!(
        after.locked,
        "a locked allocation must still read back as locked"
    );
}

// ── 10 ─ the rejected path, cross-checked ────────────────────────────────────

/// The overflow suite already covers the rejected paths exhaustively. This is
/// the single cross-check that a rejected call is likewise a non-mutation, so
/// that the two suites cannot drift apart unnoticed.
#[test]
fn rejected_call_also_leaves_the_ledger_unchanged() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    let before = env.to_ledger_snapshot();
    let result = f.client.try_calculate_platform_fee_split(&i128::MAX);
    let after = env.to_ledger_snapshot();

    assert_eq!(
        result,
        Err(Ok(Error::ArithmeticOverflow)),
        "i128::MAX must be a typed overflow, not a panic"
    );
    assert_eq!(
        before, after,
        "a rejected calculate_platform_fee_split must not mutate the ledger"
    );
    assert_eq!(
        pf_split_event_count(&env),
        0,
        "a rejected call must not publish a pf_split event"
    );
}
