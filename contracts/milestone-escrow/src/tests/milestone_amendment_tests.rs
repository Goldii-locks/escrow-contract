#![cfg(test)]
//! Tests for `add_milestone` / `reduce_milestone` (issue #737) and
//! `increase_milestone` (issue #1020).
//!
//! Coverage required by the acceptance criteria:
//! * add with funding (client deposit, count/total update, event),
//! * reduce with refund (difference returned to client, event),
//! * rejection without both signatures (host-level revert, no mutation),
//! * rejection on non-`Pending` milestones and invalid inputs,
//! * the `MAX_MILESTONES` cap,
//! * the conservation invariant `contract balance == total_amount ==
//!   Σ milestone.amounts` across an amend sequence.

use crate::test::{setup_funded_escrow, setup_unfunded_escrow};
use crate::{
    all_event_tuples, DataKey, Error, FundingMode, JobMeta, MilestoneAddedEvent, MilestoneEscrow,
    MilestoneEscrowClient, MilestoneIncreasedEvent, MilestoneReducedEvent, MilestoneStatus,
    MAX_MILESTONES,
};
use soroban_sdk::testutils::{Address as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{symbol_short, token, vec, Address, Env, FromVal, IntoVal, Symbol, Val};

fn topic_count(env: &Env, name: Symbol) -> usize {
    let topic: Val = name.into_val(env);
    all_event_tuples(env)
        .iter()
        .filter(|e| {
            e.1.get(0)
                .map(|t| t.get_payload() == topic.get_payload())
                .unwrap_or(false)
        })
        .count()
}

fn last_event_payload<T>(env: &Env) -> T
where
    T: FromVal<Env, Val>,
{
    let events = all_event_tuples(env);
    let payload = events.last().expect("expected at least one event").2;
    T::from_val(env, &payload)
}

fn balance(env: &Env, tok: &Address, who: &Address) -> i128 {
    token::Client::new(env, tok).balance(who)
}

/// Read the stored `JobMeta` (instance storage) to observe `total_amount`
/// and `milestone_count`, which `get_job` does not surface.
fn job_meta(env: &Env, contract_id: &Address) -> JobMeta {
    env.as_contract(contract_id, || {
        env.storage()
            .instance()
            .get::<_, JobMeta>(&DataKey::Job)
            .expect("job meta must be stored")
    })
}

fn milestone_sum(client: &MilestoneEscrowClient<'_>) -> i128 {
    client.get_job().milestones.iter().map(|m| m.amount).sum()
}

// ── add_milestone ────────────────────────────────────────────────────────────

#[test]
fn add_milestone_appends_pending_milestone_and_updates_totals() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token_addr, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);
    token::StellarAssetClient::new(&env, &token_addr).mint(&client_addr, &500);

    assert_eq!(job_meta(&env, &contract_id).total_amount, 3_000);

    escrow.add_milestone(&client_addr, &freelancer_addr, &500_i128);

    let job = escrow.get_job();
    assert_eq!(job.milestones.len(), 3);
    let added = job.milestones.get(2).expect("third milestone exists");
    assert_eq!(added.amount, 500);
    assert_eq!(added.released_amount, 0);
    assert_eq!(added.status, MilestoneStatus::Pending);

    let meta = job_meta(&env, &contract_id);
    assert_eq!(meta.milestone_count, 3);
    assert_eq!(meta.total_amount, 3_500);
    assert_eq!(milestone_sum(&escrow), meta.total_amount);
}

#[test]
fn add_milestone_updates_balances_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    // Setup funds 3_000 (client left with 0), then mint 500 for the add.
    let (client_addr, freelancer_addr, _, _, token_addr, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);
    token::StellarAssetClient::new(&env, &token_addr).mint(&client_addr, &500);

    escrow.add_milestone(&client_addr, &freelancer_addr, &500_i128);

    // Read events immediately after the call — all_event_tuples only captures
    // the most recent contract invocation; any subsequent contract/token call
    // would clear the log.
    assert_eq!(topic_count(&env, symbol_short!("ms_add")), 1);
    let event: MilestoneAddedEvent = last_event_payload(&env);
    assert_eq!(event.total_amount, 3_500_i128);
    assert_eq!(event.client, client_addr);
    assert_eq!(event.freelancer, freelancer_addr);
    assert_eq!(event.milestone_index, 2);
    assert_eq!(event.amount, 500);
    assert_eq!(event.milestone_count, 3);

    assert_eq!(balance(&env, &token_addr, &client_addr), 0);
    assert_eq!(balance(&env, &token_addr, &contract_id), 3_500);
    // Conservation invariant: escrow balance == total_amount.
    let meta = job_meta(&env, &contract_id);
    assert_eq!(balance(&env, &token_addr, &contract_id), meta.total_amount);
}

