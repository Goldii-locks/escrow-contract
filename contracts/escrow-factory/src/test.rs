#![cfg(test)]

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    token, vec,
    xdr::ToXdr,
    Address, Bytes, BytesN, Env, FromVal, TryIntoVal, Val,
};

use crate::{
    Error, EscrowCreatedEvent, EscrowFactory, EscrowFactoryClient, EscrowSettings, FundingMode,
    PendingWasmHash, TokenAllowedEvent, WasmHashActivatedEvent, WasmHashCancelledEvent,
    WasmHashProposedEvent, WASM_HASH_DELAY,
};

// Import the compiled milestone-escrow WASM so the test env can deploy real
// escrow instances through the factory.
#[allow(clippy::too_many_arguments)]
mod escrow {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/milestone_escrow.wasm");
}

fn zero_hashes(env: &Env, n: u32) -> soroban_sdk::Vec<BytesN<32>> {
    let mut v = soroban_sdk::Vec::new(env);
    for _ in 0..n {
        v.push_back(BytesN::from_array(env, &[0u8; 32]));
    }
    v
}

/// Topics and data of the most recent contract event.
fn last_event(env: &Env) -> (soroban_sdk::Vec<soroban_sdk::Val>, soroban_sdk::Val) {
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::{xdr, TryFromVal};

    let all = env.events().all();
    let event = all.events().last().expect("an event was emitted").clone();
    let xdr::ContractEventBody::V0(body) = event.body;
    let mut topics = soroban_sdk::Vec::new(env);
    for topic in body.topics.iter() {
        topics.push_back(Val::try_from_val(env, topic).unwrap());
    }
    (topics, Val::try_from_val(env, &body.data).unwrap())
}

/// All events from the current invocation carrying `topic` as topic 0.
fn events_with_topic(
    env: &Env,
    topic: &soroban_sdk::Symbol,
) -> soroban_sdk::Vec<(soroban_sdk::Vec<Val>, Val)> {
    use soroban_sdk::testutils::Events as _;
    use soroban_sdk::{xdr, TryFromVal};

    let mut matched = soroban_sdk::Vec::new(env);
    for event in env.events().all().events().iter() {
        let xdr::ContractEventBody::V0(body) = event.body.clone();
        let mut topics = soroban_sdk::Vec::new(env);
        for t in body.topics.iter() {
            topics.push_back(Val::try_from_val(env, t).unwrap());
        }
        if let Some(first) = topics.get(0) {
            let first: soroban_sdk::Symbol = first.try_into_val(env).unwrap();
            if first == *topic {
                matched.push_back((topics, Val::try_from_val(env, &body.data).unwrap()));
            }
        }
    }
    matched
}

fn default_settings() -> EscrowSettings {
    EscrowSettings {
        funding_mode: FundingMode::Upfront,
        dispute_timeout_seconds: 30 * 24 * 60 * 60,
        arbiter_fee_bps: 0,
    }
}

fn create_escrow_with_salt(
    env: &Env,
    factory: &EscrowFactoryClient<'_>,
    token: &Address,
    salt_byte: u8,
) -> Address {
    factory.create_escrow(
        &Address::generate(env),
        &Address::generate(env),
        &Address::generate(env),
        token,
        &604800u64,
        &vec![env, 1_000_i128],
        &zero_hashes(env, 1),
        &default_settings(),
        &BytesN::from_array(env, &[salt_byte; 32]),
    )
}

fn mint_token(env: &Env, recipient: &Address, amount: i128) -> Address {
    let admin = Address::generate(env);
    let tok = env.register_stellar_asset_contract_v2(admin).address();
    token::StellarAssetClient::new(env, &tok).mint(recipient, &amount);
    tok
}

// Returns (env, factory_id, admin, whitelisted_token, wasm_hash).
// Tests construct their own client from factory_id to avoid lifetime issues.
fn setup() -> (Env, Address, Address, Address, BytesN<32>) {
    let env = Env::default();
    env.mock_all_auths();
    // Uploading the escrow wasm writes more bytes than the test env's default
    // per-invocation limit allows.
    env.cost_estimate().disable_resource_limits();

    let admin = Address::generate(&env);
    let wasm_hash = env.deployer().upload_contract_wasm(escrow::WASM);

    let factory_id = env.register(EscrowFactory, (admin.clone(), wasm_hash.clone()));
    let factory = EscrowFactoryClient::new(&env, &factory_id);

    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    factory.allow_token(&admin, &token);

    (env, factory_id, admin, token, wasm_hash)
}

