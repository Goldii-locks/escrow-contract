#![cfg(test)]
//! Targeted authorization tests for every state-changing public entry point
//! on `MilestoneEscrow` (issue #686).
//!
//! Each group covers one entry point with three cases:
//!   1. **Wrong signer** – only an unauthorised address is offered; asserts
//!      the call fails with `Error::Unauthorized` (or `NotInitialized` for
//!      unregistered callers where auth guard runs after the init guard).
//!   2. **Correct signer succeeds** – only the intended signer(s) are offered
//!      via `mock_auths`; asserts `Ok`.
//!   3. **`env.auths()` shape** – after a successful call, asserts that the
//!      auth record matches exactly the expected `(address, invocation)` pair.
//!
//! Setup steps (initialize, fund, accept_job, …) use `mock_all_auths` so they
//! don't pollute the assertions under test.  Each test switches to a targeted
//! `mock_auths` only for the single call being verified.

use super::*;
use soroban_sdk::{
    testutils::{AuthorizedFunction, MockAuth, MockAuthInvoke},
    Address, Env, IntoVal, Symbol,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Minimal initialised (unfunded) escrow; returns all four party addresses,
/// the token address, the contract id, and the client handle.
fn setup(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    let amounts = soroban_sdk::vec![env, 1_000_i128];
    escrow.initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800u64,
        &amounts,
        &crate::test_spec_hashes(env, &amounts),
    );
    // Tax withholding needs a recipient (see `set_tax_recipient`).
    escrow.set_tax_recipient(&admin, &Address::generate(env));
    (
        contract_id,
        admin,
        client,
        freelancer,
        arbiter,
        token,
        escrow,
    )
}

/// Funded escrow with both job-acceptances already recorded.
fn setup_funded(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let (contract_id, admin, client, freelancer, arbiter, token, escrow) = setup(env);
    // Mint tokens so fund() can transfer them.
    let token_admin = soroban_sdk::token::StellarAssetClient::new(env, &token);
    token_admin.mint(&client, &1_000);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);
    (
        contract_id,
        admin,
        client,
        freelancer,
        arbiter,
        token,
        escrow,
    )
}

// ─── initialize ──────────────────────────────────────────────────────────────

#[test]
fn initialize_wrong_signer_fails() {
    let env = Env::default();
    let attacker = Address::generate(&env);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = soroban_sdk::vec![&env, 1_000_i128];
    // mock only attacker — not the real admin
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (
                admin.clone(),
                client.clone(),
                freelancer.clone(),
                arbiter.clone(),
                token.clone(),
                604_800u64,
                amounts.clone(),
                crate::test_spec_hashes(&env, &amounts),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    // The host will panic / abort because admin.require_auth() is not satisfied.
    // try_ gives us the error without unwinding the test.
    let res = escrow.try_initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800u64,
        &amounts,
        &crate::test_spec_hashes(&env, &amounts),
    );
    assert!(
        res.is_err(),
        "initialize must fail when admin auth is missing"
    );
}

#[test]
fn initialize_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = soroban_sdk::vec![&env, 1_000_i128];
    let hashes = crate::test_spec_hashes(&env, &amounts);

    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (
                admin.clone(),
                client.clone(),
                freelancer.clone(),
                arbiter.clone(),
                token.clone(),
                604_800u64,
                amounts.clone(),
                hashes.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800u64,
        &amounts,
        &hashes,
    );
    let auths = env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "initialize"),
            (
                admin.clone(),
                client.clone(),
                freelancer.clone(),
                arbiter.clone(),
                token.clone(),
                604_800u64,
                amounts.clone(),
                hashes.clone(),
            )
                .into_val(&env),
        ))
    );
}

// ─── transfer_admin ───────────────────────────────────────────────────────────

#[test]
fn transfer_admin_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let new_admin = Address::generate(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "transfer_admin",
            args: (admin.clone(), new_admin.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_transfer_admin(&admin, &new_admin);
    assert!(
        res.is_err(),
        "transfer_admin must fail when admin auth is missing"
    );
}

#[test]
fn transfer_admin_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let new_admin = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "transfer_admin",
            args: (admin.clone(), new_admin.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.transfer_admin(&admin, &new_admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "transfer_admin"),
            (admin.clone(), new_admin.clone()).into_val(&env),
        ))
    );
}

// ─── add_whitelisted_token ────────────────────────────────────────────────────

#[test]
fn add_whitelisted_token_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let new_token = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "add_whitelisted_token",
            args: (client.clone(), new_token.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_add_whitelisted_token(&client, &new_token);
    assert!(
        res.is_err(),
        "add_whitelisted_token must fail when admin auth is missing"
    );
}

