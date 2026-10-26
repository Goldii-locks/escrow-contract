#![cfg(test)]

extern crate std;
use std::println;

use crate::{test::dispute_reason_hash, MilestoneEscrow, MilestoneEscrowClient};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger as _},
    token, vec, Address, Env,
};

const MILESTONE_AMOUNT: i128 = 1_000;
const AUTO_RELEASE_SECONDS: u64 = 604_800;
const INITIAL_TIMESTAMP: u64 = 1_700_000_000;

struct Fixture<'a> {
    client_addr: Address,
    freelancer_addr: Address,
    arbiter_addr: Address,
    _admin_addr: Address,
    _token_contract_id: Address,
    escrow: MilestoneEscrowClient<'a>,
}

#[derive(Clone, Copy)]
struct Cost {
    name: &'static str,
    instructions: i64,
    memory_read_entries: u32,
    mem_bytes: i64,
    disk_read_entries: u32,
    disk_read_bytes: u32,
    write_entries: u32,
    write_bytes: u32,
}

/// Budget per entry point: (name, max instructions, max write entries,
/// max write bytes). Recorded from the current baselines (#764): write entries
/// are exact, so an extra storage write fails the test; instructions and write
/// bytes allow ~10% headroom for incidental changes. Raise a budget only with
/// a reason in the PR. Last re-baselined after the dispute timeout (#784),
/// per-milestone funding (#766) and job acceptance (#757) checks landed, and
/// write bytes again once milestones carry their 32-byte spec hash (#767);
/// no flow gained a ledger write. Re-baselined for the dispute `reason_hash`
/// and the evidence trail (#712): `raise_dispute` writes 13 more bytes (the
/// hash plus an empty evidence list) and `submit_evidence` is measured for
/// the first time. Re-baselined again after constructor deployment (#665),
/// which includes the contract instance ledger entry alongside
/// initialization. Instructions re-baselined after the TTL extension (#669):
/// storing a milestone now extends its entry's and the instance's TTL, which
/// costs instructions but no extra ledger write. Write bytes raised by 60 for
/// every flow that stores a milestone once milestones carry their 32-byte
/// `deliverable` hash (a 20-byte `deliverable` key plus a 40-byte value); no
/// flow gained a ledger write.
const BUDGETS: [(&str, i64, u32, u32); 8] = [
    ("initialize", 235_000, 7, 1_910),
    ("fund", 362_000, 4, 1_530),
    ("mark_delivered", 224_000, 3, 650),
    ("approve_milestone", 490_000, 7, 1_440),
    ("claim_auto_release", 517_000, 6, 1_315),
    ("raise_dispute", 316_000, 5, 950),
    ("submit_evidence", 201_000, 2, 580),
    ("resolve_dispute", 558_000, 7, 1_315),
];

fn fixture(env: &Env) -> Fixture<'_> {
    env.ledger().set_timestamp(INITIAL_TIMESTAMP);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);
    let token_contract_id = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    token::StellarAssetClient::new(env, &token_contract_id).mint(&client_addr, &MILESTONE_AMOUNT);
    let amounts = vec![env, MILESTONE_AMOUNT];
    let spec_hashes = crate::spec_hashes_for(&amounts);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token_contract_id,
        &AUTO_RELEASE_SECONDS,
        &amounts,
        &spec_hashes,
    );

    Fixture {
        client_addr,
        freelancer_addr,
        arbiter_addr,
        _admin_addr: admin_addr,
        _token_contract_id: token_contract_id,
        escrow,
    }
}

