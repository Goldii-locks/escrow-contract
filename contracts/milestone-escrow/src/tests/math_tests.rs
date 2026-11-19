#![cfg(test)]
//! Direct tests for `math.rs`, written so that each mutant
//! `cargo mutants --file contracts/milestone-escrow/src/math.rs` generates
//! changes an asserted value or error (issue #1022).

use crate::math::{
    allocate_platform_fee_shares, allocate_weights, split_bps, split_ratio_nearest, Rounding,
};
use crate::Error;
use soroban_sdk::{vec, Env};

#[test]
fn split_bps_rounds_the_first_leg() {
    assert_eq!(split_bps(1_001, 5_000, Rounding::Floor), Ok((500, 501)));
    assert_eq!(split_bps(1_001, 5_000, Rounding::HalfUp), Ok((501, 500)));
    assert_eq!(split_bps(1, 5_000, Rounding::Floor), Ok((0, 1)));
    assert_eq!(split_bps(1, 5_000, Rounding::HalfUp), Ok((1, 0)));
    assert_eq!(split_bps(1, 5_000, Rounding::Nearest), Ok((1, 0)));
}

#[test]
fn split_bps_accepts_the_bounds() {
    assert_eq!(split_bps(0, 5_000, Rounding::Floor), Ok((0, 0)));
    assert_eq!(split_bps(1_000, 0, Rounding::Floor), Ok((0, 1_000)));
    assert_eq!(split_bps(1_000, 10_000, Rounding::Floor), Ok((1_000, 0)));
    assert_eq!(split_bps(1_000, 10_000, Rounding::HalfUp), Ok((1_000, 0)));
}

#[test]
fn split_bps_rejects_invalid_input() {
    assert_eq!(
        split_bps(-1, 10_000, Rounding::Floor),
        Err(Error::InvalidAmount)
    );
    assert_eq!(
        split_bps(100, 10_001, Rounding::Floor),
        Err(Error::InvalidRatio)
    );
}

#[test]
fn split_ratio_nearest_rounds_half_up() {
    assert_eq!(split_ratio_nearest(10, 1, 3), Ok((3, 7)));
    assert_eq!(split_ratio_nearest(1, 1, 2), Ok((1, 0)));
}

#[test]
fn split_ratio_nearest_accepts_the_bounds() {
    assert_eq!(split_ratio_nearest(0, 1, 2), Ok((0, 0)));
    assert_eq!(split_ratio_nearest(10, 0, 2), Ok((0, 10)));
    assert_eq!(split_ratio_nearest(10, 2, 2), Ok((10, 0)));
}

#[test]
fn split_ratio_nearest_rejects_each_invalid_input_alone() {
    assert_eq!(split_ratio_nearest(-1, 1, 2), Err(Error::InvalidRatio));
    assert_eq!(split_ratio_nearest(10, -1, 2), Err(Error::InvalidRatio));
    assert_eq!(split_ratio_nearest(10, 0, 0), Err(Error::InvalidRatio));
    assert_eq!(split_ratio_nearest(10, 3, 2), Err(Error::InvalidRatio));
}

#[test]
fn allocate_weights_gives_leftover_units_to_the_largest_remainders() {
    let env = Env::default();
    // Remainders 1 and 2: the leftover unit goes to the second weight.
    assert_eq!(
        allocate_weights(&env, 5, &vec![&env, 2, 1]),
        Ok(vec![&env, 3, 2])
    );
    // Equal remainders: the earliest index wins the tie.
    assert_eq!(
        allocate_weights(&env, 10, &vec![&env, 1, 1, 1]),
        Ok(vec![&env, 4, 3, 3])
    );
    // A zero weight is allowed and receives nothing.
    assert_eq!(
        allocate_weights(&env, 10, &vec![&env, 0, 1]),
        Ok(vec![&env, 0, 10])
    );
}

#[test]
fn allocate_weights_rejects_invalid_input() {
    let env = Env::default();
    assert_eq!(
        allocate_weights(&env, 0, &vec![&env, 1]),
        Err(Error::InvalidAmount)
    );
    assert_eq!(
        allocate_weights(&env, -5, &vec![&env, 1, 1]),
        Err(Error::InvalidAmount)
    );
    assert_eq!(
        allocate_weights(&env, 10, &vec![&env]),
        Err(Error::InvalidRatio)
    );
    assert_eq!(
        allocate_weights(&env, 10, &vec![&env, -1, 2]),
        Err(Error::InvalidRatio)
    );
    assert_eq!(
        allocate_weights(&env, 10, &vec![&env, 0, 0]),
        Err(Error::InvalidRatio)
    );
}

#[test]
fn allocate_platform_fee_shares_splits_exactly() {
    assert_eq!(
        allocate_platform_fee_shares(10_000, 2_000, 7_000, 1_000),
        Ok([2_000, 7_000, 1_000])
    );
    assert_eq!(
        allocate_platform_fee_shares(0, 2_000, 7_000, 1_000),
        Ok([0, 0, 0])
    );
}

#[test]
fn allocate_platform_fee_shares_gives_leftover_units_by_remainder() {
    // Remainders 6_666, 6_666, 6_668: the first leftover unit goes to the
    // largest, the second to the earliest of the tied pair.
    assert_eq!(
        allocate_platform_fee_shares(2, 3_333, 3_333, 3_334),
        Ok([1, 0, 1])
    );
}

#[test]
fn allocate_platform_fee_shares_rejects_invalid_input() {
    assert_eq!(
        allocate_platform_fee_shares(-1, 2_000, 7_000, 1_000),
        Err(Error::InvalidAmount)
    );
    assert_eq!(
        allocate_platform_fee_shares(i128::MIN, 2_000, 7_000, 1_000),
        Err(Error::ArithmeticOverflow)
    );
    // Shares above 100% allocate more than the total.
    assert_eq!(
        allocate_platform_fee_shares(100, 6_000, 6_000, 0),
        Err(Error::ArithmeticOverflow)
    );
}
