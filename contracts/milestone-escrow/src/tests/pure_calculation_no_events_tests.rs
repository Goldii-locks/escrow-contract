#![cfg(test)]

//! #676: pure calculation endpoints must not publish events, because indexers
//! cannot distinguish a spoofed "calculation" transaction from a real state
//! change when the event comes from the escrow's own contract ID.

use super::*;
use crate::testutils::EscrowFixture;
use soroban_sdk::testutils::Events;

#[test]
fn pure_calculations_leave_no_events() {
    let env = Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env)
        .milestones(&[1_000])
        .funded()
        .build();
    let escrow = &fx.escrow;

    let allocation = PlatformFeeAllocation {
        client_bps: 0,
        freelancer_bps: BPS_SCALE,
        treasury_bps: 0,
        locked: false,
    };

    let _ = escrow.try_escrow_interest_yield(&fx.admin, &1_000_i128, &500_i128, &100_i128);
    assert!(
        env.events().all().events().is_empty(),
        "escrow_interest_yield emitted"
    );

    let _ =
        escrow.try_split_refund_net_distribution(&1_000_i128, &5_000_u32, &5_000_u32, &allocation);
    assert!(
        env.events().all().events().is_empty(),
        "split_refund_net_distribution emitted"
    );

    let _ = escrow.try_calculate_platform_fee_split(&1_000_i128);
    assert!(
        env.events().all().events().is_empty(),
        "calculate_platform_fee_split emitted"
    );

    let _ = escrow.try_dispute_arbitration_split(&1_000_i128, &5_000_u32);
    assert!(
        env.events().all().events().is_empty(),
        "dispute_arbitration_split emitted"
    );

    let _ = escrow.try_interest_yield_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert!(
        env.events().all().events().is_empty(),
        "interest_yield_split_refund emitted"
    );

    let _ = escrow.try_tax_withholding_split_refund(&1_000_i128, &100_i128, &5_000_u32, &5_000_u32);
    assert!(
        env.events().all().events().is_empty(),
        "tax_withholding_split_refund emitted"
    );

    let _ = escrow.try_multisig_split_refund(&fx.admin, &1_000_i128, &5_000_u32, &5_000_u32);
    assert!(
        env.events().all().events().is_empty(),
        "multisig_split_refund emitted"
    );

    let _ = escrow.try_cancel_escrow_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert!(
        env.events().all().events().is_empty(),
        "cancel_escrow_split_refund emitted"
    );

    let _ = escrow.try_emergency_pause_split_refund(&fx.admin, &1_000_i128, &5_000_u32, &5_000_u32);
    assert!(
        env.events().all().events().is_empty(),
        "emergency_pause_split_refund emitted"
    );
}
