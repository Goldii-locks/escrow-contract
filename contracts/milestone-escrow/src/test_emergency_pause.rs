#![cfg(test)]
//! Dedicated suite for the `emergency_pause` module.
//!
//! Covers two things:
//!
//! 1. **Business rules** — every bad setup (uninitialised contract, wrong
//!    caller, redundant transition, unfrozen escrow) is rejected immediately
//!    with its own descriptive error variant, before any state is written.
//! 2. **High-precision division** — `emergency_pause_allocation` divides a
//!    frozen balance across parties without losing value or rounding anyone
//!    systematically down.

use super::*;
use soroban_sdk::{
    testutils::Address as _, testutils::EnvTestConfig, testutils::Ledger as _, token, vec, Address,
    Env, FromVal, IntoVal, Symbol, TryFromVal, Val,
};

// ── fixtures ────────────────────────────────────────────────────────────────

fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A contract that has never been initialised — no admin key stored.
fn bare_contract(env: &Env) -> MilestoneEscrowClient<'_> {
    let contract_id = env.register(MilestoneEscrow, ());
    MilestoneEscrowClient::new(env, &contract_id)
}

/// A fully initialised, unpaused escrow plus its admin address.
fn initialised_escrow(env: &Env) -> (MilestoneEscrowClient<'_>, Address, Address, Address) {
    env.mock_all_auths();

    let admin_addr = Address::generate(env);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);

    let amounts = vec![env, 1_000_i128];
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604_800u64,
        &amounts,
    );

    // Fund the contract so pause-gated refund settlements (which reject an
    // empty balance) can proceed in the tests that exercise the split math.
    let token_admin = token::StellarAssetClient::new(env, &token_contract_id);
    token_admin.mint(&contract_id, &100_000_i128);

    (escrow, admin_addr, client_addr, freelancer_addr)
}

// ============================================================================
// emergency_pause — business rules
// ============================================================================

#[test]
fn test_pause_requires_an_initialised_contract() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);
    let stranger = Address::generate(&env);

    // Pausing an uninitialised contract would set a flag that no admin path
    // could ever clear.
    assert_eq!(
        escrow.try_emergency_pause(&stranger, &stranger),
        Err(Ok(Error::NotInitialized))
    );
    assert!(!escrow.is_emergency_paused());
}

#[test]
fn test_unpause_requires_an_initialised_contract() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        escrow.try_emergency_unpause(&stranger),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_pause_rejects_a_non_admin_caller() {
    let env = test_env();
    let (escrow, _admin, _client, _freelancer) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    assert_eq!(
        escrow.try_emergency_pause(&attacker, &attacker),
        Err(Ok(Error::Unauthorized))
    );
    assert!(
        !escrow.is_emergency_paused(),
        "a rejected pause must not change state"
    );
}

#[test]
fn test_unpause_rejects_a_non_admin_caller() {
    let env = test_env();
    let (escrow, _admin, client, freelancer) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    escrow.emergency_pause(&client, &freelancer);

    assert_eq!(
        escrow.try_emergency_unpause(&attacker),
        Err(Ok(Error::Unauthorized))
    );
    assert!(
        escrow.is_emergency_paused(),
        "a rejected unpause must leave the freeze in place"
    );
}

#[test]
fn test_pause_sets_the_flag() {
    let env = test_env();
    let (escrow, _admin, client, freelancer) = initialised_escrow(&env);

    assert!(!escrow.is_emergency_paused());
    escrow.emergency_pause(&client, &freelancer);
    assert!(escrow.is_emergency_paused());
}

#[test]
fn test_pause_twice_is_rejected_as_already_paused() {
    let env = test_env();
    let (escrow, _admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    // A redundant pause must not read as fresh action during an incident.
    assert_eq!(
        escrow.try_emergency_pause(&client, &freelancer),
        Err(Ok(Error::AlreadyPaused))
    );
    assert!(escrow.is_emergency_paused());
}

#[test]
fn test_unpause_without_a_pause_is_rejected_as_not_paused() {
    let env = test_env();
    let (escrow, admin, _client, _freelancer) = initialised_escrow(&env);

    assert_eq!(
        escrow.try_emergency_unpause(&admin),
        Err(Ok(Error::NotPaused))
    );
    assert!(!escrow.is_emergency_paused());
}

#[test]
fn test_unpause_twice_is_rejected_as_not_paused() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);
    escrow.emergency_unpause(&admin);

    assert_eq!(
        escrow.try_emergency_unpause(&admin),
        Err(Ok(Error::NotPaused))
    );
}

#[test]
fn test_pause_unpause_cycle_is_repeatable() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    for _ in 0..3 {
        escrow.emergency_pause(&client, &freelancer);
        assert!(escrow.is_emergency_paused());
        escrow.emergency_unpause(&admin);
        assert!(!escrow.is_emergency_paused());
    }
}

#[test]
fn test_pause_releases_its_transition_lock() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    // If the lock leaked, the following unpause would fail with
    // EmergencyPauseInProgress instead of succeeding.
    escrow.emergency_pause(&client, &freelancer);
    escrow.emergency_unpause(&admin);
    escrow.emergency_pause(&client, &freelancer);

    assert!(escrow.is_emergency_paused());
}

