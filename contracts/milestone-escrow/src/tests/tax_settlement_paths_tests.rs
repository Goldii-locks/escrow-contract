#![cfg(test)]
//! Branch and sequence coverage for the tax settlement paths (issue #747).
//!
//! Covers the error branches of `admin_override_tax_release`,
//! `admin_override_tax_refund`, `admin_override_tax_split_refund`,
//! `admin_tax_withholding_deductions`, `tax_withholding_deductions` and the
//! `allocate_withholding_refund` helper (reached through
//! `tax_withholding_split_refund`).
//!
//! The two sequence tests at the bottom pin the stranded-tax (#788) and
//! stale-record (#689) behaviour. The #689 one fails on `main` today and is
//! ignored until that fix lands.

use super::*;
use crate::{DataKey, Error, TaxWithholdingRecord};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, Address, Env};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A contract that has never been initialised — no admin key stored.
fn bare_contract(env: &Env) -> MilestoneEscrowClient<'_> {
    let contract_id = env.register(MilestoneEscrow, ());
    MilestoneEscrowClient::new(env, &contract_id)
}

fn set_milestone_status(env: &Env, contract_id: &Address, index: u32, status: MilestoneStatus) {
    env.as_contract(contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(index))
            .unwrap();
        milestone.status = status;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(index), &milestone);
    });
}

fn put_tax_record(env: &Env, contract_id: &Address, index: u32, record: &TaxWithholdingRecord) {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::TaxWithholdingLock(index), record);
    });
}

fn tax_record(env: &Env, contract_id: &Address, index: u32) -> Option<TaxWithholdingRecord> {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(index))
    })
}

fn sample_record() -> TaxWithholdingRecord {
    TaxWithholdingRecord {
        gross_amount: 1_000,
        tax_amount: 250,
        net_amount: 750,
        tax_rate_bps: 2_500,
    }
}

// ── admin_override_tax_release ───────────────────────────────────────────────

#[test]
fn tax_release_rejects_uninitialised_contract() {
    let env = test_env();
    env.mock_all_auths();

    let client = bare_contract(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_admin_override_tax_release(&stranger, &0u32),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn tax_release_rejects_non_admin() {
    let env = test_env();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.tax_withholding_deductions(&0u32, &2_500u32);

    assert_eq!(
        client.try_admin_override_tax_release(&client_addr, &0u32),
        Err(Ok(Error::Unauthorized))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_release_rejects_unfunded_escrow() {
    let env = test_env();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    assert_eq!(
        client.try_admin_override_tax_release(&admin_addr, &0u32),
        Err(Ok(Error::NotFunded))
    );
}

#[test]
fn tax_release_rejects_out_of_range_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_override_tax_release(&admin_addr, &1u32),
        Err(Ok(Error::InvalidMilestone))
    );
}

#[test]
fn tax_release_rejects_missing_record() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_override_tax_release(&admin_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn tax_release_rejects_released_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Released);

    assert_eq!(
        client.try_admin_override_tax_release(&admin_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_release_rejects_refunded_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Refunded);

    assert_eq!(
        client.try_admin_override_tax_release(&admin_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_release_rejects_zero_net_amount() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    // A 100 % rate leaves nothing to release.
    client.tax_withholding_deductions(&0u32, &10_000u32);

    assert_eq!(
        client.try_admin_override_tax_release(&admin_addr, &0u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
    );
}

// ── admin_override_tax_refund ────────────────────────────────────────────────

#[test]
fn tax_refund_rejects_uninitialised_contract() {
    let env = test_env();
    env.mock_all_auths();

    let client = bare_contract(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_admin_override_tax_refund(&stranger, &0u32),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn tax_refund_rejects_unfunded_escrow() {
    let env = test_env();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, contract_id, client) = setup_multisig_env(&env);
    put_tax_record(&env, &contract_id, 0, &sample_record());

    assert_eq!(
        client.try_admin_override_tax_refund(&admin_addr, &0u32),
        Err(Ok(Error::NotFunded))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_refund_rejects_out_of_range_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    // The record is read before the range check, so one has to exist at the
    // out-of-range index for the range guard to be reached.
    put_tax_record(&env, &contract_id, 5, &sample_record());

    assert_eq!(
        client.try_admin_override_tax_refund(&admin_addr, &5u32),
        Err(Ok(Error::InvalidMilestone))
    );
    assert!(tax_record(&env, &contract_id, 5).is_some());
}

#[test]
fn tax_refund_rejects_released_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Released);

    assert_eq!(
        client.try_admin_override_tax_refund(&admin_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_refund_rejects_refunded_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Refunded);

    assert_eq!(
        client.try_admin_override_tax_refund(&admin_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_refund_rejects_zero_gross_amount() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    put_tax_record(
        &env,
        &contract_id,
        0,
        &TaxWithholdingRecord {
            gross_amount: 0,
            tax_amount: 0,
            net_amount: 0,
            tax_rate_bps: 0,
        },
    );

    assert_eq!(
        client.try_admin_override_tax_refund(&admin_addr, &0u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

// ── admin_override_tax_split_refund ──────────────────────────────────────────

#[test]
fn tax_split_refund_rejects_uninitialised_contract() {
    let env = test_env();
    env.mock_all_auths();

    let client = bare_contract(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        client.try_admin_override_tax_split_refund(&stranger, &0u32, &5_000u32, &5_000u32),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn tax_split_refund_rejects_missing_record() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn tax_split_refund_rejects_unfunded_escrow() {
    let env = test_env();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, contract_id, client) = setup_multisig_env(&env);
    put_tax_record(&env, &contract_id, 0, &sample_record());

    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &5_000u32, &5_000u32),
        Err(Ok(Error::NotFunded))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_split_refund_rejects_out_of_range_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    put_tax_record(&env, &contract_id, 5, &sample_record());

    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &5u32, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidMilestone))
    );
    assert!(tax_record(&env, &contract_id, 5).is_some());
}

#[test]
fn tax_split_refund_rejects_released_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Released);

    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_split_refund_rejects_refunded_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Refunded);

    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

#[test]
fn tax_split_refund_rejects_zero_gross_amount() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    put_tax_record(
        &env,
        &contract_id,
        0,
        &TaxWithholdingRecord {
            gross_amount: 0,
            tax_amount: 0,
            net_amount: 0,
            tax_rate_bps: 0,
        },
    );

    assert_eq!(
        client.try_admin_override_tax_split_refund(&admin_addr, &0u32, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(tax_record(&env, &contract_id, 0).is_some());
}

// ── admin_tax_withholding_deductions ─────────────────────────────────────────

#[test]
fn admin_tax_rejects_zero_milestone_amount_before_ratio_check() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    env.as_contract(&contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0))
            .unwrap();
        milestone.amount = 0;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0), &milestone);
    });

    // The amount guard runs before the ratio guard, so an over-scale rate
    // still reports the amount error.
    assert_eq!(
        client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &10_001u32),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn admin_tax_accepts_full_scale_rate() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        client.admin_tax_withholding_deductions(&admin_addr, &0u32, &10_000u32),
        (1_000, 1_000, 0)
    );
}

