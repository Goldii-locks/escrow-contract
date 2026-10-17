#![cfg(test)]
//! Regression suite: every `i128` operation reachable from
//! `calculate_platform_fee_split` is overflow-safe.
//!
//! # What was changed
//!
//! The largest-remainder split behind `calculate_platform_fee_split` lives in
//! the private helper `MilestoneEscrow::allocate_platform_fee`. Two problems
//! were addressed there:
//!
//! 1. **Unchecked `i128` operators.** The largest-remainder loop ended with
//!    `remaining -= 1`, a bare `SubAssign` that would wrap (or, under the
//!    workspace's `overflow-checks = true` / `panic = "abort"` release
//!    profile, abort the entire transaction) if the loop invariant were ever
//!    broken. It is now `remaining.checked_sub(1)`. The division and remainder
//!    moved to `checked_div` / `checked_rem` for the same reason: a zero
//!    divisor traps on `/` and `%`, whereas the checked forms return `None`.
//!    Every step in the helper is now a `checked_*` counterpart, and a final
//!    conservation check asserts the three shares sum back to `total_amount`
//!    before anything is returned.
//! 2. **A non-specific failure mode.** Overflow used to surface as
//!    `Error::InvalidAmount`, which is indistinguishable from a caller simply
//!    passing a negative total. A dedicated `Error::ArithmeticOverflow` (code
//!    36) now names the cause, so an indexer can tell "your input was out of
//!    range" apart from "this amount cannot be split at all".
//!
//! `i128::MIN` is special-cased: it is the one input whose magnitude has no
//! `i128` counterpart, and because the helper distributes the *whole* total and
//! asserts the three shares sum back to it, an `i128::MIN` total has no valid
//! decomposition. It therefore reports `ArithmeticOverflow` rather than
//! `InvalidAmount`. Every other negative total keeps reporting
//! `InvalidAmount`, so the pre-existing `-1` contract is preserved.
//!
//! # Validation strategy
//!
//! Two independent checks are applied to every rejected input:
//!
//! * **Typed error, not a panic.** `try_calculate_platform_fee_split` is
//!   asserted to return `Err(Ok(Error::ArithmeticOverflow))`. In the Soroban
//!   test harness a genuine host-side trap (overflow / divide-by-zero) aborts
//!   the invocation instead of producing an `Err`, so a passing assertion is
//!   direct evidence that the arithmetic was checked rather than panicking.
//! * **No partial write survives the failure.** `Env::to_ledger_snapshot` is
//!   taken immediately before and after each rejected call and the two
//!   snapshots are compared. `LedgerSnapshot` captures every ledger entry
//!   (instance, persistent and temporary storage for all registered contracts,
//!   including their live-until sequences) plus ledger info, so a leaked
//!   storage write or a TTL bump fails the assertion. The event channel is
//!   covered separately: `Env::events().all()` reports the events of the *last*
//!   invocation only, so the failure-path check is the host's own guarantee
//!   that a failed invocation publishes nothing, while the success path
//!   asserts exactly one `pf_split` event with the exact payload.
//!
//! # Test matrix
//!
//! | # | Scenario | Assertion |
//! |---|----------|-----------|
//! | 1 | `i128::MAX` on the default allocation | `ArithmeticOverflow`, ledger unchanged |
//! | 2 | `i128::MAX` on a 20/70/10 allocation | `ArithmeticOverflow`, ledger unchanged |
//! | 3 | `i128::MAX` on a locked 0/100/0 allocation | `ArithmeticOverflow`, ledger unchanged |
//! | 4 | `i128::MIN` (magnitude unrepresentable) | `ArithmeticOverflow`, ledger unchanged |
//! | 5 | `i128::MIN + 1` (largest representable negative) | `InvalidAmount`, ledger unchanged |
//! | 6 | `-1` keeps its pre-existing contract | `InvalidAmount` (regression guard) |
//! | 7 | Repeat the `i128::MAX` rejection 5x | identical error and snapshot each time |
//! | 8 | Overflow attempt does not poison later valid calls | error, then the exact expected `Ok` split |
//! | 9 | Largest total a 9_000 bps weight can multiply | `Ok`, shares within 1 of ideal, sum == total |
//! | 10 | One step past that boundary (7_000 bps weight) | `ArithmeticOverflow`, boundary pinned from above |
//! | 11 | Failed call leaves allocation + token balance intact | snapshot equality, both reads unchanged |
//! | 12 | Failed call emits no `pf_split` event | count 0 on failure, exactly 1 on success |
//! | 13 | Conservation + non-negativity across boundary totals | shares always sum, never negative |
//! | 14 | Uninitialised contract still reports `NotInitialized` first | `Err(NotInitialized)` |
//! | 15 | Ordinary amounts are unaffected by the refactor | exact expected shares |
//!
//! # Basis-point caps
//!
//! `set_platform_fee_allocation` rejects `treasury_bps > MAX_TREASURY_FEE_BPS`
//! (2_000) and `client_bps > MAX_CLIENT_FEE_BPS` (5_000) with
//! `Error::FeeTooHigh`, so every allocation used below keeps treasury at or
//! below 2_000 and client at or below 5_000. The largest weight in each
//! allocation is what bounds the arithmetic headroom, and the overflow
//! boundary tests derive their totals from that weight.

