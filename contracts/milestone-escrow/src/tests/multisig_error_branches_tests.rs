#![cfg(test)]
//! Error-branch coverage for `multisig_admin_override_release`,
//! `multisig_admin_override_refund`, `multisig_approve` and
//! `multisig_transfer_admin` (issue #748).
//!
//! These pin the current behaviour of the multisig paths. Every call —
//! including setup — is authorised with an explicit `mock_auths` entry for the
//! signer or admin that the call requires; nothing here uses `mock_all_auths`.

use super::*;
use crate::{DataKey, Error, MultiSigConfig};
use soroban_sdk::testutils::{EnvTestConfig, MockAuth, MockAuthInvoke};
use soroban_sdk::{vec, Address, Env, IntoVal, Val, Vec};

const MILESTONE_AMOUNT: i128 = 1_000;

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

struct Ctx<'a> {
    admin: Address,
    client_addr: Address,
    contract_id: Address,
    escrow: MilestoneEscrowClient<'a>,
}

/// Authorise exactly one root invocation of `fn_name` on `contract` by
/// `address`.
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

/// An initialised single-milestone escrow, funded when `funded` is true.
fn setup(env: &Env, funded: bool) -> Ctx<'_> {
    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = vec![env, MILESTONE_AMOUNT];
    mock_auth(
        env,
        &admin,
        &contract_id,
        "initialize",
        (
            &admin,
            &client_addr,
            &freelancer_addr,
            &arbiter_addr,
            &token_id,
            &604800u64,
            &amounts,
            &crate::spec_hashes_for(&amounts),
        )
            .into_val(env),
    );
    escrow.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800u64,
        &amounts,
        &crate::spec_hashes_for(&amounts),
    );

    if funded {
        mock_auth(
            env,
            &admin,
            &token_id,
            "mint",
            (&client_addr, &MILESTONE_AMOUNT).into_val(env),
        );
        token::StellarAssetClient::new(env, &token_id).mint(&client_addr, &MILESTONE_AMOUNT);

        env.mock_auths(&[MockAuth {
            address: &client_addr,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "fund",
                args: (&client_addr,).into_val(env),
                sub_invokes: &[MockAuthInvoke {
                    contract: &token_id,
                    fn_name: "transfer",
                    args: (&client_addr, &contract_id, &MILESTONE_AMOUNT).into_val(env),
                    sub_invokes: &[],
                }],
            },
        }]);
        escrow.fund(&client_addr);
    }

    Ctx {
        admin,
        client_addr,
        contract_id,
        escrow,
    }
}

fn lock_multisig(env: &Env, ctx: &Ctx) {
    mock_auth(
        env,
        &ctx.admin,
        &ctx.contract_id,
        "multisig_lock",
        (&ctx.admin,).into_val(env),
    );
    ctx.escrow.multisig_lock(&ctx.admin);
}

fn multisig_locked(env: &Env, contract_id: &Address) -> bool {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::MultisigLocked)
            .unwrap_or(false)
    })
}

fn update_milestone(env: &Env, contract_id: &Address, f: impl FnOnce(&mut Milestone)) {
    env.as_contract(contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0))
            .unwrap();
        f(&mut milestone);
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0), &milestone);
    });
}

fn try_release(env: &Env, ctx: &Ctx, caller: &Address, index: u32) -> Result<(), Error> {
    mock_auth(
        env,
        caller,
        &ctx.contract_id,
        "multisig_admin_override_release",
        (caller, &index).into_val(env),
    );
    match ctx
        .escrow
        .try_multisig_admin_override_release(caller, &index)
    {
        Ok(Ok(())) => Ok(()),
        Err(Ok(error)) => Err(error),
        other => panic!("unexpected host result: {other:?}"),
    }
}

