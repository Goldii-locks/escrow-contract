#![no_std]
// Reentrancy guards throughout this contract use the `let result = (|| { … })();`
// idiom: set the lock, run the body, release the lock, then return the body's
// result. clippy::redundant_closure_call suggests flattening these to a plain
// block, which is NOT equivalent -- inside a block, `?` and `return` exit the
// enclosing function and skip the lock release below, leaving the escrow
// permanently locked. The closure boundary is what makes the release
// unconditional, so the lint is disabled here deliberately.
#![allow(clippy::redundant_closure_call)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

use soroban_sdk::{contract, contractmeta, Address, BytesN, Env, Vec};

pub mod math;
mod types;
mod events;
mod storage;
mod guards;
mod lifecycle;
mod disputes;
mod cancel;
mod admin;
mod multisig;
mod yield_;
mod tax;
mod fees;
mod emergency;

pub use events::*;
pub use types::*;

contractmeta!(key = "name", val = "milesto-milestone-escrow");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "source",
    val = "https://github.com/Goldii-locks/escrow-contract"
);

#[contract]
pub struct MilestoneEscrow;

#[cfg(test)]
extern crate std;

/// Test-only bridge from the SDK 28 event representation back to the
/// `(contract, topics, data)` tuple shape that `Events::all()` returned before
/// v25.
///
/// In SDK 28 `Events::all()` yields a `ContractEvents` struct whose only
/// accessor is `events() -> &[xdr::ContractEvent]`, where topics and data are
/// XDR `ScVal`s rather than host `Val`s. Converting each `ScVal` back through
/// `TryFromVal<Env, ScVal> for Val` reproduces exactly the values the old API
/// handed out, so the assertions built on top of this keep comparing what they
/// always compared -- including `Val::get_payload()` identity, which is only
/// equivalent to value equality because every topic in this suite is a
/// `symbol_short!` (packed inline, never an object handle).
#[cfg(test)]
pub(crate) fn all_event_tuples(
    env: &Env,
) -> std::vec::Vec<(Address, Vec<soroban_sdk::Val>, soroban_sdk::Val)> {
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::{xdr, TryFromVal, Val};

    env.events()
        .all()
        .events()
        .iter()
        .map(|e| {
            let xdr::ContractEventBody::V0(body) = &e.body;

            let contract_id = e
                .contract_id
                .clone()
                .expect("contract event without a contract id");
            let address = Address::try_from_val(
                env,
                &xdr::ScVal::Address(xdr::ScAddress::Contract(contract_id)),
            )
            .expect("contract id is not a valid address");

            let mut topics = Vec::new(env);
            for topic in body.topics.iter() {
                topics.push_back(
                    Val::try_from_val(env, topic).expect("event topic is not convertible to Val"),
                );
            }

            let data =
                Val::try_from_val(env, &body.data).expect("event data is not convertible to Val");

            (address, topics, data)
        })
        .collect()
}

/// Placeholder spec hashes (#767) for tests that do not care about them.
#[cfg(test)]
pub(crate) fn spec_hashes_for(amounts: &Vec<i128>) -> Vec<BytesN<32>> {
    test_spec_hashes(amounts.env(), amounts)
}

#[cfg(test)]
pub(crate) fn test_spec_hashes(env: &Env, amounts: &Vec<i128>) -> Vec<BytesN<32>> {
    let mut hashes = Vec::new(env);
    for _ in 0..amounts.len() {
        hashes.push_back(BytesN::from_array(env, &[0; 32]));
    }
    hashes
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests as test;
#[cfg(test)]
pub(crate) use tests::testutils;

