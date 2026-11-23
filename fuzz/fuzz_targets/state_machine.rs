#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use milestone_escrow::{
    EscrowClient, EscrowSettings, FundingMode, MilestoneStatus,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::StellarAssetClient,
    vec, Address, BytesN, Env,
};

#[derive(Arbitrary, Debug, Clone, Copy)]
pub enum Actor {
    Admin,
    Client,
    Freelancer,
    Arbiter,
    Stranger,
}

#[derive(Arbitrary, Debug, Clone)]
pub enum FuzzOp {
    AcceptJob,
    AcceptArbiter,
    Fund,
    FundMilestone { index: u32 },
    MarkDelivered { index: u32 },
    RequestRevision { index: u32, hash: [u8; 32] },
    ApproveMilestone { index: u32 },
    RaiseDispute { index: u32, hash: [u8; 32] },
    ResolveDispute { index: u32, release_to_freelancer: bool, hash: [u8; 32] },
    AdvanceTime { seconds: u64 },
    Withdraw,
    WithdrawUnaccepted,
    ReclaimOverdue { index: u32 },
}

#[derive(Arbitrary, Debug)]
pub struct FuzzScript {
    pub funding_mode_per_milestone: bool,
    pub operations: Vec<(Actor, FuzzOp)>,
}

fuzz_target!(|script: FuzzScript| {
    if script.operations.is_empty() || script.operations.len() > 100 {
        return;
    }

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);

    let admin = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let stranger = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin.clone());
    let token_client = StellarAssetClient::new(&env, &sac.address());

    // Mint test tokens to client
    token_client.mint(&client_addr, &100_000_000);

    let contract_id = env.register(milestone_escrow::MilestoneEscrow, ());
    let escrow = EscrowClient::new(&env, &contract_id);

    let funding_mode = if script.funding_mode_per_milestone {
        FundingMode::PerMilestone
    } else {
        FundingMode::Upfront
    };

    let settings = EscrowSettings {
        funding_mode,
        dispute_timeout_seconds: 2592000,
        arbiter_fee_bps: 0,
    };

    let milestone_amounts = vec![&env, 1_000_000i128, 2_000_000i128];
    let milestone_spec_hashes = vec![
        &env,
        BytesN::from_array(&env, &[0u8; 32]),
        BytesN::from_array(&env, &[1u8; 32]),
    ];

    let init_res = escrow.try_initialize(
        &admin,
        &client_addr,
        &freelancer,
        &arbiter,
        &sac.address(),
        &604800u64,
        &milestone_amounts,
        &milestone_spec_hashes,
        &settings,
    );
    if init_res.is_err() {
        return;
    }

    let actor_to_address = |actor: Actor| -> Address {
        match actor {
            Actor::Admin => admin.clone(),
            Actor::Client => client_addr.clone(),
            Actor::Freelancer => freelancer.clone(),
            Actor::Arbiter => arbiter.clone(),
            Actor::Stranger => stranger.clone(),
        }
    };

    // Track previous terminal milestone states to assert terminal immutability
    let mut prev_terminal: [Option<MilestoneStatus>; 2] = [None, None];

    for (actor, op) in script.operations {
        let caller = actor_to_address(actor);

        match op {
            FuzzOp::AcceptJob => {
                let _ = escrow.try_accept_job(&caller);
            }
            FuzzOp::AcceptArbiter => {
                let _ = escrow.try_accept_arbiter(&caller);
            }
            FuzzOp::Fund => {
                let _ = escrow.try_fund(&caller);
            }
            FuzzOp::FundMilestone { index } => {
                let _ = escrow.try_fund_milestone(&caller, &index);
            }
            FuzzOp::MarkDelivered { index } => {
                let _ = escrow.try_mark_delivered(&caller, &index);
            }
            FuzzOp::RequestRevision { index, hash } => {
                let note = BytesN::from_array(&env, &hash);
                let _ = escrow.try_request_revision(&caller, &index, &note);
            }
            FuzzOp::ApproveMilestone { index } => {
                let _ = escrow.try_approve_milestone(&caller, &index);
            }
            FuzzOp::RaiseDispute { index, hash } => {
                let reason = BytesN::from_array(&env, &hash);
                let _ = escrow.try_raise_dispute(&caller, &index, &reason);
            }
            FuzzOp::ResolveDispute {
                index,
                release_to_freelancer,
                hash,
            } => {
                let decision = BytesN::from_array(&env, &hash);
                let _ = escrow.try_resolve_dispute(
                    &caller,
                    &index,
                    &release_to_freelancer,
                    &decision,
                );
            }
            FuzzOp::AdvanceTime { seconds } => {
                let step = seconds.min(30 * 24 * 60 * 60);
                env.ledger().set_timestamp(env.ledger().timestamp() + step);
            }
            FuzzOp::Withdraw => {
                let _ = escrow.try_withdraw(&caller);
            }
            FuzzOp::WithdrawUnaccepted => {
                let _ = escrow.try_withdraw_unaccepted(&caller);
            }
            FuzzOp::ReclaimOverdue { index } => {
                let _ = escrow.try_reclaim_overdue(&caller, &index);
            }
        }

        // Post-operation invariant checks:
        if let Ok(job) = escrow.try_get_job() {
            // Invariant 1: Terminal status immutability
            for (idx, ms) in job.milestones.iter().enumerate() {
                if idx < 2 {
                    if let Some(prev) = prev_terminal[idx] {
                        assert_eq!(
                            ms.status, prev,
                            "Terminal milestone state changed"
                        );
                    } else if ms.status == MilestoneStatus::Released
                        || ms.status == MilestoneStatus::Refunded
                    {
                        prev_terminal[idx] = Some(ms.status);
                    }
                }
            }

            // Invariant 2: Released amount cannot exceed milestone total
            for ms in job.milestones.iter() {
                assert!(
                    ms.released_amount <= ms.amount,
                    "Released amount exceeded milestone amount"
                );
            }
        }
    }
});