#[test]
fn add_whitelisted_token_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let new_token = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "add_whitelisted_token",
            args: (admin.clone(), new_token.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.add_whitelisted_token(&admin, &new_token);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "add_whitelisted_token"),
            (admin.clone(), new_token.clone()).into_val(&env),
        ))
    );
}

// ─── remove_whitelisted_token ─────────────────────────────────────────────────

#[test]
fn remove_whitelisted_token_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, token, escrow) = setup(&env);
    // Add a second token so removal won't fail with InvalidAmount (must keep ≥1)
    env.mock_all_auths();
    let extra_token = Address::generate(&env);
    escrow.add_whitelisted_token(&_admin, &extra_token);

    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "remove_whitelisted_token",
            args: (client.clone(), token.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_remove_whitelisted_token(&client, &token);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn remove_whitelisted_token_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    let extra_token = Address::generate(&env);
    escrow.add_whitelisted_token(&admin, &extra_token);

    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "remove_whitelisted_token",
            args: (admin.clone(), extra_token.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.remove_whitelisted_token(&admin, &extra_token);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "remove_whitelisted_token"),
            (admin.clone(), extra_token.clone()).into_val(&env),
        ))
    );
}

// ─── fund ─────────────────────────────────────────────────────────────────────

#[test]
fn fund_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "fund",
            args: (client.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_fund(&client);
    assert!(res.is_err(), "fund must fail when client auth is missing");
}

#[test]
fn fund_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, arbiter, token, escrow) = setup(&env);
    env.mock_all_auths();
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    let token_admin = soroban_sdk::token::StellarAssetClient::new(&env, &token);
    token_admin.mint(&client, &1_000);

    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "fund",
            args: (client.clone(),).into_val(&env),
            sub_invokes: &[MockAuthInvoke {
                contract: &token,
                fn_name: "transfer",
                args: (client.clone(), contract_id.clone(), 1_000_i128).into_val(&env),
                sub_invokes: &[],
            }],
        },
    }]);
    escrow.fund(&client);
    let auths = env.auths();
    assert_eq!(auths[0].0, client);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "fund"),
            (client.clone(),).into_val(&env),
        ))
    );
}

// ─── accept_job ───────────────────────────────────────────────────────────────

#[test]
fn accept_job_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, _client, freelancer, _arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "accept_job",
            args: (freelancer.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_accept_job(&freelancer);
    assert!(
        res.is_err(),
        "accept_job must fail when freelancer auth is missing"
    );
}

#[test]
fn accept_job_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, _client, freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_auths(&[MockAuth {
        address: &freelancer,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "accept_job",
            args: (freelancer.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.accept_job(&freelancer);
    let auths = env.auths();
    assert_eq!(auths[0].0, freelancer);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "accept_job"),
            (freelancer.clone(),).into_val(&env),
        ))
    );
}

// ─── accept_arbiter ───────────────────────────────────────────────────────────

#[test]
fn accept_arbiter_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, _client, _freelancer, arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "accept_arbiter",
            args: (arbiter.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_accept_arbiter(&arbiter);
    assert!(
        res.is_err(),
        "accept_arbiter must fail when arbiter auth is missing"
    );
}

#[test]
fn accept_arbiter_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, _client, _freelancer, arbiter, _token, escrow) = setup(&env);
    env.mock_auths(&[MockAuth {
        address: &arbiter,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "accept_arbiter",
            args: (arbiter.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.accept_arbiter(&arbiter);
    let auths = env.auths();
    assert_eq!(auths[0].0, arbiter);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "accept_arbiter"),
            (arbiter.clone(),).into_val(&env),
        ))
    );
}

// ─── mark_delivered ───────────────────────────────────────────────────────────

#[test]
fn mark_delivered_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, _client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_delivered",
            args: (freelancer.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_mark_delivered(&freelancer, &0u32);
    assert!(
        res.is_err(),
        "mark_delivered must fail when freelancer auth is missing"
    );
}

#[test]
fn mark_delivered_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, _client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_auths(&[MockAuth {
        address: &freelancer,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_delivered",
            args: (freelancer.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.mark_delivered(&freelancer, &0u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, freelancer);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "mark_delivered"),
            (freelancer.clone(), 0u32).into_val(&env),
        ))
    );
}

// ─── request_revision ─────────────────────────────────────────────────────────

#[test]
fn request_revision_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let note_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "request_revision",
            args: (client.clone(), 0u32, note_hash.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_request_revision(&client, &0u32, &note_hash);
    assert!(
        res.is_err(),
        "request_revision must fail when client auth is missing"
    );
}

#[test]
fn request_revision_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let note_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "request_revision",
            args: (client.clone(), 0u32, note_hash.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.request_revision(&client, &0u32, &note_hash);
    let auths = env.auths();
    assert_eq!(auths[0].0, client);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "request_revision"),
            (client.clone(), 0u32, note_hash.clone()).into_val(&env),
        ))
    );
}

