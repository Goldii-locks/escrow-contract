#![cfg(test)]
//! Split-refund allocation of a tax-withheld balance (issue #300).
//!
//! `tax_withholding_deductions` withholds a tax from a milestone, but nothing
//! told the parties how a *refund* of that withheld balance should be divided.
//! The new pure calculator `tax_withholding_split_refund` does: it apportions
//! the gross amount and the withheld tax over the same client/freelancer ratio,
//! then hands each party its post-tax share.
//!
//! The matrix below pins:
//! 1. Conservation — `client_refund + freelancer_payout == gross − tax` exactly,
//!    for every ratio including the extremes, so no stroop is created or lost.
//! 2. The tax is attributed proportionally — a party never receives more than
//!    its own post-tax share, and the withheld tax is fully absorbed.
//! 3. Boundary behaviour — 0% tax, 100% tax, 0%/100% splits, 1-stroop amounts.
//! 4. Every rejection is typed, and a rejected call emits no event.

use super::*;
use soroban_sdk::testutils::storage::{Instance as _, Persistent as _};
use soroban_sdk::{
    testutils::EnvTestConfig, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val,
};

/// Snapshot capture is disabled; no assertion here reads a JSON snapshot.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

struct Calculator<'a> {
    contract_id: Address,
    escrow: MilestoneEscrowClient<'a>,
}

/// A bare registration: the endpoint is a pure calculator, so it needs no
/// initialization, no token and no funded balance.
fn calculator(env: &Env) -> Calculator<'_> {
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    Calculator {
        contract_id,
        escrow,
    }
}

/// Count events carrying `topic` as their first topic.
///
/// The SDK's event log reflects only the most recent invocation, so this counts
/// the log rather than diffing it: an assertion that a rejected call publishes
/// nothing is then valid whether or not the failed frame cleared the log.
fn count_topic(env: &Env, topic: soroban_sdk::Symbol) -> u32 {
    let topic_val: Val = topic.into_val(env);
    crate::all_event_tuples(env)
        .iter()
        .fold(0u32, |acc, event| {
            if let Some(first) = event.1.get(0) {
                if first.get_payload() == topic_val.get_payload() {
                    return acc + 1;
                }
            }
            acc
        })
}

const BPS_SCALE: i128 = 10_000;
const BPS_SCALE_U32: u32 = 10_000;

// ── conservation across the whole ratio space ────────────────────────────────

/// The defining invariant. Swept over every whole-percent split plus both
/// extremes, with several tax rates: the two post-tax legs always sum to the
/// net, and neither ever goes negative.
#[test]
fn test_tax_withholding_split_refund_conserves_net_amount() {
    let env = test_env();
    let c = calculator(&env);

    for percent in 0..=100_u32 {
        let client_bps = percent * 100;
        let freelancer_bps = BPS_SCALE_U32 - client_bps;
        for tax in [0_i128, 1, 7, 100, 999, 5_000] {
            for gross in [1_i128, 2, 999, 1_000, 12_345, 1_000_000] {
                if tax > gross {
                    continue;
                }
                let net = gross - tax;
                let a = c.escrow.tax_withholding_split_refund(
                    &gross,
                    &tax,
                    &client_bps,
                    &freelancer_bps,
                );
                assert_eq!(a.client_refund_bps, client_bps);
                assert_eq!(a.freelancer_payout_bps, freelancer_bps);
                assert!(
                    a.client_refund >= 0 && a.freelancer_payout >= 0,
                    "negative leg at percent={percent} tax={tax} gross={gross}"
                );
                assert_eq!(
                    a.client_refund + a.freelancer_payout,
                    net,
                    "net not conserved at percent={percent} tax={tax} gross={gross}"
                );
            }
        }
    }
}

/// Round-nearest, not truncating: a client share that lands on a half stroop
/// rounds up. 1 stroop at 5 000 bps is exactly 0.5, so `split_round_nearest`
/// gives the whole stroop to the client.
#[test]
fn test_tax_withholding_split_refund_rounds_half_up() {
    let env = test_env();
    let c = calculator(&env);

    let a = c
        .escrow
        .tax_withholding_split_refund(&1, &0, &5_000u32, &5_000u32);
    assert_eq!(a.client_refund, 1);
    assert_eq!(a.freelancer_payout, 0);

    // 3 at 5 000 bps is 1.5 -> 2 to the client, 1 to the freelancer.
    let a = c
        .escrow
        .tax_withholding_split_refund(&3, &0, &5_000u32, &5_000u32);
    assert_eq!(a.client_refund, 2);
    assert_eq!(a.freelancer_payout, 1);
    assert_eq!(a.client_refund + a.freelancer_payout, 3);
}

