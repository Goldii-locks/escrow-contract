//! Branch coverage for the dispute resolution paths (issue #751):
//! `resolve_dispute`, `apply_dispute_arbitration_split`, and the
//! `DisputeAlreadyRaised` guard in `raise_dispute`.
//!
//! The shortfall tests move tokens out of the escrow before resolving so the
//! contract balance is below the milestone's remaining amount, and pin the
//! current capping behaviour.

use super::*;
use crate::{DataKey, Error, Milestone, MilestoneStatus};
use soroban_sdk::{Address, Env};

const ZERO_ACCOUNT: &str = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
const ZERO_CONTRACT: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

/// Fund a single-milestone escrow and raise a dispute on milestone 0.
/// Returns `(client_addr, freelancer_addr, arbiter_addr, token, contract_id, client)`.
fn disputed_escrow(
    env: &Env,
    amount: i128,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let (client_addr, freelancer_addr, arbiter_addr, _admin, token, contract_id, client) =
        setup_funded_escrow(env, vec![env, amount]);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(env));
    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        token,
        contract_id,
        client,
    )
}

/// Move `amount` out of the escrow so its balance falls below what is owed.
fn drain(env: &Env, token: &Address, contract_id: &Address, amount: i128) {
    token::Client::new(env, token).transfer(contract_id, Address::generate(env), &amount);
}

/// Mark milestone 0 as fully released while leaving it `Disputed`, so the
/// remaining balance is zero.
fn zero_remaining(env: &Env, contract_id: &Address) {
    env.as_contract(contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0u32))
            .unwrap();
        milestone.released_amount = milestone.amount;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0u32), &milestone);
    });
}

fn milestone_0(client: &MilestoneEscrowClient<'_>) -> Milestone {
    client.get_job().milestones.get(0).unwrap()
}

// ── raise_dispute ───────────────────────────────────────────────────────────

// ── resolve_dispute ─────────────────────────────────────────────────────────

#[test]
fn resolve_dispute_rejects_zero_remaining() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, arbiter_addr, _, contract_id, client) = disputed_escrow(&env, 1_000);
    zero_remaining(&env, &contract_id);

    let result = client.try_resolve_dispute(&arbiter_addr, &0u32, &true, &decision_hash(&env));
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

#[test]
fn resolve_dispute_rejects_zero_contract_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, arbiter_addr, token, contract_id, client) = disputed_escrow(&env, 1_000);
    drain(&env, &token, &contract_id, 1_000);

    let result = client.try_resolve_dispute(&arbiter_addr, &0u32, &false, &decision_hash(&env));
    assert_eq!(result, Err(Ok(Error::InsufficientBalance)));
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

/// A balance shortfall is rejected rather than paid out partially (#689):
/// nothing moves and the milestone stays disputed.
#[test]
fn resolve_dispute_shortfall_release_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, token, contract_id, client) =
        disputed_escrow(&env, 1_000);
    drain(&env, &token, &contract_id, 700);

    assert_eq!(
        client.try_resolve_dispute(&arbiter_addr, &0u32, &true, &decision_hash(&env)),
        Err(Ok(Error::InsufficientBalance))
    );

    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&freelancer_addr), 0);
    assert_eq!(token_client.balance(&client_addr), 0);
    assert_eq!(token_client.balance(&contract_id), 300);
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

/// A balance shortfall is rejected rather than paid out partially (#689):
/// nothing moves and the milestone stays disputed.
#[test]
fn resolve_dispute_shortfall_refund_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, token, contract_id, client) =
        disputed_escrow(&env, 1_000);
    drain(&env, &token, &contract_id, 700);

    assert_eq!(
        client.try_resolve_dispute(&arbiter_addr, &0u32, &false, &decision_hash(&env)),
        Err(Ok(Error::InsufficientBalance))
    );

    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&freelancer_addr), 0);
    assert_eq!(token_client.balance(&client_addr), 0);
    assert_eq!(token_client.balance(&contract_id), 300);
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

// ── apply_dispute_arbitration_split ─────────────────────────────────────────

#[test]
fn split_rejects_zero_and_self_arbiter_addresses() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, contract_id, client) = disputed_escrow(&env, 1_000);

    for bad in [
        Address::from_str(&env, ZERO_ACCOUNT),
        Address::from_str(&env, ZERO_CONTRACT),
        contract_id.clone(),
    ] {
        let result = client.try_apply_dispute_arbitration_split(
            &bad,
            &0u32,
            &5_000u32,
            &decision_hash(&env),
        );
        assert_eq!(result, Err(Ok(Error::InvalidAddress)));
    }
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

#[test]
fn split_rejects_unfunded_escrow() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token,
        &604800,
        &vec![&env, 1_000_i128],
        &crate::test_spec_hashes(&env, &vec![&env, 1_000_i128]),
    );

    let result = client.try_apply_dispute_arbitration_split(
        &arbiter_addr,
        &0u32,
        &5_000u32,
        &decision_hash(&env),
    );
    assert_eq!(result, Err(Ok(Error::NotFunded)));
}

#[test]
fn split_rejects_non_disputed_milestone() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, arbiter_addr, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let result = client.try_apply_dispute_arbitration_split(
        &arbiter_addr,
        &0u32,
        &5_000u32,
        &decision_hash(&env),
    );
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Pending);
}

#[test]
fn split_rejects_zero_remaining() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, arbiter_addr, _, contract_id, client) = disputed_escrow(&env, 1_000);
    zero_remaining(&env, &contract_id);

    let result = client.try_apply_dispute_arbitration_split(
        &arbiter_addr,
        &0u32,
        &5_000u32,
        &decision_hash(&env),
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

#[test]
fn split_rejects_zero_contract_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, arbiter_addr, token, contract_id, client) = disputed_escrow(&env, 1_000);
    drain(&env, &token, &contract_id, 1_000);

    let result = client.try_apply_dispute_arbitration_split(
        &arbiter_addr,
        &0u32,
        &5_000u32,
        &decision_hash(&env),
    );
    assert_eq!(result, Err(Ok(Error::InsufficientBalance)));
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

/// A balance shortfall is rejected rather than split partially (#689):
/// nothing moves and the milestone stays disputed.
#[test]
fn split_shortfall_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, token, contract_id, client) =
        disputed_escrow(&env, 10_000);
    drain(&env, &token, &contract_id, 2000);

    assert_eq!(
        client.try_apply_dispute_arbitration_split(
            &arbiter_addr,
            &0u32,
            &7_000u32,
            &decision_hash(&env)
        ),
        Err(Ok(Error::InsufficientBalance))
    );

    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&client_addr), 0);
    assert_eq!(token_client.balance(&freelancer_addr), 0);
    assert_eq!(token_client.balance(&contract_id), 8000);
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}

/// A balance shortfall is rejected rather than split partially (#689):
/// nothing moves and the milestone stays disputed.
#[test]
fn split_shortfall_below_client_share_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, token, contract_id, client) =
        disputed_escrow(&env, 10_000);
    drain(&env, &token, &contract_id, 4000);

    assert_eq!(
        client.try_apply_dispute_arbitration_split(
            &arbiter_addr,
            &0u32,
            &7_000u32,
            &decision_hash(&env)
        ),
        Err(Ok(Error::InsufficientBalance))
    );

    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&client_addr), 0);
    assert_eq!(token_client.balance(&freelancer_addr), 0);
    assert_eq!(token_client.balance(&contract_id), 6000);
    assert_eq!(milestone_0(&client).status, MilestoneStatus::Disputed);
}
