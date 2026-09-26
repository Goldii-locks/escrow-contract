#![cfg(test)]
//! Refund-allocation suite for `tax_withholding_split_refund` (issue #300).
//!
//! `tax_withholding_deductions` reduces a milestone's remaining gross to a
//! single `net_amount` and `admin_override_tax_release` hands that whole net
//! amount to the freelancer.  The pathway this suite covers is the *split* one:
//! when a withheld balance has to be shared, each party's post-tax share is
//! `its share of the gross − its share of the tax`.
//!
//! The invariants pinned here are:
//!   1. **Percentages are correct.** At a handful of ratios, the client's
//!      post-tax leg equals `gross × client_bps / 10_000` net of the tax taken
//!      in the same ratio.
//!   2. **Value is conserved.** `client_refund + freelancer_payout` equals
//!      `gross − tax` exactly, for every ratio, including the ones that make
//!      both independent `round_nearest` calls land on a .5 boundary.
//!   3. **No leg is negative.** A party never receives less than zero even when
//!      the tax rate is at its maximum.
//!   4. **The extremes are typed errors**, not panics: `i128::MAX` / `i128::MIN`
//!      amounts and BPS values that do not sum to 10 000 are rejected.
//!   5. **It is a pure calculator** — it works on an uninitialised contract and
//!      writes no ledger entry.

use super::*;
use crate::test::setup_funded_escrow;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _};
use soroban_sdk::{
    testutils::Address as _, testutils::EnvTestConfig, vec, Env, FromVal, IntoVal, Map, Val,
};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A bare registered contract — the endpoint needs no job, admin or funding, so
/// the whole suite runs against this.
fn calculator(env: &Env) -> (Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    (contract_id, client)
}

fn twspltref_events(env: &Env) -> std::vec::Vec<TaxWithholdingSplitRefundEvent> {
    let topic_val: Val = symbol_short!("twspltref").into_val(env);
    let mut events = std::vec::Vec::new();
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                events.push(TaxWithholdingSplitRefundEvent::from_val(env, &event.2));
            }
        }
    }
    events
}

// ── the refund percentages are what the ratio asks for ───────────────────────

/// 70/30 on a 1 000 gross with 100 withheld. Each leg is its share of the
/// gross minus its share of the tax, and the two sum to the 900 that survives
/// withholding.
#[test]
fn test_tax_withholding_split_refund_applies_requested_percentages() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let alloc = client.tax_withholding_split_refund(&1_000_i128, &100_i128, &7_000_u32, &3_000_u32);

    // client: 700 of gross, 70 of tax -> 630
    // freelancer: 300 of gross, 30 of tax -> 270
    assert_eq!(alloc.client_refund, 630);
    assert_eq!(alloc.freelancer_payout, 270);
    assert_eq!(alloc.client_refund_bps, 7_000);
    assert_eq!(alloc.freelancer_payout_bps, 3_000);
    assert_eq!(alloc.client_refund + alloc.freelancer_payout, 900);
}

/// 0 bps to the client is a full refund to the freelancer of the whole net
/// amount — the degenerate case of the pathway.
#[test]
fn test_tax_withholding_split_refund_zero_client_bps_pays_freelancer_everything() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let alloc = client.tax_withholding_split_refund(&1_000_i128, &250_i128, &0_u32, &10_000_u32);
    assert_eq!(alloc.client_refund, 0);
    assert_eq!(alloc.freelancer_payout, 750);
}

/// 10 000 bps to the client is the mirror image.
#[test]
fn test_tax_withholding_split_refund_full_client_bps_pays_client_everything() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let alloc = client.tax_withholding_split_refund(&1_000_i128, &250_i128, &10_000_u32, &0_u32);
    assert_eq!(alloc.client_refund, 750);
    assert_eq!(alloc.freelancer_payout, 0);
}

/// An even 50/50 split of a taxed balance: each side bears half the withholding,
/// so neither is charged the other's tax.
#[test]
fn test_tax_withholding_split_refund_even_split_attributes_tax_proportionally() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let alloc = client.tax_withholding_split_refund(&1_000_i128, &200_i128, &5_000_u32, &5_000_u32);
    assert_eq!(alloc.client_refund, 400);
    assert_eq!(alloc.freelancer_payout, 400);
}

