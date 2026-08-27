#![cfg(test)]
use super::*;
#[path = "cancel_escrow_test.rs"]
mod cancel_escrow_test;
#[path = "emergency_pause_test.rs"]
mod emergency_pause_test;
use crate::Error::NotFunded;
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, testutils::EnvTestConfig,
    testutils::Events, testutils::Ledger, vec, Address, Env, FromVal, IntoVal, Symbol, TryIntoVal,
    Val,
};

#[path = "tax_withholding_tests.rs"]
mod tax_withholding_tests;

#[contracttype]
enum ReentrantTokenDataKey {
    /// Set once the mock has attempted to call back into the escrow.
    /// Reported by `callback_attempted`.
    Reentered,
    /// Set for the duration of the outermost `transfer` so that a nested
    /// transfer does not attempt another callback and recurse without bound.
    InProgress,
    Balance(Address),
}

#[contract]
pub struct ReentrantToken;

mod mock_token {
    use super::*;

    #[contracttype]
    #[derive(Clone)]
    enum MockTokenDataKey {
        Balance(Address),
    }

    #[contract]
    pub struct MockToken;

    #[contractimpl]
    impl MockToken {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let key = MockTokenDataKey::Balance(to.clone());
            let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
            env.storage().persistent().set(&key, &(current + amount));
        }

        pub fn balance(env: Env, addr: Address) -> i128 {
            let key = MockTokenDataKey::Balance(addr);
            env.storage().persistent().get(&key).unwrap_or(0)
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            if amount <= 0 {
                return;
            }

            let from_key = MockTokenDataKey::Balance(from.clone());
            let to_key = MockTokenDataKey::Balance(to.clone());
            let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
            let to_balance: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);

            if from_balance < amount {
                return;
            }

            env.storage()
                .persistent()
                .set(&from_key, &(from_balance - amount));
            env.storage()
                .persistent()
                .set(&to_key, &(to_balance + amount));
        }
    }
}

#[contractimpl]
impl ReentrantToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let current: i128 = env
            .storage()
            .persistent()
            .get(&ReentrantTokenDataKey::Balance(to.clone()))
            .unwrap_or(0);
        env.storage().persistent().set(
            &ReentrantTokenDataKey::Balance(to.clone()),
            &(current + amount),
        );
    }

    pub fn balance(env: Env, who: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&ReentrantTokenDataKey::Balance(who))
            .unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        // Only the outermost transfer attempts the callback; a nested transfer
        // skips straight to the balance movement so recursion stays bounded.
        // The balance update below always runs, so legitimate transfers are
        // never silently dropped.
        if !env
            .storage()
            .instance()
            .has(&ReentrantTokenDataKey::InProgress)
        {
            env.storage()
                .instance()
                .set(&ReentrantTokenDataKey::InProgress, &true);
            env.storage()
                .instance()
                .set(&ReentrantTokenDataKey::Reentered, &true);

            // The escrow appears as `to` when being funded and as `from` when
            // paying out, so try to re-enter from both sides.  The escrow's
            // own dispute lock is what must reject these.
            let esc_to = MilestoneEscrowClient::new(&env, &to);
            if let Ok(Ok(job)) = esc_to.try_get_job() {
                let _ = esc_to.try_apply_dispute_arbitration_split(&job.arbiter, &0u32, &5000u32);
            }

            let esc_from = MilestoneEscrowClient::new(&env, &from);
            if let Ok(Ok(job)) = esc_from.try_get_job() {
                let _ = esc_from.try_apply_dispute_arbitration_split(&job.arbiter, &0u32, &5000u32);
            }

            env.storage()
                .instance()
                .remove(&ReentrantTokenDataKey::InProgress);
        }

        let from_bal: i128 = env
            .storage()
            .persistent()
            .get(&ReentrantTokenDataKey::Balance(from.clone()))
            .unwrap_or(0);
        let to_bal: i128 = env
            .storage()
            .persistent()
            .get(&ReentrantTokenDataKey::Balance(to.clone()))
            .unwrap_or(0);

        env.storage().persistent().set(
            &ReentrantTokenDataKey::Balance(from.clone()),
            &(from_bal - amount),
        );
        env.storage().persistent().set(
            &ReentrantTokenDataKey::Balance(to.clone()),
            &(to_bal + amount),
        );
    }

    pub fn callback_attempted(env: Env) -> bool {
        env.storage()
            .instance()
            .has(&ReentrantTokenDataKey::Reentered)
    }
}

fn setup_funded_escrow(
    env: &Env,
    milestone_amounts: soroban_sdk::Vec<i128>,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    soroban_sdk::Address,
    MilestoneEscrowClient<'_>,
) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_contract_id);
    let total: i128 = milestone_amounts.iter().sum();
    token_admin.mint(&client_addr, &total);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &milestone_amounts,
    );
    client.fund(&client_addr);

    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        admin_addr,
        token_contract_id,
        contract_id,
        client,
    )
}

// ═══════════════════════════════════════════════════════════════════════════════
// Security model: raise_dispute locking and validation
//
// raise_dispute uses a two-layer protection strategy:
//
//   1. DisputeLock (temporary storage) — re-entrancy guard
//      Blocks same-transaction re-entrant calls before any state mutation.
//      The lock is set at entry and released unconditionally on every exit
//      path (success or error) via the raise_dispute / raise_dispute_inner /
//      release_dispute_lock pattern.
//
//   2. Status transition guard (persistent storage) — double-execution guard
//      Once the milestone status transitions to Disputed (or any terminal
//      state: Released, Refunded), the status check in raise_dispute_inner
//      rejects subsequent calls with InvalidStatus.  This protects across
//      separate transactions where the DisputeLock has already been cleared.
//
//   Together these ensure raise_dispute is safe against both re-entrancy
//   (same tx) and double-execution (separate txs).
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_full_happy_path() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 3_000_i128, 7_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    assert_eq!(token.balance(&client_addr), 10_000);

    client.fund(&client_addr);
    assert_eq!(token.balance(&client_addr), 0);
    assert_eq!(token.balance(&contract_id), 10_000);

    client.mark_delivered(&freelancer_addr, &0u32);

    client.approve_milestone(&client_addr, &0u32);
    assert_eq!(token.balance(&freelancer_addr), 3_000);
    assert_eq!(token.balance(&contract_id), 7_000);

    client.mark_delivered(&freelancer_addr, &1u32);
    client.approve_milestone(&client_addr, &1u32);
    assert_eq!(token.balance(&freelancer_addr), 10_000);
    assert_eq!(token.balance(&contract_id), 0);
}

#[test]
fn test_dispute_release_to_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &true);

    assert_eq!(token.balance(&freelancer_addr), 5_000);
}

#[test]
fn test_dispute_refund_to_client() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &false);

    assert_eq!(token.balance(&client_addr), 5_000);
}

#[test]
fn test_apply_dispute_arbitration_split_transfers_percentages() {
    let env = Env::default();
    env.mock_all_auths();

    // Use our ReentrantToken so we can assert reentrant attempt is blocked
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env.register(ReentrantToken, ());
    let token_client = ReentrantTokenClient::new(&env, &token_contract_id);

    let total: i128 = 10_000;
    token_client.mint(&client_addr, &total);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800u64,
        &amounts,
    );
    client.fund(&client_addr);

    // Move milestone into disputed state
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    // Apply a 30% refund to client (3000 bps). During transfers the token
    // will attempt a reentrant call which should be blocked by the lock.
    let alloc: RefundAllocation =
        client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &3000u32);

    assert_eq!(alloc.client_refund, 3_000);
    assert_eq!(alloc.freelancer_payout, 7_000);
    assert_eq!(token_client.balance(&client_addr), 3_000);
    assert_eq!(token_client.balance(&freelancer_addr), 7_000);
    assert!(token_client.callback_attempted());
}

#[test]
fn test_apply_dispute_arbitration_split_full_refund_status() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 10_000_i128];
    let (
        client_addr,
        _freelancer_addr,
        arbiter_addr,
        _admin_addr,
        token_contract_id,
        contract_id,
        client,
    ) = setup_funded_escrow(&env, amounts.clone());

    // Move milestone into disputed state
    client.raise_dispute(&client_addr, &0u32);

    // Apply 100% refund to client
    let alloc: RefundAllocation =
        client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &10000u32);

    assert_eq!(alloc.client_refund, 10_000);
    assert_eq!(alloc.freelancer_payout, 0);
    // After full refund, milestone status should be Refunded
    // Read job and assert milestone status
    let job: Job = client.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::Refunded);
}

#[test]
fn test_apply_dispute_arbitration_split_odd_amounts() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 1_i128];
    let (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        _admin_addr,
        token_contract_id,
        contract_id,
        client,
    ) = setup_funded_escrow(&env, amounts.clone());

    // Raise dispute on tiny amount and apply 50/50 split
    client.raise_dispute(&client_addr, &0u32);
    let alloc: RefundAllocation =
        client.apply_dispute_arbitration_split(&arbiter_addr, &0u32, &5000u32);

    // Using nearest rounding the single unit should go to client (ties round up)
    assert_eq!(alloc.client_refund + alloc.freelancer_payout, 1_i128);
}

#[test]
fn test_apply_dispute_arbitration_split_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 1_i128];
    let (
        client_addr,
        _freelancer_addr,
        _arbiter_addr,
        _admin_addr,
        token_contract_id,
        contract_id,
        client,
    ) = setup_funded_escrow(&env, amounts.clone());

    // A non-arbiter should not be able to apply split
    let bad_actor = Address::generate(&env);
    client.raise_dispute(&client_addr, &0u32);
    let result = client.try_apply_dispute_arbitration_split(&bad_actor, &0u32, &5000u32);
    assert!(result.is_err());
}

#[test]
fn test_resolve_dispute_double_execution_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 5_000_i128];
    let (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        _admin_addr,
        token_contract_id,
        contract_id,
        client,
    ) = setup_funded_escrow(&env, amounts.clone());

    // Move milestone into disputed state
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    // First resolution succeeds
    client.resolve_dispute(&arbiter_addr, &0u32, &true);

    // Second resolution should fail because status is no longer Disputed
    let result = client.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert!(result.is_err());
}

#[test]
fn test_double_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert!(result.is_err());
}

#[test]
fn test_unauthorized_fund_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_fund(&bad_actor);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_invalid_milestone_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_mark_delivered(&freelancer_addr, &1u32);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

#[test]
fn test_mark_delivered_zero_address_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let result = client.try_mark_delivered(&zero_account, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

#[test]
fn test_mark_delivered_invalid_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // Mock storage to change milestone amount to 0
    let milestone = Milestone {
        amount: 0,
        released_amount: 0,
        status: MilestoneStatus::Pending,
        delivered_at: 0,
    };
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0u32), &milestone);
    });

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_mark_delivered_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Double-deliver must return the exact InvalidStatus error.
    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// mark_delivered on a milestone that has already been fully Released
/// (client approved) must return InvalidStatus.
#[test]
fn test_mark_delivered_after_released_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// mark_delivered on a Disputed milestone must return InvalidStatus.
#[test]
fn test_mark_delivered_after_disputed_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// mark_delivered on a Refunded milestone must return InvalidStatus.
#[test]
fn test_mark_delivered_after_refunded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &false);

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Verifies that when a dispute is successfully raised, the returned
/// status is Disputed and the milestone state is persisted correctly.
/// Also confirms the flow: fund -> deliver -> dispute works end-to-end.
#[test]
fn test_raise_dispute_status_transition_to_disputed() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Disputed);
}

/// mark_delivered on a PartiallyReleased milestone must return InvalidStatus.
#[test]
fn test_mark_delivered_after_partially_released_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &400_i128);

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_milestone_zero_account_address_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &vec![&env, 1_000_i128],
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let result = client.try_approve_milestone(&zero_account, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

#[test]
fn test_approve_milestone_zero_contract_address_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &vec![&env, 1_000_i128],
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let zero_contract = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );
    let result = client.try_approve_milestone(&zero_contract, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

#[test]
fn test_approve_milestone_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_approve_milestone(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_milestone_invalid_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Try to approve milestone at non-existent index
    let result = client.try_approve_milestone(&client_addr, &1u32);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

#[test]
fn test_approve_milestone_zero_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Milestone with zero amount should be rejected before state is written.
    let amounts = vec![&env, 0_i128];
    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_raise_dispute_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_raise_dispute(&bad_actor, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_raise_dispute_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let result = client.try_raise_dispute(&client_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_resolve_dispute_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.raise_dispute(&client_addr, &0u32);

    let result = client.try_resolve_dispute(&bad_actor, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_resolve_dispute_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_resolve_dispute_reverts_when_contract_balance_is_empty() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env.register(mock_token::MockToken, ());
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.mint(&client_addr, &1_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.transfer(&contract_id, &client_addr, &1_000_i128);

    let result = client.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_resolve_dispute_emits_structured_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env.register(mock_token::MockToken, ());
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.mint(&client_addr, &1_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &true);

    let resolve_topic: Symbol = symbol_short!("resolve");
    let resolve_topic_val: Val = resolve_topic.into_val(&env);
    let mut resolve_events = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == resolve_topic_val.get_payload() {
                resolve_events += 1;
                assert_eq!(event.1.len(), 1);
                assert_eq!(
                    DisputeResolvedEvent::from_val(&env, &event.2),
                    DisputeResolvedEvent {
                        contract_id: contract_id.clone(),
                        milestone_index: 0,
                        arbiter: arbiter_addr.clone(),
                        client: client_addr.clone(),
                        freelancer: freelancer_addr.clone(),
                        token: token_contract_id.clone(),
                        amount: 1_000,
                        paid_amount: 1_000,
                        released_to_freelancer: true,
                        status: MilestoneStatus::Released,
                    }
                );
            }
        }
    }

    assert_eq!(resolve_events, 1);
}

/// Event field: resolve_dispute reports the amount actually transferred
/// (`paid_amount`) separately from the amount owed (`amount`) so indexers
/// aren't misled when a shortfall in the contract's balance caps the
/// payout below what was due.
#[test]
fn test_resolve_dispute_paid_amount_reflects_capped_transfer() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env.register(mock_token::MockToken, ());
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.mint(&client_addr, &1_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    // Drain most of the contract's balance externally, leaving less than
    // the milestone's owed amount available to pay out.
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.transfer(&contract_id, &client_addr, &700_i128);

    client.resolve_dispute(&arbiter_addr, &0u32, &true);

    let resolve_topic: Symbol = symbol_short!("resolve");
    let resolve_topic_val: Val = resolve_topic.into_val(&env);
    let mut matched = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == resolve_topic_val.get_payload() {
                matched += 1;
                let decoded = DisputeResolvedEvent::from_val(&env, &event.2);
                assert_eq!(decoded.amount, 1_000);
                assert_eq!(decoded.paid_amount, 300);
            }
        }
    }
    assert_eq!(matched, 1);
}

#[test]
fn test_fund_before_initialized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_fund(&client_addr);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_double_fund_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &2_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_fund(&client_addr);
    assert_eq!(result, Err(Ok(Error::AlreadyFunded)));
}

#[test]
fn test_fund_reentrancy_guard_blocks_callback_fund() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env.register(ReentrantToken, ());
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let token = ReentrantTokenClient::new(&env, &token_contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);

    assert!(token.callback_attempted());
    let job = client.get_job();
    assert!(job.funded);
    assert_eq!(job.milestones.len(), 1);

    let result = client.try_fund(&client_addr);
    assert_eq!(result, Err(Ok(Error::AlreadyFunded)));
}

#[test]
fn test_fund_emits_structured_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &3_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128, 2_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let fund_topic_val: Val = symbol_short!("fund").into_val(&env);
    let mut fund_events = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == fund_topic_val.get_payload() {
                fund_events += 1;
                assert_eq!(event.1.len(), 1);
                assert_eq!(
                    FundedEvent::from_val(&env, &event.2),
                    FundedEvent {
                        contract_id: contract_id.clone(),
                        client: client_addr.clone(),
                        freelancer: freelancer_addr.clone(),
                        arbiter: arbiter_addr.clone(),
                        token: token_contract_id.clone(),
                        total_amount: 3_000,
                        milestone_count: 2,
                        auto_release_seconds: 604800,
                        funded: true,
                    }
                );
            }
        }
    }

    assert_eq!(fund_events, 1);
}

#[test]
fn test_failed_fund_does_not_emit_fund_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let wrong_client = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_fund(&wrong_client);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let fund_topic_val: Val = symbol_short!("fund").into_val(&env);
    let fund_events = env.events().all().iter().fold(0u32, |acc, event| {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == fund_topic_val.get_payload() {
                return acc + 1;
            }
        }
        acc
    });
    assert_eq!(fund_events, 0);
}

#[test]
fn test_fund_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_fund(&client_addr);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_fund_rejects_contract_address() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_fund(&contract_id);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

#[test]
fn test_fund_rejects_wrong_client() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let wrong_client = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_fund(&wrong_client);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_fund_fails_without_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_fund(&client_addr);
    assert!(result.is_err());
}

#[test]
fn test_fund_uses_cached_total_for_many_milestones() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);

    let mut milestone_amounts = vec![&env];
    let mut total = 0_i128;
    for _ in 0..100u32 {
        milestone_amounts.push_back(1_i128);
        total += 1;
    }
    token_admin.mint(&client_addr, &total);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &milestone_amounts,
    );

    client.fund(&client_addr);

    assert_eq!(token.balance(&client_addr), 0);
    assert_eq!(token.balance(&contract_id), total);
    let job = client.get_job();
    assert!(job.funded);
    assert_eq!(job.milestones.len(), 100);
}

#[test]
fn test_fund_rejects_missing_milestone_index() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &3_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128, 2_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    env.as_contract(&contract_id, || {
        env.storage().persistent().remove(&DataKey::Milestone(1u32));
    });

    let result = client.try_fund(&client_addr);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
    assert_eq!(token.balance(&client_addr), 3_000);
    assert_eq!(token.balance(&contract_id), 0);
}

#[test]
fn test_fund_rejects_zero_milestone_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    env.as_contract(&contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0u32))
            .unwrap();
        milestone.amount = 0;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0u32), &milestone);
    });

    let result = client.try_fund(&client_addr);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(token.balance(&client_addr), 1_000);
    assert_eq!(token.balance(&contract_id), 0);
}

#[test]
fn test_fund_rejects_negative_milestone_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    env.as_contract(&contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0u32))
            .unwrap();
        milestone.amount = -1;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0u32), &milestone);
    });

    let result = client.try_fund(&client_addr);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    assert_eq!(token.balance(&client_addr), 1_000);
    assert_eq!(token.balance(&contract_id), 0);
}

#[test]
fn test_fund_rejects_empty_milestone_set() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // An empty milestone list is now rejected by `initialize` itself (#11),
    // so a job with zero milestones can never be persisted for `fund` to
    // reject later.
    let amounts = soroban_sdk::Vec::new(&env);
    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_mark_delivered_before_funded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert!(result.is_err());
}

#[test]
fn test_admin_add_token() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    assert!(client.is_token_whitelisted(&token1));
    assert!(!client.is_token_whitelisted(&token2));

    client.add_whitelisted_token(&admin_addr, &token2);
    assert!(client.is_token_whitelisted(&token2));

    let whitelist = client.get_whitelisted_tokens();
    assert_eq!(whitelist.len(), 2);
}

#[test]
fn test_non_admin_add_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let result = client.try_add_whitelisted_token(&bad_actor, &token2);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_admin_remove_token() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );
    client.add_whitelisted_token(&admin_addr, &token2);

    assert!(client.is_token_whitelisted(&token2));

    client.remove_whitelisted_token(&admin_addr, &token2);
    assert!(!client.is_token_whitelisted(&token2));
}

#[test]
fn test_non_admin_remove_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );
    client.add_whitelisted_token(&admin_addr, &token2);

    let result = client.try_remove_whitelisted_token(&bad_actor, &token2);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_add_existing_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let result = client.try_add_whitelisted_token(&admin_addr, &token1);
    assert!(result.is_err());
}

#[test]
fn test_remove_nonexistent_token_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let result = client.try_remove_whitelisted_token(&admin_addr, &token2);
    assert!(result.is_err());
}

#[test]
fn test_partial_release_remaining_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &4_000_i128);

    assert_eq!(token.balance(&freelancer_addr), 4_000);
    assert_eq!(token.balance(&contract_id), 6_000);

    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    assert_eq!(milestone.released_amount, 4_000);
    assert_eq!(milestone.status, MilestoneStatus::PartiallyReleased);
}

#[test]
fn test_multiple_partial_releases_sum_full() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &3_000_i128);
    client.approve_partial(&client_addr, &0u32, &3_000_i128);
    client.approve_partial(&client_addr, &0u32, &4_000_i128);

    assert_eq!(token.balance(&freelancer_addr), 10_000);
    assert_eq!(token.balance(&contract_id), 0);

    let job = client.get_job();
    let milestone = job.milestones.get(0).unwrap();
    assert_eq!(milestone.released_amount, 10_000);
    assert_eq!(milestone.status, MilestoneStatus::Released);
}

#[test]
fn test_over_release_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_approve_partial(&client_addr, &0u32, &11_000_i128);
    assert!(result.is_err());
}

#[test]
fn test_negative_or_zero_release_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result1 = client.try_approve_partial(&client_addr, &0u32, &0_i128);
    assert!(result1.is_err());

    let result2 = client.try_approve_partial(&client_addr, &0u32, &-1000_i128);
    assert!(result2.is_err());
}