fn initialize(_fixture: &Fixture<'_>, _env: &Env) {}

fn fund(fixture: &Fixture<'_>) {
    // Delivery and disputes require both acceptances (#757); they are separate
    // invocations, so they do not count toward the measured fund cost.
    fixture.escrow.accept_job(&fixture.freelancer_addr);
    fixture.escrow.accept_arbiter(&fixture.arbiter_addr);
    fixture.escrow.fund(&fixture.client_addr);
}

fn measure(name: &'static str, env: &Env) -> Cost {
    let resources = env.cost_estimate().resources();
    Cost {
        name,
        instructions: resources.instructions,
        memory_read_entries: resources.memory_read_entries,
        mem_bytes: resources.mem_bytes,
        disk_read_entries: resources.disk_read_entries,
        disk_read_bytes: resources.disk_read_bytes,
        write_entries: resources.write_entries,
        write_bytes: resources.write_bytes,
    }
}

fn new_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

#[test]
fn main_entrypoint_cost_baselines() {
    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    let initialize_cost = measure("initialize", &env);

    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    fund(&setup);
    let fund_cost = measure("fund", &env);

    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    fund(&setup);
    setup.escrow.mark_delivered(&setup.freelancer_addr, &0);
    let mark_delivered_cost = measure("mark_delivered", &env);

    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    fund(&setup);
    setup.escrow.mark_delivered(&setup.freelancer_addr, &0);
    setup.escrow.approve_milestone(&setup.client_addr, &0);
    let approve_milestone_cost = measure("approve_milestone", &env);

    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    fund(&setup);
    setup.escrow.mark_delivered(&setup.freelancer_addr, &0);
    env.ledger()
        .set_timestamp(INITIAL_TIMESTAMP + AUTO_RELEASE_SECONDS);
    setup.escrow.claim_auto_release(&setup.freelancer_addr, &0);
    let claim_auto_release_cost = measure("claim_auto_release", &env);

    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    fund(&setup);
    setup.escrow.mark_delivered(&setup.freelancer_addr, &0);
    setup
        .escrow
        .raise_dispute(&setup.client_addr, &0, &dispute_reason_hash(&env));
    let raise_dispute_cost = measure("raise_dispute", &env);

    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    fund(&setup);
    setup.escrow.mark_delivered(&setup.freelancer_addr, &0);
    setup
        .escrow
        .raise_dispute(&setup.client_addr, &0, &dispute_reason_hash(&env));
    setup
        .escrow
        .submit_evidence(&setup.client_addr, &0, &dispute_reason_hash(&env));
    let submit_evidence_cost = measure("submit_evidence", &env);

    let env = new_env();
    let setup = fixture(&env);
    initialize(&setup, &env);
    fund(&setup);
    setup.escrow.mark_delivered(&setup.freelancer_addr, &0);
    setup
        .escrow
        .raise_dispute(&setup.client_addr, &0, &dispute_reason_hash(&env));
    setup.escrow.resolve_dispute(&setup.arbiter_addr, &0, &true);
    let resolve_dispute_cost = measure("resolve_dispute", &env);

    let costs = [
        initialize_cost,
        fund_cost,
        mark_delivered_cost,
        approve_milestone_cost,
        claim_auto_release_cost,
        raise_dispute_cost,
        submit_evidence_cost,
        resolve_dispute_cost,
    ];

    println!("\nMain entry-point resource costs");
    println!(
        "{:<22} {:>14} {:>11} {:>11} {:>10} {:>10} {:>12} {:>12}",
        "flow",
        "instructions",
        "mem reads",
        "mem bytes",
        "disk reads",
        "disk bytes",
        "write entries",
        "write bytes"
    );
    for cost in costs {
        println!(
            "{:<22} {:>14} {:>11} {:>11} {:>10} {:>10} {:>12} {:>12}",
            cost.name,
            cost.instructions,
            cost.memory_read_entries,
            cost.mem_bytes,
            cost.disk_read_entries,
            cost.disk_read_bytes,
            cost.write_entries,
            cost.write_bytes
        );
    }

    // Print the whole table before checking, so a failure shows every flow.
    for cost in costs {
        assert!(cost.instructions > 0, "{} was not metered", cost.name);
        let (_, max_instructions, max_write_entries, max_write_bytes) = BUDGETS
            .iter()
            .find(|(name, ..)| *name == cost.name)
            .copied()
            .expect("every measured flow has a budget");
        assert!(
            cost.instructions <= max_instructions,
            "{}: {} instructions exceeds the budget of {}",
            cost.name,
            cost.instructions,
            max_instructions
        );
        assert!(
            cost.write_entries <= max_write_entries,
            "{}: {} ledger writes exceeds the budget of {}",
            cost.name,
            cost.write_entries,
            max_write_entries
        );
        assert!(
            cost.write_bytes <= max_write_bytes,
            "{}: {} write bytes exceeds the budget of {}",
            cost.name,
            cost.write_bytes,
            max_write_bytes
        );
    }
}
