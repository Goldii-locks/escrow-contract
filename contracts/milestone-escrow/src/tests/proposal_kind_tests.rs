#![cfg(test)]
//! Unified `Proposal` record (issue #996): `get_proposal` reveals the action
//! behind a proposal ID, the payload hash commits to the ID and action, and
//! approvals cannot be applied to a different ID or payload.

use crate::test::setup_funded_escrow;
use crate::{
    DataKey, Error, MilestoneEscrowClient, PendingAdminTransfer, PendingMultiSigConfig, Proposal,
    ProposalKind, PROPOSAL_TTL_SECONDS,
};
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger};
use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{vec, Address, BytesN, Env, IntoVal, Map, Symbol, Val, Vec};

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000_000);
    env
}

struct Setup<'a> {
    env: &'a Env,
    admin: Address,
    signers: Vec<Address>,
    new_set: Vec<Address>,
    escrow: MilestoneEscrowClient<'a>,
}

fn setup(env: &Env) -> Setup<'_> {
    let (_, _, _, admin, _, _, escrow) = setup_funded_escrow(env, vec![env, 1_000_i128]);
    let signers = vec![
        env,
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ];
    escrow.multisig_approval_init(&admin, &signers, &2);
    let new_set = vec![env, Address::generate(env), Address::generate(env)];
    Setup {
        env,
        admin,
        signers,
        new_set,
        escrow,
    }
}

/// Independent re-implementation of the documented commitments.
fn expected_signers_hash(env: &Env, signers: &Vec<Address>) -> BytesN<32> {
    env.crypto().sha256(&signers.clone().to_xdr(env)).to_bytes()
}

fn expected_payload_hash(env: &Env, id: u32, kind: &ProposalKind) -> BytesN<32> {
    let payload = (Symbol::new(env, "proposal"), id, kind.clone()).to_xdr(env);
    env.crypto().sha256(&payload).to_bytes()
}

fn put(s: &Setup<'_>, record: &Proposal) {
    s.env.as_contract(&s.escrow.address, || {
        s.env
            .storage()
            .persistent()
            .set(&DataKey::Proposal(record.proposal_id), record);
    });
}

fn approve_twice(s: &Setup<'_>, id: u32) {
    s.escrow.multisig_approve(&s.signers.get(0).unwrap(), &id);
    s.escrow.multisig_approve(&s.signers.get(1).unwrap(), &id);
}

// ── inspection ───────────────────────────────────────────────────────────────

#[test]
fn get_proposal_identifies_an_admin_transfer() {
    let env = test_env();
    let s = setup(&env);
    let new_admin = Address::generate(&env);
    let id = s.escrow.propose_admin_transfer(&s.admin, &new_admin);

    let p = s.escrow.get_proposal(&id).unwrap();
    assert_eq!(p.proposal_id, id);
    assert_eq!(p.kind, ProposalKind::AdminTransfer(new_admin));
    assert_eq!(p.created_at, env.ledger().timestamp());
    assert_eq!(p.approvals_bitmap, 0);
    assert_eq!(p.payload_hash, expected_payload_hash(&env, id, &p.kind));

    s.escrow.multisig_approve(&s.signers.get(1).unwrap(), &id);
    assert_eq!(s.escrow.get_proposal(&id).unwrap().approvals_bitmap, 0b010);
    assert_eq!(s.escrow.get_proposal(&(id + 1)), None);
}

#[test]
fn get_proposal_reveals_the_multisig_config_being_approved() {
    let env = test_env();
    let s = setup(&env);
    let id = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);

    let p = s.escrow.get_proposal(&id).unwrap();
    // No longer an AdminTransfer to the current admin.
    assert_eq!(
        p.kind,
        ProposalKind::MultisigConfig(expected_signers_hash(&env, &s.new_set), 2)
    );
    assert_eq!(p.payload_hash, expected_payload_hash(&env, id, &p.kind));
    // The signer list behind the committed hash is the pending config's.
    let pending = s.escrow.get_pending_multisig_config().unwrap();
    assert_eq!(pending.signers, s.new_set);
    assert_eq!(
        ProposalKind::MultisigConfig(
            expected_signers_hash(&env, &pending.signers),
            pending.threshold
        ),
        p.kind
    );
}