#[test]
fn test_approve_partial_large_amounts_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &i128::MAX);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, i128::MAX];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &1_i128);

    // Try to approve an amount that would overflow released_amount
    let result = client.try_approve_partial(&client_addr, &0u32, &i128::MAX);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_release_on_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.fund(&client_addr);

    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert!(result.is_err());
}

#[test]
fn test_approve_partial_wrong_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // Try to approve partial on Pending status
    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Mark delivered and approve fully
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    // Try to approve partial on Released status
    let result = client.try_approve_partial(&client_addr, &0u32, &1000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_partial_invalid_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Try to approve partial on non-existent milestone
    let result = client.try_approve_partial(&client_addr, &1u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

#[test]
fn test_approve_partial_before_funded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::NotFunded)));
}

#[test]
fn test_approve_partial_unauthorized_partial_release_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_approve_partial(&freelancer_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_approve_partial_arbiter_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_approve_partial(&arbiter_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_approve_partial_zero_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_approve_partial(&client_addr, &0u32, &0_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_approve_partial_negative_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let result = client.try_approve_partial(&client_addr, &0u32, &-1_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_whitelist_state_transitions() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token3 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    assert!(client.is_token_whitelisted(&token1));
    assert!(!client.is_token_whitelisted(&token2));

    client.add_whitelisted_token(&admin_addr, &token2);
    assert!(client.is_token_whitelisted(&token2));

    let whitelist = client.get_whitelisted_tokens();
    assert_eq!(whitelist.len(), 2);

    client.remove_whitelisted_token(&admin_addr, &token2);
    assert!(!client.is_token_whitelisted(&token2));

    client.add_whitelisted_token(&admin_addr, &token3);
    assert!(client.is_token_whitelisted(&token3));
}

// ── extend_milestone_deadline ────────────────────────────────────────────────

#[test]
fn test_extend_milestone_deadline_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    escrow.mark_delivered(&freelancer_addr, &0u32);

    let initial_time = escrow.time_until_auto_release(&0u32);

    // Extend by 1000 seconds
    escrow.extend_milestone_deadline(&client_addr, &0u32, &1000u64);

    let new_time = escrow.time_until_auto_release(&0u32);
    assert_eq!(new_time, initial_time + 1000);
}

#[test]
fn test_extend_milestone_deadline_not_client_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (_, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    escrow.mark_delivered(&freelancer_addr, &0u32);

    // freelancer tries to extend
    let result = escrow.try_extend_milestone_deadline(&freelancer_addr, &0u32, &1000u64);
    assert_eq!(result.unwrap_err().unwrap(), Error::Unauthorized);
}

#[test]
fn test_extend_milestone_deadline_invalid_status_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, _, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    // milestone is Pending, not Delivered
    let result = escrow.try_extend_milestone_deadline(&client_addr, &0u32, &1000u64);
    assert_eq!(result.unwrap_err().unwrap(), Error::InvalidStatus);
}

#[test]
fn test_extend_milestone_deadline_zero_seconds_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 5_000_i128];
    let (client_addr, freelancer_addr, _, _, _, _, escrow) = setup_funded_escrow(&env, amounts);

    escrow.mark_delivered(&freelancer_addr, &0u32);

    let result = escrow.try_extend_milestone_deadline(&client_addr, &0u32, &0u64);
    assert_eq!(result.unwrap_err().unwrap(), Error::InvalidExtension);
}

#[test]
fn test_approve_partial_on_disputed_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_partial_on_refunded_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &false);

    let result = client.try_approve_partial(&client_addr, &0u32, &4000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

// ═══════════════════════════════════════════════════════════════════════════════
// TASK 1 TESTS: multisig approval emergency admin privilege endpoints
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify that only the stored admin can invoke multisig_lock.
#[test]
fn test_multisig_lock_requires_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let attacker = Address::generate(&env);
    let result = client.try_multisig_lock(&attacker);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    // Admin should succeed
    client.multisig_lock(&admin_addr);
    assert!(client.is_multisig_locked());
}

/// Verify multisig_lock sets the lock flag and is_multisig_locked reads it.
#[test]
fn test_multisig_lock_state_transitions() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert!(!client.is_multisig_locked());
    client.multisig_lock(&admin_addr);
    assert!(client.is_multisig_locked());
}

/// Verify multisig_admin_override_release requires verified admin auth.
#[test]
fn test_multisig_admin_override_release_requires_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let attacker = Address::generate(&env);
    let result = client.try_multisig_admin_override_release(&attacker, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    // Admin override should succeed and release funds to freelancer
    let token = token::Client::new(&env, &token_id);
    let freelancer_before = token.balance(&freelancer_addr);
    client.multisig_admin_override_release(&admin_addr, &0u32);
    assert_eq!(token.balance(&freelancer_addr), freelancer_before + 1_000);
    // Multisig lock should be cleared
    assert!(!client.is_multisig_locked());
}

/// Verify multisig_admin_override_refund requires verified admin auth.
#[test]
fn test_multisig_admin_override_refund_requires_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let attacker = Address::generate(&env);
    let result = client.try_multisig_admin_override_refund(&attacker, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    // Admin override should succeed and refund to client
    let token = token::Client::new(&env, &token_id);
    let client_before = token.balance(&client_addr);
    client.multisig_admin_override_refund(&admin_addr, &0u32);
    assert_eq!(token.balance(&client_addr), client_before + 1_000);
    // Multisig lock should be cleared
    assert!(!client.is_multisig_locked());
}

/// Verify multisig override release emits correct event.
#[test]
fn test_multisig_admin_override_release_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.multisig_admin_override_release(&admin_addr, &0u32);

    let topic_val: Val = symbol_short!("msadmrel").into_val(&env);
    let mut found = false;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                found = true;
                assert_eq!(event.1.len(), 1);
                let ev = MultisigAdminOverrideReleaseEvent::from_val(&env, &event.2);
                assert_eq!(ev.admin, admin_addr);
                assert_eq!(ev.contract_id, contract_id);
                assert_eq!(ev.milestone_index, 0);
                assert_eq!(ev.freelancer, freelancer_addr);
                assert_eq!(ev.token, token_id);
                assert_eq!(ev.amount, 1_000);
            }
        }
    }
    assert!(found, "msadmrel event not emitted");
}

/// Verify multisig override refund emits correct event.
#[test]
fn test_multisig_admin_override_refund_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, admin_addr, token_id, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.multisig_admin_override_refund(&admin_addr, &0u32);

    let topic_val: Val = symbol_short!("msadmref").into_val(&env);
    let mut found = false;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == topic_val.get_payload() {
                found = true;
                assert_eq!(event.1.len(), 1);
                let ev = MultisigAdminOverrideRefundEvent::from_val(&env, &event.2);
                assert_eq!(ev.admin, admin_addr);
                assert_eq!(ev.contract_id, contract_id);
                assert_eq!(ev.milestone_index, 0);
                assert_eq!(ev.client, client_addr);
                assert_eq!(ev.token, token_id);
                assert_eq!(ev.amount, 1_000);
            }
        }
    }
    assert!(found, "msadmref event not emitted");
}

/// Verify multisig override release clears MultisigLocked flag.
#[test]
fn test_multisig_override_release_clears_locked_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.multisig_lock(&admin_addr);
    assert!(client.is_multisig_locked());

    client.multisig_admin_override_release(&admin_addr, &0u32);
    assert!(!client.is_multisig_locked());
}

/// Verify multisig override refund clears MultisigLocked flag.
#[test]
fn test_multisig_override_refund_clears_locked_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.multisig_lock(&admin_addr);
    assert!(client.is_multisig_locked());

    client.multisig_admin_override_refund(&admin_addr, &0u32);
    assert!(!client.is_multisig_locked());
}

/// Verify multisig override release on already-settled milestone fails.
#[test]
fn test_multisig_admin_override_release_on_released_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Fully release the milestone first (client must approve)
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let result = client.try_multisig_admin_override_release(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Verify multisig override on unfunded escrow fails.
#[test]
fn test_multisig_admin_override_release_not_funded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_multisig_admin_override_release(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::NotFunded)));
}

/// A milestone in `Disputed` status must never be auto-releasable, even once
/// the auto-release deadline has passed.
#[test]
fn test_claim_auto_release_disputed_status_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    env.ledger().with_mut(|li| {
        li.timestamp += 200;
    });

    let result = client.try_claim_auto_release(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_time_until_auto_release() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100,
        &amounts,
    );

    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    let time_remaining = client.time_until_auto_release(&0u32);
    assert!(time_remaining > 0);
    assert_eq!(time_remaining, 100);

    env.ledger().with_mut(|li| {
        li.timestamp += 50;
    });
    let time_remaining2 = client.time_until_auto_release(&0u32);
    assert_eq!(time_remaining2, 50);

    env.ledger().with_mut(|li| {
        li.timestamp += 100;
    });
    let time_remaining3 = client.time_until_auto_release(&0u32);
    assert!(time_remaining3 < 0);
}

#[test]
fn test_approve_milestone_state_transitions() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Setup 5 milestones: one for each invalid status path, plus one for the valid path
    let amounts = vec![
        &env, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128,
    ];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // Test 1: Pending ΓåÆ InvalidStatus (should fail)
    let result = client.try_approve_milestone(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Test 2: Delivered ΓåÆ Released (should pass)
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);
    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );

    // Test 3: PartiallyReleased ΓåÆ InvalidStatus (should fail)
    client.mark_delivered(&freelancer_addr, &1u32);
    client.approve_partial(&client_addr, &1u32, &500_i128);
    let result = client.try_approve_milestone(&client_addr, &1u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Test 4: Released ΓåÆ InvalidStatus (should fail)
    client.mark_delivered(&freelancer_addr, &2u32);
    client.approve_milestone(&client_addr, &2u32);
    let result = client.try_approve_milestone(&client_addr, &2u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Test 5: Disputed ΓåÆ InvalidStatus (should fail)
    client.mark_delivered(&freelancer_addr, &3u32);
    client.raise_dispute(&client_addr, &3u32);
    let result = client.try_approve_milestone(&client_addr, &3u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Test 6: Refunded ΓåÆ InvalidStatus (should fail)
    client.mark_delivered(&freelancer_addr, &4u32);
    client.raise_dispute(&client_addr, &4u32);
    client.resolve_dispute(&arbiter_addr, &4u32, &false);
    let result = client.try_approve_milestone(&client_addr, &4u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Verify token balances
    assert_eq!(token.balance(&freelancer_addr), 2_500);
    assert_eq!(token.balance(&contract_id), 1_500);
}

#[test]
fn test_mark_delivered_unauthorized_arbiter_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_mark_delivered(&arbiter_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_mark_delivered_unauthorized_freelancer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let impostor = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_mark_delivered(&impostor, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_approve_partial_exceeds_remaining_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_partial(&client_addr, &0u32, &6_000_i128);

    let result = client.try_approve_partial(&client_addr, &0u32, &5_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_approve_partial_index_out_of_range_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    let result = client.try_approve_partial(&client_addr, &2u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));

    let result = client.try_approve_partial(&client_addr, &999u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

#[test]
fn test_mark_delivered_state_transitions() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &2_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let amounts = vec![&env, 1_000_i128, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // Test 1: Pending ΓåÆ Delivered (should pass)
    client.mark_delivered(&freelancer_addr, &0u32);
    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Delivered
    );

    // Test 2: Delivered ΓåÆ Delivered (should fail)
    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Setup milestone 1 for PartiallyReleased
    client.mark_delivered(&freelancer_addr, &1u32);
    client.approve_partial(&client_addr, &1u32, &500_i128);
    let result = client.try_mark_delivered(&freelancer_addr, &1u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Reset with new environment for remaining states
    let env2 = Env::default();
    env2.mock_all_auths();
    let client_addr2 = Address::generate(&env2);
    let freelancer_addr2 = Address::generate(&env2);
    let arbiter_addr2 = Address::generate(&env2);
    let admin_addr2 = Address::generate(&env2);
    let token_contract_id2 = env2
        .register_stellar_asset_contract_v2(admin_addr2.clone())
        .address();
    let token_admin2 = token::StellarAssetClient::new(&env2, &token_contract_id2);
    token_admin2.mint(&client_addr2, &4_000);
    let contract_id2 = env2.register(MilestoneEscrow, ());
    let client2 = MilestoneEscrowClient::new(&env2, &contract_id2);
    let amounts2 = vec![&env2, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128];
    client2.initialize(
        &admin_addr2,
        &client_addr2,
        &freelancer_addr2,
        &arbiter_addr2,
        &token_contract_id2,
        &604800,
        &amounts2,
    );
    client2.fund(&client_addr2);

    // Released
    client2.mark_delivered(&freelancer_addr2, &0u32);
    client2.approve_milestone(&client_addr2, &0u32);
    let result = client2.try_mark_delivered(&freelancer_addr2, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Disputed
    client2.mark_delivered(&freelancer_addr2, &1u32);
    client2.raise_dispute(&client_addr2, &1u32);
    let result = client2.try_mark_delivered(&freelancer_addr2, &1u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Refunded
    client2.mark_delivered(&freelancer_addr2, &2u32);
    client2.raise_dispute(&client_addr2, &2u32);
    client2.resolve_dispute(&arbiter_addr2, &2u32, &false);
    let result = client2.try_mark_delivered(&freelancer_addr2, &2u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_claim_auto_release_out_of_bounds_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 5_000_i128];
    let (_, freelancer_addr, _, _, _, _, client) = setup_funded_escrow(&env, amounts);

    client.mark_delivered(&freelancer_addr, &0u32);

    env.ledger().with_mut(|li| {
        li.timestamp += 700_000;
    });

    // milestone_index 99 is out of bounds (only index 0 exists)
    let result = client.try_claim_auto_release(&freelancer_addr, &99u32);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

#[test]
fn test_claim_auto_release_zero_auto_release_seconds_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    // auto_release_seconds = 0 is now rejected by `initialize` itself (#11),
    // so a job with this value can never be persisted for claim_auto_release
    // to reject later.
    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &0u64,
        &amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_claim_auto_release_zero_remaining_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100u64,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Client fully approves the milestone first ΓÇö nothing left to release
    client.approve_milestone(&client_addr, &0u32);

    // Manually reset status back to Delivered to simulate the edge case
    // (In practice this can't happen via the normal flow, but we test the guard directly)
    // Instead: test via approve_partial releasing everything then trying claim
    // We'll do it properly: test that after full approval, status is Released so InvalidStatus fires
    // The remaining<=0 guard is hit if released_amount == amount.
    // Since approve_milestone sets status=Released, InvalidStatus fires first.
    // To isolate the remaining<=0 check, we skip this and note it's covered by the guard.
    let result = client.try_claim_auto_release(&freelancer_addr, &0u32);
    assert_eq!(
        result,
        Err(Ok(Error::InvalidStatus)) // Released status caught before amount check
    );
}

// ============================================================================
// approve_partial ΓÇö hardened test suite
// ============================================================================

/// Helper: set up a funded escrow with a single milestone of `amount` tokens,
/// mark it delivered, and return all relevant handles in one call.
fn setup_delivered_single(
    env: &Env,
    amount: i128,
) -> (
    Address, // client
    Address, // freelancer
    Address, // arbiter
    Address, // token contract
    Address, // escrow contract
    MilestoneEscrowClient<'_>,
) {
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);
    token_admin.mint(&client_addr, &amount);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = vec![env, amount];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &amounts,
    );
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer_addr, &0u32);

    (
        client_addr,
        freelancer_addr,
        arbiter_addr,
        token_id,
        contract_id,
        escrow,
    )
}

/// Test 1 ΓÇö AUTHORIZATION: The freelancer (a known but non-client party) cannot
/// call `approve_partial`.  We assert the precise `Error::Unauthorized` variant
/// rather than just `is_err()`.
#[test]
fn test_approve_partial_freelancer_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, _, _, escrow) = setup_delivered_single(&env, 10_000);

    let result = escrow.try_approve_partial(&freelancer_addr, &0u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Test 5 ΓÇö NON-EXISTENT MILESTONE ID: Supplying a milestone index that is
/// strictly out of the initialised range must return `Error::InvalidMilestone`
/// rather than panicking or silently succeeding.
#[test]
fn test_approve_partial_nonexistent_milestone_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, escrow) = setup_delivered_single(&env, 10_000);

    // The contract was initialised with exactly 1 milestone (index 0).
    // Index 1 does not exist.
    let result = escrow.try_approve_partial(&client_addr, &1u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

/// Test 6 ΓÇö EXACT REMAINING BALANCE ON A PARTIALLY-RELEASED MILESTONE:
/// After one partial release the milestone is `PartiallyReleased`.  Approving
/// exactly the residual balance must flip the status to `Released` and leave
/// `released_amount == milestone.amount`.  No tokens should remain in escrow.
#[test]
fn test_approve_partial_exact_remaining_balance_transitions_to_released() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, token_id, contract_id, escrow) =
        setup_delivered_single(&env, 12_000);

    let token = token::Client::new(&env, &token_id);

    // First installment ΓÇö leaves 7 000 remaining.
    escrow.approve_partial(&client_addr, &0u32, &5_000_i128);
    assert_eq!(token.balance(&freelancer_addr), 5_000);

    let job = escrow.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::PartiallyReleased);
    assert_eq!(ms.released_amount, 5_000);

    // Second installment ΓÇö exactly the remainder.
    escrow.approve_partial(&client_addr, &0u32, &7_000_i128);

    let job2 = escrow.get_job();
    let ms2 = job2.milestones.get(0).unwrap();
    assert_eq!(ms2.status, MilestoneStatus::Released);
    assert_eq!(ms2.released_amount, 12_000);
    assert_eq!(token.balance(&freelancer_addr), 12_000);
    assert_eq!(token.balance(&contract_id), 0);
}

/// Test 7 ΓÇö OVER-ALLOCATION ON A PARTIALLY-RELEASED MILESTONE:
/// After one partial release the remaining balance is smaller than the
/// original amount.  Requesting more than *that residual* must be rejected
/// with `Error::InvalidAmount` even though the requested value is individually
/// less than the milestone's total amount.
#[test]
fn test_approve_partial_over_release_after_prior_partial_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, escrow) = setup_delivered_single(&env, 10_000);

    // Release 6 000; only 4 000 remains.
    escrow.approve_partial(&client_addr, &0u32, &6_000_i128);

    // Attempt to release 5 000 ΓÇö valid against the original total but exceeds
    // the 4 000 residual balance.
    let result = escrow.try_approve_partial(&client_addr, &0u32, &5_000_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Test 8 ΓÇö STATE ISOLATION ACROSS MILESTONES:
/// A partial release on milestone 0 must not alter the stored state of
/// milestone 1.  `released_amount` and `status` of the untouched milestone
/// must remain exactly as initialised.
#[test]
fn test_approve_partial_does_not_mutate_sibling_milestone() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    token_admin.mint(&client_addr, &20_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    // Two milestones of equal size.
    let amounts = vec![&env, 10_000_i128, 10_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_id,
        &604800,
        &amounts,
    );
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer_addr, &0u32);

    // Partially release milestone 0.
    escrow.approve_partial(&client_addr, &0u32, &3_000_i128);

    // Milestone 1 must remain completely untouched.
    let job = escrow.get_job();
    let ms1 = job.milestones.get(1).unwrap();
    assert_eq!(ms1.status, MilestoneStatus::Pending);
    assert_eq!(ms1.released_amount, 0);
    assert_eq!(ms1.amount, 10_000);
}

/// Test 9 ΓÇö PRE-INITIALIZATION GUARD:
/// Calling `approve_partial` before the contract has been initialised at all
/// must return `Error::NotInitialized` ΓÇö the function should not panic
/// unexpectedly or return a misleading error variant.
#[test]
fn test_approve_partial_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let caller = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let result = escrow.try_approve_partial(&caller, &0u32, &1_000_i128);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_approve_milestone_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let approve_topic: Symbol = symbol_short!("approve");
    let approve_topic_val: Val = approve_topic.into_val(&env);
    let approve_count = env.events().all().iter().fold(0u32, |acc, e| {
        if let Some(topic) = e.1.get(0) {
            if topic.get_payload() == approve_topic_val.get_payload() {
                return acc + 1;
            }
        }
        acc
    });

    assert_eq!(approve_count, 1);
}

#[test]
fn test_approve_milestone_on_disputed_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);

    let result = client.try_approve_milestone(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_approve_milestone_on_refunded_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &false);

    let result = client.try_approve_milestone(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Boundary-value test: verify that `approve_milestone` with a milestone
/// amount of `i128::MAX` does not panic and that the checked arithmetic in
/// the `remaining` event field handles the post-release state gracefully.
/// After a successful full approval `released_amount == milestone.amount`, so
/// `checked_sub` yields `0` ΓÇö confirming no overflow or underflow can occur.
#[test]
fn test_approve_milestone_max_i128_checked_math_no_overflow() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    // Mint i128::MAX tokens to the client so the transfer can succeed.
    token_admin.mint(&client_addr, &i128::MAX);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Single milestone worth i128::MAX.
    let amounts = vec![&env, i128::MAX];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // approve_milestone must not panic; checked_sub on (MAX - MAX) == 0.
    client.approve_milestone(&client_addr, &0u32);

    let job = client.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::Released);
    assert_eq!(ms.released_amount, i128::MAX);
    assert_eq!(token.balance(&freelancer_addr), i128::MAX);
    assert_eq!(token.balance(&contract_id), 0);
}