#[test]
fn add_milestone_rejected_without_freelancer_signature() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "add_milestone",
        args: (&client_addr, &freelancer_addr, &500_i128).into_val(&env),
        sub_invokes: &[],
    };
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &client_addr,
            invoke: &invoke,
        }])
        .try_add_milestone(&client_addr, &freelancer_addr, &500_i128);

    // Missing second signature reverts at host level, not via a contract Error.
    assert!(
        matches!(result, Err(Err(_))),
        "single signature must revert at host level"
    );
    assert_eq!(escrow.get_job().milestones.len(), 1);
    assert_eq!(job_meta(&env, &contract_id).total_amount, 1_000);
    assert_eq!(topic_count(&env, symbol_short!("ms_add")), 0);
}

#[test]
fn add_milestone_rejected_without_client_signature() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "add_milestone",
        args: (&client_addr, &freelancer_addr, &500_i128).into_val(&env),
        sub_invokes: &[],
    };
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &freelancer_addr,
            invoke: &invoke,
        }])
        .try_add_milestone(&client_addr, &freelancer_addr, &500_i128);

    assert!(
        matches!(result, Err(Err(_))),
        "single signature must revert at host level"
    );
    assert_eq!(escrow.get_job().milestones.len(), 1);
    assert_eq!(job_meta(&env, &contract_id).total_amount, 1_000);
}

#[test]
fn add_milestone_rejects_third_party_addresses() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let fake_freelancer = Address::generate(&env);
    let result = escrow.try_add_milestone(&client_addr, &fake_freelancer, &500_i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let fake_client = Address::generate(&env);
    let result = escrow.try_add_milestone(&fake_client, &client_addr, &500_i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn add_milestone_requires_funded_job() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_unfunded_escrow(&env, vec![&env, 1_000_i128]);

    let result = escrow.try_add_milestone(&client_addr, &freelancer_addr, &500_i128);
    assert_eq!(result, Err(Ok(Error::NotFunded)));
}

#[test]
fn add_milestone_rejects_non_positive_amounts() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(
        escrow.try_add_milestone(&client_addr, &freelancer_addr, &0_i128),
        Err(Ok(Error::InvalidAmount))
    );
    let res = escrow.try_add_milestone(&client_addr, &freelancer_addr, &(-100_i128));
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
    assert_eq!(escrow.get_job().milestones.len(), 1);
}

#[test]
fn add_milestone_enforces_max_milestones_cap() {
    let env = Env::default();
    env.mock_all_auths();
    let mut amounts = vec![&env];
    for _ in 0..MAX_MILESTONES {
        amounts.push_back(1_i128);
    }
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    let res = escrow.try_add_milestone(&client_addr, &freelancer_addr, &10_i128);
    assert_eq!(res, Err(Ok(Error::TooManyMilestones)));
    assert_eq!(escrow.get_job().milestones.len(), MAX_MILESTONES);
}

// ── reduce_milestone ─────────────────────────────────────────────────────────