use super::*;
use soroban_sdk::{symbol_short, token, vec, Address, Env, IntoVal, Symbol, Val};

/// Topic emitted by `calculate_platform_fee_split` on success.
const PF_SPLIT: Symbol = symbol_short!("pf_split");

/// A fully initialised escrow with an explicit platform-fee allocation.
struct Fixture<'a> {
    client: MilestoneEscrowClient<'a>,
    contract_id: Address,
    token_id: Address,
    admin: Address,
}

/// Count `pf_split` events published by the **last** contract invocation.
///
/// `Env::events().all()` is documented to report the last invocation only, and
/// to report nothing at all when that invocation failed. The success paths
/// therefore assert exactly one event, while the failure paths rely on the
/// ledger snapshot comparison in
/// [`assert_rejected_without_side_effects`] for the atomicity guarantee.
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

/// Build an initialised, funded escrow carrying the given platform-fee
/// allocation. The three weights must sum to `BPS_SCALE` (10_000), which every
/// production write path enforces.
fn fixture(env: &Env, client_bps: u32, freelancer_bps: u32, treasury_bps: u32) -> Fixture<'_> {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

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
    escrow.set_platform_fee_allocation(&admin, &client_bps, &freelancer_bps, &treasury_bps);

    Fixture {
        client: escrow,
        contract_id,
        token_id,
        admin,
    }
}

/// Assert that `total` is rejected with `expected` and that nothing on the
/// ledger moved while the contract produced that error.
#[track_caller]
fn assert_rejected_without_side_effects(
    env: &Env,
    escrow: &MilestoneEscrowClient<'_>,
    total: i128,
    expected: Error,
) {
    let before = env.to_ledger_snapshot();

    let result = escrow.try_calculate_platform_fee_split(&total);

    let after = env.to_ledger_snapshot();

    assert_eq!(
        result,
        Err(Ok(expected)),
        "total_amount = {total} must return the typed error {expected:?}, not panic or wrap"
    );
    assert_eq!(
        before, after,
        "a rejected calculate_platform_fee_split (total_amount = {total}) must not leave \
         a partial write behind: storage, TTLs and ledger info are identical"
    );
    assert_eq!(
        pf_split_event_count(env),
        0,
        "a failed invocation must not publish a pf_split event"
    );
}

// ── 1 ─ i128::MAX on the default allocation ──────────────────────────────────

/// `i128::MAX` against the default `{0, 10_000, 0}` allocation overflows the
/// `total_amount * freelancer_bps` product and must return the typed overflow
/// error with the ledger untouched.
#[test]
fn i128_max_on_default_allocation_is_typed_overflow() {
    let env = Env::default();
    let f = fixture(&env, 0, 10_000, 0);

    assert_rejected_without_side_effects(&env, &f.client, i128::MAX, Error::ArithmeticOverflow);
}

// ── 2 ─ i128::MAX on a 20/70/10 allocation ───────────────────────────────────