#[test]
fn payload_hash_is_deterministic_and_binds_id_and_data() {
    let env = test_env();
    let s = setup(&env);

    // Same action under two IDs: different hashes (ID is committed).
    let target = Address::generate(&env);
    let first = s.escrow.propose_admin_transfer(&s.admin, &target);
    s.escrow.cancel_admin_transfer_proposal(&s.admin);
    let second = s.escrow.propose_admin_transfer(&s.admin, &target);
    let a = expected_payload_hash(&env, first, &ProposalKind::AdminTransfer(target.clone()));
    let b = s.escrow.get_proposal(&second).unwrap().payload_hash;
    assert_ne!(a, b);
    // Recomputing gives the identical value every time.
    let kind = ProposalKind::AdminTransfer(target);
    assert_eq!(b, expected_payload_hash(&env, second, &kind));
    assert_eq!(b, expected_payload_hash(&env, second, &kind));

    // Threshold and signer order change the committed payload.
    s.escrow.cancel_admin_transfer_proposal(&s.admin);
    let reversed = vec![&env, s.new_set.get(1).unwrap(), s.new_set.get(0).unwrap()];
    assert_ne!(
        expected_signers_hash(&env, &s.new_set),
        expected_signers_hash(&env, &reversed)
    );
    let c1 = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &1);
    let h1 = s.escrow.get_proposal(&c1).unwrap();
    s.escrow.cancel_multisig_config_proposal(&s.admin);
    let c2 = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);
    let h2 = s.escrow.get_proposal(&c2).unwrap();
    s.escrow.cancel_multisig_config_proposal(&s.admin);
    let c3 = s.escrow.propose_multisig_config(&s.admin, &reversed, &2);
    let h3 = s.escrow.get_proposal(&c3).unwrap();
    assert_ne!(h1.kind, h2.kind);
    assert_ne!(h2.kind, h3.kind);
    assert_ne!(h2.payload_hash, h3.payload_hash);
}

// ── approvals cannot be reused ───────────────────────────────────────────────

#[test]
fn approvals_do_not_carry_across_ids_even_for_the_same_payload() {
    let env = test_env();
    let s = setup(&env);
    let target = Address::generate(&env);

    let first = s.escrow.propose_admin_transfer(&s.admin, &target);
    approve_twice(&s, first);
    s.escrow.cancel_admin_transfer_proposal(&s.admin);

    let second = s.escrow.propose_admin_transfer(&s.admin, &target);
    assert_ne!(first, second);
    assert_eq!(s.escrow.get_proposal(&second).unwrap().approvals_bitmap, 0);
    assert_eq!(
        s.escrow.try_execute_admin_transfer(),
        Err(Ok(Error::MultiSigThresholdNotMet))
    );
}

#[test]
fn tampered_payload_is_rejected_on_approve() {
    let env = test_env();
    let s = setup(&env);
    let id = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));

    // Swap the action but keep the original hash.
    let mut p = s.escrow.get_proposal(&id).unwrap();
    p.kind = ProposalKind::AdminTransfer(Address::generate(&env));
    put(&s, &p);
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.signers.get(0).unwrap(), &id),
        Err(Ok(Error::ProposalPayloadMismatch))
    );

    // A hash lifted from another ID does not validate either.
    let other = s.escrow.get_proposal(&id).unwrap();
    let mut q = other.clone();
    q.payload_hash = expected_payload_hash(&env, id + 7, &q.kind);
    put(&s, &q);
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.signers.get(0).unwrap(), &id),
        Err(Ok(Error::ProposalPayloadMismatch))
    );
}

