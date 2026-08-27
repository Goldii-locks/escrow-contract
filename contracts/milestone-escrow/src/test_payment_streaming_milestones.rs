#![cfg(test)]
//! Dedicated workflow matrix for the `payment_streaming_milestones` module.
//!
//! This file covers two things end to end:
//!
//! 1. **Dual-signature validation** on `payment_streaming_consent` — both the
//!    client and the freelancer recorded on the job must sign, and any
//!    single-signature attempt reverts at the host level before touching
//!    state.
//! 2. **The ratio matrix** for the unauthenticated calculator
//!    `payment_streaming_milestones` — boundaries, rounding, conservation,
//!    overflow and event emission.

use super::*;
use soroban_sdk::{
    testutils::Address as _, testutils::EnvTestConfig, testutils::Events, testutils::MockAuth,
    testutils::MockAuthInvoke, vec, Address, Env, FromVal, IntoVal, Symbol, Val,
};

// ── fixtures ────────────────────────────────────────────────────────────────

/// Snapshot capture is disabled so this suite does not write a JSON file per
/// test into `test_snapshots/`; none of these assertions read snapshots.
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

/// A bare contract with no job metadata — enough for the calculator, which
/// does not read storage.
fn calculator_only(env: &Env) -> MilestoneEscrowClient<'_> {
    let contract_id = env.register(MilestoneEscrow, ());
    MilestoneEscrowClient::new(env, &contract_id)
}

struct Parties {
    client: Address,
    freelancer: Address,
    contract_id: Address,
}

/// A fully initialised escrow, so `payment_streaming_consent` has a
/// client/freelancer pair to collect signatures from.
fn initialised_escrow(env: &Env) -> (MilestoneEscrowClient<'_>, Parties) {
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

    (
        escrow,
        Parties {
            client: client_addr,
            freelancer: freelancer_addr,
            contract_id,
        },
    )
}

const ARGS_TOTAL: i128 = 1_000;
const ARGS_NUM: i128 = 1;
const ARGS_DEN: i128 = 2;

/// The invoke descriptor the signature tests authorise. Expanded into a local
/// binding in each test so the borrow outlives the `MockAuth` entries that
/// point at it — a helper function returning `MockAuth` cannot, because the
/// descriptor would be a temporary owned by that function.
macro_rules! consent_invoke {
    ($env:expr, $contract:expr) => {
        MockAuthInvoke {
            contract: $contract,
            fn_name: "payment_streaming_consent",
            args: (ARGS_TOTAL, ARGS_NUM, ARGS_DEN).into_val($env),
            sub_invokes: &[],
        }
    };
}

// ============================================================================
// Dual-signature validation hooks
// ============================================================================

#[test]
fn test_consent_succeeds_when_both_parties_sign() {
    let env = test_env();
    let (escrow, p) = initialised_escrow(&env);

    let invoke = consent_invoke!(&env, &p.contract_id);

    let split = escrow
        .mock_auths(&[
            MockAuth {
                address: &p.client,
                invoke: &invoke,
            },
            MockAuth {
                address: &p.freelancer,
                invoke: &invoke,
            },
        ])
        .payment_streaming_consent(&ARGS_TOTAL, &ARGS_NUM, &ARGS_DEN);

    assert_eq!(split.first, 500);
    assert_eq!(split.second, 500);
}

#[test]
fn test_consent_reverts_when_only_client_signs() {
    let env = test_env();
    let (escrow, p) = initialised_escrow(&env);

    let invoke = consent_invoke!(&env, &p.contract_id);

    let result = escrow
        .mock_auths(&[MockAuth {
            address: &p.client,
            invoke: &invoke,
        }])
        .try_payment_streaming_consent(&ARGS_TOTAL, &ARGS_NUM, &ARGS_DEN);

    // The freelancer's `require_auth()` panics at the host level, so this is
    // the host-error arm rather than a contract `Error` variant.
    assert!(
        matches!(result, Err(Err(_))),
        "a client-only signature must revert the invocation"
    );
}