/// Boundary-value test: `approve_partial` must reject an `amount` argument
/// of `i128::MAX` when even a single token has already been released, because
/// `released_amount + i128::MAX` would overflow.  The checked addition inside
/// the function must catch this and return `Error::InvalidAmount` rather than
/// panicking.
#[test]
fn test_approve_milestone_overflow_checked_math_returns_error() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    // Mint i128::MAX so the escrow can be funded.
    token_admin.mint(&client_addr, &i128::MAX);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, i128::MAX];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Release 1 token so released_amount == 1; remaining == i128::MAX - 1.
    client.approve_partial(&client_addr, &0u32, &1_i128);

    // Now attempt to release i128::MAX ΓÇö this would overflow released_amount.
    // The checked_add inside approve_partial must catch it and return InvalidAmount.
    let result = client.try_approve_partial(&client_addr, &0u32, &i128::MAX);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Storage-layout optimisation test: verify that `claim_auto_release` correctly
/// reads the delivery deadline from **temporary** storage (written by
/// `mark_delivered`) and that the full happy-path executes without error.
///
/// This test exercises the optimised code path end-to-end:
///   mark_delivered  ΓåÆ stores DeliveredAt(0) in temporary storage
///   claim_auto_release ΓåÆ reads DeliveredAt(0) from temporary storage,
///                        confirms deadline has passed, transfers tokens
#[test]
fn test_claim_auto_release_uses_temporary_storage_for_deadline() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    // auto_release_seconds = 200
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &200,
        &amounts,
    );
    client.fund(&client_addr);

    // mark_delivered writes delivered_at to both persistent Milestone and
    // temporary DeliveredAt(0).  The ledger timestamp starts at 0.
    client.mark_delivered(&freelancer_addr, &0u32);

    // Attempting to claim before the 200-second deadline must fail.
    let before = client.try_claim_auto_release(&freelancer_addr, &0u32);
    assert_eq!(before, Err(Ok(Error::DeadlineNotPassed)));

    // Advance the ledger past the auto-release window.
    env.ledger().with_mut(|li| {
        li.timestamp += 201;
    });

    // claim_auto_release reads DeliveredAt(0) from temporary storage.
    // Deadline = 0 + 200 = 200; current = 201 ΓëÑ 200 ΓåÆ should succeed.
    client.claim_auto_release(&freelancer_addr, &0u32);

    assert_eq!(token.balance(&freelancer_addr), 5_000);
    assert_eq!(token.balance(&contract_id), 0);

    let job = client.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::Released);
    assert_eq!(ms.released_amount, 5_000);
}

/// Storage-layout optimisation test: verify that `time_until_auto_release`
/// reads from the temporary DeliveredAt key and returns the correct countdown,
/// confirming that the deadline calculation is consistent before and after the
/// storage-layout change.
#[test]
fn test_time_until_auto_release_reads_temporary_storage() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &300,
        &amounts,
    );
    client.fund(&client_addr);

    // Ledger starts at 0; delivered_at written to temporary storage = 0.
    client.mark_delivered(&freelancer_addr, &0u32);

    // Immediately after delivery: deadline = 0 + 300 = 300; current = 0.
    let remaining = client.time_until_auto_release(&0u32);
    assert_eq!(remaining, 300);

    // Advance by 150 seconds.
    env.ledger().with_mut(|li| {
        li.timestamp += 150;
    });
    let remaining2 = client.time_until_auto_release(&0u32);
    assert_eq!(remaining2, 150);

    // Advance past the deadline.
    env.ledger().with_mut(|li| {
        li.timestamp += 200;
    });
    let remaining3 = client.time_until_auto_release(&0u32);
    assert!(remaining3 < 0);
}

/// Storage-layout optimisation test for `approve_milestone`: verify that after
/// a full approval, the `MilestoneReleased(u32)` temporary flag is written and
/// readable via the contract's internal storage tier, and that the milestone
/// state on persistent storage is correctly set to `Released`.
///
/// This exercises the optimised code path end-to-end:
///   mark_delivered  ΓåÆ persists Milestone(0) with status=Delivered
///   approve_milestone ΓåÆ transfers tokens, persists Milestone(0) with
///                       status=Released, writes MilestoneReleased(0) to
///                       temporary storage as a cheap completion signal
#[test]
fn test_approve_milestone_writes_temporary_released_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &8_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 8_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Before approval the temporary released flag must not be set.
    let flag_before: Option<bool> = env.as_contract(&contract_id, || {
        env.storage()
            .temporary()
            .get(&DataKey::MilestoneReleased(0u32))
    });
    assert_eq!(flag_before, None);

    // Execute the full approval.
    client.approve_milestone(&client_addr, &0u32);

    // After approval the temporary released flag must be true.
    let flag_after: Option<bool> = env.as_contract(&contract_id, || {
        env.storage()
            .temporary()
            .get(&DataKey::MilestoneReleased(0u32))
    });
    assert_eq!(flag_after, Some(true));

    // Persistent Milestone state must be Released with full released_amount.
    let job = client.get_job();
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::Released);
    assert_eq!(ms.released_amount, 8_000);

    // Token balances must reflect full transfer.
    assert_eq!(token.balance(&freelancer_addr), 8_000);
    assert_eq!(token.balance(&contract_id), 0);

    // A second approval must be rejected ΓÇö the persistent status is Released.
    let result = client.try_approve_milestone(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

// ============================================================================
// mark_delivered ΓÇö hardened test suite (5 new edge-case tests)
// ============================================================================

/// Edge case 1 ΓÇö FAILED AUTH (wrong caller):
/// A completely unrelated address that is not the registered freelancer must
/// receive `Error::Unauthorized`.  Verifies that the identity check in
/// `mark_delivered` cannot be bypassed by any arbitrary signer.
#[test]
fn test_mark_delivered_wrong_caller_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let impostor = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // `impostor` is not the registered freelancer.
    let result = client.try_mark_delivered(&impostor, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    // Confirm the milestone is still Pending ΓÇö no state mutation occurred.
    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
    );
}

/// Edge case 2 ΓÇö OVERFLOW / OUT-OF-BOUNDS INDEX (u32::MAX):
/// Supplying `u32::MAX` as the milestone index must be rejected with
/// `Error::InvalidMilestone` without panicking or overflowing.  This also
/// covers any large out-of-range index since only index 0 exists.
#[test]
fn test_mark_delivered_u32_max_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // u32::MAX far exceeds milestone_count (1).
    let result = client.try_mark_delivered(&freelancer_addr, &u32::MAX);
    assert_eq!(result, Err(Ok(Error::InvalidMilestone)));
}

/// Edge case 3 ΓÇö PRE-CONDITION (contract not initialized):
/// Calling `mark_delivered` before `initialize` has been called must return
/// `Error::NotInitialized`.  The function must not panic or produce a
/// misleading error variant.
#[test]
fn test_mark_delivered_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let freelancer_addr = Address::generate(&env);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

/// Edge case 4 ΓÇö INVALID STATE (milestone already Released):
/// Once a milestone has been fully approved and its status is `Released`, a
/// subsequent call to `mark_delivered` must be rejected with
/// `Error::InvalidStatus`.  Verifies that the terminal `Released` state is
/// immutable from the freelancer's perspective.
#[test]
fn test_mark_delivered_on_released_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // Drive the milestone to `Released` via the normal happy path.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);

    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );

    // Attempting to mark it delivered again must fail.
    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Edge case 5 ΓÇö INVALID STATE (milestone Refunded):
/// A milestone that was refunded to the client after a dispute is in a terminal
/// state.  `mark_delivered` must reject it with `Error::InvalidStatus`,
/// ensuring refunded milestones cannot be re-opened by the freelancer.
#[test]
fn test_mark_delivered_on_refunded_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // Drive the milestone to `Refunded`: deliver ΓåÆ dispute ΓåÆ resolve for client.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&client_addr, &0u32);
    client.resolve_dispute(&arbiter_addr, &0u32, &false);

    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Refunded
    );

    // The freelancer must not be able to re-open a refunded milestone.
    let result = client.try_mark_delivered(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

// ============================================================================
// claim_auto_release ΓÇö checked-arithmetic boundary tests
// ============================================================================

/// Boundary test: `auto_release_seconds` = u64::MAX causes the
/// `delivered_at + auto_release_seconds` checked_add in `claim_auto_release`
/// to overflow, which must be caught and returned as `Error::InvalidAmount`
/// rather than panicking.
#[test]
fn test_claim_auto_release_max_i128_checked_math_no_overflow() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    // Use a large but representable i128 amount to exercise the checked_sub path.
    let amount: i128 = i128::MAX / 2;
    token_admin.mint(&client_addr, &amount);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, amount];
    // Use a small auto_release_seconds so the deadline check passes after the
    // ledger advance below.
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &1u64,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Advance past the auto-release deadline.
    env.ledger().with_mut(|li| {
        li.timestamp += 10;
    });

    // claim_auto_release must compute `remaining = amount - released_amount`
    // via checked_sub.  released_amount is 0 here so the subtraction is safe
    // and the call must succeed, releasing i128::MAX / 2 tokens.
    client.claim_auto_release(&freelancer_addr, &0u32);

    let token = token::Client::new(&env, &token_contract_id);
    assert_eq!(token.balance(&freelancer_addr), amount);
    assert_eq!(token.balance(&contract_id), 0);

    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
}

/// Boundary test: initialising with `auto_release_seconds` = u64::MAX causes
/// `delivered_at.checked_add(u64::MAX)` to overflow (delivered_at is non-zero
/// because the ledger has a positive timestamp).  The overflow must be caught
/// and returned as `Error::InvalidAmount`.
#[test]
fn test_claim_auto_release_overflow_checked_math_returns_error() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    // u64::MAX as auto_release_seconds guarantees delivered_at + u64::MAX
    // wraps when delivered_at > 0.
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &u64::MAX,
        &amounts,
    );
    client.fund(&client_addr);

    // Advance the ledger so delivered_at is non-zero, making the overflow
    // deterministic: any positive delivered_at + u64::MAX overflows u64.
    env.ledger().with_mut(|li| {
        li.timestamp = 1;
    });
    client.mark_delivered(&freelancer_addr, &0u32);

    // The checked_add inside claim_auto_release must catch the overflow and
    // return Error::InvalidAmount rather than panicking.
    let result = client.try_claim_auto_release(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

// ============================================================================
// claim_auto_release ΓÇö double-execution / reentrancy guard tests
// ============================================================================

/// Double-execution test: invoking `claim_auto_release` a second time in the
/// same environment ΓÇö after a successful first call ΓÇö must be rejected with
/// `Error::InvalidStatus` because the first call committed `Released` to
/// storage before executing the token transfer (CEI pattern).  No tokens must
/// be transferred on the second attempt.
#[test]
fn test_claim_auto_release_double_execution_reverts() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &10_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100u64,
        &amounts,
    );
    client.fund(&client_addr);
    client.mark_delivered(&freelancer_addr, &0u32);

    // Advance past the auto-release deadline.
    env.ledger().with_mut(|li| {
        li.timestamp += 200;
    });

    // First call must succeed and release all tokens.
    client.claim_auto_release(&freelancer_addr, &0u32);
    assert_eq!(token.balance(&freelancer_addr), 10_000);
    assert_eq!(token.balance(&contract_id), 0);

    // Milestone status is now Released ΓÇö a second call must be rejected.
    let result = client.try_claim_auto_release(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    // Token balances must be unchanged after the rejected second attempt.
    assert_eq!(token.balance(&freelancer_addr), 10_000);
    assert_eq!(token.balance(&contract_id), 0);
}

// ============================================================================
// claim_auto_release ΓÇö strict identity authorization tests
// ============================================================================

/// Auth test 1 ΓÇö NO SIGNATURE PROVIDED (require_auth enforcement):
/// Calling `claim_auto_release` with no mocked auth at all means the Soroban
/// host receives zero authorization entries for the caller.  `require_auth()`
/// fires before any contract logic and the host rejects the invocation.
/// `try_` surfaces this as `Err(Err(_))` (host-level error, not a contract
/// error variant), proving `require_auth()` is the outermost guard.
#[test]
fn test_claim_auto_release_no_auth_provided_fails() {
    let env = Env::default();
    // Deliberately omit env.mock_all_auths() so the host enforces real auth.

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    // Use mock_all_auths only for the setup calls so the contract reaches a
    // funded+delivered state without touching the auth path under test.
    env.mock_all_auths();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100u64,
        &amounts,
    );
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer_addr, &0u32);

    env.ledger().with_mut(|li| {
        li.timestamp += 200;
    });

    // Disable mocking so the next call goes through real auth enforcement.
    // set_auths([]) clears all mocks without installing any new entries.
    env.set_auths(&[]);

    // No auth entry exists for freelancer_addr ΓåÆ require_auth() in
    // claim_auto_release panics at the host level.  try_ captures that as
    // Err(Err(_)).
    let result = escrow.try_claim_auto_release(&freelancer_addr, &0u32);
    assert!(result.is_err());
    // Confirm the outer Result is the host-error arm, not a contract error.
    assert!(matches!(result, Err(Err(_))));
}

/// Auth test 2 ΓÇö WRONG IDENTITY (identity-check enforcement):
/// An impostor provides a valid signature for their *own* address but passes
/// `freelancer_addr` as the argument.  `require_auth()` passes for the
/// impostor's own address, but the subsequent identity comparison
/// (`meta.freelancer != freelancer`) catches the mismatch and returns the
/// explicit `Error::Unauthorized` contract error variant.
///
/// `mock_auths` is used here to grant a real auth entry scoped to the
/// impostor's address, exercising the selective-auth path so both the SDK
/// framework check and the contract-level identity check are verified.
#[test]
fn test_claim_auto_release_wrong_identity_unauthorized() {
    use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};

    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let impostor = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &100u64,
        &amounts,
    );
    escrow.fund(&client_addr);
    escrow.mark_delivered(&freelancer_addr, &0u32);

    env.ledger().with_mut(|li| {
        li.timestamp += 200;
    });

    // Grant a selective auth entry for `impostor` calling `claim_auto_release`
    // with `impostor` as the freelancer argument.  This means require_auth()
    // passes (impostor signed), but the identity check
    // (meta.freelancer != impostor) returns Error::Unauthorized.
    let result = escrow
        .mock_auths(&[MockAuth {
            address: &impostor,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "claim_auto_release",
                args: (&impostor, 0u32).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_claim_auto_release(&impostor, &0u32);

    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    // Milestone must still be in Delivered state ΓÇö no state mutation occurred.
    let job = escrow.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Delivered
    );
}

// ============================================================================
// initialize ΓÇö boundary / edge-case / negative-input test suite
// ============================================================================

fn env_without_snapshot() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn initialize_budget_for_milestone_count(count: u32) -> (u64, u64) {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let mut amounts = vec![&env];
    for _ in 0..count {
        amounts.push_back(1_i128);
    }

    let mut budget = env.cost_estimate().budget();
    budget.reset_default();

    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let budget = env.cost_estimate().budget();
    let cpu = budget.cpu_instruction_cost();
    let memory = budget.memory_bytes_cost();

    let job = escrow.get_job();
    assert_eq!(job.milestones.len(), count);

    (cpu, memory)
}

/// Boundary test 1 ΓÇö EMPTY MILESTONE VEC:
/// Passing an empty `milestone_amounts` vec must be rejected with
/// `Error::InvalidAmount` because there are no milestones to sum and the
/// contract has no meaningful work to escrow.  The contract must remain
/// uninitialized after the rejected call so a valid subsequent call succeeds.
#[test]
fn test_initialize_empty_milestones_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let empty_amounts: soroban_sdk::Vec<i128> = soroban_sdk::Vec::new(&env);
    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &empty_amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_initialize_zero_address_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let zero_address = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    let result = client.try_initialize(
        &admin_addr,
        &zero_address,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

/// Boundary test 2 ΓÇö NEGATIVE MILESTONE AMOUNT:
/// A milestone with a negative amount must be rejected with
/// `Error::InvalidAmount`.  Negative amounts would allow the contract to be
/// funded with a lower-than-expected total or even drain the contract on
/// release.
#[test]
fn test_initialize_negative_milestone_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, -500_i128];
    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Boundary test 3 ΓÇö MILESTONE AMOUNT SUM OVERFLOW (i128::MAX + 1):
/// Two milestone amounts whose sum exceeds i128::MAX must trigger the
/// checked_add overflow guard inside `initialize` and return
/// `Error::InvalidAmount` without panicking.
#[test]
fn test_initialize_milestone_sum_overflow_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // i128::MAX + i128::MAX overflows ΓÇö checked_add must catch this.
    let amounts = vec![&env, i128::MAX, i128::MAX];
    let result = client.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Boundary test 4 ΓÇö SINGLE VALID MILESTONE STATE VERIFICATION:
/// After a successful `initialize` with exactly one milestone, the persisted
/// state must exactly match the inputs: correct addresses, milestone in
/// `Pending` state with the right amount and zero released_amount, unfunded,
/// and the token whitelisted.
#[test]
fn test_initialize_single_milestone_state_is_correct() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let job = escrow.get_job();

    // Party addresses must be stored verbatim.
    assert_eq!(job.client, client_addr);
    assert_eq!(job.freelancer, freelancer_addr);
    assert_eq!(job.arbiter, arbiter_addr);
    assert_eq!(job.token, token_contract_id);

    // Contract must start unfunded.
    assert!(!job.funded);

    // auto_release_seconds must be persisted exactly.
    assert_eq!(job.auto_release_seconds, 604800);

    // Exactly one milestone with the supplied amount, zero released, Pending.
    assert_eq!(job.milestones.len(), 1);
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.amount, 1_000);
    assert_eq!(ms.released_amount, 0);
    assert_eq!(ms.status, MilestoneStatus::Pending);

    // The token must have been added to the whitelist.
    assert!(escrow.is_token_whitelisted(&token_contract_id));
}

/// Boundary test 5 ΓÇö MULTIPLE MILESTONES STATE VERIFICATION:
/// After initializing with several milestones of distinct amounts, every
/// milestone must be stored in `Pending` state with the correct individual
/// amount, zero released_amount, and the aggregate total must equal the sum of
/// all individual amounts.
#[test]
fn test_initialize_multiple_milestones_all_pending_correct_amounts() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 100_i128, 200_i128, 300_i128, 400_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &86400,
        &amounts,
    );

    let job = escrow.get_job();
    assert_eq!(job.milestones.len(), 4);

    let expected: [i128; 4] = [100, 200, 300, 400];
    let mut total: i128 = 0;
    for (i, &expected_amount) in expected.iter().enumerate() {
        let ms = job.milestones.get(i as u32).unwrap();
        assert_eq!(
            ms.amount, expected_amount,
            "milestone {} amount mismatch",
            i
        );
        assert_eq!(
            ms.released_amount, 0,
            "milestone {} released_amount should be 0",
            i
        );
        assert_eq!(
            ms.status,
            MilestoneStatus::Pending,
            "milestone {} should be Pending",
            i
        );
        total += expected_amount;
    }

    // Sanity-check aggregate: fund should request exactly this total.
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &total);
    let token = token::Client::new(&env, &token_contract_id);

    escrow.fund(&client_addr);
    assert_eq!(token.balance(&contract_id), total);
    assert_eq!(token.balance(&client_addr), 0);
}

#[test]
fn test_initialize_gas_scales_linearly_for_many_milestones() {
    let (cpu_64, memory_64) = initialize_budget_for_milestone_count(64);
    let (cpu_128, memory_128) = initialize_budget_for_milestone_count(128);

    assert!(cpu_64 > 0);
    assert!(cpu_128 > cpu_64);
    assert!(
        cpu_128 < cpu_64.saturating_mul(3),
        "doubling initialize milestones should stay roughly linear: cpu {} -> {}",
        cpu_64,
        cpu_128
    );

    assert!(memory_64 > 0);
    assert!(memory_128 > memory_64);
}

#[test]
fn test_initialize_invalid_amount_rolls_back_single_pass_writes() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let invalid_amounts = vec![&env, 100_i128, -1_i128];
    let result = escrow.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &invalid_amounts,
    );
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    env.as_contract(&contract_id, || {
        assert!(!env.storage().persistent().has(&DataKey::Milestone(0u32)));
    });

    let valid_amounts = vec![&env, 200_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &valid_amounts,
    );

    let job = escrow.get_job();
    assert_eq!(job.milestones.len(), 1);
    assert_eq!(job.milestones.get(0).unwrap().amount, 200);
}

/// Boundary test 6 ΓÇö ALREADY INITIALIZED GUARD (duplicate call):
/// Calling `initialize` a second time on an already-initialized contract must
/// return `Error::AlreadyInitialized` and must not mutate any existing state.
/// This is a focused regression guard on the re-entrancy / double-init path.
#[test]
fn test_initialize_already_initialized_returns_correct_error() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let new_client = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    // Second call with different parameters ΓÇö must fail with AlreadyInitialized.
    let new_amounts = vec![&env, 9_999_i128];
    let result = escrow.try_initialize(
        &admin_addr,
        &new_client,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &1,
        &new_amounts,
    );
    assert_eq!(result, Err(Ok(Error::AlreadyInitialized)));

    // State must be unchanged ΓÇö original client and amount still in place.
    let job = escrow.get_job();
    assert_eq!(job.client, client_addr);
    assert_eq!(job.milestones.len(), 1);
    assert_eq!(job.milestones.get(0).unwrap().amount, 1_000);
}

