#![cfg(test)]
//! Issue #694: once the escrow is funded, the fee allocation may only be
//! changed with both parties' consent.

use super::*;
use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
use soroban_sdk::IntoVal;

fn fixture(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let (client_addr, freelancer, _arbiter, admin, _token, contract_id, escrow) =
        setup_funded_escrow(env, vec![env, 1_000_i128]);
    (contract_id, admin, client_addr, freelancer, escrow)
}

#[test]
fn set_allocation_after_fund_requires_party_consent() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract_id, admin, _client_addr, _freelancer, escrow) = fixture(&env);
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_platform_fee_allocation",
            args: (admin.clone(), 5_000u32, 4_000u32, 1_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);

    assert!(escrow
        .try_set_platform_fee_allocation(&admin, &5_000, &4_000, &1_000)
        .is_err());
}

#[test]
fn admin_override_after_fund_requires_party_consent() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract_id, admin, _client_addr, _freelancer, escrow) = fixture(&env);
    escrow.lock_platform_fee_allocation(&admin);

    // Only the admin signs: the lock cannot be undone unilaterally.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "pf_alloc_admin_override",
            args: (admin.clone(), 5_000u32, 4_000u32, 1_000u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(escrow
        .try_pf_alloc_admin_override(&admin, &5_000, &4_000, &1_000)
        .is_err());
    assert!(escrow.get_platform_fee_allocation().locked);
}

#[test]
fn both_parties_can_consent_to_a_funded_change() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, admin, _client_addr, _freelancer, escrow) = fixture(&env);

    escrow.set_platform_fee_allocation(&admin, &5_000, &4_000, &1_000);
    let allocation = escrow.get_platform_fee_allocation();
    assert_eq!(allocation.client_bps, 5_000);
    assert_eq!(allocation.treasury_bps, 1_000);
}