/// The same rejection must hold when every party carries a non-zero weight, so
/// the overflow cannot be attributed to a single degenerate ratio.
#[test]
fn i128_max_on_three_way_allocation_is_typed_overflow() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    assert_rejected_without_side_effects(&env, &f.client, i128::MAX, Error::ArithmeticOverflow);
}

// ── 3 ─ i128::MAX on a locked allocation ─────────────────────────────────────

/// Locking the allocation must not change the overflow behaviour: a read-only
/// calculator still has to refuse an unsplittable amount.
#[test]
fn i128_max_on_locked_allocation_is_typed_overflow() {
    let env = Env::default();
    let f = fixture(&env, 0, 10_000, 0);
    f.client.lock_platform_fee_allocation(&f.admin);

    assert!(f.client.get_platform_fee_allocation().locked);
    assert_rejected_without_side_effects(&env, &f.client, i128::MAX, Error::ArithmeticOverflow);
}

// ── 4 ─ i128::MIN ─────────────────────────────────────────────────────────────

/// `i128::MIN` is the one total whose magnitude is not representable, so it is
/// reported as an arithmetic overflow rather than a range error.
#[test]
fn i128_min_is_typed_overflow() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    assert_rejected_without_side_effects(&env, &f.client, i128::MIN, Error::ArithmeticOverflow);
}

// ── 5 ─ largest representable negative total ─────────────────────────────────

/// `i128::MIN + 1` has a representable magnitude, so it is a plain negative
/// total and keeps the `InvalidAmount` contract.
#[test]
fn largest_representable_negative_total_is_invalid_amount() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    assert_rejected_without_side_effects(&env, &f.client, i128::MIN + 1, Error::InvalidAmount);
}

// ── 6 ─ -1 keeps its pre-existing contract ───────────────────────────────────

/// Guards the pre-existing assertion in `test.rs`: only `i128::MIN` was
/// reclassified, `-1` is still `InvalidAmount`.
#[test]
fn negative_one_keeps_invalid_amount_contract() {
    let env = Env::default();
    let f = fixture(&env, 4_000, 4_000, 2_000);

    assert_rejected_without_side_effects(&env, &f.client, -1, Error::InvalidAmount);
}

// ── 7 ─ repeated rejections are stable ───────────────────────────────────────

/// The checked paths must be deterministic: five identical `i128::MAX` calls
/// each return the same typed error and each leave the ledger byte-for-byte
/// identical. This also proves no accumulator is carried across invocations.
#[test]
fn repeated_overflow_attempts_are_deterministic() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    let baseline = env.to_ledger_snapshot();
    for attempt in 1..=5_u32 {
        assert_eq!(
            f.client.try_calculate_platform_fee_split(&i128::MAX),
            Err(Ok(Error::ArithmeticOverflow)),
            "attempt {attempt} diverged from the typed overflow error"
        );
        assert_eq!(
            baseline,
            env.to_ledger_snapshot(),
            "attempt {attempt} mutated the ledger before failing"
        );
    }
    assert_eq!(pf_split_event_count(&env), 0);
}

// ── 8 ─ a failed call does not poison the next one ───────────────────────────

/// A rejected overflow must not leave internal residue behind: the very next
/// call with a representable amount has to produce the exact expected split.
#[test]
fn failed_call_does_not_poison_subsequent_call() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    assert_eq!(
        f.client.try_calculate_platform_fee_split(&i128::MAX),
        Err(Ok(Error::ArithmeticOverflow))
    );
    assert_eq!(
        f.client.try_calculate_platform_fee_split(&i128::MIN),
        Err(Ok(Error::ArithmeticOverflow))
    );

    let split = f.client.calculate_platform_fee_split(&1_000_i128);
    assert_eq!(split.client_amount, 200);
    assert_eq!(split.freelancer_amount, 700);
    assert_eq!(split.treasury_amount, 100);
    assert_eq!(pf_split_event_count(&env), 1);
}

/// Assert that `actual` is the floored share `floor` or the floored share plus
/// the single largest-remainder unit.
#[track_caller]
fn assert_within_one_of_floor(actual: i128, floor: i128) {
    assert!(
        (floor..=floor + 1).contains(&actual),
        "share {actual} is neither the floored value {floor} nor one unit above it"
    );
}

