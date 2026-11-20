#![cfg(test)]
//! #674: the configured treasury share is deducted from freelancer payouts,
//! while refunds stay fee-free.

use crate::testutils::EscrowFixture;
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::Address;

fn configure_treasury_fee(
    env: &soroban_sdk::Env,
    fx: &crate::testutils::Escrow<'_>,
    treasury: &Address,
) {
    // 1% to the treasury, none to the client.
    fx.escrow
        .mock_all_auths()
        .set_platform_fee_allocation(&fx.admin, &0u32, &9_900u32, &100u32);
    fx.escrow
        .mock_all_auths()
        .propose_treasury_update(&fx.admin, treasury);
    env.ledger()
        .with_mut(|l| l.timestamp += 7 * 24 * 60 * 60 + 1);
    fx.escrow.mock_all_auths().apply_treasury_update(&fx.admin);
}

#[test]
fn approve_milestone_sends_treasury_share_to_treasury() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env)
        .milestones(&[10_000])
        .funded()
        .build();
    let treasury = Address::generate(&env);
    configure_treasury_fee(&env, &fx, &treasury);

    fx.escrow
        .mock_all_auths()
        .mark_delivered(&fx.freelancer, &0);
    fx.escrow.mock_all_auths().approve_milestone(&fx.client, &0);

    let token = fx.token_client();
    assert_eq!(token.balance(&fx.freelancer), 9_900);
    assert_eq!(token.balance(&treasury), 100);
    assert_eq!(token.balance(&fx.contract_id), 0);
}

#[test]
fn zero_treasury_bps_leaves_full_payout_unchanged() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env)
        .milestones(&[10_000])
        .funded()
        .build();
    fx.escrow
        .mock_all_auths()
        .set_platform_fee_allocation(&fx.admin, &0u32, &10_000u32, &0u32);
    fx.escrow
        .mock_all_auths()
        .mark_delivered(&fx.freelancer, &0);
    fx.escrow.mock_all_auths().approve_milestone(&fx.client, &0);

    let token = fx.token_client();
    assert_eq!(token.balance(&fx.freelancer), 10_000);
}

#[test]
fn refunds_take_no_treasury_fee() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env)
        .milestones(&[10_000])
        .funded()
        .build();
    let treasury = Address::generate(&env);
    configure_treasury_fee(&env, &fx, &treasury);

    fx.escrow
        .mock_all_auths()
        .mark_delivered(&fx.freelancer, &0);
    fx.escrow.mock_all_auths().raise_dispute(
        &fx.client,
        &0,
        &crate::test::dispute_reason_hash(&env),
    );
    fx.escrow
        .mock_all_auths()
        .resolve_dispute(&fx.arbiter, &0, &false);

    let token = fx.token_client();
    assert_eq!(token.balance(&fx.client), 10_000);
    assert_eq!(token.balance(&treasury), 0);
}