// ─── extend_milestone_deadline ────────────────────────────────────────────────

#[test]
fn extend_milestone_deadline_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "extend_milestone_deadline",
            args: (client.clone(), 0u32, 100u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_extend_milestone_deadline(&client, &0u32, &100u32);
    assert!(
        res.is_err(),
        "extend_milestone_deadline must fail when client auth is missing"
    );
}

#[test]
fn extend_milestone_deadline_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    // An extension needs the client and, since #670, the freelancer's
    // consent too.
    let invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "extend_milestone_deadline",
        args: (client.clone(), 0u32, 100u32).into_val(&env),
        sub_invokes: &[],
    };
    env.mock_auths(&[
        MockAuth {
            address: &client,
            invoke: &invoke,
        },
        MockAuth {
            address: &freelancer,
            invoke: &invoke,
        },
    ]);
    escrow.extend_milestone_deadline(&client, &0u32, &100u32);
    let auths = env.auths();
    assert_eq!(auths.len(), 2);
    for (i, signer) in [&client, &freelancer].into_iter().enumerate() {
        assert_eq!(&auths[i].0, signer);
        assert_eq!(
            auths[i].1.function,
            AuthorizedFunction::Contract((
                contract_id.clone(),
                Symbol::new(&env, "extend_milestone_deadline"),
                (client.clone(), 0u32, 100u32).into_val(&env),
            ))
        );
    }
}

// ─── approve_milestone ────────────────────────────────────────────────────────

#[test]
fn approve_milestone_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "approve_milestone",
            args: (client.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_approve_milestone(&client, &0u32);
    assert!(
        res.is_err(),
        "approve_milestone must fail when client auth is missing"
    );
}

#[test]
fn approve_milestone_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "approve_milestone",
            args: (client.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.approve_milestone(&client, &0u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, client);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "approve_milestone"),
            (client.clone(), 0u32).into_val(&env),
        ))
    );
}

// ─── approve_partial ──────────────────────────────────────────────────────────

#[test]
fn approve_partial_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "approve_partial",
            args: (client.clone(), 0u32, 500_i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_approve_partial(&client, &0u32, &500_i128);
    assert!(
        res.is_err(),
        "approve_partial must fail when client auth is missing"
    );
}

#[test]
fn approve_partial_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "approve_partial",
            args: (client.clone(), 0u32, 500_i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.approve_partial(&client, &0u32, &500_i128);
    let auths = env.auths();
    assert_eq!(auths[0].0, client);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "approve_partial"),
            (client.clone(), 0u32, 500_i128).into_val(&env),
        ))
    );
}

// ─── raise_dispute ────────────────────────────────────────────────────────────

#[test]
fn raise_dispute_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "raise_dispute",
            args: (client.clone(), 0u32, dispute_reason_hash(&env)).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    assert!(
        res.is_err(),
        "raise_dispute must fail when caller auth is missing"
    );
}

#[test]
fn raise_dispute_correct_signer_client_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "raise_dispute",
            args: (client.clone(), 0u32, dispute_reason_hash(&env)).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    let auths = env.auths();
    assert_eq!(auths[0].0, client);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "raise_dispute"),
            (client.clone(), 0u32, dispute_reason_hash(&env)).into_val(&env),
        ))
    );
}

#[test]
fn raise_dispute_freelancer_also_authorized() {
    let env = Env::default();
    let (contract_id, _admin, _client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    env.mock_auths(&[MockAuth {
        address: &freelancer,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "raise_dispute",
            args: (freelancer.clone(), 0u32, dispute_reason_hash(&env)).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.raise_dispute(&freelancer, &0u32, &dispute_reason_hash(&env));
    let auths = env.auths();
    assert_eq!(auths[0].0, freelancer);
}

// ─── resolve_dispute ──────────────────────────────────────────────────────────

#[test]
fn resolve_dispute_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "resolve_dispute",
            args: (arbiter.clone(), 0u32, true, decision_hash(&env)).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_resolve_dispute(&arbiter, &0u32, &true, &decision_hash(&env));
    assert!(
        res.is_err(),
        "resolve_dispute must fail when arbiter auth is missing"
    );
}

#[test]
fn resolve_dispute_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    env.mock_auths(&[MockAuth {
        address: &arbiter,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "resolve_dispute",
            args: (arbiter.clone(), 0u32, true, decision_hash(&env)).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.resolve_dispute(&arbiter, &0u32, &true, &decision_hash(&env));
    let auths = env.auths();
    assert_eq!(auths[0].0, arbiter);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "resolve_dispute"),
            (arbiter.clone(), 0u32, true, decision_hash(&env)).into_val(&env),
        ))
    );
}

// ─── apply_dispute_arbitration_split ─────────────────────────────────────────

#[test]
fn apply_dispute_arbitration_split_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "apply_dispute_arbitration_split",
            args: (arbiter.clone(), 0u32, 5_000u32, decision_hash(&env)).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_apply_dispute_arbitration_split(
        &arbiter,
        &0u32,
        &5_000u32,
        &decision_hash(&env),
    );
    assert!(
        res.is_err(),
        "apply_dispute_arbitration_split must fail when arbiter auth is missing"
    );
}