#[test]
fn test_pause_blocks_guarded_endpoints() {
    let env = test_env();
    let (escrow, _admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    // `fund` is guarded by ensure_not_paused, so the freeze is observable
    // through the normal escrow flow, not just the status getter.
    let job = escrow.get_job();
    assert_eq!(escrow.try_fund(&job.client), Err(Ok(Error::Paused)));
}

#[test]
fn test_unpause_restores_guarded_endpoints() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);
    escrow.emergency_unpause(&admin);

    let job = escrow.get_job();
    assert_ne!(escrow.try_fund(&job.client), Err(Ok(Error::Paused)));
}

#[test]
fn test_pause_emits_a_state_change_event() {
    let env = test_env();
    let (escrow, _admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    let topic: Val = symbol_short!("empause").into_val(&env);
    let mut found = false;

    for e in crate::all_event_tuples(&env).iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                found = true;
                let data = EmergencyPausedEvent::from_val(&env, &e.2);
                assert_eq!(data.client, client);
                assert_eq!(data.freelancer, freelancer);
            }
        }
    }

    assert!(found, "expected an empause event naming the admin");
}

#[test]
fn test_rejected_transitions_emit_no_event() {
    let env = test_env();
    let (escrow, admin, _client, _freelancer) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    // Neither an unauthorised pause nor an unpause of a running contract may
    // publish a state-change event — on either topic.
    let _ = escrow.try_emergency_pause(&attacker, &attacker);
    let _ = escrow.try_emergency_unpause(&admin);

    let paused_topic: Val = symbol_short!("empause").into_val(&env);
    let unpaused_topic: Val = symbol_short!("emunpause").into_val(&env);

    for e in crate::all_event_tuples(&env).iter() {
        if let Some(t) = e.1.get(0) {
            assert_ne!(
                t.get_payload(),
                paused_topic.get_payload(),
                "a rejected pause published a state-change event"
            );
            assert_ne!(
                t.get_payload(),
                unpaused_topic.get_payload(),
                "a rejected unpause published a state-change event"
            );
        }
    }
}

// ============================================================================
// emergency_pause_claim_refund — pause-gated settlement rules
// ============================================================================

#[test]
fn test_claim_refund_requires_the_contract_to_be_paused() {
    let env = test_env();
    let (escrow, admin, _client, _freelancer) = initialised_escrow(&env);

    // Settling an emergency refund on a running escrow would bypass the
    // normal release and dispute paths.
    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::NotPaused))
    );
}

#[test]
fn test_claim_refund_requires_an_initialised_contract() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&stranger, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_claim_refund_rejects_a_non_admin_caller() {
    let env = test_env();
    let (escrow, _admin, client, freelancer) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    escrow.emergency_pause(&client, &freelancer);

    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&attacker, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_claim_refund_succeeds_while_paused() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    let allocation =
        escrow.emergency_pause_claim_refund(&admin, &1_000_i128, &6_000_u32, &4_000_u32);

    assert_eq!(allocation.client_refund, 600);
    assert_eq!(allocation.freelancer_payout, 400);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        1_000
    );
}

#[test]
fn test_claim_refund_rejects_shares_that_do_not_total_full_scale() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&admin, &1_000_i128, &5_000_u32, &3_000_u32),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&admin, &1_000_i128, &6_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn test_claim_refund_rejects_non_positive_totals() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&admin, &0_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&admin, &-1_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_claim_refund_conserves_odd_totals() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    for total in [1_i128, 3, 7, 101, 99_999] {
        let allocation =
            escrow.emergency_pause_claim_refund(&admin, &total, &3_333_u32, &6_667_u32);
        assert_eq!(
            allocation.client_refund + allocation.freelancer_payout,
            total,
            "total {total} was not conserved"
        );
    }
}

#[test]
fn test_claim_refund_rejects_an_empty_contract_balance() {
    let env = test_env();
    env.mock_all_auths();

    let admin_addr = Address::generate(&env);
    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let arbiter_addr = Address::generate(&env);

    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();

    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    // Initialised and paused, but never funded: nothing to settle.
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &604_800u64,
        &vec![&env, 1_000_i128],
    );
    escrow.emergency_pause(&client_addr, &freelancer_addr);

    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&admin_addr, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::EmptyBalance))
    );
}

// ============================================================================
// emergency_pause_allocation — high-precision division
// ============================================================================

#[test]
fn test_allocation_splits_an_even_total_exactly() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let weights = vec![&env, 1_i128, 1_i128];
    let out = escrow.emergency_pause_allocation(&100_i128, &weights);

    assert_eq!(out.get(0).unwrap(), 50);
    assert_eq!(out.get(1).unwrap(), 50);
}

