#![cfg(test)]
use super::*;
use soroban_sdk::{token, vec, Address, Env};

fn mint(env: &Env, token: &Address, admin: &Address, to: &Address, amount: i128) {
    token::StellarAssetClient::new(env, token).mint(to, &amount);
    let _ = admin;
}

#[test]
fn direct_transfer_recovery() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    mint(&env, &token, &admin_addr, &client_addr, 50);
    token::Client::new(&env, &token).transfer(&client_addr, &escrow.address, &50);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);
    let swept = escrow.sweep_excess(&client_addr, &token);
    assert_eq!(swept, 50);
    assert_eq!(token::Client::new(&env, &token).balance(&escrow.address), 0);
}

#[test]
fn outstanding_milestone_protection() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, _, token, _, escrow) = setup_funded_escrow(&env, vec![&env, 100_i128]);
    assert_eq!(
        escrow.try_sweep_excess(&client_addr, &token),
        Err(Ok(Error::NothingToSweep))
    );
}

#[test]
fn partial_settlement() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    mint(&env, &token, &admin_addr, &client_addr, 50);
    token::Client::new(&env, &token).transfer(&client_addr, &escrow.address, &50);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_partial(&client_addr, &0u32, &40_i128);
    let swept = escrow.sweep_excess(&client_addr, &token);
    assert_eq!(swept, 50);
    assert_eq!(
        token::Client::new(&env, &token).balance(&escrow.address),
        60
    );
}

#[test]
fn non_escrow_token_recovery() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, admin_addr, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    let other = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    mint(&env, &other, &admin_addr, &client_addr, 25);
    token::Client::new(&env, &other).transfer(&client_addr, &escrow.address, &25);
    assert_eq!(escrow.sweep_excess(&admin_addr, &other), 25);
}

#[test]
fn authorization() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, freelancer_addr, _, _, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    assert_eq!(
        escrow.try_sweep_excess(&freelancer_addr, &token),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn insufficient_balance() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, admin_addr, token, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    token::Client::new(&env, &token).transfer(&contract_id, &admin_addr, &100);
    assert_eq!(
        escrow.try_sweep_excess(&client_addr, &token),
        Err(Ok(Error::InsufficientBalance))
    );
}

#[test]
fn zero_excess() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);
    assert_eq!(
        escrow.try_sweep_excess(&client_addr, &token),
        Err(Ok(Error::NothingToSweep))
    );
}

#[test]
fn accounting_invariant() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, token, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 80_i128, 20_i128]);
    mint(&env, &token, &admin_addr, &client_addr, 50);
    token::Client::new(&env, &token).transfer(&client_addr, &escrow.address, &50);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);
    escrow.sweep_excess(&client_addr, &token);
    assert_eq!(
        token::Client::new(&env, &token).balance(&escrow.address),
        20
    );
    escrow.mark_delivered(&freelancer_addr, &1u32);
    escrow.approve_milestone(&client_addr, &1u32);
    assert_eq!(token::Client::new(&env, &token).balance(&escrow.address), 0);
}

#[test]
fn accrued_yield_is_not_swept() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, admin_addr, token, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128]);
    mint(&env, &token, &admin_addr, &contract_id, 50);
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &20_i128);
    });

    assert_eq!(escrow.sweep_excess(&client_addr, &token), 30);
}