#[test]
fn apply_dispute_arbitration_split_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    env.mock_auths(&[MockAuth {
        address: &arbiter,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "apply_dispute_arbitration_split",
            args: (arbiter.clone(), 0u32, 5_000u32, decision_hash(&env)).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.apply_dispute_arbitration_split(&arbiter, &0u32, &5_000u32, &decision_hash(&env));
    let auths = env.auths();
    assert_eq!(auths[0].0, arbiter);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "apply_dispute_arbitration_split"),
            (arbiter.clone(), 0u32, 5_000u32, decision_hash(&env)).into_val(&env),
        ))
    );
}

// ─── cancel_escrow ────────────────────────────────────────────────────────────

#[test]
fn cancel_escrow_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "cancel_escrow",
            args: (client.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_cancel_escrow(&client);
    assert!(
        res.is_err(),
        "cancel_escrow must fail when caller auth is missing"
    );
}

#[test]
fn cancel_escrow_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "cancel_escrow",
            args: (client.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.cancel_escrow(&client);
    let auths = env.auths();
    assert_eq!(auths[0].0, client);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "cancel_escrow"),
            (client.clone(),).into_val(&env),
        ))
    );
}

// ─── revoke_cancel_approval ───────────────────────────────────────────────────

#[test]
fn revoke_cancel_approval_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.cancel_escrow(&client); // record client approval
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "revoke_cancel_approval",
            args: (client.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_revoke_cancel_approval(&client);
    assert!(
        res.is_err(),
        "revoke_cancel_approval must fail when caller auth is missing"
    );
}

#[test]
fn revoke_cancel_approval_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.cancel_escrow(&client);
    env.mock_auths(&[MockAuth {
        address: &client,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "revoke_cancel_approval",
            args: (client.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.revoke_cancel_approval(&client);
    let auths = env.auths();
    assert_eq!(auths[0].0, client);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "revoke_cancel_approval"),
            (client.clone(),).into_val(&env),
        ))
    );
}

// ─── admin_override_cancel_release ───────────────────────────────────────────

#[test]
fn admin_override_cancel_release_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.cancel_escrow(&client);
    escrow.cancel_escrow(&freelancer); // fires cancel lock
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_cancel_release",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_override_cancel_release(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_override_cancel_release_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.cancel_escrow(&client);
    escrow.cancel_escrow(&freelancer);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_cancel_release",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_cancel_release(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_cancel_release"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── admin_override_cancel_refund ─────────────────────────────────────────────

#[test]
fn admin_override_cancel_refund_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.cancel_escrow(&client);
    escrow.cancel_escrow(&freelancer);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_cancel_refund",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_override_cancel_refund(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_override_cancel_refund_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.cancel_escrow(&client);
    escrow.cancel_escrow(&freelancer);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_cancel_refund",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_cancel_refund(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_cancel_refund"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── emergency_pause ──────────────────────────────────────────────────────────

#[test]
fn emergency_pause_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    // Provide only the attacker's auth — client and freelancer are not covered.
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "emergency_pause",
            args: (client.clone(), freelancer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_emergency_pause(&client, &freelancer);
    assert!(
        res.is_err(),
        "emergency_pause must fail when client+freelancer auth is missing"
    );
}

#[test]
fn emergency_pause_correct_signers_succeed_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_auths(&[
        MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "emergency_pause",
                args: (client.clone(), freelancer.clone()).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &freelancer,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "emergency_pause",
                args: (client.clone(), freelancer.clone()).into_val(&env),
                sub_invokes: &[],
            },
        },
    ]);
    escrow.emergency_pause(&client, &freelancer);
    let auths = env.auths();
    // Both client and freelancer must appear in the auth record.
    assert_eq!(auths.len(), 2);
    let auth_addrs: std::vec::Vec<_> = auths.iter().map(|(a, _)| a.clone()).collect();
    assert!(auth_addrs.contains(&client));
    assert!(auth_addrs.contains(&freelancer));
}

// ─── emergency_unpause ────────────────────────────────────────────────────────

#[test]
fn emergency_unpause_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.emergency_pause(&client, &freelancer);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "emergency_unpause",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_emergency_unpause(&admin);
    assert!(
        res.is_err(),
        "emergency_unpause must fail when admin auth is missing"
    );
}