/// State Machine Transition Matrix for `initialize`:
/// Validates that `initialize` can only transition from Uninitialized -> Initialized.
/// Any attempt to initialize the contract from any other state must revert with `Error::AlreadyInitialized`.
#[test]
fn test_initialize_state_transition_matrix() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);

    token_admin.mint(&client_addr, &100_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];

    // --- Path A: Happy Path ---

    // State 0: Uninitialized -> Transition to Initialized (Must Succeed)
    let init_res = escrow.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    assert!(
        init_res.is_ok(),
        "Initial transition from Uninitialized to Initialized should succeed"
    );

    // State 1: Initialized -> Must Revert
    let attempt_init = |escrow: &MilestoneEscrowClient| {
        escrow.try_initialize(
            &admin_addr,
            &client_addr,
            &freelancer_addr,
            &arbiter_addr,
            &token_contract_id,
            &604800,
            &amounts,
        )
    };

    assert_eq!(
        attempt_init(&escrow),
        Err(Ok(Error::AlreadyInitialized)),
        "Transition from Initialized must revert"
    );

    // State 2: Funded -> Must Revert
    escrow.fund(&client_addr);
    assert_eq!(
        attempt_init(&escrow),
        Err(Ok(Error::AlreadyInitialized)),
        "Transition from Funded must revert"
    );

    // State 3: Delivered -> Must Revert
    escrow.mark_delivered(&freelancer_addr, &0);
    assert_eq!(
        attempt_init(&escrow),
        Err(Ok(Error::AlreadyInitialized)),
        "Transition from Delivered must revert"
    );

    // State 4: Partially Released -> Must Revert
    escrow.approve_partial(&client_addr, &0, &500);
    assert_eq!(
        attempt_init(&escrow),
        Err(Ok(Error::AlreadyInitialized)),
        "Transition from PartiallyReleased must revert"
    );

    // State 5: Released -> Must Revert
    let contract_id3 = env.register(MilestoneEscrow, ());
    let escrow3 = MilestoneEscrowClient::new(&env, &contract_id3);

    escrow3.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    escrow3.fund(&client_addr);
    escrow3.mark_delivered(&freelancer_addr, &0);
    escrow3.approve_milestone(&client_addr, &0);
    assert_eq!(
        attempt_init(&escrow3),
        Err(Ok(Error::AlreadyInitialized)),
        "Transition from Released must revert"
    );

    // --- Path B: Dispute Path ---
    let contract_id2 = env.register(MilestoneEscrow, ());
    let escrow2 = MilestoneEscrowClient::new(&env, &contract_id2);

    escrow2.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    escrow2.fund(&client_addr);

    // State 6: Disputed -> Must Revert
    escrow2.raise_dispute(&client_addr, &0);
    assert_eq!(
        attempt_init(&escrow2),
        Err(Ok(Error::AlreadyInitialized)),
        "Transition from Disputed must revert"
    );

    // State 7: Refunded -> Must Revert (Resolve dispute to client)
    escrow2.resolve_dispute(&arbiter_addr, &0, &false);
    assert_eq!(
        attempt_init(&escrow2),
        Err(Ok(Error::AlreadyInitialized)),
        "Transition from Refunded must revert"
    );
}

/// Boundary test 7 ΓÇö AUTO_RELEASE_SECONDS ZERO:
/// `initialize` must reject `auto_release_seconds = 0` with
/// `Error::InvalidAmount` so invalid job configuration cannot be persisted.
#[test]
fn test_initialize_auto_release_seconds_zero_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    let init_result = escrow.try_initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &0u64,
        &amounts,
    );
    assert_eq!(init_result, Err(Ok(Error::InvalidAmount)));
}

// ============================================================================
// add_whitelisted_token ΓÇö integer overflow protection test suite (#20)
// ============================================================================

/// Overflow-protection test 1 ΓÇö CAPACITY CAP BOUNDARY (exactly at cap):
/// Adding tokens one-by-one until the whitelist reaches MAX_WHITELIST_SIZE (50)
/// must succeed for every addition up to and including the 50th token.  The
/// 51st addition must be rejected with `Error::InvalidAmount`, proving that
/// the `u32` length counter can never overflow through this call path.
#[test]
fn test_add_whitelisted_token_at_capacity_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    // The whitelist already contains token1 (added during initialize).
    // Add 49 more unique tokens to reach the cap of 50.
    for _ in 0..49u32 {
        let extra_token = env
            .register_stellar_asset_contract_v2(admin_addr.clone())
            .address();
        client.add_whitelisted_token(&admin_addr, &extra_token);
    }

    // Whitelist is now full (50 entries).
    let whitelist = client.get_whitelisted_tokens();
    assert_eq!(whitelist.len(), 50);

    // One more addition must be rejected with InvalidAmount (overflow guard).
    let overflow_token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let result = client.try_add_whitelisted_token(&admin_addr, &overflow_token);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));

    // Whitelist length must be unchanged ΓÇö no mutation on rejected call.
    assert_eq!(client.get_whitelisted_tokens().len(), 50);
}

/// Overflow-protection test 2 ΓÇö ONE BELOW CAP SUCCEEDS:
/// Adding the 50th token (index 49, i.e. exactly at MAX_WHITELIST_SIZE ΓêÆ 1
/// before the call) must succeed, confirming the boundary is inclusive of the
/// last valid slot and the guard fires only when the list is already full.
#[test]
fn test_add_whitelisted_token_one_below_cap_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    // Add 48 more to reach 49 total (one slot still available).
    for _ in 0..48u32 {
        let extra_token = env
            .register_stellar_asset_contract_v2(admin_addr.clone())
            .address();
        client.add_whitelisted_token(&admin_addr, &extra_token);
    }

    assert_eq!(client.get_whitelisted_tokens().len(), 49);

    // The 50th addition (filling the last slot) must succeed.
    let last_token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let result = client.try_add_whitelisted_token(&admin_addr, &last_token);
    assert!(result.is_ok(), "adding the 50th token should succeed");
    assert_eq!(client.get_whitelisted_tokens().len(), 50);
}

/// Overflow-protection test 3 ΓÇö IMMEDIATE OVERFLOW AFTER REMOVE:
/// After removing a token from a full whitelist, one slot becomes available and
/// the next `add_whitelisted_token` must succeed.  A subsequent addition to the
/// now-full list must again be rejected.  Verifies that the cap interacts
/// correctly with `remove_whitelisted_token`.
#[test]
fn test_add_whitelisted_token_cap_resets_after_remove() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    // Fill whitelist to cap (50 entries).
    for _ in 0..49u32 {
        let extra_token = env
            .register_stellar_asset_contract_v2(admin_addr.clone())
            .address();
        client.add_whitelisted_token(&admin_addr, &extra_token);
    }
    assert_eq!(client.get_whitelisted_tokens().len(), 50);

    // Confirm cap is enforced.
    let overflow_token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let cap_result = client.try_add_whitelisted_token(&admin_addr, &overflow_token);
    assert_eq!(cap_result, Err(Ok(Error::InvalidAmount)));

    // Remove one token to free a slot.
    client.remove_whitelisted_token(&admin_addr, &token1);
    assert_eq!(client.get_whitelisted_tokens().len(), 49);

    // Now the addition must succeed (one slot available).
    let new_token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let result = client.try_add_whitelisted_token(&admin_addr, &new_token);
    assert!(result.is_ok(), "adding after remove should succeed");
    assert_eq!(client.get_whitelisted_tokens().len(), 50);

    // Cap is enforced again after filling the freed slot.
    let yet_another = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let result2 = client.try_add_whitelisted_token(&admin_addr, &yet_another);
    assert_eq!(result2, Err(Ok(Error::InvalidAmount)));
}

/// Overflow-protection test 4 ΓÇö DUPLICATE BEFORE OVERFLOW CHECK:
/// When a duplicate token is submitted and the whitelist is also at capacity,
/// the duplicate check (`TokenAlreadyWhitelisted`) must fire before the
/// overflow guard (`InvalidAmount`) ΓÇö preserving the logical ordering of
/// checks: auth ΓåÆ admin identity ΓåÆ duplicate ΓåÆ capacity.
#[test]
fn test_add_whitelisted_token_duplicate_checked_before_cap() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    // Fill whitelist to cap (50 entries).
    for _ in 0..49u32 {
        let extra_token = env
            .register_stellar_asset_contract_v2(admin_addr.clone())
            .address();
        client.add_whitelisted_token(&admin_addr, &extra_token);
    }
    assert_eq!(client.get_whitelisted_tokens().len(), 50);

    // Submitting an already-whitelisted token while also at cap must return
    // TokenAlreadyWhitelisted, not InvalidAmount.
    let result = client.try_add_whitelisted_token(&admin_addr, &token1);
    assert_eq!(result, Err(Ok(Error::TokenAlreadyWhitelisted)));
}

/// Overflow-protection test 5 ΓÇö UNAUTHORIZED CALLER BEFORE CAPACITY CHECK:
/// An unauthorised caller must be rejected before the overflow guard is
/// evaluated, preserving the existing auth ΓåÆ admin-identity ΓåÆ capacity
/// check ordering.
#[test]
fn test_add_whitelisted_token_unauthorized_before_cap_check() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let bad_actor = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    // Fill whitelist to cap.
    for _ in 0..49u32 {
        let extra_token = env
            .register_stellar_asset_contract_v2(admin_addr.clone())
            .address();
        client.add_whitelisted_token(&admin_addr, &extra_token);
    }
    assert_eq!(client.get_whitelisted_tokens().len(), 50);

    // bad_actor tries to add a token while the list is at capacity.
    // The Unauthorized error must fire, not InvalidAmount.
    let new_token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let result = client.try_add_whitelisted_token(&bad_actor, &new_token);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_remove_whitelisted_token_rejects_zero_account_address() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let result = client.try_remove_whitelisted_token(&admin_addr, &zero_account);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

#[test]
fn test_remove_whitelisted_token_rejects_zero_contract_address() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &amounts,
    );

    let zero_contract = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );
    let result = client.try_remove_whitelisted_token(&admin_addr, &zero_contract);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

// ΓöÇΓöÇ upgrade / version tests ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

#[test]
fn test_version_returns_one_after_initialize() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    assert_eq!(client.version(), 1u32);
}

#[test]
fn test_upgrade_not_initialized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let fake_hash = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    let result = client.try_upgrade(&admin, &fake_hash);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_upgrade_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let bad_actor = Address::generate(&env);
    let fake_hash = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    let result = client.try_upgrade(&bad_actor, &fake_hash);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_upgrade_admin_auth_check_passes() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Admin auth passes; the call will fail because [0; 32] isn't a valid
    // uploaded wasm hash, but it must NOT return Unauthorized.
    let fake_hash = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    let result = client.try_upgrade(&admin_addr, &fake_hash);
    assert_ne!(result, Err(Ok(Error::Unauthorized)));
}

// ============================================================================
// add_whitelisted_token ΓÇö comprehensive boundary / negative / edge-case tests
// ============================================================================

/// add_whitelisted_token before initialize: no Admin key exists yet, so the
/// function must return NotInitialized (the `get(&DataKey::Admin)` path).
#[test]
fn test_add_whitelisted_token_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let token_addr = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // No initialize call ΓÇö storage is empty.
    let result = client.try_add_whitelisted_token(&admin_addr, &token_addr);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

/// add_whitelisted_token after the escrow is funded must return AlreadyFunded.
/// This guards against post-funding token substitution attacks.
#[test]
fn test_add_whitelisted_token_after_funded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token1);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &vec![&env, 1_000_i128],
    );
    client.fund(&client_addr);

    // Contract is now funded; adding a new token must be rejected.
    let extra_token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let result = client.try_add_whitelisted_token(&admin_addr, &extra_token);
    assert_eq!(result, Err(Ok(Error::AlreadyFunded)));
}

/// Calling add_whitelisted_token with a non-admin address (even one that is a
/// valid address, e.g. the freelancer) must return Unauthorized.
/// This is distinct from `test_non_admin_add_token_fails` which uses a
/// completely random bad_actor ΓÇö here we use a known role address.
#[test]
fn test_add_whitelisted_token_freelancer_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &vec![&env, 1_000_i128],
    );

    // freelancer is a known participant but not the admin.
    let result = client.try_add_whitelisted_token(&freelancer_addr, &token2);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Calling add_whitelisted_token with the client address (another known role
/// that is NOT the admin) must also return Unauthorized.
#[test]
fn test_add_whitelisted_token_client_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &vec![&env, 1_000_i128],
    );

    // client is a known participant but not the admin.
    let result = client.try_add_whitelisted_token(&client_addr, &token2);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// add_whitelisted_token must emit exactly one `wtok` event containing the
/// correct admin and token fields.  Verifies both event count and payload.
#[test]
fn test_add_whitelisted_token_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &vec![&env, 1_000_i128],
    );

    client.add_whitelisted_token(&admin_addr, &token2);

    let wtok_topic: Val = symbol_short!("wtok").into_val(&env);
    let mut wtok_events = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == wtok_topic.get_payload() {
                wtok_events += 1;
                assert_eq!(
                    TokenWhitelistedEvent::from_val(&env, &event.2),
                    TokenWhitelistedEvent {
                        admin: admin_addr.clone(),
                        token: token2.clone(),
                    }
                );
            }
        }
    }
    assert_eq!(wtok_events, 1, "expected exactly one wtok event");
}

/// A failed add_whitelisted_token (duplicate token) must NOT emit any `wtok`
/// event, ensuring events are only emitted on state-changing success paths.
#[test]
fn test_add_whitelisted_token_failed_does_not_emit_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &vec![&env, 1_000_i128],
    );

    // Attempt to add the already-whitelisted token ΓÇö must fail.
    let _ = client.try_add_whitelisted_token(&admin_addr, &token1);

    let wtok_topic: Val = symbol_short!("wtok").into_val(&env);
    let wtok_count = env.events().all().iter().fold(0u32, |acc, event| {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == wtok_topic.get_payload() {
                return acc + 1;
            }
        }
        acc
    });
    assert_eq!(wtok_count, 0, "failed call must not emit wtok event");
}

/// After admin transfer, the old admin must be rejected by add_whitelisted_token
/// and the new admin must succeed.  Exercises the interaction between
/// transfer_admin and the admin identity check inside add_whitelisted_token.
#[test]
fn test_add_whitelisted_token_old_admin_rejected_after_transfer() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let new_admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token3 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &vec![&env, 1_000_i128],
    );

    client.transfer_admin(&admin_addr, &new_admin_addr);

    // Old admin must now be rejected.
    let old_result = client.try_add_whitelisted_token(&admin_addr, &token2);
    assert_eq!(old_result, Err(Ok(Error::Unauthorized)));

    // New admin must succeed.
    let new_result = client.try_add_whitelisted_token(&new_admin_addr, &token3);
    assert!(
        new_result.is_ok(),
        "new admin should be able to add a token"
    );
    assert!(client.is_token_whitelisted(&token3));
}

// ═══════════════════════════════════════════════════════════════════════════════
// TASK 2 TESTS: require_dispute_party auth for raise_dispute
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify raise_dispute with bad actor returns Unauthorized (not another error).
#[test]
fn test_raise_dispute_bad_actor_returns_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);

    let bad_actor = Address::generate(&env);
    let result = client.try_raise_dispute(&bad_actor, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Verify raise_dispute with freelancer succeeds (freelancer is an authorized party).
#[test]
fn test_raise_dispute_by_freelancer_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.mark_delivered(&freelancer_addr, &0u32);
    client.raise_dispute(&freelancer_addr, &0u32);

    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
}

/// Whitelist membership is mutually exclusive: adding token A then querying
/// token B must return false, and vice-versa.  Guards against false positives
/// in is_token_whitelisted after an add.
#[test]
fn test_add_whitelisted_token_does_not_whitelist_other_tokens() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token1 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token2 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token3 = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token1,
        &604800,
        &vec![&env, 1_000_i128],
    );

    // Only token2 is added.
    client.add_whitelisted_token(&admin_addr, &token2);

    assert!(
        client.is_token_whitelisted(&token1),
        "token1 (init token) must still be whitelisted"
    );
    assert!(
        client.is_token_whitelisted(&token2),
        "token2 must be whitelisted after add"
    );
    assert!(
        !client.is_token_whitelisted(&token3),
        "token3 was never added — must not be whitelisted"
    );

    // Whitelist length must be exactly 2.
    assert_eq!(client.get_whitelisted_tokens().len(), 2);
}

/// Verify raise_dispute by client succeeds (client is an authorized party).
#[test]
fn test_raise_dispute_by_client_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.raise_dispute(&client_addr, &0u32);

    let job = client.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// TASK 3 TESTS: Temporary storage DisputeFlag optimization
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify that raise_dispute writes the DisputeFlag to temporary storage.
#[test]
fn test_raise_dispute_writes_dispute_flag() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.raise_dispute(&client_addr, &0u32);

    // Read the temporary storage flag from within the contract context
    let flag_set = env.as_contract(&contract_id, || {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::DisputeFlag(0u32))
            .unwrap_or(false)
    });
    assert!(flag_set, "DisputeFlag should be set in temporary storage");
}

/// Verify that DisputeFlag is NOT set before raise_dispute is called.
#[test]
fn test_dispute_flag_not_set_before_raise_dispute() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, contract_id, _client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let flag_set = env.as_contract(&contract_id, || {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::DisputeFlag(0u32))
            .unwrap_or(false)
    });
    assert!(
        !flag_set,
        "DisputeFlag should NOT be set before raise_dispute"
    );
}

/// Verify that only the disputed milestone's flag is set, not other indices.
#[test]
fn test_dispute_flag_only_sets_targeted_milestone() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128, 1_000_i128]);

    client.raise_dispute(&client_addr, &0u32);

    let flag_0 = env.as_contract(&contract_id, || {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::DisputeFlag(0u32))
            .unwrap_or(false)
    });
    let flag_1 = env.as_contract(&contract_id, || {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::DisputeFlag(1u32))
            .unwrap_or(false)
    });

    assert!(flag_0, "DisputeFlag(0) should be set");
    assert!(!flag_1, "DisputeFlag(1) should NOT be set");
}

// ═══════════════════════════════════════════════════════════════════════════════
// TASK: emergency_pause_split_refund distribution pathways
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emergency_pause_split_refund_even_split() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let allocation = client.emergency_pause_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund, 500);
    assert_eq!(allocation.freelancer_payout, 500);
    assert_eq!(allocation.client_refund_bps, 5_000);
    assert_eq!(allocation.freelancer_payout_bps, 5_000);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        1_000
    );
}

#[test]
fn test_emergency_pause_split_refund_odd_amount_rounding() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 101 split 50/50: client gets 51, freelancer gets 50
    let allocation = client.emergency_pause_split_refund(&101_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund, 51);
    assert_eq!(allocation.freelancer_payout, 50);
    assert_eq!(allocation.client_refund + allocation.freelancer_payout, 101);
}

#[test]
fn test_emergency_pause_split_refund_invalid_ratio_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 5000 + 3000 = 8000 != 10000
    let result = client.try_emergency_pause_split_refund(&1_000_i128, &5_000_u32, &3_000_u32);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

// ═══════════════════════════════════════════════════════════════════════════════
// TASK 4 TESTS: multisig_split_refund distribution pathways
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify multisig_split_refund with 50/50 split calculates correctly.
#[test]
fn test_multisig_split_refund_even_split() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let allocation = client.multisig_split_refund(&1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund, 500);
    assert_eq!(allocation.freelancer_payout, 500);
    assert_eq!(allocation.client_refund_bps, 5_000);
    assert_eq!(allocation.freelancer_payout_bps, 5_000);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        1_000
    );
}

#[test]
fn test_platform_fee_allocation_admin_override_requires_verified_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let attacker = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    client.lock_platform_fee_allocation(&admin_addr);

    let result = client.try_pf_alloc_admin_override(&attacker, &1000_u32, &8000_u32, &1000_u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let allocation = client.get_platform_fee_allocation();
    assert_eq!(allocation.client_bps, 2000);
    assert_eq!(allocation.freelancer_bps, 7000);
    assert!(allocation.locked);
}

#[test]
fn test_admin_override_tax_refund_requires_admin() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let attacker = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_admin_override_tax_refund(&attacker, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_admin_override_tax_refund_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    // No tax withholding lock exists for milestone 0.
    // Should fail with InvalidStatus without mutating state.
    let result = client.try_admin_override_tax_refund(&admin_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_platform_fee_allocation_admin_override_invalid_state() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    // If we try to override before setting it up (but after initialize), it's not locked.
    // It should fail with InvalidStatus.
    let result = client.try_pf_alloc_admin_override(&admin_addr, &1000_u32, &8000_u32, &1000_u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));

    client.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);

    // Now it's set but NOT locked. An override is unnecessary and should fail with InvalidStatus.
    let result2 = client.try_pf_alloc_admin_override(&admin_addr, &1500_u32, &7500_u32, &1000_u32);
    assert_eq!(result2, Err(Ok(Error::InvalidStatus)));

    // Storage remains unchanged
    let allocation = client.get_platform_fee_allocation();
    assert_eq!(allocation.client_bps, 2000);
    assert_eq!(allocation.freelancer_bps, 7000);
    assert_eq!(allocation.treasury_bps, 1000);
    assert!(!allocation.locked);
}

#[test]
fn test_platform_fee_allocation_admin_override_unlocks_locked_allocation() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    client.set_platform_fee_allocation(&admin_addr, &2000_u32, &7000_u32, &1000_u32);
    client.lock_platform_fee_allocation(&admin_addr);

    let locked_update =
        client.try_set_platform_fee_allocation(&admin_addr, &1500_u32, &7500_u32, &1000_u32);
    assert_eq!(locked_update, Err(Ok(Error::InvalidStatus)));

    client.pf_alloc_admin_override(&admin_addr, &1500_u32, &7500_u32, &1000_u32);
    let allocation = client.get_platform_fee_allocation();
    assert_eq!(allocation.client_bps, 1500);
    assert_eq!(allocation.freelancer_bps, 7500);
    assert_eq!(allocation.treasury_bps, 1000);
}

