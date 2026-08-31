//! Tests for `admin_override_cancel_release` (issue #383) and
//! `admin_override_cancel_refund` (issue #386).
//!
//! Issue #383 - Reduce ledger storage footprint of admin_override_cancel_release
//! The MilestoneReleased(u32) temporary key is no longer written inside
//! admin_override_cancel_release. These tests verify:
//!   - All existing happy-path behaviour (tokens transferred, milestone status
//!     updated, CancelLock cleared) is unchanged.
//!   - The temporary MilestoneReleased flag is NOT set after the call,
//!     confirming the redundant write was removed.
//!
//! Issue #386 - Replace unchecked arithmetic in admin_override_cancel_refund
//! All arithmetic now uses checked operations and explicit non-negativity
//! guards. These tests verify:
//!   - Valid amounts produce results identical to the previous behaviour.
//!   - Error::InvalidAmount is returned (not a panic) for edge-case amounts.
//!   - Terminal milestones are correctly skipped in every scenario.

use super::*;
use crate::{DataKey, Error, MilestoneEscrowClient, MilestoneStatus};
use soroban_sdk::{token, vec, Address, Env};

// ────────────────────────────────────────────────────────────────────────────
// Issue #383: admin_override_cancel_release storage footprint
// ────────────────────────────────────────────────────────────────────────────

/// Happy path: cancel-locked escrow with two pending milestones is fully
/// released to the freelancer; persistent milestone status is Released and
/// the temporary MilestoneReleased flag is absent (issue #383 optimisation).
#[test]
fn test_cancel_release_happy_path_no_temporary_released_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 3_000_i128, 7_000_i128]);

    client.cancel_escrow(&freelancer_addr);

    let token = token::Client::new(&env, &token_id);
    let freelancer_before = token.balance(&freelancer_addr);
    let contract_before = token.balance(&client.address);

    client.admin_override_cancel_release(&admin_addr);

    // token balances
    assert_eq!(token.balance(&freelancer_addr), freelancer_before + 10_000);
    assert_eq!(token.balance(&client.address), contract_before - 10_000);

    // milestone persistent state
    let job = client.get_job();
    for idx in 0..2u32 {
        let ms = job.milestones.get(idx).unwrap();
        assert_eq!(ms.status, MilestoneStatus::Released);
        assert_eq!(ms.released_amount, ms.amount);
    }

    // CancelLock cleared
    let still_locked: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::CancelLock)
            .unwrap_or(false)
    });
    assert!(!still_locked);

    // MilestoneReleased temporary flag must NOT be set (issue #383)
    for idx in 0..2u32 {
        let flag: Option<bool> = env.as_contract(&contract_id, || {
            env.storage()
                .temporary()
                .get(&DataKey::MilestoneReleased(idx))
        });
        assert_eq!(flag, None);
    }
}

/// Terminal milestones (already Released) are skipped; only pending ones count
/// toward the total and no temporary flag is written for any of them.
#[test]
fn test_cancel_release_skips_terminal_milestones_no_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 2_000_i128, 3_000_i128, 5_000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, amounts);

    // Approve milestone 0 via normal path so it becomes Released.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    client.cancel_escrow(&client_addr);

    let token = token::Client::new(&env, &token_id);
    let freelancer_before = token.balance(&freelancer_addr);

    client.admin_override_cancel_release(&admin_addr);

    // Only milestones 1 and 2 (8_000 total) transferred.
    assert_eq!(token.balance(&freelancer_addr), freelancer_before + 8_000);

    // Milestones 1 and 2 must not have the temporary released flag.
    for idx in 1..3u32 {
        let flag: Option<bool> = env.as_contract(&contract_id, || {
            env.storage()
                .temporary()
                .get(&DataKey::MilestoneReleased(idx))
        });
        assert_eq!(flag, None);
    }
}

/// admin_override_cancel_release must fail with InvalidStatus when no cancel
/// lock is active.
#[test]
fn test_cancel_release_requires_cancel_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let result = client.try_admin_override_cancel_release(&admin_addr);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// admin_override_cancel_release must fail with Unauthorized for non-admin.
#[test]
fn test_cancel_release_unauthorized_caller_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.cancel_escrow(&client_addr);

    let attacker = Address::generate(&env);
    let result = client.try_admin_override_cancel_release(&attacker);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// YieldAccrued is reset to zero after a successful release.
#[test]
fn test_cancel_release_resets_yield_accrued() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 5_000_i128]);
    client.cancel_escrow(&client_addr);
    client.admin_override_cancel_release(&admin_addr);

    let yield_after: i128 = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0_i128)
    });
    assert_eq!(yield_after, 0);
}

