#![cfg(test)]
//! Dedicated test suite for `split_refund_net_distribution`:
//! - Issue #516: Rounding direction of basis-point splits (half-up / round-to-nearest)
//!   and remainder preservation.
//! - Issue #517: Value conservation ensuring returned outputs sum exactly to total_amount.
//! - Issue #518: Structured event publication on success path and absence on error paths.

use super::*;
use crate::{DataKey, Error, PlatformFeeAllocation, SplitRefundNetDistributionEvent};
use soroban_sdk::testutils::EnvTestConfig;
use soroban_sdk::{symbol_short, Env, FromVal, IntoVal, Val};

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn sprefnet_events(env: &Env) -> std::vec::Vec<SplitRefundNetDistributionEvent> {
    let topic_val: Val = symbol_short!("sprefnet").into_val(env);
    let mut events = std::vec::Vec::new();
    for event in crate::all_event_tuples(env).iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                events.push(SplitRefundNetDistributionEvent::from_val(env, &event.2));
            }
        }
    }
    events
}

// Issue #516: Rounding direction tests

#[test]
fn test_split_refund_net_distribution_uneven_half_rounds_up() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    // total_amount = 101, 50% / 50% split.
    // Client share = round_nearest(101 * 5000 / 10000) = (505000 + 5000) / 10000 = 51.
    // Freelancer gross payout receives the exact remainder: 101 - 51 = 50.
    // Remainder is not discarded.
    let dist =
        client.split_refund_net_distribution(&101_i128, &5000_u32, &5000_u32, &fee_allocation);

    assert_eq!(dist.client_net_refund, 51);
    // On gross payout of 50:
    // client_fee_share = round_nearest(50 * 1000 / 10000) = (50000 + 5000) / 10000 = 5
    // treasury_fee_share = round_nearest(50 * 1000 / 10000) = (50000 + 5000) / 10000 = 5
    // freelancer_net_payout = 50 - 5 - 5 = 40
    assert_eq!(dist.client_fee_share, 5);
    assert_eq!(dist.treasury_fee_share, 5);
    assert_eq!(dist.freelancer_net_payout, 40);

    // Sum invariant
    assert_eq!(
        dist.client_net_refund
            + dist.client_fee_share
            + dist.freelancer_net_payout
            + dist.treasury_fee_share,
        101
    );
}

#[test]
fn test_split_refund_net_distribution_rounds_down_when_fraction_below_half() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 0,
        freelancer_bps: 10000,
        treasury_bps: 0,
        locked: false,
    };

    // 100 * 3333 bps = 333300.
    // (333300 + 5000) / 10000 = 338300 / 10000 = 33 (rounds down, remainder is 0.33 < 0.5).
    // Freelancer gross receives 100 - 33 = 67.
    let dist =
        client.split_refund_net_distribution(&100_i128, &3333_u32, &6667_u32, &fee_allocation);

    assert_eq!(dist.client_net_refund, 33);
    assert_eq!(dist.freelancer_net_payout, 67);
    assert_eq!(dist.client_fee_share, 0);
    assert_eq!(dist.treasury_fee_share, 0);
    assert_eq!(dist.client_net_refund + dist.freelancer_net_payout, 100);
}

#[test]
fn test_split_refund_net_distribution_rounds_up_when_fraction_at_or_above_half() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 0,
        freelancer_bps: 10000,
        treasury_bps: 0,
        locked: false,
    };

    // 100 * 6667 bps = 666700.
    // (666700 + 5000) / 10000 = 671700 / 10000 = 67 (rounds up, remainder is 0.67 >= 0.5).
    // Freelancer gross receives 100 - 67 = 33.
    let dist =
        client.split_refund_net_distribution(&100_i128, &6667_u32, &3333_u32, &fee_allocation);

    assert_eq!(dist.client_net_refund, 67);
    assert_eq!(dist.freelancer_net_payout, 33);
    assert_eq!(dist.client_fee_share, 0);
    assert_eq!(dist.treasury_fee_share, 0);
    assert_eq!(dist.client_net_refund + dist.freelancer_net_payout, 100);
}

#[test]
fn test_split_refund_net_distribution_single_indivisible_unit() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    // total_amount = 1, 50/50 split.
    // client share = (1 * 5000 + 5000) / 10000 = 1.
    // freelancer gross = 0.
    let dist = client.split_refund_net_distribution(&1_i128, &5000_u32, &5000_u32, &fee_allocation);

    assert_eq!(dist.client_net_refund, 1);
    assert_eq!(dist.client_fee_share, 0);
    assert_eq!(dist.freelancer_net_payout, 0);
    assert_eq!(dist.treasury_fee_share, 0);
    assert_eq!(
        dist.client_net_refund
            + dist.client_fee_share
            + dist.freelancer_net_payout
            + dist.treasury_fee_share,
        1
    );
}

// Issue #517: Value conservation sweep across amounts and ratios