#[test]
fn test_allocation_does_not_lose_a_stroop_to_truncation() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // 10 / 3 = 3.33… each. Plain floor division would hand out 3+3+3 = 9 and
    // strand the tenth stroop in the contract.
    let weights = vec![&env, 1_i128, 1_i128, 1_i128];
    let out = escrow.emergency_pause_allocation(&10_i128, &weights);

    let sum: i128 = out.iter().sum();
    assert_eq!(sum, 10, "the residue stroop must be allocated, not lost");

    // The extra unit goes to exactly one party; nobody is rounded down twice.
    assert_eq!(out.get(0).unwrap(), 4);
    assert_eq!(out.get(1).unwrap(), 3);
    assert_eq!(out.get(2).unwrap(), 3);
}

#[test]
fn test_allocation_conserves_the_total_across_a_wide_matrix() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // Awkward totals against awkward weights: the sum must land exactly on
    // the total every single time.
    for total in [1_i128, 2, 3, 7, 11, 97, 1_000, 100_003, 999_999_937] {
        for weights in [
            vec![&env, 1_i128, 1_i128, 1_i128],
            vec![&env, 1_i128, 2_i128, 3_i128],
            vec![&env, 7_i128, 11_i128, 13_i128, 17_i128],
            vec![&env, 1_i128, 1_i128, 1_i128, 1_i128, 1_i128, 1_i128, 1_i128],
            vec![&env, 9_999_i128, 1_i128],
        ] {
            let out = escrow.emergency_pause_allocation(&total, &weights);
            let sum: i128 = out.iter().sum();
            assert_eq!(sum, total, "total {total} was not conserved");
            assert_eq!(out.len(), weights.len());
        }
    }
}

#[test]
fn test_allocation_never_rounds_a_party_more_than_one_unit_down() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let total = 1_000_i128;
    let weights = vec![&env, 1_i128, 2_i128, 3_i128, 4_i128, 5_i128];
    let weight_sum: i128 = weights.iter().sum();

    let out = escrow.emergency_pause_allocation(&total, &weights);

    for (idx, weight) in weights.iter().enumerate() {
        let allocated = out.get(idx as u32).unwrap();
        let exact_floor = total * weight / weight_sum;

        // Each party gets its floor share, plus at most one residue unit.
        assert!(
            allocated >= exact_floor,
            "party {idx} was rounded below its floor share"
        );
        assert!(
            allocated <= exact_floor + 1,
            "party {idx} received more than one residue unit"
        );
    }
}

#[test]
fn test_allocation_respects_weight_proportions() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let weights = vec![&env, 1_i128, 3_i128];
    let out = escrow.emergency_pause_allocation(&400_i128, &weights);

    assert_eq!(out.get(0).unwrap(), 100);
    assert_eq!(out.get(1).unwrap(), 300);
}

#[test]
fn test_allocation_is_scale_invariant() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // Only the ratios matter, so scaling every weight changes nothing.
    let small = escrow.emergency_pause_allocation(&997_i128, &vec![&env, 1_i128, 2_i128, 3_i128]);
    let large = escrow
        .emergency_pause_allocation(&997_i128, &vec![&env, 1_000_i128, 2_000_i128, 3_000_i128]);

    for idx in 0..3u32 {
        assert_eq!(small.get(idx).unwrap(), large.get(idx).unwrap());
    }
}

#[test]
fn test_allocation_gives_a_zero_weighted_party_nothing() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // A zero weight also means a zero remainder, so this party can never win
    // a residue unit ahead of someone with a real fractional claim.
    let weights = vec![&env, 0_i128, 1_i128, 1_i128];
    let out = escrow.emergency_pause_allocation(&11_i128, &weights);

    assert_eq!(out.get(0).unwrap(), 0);
    assert_eq!(out.get(1).unwrap() + out.get(2).unwrap(), 11);
}

#[test]
fn test_allocation_handles_a_single_party() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let out = escrow.emergency_pause_allocation(&12_345_i128, &vec![&env, 5_i128]);
    assert_eq!(out.len(), 1);
    assert_eq!(out.get(0).unwrap(), 12_345);
}

#[test]
fn test_allocation_handles_a_total_smaller_than_the_party_count() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // Two stroops, five equal parties: three parties must get nothing, and
    // the two units must still be handed out rather than stranded.
    let weights = vec![&env, 1_i128, 1_i128, 1_i128, 1_i128, 1_i128];
    let out = escrow.emergency_pause_allocation(&2_i128, &weights);

    let sum: i128 = out.iter().sum();
    assert_eq!(sum, 2);
    assert_eq!(out.iter().filter(|a| *a == 1).count(), 2);
    assert_eq!(out.iter().filter(|a| *a == 0).count(), 3);
}

#[test]
fn test_allocation_breaks_ties_by_lowest_index_deterministically() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // All three remainders tie; the residue unit goes to index 0 every time.
    let weights = vec![&env, 1_i128, 1_i128, 1_i128];
    let first = escrow.emergency_pause_allocation(&10_i128, &weights);
    let second = escrow.emergency_pause_allocation(&10_i128, &weights);

    for idx in 0..3u32 {
        assert_eq!(first.get(idx).unwrap(), second.get(idx).unwrap());
    }
    assert_eq!(first.get(0).unwrap(), 4);
}

