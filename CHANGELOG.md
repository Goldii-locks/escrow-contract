# Changelog

All notable changes to the Milestone Escrow Contract will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and every released or deployed build lists the contract address, source commit (if known),
and the SHA-256 hash of `milestone_escrow.wasm` produced by `scripts/build-reproducible.sh`.
See "Verifying the deployed contract" in the README.

## [Unreleased]

WASM: build with `scripts/build-reproducible.sh <commit>`; CI publishes the
hash for every commit in the `reproducible-wasm` job summary.

### Fixed

- `raise_dispute` now performs zero-address check, `require_auth`, and
  identity validation **before** writing the re-entrancy lock (#566), matching
  the authorization pattern used by `fund`, `approve_milestone`, and
  `resolve_dispute`.
- `lock_escrow_interest_yield` now rejects unauthorized callers with
  `Unauthorized` and an illegal source state with `InvalidStatus` **before** it
  reads or writes the ledger entry (#464); authorization runs first via
  `require_admin_from_instance`, so a non-admin cannot probe whether an escrow
  is locked.
- `cancel_escrow` single-party approvals now expire after
  `CANCEL_APPROVAL_TTL_SECONDS` (7 days, #713): an approval older than that is
  treated as absent, so the counter-party's call records a fresh first approval
  instead of locking the escrow. `CancelApprovalRecordedEvent` (`cxlappr`)
  carries `approved_at` so indexers can track the same window.
- `__constructor` rejects empty milestone lists, zero/negative amounts, zero
  addresses, zero auto-release windows, and party collisions (`admin == client`,
  `admin == freelancer`, `arbiter == client`, `arbiter == freelancer`,
  `client == freelancer`) with explicit contract errors.

### Security

- `propose_admin_transfer` and `transfer_admin` reject the contract's own
  address and the current admin as `new_admin` with `InvalidAddress` (#771).
- `set_escrow_interest_yield` (admin only) is closed once the escrow is funded
  and returns `AlreadyFunded`; after funding the yield split can only change
  through `set_interest_yield_consent`, which the client and freelancer must
  sign. `unlock_escrow_interest_yield` now requires the admin, the client and
  the freelancer, so the admin cannot undo its own lock (#776).
- `raise_dispute` and `request_revision` return the new `ReviewWindowClosed`
  error on a `Delivered` milestone once its auto-release deadline has passed,
  and `escalate_dispute` restores the original deadline instead of starting a
  fresh window, so the client cannot undo an expired review window (#991).
- `dispute_timeout_seconds` is bounded to `MIN_DISPUTE_TIMEOUT_SECONDS` (1 day)
  through `MAX_DISPUTE_TIMEOUT_SECONDS` (180 days); the escrow constructor and
  the factory's `create_escrow` return `InvalidDisputeTimeout` outside that
  range, so disputed funds cannot be locked by an unbounded timeout (#992).

### Changed

- **Breaking**: `resolve_dispute` and `apply_dispute_arbitration_split` take a
  trailing `decision_hash: BytesN<32>` — a tamper-evident commitment to the
  arbiter's written decision (#1015). The hash is echoed verbatim in the
  `resolve` event and, for a split, also in `arbsplit`. Outcomes, fund
  distribution, permissions and dispute storage are unchanged; the written
  decision stays off-chain in escrow-backend storage, keyed by the hash.
  Clients must pass the new argument (regenerate the TypeScript bindings to
  pick up the new signatures).
- `DisputeInfo` gains `raised_by: Address`, the party that called `raise_dispute`; `SCHEMA_VERSION` is now 4 and `migrate` adds the field to open disputes, using the job's client as a placeholder because the raiser was not stored (#997). Run `migrate` after upgrading an existing escrow.
- `fund_milestone` no longer rescans every milestone: a new `FundedCount` instance counter, kept up to date by `fund`, `fund_milestone` and `add_milestone`, decides when the job is fully funded in O(1). Escrows created before the counter fall back to counting once (#999).
- Factory `created` event now has topics `created`, `client`, `freelancer`, `arbiter` and data `{ escrow, token, total_amount, milestone_count, auto_release_seconds, settings, wasm_hash }`; party addresses moved out of the data. **Breaking for indexers**; see "Escrow factory events" in `EVENTS.md` (#1007).
- `context.md` removed; its useful history now lives under "Project history" below (#1005).
- `emergency_unpause_parties` publishes its `EmergencyPartyUnpausedEvent` under
  the new `emunpausp` topic instead of `emunpause`, which now carries only
  `EmergencyUnpausedEvent` from the admin unpause (#993).
- One admin pause flag (`Paused`) and the party emergency pause (`Ep`), both
  checked through one `is_paused` helper (#994).
  `emergency_pause_admin_override` is a deprecated alias that sets `Paused`;
  `emergency_unpause` lifts either pause; `is_emergency_paused`,
  `get_yield_info` and the emergency refund endpoints see both pauses;
  `cancel_escrow`, `extend_milestone_deadline` and the admin overrides are
  blocked by either pause. `SCHEMA_VERSION` is 5: `migrate` moves an
  admin-set `Ep` to `Paused`.
- `WhitelistedTokens`, `MultiSigConfig`, `InterestYieldState` and
  `PlatformFeeAllocation` moved from instance storage to persistent keys, so
  calls that do not use them stop loading them. With a 50-token whitelist and a
  32-signer multisig, the instance entry `approve_milestone` loads shrinks from
  4,212 to 644 bytes and its metered instructions drop by about 15 % (#772).
- `msfund`, `withdraw`, `sweep`, `overdue`, `unaccref`, `msrev`, `extend`,
  `ms_add`, `ms_reduce`, `cancel`, `cxlappr`, `cancelrev`, `acceptjob` and
  `acceptarb` carry the party addresses as topics after the event name, actor
  first, so `getEvents` can filter them by participant (#1019).
- `transfer_admin` and `execute_admin_transfer` clear every delegated
  `AdminRole` assignment, so all four roles revert to the incoming admin, and
  emit one `rolechg` event per cleared assignment before the `admin` /
  `adminexc` event (#955). Delegates appointed by an outgoing (possibly
  compromised) admin no longer keep `Upgrader` or `Resolver` after a rotation.
- E2E runs on the Protocol 30 quickstart `v695-b1655.1-latest` (stellar-core
  v30.0.0, stellar-rpc v30.0.1) with stellar-cli 30.0.1 (#961).

### Added

- `@goldii-locks/escrow-factory-bindings`, generated from `escrow_factory.wasm`
  and versioned, uploaded and published together with
  `@goldii-locks/milestone-escrow-bindings` (#965).
- Resource-cost test at `MAX_MILESTONES` for every entry point that walks all
  milestones, checked against the per-transaction network limits with 20%
  headroom (#963).

- `increase_milestone(client, freelancer, milestone_index, new_amount)`:
  raise a `Pending` milestone's amount with both parties' consent. On a
  funded milestone the client deposits the difference in the same call; in
  per-milestone mode an unfunded milestone only has its amount changed.
  Updates `total_amount` and emits `ms_incr` with the old and new amounts
  (#1020).
- Property-based tests (`proptest`) for conservation of funds, terminal state immutability,
  and failed-call balance isolation across arbitrary operation sequences (#722).
- `migrate(admin)` moves the configuration above out of instance storage when
  an existing deployment is upgraded from schema 1 (#772).
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
- `withdraw_to(caller, recipient)`: pay the amount owed to `caller` to a
  different `recipient` when `caller` can no longer receive the token (for
  example after the issuer deauthorized it). `recipient` must not be a zero
  address or the contract (`InvalidAddress`). Emits `withdraw` with `caller`
  as an extra topic (#1012).

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

## Project history (migrated from `context.md`)

`context.md` was a running session log and has been removed. The facts that are
still useful are kept here; the per-day frontend and backend notes are in the
git history of that file.

### Testnet contract ID lineage

| Contract ID | Period | Notes |
|---|---|---|
| `CBBRYWY6ROXCM6AHP4COM3AL6UDPTY66FXF43Q7PNEIPU53RZOGHBYP3` | 0.1.0 (2026-06-13) | First testnet deployment. |
| `CDD5WKK3WT3QVKXMXTJNDIXE4T73FK6GGXDSD6UTJAH6YYZU52SQ4MUH` | 0.2.0 (2026-06-20) | Superseded. Older docs and `escrow-backend/.env.example` may still list it. |
| `CARZQMYKDGA4TTIE2XBDJ42EX27VYLLJGGG4WGUIXXWIVHPDTX3UCLJA` | Current | Confirmed from production `/health` and the frontend `CONTRACT_ID` variable. |

The current deployment record is `deployments/testnet.json`.

### Campaigns

- **Drips Wave (2026-09-21 to 2026-10-03):** about 348 issues across `escrow-contract`, `escrow-backend` and `escrow-frontend`; 1,092 of 1,101 closed by the end.
- **Stellar Wave (2026-10-15 to 2026-10-27):** audit-based batch of 375 issues (125 per repo). Contract issues #664-#788. Each issue carries a `p1`/`p2`/`p3` priority and the `Stellar Wave` label. By 2026-10-27, 107 of 125 contract issues were closed.

### Platform and infrastructure notes

- **Event indexer:** the SQLite-backed indexer in `escrow-backend` indexes all contract events by ledger sequence (first added 2026-06-22).
- **Protocol 30 (2026-11-03):** CAP-88 changed the ledger header format and broke the indexer (`unknown StellarValueType member for value 3`). Fixed by upgrading `@stellar/stellar-sdk` in `escrow-backend` from `^15.1.0` to `^16.4.1`.
- **Production hosting:** milestonepay.app serves HTTPS, CORS is restricted to the `ALLOWED_ORIGINS` allowlist, and deploys run through `deploy.yml`. A deploy failed on 2026-11-09 because the host disk was full of old Docker images; `docker image prune -af` fixed it.

### Known gaps recorded at the time of removal

- The frontend sends CSP reports to `/api/csp-report` and the backend has no handler (frontend #685).
- The frontend's batched deadline call needs a backend `GET /api/jobs/:id/deadlines` route (frontend #671).