#[test]
fn reduce_milestone_refunds_difference_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token_addr, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);

    escrow.reduce_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);

    // Read events immediately — all_event_tuples only captures the most
    // recent contract invocation; any subsequent call would clear the log.
    assert_eq!(topic_count(&env, symbol_short!("ms_reduce")), 1);
    let event: MilestoneReducedEvent = last_event_payload(&env);
    assert_eq!(event.milestone_index, 0);
    assert_eq!(event.old_amount, 1_000);
    assert_eq!(event.new_amount, 600);
    assert_eq!(event.refund, 400);
    assert_eq!(event.total_amount, 2_600);
    assert_eq!(event.milestone_count, 2);

    let job = escrow.get_job();
    let milestone = job.milestones.get(0).expect("first milestone exists");
    assert_eq!(milestone.amount, 600);
    assert_eq!(milestone.status, MilestoneStatus::Pending);
    assert_eq!(job.milestones.len(), 2);

    let meta = job_meta(&env, &contract_id);
    assert_eq!(meta.total_amount, 2_600);
    assert_eq!(meta.milestone_count, 2);

    // Conservation: exactly the 400 difference moved back to the client.
    assert_eq!(balance(&env, &token_addr, &client_addr), 400);
    assert_eq!(balance(&env, &token_addr, &contract_id), 2_600);
}

#[test]
fn reduce_milestone_rejected_without_both_signatures() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token_addr, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "reduce_milestone",
        args: (&client_addr, &freelancer_addr, &0u32, &600_i128).into_val(&env),
        sub_invokes: &[],
    };
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &client_addr,
            invoke: &invoke,
        }])
        .try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);
    assert!(
        matches!(result, Err(Err(_))),
        "client-only signature must revert"
    );

    let result = escrow
        .mock_auths(&[MockAuth {
            address: &freelancer_addr,
            invoke: &invoke,
        }])
        .try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);
    assert!(
        matches!(result, Err(Err(_))),
        "freelancer-only signature must revert"
    );

    // No mutation, no refund.
    assert_eq!(job_meta(&env, &contract_id).total_amount, 1_000);
    assert_eq!(balance(&env, &token_addr, &contract_id), 1_000);
    assert_eq!(topic_count(&env, symbol_short!("ms_reduce")), 0);
}

#[test]
fn reduce_milestone_rejects_non_pending_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    let res = escrow.try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));

    escrow.approve_milestone(&client_addr, &0u32);
    let res = escrow.try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));

    assert_eq!(milestone_sum(&escrow), 3_000);
}

#[test]
fn reduce_milestone_rejects_invalid_amounts() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Not a reduction (equal or larger).
    let res = escrow.try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &1_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    let res = escrow.try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &1_500_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    // Zero / negative new amount would delete the milestone's value.
    let res = escrow.try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &0_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    let res = escrow.try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &(-1_i128));
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    assert_eq!(milestone_sum(&escrow), 1_000);
}

#[test]
fn reduce_milestone_rejects_bad_index_third_party_and_unfunded() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let res = escrow.try_reduce_milestone(&client_addr, &freelancer_addr, &1u32, &600_i128);
    assert_eq!(res, Err(Ok(Error::InvalidMilestone)));

    let fake = Address::generate(&env);
    let res = escrow.try_reduce_milestone(&fake, &freelancer_addr, &0u32, &600_i128);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));

    let env2 = Env::default();
    env2.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, unfunded) =
        setup_unfunded_escrow(&env2, vec![&env2, 1_000_i128]);
    let res = unfunded.try_reduce_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);
    assert_eq!(res, Err(Ok(Error::NotFunded)));
}

// ── conservation across an amend sequence ────────────────────────────────────

#[test]
fn amendment_sequence_preserves_conservation_invariant() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token_addr, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);
    token::StellarAssetClient::new(&env, &token_addr).mint(&client_addr, &700);

    // add 700, then reduce the appended milestone back to 200.
    escrow.add_milestone(&client_addr, &freelancer_addr, &700_i128);
    escrow.reduce_milestone(&client_addr, &freelancer_addr, &2u32, &200_i128);

    let meta = job_meta(&env, &contract_id);
    assert_eq!(meta.total_amount, 3_200);
    assert_eq!(milestone_sum(&escrow), meta.total_amount);
    // Contract holds exactly total_amount: 3_000 funded + 700 added − 500 refunded.
    assert_eq!(balance(&env, &token_addr, &contract_id), 3_200);
    // Client: 3_000 minted − 3_000 funded + 700 minted − 700 deposited + 500 refunded.
    assert_eq!(balance(&env, &token_addr, &client_addr), 500);

    // A further reduce on a Pending milestone still balances.
    escrow.reduce_milestone(&client_addr, &freelancer_addr, &0u32, &400_i128);
    let meta = job_meta(&env, &contract_id);
    assert_eq!(milestone_sum(&escrow), meta.total_amount);
    assert_eq!(balance(&env, &token_addr, &contract_id), meta.total_amount);
}