/// With no tax withheld the pathway degenerates to a plain ratio split of the
/// gross, which is what makes it safe to preview before the tax is applied.
#[test]
fn test_tax_withholding_split_refund_zero_tax_is_a_plain_ratio_split() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let alloc = client.tax_withholding_split_refund(&1_000_i128, &0_i128, &7_000_u32, &3_000_u32);
    assert_eq!(alloc.client_refund, 700);
    assert_eq!(alloc.freelancer_payout, 300);
}

/// A 100 % tax rate leaves a net amount of zero: the allocation is (0, 0) and
/// never negative, even though the gross is fully absorbed.
#[test]
fn test_tax_withholding_split_refund_full_tax_rate_yields_zero_allocations() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let alloc =
        client.tax_withholding_split_refund(&1_000_i128, &1_000_i128, &3_333_u32, &6_667_u32);
    assert_eq!(alloc.client_refund, 0);
    assert_eq!(alloc.freelancer_payout, 0);
    assert_eq!(alloc.client_refund + alloc.freelancer_payout, 0);
}

// ── value conservation across the rounding boundaries ───────────────────────

/// Sweep every 1 % ratio plus the awkward .5 boundaries and assert the
/// conservation invariant on each one: the two post-tax legs must always add up
/// to `gross − tax` exactly, and must never be negative.
#[test]
fn test_tax_withholding_split_refund_conserves_value_across_all_ratios() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let (gross, tax) = (1_001_i128, 333_i128);
    for client_bps in 0..=10_000u32 {
        let alloc =
            client.tax_withholding_split_refund(&gross, &tax, &client_bps, &(10_000 - client_bps));

        assert!(
            alloc.client_refund >= 0,
            "client leg negative at {client_bps} bps"
        );
        assert!(
            alloc.freelancer_payout >= 0,
            "freelancer leg negative at {client_bps} bps"
        );
        assert_eq!(
            alloc.client_refund + alloc.freelancer_payout,
            gross - tax,
            "value not conserved at {client_bps} bps"
        );
    }
}

/// The same invariant over a range of gross/tax pairs, including odd totals
/// where both `round_nearest` calls hit a .5 tie and the naive "subtract one
/// half, add one half" implementation would drift.
#[test]
fn test_tax_withholding_split_refund_conserves_value_across_amounts() {
    let env = test_env();
    let (_, client) = calculator(&env);

    for gross in [1_i128, 2, 3, 7, 99, 101, 1_000, 1_001, 12_345] {
        for tax in [0_i128, 1, gross / 3, gross / 2, gross] {
            if tax > gross {
                continue;
            }
            for client_bps in [0_u32, 1, 2_500, 3_333, 6_667, 9_999, 10_000] {
                let alloc = client.tax_withholding_split_refund(
                    &gross,
                    &tax,
                    &client_bps,
                    &(10_000 - client_bps),
                );
                assert_eq!(
                    alloc.client_refund + alloc.freelancer_payout,
                    gross - tax,
                    "gross={gross} tax={tax} client_bps={client_bps}"
                );
                assert!(alloc.client_refund >= 0);
                assert!(alloc.freelancer_payout >= 0);
            }
        }
    }
}

/// A single-stroop net amount survives the split intact: it is never rounded
/// away to (0, 0) on one leg and (1, 0) on the other.
#[test]
fn test_tax_withholding_split_refund_single_stroop_net_is_conserved() {
    let env = test_env();
    let (_, client) = calculator(&env);

    let alloc = client.tax_withholding_split_refund(&2_i128, &1_i128, &5_000_u32, &5_000_u32);
    assert_eq!(alloc.client_refund + alloc.freelancer_payout, 1);
    assert!(alloc.client_refund >= 0);
    assert!(alloc.freelancer_payout >= 0);
}

// ── bad input is a typed error ───────────────────────────────────────────────