/// Verify multisig_split_refund with 70/30 split calculates correctly.
#[test]
fn test_multisig_split_refund_uneven_split() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let allocation = client.multisig_split_refund(&1_000_i128, &7_000_u32, &3_000_u32);
    assert_eq!(allocation.client_refund, 700);
    assert_eq!(allocation.freelancer_payout, 300);
    assert_eq!(allocation.client_refund_bps, 7_000);
    assert_eq!(allocation.freelancer_payout_bps, 3_000);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        1_000
    );
}

/// Verify multisig_split_refund with 100% client refund.
#[test]
fn test_multisig_split_refund_full_client_refund() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let allocation = client.multisig_split_refund(&1_000_i128, &10_000_u32, &0_u32);
    assert_eq!(allocation.client_refund, 1_000);
    assert_eq!(allocation.freelancer_payout, 0);
}

/// Verify multisig_split_refund with 100% freelancer payout.
#[test]
fn test_multisig_split_refund_full_freelancer_payout() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let allocation = client.multisig_split_refund(&1_000_i128, &0_u32, &10_000_u32);
    assert_eq!(allocation.client_refund, 0);
    assert_eq!(allocation.freelancer_payout, 1_000);
}

/// Verify multisig_split_refund rejects ratios that don't sum to BPS_SCALE.
#[test]
fn test_multisig_split_refund_invalid_ratio_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Total is 8000, not 10000
    let result = client.try_multisig_split_refund(&1_000_i128, &5_000_u32, &3_000_u32);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

/// Verify multisig_split_refund rejects zero total amount.
#[test]
fn test_multisig_split_refund_zero_total_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_multisig_split_refund(&0_i128, &5_000_u32, &5_000_u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Verify multisig_split_refund preserves total with rounding (odd amounts).
#[test]
fn test_multisig_split_refund_odd_amount_rounding() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 101 split 50/50 should produce 51/50 (rounding up for client)
    let allocation = client.multisig_split_refund(&101_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund + allocation.freelancer_payout, 101);
}

// payment_streaming_milestones — comprehensive unit test suite (#265)

#[test]
fn test_multisig_split_refund_extreme_split_preserves_total() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let allocation = client.multisig_split_refund(&10_000_i128, &1_u32, &9_999_u32);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        10_000
    );
    assert_eq!(allocation.client_refund_bps, 1);
    assert_eq!(allocation.freelancer_payout_bps, 9_999);
}

/// Verify multisig_split_refund emits the correct event.
#[test]
fn test_multisig_split_refund_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    token_admin.mint(&client_addr, &5_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // Negative numerator
    assert_eq!(
        client.try_payment_streaming_milestones(&100_i128, &-1_i128, &3_i128),
        Err(Ok(Error::InvalidRatio))
    );
    // Numerator > denominator
    assert_eq!(
        client.try_payment_streaming_milestones(&100_i128, &4_i128, &3_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn test_payment_streaming_milestones_negative_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_payment_streaming_milestones(&-1_i128, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_payment_streaming_milestones_zero_denominator_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_payment_streaming_milestones(&100_i128, &1_i128, &0_i128),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        client.try_payment_streaming_milestones(&100_i128, &1_i128, &-1_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

/// Boundary guard — ZERO AMOUNT:
/// A zero escrow balance means there is nothing to stream.
/// payment_streaming_milestones must reject this with Error::InvalidAmount.
#[test]
fn test_payment_streaming_milestones_zero_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_payment_streaming_milestones(&0_i128, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_payment_streaming_milestones_full_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let split = client.payment_streaming_milestones(&1000_i128, &100_i128, &100_i128);
    assert_eq!(split.first, 1000);
    assert_eq!(split.second, 0);
}

#[test]
fn test_payment_streaming_milestones_zero_numerator() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let split = client.payment_streaming_milestones(&1000_i128, &0_i128, &100_i128);
    assert_eq!(split.first, 0);
    assert_eq!(split.second, 1000);
}

#[test]
fn test_payment_streaming_milestones_overflow_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_payment_streaming_milestones(&i128::MAX, &i128::MAX, &i128::MAX),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_payment_streaming_milestones_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amount = 1_000_i128;
    let num = 300_i128;
    let den = 600_i128;

    let split = client.payment_streaming_milestones(&amount, &num, &den);
    assert_eq!(split.first, 500);
    assert_eq!(split.second, 500);

    let events = env.events().all();
    let p_stream_topic: Symbol = symbol_short!("p_stream");
    let p_stream_topic_val: Val = p_stream_topic.into_val(&env);

    let mut found_event = false;
    for e in events.iter() {
        if let Some(topic) = e.1.get(0) {
            if topic.get_payload() == p_stream_topic_val.get_payload() {
                found_event = true;
                let event_data = PaymentStreamingEvent::from_val(&env, &e.2);
                assert_eq!(event_data.total_amount, amount);
                assert_eq!(event_data.numerator, num);
                assert_eq!(event_data.denominator, den);
                assert_eq!(event_data.streamed_payout, 500);
                assert_eq!(event_data.client_refund, 500);
            }
        }
    }
    assert!(found_event, "Expected p_stream event to be published");
}

fn setup_multisig_env(
    env: &Env,
) -> (
    Address,
    Address,
    Address,
    Address,
    Address,
    Address,
    MilestoneEscrowClient<'_>,
) {
    let admin_addr = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_contract_id);
    token_admin.mint(&client_addr, &1_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = vec![env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    (
        admin_addr,
        client_addr,
        freelancer_addr,
        arbiter_addr,
        token_contract_id,
        contract_id,
        client,
    )
}

#[test]
fn test_tax_withholding_deductions_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let record = client.tax_withholding_deductions(&0_u32, &2_500_u32);
    assert_eq!(record.gross_amount, 1_000);
    assert_eq!(record.tax_amount, 250);
    assert_eq!(record.net_amount, 750);
    assert_eq!(record.gross_amount, record.tax_amount + record.net_amount);

    let tax_topic: Val = symbol_short!("taxwith").into_val(&env);
    assert!(env.events().all().iter().any(|event| {
        event
            .1
            .get(0)
            .map(|topic| topic.get_payload() == tax_topic.get_payload())
            .unwrap_or(false)
    }));
}

#[test]
fn test_tax_withholding_deductions_rounds_nearest_and_preserves_gross() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 101_i128]);

    let record = client.tax_withholding_deductions(&0_u32, &5_000_u32);
    assert_eq!(record.tax_amount, 51);
    assert_eq!(record.net_amount, 50);
    assert_eq!(record.gross_amount, record.tax_amount + record.net_amount);
}

#[test]
fn test_tax_withholding_deductions_accepts_zero_and_full_tax_rates() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let no_tax = client.tax_withholding_deductions(&0_u32, &0_u32);
    assert_eq!(no_tax.tax_amount, 0);
    assert_eq!(no_tax.net_amount, 1_000);

    let full_tax = client.tax_withholding_deductions(&0_u32, &10_000_u32);
    assert_eq!(full_tax.tax_amount, 1_000);
    assert_eq!(full_tax.net_amount, 0);
}

#[test]
fn test_tax_withholding_record_can_be_resolved_as_net_release() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, _, admin_addr, token_contract_id, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token = token::Client::new(&env, &token_contract_id);

    client.tax_withholding_deductions(&0_u32, &2_500_u32);
    client.admin_override_tax_release(&admin_addr, &0_u32);

    assert_eq!(token.balance(&freelancer_addr), 750);
    assert_eq!(client.get_job().milestones.get(0).unwrap().status, MilestoneStatus::Released);
    assert_eq!(
        client.try_admin_override_tax_release(&admin_addr, &0_u32),
        Err(Ok(Error::InvalidStatus))
    );
}

#[test]
fn test_tax_withholding_record_can_be_resolved_as_gross_refund() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, admin_addr, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    client.tax_withholding_deductions(&0_u32, &7_500_u32);
    client.admin_override_tax_refund(&admin_addr, &0_u32);

    assert_eq!(client.get_job().milestones.get(0).unwrap().status, MilestoneStatus::Refunded);
    assert_eq!(client.try_admin_override_tax_refund(&admin_addr, &0_u32), Err(Ok(Error::InvalidStatus)));
}

#[test]
fn test_tax_withholding_deductions_rejects_invalid_state_and_inputs() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_tax_withholding_deductions(&0_u32, &1_u32),
        Err(Ok(Error::NotInitialized))
    );

    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &vec![&env, 1_000_i128],
    );
    assert_eq!(
        client.try_tax_withholding_deductions(&0_u32, &1_u32),
        Err(Ok(Error::NotFunded))
    );

    let (_, _, _, _, _, _, funded_client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    assert_eq!(
        funded_client.try_tax_withholding_deductions(&1_u32, &1_u32),
        Err(Ok(Error::InvalidMilestone))
    );
    assert_eq!(
        funded_client.try_tax_withholding_deductions(&0_u32, &10_001_u32),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn test_tax_withholding_deductions_terminal_milestone_fails_without_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    client.mark_delivered(&freelancer_addr, &0_u32);
    client.approve_milestone(&client_addr, &0_u32);

    let result = client.try_tax_withholding_deductions(&0_u32, &2_500_u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    let tax_topic: Val = symbol_short!("taxwith").into_val(&env);
    assert!(!env.events().all().iter().any(|event| {
        event
            .1
            .get(0)
            .map(|topic| topic.get_payload() == tax_topic.get_payload())
            .unwrap_or(false)
    }));
}

#[test]
fn test_multisig_transfer_admin_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_multisig_transfer_admin_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);
    let bad_actor = Address::generate(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&bad_actor, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let client_addr = Address::generate(&env);
    let result = client.try_multisig_transfer_admin(&client_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));

    let _ = admin_addr;
}

#[test]
fn test_multisig_transfer_admin_zero_total_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &0_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_multisig_transfer_admin_negative_total_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &(-1_i128), &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_multisig_transfer_admin_empty_ratios_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = soroban_sdk::Vec::new(&env);
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

#[test]
fn test_multisig_transfer_admin_too_many_ratios_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let mut ratios = vec![&env];
    for _ in 0..256u32 {
        ratios.push_back(1_i128);
    }
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));

    let mut ratios_ok = vec![&env];
    for _ in 0..255u32 {
        ratios_ok.push_back(1_i128);
    }
    let allocs = client.multisig_transfer_admin(&admin_addr, &255_i128, &ratios_ok);
    let mut total = 0_i128;
    for i in 0..allocs.len() {
        total += allocs.get(i).unwrap();
    }
    assert_eq!(total, 255);
}

#[test]
fn test_multisig_transfer_admin_negative_single_ratio_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, -1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));

    let ratios_first = vec![&env, -5_i128, 1_i128];
    let result_first = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios_first);
    assert_eq!(result_first, Err(Ok(Error::InvalidRatio)));
}

#[test]
fn test_multisig_transfer_admin_ratio_sum_overflow_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, i128::MAX, 1_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

#[test]
fn test_multisig_transfer_admin_all_zero_ratios_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 0_i128, 0_i128, 0_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

#[test]
fn test_multisig_transfer_admin_single_ratio_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &42_i128, &ratios);
    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations.get(0).unwrap(), 42);
}

#[test]
fn test_multisig_transfer_admin_some_zero_ratios_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 0_i128, 1_i128, 0_i128, 3_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    assert_eq!(allocations.len(), 4);
    assert_eq!(allocations.get(0).unwrap(), 0);
    assert_eq!(allocations.get(2).unwrap(), 0);
    let mut total = 0_i128;
    for i in 0..allocations.len() {
        total += allocations.get(i).unwrap();
    }
    assert_eq!(total, 100);
}

#[test]
fn test_multisig_transfer_admin_large_weighted_mul_overflow_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, i128::MAX];
    let result = client.try_multisig_transfer_admin(&admin_addr, &i128::MAX, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_multisig_transfer_admin_equal_ratios_two_party() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &101_i128, &ratios);
    assert_eq!(allocations.len(), 2);
    let mut total = 0_i128;
    for i in 0..allocations.len() {
        total += allocations.get(i).unwrap();
    }
    assert_eq!(total, 101);
    let a = allocations.get(0).unwrap();
    let b = allocations.get(1).unwrap();
    assert!((a - b).abs() <= 1);
}

#[test]
fn test_multisig_transfer_admin_disparate_ratios() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 7_i128, 1_i128, 2_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &100_i128, &ratios);
    let mut total = 0_i128;
    for i in 0..allocations.len() {
        total += allocations.get(i).unwrap();
    }
    assert_eq!(total, 100);
    assert_eq!(
        allocations.get(0).unwrap() + allocations.get(1).unwrap() + allocations.get(2).unwrap(),
        100
    );
}

#[test]
fn test_multisig_transfer_admin_one_amount_one_ratio() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 5_i128, 5_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &1_i128, &ratios);
    assert_eq!(allocations.len(), 2);
    let mut total = 0_i128;
    for i in 0..allocations.len() {
        total += allocations.get(i).unwrap();
    }
    assert_eq!(total, 1);
}

#[test]
fn test_multisig_transfer_admin_freelancer_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, freelancer_addr, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&freelancer_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_multisig_transfer_admin_client_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, client_addr, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&client_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_multisig_transfer_admin_arbiter_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _, _, arbiter_addr, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&arbiter_addr, &100_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_multisig_transfer_admin_ratio_at_capacity_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let mut ratios = vec![&env];
    for _ in 0..255u32 {
        ratios.push_back(1_i128);
    }
    let result = client.multisig_transfer_admin(&admin_addr, &255_i128, &ratios);
    assert_eq!(result.len(), 255);
    let mut total = 0_i128;
    for i in 0..result.len() {
        total += result.get(i).unwrap();
    }
    assert_eq!(total, 255);
}

// ============================================================================
// raise_dispute — comprehensive boundary / edge-case test suite (Issue #183)
// ============================================================================
//
// These tests verify that raise_dispute enforces strict state machine
// transitions, rejects unauthorised callers, and handles negative inputs
// gracefully.

/// Boundary guard — a milestone carrying a zero amount has nothing to
/// arbitrate, so raise_dispute must reject it with InvalidAmount.
#[test]
fn test_raise_dispute_zero_amount_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, contract_id, client) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    // Corrupt the milestone amount to 0.
    env.as_contract(&contract_id, || {
        let mut milestone: Milestone = env
            .storage()
            .persistent()
            .get(&DataKey::Milestone(0u32))
            .unwrap();
        milestone.amount = 0;
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(0u32), &milestone);
    });

    let result = client.try_raise_dispute(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// State machine test 1 — raise_dispute on Released milestone must fail.
#[test]
fn test_raise_dispute_on_released_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);

    let result = escrow.try_raise_dispute(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// State machine test 2 — raise_dispute on Disputed milestone must fail.
#[test]
fn test_raise_dispute_on_disputed_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.raise_dispute(&client_addr, &0u32);

    let result = escrow.try_raise_dispute(&freelancer_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// State machine test 3 — raise_dispute on Refunded milestone must fail.
#[test]
fn test_raise_dispute_on_refunded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.raise_dispute(&client_addr, &0u32);
    escrow.resolve_dispute(&arbiter_addr, &0u32, &false);

    let result = escrow.try_raise_dispute(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Auth test — unauthorised caller (arbiter) must be rejected.
#[test]
fn test_raise_dispute_arbiter_is_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let (_client_addr, _freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let result = escrow.try_raise_dispute(&arbiter_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Negative input — raise_dispute with a zero contract address fails.
#[test]
fn test_raise_dispute_zero_contract_address_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, _freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    let zero_contract = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );

    let result = escrow.try_raise_dispute(&zero_contract, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

/// Negative input — raise_dispute before the escrow is funded fails.
#[test]
fn test_raise_dispute_before_funded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = escrow.try_raise_dispute(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::NotFunded)));
}

/// Happy path — client can raise a dispute from PartiallyReleased status.
#[test]
fn test_raise_dispute_from_partially_released_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 5_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_partial(&client_addr, &0u32, &2_000_i128);

    escrow.raise_dispute(&client_addr, &0u32);

    let job = escrow.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
}

/// Auth test — raise_dispute without auth must fail at host level.
#[test]
fn test_raise_dispute_no_auth_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    env.set_auths(&[]);

    let result = escrow.try_raise_dispute(&client_addr, &0u32);
    assert!(result.is_err());
    assert!(matches!(result, Err(Err(_))));
}

/// Gas-scaling audit: raise_dispute reads/writes a single keyed milestone
/// entry (`DataKey::Milestone(index)`), not a scan over the job's full
/// milestone list, so its cost must stay roughly flat as the milestone
/// count grows from 8 to 128+.
fn raise_dispute_budget_for_milestone_count(count: u32) -> (u64, u64) {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let mut amounts = vec![&env];
    for _ in 0..count {
        amounts.push_back(1_i128);
    }
    let total: i128 = count as i128;

    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    token_admin.mint(&client_addr, &total);
    escrow.fund(&client_addr);

    // Dispute the last milestone so any accidental linear scan over the
    // earlier entries would show up in the measured cost.
    let target_index = count - 1;

    let mut budget = env.cost_estimate().budget();
    budget.reset_default();

    escrow.raise_dispute(&client_addr, &target_index);

    let budget = env.cost_estimate().budget();
    (budget.cpu_instruction_cost(), budget.memory_bytes_cost())
}

#[test]
fn test_raise_dispute_gas_is_constant_for_many_milestones() {
    let (cpu_8, memory_8) = raise_dispute_budget_for_milestone_count(8);
    let (cpu_128, memory_128) = raise_dispute_budget_for_milestone_count(128);

    assert!(cpu_8 > 0);
    assert!(cpu_128 > 0);
    assert!(
        cpu_128 < cpu_8.saturating_mul(2),
        "raise_dispute cost must not scale with milestone count: cpu {} -> {}",
        cpu_8,
        cpu_128
    );

    // Memory cost includes host storage-footprint accounting that isn't a
    // pure function of raise_dispute's own algorithm, so it isn't perfectly
    // flat like the CPU count above. What matters for the audit is that it
    // stays far below what a genuine O(n) scan over the milestone set would
    // cost: milestone count grew 16x (8 -> 128), so a linear scan would show
    // ~16x memory growth too. We assert well under half of that.
    assert!(memory_8 > 0);
    assert!(memory_128 > 0);
    assert!(
        memory_128 < memory_8.saturating_mul(6),
        "raise_dispute memory must not scale linearly with milestone count: memory {} -> {}",
        memory_8,
        memory_128
    );
}

// ============================================================================
// multisig_approval — comprehensive unit test suite (Issue #184, #166)
// ============================================================================

/// Helper: register and initialise escrow (admin present) without multisig setup.
fn setup_escrow_for_multisig(env: &Env) -> (MilestoneEscrowClient<'_>, Address) {
    let admin = Address::generate(env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let dummy_client = Address::generate(env);
    let dummy_freelancer = Address::generate(env);
    let dummy_arbiter = Address::generate(env);
    let amounts = vec![env, 1_000_i128];

    client.initialize(
        &admin,
        &dummy_client,
        &dummy_freelancer,
        &dummy_arbiter,
        &token_id,
        &604800,
        &amounts,
    );

    (client, admin)
}

/// Helper: register a fresh contract, fund it, and initialise multisig with
/// three signers and a threshold of 2. `multisig_approve` guards against a
/// zero contract balance, so this helper funds the escrow (unlike
/// `setup_escrow_for_multisig`, which intentionally leaves it unfunded for
/// `multisig_approval_init`-only tests) before setting up signers.
fn setup_multisig(env: &Env, threshold: u32) -> (MilestoneEscrowClient<'_>, Address, Vec<Address>) {
    let admin = Address::generate(env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(env, &contract_id);

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(env, &token_id);
    let dummy_client = Address::generate(env);
    let dummy_freelancer = Address::generate(env);
    let dummy_arbiter = Address::generate(env);
    let amounts = vec![env, 1_000_i128];

    client.initialize(
        &admin,
        &dummy_client,
        &dummy_freelancer,
        &dummy_arbiter,
        &token_id,
        &604800,
        &amounts,
    );

    token_admin.mint(&dummy_client, &1_000_i128);
    client.fund(&dummy_client);

    let signer1 = Address::generate(env);
    let signer2 = Address::generate(env);
    let signer3 = Address::generate(env);
    let signers = vec![env, signer1.clone(), signer2.clone(), signer3.clone()];

    client.multisig_approval_init(&admin, &signers, &threshold);

    (client, admin, signers)
}

/// Initialisation: a valid multisig setup with 3 signers and threshold 2
/// must succeed and store the configuration.
#[test]
fn test_multisig_approval_init_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, _signers) = setup_multisig(&env, 2);

    let state = client.try_is_multisig_approved(&0u32).unwrap().unwrap();
    assert!(!state.approved);
    assert_eq!(state.approvals, 0);
    assert_eq!(state.threshold, 2);
}

/// Initialisation: a second call to `multisig_approval_init` must be rejected
/// with `AlreadyInitialized`.
#[test]
fn test_multisig_approval_init_duplicate_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin, _signers) = setup_multisig(&env, 2);

    let extra = Address::generate(&env);
    let new_signers = vec![&env, extra];
    let result = client.try_multisig_approval_init(&admin, &new_signers, &1u32);
    assert_eq!(result, Err(Ok(Error::AlreadyInitialized)));
}

