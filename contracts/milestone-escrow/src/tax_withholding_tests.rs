use super::*;
use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};

macro_rules! withholding_auth {
    ($env:expr, $contract_id:expr, $address:expr) => {
        MockAuth {
            address: $address,
            invoke: &MockAuthInvoke {
                contract: $contract_id,
                fn_name: "tax_withholding_deductions",
                args: (&0u32, &500u32).into_val($env),
                sub_invokes: &[],
            },
        }
    };
}

fn setup_withholding_case(env: &Env) -> (Address, Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    let amounts = vec![env, 1_000_i128];
    let (client, freelancer, _, _, _, _, escrow) = setup_funded_escrow(env, amounts);
    (client, freelancer, escrow.address.clone(), escrow)
}

#[test]
fn tax_withholding_requires_both_signatures() {
    let env = Env::default();
    let (client_addr, freelancer_addr, contract_id, escrow) = setup_withholding_case(&env);

    let result = escrow
        .mock_auths(&[
            withholding_auth!(&env, &contract_id, &client_addr),
            withholding_auth!(&env, &contract_id, &freelancer_addr),
        ])
        .tax_withholding_deductions(&0, &500);

    assert_eq!(result.gross_amount, 1_000);
    assert_eq!(result.tax_amount, 50);
    assert_eq!(result.net_amount, 950);
}

#[test]
fn tax_withholding_rejects_client_only_signature() {
    let env = Env::default();
    let (client_addr, freelancer_addr, contract_id, escrow) = setup_withholding_case(&env);

    let result = escrow
        .mock_auths(&[withholding_auth!(&env, &contract_id, &client_addr)])
        .try_tax_withholding_deductions(&0, &500);

    assert!(matches!(result, Err(Err(_))));
    assert!(freelancer_addr != client_addr);
}

#[test]
fn tax_withholding_rejects_freelancer_only_signature() {
    let env = Env::default();
    let (client_addr, freelancer_addr, contract_id, escrow) = setup_withholding_case(&env);

    let result = escrow
        .mock_auths(&[withholding_auth!(&env, &contract_id, &freelancer_addr)])
        .try_tax_withholding_deductions(&0, &500);

    assert!(matches!(result, Err(Err(_))));
    assert!(client_addr != freelancer_addr);
}

// ── Issue #298: validation requirements ─────────────────────────────────────
//
// tax_withholding_deductions already rejected Released/Refunded milestones,
// but not Disputed ones — the one status this file's dual-signature setup
// doesn't otherwise exercise, and unlike `raise_dispute_inner` /
// `resolve_dispute` elsewhere in this crate, which both treat Disputed as its
// own case via an exhaustive match rather than folding it into "terminal".
// A disputed milestone's funds are meant to be frozen until `resolve_dispute`
// runs; letting tax_withholding_deductions compute and persist a
// TaxWithholdingRecord for one would let this entry point move money around
// that freeze. Covered here since no existing test combined raise_dispute
// with tax_withholding_deductions.

#[test]
fn tax_withholding_rejects_disputed_milestone() {
    let env = Env::default();
    let (client_addr, _freelancer_addr, _contract_id, escrow) = setup_withholding_case(&env);

    escrow.raise_dispute(&client_addr, &0u32);

    let result = escrow.try_tax_withholding_deductions(&0u32, &500u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn failed_single_signature_does_not_create_withholding_record() {
    let env = Env::default();
    let (client_addr, freelancer_addr, contract_id, escrow) = setup_withholding_case(&env);

    let failed = escrow
        .mock_auths(&[withholding_auth!(&env, &contract_id, &client_addr)])
        .try_tax_withholding_deductions(&0, &500);
    assert!(matches!(failed, Err(Err(_))));

    let succeeded = escrow
        .mock_auths(&[
            withholding_auth!(&env, &contract_id, &client_addr),
            withholding_auth!(&env, &contract_id, &freelancer_addr),
        ])
        .try_tax_withholding_deductions(&0, &500);
    assert!(succeeded.is_ok());
}

// ── Issue #452 & #453: tax_deduction_estimator precision & rate limiting ──────

#[test]
fn test_tax_deduction_estimator_preserves_full_precision() {
    let env = Env::default();
    env.mock_all_auths();
    let milestone_amounts = vec![&env, 12_345_678_901_234_567_890_i128];
    let (_client_addr, _freelancer_addr, _, _, _, _contract_id, escrow) =
        setup_funded_escrow(&env, milestone_amounts);

    let result = escrow.tax_withholding_deductions(&0, &2_500);

    // Assert full precision preservation in stored record attributes
    assert_eq!(result.gross_amount, 12_345_678_901_234_567_890);
    let expected_tax = (12_345_678_901_234_567_890_i128 * 2500 + 5000) / 10000;
    assert_eq!(result.tax_amount, expected_tax);
    assert_eq!(result.net_amount, result.gross_amount - result.tax_amount);
    assert_eq!(result.tax_rate_bps, 2_500);
}

#[test]
fn test_tax_deduction_estimator_rate_limiting_execution_lock() {
    let env = Env::default();
    env.mock_all_auths();
    let milestone_amounts = vec![&env, 1_000_i128];
    let (_client_addr, _freelancer_addr, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, milestone_amounts);

    // Verify rate limit execution lock behavior
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::TaxWithholdingExecutionLock, &true);
    });

    let reentrant_call = escrow.try_tax_withholding_deductions(&0, &500);
    assert_eq!(reentrant_call, Err(Ok(Error::TaxWithholdingInProgress)));
}
