#![cfg(test)]
//! #672: a partially released milestone must remain auto-releasable so the
//! freelancer cannot be stranded by a silent client.

use crate::testutils::{EscrowFixture, DEFAULT_AUTO_RELEASE_SECONDS};
use crate::{Error, MilestoneStatus};
use soroban_sdk::testutils::Ledger;

#[test]
fn auto_release_pays_remainder_of_partially_released_milestone() {
    let env = soroban_sdk::Env::default();
    let fx = EscrowFixture::new(&env)
        .milestones(&[100])
        .funded()
        .build();
    fx.escrow.mock_all_auths().mark_delivered(&fx.freelancer, &0);
    fx.escrow
        .mock_all_auths()
        .approve_partial(&fx.client, &0, &40);
    assert_eq!(
        fx.escrow.get_milestone(&0).status,
        MilestoneStatus::PartiallyReleased
    );

    // The deadline is still measured from the original delivery time.
    env.ledger()
        .with_mut(|l| l.timestamp += DEFAULT_AUTO_RELEASE_SECONDS + 1);
    fx.escrow
        .mock_all_auths()
        .claim_auto_release(&fx.freelancer, &0);

    let milestone = fx.escrow.get_milestone(&0);
    assert_eq!(milestone.status, MilestoneStatus::Released);
    assert_eq!(milestone.released_amount, 100);
    let token = fx.token_client();
    assert_eq!(token.balance(&fx.freelancer), 100);
    assert_eq!(token.balance(&fx.contract_id), 0);

    // A second claim is still rejected.
    assert_eq!(
        fx.escrow
            .mock_all_auths()
            .try_claim_auto_release(&fx.freelancer, &0),
        Err(Ok(Error::InvalidStatus))
    );
}
