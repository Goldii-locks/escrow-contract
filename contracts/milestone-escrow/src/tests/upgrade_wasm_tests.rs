//! Upgrade tests against a real WASM swap (issue #752).
//!
//! `testdata/escrow_v2.wasm` is built from `testdata/escrow_v2`: the full
//! escrow plus a `v2_only` entrypoint and a `milestone_v2` reader that uses a
//! changed `Milestone` layout.

use super::*;
use crate::{DataKey, Error, MilestoneStatus};
use soroban_sdk::{Address, BytesN, Env, String, Symbol, Val};

const ESCROW_V2_WASM: &[u8] = include_bytes!("../../testdata/escrow_v2.wasm");

fn upload_v2(env: &Env) -> BytesN<32> {
    // The fixture links the whole escrow, so uploading it writes more bytes
    // than the test env's default per-invocation limit allows. These tests
    // check upgrade behaviour, not resource cost.
    env.cost_estimate().disable_resource_limits();
    env.deployer()
        .upload_contract_wasm(soroban_sdk::Bytes::from_slice(env, ESCROW_V2_WASM))
}

fn call_v2_only(env: &Env, contract_id: &Address) -> bool {
    env.try_invoke_contract::<u32, soroban_sdk::Error>(
        contract_id,
        &Symbol::new(env, "v2_only"),
        vec![env],
    ) == Ok(Ok(2))
}

#[test]
fn upgrade_to_v2_wasm_preserves_state_and_runs_new_code() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, admin_addr, token, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 2_000_i128]);
    let whitelist_before = client.get_whitelisted_tokens();
    let stored_admin = || -> Address {
        env.as_contract(&contract_id, || {
            env.storage().persistent().get(&DataKey::Admin).unwrap()
        })
    };
    assert_eq!(stored_admin(), admin_addr);

    // v1 has no `v2_only` entrypoint.
    assert!(!call_v2_only(&env, &contract_id));

    let hash = upload_v2(&env);
    client.upgrade(&admin_addr, &hash);

    // The new code is running.
    assert!(call_v2_only(&env, &contract_id));

    // Version marker bumped from the implicit 1 to 2.
    assert_eq!(client.version(), String::from_str(&env, crate::CONTRACT_VERSION));
    let stored_version: Option<u32> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::Version)
    });
    assert_eq!(stored_version, Some(2));

    // Existing storage still decodes under the new code.
    let job = client.get_job();
    assert_eq!(job.client, client_addr);
    assert_eq!(job.freelancer, freelancer_addr);
    assert_eq!(job.arbiter, arbiter_addr);
    assert_eq!(job.token, token);
    assert!(job.funded);
    assert_eq!(job.milestones.len(), 2);
    assert_eq!(job.milestones.get(0).unwrap().amount, 1_000);
    assert_eq!(job.milestones.get(1).unwrap().amount, 2_000);
    assert_eq!(client.get_whitelisted_tokens(), whitelist_before);
    assert_eq!(stored_admin(), admin_addr);

    // The normal flow continues on the upgraded contract.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
    assert_eq!(
        token::Client::new(&env, &token).balance(&freelancer_addr),
        1_000
    );
}

#[test]
fn upgrade_rejects_version_overflow() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Version, &u32::MAX);
    });

    let hash = upload_v2(&env);
    let result = client.try_upgrade(&admin_addr, &hash);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));

    // The failed call rolled back: still v1 code and the same version.
    assert!(!call_v2_only(&env, &contract_id));
    assert_eq!(client.version(), String::from_str(&env, crate::CONTRACT_VERSION));
}

/// A v2 whose `Milestone` layout differs from what v1 wrote cannot decode the
/// existing entries: reading them traps.
#[test]
fn upgrade_with_incompatible_storage_layout_fails_to_decode() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let hash = upload_v2(&env);
    client.upgrade(&admin_addr, &hash);

    let result = env.try_invoke_contract::<Val, soroban_sdk::Error>(
        &contract_id,
        &Symbol::new(&env, "milestone_v2"),
        vec![&env, 0u32.into_val(&env)],
    );
    assert!(result.is_err());

    // The v1 layout still reads fine.
    assert_eq!(client.get_job().milestones.get(0).unwrap().amount, 1_000);
}
