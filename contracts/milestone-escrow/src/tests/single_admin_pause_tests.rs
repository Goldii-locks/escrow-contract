#![cfg(test)]
//! One admin pause flag (`DataKey::Paused`) plus the party emergency pause
//! (`DataKey::Ep`), both checked through one helper (#994).

use super::*;
use soroban_sdk::{vec, Env};

fn read_flag(env: &Env, contract_id: &Address, key: DataKey) -> bool {
    env.as_contract(contract_id, || {
        env.storage().instance().get(&key).unwrap_or(false)
    })
}

#[test]
fn admin_override_sets_the_admin_pause_flag() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.emergency_pause_admin_override(&admin_addr);
    assert!(read_flag(&env, &contract_id, DataKey::Paused));
    assert!(!read_flag(&env, &contract_id, DataKey::Ep));
    assert_eq!(
        escrow.try_admin_pause_escrow(&admin_addr),
        Err(Ok(Error::AlreadyPaused))
    );

    escrow.admin_resume_escrow(&admin_addr);
    assert!(!escrow.is_emergency_paused());
}

#[test]
fn party_pause_blocks_every_pause_guarded_endpoint() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0);

    escrow.emergency_pause(&client_addr, &freelancer_addr);
    assert_eq!(
        escrow.try_extend_milestone_deadline(&client_addr, &0, &60),
        Err(Ok(Error::Paused))
    );
    assert_eq!(
        escrow.try_admin_override_release(&admin_addr, &0),
        Err(Ok(Error::Paused))
    );
    assert!(escrow.get_yield_info().2);
}

#[test]
fn admin_pause_blocks_cancel_escrow() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, admin_addr, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.admin_pause_escrow(&admin_addr);
    assert!(escrow.is_emergency_paused());
    assert_eq!(
        escrow.try_cancel_escrow(&client_addr),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn migrate_moves_an_admin_set_emergency_pause_to_the_admin_flag() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, admin_addr, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Schema-3 state left by the old `emergency_pause_admin_override`.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Ep, &true);
        env.storage().instance().set(&DataKey::EpParty, &false);
        env.storage().instance().set(&DataKey::SchemaVersion, &3u32);
    });

    assert_eq!(escrow.migrate(&admin_addr), SCHEMA_VERSION);
    assert!(read_flag(&env, &contract_id, DataKey::Paused));
    assert!(!read_flag(&env, &contract_id, DataKey::Ep));
    escrow.admin_resume_escrow(&admin_addr);
    assert!(!escrow.is_emergency_paused());
}

#[test]
fn migrate_keeps_a_party_emergency_pause() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, admin_addr, _, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.emergency_pause(&client_addr, &freelancer_addr);
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::SchemaVersion, &3u32);
    });

    assert_eq!(escrow.migrate(&admin_addr), SCHEMA_VERSION);
    assert!(read_flag(&env, &contract_id, DataKey::Ep));
    assert!(!read_flag(&env, &contract_id, DataKey::Paused));
    escrow.emergency_unpause_parties(&client_addr, &freelancer_addr);
    assert!(!escrow.is_emergency_paused());
}