#[test]
fn test_split_refund_net_distribution_conserves_total_across_sweep() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let test_amounts = [
        1_i128,
        2,
        3,
        5,
        7,
        10,
        11,
        49,
        101,
        333,
        999,
        1000,
        1001,
        7777,
        10000,
        99999,
        1_000_000,
        123_456_789,
    ];

    let test_bps_pairs = [
        (0_u32, 10000_u32),
        (1, 9999),
        (1000, 9000),
        (2500, 7500),
        (3333, 6667),
        (5000, 5000),
        (6667, 3333),
        (7500, 2500),
        (9000, 1000),
        (9999, 1),
        (10000, 0),
    ];

    let fee_allocations = [
        PlatformFeeAllocation {
            client_bps: 1000,
            freelancer_bps: 8000,
            treasury_bps: 1000,
            locked: false,
        },
        PlatformFeeAllocation {
            client_bps: 0,
            freelancer_bps: 10000,
            treasury_bps: 0,
            locked: false,
        },
        PlatformFeeAllocation {
            client_bps: 500,
            freelancer_bps: 9000,
            treasury_bps: 500,
            locked: false,
        },
        PlatformFeeAllocation {
            client_bps: 2000,
            freelancer_bps: 6000,
            treasury_bps: 2000,
            locked: false,
        },
    ];

    for &amt in test_amounts.iter() {
        for &(client_bps, freelancer_bps) in test_bps_pairs.iter() {
            for fee_alloc in fee_allocations.iter() {
                let dist = client.split_refund_net_distribution(
                    &amt,
                    &client_bps,
                    &freelancer_bps,
                    fee_alloc,
                );

                let sum = dist.client_net_refund
                    + dist.client_fee_share
                    + dist.freelancer_net_payout
                    + dist.treasury_fee_share;

                assert_eq!(
                    sum, amt,
                    "Conservation failed for amount={}, client_bps={}, fee_alloc={:?}",
                    amt, client_bps, fee_alloc
                );
                assert!(dist.client_net_refund >= 0);
                assert!(dist.client_fee_share >= 0);
                assert!(dist.freelancer_net_payout >= 0);
                assert!(dist.treasury_fee_share >= 0);
            }
        }
    }
}

#[test]
fn test_split_refund_net_distribution_boundary_extremes() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_alloc = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    // Full client refund: client gets everything, fee shares and freelancer payout are 0
    let full_client =
        client.split_refund_net_distribution(&10_000_i128, &10_000_u32, &0_u32, &fee_alloc);
    assert_eq!(full_client.client_net_refund, 10_000);
    assert_eq!(full_client.client_fee_share, 0);
    assert_eq!(full_client.freelancer_net_payout, 0);
    assert_eq!(full_client.treasury_fee_share, 0);

    // Full freelancer payout: client gets 0, fees apply to all 10_000
    let full_freelancer =
        client.split_refund_net_distribution(&10_000_i128, &0_u32, &10_000_u32, &fee_alloc);
    assert_eq!(full_freelancer.client_net_refund, 0);
    assert_eq!(full_freelancer.client_fee_share, 1000);
    assert_eq!(full_freelancer.treasury_fee_share, 1000);
    assert_eq!(full_freelancer.freelancer_net_payout, 8000);
    assert_eq!(
        full_freelancer.client_net_refund
            + full_freelancer.client_fee_share
            + full_freelancer.freelancer_net_payout
            + full_freelancer.treasury_fee_share,
        10_000
    );
}

// Issue #518: Structured event emission tests

#[test]
fn test_split_refund_net_distribution_emits_structured_event_exactly_once() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    let dist =
        client.split_refund_net_distribution(&1000_i128, &6000_u32, &4000_u32, &fee_allocation);

    let events = sprefnet_events(&env);
    assert_eq!(
        events.len(),
        1,
        "Expected exactly one sprefnet event on success path"
    );

    let event = &events[0];
    assert_eq!(event.total_amount, 1000);
    assert_eq!(event.client_refund_bps, 6000);
    assert_eq!(event.freelancer_payout_bps, 4000);
    assert_eq!(event.client_net_refund, dist.client_net_refund);
    assert_eq!(event.client_fee_share, dist.client_fee_share);
    assert_eq!(event.freelancer_net_payout, dist.freelancer_net_payout);
    assert_eq!(event.treasury_fee_share, dist.treasury_fee_share);
}

#[test]
fn test_split_refund_net_distribution_no_event_on_zero_amount() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    let res =
        client.try_split_refund_net_distribution(&0_i128, &5000_u32, &5000_u32, &fee_allocation);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    let events = sprefnet_events(&env);
    assert_eq!(events.len(), 0, "No event should be emitted on error path");
}

#[test]
fn test_split_refund_net_distribution_no_event_on_negative_amount() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    let res =
        client.try_split_refund_net_distribution(&-100_i128, &5000_u32, &5000_u32, &fee_allocation);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));

    let events = sprefnet_events(&env);
    assert_eq!(events.len(), 0, "No event should be emitted on error path");
}

#[test]
fn test_split_refund_net_distribution_no_event_on_invalid_bps() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    let res =
        client.try_split_refund_net_distribution(&1000_i128, &5000_u32, &4000_u32, &fee_allocation);
    assert_eq!(res, Err(Ok(Error::InvalidRatio)));

    let events = sprefnet_events(&env);
    assert_eq!(events.len(), 0, "No event should be emitted on error path");
}

#[test]
fn test_split_refund_net_distribution_no_event_when_paused() {
    let env = test_env();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Set pause state in instance storage
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Paused, &true);
    });

    let fee_allocation = PlatformFeeAllocation {
        client_bps: 1000,
        freelancer_bps: 8000,
        treasury_bps: 1000,
        locked: false,
    };

    let res =
        client.try_split_refund_net_distribution(&1000_i128, &5000_u32, &5000_u32, &fee_allocation);
    assert_eq!(res, Err(Ok(Error::Paused)));

    let events = sprefnet_events(&env);
    assert_eq!(
        events.len(),
        0,
        "No event should be emitted on paused error path"
    );
}
