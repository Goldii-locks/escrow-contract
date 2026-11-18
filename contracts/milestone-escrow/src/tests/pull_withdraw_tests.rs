#![cfg(test)]
//! Tests for the pull-based fallback behind every settlement payout.
//!
//! A SAC-wrapped classic asset rejects a transfer to an account whose
//! trustline is missing or deauthorized. Settlement must not revert in that
//! case: the payout is credited to the recipient and claimed later through
//! `withdraw`, which only succeeds once the account can receive again.

use crate::test::{decision_hash, dispute_reason_hash};
use crate::{Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::{Address as _, IssuerFlags};
use soroban_sdk::{token, vec, Address, Env};

fn setup_revocable_escrow(
    env: &Env,
) -> (
    Address,
    Address,
    token::StellarAssetClient<'_>,
    token::Client<'_>,
    MilestoneEscrowClient<'_>,
    Address,
) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);

    let sac = env.register_stellar_asset_contract_v2(admin_addr.clone());
    // Deauthorizing a holder requires the issuer to be auth-revocable.
    sac.issuer().set_flag(IssuerFlags::RevocableFlag);
    let token_id = sac.address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);
    let token_client = token::Client::new(env, &token_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &vec![env, 1_000_i128],
        &crate::spec_hashes_for(&vec![env, 1_000_i128]),
    );
    client.accept_job(&freelancer_addr);
    client.accept_arbiter(&arbiter_addr);
    client.fund(&client_addr);

    (
        client_addr,
        freelancer_addr,
        token_admin,
        token_client,
        client,
        contract_id,
    )
}

#[test]
fn test_approve_succeeds_without_trustline_and_withdraw_waits_for_it() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, token_admin, token_client, client, contract_id) =
        setup_revocable_escrow(&env);

    client.mark_delivered(&freelancer_addr, &0u32);

    // The freelancer can no longer receive the asset.
    token_admin.set_authorized(&freelancer_addr, &false);

    // Settlement is not blocked by the recipient's account state.
    client.approve_milestone(&client_addr, &0u32);
    assert_eq!(token_client.balance(&freelancer_addr), 0);
    assert_eq!(token_client.balance(&contract_id), 1_000);

    // Withdrawing fails while the recipient still cannot receive...
    assert!(client.try_withdraw(&freelancer_addr).is_err());
    assert_eq!(token_client.balance(&contract_id), 1_000);

    // ...and succeeds once it can.
    token_admin.set_authorized(&freelancer_addr, &true);
    client.withdraw(&freelancer_addr);
    assert_eq!(token_client.balance(&freelancer_addr), 1_000);
    assert_eq!(token_client.balance(&contract_id), 0);

    // The credit is consumed exactly once.
    assert_eq!(
        client.try_withdraw(&freelancer_addr),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_refund_without_trustline_is_credited_to_client() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, token_admin, token_client, client, contract_id) =
        setup_revocable_escrow(&env);
    let arbiter_addr = client.get_job().arbiter;

    // Disputes need delivered work (#784).
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(&env));
    token_admin.set_authorized(&client_addr, &false);

    client.resolve_dispute(&arbiter_addr, &0u32, &false, &decision_hash(&env));
    assert_eq!(token_client.balance(&client_addr), 0);

    token_admin.set_authorized(&client_addr, &true);
    client.withdraw(&client_addr);
    assert_eq!(token_client.balance(&client_addr), 1_000);
    assert_eq!(token_client.balance(&contract_id), 0);
}

#[test]
fn test_payout_with_trustline_is_pushed_directly() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _token_admin, token_client, client, contract_id) =
        setup_revocable_escrow(&env);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    // Nothing is credited when the push succeeds.
    assert_eq!(token_client.balance(&freelancer_addr), 1_000);
    assert_eq!(token_client.balance(&contract_id), 0);
    assert_eq!(
        client.try_withdraw(&freelancer_addr),
        Err(Ok(Error::InvalidAmount))
    );
}

/// A credited refund stays owed to the client, so `sweep_excess` must not
/// treat it as surplus (#755, #760).
#[test]
fn test_sweep_excess_does_not_take_credited_balances() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, token_admin, token_client, client, contract_id) =
        setup_revocable_escrow(&env);
    let arbiter_addr = client.get_job().arbiter;

    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(&env));
    token_admin.set_authorized(&client_addr, &false);
    client.resolve_dispute(&arbiter_addr, &0u32, &false, &decision_hash(&env));
    token_admin.set_authorized(&client_addr, &true);

    // Everything left in the contract is the client's credited refund.
    assert_eq!(token_client.balance(&contract_id), 1_000);
    assert_eq!(
        client.try_sweep_excess(&client_addr, &token_client.address),
        Err(Ok(Error::NothingToSweep))
    );

    // A genuine surplus on top of the credit is still recoverable.
    token_admin.mint(&contract_id, &50);
    assert_eq!(client.sweep_excess(&client_addr, &token_client.address), 50);
    assert_eq!(token_client.balance(&contract_id), 1_000);
}