/// Both legs round in the same direction because gross and tax are apportioned
/// by the same ratio, so the difference never borrows a stroop from the other
/// party.
#[test]
fn test_tax_withholding_split_refund_rounding_cannot_overpay_either_leg() {
    let env = test_env();
    let c = calculator(&env);

    for gross in 1_i128..=64 {
        for tax in 0_i128..=gross {
            let a = c
                .escrow
                .tax_withholding_split_refund(&gross, &tax, &3_333u32, &6_667u32);
            let client_gross = (gross * 3_333 + BPS_SCALE / 2) / BPS_SCALE;
            let client_tax = (tax * 3_333 + BPS_SCALE / 2) / BPS_SCALE;
            assert_eq!(a.client_refund, client_gross - client_tax);
            assert_eq!(a.freelancer_payout, gross - tax - a.client_refund);
        }
    }
}

// ── the tax is attributed proportionally ─────────────────────────────────────

/// A 10% tax on 1 000 split 50/50: each party absorbs half the tax, so each
/// takes 450 of the 900 net.
#[test]
fn test_tax_withholding_split_refund_equal_split_absorbs_tax_evenly() {
    let env = test_env();
    let c = calculator(&env);

    let a = c
        .escrow
        .tax_withholding_split_refund(&1_000, &100, &5_000u32, &5_000u32);
    assert_eq!(a.client_refund, 450);
    assert_eq!(a.freelancer_payout, 450);
    assert_eq!(a.client_refund + a.freelancer_payout, 900);
}

/// A 70/30 split of the same 1 000 gross with a 100 tax: the client gives up
/// 70 of the tax, the freelancer 30.
#[test]
fn test_tax_withholding_split_refund_asymmetric_split_attributes_tax_by_ratio() {
    let env = test_env();
    let c = calculator(&env);

    let a = c
        .escrow
        .tax_withholding_split_refund(&1_000, &100, &7_000u32, &3_000u32);
    assert_eq!(a.client_refund, 700 - 70);
    assert_eq!(a.freelancer_payout, 300 - 30);
    assert_eq!(a.client_refund + a.freelancer_payout, 900);
}

/// No tax at all reduces the endpoint to the plain gross split.
#[test]
fn test_tax_withholding_split_refund_zero_tax_is_the_gross_split() {
    let env = test_env();
    let c = calculator(&env);

    for (client_bps, freelancer_bps) in [(0_u32, 10_000_u32), (10_000, 0), (7_000, 3_000)] {
        let a = c
            .escrow
            .tax_withholding_split_refund(&1_000, &0, &client_bps, &freelancer_bps);
        let plain = c
            .escrow
            .cancel_escrow_split_refund(&1_000, &client_bps, &freelancer_bps);
        assert_eq!(a.client_refund, plain.client_refund);
        assert_eq!(a.freelancer_payout, plain.freelancer_payout);
    }
}

/// A 100% tax leaves nothing to distribute, and neither leg goes negative.
#[test]
fn test_tax_withholding_split_refund_full_tax_pays_nothing() {
    let env = test_env();
    let c = calculator(&env);

    for (client_bps, freelancer_bps) in [(0_u32, 10_000_u32), (10_000, 0), (5_000, 5_000)] {
        let a = c
            .escrow
            .tax_withholding_split_refund(&1_000, &1_000, &client_bps, &freelancer_bps);
        assert_eq!(a.client_refund, 0);
        assert_eq!(a.freelancer_payout, 0);
    }
}

// ── ratio extremes ───────────────────────────────────────────────────────────

/// All of the net to one side, none to the other.
#[test]
fn test_tax_withholding_split_refund_extreme_ratios() {
    let env = test_env();
    let c = calculator(&env);

    let a = c
        .escrow
        .tax_withholding_split_refund(&1_000, &250, &10_000u32, &0u32);
    assert_eq!(a.client_refund, 750);
    assert_eq!(a.freelancer_payout, 0);

    let a = c
        .escrow
        .tax_withholding_split_refund(&1_000, &250, &0u32, &10_000u32);
    assert_eq!(a.client_refund, 0);
    assert_eq!(a.freelancer_payout, 750);
}

// ── every rejection is typed ─────────────────────────────────────────────────

