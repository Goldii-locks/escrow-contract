use super::*;
use soroban_sdk::{symbol_short, Symbol, TryIntoVal};

fn assert_topics(env: &Env, name: Symbol, addresses: &[Address]) {
    let events = crate::all_event_tuples(env);
    let topics = &events.last().expect("event was emitted").1;
    assert_eq!(topics.len(), addresses.len() as u32 + 1);
    let first: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
    assert_eq!(first, name);
    for (index, expected_address) in addresses.iter().enumerate() {
        let address: Address = topics
            .get(index as u32 + 1)
            .unwrap()
            .try_into_val(env)
            .unwrap();
        assert_eq!(&address, expected_address);
    }
}

#[test]
fn init_topics_keep_event_name_and_index_all_parties() {
    let env = Env::default();
    env.mock_all_auths();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    escrow.initialize(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604_800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
    );

    assert_topics(&env, symbol_short!("init"), &[client, freelancer, arbiter]);
}

#[test]
fn fund_topics_keep_event_name_and_index_all_parties() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, _, _, _, _) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_topics(&env, symbol_short!("fund"), &[client, freelancer, arbiter]);
}

#[test]
fn approve_topics_keep_event_name_and_index_client() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    assert_topics(&env, symbol_short!("approve"), &[client]);
}

#[test]
fn arbchg_topics_keep_event_name_and_index_new_arbiter() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let new_arbiter = Address::generate(&env);
    escrow.replace_arbiter(&new_arbiter);

    assert_topics(&env, symbol_short!("arbchg"), &[new_arbiter]);
}

#[test]
fn dispute_topics_keep_event_name_and_index_caller() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.raise_dispute(&client, &0, &dispute_reason_hash(&env));

    assert_topics(&env, symbol_short!("dispute"), &[client]);
}
