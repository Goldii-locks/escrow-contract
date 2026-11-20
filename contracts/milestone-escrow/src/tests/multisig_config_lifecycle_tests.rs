#![cfg(test)]
//! End-to-end lifecycle coverage for multisig signer-set proposals (#981).
//!
//! Every authenticated call uses an explicit mock authorization for the
//! specific admin or signer; no test uses `mock_all_auths`.

use crate::testutils::EscrowFixture;
use crate::{Error, MilestoneEscrowClient, PendingMultiSigConfig};
use soroban_sdk::testutils::{Address as _, EnvTestConfig, MockAuth, MockAuthInvoke};
use soroban_sdk::{vec, Address, Env, IntoVal, Val, Vec};

struct Setup<'a> {
    admin: Address,
    contract_id: Address,
    old_signers: Vec<Address>,
    new_signers: Vec<Address>,
    escrow: MilestoneEscrowClient<'a>,
}

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn mock_auth(env: &Env, address: &Address, contract: &Address, fn_name: &str, args: Vec<Val>) {
    env.mock_auths(&[MockAuth {
        address,
        invoke: &MockAuthInvoke {
            contract,
            fn_name,
            args,
            sub_invokes: &[],
        },
    }]);
}

fn setup(env: &Env) -> Setup<'_> {
    let fixture = EscrowFixture::new(env).funded().build();
    let old_signers = vec![
        env,
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ];
    let new_signers = vec![env, Address::generate(env), Address::generate(env)];
    let init_args = (&fixture.admin, &old_signers, &2u32).into_val(env);
    mock_auth(
        env,
        &fixture.admin,
        &fixture.contract_id,
        "multisig_approval_init",
        init_args,
    );
    fixture
        .escrow
        .multisig_approval_init(&fixture.admin, &old_signers, &2);

    Setup {
        admin: fixture.admin,
        contract_id: fixture.contract_id,
        old_signers,
        new_signers,
        escrow: fixture.escrow,
    }
}

fn propose(env: &Env, setup: &Setup, signers: &Vec<Address>, threshold: u32) -> u32 {
    mock_auth(
        env,
        &setup.admin,
        &setup.contract_id,
        "propose_multisig_config",
        (&setup.admin, signers, &threshold).into_val(env),
    );
    setup
        .escrow
        .propose_multisig_config(&setup.admin, signers, &threshold)
}

fn approve(env: &Env, setup: &Setup, signer: &Address, proposal_id: u32) {
    mock_auth(
        env,
        signer,
        &setup.contract_id,
        "multisig_approve",
        (signer, &proposal_id).into_val(env),
    );
    setup.escrow.multisig_approve(signer, &proposal_id);
}

#[test]
fn approvals_reach_threshold_and_new_signers_control_admin_transfer() {
    let env = test_env();
    let s = setup(&env);
    assert_eq!(s.escrow.get_pending_multisig_config(), None);

    let proposal_id = propose(&env, &s, &s.new_signers, 2);
    assert_eq!(
        s.escrow.get_pending_multisig_config(),
        Some(PendingMultiSigConfig {
            signers: s.new_signers.clone(),
            threshold: 2,
            proposal_id,
        })
    );

    approve(&env, &s, &s.old_signers.get(0).unwrap(), proposal_id);
    assert_eq!(
        s.escrow.try_execute_multisig_config(),
        Err(Ok(Error::MultiSigThresholdNotMet))
    );
    assert!(s.escrow.get_pending_multisig_config().is_some());

    approve(&env, &s, &s.old_signers.get(2).unwrap(), proposal_id);
    assert!(s.escrow.get_pending_multisig_config().is_some());
    s.escrow.execute_multisig_config();
    assert_eq!(s.escrow.get_pending_multisig_config(), None);

    let target = Address::generate(&env);
    mock_auth(
        &env,
        &s.admin,
        &s.contract_id,
        "propose_admin_transfer",
        (&s.admin, &target).into_val(&env),
    );
    let transfer_id = s.escrow.propose_admin_transfer(&s.admin, &target);

    assert_eq!(
        s.escrow
            .mock_auths(&[MockAuth {
                address: &s.old_signers.get(0).unwrap(),
                invoke: &MockAuthInvoke {
                    contract: &s.contract_id,
                    fn_name: "multisig_approve",
                    args: (&s.old_signers.get(0).unwrap(), &transfer_id).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .try_multisig_approve(&s.old_signers.get(0).unwrap(), &transfer_id),
        Err(Ok(Error::Unauthorized))
    );
    approve(&env, &s, &s.new_signers.get(0).unwrap(), transfer_id);
    approve(&env, &s, &s.new_signers.get(1).unwrap(), transfer_id);
    assert!(s.escrow.is_multisig_approved(&transfer_id).approved);
}

#[test]
fn only_one_config_proposal_may_be_pending() {
    let env = test_env();
    let s = setup(&env);
    assert_eq!(s.escrow.get_pending_multisig_config(), None);
    let proposal_id = propose(&env, &s, &s.new_signers, 2);
    let pending = s.escrow.get_pending_multisig_config();
    assert_eq!(pending.as_ref().unwrap().proposal_id, proposal_id);

    let other_signers = vec![&env, Address::generate(&env)];
    mock_auth(
        &env,
        &s.admin,
        &s.contract_id,
        "propose_multisig_config",
        (&s.admin, &other_signers, &1u32).into_val(&env),
    );
    assert_eq!(
        s.escrow
            .try_propose_multisig_config(&s.admin, &other_signers, &1),
        Err(Ok(Error::MultiSigConfigPending))
    );
    assert_eq!(s.escrow.get_pending_multisig_config(), pending);
}

#[test]
fn cancelled_proposal_cannot_be_executed_or_approved_and_cancel_requires_admin() {
    let env = test_env();
    let s = setup(&env);
    assert_eq!(s.escrow.get_pending_multisig_config(), None);
    let proposal_id = propose(&env, &s, &s.new_signers, 2);
    assert!(s.escrow.get_pending_multisig_config().is_some());

    let unauthorized = Address::generate(&env);
    mock_auth(
        &env,
        &unauthorized,
        &s.contract_id,
        "cancel_multisig_config_proposal",
        (&unauthorized,).into_val(&env),
    );
    assert_eq!(
        s.escrow.try_cancel_multisig_config_proposal(&unauthorized),
        Err(Ok(Error::Unauthorized))
    );
    assert!(s.escrow.get_pending_multisig_config().is_some());

    mock_auth(
        &env,
        &s.admin,
        &s.contract_id,
        "cancel_multisig_config_proposal",
        (&s.admin,).into_val(&env),
    );
    s.escrow.cancel_multisig_config_proposal(&s.admin);
    assert_eq!(s.escrow.get_pending_multisig_config(), None);
    assert_eq!(
        s.escrow.try_execute_multisig_config(),
        Err(Ok(Error::NoPendingMultiSigConfig))
    );

    mock_auth(
        &env,
        &s.old_signers.get(0).unwrap(),
        &s.contract_id,
        "multisig_approve",
        (&s.old_signers.get(0).unwrap(), &proposal_id).into_val(&env),
    );
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.old_signers.get(0).unwrap(), &proposal_id),
        Err(Ok(Error::InvalidStatus))
    );
    assert_eq!(s.escrow.get_pending_multisig_config(), None);
}