fn try_refund(env: &Env, ctx: &Ctx, caller: &Address, index: u32) -> Result<(), Error> {
    mock_auth(
        env,
        caller,
        &ctx.contract_id,
        "multisig_admin_override_refund",
        (caller, &index).into_val(env),
    );
    match ctx
        .escrow
        .try_multisig_admin_override_refund(caller, &index)
    {
        Ok(Ok(())) => Ok(()),
        Err(Ok(error)) => Err(error),
        other => panic!("unexpected host result: {other:?}"),
    }
}

// ── multisig_admin_override_release ──────────────────────────────────────────

#[test]
fn override_release_rejects_uninitialised_contract() {
    let env = test_env();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let stranger = Address::generate(&env);

    mock_auth(
        &env,
        &stranger,
        &contract_id,
        "multisig_admin_override_release",
        (&stranger, &0u32).into_val(&env),
    );
    assert_eq!(
        escrow.try_multisig_admin_override_release(&stranger, &0u32),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn override_release_requires_admin_signature() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);

    let result = ctx
        .escrow
        .mock_auths(&[])
        .try_multisig_admin_override_release(&ctx.admin, &0u32);
    assert!(matches!(result, Err(Err(_))));
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_release_rejects_non_admin() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);

    assert_eq!(
        try_release(&env, &ctx, &ctx.client_addr, 0),
        Err(Error::Unauthorized)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_release_rejects_when_not_locked() {
    let env = test_env();
    let ctx = setup(&env, true);

    assert_eq!(
        try_release(&env, &ctx, &ctx.admin, 0),
        Err(Error::InvalidStatus)
    );
}

#[test]
fn override_release_rejects_unfunded_escrow() {
    let env = test_env();
    let ctx = setup(&env, false);
    lock_multisig(&env, &ctx);

    assert_eq!(
        try_release(&env, &ctx, &ctx.admin, 0),
        Err(Error::NotFunded)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_release_rejects_out_of_range_milestone() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);

    assert_eq!(
        try_release(&env, &ctx, &ctx.admin, 1),
        Err(Error::InvalidMilestone)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_release_rejects_terminal_milestone() {
    for status in [MilestoneStatus::Released, MilestoneStatus::Refunded] {
        let env = test_env();
        let ctx = setup(&env, true);
        lock_multisig(&env, &ctx);
        update_milestone(&env, &ctx.contract_id, |m| m.status = status);

        assert_eq!(
            try_release(&env, &ctx, &ctx.admin, 0),
            Err(Error::InvalidStatus)
        );
        assert!(multisig_locked(&env, &ctx.contract_id));
    }
}

#[test]
fn override_release_rejects_negative_amounts() {
    for (amount, released_amount) in [(-1_i128, 0_i128), (MILESTONE_AMOUNT, -1_i128)] {
        let env = test_env();
        let ctx = setup(&env, true);
        lock_multisig(&env, &ctx);
        update_milestone(&env, &ctx.contract_id, |m| {
            m.amount = amount;
            m.released_amount = released_amount;
        });

        assert_eq!(
            try_release(&env, &ctx, &ctx.admin, 0),
            Err(Error::InvalidAmount)
        );
        assert!(multisig_locked(&env, &ctx.contract_id));
    }
}

#[test]
fn override_release_rejects_released_amount_above_amount() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);
    update_milestone(&env, &ctx.contract_id, |m| {
        m.released_amount = MILESTONE_AMOUNT + 1;
        m.status = MilestoneStatus::PartiallyReleased;
    });

    assert_eq!(
        try_release(&env, &ctx, &ctx.admin, 0),
        Err(Error::InvalidAmount)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_release_rejects_zero_remaining_balance() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);
    update_milestone(&env, &ctx.contract_id, |m| {
        m.released_amount = MILESTONE_AMOUNT;
        m.status = MilestoneStatus::PartiallyReleased;
    });

    assert_eq!(
        try_release(&env, &ctx, &ctx.admin, 0),
        Err(Error::InvalidAmount)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

// ── multisig_admin_override_refund ───────────────────────────────────────────

#[test]
fn override_refund_rejects_uninitialised_contract() {
    let env = test_env();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let stranger = Address::generate(&env);

    mock_auth(
        &env,
        &stranger,
        &contract_id,
        "multisig_admin_override_refund",
        (&stranger, &0u32).into_val(&env),
    );
    assert_eq!(
        escrow.try_multisig_admin_override_refund(&stranger, &0u32),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn override_refund_requires_admin_signature() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);

    let result = ctx
        .escrow
        .mock_auths(&[])
        .try_multisig_admin_override_refund(&ctx.admin, &0u32);
    assert!(matches!(result, Err(Err(_))));
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_refund_rejects_non_admin() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);

    assert_eq!(
        try_refund(&env, &ctx, &ctx.client_addr, 0),
        Err(Error::Unauthorized)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_refund_rejects_when_not_locked() {
    let env = test_env();
    let ctx = setup(&env, true);

    assert_eq!(
        try_refund(&env, &ctx, &ctx.admin, 0),
        Err(Error::InvalidStatus)
    );
}

#[test]
fn override_refund_rejects_unfunded_escrow() {
    let env = test_env();
    let ctx = setup(&env, false);
    lock_multisig(&env, &ctx);

    assert_eq!(try_refund(&env, &ctx, &ctx.admin, 0), Err(Error::NotFunded));
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_refund_rejects_out_of_range_milestone() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);

    assert_eq!(
        try_refund(&env, &ctx, &ctx.admin, 1),
        Err(Error::InvalidMilestone)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_refund_rejects_terminal_milestone() {
    for status in [MilestoneStatus::Released, MilestoneStatus::Refunded] {
        let env = test_env();
        let ctx = setup(&env, true);
        lock_multisig(&env, &ctx);
        update_milestone(&env, &ctx.contract_id, |m| m.status = status);

        assert_eq!(
            try_refund(&env, &ctx, &ctx.admin, 0),
            Err(Error::InvalidStatus)
        );
        assert!(multisig_locked(&env, &ctx.contract_id));
    }
}

#[test]
fn override_refund_rejects_negative_amounts() {
    for (amount, released_amount) in [(-1_i128, 0_i128), (MILESTONE_AMOUNT, -1_i128)] {
        let env = test_env();
        let ctx = setup(&env, true);
        lock_multisig(&env, &ctx);
        update_milestone(&env, &ctx.contract_id, |m| {
            m.amount = amount;
            m.released_amount = released_amount;
        });

        assert_eq!(
            try_refund(&env, &ctx, &ctx.admin, 0),
            Err(Error::InvalidAmount)
        );
        assert!(multisig_locked(&env, &ctx.contract_id));
    }
}

#[test]
fn override_refund_rejects_released_amount_above_amount() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);
    update_milestone(&env, &ctx.contract_id, |m| {
        m.released_amount = MILESTONE_AMOUNT + 1;
        m.status = MilestoneStatus::PartiallyReleased;
    });

    assert_eq!(
        try_refund(&env, &ctx, &ctx.admin, 0),
        Err(Error::InvalidAmount)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

#[test]
fn override_refund_rejects_zero_remaining_balance() {
    let env = test_env();
    let ctx = setup(&env, true);
    lock_multisig(&env, &ctx);
    update_milestone(&env, &ctx.contract_id, |m| {
        m.released_amount = MILESTONE_AMOUNT;
        m.status = MilestoneStatus::PartiallyReleased;
    });

    assert_eq!(
        try_refund(&env, &ctx, &ctx.admin, 0),
        Err(Error::InvalidAmount)
    );
    assert!(multisig_locked(&env, &ctx.contract_id));
}

// ── multisig_approve ─────────────────────────────────────────────────────────

fn init_signers(env: &Env, ctx: &Ctx, signers: &Vec<Address>, threshold: u32) {
    mock_auth(
        env,
        &ctx.admin,
        &ctx.contract_id,
        "multisig_approval_init",
        (&ctx.admin, signers, &threshold).into_val(env),
    );
    ctx.escrow
        .multisig_approval_init(&ctx.admin, signers, &threshold);
}

fn try_approve(
    env: &Env,
    ctx: &Ctx,
    signer: &Address,
    proposal_id: u32,
) -> Result<MultiSigApprovalState, Error> {
    mock_auth(
        env,
        signer,
        &ctx.contract_id,
        "multisig_approve",
        (signer, &proposal_id).into_val(env),
    );
    match ctx.escrow.try_multisig_approve(signer, &proposal_id) {
        Ok(Ok(state)) => Ok(state),
        Err(Ok(error)) => Err(error),
        other => panic!("unexpected host result: {other:?}"),
    }
}

/// The approvals recorded on `proposal_id`'s record (issue #785), or `None`
/// when none have been recorded.
fn approval_bitmap(env: &Env, contract_id: &Address, proposal_id: u32) -> Option<u32> {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get::<_, crate::Proposal>(&DataKey::Proposal(proposal_id))
            .map(|record| record.approvals_bitmap)
            .filter(|bitmap| *bitmap != 0)
    })
}

#[test]
fn approve_rejects_missing_signature() {
    let env = test_env();
    let ctx = setup(&env, true);
    let signer = Address::generate(&env);
    init_signers(&env, &ctx, &vec![&env, signer.clone()], 1);

    let result = ctx
        .escrow
        .mock_auths(&[])
        .try_multisig_approve(&signer, &7u32);
    assert!(matches!(result, Err(Err(_))));
    assert_eq!(approval_bitmap(&env, &ctx.contract_id, 7), None);
}

#[test]
fn approve_rejects_when_multisig_not_initialised() {
    let env = test_env();
    let ctx = setup(&env, true);
    let signer = Address::generate(&env);

    assert_eq!(
        try_approve(&env, &ctx, &signer, 7).err(),
        Some(Error::NotInitialized)
    );
    assert_eq!(approval_bitmap(&env, &ctx.contract_id, 7), None);
}

#[test]
fn approve_rejects_non_signer() {
    let env = test_env();
    let ctx = setup(&env, true);
    let signer_a = Address::generate(&env);
    let signer_b = Address::generate(&env);
    init_signers(&env, &ctx, &vec![&env, signer_a, signer_b], 2);

    let stranger = Address::generate(&env);
    assert_eq!(
        try_approve(&env, &ctx, &stranger, 7).err(),
        Some(Error::Unauthorized)
    );
    assert_eq!(approval_bitmap(&env, &ctx.contract_id, 7), None);
}

#[test]
fn approve_accepts_empty_balance() {
    // Issue #787: recording an approval moves no tokens, so an unfunded
    // escrow is a valid place to collect signatures.
    let env = test_env();
    let ctx = setup(&env, false);
    let signer = Address::generate(&env);
    init_signers(&env, &ctx, &vec![&env, signer.clone()], 1);
    crate::tests::seed_admin_proposal(&env, &ctx.contract_id, 7);

    assert!(try_approve(&env, &ctx, &signer, 7).is_ok());
    assert_eq!(approval_bitmap(&env, &ctx.contract_id, 7), Some(1));
}

#[test]
fn approve_rejects_unknown_proposal() {
    // Issue #785: an ID the contract never assigned has no record to approve.
    let env = test_env();
    let ctx = setup(&env, true);
    let signer = Address::generate(&env);
    init_signers(&env, &ctx, &vec![&env, signer.clone()], 1);

    assert_eq!(
        try_approve(&env, &ctx, &signer, 7).err(),
        Some(Error::NoPendingAdminTransfer)
    );
    assert_eq!(approval_bitmap(&env, &ctx.contract_id, 7), None);
}

#[test]
fn approve_rejects_signer_past_bitmap_width() {
    let env = test_env();
    let ctx = setup(&env, true);

    // `multisig_approval_init` caps the set at 32 signers, so a 33rd can only
    // come from stored state. Index 32 does not fit the `u32` bitmap.
    let mut signers: Vec<Address> = Vec::new(&env);
    for _ in 0..33 {
        signers.push_back(Address::generate(&env));
    }
    let last = signers.get(32).unwrap();
    env.as_contract(&ctx.contract_id, || {
        env.storage().instance().set(
            &DataKey::MultiSigConfig,
            &MultiSigConfig(signers.clone(), 1),
        );
    });
    crate::tests::seed_admin_proposal(&env, &ctx.contract_id, 7);

    assert_eq!(
        try_approve(&env, &ctx, &last, 7).err(),
        Some(Error::ArithmeticOverflow)
    );
    assert_eq!(approval_bitmap(&env, &ctx.contract_id, 7), None);
}

// ── compute_ratio_allocation (formerly multisig_transfer_admin, #775) ───────

fn try_transfer(
    _env: &Env,
    ctx: &Ctx,
    total_amount: i128,
    ratios: &Vec<i128>,
) -> Result<Vec<i128>, Error> {
    match ctx
        .escrow
        .try_compute_ratio_allocation(&total_amount, ratios)
    {
        Ok(Ok(allocations)) => Ok(allocations),
        Err(Ok(error)) => Err(error),
        other => panic!("unexpected host result: {other:?}"),
    }
}

#[test]
fn transfer_admin_rejects_empty_ratios() {
    let env = test_env();
    let ctx = setup(&env, false);

    assert_eq!(
        try_transfer(&env, &ctx, 1_000, &Vec::new(&env)),
        Err(Error::InvalidRatio)
    );
}

#[test]
fn transfer_admin_rejects_too_many_ratios() {
    let env = test_env();
    let ctx = setup(&env, false);

    let mut ratios: Vec<i128> = Vec::new(&env);
    for _ in 0..256 {
        ratios.push_back(1);
    }
    assert_eq!(
        try_transfer(&env, &ctx, 1_000, &ratios),
        Err(Error::TooManyParties)
    );
}

#[test]
fn transfer_admin_rejects_negative_ratio() {
    let env = test_env();
    let ctx = setup(&env, false);

    assert_eq!(
        try_transfer(&env, &ctx, 1_000, &vec![&env, 1_i128, -1_i128]),
        Err(Error::InvalidRatio)
    );
}

#[test]
fn transfer_admin_rejects_ratio_sum_overflow() {
    let env = test_env();
    let ctx = setup(&env, false);

    assert_eq!(
        try_transfer(&env, &ctx, 1_000, &vec![&env, i128::MAX, 1_i128]),
        Err(Error::InvalidRatio)
    );
}

#[test]
fn transfer_admin_rejects_all_zero_ratios() {
    let env = test_env();
    let ctx = setup(&env, false);

    assert_eq!(
        try_transfer(&env, &ctx, 1_000, &vec![&env, 0_i128, 0_i128]),
        Err(Error::InvalidRatio)
    );
}

#[test]
fn transfer_admin_rejects_weighted_product_overflow() {
    let env = test_env();
    let ctx = setup(&env, false);

    assert_eq!(
        try_transfer(&env, &ctx, i128::MAX, &vec![&env, 2_i128, 1_i128]),
        Err(Error::InvalidAmount)
    );
}

#[test]
fn transfer_admin_hands_residue_to_lowest_index_on_tie() {
    let env = test_env();
    let ctx = setup(&env, false);

    assert_eq!(
        try_transfer(&env, &ctx, 10, &vec![&env, 1_i128, 1_i128, 1_i128]),
        Ok(vec![&env, 4_i128, 3_i128, 3_i128])
    );
}