#[test]
fn initialize_and_getters() {
    let (env, fid, admin, token, wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    assert_eq!(factory.get_admin(), admin);
    assert_eq!(factory.get_wasm_hash(), wasm_hash);
    let tokens = factory.get_allowed_tokens();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens.get(0).unwrap(), token);
}

#[test]
#[should_panic]
fn deploy_without_constructor_args_fails() {
    let env = Env::default();
    env.register(EscrowFactory, ());
}

#[test]
fn allow_token_idempotent() {
    let (env, fid, admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    factory.allow_token(&admin, &token);
    assert_eq!(factory.get_allowed_tokens().len(), 1);
}

#[test]
fn allow_token_emits_tokenallow_event_with_token_payload() {
    let (env, fid, admin, _token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let new_token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();

    factory.allow_token(&admin, &new_token);

    let (topics, data) = last_event(&env);
    assert_eq!(topics.len(), 1);
    let topic: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("tokallow"));
    assert_eq!(
        TokenAllowedEvent::from_val(&env, &data),
        TokenAllowedEvent {
            token: new_token.clone(),
        }
    );

    // Repeating the call is a no-op and publishes no second event. The test
    // environment exposes events from the current invocation only.
    use soroban_sdk::testutils::Events as _;
    factory.allow_token(&admin, &new_token);
    assert_eq!(env.events().all().events().len(), 0);
    assert_eq!(factory.get_allowed_tokens().len(), 2);
}

#[test]
fn propose_wasm_hash_verifies_bytes_and_records_pending_proposal() {
    let (env, fid, admin, _token, old_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let wasm_bytes = Bytes::from_slice(&env, escrow::WASM);
    let new_hash = env.deployer().upload_contract_wasm(wasm_bytes.clone());
    let started_at = env.ledger().timestamp();
    let effective_at = started_at + WASM_HASH_DELAY;

    assert_eq!(
        factory.try_propose_wasm_hash(&Address::generate(&env), &new_hash, &wasm_bytes),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        factory.try_propose_wasm_hash(&admin, &BytesN::from_array(&env, &[99u8; 32]), &wasm_bytes),
        Err(Ok(Error::InvalidWasmHash))
    );

    factory.propose_wasm_hash(&admin, &new_hash, &wasm_bytes);

    let (topics, data) = last_event(&env);
    assert_eq!(topics.len(), 1);
    let topic: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("whashprp"));
    assert_eq!(
        WasmHashProposedEvent::from_val(&env, &data),
        WasmHashProposedEvent {
            hash: new_hash.clone(),
            effective_at,
        }
    );
    assert_eq!(
        factory.get_pending_wasm_hash(),
        PendingWasmHash {
            hash: new_hash,
            effective_at,
        }
    );
    // The proposal has not taken effect yet.
    assert_eq!(factory.get_wasm_hash(), old_hash);
}

#[test]
fn pending_wasm_hash_is_unused_before_delay_and_applied_after_it() {
    let (env, fid, admin, token, old_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let wasm_bytes = Bytes::from_slice(&env, escrow::WASM);
    let new_hash = env.deployer().upload_contract_wasm(wasm_bytes.clone());
    let effective_at = env.ledger().timestamp() + WASM_HASH_DELAY;
    factory.propose_wasm_hash(&admin, &new_hash, &wasm_bytes);

    create_escrow_with_salt(&env, &factory, &token, 1);
    let (_, data) = last_event(&env);
    assert_eq!(
        EscrowCreatedEvent::from_val(&env, &data).wasm_hash,
        old_hash
    );
    assert_eq!(factory.get_wasm_hash(), old_hash);

    env.ledger().set_timestamp(effective_at);
    create_escrow_with_salt(&env, &factory, &token, 2);
    let (_, data) = last_event(&env);
    assert_eq!(
        EscrowCreatedEvent::from_val(&env, &data).wasm_hash,
        new_hash
    );

    // Event access is per invocation, so read both events from this call
    // before making any further contract calls.
    let activations = events_with_topic(&env, &symbol_short!("whashact"));
    assert_eq!(activations.len(), 1);
    assert_eq!(
        WasmHashActivatedEvent::from_val(&env, &activations.get(0).unwrap().1),
        WasmHashActivatedEvent {
            hash: new_hash.clone(),
            effective_at,
        }
    );

    assert_eq!(factory.get_wasm_hash(), new_hash);
    assert_eq!(
        factory.try_get_pending_wasm_hash(),
        Err(Ok(Error::NoPendingWasmHash))
    );
}

#[test]
fn cancelling_wasm_hash_proposal_restores_original_behaviour() {
    let (env, fid, admin, token, old_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let wasm_bytes = Bytes::from_slice(&env, escrow::WASM);
    let new_hash = env.deployer().upload_contract_wasm(wasm_bytes.clone());
    let effective_at = env.ledger().timestamp() + WASM_HASH_DELAY;
    factory.propose_wasm_hash(&admin, &new_hash, &wasm_bytes);

    assert_eq!(
        factory.try_cancel_wasm_hash_proposal(&Address::generate(&env)),
        Err(Ok(Error::Unauthorized))
    );
    factory.cancel_wasm_hash_proposal(&admin);

    let (topics, data) = last_event(&env);
    assert_eq!(topics.len(), 1);
    let topic: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("whashcxl"));
    assert_eq!(
        WasmHashCancelledEvent::from_val(&env, &data),
        WasmHashCancelledEvent {
            hash: new_hash,
            effective_at,
        }
    );
    assert_eq!(
        factory.try_get_pending_wasm_hash(),
        Err(Ok(Error::NoPendingWasmHash))
    );
    assert_eq!(factory.get_wasm_hash(), old_hash);

    // Even after the original delay has passed, the cancelled hash is unused.
    env.ledger().set_timestamp(effective_at);
    create_escrow_with_salt(&env, &factory, &token, 3);
    let (_, data) = last_event(&env);
    assert_eq!(
        EscrowCreatedEvent::from_val(&env, &data).wasm_hash,
        old_hash
    );
}

#[test]
fn create_escrow_unlisted_token_rejected() {
    let (env, fid, _admin, _token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    assert_eq!(
        factory.try_create_escrow(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env), // not whitelisted
            &604800u64,
            &vec![&env, 1_000_i128],
            &zero_hashes(&env, 1),
            &default_settings(),
            &BytesN::from_array(&env, &[1u8; 32]),
        ),
        Err(Ok(Error::TokenNotAllowed))
    );
}

#[test]
fn create_escrow_empty_milestones_rejected() {
    let (env, fid, _admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    assert_eq!(
        factory.try_create_escrow(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &token,
            &604800u64,
            &soroban_sdk::Vec::new(&env),
            &soroban_sdk::Vec::new(&env),
            &default_settings(),
            &BytesN::from_array(&env, &[2u8; 32]),
        ),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn create_escrow_rejects_out_of_range_dispute_timeout() {
    let (env, fid, _admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    for timeout in [
        crate::MIN_DISPUTE_TIMEOUT_SECONDS - 1,
        crate::MAX_DISPUTE_TIMEOUT_SECONDS + 1,
    ] {
        assert_eq!(
            factory.try_create_escrow(
                &Address::generate(&env),
                &Address::generate(&env),
                &Address::generate(&env),
                &token,
                &604800u64,
                &vec![&env, 1_000_i128],
                &zero_hashes(&env, 1),
                &EscrowSettings {
                    dispute_timeout_seconds: timeout,
                    ..default_settings()
                },
                &BytesN::from_array(&env, &[3u8; 32]),
            ),
            Err(Ok(Error::InvalidDisputeTimeout))
        );
    }
}

/// Deploy two escrows for the same client; verify they are independent by
/// running fund -> deliver -> approve on both.
#[test]
fn two_escrows_independent_lifecycle() {
    let (env, fid, admin, _default_tok, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);

    let tok = mint_token(&env, &client, 2_000);
    factory.allow_token(&admin, &tok);

    let amounts = vec![&env, 1_000_i128];
    let hashes = zero_hashes(&env, 1);

    let escrow1 = factory.create_escrow(
        &client,
        &freelancer,
        &arbiter,
        &tok,
        &604800u64,
        &amounts,
        &hashes,
        &default_settings(),
        &BytesN::from_array(&env, &[10u8; 32]),
    );
    let escrow2 = factory.create_escrow(
        &client,
        &freelancer,
        &arbiter,
        &tok,
        &604800u64,
        &amounts,
        &hashes,
        &default_settings(),
        &BytesN::from_array(&env, &[20u8; 32]),
    );

    assert_ne!(escrow1, escrow2);

    let e1 = escrow::Client::new(&env, &escrow1);
    let e2 = escrow::Client::new(&env, &escrow2);

    e1.accept_job(&freelancer);
    e1.accept_arbiter(&arbiter);
    e2.accept_job(&freelancer);
    e2.accept_arbiter(&arbiter);

    e1.fund(&client);
    e1.mark_delivered(&freelancer, &0u32);
    e1.approve_milestone(&client, &0u32);

    e2.fund(&client);
    e2.mark_delivered(&freelancer, &0u32);
    e2.approve_milestone(&client, &0u32);

    assert_eq!(token::Client::new(&env, &tok).balance(&freelancer), 2_000);
}

#[test]
fn created_event_carries_party_topics_and_deployment_details() {
    let (env, fid, admin, _default_tok, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let tok = mint_token(&env, &client, 1_000);
    factory.allow_token(&admin, &tok);

    let escrow = factory.create_escrow(
        &client,
        &freelancer,
        &arbiter,
        &tok,
        &604800u64,
        &vec![&env, 1_000_i128],
        &zero_hashes(&env, 1),
        &default_settings(),
        &BytesN::from_array(&env, &[3u8; 32]),
    );

    let (topics, data) = last_event(&env);
    assert_eq!(topics.len(), 4);
    let topic: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("created"));
    let topic_client: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
    let topic_freelancer: Address = topics.get(2).unwrap().try_into_val(&env).unwrap();
    let topic_arbiter: Address = topics.get(3).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic_client, client);
    assert_eq!(topic_freelancer, freelancer);
    assert_eq!(topic_arbiter, arbiter);

    let event = EscrowCreatedEvent::from_val(&env, &data);
    assert_eq!(event.escrow, escrow);
    assert_eq!(event.token, tok);
    assert_eq!(event.total_amount, 1_000);
    assert_eq!(event.milestone_count, 1);
    assert_eq!(event.auto_release_seconds, 604800);
    assert_eq!(event.settings, default_settings());
    assert_eq!(event.wasm_hash, factory.get_wasm_hash());
}

#[test]
fn disallow_token_removes_allowed_token_and_blocks_new_escrows() {
    let (env, fid, admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);

    // Only the stored admin may change the allowlist.
    assert_eq!(
        factory.try_disallow_token(&Address::generate(&env), &token),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(factory.get_allowed_tokens().len(), 1);

    // Removing a token that was never allowed is a defined no-op.
    factory.disallow_token(&admin, &Address::generate(&env));
    assert_eq!(factory.get_allowed_tokens().len(), 1);

    factory.disallow_token(&admin, &token);
    let (topics, _) = last_event(&env);
    let topic: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("disallowd"));
    let event_token: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
    assert_eq!(event_token, token);
    assert_eq!(factory.get_allowed_tokens().len(), 0);

    // A second removal is also a no-op.
    factory.disallow_token(&admin, &token);
    assert_eq!(factory.get_allowed_tokens().len(), 0);

    assert_eq!(
        factory.try_create_escrow(
            &Address::generate(&env),
            &Address::generate(&env),
            &Address::generate(&env),
            &token,
            &604800u64,
            &vec![&env, 1_000_i128],
            &zero_hashes(&env, 1),
            &default_settings(),
            &BytesN::from_array(&env, &[5u8; 32]),
        ),
        Err(Ok(Error::TokenNotAllowed))
    );
}

#[test]
fn salt_is_bound_to_client_and_address_is_predictable() {
    let (env, fid, _admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let salt = BytesN::from_array(&env, &[7u8; 32]);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let amounts = vec![&env, 1_000_i128];
    let hashes = zero_hashes(&env, 1);

    let mut addresses = soroban_sdk::Vec::new(&env);
    for _ in 0..2 {
        let client = Address::generate(&env);
        let predicted = factory.escrow_address(&client, &salt);
        let deployed = factory.create_escrow(
            &client,
            &freelancer,
            &arbiter,
            &token,
            &604800u64,
            &amounts,
            &hashes,
            &default_settings(),
            &salt,
        );
        assert_eq!(deployed, predicted);
        assert_eq!(
            escrow::Client::new(&env, &deployed).get_job().client,
            client
        );
        addresses.push_back(deployed);
    }
    assert_ne!(addresses.get(0).unwrap(), addresses.get(1).unwrap());
}

/// Creating an escrow needs only the client's signature: the platform admin
/// is named as the escrow admin without having to co-sign every job.
#[test]
fn create_escrow_needs_only_the_client_signature() {
    use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
    use soroban_sdk::IntoVal;

    let (env, fid, _admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let amounts = vec![&env, 1_000_i128];
    let hashes = zero_hashes(&env, 1);
    let salt = BytesN::from_array(&env, &[4u8; 32]);
    let args = (
        client.clone(),
        freelancer.clone(),
        arbiter.clone(),
        token.clone(),
        604800u64,
        amounts.clone(),
        hashes.clone(),
        default_settings(),
        salt.clone(),
    )
        .into_val(&env);

    let escrow = factory
        .mock_auths(&[MockAuth {
            address: &client,
            invoke: &MockAuthInvoke {
                contract: &fid,
                fn_name: "create_escrow",
                args,
                sub_invokes: &[],
            },
        }])
        .create_escrow(
            &client,
            &freelancer,
            &arbiter,
            &token,
            &604800u64,
            &amounts,
            &hashes,
            &default_settings(),
            &salt,
        );

    let job = escrow::Client::new(&env, &escrow).get_job();
    assert_eq!(job.client, client);
}

#[test]
fn pause_blocks_new_escrows_but_not_existing_ones() {
    let (env, fid, admin, _default_tok, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let tok = mint_token(&env, &client, 1_000);
    factory.allow_token(&admin, &tok);

    let amounts = vec![&env, 1_000_i128];
    let hashes = zero_hashes(&env, 1);
    let existing = factory.create_escrow(
        &client,
        &freelancer,
        &arbiter,
        &tok,
        &604800u64,
        &amounts,
        &hashes,
        &default_settings(),
        &BytesN::from_array(&env, &[30u8; 32]),
    );

    assert!(!factory.is_paused());
    factory.pause(&admin);
    assert!(factory.is_paused());

    // A deployed escrow keeps working while the factory is paused.
    escrow::Client::new(&env, &existing).accept_job(&freelancer);

    assert_eq!(
        factory.try_create_escrow(
            &client,
            &freelancer,
            &arbiter,
            &tok,
            &604800u64,
            &amounts,
            &hashes,
            &default_settings(),
            &BytesN::from_array(&env, &[31u8; 32]),
        ),
        Err(Ok(Error::Paused))
    );

    factory.unpause(&admin);
    assert!(!factory.is_paused());
    let created = factory.create_escrow(
        &client,
        &freelancer,
        &arbiter,
        &tok,
        &604800u64,
        &amounts,
        &hashes,
        &default_settings(),
        &BytesN::from_array(&env, &[31u8; 32]),
    );
    assert_ne!(created, existing);
}

#[test]
fn pause_and_unpause_emit_events_and_are_idempotent() {
    use soroban_sdk::testutils::Events as _;

    let (env, fid, admin, _token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);

    factory.pause(&admin);
    let (topics, _) = last_event(&env);
    assert_eq!(topics.len(), 1);
    let topic: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("paused"));

    factory.pause(&admin);
    assert!(env.events().all().events().is_empty());

    factory.unpause(&admin);
    let (topics, _) = last_event(&env);
    assert_eq!(topics.len(), 1);
    let topic: soroban_sdk::Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("unpaused"));

    factory.unpause(&admin);
    assert!(env.events().all().events().is_empty());
}

#[test]
fn only_admin_can_toggle_pause() {
    let (env, fid, admin, _token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let intruder = Address::generate(&env);

    assert_eq!(factory.try_pause(&intruder), Err(Ok(Error::Unauthorized)));
    assert!(!factory.is_paused());

    factory.pause(&admin);
    assert!(factory.is_paused());
    assert_eq!(factory.try_unpause(&intruder), Err(Ok(Error::Unauthorized)));
    assert!(factory.is_paused());
}

/// The factory passes these types straight into the escrow constructor. They
/// come from `escrow-types` now, so this test fails to compile if either
/// contract stops using the shared definition; the XDR assertion additionally
/// guards the runtime wire format.
#[test]
fn shared_settings_xdr_matches_milestone_escrow() {
    let env = Env::default();

    for mode in [FundingMode::Upfront, FundingMode::PerMilestone] {
        let factory_settings = EscrowSettings {
            funding_mode: mode,
            dispute_timeout_seconds: 30 * 24 * 60 * 60,
            arbiter_fee_bps: 25,
        };
        let escrow_settings: milestone_escrow::EscrowSettings = EscrowSettings {
            funding_mode: mode,
            dispute_timeout_seconds: 30 * 24 * 60 * 60,
            arbiter_fee_bps: 25,
        };
        assert_eq!(factory_settings.to_xdr(&env), escrow_settings.to_xdr(&env));
    }

    let escrow_mode: milestone_escrow::FundingMode = FundingMode::PerMilestone;
    assert_eq!(
        FundingMode::PerMilestone.to_xdr(&env),
        escrow_mode.to_xdr(&env)
    );
}