#[test]
fn test_tax_withholding_split_refund_rejects_invalid_amounts() {
    let env = test_env();
    let c = calculator(&env);

    for gross in [0_i128, -1, i128::MIN] {
        assert_eq!(
            c.escrow
                .try_tax_withholding_split_refund(&gross, &0, &5_000u32, &5_000u32),
            Err(Ok(Error::InvalidAmount))
        );
    }
    for tax in [-1_i128, i128::MIN] {
        assert_eq!(
            c.escrow
                .try_tax_withholding_split_refund(&1_000, &tax, &5_000u32, &5_000u32),
            Err(Ok(Error::InvalidAmount))
        );
    }
    // Tax above the gross balance is not withheld tax.
    assert_eq!(
        c.escrow
            .try_tax_withholding_split_refund(&1_000, &1_001, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_tax_withholding_split_refund_rejects_invalid_ratios() {
    let env = test_env();
    let c = calculator(&env);

    let bad = [
        (0_u32, 0_u32),
        (1_000_u32, 1_000_u32),
        (5_000_u32, 5_001_u32),
        (10_001_u32, 0_u32),
        (0_u32, 10_001_u32),
        (u32::MAX, u32::MAX),
    ];
    for (client_bps, freelancer_bps) in bad {
        assert_eq!(
            c.escrow
                .try_tax_withholding_split_refund(&1_000, &100, &client_bps, &freelancer_bps),
            Err(Ok(Error::InvalidRatio))
        );
    }
}

/// The `u32` addition is checked, so a pair that overflows `u32` reports
/// `InvalidRatio` rather than wrapping to a value that happens to equal
/// `BPS_SCALE`.
#[test]
fn test_tax_withholding_split_refund_ratio_sum_overflow_is_typed() {
    let env = test_env();
    let c = calculator(&env);

    assert_eq!(
        c.escrow
            .try_tax_withholding_split_refund(&1_000, &100, &u32::MAX, &1_u32),
        Err(Ok(Error::InvalidRatio))
    );
}

/// A rejected call publishes no event, so an indexer never records an
/// allocation that did not happen.
#[test]
fn test_tax_withholding_split_refund_rejected_call_emits_no_event() {
    let env = test_env();
    let c = calculator(&env);

    for (gross, tax, client_bps, freelancer_bps) in [
        (0_i128, 0_i128, 5_000_u32, 5_000_u32),
        (1_000, -1_i128, 5_000, 5_000),
        (1_000, 1_001, 5_000, 5_000),
        (1_000, 100, 1_000_u32, 1_000_u32),
    ] {
        assert!(c
            .escrow
            .try_tax_withholding_split_refund(&gross, &tax, &client_bps, &freelancer_bps)
            .is_err());
    }
    assert_eq!(
        count_topic(&env, soroban_sdk::symbol_short!("twspltref")),
        0
    );
}

// ── the event ────────────────────────────────────────────────────────────────

/// The success path publishes exactly one `twspltref` event carrying the gross,
/// the tax, the net and both post-tax legs, and the payload is self-consistent.
#[test]
fn test_tax_withholding_split_refund_emits_one_consistent_event() {
    let env = test_env();
    let c = calculator(&env);

    let a = c
        .escrow
        .tax_withholding_split_refund(&1_000, &100, &7_000u32, &3_000u32);
    assert_eq!(
        count_topic(&env, soroban_sdk::symbol_short!("twspltref")),
        1,
        "expected exactly one twspltref event"
    );
    let events = crate::all_event_tuples(&env);
    let last = events.last().unwrap();
    assert_eq!(last.0, c.contract_id);
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, soroban_sdk::symbol_short!("twspltref"));

    let ev = TaxWithholdingSplitRefundEvent::from_val(&env, &last.2);
    assert_eq!(ev.gross_amount, 1_000);
    assert_eq!(ev.tax_amount, 100);
    assert_eq!(ev.net_amount, 900);
    assert_eq!(ev.client_refund, a.client_refund);
    assert_eq!(ev.freelancer_payout, a.freelancer_payout);
    assert_eq!(ev.client_refund_bps, 7_000);
    assert_eq!(ev.freelancer_payout_bps, 3_000);

    // The event is enough on its own to reconstruct the split.
    assert_eq!(ev.net_amount, ev.gross_amount - ev.tax_amount);
    assert_eq!(ev.client_refund + ev.freelancer_payout, ev.net_amount);
}

// ── it is a pure calculator ──────────────────────────────────────────────────

/// No ledger entry is written: the endpoint can be called repeatedly as a
/// preview without leaving any trace. Instance storage stays empty because the
/// contract is never initialized.
#[test]
fn test_tax_withholding_split_refund_writes_no_ledger_entry() {
    let env = test_env();
    let c = calculator(&env);

    let instance_before = env.as_contract(&c.contract_id, || env.storage().instance().all());
    let persistent_before = env.as_contract(&c.contract_id, || env.storage().persistent().all());
    assert!(instance_before.is_empty());
    assert!(persistent_before.is_empty());

    let _ = c
        .escrow
        .tax_withholding_split_refund(&1_000, &100, &5_000u32, &5_000u32);
    let _ = c
        .escrow
        .tax_withholding_split_refund(&2_000, &200, &1_000u32, &9_000u32);

    assert_eq!(
        env.as_contract(&c.contract_id, || env.storage().instance().all()),
        instance_before
    );
    assert_eq!(
        env.as_contract(&c.contract_id, || env.storage().persistent().all()),
        persistent_before
    );
}

/// The endpoint needs no initialization: an unregistered-account caller can
/// still compute an allocation, matching the other pure calculators in the
/// contract.
#[test]
fn test_tax_withholding_split_refund_works_without_initialization() {
    let env = test_env();
    let c = calculator(&env);

    assert!(matches!(
        c.escrow.try_get_job(),
        Err(Ok(Error::NotInitialized))
    ));

    let a = c
        .escrow
        .tax_withholding_split_refund(&1_000, &250, &2_500u32, &7_500u32);
    assert_eq!(a.client_refund, 187);
    assert_eq!(a.freelancer_payout, 563);
    assert_eq!(a.client_refund + a.freelancer_payout, 750);
}