// ── increase_milestone ───────────────────────────────────────────────────────

/// A per-milestone escrow over `[1_000, 2_000]`, accepted but with no
/// milestone funded. The client holds 3_500.
fn setup_per_milestone(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);
    let token_addr = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    token::StellarAssetClient::new(env, &token_addr).mint(&client_addr, &3_500);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    let amounts = vec![env, 1_000_i128, 2_000_i128];
    escrow.initialize_with_funding_mode(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_addr,
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        &FundingMode::PerMilestone,
    );
    escrow.accept_job(&freelancer_addr);
    escrow.accept_arbiter(&arbiter_addr);
    (
        client_addr,
        freelancer_addr,
        token_addr,
        contract_id,
        escrow,
    )
}

#[test]
fn increase_milestone_upfront_collects_difference_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token_addr, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);
    token::StellarAssetClient::new(&env, &token_addr).mint(&client_addr, &500);

    escrow.increase_milestone(&client_addr, &freelancer_addr, &0u32, &1_500_i128);

    assert_eq!(topic_count(&env, symbol_short!("ms_incr")), 1);
    let event: MilestoneIncreasedEvent = last_event_payload(&env);
    assert_eq!(event.milestone_index, 0);
    assert_eq!(event.old_amount, 1_000);
    assert_eq!(event.new_amount, 1_500);
    assert_eq!(event.total_amount, 3_500);
    assert_eq!(event.milestone_count, 2);

    let milestone = escrow
        .get_job()
        .milestones
        .get(0)
        .expect("first milestone exists");
    assert_eq!(milestone.amount, 1_500);
    assert_eq!(milestone.status, MilestoneStatus::Pending);

    let meta = job_meta(&env, &contract_id);
    assert_eq!(meta.total_amount, 3_500);
    assert_eq!(milestone_sum(&escrow), meta.total_amount);
    assert_eq!(balance(&env, &token_addr, &client_addr), 0);
    assert_eq!(balance(&env, &token_addr, &contract_id), 3_500);
}

#[test]
fn increase_milestone_per_milestone_funded_collects_difference() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, token_addr, contract_id, escrow) = setup_per_milestone(&env);
    escrow.fund_milestone(&client_addr, &0u32);

    escrow.increase_milestone(&client_addr, &freelancer_addr, &0u32, &1_500_i128);

    let milestone = escrow
        .get_job()
        .milestones
        .get(0)
        .expect("first milestone exists");
    assert_eq!(milestone.amount, 1_500);
    assert_eq!(job_meta(&env, &contract_id).total_amount, 3_500);
    assert_eq!(balance(&env, &token_addr, &client_addr), 2_000);
    assert_eq!(balance(&env, &token_addr, &contract_id), 1_500);
}

#[test]
fn increase_milestone_per_milestone_unfunded_only_changes_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, token_addr, contract_id, escrow) = setup_per_milestone(&env);

    escrow.increase_milestone(&client_addr, &freelancer_addr, &1u32, &2_500_i128);

    assert_eq!(topic_count(&env, symbol_short!("ms_incr")), 1);
    let milestone = escrow
        .get_job()
        .milestones
        .get(1)
        .expect("second milestone exists");
    assert_eq!(milestone.amount, 2_500);
    assert_eq!(job_meta(&env, &contract_id).total_amount, 3_500);
    assert_eq!(balance(&env, &token_addr, &client_addr), 3_500);
    assert_eq!(balance(&env, &token_addr, &contract_id), 0);
}

#[test]
fn increase_milestone_rejects_non_pending_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    let res = escrow.try_increase_milestone(&client_addr, &freelancer_addr, &0u32, &1_500_i128);
    assert_eq!(res, Err(Ok(Error::InvalidStatus)));

    assert_eq!(milestone_sum(&escrow), 3_000);
}

