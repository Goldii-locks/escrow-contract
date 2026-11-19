#![cfg(test)]
//! Read-only settlement-token accessor (issue #680).

use super::*;
use crate::testutils::EscrowFixture;

#[test]
fn token_getter_returns_the_job_settlement_token() {
    let env = Env::default();
    env.mock_all_auths();
    let fx = EscrowFixture::new(&env).funded().build();

    assert_eq!(fx.escrow.token(), fx.token);
}