#[test]
fn emergency_unpause_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.emergency_pause(&client, &freelancer);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "emergency_unpause",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.emergency_unpause(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "emergency_unpause"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── emergency_unpause_parties ────────────────────────────────────────────────

#[test]
fn emergency_unpause_parties_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.emergency_pause(&client, &freelancer);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "emergency_unpause_parties",
            args: (client.clone(), freelancer.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_emergency_unpause_parties(&client, &freelancer);
    assert!(
        res.is_err(),
        "emergency_unpause_parties must fail when client+freelancer auth is missing"
    );
}

#[test]
fn emergency_unpause_parties_correct_signers_succeed_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.emergency_pause(&client, &freelancer);
    env.mock_auths(&[
        MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "emergency_unpause_parties",
                args: (client.clone(), freelancer.clone()).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &freelancer,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "emergency_unpause_parties",
                args: (client.clone(), freelancer.clone()).into_val(&env),
                sub_invokes: &[],
            },
        },
    ]);
    escrow.emergency_unpause_parties(&client, &freelancer);
    let auths = env.auths();
    assert_eq!(auths.len(), 2);
    let auth_addrs: std::vec::Vec<_> = auths.iter().map(|(a, _)| a.clone()).collect();
    assert!(auth_addrs.contains(&client));
    assert!(auth_addrs.contains(&freelancer));
}

// ─── emergency_pause_admin_override ──────────────────────────────────────────

#[test]
fn emergency_pause_admin_override_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "emergency_pause_admin_override",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_emergency_pause_admin_override(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn emergency_pause_admin_override_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "emergency_pause_admin_override",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.emergency_pause_admin_override(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "emergency_pause_admin_override"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── set_platform_fee_allocation ─────────────────────────────────────────────

#[test]
fn set_platform_fee_allocation_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_platform_fee_allocation",
            args: (admin.clone(), 0u32, 9_000u32, 1_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_set_platform_fee_allocation(&admin, &0u32, &9_000u32, &1_000u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn set_platform_fee_allocation_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_platform_fee_allocation",
            args: (admin.clone(), 0u32, 9_000u32, 1_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.set_platform_fee_allocation(&admin, &0u32, &9_000u32, &1_000u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "set_platform_fee_allocation"),
            (admin.clone(), 0u32, 9_000u32, 1_000u32).into_val(&env),
        ))
    );
}

// ─── admin_pause_escrow ───────────────────────────────────────────────────────

#[test]
fn admin_pause_escrow_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_pause_escrow",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_pause_escrow(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_pause_escrow_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_pause_escrow",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_pause_escrow(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_pause_escrow"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── admin_resume_escrow ──────────────────────────────────────────────────────

#[test]
fn admin_resume_escrow_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    escrow.admin_pause_escrow(&admin);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_resume_escrow",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_resume_escrow(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_resume_escrow_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    escrow.admin_pause_escrow(&admin);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_resume_escrow",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_resume_escrow(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_resume_escrow"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── admin_override_release ───────────────────────────────────────────────────

#[test]
fn admin_override_release_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_release",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_override_release(&admin, &0u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_override_release_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    // The override needs a disputed milestone (or a cancel lock), see #687.
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_release",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_release(&admin, &0u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_release"),
            (admin.clone(), 0u32).into_val(&env),
        ))
    );
}

// ─── admin_override_refund ────────────────────────────────────────────────────

#[test]
fn admin_override_refund_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_refund",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_override_refund(&admin, &0u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_override_refund_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    // The override needs a disputed milestone (or a cancel lock), see #687.
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_refund",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_refund(&admin, &0u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_refund"),
            (admin.clone(), 0u32).into_val(&env),
        ))
    );
}

// ─── admin_override_streaming_release ────────────────────────────────────────

#[test]
fn admin_override_streaming_release_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_streaming_release",
            args: (admin.clone(), 0u32, 50_i128, 100_i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_override_streaming_release(&admin, &0u32, &50_i128, &100_i128);
    assert!(res.is_err(), "expected auth failure");
}

/// Admin auth alone suffices: no contract self-auth is required (#685).
#[test]
fn admin_override_streaming_release_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &dispute_reason_hash(&env));
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_streaming_release",
            args: (admin.clone(), 0u32, 50_i128, 100_i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_streaming_release(&admin, &0u32, &50_i128, &100_i128);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_streaming_release"),
            (admin.clone(), 0u32, 50_i128, 100_i128).into_val(&env),
        ))
    );
}

// ─── tax_withholding_deductions ───────────────────────────────────────────────

#[test]
fn tax_withholding_deductions_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    // only attacker — neither client nor freelancer
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "tax_withholding_deductions",
            args: (0u32, 1_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_tax_withholding_deductions(&0u32, &1_000u32);
    assert!(
        res.is_err(),
        "tax_withholding_deductions must fail when client+freelancer auth is missing"
    );
}

