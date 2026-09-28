#![cfg(test)]
//! Regression suite for issue #479: guarantee that `get_reputation` performs
//! no state mutation.
//!
//! Each test takes a full ledger snapshot (`Env::to_ledger_snapshot`)
//! immediately before calling `get_reputation` and another immediately after,
//! and asserts the two are identical.  The snapshot covers every ledger entry
//! of every contract registered on the `Env` — instance, persistent and
//! temporary storage, including each entry's live-until ledger — plus the
//! ledger info, so it is a byte-for-byte check of the whole ledger rather
//! than of the keys we expect to be read.  Any `.set(`, `.remove(` or
//! `.extend_ttl(` added to `get_reputation` fails these tests, and the
//! metering test additionally pins that the call writes zero entries.

use crate::test::setup_funded_escrow;
use crate::{DataKey, Error, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::testutils::{EnvTestConfig, Ledger as _};
use soroban_sdk::{testutils::Address as _, vec, Address, Env};

/// Snapshot the ledger, call `get_reputation`, snapshot again, and assert
/// the ledger is unchanged and no event was published.
fn assert_read_only(env: &Env, escrow: &MilestoneEscrowClient<'_>, address: &Address) -> u32 {
    let before = env.to_ledger_snapshot();
    let value = escrow.get_reputation(address);
    assert_eq!(
        crate::all_event_tuples(env).len(),
        0,
        "get_reputation publishes no event"
    );
    let after = env.to_ledger_snapshot();
    assert_eq!(before, after, "get_reputation mutated the ledger");
    value
}

#[test]
fn get_reputation_absent_entry_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, arbiter, admin, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    for address in [client, freelancer, arbiter, admin, Address::generate(&env)] {
        assert_eq!(assert_read_only(&env, &escrow, &address), 0);
    }
}

#[test]
fn get_reputation_populated_entry_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    assert_eq!(assert_read_only(&env, &escrow, &client), 1);
    assert_eq!(assert_read_only(&env, &escrow, &freelancer), 1);
}

/// Boundary value: a counter at `u32::MAX` is returned as-is, read-only.
#[test]
fn get_reputation_max_counter_does_not_mutate_ledger() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, _, _, _, _, contract_id, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    let holder = Address::generate(&env);
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Reputation(holder.clone()), &u32::MAX);
    });

    assert_eq!(assert_read_only(&env, &escrow, &holder), u32::MAX);
}

/// Advance the ledger so any TTL bump would move a live-until ledger, then
/// confirm the snapshot is still unchanged.
#[test]
fn get_reputation_does_not_extend_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);
    env.ledger().with_mut(|li| li.sequence_number += 100);

    assert_eq!(assert_read_only(&env, &escrow, &client), 1);
}

/// Repeated reads return a stable value and never accumulate state.
#[test]
fn repeated_get_reputation_calls_are_stable() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, freelancer, _, _, _, _, escrow) =
        setup_funded_escrow(&env, vec![&env, 1_000_i128]);
    escrow.mark_delivered(&freelancer, &0);
    escrow.approve_milestone(&client, &0);

    let before = env.to_ledger_snapshot();
    for _ in 0..5 {
        assert_eq!(escrow.get_reputation(&client), 1);
    }
    assert_eq!(before, env.to_ledger_snapshot());
}

/// The error path is read-only too.
#[test]
fn get_reputation_uninitialized_does_not_mutate_ledger() {
    let env = Env::default();
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(&env, &contract_id);

    let before = env.to_ledger_snapshot();
    assert_eq!(
        escrow.try_get_reputation(&Address::generate(&env)),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(before, env.to_ledger_snapshot());
}

/// The host's own metering agrees: the call writes zero ledger entries.
#[test]
fn get_reputation_metered_write_entries_is_zero() {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    let (client, _, _, _, _, _, escrow) = setup_funded_escrow(&env, vec![&env, 1_000_i128]);

    escrow.get_reputation(&client);
    let resources = env.cost_estimate().resources();
    assert_eq!(resources.write_entries, 0, "measured {resources:?}");
}
