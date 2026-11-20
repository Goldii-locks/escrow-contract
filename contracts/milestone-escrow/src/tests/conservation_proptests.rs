extern crate std;
use std::vec;
use std::vec::Vec;

use super::*;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, IssuerFlags, Ledger},
    token, Address, BytesN, Env,
};

const MAX_MILESTONES: usize = 4;
const DEFAULT_AUTO_RELEASE_SECONDS: u64 = 604_800;

#[derive(Clone, Debug)]
enum Operation {
    AcceptJob {
        wrong_caller: bool,
    },
    AcceptArbiter {
        wrong_caller: bool,
    },
    FundMilestone {
        milestone_idx: u32,
        wrong_caller: bool,
    },
    SetMilestoneDueDates {
        due_offsets: Vec<u64>,
        wrong_caller: bool,
    },
    ReclaimOverdue {
        milestone_idx: u32,
        advance_time: u64,
        wrong_caller: bool,
    },
    Deliver {
        milestone_idx: u32,
        wrong_caller: bool,
    },
    RequestRevision {
        milestone_idx: u32,
        wrong_caller: bool,
    },
    ApprovePartial {
        milestone_idx: u32,
        amount: i128,
        wrong_caller: bool,
    },
    Approve {
        milestone_idx: u32,
        wrong_caller: bool,
    },
    ApproveMilestones {
        indices: Vec<u32>,
        wrong_caller: bool,
    },
    ClaimAutoRelease {
        milestone_idx: u32,
        advance_time: u64,
        wrong_caller: bool,
    },
    AddMilestone {
        amount: i128,
        wrong_caller: bool,
    },
    ReduceMilestone {
        milestone_idx: u32,
        new_amount: i128,
        wrong_caller: bool,
    },
    WithdrawUnaccepted {
        advance_time: u64,
        wrong_caller: bool,
    },
    ToggleAuthorized {
        party_choice: u8,
        authorized: bool,
    },
    Withdraw {
        caller_choice: u8,
    },
    DonateExcess {
        amount: i128,
    },
    SweepExcess {
        caller_choice: u8,
    },
    Dispute {
        milestone_idx: u32,
        caller_choice: u8,
    },
    Resolve {
        milestone_idx: u32,
        release_to_freelancer: bool,
        wrong_caller: bool,
    },
    Split {
        milestone_idx: u32,
        client_refund_bps: u32,
        wrong_caller: bool,
    },
    Cancel {
        caller_choice: u8,
    },
    AdminOverrideRelease {
        milestone_idx: u32,
        wrong_caller: bool,
    },
    AdminOverrideRefund {
        milestone_idx: u32,
        wrong_caller: bool,
    },
    AdminOverrideCancelRelease {
        wrong_caller: bool,
    },
    AdminOverrideCancelRefund {
        wrong_caller: bool,
    },
}

#[derive(Clone, Debug)]
struct MilestoneTracker {
    initial_amount: i128,
    released_amount: i128,
    refunded_amount: i128,
    approved_amount: i128,
    delivered_on_time: bool,
    disputed: bool,
    terminal_status: Option<MilestoneStatus>,
}

struct TestParties<'a> {
    client: &'a Address,
    freelancer: &'a Address,
    arbiter: &'a Address,
    admin: &'a Address,
    unauthorized: &'a Address,
}