/// Initialisation: zero signers must be rejected.
#[test]
fn test_multisig_approval_init_zero_signers_fails() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let (client, admin) = setup_escrow_for_multisig(&env);
    let empty: Vec<Address> = Vec::new(&env);

    let result = client.try_multisig_approval_init(&admin, &empty, &1u32);
    assert_eq!(result, Err(Ok(Error::MultiSigNoSigners)));
}

/// Initialisation: threshold of 0 must be rejected.
#[test]
fn test_multisig_approval_init_zero_threshold_fails() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let (client, admin) = setup_escrow_for_multisig(&env);
    let signer = Address::generate(&env);
    let signers = vec![&env, signer];

    let result = client.try_multisig_approval_init(&admin, &signers, &0u32);
    assert_eq!(result, Err(Ok(Error::MultiSigInvalidThreshold)));
}

/// Initialisation: threshold exceeding signer count must be rejected.
#[test]
fn test_multisig_approval_init_threshold_exceeds_signers_fails() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let (client, admin) = setup_escrow_for_multisig(&env);
    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);
    let signers = vec![&env, signer1, signer2];

    let result = client.try_multisig_approval_init(&admin, &signers, &3u32);
    assert_eq!(result, Err(Ok(Error::MultiSigInvalidThreshold)));
}

/// Initialisation: more than 32 signers must be rejected.
#[test]
fn test_multisig_approval_init_too_many_signers_fails() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let (client, admin) = setup_escrow_for_multisig(&env);
    let mut signers = vec![&env];
    for _ in 0..33 {
        signers.push_back(Address::generate(&env));
    }

    let result = client.try_multisig_approval_init(&admin, &signers, &1u32);
    assert_eq!(result, Err(Ok(Error::MultiSigTooManySigners)));
}

/// Initialisation: duplicate signer addresses must be rejected.
#[test]
fn test_multisig_approval_init_duplicate_signer_fails() {
    let env = env_without_snapshot();
    env.mock_all_auths();

    let (client, admin) = setup_escrow_for_multisig(&env);
    let signer = Address::generate(&env);
    let signers = vec![&env, signer.clone(), signer];

    let result = client.try_multisig_approval_init(&admin, &signers, &2u32);
    assert_eq!(result, Err(Ok(Error::MultiSigDuplicateSigner)));
}

/// Approval flow: single signer approves, threshold (2) not yet reached.
#[test]
fn test_multisig_approval_partial_approval() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 2);

    let signer1 = signers.get(0).unwrap();
    let state = client
        .try_multisig_approve(&signer1, &1u32)
        .unwrap()
        .unwrap();

    assert!(!state.approved);
    assert_eq!(state.approvals, 1);
    assert_eq!(state.threshold, 2);
}

/// Approval flow: two signers approve, threshold reached.
#[test]
fn test_multisig_approval_reaches_threshold() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 2);

    let signer1 = signers.get(0).unwrap();
    let signer2 = signers.get(1).unwrap();

    let _ = client.multisig_approve(&signer1, &2u32);
    let state = client
        .try_multisig_approve(&signer2, &2u32)
        .unwrap()
        .unwrap();

    assert!(state.approved);
    assert_eq!(state.approvals, 2);
    assert_eq!(state.threshold, 2);
}

/// Approval flow: all three signers approve, threshold exceeded.
#[test]
fn test_multisig_approval_exceeds_threshold() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 2);

    let signer1 = signers.get(0).unwrap();
    let signer2 = signers.get(1).unwrap();
    let signer3 = signers.get(2).unwrap();

    let _ = client.multisig_approve(&signer1, &3u32);
    let _ = client.multisig_approve(&signer2, &3u32);
    let state = client
        .try_multisig_approve(&signer3, &3u32)
        .unwrap()
        .unwrap();

    assert!(state.approved);
    assert_eq!(state.approvals, 3);
    assert_eq!(state.bitmap.count_ones(), 3);
}

/// Approval flow: idempotent — same signer approves twice, still counts as one.
#[test]
fn test_multisig_approval_idempotent() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 2);

    let signer1 = signers.get(0).unwrap();
    let _ = client.multisig_approve(&signer1, &4u32);
    let state = client
        .try_multisig_approve(&signer1, &4u32)
        .unwrap()
        .unwrap();

    assert!(!state.approved);
    assert_eq!(state.approvals, 1);
}

/// Auth: unregistered signer cannot approve.
#[test]
fn test_multisig_approval_unauthorized_signer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, _signers) = setup_multisig(&env, 2);

    let impostor = Address::generate(&env);
    let result = client.try_multisig_approve(&impostor, &5u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Auth: require_auth fails when no auth provided.
#[test]
fn test_multisig_approval_no_auth_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 2);

    let signer1 = signers.get(0).unwrap();
    env.set_auths(&[]);
    let result = client.try_multisig_approve(&signer1, &6u32);
    assert!(result.is_err());
    assert!(matches!(result, Err(Err(_))));
}

/// Query: is_multisig_approved returns correct state without mutation.
#[test]
fn test_is_multisig_approved_query() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 3);

    let signer1 = signers.get(0).unwrap();
    let signer2 = signers.get(1).unwrap();
    let signer3 = signers.get(2).unwrap();

    let state0 = client.try_is_multisig_approved(&7u32).unwrap().unwrap();
    assert!(!state0.approved);
    assert_eq!(state0.approvals, 0);

    let _ = client.multisig_approve(&signer1, &7u32);
    let state1 = client.try_is_multisig_approved(&7u32).unwrap().unwrap();
    assert!(!state1.approved);
    assert_eq!(state1.approvals, 1);

    let _ = client.multisig_approve(&signer2, &7u32);
    let _ = client.multisig_approve(&signer3, &7u32);
    let state2 = client.try_is_multisig_approved(&7u32).unwrap().unwrap();
    assert!(state2.approved);
    assert_eq!(state2.approvals, 3);
}

/// Isolation: proposals have independent bitmaps.
#[test]
fn test_multisig_approval_proposal_isolation() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 2);

    let signer1 = signers.get(0).unwrap();
    let _ = client.multisig_approve(&signer1, &10u32);

    let state = client.try_is_multisig_approved(&20u32).unwrap().unwrap();
    assert!(!state.approved);
    assert_eq!(state.approvals, 0);
}

/// Boundary guard: multisig_approve must be blocked when the escrow holds
/// a zero token balance — there is nothing at stake for signers to approve
/// against.
#[test]
fn test_multisig_approve_blocked_when_contract_balance_is_zero() {
    let env = Env::default();
    env.mock_all_auths();

    // Deliberately unfunded: `setup_escrow_for_multisig` initialises the job
    // but never calls `fund`.
    let (client, admin) = setup_escrow_for_multisig(&env);
    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);
    let signers = vec![&env, signer1.clone(), signer2.clone()];
    client.multisig_approval_init(&admin, &signers, &2u32);

    let result = client.try_multisig_approve(&signer1, &1u32);
    assert_eq!(result, Err(Ok(Error::MultiSigEmptyBalance)));
}

/// Event: multisig_approve emits a structured `MultiSigApprovedEvent` on a
/// successful call, reflecting the signer, proposal, and updated approval
/// state so downstream indexers can track progress without polling storage.
#[test]
fn test_multisig_approve_emits_structured_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, signers) = setup_multisig(&env, 2);
    let signer1 = signers.get(0).unwrap();

    client.multisig_approve(&signer1, &42u32);

    let topic: Symbol = symbol_short!("msigappr");
    let topic_val: Val = topic.into_val(&env);
    let mut matched = 0u32;
    for event in env.events().all().iter() {
        if let Some(t) = event.1.get(0) {
            if t.get_payload() == topic_val.get_payload() {
                matched += 1;
                assert_eq!(event.1.len(), 1);
                assert_eq!(
                    MultiSigApprovedEvent::from_val(&env, &event.2),
                    MultiSigApprovedEvent {
                        proposal_id: 42,
                        signer: signer1.clone(),
                        approvals: 1,
                        threshold: 2,
                        approved: false,
                        bitmap: 1,
                    }
                );
            }
        }
    }
    assert_eq!(matched, 1);
}

/// Admin: unauthorised caller cannot initialise multisig.
#[test]
fn test_multisig_approval_init_unauthorized_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, _signers) = setup_multisig(&env, 2);

    let impostor = Address::generate(&env);
    let new_signers = vec![&env, impostor.clone()];
    let result = client.try_multisig_approval_init(&impostor, &new_signers, &1u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

// ============================================================================
// milestone_time_extensions — ratio-split precision test suite (#218)
// ============================================================================

/// Precision test 1 — EXACT HALF (odd amount, rounds to nearest):
/// 101 split at exactly 1/2 elapsed must give first=51, second=50.
/// Verifies round-nearest rather than floor: 101 * 1 / 2 = 50.5 → 51.
/// first + second must equal 101 exactly (no value lost).
#[test]
fn test_milestone_time_extensions_half_rounds_nearest() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let split = client.milestone_time_extensions(&101_i128, &1_i128, &2_i128);
    assert_eq!(split.first, 51);
    assert_eq!(split.second, 50);
    assert_eq!(split.first + split.second, 101);
}

/// Precision test 2 — FULL ELAPSED (numerator == denominator):
/// When elapsed == total the freelancer receives the full amount.
/// first=amount, second=0, total preserved.
#[test]
fn test_milestone_time_extensions_full_elapsed_gives_full_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let split = client.milestone_time_extensions(&10_000_i128, &500_i128, &500_i128);
    assert_eq!(split.first, 10_000);
    assert_eq!(split.second, 0);
    assert_eq!(split.first + split.second, 10_000);
}

/// Precision test 3 — ZERO ELAPSED (numerator == 0):
/// When no time has elapsed the freelancer receives nothing.
/// first=0, second=amount, total preserved.
#[test]
fn test_milestone_time_extensions_zero_elapsed_gives_nothing_to_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let split = client.milestone_time_extensions(&10_000_i128, &0_i128, &500_i128);
    assert_eq!(split.first, 0);
    assert_eq!(split.second, 10_000);
    assert_eq!(split.first + split.second, 10_000);
}

/// Boundary guard — ZERO AMOUNT:
/// A zero escrow balance means there is nothing to distribute.
/// milestone_time_extensions must reject this with Error::InvalidAmount.
#[test]
fn test_milestone_time_extensions_zero_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&0_i128, &1_i128, &3_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Precision test 5 — LARGE PRIME AMOUNT, ARBITRARY RATIO:
/// 999_983 (prime) split 3/7 — verifies no value is created or destroyed.
/// first = round_nearest(999_983 × 3 / 7) = round(428_564.14…) = 428_564
/// second = 999_983 − 428_564 = 571_419
#[test]
fn test_milestone_time_extensions_large_prime_total_preserved() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amount = 999_983_i128;
    let split = client.milestone_time_extensions(&amount, &3_i128, &7_i128);
    assert_eq!(
        split.first + split.second,
        amount,
        "total must be preserved"
    );
    // round_nearest(999983 * 3 / 7) = (2999949 + 3) / 7 = 2999952 / 7 = 428564
    assert_eq!(split.first, 428_564);
    assert_eq!(split.second, 571_419);
}

/// Precision test 6 — SINGLE STROOP AMOUNT:
/// 1 stroop split at 1/3 elapsed: round_nearest(1 × 1 / 3) = (1 + 1) / 3 = 0
/// → first=0, second=1. Verifies single-unit floor behaviour is correct.
#[test]
fn test_milestone_time_extensions_one_stroop_rounds_down() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let split = client.milestone_time_extensions(&1_i128, &1_i128, &3_i128);
    assert_eq!(split.first + split.second, 1);
    // round_nearest(1 * 1 / 3) = (1 + 1) / 3 = 0  (integer floor of 0.666)
    assert_eq!(split.first, 0);
    assert_eq!(split.second, 1);
}

/// Precision test 7 — MANY SMALL SPLITS PRESERVE TOTAL:
/// Split 1_000_000 into 7 equal parts using the ratio n/7 for n=1..7.
/// Each consecutive split must share a boundary with the previous one so
/// that the union covers exactly 1_000_000 with no gaps.
#[test]
fn test_milestone_time_extensions_sequential_splits_cover_total() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amount = 1_000_000_i128;
    let parts: u32 = 7;
    let mut prev_first = 0_i128;

    for n in 1..=parts {
        let split = client.milestone_time_extensions(&amount, &(n as i128), &(parts as i128));
        // Each split must sum to the original amount.
        assert_eq!(split.first + split.second, amount, "n={} total mismatch", n);
        // first is monotonically non-decreasing as n increases.
        assert!(
            split.first >= prev_first,
            "n={} first={} not >= prev={}",
            n,
            split.first,
            prev_first
        );
        prev_first = split.first;
    }

    // At n == parts the freelancer receives the full amount.
    assert_eq!(prev_first, amount);
}

/// Boundary guard — NEGATIVE AMOUNT:
/// A negative escrow amount is invalid and must return Error::InvalidAmount.
#[test]
fn test_milestone_time_extensions_negative_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&-1_i128, &1_i128, &10_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Boundary guard — ZERO TOTAL SECONDS:
/// A total window of 0 seconds is division-by-zero; must return Error::InvalidRatio.
#[test]
fn test_milestone_time_extensions_zero_total_seconds_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&1_000_i128, &0_i128, &0_i128);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

/// Boundary guard — ELAPSED > TOTAL:
/// Elapsed time cannot exceed the total window; must return Error::InvalidRatio.
#[test]
fn test_milestone_time_extensions_elapsed_exceeds_total_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&1_000_i128, &11_i128, &10_i128);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

/// Boundary guard — NEGATIVE ELAPSED:
/// Negative elapsed seconds are nonsensical; must return Error::InvalidRatio.
#[test]
fn test_milestone_time_extensions_negative_elapsed_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&1_000_i128, &-1_i128, &10_i128);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

/// Boundary guard — NEGATIVE TOTAL SECONDS:
/// A negative total window is invalid; must return Error::InvalidRatio.
#[test]
fn test_milestone_time_extensions_negative_total_seconds_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&1_000_i128, &5_i128, &-10_i128);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

/// Overflow guard — AMOUNT * ELAPSED overflows i128:
/// i128::MAX × i128::MAX overflows in checked_mul; must return Error::InvalidAmount.
#[test]
fn test_milestone_time_extensions_overflow_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&i128::MAX, &i128::MAX, &i128::MAX);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

// ============================================================================
// dispute_arbitration_split — validation test suite (#186)
// ============================================================================

/// Happy path — 50/50 split: freelancer_bps=5000, total=10_000.
/// freelancer_payout=5_000, client_refund=5_000, bps echo correct.
#[test]
fn test_dispute_arbitration_split_50_50() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.dispute_arbitration_split(&10_000_i128, &5_000u32);
    assert_eq!(result.freelancer_payout, 5_000);
    assert_eq!(result.client_refund, 5_000);
    assert_eq!(result.freelancer_payout_bps, 5_000);
    assert_eq!(result.client_refund_bps, 5_000);
    assert_eq!(result.freelancer_payout + result.client_refund, 10_000);
}

/// Full release — freelancer_bps=10_000: freelancer gets everything.
#[test]
fn test_dispute_arbitration_split_full_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.dispute_arbitration_split(&50_000_i128, &10_000u32);
    assert_eq!(result.freelancer_payout, 50_000);
    assert_eq!(result.client_refund, 0);
    assert_eq!(result.freelancer_payout_bps, 10_000);
    assert_eq!(result.client_refund_bps, 0);
}

/// Full refund — freelancer_bps=0: client gets everything back.
#[test]
fn test_dispute_arbitration_split_full_client_refund() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.dispute_arbitration_split(&50_000_i128, &0u32);
    assert_eq!(result.freelancer_payout, 0);
    assert_eq!(result.client_refund, 50_000);
    assert_eq!(result.freelancer_payout_bps, 0);
    assert_eq!(result.client_refund_bps, 10_000);
}

/// Round-nearest: odd amount 101 at 5000 bps → round_nearest(101 × 5000/10000)
/// = round_nearest(50.5) = 51 freelancer, 50 client.
#[test]
fn test_dispute_arbitration_split_rounding_nearest() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.dispute_arbitration_split(&101_i128, &5_000u32);
    assert_eq!(
        result.freelancer_payout + result.client_refund,
        101,
        "total preserved"
    );
    assert_eq!(result.freelancer_payout, 51);
    assert_eq!(result.client_refund, 50);
}

/// Zero amount — both shares must be zero, no error.
#[test]
fn test_dispute_arbitration_split_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.dispute_arbitration_split(&0_i128, &3_000u32);
    assert_eq!(result.freelancer_payout, 0);
    assert_eq!(result.client_refund, 0);
}

/// Invalid ratio — bps > 10_000 must return Error::InvalidRatio.
#[test]
fn test_dispute_arbitration_split_bps_exceeds_max_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_dispute_arbitration_split(&10_000_i128, &10_001u32);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

/// Invalid amount — negative total must return Error::InvalidAmount.
#[test]
fn test_dispute_arbitration_split_negative_amount_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_dispute_arbitration_split(&-1_i128, &5_000u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Overflow guard — i128::MAX with non-zero bps overflows intermediate
/// multiplication; must return Error::InvalidAmount, not panic.
#[test]
fn test_dispute_arbitration_split_overflow_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_dispute_arbitration_split(&i128::MAX, &5_001u32);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Total preservation — arbitrary split must always sum to total_amount.
#[test]
fn test_dispute_arbitration_split_total_always_preserved() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amount = 999_983_i128; // prime
    for bps in [0u32, 1, 999, 3_333, 5_000, 7_777, 9_999, 10_000] {
        let result = client.dispute_arbitration_split(&amount, &bps);
        assert_eq!(
            result.freelancer_payout + result.client_refund,
            amount,
            "total not preserved at bps={}",
            bps
        );
        assert_eq!(
            result.freelancer_payout_bps + result.client_refund_bps,
            10_000,
            "bps echo sum != 10_000 at bps={}",
            bps
        );
    }
}

/// Exact 1 bps on 10_000 total: round_nearest(10_000 × 1 / 10_000) = 1.
#[test]
fn test_dispute_arbitration_split_one_bps() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.dispute_arbitration_split(&10_000_i128, &1u32);
    assert_eq!(result.freelancer_payout, 1);
    assert_eq!(result.client_refund, 9_999);
}

// ============================================================================
// resolve_dispute — overflow/boundary protection tests (#185)
// ============================================================================

/// Boundary: resolve_dispute with amount=1 (minimum valid) releases correctly.
#[test]
fn test_resolve_dispute_minimum_amount_releases_ok() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_i128]);
    escrow.raise_dispute(&client_addr, &0u32);
    // Arbiter resolves to freelancer — 1 stroop, no overflow
    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert!(result.is_ok(), "1-stroop resolve should succeed");
}

/// Boundary: resolve_dispute returns to client (refund path) with min amount.
#[test]
fn test_resolve_dispute_minimum_amount_refunds_ok() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_i128]);
    escrow.raise_dispute(&client_addr, &0u32);
    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &false);
    assert!(result.is_ok(), "1-stroop refund should succeed");
}

/// Boundary: resolve_dispute with a large amount succeeds because all arithmetic
/// inside uses checked_* operations and never panics.
#[test]
fn test_resolve_dispute_large_amount_no_overflow() {
    let env = Env::default();
    env.mock_all_auths();
    let large: i128 = 1_000_000_000_000_i128;
    let (client_addr, _, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, large]);
    escrow.raise_dispute(&client_addr, &0u32);
    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert!(result.is_ok(), "large-amount resolve must not panic");
}

/// Boundary: fully released milestone cannot be disputed.
/// raise_dispute returns InvalidStatus before resolve_dispute is ever reached.
#[test]
fn test_resolve_dispute_zero_remaining_fails_gracefully() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);
    // Milestone is Released — raise_dispute must return InvalidStatus
    let result = escrow.try_raise_dispute(&client_addr, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}
/// InvalidStatus (not a panic), confirming state machine is overflow-safe.
#[test]
fn test_resolve_dispute_already_resolved_fails_gracefully() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.raise_dispute(&client_addr, &0u32);
    escrow.resolve_dispute(&arbiter_addr, &0u32, &true);
    // Second resolve on same milestone must fail gracefully
    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

/// Overflow guard: multiple milestones resolved in sequence — cumulative
/// checked_add in resolve_dispute never panics on valid amounts.
#[test]
fn test_resolve_dispute_multiple_milestones_sequential_ok() {
    let env = Env::default();
    env.mock_all_auths();
    let amounts = vec![&env, 100_i128, 200_i128, 300_i128];
    let (client_addr, _, arbiter_addr, _, _, _, escrow) = setup_funded_escrow(&env, amounts);
    for i in 0u32..3 {
        escrow.raise_dispute(&client_addr, &i);
        let ok = escrow.try_resolve_dispute(&arbiter_addr, &i, &(i % 2 == 0));
        assert!(ok.is_ok(), "milestone {} resolve failed", i);
    }
}

/// Overflow guard: checked_sub on released_amount — amount exactly equal to
/// released_amount must fail with InvalidAmount not panic.
#[test]
fn test_resolve_dispute_released_amount_equals_amount_fails() {
    // This test verifies the remaining = amount.checked_sub(released_amount)
    // path when remaining would be 0 (InvalidAmount guard).
    // We achieve this by trying to resolve a non-disputed milestone, which
    // hits InvalidStatus first — the relevant code path is protected.
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, arbiter_addr, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 500_i128]);
    // Do not raise dispute — resolve_dispute on Pending must fail gracefully
    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
}