#[test]
fn execution_requires_record_to_match_pending_state() {
    let env = test_env();
    let s = setup(&env);

    // Config record whose committed threshold differs from the pending one.
    let id = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);
    approve_twice(&s, id);
    env.as_contract(&s.escrow.address, || {
        let mut pending: PendingMultiSigConfig = env
            .storage()
            .persistent()
            .get(&DataKey::PendingMultiSigConfig)
            .unwrap();
        pending.threshold = 1;
        env.storage()
            .persistent()
            .set(&DataKey::PendingMultiSigConfig, &pending);
    });
    assert_eq!(
        s.escrow.try_execute_multisig_config(),
        Err(Ok(Error::ProposalPayloadMismatch))
    );

    // Same for a swapped signer list.
    env.as_contract(&s.escrow.address, || {
        let pending = PendingMultiSigConfig {
            signers: vec![&env, Address::generate(&env)],
            threshold: 2,
            proposal_id: id,
        };
        env.storage()
            .persistent()
            .set(&DataKey::PendingMultiSigConfig, &pending);
    });
    assert_eq!(
        s.escrow.try_execute_multisig_config(),
        Err(Ok(Error::ProposalPayloadMismatch))
    );
}

#[test]
fn a_lock_cannot_execute_a_proposal_of_the_other_kind() {
    let env = test_env();
    let s = setup(&env);

    // The config proposal's approvals must not satisfy an admin-transfer lock
    // that points at its ID.
    let config = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);
    approve_twice(&s, config);
    env.as_contract(&s.escrow.address, || {
        env.storage().persistent().set(
            &DataKey::PendingAdminTransfer,
            &PendingAdminTransfer {
                new_admin: s.admin.clone(),
                proposal_id: config,
            },
        );
    });
    assert_eq!(
        s.escrow.try_execute_admin_transfer(),
        Err(Ok(Error::ProposalPayloadMismatch))
    );
}

// ── lifecycle ────────────────────────────────────────────────────────────────

#[test]
fn proposals_disappear_when_final_and_expiry_still_applies() {
    let env = test_env();
    let s = setup(&env);
    let created = env.ledger().timestamp();

    let config = s.escrow.propose_multisig_config(&s.admin, &s.new_set, &2);
    approve_twice(&s, config);
    s.escrow.execute_multisig_config();
    assert_eq!(s.escrow.get_proposal(&config), None);

    let t = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    assert!(s.escrow.get_proposal(&t).is_some());
    env.ledger()
        .set_timestamp(created + PROPOSAL_TTL_SECONDS + 1);
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.new_set.get(0).unwrap(), &t),
        Err(Ok(Error::ProposalExpired))
    );
    s.escrow
        .clear_expired_proposal(&Address::generate(&env), &t);
    assert_eq!(s.escrow.get_proposal(&t), None);

    let t2 = s
        .escrow
        .propose_admin_transfer(&s.admin, &Address::generate(&env));
    s.escrow.cancel_admin_transfer_proposal(&s.admin);
    assert_eq!(s.escrow.get_proposal(&t2), None);
}

#[test]
fn legacy_pre_996_proposal_is_inert_and_cancel_removes_it() {
    let env = test_env();
    let s = setup(&env);
    let new_admin = Address::generate(&env);

    // A record in the pre-#996 `AdminTransferProposal` shape under the old key.
    env.as_contract(&s.escrow.address, || {
        let mut legacy: Map<Symbol, Val> = Map::new(&env);
        legacy.set(Symbol::new(&env, "proposal_id"), 1u32.into_val(&env));
        legacy.set(Symbol::new(&env, "new_admin"), new_admin.into_val(&env));
        legacy.set(Symbol::new(&env, "created_at"), 1u64.into_val(&env));
        legacy.set(Symbol::new(&env, "approvals_bitmap"), 0u32.into_val(&env));
        env.storage()
            .persistent()
            .set(&DataKey::AdminProposal(1), &legacy);
        env.storage().persistent().set(
            &DataKey::PendingAdminTransfer,
            &PendingAdminTransfer {
                new_admin: new_admin.clone(),
                proposal_id: 1,
            },
        );
    });

    assert_eq!(s.escrow.get_proposal(&1), None);
    assert_eq!(
        s.escrow
            .try_multisig_approve(&s.signers.get(0).unwrap(), &1),
        Err(Ok(Error::NoPendingAdminTransfer))
    );
    assert_eq!(
        s.escrow.try_execute_admin_transfer(),
        Err(Ok(Error::InvalidStatus))
    );

    s.escrow.cancel_admin_transfer_proposal(&s.admin);
    env.as_contract(&s.escrow.address, || {
        assert!(!env.storage().persistent().has(&DataKey::AdminProposal(1)));
        assert!(!env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer));
    });
}