fn operation_strategy(milestone_count: u32) -> impl Strategy<Value = Operation> {
    let max_idx = milestone_count + 2;
    prop_oneof![
        any::<bool>().prop_map(|wrong_caller| Operation::AcceptJob { wrong_caller }),
        any::<bool>().prop_map(|wrong_caller| Operation::AcceptArbiter { wrong_caller }),
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| {
            Operation::FundMilestone {
                milestone_idx,
                wrong_caller,
            }
        }),
        (
            prop::collection::vec(prop_oneof![Just(0u64), 100u64..100_000u64], 1..=MAX_MILESTONES),
            any::<bool>(),
        )
            .prop_map(|(due_offsets, wrong_caller)| Operation::SetMilestoneDueDates {
                due_offsets,
                wrong_caller,
            }),
        (0..max_idx, 0u64..200_000u64, any::<bool>()).prop_map(
            |(milestone_idx, advance_time, wrong_caller)| Operation::ReclaimOverdue {
                milestone_idx,
                advance_time,
                wrong_caller,
            }
        ),
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| Operation::Deliver {
            milestone_idx,
            wrong_caller,
        }),
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| {
            Operation::RequestRevision {
                milestone_idx,
                wrong_caller,
            }
        }),
        (0..max_idx, -100i128..500_000i128, any::<bool>()).prop_map(
            |(milestone_idx, amount, wrong_caller)| Operation::ApprovePartial {
                milestone_idx,
                amount,
                wrong_caller,
            }
        ),
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| Operation::Approve {
            milestone_idx,
            wrong_caller,
        }),
        (prop::collection::vec(0..max_idx, 1..=3), any::<bool>()).prop_map(
            |(indices, wrong_caller)| Operation::ApproveMilestones {
                indices,
                wrong_caller,
            }
        ),
        (0..max_idx, 0u64..1_500_000u64, any::<bool>()).prop_map(
            |(milestone_idx, advance_time, wrong_caller)| Operation::ClaimAutoRelease {
                milestone_idx,
                advance_time,
                wrong_caller,
            }
        ),
        (1i128..100_000i128, any::<bool>()).prop_map(|(amount, wrong_caller)| {
            Operation::AddMilestone {
                amount,
                wrong_caller,
            }
        }),
        (0..max_idx, 1i128..100_000i128, any::<bool>()).prop_map(
            |(milestone_idx, new_amount, wrong_caller)| Operation::ReduceMilestone {
                milestone_idx,
                new_amount,
                wrong_caller,
            }
        ),
        (0u64..1_000_000u64, any::<bool>()).prop_map(|(advance_time, wrong_caller)| {
            Operation::WithdrawUnaccepted {
                advance_time,
                wrong_caller,
            }
        }),
        (0u8..2u8, any::<bool>()).prop_map(|(party_choice, authorized)| {
            Operation::ToggleAuthorized {
                party_choice,
                authorized,
            }
        }),
        (0u8..3u8).prop_map(|caller_choice| Operation::Withdraw { caller_choice }),
        (1i128..50_000i128).prop_map(|amount| Operation::DonateExcess { amount }),
        (0u8..3u8).prop_map(|caller_choice| Operation::SweepExcess { caller_choice }),
        (0..max_idx, 0u8..4u8).prop_map(|(milestone_idx, caller_choice)| Operation::Dispute {
            milestone_idx,
            caller_choice,
        }),
        (0..max_idx, any::<bool>(), any::<bool>()).prop_map(
            |(milestone_idx, release_to_freelancer, wrong_caller)| Operation::Resolve {
                milestone_idx,
                release_to_freelancer,
                wrong_caller,
            }
        ),
        (0..max_idx, 0u32..15_000u32, any::<bool>()).prop_map(
            |(milestone_idx, client_refund_bps, wrong_caller)| Operation::Split {
                milestone_idx,
                client_refund_bps,
                wrong_caller,
            }
        ),
        (0u8..4u8).prop_map(|caller_choice| Operation::Cancel { caller_choice }),
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| {
            Operation::AdminOverrideRelease {
                milestone_idx,
                wrong_caller,
            }
        }),
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| {
            Operation::AdminOverrideRefund {
                milestone_idx,
                wrong_caller,
            }
        }),
        any::<bool>()
            .prop_map(|wrong_caller| Operation::AdminOverrideCancelRelease { wrong_caller }),
        any::<bool>()
            .prop_map(|wrong_caller| Operation::AdminOverrideCancelRefund { wrong_caller }),
    ]
}

