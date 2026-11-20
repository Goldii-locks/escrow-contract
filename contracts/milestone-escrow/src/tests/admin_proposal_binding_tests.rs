#![cfg(test)]
//! Contract-assigned admin-transfer proposals (issue #785), the multisig gate
//! on `transfer_admin` (issue #786) and tax settlement to the configured
//! recipient (issue #788).

use super::*;
use crate::{DataKey, Error, Proposal, ProposalKind};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{vec, Address, Env};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// Funded escrow with a 2-of-2 multisig regime: `(admin, signers, client)`.
fn multisig_setup(
    env: &Env,
) -> (
    Address,
    soroban_sdk::Vec<Address>,
    MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();
    let (_, _, _, admin, _, _, client) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    let signers = vec![env, Address::generate(env), Address::generate(env)];
    client.multisig_approval_init(&admin, &signers, &2u32);
    (admin, signers, client)
}

fn proposal(env: &Env, client: &MilestoneEscrowClient<'_>, id: u32) -> Proposal {
    env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(id))
            .unwrap()
    })
}

fn proposal_exists(env: &Env, client: &MilestoneEscrowClient<'_>, id: u32) -> bool {
    env.as_contract(&client.address, || {
        env.storage().persistent().has(&DataKey::Proposal(id))
    })
}

// ── #785: IDs are assigned by the contract and bound to their record ─────────

#[test]
fn proposal_ids_are_sequential_and_never_reused() {
    let env = test_env();
    let (admin, _, client) = multisig_setup(&env);

    let first = client.propose_admin_transfer(&admin, &Address::generate(&env));
    client.cancel_admin_transfer_proposal(&admin);
    let second = client.propose_admin_transfer(&admin, &Address::generate(&env));

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    // #995: the cancelled record is deleted, the new one is live.
    assert!(!proposal_exists(&env, &client, 1));
    assert!(proposal_exists(&env, &client, 2));
    assert!(matches!(
        proposal(&env, &client, 2).kind,
        ProposalKind::AdminTransfer(_)
    ));
}

#[test]
fn approvals_on_a_cancelled_proposal_do_not_carry_over() {
    let env = test_env();
    let (admin, signers, client) = multisig_setup(&env);

    let first = client.propose_admin_transfer(&admin, &Address::generate(&env));
    client.multisig_approve(&signers.get(0).unwrap(), &first);
    client.multisig_approve(&signers.get(1).unwrap(), &first);
    assert!(client.is_multisig_approved(&first).approved);
    client.cancel_admin_transfer_proposal(&admin);

    // The spent proposal can no longer collect approvals ...
    assert_eq!(
        client.try_multisig_approve(&signers.get(0).unwrap(), &first),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
    assert!(!client.is_multisig_approved(&first).approved);

    // ... and a new proposal starts from zero, so it cannot be executed with
    // the old signatures.
    let second = client.propose_admin_transfer(&admin, &Address::generate(&env));
    assert_eq!(client.is_multisig_approved(&second).approvals, 0);
    assert_eq!(
        client.try_execute_admin_transfer(),
        Err(Ok(Error::MultiSigThresholdNotMet))
    );
}

#[test]
fn approving_an_unassigned_id_is_rejected() {
    let env = test_env();
    let (admin, signers, client) = multisig_setup(&env);
    let assigned = client.propose_admin_transfer(&admin, &Address::generate(&env));

    assert_eq!(
        client.try_multisig_approve(&signers.get(0).unwrap(), &(assigned + 1)),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
}

#[test]
fn executed_proposal_is_consumed() {
    let env = test_env();
    let (admin, signers, client) = multisig_setup(&env);
    let new_admin = Address::generate(&env);

    let id = client.propose_admin_transfer(&admin, &new_admin);
    client.multisig_approve(&signers.get(0).unwrap(), &id);
    client.multisig_approve(&signers.get(1).unwrap(), &id);
    client.execute_admin_transfer();

    // #995: the executed record (and its bitmap) is deleted.
    assert!(!proposal_exists(&env, &client, id));
    assert_eq!(client.is_multisig_approved(&id).approvals, 0);
    assert_eq!(
        client.try_multisig_approve(&signers.get(0).unwrap(), &id),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
}

// ── #786: no single-signature transfer once signers are registered ───────────

#[test]
fn transfer_admin_is_closed_under_multisig() {
    let env = test_env();
    let (admin, _, client) = multisig_setup(&env);

    assert_eq!(
        client.try_transfer_admin(&admin, &Address::generate(&env)),
        Err(Ok(Error::Unauthorized))
    );
}

// ── #788: withheld tax is paid to the configured recipient ───────────────────

/// Escrow whose tax recipient is replaced by a known address.
fn tax_setup(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    token::Client<'_>,
    MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();
    let (client_addr, freelancer, _, admin, token_id, _, client) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    let recipient = Address::generate(env);
    client.set_tax_recipient(&admin, &recipient);
    (
        client_addr,
        freelancer,
        admin,
        recipient,
        token::Client::new(env, &token_id),
        client,
    )
}

#[test]
fn tax_release_pays_net_to_freelancer_and_tax_to_recipient() {
    let env = test_env();
    let (_, freelancer, admin, recipient, token, client) = tax_setup(&env);

    client.tax_withholding_deductions(&0u32, &2_500u32);
    client.admin_override_tax_release(&admin, &0u32);

    assert_eq!(token.balance(&freelancer), 750);
    assert_eq!(token.balance(&recipient), 250);
    assert_eq!(token.balance(&client.address), 0);
}

#[test]
fn tax_refund_pays_net_to_client_and_tax_to_recipient() {
    let env = test_env();
    let (client_addr, _, admin, recipient, token, client) = tax_setup(&env);

    client.tax_withholding_deductions(&0u32, &2_500u32);
    client.admin_override_tax_refund(&admin, &0u32);

    assert_eq!(token.balance(&client_addr), 750);
    assert_eq!(token.balance(&recipient), 250);
    assert_eq!(token.balance(&client.address), 0);
}

#[test]
fn tax_split_refund_pays_tax_to_recipient() {
    let env = test_env();
    let (client_addr, freelancer, admin, recipient, token, client) = tax_setup(&env);

    client.tax_withholding_deductions(&0u32, &1_000u32);
    client.admin_override_tax_split_refund(&admin, &0u32, &5_000u32, &5_000u32);

    assert_eq!(token.balance(&client_addr), 450);
    assert_eq!(token.balance(&freelancer), 450);
    assert_eq!(token.balance(&recipient), 100);
    assert_eq!(token.balance(&client.address), 0);
}

#[test]
fn recipient_is_captured_at_withholding_time() {
    let env = test_env();
    let (_, _, admin, recipient, token, client) = tax_setup(&env);

    client.tax_withholding_deductions(&0u32, &2_500u32);
    client.set_tax_recipient(&admin, &Address::generate(&env));
    client.admin_override_tax_release(&admin, &0u32);

    assert_eq!(token.balance(&recipient), 250);
}

#[test]
fn withholding_tax_without_a_recipient_is_rejected() {
    let env = test_env();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    env.as_contract(&contract_id, || {
        env.storage().persistent().remove(&DataKey::TaxRecipient);
    });

    assert_eq!(
        client.try_tax_withholding_deductions(&0u32, &2_500u32),
        Err(Ok(Error::InvalidStatus))
    );
    // A zero-tax record strands nothing, so it needs no recipient.
    assert!(client.try_tax_withholding_deductions(&0u32, &0u32).is_ok());
}
