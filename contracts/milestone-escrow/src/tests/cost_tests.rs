#![cfg(test)]

extern crate std;
use std::println;

use crate::{
    test::{decision_hash, dispute_reason_hash},
    DataKey, FundingMode, Milestone, MilestoneEscrow, MilestoneEscrowClient, MilestoneV1,
    ACCEPTANCE_WINDOW_SECONDS, MAX_MILESTONES,
};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger as _},
    token, vec, Address, Env, Vec,
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
/// flow gained a ledger write. Write entries lowered by one for
/// `mark_delivered`, `approve_milestone` and `raise_dispute` once the
/// write-only `DeliveredAt`, `MilestoneReleased` and `DisputeFlag` temporary
/// keys were retired (#1001).
/// `submit_evidence` raised to 215k instructions / 600 write bytes once it
/// runs the schema guard and `DisputeInfo` carries `raised_by` (#997).
const BUDGETS: [(&str, i64, u32, u32); 8] = [
    ("initialize", 245_000, 7, 1_910),
    ("fund", 362_000, 4, 1_530),
    ("mark_delivered", 234_000, 2, 650),
    ("approve_milestone", 490_000, 6, 1_440),
    ("claim_auto_release", 517_000, 6, 1_315),
    ("raise_dispute", 316_000, 4, 950),
    ("submit_evidence", 215_000, 2, 600),
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
    setup
        .escrow
        .resolve_dispute(&setup.arbiter_addr, &0, &true, &decision_hash(&env));
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

// ── Resource cost at MAX_MILESTONES (issue #963) ────────────────────────────
//
// Every entry point that walks all milestones is measured on a job holding
// MAX_MILESTONES (50) milestones and checked against the per-transaction
// network limits with headroom to spare, so a job created at the cap can
// always be settled, refunded, read and migrated on mainnet.
//
// The limits are the mainnet per-transaction Soroban settings this suite is
// held to. Re-check them against `stellar network settings --network mainnet`
// (or https://lab.stellar.org/network-limits) before launch and whenever a
// protocol upgrade changes them: a lower live limit makes this check too
// lenient, a higher one makes it conservative.
const TX_MAX_INSTRUCTIONS: i64 = 100_000_000;
const TX_MEMORY_LIMIT_BYTES: i64 = 41_943_040;
const TX_MAX_READ_ENTRIES: u32 = 100;
const TX_MAX_DISK_READ_BYTES: u32 = 200_000;
const TX_MAX_WRITE_ENTRIES: u32 = 50;
const TX_MAX_WRITE_BYTES: u32 = 132_096;
/// Share of each limit a flow may use at the cap: 80%, leaving 20% for growth
/// and for limit changes.
const HEADROOM_NUM: i64 = 4;
const HEADROOM_DEN: i64 = 5;

fn within_headroom(value: i64, limit: i64) -> bool {
    value * HEADROOM_DEN <= limit * HEADROOM_NUM
}

struct CapFixture<'a> {
    client_addr: Address,
    freelancer_addr: Address,
    arbiter_addr: Address,
    admin_addr: Address,
    token: Address,
    contract_id: Address,
    escrow: MilestoneEscrowClient<'a>,
}

/// A job at the milestone cap, initialized but not funded or accepted. The
/// client holds enough for every milestone plus one spare unit, used to create
/// sweepable excess.
fn cap_fixture(env: &Env, mode: FundingMode) -> CapFixture<'_> {
    env.ledger().set_timestamp(INITIAL_TIMESTAMP);
    let client_addr = Address::generate(env);
    let freelancer_addr = Address::generate(env);
    let arbiter_addr = Address::generate(env);
    let admin_addr = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(admin_addr.clone())
        .address();
    let mut amounts = Vec::new(env);
    for _ in 0..MAX_MILESTONES {
        amounts.push_back(MILESTONE_AMOUNT);
    }
    let total = MILESTONE_AMOUNT * MAX_MILESTONES as i128;
    token::StellarAssetClient::new(env, &token).mint(&client_addr, &(total + 1));
    let spec_hashes = crate::spec_hashes_for(&amounts);
    let contract_id = env.register(MilestoneEscrow, ());
    let escrow = MilestoneEscrowClient::new(env, &contract_id);
    escrow.initialize_with_funding_mode(
        &admin_addr,
        &client_addr,
        &freelancer_addr,
        &arbiter_addr,
        &token,
        &AUTO_RELEASE_SECONDS,
        &amounts,
        &spec_hashes,
        &mode,
    );
    assert_eq!(escrow.get_job().milestones.len(), MAX_MILESTONES);
    CapFixture {
        client_addr,
        freelancer_addr,
        arbiter_addr,
        admin_addr,
        token,
        contract_id,
        escrow,
    }
}