fn job_amounts_strategy() -> impl Strategy<Value = Vec<i128>> {
    prop::collection::vec(1_000i128..200_000i128, 1..=MAX_MILESTONES)
}

fn read_total_owed(env: &Env, contract_id: &Address) -> i128 {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get(&DataKey::TotalOwed)
            .unwrap_or(0)
    })
}

fn read_owed(env: &Env, contract_id: &Address, addr: &Address) -> i128 {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::Owed(addr.clone()))
            .unwrap_or(0)
    })
}

fn read_yield_accrued(env: &Env, contract_id: &Address) -> i128 {
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0)
    })
}

fn execute_operation(
    client: &MilestoneEscrowClient<'_>,
    env: &Env,
    token_admin: &token::StellarAssetClient<'_>,
    token_id: &Address,
    contract_id: &Address,
    op: &Operation,
    parties: &TestParties<'_>,
) -> bool {
    match op {
        Operation::AcceptJob { wrong_caller } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.freelancer
            };
            client.try_accept_job(caller).is_ok()
        }
        Operation::AcceptArbiter { wrong_caller } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.arbiter
            };
            client.try_accept_arbiter(caller).is_ok()
        }
        Operation::FundMilestone {
            milestone_idx,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            client.try_fund_milestone(caller, milestone_idx).is_ok()
        }
        Operation::SetMilestoneDueDates {
            due_offsets,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            let now = env.ledger().timestamp();
            let mut sdk_dates = soroban_sdk::Vec::new(env);
            let count = client.get_job().milestones.len();
            for i in 0..count {
                let offset = due_offsets.get(i as usize).copied().unwrap_or(0);
                if offset == 0 {
                    sdk_dates.push_back(0);
                } else {
                    sdk_dates.push_back(now.saturating_add(offset));
                }
            }
            client
                .try_set_milestone_due_dates(caller, &sdk_dates)
                .is_ok()
        }
        Operation::ReclaimOverdue {
            milestone_idx,
            advance_time,
            wrong_caller,
        } => {
            env.ledger().with_mut(|li| {
                li.timestamp = li.timestamp.saturating_add(*advance_time);
            });
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            client.try_reclaim_overdue(caller, milestone_idx).is_ok()
        }
        Operation::Deliver {
            milestone_idx,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.freelancer
            };
            client.try_mark_delivered(caller, milestone_idx).is_ok()
        }
        Operation::RequestRevision {
            milestone_idx,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            let note = BytesN::from_array(env, &[0x22u8; 32]);
            client
                .try_request_revision(caller, milestone_idx, &note)
                .is_ok()
        }
        Operation::ApprovePartial {
            milestone_idx,
            amount,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            client
                .try_approve_partial(caller, milestone_idx, amount)
                .is_ok()
        }
        Operation::Approve {
            milestone_idx,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            client.try_approve_milestone(caller, milestone_idx).is_ok()
        }
        Operation::ApproveMilestones {
            indices,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            let mut sdk_indices = soroban_sdk::Vec::new(env);
            for &idx in indices.iter() {
                sdk_indices.push_back(idx);
            }
            client
                .try_approve_milestones(caller, &sdk_indices)
                .is_ok()
        }
        Operation::ClaimAutoRelease {
            milestone_idx,
            advance_time,
            wrong_caller,
        } => {
            env.ledger().with_mut(|li| {
                li.timestamp = li.timestamp.saturating_add(*advance_time);
            });
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.freelancer
            };
            client.try_claim_auto_release(caller, milestone_idx).is_ok()
        }
        Operation::AddMilestone {
            amount,
            wrong_caller,
        } => {
            let c = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            client
                .try_add_milestone(c, parties.freelancer, amount)
                .is_ok()
        }
        Operation::ReduceMilestone {
            milestone_idx,
            new_amount,
            wrong_caller,
        } => {
            let c = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            client
                .try_reduce_milestone(c, parties.freelancer, milestone_idx, new_amount)
                .is_ok()
        }
        Operation::WithdrawUnaccepted {
            advance_time,
            wrong_caller,
        } => {
            env.ledger().with_mut(|li| {
                li.timestamp = li.timestamp.saturating_add(*advance_time);
            });
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.client
            };
            client.try_withdraw_unaccepted(caller).is_ok()
        }
        Operation::ToggleAuthorized {
            party_choice,
            authorized,
        } => {
            let target = if *party_choice == 0 {
                parties.client
            } else {
                parties.freelancer
            };
            token_admin.set_authorized(target, authorized);
            true
        }
        Operation::Withdraw { caller_choice } => {
            let caller = match caller_choice {
                0 => parties.client,
                1 => parties.freelancer,
                _ => parties.unauthorized,
            };
            client.try_withdraw(caller).is_ok()
        }
        Operation::DonateExcess { amount } => {
            token_admin.mint(contract_id, amount);
            true
        }
        Operation::SweepExcess { caller_choice } => {
            let caller = match caller_choice {
                0 => parties.client,
                1 => parties.admin,
                _ => parties.unauthorized,
            };
            client.try_sweep_excess(caller, token_id).is_ok()
        }
        Operation::Dispute {
            milestone_idx,
            caller_choice,
        } => {
            let caller = match caller_choice {
                0 => parties.client,
                1 => parties.freelancer,
                2 => parties.arbiter,
                _ => parties.unauthorized,
            };
            client
                .try_raise_dispute(caller, milestone_idx, &dispute_reason_hash(env))
                .is_ok()
        }
        Operation::Resolve {
            milestone_idx,
            release_to_freelancer,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.arbiter
            };
            client
                .try_resolve_dispute(
                    caller,
                    milestone_idx,
                    release_to_freelancer,
                    &decision_hash(env),
                )
                .is_ok()
        }
        Operation::Split {
            milestone_idx,
            client_refund_bps,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.arbiter
            };
            client
                .try_apply_dispute_arbitration_split(
                    caller,
                    milestone_idx,
                    client_refund_bps,
                    &decision_hash(env),
                )
                .is_ok()
        }
        Operation::Cancel { caller_choice } => {
            let caller = match caller_choice {
                0 => parties.client,
                1 => parties.freelancer,
                _ => parties.unauthorized,
            };
            client.try_cancel_escrow(caller).is_ok()
        }
        Operation::AdminOverrideRelease {
            milestone_idx,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.admin
            };
            client
                .try_admin_override_release(caller, milestone_idx)
                .is_ok()
        }
        Operation::AdminOverrideRefund {
            milestone_idx,
            wrong_caller,
        } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.admin
            };
            client
                .try_admin_override_refund(caller, milestone_idx)
                .is_ok()
        }
        Operation::AdminOverrideCancelRelease { wrong_caller } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.admin
            };
            client.try_admin_override_cancel_release(caller).is_ok()
        }
        Operation::AdminOverrideCancelRefund { wrong_caller } => {
            let caller = if *wrong_caller {
                parties.unauthorized
            } else {
                parties.admin
            };
            client.try_admin_override_cancel_refund(caller).is_ok()
        }
    }
}