#[test]
fn increase_milestone_rejected_without_both_signatures() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token_addr, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    token::StellarAssetClient::new(&env, &token_addr).mint(&client_addr, &500);

    let invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "increase_milestone",
        args: (&client_addr, &freelancer_addr, &0u32, &1_500_i128).into_val(&env),
        sub_invokes: &[],
    };
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &client_addr,
            invoke: &invoke,
        }])
        .try_increase_milestone(&client_addr, &freelancer_addr, &0u32, &1_500_i128);
    assert!(
        matches!(result, Err(Err(_))),
        "client-only signature must revert"
    );

    let result = escrow
        .mock_auths(&[MockAuth {
            address: &freelancer_addr,
            invoke: &invoke,
        }])
        .try_increase_milestone(&client_addr, &freelancer_addr, &0u32, &1_500_i128);
    assert!(
        matches!(result, Err(Err(_))),
        "freelancer-only signature must revert"
    );

    // No mutation, no deposit.
    assert_eq!(job_meta(&env, &contract_id).total_amount, 1_000);
    assert_eq!(balance(&env, &token_addr, &contract_id), 1_000);
    assert_eq!(topic_count(&env, symbol_short!("ms_incr")), 0);
}

#[test]
fn increase_milestone_rejects_amounts_not_above_current() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let res = escrow.try_increase_milestone(&client_addr, &freelancer_addr, &0u32, &1_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    let res = escrow.try_increase_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    assert_eq!(milestone_sum(&escrow), 1_000);
}

#[test]
fn increase_milestone_rejects_unfunded_upfront_escrow() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_unfunded_escrow(&env, vec![&env, 1_000_i128]);

    let res = escrow.try_increase_milestone(&client_addr, &freelancer_addr, &0u32, &1_500_i128);
    assert_eq!(res, Err(Ok(Error::NotFunded)));
}

#[test]
fn add_milestone_under_per_milestone_appends_unfunded_and_leaves_meta_unfunded() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, token_addr, contract_id, escrow) = setup_per_milestone(&env);
    // Initially milestones 0 and 1 are unfunded. Fund milestone 0 so the job has funding.
    escrow.fund_milestone(&client_addr, &0u32);

    let initial_balance = balance(&env, &token_addr, &contract_id);
    let initial_client_balance = balance(&env, &token_addr, &client_addr);
    assert_eq!(initial_balance, 1_000);

    // Add milestone under PerMilestone mode.
    escrow.add_milestone(&client_addr, &freelancer_addr, &500_i128);

    let job = escrow.get_job();
    assert_eq!(job.milestones.len(), 3);
    let added = job.milestones.get(2).expect("third milestone exists");
    assert_eq!(added.amount, 500);
    assert_eq!(added.released_amount, 0);
    assert_eq!(added.status, MilestoneStatus::Pending);
    assert_eq!(added.funded, false);

    let meta = job_meta(&env, &contract_id);
    assert_eq!(meta.milestone_count, 3);
    assert_eq!(meta.total_amount, 3_500);
    assert_eq!(meta.funded, false);

    // No funds transferred from client upon add_milestone under PerMilestone
    assert_eq!(balance(&env, &token_addr, &contract_id), initial_balance);
    assert_eq!(balance(&env, &token_addr, &client_addr), initial_client_balance);

    // Now fund the added milestone through fund_milestone
    escrow.fund_milestone(&client_addr, &2u32);
    let funded_added = escrow.get_job().milestones.get(2).unwrap();
    assert_eq!(funded_added.funded, true);
    assert_eq!(balance(&env, &token_addr, &contract_id), initial_balance + 500);
}

#[test]
fn reduce_milestone_under_per_milestone_with_funding_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, token_addr, contract_id, escrow) = setup_per_milestone(&env);
    // Fund milestone 0
    escrow.fund_milestone(&client_addr, &0u32);

    let client_balance_before = balance(&env, &token_addr, &client_addr);
    escrow.reduce_milestone(&client_addr, &freelancer_addr, &0u32, &600_i128);

    let m0 = escrow.get_job().milestones.get(0).unwrap();
    assert_eq!(m0.amount, 600);
    assert_eq!(balance(&env, &token_addr, &contract_id), 600);
    assert_eq!(balance(&env, &token_addr, &client_addr), client_balance_before + 400);
}
