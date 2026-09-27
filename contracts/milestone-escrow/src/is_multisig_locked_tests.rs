#![cfg(test)]

use crate::test::setup_funded_escrow;
use soroban_sdk::vec;

#[test]
fn test_is_multisig_locked_returns_not_initialized_on_uninitialized_contract() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();

    let contract_id = env.register(crate::MilestoneEscrow, ());
    let client = crate::MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_is_multisig_locked();
    assert_eq!(result, Ok(Err(crate::Error::NotInitialized)));
}

#[test]
fn test_is_multisig_locked_performs_no_state_mutation() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 101_i128];
    let (_payee, _payer, _admin, _platform_fee_admin, _contract_id, client) =
        setup_funded_escrow(&env, amounts);

    // Take a snapshot of the entire ledger state before calling is_multisig_locked
    let storage_before = env.as_contract(&client.address, || {
        let mut state = Vec::<(soroban_sdk::Symbol, Vec<u8>)>::new(&env);

        // Check all data keys in instance storage
        for key_bytes in [
            b"admin",
            b"paused",
            b"platform_fee_admin",
            b"platform_fee_allocation",
            b"milestones",
            b"whitelisted_tokens",
            b"multisig_transfer_pending",
            b"multisig_locked",
            b"multisig_signers",
            b"interest_yield_state",
        ]
        .iter()
        {
            let symbol = soroban_sdk::Symbol::new(&env, std::str::from_utf8(key_bytes).unwrap());
            if let Some(value) = env.storage().instance().get::<_, Vec<u8>>(&symbol) {
                state.push_back((symbol, value));
            }
        }
        state
    });

    // Call is_multisig_locked
    let result = client.is_multisig_locked();
    assert!(result.is_ok());

    // Take a snapshot after the call
    let storage_after = env.as_contract(&client.address, || {
        let mut state = Vec::<(soroban_sdk::Symbol, Vec<u8>)>::new(&env);

        for key_bytes in [
            b"admin",
            b"paused",
            b"platform_fee_admin",
            b"platform_fee_allocation",
            b"milestones",
            b"whitelisted_tokens",
            b"multisig_transfer_pending",
            b"multisig_locked",
            b"multisig_signers",
            b"interest_yield_state",
        ]
        .iter()
        {
            let symbol = soroban_sdk::Symbol::new(&env, std::str::from_utf8(key_bytes).unwrap());
            if let Some(value) = env.storage().instance().get::<_, Vec<u8>>(&symbol) {
                state.push_back((symbol, value));
            }
        }
        state
    });

    // Verify storage is unchanged
    assert_eq!(storage_before.len(), storage_after.len());
    for i in 0..storage_before.len() {
        let before = storage_before.get(i).unwrap();
        let after = storage_after.get(i).unwrap();
        assert_eq!(before.0, after.0);
        assert_eq!(before.1, after.1);
    }
}

#[test]
fn test_is_multisig_locked_returns_false_when_not_locked() {
    let env = soroban_sdk::Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 101_i128];
    let (_payee, _payer, _admin, _platform_fee_admin, _contract_id, client) =
        setup_funded_escrow(&env, amounts);

    let result = client.is_multisig_locked();
    assert_eq!(result, Ok(false));
}