#[test]
fn test_allocation_favours_the_largest_discarded_fraction() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // total 10, weights 1:1:4 over a sum of 6:
    //   party 0 → 10/6  = 1 rem 4
    //   party 1 → 10/6  = 1 rem 4
    //   party 2 → 40/6  = 6 rem 4
    // floors sum to 8, so two units are handed to the two lowest indices
    // among the tied remainders.
    let out = escrow.emergency_pause_allocation(&10_i128, &vec![&env, 1_i128, 1_i128, 4_i128]);

    let sum: i128 = out.iter().sum();
    assert_eq!(sum, 10);
    assert_eq!(out.get(2).unwrap(), 6);
    assert_eq!(out.get(0).unwrap() + out.get(1).unwrap(), 4);
}

#[test]
fn test_allocation_rejects_non_positive_totals() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let weights = vec![&env, 1_i128, 1_i128];

    assert_eq!(
        escrow.try_emergency_pause_allocation(&0_i128, &weights),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        escrow.try_emergency_pause_allocation(&-5_i128, &weights),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_allocation_rejects_an_empty_weight_vector() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let empty: Vec<i128> = Vec::new(&env);
    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &empty),
        Err(Ok(Error::InvalidAllocationWeights))
    );
}

#[test]
fn test_allocation_rejects_negative_weights() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let weights = vec![&env, 3_i128, -1_i128];
    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &weights),
        Err(Ok(Error::InvalidAllocationWeights))
    );
}

#[test]
fn test_allocation_rejects_weights_summing_to_zero() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // All-zero weights describe no distribution at all — dividing by the sum
    // would be a division by zero.
    let weights = vec![&env, 0_i128, 0_i128, 0_i128];
    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &weights),
        Err(Ok(Error::InvalidAllocationWeights))
    );
}

// ── weight-vector validation ───────────────────────────────────────────────
//
// The three malformed shapes the contract documents — an empty vector, a
// zero-sum vector, and a vector holding a negative weight — all have to be
// turned away by a *typed contract error* before the largest-remainder phase
// reaches its first division by `weight_sum`.
//
// A trap would look different at the call site: a panic (division by zero,
// an out-of-bounds index, an escaping overflow) surfaces as `Err(Err(..))`
// with a host/invoke error, whereas a guard that returns cleanly surfaces as
// `Err(Ok(Error::InvalidAllocationWeights))`. Every assertion below matches
// on the outer `Ok`, so a guard that panicked instead of returning would fail
// the test rather than pass it.

/// Count the `epalloc` events on the ledger. A rejected weight vector must
/// never reach the allocation phase, so this must stay at zero.
fn epalloc_event_count(env: &Env) -> usize {
    let topic: Val = symbol_short!("epalloc").into_val(env);

    crate::all_event_tuples(env)
        .iter()
        .filter(|e| match e.1.get(0) {
            Some(t) => t.get_payload() == topic.get_payload(),
            None => false,
        })
        .count()
}

/// Every all-zero weight vector divides by a zero sum. All of them must be
/// rejected with the typed error — a single-element `[0]` is included because
/// it is the smallest vector that would panic on `weighted / weight_sum`.
#[test]
fn test_allocation_rejects_every_all_zero_weight_vector() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let all_zero = [
        vec![&env, 0_i128],
        vec![&env, 0_i128, 0_i128],
        vec![&env, 0_i128, 0_i128, 0_i128],
        vec![&env, 0_i128, 0_i128, 0_i128, 0_i128, 0_i128],
    ];

    for weights in all_zero.iter() {
        assert_eq!(
            escrow.try_emergency_pause_allocation(&100_i128, weights),
            Err(Ok(Error::InvalidAllocationWeights)),
            "an all-zero weight vector must be rejected, never divided through"
        );
    }
}

/// A zero *sum* is the case the divisor guard protects. A vector of all zeros
/// is the only way to reach it once negatives are excluded, so this pins both
/// facts: the sum check is reachable, and it fires before any division.
#[test]
fn test_allocation_rejects_a_zero_weight_sum_before_dividing() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // total 100 across zero weights is `100 × 0 / 0` — a division by zero on
    // every iteration, and a residue loop over undefined remainders after it.
    let weights = vec![&env, 0_i128, 0_i128, 0_i128];

    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &weights),
        Err(Ok(Error::InvalidAllocationWeights))
    );
    assert_eq!(
        epalloc_event_count(&env),
        0,
        "the zero-sum guard must fire before the allocation phase"
    );
}

/// A negative weight has to be caught by its own per-entry scan, not merely
/// by the sum check. This vector sums to a perfectly healthy `10`, so if only
/// the sum were validated the negative share would silently subtract itself
/// out of the divisor and hand party 1 a negative allocation.
#[test]
fn test_allocation_rejects_a_negative_weight_even_when_the_sum_stays_positive() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // 5 + (-1) + 6 = 10 > 0 — a positive sum cannot launder a negative entry.
    let weights = vec![&env, 5_i128, -1_i128, 6_i128];

    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &weights),
        Err(Ok(Error::InvalidAllocationWeights))
    );
    assert_eq!(epalloc_event_count(&env), 0);
}