fn accept(fx: &CapFixture<'_>) {
    fx.escrow.accept_job(&fx.freelancer_addr);
    fx.escrow.accept_arbiter(&fx.arbiter_addr);
}

/// Funded, accepted Upfront job at the cap.
fn funded_cap_fixture(env: &Env) -> CapFixture<'_> {
    let fx = cap_fixture(env, FundingMode::Upfront);
    accept(&fx);
    fx.escrow.fund(&fx.client_addr);
    fx
}

/// Rewrite the cap job's storage into the schema-1 layout (the same steps as
/// `migrate_tests::revert_to_schema_1`, for every milestone).
fn revert_cap_to_schema_1(env: &Env, fx: &CapFixture<'_>) {
    env.as_contract(&fx.contract_id, || {
        let persistent = env.storage().persistent();
        let instance = env.storage().instance();
        for index in 0..MAX_MILESTONES {
            let key = DataKey::Milestone(index);
            let current: Milestone = persistent.get(&key).unwrap();
            persistent.set(
                &key,
                &MilestoneV1 {
                    amount: current.amount,
                    released_amount: current.released_amount,
                    status: current.status,
                    delivered_at: current.delivered_at,
                    funded: current.funded,
                },
            );
        }
        let whitelist: Vec<Address> = persistent.get(&DataKey::WhitelistedTokens).unwrap();
        persistent.remove(&DataKey::WhitelistedTokens);
        instance.set(&DataKey::WhitelistedTokens, &whitelist);
        instance.remove(&DataKey::SchemaVersion);
    });
}

/// Measures each milestone-looping entry point at MAX_MILESTONES. Every flow
/// runs in a fresh Env, so `cost_estimate` covers only the measured call.
fn measure_cap_flows() -> std::vec::Vec<Cost> {
    let mut costs = std::vec::Vec::new();
    let total = MILESTONE_AMOUNT * MAX_MILESTONES as i128;

    // fund: constant writes since #730, measured at the cap to prove it.
    let env = new_env();
    let fx = cap_fixture(&env, FundingMode::Upfront);
    accept(&fx);
    fx.escrow.fund(&fx.client_addr);
    costs.push(measure("fund", &env));

    // fund_milestone: the call that funds the last milestone, which runs the
    // "all funded" check and stores the job meta.
    let env = new_env();
    let fx = cap_fixture(&env, FundingMode::PerMilestone);
    accept(&fx);
    for index in 0..MAX_MILESTONES - 1 {
        fx.escrow.fund_milestone(&fx.client_addr, &index);
    }
    fx.escrow
        .fund_milestone(&fx.client_addr, &(MAX_MILESTONES - 1));
    costs.push(measure("fund_milestone", &env));

    // fund_milestone, worst case: storage without `FundedCount` (written
    // before it existed), so the count falls back to reading every milestone.
    let env = new_env();
    let fx = cap_fixture(&env, FundingMode::PerMilestone);
    accept(&fx);
    for index in 0..MAX_MILESTONES - 1 {
        fx.escrow.fund_milestone(&fx.client_addr, &index);
    }
    env.as_contract(&fx.contract_id, || {
        env.storage().instance().remove(&DataKey::FundedCount);
    });
    fx.escrow
        .fund_milestone(&fx.client_addr, &(MAX_MILESTONES - 1));
    costs.push(measure("fund_milestone_legacy", &env));

    // get_job / get_escrow_state: assemble every milestone.
    let env = new_env();
    let fx = funded_cap_fixture(&env);
    fx.escrow.get_job();
    costs.push(measure("get_job", &env));

    let env = new_env();
    let fx = funded_cap_fixture(&env);
    fx.escrow.get_escrow_state();
    costs.push(measure("get_escrow_state", &env));

    // replace_arbiter: scans every milestone for an open dispute.
    let env = new_env();
    let fx = funded_cap_fixture(&env);
    fx.escrow.replace_arbiter(&Address::generate(&env));
    costs.push(measure("replace_arbiter", &env));

    // sweep_excess: sums the outstanding obligation of every milestone
    // (outstanding_escrow_obligations).
    let env = new_env();
    let fx = funded_cap_fixture(&env);
    token::Client::new(&env, &fx.token).transfer(&fx.client_addr, &fx.contract_id, &1);
    fx.escrow.sweep_excess(&fx.client_addr, &fx.token);
    costs.push(measure("sweep_excess", &env));

    // withdraw_unaccepted: refunds and rewrites every milestone. Funded
    // without the parties' acceptance, then the acceptance window passes.
    let env = new_env();
    let fx = cap_fixture(&env, FundingMode::Upfront);
    fx.escrow.fund(&fx.client_addr);
    env.ledger()
        .set_timestamp(INITIAL_TIMESTAMP + ACCEPTANCE_WINDOW_SECONDS);
    fx.escrow.withdraw_unaccepted(&fx.client_addr);
    costs.push(measure("withdraw_unaccepted", &env));

    // Cancel overrides (settle_all_remaining): settle every milestone.
    let env = new_env();
    let fx = funded_cap_fixture(&env);
    fx.escrow.cancel_escrow(&fx.client_addr);
    fx.escrow.cancel_escrow(&fx.freelancer_addr);
    fx.escrow.admin_override_cancel_refund(&fx.admin_addr);
    costs.push(measure("admin_override_cancel_refund", &env));

    let env = new_env();
    let fx = funded_cap_fixture(&env);
    fx.escrow.cancel_escrow(&fx.client_addr);
    fx.escrow.cancel_escrow(&fx.freelancer_addr);
    fx.escrow.admin_override_cancel_release(&fx.admin_addr);
    costs.push(measure("admin_override_cancel_release", &env));

    // Split refunds on the cancel and emergency paths, for the full balance.
    let env = new_env();
    let fx = funded_cap_fixture(&env);
    fx.escrow.cancel_escrow(&fx.client_addr);
    fx.escrow.cancel_escrow(&fx.freelancer_addr);
    fx.escrow
        .cancel_escrow_claim_refund(&fx.admin_addr, &total, &5_000, &5_000);
    costs.push(measure("cancel_escrow_claim_refund", &env));

    let env = new_env();
    let fx = funded_cap_fixture(&env);
    fx.escrow
        .emergency_pause(&fx.client_addr, &fx.freelancer_addr);
    fx.escrow
        .emergency_pause_claim_refund(&fx.admin_addr, &total, &5_000, &5_000);
    costs.push(measure("emergency_pause_claim_refund", &env));

    // migrate from schema 1: rewrites every milestone.
    let env = new_env();
    let fx = funded_cap_fixture(&env);
    revert_cap_to_schema_1(&env, &fx);
    fx.escrow.migrate(&fx.admin_addr);
    costs.push(measure("migrate", &env));

    costs
}