// ────────────────────────────────────────────────────────────────────────────
// Issue #386: admin_override_cancel_refund checked arithmetic
// ────────────────────────────────────────────────────────────────────────────

/// Happy path: cancel-locked escrow is fully refunded to the client with
/// correct token amounts and milestone state.
#[test]
fn test_cancel_refund_happy_path_produces_correct_amounts() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 4_000_i128, 6_000_i128]);

    client.cancel_escrow(&client_addr);

    let token = token::Client::new(&env, &token_id);
    let client_before = token.balance(&client_addr);
    let contract_before = token.balance(&client.address);

    client.admin_override_cancel_refund(&admin_addr);

    assert_eq!(token.balance(&client_addr), client_before + 10_000);
    assert_eq!(token.balance(&client.address), contract_before - 10_000);

    let job = client.get_job();
    for idx in 0..2u32 {
        let ms = job.milestones.get(idx).unwrap();
        assert_eq!(ms.status, MilestoneStatus::Refunded);
        assert_eq!(ms.released_amount, ms.amount);
    }
}

/// Terminal milestones are skipped; only pending ones contribute to the refund.
#[test]
fn test_cancel_refund_skips_terminal_milestones() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 1_000_i128, 4_000_i128, 5_000_i128];
    let (client_addr, freelancer_addr, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, amounts);

    // Release milestone 0 normally.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    client.cancel_escrow(&client_addr);

    let token = token::Client::new(&env, &token_id);
    let client_before = token.balance(&client_addr);

    client.admin_override_cancel_refund(&admin_addr);

    // Only milestones 1 and 2 refunded.
    assert_eq!(token.balance(&client_addr), client_before + 9_000);
}

/// admin_override_cancel_refund must fail with InvalidStatus when no cancel
/// lock is active (no wrap or panic must occur).
#[test]
fn test_cancel_refund_requires_cancel_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let result = client.try_admin_override_cancel_refund(&admin_addr);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// admin_override_cancel_refund must fail with Unauthorized for non-admin;
/// no storage must be mutated.
#[test]
fn test_cancel_refund_unauthorized_caller_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.cancel_escrow(&client_addr);

    let token = token::Client::new(&env, &token_id);
    let client_balance_before = token.balance(&client_addr);
    let contract_balance_before = token.balance(&client.address);

    let attacker = Address::generate(&env);
    let result = client.try_admin_override_cancel_refund(&attacker);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    assert_eq!(token.balance(&client_addr), client_balance_before);
    assert_eq!(token.balance(&client.address), contract_balance_before);
}

/// If every milestone is already terminal the total refund is zero and the
/// function returns Error::InvalidAmount rather than panicking. This validates
/// the checked arithmetic boundary (issue #386).
#[test]
fn test_cancel_refund_all_terminal_returns_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 2_000_i128]);

    // Fully release the single milestone via normal path.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    client.cancel_escrow(&client_addr);

    let result = client.try_admin_override_cancel_refund(&admin_addr);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// YieldAccrued is reset to zero after a successful refund.
#[test]
fn test_cancel_refund_resets_yield_accrued() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 5_000_i128]);
    client.cancel_escrow(&client_addr);
    client.admin_override_cancel_refund(&admin_addr);

    let yield_after: i128 = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0_i128)
    });
    assert_eq!(yield_after, 0);
}

/// CancelLock is cleared after a successful refund.
#[test]
fn test_cancel_refund_clears_cancel_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.cancel_escrow(&client_addr);
    client.admin_override_cancel_refund(&admin_addr);

    let locked: bool = env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::CancelLock)
            .unwrap_or(false)
    });
    assert!(!locked);
}

/// Multiple pending milestones accumulate through checked_add correctly;
/// the sum equals the total of all milestone amounts (issue #386).
#[test]
fn test_cancel_refund_multiple_milestones_sum_correctly() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 100_i128, 200_i128, 300_i128, 400_i128];
    let (client_addr, _, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, amounts);

    client.cancel_escrow(&client_addr);

    let token = token::Client::new(&env, &token_id);
    let client_before = token.balance(&client_addr);

    client.admin_override_cancel_refund(&admin_addr);

    assert_eq!(token.balance(&client_addr), client_before + 1_000);
}

/// Single-milestone refund with minimum valid amount (1 stroop) succeeds
/// without any arithmetic error; confirms checked path handles small positive
/// values correctly (issue #386).
#[test]
fn test_cancel_refund_minimum_valid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_i128]);

    client.cancel_escrow(&client_addr);

    let token = token::Client::new(&env, &token_id);
    let client_before = token.balance(&client_addr);

    client.admin_override_cancel_refund(&admin_addr);

    assert_eq!(token.balance(&client_addr), client_before + 1);
}

