//! Per-milestone delivery deadlines and `reclaim_overdue`.

use crate::{
    Error, MilestoneEscrow, MilestoneEscrowClient, MilestoneStatus, OVERDUE_GRACE_SECONDS,
};
use soroban_sdk::{testutils::Address as _, testutils::Ledger, token, vec, Address, Env, Vec};

const DUE_AT: u64 = 10_000;

/// Escrow with milestone 0 due at `DUE_AT` and milestone 1 without a
/// deadline, accepted and funded.  Returns (client, freelancer, token, escrow).
fn setup(env: &Env) -> (Address, Address, Address, MilestoneEscrowClient<'_>) {
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let admin = Address::generate(env);
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(env, &token).mint(&client, &1_000);

    let amounts: Vec<i128> = vec![env, 400, 600];
    let escrow = MilestoneEscrowClient::new(env, &env.register(MilestoneEscrow, ()));
    escrow.initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800u64,
        &amounts,
        &crate::spec_hashes_for(&amounts),
    );
    escrow.set_milestone_due_dates(&client, &vec![env, DUE_AT, 0]);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund(&client);
    (client, freelancer, token, escrow)
}

#[test]
fn due_dates_are_stored() {
    let env = Env::default();
    let (_, _, _, escrow) = setup(&env);
    assert_eq!(escrow.get_milestone_due_at(&0), DUE_AT);
    assert_eq!(escrow.get_milestone_due_at(&1), 0);
}

#[test]
fn reclaim_before_deadline_fails() {
    let env = Env::default();
    let (client, _, _, escrow) = setup(&env);

    env.ledger().set_timestamp(DUE_AT + 1);
    assert_eq!(
        escrow.try_reclaim_overdue(&client, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
    env.ledger().set_timestamp(DUE_AT + OVERDUE_GRACE_SECONDS);
    assert_eq!(
        escrow.try_reclaim_overdue(&client, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
}

#[test]
fn reclaim_after_deadline_pays_client() {
    let env = Env::default();
    let (client, _, token, escrow) = setup(&env);
    let token_client = token::Client::new(&env, &token);
    assert_eq!(token_client.balance(&client), 0);

    env.ledger()
        .set_timestamp(DUE_AT + OVERDUE_GRACE_SECONDS + 1);
    escrow.reclaim_overdue(&client, &0);

    assert_eq!(token_client.balance(&client), 400);
    assert_eq!(escrow.get_milestone(&0).status, MilestoneStatus::Refunded);
    assert_eq!(escrow.get_milestone(&1).status, MilestoneStatus::Pending);
    assert_eq!(
        escrow.try_reclaim_overdue(&client, &0),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn reclaim_after_delivery_fails() {
    let env = Env::default();
    let (client, freelancer, _, escrow) = setup(&env);

    // Late delivery is still allowed until the client reclaims.
    env.ledger()
        .set_timestamp(DUE_AT + OVERDUE_GRACE_SECONDS + 1);
    escrow.mark_delivered(&freelancer, &0);
    assert_eq!(
        escrow.try_reclaim_overdue(&client, &0),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn milestone_without_deadline_cannot_be_reclaimed() {
    let env = Env::default();
    let (client, _, _, escrow) = setup(&env);

    env.ledger().set_timestamp(u64::MAX / 2);
    assert_eq!(
        escrow.try_reclaim_overdue(&client, &1),
        Err(Ok(Error::DeadlineNotPassed))
    );
}

#[test]
fn only_client_can_reclaim() {
    let env = Env::default();
    let (_, freelancer, _, escrow) = setup(&env);

    env.ledger()
        .set_timestamp(DUE_AT + OVERDUE_GRACE_SECONDS + 1);
    assert_eq!(
        escrow.try_reclaim_overdue(&freelancer, &0),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn set_due_dates_rejects_past_dates_but_accepts_zero_and_future() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let admin = Address::generate(&env);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let amounts = vec![&env, 400, 600];
    let escrow = MilestoneEscrowClient::new(&env, &env.register(MilestoneEscrow, ()));
    escrow.initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800u64,
        &amounts,
        &crate::spec_hashes_for(&amounts),
    );

    // A past date fails the whole call, with no partial write (#924).
    assert_eq!(
        escrow.try_set_milestone_due_dates(&client, &vec![&env, 999, 2_000]),
        Err(Ok(Error::InvalidDeadline))
    );
    assert_eq!(escrow.get_milestone_due_at(&0), 0);
    assert_eq!(escrow.get_milestone_due_at(&1), 0);

    // A date equal to the current ledger timestamp is already stale.
    assert_eq!(
        escrow.try_set_milestone_due_dates(&client, &vec![&env, 1_000, 1]),
        Err(Ok(Error::InvalidDeadline))
    );

    // Zero still clears, and a future date is stored.
    escrow.set_milestone_due_dates(&client, &vec![&env, 0, 2_000]);
    assert_eq!(escrow.get_milestone_due_at(&0), 0);
    assert_eq!(escrow.get_milestone_due_at(&1), 2_000);
}

#[test]
fn due_dates_cannot_change_after_funding() {
    let env = Env::default();
    let (client, _, _, escrow) = setup(&env);
    assert_eq!(
        escrow.try_set_milestone_due_dates(&client, &vec![&env, 1, 1]),
        Err(Ok(Error::AlreadyFunded))
    );
}

#[test]
fn revision_after_timely_delivery_resets_overdue_clock() {
    let env = Env::default();
    let (client, freelancer, _, escrow) = setup(&env);

    // 1-2. Freelancer delivers before the due date; the client waits past the
    // original due date plus grace without approving.
    env.ledger().set_timestamp(DUE_AT - 1);
    escrow.mark_delivered(&freelancer, &0);
    env.ledger()
        .set_timestamp(DUE_AT + OVERDUE_GRACE_SECONDS + 1);

    // 3. A revision moves the milestone back to Pending and pushes the due
    // date out by a full fresh delivery window.
    escrow.request_revision(&client, &0, &crate::BytesN::from_array(&env, &[7; 32]));
    assert_eq!(escrow.get_milestone(&0).status, MilestoneStatus::Pending);
    let revised_due_at = escrow.get_milestone_due_at(&0);
    assert!(revised_due_at >= env.ledger().timestamp() + crate::REVISION_DELIVERY_WINDOW);

    // 4. The old deadline can no longer be used to reclaim the milestone.
    assert_eq!(
        escrow.try_reclaim_overdue(&client, &0),
        Err(Ok(Error::DeadlineNotPassed))
    );
}
