//! Authorization and source-state guards for `multisig_admin_override_refund`
//! (issue #337).
//!
//! An unauthorized caller and an illegal (unlocked) source state must each
//! return their specific typed error, and neither path may mutate storage.

use super::*;
use crate::{DataKey, Error, MilestoneStatus};
use soroban_sdk::token;
use soroban_sdk::{symbol_short, vec, Address, Env, IntoVal, Val};

fn refund_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("msadmref").into_val(env);
    let mut count = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                count += 1;
            }
        }
    }
    count
}

struct RefundLedgerSnapshot {
    locked: bool,
    status: MilestoneStatus,
    released_amount: i128,
    client_balance: i128,
    contract_balance: i128,
}

fn snapshot_refund_ledger(
    env: &Env,
    client: &MilestoneEscrowClient,
    token_id: &Address,
    client_addr: &Address,
) -> RefundLedgerSnapshot {
    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    let token = token::Client::new(env, token_id);
    RefundLedgerSnapshot {
        locked: client.is_multisig_locked(),
        status: milestone.status,
        released_amount: milestone.released_amount,
        client_balance: token.balance(client_addr),
        contract_balance: token.balance(&client.address),
    }
}

fn assert_ledger_unchanged(before: &RefundLedgerSnapshot, after: &RefundLedgerSnapshot) {
    assert_eq!(after.locked, before.locked);
    assert_eq!(after.status, before.status);
    assert_eq!(after.released_amount, before.released_amount);
    assert_eq!(after.client_balance, before.client_balance);
    assert_eq!(after.contract_balance, before.contract_balance);
}

#[test]
fn unauthorized_caller_returns_unauthorized_and_does_not_mutate_storage() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, _contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    // Lock first so a skipped auth check would otherwise proceed to write.
    client.multisig_lock(&admin_addr);

    let before = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
    assert!(before.locked);
    assert_eq!(before.status, MilestoneStatus::Pending);
    assert_eq!(before.released_amount, 0);

    let attacker = Address::generate(&env);
    let result = client.try_multisig_admin_override_refund(&attacker, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let after = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
    assert_ledger_unchanged(&before, &after);
    assert_eq!(refund_event_count(&env), 0);
}

#[test]
fn illegal_unlocked_source_state_returns_invalid_status_and_does_not_mutate_storage() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, _contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
    assert!(!before.locked);
    assert_eq!(before.status, MilestoneStatus::Pending);
    assert_eq!(before.released_amount, 0);

    let result = client.try_multisig_admin_override_refund(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    let after = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
    assert_ledger_unchanged(&before, &after);
    assert!(!after.locked);
    assert_eq!(refund_event_count(&env), 0);
}

#[test]
fn client_freelancer_and_arbiter_are_unauthorized_without_storage_mutation() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let before = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
    for caller in [client_addr.clone(), freelancer_addr, arbiter_addr] {
        let result = client.try_multisig_admin_override_refund(&caller, &0u32);
        assert_eq!(result, Err(Ok(Error::Unauthorized)));
        let after = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
        assert_ledger_unchanged(&before, &after);
    }
    assert_eq!(refund_event_count(&env), 0);
}

#[test]
fn locked_admin_override_refunds_client_and_clears_lock() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let before = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
    client.multisig_admin_override_refund(&admin_addr, &0u32);

    let after = snapshot_refund_ledger(&env, &client, &token_id, &client_addr);
    assert!(!after.locked);
    assert_eq!(after.status, MilestoneStatus::Refunded);
    assert_eq!(after.released_amount, 1_000);
    assert_eq!(after.client_balance, before.client_balance + 1_000);
    assert_eq!(after.contract_balance, before.contract_balance - 1_000);

    let lock: Option<bool> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::MultisigLocked)
    });
    assert_eq!(lock, None);
}

// ── arithmetic hardening (issue #395) ────────────────────────────────────────