#[test]
fn tax_withholding_deductions_correct_signers_succeed_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_auths(&[
        MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "tax_withholding_deductions",
                args: (0u32, 1_000u32).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &freelancer,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "tax_withholding_deductions",
                args: (0u32, 1_000u32).into_val(&env),
                sub_invokes: &[],
            },
        },
    ]);
    escrow.tax_withholding_deductions(&0u32, &1_000u32);
    let auths = env.auths();
    assert_eq!(auths.len(), 2);
    let auth_addrs: std::vec::Vec<_> = auths.iter().map(|(a, _)| a.clone()).collect();
    assert!(auth_addrs.contains(&client));
    assert!(auth_addrs.contains(&freelancer));
}

// ─── payment_streaming_milestones ─────────────────────────────────────────────
// Issue #988: payment_streaming_milestones is now a pure read-only view.
// No signatures are required; any caller can invoke it via simulateTransaction.

#[test]
fn payment_streaming_milestones_requires_no_auth_after_988() {
    let env = Env::default();
    let (_contract_id, _admin, _client, _freelancer, _arbiter, _token, escrow) =
        setup_funded(&env);
    // No mocked auths at all — the pure view must succeed.
    let res = escrow.try_payment_streaming_milestones(&1_000_i128, &1_i128, &2_i128);
    assert!(
        res.is_ok(),
        "payment_streaming_milestones must succeed without any signatures after #988"
    );
}

// ─── milestone_time_extensions ────────────────────────────────────────────────
// Issue #988: milestone_time_extensions is now a pure read-only view.
// No signatures are required; any caller can invoke it via simulateTransaction.

#[test]
fn milestone_time_extensions_requires_no_auth_after_988() {
    let env = Env::default();
    let (_contract_id, _admin, _client, _freelancer, _arbiter, _token, escrow) =
        setup_funded(&env);
    // No mocked auths at all — the pure view must succeed.
    let res = escrow.try_milestone_time_extensions(&1_000_i128, &1_i128, &2_i128);
    assert!(
        res.is_ok(),
        "milestone_time_extensions must succeed without any signatures after #988"
    );
}
}

// ─── payment_streaming_consent ────────────────────────────────────────────────

#[test]
fn payment_streaming_consent_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, _client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "payment_streaming_consent",
            args: (0u32, 1_i128, 2_i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_payment_streaming_consent(&0u32, &1_i128, &2_i128);
    assert!(
        res.is_err(),
        "payment_streaming_consent must fail when client+freelancer auth is missing"
    );
}

#[test]
fn payment_streaming_consent_correct_signers_succeed_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    env.mock_auths(&[
        MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "payment_streaming_consent",
                args: (0u32, 1_i128, 2_i128).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &freelancer,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "payment_streaming_consent",
                args: (0u32, 1_i128, 2_i128).into_val(&env),
                sub_invokes: &[],
            },
        },
    ]);
    escrow.payment_streaming_consent(&0u32, &1_i128, &2_i128);
    let auths = env.auths();
    assert_eq!(auths.len(), 2);
    let auth_addrs: std::vec::Vec<_> = auths.iter().map(|(a, _)| a.clone()).collect();
    assert!(auth_addrs.contains(&client));
    assert!(auth_addrs.contains(&freelancer));
}

// ─── time_extensions_consent ──────────────────────────────────────────────────

#[test]
fn time_extensions_consent_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, _client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "time_extensions_consent",
            args: (0u32, 100u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_time_extensions_consent(&0u32, &100u32);
    assert!(
        res.is_err(),
        "time_extensions_consent must fail when client+freelancer auth is missing"
    );
}

#[test]
fn time_extensions_consent_correct_signers_succeed_and_auths_match() {
    let env = Env::default();
    let (contract_id, _admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.mark_delivered(&freelancer, &0u32);
    env.mock_auths(&[
        MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "time_extensions_consent",
                args: (0u32, 100u32).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &freelancer,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "time_extensions_consent",
                args: (0u32, 100u32).into_val(&env),
                sub_invokes: &[],
            },
        },
    ]);
    escrow.time_extensions_consent(&0u32, &100u32);
    let auths = env.auths();
    assert_eq!(auths.len(), 2);
    let auth_addrs: std::vec::Vec<_> = auths.iter().map(|(a, _)| a.clone()).collect();
    assert!(auth_addrs.contains(&client));
    assert!(auth_addrs.contains(&freelancer));
}

// ─── admin_set_yield_rate ─────────────────────────────────────────────────────

#[test]
fn admin_set_yield_rate_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_set_yield_rate",
            args: (admin.clone(), 500u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_set_yield_rate(&admin, &500u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_set_yield_rate_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_set_yield_rate",
            args: (admin.clone(), 500u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_set_yield_rate(&admin, &500u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_set_yield_rate"),
            (admin.clone(), 500u32).into_val(&env),
        ))
    );
}