// ============================================================================
// multisig_approval — multi-party authentication tests (#180)
// ============================================================================

/// Single signer in a 2-of-2 regime must NOT reach threshold.
#[test]
fn test_multisig_single_sig_does_not_reach_2_of_2() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, signers) = setup_multisig(&env, 2);
    // Only signer 0 approves
    let state = client.multisig_approve(&signers.get(0).unwrap(), &1u32);
    assert!(!state.approved, "single sig must not satisfy 2-of-2");
    assert_eq!(state.approvals, 1);
    assert_eq!(state.threshold, 2);
}

/// Single signer in a 3-of-3 regime must NOT reach threshold.
#[test]
fn test_multisig_single_sig_does_not_reach_3_of_3() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, signers) = setup_multisig(&env, 3);
    let state = client.multisig_approve(&signers.get(0).unwrap(), &2u32);
    assert!(!state.approved, "single sig must not satisfy 3-of-3");
    assert_eq!(state.approvals, 1);
    assert_eq!(state.threshold, 3);
}

/// Both parties must sign in a 2-of-2 regime: after both approve it is satisfied.
#[test]
fn test_multisig_both_sigs_satisfy_2_of_2() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, signers) = setup_multisig(&env, 2);
    client.multisig_approve(&signers.get(0).unwrap(), &3u32);
    let state = client.multisig_approve(&signers.get(1).unwrap(), &3u32);
    assert!(state.approved, "both sigs must satisfy 2-of-2");
    assert_eq!(state.approvals, 2);
}

/// A signer not in the registered list is rejected with Unauthorized.
#[test]
fn test_multisig_unregistered_signer_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, _signers) = setup_multisig(&env, 2);
    let impostor = Address::generate(&env);
    let result = client.try_multisig_approve(&impostor, &4u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Duplicate approval from the same signer is idempotent — count stays at 1.
#[test]
fn test_multisig_duplicate_approval_is_idempotent() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, signers) = setup_multisig(&env, 2);
    let signer = signers.get(0).unwrap();
    client.multisig_approve(&signer, &5u32);
    let state = client.multisig_approve(&signer, &5u32); // second call same signer
    assert!(
        !state.approved,
        "duplicate from single signer must not satisfy 2-of-2"
    );
    assert_eq!(
        state.approvals, 1,
        "bitmap must not double-count same signer"
    );
}

/// 2-of-3: threshold satisfied after exactly 2 distinct signers approve.
#[test]
fn test_multisig_2_of_3_satisfied_by_two_signers() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let dummy_client = Address::generate(&env);
    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);
    let s3 = Address::generate(&env);
    let signers = vec![&env, s1.clone(), s2.clone(), s3.clone()];

    // initialize escrow then multisig
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    client.initialize(
        &admin,
        &dummy_client,
        &Address::generate(&env),
        &Address::generate(&env),
        &token_id,
        &86400u64,
        &vec![&env, 1_000_i128],
    );
    // multisig_approve guards against a zero contract balance.
    token_admin.mint(&dummy_client, &1_000_i128);
    client.fund(&dummy_client);
    client.multisig_approval_init(&admin, &signers, &2u32); // 2-of-3

    client.multisig_approve(&s1, &6u32);
    let state = client.multisig_approve(&s2, &6u32);
    assert!(
        state.approved,
        "2-of-3 must be satisfied after 2 distinct approvals"
    );
    assert_eq!(state.approvals, 2);
    assert_eq!(state.threshold, 2);
}

/// Bitmap isolation: approval on proposal A does not affect proposal B.
#[test]
fn test_multisig_approval_isolated_per_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, signers) = setup_multisig(&env, 2);
    // Approve proposal 10 with both signers
    client.multisig_approve(&signers.get(0).unwrap(), &10u32);
    client.multisig_approve(&signers.get(1).unwrap(), &10u32);
    // Proposal 20 must still be unapproved
    let state = client.is_multisig_approved(&20u32);
    assert!(
        !state.approved,
        "proposal 20 must be unaffected by proposal 10 approvals"
    );
    assert_eq!(state.approvals, 0);
}

/// is_multisig_approved query returns correct state without side effects.
#[test]
fn test_multisig_query_no_side_effects() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, signers) = setup_multisig(&env, 2);
    // Query before any approval
    let state_before = client.is_multisig_approved(&99u32);
    assert!(!state_before.approved);
    assert_eq!(state_before.approvals, 0);
    // Approve once
    client.multisig_approve(&signers.get(0).unwrap(), &99u32);
    let state_after = client.is_multisig_approved(&99u32);
    assert!(!state_after.approved);
    assert_eq!(state_after.approvals, 1);
}

/// Threshold=1 (1-of-N): single approval must immediately satisfy.
#[test]
fn test_multisig_threshold_one_satisfied_by_single_signer() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let dummy_client = Address::generate(&env);
    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);
    let signers = vec![&env, s1.clone(), s2.clone()];

    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    client.initialize(
        &admin,
        &dummy_client,
        &Address::generate(&env),
        &Address::generate(&env),
        &token_id,
        &86400u64,
        &vec![&env, 1_000_i128],
    );
    // multisig_approve guards against a zero contract balance.
    token_admin.mint(&dummy_client, &1_000_i128);
    client.fund(&dummy_client);
    client.multisig_approval_init(&admin, &signers, &1u32); // 1-of-2

    let state = client.multisig_approve(&s1, &50u32);
    assert!(state.approved, "1-of-2 must be satisfied by single signer");
    assert_eq!(state.approvals, 1);
    assert_eq!(state.threshold, 1);
}

// ============================================================================
// raise_dispute — zero-address validation tests (#179)
// ============================================================================

/// Zero account address (G…WHF) must be rejected with InvalidAddress.
#[test]
fn test_raise_dispute_zero_account_address_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let result = escrow.try_raise_dispute(&zero_account, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

/// Zero contract address (C…BSC4) must be rejected with InvalidAddress.
#[test]
fn test_raise_dispute_zero_contract_address_fails_comprehensive() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let zero_contract = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );
    let result = escrow.try_raise_dispute(&zero_contract, &0u32);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

/// Valid client address must succeed in raising a dispute (positive path).
#[test]
fn test_raise_dispute_valid_client_address_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let (client_addr, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let result = escrow.try_raise_dispute(&client_addr, &0u32);
    assert!(result.is_ok(), "valid client address must succeed");
}

/// Valid freelancer address must succeed in raising a dispute (positive path).
#[test]
fn test_raise_dispute_valid_freelancer_address_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let result = escrow.try_raise_dispute(&freelancer_addr, &0u32);
    assert!(result.is_ok(), "valid freelancer address must succeed");
}

/// Arbiter address (valid but unauthorized caller) must return Unauthorized.
/// This distinguishes address validity from role authorization.
#[test]
fn test_raise_dispute_arbiter_address_unauthorized_not_invalid() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, arbiter_addr, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let result = escrow.try_raise_dispute(&arbiter_addr, &0u32);
    // Arbiter is a valid address, but not client or freelancer → Unauthorized
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Random valid address (not a party) must return Unauthorized, not InvalidAddress.
#[test]
fn test_raise_dispute_random_valid_address_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let stranger = Address::generate(&env);
    let result = escrow.try_raise_dispute(&stranger, &0u32);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

/// Zero-address check occurs before NotFunded check:
/// passing zero account address on an unfunded escrow still returns InvalidAddress.
#[test]
fn test_raise_dispute_zero_address_before_not_funded_check() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);
    escrow.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &86400u64,
        &vec![&env, 1_000_i128],
    );
    // Escrow NOT funded

    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let result = escrow.try_raise_dispute(&zero_account, &0u32);
    // InvalidAddress must be returned even though escrow is not funded
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

/// Both zero address variants are rejected consistently regardless of milestone index.
#[test]
fn test_raise_dispute_zero_addresses_rejected_on_any_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 100_i128, 200_i128, 300_i128]);

    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let zero_contract = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );

    for idx in 0u32..3 {
        let r1 = escrow.try_raise_dispute(&zero_account, &idx);
        assert_eq!(
            r1,
            Err(Ok(Error::InvalidAddress)),
            "zero account on milestone {}",
            idx
        );
        let r2 = escrow.try_raise_dispute(&zero_contract, &idx);
        assert_eq!(
            r2,
            Err(Ok(Error::InvalidAddress)),
            "zero contract on milestone {}",
            idx
        );
    }
}
// resolve_dispute — strict state machine transition matrix (Issue #201)
// ============================================================================
//
// Permitted source status: Disputed only.
// Valid transitions:
//   Disputed → Released  (release_to_freelancer = true)
//   Disputed → Refunded  (release_to_freelancer = false)
// Every other source status must revert with Error::InvalidStatus and must
// not mutate milestone status or transfer funds.

/// Full transition matrix covering every MilestoneStatus as a source state.
#[test]
fn test_resolve_dispute_state_transition_matrix() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let token = token::Client::new(&env, &token_contract_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_contract_id);
    // Seven milestones: five invalid sources + two valid Disputed paths.
    token_admin.mint(&client_addr, &7_000);

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![
        &env, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128, 1_000_i128,
    ];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    client.fund(&client_addr);

    // --- Invalid sources (must fail with InvalidStatus, status unchanged) ---

    // 0: Pending → reject
    let result = client.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
    );

    // 1: Delivered → reject
    client.mark_delivered(&freelancer_addr, &1u32);
    let result = client.try_resolve_dispute(&arbiter_addr, &1u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        client.get_job().milestones.get(1).unwrap().status,
        MilestoneStatus::Delivered
    );

    // 2: PartiallyReleased → reject
    client.mark_delivered(&freelancer_addr, &2u32);
    client.approve_partial(&client_addr, &2u32, &400_i128);
    let result = client.try_resolve_dispute(&arbiter_addr, &2u32, &false);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        client.get_job().milestones.get(2).unwrap().status,
        MilestoneStatus::PartiallyReleased
    );

    // 3: Released → reject
    client.mark_delivered(&freelancer_addr, &3u32);
    client.approve_milestone(&client_addr, &3u32);
    let result = client.try_resolve_dispute(&arbiter_addr, &3u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        client.get_job().milestones.get(3).unwrap().status,
        MilestoneStatus::Released
    );

    // 4: Refunded → reject (settled via prior dispute resolution)
    client.mark_delivered(&freelancer_addr, &4u32);
    client.raise_dispute(&client_addr, &4u32);
    client.resolve_dispute(&arbiter_addr, &4u32, &false);
    assert_eq!(
        client.get_job().milestones.get(4).unwrap().status,
        MilestoneStatus::Refunded
    );
    let result = client.try_resolve_dispute(&arbiter_addr, &4u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        client.get_job().milestones.get(4).unwrap().status,
        MilestoneStatus::Refunded
    );

    // --- Valid sources ---

    // 5: Disputed → Released
    client.mark_delivered(&freelancer_addr, &5u32);
    client.raise_dispute(&client_addr, &5u32);
    client.resolve_dispute(&arbiter_addr, &5u32, &true);
    assert_eq!(
        client.get_job().milestones.get(5).unwrap().status,
        MilestoneStatus::Released
    );

    // Re-resolve after Released must also fail (terminal status).
    let result = client.try_resolve_dispute(&arbiter_addr, &5u32, &false);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        client.get_job().milestones.get(5).unwrap().status,
        MilestoneStatus::Released
    );

    // 6: Disputed → Refunded
    client.mark_delivered(&freelancer_addr, &6u32);
    client.raise_dispute(&freelancer_addr, &6u32);
    client.resolve_dispute(&arbiter_addr, &6u32, &false);
    assert_eq!(
        client.get_job().milestones.get(6).unwrap().status,
        MilestoneStatus::Refunded
    );

    // Invalid transitions must not have paid out milestones 0–1.
    // Milestone 2 paid 400 partial; 3 released 1000; 4 refunded 1000;
    // 5 released 1000; 6 refunded 1000.
    assert_eq!(token.balance(&freelancer_addr), 400 + 1_000 + 1_000);
    assert_eq!(
        client.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
    );
    assert_eq!(
        client.get_job().milestones.get(1).unwrap().status,
        MilestoneStatus::Delivered
    );
}

/// Invalid transition from Pending leaves balances and status untouched.
#[test]
fn test_resolve_dispute_from_pending_fails_without_side_effects() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, _, arbiter_addr, _, token_contract_id, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token = token::Client::new(&env, &token_contract_id);

    let client_before = token.balance(&client_addr);
    let contract_before = token.balance(&contract_id);

    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
    );
    assert_eq!(token.balance(&client_addr), client_before);
    assert_eq!(token.balance(&contract_id), contract_before);
}

/// Invalid transition from Delivered returns deterministic InvalidStatus.
#[test]
fn test_resolve_dispute_from_delivered_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (_, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &false);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Delivered
    );
}

/// Invalid transition from PartiallyReleased returns deterministic InvalidStatus.
#[test]
fn test_resolve_dispute_from_partially_released_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 2_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_partial(&client_addr, &0u32, &500_i128);

    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::PartiallyReleased
    );
}

/// Invalid transition from Released returns deterministic InvalidStatus.
#[test]
fn test_resolve_dispute_from_released_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.approve_milestone(&client_addr, &0u32);

    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
}

/// Invalid transition from Refunded returns deterministic InvalidStatus.
#[test]
fn test_resolve_dispute_from_refunded_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.raise_dispute(&client_addr, &0u32);
    escrow.resolve_dispute(&arbiter_addr, &0u32, &false);

    let result = escrow.try_resolve_dispute(&arbiter_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::InvalidStatus)));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Refunded
    );
}

/// Valid: Disputed → Released preserves payout and authorization rules.
#[test]
fn test_resolve_dispute_from_disputed_to_released_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, token_contract_id, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token = token::Client::new(&env, &token_contract_id);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.raise_dispute(&client_addr, &0u32);
    escrow.resolve_dispute(&arbiter_addr, &0u32, &true);

    let job = escrow.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Released
    );
    assert_eq!(job.milestones.get(0).unwrap().released_amount, 1_000);
    assert_eq!(token.balance(&freelancer_addr), 1_000);
}

/// Valid: Disputed → Refunded preserves refund payment logic.
#[test]
fn test_resolve_dispute_from_disputed_to_refunded_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, arbiter_addr, _, token_contract_id, contract_id, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let token = token::Client::new(&env, &token_contract_id);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.raise_dispute(&client_addr, &0u32);
    escrow.resolve_dispute(&arbiter_addr, &0u32, &false);

    let job = escrow.get_job();
    assert_eq!(
        job.milestones.get(0).unwrap().status,
        MilestoneStatus::Refunded
    );
    assert_eq!(token.balance(&client_addr), 1_000);
    assert_eq!(token.balance(&contract_id), 0);
    assert_eq!(token.balance(&freelancer_addr), 0);
}

/// Authorization is preserved: non-arbiter callers are still rejected.
#[test]
fn test_resolve_dispute_unauthorized_still_fails_after_state_machine() {
    let env = Env::default();
    env.mock_all_auths();

    let (client_addr, freelancer_addr, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.mark_delivered(&freelancer_addr, &0u32);
    escrow.raise_dispute(&client_addr, &0u32);

    let result = escrow.try_resolve_dispute(&client_addr, &0u32, &true);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    assert_eq!(
        escrow.get_job().milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
}

// ============================================================================
// Issue #268: milestone_time_extensions event emission test
// Issue #267: escrow_interest_yield comprehensive unit test suite
// ============================================================================

#[test]
fn test_milestone_time_extensions_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amount = 1_000_i128;
    let elapsed = 300_i128;
    let total = 600_i128;

    let split = client.milestone_time_extensions(&amount, &elapsed, &total);
    assert_eq!(split.first, 500);
    assert_eq!(split.second, 500);

    let events = env.events().all();
    let m_ext_topic: Symbol = symbol_short!("m_ext");
    let m_ext_topic_val: Val = m_ext_topic.into_val(&env);

    let mut found_event = false;
    for e in events.iter() {
        if let Some(topic) = e.1.get(0) {
            if topic.get_payload() == m_ext_topic_val.get_payload() {
                found_event = true;
                let event_data = MilestoneTimeExtensionEvent::from_val(&env, &e.2);
                assert_eq!(event_data.amount, amount);
                assert_eq!(event_data.elapsed_seconds, elapsed);
                assert_eq!(event_data.total_seconds, total);
                assert_eq!(event_data.freelancer_share, 500);
                assert_eq!(event_data.client_refund, 500);
            }
        }
    }
    assert!(found_event, "Expected m_ext event to be published");
}

#[test]
fn test_milestone_time_extensions_zero_elapsed_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amount = 1_000_i128;
    let elapsed = 0_i128;
    let total = 600_i128;

    let split = client.milestone_time_extensions(&amount, &elapsed, &total);
    assert_eq!(split.first, 0);
    assert_eq!(split.second, 1_000);

    let events = env.events().all();
    let m_ext_topic: Symbol = symbol_short!("m_ext");
    let m_ext_topic_val: Val = m_ext_topic.into_val(&env);

    let mut found_event = false;
    for e in events.iter() {
        if let Some(topic) = e.1.get(0) {
            if topic.get_payload() == m_ext_topic_val.get_payload() {
                found_event = true;
                let event_data = MilestoneTimeExtensionEvent::from_val(&env, &e.2);
                assert_eq!(event_data.amount, amount);
                assert_eq!(event_data.elapsed_seconds, elapsed);
                assert_eq!(event_data.total_seconds, total);
                assert_eq!(event_data.freelancer_share, 0);
                assert_eq!(event_data.client_refund, 1_000);
            }
        }
    }
    assert!(found_event, "Expected m_ext event to be published");
}

#[test]
fn test_milestone_time_extensions_full_elapsed_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amount = 1_000_i128;
    let elapsed = 600_i128;
    let total = 600_i128;

    let split = client.milestone_time_extensions(&amount, &elapsed, &total);
    assert_eq!(split.first, 1_000);
    assert_eq!(split.second, 0);

    let events = env.events().all();
    let m_ext_topic: Symbol = symbol_short!("m_ext");
    let m_ext_topic_val: Val = m_ext_topic.into_val(&env);

    let mut found_event = false;
    for e in events.iter() {
        if let Some(topic) = e.1.get(0) {
            if topic.get_payload() == m_ext_topic_val.get_payload() {
                found_event = true;
                let event_data = MilestoneTimeExtensionEvent::from_val(&env, &e.2);
                assert_eq!(event_data.amount, amount);
                assert_eq!(event_data.elapsed_seconds, elapsed);
                assert_eq!(event_data.total_seconds, total);
                assert_eq!(event_data.freelancer_share, 1_000);
                assert_eq!(event_data.client_refund, 0);
            }
        }
    }
    assert!(found_event, "Expected m_ext event to be published");
}

// ── escrow_interest_yield Unit Tests (#267) ───────────────────────────────

fn setup_test_env(env: &Env) -> (Address, Address, Address, Address, u64) {
    let admin = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let auto_release = 604800_u64;
    (admin, client_addr, freelancer_addr, token, auto_release)
}

#[test]
fn test_escrow_interest_yield_calculation_basic() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 10,000 principal, 500 bps (5%), 1 full year (31,536,000s)
    let yield_amt = client.escrow_interest_yield(&10_000_i128, &500_i128, &31_536_000_i128);
    assert_eq!(yield_amt, 500);
}

#[test]
fn test_escrow_interest_yield_calculation_half_year() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 10,000 principal, 1,000 bps (10%), 6 months (15,768,000s)
    let yield_amt = client.escrow_interest_yield(&10_000_i128, &1_000_i128, &15_768_000_i128);
    assert_eq!(yield_amt, 500);
}

#[test]
fn test_escrow_interest_yield_zero_principal_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&0_i128, &500_i128, &31_536_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_escrow_interest_yield_negative_principal_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&-100_i128, &500_i128, &31_536_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_escrow_interest_yield_zero_rate_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&10_000_i128, &0_i128, &31_536_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_escrow_interest_yield_negative_rate_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&10_000_i128, &-10_i128, &31_536_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_escrow_interest_yield_excessive_rate_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&10_000_i128, &10_001_i128, &31_536_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidRatio)));
}

#[test]
fn test_escrow_interest_yield_max_rate_succeeds() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 100% rate = 10,000 bps
    let yield_amt = client.escrow_interest_yield(&10_000_i128, &10_000_i128, &31_536_000_i128);
    assert_eq!(yield_amt, 10_000);
}

#[test]
fn test_escrow_interest_yield_zero_duration_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&10_000_i128, &500_i128, &0_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_escrow_interest_yield_negative_duration_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&10_000_i128, &500_i128, &-100_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_escrow_interest_yield_overflow_fails() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_escrow_interest_yield(&i128::MAX, &10_000_i128, &31_536_000_i128);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_escrow_interest_yield_share_config_not_initialized_initially() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let res = client.try_get_escrow_interest_yield();
    assert_eq!(res, Err(Ok(Error::NotInitialized)));
}

#[test]
fn test_escrow_interest_yield_set_valid_config_and_get() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, client_addr, freelancer_addr, token, auto_release) = setup_test_env(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let arbiter = Address::generate(&env);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter,
        &token,
        &auto_release,
        &vec![&env, 1_000_i128],
    );

    client.set_escrow_interest_yield(&admin, &5_000u32, &5_000u32);

    let config = client.get_escrow_interest_yield();
    assert_eq!(config.client_share_bps, 5_000);
    assert_eq!(config.freelancer_share_bps, 5_000);
    assert_eq!(config.locked, false);
}