/// Overwrite the persistent `Milestone(index)` entry directly with an
/// adversarial value. `amount` / `released_amount` are signed i128 values that
/// the normal flow would never produce, so they are injected straight into
/// storage to prove the checked arithmetic returns a typed error instead of
/// panicking or wrapping.
fn set_milestone_raw(env: &Env, contract_id: &Address, index: u32, milestone: &Milestone) {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(index), milestone);
    });
}

fn unlocked_milestone(amount: i128, released_amount: i128) -> Milestone {
    Milestone {
        amount,
        released_amount,
        status: MilestoneStatus::Pending,
        delivered_at: 0,
    }
}

/// A milestone whose `amount` is `i128::MIN` must be rejected with
/// `Error::InvalidAmount` before any arithmetic runs — it must not panic or
/// wrap.
#[test]
fn refund_negative_amount_returns_invalid_amount_without_panic() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _, _, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let milestone = unlocked_milestone(i128::MIN, 0);
    set_milestone_raw(&env, &contract_id, 0, &milestone);

    let result = client.try_multisig_admin_override_refund(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));

    assert!(client.is_multisig_locked());
    assert_eq!(refund_event_count(&env), 0);
}

/// A negative `released_amount` (e.g. `i128::MIN`) is a pathological operand:
/// subtracting it could overflow `i128::MAX`. It must be rejected with
/// `Error::InvalidAmount` rather than panic.
#[test]
fn refund_negative_released_amount_returns_invalid_amount_without_panic() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _, _, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let milestone = unlocked_milestone(i128::MAX, i128::MIN);
    set_milestone_raw(&env, &contract_id, 0, &milestone);

    let result = client.try_multisig_admin_override_refund(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));

    assert!(client.is_multisig_locked());
    assert_eq!(refund_event_count(&env), 0);
}

/// `amount == i128::MAX` with a positive `released_amount` would previously
/// risk wrapping; the checked_sub + `remaining <= 0` guards must yield
/// `Error::InvalidAmount` for any over-full (or equal) released_amount.
#[test]
fn refund_released_exceeds_amount_returns_invalid_amount_without_panic() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _, _, admin_addr, _token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    let milestone = unlocked_milestone(i128::MAX, i128::MAX);
    set_milestone_raw(&env, &contract_id, 0, &milestone);

    // released_amount == amount → remaining == 0 → InvalidAmount.
    let result = client.try_multisig_admin_override_refund(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert!(client.is_multisig_locked());

    // released_amount > amount → remaining < 0 → InvalidAmount (no wrap).
    let milestone2 = unlocked_milestone(100, 200);
    set_milestone_raw(&env, &contract_id, 0, &milestone2);
    let result = client.try_multisig_admin_override_refund(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert!(client.is_multisig_locked());

    assert_eq!(refund_event_count(&env), 0);
}

/// Valid amounts must produce results identical to before the hardening.
/// A partially-released milestone refunds exactly `amount - released_amount`.
#[test]
fn refund_valid_amount_equals_amount_minus_released_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.multisig_lock(&admin_addr);

    // Inject an already partially-released milestone (released 400 of 1000).
    // Remaining refund == 1000 - 400 == 600.
    let milestone = unlocked_milestone(1_000, 400);
    set_milestone_raw(&env, &contract_id, 0, &milestone);

    let token = token::Client::new(&env, &token_id);
    let client_before = token.balance(&client_addr);

    client.multisig_admin_override_refund(&admin_addr, &0u32);

    // Read the event tally first: every later `client.*` / `token.*` call is
    // itself a contract invocation, and the test env's event buffer reflects
    // the most recent one.
    assert_eq!(refund_event_count(&env), 1);

    assert!(!client.is_multisig_locked());
    let job = client.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::Refunded);
    assert_eq!(ms.released_amount, 1_000);
    assert_eq!(token.balance(&client_addr), client_before + 600);
}