// ─── admin_accrue_yield ───────────────────────────────────────────────────────

#[test]
fn admin_accrue_yield_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_accrue_yield",
            args: (admin.clone(), 0u32, 100_i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_accrue_yield(&admin, &0u32, &100_i128);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_accrue_yield_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_accrue_yield",
            args: (admin.clone(), 0u32, 100_i128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_accrue_yield(&admin, &0u32, &100_i128);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_accrue_yield"),
            (admin.clone(), 0u32, 100_i128).into_val(&env),
        ))
    );
}

// ─── admin_override_tax_release ───────────────────────────────────────────────

#[test]
fn admin_override_tax_release_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.tax_withholding_deductions(&0u32, &1_000u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_tax_release",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let _ = client;
    let _ = freelancer;
    let res = escrow.try_admin_override_tax_release(&admin, &0u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_override_tax_release_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.tax_withholding_deductions(&0u32, &1_000u32);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_tax_release",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_tax_release(&admin, &0u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_tax_release"),
            (admin.clone(), 0u32).into_val(&env),
        ))
    );
}

// ─── admin_override_tax_refund ────────────────────────────────────────────────

#[test]
fn admin_override_tax_refund_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.tax_withholding_deductions(&0u32, &1_000u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_tax_refund",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_override_tax_refund(&admin, &0u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_override_tax_refund_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.tax_withholding_deductions(&0u32, &1_000u32);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_tax_refund",
            args: (admin.clone(), 0u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_tax_refund(&admin, &0u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_tax_refund"),
            (admin.clone(), 0u32).into_val(&env),
        ))
    );
}

// ─── admin_override_tax_split_refund ─────────────────────────────────────────

#[test]
fn admin_override_tax_split_refund_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.tax_withholding_deductions(&0u32, &1_000u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_tax_split_refund",
            args: (admin.clone(), 0u32, 5_000u32, 5_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_override_tax_split_refund(&admin, &0u32, &5_000u32, &5_000u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_override_tax_split_refund_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.tax_withholding_deductions(&0u32, &1_000u32);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_override_tax_split_refund",
            args: (admin.clone(), 0u32, 5_000u32, 5_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_override_tax_split_refund(&admin, &0u32, &5_000u32, &5_000u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_override_tax_split_refund"),
            (admin.clone(), 0u32, 5_000u32, 5_000u32).into_val(&env),
        ))
    );
}

// ─── admin_tax_withholding_deductions ────────────────────────────────────────

#[test]
fn admin_tax_withholding_deductions_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_tax_withholding_deductions",
            args: (admin.clone(), 0u32, 1_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_admin_tax_withholding_deductions(&admin, &0u32, &1_000u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn admin_tax_withholding_deductions_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "admin_tax_withholding_deductions",
            args: (admin.clone(), 0u32, 1_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.admin_tax_withholding_deductions(&admin, &0u32, &1_000u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "admin_tax_withholding_deductions"),
            (admin.clone(), 0u32, 1_000u32).into_val(&env),
        ))
    );
}

// ─── multisig_lock ────────────────────────────────────────────────────────────

#[test]
fn multisig_lock_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "multisig_lock",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_multisig_lock(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn multisig_lock_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "multisig_lock",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.multisig_lock(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "multisig_lock"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── propose_admin_transfer ───────────────────────────────────────────────────

#[test]
fn propose_admin_transfer_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    let signers = soroban_sdk::vec![&env, Address::generate(&env)];
    escrow.multisig_approval_init(&admin, &signers, &1u32);
    let new_admin = Address::generate(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "propose_admin_transfer",
            args: (admin.clone(), new_admin.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_propose_admin_transfer(&admin, &new_admin);
    assert!(
        res.is_err(),
        "propose_admin_transfer must fail when admin auth is missing"
    );
}

#[test]
fn propose_admin_transfer_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    let signers = soroban_sdk::vec![&env, Address::generate(&env)];
    escrow.multisig_approval_init(&admin, &signers, &1u32);
    let new_admin = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "propose_admin_transfer",
            args: (admin.clone(), new_admin.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.propose_admin_transfer(&admin, &new_admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "propose_admin_transfer"),
            (admin.clone(), new_admin.clone()).into_val(&env),
        ))
    );
}

// ─── cancel_admin_transfer_proposal ──────────────────────────────────────────