// ── tax_withholding_deductions ───────────────────────────────────────────────

#[test]
fn tax_withholding_rejects_released_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Released);

    assert_eq!(
        client.try_tax_withholding_deductions(&0u32, &2_500u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_none());
}

#[test]
fn tax_withholding_rejects_refunded_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    set_milestone_status(&env, &contract_id, 0, MilestoneStatus::Refunded);

    assert_eq!(
        client.try_tax_withholding_deductions(&0u32, &2_500u32),
        Err(Ok(Error::InvalidStatus))
    );
    assert!(tax_record(&env, &contract_id, 0).is_none());
}

#[test]
fn tax_withholding_rejects_empty_contract_balance() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, _, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token_client = token::Client::new(&env, &token_contract_id);
    token_client.transfer(&contract_id, Address::generate(&env), &1_000_i128);

    assert_eq!(
        client.try_tax_withholding_deductions(&0u32, &2_500u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(tax_record(&env, &contract_id, 0).is_none());
}

#[test]
fn tax_withholding_rejects_fully_released_balance() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    env.as_contract(&contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0))
            .unwrap();
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::PartiallyReleased;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0), &milestone);
    });

    assert_eq!(
        client.try_tax_withholding_deductions(&0u32, &2_500u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert!(tax_record(&env, &contract_id, 0).is_none());
}

// ── allocate_withholding_refund (via tax_withholding_split_refund) ───────────

#[test]
fn withholding_refund_rejects_non_positive_gross() {
    let env = test_env();
    let client = bare_contract(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&0_i128, &0_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&-1_i128, &0_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn withholding_refund_rejects_negative_tax() {
    let env = test_env();
    let client = bare_contract(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &-1_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn withholding_refund_rejects_tax_above_gross() {
    let env = test_env();
    let client = bare_contract(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &1_001_i128, &5_000u32, &5_000u32),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn withholding_refund_rejects_bps_addition_overflow() {
    let env = test_env();
    let client = bare_contract(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &u32::MAX, &1u32),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn withholding_refund_rejects_bps_not_summing_to_scale() {
    let env = test_env();
    let client = bare_contract(&env);

    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &5_000u32, &4_999u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &5_000u32, &5_001u32),
        Err(Ok(Error::InvalidRatio))
    );
}

// ── sequence tests ───────────────────────────────────────────────────────────

/// withhold → `admin_override_tax_release`: once the milestone is settled the
/// contract should hold nothing for it: the withheld tax goes to the tax
/// recipient (#788).
#[test]
fn tax_release_leaves_no_balance_for_the_milestone() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token_client = token::Client::new(&env, &token_contract_id);

    client.tax_withholding_deductions(&0u32, &2_500u32);
    client.admin_override_tax_release(&admin_addr, &0u32);

    assert_eq!(token_client.balance(&contract_id), 0);
}

/// withhold → `approve_partial` → `admin_override_tax_refund`: the refund must
/// not exceed what is left on the milestone. On `main` the record still holds
/// the pre-partial gross, so the refund is paid out of milestone 1's funds.
#[test]
fn tax_refund_after_partial_release_does_not_exceed_remaining_balance() {
    let env = test_env();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 1_000_i128]);
    let token_client = token::Client::new(&env, &token_contract_id);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    client.approve_partial(&client_addr, &0u32, &400_i128);

    let client_before = token_client.balance(&client_addr);
    client.admin_override_tax_refund(&admin_addr, &0u32);
    let refunded = token_client.balance(&client_addr) - client_before;

    assert!(refunded <= 600);
    assert!(token_client.balance(&contract_id) >= 1_000);
}

/// withhold → `approve_partial` → `admin_override_tax_release`: the release
/// must only pay the net of the *remaining* balance (after the partial release),
/// not the original gross. Fixes issue #689.
#[test]
fn tax_release_after_partial_release_does_not_exceed_remaining_balance() {
    let env = test_env();
    env.mock_all_auths();

    let (_, freelancer_addr, _, admin_addr, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 1_000_i128]);
    let token_client = token::Client::new(&env, &token_contract_id);

    // Record tax on the full 1 000 (25 %) → gross 1 000, tax 250, net 750.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.tax_withholding_deductions(&0u32, &2_500u32);

    // Partially release 400 → remaining = 600.
    client.approve_partial(&Address::generate(&env), &0u32, &400_i128);

    let freelancer_before = token_client.balance(&freelancer_addr);
    client.admin_override_tax_release(&admin_addr, &0u32);
    let paid_out = token_client.balance(&freelancer_addr) - freelancer_before;

    // Net of remaining 600 at 25 % = 450; must not be 750 (stale record).
    assert!(paid_out <= 600, "paid_out={paid_out} must be ≤ remaining 600");
    assert!(token_client.balance(&contract_id) >= 1_000);
}

/// withhold → `approve_partial` → `admin_override_tax_split_refund`: neither
/// party should receive more than the remaining balance allows. Fixes issue #689.
#[test]
fn tax_split_refund_after_partial_release_does_not_exceed_remaining_balance() {
    let env = test_env();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 1_000_i128]);
    let token_client = token::Client::new(&env, &token_contract_id);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.tax_withholding_deductions(&0u32, &2_500u32);
    client.approve_partial(&client_addr, &0u32, &400_i128);

    let client_before = token_client.balance(&client_addr);
    let freelancer_before = token_client.balance(&freelancer_addr);
    // 50/50 split
    client.admin_override_tax_split_refund(&admin_addr, &0u32, &5_000u32, &5_000u32);
    let client_got = token_client.balance(&client_addr) - client_before;
    let freelancer_got = token_client.balance(&freelancer_addr) - freelancer_before;

    // Combined payout must not exceed the net of the remaining 600 at 25 % = 450.
    assert!(
        client_got + freelancer_got <= 600,
        "total payout {} must be ≤ remaining 600",
        client_got + freelancer_got
    );
    assert!(token_client.balance(&contract_id) >= 1_000);
}

