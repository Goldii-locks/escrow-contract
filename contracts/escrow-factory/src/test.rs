#![cfg(test)]

use soroban_sdk::{
    symbol_short, testutils::Address as _, token, vec, Address, BytesN, Env, FromVal, TryIntoVal,
};

use crate::{
    Error, EscrowCreatedEvent, EscrowFactory, EscrowFactoryClient, EscrowSettings, FundingMode,
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
    use soroban_sdk::{xdr, TryFromVal, Val};

    let all = env.events().all();
    let event = all.events().last().expect("an event was emitted").clone();
    let xdr::ContractEventBody::V0(body) = event.body;
    let mut topics = soroban_sdk::Vec::new(env);
    for topic in body.topics.iter() {
        topics.push_back(Val::try_from_val(env, topic).unwrap());
    }
    (topics, Val::try_from_val(env, &body.data).unwrap())
}

fn default_settings() -> EscrowSettings {
    EscrowSettings {
        funding_mode: FundingMode::Upfront,
        dispute_timeout_seconds: 30 * 24 * 60 * 60,
        arbiter_fee_bps: 0,
    }
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

// ── Issue #935: factory coverage ─────────────────────────────────────────────

/// Full escrow wasm plus a `v2_only` entrypoint (see
/// `milestone-escrow/testdata/escrow_v2`): a second, distinguishable wasm for
/// the factory to deploy after `set_wasm_hash`.
const ESCROW_V2_WASM: &[u8] = include_bytes!("../../milestone-escrow/testdata/escrow_v2.wasm");

/// The single factory call `fn_name(args)` a signer authorizes.
fn factory_call<'a>(
    factory: &'a Address,
    fn_name: &'a str,
    args: soroban_sdk::Vec<soroban_sdk::Val>,
) -> soroban_sdk::testutils::MockAuthInvoke<'a> {
    soroban_sdk::testutils::MockAuthInvoke {
        contract: factory,
        fn_name,
        args,
        sub_invokes: &[],
    }
}

#[test]
fn allow_token_rejects_a_non_admin_even_when_it_signs() {
    use soroban_sdk::IntoVal;

    let (env, fid, _admin, _token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let stranger = Address::generate(&env);
    let new_token = Address::generate(&env);

    let args = (stranger.clone(), new_token.clone()).into_val(&env);
    let result = factory
        .mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &stranger,
            invoke: &factory_call(&fid, "allow_token", args),
        }])
        .try_allow_token(&stranger, &new_token);

    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    assert!(!factory.get_allowed_tokens().contains(&new_token));
}

#[test]
fn allow_token_rejects_the_admin_address_without_its_signature() {
    use soroban_sdk::IntoVal;

    let (env, fid, admin, _token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let new_token = Address::generate(&env);

    // Nobody signs.
    assert!(factory
        .mock_auths(&[])
        .try_allow_token(&admin, &new_token)
        .is_err());

    // Someone else signs a call that names the admin.
    let impostor = Address::generate(&env);
    let args = (admin.clone(), new_token.clone()).into_val(&env);
    assert!(factory
        .mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &impostor,
            invoke: &factory_call(&fid, "allow_token", args),
        }])
        .try_allow_token(&admin, &new_token)
        .is_err());

    assert!(!factory.get_allowed_tokens().contains(&new_token));
}

#[test]
fn set_wasm_hash_rejects_a_non_admin_even_when_it_signs() {
    use soroban_sdk::IntoVal;

    let (env, fid, _admin, _token, wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let stranger = Address::generate(&env);
    let other_hash = BytesN::from_array(&env, &[9u8; 32]);

    let args = (stranger.clone(), other_hash.clone()).into_val(&env);
    let result = factory
        .mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &stranger,
            invoke: &factory_call(&fid, "set_wasm_hash", args),
        }])
        .try_set_wasm_hash(&stranger, &other_hash);

    assert_eq!(result, Err(Ok(Error::Unauthorized)));
    assert_eq!(factory.get_wasm_hash(), wasm_hash);
}

#[test]
fn set_wasm_hash_rejects_the_admin_address_without_its_signature() {
    use soroban_sdk::IntoVal;

    let (env, fid, admin, _token, wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let other_hash = BytesN::from_array(&env, &[9u8; 32]);

    assert!(factory
        .mock_auths(&[])
        .try_set_wasm_hash(&admin, &other_hash)
        .is_err());

    let impostor = Address::generate(&env);
    let args = (admin.clone(), other_hash.clone()).into_val(&env);
    assert!(factory
        .mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &impostor,
            invoke: &factory_call(&fid, "set_wasm_hash", args),
        }])
        .try_set_wasm_hash(&admin, &other_hash)
        .is_err());

    assert_eq!(factory.get_wasm_hash(), wasm_hash);
}

#[test]
fn admin_signed_allow_token_and_set_wasm_hash_succeed() {
    use soroban_sdk::IntoVal;

    let (env, fid, admin, _token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let new_token = Address::generate(&env);
    let new_hash = env.deployer().upload_contract_wasm(ESCROW_V2_WASM);

    let args = (admin.clone(), new_token.clone()).into_val(&env);
    factory
        .mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin,
            invoke: &factory_call(&fid, "allow_token", args),
        }])
        .allow_token(&admin, &new_token);
    assert!(factory.get_allowed_tokens().contains(&new_token));

    let args = (admin.clone(), new_hash.clone()).into_val(&env);
    factory
        .mock_auths(&[soroban_sdk::testutils::MockAuth {
            address: &admin,
            invoke: &factory_call(&fid, "set_wasm_hash", args),
        }])
        .set_wasm_hash(&admin, &new_hash);
    assert_eq!(factory.get_wasm_hash(), new_hash);
}

