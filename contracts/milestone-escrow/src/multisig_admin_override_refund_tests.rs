//! Authorization and source-state guards for `multisig_admin_override_refund`
//! (issue #337).
//!
//! An unauthorized caller and an illegal (unlocked) source state must each
//! return their specific typed error, and neither path may mutate storage.

use super::*;
use crate::{Error, MilestoneStatus};
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

    let (client_addr, _, _, admin_addr, token_id, _, client) =
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

    let (client_addr, _, _, admin_addr, token_id, _, client) =
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

    let (client_addr, _, _, admin_addr, token_id, _, client) =
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
}