// ── shared calculation (issue #728) ──────────────────────────────────────────

/// Both entry points report the same numbers for the same milestone and rate,
/// including where round-to-nearest and floor would disagree (1_005 × 50 % =
/// 502.5), and on a partly-paid milestone (remaining basis).
#[test]
fn admin_and_party_tax_withholding_agree() {
    let env = test_env();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_005_i128]);

    let record = client.tax_withholding_deductions(&0u32, &5_000u32);
    assert_eq!(
        client.admin_tax_withholding_deductions(&admin_addr, &0u32, &5_000u32),
        (record.gross_amount, record.tax_amount, record.net_amount)
    );
    assert_eq!(record.tax_amount, 503);

    env.as_contract(&contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0))
            .unwrap();
        milestone.released_amount = 5;
        milestone.status = MilestoneStatus::PartiallyReleased;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0), &milestone);
    });

    let record = client.tax_withholding_deductions(&0u32, &5_000u32);
    assert_eq!(record.gross_amount, 1_000);
    assert_eq!(
        client.admin_tax_withholding_deductions(&admin_addr, &0u32, &5_000u32),
        (record.gross_amount, record.tax_amount, record.net_amount)
    );
}

/// The admin entry point rejects settled milestones, like the party one.
#[test]
fn admin_tax_rejects_settled_milestones() {
    for status in [
        MilestoneStatus::Released,
        MilestoneStatus::Refunded,
        MilestoneStatus::Disputed,
    ] {
        let env = test_env();
        env.mock_all_auths();

        let (_, _, _, admin_addr, _, contract_id, client) =
            setup_funded_escrow(&env, vec![&env, 1_000_i128]);
        env.as_contract(&contract_id, || {
            let mut milestone: Milestone = env
                .storage()
                .persistent()
                .get(&DataKey::Milestone(0))
                .unwrap();
            milestone.status = status.clone();
            env.storage()
                .persistent()
                .set(&DataKey::Milestone(0), &milestone);
        });

        assert_eq!(
            client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &1_000u32),
            Err(Ok(Error::InvalidStatus))
        );
        assert_eq!(
            client.try_tax_withholding_deductions(&0u32, &1_000u32),
            Err(Ok(Error::InvalidStatus))
        );
    }
}