#[test]
fn set_wasm_hash_applies_to_new_escrows_only() {
    use soroban_sdk::Symbol;

    let (env, fid, admin, token, v1_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let amounts = vec![&env, 1_000_i128];
    let hashes = zero_hashes(&env, 1);
    let create = |client: &Address, salt: u8| {
        factory.create_escrow(
            client,
            &freelancer,
            &arbiter,
            &token,
            &604800u64,
            &amounts,
            &hashes,
            &default_settings(),
            &BytesN::from_array(&env, &[salt; 32]),
        )
    };

    let old_client = Address::generate(&env);
    let old_escrow = create(&old_client, 21);

    let v2_hash = env.deployer().upload_contract_wasm(ESCROW_V2_WASM);
    assert_ne!(v2_hash, v1_hash);
    factory.set_wasm_hash(&admin, &v2_hash);

    let new_client = Address::generate(&env);
    let new_escrow = create(&new_client, 22);
    let (_, data) = last_event(&env);
    assert_eq!(EscrowCreatedEvent::from_val(&env, &data).wasm_hash, v2_hash);

    // The new escrow runs the v2 code ...
    let v2_only = Symbol::new(&env, "v2_only");
    let no_args = soroban_sdk::Vec::<soroban_sdk::Val>::new(&env);
    let version: u32 = env.invoke_contract(&new_escrow, &v2_only, no_args.clone());
    assert_eq!(version, 2);

    // ... the existing one still runs the code it was deployed with.
    assert!(env
        .try_invoke_contract::<u32, soroban_sdk::Error>(&old_escrow, &v2_only, no_args)
        .is_err());
    assert_eq!(
        escrow::Client::new(&env, &old_escrow).get_job().client,
        old_client
    );
    assert_eq!(
        escrow::Client::new(&env, &new_escrow).get_job().client,
        new_client
    );
}

#[test]
fn escrow_settings_propagate_to_the_deployed_escrow() {
    use soroban_sdk::testutils::Ledger as _;

    let (env, fid, _admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let amounts = vec![&env, 1_000_i128, 2_000_i128];
    let two_days: u64 = 2 * 24 * 60 * 60;
    let settings = EscrowSettings {
        funding_mode: FundingMode::PerMilestone,
        dispute_timeout_seconds: two_days,
        arbiter_fee_bps: 250,
    };
    env.ledger().set_timestamp(1_700_000_000);

    let escrow_id = factory.create_escrow(
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604800u64,
        &amounts,
        &zero_hashes(&env, 2),
        &settings,
        &BytesN::from_array(&env, &[31u8; 32]),
    );
    let escrow = escrow::Client::new(&env, &escrow_id);

    // Read back through the escrow's own getters.
    assert_eq!(escrow.get_funding_mode(), escrow::FundingMode::PerMilestone);
    assert_eq!(escrow.get_arbiter_fee_bps(), 250);
    let job = escrow.get_job();
    assert_eq!(job.client, client);
    assert_eq!(job.milestones.len(), 2);

    // PerMilestone: one milestone can be funded on its own.
    token::StellarAssetClient::new(&env, &token).mint(&client, &1_000_i128);
    escrow.accept_job(&freelancer);
    escrow.accept_arbiter(&arbiter);
    escrow.fund_milestone(&client, &0u32);

    // The dispute timeout has no getter; it shows in when escalation opens.
    escrow.mark_delivered(&freelancer, &0u32);
    escrow.raise_dispute(&client, &0u32, &BytesN::from_array(&env, &[5u8; 32]));
    env.ledger().set_timestamp(1_700_000_000 + two_days - 1);
    assert!(escrow.try_escalate_dispute(&client, &0u32).is_err());
    // Two days, far short of the 30-day default, is enough.
    env.ledger().set_timestamp(1_700_000_000 + two_days);
    escrow.escalate_dispute(&client, &0u32);
}

#[test]
fn create_escrow_rejects_a_reused_salt_for_the_same_client() {
    let (env, fid, _admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let amounts = vec![&env, 1_000_i128];
    let hashes = zero_hashes(&env, 1);
    let salt = BytesN::from_array(&env, &[41u8; 32]);

    let first = factory.create_escrow(
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

    // The deployment address is already taken.
    assert!(factory
        .try_create_escrow(
            &client,
            &freelancer,
            &arbiter,
            &token,
            &604800u64,
            &amounts,
            &hashes,
            &default_settings(),
            &salt,
        )
        .is_err());

    // The first escrow is untouched.
    assert_eq!(escrow::Client::new(&env, &first).get_job().client, client);
}

#[test]
fn create_escrow_rejects_mismatched_milestone_and_spec_hash_lengths() {
    let (env, fid, _admin, token, _wasm_hash) = setup();
    let factory = EscrowFactoryClient::new(&env, &fid);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let arbiter = Address::generate(&env);
    let amounts = vec![&env, 1_000_i128, 2_000_i128];
    let salt = BytesN::from_array(&env, &[51u8; 32]);

    // Two amounts, one hash; and two amounts, three hashes.
    for hash_count in [1u32, 3u32] {
        assert!(factory
            .try_create_escrow(
                &client,
                &freelancer,
                &arbiter,
                &token,
                &604800u64,
                &amounts,
                &zero_hashes(&env, hash_count),
                &default_settings(),
                &salt,
            )
            .is_err());
    }

    // The rejected constructor left nothing at the address: the same salt
    // works once the lengths match.
    let predicted = factory.escrow_address(&client, &salt);
    let deployed = factory.create_escrow(
        &client,
        &freelancer,
        &arbiter,
        &token,
        &604800u64,
        &amounts,
        &zero_hashes(&env, 2),
        &default_settings(),
        &salt,
    );
    assert_eq!(deployed, predicted);
}
