#![cfg(test)]

//! Property test for #677: the read-only preview and the real payout allocation
//! must agree for every (remaining, bps) pair, before balance capping.

use super::*;
use proptest::prelude::*;

proptest! {
    #[test]
    fn preview_equals_applied_allocation(
        remaining in 1_i128..1_000_000_000_000_i128,
        client_refund_bps in 0_u32..=10_000_u32,
    ) {
        let env = Env::default();
        let contract_id = env.register(MilestoneEscrow, ());
        let client = MilestoneEscrowClient::new(&env, &contract_id);

        let preview = client.dispute_arbitration_split(&remaining, &client_refund_bps);
        let applied =
            MilestoneEscrow::allocate_refund_by_bps(remaining, client_refund_bps).unwrap();

        prop_assert_eq!(preview.client_refund, applied.client_refund);
        prop_assert_eq!(preview.freelancer_payout, applied.freelancer_payout);
        prop_assert_eq!(preview.client_refund_bps, applied.client_refund_bps);
        prop_assert_eq!(preview.freelancer_payout_bps, applied.freelancer_payout_bps);
        prop_assert_eq!(preview.client_refund + preview.freelancer_payout, remaining);
    }
}