/// The scan walks the whole vector, so a negative entry is caught wherever it
/// sits: head, middle, or tail. A guard that only looked at the first element
/// would let the tail case through.
#[test]
fn test_allocation_rejects_a_negative_weight_at_any_position() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let with_negative = [
        vec![&env, -1_i128, 10_i128],        // head
        vec![&env, 5_i128, -1_i128, 5_i128], // middle
        vec![&env, 10_i128, -1_i128],        // tail
        vec![&env, -1_i128],                 // sole entry
        vec![&env, 4_i128, -4_i128],         // sum is exactly zero
        vec![&env, 1_i128, 2_i128, 3_i128, -6_i128],
    ];

    for weights in with_negative.iter() {
        assert_eq!(
            escrow.try_emergency_pause_allocation(&100_i128, weights),
            Err(Ok(Error::InvalidAllocationWeights)),
            "a negative weight at any index must be rejected"
        );
    }
}

/// `i128::MIN` is the one weight that negates into a positive value, so a
/// naive `< 0` scan on the *abs* value would miss it. It is also a useful
/// overflow probe: `i128::MIN + anything` never traps, so the sum check
/// cannot stand in for the per-entry scan.
#[test]
fn test_allocation_rejects_the_most_negative_weight() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let weights = vec![&env, i128::MIN, 7_i128];
    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &weights),
        Err(Ok(Error::InvalidAllocationWeights))
    );
}

/// An empty vector has no divisor to build and no party to pay. A loop that
/// iterated over it would return an empty allocation rather than an error —
/// a silent, unpayable settlement — so the emptiness check must come first
/// and must not emit anything.
#[test]
fn test_allocation_rejects_an_empty_vector_before_any_allocation_work() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let empty: Vec<i128> = Vec::new(&env);
    assert!(empty.is_empty());

    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &empty),
        Err(Ok(Error::InvalidAllocationWeights))
    );
    assert_eq!(
        epalloc_event_count(&env),
        0,
        "an empty weight vector must not reach the allocation phase"
    );
}

/// One matrix over every documented malformed shape, asserting the same typed
/// error each time. Keeping it in a single loop pins the contract's promise
/// that all three shapes are the *same* caller-visible failure, so off-chain
/// callers need only match one variant.
#[test]
fn test_allocation_rejects_every_malformed_weight_shape_with_one_typed_error() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let empty: Vec<i128> = Vec::new(&env);
    let malformed = [
        // empty
        empty.clone(),
        // zero total weight
        vec![&env, 0_i128],
        vec![&env, 0_i128, 0_i128, 0_i128],
        // negative entry, positive sum
        vec![&env, 5_i128, -1_i128, 6_i128],
        // negative entry, zero sum
        vec![&env, 4_i128, -4_i128],
        // over the party cap
        {
            let mut over_cap: Vec<i128> = Vec::new(&env);
            for _ in 0..(MAX_EMERGENCY_ALLOCATION_PARTIES + 1) {
                over_cap.push_back(1_i128);
            }
            over_cap
        },
        // weight sum overflows i128
        vec![&env, i128::MAX, 1_i128],
    ];

    for weights in malformed.iter() {
        assert_eq!(
            escrow.try_emergency_pause_allocation(&100_i128, weights),
            Err(Ok(Error::InvalidAllocationWeights)),
            "weights {weights:?} must map onto the single typed error"
        );
    }
}

/// The guards must be total: no malformed vector may reach the ledger. The
/// full ledger snapshot (instance, persistent and temporary entries, plus the
/// live-until TTLs) is byte-identical before and after the three rejected
/// calls, and no `epalloc` event is recorded.
#[test]
fn test_allocation_rejects_malformed_weights_without_touching_the_ledger() {
    let env = test_env();
    env.mock_all_auths();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let empty: Vec<i128> = Vec::new(&env);

    let before = env.to_ledger_snapshot();
    let rejected = [
        escrow.try_emergency_pause_allocation(&100_i128, &empty),
        escrow.try_emergency_pause_allocation(&100_i128, &vec![&env, 0_i128, 0_i128]),
        escrow.try_emergency_pause_allocation(&100_i128, &vec![&env, 3_i128, -1_i128]),
    ];
    let after = env.to_ledger_snapshot();

    for result in rejected.iter() {
        assert_eq!(*result, Err(Ok(Error::InvalidAllocationWeights)));
    }
    assert_eq!(
        before, after,
        "a rejected weight vector must not mutate any ledger entry"
    );
    assert_eq!(epalloc_event_count(&env), 0);
}