#[test]
fn test_consent_reverts_when_only_freelancer_signs() {
    let env = test_env();
    let (escrow, p) = initialised_escrow(&env);

    let invoke = consent_invoke!(&env, &p.contract_id);

    let result = escrow
        .mock_auths(&[MockAuth {
            address: &p.freelancer,
            invoke: &invoke,
        }])
        .try_payment_streaming_consent(&ARGS_TOTAL, &ARGS_NUM, &ARGS_DEN);

    assert!(
        matches!(result, Err(Err(_))),
        "a freelancer-only signature must revert the invocation"
    );
}

#[test]
fn test_consent_reverts_with_no_signatures_at_all() {
    let env = test_env();
    let (escrow, _p) = initialised_escrow(&env);

    let result =
        escrow
            .mock_auths(&[])
            .try_payment_streaming_consent(&ARGS_TOTAL, &ARGS_NUM, &ARGS_DEN);

    assert!(matches!(result, Err(Err(_))));
}

#[test]
fn test_consent_reverts_when_a_third_party_signs_instead() {
    let env = test_env();
    let (escrow, p) = initialised_escrow(&env);

    // An impostor cannot substitute their own signature: the addresses are
    // read from job metadata, never from arguments.
    let impostor = Address::generate(&env);
    let invoke = consent_invoke!(&env, &p.contract_id);

    let result = escrow
        .mock_auths(&[MockAuth {
            address: &impostor,
            invoke: &invoke,
        }])
        .try_payment_streaming_consent(&ARGS_TOTAL, &ARGS_NUM, &ARGS_DEN);

    assert!(matches!(result, Err(Err(_))));
}

#[test]
fn test_consent_reverts_when_arbiter_substitutes_for_freelancer() {
    let env = test_env();
    let (escrow, p) = initialised_escrow(&env);

    let arbiter = Address::generate(&env);
    let invoke = consent_invoke!(&env, &p.contract_id);

    let result = escrow
        .mock_auths(&[
            MockAuth {
                address: &p.client,
                invoke: &invoke,
            },
            MockAuth {
                address: &arbiter,
                invoke: &invoke,
            },
        ])
        .try_payment_streaming_consent(&ARGS_TOTAL, &ARGS_NUM, &ARGS_DEN);

    assert!(matches!(result, Err(Err(_))));
}

