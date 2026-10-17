#![cfg(test)]
//! Dedicated unit-test suite for `admin_tax_withholding_deductions`
//! authorization and precondition guards (issue #335).
//!
//! The endpoint must reject an unauthorised caller with `Unauthorized` and
//! illegal source states (uninitialised contract, unfunded escrow,
//! out-of-range milestone, non-positive balance, over-scale tax rate) with
//! their specific typed error **before** any ledger entry is read or written.
//! Every rejected path must leave the ledger untouched and emit no `taxwh`
//! event; the happy path must compute the split and emit exactly one event.

use super::*;
use crate::{DataKey, Error, TaxWithholdingDeductionsEvent};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal, Val};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A contract that has never been initialised — no admin key stored.
fn bare_contract(env: &Env) -> (MilestoneEscrowClient<'_>, Address) {
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    (client, contract_id)
}

fn execution_lock_held(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .has(&DataKey::TaxWithholdingExecutionLock)
    })
}

fn first_milestone_status(_env: &Env, client: &MilestoneEscrowClient<'_>) -> MilestoneStatus {
    client.get_job().milestones.get(0).unwrap().status
}

fn taxwh_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("taxwh").into_val(env);
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

fn last_taxwh_event(env: &Env) -> TaxWithholdingDeductionsEvent {
    let events = crate::all_event_tuples(env);
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(topic, Symbol::new(env, "taxwh"));
    TaxWithholdingDeductionsEvent::from_val(env, &last.2)
}

// ── authorization ────────────────────────────────────────────────────────────

#[test]
fn admin_tax_rejects_caller_on_uninitialised_contract() {
    let env = test_env();
    env.mock_all_auths();

    let (client, contract_id) = bare_contract(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&stranger, &0u32, &1000u32),
        Err(Ok(Error::NotInitialized))
    );
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(taxwh_event_count(&env), 0);
}

#[test]
fn admin_tax_rejects_non_admin_caller() {
    let env = test_env();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&client_addr, &0u32, &1000u32),
        Err(Ok(Error::Unauthorized))
    );

    // Rejection happened before any ledger write: no execution lock was
    // created and the milestone is untouched.
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(
        first_milestone_status(&env, &client),
        MilestoneStatus::Pending
    );
    assert_eq!(taxwh_event_count(&env), 0);
}

// ── illegal source states ────────────────────────────────────────────────────

#[test]
fn admin_tax_rejects_unfunded_contract() {
    let env = test_env();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, contract_id, client) = setup_multisig_env(&env);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &1000u32),
        Err(Ok(Error::NotFunded))
    );
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(taxwh_event_count(&env), 0);
}

#[test]
fn admin_tax_rejects_out_of_range_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&admin_addr, &99u32, &1000u32),
        Err(Ok(Error::InvalidMilestone))
    );
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(
        first_milestone_status(&env, &client),
        MilestoneStatus::Pending
    );
    assert_eq!(taxwh_event_count(&env), 0);
}

#[test]
fn admin_tax_rejects_rate_above_full_scale() {
    let env = test_env();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &10_001u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(taxwh_event_count(&env), 0);
}

#[test]
fn admin_tax_rejects_empty_contract_balance() {
    let env = test_env();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Drain the escrow so the fund balance is zero.
    let token_client = token::Client::new(&env, &token_contract_id);
    token_client.transfer(&contract_id, Address::generate(&env), &1_000_i128);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &1000u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(
        first_milestone_status(&env, &client),
        MilestoneStatus::Pending
    );
    assert_eq!(taxwh_event_count(&env), 0);
}

// ── happy path ───────────────────────────────────────────────────────────────

#[test]
fn admin_tax_success_computes_and_emits_exactly_one_event() {
    let env = test_env();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    let (gross_amount, tax_amount, net_amount) =
        client.admin_tax_withholding_deductions(&admin_addr, &0u32, &2_500u32);

    assert_eq!(gross_amount, 1_000);
    assert_eq!(tax_amount, 250);
    assert_eq!(net_amount, 750);
    assert_eq!(gross_amount, tax_amount + net_amount);

    let event = last_taxwh_event(&env);
    assert_eq!(taxwh_event_count(&env), 1);
    assert_eq!(event.admin, admin_addr);
    assert_eq!(event.contract_id, contract_id);
    assert_eq!(event.milestone_index, 0);
    assert_eq!(event.gross_amount, 1_000);
    assert_eq!(event.tax_amount, 250);
    assert_eq!(event.net_amount, 750);
    assert_eq!(event.tax_rate_bps, 2_500);

    // The execution lock is released before returning; no stale entry remains.
    assert!(!execution_lock_held(&env, &contract_id));
}