/// The guards are pure reads, so they must not poison the endpoint: a valid
/// call in the same env, immediately after all three rejections, still
/// allocates and still conserves the total exactly.
#[test]
fn test_allocation_still_allocates_after_every_rejection() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let empty: Vec<i128> = Vec::new(&env);
    assert!(escrow
        .try_emergency_pause_allocation(&100_i128, &empty)
        .is_err());
    assert!(escrow
        .try_emergency_pause_allocation(&100_i128, &vec![&env, 0_i128])
        .is_err());
    assert!(escrow
        .try_emergency_pause_allocation(&100_i128, &vec![&env, 1_i128, -2_i128])
        .is_err());

    let out = escrow.emergency_pause_allocation(&100_i128, &vec![&env, 1_i128, 1_i128, 2_i128]);
    let sum: i128 = out.iter().sum();

    assert_eq!(out.len(), 3);
    assert_eq!(sum, 100, "the happy path must be unaffected by the guards");
    assert_eq!(epalloc_event_count(&env), 1);
}

/// The total is validated first, so a zero/negative total wins over malformed
/// weights and reports `InvalidAmount` — still a typed error, never a panic,
/// and never a division.
#[test]
fn test_allocation_validates_the_total_before_the_weight_vector() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let empty: Vec<i128> = Vec::new(&env);

    assert_eq!(
        escrow.try_emergency_pause_allocation(&0_i128, &empty),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        escrow.try_emergency_pause_allocation(&-1_i128, &vec![&env, 0_i128, 0_i128]),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        escrow.try_emergency_pause_allocation(&-1_i128, &vec![&env, 1_i128, -1_i128]),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(epalloc_event_count(&env), 0);
}

/// The guard boundary must sit exactly where the documentation says it does:
/// the smallest usable vector `[1]` and the largest legal one (exactly the
/// party cap) are accepted, and neither a zero total nor a zero weight is
/// needed for the happy path to work.
#[test]
fn test_allocation_accepts_the_boundary_weight_vectors() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // Smallest non-empty vector.
    let single = escrow.emergency_pause_allocation(&7_i128, &vec![&env, 1_i128]);
    assert_eq!(single.get(0).unwrap(), 7);

    // Largest legal vector: exactly the cap.
    let mut at_cap: Vec<i128> = Vec::new(&env);
    for _ in 0..MAX_EMERGENCY_ALLOCATION_PARTIES {
        at_cap.push_back(1_i128);
    }
    let wide = escrow.emergency_pause_allocation(&1_000_000_i128, &at_cap);
    let sum: i128 = wide.iter().sum();
    assert_eq!(wide.len(), MAX_EMERGENCY_ALLOCATION_PARTIES);
    assert_eq!(sum, 1_000_000);
}

#[test]
fn test_allocation_rejects_more_parties_than_the_cap() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let mut weights: Vec<i128> = Vec::new(&env);
    for _ in 0..(MAX_EMERGENCY_ALLOCATION_PARTIES + 1) {
        weights.push_back(1_i128);
    }

    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &weights),
        Err(Ok(Error::InvalidAllocationWeights))
    );
}

#[test]
fn test_allocation_accepts_exactly_the_cap() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let mut weights: Vec<i128> = Vec::new(&env);
    for _ in 0..MAX_EMERGENCY_ALLOCATION_PARTIES {
        weights.push_back(1_i128);
    }

    let total = 100_000_i128;
    let out = escrow.emergency_pause_allocation(&total, &weights);
    let sum: i128 = out.iter().sum();

    assert_eq!(out.len(), MAX_EMERGENCY_ALLOCATION_PARTIES);
    assert_eq!(sum, total);
}

#[test]
fn test_allocation_rejects_overflow_instead_of_wrapping() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // Weights sum cleanly to 3, but total × 2 overflows i128 — the weighted
    // product must error rather than wrap to a nonsensical allocation.
    let weights = vec![&env, 2_i128, 1_i128];
    assert_eq!(
        escrow.try_emergency_pause_allocation(&i128::MAX, &weights),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_allocation_rejects_a_weight_sum_that_overflows() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    // Summing the weights themselves overflows, which is a malformed weight
    // vector rather than an amount problem.
    let weights = vec![&env, i128::MAX, 1_i128];
    assert_eq!(
        escrow.try_emergency_pause_allocation(&100_i128, &weights),
        Err(Ok(Error::InvalidAllocationWeights))
    );
}

#[test]
fn test_allocation_emits_an_event_matching_the_returned_vector() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let weights = vec![&env, 1_i128, 2_i128, 3_i128];
    let out = escrow.emergency_pause_allocation(&600_i128, &weights);

    let topic: Val = symbol_short!("epalloc").into_val(&env);
    let mut found = false;

    for e in crate::all_event_tuples(&env).iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                found = true;
                let data = EmergencyPauseAllocationEvent::from_val(&env, &e.2);
                assert_eq!(data.total_amount, 600);
                assert_eq!(data.num_parties, 3);
                assert_eq!(data.allocations, out);

                let sum: i128 = data.allocations.iter().sum();
                assert_eq!(sum, data.total_amount);
            }
        }
    }

    assert!(found, "expected an epalloc event");
}

#[test]
fn test_allocation_emits_no_event_when_rejected() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);

    let _ = escrow.try_emergency_pause_allocation(&0_i128, &vec![&env, 1_i128]);

    let topic: Val = symbol_short!("epalloc").into_val(&env);
    for e in crate::all_event_tuples(&env).iter() {
        if let Some(t) = e.1.get(0) {
            assert_ne!(t.get_payload(), topic.get_payload());
        }
    }
}

