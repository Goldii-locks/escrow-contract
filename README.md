# Milestone Escrow Contract

A Soroban smart contract on Stellar that enables trustless milestone-based escrow between a client, freelancer, and arbiter.

## Overview

This contract allows a client to fund a job broken into milestones. Funds are locked in the contract and released per milestone only when the client approves delivery. Disputes can be raised by either party and resolved by a designated arbiter.

## Trust Model

The escrow's protection depends on **four distinct roles** assigned at deploy time via `__constructor`:

- **Admin** — the platform operator. Holds admin-only endpoints (whitelist management, pause/resume, admin overrides, upgrades). The admin is expected to be a neutral platform address, not the client or freelancer.
- **Client** — funds the job and approves milestone releases.
- **Freelancer** — delivers milestones and receives payouts.
- **Arbiter** — a neutral third party that resolves disputes via `resolve_dispute` and `apply_dispute_arbitration_split`.

`__constructor` enforces party distinctness: it rejects any collision among `admin`, `client`, `freelancer`, and `arbiter` with `Error::InvalidParties`. This prevents the client from resolving their own disputes (arbiter == client), the freelancer from ruling in their own favour (arbiter == freelancer), or the client from holding every admin override (admin == client). The frontend must pass four genuinely distinct addresses — it must not reuse the connected wallet as both admin and client.

### Administrative Roles

The admin's powers are split into narrower roles (issue #761), so day-to-day
keys need not hold upgrade or fund-moving rights. The root admin assigns each
role with `set_admin_role`; until then the root admin holds it. Assigning the
root admin's own address restores the default. `get_admin_role` returns the
current holder, and [`AUTH.md`](contracts/milestone-escrow/AUTH.md) lists the
role each function requires.

| Role | Permissions |
|---|---|
| `Upgrader` | `upgrade`, `migrate`, `migrate_cold_config`. Use a cold key. |
| `Operator` | Pause and resume, including the emergency pause controls. |
| `FeeManager` | Fees, yield, tax settings and the token whitelist. |
| `Resolver` | Admin overrides of stuck payouts, and multisig lock and setup. A cancel override (`admin_override_cancel_release` / `_refund`) is refused with `DisputeTimeoutNotReached` while any milestone's dispute is younger than the dispute timeout. |
| root admin | Assigns roles, admin transfer (`transfer_admin`, `propose_admin_transfer`), signer-set changes (`propose_multisig_config`) and `sweep_excess`. |

## Arbiter Fee