// ── 9 ─ the boundary just below the overflow, 9_000 bps weight ───────────────

/// The largest total a 9_000 bps weight can multiply without exceeding
/// `i128::MAX` is `i128::MAX / 9_000`. That exact value must be accepted, and
/// each share must land on its floor or one unit above it (largest-remainder),
/// proving the checked paths are not simply rejecting everything large.
#[test]
fn largest_representable_total_for_wide_weight_still_succeeds() {
    let env = Env::default();
    let f = fixture(&env, 0, 9_000, 1_000);

    let total = i128::MAX / 9_000;
    let split = f.client.calculate_platform_fee_split(&total);

    assert_within_one_of_floor(split.client_amount, 0);
    assert_within_one_of_floor(split.freelancer_amount, total * 9_000 / 10_000);
    assert_within_one_of_floor(split.treasury_amount, total * 1_000 / 10_000);
    assert_eq!(
        split.client_amount + split.freelancer_amount + split.treasury_amount,
        total,
        "the largest still-representable total must round-trip exactly"
    );
}

// ── 10 ─ one step past the boundary, 7_000 bps weight ────────────────────────

/// The same boundary check from the other side, for a 7_000 bps weight:
/// `i128::MAX / 7_000` is accepted and `i128::MAX / 7_000 + 1` — the smallest
/// total whose weighted product no longer fits — is rejected. Together with
/// test 9 this pins the exact edge of the checked guard.
#[test]
fn overflow_boundary_is_exact_for_narrow_weight() {
    let env = Env::default();
    let f = fixture(&env, 1_000, 7_000, 2_000);

    let largest_safe = i128::MAX / 7_000;
    let split = f.client.calculate_platform_fee_split(&largest_safe);
    assert_eq!(
        split.client_amount + split.freelancer_amount + split.treasury_amount,
        largest_safe,
        "the largest total a 7_000 bps weight can multiply must round-trip exactly"
    );

    // One unit further and `largest_safe * 7_000` no longer fits in an `i128`.
    assert_rejected_without_side_effects(
        &env,
        &f.client,
        largest_safe + 1,
        Error::ArithmeticOverflow,
    );
}

// ── 11 ─ nothing survives a failure, including escrow state ──────────────────

/// The full ledger snapshot check in [`assert_rejected_without_side_effects`]
/// already covers storage globally. This test makes the specific claim
/// explicit: the platform-fee allocation and the escrowed token balance remain
/// exactly as they were after the rejected call.
#[test]
fn failed_call_leaves_allocation_and_escrow_state_intact() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    let tokens = token::Client::new(&env, &f.token_id);
    let allocation_before = f.client.get_platform_fee_allocation();
    let balance_before = tokens.balance(&f.contract_id);

    assert_eq!(
        f.client.try_calculate_platform_fee_split(&i128::MAX),
        Err(Ok(Error::ArithmeticOverflow))
    );

    assert_eq!(f.client.get_platform_fee_allocation(), allocation_before);
    assert_eq!(
        tokens.balance(&f.contract_id),
        balance_before,
        "a rejected split must not touch the escrowed token balance"
    );
    assert_rejected_without_side_effects(&env, &f.client, i128::MIN, Error::ArithmeticOverflow);
}

// ── 12 ─ no audit event for a rejected calculation ───────────────────────────

/// The `pf_split` event is the indexer-facing record of a calculation. A failed
/// calculation must not appear there, otherwise an auditor could attribute a
/// split that never happened. This test rejects two values, then succeeds, and
/// asserts exactly one event — the successful one.
#[test]
fn rejected_calculation_publishes_no_audit_event() {
    let env = Env::default();
    let f = fixture(&env, 2_000, 7_000, 1_000);

    assert_eq!(pf_split_event_count(&env), 0);
    assert_eq!(
        f.client.try_calculate_platform_fee_split(&i128::MAX),
        Err(Ok(Error::ArithmeticOverflow))
    );
    assert_eq!(pf_split_event_count(&env), 0);

    assert_eq!(
        f.client.try_calculate_platform_fee_split(&i128::MIN),
        Err(Ok(Error::ArithmeticOverflow))
    );
    assert_eq!(pf_split_event_count(&env), 0);

    f.client.calculate_platform_fee_split(&10_000_i128);
    assert_eq!(
        pf_split_event_count(&env),
        1,
        "only the successful calculation may be audited"
    );
}