#[test]
fn test_allocation_agrees_with_the_two_party_split_refund() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);

    escrow.emergency_pause(&client, &freelancer);

    // The multi-party allocator and the bps split refund must not disagree
    // about how the same money is divided.
    let total = 1_000_i128;
    let allocation = escrow.emergency_pause_claim_refund(&admin, &total, &6_000_u32, &4_000_u32);
    let precise = escrow.emergency_pause_allocation(&total, &vec![&env, 6_000_i128, 4_000_i128]);

    assert_eq!(precise.get(0).unwrap(), allocation.client_refund);
    assert_eq!(precise.get(1).unwrap(), allocation.freelancer_payout);
}

// ============================================================================
// emergency_pause_claim_refund — hardening (#532 auth, #533 checked math,
// #534 event, #535 storage footprint)
// ============================================================================

fn claim_refund_topic(env: &Env) -> Symbol {
    Symbol::new(env, "emergency_pause_claim_refund")
}

/// Every `emergency_pause_claim_refund` event from the last invocation, as
/// `(topic admin, payload)`.
fn claim_refund_events(env: &Env) -> std::vec::Vec<(Address, EmergencyPauseClaimRefundEvent)> {
    let topic = claim_refund_topic(env);
    let mut out = std::vec::Vec::new();
    for (_contract, topics, data) in crate::all_event_tuples(env).iter() {
        let is_claim = topics
            .get(0)
            .and_then(|t| Symbol::try_from_val(env, &t).ok())
            .is_some_and(|symbol| symbol == topic);
        if !is_claim {
            continue;
        }
        let admin = Address::try_from_val(env, &topics.get(1).unwrap()).unwrap();
        let event = EmergencyPauseClaimRefundEvent::from_val(env, data);
        out.push((admin, event));
    }
    out
}

/// The ledger keys `emergency_pause_claim_refund` reads, captured so a call
/// can be shown to have mutated none of them.
fn claim_state(
    env: &Env,
    escrow: &MilestoneEscrowClient<'_>,
) -> (Option<Address>, Option<Address>, Option<bool>, Option<bool>) {
    env.as_contract(&escrow.address, || {
        (
            env.storage().instance().get(&DataKey::Admin),
            env.storage().persistent().get(&DataKey::Admin),
            env.storage().instance().get(&DataKey::Ep),
            env.storage().instance().get(&DataKey::EpLk),
        )
    })
}

/// Assert that a rejected claim returned `expected`, published no claim
/// event and left every key it reads untouched.
fn assert_claim_rejected(
    env: &Env,
    escrow: &MilestoneEscrowClient<'_>,
    caller: &Address,
    total: i128,
    client_bps: u32,
    freelancer_bps: u32,
    expected: Error,
) {
    let before = claim_state(env, escrow);
    assert_eq!(
        escrow.try_emergency_pause_claim_refund(caller, &total, &client_bps, &freelancer_bps),
        Err(Ok(expected))
    );
    assert!(
        claim_refund_events(env).is_empty(),
        "a rejected claim ({expected:?}) published a claim event"
    );
    assert_eq!(
        claim_state(env, escrow),
        before,
        "a rejected claim ({expected:?}) mutated storage"
    );
}

// ── #532: caller authorisation and preconditions ────────────────────────────

#[test]
fn test_claim_refund_requires_the_admin_signature() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);
    let before = claim_state(&env, &escrow);

    // Drop every mocked auth: the stored admin is passed but never signs.
    env.set_auths(&[]);
    let result =
        escrow.try_emergency_pause_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32);

    assert!(
        matches!(result, Err(Err(_))),
        "an unsigned claim must abort at the host auth check"
    );
    assert!(claim_refund_events(&env).is_empty());
    assert_eq!(claim_state(&env, &escrow), before);
}

#[test]
fn test_claim_refund_records_the_admin_auth() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);

    escrow.emergency_pause_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32);

    assert!(
        env.auths().iter().any(|(addr, _)| *addr == admin),
        "the claim must require the admin's authorisation"
    );
}

#[test]
fn test_claim_refund_checks_authorisation_before_state() {
    let env = test_env();
    let (escrow, _admin, _client, _freelancer) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    // Contract is *not* paused: a stranger must see `Unauthorized`, never
    // learn the pause state through `NotPaused`.
    assert_claim_rejected(
        &env,
        &escrow,
        &attacker,
        1_000,
        5_000,
        5_000,
        Error::Unauthorized,
    );
}