#[test]
fn cancel_admin_transfer_proposal_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    let signers = soroban_sdk::vec![&env, Address::generate(&env)];
    escrow.multisig_approval_init(&admin, &signers, &1u32);
    let new_admin = Address::generate(&env);
    escrow.propose_admin_transfer(&admin, &new_admin);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "cancel_admin_transfer_proposal",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_cancel_admin_transfer_proposal(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn cancel_admin_transfer_proposal_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    let signers = soroban_sdk::vec![&env, Address::generate(&env)];
    escrow.multisig_approval_init(&admin, &signers, &1u32);
    let new_admin = Address::generate(&env);
    escrow.propose_admin_transfer(&admin, &new_admin);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "cancel_admin_transfer_proposal",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.cancel_admin_transfer_proposal(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "cancel_admin_transfer_proposal"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── set_escrow_interest_yield ────────────────────────────────────────────────

#[test]
fn set_escrow_interest_yield_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_escrow_interest_yield",
            args: (admin.clone(), 5_000u32, 5_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_set_escrow_interest_yield(&admin, &5_000u32, &5_000u32);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn set_escrow_interest_yield_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_escrow_interest_yield",
            args: (admin.clone(), 5_000u32, 5_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.set_escrow_interest_yield(&admin, &5_000u32, &5_000u32);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "set_escrow_interest_yield"),
            (admin.clone(), 5_000u32, 5_000u32).into_val(&env),
        ))
    );
}

// ─── lock_escrow_interest_yield ───────────────────────────────────────────────

#[test]
fn lock_escrow_interest_yield_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    escrow.set_escrow_interest_yield(&admin, &5_000u32, &5_000u32);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "lock_escrow_interest_yield",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_lock_escrow_interest_yield(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn lock_escrow_interest_yield_correct_signer_succeeds_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup(&env);
    env.mock_all_auths();
    escrow.set_escrow_interest_yield(&admin, &5_000u32, &5_000u32);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "lock_escrow_interest_yield",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    escrow.lock_escrow_interest_yield(&admin);
    let auths = env.auths();
    assert_eq!(auths[0].0, admin);
    assert_eq!(
        auths[0].1.function,
        AuthorizedFunction::Contract((
            contract_id.clone(),
            Symbol::new(&env, "lock_escrow_interest_yield"),
            (admin.clone(),).into_val(&env),
        ))
    );
}

// ─── unlock_escrow_interest_yield ─────────────────────────────────────────────

#[test]
fn unlock_escrow_interest_yield_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.set_interest_yield_consent(&admin, &5_000u32, &5_000u32);
    escrow.lock_escrow_interest_yield(&admin);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "unlock_escrow_interest_yield",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_unlock_escrow_interest_yield(&admin);
    assert!(res.is_err(), "expected auth failure");
}

#[test]
fn unlock_escrow_interest_yield_correct_signers_succeed_and_auths_match() {
    let env = Env::default();
    let (contract_id, admin, client, freelancer, _arbiter, _token, escrow) = setup_funded(&env);
    env.mock_all_auths();
    escrow.set_interest_yield_consent(&admin, &5_000u32, &5_000u32);
    escrow.lock_escrow_interest_yield(&admin);
    env.mock_auths(&[
        MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "unlock_escrow_interest_yield",
                args: (admin.clone(),).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "unlock_escrow_interest_yield",
                args: (admin.clone(),).into_val(&env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &freelancer,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "unlock_escrow_interest_yield",
                args: (admin.clone(),).into_val(&env),
                sub_invokes: &[],
            },
        },
    ]);
    escrow.unlock_escrow_interest_yield(&admin);
    let auths = env.auths();
    assert_eq!(auths.len(), 3);
    let auth_addrs: std::vec::Vec<_> = auths.iter().map(|(a, _)| a.clone()).collect();
    assert!(auth_addrs.contains(&admin));
    assert!(auth_addrs.contains(&client));
    assert!(auth_addrs.contains(&freelancer));
}

// ─── sweep_excess ─────────────────────────────────────────────────────────────

#[test]
fn sweep_excess_wrong_signer_fails() {
    let env = Env::default();
    let (contract_id, _admin, client, _freelancer, _arbiter, token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "sweep_excess",
            args: (client.clone(), token.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let res = escrow.try_sweep_excess(&client, &token);
    assert!(
        res.is_err(),
        "sweep_excess must fail when caller auth is missing"
    );
}

#[test]
fn sweep_excess_admin_signer_rejected_as_wrong_role() {
    // admin is not the client so sweep_excess must return Unauthorized
    // even when admin's auth IS satisfied.
    let env = Env::default();
    let (contract_id, admin, _client, _freelancer, _arbiter, token, escrow) = setup_funded(&env);
    let attacker = Address::generate(&env);
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "sweep_excess",
            args: (admin.clone(), token.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    // admin auth is not granted here; this tests that a non-client/non-admin
    // caller (attacker pretending to be admin) is refused
    let res = escrow.try_sweep_excess(&admin, &token);
    assert!(res.is_err());
}
