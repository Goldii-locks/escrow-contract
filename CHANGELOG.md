# Changelog

All notable changes to the Milestone Escrow Contract will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and every released or deployed build lists the contract address, source commit (if known),
and the SHA-256 hash of `milestone_escrow.wasm` produced by `scripts/build-reproducible.sh`.
See "Verifying the deployed contract" in the README.

## [Unreleased]

WASM: build with `scripts/build-reproducible.sh <commit>`; CI publishes the
hash for every commit in the `reproducible-wasm` job summary.

### Security

- `propose_admin_transfer` and `transfer_admin` reject the contract's own
  address and the current admin as `new_admin` with `InvalidAddress` (#771).
- `set_escrow_interest_yield` (admin only) is closed once the escrow is funded
  and returns `AlreadyFunded`; after funding the yield split can only change
  through `set_interest_yield_consent`, which the client and freelancer must
  sign. `unlock_escrow_interest_yield` now requires the admin, the client and
  the freelancer, so the admin cannot undo its own lock (#776).

### Fixed

- `approve_milestone` and `claim_auto_release` now accept `PartiallyReleased` milestones in addition to `Delivered`, enabling clients to approve remaining milestone balances in one call and ensuring freelancers can auto-release funds if clients go silent after a partial payment (#672, #673).
- `time_until_auto_release` now bounds-checks milestone indices with `InvalidMilestone` and enforces `Delivered` or `PartiallyReleased` status with `InvalidStatus`, preventing misleading overdue reports on uncompleted milestones (#678).

### Changed

- `WhitelistedTokens`, `MultiSigConfig`, `InterestYieldState` and
  `PlatformFeeAllocation` moved from instance storage to persistent keys, so
  calls that do not use them stop loading them. With a 50-token whitelist and a
  32-signer multisig, the instance entry `approve_milestone` loads shrinks from
  4,212 to 644 bytes and its metered instructions drop by about 15 % (#772).

### Added

- Property-based tests (`proptest`) for conservation of funds, terminal state immutability,
  and failed-call balance isolation across arbitrary operation sequences (#722).
- `migrate_cold_config(admin)`: run once after upgrading an existing deployment
  to move the configuration above out of instance storage. Reads fall back to
  the old location until then (#772).
- Reproducible WASM builds: `scripts/build-reproducible.sh`, the pinned
  `build/reproducible.Dockerfile`, a CI job that publishes the hash per commit,
  and `deployments/testnet.json` (#773).
- `add_milestone(client, freelancer, amount)` and
  `reduce_milestone(client, freelancer, milestone_index, new_amount)`: amend a
  funded job with both parties' consent. `add_milestone` deposits the new
  milestone's amount from the client in the same call and is capped at
  `MAX_MILESTONES` (50, new `TooManyMilestones` error); `reduce_milestone`
  only applies to `Pending` milestones and refunds the difference to the
  client. Both emit `ms_add` / `ms_reduce` events and preserve the
  conservation invariant `balance == total_amount == Σ milestone amounts`
  (#737).
- TypeScript bindings generated from the built wasm in CI for every main
  build (artifact) and published as the versioned GitHub Package
  `@goldii-locks/milestone-escrow-bindings` on `v*` tags; consumption guide in
  `docs/typescript-bindings.md` (#744).

## [0.2.0] - 2026-06-20

### Deployed Contract (Testnet)

- Contract ID: `CDD5WKK3WT3QVKXMXTJNDIXE4T73FK6GGXDSD6UTJAH6YYZU52SQ4MUH`
- WASM SHA-256: `67d0eb543aaa9ca99c1b23ba18a3f4713c899eba574bbef96540f2540b8b2f0e`
- Explorer: [StellarExpert Testnet](https://stellar.expert/explorer/testnet/contract/CDD5WKK3WT3QVKXMXTJNDIXE4T73FK6GGXDSD6UTJAH6YYZU52SQ4MUH)

### Added

- **Partial/Installment Releases**: Added `approve_partial` endpoint and `released_amount` field on `Milestone` struct, enabling gradual milestone payouts.
- **Time-Locked Auto-Release**: Added `claim_auto_release` and `time_until_auto_release` endpoints; added `delivered_at` on `Milestone` and `auto_release_seconds` on `Job`.
- **Token Whitelist Management**: Added `add_whitelisted_token`, `remove_whitelisted_token`, and `get_whitelisted_tokens` admin functions with strict capacity limits.
- **Acceptance Gate**: Added `accept_job` (freelancer) and `accept_arbiter` (arbiter) requirements before delivery or disputes can proceed, along with `withdraw_unaccepted` for client fund recovery.
- **Arbitration Splits**: Added `apply_dispute_arbitration_split` and `MilestoneStatus::Split` status for proportional dispute payouts.
- **Tax Withholding**: Added `tax_withholding_deductions` and admin deduction endpoints with strict basis-point validation.
- **Platform Fee Allocation**: Added configurable fee sharing across client, freelancer, and treasury via `set_platform_fee_allocation`.
- **Yield Integration**: Added `set_escrow_interest_yield`, `set_interest_yield_consent`, and `admin_accrue_yield`.
- **Emergency and Cancellation Controls**: Added multi-party `cancel_escrow`, `emergency_pause`, `emergency_unpause_parties`, and admin override resolution endpoints.
- **Surplus Sweep**: Added `sweep_excess` for recovering excess non-obligation tokens.

### Changed

- Migrated contract to Soroban SDK `28.0.0-rc.1`.
- Upgraded `initialize` signature to include `auto_release_seconds` and spec hash commitments.

## [0.1.0] - 2026-06-13

### Deployed Contract (Testnet)

- Contract ID: `CBBRYWY6ROXCM6AHP4COM3AL6UDPTY66FXF43Q7PNEIPU53RZOGHBYP3`
- Explorer: [StellarExpert Testnet](https://stellar.expert/explorer/testnet/contract/CBBRYWY6ROXCM6AHP4COM3AL6UDPTY66FXF43Q7PNEIPU53RZOGHBYP3)

### Added

- Initial release of the milestone escrow smart contract on Stellar Testnet.
- Four distinct operational roles: Admin, Client, Freelancer, and Arbiter.
- Core escrow lifecycle endpoints:
  - `initialize`: set up job milestones with upfront funding.
  - `fund`: deposit full milestone balance into escrow.
  - `mark_delivered`: signal milestone completion by freelancer.
  - `approve_milestone`: release milestone funds to freelancer.
  - `request_revision`: request revisions on delivered milestones (capped at 3).
  - `raise_dispute`: freeze milestone and request arbiter intervention.
  - `resolve_dispute`: arbiter release or refund resolution.
  - `get_job`: view current job state.