#[test]
fn max_milestones_cost_within_network_limits() {
    let costs = measure_cap_flows();

    println!("\nResource costs at MAX_MILESTONES = {MAX_MILESTONES}");
    println!(
        "{:<30} {:>13} {:>10} {:>10} {:>10} {:>10} {:>9} {:>11}",
        "flow",
        "instructions",
        "mem bytes",
        "read ent.",
        "disk ent.",
        "disk bytes",
        "writes",
        "write bytes"
    );
    for cost in &costs {
        println!(
            "{:<30} {:>13} {:>10} {:>10} {:>10} {:>10} {:>9} {:>11}",
            cost.name,
            cost.instructions,
            cost.mem_bytes,
            cost.memory_read_entries + cost.disk_read_entries,
            cost.disk_read_entries,
            cost.disk_read_bytes,
            cost.write_entries,
            cost.write_bytes
        );
    }

    // Print the whole table before checking, so a failure shows every flow.
    let mut failures = std::vec::Vec::new();
    for cost in &costs {
        let read_entries = cost.memory_read_entries + cost.disk_read_entries;
        let checks = [
            ("instructions", cost.instructions, TX_MAX_INSTRUCTIONS),
            ("memory bytes", cost.mem_bytes, TX_MEMORY_LIMIT_BYTES),
            (
                "read entries",
                read_entries as i64,
                TX_MAX_READ_ENTRIES as i64,
            ),
            (
                "disk read bytes",
                cost.disk_read_bytes as i64,
                TX_MAX_DISK_READ_BYTES as i64,
            ),
            (
                "write entries",
                cost.write_entries as i64,
                TX_MAX_WRITE_ENTRIES as i64,
            ),
            (
                "write bytes",
                cost.write_bytes as i64,
                TX_MAX_WRITE_BYTES as i64,
            ),
        ];
        for (resource, value, limit) in checks {
            if !within_headroom(value, limit) {
                failures.push(std::format!(
                    "{}: {resource} {value} is over 80% of the {limit} limit",
                    cost.name
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "flows at MAX_MILESTONES exceed the network-limit headroom:\n{}",
        failures.join("\n")
    );
}