// ── 13 ─ conservation and non-negativity across boundary totals ─────────────

/// The helper ends with an explicit conservation assertion, so this test sweeps
/// the totals that stress the largest-remainder phase — around zero, around
/// the party count, and at the very top of the representable range — and
/// requires that every successful split is non-negative and reconstructs the
/// input exactly.
#[test]
fn conservation_holds_across_boundary_totals() {
    let env = Env::default();
    // Largest weight is 4_667 bps, so the largest total that still fits every
    // weighted product is `i128::MAX / 4_667`.
    let f = fixture(&env, 3_333, 4_667, 2_000);

    let totals = [
        0_i128,
        1,
        2,
        3,
        4,
        9_999,
        10_000,
        10_001,
        1_000_001,
        i128::MAX / 10_000,
        i128::MAX / 4_667,
    ];

    for total in totals {
        let split = f.client.calculate_platform_fee_split(&total);
        assert!(
            split.client_amount >= 0 && split.freelancer_amount >= 0 && split.treasury_amount >= 0,
            "total_amount = {total} produced a negative share: {split:?}"
        );
        assert_eq!(
            split.client_amount + split.freelancer_amount + split.treasury_amount,
            total,
            "total_amount = {total} did not round-trip"
        );
    }
}

// ── 14 ─ pre-initialisation is still checked first ───────────────────────────

/// The allocation read happens before any arithmetic, so an uninitialised
/// contract keeps reporting `NotInitialized` regardless of the amount. This
/// pins the error precedence so the new variant cannot shadow it.
#[test]
fn uninitialized_contract_reports_not_initialized_before_arithmetic() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        escrow.try_calculate_platform_fee_split(&i128::MAX),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        escrow.try_calculate_platform_fee_split(&i128::MIN),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(pf_split_event_count(&env), 0);
}

// ── 15 ─ ordinary amounts are untouched by the refactor ──────────────────────

/// Regression sweep over the small amounts a real caller would use, including
/// the largest-remainder edge cases (totals smaller than the number of
/// parties) and the zero total.
#[test]
fn ordinary_amounts_still_split_exactly() {
    let env = Env::default();
    let f = fixture(&env, 4_000, 4_000, 2_000);

    // 40 / 40 / 20 -- exact at 1_000.
    let split = f.client.calculate_platform_fee_split(&1_000_i128);
    assert_eq!(split.client_amount, 400);
    assert_eq!(split.freelancer_amount, 400);
    assert_eq!(split.treasury_amount, 200);

    // Zero total: all shares floor to zero, nothing to hand out.
    let zero = f.client.calculate_platform_fee_split(&0_i128);
    assert_eq!(zero.client_amount, 0);
    assert_eq!(zero.freelancer_amount, 0);
    assert_eq!(zero.treasury_amount, 0);

    // Total smaller than the party count: every share floors to zero and the
    // largest-remainder phase hands the single unit to the first party.
    let one = f.client.calculate_platform_fee_split(&1_i128);
    assert_eq!(one.client_amount, 1);
    assert_eq!(one.freelancer_amount, 0);
    assert_eq!(one.treasury_amount, 0);

    // Two units: the two largest remainders (client, freelancer) each win one.
    let two = f.client.calculate_platform_fee_split(&2_i128);
    assert_eq!(two.client_amount, 1);
    assert_eq!(two.freelancer_amount, 1);
    assert_eq!(two.treasury_amount, 0);

    // A total that is not a multiple of the scale still round-trips.
    let odd = f.client.calculate_platform_fee_split(&1_234_567_i128);
    assert_eq!(
        odd.client_amount + odd.freelancer_amount + odd.treasury_amount,
        1_234_567
    );

    // `Env::events().all()` reports the last invocation only, so the count is
    // checked per call: every successful calculation emits exactly one event.
    assert_eq!(pf_split_event_count(&env), 1);
}
