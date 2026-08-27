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
    testutils::Address as _, testutils::EnvTestConfig, testutils::Events, vec, Address, Env,
    FromVal, IntoVal, Val,
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
fn initialised_escrow(env: &Env) -> (MilestoneEscrowClient<'_>, Address) {
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

    (escrow, admin_addr)
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
        escrow.try_emergency_pause(&stranger),
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
    let (escrow, _admin) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    assert_eq!(
        escrow.try_emergency_pause(&attacker),
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
    let (escrow, admin) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    escrow.emergency_pause(&admin);

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
    let (escrow, admin) = initialised_escrow(&env);

    assert!(!escrow.is_emergency_paused());
    escrow.emergency_pause(&admin);
    assert!(escrow.is_emergency_paused());
}

#[test]
fn test_pause_twice_is_rejected_as_already_paused() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

    // A redundant pause must not read as fresh action during an incident.
    assert_eq!(
        escrow.try_emergency_pause(&admin),
        Err(Ok(Error::AlreadyPaused))
    );
    assert!(escrow.is_emergency_paused());
}

#[test]
fn test_unpause_without_a_pause_is_rejected_as_not_paused() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    assert_eq!(
        escrow.try_emergency_unpause(&admin),
        Err(Ok(Error::NotPaused))
    );
    assert!(!escrow.is_emergency_paused());
}

#[test]
fn test_unpause_twice_is_rejected_as_not_paused() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);
    escrow.emergency_unpause(&admin);

    assert_eq!(
        escrow.try_emergency_unpause(&admin),
        Err(Ok(Error::NotPaused))
    );
}

#[test]
fn test_pause_unpause_cycle_is_repeatable() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    for _ in 0..3 {
        escrow.emergency_pause(&admin);
        assert!(escrow.is_emergency_paused());
        escrow.emergency_unpause(&admin);
        assert!(!escrow.is_emergency_paused());
    }
}

#[test]
fn test_pause_releases_its_transition_lock() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    // If the lock leaked, the following unpause would fail with
    // EmergencyPauseInProgress instead of succeeding.
    escrow.emergency_pause(&admin);
    escrow.emergency_unpause(&admin);
    escrow.emergency_pause(&admin);

    assert!(escrow.is_emergency_paused());
}

#[test]
fn test_pause_blocks_guarded_endpoints() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

    // `fund` is guarded by ensure_not_paused, so the freeze is observable
    // through the normal escrow flow, not just the status getter.
    let job = escrow.get_job();
    assert_eq!(escrow.try_fund(&job.client), Err(Ok(Error::Paused)));
}

#[test]
fn test_unpause_restores_guarded_endpoints() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);
    escrow.emergency_unpause(&admin);

    let job = escrow.get_job();
    assert_ne!(escrow.try_fund(&job.client), Err(Ok(Error::Paused)));
}

#[test]
fn test_pause_emits_a_state_change_event() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

    let topic: Val = symbol_short!("empause").into_val(&env);
    let mut found = false;

    for e in env.events().all().iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                found = true;
                let data = EmergencyPausedEvent::from_val(&env, &e.2);
                assert_eq!(data.admin, admin);
            }
        }
    }

    assert!(found, "expected an empause event naming the admin");
}

#[test]
fn test_rejected_transitions_emit_no_event() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    // Neither an unauthorised pause nor an unpause of a running contract may
    // publish a state-change event — on either topic.
    let _ = escrow.try_emergency_pause(&attacker);
    let _ = escrow.try_emergency_unpause(&admin);

    let paused_topic: Val = symbol_short!("empause").into_val(&env);
    let unpaused_topic: Val = symbol_short!("emunpause").into_val(&env);

    for e in env.events().all().iter() {
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
    let (escrow, admin) = initialised_escrow(&env);

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
    let (escrow, admin) = initialised_escrow(&env);
    let attacker = Address::generate(&env);

    escrow.emergency_pause(&admin);

    assert_eq!(
        escrow.try_emergency_pause_claim_refund(&attacker, &1_000_i128, &5_000_u32, &5_000_u32),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_claim_refund_succeeds_while_paused() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

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
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

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
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

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
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

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

    for e in env.events().all().iter() {
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
    for e in env.events().all().iter() {
        if let Some(t) = e.1.get(0) {
            assert_ne!(t.get_payload(), topic.get_payload());
        }
    }
}

#[test]
fn test_allocation_agrees_with_the_two_party_split_refund() {
    let env = test_env();
    let (escrow, admin) = initialised_escrow(&env);

    escrow.emergency_pause(&admin);

    // The multi-party allocator and the bps split refund must not disagree
    // about how the same money is divided.
    let total = 1_000_i128;
    let allocation = escrow.emergency_pause_claim_refund(&admin, &total, &6_000_u32, &4_000_u32);
    let precise = escrow.emergency_pause_allocation(&total, &vec![&env, 6_000_i128, 4_000_i128]);

    assert_eq!(precise.get(0).unwrap(), allocation.client_refund);
    assert_eq!(precise.get(1).unwrap(), allocation.freelancer_payout);
}