/// A non-positive gross, a negative tax, and a tax larger than the gross are
/// each rejected with `InvalidAmount` — before any rounding happens.
#[test]
fn test_tax_withholding_split_refund_rejects_impossible_amounts() {
    let env = test_env();
    let (_, client) = calculator(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&0_i128, &0_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&(-1_i128), &0_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &(-1_i128), &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &1_001_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
}

/// Shares that do not sum to `BPS_SCALE`, and a `u32` addition that overflows,
/// are rejected with `InvalidRatio`.
#[test]
fn test_tax_withholding_split_refund_rejects_bad_ratios() {
    let env = test_env();
    let (_, client) = calculator(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &0_u32, &0_u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &4_000_u32, &4_000_u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &10_001_u32, &0_u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &u32::MAX, &u32::MAX),
        Err(Ok(Error::InvalidRatio))
    );
}

/// `i128::MAX` / `i128::MIN` amounts surface as a typed error rather than
/// panicking or wrapping — the shared `split_round_nearest` primitive does the
/// checked multiplication, and every subtraction here is `checked_sub`.
#[test]
fn test_tax_withholding_split_refund_rejects_i128_extremes() {
    let env = test_env();
    let (_, client) = calculator(&env);

    // i128::MAX gross with a 10 000 bps ratio overflows the scaled product.
    assert_eq!(
        client.try_tax_withholding_split_refund(&i128::MAX, &0_i128, &10_000_u32, &0_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&i128::MIN, &0_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
}

/// A tax that is the full `i128::MAX` against a `i128::MAX` gross is legal in
/// range but overflows when the ratio is applied, and must still be a typed
/// error rather than a wrap.
#[test]
fn test_tax_withholding_split_refund_rejects_tax_extreme_overflow() {
    let env = test_env();
    let (_, client) = calculator(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&i128::MAX, &i128::MAX, &1_u32, &9_999_u32),
        Err(Ok(Error::InvalidAmount))
    );
}

// ── it is a pure calculator, and it announces itself ─────────────────────────

/// The endpoint needs no initialization, no admin, no funding and no token
/// balance, and writes no ledger entry — it is a preview, so it must answer on a
/// bare contract and leave storage exactly as it found it.
#[test]
fn test_tax_withholding_split_refund_is_a_pure_calculator() {
    let env = test_env();
    let (contract_id, client) = calculator(&env);

    let before: Map<Val, Val> = env.as_contract(&contract_id, || env.storage().instance().all());
    let persistent_before: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());

    let alloc = client.tax_withholding_split_refund(&1_000_i128, &100_i128, &6_000_u32, &4_000_u32);
    assert_eq!(alloc.client_refund, 540);
    assert_eq!(alloc.freelancer_payout, 360);

    let after: Map<Val, Val> = env.as_contract(&contract_id, || env.storage().instance().all());
    let persistent_after: Map<Val, Val> =
        env.as_contract(&contract_id, || env.storage().persistent().all());
    assert_eq!(after, before, "instance storage must be untouched");
    assert_eq!(
        persistent_after, persistent_before,
        "persistent storage untouched"
    );
}

/// Success publishes exactly one `twspltref` event carrying the gross, tax, net
/// and both post-tax legs; a rejected call publishes none.
#[test]
fn test_tax_withholding_split_refund_emits_exactly_one_event_on_success() {
    let env = test_env();
    let (_, client) = calculator(&env);

    client.tax_withholding_split_refund(&1_000_i128, &100_i128, &7_000_u32, &3_000_u32);

    let events = twspltref_events(&env);
    assert_eq!(events.len(), 1, "expected exactly one twspltref event");
    let ev = events.first().unwrap();
    assert_eq!(ev.gross_amount, 1_000);
    assert_eq!(ev.tax_amount, 100);
    assert_eq!(ev.net_amount, 900);
    assert_eq!(ev.client_refund, 630);
    assert_eq!(ev.freelancer_payout, 270);
    assert_eq!(ev.client_refund_bps, 7_000);
    assert_eq!(ev.freelancer_payout_bps, 3_000);
    assert_eq!(ev.client_refund + ev.freelancer_payout, ev.net_amount);
}