#[test]
fn test_consent_requires_initialised_job() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // No job metadata means no signer pair to collect signatures from.
    assert_eq!(
        escrow.try_payment_streaming_consent(&ARGS_TOTAL, &ARGS_NUM, &ARGS_DEN),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_consent_validates_amount_after_collecting_signatures() {
    let env = test_env();
    let (escrow, _p) = initialised_escrow(&env);

    // Signatures are collected first, so with both present the ratio rules
    // still apply and surface as ordinary contract errors.
    assert_eq!(
        escrow.try_payment_streaming_consent(&0_i128, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        escrow.try_payment_streaming_consent(&-1_i128, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_consent_validates_ratio_after_collecting_signatures() {
    let env = test_env();
    let (escrow, _p) = initialised_escrow(&env);

    assert_eq!(
        escrow.try_payment_streaming_consent(&100_i128, &1_i128, &0_i128),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        escrow.try_payment_streaming_consent(&100_i128, &1_i128, &-2_i128),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        escrow.try_payment_streaming_consent(&100_i128, &-1_i128, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        escrow.try_payment_streaming_consent(&100_i128, &3_i128, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn test_consent_emits_event_naming_both_signers() {
    let env = test_env();
    let (escrow, p) = initialised_escrow(&env);

    let split = escrow.payment_streaming_consent(&1_000_i128, &300_i128, &600_i128);
    assert_eq!(split.first, 500);
    assert_eq!(split.second, 500);

    let topic: Val = symbol_short!("p_strcns").into_val(&env);
    let mut found = false;

    for e in env.events().all().iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                found = true;
                let data = PaymentStreamingConsentEvent::from_val(&env, &e.2);
                assert_eq!(data.client, p.client);
                assert_eq!(data.freelancer, p.freelancer);
                assert_eq!(data.total_amount, 1_000);
                assert_eq!(data.streamed_payout, 500);
                assert_eq!(data.client_refund, 500);
            }
        }
    }

    assert!(found, "expected a p_strcns event naming both signers");
}

#[test]
fn test_consent_does_not_emit_event_when_validation_fails() {
    let env = test_env();
    let (escrow, _p) = initialised_escrow(&env);

    let _ = escrow.try_payment_streaming_consent(&0_i128, &1_i128, &2_i128);

    let topic: Val = symbol_short!("p_strcns").into_val(&env);
    for e in env.events().all().iter() {
        if let Some(t) = e.1.get(0) {
            assert_ne!(
                t.get_payload(),
                topic.get_payload(),
                "a rejected consent call must not publish a settlement event"
            );
        }
    }
}

#[test]
fn test_consent_matches_the_unauthenticated_calculator() {
    let env = test_env();
    let (escrow, _p) = initialised_escrow(&env);

    // The consent gate changes who may call, never the arithmetic.
    for (total, num, den) in [
        (1_000_i128, 1_i128, 3_i128),
        (7_i128, 1_i128, 2_i128),
        (999_i128, 333_i128, 1_000_i128),
    ] {
        let gated = escrow.payment_streaming_consent(&total, &num, &den);
        let plain = escrow.payment_streaming_milestones(&total, &num, &den);
        assert_eq!(gated.first, plain.first);
        assert_eq!(gated.second, plain.second);
    }
}

// ============================================================================
// Ratio workflow matrix — the unauthenticated calculator
// ============================================================================

#[test]
fn test_streaming_matrix_zero_numerator_streams_nothing() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    let split = escrow.payment_streaming_milestones(&1_000_i128, &0_i128, &4_i128);
    assert_eq!(split.first, 0);
    assert_eq!(split.second, 1_000);
}

#[test]
fn test_streaming_matrix_full_numerator_streams_everything() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    let split = escrow.payment_streaming_milestones(&1_000_i128, &4_i128, &4_i128);
    assert_eq!(split.first, 1_000);
    assert_eq!(split.second, 0);
}

#[test]
fn test_streaming_matrix_quarter_thirds_and_halves() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    let quarter = escrow.payment_streaming_milestones(&1_000_i128, &1_i128, &4_i128);
    assert_eq!((quarter.first, quarter.second), (250, 750));

    let half = escrow.payment_streaming_milestones(&1_000_i128, &1_i128, &2_i128);
    assert_eq!((half.first, half.second), (500, 500));

    // 1000/3 = 333.33… → rounds to nearest, client takes the remainder.
    let third = escrow.payment_streaming_milestones(&1_000_i128, &1_i128, &3_i128);
    assert_eq!((third.first, third.second), (333, 667));
}

#[test]
fn test_streaming_matrix_rounds_to_nearest_not_down() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // 7 × 1/2 = 3.5 → nearest is 4, floor would have given 3.
    let split = escrow.payment_streaming_milestones(&7_i128, &1_i128, &2_i128);
    assert_eq!(split.first, 4);
    assert_eq!(split.second, 3);

    // 5 × 1/2 = 2.5 → nearest is 3.
    let split = escrow.payment_streaming_milestones(&5_i128, &1_i128, &2_i128);
    assert_eq!(split.first, 3);
    assert_eq!(split.second, 2);
}

#[test]
fn test_streaming_matrix_conserves_the_total_across_the_ratio_range() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // The two halves must reconstruct the total exactly for every ratio —
    // no value may be lost or created by rounding.
    for total in [1_i128, 2, 3, 97, 1_000, 999_983] {
        for numerator in 0..=17_i128 {
            let split = escrow.payment_streaming_milestones(&total, &numerator, &17_i128);
            assert_eq!(
                split.first + split.second,
                total,
                "total {total} numerator {numerator} did not reconstruct"
            );
            assert!(split.first >= 0 && split.second >= 0);
        }
    }
}

#[test]
fn test_streaming_matrix_is_monotonic_in_the_numerator() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // Streaming more of the window can never pay the freelancer less.
    let mut previous = -1_i128;
    for numerator in 0..=20_i128 {
        let split = escrow.payment_streaming_milestones(&1_000_i128, &numerator, &20_i128);
        assert!(
            split.first >= previous,
            "payout dropped at numerator {numerator}"
        );
        previous = split.first;
    }
}