#[test]
fn test_claim_refund_rejections_leave_state_untouched() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    assert_claim_rejected(&env, &escrow, &admin, 500, 5_000, 5_000, Error::NotPaused);

    escrow.emergency_pause(&client, &freelancer);

    let cases = [
        (&attacker, 1_000, 5_000, 5_000, Error::Unauthorized),
        (&admin, 0, 5_000, 5_000, Error::InvalidAmount),
        (&admin, -1, 5_000, 5_000, Error::InvalidAmount),
        (&admin, 1_000, 5_000, 4_000, Error::InvalidRatio),
        (&admin, 1_000, u32::MAX, 1, Error::InvalidRatio),
        (&admin, i128::MAX, 5_000, 5_000, Error::ArithmeticOverflow),
        (&admin, 100_001, 5_000, 5_000, Error::InsufficientBalance),
    ];
    for (caller, total, client_bps, freelancer_bps, expected) in cases {
        assert_claim_rejected(
            &env,
            &escrow,
            caller,
            total,
            client_bps,
            freelancer_bps,
            expected,
        );
    }
}

#[test]
fn test_claim_refund_is_rejected_while_the_pause_lock_is_held() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);

    env.as_contract(&escrow.address, || {
        env.storage().instance().set(&DataKey::EpLk, &true);
    });

    assert_claim_rejected(
        &env,
        &escrow,
        &admin,
        1_000,
        5_000,
        5_000,
        Error::EmergencyPauseInProgress,
    );
}

// ── #533: checked arithmetic ────────────────────────────────────────────────

#[test]
fn test_claim_refund_overflow_returns_a_typed_error() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);

    // `i128::MAX × bps` overflows for any non-zero client share.
    for (client_bps, freelancer_bps) in [(1_u32, 9_999_u32), (5_000, 5_000), (10_000, 0)] {
        assert_claim_rejected(
            &env,
            &escrow,
            &admin,
            i128::MAX,
            client_bps,
            freelancer_bps,
            Error::ArithmeticOverflow,
        );
    }
}

#[test]
fn test_claim_refund_max_total_without_overflow_hits_the_balance_guard() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);

    // A zero client share never multiplies into overflow, so `i128::MAX`
    // reaches the balance guard and is refused there instead of wrapping.
    assert_claim_rejected(
        &env,
        &escrow,
        &admin,
        i128::MAX,
        0,
        10_000,
        Error::InsufficientBalance,
    );
}

#[test]
fn test_claim_refund_accepts_the_full_contract_balance() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);

    let allocation =
        escrow.emergency_pause_claim_refund(&admin, &100_000_i128, &3_333_u32, &6_667_u32);
    assert_eq!(
        allocation.client_refund + allocation.freelancer_payout,
        100_000
    );

    let events = claim_refund_events(&env);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1.remaining_balance, 0);
}

// ── #534: structured event ──────────────────────────────────────────────────

#[test]
fn test_claim_refund_publishes_exactly_one_structured_event() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);
    env.ledger().set_timestamp(1_700_000_000);

    let allocation =
        escrow.emergency_pause_claim_refund(&admin, &1_000_i128, &6_000_u32, &4_000_u32);

    let events = claim_refund_events(&env);
    assert_eq!(events.len(), 1, "expected exactly one claim event");

    let (topic_admin, data) = &events[0];
    assert_eq!(*topic_admin, admin);
    assert_eq!(
        *data,
        EmergencyPauseClaimRefundEvent {
            claimant: client,
            refund_amount: allocation.client_refund,
            freelancer_payout: allocation.freelancer_payout,
            remaining_balance: 99_000,
            timestamp: 1_700_000_000,
        }
    );
    assert_eq!(data.refund_amount, 600);
    assert_eq!(data.freelancer_payout, 400);

    // The claim publishes only its own event, not the calculator's.
    let escrow_events = crate::all_event_tuples(&env)
        .iter()
        .filter(|(contract, _, _)| *contract == escrow.address)
        .count();
    assert_eq!(escrow_events, 1);
}

#[test]
fn test_claim_refund_publishes_no_event_on_an_uninitialised_contract() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = bare_contract(&env);
    let stranger = Address::generate(&env);

    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&stranger, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::NotInitialized))
    );
    assert!(claim_refund_events(&env).is_empty());
}

// ── #535: storage footprint ─────────────────────────────────────────────────

#[test]
fn test_claim_refund_does_not_read_the_persistent_admin_entry() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);

    // Remove the persistent Admin copy.  The instance copy stays, so if the
    // claim still succeeds it cannot have touched the persistent entry — its
    // footprint is the contract instance plus the token balance only.
    env.as_contract(&escrow.address, || {
        env.storage().persistent().remove(&DataKey::Admin);
    });

    let allocation =
        escrow.emergency_pause_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32);
    assert_eq!(allocation.client_refund, 500);
    assert_eq!(allocation.freelancer_payout, 500);
}

#[test]
fn test_claim_refund_success_writes_no_storage() {
    let env = test_env();
    let (escrow, admin, client, freelancer) = initialised_escrow(&env);
    escrow.emergency_pause(&client, &freelancer);
    let before = claim_state(&env, &escrow);

    escrow.emergency_pause_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32);
    escrow.emergency_pause_claim_refund(&admin, &1_000_i128, &5_000_u32, &5_000_u32);

    // Settling is read-only: repeated claims leave every key it reads as-is.
    assert_eq!(claim_state(&env, &escrow), before);
}