#[test]
fn test_tax_withholding_split_refund_emits_no_event_on_rejection() {
    let env = test_env();
    let (_, client) = calculator(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &2_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(twspltref_events(&env).is_empty());
}

// ── it agrees with the tax record the milestone actually produced ────────────

/// End-to-end against a real escrow: run `tax_withholding_deductions` at a real
/// rate, then split the gross/tax pair it recorded. The legs the calculator
/// returns must add up to the `net_amount` the milestone was locked at, which is
/// what an actual settlement would move.
#[test]
fn test_tax_withholding_split_refund_matches_recorded_tax_withholding_record() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // 1 000 escrowed, nothing released -> gross 1 000. 10 % = 100 bps tax.
    let record = client.tax_withholding_deductions(&0u32, &1_000_u32);
    assert_eq!(record.gross_amount, 1_000);
    assert_eq!(record.tax_amount, 100);
    assert_eq!(record.net_amount, 900);

    let alloc = client.tax_withholding_split_refund(
        &record.gross_amount,
        &record.tax_amount,
        &7_500_u32,
        &2_500_u32,
    );

    // client: 750 gross − 75 tax = 675; freelancer: 250 − 25 = 225.
    assert_eq!(alloc.client_refund, 675);
    assert_eq!(alloc.freelancer_payout, 225);
    assert_eq!(
        alloc.client_refund + alloc.freelancer_payout,
        record.net_amount,
        "the split must settle exactly the locked net amount"
    );

    // The calculator itself is still side-effect free even on a live escrow: the
    // tax record the milestone is holding is untouched.
    let stored: TaxWithholdingRecord = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(0u32))
            .unwrap()
    });
    assert_eq!(stored, record);
}

// ── the settlement pathway actually moves the money ──────────────────────────

fn token_balance(env: &Env, token_addr: &Address, who: &Address) -> i128 {
    token::Client::new(env, token_addr).balance(who)
}

fn adtwsplt_events(env: &Env) -> std::vec::Vec<AdminOverrideTaxSplitRefundEvent> {
    let topic_val: Val = symbol_short!("adtwsplt").into_val(env);
    let mut events = std::vec::Vec::new();
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                events.push(AdminOverrideTaxSplitRefundEvent::from_val(env, &event.2));
            }
        }
    }
    events
}

/// The validation the issue asks for, on a real escrow: after settling a
/// split-refund claim the *balances* have to show the requested percentages —
/// not just the returned allocation. 10 % is withheld from a 1 000 milestone,
/// then 75/25 of the net goes to each party.
#[test]
fn test_admin_override_tax_split_refund_transfers_requested_percentages() {
    let env = test_env();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, token_addr, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let client_before = token_balance(&env, &token_addr, &client_addr);
    let freelancer_before = token_balance(&env, &token_addr, &freelancer_addr);

    let record = client.tax_withholding_deductions(&0u32, &1_000_u32);
    assert_eq!(record.net_amount, 900);

    client.admin_override_tax_split_refund(&admin_addr, &0u32, &7_500_u32, &2_500_u32);

    // 750 of gross − 75 of tax = 675; 250 − 25 = 225. 675 + 225 == 900, the
    // exact net the milestone was locked at, so no stroop is stranded.
    assert_eq!(
        token_balance(&env, &token_addr, &client_addr) - client_before,
        675
    );
    assert_eq!(
        token_balance(&env, &token_addr, &freelancer_addr) - freelancer_before,
        225
    );

    // Both parties got a real payout, so the milestone is settled as Released
    // and its tax lock is gone.
    let milestone = client.get_job().milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Released);
    assert_eq!(milestone.released_amount, 900);
    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &7_500_u32, &2_500_u32),
        Err(Ok(Error::InvalidStatus))
    );
}