// ── checked-arithmetic boundary cases ───────────────────────────────────────
//
// The multiply/divide/subtract chain that derives `tax_amount` and
// `net_amount` from the milestone's gross amount must use checked
// operations end-to-end: any operand that would overflow or underflow an
// i128 must surface as `Error::InvalidAmount` rather than panicking (debug
// builds) or silently wrapping (release builds). These tests exercise the
// two extreme `i128` operands directly against stored ledger state, since
// neither `i128::MAX` nor `i128::MIN` can be produced through the normal
// `initialize`/`fund` token-minting path.

/// Overwrite milestone 0's `amount` after a normal funded setup, so the
/// authorization/funded/milestone-range/balance guards all still pass and
/// only the arithmetic itself is under test.
fn set_milestone_amount(env: &Env, contract_id: &Address, index: u32, amount: i128) {
    env.as_contract(contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(index))
            .unwrap();
        milestone.amount = amount;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(index), &milestone);
    });
}

#[test]
fn admin_tax_rejects_overflow_at_i128_max_gross_amount() {
    let env = test_env();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Push the stored milestone amount to i128::MAX. The contract's token
    // balance stays at 1_000 (untouched), which is still > 0, so the
    // balance guard passes and execution reaches the checked-arithmetic
    // chain: gross_amount.checked_mul(tax_rate_bps) must overflow and be
    // reported as `InvalidAmount` rather than wrapping or panicking.
    set_milestone_amount(&env, &contract_id, 0, i128::MAX);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &2_500u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(taxwh_event_count(&env), 0);
}

#[test]
fn admin_tax_rejects_i128_min_gross_amount() {
    let env = test_env();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // i128::MIN as a gross amount is caught by the existing
    // `milestone.amount <= 0` precondition guard before any arithmetic
    // runs. It must still resolve to `InvalidAmount`, not a panic — in
    // particular it must never reach a bare negation or division that
    // could overflow (i128::MIN has no positive counterpart).
    set_milestone_amount(&env, &contract_id, 0, i128::MIN);

    assert_eq!(
        client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &2_500u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(!execution_lock_held(&env, &contract_id));
    assert_eq!(taxwh_event_count(&env), 0);
}

#[test]
fn admin_tax_valid_amounts_unchanged_after_checked_arithmetic() {
    let env = test_env();
    env.mock_all_auths();

    // Regression guard: ordinary, well within range amounts must produce
    // byte-for-byte identical results to the pre-checked-arithmetic
    // implementation.
    let milestone_amounts = vec![&env, 1_000_000_i128];
    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let (gross_amount, tax_amount, net_amount) =
        client.admin_tax_withholding_deductions(&admin_addr, &0u32, &1_500u32);

    assert_eq!(gross_amount, 1_000_000);
    assert_eq!(tax_amount, 150_000);
    assert_eq!(net_amount, 850_000);
    assert_eq!(gross_amount, tax_amount + net_amount);
}

#[test]
fn admin_tax_rejects_zero_rate_overflow_still_checked() {
    let env = test_env();
    env.mock_all_auths();

    // Even with tax_rate_bps == 0 (net == gross, tax == 0), a gross amount
    // large enough to overflow checked_mul(0) never actually overflows
    // mathematically, but the call path must still resolve through the
    // checked operators without panicking, returning the trivial split.
    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    set_milestone_amount(&env, &contract_id, 0, i128::MAX);

    let (gross_amount, tax_amount, net_amount) =
        client.admin_tax_withholding_deductions(&admin_addr, &0u32, &0u32);

    assert_eq!(gross_amount, i128::MAX);
    assert_eq!(tax_amount, 0);
    assert_eq!(net_amount, i128::MAX);
}