fn run_state_machine_case(
    amounts: Vec<i128>,
    per_milestone: bool,
    accept_on_setup: bool,
    operations: Vec<Operation>,
) {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let unauthorized_addr = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(admin_addr.clone());
    sac.issuer().set_flag(IssuerFlags::RevocableFlag);
    let token_id = sac.address();
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    let token_client = token::Client::new(&env, &token_id);

    let total_initial: i128 = amounts.iter().sum();
    // Mint generous balance so AddMilestone / FundMilestone can succeed.
    token_admin.mint(&client_addr, &(total_initial + 1_000_000));

    let sdk_amounts = {
        let mut v = soroban_sdk::Vec::new(&env);
        for &amt in amounts.iter() {
            v.push_back(amt);
        }
        v
    };
    let spec_hashes = spec_hashes_for(&sdk_amounts);
    let funding_mode = if per_milestone {
        FundingMode::PerMilestone
    } else {
        FundingMode::Upfront
    };

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize_with_funding_mode(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &DEFAULT_AUTO_RELEASE_SECONDS,
        &sdk_amounts,
        &spec_hashes,
        &funding_mode,
    );

    if accept_on_setup {
        client.accept_job(&freelancer_addr);
        client.accept_arbiter(&arbiter_addr);
    }

    let mut total_deposited: i128 = 0;
    if !per_milestone {
        client.fund(&client_addr);
        total_deposited = total_initial;
    }

    let parties = TestParties {
        client: &client_addr,
        freelancer: &freelancer_addr,
        arbiter: &arbiter_addr,
        admin: &admin_addr,
        unauthorized: &unauthorized_addr,
    };

    let initial_client_balance = token_client.balance(&client_addr);
    let initial_freelancer_balance = token_client.balance(&freelancer_addr);
    let initial_admin_balance = token_client.balance(&admin_addr);

    assert_eq!(
        token_client.balance(&contract_id),
        total_deposited,
        "Initial escrow balance must equal total_deposited"
    );

    let mut trackers: Vec<MilestoneTracker> = amounts
        .iter()
        .map(|&amt| MilestoneTracker {
            initial_amount: amt,
            released_amount: 0,
            refunded_amount: 0,
            approved_amount: 0,
            delivered_on_time: false,
            disputed: false,
            terminal_status: None,
        })
        .collect();

    let mut stray_excess: i128 = 0;
    let mut stray_excess_added: i128 = 0;
    let mut cancel_client_approved = false;
    let mut cancel_freelancer_approved = false;
    let mut both_cancelled = false;

    for op in operations {
        let pre_escrow = token_client.balance(&contract_id);
        let pre_client = token_client.balance(&client_addr);
        let pre_freelancer = token_client.balance(&freelancer_addr);
        let pre_admin = token_client.balance(&admin_addr);
        let pre_total_owed = read_total_owed(&env, &contract_id);
        let pre_client_owed = read_owed(&env, &contract_id, &client_addr);
        let pre_freelancer_owed = read_owed(&env, &contract_id, &freelancer_addr);

        let success = execute_operation(
            &client,
            &env,
            &token_admin,
            &token_id,
            &contract_id,
            &op,
            &parties,
        );

        let post_escrow = token_client.balance(&contract_id);
        let post_client = token_client.balance(&client_addr);
        let post_freelancer = token_client.balance(&freelancer_addr);
        let post_admin = token_client.balance(&admin_addr);
        let post_total_owed = read_total_owed(&env, &contract_id);
        let post_client_owed = read_owed(&env, &contract_id, &client_addr);
        let post_freelancer_owed = read_owed(&env, &contract_id, &freelancer_addr);
        let post_yield_accrued = read_yield_accrued(&env, &contract_id);

        if !success {
            assert_eq!(
                post_escrow, pre_escrow,
                "Failed call must not mutate escrow balance"
            );
            assert_eq!(
                post_client, pre_client,
                "Failed call must not mutate client balance"
            );
            assert_eq!(
                post_freelancer, pre_freelancer,
                "Failed call must not mutate freelancer balance"
            );
            assert_eq!(
                post_admin, pre_admin,
                "Failed call must not mutate admin balance"
            );
            assert_eq!(
                post_total_owed, pre_total_owed,
                "Failed call must not mutate TotalOwed"
            );
        } else {
            let delta_freelancer =
                (post_freelancer - pre_freelancer) + (post_freelancer_owed - pre_freelancer_owed);
            let delta_client = (post_client - pre_client) + (post_client_owed - pre_client_owed);

            match op {
                Operation::AcceptJob { .. }
                | Operation::AcceptArbiter { .. }
                | Operation::SetMilestoneDueDates { .. }
                | Operation::RequestRevision { .. }
                | Operation::ToggleAuthorized { .. }
                | Operation::Withdraw { .. } => {}
                Operation::FundMilestone { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get(milestone_idx as usize) {
                        total_deposited += tr.initial_amount;
                    }
                }
                Operation::ReclaimOverdue { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.refunded_amount += delta_client;
                        tr.terminal_status = Some(MilestoneStatus::Refunded);
                    }
                }
                Operation::Deliver { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        let due_at = client.get_milestone_due_at(&milestone_idx);
                        let now = env.ledger().timestamp();
                        if due_at == 0 || now <= due_at {
                            tr.delivered_on_time = true;
                        }
                    }
                }
                Operation::ApprovePartial {
                    milestone_idx,
                    amount,
                    ..
                } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.approved_amount += amount;
                        if tr.released_amount == tr.initial_amount {
                            tr.terminal_status = Some(MilestoneStatus::Released);
                        }
                    }
                }
                Operation::Approve { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.approved_amount += delta_freelancer;
                        tr.terminal_status = Some(MilestoneStatus::Released);
                    }
                }
                Operation::ApproveMilestones { ref indices, .. } => {
                    for &idx in indices.iter() {
                        if let Some(tr) = trackers.get_mut(idx as usize) {
                            let rem = tr.initial_amount - tr.released_amount;
                            tr.released_amount += rem;
                            tr.approved_amount += rem;
                            tr.terminal_status = Some(MilestoneStatus::Released);
                        }
                    }
                }
                Operation::ClaimAutoRelease { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.approved_amount += delta_freelancer;
                        tr.terminal_status = Some(MilestoneStatus::Released);
                    }
                }
                Operation::AddMilestone { amount, .. } => {
                    total_deposited += amount;
                    trackers.push(MilestoneTracker {
                        initial_amount: amount,
                        released_amount: 0,
                        refunded_amount: 0,
                        approved_amount: 0,
                        delivered_on_time: false,
                        disputed: false,
                        terminal_status: None,
                    });
                }
                Operation::ReduceMilestone {
                    milestone_idx,
                    new_amount,
                    ..
                } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        let refund = tr.initial_amount - new_amount;
                        tr.initial_amount = new_amount;
                        total_deposited -= refund;
                    }
                }
                Operation::WithdrawUnaccepted { .. } => {
                    let job = client.get_job();
                    for (i, tr) in trackers.iter_mut().enumerate() {
                        let ms = job.milestones.get(i as u32).unwrap();
                        if ms.status == MilestoneStatus::Refunded {
                            tr.terminal_status = Some(MilestoneStatus::Refunded);
                        }
                    }
                }
                Operation::DonateExcess { amount } => {
                    stray_excess += amount;
                    stray_excess_added += amount;
                }
                Operation::SweepExcess { .. } => {
                    let swept = post_client - pre_client;
                    stray_excess -= swept;
                }
                Operation::Dispute { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.disputed = true;
                    }
                }
                Operation::Resolve {
                    milestone_idx,
                    release_to_freelancer,
                    ..
                } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.refunded_amount += delta_client;
                        if release_to_freelancer {
                            tr.terminal_status = Some(MilestoneStatus::Released);
                        } else {
                            tr.terminal_status = Some(MilestoneStatus::Refunded);
                        }
                    }
                }
                Operation::Split { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.refunded_amount += delta_client;
                        if delta_freelancer > 0 && delta_client > 0 {
                            tr.terminal_status = Some(MilestoneStatus::Split);
                        } else if delta_freelancer > 0 {
                            tr.terminal_status = Some(MilestoneStatus::Released);
                        } else {
                            tr.terminal_status = Some(MilestoneStatus::Refunded);
                        }
                    }
                }
                Operation::Cancel { caller_choice } => {
                    if caller_choice == 0 {
                        cancel_client_approved = true;
                    } else if caller_choice == 1 {
                        cancel_freelancer_approved = true;
                    }
                    if cancel_client_approved && cancel_freelancer_approved {
                        both_cancelled = true;
                    }
                }
                Operation::AdminOverrideRelease { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.terminal_status = Some(MilestoneStatus::Released);
                    }
                }
                Operation::AdminOverrideRefund { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.refunded_amount += delta_client;
                        tr.terminal_status = Some(MilestoneStatus::Refunded);
                    }
                }
                Operation::AdminOverrideCancelRelease { .. } => {
                    let job = client.get_job();
                    for (i, tr) in trackers.iter_mut().enumerate() {
                        let ms = job.milestones.get(i as u32).unwrap();
                        if ms.status == MilestoneStatus::Released {
                            tr.terminal_status = Some(MilestoneStatus::Released);
                        }
                    }
                }
                Operation::AdminOverrideCancelRefund { .. } => {
                    let job = client.get_job();
                    for (i, tr) in trackers.iter_mut().enumerate() {
                        let ms = job.milestones.get(i as u32).unwrap();
                        if ms.status == MilestoneStatus::Refunded {
                            tr.terminal_status = Some(MilestoneStatus::Refunded);
                        }
                    }
                }
            }
        }

        let paid_to_freelancer = post_freelancer - initial_freelancer_balance;
        let refunded_to_client =
            (post_client - initial_client_balance) + (total_initial - total_deposited)
                - (total_initial - total_deposited);
        // Note: initial_client_balance was before `fund`, so `initial_client_balance - post_client`
        // is net outflow from client (deposits minus refunds/sweeps).
        let net_client_outflow = initial_client_balance - post_client;
        let fees = post_admin - initial_admin_balance;
        let _ = refunded_to_client;

        assert_eq!(
            post_escrow,
            net_client_outflow + stray_excess_added - paid_to_freelancer - fees,
            "Global conservation invariant violated: escrow balance mismatch"
        );

        let current_job = client.get_job();
        let mut funded_unreleased_remainders: i128 = 0;
        let mut total_approved: i128 = 0;

        for (i, tr) in trackers.iter().enumerate() {
            assert!(
                tr.released_amount + tr.refunded_amount <= tr.initial_amount,
                "Milestone {} settlement sum {} exceeds initial amount {}",
                i,
                tr.released_amount + tr.refunded_amount,
                tr.initial_amount
            );

            let on_chain_ms = current_job.milestones.get(i as u32).unwrap();
            if let Some(ref term) = tr.terminal_status {
                assert_eq!(
                    on_chain_ms.status, *term,
                    "Terminal milestone {} status mutated from {:?} to {:?}",
                    i, term, on_chain_ms.status
                );
            }

            let is_funded = current_job.funded || on_chain_ms.funded;
            let is_terminal = matches!(
                on_chain_ms.status,
                MilestoneStatus::Released | MilestoneStatus::Refunded | MilestoneStatus::Split
            );
            if is_funded && !is_terminal {
                funded_unreleased_remainders += on_chain_ms.amount - on_chain_ms.released_amount;
            }

            total_approved += tr.approved_amount;

            assert!(
                on_chain_ms.released_amount >= tr.approved_amount,
                "Milestone {} released_amount {} is less than approved_amount {}",
                i,
                on_chain_ms.released_amount,
                tr.approved_amount
            );

            if tr.delivered_on_time && !tr.disputed && !both_cancelled {
                assert_ne!(
                    on_chain_ms.status,
                    MilestoneStatus::Refunded,
                    "Freelancer lost delivered-on-time milestone {} without a dispute",
                    i
                );
            }
        }

        assert_eq!(
            post_escrow,
            funded_unreleased_remainders + post_total_owed + post_yield_accrued + stray_excess,
            "Obligation conservation invariant violated: balance {} != remainders {} + total_owed {} + yield {} + stray {}",
            post_escrow,
            funded_unreleased_remainders,
            post_total_owed,
            post_yield_accrued,
            stray_excess
        );

        assert!(
            paid_to_freelancer + post_freelancer_owed >= total_approved,
            "Freelancer paid ({}) + owed ({}) is less than total approved ({})",
            paid_to_freelancer,
            post_freelancer_owed,
            total_approved
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn conservation_of_funds_lifecycle_proptest(
        amounts in job_amounts_strategy(),
        per_milestone in any::<bool>(),
        accept_on_setup in any::<bool>(),
        ops in prop::collection::vec(
            (0u32..MAX_MILESTONES as u32).prop_flat_map(operation_strategy),
            5..=25
        )
    ) {
        run_state_machine_case(amounts, per_milestone, accept_on_setup, ops);
    }
}