/// The event reconciles with the transfers that actually happened.
#[test]
fn test_admin_override_tax_split_refund_event_reconciles_with_transfers() {
    let env = test_env();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, token_addr, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let client_before = token_balance(&env, &token_addr, &client_addr);
    let freelancer_before = token_balance(&env, &token_addr, &freelancer_addr);

    client.tax_withholding_deductions(&0u32, &1_000_u32);
    client.admin_override_tax_split_refund(&admin_addr, &0u32, &3_333_u32, &6_667_u32);

    let events = adtwsplt_events(&env);
    assert_eq!(events.len(), 1);
    let ev = events.first().unwrap();

    assert_eq!(ev.admin, admin_addr);
    assert_eq!(ev.contract_id, contract_id);
    assert_eq!(ev.milestone_index, 0);
    assert_eq!(ev.client, client_addr);
    assert_eq!(ev.freelancer, freelancer_addr);
    assert_eq!(ev.token, token_addr);
    assert_eq!(ev.gross_amount, 1_000);
    assert_eq!(ev.tax_amount, 100);
    assert_eq!(ev.client_refund_bps, 3_333);
    assert_eq!(ev.freelancer_payout_bps, 6_667);

    assert_eq!(
        ev.client_refund,
        token_balance(&env, &token_addr, &client_addr) - client_before
    );
    assert_eq!(
        ev.freelancer_payout,
        token_balance(&env, &token_addr, &freelancer_addr) - freelancer_before
    );
    assert_eq!(ev.client_refund + ev.freelancer_payout, 900);
}

/// A 100 % client split degenerates to a plain gross refund: the freelancer is
/// paid nothing, so no zero-amount transfer is attempted and the milestone is
/// marked `Refunded` instead of `Released`.
#[test]
fn test_admin_override_tax_split_refund_full_client_split_refunds_client() {
    let env = test_env();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, token_addr, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let client_before = token_balance(&env, &token_addr, &client_addr);
    let freelancer_before = token_balance(&env, &token_addr, &freelancer_addr);

    client.tax_withholding_deductions(&0u32, &1_000_u32);
    client.admin_override_tax_split_refund(&admin_addr, &0u32, &10_000_u32, &0_u32);

    assert_eq!(
        token_balance(&env, &token_addr, &client_addr) - client_before,
        900
    );
    assert_eq!(
        token_balance(&env, &token_addr, &freelancer_addr),
        freelancer_before,
        "a zero share must not move tokens"
    );
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Refunded
    );
}

/// Only the verified admin may settle, and the rejection mutates nothing.
#[test]
fn test_admin_override_tax_split_refund_requires_admin() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.tax_withholding_deductions(&0u32, &1_000_u32);

    let stranger = Address::generate(&env);
    let before: Map<Val, Val> = env.as_contract(&contract_id, || env.storage().persistent().all());

    assert_eq!(
        client.try_admin_override_tax_split_refund(&stranger, &0u32, &7_500_u32, &2_500_u32),
        Err(Ok(Error::Unauthorized))
    );

    let after: Map<Val, Val> = env.as_contract(&contract_id, || env.storage().persistent().all());
    assert_eq!(after, before, "a rejected claim must not touch storage");
    assert!(adtwsplt_events(&env).is_empty());

    // The tax lock survived the rejection, so the real admin can still settle it.
    client.admin_override_tax_split_refund(&admin_addr, &0u32, &7_500_u32, &2_500_u32);
    assert_eq!(adtwsplt_events(&env).len(), 1);
}

/// An illegal ratio is caught before any state is committed, so the milestone's
/// tax lock is still intact and a well-formed retry still succeeds.
#[test]
fn test_admin_override_tax_split_refund_rejects_bad_ratio_without_consuming_lock() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.tax_withholding_deductions(&0u32, &1_000_u32);

    let locked: TaxWithholdingRecord = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(0u32))
            .unwrap()
    });

    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &4_000_u32, &4_000_u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &u32::MAX, &u32::MAX),
        Err(Ok(Error::InvalidRatio))
    );

    let still_locked: Option<TaxWithholdingRecord> = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(0u32))
    });
    assert_eq!(still_locked, Some(locked));
    assert!(adtwsplt_events(&env).is_empty());

    // The lock is untouched, so the claim can still be made properly.
    client.admin_override_tax_split_refund(&admin_addr, &0u32, &5_000_u32, &5_000_u32);
    assert_eq!(adtwsplt_events(&env).len(), 1);
}
