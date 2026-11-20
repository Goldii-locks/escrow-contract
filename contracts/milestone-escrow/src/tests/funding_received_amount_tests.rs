#![cfg(test)]
//! `fund`, `fund_milestone`, `add_milestone` and `increase_milestone` verify
//! that the escrow received exactly the requested amount (issue #998).

use super::*;
use soroban_sdk::{token, vec, Address, Env};

mod fee_token {
    use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

    #[contracttype]
    #[derive(Clone)]
    enum Key {
        Balance(Address),
    }

    /// Minimal token that skims 1% of every transfer (the fee is burnt).
    #[contract]
    pub struct FeeToken;

    #[contractimpl]
    impl FeeToken {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let k = Key::Balance(to);
            let cur: i128 = env.storage().persistent().get(&k).unwrap_or(0);
            env.storage().persistent().set(&k, &(cur + amount));
        }

        pub fn balance(env: Env, id: Address) -> i128 {
            env.storage()
                .persistent()
                .get(&Key::Balance(id))
                .unwrap_or(0)
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            let fb = Self::balance(env.clone(), from.clone());
            assert!(fb >= amount, "insufficient");
            let fee = amount / 100;
            let tb = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&Key::Balance(from), &(fb - amount));
            env.storage()
                .persistent()
                .set(&Key::Balance(to), &(tb + amount - fee));
        }
    }
}

struct Fx<'a> {
    client: Address,
    freelancer: Address,
    token: Address,
    escrow: MilestoneEscrowClient<'a>,
}

fn setup(env: &Env, fee: bool, mode: FundingMode) -> Fx<'_> {
    env.mock_all_auths();
    let client = Address::generate(env);
    let freelancer = Address::generate(env);
    let arbiter = Address::generate(env);
    let admin = Address::generate(env);
    let amounts = vec![env, 1_000_i128, 2_000_i128];
    let token = if fee {
        let id = env.register(fee_token::FeeToken, ());
        fee_token::FeeTokenClient::new(env, &id).mint(&client, &10_000);
        id
    } else {
        let id = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        token::StellarAssetClient::new(env, &id).mint(&client, &10_000);
        id
    };
    let escrow = MilestoneEscrowClient::new(env, &env.register(MilestoneEscrow, ()));
    escrow.initialize_with_funding_mode(
        &admin,
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604800,
        &amounts,
        &crate::spec_hashes_for(&amounts),
        &mode,
    );
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    Fx {
        client,
        freelancer,
        token,
        escrow,
    }
}

fn bal(env: &Env, f: &Fx<'_>, who: &Address) -> i128 {
    token::Client::new(env, &f.token).balance(who)
}

fn total(env: &Env, f: &Fx<'_>) -> i128 {
    env.as_contract(&f.escrow.address, || {
        MilestoneEscrow::load_job_meta(env).unwrap().total_amount
    })
}

#[test]
fn fund_with_exact_token_succeeds() {
    let env = Env::default();
    let f = setup(&env, false, FundingMode::Upfront);
    f.escrow.fund(&f.client);
    assert!(f.escrow.get_job().funded);
    assert_eq!(bal(&env, &f, &f.escrow.address), 3_000);
}

#[test]
fn fund_with_fee_on_transfer_token_fails_and_stays_unfunded() {
    let env = Env::default();
    let f = setup(&env, true, FundingMode::Upfront);
    assert_eq!(
        f.escrow.try_fund(&f.client),
        Err(Ok(Error::InsufficientBalance))
    );
    let job = f.escrow.get_job();
    assert!(!job.funded);
    assert!(job.milestones.iter().all(|m| !m.funded));
    // Nothing moved and the client can still fund correctly elsewhere.
    assert_eq!(bal(&env, &f, &f.escrow.address), 0);
    assert_eq!(bal(&env, &f, &f.client), 10_000);
}

#[test]
fn fund_milestone_exact_and_fee_tokens() {
    let env = Env::default();
    let f = setup(&env, false, FundingMode::PerMilestone);
    f.escrow.fund_milestone(&f.client, &0u32);
    assert!(f.escrow.get_job().milestones.get(0).unwrap().funded);
    assert!(!f.escrow.get_job().funded);

    let env = Env::default();
    let f = setup(&env, true, FundingMode::PerMilestone);
    assert_eq!(
        f.escrow.try_fund_milestone(&f.client, &0u32),
        Err(Ok(Error::InsufficientBalance))
    );
    assert!(!f.escrow.get_job().milestones.get(0).unwrap().funded);
}

#[test]
fn add_milestone_exact_token_succeeds_fee_token_rejected_atomically() {
    let env = Env::default();
    let f = setup(&env, false, FundingMode::Upfront);
    f.escrow.fund(&f.client);
    f.escrow.add_milestone(&f.client, &f.freelancer, &500);
    let job = f.escrow.get_job();
    assert_eq!(job.milestones.len(), 3);
    assert_eq!(total(&env, &f), 3_500);
    assert_eq!(bal(&env, &f, &f.escrow.address), 3_500);

    // Fee token: get a funded job by seeding the escrow directly, then add.
    let env = Env::default();
    let f = setup(&env, true, FundingMode::Upfront);
    env.as_contract(&f.escrow.address, || {
        let mut meta = MilestoneEscrow::load_job_meta(&env).unwrap();
        meta.funded = true;
        MilestoneEscrow::store_job_meta(&env, &meta);
    });
    let before = f.escrow.get_job();
    let total_before = total(&env, &f);
    assert_eq!(
        f.escrow.try_add_milestone(&f.client, &f.freelancer, &500),
        Err(Ok(Error::InsufficientBalance))
    );
    let after = f.escrow.get_job();
    assert_eq!(after.milestones.len(), before.milestones.len());
    assert_eq!(total(&env, &f), total_before);
}

#[test]
fn increase_milestone_fee_token_rejected_atomically() {
    let env = Env::default();
    let f = setup(&env, true, FundingMode::Upfront);
    env.as_contract(&f.escrow.address, || {
        let mut meta = MilestoneEscrow::load_job_meta(&env).unwrap();
        meta.funded = true;
        MilestoneEscrow::store_job_meta(&env, &meta);
    });
    assert_eq!(
        f.escrow
            .try_increase_milestone(&f.client, &f.freelancer, &0u32, &1_500),
        Err(Ok(Error::InsufficientBalance))
    );
    let job = f.escrow.get_job();
    assert_eq!(job.milestones.get(0).unwrap().amount, 1_000);
    assert_eq!(total(&env, &f), 3_000);
}