#[test]
fn revision_then_overdue_reclaim_is_rejected() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(admin_addr.clone());
    let token_id = sac.address();
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    token_admin.mint(&client_addr, &1_000);

    let amounts = soroban_sdk::vec![&env, 1_000i128];
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &DEFAULT_AUTO_RELEASE_SECONDS,
        &amounts,
        &spec_hashes_for(&amounts),
    );
    client.set_milestone_due_dates(&client_addr, &soroban_sdk::vec![&env, 5_000u64]);
    client.accept_job(&freelancer_addr);
    client.accept_arbiter(&arbiter_addr);
    client.fund(&client_addr);

    // Freelancer delivers on time.
    env.ledger().set_timestamp(2_000);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Client requests a revision, returning the milestone to Pending.
    let note = BytesN::from_array(&env, &[0x33u8; 32]);
    client.request_revision(&client_addr, &0u32, &note);

    // Even after due_at + OVERDUE_GRACE_SECONDS, client cannot reclaim_overdue.
    env.ledger()
        .set_timestamp(5_000 + OVERDUE_GRACE_SECONDS + 1);
    assert_eq!(
        client.try_reclaim_overdue(&client_addr, &0u32),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn per_milestone_request_revision_does_not_fail_not_funded() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(admin_addr.clone());
    let token_id = sac.address();
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    token_admin.mint(&client_addr, &1_000);

    let amounts = soroban_sdk::vec![&env, 400i128, 600i128];
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize_with_funding_mode(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &DEFAULT_AUTO_RELEASE_SECONDS,
        &amounts,
        &spec_hashes_for(&amounts),
        &FundingMode::PerMilestone,
    );
    client.accept_job(&freelancer_addr);
    client.accept_arbiter(&arbiter_addr);
    client.fund_milestone(&client_addr, &0u32);

    client.mark_delivered(&freelancer_addr, &0u32);
    let note = BytesN::from_array(&env, &[0x44u8; 32]);
    client.request_revision(&client_addr, &0u32, &note);
    assert_eq!(client.get_revision_count(&0u32), 1);
}

#[test]
#[ignore = "catches known issue on main: admin_override_refund sets released_amount = amount even on client refund"]
fn catches_refund_accounting_through_released_amount_issue() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let amounts = {
        let mut v = soroban_sdk::Vec::new(&env);
        v.push_back(1_000i128);
        v
    };
    let (client_addr, freelancer_addr, _, admin_addr, token_id, _contract_id, client) =
        setup_funded_escrow(&env, amounts);

    let token_client = token::Client::new(&env, &token_id);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &400i128);

    assert_eq!(token_client.balance(&freelancer_addr), 400);

    client.admin_override_refund(&admin_addr, &0u32);

    let milestone = client.get_job().milestones.get(0).unwrap();
    let refunded_to_client = 600i128;

    assert!(
        milestone.released_amount + refunded_to_client <= milestone.amount,
        "Failed core invariant: contract released_amount ({}) + refunded_to_client ({}) = {} > amount ({})",
        milestone.released_amount,
        refunded_to_client,
        milestone.released_amount + refunded_to_client,
        milestone.amount
    );
}