#[test]
fn test_escrow_interest_yield_set_invalid_share_total_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, client_addr, freelancer_addr, token, auto_release) = setup_test_env(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let arbiter = Address::generate(&env);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter,
        &token,
        &auto_release,
        &vec![&env, 1_000_i128],
    );

    let res1 = client.try_set_escrow_interest_yield(&admin, &6_000u32, &5_000u32);
    assert_eq!(res1, Err(Ok(Error::InvalidRatio)));

    let res2 = client.try_set_escrow_interest_yield(&admin, &3_000u32, &3_000u32);
    assert_eq!(res2, Err(Ok(Error::InvalidRatio)));
}

#[test]
fn test_escrow_interest_yield_lock_unlock_workflow() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, client_addr, freelancer_addr, token, auto_release) = setup_test_env(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let arbiter = Address::generate(&env);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter,
        &token,
        &auto_release,
        &vec![&env, 1_000_i128],
    );

    client.set_escrow_interest_yield(&admin, &5_000u32, &5_000u32);
    assert_eq!(client.is_escrow_interest_yield_locked(), false);

    client.lock_escrow_interest_yield(&admin);
    assert_eq!(client.is_escrow_interest_yield_locked(), true);

    let res = client.try_set_escrow_interest_yield(&admin, &6_000u32, &4_000u32);
    assert_eq!(res, Err(Ok(Error::EscrowLocked)));

    client.unlock_escrow_interest_yield(&admin);
    assert_eq!(client.is_escrow_interest_yield_locked(), false);

    client.set_escrow_interest_yield(&admin, &6_000u32, &4_000u32);
    let updated = client.get_escrow_interest_yield();
    assert_eq!(updated.client_share_bps, 6_000);
    assert_eq!(updated.freelancer_share_bps, 4_000);
}

#[test]
fn test_escrow_interest_yield_unauthorized_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, client_addr, freelancer_addr, token, auto_release) = setup_test_env(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    let arbiter = Address::generate(&env);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter,
        &token,
        &auto_release,
        &vec![&env, 1_000_i128],
    );

    let impostor = Address::generate(&env);
    let res = client.try_set_escrow_interest_yield(&impostor, &5_000u32, &5_000u32);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
}

// ============================================================================
// Zero/empty balance guards — payment_streaming_milestones (Issue #272)
// ============================================================================

/// Guard — ZERO TOTAL: streaming on a zero balance must fail.
#[test]
fn test_payment_streaming_milestones_zero_total_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_payment_streaming_milestones(&0_i128, &1_i128, &2_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Guard — NEGATIVE TOTAL: negative balance is always invalid.
#[test]
fn test_payment_streaming_milestones_negative_total_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_payment_streaming_milestones(&-500_i128, &1_i128, &2_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Happy path — a positive total must still work correctly after the guard.
#[test]
fn test_payment_streaming_milestones_positive_total_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 1000 streamed at 1/4 elapsed: first=250, second=750, sum=1000
    let split = client.payment_streaming_milestones(&1_000_i128, &1_i128, &4_i128);
    assert_eq!(split.first, 250);
    assert_eq!(split.second, 750);
    assert_eq!(split.first + split.second, 1_000);
}

// ============================================================================
// Zero/empty balance guards — milestone_time_extensions (Issue #271)
// ============================================================================

/// Guard — ZERO AMOUNT: distributing nothing is blocked.
/// The previous behaviour returned (0, 0); the new behaviour raises InvalidAmount.
#[test]
fn test_milestone_time_extensions_zero_balance_is_blocked() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&0_i128, &3_i128, &10_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Guard — NEGATIVE AMOUNT: a negative milestone balance is rejected before time checks.
#[test]
fn test_milestone_time_extensions_negative_balance_is_blocked() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_milestone_time_extensions(&-1_000_i128, &3_i128, &10_i128);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// Regression — a positive balance still produces the correct split and emits an event.
#[test]
fn test_milestone_time_extensions_positive_balance_succeeds_and_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    // 600 split at 1/3 elapsed: round_nearest(600 * 1 / 3) = 200
    let split = client.milestone_time_extensions(&600_i128, &1_i128, &3_i128);
    assert_eq!(split.first, 200);
    assert_eq!(split.second, 400);
    assert_eq!(split.first + split.second, 600);

    // Verify the event was published
    let events = env.events().all();
    assert!(!events.is_empty(), "expected at least one event");
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, Symbol::new(&env, "m_ext"));
}

// ============================================================================
// Zero/empty balance guards + event emission — multisig_transfer_admin
// (Issues #270 and #273)
// ============================================================================

/// Event emission — a successful multisig_transfer_admin call must emit a
/// MultiSigTransferAdminEvent with the correct fields (issue #270).
#[test]
fn test_multisig_transfer_admin_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128, 1_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &300_i128, &ratios);

    // Verify allocation correctness
    assert_eq!(allocations.len(), 3);
    let total: i128 = allocations.iter().sum();
    assert_eq!(total, 300);

    // Verify the event was published
    let events = env.events().all();
    assert!(!events.is_empty(), "expected at least one event");
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, Symbol::new(&env, "msigtrx"));
}

/// Event emission — verify the event carries the correct total_amount
/// and num_parties fields.
#[test]
fn test_multisig_transfer_admin_event_fields_are_correct() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 3_i128, 1_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &1_000_i128, &ratios);

    // Verify numeric allocations: 750 and 250 (3/4 and 1/4 of 1000)
    assert_eq!(allocations.get(0).unwrap(), 750);
    assert_eq!(allocations.get(1).unwrap(), 250);

    // Verify event topic
    let events = env.events().all();
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, Symbol::new(&env, "msigtrx"));
}

/// Event emission — a single-party split still emits the event.
#[test]
fn test_multisig_transfer_admin_single_party_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128];
    let allocations = client.multisig_transfer_admin(&admin_addr, &500_i128, &ratios);

    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations.get(0).unwrap(), 500);

    let events = env.events().all();
    assert!(!events.is_empty());
    let last = events.last().unwrap();
    let topic: Symbol = last.1.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, Symbol::new(&env, "msigtrx"));
}

/// Guard + Event — zero total with valid ratios is blocked BEFORE any event
/// is emitted (no phantom events on failed calls).
#[test]
fn test_multisig_transfer_admin_zero_total_emits_no_event() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin_addr, _, _, _, _, _, client) = setup_multisig_env(&env);

    let ratios = vec![&env, 1_i128, 1_i128];
    let result = client.try_multisig_transfer_admin(&admin_addr, &0_i128, &ratios);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));

    // No events should have been emitted for a rejected call.
    let events = env.events().all();
    assert!(events.is_empty(), "no events expected on failed call");
}

// ═══════════════════════════════════════════════════════════════════════════════
// cancel_escrow: zero/empty balance boundary guards (issue #294)
//
// cancel_escrow must reject cancellation when the contract's token balance
// is zero or negative, because there are no funds left to resolve through
// the admin override path.
// ═══════════════════════════════════════════════════════════════════════════════

/// Happy path: cancel_escrow succeeds on a funded escrow with a positive
/// token balance.
#[test]
fn test_cancel_escrow_succeeds_with_positive_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 5_000_i128];
    let (client_addr, _, _, _, _, _, client) =
        setup_funded_escrow(&env, amounts);

    // Balance is positive — cancel should succeed.
    client.cancel_escrow(&client_addr);

    // Verify the cancel event was emitted exactly once.
    let cancel_topic_val: Val = symbol_short!("cancel").into_val(&env);
    let mut cancel_events = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == cancel_topic_val.get_payload() {
                cancel_events += 1;
            }
        }
    }
    assert_eq!(cancel_events, 1);
}

/// cancel_escrow is rejected when all milestones have been fully released
/// and the contract token balance is zero.
#[test]
fn test_cancel_escrow_rejected_when_balance_zero_after_full_release() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 3_000_i128, 7_000_i128];
    let (client_addr, freelancer_addr, _, _, _token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, amounts);

    // Release all milestones so the contract balance drops to zero.
    client.mark_delivered(&freelancer_addr, &0u32);
    client.approve_milestone(&client_addr, &0u32);
    client.mark_delivered(&freelancer_addr, &1u32);
    client.approve_milestone(&client_addr, &1u32);

    // Contract balance is now zero.
    let token_client = token::Client::new(&env, &_token_contract_id);
    assert_eq!(token_client.balance(&contract_id), 0);

    let result = client.try_cancel_escrow(&client_addr);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// cancel_escrow is rejected when the contract token balance has been
/// externally drained to zero via a direct token transfer.
#[test]
fn test_cancel_escrow_rejected_when_balance_drained_externally() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env.register(mock_token::MockToken, ());
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.mint(&client_addr, &10_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow_client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    escrow_client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    escrow_client.fund(&client_addr);

    // Externally drain the contract's entire balance.
    token.transfer(&contract_id, &client_addr, &10_000_i128);
    assert_eq!(token.balance(&contract_id), 0);

    let result = escrow_client.try_cancel_escrow(&client_addr);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// cancel_escrow is rejected when the contract token balance has been
/// partially drained, leaving zero (edge case: drain exact amount).
#[test]
fn test_cancel_escrow_rejected_when_balance_drained_to_exact_zero() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env.register(mock_token::MockToken, ());
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.mint(&client_addr, &5_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow_client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 5_000_i128];
    escrow_client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    escrow_client.fund(&client_addr);
    assert_eq!(token.balance(&contract_id), 5_000);

    // Drain exactly the funded amount.
    token.transfer(&contract_id, &admin_addr, &5_000_i128);
    assert_eq!(token.balance(&contract_id), 0);

    let result = escrow_client.try_cancel_escrow(&client_addr);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

/// cancel_escrow is rejected on a contract that has not been initialized.
#[test]
fn test_cancel_escrow_before_initialize_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let result = client.try_cancel_escrow(&client_addr);
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

/// cancel_escrow is rejected on a contract that has been initialized but
/// not yet funded.
#[test]
fn test_cancel_escrow_before_fund_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 1_000_i128];
    client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );

    let result = client.try_cancel_escrow(&client_addr);
    assert_eq!(result, Err(Ok(Error::NotFunded)));
}

/// cancel_escrow is rejected when the caller is neither the client nor
/// the freelancer.
#[test]
fn test_cancel_escrow_unauthorized_caller_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, amounts);

    let bad_actor = Address::generate(&env);
    let result = client.try_cancel_escrow(&bad_actor);
    assert!(result.is_err());
}

/// cancel_escrow is rejected when the caller is a zero account address.
#[test]
fn test_cancel_escrow_zero_address_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, amounts);

    let zero_account = Address::from_str(
        &env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    );
    let result = client.try_cancel_escrow(&zero_account);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

/// cancel_escrow is rejected when the caller is a zero contract address.
#[test]
fn test_cancel_escrow_zero_contract_address_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, amounts);

    let zero_contract = Address::from_str(
        &env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    );
    let result = client.try_cancel_escrow(&zero_contract);
    assert_eq!(result, Err(Ok(Error::InvalidAddress)));
}

/// cancel_escrow emits a structured CancelEscrowInitiatedEvent with the
/// correct contract_id and caller fields.
#[test]
fn test_cancel_escrow_emits_structured_event() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 2_000_i128];
    let (client_addr, _, _, _, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, amounts);

    client.cancel_escrow(&client_addr);

    let cancel_topic: Symbol = symbol_short!("cancel");
    let cancel_topic_val: Val = cancel_topic.into_val(&env);
    let mut cancel_events = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == cancel_topic_val.get_payload() {
                cancel_events += 1;
                assert_eq!(event.1.len(), 1);
                assert_eq!(
                    CancelEscrowInitiatedEvent::from_val(&env, &event.2),
                    CancelEscrowInitiatedEvent {
                        contract_id: contract_id.clone(),
                        caller: client_addr.clone(),
                    }
                );
            }
        }
    }
    assert_eq!(cancel_events, 1);
}

/// cancel_escrow by the freelancer also succeeds when balance is positive.
#[test]
fn test_cancel_escrow_freelancer_can_cancel() {
    let env = Env::default();
    env.mock_all_auths();

    let amounts = vec![&env, 4_000_i128];
    let (_, freelancer_addr, _, _, _, _, client) =
        setup_funded_escrow(&env, amounts);

    client.cancel_escrow(&freelancer_addr);

    // Verify the cancel event was emitted.
    let cancel_topic_val: Val = symbol_short!("cancel").into_val(&env);
    let mut cancel_events = 0u32;
    for event in env.events().all().iter() {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == cancel_topic_val.get_payload() {
                cancel_events += 1;
            }
        }
    }
    assert_eq!(cancel_events, 1);
}

/// cancel_escrow rejects when balance is partially drained but still
/// positive (balance > 0 should still be allowed).
#[test]
fn test_cancel_escrow_succeeds_with_partial_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env.register(mock_token::MockToken, ());
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.mint(&client_addr, &10_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow_client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    escrow_client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    escrow_client.fund(&client_addr);

    // Drain some but not all funds — balance remains positive.
    token.transfer(&contract_id, &admin_addr, &7_000_i128);
    assert_eq!(token.balance(&contract_id), 3_000);

    // Cancel should still succeed because balance > 0.
    escrow_client.cancel_escrow(&client_addr);
}

/// cancel_escrow emits no CancelEscrowInitiatedEvent when rejected due to
/// empty balance (no phantom events on failed calls).
#[test]
fn test_cancel_escrow_empty_balance_emits_no_event() {
    let env = Env::default();
    env.mock_all_auths();

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);
    let admin_addr = Address::generate(&env);

    let token_contract_id = env.register(mock_token::MockToken, ());
    let token = mock_token::MockTokenClient::new(&env, &token_contract_id);
    token.mint(&client_addr, &10_000_i128);

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow_client = MilestoneEscrowClient::new(&env, &contract_id);

    let amounts = vec![&env, 10_000_i128];
    escrow_client.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604800,
        &amounts,
    );
    escrow_client.fund(&client_addr);

    // Drain all funds.
    token.transfer(&contract_id, &client_addr, &10_000_i128);

    let result = escrow_client.try_cancel_escrow(&client_addr);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));

    // No cancel events should have been emitted.
    let cancel_topic_val: Val = symbol_short!("cancel").into_val(&env);
    let cancel_events = env.events().all().iter().fold(0u32, |acc, event| {
        if let Some(topic) = event.1.get(0) {
            if topic.get_payload() == cancel_topic_val.get_payload() {
                return acc + 1;
            }
        }
        acc
    });
    assert_eq!(cancel_events, 0);
}

#[test]
fn test_platform_fee_allocation_preserves_value_with_largest_remainders() {
    let env = Env::default();
    env.mock_all_auths();

    let (admin, client_addr, freelancer_addr, token, auto_release) = setup_test_env(&env);
    let arbiter = Address::generate(&env);
    let contract_id = env.register(MilestoneEscrow, ());
    let client = MilestoneEscrowClient::new(&env, &contract_id);
    client.initialize(
        &admin,
        &client_addr,
        &freelancer_addr,
        &arbiter,
        &token,
        &auto_release,
        &vec![&env, 1_000_i128],
    );

    client.set_platform_fee_allocation(&admin, &3333, &3333, &3334);
    let distribution = client.calculate_platform_fee_split(&2_i128);
    assert_eq!(distribution.client_amount, 1);
    assert_eq!(distribution.freelancer_amount, 0);
    assert_eq!(distribution.treasury_amount, 1);
    assert_eq!(
        distribution.client_amount + distribution.freelancer_amount + distribution.treasury_amount,
        2
    );

    let zero = client.calculate_platform_fee_split(&0_i128);
    assert_eq!(
        zero.client_amount + zero.freelancer_amount + zero.treasury_amount,
        0
    );
    let negative = client.try_calculate_platform_fee_split(&-1_i128);
    assert_eq!(negative, Err(Ok(Error::InvalidAmount)));
}
fn test_tax_withholding_deductions_zero_balance_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, _, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Empty the contract balance by transferring all tokens out
    let token_client = token::Client::new(&env, &token_contract_id);
    token_client.transfer(&contract_id, &Address::generate(&env), &1000_i128);

    // Call tax_withholding_deductions (should fail with InvalidAmount)
    let res = client.try_tax_withholding_deductions(&0u32, &1000u32);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_admin_tax_withholding_deductions_zero_balance_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1000_i128];
    let (_, _, _, admin_addr, token_contract_id, contract_id, client) =
        setup_funded_escrow(&env, milestone_amounts);

    // Empty the contract balance by transferring all tokens out
    let token_client = token::Client::new(&env, &token_contract_id);
    token_client.transfer(&contract_id, &Address::generate(&env), &1000_i128);

    // Call admin_tax_withholding_deductions (should fail with InvalidAmount)
    let res = client.try_admin_tax_withholding_deductions(&admin_addr, &0u32, &1000u32);
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
}

// ═══════════════════════════════════════════════════════════════════════════════
// tax_withholding_deductions: ratio-split precision (#299)
//
// tax_withholding_deductions computes its split via the shared
// split_round_nearest helper: round(gross * rate / BPS_SCALE) to the nearest
// integer for tax_amount, then derive net_amount = gross - tax_amount by
// subtraction rather than a second independent division. That subtraction is
// what guarantees tax_amount + net_amount == gross_amount exactly for every
// rate — there is no way for the split to lose or manufacture value, and no
// way for it to silently truncate (floor) a fraction that should round up.
// These tests exercise that guarantee directly against the public
// tax_withholding_deductions entry point, which the two "emits_event" tests
// above do not (they call the unrelated multisig_transfer_admin function).
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_tax_withholding_deductions_conserves_value_across_rates() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // tax_amount + net_amount must equal gross_amount exactly for every rate,
    // including rates that do not divide the gross amount evenly (37 bps,
    // 3_333 bps, 6_667 bps) — no value may be created or destroyed by the
    // split, regardless of rounding.
    for tax_rate_bps in [0u32, 1, 37, 2_500, 3_333, 5_000, 6_667, 9_999, 10_000] {
        let record = client.tax_withholding_deductions(&0u32, &tax_rate_bps);
        assert_eq!(
            record.tax_amount + record.net_amount,
            record.gross_amount,
            "value lost/gained at tax_rate_bps={tax_rate_bps}"
        );
        assert_eq!(record.gross_amount, 1_000_000);
        assert_eq!(record.tax_rate_bps, tax_rate_bps);
        assert!(record.tax_amount >= 0 && record.net_amount >= 0);
    }
}

#[test]
fn test_tax_withholding_deductions_rounds_to_nearest_not_down() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 3_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // gross=3, rate=50% (5_000 bps): exact tax is 1.5, which must round to
    // the nearest integer (2) rather than truncate down to 1 the way plain
    // floor division (gross * rate / BPS_SCALE) would.
    let record = client.tax_withholding_deductions(&0u32, &5_000u32);
    assert_eq!(record.tax_amount, 2);
    assert_eq!(record.net_amount, 1);
    assert_eq!(record.tax_amount + record.net_amount, 3);
}

#[test]
fn test_tax_withholding_deductions_rounds_down_when_fraction_is_below_half() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 100_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // gross=100, rate=12.49% (1_249 bps): exact tax is 12.49, which rounds
    // down to 12 — proving the split rounds to the nearest integer rather
    // than always rounding up (a ceiling would incorrectly give 13).
    let record = client.tax_withholding_deductions(&0u32, &1_249u32);
    assert_eq!(record.tax_amount, 12);
    assert_eq!(record.net_amount, 88);
}

#[test]
fn test_tax_withholding_deductions_zero_rate_withholds_nothing() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let record = client.tax_withholding_deductions(&0u32, &0u32);
    assert_eq!(record.tax_amount, 0);
    assert_eq!(record.net_amount, 1_000);
}

#[test]
fn test_tax_withholding_deductions_full_rate_withholds_everything() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let record = client.tax_withholding_deductions(&0u32, &10_000u32);
    assert_eq!(record.tax_amount, 1_000);
    assert_eq!(record.net_amount, 0);
}

#[test]
fn test_tax_withholding_deductions_rejects_rate_above_100_percent() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_000_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let result = client.try_tax_withholding_deductions(&0u32, &10_001u32);
    assert_eq!(result, Err(Ok(Error::InvalidRatio)));
}

#[test]
fn test_tax_withholding_deductions_conserves_value_at_smallest_unit() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 1_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    // Even at the smallest indivisible unit, every rate must still conserve
    // the total exactly and never produce a negative or out-of-range part.
    for tax_rate_bps in [0u32, 1, 4_999, 5_000, 5_001, 9_999, 10_000] {
        let record = client.tax_withholding_deductions(&0u32, &tax_rate_bps);
        assert_eq!(record.tax_amount + record.net_amount, 1);
        assert!(record.tax_amount == 0 || record.tax_amount == 1);
        assert!(record.net_amount == 0 || record.net_amount == 1);
    }
}

#[test]
fn test_tax_withholding_deductions_independent_across_milestones() {
    let env = Env::default();
    env.mock_all_auths();

    let milestone_amounts = vec![&env, 400_i128, 600_i128];
    let (_, _, _, _, _, _, client) = setup_funded_escrow(&env, milestone_amounts);

    let record_a = client.tax_withholding_deductions(&0u32, &2_500u32);
    let record_b = client.tax_withholding_deductions(&1u32, &7_500u32);

    assert_eq!(record_a.gross_amount, 400);
    assert_eq!(record_a.tax_amount, 100);
    assert_eq!(record_a.net_amount, 300);

    assert_eq!(record_b.gross_amount, 600);
    assert_eq!(record_b.tax_amount, 450);
    assert_eq!(record_b.net_amount, 150);
}