#[test]
fn test_streaming_matrix_smallest_indivisible_total() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // A single stroop cannot be halved; nearest-rounding awards it to the
    // streamed side and the client refund goes to zero.
    let split = escrow.payment_streaming_milestones(&1_i128, &1_i128, &2_i128);
    assert_eq!(split.first + split.second, 1);
    assert_eq!(split.first, 1);
    assert_eq!(split.second, 0);
}

#[test]
fn test_streaming_matrix_equivalent_ratios_agree() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // 1/2, 2/4 and 500/1000 all describe the same point in the stream.
    let a = escrow.payment_streaming_milestones(&1_000_i128, &1_i128, &2_i128);
    let b = escrow.payment_streaming_milestones(&1_000_i128, &2_i128, &4_i128);
    let c = escrow.payment_streaming_milestones(&1_000_i128, &500_i128, &1_000_i128);

    assert_eq!((a.first, a.second), (b.first, b.second));
    assert_eq!((b.first, b.second), (c.first, c.second));
}

#[test]
fn test_streaming_matrix_large_total_without_overflow() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    let large = 1_000_000_000_000_000_000_i128;
    let split = escrow.payment_streaming_milestones(&large, &1_i128, &2_i128);
    assert_eq!(split.first, large / 2);
    assert_eq!(split.first + split.second, large);
}

#[test]
fn test_streaming_matrix_overflow_is_rejected_not_wrapped() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // total × numerator overflows i128 — must surface as an error rather
    // than silently wrapping to a bogus payout.
    assert_eq!(
        escrow.try_payment_streaming_milestones(&i128::MAX, &i128::MAX, &i128::MAX),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_streaming_matrix_rejects_non_positive_totals() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    assert_eq!(
        escrow.try_payment_streaming_milestones(&0_i128, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        escrow.try_payment_streaming_milestones(&-1_i128, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        escrow.try_payment_streaming_milestones(&i128::MIN, &1_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_streaming_matrix_rejects_invalid_denominators() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    assert_eq!(
        escrow.try_payment_streaming_milestones(&100_i128, &1_i128, &0_i128),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        escrow.try_payment_streaming_milestones(&100_i128, &1_i128, &-1_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn test_streaming_matrix_rejects_numerators_outside_the_window() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    assert_eq!(
        escrow.try_payment_streaming_milestones(&100_i128, &-1_i128, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );
    assert_eq!(
        escrow.try_payment_streaming_milestones(&100_i128, &3_i128, &2_i128),
        Err(Ok(Error::InvalidRatio))
    );
}

#[test]
fn test_streaming_matrix_amount_error_precedes_ratio_error() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // Both inputs are bad; the amount guard runs first, so the caller sees
    // the amount error. Pinning the order keeps the API predictable.
    assert_eq!(
        escrow.try_payment_streaming_milestones(&0_i128, &5_i128, &2_i128),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_streaming_matrix_emits_event_with_the_computed_split() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    let split = escrow.payment_streaming_milestones(&1_000_i128, &1_i128, &4_i128);
    assert_eq!(split.first, 250);

    let topic: Val = Symbol::new(&env, "p_stream").into_val(&env);
    let mut found = false;

    for e in env.events().all().iter() {
        if let Some(t) = e.1.get(0) {
            if t.get_payload() == topic.get_payload() {
                found = true;
                let data = PaymentStreamingEvent::from_val(&env, &e.2);
                assert_eq!(data.total_amount, 1_000);
                assert_eq!(data.numerator, 1);
                assert_eq!(data.denominator, 4);
                assert_eq!(data.streamed_payout, 250);
                assert_eq!(data.client_refund, 750);
            }
        }
    }

    assert!(found, "expected a p_stream event");
}

#[test]
fn test_streaming_matrix_repeated_calls_are_pure() {
    let env = test_env();
    env.mock_all_auths();
    let escrow = calculator_only(&env);

    // The calculator holds no state, so the same query always answers the
    // same way no matter what ran in between.
    let first = escrow.payment_streaming_milestones(&997_i128, &13_i128, &29_i128);
    let _ = escrow.payment_streaming_milestones(&5_i128, &1_i128, &2_i128);
    let again = escrow.payment_streaming_milestones(&997_i128, &13_i128, &29_i128);

    assert_eq!((first.first, first.second), (again.first, again.second));
}
