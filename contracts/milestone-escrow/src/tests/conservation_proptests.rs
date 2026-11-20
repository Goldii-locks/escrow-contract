extern crate std;
use std::vec;
use std::vec::Vec;

use super::*;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{EnvTestConfig, Ledger},
    Address, Env,
};

const MAX_MILESTONES: usize = 4;

#[derive(Clone, Debug)]
enum Operation {
    Deliver {
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
    ClaimAutoRelease {
        milestone_idx: u32,
        advance_time: u64,
        wrong_caller: bool,
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
    let max_idx = milestone_count + 1;
    prop_oneof![
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| {
            Operation::Deliver {
                milestone_idx,
                wrong_caller,
            }
        }),
        (0..max_idx, -100i128..500_000i128, any::<bool>()).prop_map(
            |(milestone_idx, amount, wrong_caller)| {
                Operation::ApprovePartial {
                    milestone_idx,
                    amount,
                    wrong_caller,
                }
            }
        ),
        (0..max_idx, any::<bool>()).prop_map(|(milestone_idx, wrong_caller)| {
            Operation::Approve {
                milestone_idx,
                wrong_caller,
            }
        }),
        (0..max_idx, 0u64..1_500_000u64, any::<bool>()).prop_map(
            |(milestone_idx, advance_time, wrong_caller)| {
                Operation::ClaimAutoRelease {
                    milestone_idx,
                    advance_time,
                    wrong_caller,
                }
            }
        ),
        (0..max_idx, 0u8..4u8).prop_map(|(milestone_idx, caller_choice)| {
            Operation::Dispute {
                milestone_idx,
                caller_choice,
            }
        }),
        (0..max_idx, any::<bool>(), any::<bool>()).prop_map(
            |(milestone_idx, release_to_freelancer, wrong_caller)| {
                Operation::Resolve {
                    milestone_idx,
                    release_to_freelancer,
                    wrong_caller,
                }
            }
        ),
        (0..max_idx, 0u32..15_000u32, any::<bool>()).prop_map(
            |(milestone_idx, client_refund_bps, wrong_caller)| {
                Operation::Split {
                    milestone_idx,
                    client_refund_bps,
                    wrong_caller,
                }
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

fn execute_operation(
    client: &MilestoneEscrowClient<'_>,
    env: &Env,
    op: &Operation,
    parties: &TestParties<'_>,
) -> bool {
    match op {
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

fn run_state_machine_case(amounts: Vec<i128>, operations: Vec<Operation>) {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let sdk_amounts = {
        let mut v = soroban_sdk::Vec::new(&env);
        for &amt in amounts.iter() {
            v.push_back(amt);
        }
        v
    };

    let total_funded: i128 = amounts.iter().sum();
    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, sdk_amounts);

    let unauthorized_addr = Address::generate(&env);
    let token_client = token::Client::new(&env, &token_id);

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
        total_funded,
        "Initial escrow balance must equal total_funded"
    );

    let mut trackers: Vec<MilestoneTracker> = amounts
        .iter()
        .map(|&amt| MilestoneTracker {
            initial_amount: amt,
            released_amount: 0,
            refunded_amount: 0,
            terminal_status: None,
        })
        .collect();

    for op in operations {
        let pre_escrow = token_client.balance(&contract_id);
        let pre_client = token_client.balance(&client_addr);
        let pre_freelancer = token_client.balance(&freelancer_addr);
        let pre_admin = token_client.balance(&admin_addr);

        let success = execute_operation(&client, &env, &op, &parties);

        let post_escrow = token_client.balance(&contract_id);
        let post_client = token_client.balance(&client_addr);
        let post_freelancer = token_client.balance(&freelancer_addr);
        let post_admin = token_client.balance(&admin_addr);

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
        } else {
            let delta_freelancer = post_freelancer - pre_freelancer;
            let delta_client = post_client - pre_client;

            match op {
                Operation::Deliver { .. } => {}
                Operation::ApprovePartial { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        if tr.released_amount == tr.initial_amount {
                            tr.terminal_status = Some(MilestoneStatus::Released);
                        }
                    }
                }
                Operation::Approve { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.terminal_status = Some(MilestoneStatus::Released);
                    }
                }
                Operation::ClaimAutoRelease { milestone_idx, .. } => {
                    if let Some(tr) = trackers.get_mut(milestone_idx as usize) {
                        tr.released_amount += delta_freelancer;
                        tr.terminal_status = Some(MilestoneStatus::Released);
                    }
                }
                Operation::Dispute { .. } => {}
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
                Operation::Cancel { .. } => {}
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
        let refunded_to_client = post_client - initial_client_balance;
        let fees = post_admin - initial_admin_balance;

        assert_eq!(
            post_escrow,
            total_funded - paid_to_freelancer - refunded_to_client - fees,
            "Core conservation invariant violated: escrow balance mismatch"
        );

        let current_job = client.get_job();
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
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn conservation_of_funds_lifecycle_proptest(
        amounts in job_amounts_strategy(),
        ops in prop::collection::vec(
            (0u32..MAX_MILESTONES as u32).prop_flat_map(operation_strategy),
            5..=20
        )
    ) {
        run_state_machine_case(amounts, ops);
    }
}

#[test]
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

    // admin_override_refund only intervenes on a genuinely blocked milestone
    // (Disputed, or an active cancel lock), so raise a dispute first.
    client.raise_dispute(&client_addr, &0u32, &dispute_reason_hash(&env));
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
    let client_balance_before = token_client.balance(&client_addr);

    client.admin_override_refund(&admin_addr, &0u32);

    let milestone = client.get_job().milestones.get(0).unwrap();
    let refunded_to_client = token_client.balance(&client_addr) - client_balance_before;
    assert_eq!(refunded_to_client, 600i128);
    assert_eq!(milestone.status, MilestoneStatus::Refunded);
    assert_eq!(milestone.released_amount, 400i128);

    assert!(
        milestone.released_amount + refunded_to_client <= milestone.amount,
        "Failed core invariant: contract released_amount ({}) + refunded_to_client ({}) = {} > amount ({})",
        milestone.released_amount,
        refunded_to_client,
        milestone.released_amount + refunded_to_client,
        milestone.amount
    );
}
