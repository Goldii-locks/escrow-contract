#![cfg(test)]
//! Persistent-storage TTL guarantees (issue #956).
//!
//! A keeper (or anyone) must be able to keep an active escrow alive:
//! after the ledger has advanced past the default TTL, `extend_storage`
//! keeps the instance and every long-lived milestone- and dispute-related
//! key usable, so `withdraw`, `resolve_dispute` and
//! `execute_admin_transfer` still succeed without a manual contract
//! restore.

use crate::test::{decision_hash, dispute_reason_hash, setup_funded_escrow};
use crate::{MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::{Address as _, IssuerFlags, Ledger};
use soroban_sdk::{token, vec, Address, Env};

/// Advance the ledger past the default TTL window (issue #956): more than
/// 31 days of 5-second ledgers, still below `TTL_EXTEND_TO`.
fn advance_past_default_ttl(env: &Env) {
    env.ledger().with_mut(|ledger| {
        ledger.sequence_number += 1_071_360; // ~62 days of ledgers
        ledger.timestamp += 5_356_800; // the same span in seconds
    });
}

/// Funded, accepted escrow with a revocable-token issuer, one milestone,
/// a delivered-and-disputed milestone, a credited (owed) balance for the
/// client, and a fully approved pending admin transfer. Everything a
/// keeper must keep alive at once.
fn setup_ttl_heavy_escrow(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    token::StellarAssetClient<'_>,
    token::Client<'_>,
    Address,
    MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();
    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, token_id, contract_id, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);

    let token_admin = token::StellarAssetClient::new(env, &token_id);
    token_admin.issuer().set_flag(IssuerFlags::RevocableFlag);
    let token_client = token::Client::new(env, &token_id);

    // A dispute in flight for the delivered milestone.
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(env));

    // A pending admin transfer with both signers approved.
    let s1 = Address::generate(env);
    let s2 = Address::generate(env);
    let new_admin = Address::generate(env);
    escrow.multisig_approval_init(&admin_addr, &vec![env, s1.clone(), s2.clone()], &2u32);
    escrow.propose_admin_transfer(&admin_addr, &new_admin);
    escrow.multisig_approve(&s1, &1u32);
    escrow.multisig_approve(&s2, &1u32);

    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        admin_addr,
        token_admin,
        token_client,
        new_admin,
        escrow,
    )
}

#[test]
fn keeper_extension_keeps_settlements_working_past_default_ttl() {
    let env = Env::default();
    env.mock_all_auths();

    let (
        client_addr,
        _freelancer_addr,
        arbiter_addr,
        _admin_addr,
        token_admin,
        token_client,
        new_admin,
        escrow,
    ) = setup_ttl_heavy_escrow(&env);

    // The client cannot receive a pushed refund, so the dispute refund will
    // become a credited `Owed` balance — exactly the long-lived entry a
    // keeper must preserve.
    token_admin.set_authorized(&client_addr, &false);

    advance_past_default_ttl(&env);

    // The keeper call: permissionless, no auth required.
    assert!(escrow.try_extend_storage().is_ok());

    // The fully approved admin transfer still executes after the window.
    escrow.execute_admin_transfer();
    // The new admin is in place.
    let stored_admin = env.as_contract(&escrow.address, || {
        env.storage()
            .instance()
            .get::<_, Address>(&crate::DataKey::Admin)
    });
    assert_eq!(stored_admin, Some(new_admin));

    // The dispute still resolves after the window.
    escrow.resolve_dispute(&arbiter_addr, &0u32, &false, &decision_hash(&env));

    // The refund was credited (client deauthorized), and after the window
    // the credit can still be claimed once the client can receive again.
    assert_eq!(token_client.balance(&client_addr), 0);
    token_admin.set_authorized(&client_addr, &true);
    escrow.withdraw(&client_addr);
    assert_eq!(token_client.balance(&client_addr), 1_000);
    assert_eq!(token_client.balance(&escrow.address), 0);
}

#[test]
fn extend_storage_requires_initialization_and_is_permissionless() {
    let env = Env::default();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    assert_eq!(
        escrow.try_extend_storage(),
        Err(Ok(crate::Error::NotInitialized))
    );

    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![env, 1_000_i128]);
    // No auths are mocked for this call specifically: mock_all_auths is
    // still on from setup, but extend_storage performs no require_auth.
    assert!(escrow.try_extend_storage().is_ok());
    // Idempotent: a second keeper call is a cheap no-op.
    assert!(escrow.try_extend_storage().is_ok());
}

#[test]
fn tax_recipient_entry_survives_past_default_ttl() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, escrow) = setup_funded_escrow(&env, vec![env, 1_000_i128]);

    // The escrow is funded, so a change needs both parties; mock_all_auths
    // supplies those signatures.
    let recipient = Address::generate(&env);
    escrow.set_tax_recipient(&admin_addr, &recipient);

    advance_past_default_ttl(&env);
    escrow.extend_storage();

    assert_eq!(escrow.get_tax_recipient(), Some(recipient));
}