The constructor's `settings.arbiter_fee_bps` sets an arbiter fee (basis points, max 1,000 = 10 %). When an escrow has a fee, `resolve_dispute` and `apply_dispute_arbitration_split` first pay that share of the disputed milestone's remaining balance to the arbiter, then pay or split the rest between client and freelancer. The fee is fixed at construction; `0` means no fee, and milestones that never go to dispute never pay one (#738).

### What the admin can do

- Pause and resume the escrow (`admin_pause_escrow`, `admin_resume_escrow`).
- Override-release or override-refund a milestone **only when** the milestone is `Disputed` (arbiter unreachable or timed out) **or** the two-party cancel lock is active. Overriding a `Pending`, `Delivered`, or `PartiallyReleased` milestone that has no open dispute is rejected with `InvalidStatus`.
- Override a streaming-release split on a `Disputed` milestone (`admin_override_streaming_release`).
- Manage token whitelists and upgrades.

### What the admin cannot do

- Release or refund a milestone that has not been disputed and has no cancel lock active — the arbiter's role is preserved for normal settlement.
- Act as a unilateral custodian: admin overrides require a genuine deadlock (dispute or cancel) before they fire.

## Contract Functions

The contract exposes **94 public functions** grouped by category below.

### Core Escrow (Initialization & Funding)

| Function | Caller | Description |
|---|---|---|
| `__constructor` | Deployer | Atomically initializes the job with milestone amounts, spec hashes and `settings` (`EscrowSettings`: funding mode, dispute timeout, arbiter fee) |
| `get_arbiter_fee_bps` | Anyone | Read the arbiter fee (0 when none) |
| `fund` | Client | Deposit total amount into contract (Upfront mode) |
| `fund_milestone` | Client | Deposit one milestone (PerMilestone mode) |
| `get_funding_mode` | Anyone | View the current funding mode |
| `accept_job` | Freelancer | Freelancer accepts the job |
| `accept_arbiter` | Arbiter | Arbiter accepts the job |
| `withdraw_unaccepted` | Client | Reclaim funds if job not accepted within 7 days |
| `add_milestone` | Client + Freelancer | Append a new Pending milestone to a funded job; the client deposits its amount in the same call (capped at `MAX_MILESTONES` = 50) |
| `reduce_milestone` | Client + Freelancer | Lower a Pending milestone's amount and refund the difference to the client |
| `rotate_freelancer` | Client + Freelancer | Move the freelancer role and future payouts to a new address |
| `replace_arbiter` | Client + Freelancer | Appoint a new arbiter when the sitting one is unavailable; refused while a dispute is open |
| `get_job` | Anyone | View current job state |
| `get_milestone` | Anyone | View one milestone without loading the whole job |
| `milestone_count` | Anyone | Number of milestones |
| `version` | Anyone | Get contract version |

### Milestone Lifecycle

| Function | Caller | Description |
|---|---|---|
| `mark_delivered` | Freelancer | Mark a milestone as delivered |
| `request_revision` | Client | Return a delivered milestone to Pending (max 3 revisions) |
| `get_revision_count` | Anyone | Get revision count for a milestone |
| `extend_milestone_deadline` | Client | Extend auto-release deadline for a delivered milestone |
| `claim_auto_release` | Freelancer | Auto-release a delivered milestone after deadline |
| `trigger_auto_release` | Anyone | Same release as `claim_auto_release`, callable by anyone; pays the freelancer |
| `time_until_auto_release` | Anyone | Time remaining until auto-release |
| `approve_partial` | Client | Partially release funds for a milestone |
| `approve_milestone` | Client | Release funds for a delivered milestone |
| `approve_milestones` | Client | Approve up to 10 milestones in one call |
| `withdraw` | Freelancer/Client | Withdraw released funds or refunds |

### Disputes & Arbitration

| Function | Caller | Description |
|---|---|---|
| `raise_dispute` | Client or Freelancer | Freeze a milestone for arbitration, committing a 32-byte `reason_hash` |
| `submit_evidence` | Client or Freelancer | Anchor an evidence hash on an open dispute (max 10 per dispute) |
| `get_dispute_info` | Anyone | Read the dispute reason hash, timestamps and evidence trail |
| `escalate_dispute` | Client or Freelancer | After timeout, restore milestone to pre-dispute status |
| `resolve_dispute` | Arbiter | Release to freelancer or refund to client, after any arbiter fee |
| `dispute_arbitration_split` | Arbiter | Propose a split between client/freelancer |
| `apply_dispute_arbitration_split` | Arbiter | Apply the proposed arbitration split |

### Cancellation

| Function | Caller | Description |
|---|---|---|
| `cancel_escrow` | Client or Freelancer | Initiate escrow cancellation |
| `revoke_cancel_approval` | Client or Freelancer | Revoke cancellation approval |
| `admin_override_cancel_release` | Admin | Admin override: release to freelancer |
| `admin_override_cancel_refund` | Admin | Admin override: refund to client |

### Emergency Pause

| Function | Caller | Description |
|---|---|---|
| `emergency_pause` | Client & Freelancer | Jointly pause the contract |
| `emergency_unpause` | Admin | Admin unpause the contract |
| `emergency_unpause_parties` | Client & Freelancer | Jointly lift a party-initiated pause |
| `emergency_pause_admin_override` | Admin | Admin override pause state |
| `is_emergency_paused` | Anyone | Check if contract is paused |
| `emergency_pause_split_refund` | Admin/Client/Freelancer | Compute split while paused (read-only) |
| `emergency_pause_claim_refund` | Admin | Settle emergency refund (claim) |
| `emergency_pause_allocation` | Admin/Client/Freelancer | Allocate frozen balance to parties |

### Admin & Upgrades

| Function | Caller | Description |
|---|---|---|
| `transfer_admin` | Admin | Transfer admin role |
| `set_admin_role` | Admin | Assign an administrative role (Upgrader, Operator, FeeManager, Resolver) |
| `get_admin_role` | Anyone | Current holder of a role |
| `add_whitelisted_token` | Admin | Add token to whitelist |
| `remove_whitelisted_token` | Admin | Remove token from whitelist |
| `is_token_whitelisted` | Anyone | Check if token is whitelisted |
| `get_whitelisted_tokens` | Anyone | List all whitelisted tokens |
| `upgrade` | Admin | Upgrade contract WASM |
| `migrate_cold_config` | Admin | Migrate config out of instance storage |
| `admin_pause_escrow` | Admin | Admin pause (blocks user endpoints) |
| `admin_resume_escrow` | Admin | Admin resume |
| `propose_admin_transfer` | Admin | Propose multisig admin transfer |
| `execute_admin_transfer` | Multisig | Execute proposed admin transfer |
| `cancel_admin_transfer_proposal` | Admin | Cancel pending admin transfer |
| `get_pending_admin_transfer` | Anyone | View pending admin transfer |

### Platform Fees

| Function | Caller | Description |
|---|---|---|
| `set_platform_fee_allocation` | Admin | Set fee split (client/freelancer/treasury) |
| `lock_platform_fee_allocation` | Admin | Lock fee allocation |
| `pf_alloc_admin_override` | Admin | Admin override for fee allocation |
| `get_platform_fee_allocation` | Anyone | View current fee allocation |
| `calculate_platform_fee_split` | Anyone | Calculate fee split for an amount |

### Payment Streaming

| Function | Caller | Description |
|---|---|---|
| `payment_streaming_milestones` | Admin | Configure streaming for milestones |
| `payment_streaming_consent` | Client/Freelancer | Consent to streaming config |

### Time Extensions

| Function | Caller | Description |
|---|---|---|
| `milestone_time_extensions` | Admin | Configure time extensions |
| `time_extensions_consent` | Client/Freelancer | Consent to time extensions |

### Multisig

| Function | Caller | Description |
|---|---|---|
| `multisig_approval_init` | Admin | Initialize multisig signers & threshold |
| `multisig_approve` | Signer | Approve a multisig proposal |
| `is_multisig_approved` | Anyone | Check if proposal is approved |
| `multisig_lock` | Admin | Lock multisig (require admin intervention) |
| `is_multisig_locked` | Anyone | Check if multisig is locked |
| `multisig_admin_override_release` | Admin | Override locked multisig: release |
| `multisig_admin_override_refund` | Admin | Override locked multisig: refund |
| `multisig_split_refund` | Signers | Split refund via multisig |

### Interest Yield

| Function | Caller | Description |
|---|---|---|
| `escrow_interest_yield` | Anyone | Estimate interest yield |
| `set_escrow_interest_yield` | Client/Freelancer | Configure yield share |
| `set_interest_yield_consent` | Client/Freelancer | Consent to yield config |
| `lock_escrow_interest_yield` | Admin | Lock yield config |
| `unlock_escrow_interest_yield` | Admin | Unlock yield config |
| `is_escrow_interest_yield_locked` | Anyone | Check if yield is locked |
| `interest_yield_split_refund` | Admin | Split refund with yield |
| `get_escrow_interest_yield` | Anyone | View yield state |
| `admin_set_yield_rate` | Admin | Set annual yield rate (bps) |
| `admin_accrue_yield` | Admin | Accrue yield to admin |

### Tax Withholding

| Function | Caller | Description |
|---|---|---|
| `tax_withholding_deductions` | Client + Freelancer | Record a tax withholding for a milestone (needs a tax recipient when tax > 0) |
| `set_tax_recipient` | Admin | Set the address that receives withheld tax |
| `get_tax_recipient` | Anyone | Read the tax recipient |
| `tax_withholding_split_refund` | Admin | Split refund with tax |
| `admin_override_tax_release` | Admin | Override tax: release |
| `admin_override_tax_refund` | Admin | Override tax: refund |
| `admin_override_tax_split_refund` | Admin | Override tax: split refund |
| `get_yield_info` | Anyone | View yield rate, accrued, paused |
| `admin_tax_withholding_deductions` | Admin | Admin compute tax withholding |

### Utilities

| Function | Caller | Description |
|---|---|---|
| `sweep_excess` | Client or Admin | Recover surplus tokens |
| `get_milestone_spec_hash` | Anyone | Get spec hash for milestone |
| `get_reputation` | Anyone | Get address reputation |
| `compute_ratio_allocation` | Anyone | Compute ratio allocation (largest remainder) |

Events carry party addresses as extra topics for `getEvents` filtering; see
[EVENTS.md](EVENTS.md#indexed-topics).

## Contract Errors

| Code | Error | Meaning |
|---|---|---|
| 1 | `AlreadyInitialized` | `initialize` was called more than once |
| 2 | `NotInitialized` | A call required the contract to be initialized but it was not |
| 3 | `AlreadyFunded` | `fund` was called after the escrow was already funded |
| 4 | `NotFunded` | An operation that requires the escrow to be funded was attempted while it was not |
| 5 | `Unauthorized` | Caller is not authorized to perform this action |
| 6 | `InvalidMilestone` | Milestone index is out of bounds or refers to an invalid milestone |
| 7 | `InvalidStatus` | Operation is not allowed for the current milestone status |
| 8 | `TokenNotWhitelisted` | The provided token address is not whitelisted by the admin |
| 9 | `TokenAlreadyWhitelisted` | Attempted to whitelist a token that is already whitelisted |
| 10 | `InvalidAmount` | Amount is zero, negative, or otherwise invalid for this operation |
| 11 | `DeadlineNotPassed` | Required deadline/timeout has not yet passed |
| 12 | `InvalidAddress` | Provided address is invalid |
| 13 | `Paused` | Contract is paused and this operation is not allowed while paused |
| 14 | `InvalidRatio` | Ratio sum is incorrect (must sum to BPS_SCALE) or values are out of range |
| 15 | `InvalidExtension` | Milestone time extension is invalid (e.g |
| 16 | `EscrowLocked` | Escrow is locked (e.g |
| 17 | `MultiSigNoSigners` | Multisig setup requires at least one signer |
| 18 | `MultiSigTooManySigners` | Multisig setup exceeds the maximum allowed number of signers |
| 19 | `MultiSigInvalidThreshold` | Multisig threshold is invalid (must be > 0 and <= number of signers) |
| 20 | `MultiSigDuplicateSigner` | Multisig setup contains duplicate signers |
| 21 | `AdminTransferPending` | A new `propose_admin_transfer` was attempted while one was already pending execution or cancellation |
| 22 | `NoPendingAdminTransfer` | `execute_admin_transfer` / `cancel_admin_transfer_proposal` was called with no proposal currently pending |
| 23 | `MultiSigThresholdNotMet` | `execute_admin_transfer` was called before the proposal's multisig approval threshold was reached |
| 24 | `DisputeAlreadyRaised` | `raise_dispute` was re-entered for a milestone whose dispute lock is still held, i.e |
| 25 | `MultiSigEmptyBalance` | Retained for error-code stability — no longer emitted |
| 26 | `TaxWithholdingInProgress` | A guarded endpoint was called while `tax_withholding_deductions` is mid-execution and holds `DataKey::TaxWithholdingExecutionLock` |
| 27 | `PlatformFeeAllocationInProgress` | A guarded endpoint was called while a platform-fee allocation is mid-execution and holds `DataKey::PlatformFeeAllocationLock` |
| 28 | `EmergencyPauseInProgress` | A guarded endpoint was called while an emergency pause transition is mid-execution and holds `DataKey::EpLk` |
| 29 | `AlreadyPaused` | `emergency_pause` was called while the contract is already paused |
| 30 | `NotPaused` | `emergency_unpause`, or a pause-gated endpoint such as `emergency_pause_claim_refund`, was called while the contract is not paused |
| 31 | `InvalidAllocationWeights` | A weight vector passed to `emergency_pause_allocation` was empty, exceeded the party cap, contained a negative weight, or summed to zero |
| 32 | `EmptyBalance` | An emergency refund / pause-gated endpoint was called while the contract holds zero token balance, so there is nothing to settle |
| 33 | `TimeExtInProgress` | A guarded endpoint was called while `milestone_time_extensions` is mid-execution and holds `DataKey::TimeExtExecutionLock` |
| 34 | `PaymentStreamingInProgress` | A guarded endpoint was called while `payment_streaming_milestones` is mid-execution and holds `DataKey::PaymentStreamingExecutionLock` |
| 35 | `FeeTooHigh` | A platform-fee allocation exceeded its per-party cap: treasury above `MAX_TREASURY_FEE_BPS` or client above `MAX_CLIENT_FEE_BPS` |
| 36 | `ArithmeticOverflow` | A checked `i128` operation inside a ratio/allocation computation would have overflowed or underflowed |
| 37 | `InsufficientBalance` | A settlement asked to distribute more than the contract currently holds in the escrowed token |
| 38 | `InvalidDisputeTimeout` | A dispute timeout must be greater than zero |
| 39 | `DisputeTimeoutNotReached` | The configured dispute timeout has not elapsed yet |
| 40 | `InvalidFundingMode` | `fund` was called on a PerMilestone escrow, or `fund_milestone` was called on an Upfront escrow |
| 41 | `MaxRevisions` | `request_revision` was called after `MAX_REVISIONS` revisions |
| 42 | `NothingToSweep` | `sweep_excess` found no recoverable surplus |
| 43 | `NotAccepted` | A delivery or dispute endpoint was called before both the freelancer (`accept_job`) and the arbiter (`accept_arbiter`) accepted the job |
| 44 | `InvalidParties` | `initialize` was called with overlapping party addresses |
| 45 | `TooManyMilestones` | `add_milestone` was called on a job that already carries `MAX_MILESTONES` milestones |
| 46 | `EvidenceLimitReached` | `submit_evidence` was called after `MAX_DISPUTE_EVIDENCE_ENTRIES` hashes had already been recorded for the milestone's current dispute |
| 47 | `InvalidDuration` | `auto_release_seconds` is zero or above `MAX_AUTO_RELEASE_SECONDS` |
| 48 | `WhitelistFull` | The whitelist already contains `MAX_WHITELIST_SIZE` (50) entries |
| 49 | `WhitelistMinimum` | Removing a token would leave the whitelist empty (at least one token must remain) |
| 50 | `TooManyParties` | A ratio vector exceeds the maximum allowed number of parties (255) |
| 51 | `MigrationRequired` | Storage predates this code (after `upgrade`, before `migrate`); the admin must call `migrate` first |
| 52 | `AlreadyMigrated` | `migrate` was called on storage already at `SCHEMA_VERSION` |
| 53 | `MultiSigConfigPending` | `propose_multisig_config` was called while another signer-set change is pending |
| 54 | `NoPendingMultiSigConfig` | `execute_multisig_config` / `cancel_multisig_config_proposal` found no pending signer-set change |
| 55 | `DisputePending` | `replace_arbiter` was called while a milestone is `Disputed` |

## Milestone States

Pending → Delivered → Released

↓

Disputed → Released (arbiter favors freelancer)

→ Refunded (arbiter favors client)

The full state machine, including every entry point, its required signers and the escrow-level locks, is specified in [`contracts/milestone-escrow/SPEC.md`](contracts/milestone-escrow/SPEC.md).

## Security

Roles, trust assumptions, admin powers and known risks are described in [`THREAT_MODEL.md`](THREAT_MODEL.md).

## Prerequisites

- https://rustup.rs/ (the toolchain is pinned to 1.96.0 by `rust-toolchain.toml`)
- https://developers.stellar.org/docs/smart-contracts/getting-started/setup (stellar-cli 28.0.0)
- wasm32 target: `rustup target add wasm32v1-none`
- Docker, for reproducible builds

## Build

For development, from the repository root:

```bash
stellar contract build --manifest-path contracts/milestone-escrow/Cargo.toml
```

Anything that will be deployed must come from the reproducible build instead:

```bash
scripts/build-reproducible.sh
```

It compiles a clean export of `HEAD` (or of the commit given as the first
argument) inside a pinned container (`build/reproducible.Dockerfile`: Rust
1.96.0 by image digest, stellar-cli 28.0.0 by release checksum, linux/amd64)
and writes `dist/milestone_escrow.wasm`, its SHA-256 and `dist/build-info.json`.
The same commit gives the same hash on any machine. CI runs the same script on
every commit and publishes the hash in the job summary and the wasm as an
artifact.

### TypeScript bindings

CI also generates typed TypeScript bindings from the built wasm on every main
build (artifact `milestone-escrow-bindings-ts-<sha>`) and publishes them as the
versioned GitHub Package `@goldii-locks/milestone-escrow-bindings` on `v*`
tags, so clients stop hand-encoding ScVals. See
[docs/typescript-bindings.md](docs/typescript-bindings.md).

The `test` job also builds the optimized wasm, prints its size and SHA-256,
uploads it as an artifact, and fails when it is larger than
`WASM_BUDGET_BYTES` (set in `.github/workflows/ci.yml` to a measured size plus
5%). If a change legitimately needs more room, raise the
budget in the same PR and say why.

## Test

From the repository root:

```bash
cargo test -p milestone-escrow
```

## Project Status

- Mainnet release gates are tracked in the [mainnet readiness checklist](MAINNET.md)
- The admin authorization direction and migration decision are recorded in [ADMIN_MULTISIG_DECISION.md](ADMIN_MULTISIG_DECISION.md)
- Contract implementation completed in `contracts/milestone-escrow`
- Includes milestone delivery, approval, dispute, and arbitration flows
- Initialize now rejects empty milestone lists, zero/negative amounts, zero addresses, zero auto-release windows, and **party collisions** (`admin == client`, `admin == freelancer`, `arbiter == client`, `arbiter == freelancer`, `client == freelancer`) with explicit contract errors
- `raise_dispute` now performs zero-address check, `require_auth`, and identity validation **before** writing the re-entrancy lock (issue #566), matching the authorization pattern used by `fund`, `approve_milestone`, and `resolve_dispute`; includes the five precondition guard assertions present in all other mutating functions
- Disputes may only be raised on delivered or partially released milestones. The dispute timeout is set by the constructor's `settings.dispute_timeout_seconds` (non-zero; 30 days is the usual value). If neither party gets an arbitration decision by that deadline, either party may call `escalate_dispute` to return the milestone to its pre-dispute status; a restored delivered milestone gets a fresh auto-release window
- `cancel_escrow` is a two-party handshake and a single-party approval now expires after `CANCEL_APPROVAL_TTL_SECONDS` (7 days, issue #713): an approval older than that is treated as absent, so the counter-party's call records a fresh first approval instead of locking the escrow. `CancelApprovalRecordedEvent` (`cxlappr`) carries `approved_at` so indexers can track the same window
- `lock_escrow_interest_yield` now rejects unauthorized callers with `Unauthorized` and an illegal source state with `InvalidStatus` **before** it reads or writes the ledger entry (issue #464); authorization runs first via `require_admin_from_instance`, so a non-admin cannot probe whether an escrow is locked, and an already-locked configuration is refused rather than silently re-locked
- Contract tests live under `contracts/milestone-escrow/src/tests`. Soroban's per-test `test_snapshots/` are gitignored; three golden end-to-end snapshots (fund → deliver → approve, dispute → resolve, cancel → override) are committed in `contracts/milestone-escrow/golden_snapshots` and CI fails when a run changes them. After an intended change, rerun `cargo test golden_snapshot_tests` and commit the updated files.

## Escrow factory

`contracts/escrow-factory` deploys one escrow per job (issue #666). It is
deployed with `--admin <platform-admin> --wasm_hash <escrow-wasm-hash>`
constructor arguments. The admin allows tokens with `allow_token` and can
point the factory at a new escrow build with `set_wasm_hash`.

`create_escrow(client, freelancer, arbiter, token, auto_release_seconds,
milestone_amounts, milestone_spec_hashes, settings, salt)` needs only the
client's signature. It deploys the escrow with the platform admin as its
admin and emits `created` with the new escrow's address.

## Deploy (Testnet)

### Escrow factory (what the app uses)

The app creates one escrow contract per job through `escrow-factory`'s
`create_escrow`. `scripts/deploy-factory-testnet.sh` builds the reproducible
escrow wasm and uploads it, deploys the factory with the platform admin and
that wasm hash, allows each token for new escrows, and appends the deployment
to `deployments/testnet-factory.json`:

```bash
export SOURCE_ACCOUNT=testnet-deployer    # pays for the deployment
export ADMIN_ADDRESS=G...                 # platform admin; admin of every escrow
export ADMIN_SOURCE=platform-admin        # stellar-cli key for ADMIN_ADDRESS
export ALLOWED_TOKENS=C...,C...           # token contracts new escrows may use
scripts/deploy-factory-testnet.sh
```

Then set the factory ID as the `FACTORY_CONTRACT_ID` repository variable in
escrow-frontend and as `FACTORY_CONTRACT_ID` in escrow-backend's
`backend.env`, and redeploy both. The script stops before uploading if the
escrow wasm is over the network's 131,072-byte contract size limit.

### A single escrow

`scripts/deploy-testnet.sh` builds the reproducible wasm, uploads it, deploys
with constructor arguments from environment variables, and appends
`{date, network, wasm_hash, contract_id, admin}` to `deployments/testnet.json`
(issue #745):

```bash
export SOURCE_ACCOUNT=testnet-deployer
export ADMIN_ADDRESS=G...
export CLIENT_ADDRESS=G...
export FREELANCER_ADDRESS=G...
export ARBITER_ADDRESS=G...
export TOKEN_ADDRESS=C...
export AUTO_RELEASE_SECONDS=2592000
export MILESTONE_AMOUNTS='["10000000"]'
export MILESTONE_SPEC_HASHES='["0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"]'
# Optional: FUNDING_MODE (Upfront), DISPUTE_TIMEOUT_SECONDS (2592000), ARBITER_FEE_BPS (0)
scripts/deploy-testnet.sh
```

The equivalent manual deploy:

```bash
scripts/build-reproducible.sh
stellar contract deploy \
  --wasm dist/milestone_escrow.wasm \
  --network testnet \
  --source <your-account> \
  -- \
  --admin <admin-address> \
  --client <client-address> \
  --freelancer <freelancer-address> \
  --arbiter <arbiter-address> \
  --token <token-address> \
  --auto_release_seconds 604800 \
  --milestone_amounts '["1000000"]' \
  --milestone_spec_hashes '["0000000000000000000000000000000000000000000000000000000000000000"]' \
  --settings '{"funding_mode":"Upfront","dispute_timeout_seconds":2592000,"arbiter_fee_bps":0}'
```

Record the `source_commit` and `wasm_sha256` from `dist/build-info.json` with
the deployment, and the hash in `CHANGELOG.md`.

### Upgrading a deployment: upgrade, then migrate

Upgrading is always two admin calls, in this order:

1. `upgrade(admin, new_wasm_hash)` installs the new code.
2. `migrate(admin)` rewrites stored entries into the layout the new code
   expects and records it.

The contract stores the layout version it was written with
(`schema_version()`), and the code declares the version it expects
(`SCHEMA_VERSION`). Between the two calls they differ, and every
state-changing endpoint returns `MigrationRequired` (error 51), so nothing
can write an entry in the old shape or act on one misread. Getters, `upgrade`
itself and the pause/unpause controls keep working, so an incident in that
window can still be contained. Once `migrate` succeeds (it emits `migrate`
with a `MigratedEvent`) normal operation resumes. Calling `migrate` again
returns `AlreadyMigrated` (error 52). Deployments created before the marker
existed are treated as schema 1.

Schema 2 adds `spec_hash` to every stored `Milestone` (#767; old milestones
get a zero hash) and moves the whitelist, multisig, yield and fee
configuration out of instance storage (#772). A release that changes a stored
type must bump `SCHEMA_VERSION` and add the step to `migrate`.
`migrate_cold_config` remains for deployments that only need the #772 move.

## Verifying the deployed contract

Anyone can check which source a deployed contract was built from:

1. Read the installed wasm hash:

   ```bash
   stellar contract fetch --id <contract-id> --network testnet --out-file deployed.wasm
   sha256sum deployed.wasm
   ```

   (`stellar contract info` and the explorer show the same hash.)
2. Look up `source_commit` for that contract in `deployments/<network>.json`.
3. Rebuild that commit and compare:

   ```bash
   scripts/build-reproducible.sh <source_commit>
   cat dist/milestone_escrow.wasm.sha256
   ```

The two hashes must be identical. If they differ, the deployed code is not the
reviewed source at that commit.

## License

MIT

## Deployed Contract

| Network | Contract ID |
|---|---|
| Testnet | `CDD5WKK3WT3QVKXMXTJNDIXE4T73FK6GGXDSD6UTJAH6YYZU52SQ4MUH` |

Deployment records, including the installed wasm hash, are in
[`deployments/`](deployments/). The current testnet deployment predates
reproducible builds, so its source commit is unknown.

Explorer: `https://stellar.expert/explorer/testnet/contract/CDD5WKK3WT3QVKXMXTJNDIXE4T73FK6GGXDSD6UTJAH6YYZU52SQ4MUH`
 
## Companion Repositories

- Frontend: [escrow-frontend](https://github.com/Goldii-locks/escrow-frontend) — Next.js and Tailwind web client.
- Backend: [escrow-backend](https://github.com/Goldii-locks/escrow-backend) — Express and TypeScript event indexer and transaction service.
