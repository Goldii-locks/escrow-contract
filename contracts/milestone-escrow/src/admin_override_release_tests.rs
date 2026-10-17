//! Authorization and source-state guards for `admin_override_release`
//! (issue #328).
//!
//! An unauthorized caller and an illegal (terminal) source state must each
//! return their specific typed error, and neither path may mutate storage.

use super::*;
use crate::{Error, MilestoneStatus};
use soroban_sdk::token;
use soroban_sdk::{symbol_short, vec, Address, Env, IntoVal, Val};

fn release_event_count(env: &Env) -> u32 {
    let topic_val: Val = symbol_short!("admovrls").into_val(env);
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

struct ReleaseLedgerSnapshot {
    status: MilestoneStatus,
    released_amount: i128,
    freelancer_balance: i128,
    contract_balance: i128,
}

fn snapshot_release_ledger(
    env: &Env,
    client: &MilestoneEscrowClient,
    token_id: &Address,
    freelancer_addr: &Address,
) -> ReleaseLedgerSnapshot {
    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    let token = token::Client::new(env, token_id);
    ReleaseLedgerSnapshot {
        status: milestone.status,
        released_amount: milestone.released_amount,
        freelancer_balance: token.balance(freelancer_addr),
        contract_balance: token.balance(&client.address),
    }
}

fn assert_ledger_unchanged(before: &ReleaseLedgerSnapshot, after: &ReleaseLedgerSnapshot) {
    assert_eq!(after.status, before.status);
    assert_eq!(after.released_amount, before.released_amount);
    assert_eq!(after.freelancer_balance, before.freelancer_balance);
    assert_eq!(after.contract_balance, before.contract_balance);
}

#[test]
fn unauthorized_caller_returns_unauthorized_and_does_not_mutate_storage() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
    assert_eq!(before.status, MilestoneStatus::Pending);
    assert_eq!(before.released_amount, 0);

    let attacker = Address::generate(&env);
    let result = client.try_admin_override_release(&attacker, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let after = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
    assert_ledger_unchanged(&before, &after);
    assert_eq!(release_event_count(&env), 0);
}

#[test]
fn illegal_terminal_source_state_returns_invalid_status_and_does_not_mutate_storage() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Fully release the milestone first so the source state is terminal.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let before = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
    assert_eq!(before.status, MilestoneStatus::Released);
    assert_eq!(before.released_amount, 1_000);

    let result = client.try_admin_override_release(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    let after = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
    assert_ledger_unchanged(&before, &after);
    assert_eq!(release_event_count(&env), 0);
}

#[test]
fn client_freelancer_and_arbiter_are_unauthorized_without_storage_mutation() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
    for caller in [client_addr.clone(), freelancer_addr.clone(), arbiter_addr] {
        let result = client.try_admin_override_release(&caller, &0u32);
        assert_eq!(result, Err(Ok(Error::Unauthorized)));
        let after = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
        assert_ledger_unchanged(&before, &after);
    }
    assert_eq!(release_event_count(&env), 0);
}

#[test]
fn admin_override_release_releases_funds_to_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let before = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
    client.admin_override_release(&admin_addr, &0u32);

    let after = snapshot_release_ledger(&env, &client, &token_id, &freelancer_addr);
    assert_eq!(after.status, MilestoneStatus::Released);
    assert_eq!(after.released_amount, 1_000);
    assert_eq!(after.freelancer_balance, before.freelancer_balance + 1_000);
    assert_eq!(after.contract_balance, before.contract_balance - 1_000);
    assert_eq!(release_event_count(&env), 1);
}
