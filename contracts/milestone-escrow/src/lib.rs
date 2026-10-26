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
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, BytesN,
    ContractExecutable, Env, IntoVal, Map, Symbol, TryFromVal, Val, Vec,
};

pub mod math;

/// Maximum number of ratio slots accepted by `compute_ratio_allocation`.
/// A multisig setup with more than this many signers is operationally
/// unreasonable and would impose unbounded per-transaction CPU costs; the cap
/// guarantees that the nested loop over `ratios` iterates at most
/// `MAX_MULTISIG_RATIO_COUNT` times both during validation and during
/// the largest-remainder allocation phase.
const MAX_MULTISIG_RATIO_COUNT: u32 = 255;

/// Maximum number of tokens that may be held in the whitelist at any one time.
/// `add_whitelisted_token` enforces this cap before calling `push_back` so
/// that the internal `u32` length counter of the Soroban `Vec` can never
/// overflow regardless of how many times the function is invoked.
const MAX_WHITELIST_SIZE: u32 = 50;

/// Maximum number of parties that may share an emergency-pause allocation.
/// `emergency_pause_allocation` runs a nested loop over the weight
/// vector during the largest-remainder phase, so the cap bounds its worst-case
/// CPU cost and keeps the `u32` party counter from overflowing.
const MAX_EMERGENCY_ALLOCATION_PARTIES: u32 = 255;

/// Default window before either party may return an unresolved dispute to its
/// pre-dispute milestone state.
pub const DISPUTE_TIMEOUT_SECONDS: u64 = 30 * 24 * 60 * 60;
/// Maximum number of milestones `approve_milestones` accepts in one call.
/// Validation does a duplicate scan over the index list, so the cap bounds
/// its worst-case CPU cost along with the per-milestone storage writes.
const MAX_BATCH_APPROVALS: u32 = 10;

/// Maximum age of a single-party cancel approval, in seconds (7 days).
///
/// `cancel_escrow` is a two-party handshake: the first party records its intent
/// and the counter-party completes the `CancelLock`.  Without an expiry that
/// intent never dies, so an approval recorded in week one during a
/// disagreement could be completed months later — after the parties made up and
/// kept working — freezing the escrow on stale intent (issue #713).
///
/// Once an approval reaches this age it is treated as absent: the next
/// `cancel_escrow` call records a fresh first approval instead of locking, and
/// the TTL window restarts from that call's ledger timestamp.
pub const CANCEL_APPROVAL_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Maximum allowed value for `auto_release_seconds` (10 years).  Prevents
/// arithmetic overflow in deadline calculations (e.g. `claim_auto_release`)
/// and avoids absurdly long auto-release windows.
pub const MAX_AUTO_RELEASE_SECONDS: u64 = 10 * 365 * 24 * 60 * 60;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// `initialize` was called more than once.
    AlreadyInitialized = 1,
    /// A call required the contract to be initialized but it was not.
    NotInitialized = 2,
    /// `fund` was called after the escrow was already funded.
    AlreadyFunded = 3,
    /// An operation that requires the escrow to be funded was attempted while it was not.
    NotFunded = 4,
    /// Caller is not authorized to perform this action.
    Unauthorized = 5,
    /// Milestone index is out of bounds or refers to an invalid milestone.
    InvalidMilestone = 6,
    /// Operation is not allowed for the current milestone status.
    InvalidStatus = 7,
    /// The provided token address is not whitelisted by the admin.
    TokenNotWhitelisted = 8,
    /// Attempted to whitelist a token that is already whitelisted.
    TokenAlreadyWhitelisted = 9,
    /// Amount is zero, negative, or otherwise invalid for this operation.
    InvalidAmount = 10,
    /// Required deadline/timeout has not yet passed.
    DeadlineNotPassed = 11,
    /// Provided address is invalid.
    InvalidAddress = 12,
    /// Contract is paused and this operation is not allowed while paused.
    Paused = 13,
    /// Ratio sum is incorrect (must sum to BPS_SCALE) or values are out of range.
    InvalidRatio = 14,
    /// Milestone time extension is invalid (e.g. non-positive).
    InvalidExtension = 15,
    /// Escrow is locked (e.g. dispute/cancel lock or interest-yield lock) and operation is blocked.
    EscrowLocked = 16,
    /// Multisig setup requires at least one signer.
    MultiSigNoSigners = 17,
    /// Multisig setup exceeds the maximum allowed number of signers.
    MultiSigTooManySigners = 18,
    /// Multisig threshold is invalid (must be > 0 and <= number of signers).
    MultiSigInvalidThreshold = 19,
    /// Multisig setup contains duplicate signers.
    MultiSigDuplicateSigner = 20,
    /// A new `propose_admin_transfer` was attempted while one was already
    /// pending execution or cancellation.
    AdminTransferPending = 21,
    /// `execute_admin_transfer` / `cancel_admin_transfer_proposal` was called
    /// with no proposal currently pending.
    NoPendingAdminTransfer = 22,
    /// `execute_admin_transfer` was called before the proposal's multisig
    /// approval threshold was reached.
    MultiSigThresholdNotMet = 23,
    /// `raise_dispute` was re-entered for a milestone whose dispute lock is
    /// still held, i.e. a dispute is already being raised in this transaction.
    DisputeAlreadyRaised = 24,
    /// Retained for error-code stability — no longer emitted.  The guard it
    /// named (issue #787) was removed from `multisig_approve`: an approval
    /// records signer intent and moves no tokens, so it is accepted on an
    /// unfunded escrow.  Kept at `25` so off-chain decoders of the existing
    /// code space stay valid.
    MultiSigEmptyBalance = 25,
    /// A guarded endpoint was called while `tax_withholding_deductions` is
    /// mid-execution and holds `DataKey::TaxWithholdingExecutionLock`.
    TaxWithholdingInProgress = 26,
    /// A guarded endpoint was called while a platform-fee allocation is
    /// mid-execution and holds `DataKey::PlatformFeeAllocationLock`.
    PlatformFeeAllocationInProgress = 27,
    /// A guarded endpoint was called while an emergency pause transition is
    /// mid-execution and holds `DataKey::EpLk`.
    EmergencyPauseInProgress = 28,
    /// `emergency_pause` was called while the contract is already paused.
    /// Re-pausing is rejected rather than silently no-opping so an operator
    /// cannot mistake a redundant call for having taken fresh action.
    AlreadyPaused = 29,
    /// `emergency_unpause`, or a pause-gated endpoint such as
    /// `emergency_pause_claim_refund`, was called while the contract is
    /// not paused.
    NotPaused = 30,
    /// A weight vector passed to `emergency_pause_allocation` was
    /// empty, exceeded the party cap, contained a negative weight, or summed
    /// to zero.
    InvalidAllocationWeights = 31,
    /// An emergency refund / pause-gated endpoint was called while the
    /// contract holds zero token balance, so there is nothing to settle.
    EmptyBalance = 32,
    /// A guarded endpoint was called while `milestone_time_extensions` is
    /// mid-execution and holds `DataKey::TimeExtExecutionLock`.
    TimeExtInProgress = 33,
    /// A guarded endpoint was called while `payment_streaming_milestones` is
    /// mid-execution and holds `DataKey::PaymentStreamingExecutionLock`.
    PaymentStreamingInProgress = 34,
    /// A platform-fee allocation exceeded its per-party cap: treasury above
    /// `MAX_TREASURY_FEE_BPS` or client above `MAX_CLIENT_FEE_BPS`.
    FeeTooHigh = 35,
    /// A checked `i128` operation inside a ratio/allocation computation would
    /// have overflowed or underflowed.
    ///
    /// Ratio maths (platform-fee splits, refund splits, streaming splits,
    /// emergency-pause allocations) is reached with caller-supplied amounts, so
    /// an `i128::MAX` total combined with a non-zero basis-point weight is a
    /// reachable input. Every such step is evaluated with `checked_mul`,
    /// `checked_add`, `checked_sub`, `checked_div`, and `checked_rem`; when one
    /// of them cannot be represented the endpoint aborts with this variant
    /// instead of panicking (release builds abort on overflow) or silently
    /// wrapping and returning a nonsensical split.
    ///
    /// This variant is deliberately distinct from `InvalidAmount`: the input
    /// was not merely out of range, the arithmetic itself was unrepresentable.
    /// It is also distinct from `NotInitialized`, so an indexer can tell a
    /// failed calculation apart from a missing configuration.
    ArithmeticOverflow = 36,
    /// A settlement asked to distribute more than the contract currently
    /// holds in the escrowed token.
    InsufficientBalance = 37,
    /// A dispute timeout must be greater than zero.
    InvalidDisputeTimeout = 38,
    /// The configured dispute timeout has not elapsed yet.
    DisputeTimeoutNotReached = 39,
    /// `fund` was called on a PerMilestone escrow, or `fund_milestone`
    /// was called on an Upfront escrow.
    InvalidFundingMode = 40,
    /// `request_revision` was called after `MAX_REVISIONS` revisions.
    MaxRevisions = 41,
    /// `sweep_excess` found no recoverable surplus.
    NothingToSweep = 42,
    /// A delivery or dispute endpoint was called before both the freelancer
    /// (`accept_job`) and the arbiter (`accept_arbiter`) accepted the job.
    NotAccepted = 43,
    /// `initialize` was called with overlapping party addresses.  The escrow
    /// trust model requires four distinct addresses: the admin (platform),
    /// client, freelancer, and arbiter (neutral dispute resolver) must all
    /// be different.  Any collision — admin == client, admin == freelancer,
    /// arbiter == client, arbiter == freelancer, or client == freelancer —
    /// removes an independent check-and-balance and is rejected.
    InvalidParties = 44,
    /// `add_milestone` was called on a job that already carries
    /// `MAX_MILESTONES` milestones.
    TooManyMilestones = 45,
    /// `submit_evidence` was called after `MAX_DISPUTE_EVIDENCE_ENTRIES`
    /// hashes had already been recorded for the milestone's current dispute.
    EvidenceLimitReached = 46,
    /// `auto_release_seconds` is zero or above `MAX_AUTO_RELEASE_SECONDS`.
    InvalidDuration = 47,
    /// The whitelist already contains `MAX_WHITELIST_SIZE` (50) entries.
    WhitelistFull = 48,
    /// Removing a token would leave the whitelist empty (at least one token must remain).
    WhitelistMinimum = 49,
    /// A ratio vector exceeds the maximum allowed number of parties (255).
    TooManyParties = 50,
    /// A state-changing endpoint was called on a deployment whose storage is
    /// older than this code (`schema_version() < SCHEMA_VERSION`), i.e. after
    /// `upgrade` but before `migrate`.  The admin must call `migrate` first.
    MigrationRequired = 51,
    /// `migrate` was called on storage that is already at `SCHEMA_VERSION`.
    AlreadyMigrated = 52,
    /// `propose_multisig_config` was called while another signer-set change
    /// is pending; execute or cancel it first.
    MultiSigConfigPending = 53,
    /// `execute_multisig_config` / `cancel_multisig_config_proposal` found no
    /// pending signer-set change.
    NoPendingMultiSigConfig = 54,
    /// `replace_arbiter` was called while at least one milestone is in the
    /// `Disputed` state.  The sitting arbiter is the party authorised to
    /// conclude an open dispute (`resolve_dispute` /
    /// `apply_dispute_arbitration_split` both verify the stored
    /// `meta.arbiter`), so the client and freelancer may only jointly swap the
    /// arbiter once every dispute has been resolved, split, or timed out and
    /// escalated.
    DisputePending = 55,
}

const MAX_REVISIONS: u32 = 3;

/// Maximum number of evidence hashes a single dispute may accumulate through
/// `submit_evidence`.  The cap bounds the persistent `DisputeInfo` value so a
/// party cannot grow it without limit (and therefore cannot inflate the
/// ledger-rent footprint of the dispute record) by submitting repeatedly.
pub const MAX_DISPUTE_EVIDENCE_ENTRIES: u32 = 10;

const MAX_TREASURY_FEE_BPS: u32 = 2000;
const MAX_CLIENT_FEE_BPS: u32 = 5000;

/// Upper bound on the number of milestones a job may carry, including
/// milestones appended after funding by `add_milestone` (issue #737).
/// Keeps `get_job` and every whole-set iteration bounded
/// so the ledger footprint of amendment endpoints stays predictable.
const MAX_MILESTONES: u32 = 50;
/// Seconds after funding during which the freelancer and arbiter may accept
/// the job. Once it has passed without both acceptances, the client can
/// reclaim the escrow with `withdraw_unaccepted`.
const ACCEPTANCE_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;

const BPS_SCALE: u32 = 10_000;
// Keep active escrow state available for roughly one year of ledger time.
const TTL_THRESHOLD: u32 = 30 * 24 * 60 * 5;
const TTL_EXTEND_TO: u32 = 365 * 24 * 60 * 5;

/// Maximum arbiter fee in basis points (1,000 = 10 %), issue #738.
const MAX_ARBITER_FEE_BPS: u32 = 1_000;

/// Seconds in a standard Gregorian year (365 days). Used by the simple-interest
/// estimator in `escrow_interest_yield`.
const SECONDS_PER_YEAR: i128 = 31_536_000;

/// Basis-point denominator for interest math (`1 bp = 1 / 10_000`).
const BPS_DENOMINATOR: i128 = 10_000;

/// Storage layout this code reads and writes.  Bump it whenever a stored
/// type changes shape, and teach `migrate` the step up from the previous
/// layout.
///
/// * `1` – `Milestone` without `spec_hash`; whitelist, multisig, yield and fee
///   configuration in instance storage.  Deployments that predate
///   `DataKey::SchemaVersion` are read as this version.
/// * `2` – `Milestone::spec_hash` (#767); that configuration in persistent
///   storage (#772).
/// * `3` – `Milestone::deliverable`, the hash recorded by
///   `mark_delivered_with_hash`.
pub const SCHEMA_VERSION: u32 = 3;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MilestoneStatus {
    Pending,
    Delivered,
    PartiallyReleased,
    Released,
    Disputed,
    Refunded,
    Split,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Milestone {
    pub amount: i128,
    pub released_amount: i128,
    pub status: MilestoneStatus,
    pub delivered_at: u64,
    pub funded: bool,
    /// Hash of the milestone's off-chain description or acceptance criteria,
    /// committed at `initialize` (#767). Kept on the milestone itself so
    /// committing it costs no extra ledger write per milestone.
    pub spec_hash: BytesN<32>,
    /// Hash of the artifact recorded by the most recent delivery (e.g. the
    /// SHA-256 of the artifact or an IPFS CID digest). All zeroes until the
    /// milestone is delivered with a hash.
    pub deliverable: BytesN<32>,
}

/// `Milestone` as stored by schema version 1, before `spec_hash` (#767).
/// Only `migrate` reads it.
#[contracttype]
#[derive(Clone, Debug)]
struct MilestoneV1 {
    amount: i128,
    released_amount: i128,
    status: MilestoneStatus,
    delivered_at: u64,
    funded: bool,
}

/// `Milestone` as stored by schema version 2, before `deliverable`.
/// Only `migrate` reads it.
#[contracttype]
#[derive(Clone, Debug)]
struct MilestoneV2 {
    amount: i128,
    released_amount: i128,
    status: MilestoneStatus,
    delivered_at: u64,
    funded: bool,
    spec_hash: BytesN<32>,
}

/// Evidence attached to a dispute by one of the parties (#712).
///
/// `evidence_hash` is a hash of an off-chain statement or evidence bundle —
/// the contract never sees the underlying document, only its commitment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceRecord {
    /// The escrow party (client or freelancer) that submitted the hash.
    pub submitter: Address,
    /// Commitment to the off-chain evidence bundle.
    pub evidence_hash: BytesN<32>,
    /// Ledger timestamp at which the submission was recorded.
    pub submitted_at: u64,
}

/// Persistent record of an open dispute (#712).
///
/// Written by `raise_dispute`, read by `escalate_dispute` and exposed through
/// `get_dispute_info`. Kept as one struct-valued key so raising a dispute costs
/// a single persistent write.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeInfo {
    /// Ledger timestamp at which `raise_dispute` recorded the dispute.
    pub disputed_at: u64,
    /// Milestone status to restore if the dispute is escalated after timeout.
    pub previous_status: MilestoneStatus,
    /// Commitment to the reason/evidence bundle supplied by the raiser.
    pub reason_hash: BytesN<32>,
    /// Evidence hashes added by either party while the milestone is Disputed.
    pub evidence: Vec<EvidenceRecord>,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Job {
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub milestones: Vec<Milestone>,
    pub funded: bool,
    pub auto_release_seconds: u64,
}

/// Both pause switches. `emergency` is `DataKey::Ep` (`emergency_pause`);
/// `admin` is `DataKey::Paused` (the admin pause also reported by
/// `get_yield_info`).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowPausedState {
    pub emergency: bool,
    pub admin: bool,
}

/// Mutual-cancellation state: `locked` is `DataKey::CancelLock`, and
/// `approval_mask` is `DataKey::CancelApproval` (bit 1 = client approved,
/// bit 2 = freelancer approved).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCancelState {
    pub locked: bool,
    pub approval_mask: u32,
}

/// Per-milestone timing, index-aligned with `Job::milestones`.
/// `deadline` is the auto-release deadline (delivery time +
/// `auto_release_seconds` + extension); `None` unless the milestone is
/// `Delivered`, the only status auto-release applies to.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneTiming {
    pub extension_seconds: u32,
    pub deadline: Option<u64>,
}

/// Everything a UI needs to render an escrow, in one read (`get_escrow_state`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowState {
    pub job: Job,
    pub admin: Address,
    pub paused: EscrowPausedState,
    pub cancel: EscrowCancelState,
    pub milestone_timing: Vec<MilestoneTiming>,
    /// Whether a platform-fee allocation has been configured; when `false`,
    /// `fee_allocation` is all zeros.
    pub fee_allocation_set: bool,
    pub fee_allocation: PlatformFeeAllocation,
    pub version: u32,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FundingMode {
    Upfront,
    PerMilestone,
}

/// Per-escrow settings fixed at deployment by `__constructor` (issue #665).
///
/// They used to be set by separate `initialize_with_*` entry points, which a
/// constructor-deployed contract can no longer call: the constructor has
/// already initialised it.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EscrowSettings {
    /// `Upfront` funds the whole job at once; `PerMilestone` funds each
    /// milestone separately (issue #766).
    pub funding_mode: FundingMode,
    /// Seconds after which a dispute may be escalated (issue #784). Must be
    /// non-zero; `DISPUTE_TIMEOUT_SECONDS` is the usual value.
    pub dispute_timeout_seconds: u64,
    /// Arbiter fee on dispute resolution, in basis points, at most
    /// `MAX_ARBITER_FEE_BPS` (issue #738). 0 means no fee.
    pub arbiter_fee_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
struct JobMeta {
    client: Address,
    freelancer: Address,
    arbiter: Address,
    token: Address,
    created_at: u64,
    funded: bool,
    auto_release_seconds: u64,
    milestone_count: u32,
    total_amount: i128,
}

/// Result of a split-refund allocation between client and freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundAllocation {
    pub client_refund: i128,
    pub freelancer_payout: i128,
    pub client_refund_bps: u32,
    pub freelancer_payout_bps: u32,
}

/// Result of a split-refund fee distribution, detailing the net amounts and fee shares.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitRefundFeeDistribution {
    pub client_net_refund: i128,
    pub client_fee_share: i128,
    pub freelancer_net_payout: i128,
    pub treasury_fee_share: i128,
}

/// Narrower administrative roles (issue #761). Each defaults to the root
/// admin until `set_admin_role` assigns it to another address.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminRole {
    /// `upgrade`, `migrate`, `migrate_cold_config`.
    Upgrader,
    /// Pause and resume, including the emergency pause controls.
    Operator,
    /// Fees, yield, tax settings and the token whitelist.
    FeeManager,
    /// Overrides of stuck payouts, and multisig lock / setup.
    Resolver,
}

#[contracttype]
pub enum DataKey {
    Job,
    Milestone(u32),
    Admin,
    Version,
    WhitelistedTokens,
    /// Instance: whether the escrow is emergency-paused.  Short key `Ep`
    /// (2 chars vs 16) to minimise on-ledger symbol bytes.
    Ep,
    PlatformFeeAllocation,
    /// Temporary key: records the ledger timestamp at which a milestone was
    /// marked delivered.  Written by `mark_delivered`, consumed by
    /// `claim_auto_release` and `time_until_auto_release`.  Uses temporary
    /// storage because it is single-use, deadline-scoped workflow state whose
    /// ledger footprint cost should not persist beyond the auto-release window.
    DeliveredAt(u32),
    /// Temporary key: written by `approve_milestone` when a milestone reaches
    /// the terminal `Released` state via a full approval.  Acts as a cheap
    /// short-lived completion signal so callers can confirm terminal state
    /// without loading the full persistent `Milestone` entry.  Uses temporary
    /// storage because the signal is transient: once the milestone is released,
    /// the approval workflow for that milestone is permanently closed and this
    /// flag has no further use.
    MilestoneReleased(u32),
    /// Temporary: lock set when dispute arbitration split execution is in
    /// progress for a milestone to prevent reentrant or concurrent mutations.
    DisputeLock(u32),
    Reputation(Address),
    /// Instance: client/freelancer yield-share configuration and execution lock
    /// for the `escrow_interest_yield` module. Written by
    /// `set_escrow_interest_yield`; read by getters and lock/unlock helpers.
    InterestYieldState,
    // ── escrow_interest_yield admin-override keys ────────────────────────────
    /// Persistent: holds the `YieldConfig` struct (annual yield rate in basis
    /// points, 1 bp = 0.01 %, range 0–10 000 / 0 %–100 %).  Written by
    /// `admin_set_yield_rate`, read by `get_yield_info` and `admin_accrue_yield`.
    /// Consolidated into a single struct-valued key to minimise the ledger
    /// footprint versus one key per field.
    YieldConfig,
    /// Persistent: total interest (in token stroops) accrued so far by the
    /// admin via `admin_accrue_yield`.  Reset to zero on admin override release
    /// or refund so downstream indexers can detect a fresh yield cycle.
    YieldAccrued,
    /// Persistent: boolean flag set to `true` by `admin_pause_escrow` and
    /// cleared by `admin_resume_escrow`.  When `true`, the guard in
    /// `assert_not_paused` blocks all normal user-facing endpoints (fund,
    /// mark_delivered, approve_milestone, approve_partial, claim_auto_release,
    /// raise_dispute, resolve_dispute) so that an emergency admin investigation
    /// cannot be interfered with.
    Paused,
    /// Temporary key: written by `raise_dispute` when a milestone enters the
    /// `Disputed` state.  Acts as a cheap short-lived signal so that callers
    /// can verify dispute status without loading the full persistent
    /// `Milestone` entry.  Uses temporary storage because the dispute workflow
    /// is transient: once resolved, the flag has no further use and its ledger
    /// footprint should not persist.
    DisputeFlag(u32),
    /// Persistent: boolean flag set to `true` when the multisig approval
    /// workflow enters a locked condition that requires admin intervention.
    /// Written by multisig-related functions when a deadlock is detected,
    /// cleared by `multisig_admin_override_release` or
    /// `multisig_admin_override_refund`.
    MultisigLocked,
    /// Persistent key: cumulative extension seconds applied to a Delivered
    /// milestone. Written by `extend_milestone_deadline`, read by
    /// `claim_auto_release` and `time_until_auto_release`. Stored in persistent
    /// storage so the extension survives TTL eviction and remains valid until
    /// the milestone reaches a terminal state.
    ///
    /// Short key `TimeExt` (7 chars vs 22) to minimise on-ledger symbol bytes.
    TimeExt(u32),
    /// Instance key for the cancel_escrow lock.
    CancelLock,
    /// Persistent: the pending single-party cancel approval, stored as a
    /// [`CancelApprovalRecord`] holding both the bitmask and the ledger
    /// timestamp at which the round started.  Bit 0 (value 1) = client has
    /// approved; bit 1 (value 2) = freelancer has approved.  Written by
    /// `cancel_escrow` when a party signals intent, cleared when both bits are
    /// set (lock fires) or when `revoke_cancel_approval` removes a party's bit.
    /// A record older than [`CANCEL_APPROVAL_TTL_SECONDS`] is treated as absent
    /// by `cancel_escrow` (issue #713).
    CancelApproval,
    // ── tax_withholding_deductions storage keys ──────────────────────────────
    /// Persistent: tax rate in basis points (1 bp = 0.01 %) set by
    /// `admin_set_tax_rate`.  Range 0–10 000 (0 %–100 %).
    TaxRate,
    /// Persistent per-milestone: written by `tax_withholding_deductions` when
    /// tax has been computed and the milestone is pending admin resolution.
    /// Stores a `TaxWithholdingRecord` containing the computed net payout and
    /// withheld tax amount.  Cleared by the admin override endpoints once
    /// the locked condition is resolved.
    TaxWithholdingLock(u32),
    // ── multisig approval compact storage keys ─────────────────────────────
    /// **Superseded (issue #456).**  Previously held the registered multisig
    /// signer set on its own.  `multisig_approval_init` now persists the signer
    /// set *and* the approval threshold together in the single consolidated
    /// `DataKey::MultiSigConfig` entry, so this key is no longer written.  It
    /// is still read as a fallback by `load_multisig_config` (and probed by the
    /// `multisig_approval_init` already-initialised guard) so a contract
    /// initialised before the change and then upgraded keeps its multisig
    /// regime.  The variant is kept, and no variant is reordered, so existing
    /// variant discriminants stay stable — serialization compatibility for
    /// already-written ledger entries.
    MultiSigSigners,
    /// **Superseded (issue #456).**  See `MultiSigSigners` above: the approval
    /// threshold now lives inside the consolidated `DataKey::MultiSigConfig`
    /// entry instead of occupying its own storage key; read only as the legacy
    /// fallback in `load_multisig_config`.
    MultiSigThreshold,
    /// Legacy approval bitmap for a given proposal index, no longer read or
    /// written: approvals live in the persistent `AdminProposal` record
    /// (issue #785), which also keeps them past TTL expiry for M-of-N
    /// governance that spans days (issue #716).  Each bit position corresponds
    /// to a signer index in the
    /// `MultiSigSigners` vec; a set bit means that signer has approved.
    /// The `u32` value is treated as a bitset, supporting up to 32 signers.
    /// Key type: `u32` (the proposal index) — significantly smaller than a
    /// composite `(Address, u32)` alternative.
    MultiSigApproval(u32),
    // ── dispute_arbitration_split compact storage keys ─────────────────────
    /// Temporary: records the client-refund BPS applied by
    /// `apply_dispute_arbitration_split` for a given milestone.
    ///
    /// Key type is only `u32` (the milestone index) — no `Address` payload —
    /// so the ledger key footprint stays minimal compared to a composite
    /// `(Address, u32)` alternative.  Presence of the entry signals that a
    /// split was applied; the value is a single `u32` BPS rather than a full
    /// `RefundAllocation` (2×i128 + 2×u32).  Freelancer BPS is derived as
    /// `BPS_SCALE - value`, removing redundant stored bytes.  Uses temporary
    /// storage so the footprint is auto-evicted after the dispute workflow.
    ///
    /// Appended at the end of `DataKey` so existing variant discriminants stay
    /// stable (serialization compatibility for already-written ledger entries).
    ArbitrationSplitBps(u32),
    /// Persistent: records an in-flight multisig-gated admin transfer
    /// proposal created by `propose_admin_transfer`. Presence of this key
    /// blocks any further `propose_admin_transfer` calls until the pending
    /// proposal is executed (`execute_admin_transfer`) or cancelled
    /// (`cancel_admin_transfer_proposal`), so the signer approvals already
    /// collected can never be silently redirected to a different
    /// `new_admin` mid-flight.
    ///
    /// Appended at the end of `DataKey` so existing variant discriminants
    /// stay stable (serialization compatibility for already-written ledger
    /// entries).
    PendingAdminTransfer,
    // ── execution locks (see `assert_no_*_in_progress` guards) ─────────────
    //
    // These three are unit variants holding a `bool`, set for the duration of
    // a single admin operation and cleared before it returns. They exist so
    // that concurrent/reentrant calls observe the in-progress state and bail
    // out rather than interleaving state mutations.
    //
    // Appended at the end of `DataKey` so existing variant discriminants
    // stay stable (serialization compatibility for already-written ledger
    // entries).
    /// Instance: held while `tax_withholding_deductions` executes.
    ///
    /// Distinct from `TaxWithholdingLock(u32)` above, which is a *per-milestone
    /// record* rather than an execution lock. The two arrived from separate
    /// PRs that both chose the name `TaxWithholdingLock`; this one is renamed
    /// to keep both behaviours.
    TaxWithholdingExecutionLock,
    /// Instance: held while a platform-fee allocation executes.
    PlatformFeeAllocationLock,
    /// Instance: held while an emergency pause/resume transition executes.
    /// Short key `EpLk` (4 chars vs 19) to minimise on-ledger symbol bytes.
    EpLk,
    /// Instance: held while `milestone_time_extensions` executes.
    TimeExtExecutionLock,
    /// Instance: held while `payment_streaming_milestones` (or its consent
    /// counterpart) executes.
    PaymentStreamingExecutionLock,
    /// Instance: the **consolidated multisig configuration** — the registered
    /// signer set and the approval threshold — held as a single `MultiSigConfig`
    /// value under this one key (issue #456).  Written once by
    /// `multisig_approval_init`; read by `multisig_approve` and by
    /// `read_multisig_approval` behind `is_multisig_approved`.  One invocation
    /// therefore touches one distinct multisig storage key instead of the two
    /// separate `MultiSigSigners` / `MultiSigThreshold` entries it replaced,
    /// and the contract-instance entry needs 28 bytes less for a three-signer
    /// set (the removed key/value pair costs more than the tuple wrapper the
    /// consolidated value adds).
    ///
    /// Appended at the end of `DataKey` so existing variant discriminants stay
    /// stable (serialization compatibility for already-written ledger entries).
    MultiSigConfig,
    /// Persistent: records a dispute start time and prior milestone status.
    /// Appended to preserve existing key discriminants.
    DisputeInfo(u32),
    /// Instance: the configured timeout for dispute escalation.
    DisputeTimeout,
    /// Instance: `FundingMode` for this escrow. Absent keys default to
    /// `Upfront` so existing deployments keep whole-job funding.
    FundingMode,
    /// Persistent per-milestone revision request count.
    RevCnt(u32),
    /// Instance: true when the current emergency pause was set jointly by
    /// the client and freelancer. Admin pauses leave this false so parties
    /// cannot lift an admin freeze.
    EpParty,
    /// Persistent: amount owed to an address whose payout or refund could not
    /// be transferred at settlement time (e.g. a classic-asset recipient with
    /// no trustline). Credited by `pay_or_credit`, cleared by `withdraw`.
    Owed(Address),
    /// Instance: sum of every `Owed(_)` entry, excluded from the balance that
    /// settlement paths may pay out so credits stay fully backed.
    TotalOwed,
    /// Instance: bitmask of the parties that accepted the job.  Bit 0
    /// (value 1) = freelancer via `accept_job`; bit 1 (value 2) = arbiter via
    /// `accept_arbiter`.  Delivery and dispute endpoints require both bits.
    Acceptance,
    /// Instance: ledger timestamp at which `fund` ran.  Starts the
    /// `ACCEPTANCE_WINDOW_SECONDS` window checked by `withdraw_unaccepted`.
    FundedAt,
    /// Persistent: the monotonic counter behind contract-assigned
    /// admin-transfer proposal IDs (issue #785).  `propose_admin_transfer`
    /// reads it, advances it with `checked_add` (`ArithmeticOverflow` instead
    /// of a trapping overflow when exhausted), and writes the new value back
    /// atomically with the proposal record, so IDs move forward only, are
    /// never caller-chosen, and can never be reused to replay old approvals.
    AdminProposalCounter,
    /// Persistent: the admin-transfer proposal record for
    /// `AdminProposal(proposal_id)` (issue #785).  It binds the ID to the
    /// `new_admin` it would install, its creation timestamp, its lifecycle
    /// status, and the signer approval bitmap — so approvals collected for one
    /// proposal are unusable against any other, and marking the record
    /// terminal consumes both the ID and its approvals.
    AdminProposal(u32),
    /// Persistent: the address that receives withheld tax, configured by the
    /// admin through `set_tax_recipient` (issue #788).
    /// `tax_withholding_deductions` refuses to record a withholding unless a
    /// recipient exists, so a withheld amount can never be created with no
    /// destination.
    TaxRecipient,
    /// Persistent: the tax recipient captured for milestone `u32` at the
    /// moment `tax_withholding_deductions` withheld its tax (issue #788).
    /// Every tax settlement path pays this address; records written before
    /// the capture existed fall back to `DataKey::TaxRecipient`.
    TaxWithholdingRecipient(u32),
    /// Instance: the arbiter's fee in basis points of a disputed milestone's
    /// remaining balance, fixed at construction (issue #738).  Absent means
    /// no fee, which is how every escrow created through `initialize` (and
    /// every escrow deployed before the fee existed) behaves.
    ArbiterFeeBps,
    /// Instance: storage layout version (`SCHEMA_VERSION`) the stored entries
    /// follow.  Written by `initialize` and `migrate`.  Absent on deployments
    /// created before it existed, which are schema 1.  Unlike `Version`
    /// (bumped by every `upgrade`), it only moves when `migrate` rewrites
    /// storage.
    SchemaVersion,
    /// Persistent: the signer-set change awaiting approval
    /// (`propose_multisig_config`).
    PendingMultiSigConfig,
    /// Instance: the address holding an `AdminRole`. Missing means the root
    /// admin holds it.
    AdminRole(AdminRole),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitializedEvent {
    pub admin: Address,
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub auto_release_seconds: u64,
    pub milestone_amounts: Vec<i128>,
    pub milestone_spec_hashes: Vec<BytesN<32>>,
    pub total_amount: i128,
    pub milestone_count: u32,
    pub created_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcessSweptEvent {
    pub token: Address,
    pub amount: i128,
    pub recipient: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
/// Emitted by `fund`. Static job data (freelancer, arbiter, token, milestone
/// count, auto-release window) is in `InitializedEvent`; consumers join on
/// the contract ID instead of having it repeated here (#742).
pub struct FundedEvent {
    pub client: Address,
    pub total_amount: i128,
}

/// Emitted by `add_milestone` when both parties consent to extending a funded
/// escrow with a new `Pending` milestone funded by an additional client
/// transfer (issue #737).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneAddedEvent {
    pub client: Address,
    pub freelancer: Address,
    /// Index of the newly appended milestone.
    pub milestone_index: u32,
    /// Amount deposited into escrow for the new milestone.
    pub amount: i128,
    /// Contract-level total after the addition.
    pub total_amount: i128,
    /// Milestone count after the addition.
    pub milestone_count: u32,
}

/// Emitted by `reduce_milestone` when both parties consent to lowering a
/// `Pending` milestone's amount and refunding the difference to the client
/// (issue #737).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneReducedEvent {
    pub client: Address,
    pub freelancer: Address,
    pub milestone_index: u32,
    pub old_amount: i128,
    pub new_amount: i128,
    /// Amount refunded to the client (`old_amount - new_amount`).
    pub refund: i128,
    /// Contract-level total after the reduction.
    pub total_amount: i128,
    pub milestone_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneFundedEvent {
    pub client: Address,
    pub milestone_index: u32,
    pub amount: i128,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionRequestedEvent {
    pub milestone_index: u32,
    pub client: Address,
    pub note_hash: BytesN<32>,
    pub revision_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveredEvent {
    pub milestone_index: u32,
    pub freelancer: Address,
    pub client: Address,
    pub delivered_at: u64,
    pub status: MilestoneStatus,
    pub amount: i128,
    pub deliverable: BytesN<32>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadlineExtendedEvent {
    pub milestone_index: u32,
    pub client: Address,
    pub extra_seconds: u32,
    pub new_extension: u32,
    pub new_deadline: u64,
}

/// Emitted once per approved milestone by `approve_partial`,
/// `approve_milestone` and `approve_milestones`.
///
/// Carries only what the approval changed. Parties, token, milestone count,
/// total and auto-release window never change after `initialize` and are in
/// `InitializedEvent`; consumers join on the contract ID (#742).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovedEvent {
    pub milestone_index: u32,
    /// Amount released to the freelancer by this approval.
    pub amount: i128,
    /// Cumulative amount released including this approval.
    pub released_amount: i128,
    /// Remaining balance after this approval (always 0 on a full approval).
    pub remaining: i128,
    pub status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeRaisedEvent {
    pub milestone_index: u32,
    pub caller: Address,
    /// The amount of the milestone being disputed, in stroops.
    pub milestone_amount: i128,
    /// Commitment to the reason/evidence bundle the caller supplied (#712).
    pub reason_hash: BytesN<32>,
    pub disputed_at: u64,
    /// The resulting milestone status after raising the dispute (always `Disputed`).
    pub new_status: MilestoneStatus,
}

/// Emitted by `submit_evidence` when a party anchors an evidence hash to an
/// open dispute (#712).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceSubmittedEvent {
    pub milestone_index: u32,
    pub caller: Address,
    pub evidence_hash: BytesN<32>,
    pub submitted_at: u64,
}

/// Emitted by `resolve_dispute` and `apply_dispute_arbitration_split`.
/// Client, freelancer and token are in `InitializedEvent` (#742).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeEscalatedEvent {
    pub milestone_index: u32,
    pub caller: Address,
    pub disputed_at: u64,
    pub escalated_at: u64,
    pub restored_status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolvedEvent {
    pub milestone_index: u32,
    pub arbiter: Address,
    /// Amount owed on the milestone at the time of resolution (before
    /// capping to the contract's available balance).
    pub amount: i128,
    /// Amount actually transferred to the freelancer or refunded to the
    /// client. May be less than `amount` if the contract balance was
    /// insufficient to cover the full owed amount.
    pub paid_amount: i128,
    /// Arbiter fee taken from the disputed balance before the parties were
    /// paid (issue #738). Zero when the escrow has no arbiter fee.
    pub arbiter_fee_amount: i128,
    pub released_to_freelancer: bool,
    pub status: MilestoneStatus,
}

/// Immutable record of a successful `apply_dispute_arbitration_split` call.
///
/// Every field reconciles exactly with the state the call persisted:
/// `client_refund` / `freelancer_payout` are the amounts actually transferred
/// (capped to the contract balance), `client_refund_bps` is the value written
/// under `ArbitrationSplitBps(milestone_index)`, `released_amount` is the
/// milestone's cumulative release after the split, and `status` is the terminal
/// milestone status stored by the call. Emitted only on the success path.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbitrationSplitAppliedEvent {
    /// Arbiter that authorised and applied the split (the acting address).
    pub arbiter: Address,
    /// Milestone the split was applied to.
    pub milestone_index: u32,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    /// Amount actually refunded to the client.
    pub client_refund: i128,
    /// Amount actually paid to the freelancer.
    pub freelancer_payout: i128,
    /// Arbiter fee taken from the disputed balance before the split (issue
    /// #738). Zero when the escrow has no arbiter fee.
    pub arbiter_fee_amount: i128,
    /// Basis points of the disputed balance awarded to the client. Matches the
    /// value persisted under `ArbitrationSplitBps(milestone_index)`.
    pub client_refund_bps: u32,
    /// Basis points awarded to the freelancer (`10_000 - client_refund_bps`).
    pub freelancer_payout_bps: u32,
    /// Cumulative amount released on the milestone after this split.
    pub released_amount: i128,
    /// Terminal milestone status after the split.
    pub status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxWithholdingDeductionsEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub gross_amount: i128,
    pub tax_amount: i128,
    pub net_amount: i128,
    pub tax_rate_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeAllocation {
    pub client_bps: u32,
    pub freelancer_bps: u32,
    pub treasury_bps: u32,
    pub locked: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeDistribution {
    pub client_amount: i128,
    pub freelancer_amount: i128,
    pub treasury_amount: i128,
}

/// Emitted by `pf_alloc_admin_override` when the admin replaces a *locked*
/// platform-fee allocation. Every field reconciles with the state the call
/// persisted under `DataKey::PlatformFeeAllocation`: `client_bps` /
/// `freelancer_bps` / `treasury_bps` are the new ratios (which always sum to
/// `BPS_SCALE`) and `locked` is `false` because the override unlocks the
/// allocation in the same step. Emitted only on the success path.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeAllocationOverrideEvent {
    /// Admin that authorised and applied the override (the acting address).
    pub admin: Address,
    pub client_bps: u32,
    pub freelancer_bps: u32,
    pub treasury_bps: u32,
    /// Lock flag persisted with the allocation; always `false` after an override.
    pub locked: bool,
}

/// Emitted by `set_platform_fee_allocation` when the admin successfully
/// updates the platform-fee BPS configuration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeAllocationSetEvent {
    pub admin: Address,
    pub client_bps: u32,
    pub freelancer_bps: u32,
    pub treasury_bps: u32,
}

/// Emitted by `lock_platform_fee_allocation` when the admin locks the
/// current platform-fee configuration, preventing further non-override
/// modifications.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeAllocationLockedEvent {
    pub admin: Address,
    pub client_bps: u32,
    pub freelancer_bps: u32,
    pub treasury_bps: u32,
}

/// Emitted by `calculate_platform_fee_split` with the resulting per-party
/// token amounts so downstream indexers can audit the split without
/// re-querying contract storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeSplitCalculatedEvent {
    pub total_amount: i128,
    pub client_amount: i128,
    pub freelancer_amount: i128,
    pub treasury_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferAdminEvent {
    pub old_admin: Address,
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenWhitelistedEvent {
    pub admin: Address,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenRemovedEvent {
    pub admin: Address,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RatioSplit {
    pub first: i128,
    pub second: i128,
}

/// Escrow interest/yield share configuration with an execution lock.
///
/// `client_share_bps + freelancer_share_bps` must equal `BPS_SCALE` (10_000).
/// When `locked` is true, share modifications via `set_escrow_interest_yield`
/// are rejected until an admin unlocks the state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowInterestYieldState {
    pub client_share_bps: u32,
    pub freelancer_share_bps: u32,
    /// When true, share modifications are blocked until unlocked.
    pub locked: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedEvent {
    pub milestone_index: u32,
    /// Address that triggered the release. Equal to `freelancer` for
    /// `claim_auto_release`; for `trigger_auto_release` it is the
    /// self-reported, unauthenticated caller (attribution only).
    pub caller: Address,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WithdrawnEvent {
    pub recipient: Address,
    pub token: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobAcceptedEvent {
    pub party: Address,
    pub acceptance_mask: u32,
}

/// Emitted by `rotate_freelancer` (issue #732).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreelancerRotatedEvent {
    pub client: Address,
    pub previous_freelancer: Address,
    pub new_freelancer: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnacceptedWithdrawnEvent {
    pub client: Address,
    pub token: Address,
    pub amount: i128,
}

/// Emitted by `replace_arbiter` when the client and the freelancer jointly
/// appoint a new arbiter (issue #731).
///
/// `old` is the arbiter recorded in `JobMeta` immediately before the call and
/// `new` is the arbiter persisted by it.  From the next call onward every
/// arbitration endpoint (`resolve_dispute`,
/// `apply_dispute_arbitration_split`) authorises against `new`, because they
/// compare their caller against the stored `JobMeta::arbiter` field that this
/// call rewrites.  Emitted only on the success path — every rejection
/// (`Paused` / `EscrowLocked` / `InvalidAddress` / `NotInitialized` /
/// `InvalidParties` / `DisputePending`) returns before the publish, so a
/// failed call publishes nothing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbiterReplacedEvent {
    /// The stored client that co-signed the replacement.
    pub client: Address,
    /// The stored freelancer that co-signed the replacement.
    pub freelancer: Address,
    /// Arbiter recorded in `JobMeta` before this call.
    pub old: Address,
    /// Arbiter persisted in `JobMeta` by this call.
    pub new: Address,
}

/// Emitted by `cancel_escrow` once both the client and the freelancer have
/// approved and the cancel lock fires.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelEscrowInitiatedEvent {
    pub caller: Address,
    /// Whether the initiating caller is the client (`true`) or the freelancer
    /// (`false`).  Lets indexers attribute the cancellation without resolving
    /// the caller address against off-chain metadata.
    pub caller_is_client: bool,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    /// Total number of milestones in the escrow at the time of cancellation.
    pub milestone_count: u32,
    /// Sum of all milestone amounts as stored in `JobMeta`.  Does not account
    /// for amounts already released; indexers that need unreleased balance
    /// should subtract separately-indexed release events.
    pub total_amount: i128,
}

/// Stored under `DataKey::CancelApproval` between the first and the second
/// `cancel_escrow` approval.
///
/// Carries the ledger timestamp at which the approval round started so that an
/// approval older than [`CANCEL_APPROVAL_TTL_SECONDS`] can be expired instead of
/// being completed by the counter-party (issue #713).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelApprovalRecord {
    /// Bitmask of the approvals recorded so far.
    /// bit 0 (value 1) = client approved, bit 1 (value 2) = freelancer approved.
    /// A stored record only ever holds an unexpired, single-party approval,
    /// because the second approval fires the lock and clears the record.
    pub mask: u32,
    /// Ledger timestamp of the round's first approval.  The approval expires
    /// `CANCEL_APPROVAL_TTL_SECONDS` after this instant, and a fresh round
    /// restarts the window at its own first approval.
    pub approved_at: u64,
}

/// Emitted by `cancel_escrow` when a party records their cancellation approval
/// but the counter-party has not yet approved (single-signature stage).
/// Lets indexers track partial cancel intent without treating it as a finalized
/// cancel.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelApprovalRecordedEvent {
    pub caller: Address,
    /// Current bitmask after recording this approval.
    /// bit 0 = client approved, bit 1 = freelancer approved.
    pub approval_mask: u32,
    /// Ledger timestamp of the round's first approval — the instant the
    /// `CANCEL_APPROVAL_TTL_SECONDS` window started.  Lets indexers tell a
    /// live approval from one that will silently expire.
    pub approved_at: u64,
}

/// Emitted by `revoke_cancel_approval` when a party withdraws their
/// cancellation approval before the lock fires.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelApprovalRevokedEvent {
    pub caller: Address,
    /// Updated bitmask after removing this party's bit.
    pub approval_mask: u32,
}

/// Emitted by `admin_override_cancel_release` when the admin resolves a locked
/// cancel state by releasing all remaining funds to the freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminCancelOverrideReleaseEvent {
    pub admin: Address,
    pub freelancer: Address,
    pub token: Address,
    pub total_released: i128,
}

/// Emitted by `admin_override_cancel_refund` when the admin resolves a locked
/// cancel state by refunding all remaining funds to the client.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminCancelOverrideRefundEvent {
    pub admin: Address,
    pub client: Address,
    pub token: Address,
    pub total_refunded: i128,
}

/// Emitted by `emergency_pause` when the escrow is paused by the client and freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPausedEvent {
    pub client: Address,
    pub freelancer: Address,
}

/// Emitted by `emergency_unpause` when the admin unpauses the escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyUnpausedEvent {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPartyUnpausedEvent {
    pub client: Address,
    pub freelancer: Address,
}

/// Emitted by `emergency_pause_claim_refund` on its success path only.
///
/// Published under the topics
/// `(symbol_short!("epclaim"), admin)` so indexers can track
/// emergency refund settlements without replaying storage transitions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPauseClaimRefundEvent {
    /// Party receiving the refund leg (the job's client).
    pub claimant: Address,
    /// Amount allocated to the claimant.
    pub refund_amount: i128,
    /// Amount allocated to the freelancer; `refund_amount + freelancer_payout`
    /// always equals the settled total.
    pub freelancer_payout: i128,
    /// Contract token balance left once the settled total is paid out.
    pub remaining_balance: i128,
    /// Ledger timestamp at which the claim was settled.
    pub timestamp: u64,
}

/// Emitted by `emergency_pause_admin_override` when the admin overrides the
/// pause state. Every field reconciles with the state the call persisted:
/// `paused` is the new value written to `DataKey::Ep` and `previous` is the
/// value it replaced. Emitted only on the success path.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPauseAdminOverrideEvent {
    /// Admin that authorised and applied the override (the acting address).
    pub admin: Address,
    /// Pause flag persisted by this call (`DataKey::Ep`).
    pub paused: bool,
    /// Pause flag that was in effect immediately before this call.
    pub previous: bool,
}

// ── tax_withholding_deductions types and events ──────────────────────────────

/// Stored in `DataKey::TaxWithholdingLock(milestone_index)` by
/// `tax_withholding_deductions`.  Holds the pre-computed split so admin
/// override endpoints do not need to recompute tax arithmetic.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxWithholdingRecord {
    pub gross_amount: i128,
    pub tax_amount: i128,
    pub net_amount: i128,
    pub tax_rate_bps: u32,
}

/// Emitted by `tax_withholding_deductions` when tax is successfully computed
/// and the milestone is locked pending admin resolution.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxWithholdingAppliedEvent {
    pub milestone_index: u32,
    pub gross_amount: i128,
    pub tax_amount: i128,
    pub net_amount: i128,
    pub tax_rate_bps: u32,
}

/// Emitted by `tax_withholding_split_refund` when the split-refund
/// distribution pathway for a tax-withheld amount is computed.  Carries the
/// gross and tax legs alongside the two post-tax amounts so an indexer can
/// reconstruct exactly how much of the withheld balance each party receives
/// and how the withholding was attributed between them.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxWithholdingSplitRefundEvent {
    pub gross_amount: i128,
    pub tax_amount: i128,
    pub net_amount: i128,
    pub client_refund: i128,
    pub freelancer_payout: i128,
    pub client_refund_bps: u32,
    pub freelancer_payout_bps: u32,
}

/// Emitted by `admin_override_tax_release` when the admin resolves a
/// tax-locked milestone by releasing the net amount to the freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideTaxReleaseEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub token: Address,
    pub net_amount: i128,
    pub tax_amount: i128,
}

/// Emitted by `admin_override_tax_refund` when the admin resolves a
/// tax-locked milestone by refunding the gross amount to the client.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideTaxRefundEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub token: Address,
    pub gross_amount: i128,
}

/// Emitted by `admin_override_tax_split_refund` when the admin settles a
/// tax-locked milestone with a split refund. `client_refund` and
/// `freelancer_payout` are the amounts actually transferred (each net of its
/// share of `tax_amount`), so the two legs always sum to
/// `gross_amount − tax_amount` and the milestone's tax lock has been cleared.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideTaxSplitRefundEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    pub gross_amount: i128,
    pub tax_amount: i128,
    pub client_refund: i128,
    pub freelancer_payout: i128,
    pub client_refund_bps: u32,
    pub freelancer_payout_bps: u32,
}

/// Emitted by `set_tax_recipient` when the admin configures the address that
/// receives withheld tax (issue #788).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxRecipientSetEvent {
    pub admin: Address,
    pub recipient: Address,
}

/// Emitted by each tax settlement path (`admin_override_tax_release`,
/// `admin_override_tax_refund`, `admin_override_tax_split_refund`) when the
/// withheld `tax_amount` is actually transferred to the configured recipient
/// (issue #788).  Publishes only for a non-zero amount, so an indexer can
/// reconcile every `taxwith` withholding with its eventual payout: the
/// milestone it belonged to, the destination paid, and the exact amount.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxPaidEvent {
    pub milestone_index: u32,
    pub recipient: Address,
    pub token: Address,
    pub tax_amount: i128,
}

// ── escrow_interest_yield admin-override config ──────────────────────────────

/// Consolidated yield configuration stored under `DataKey::YieldConfig`.
/// Bundles all yield-related settings into a single struct-valued ledger
/// entry so `admin_set_yield_rate` touches one key instead of several.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldConfig {
    pub yield_rate: u32,
}

// ── escrow_interest_yield admin-override events ──────────────────────────────

/// Emitted by `admin_set_yield_rate` whenever the admin updates the annual
/// yield rate.  Downstream indexers can track the full rate-change history
/// by replaying these events in ledger order.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldRateSetEvent {
    pub admin: Address,
    pub old_rate_bps: u32,
    pub new_rate_bps: u32,
}

/// Emitted by `admin_accrue_yield` each time the admin books interest against
/// the escrowed balance.  `accrued_amount` is the incremental interest for
/// this call; `total_accrued` is the running total stored in `YieldAccrued`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldAccruedEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub accrued_amount: i128,
    pub total_accrued: i128,
}

/// Emitted by `admin_override_release` when the admin force-releases a locked
/// milestone directly to the freelancer, bypassing the normal approval flow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideReleaseEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
}

/// Emitted by `admin_override_refund` when the admin force-refunds a locked
/// milestone back to the client, bypassing the normal dispute flow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideRefundEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub token: Address,
    pub amount: i128,
}

/// Emitted by `set_interest_yield_consent` once both the client
/// and freelancer have authorized the share change alongside the admin.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowInterestYieldConsentSetEvent {
    pub admin: Address,
    pub client: Address,
    pub freelancer: Address,
    pub client_share_bps: u32,
    pub freelancer_share_bps: u32,
}

/// Emitted by `escrow_interest_yield` on a successful estimate. Carries the
/// computation inputs and the resulting yield so an indexer can reconstruct the
/// outcome without recomputing it. `escrow_interest_yield` is a pure estimator
/// (no caller `Address` in its signature and no persisted state), so the event
/// records the amounts involved and the resulting yield amount.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowInterestYieldEvent {
    pub principal: i128,
    pub annual_rate_bps: i128,
    pub duration_seconds: i128,
    pub yield_amount: i128,
}

/// Emitted by set_escrow_interest_yield when the admin updates the yield-share configuration.
/// Every field reconciles with the state persisted under DataKey::InterestYieldState.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowInterestYieldSetEvent {
    pub admin: Address,
    pub client_share_bps: u32,
    pub freelancer_share_bps: u32,
    pub locked: bool,
}

/// Emitted by unlock_escrow_interest_yield when the admin clears the execution lock.
/// Every field reconciles with the state persisted under DataKey::InterestYieldState.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowInterestYieldUnlockedEvent {
    pub admin: Address,
    pub client_share_bps: u32,
    pub freelancer_share_bps: u32,
    pub locked: bool,
}

/// Emitted by `lock_escrow_interest_yield` when the admin takes the execution
/// lock on the interest/yield share configuration. Every field reconciles with
/// the state the call persisted under `DataKey::InterestYieldState`:
/// `client_share_bps` / `freelancer_share_bps` are the shares the lock freezes
/// (which always sum to `BPS_SCALE`) and `locked` is `true` because the lock is
/// what the call applied. Emitted only on the success path — a rejected call
/// (`Unauthorized` / `NotInitialized`) publishes nothing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowInterestYieldLockedEvent {
    /// Admin that authorised and applied the lock (the acting address).
    pub admin: Address,
    /// Client share frozen by the lock, in basis points.
    pub client_share_bps: u32,
    /// Freelancer share frozen by the lock, in basis points.
    pub freelancer_share_bps: u32,
    /// Lock flag persisted with the state; always `true` after a lock.
    pub locked: bool,
}

/// Emitted by `admin_override_streaming_release` when the admin proportionally
/// settles a `Disputed` milestone using the streaming/time-extension split.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideStreamingReleaseEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    pub client_refund: i128,
    pub freelancer_payout: i128,
}

/// Result of checking whether a multisig proposal has reached the threshold.
/// Returned by `is_multisig_approved` to give callers both the boolean
/// decision and the raw approval bitmap for off-chain inspection.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigApprovalState {
    pub approved: bool,
    pub approvals: u32,
    pub threshold: u32,
    pub bitmap: u32,
}

/// Consolidated multisig configuration (issue #456): the registered signer set
/// and the approval threshold, persisted together in the single instance entry
/// `DataKey::MultiSigConfig`.
///
/// The fields are positional on purpose: field `0` is the signer set, field `1`
/// the threshold.  `#[contracttype]` encodes a tuple struct as a compact
/// `ScVec`, whereas a named-field struct would be encoded as a symbol-keyed
/// `ScMap` whose field symbols alone (`signers`, `threshold`) cost 36 bytes —
/// more than the 28-byte saving this consolidation achieves — while still using
/// one key.  Positional fields keep the entry smaller than the legacy two-key
/// layout, for the same signer set.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigConfig(pub Vec<Address>, pub u32);

/// Emitted by `multisig_approve` on every successful call so downstream
/// indexers can track approval progress without polling contract storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigApprovedEvent {
    pub proposal_id: u32,
    pub signer: Address,
    pub approvals: u32,
    pub threshold: u32,
    pub approved: bool,
    pub bitmap: u32,
}

/// Stored in `DataKey::PendingAdminTransfer` by `propose_admin_transfer`.
/// Read by `execute_admin_transfer` (to check the multisig threshold and
/// apply the swap) and `cancel_admin_transfer_proposal` /
/// `get_pending_admin_transfer`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminTransfer {
    pub new_admin: Address,
    pub proposal_id: u32,
}

/// A signer-set change awaiting approval by the current signers, recorded by
/// `propose_multisig_config` and applied by `execute_multisig_config`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingMultiSigConfig {
    pub signers: Vec<Address>,
    pub threshold: u32,
    pub proposal_id: u32,
}

/// Emitted (`msigcfgp`) by `propose_multisig_config`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigConfigProposedEvent {
    pub admin: Address,
    pub signers: Vec<Address>,
    pub threshold: u32,
    pub proposal_id: u32,
}

/// Emitted (`msigcfg`) by `execute_multisig_config` when the signer set and
/// threshold were replaced.  Approvals collected under the old set for a
/// pending admin transfer are cleared.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigConfigChangedEvent {
    pub old_signers: Vec<Address>,
    pub old_threshold: u32,
    pub new_signers: Vec<Address>,
    pub new_threshold: u32,
    pub proposal_id: u32,
}

/// Emitted (`msigcfgx`) by `cancel_multisig_config_proposal`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigConfigCancelledEvent {
    pub admin: Address,
    pub proposal_id: u32,
}

/// Emitted by `propose_admin_transfer` when a new multisig-gated admin
/// transfer proposal is created.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferProposedEvent {
    pub admin: Address,
    pub new_admin: Address,
    pub proposal_id: u32,
}

/// Emitted by `execute_admin_transfer` once the proposal's multisig
/// threshold is reached and the admin key is swapped.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferExecutedEvent {
    pub old_admin: Address,
    pub new_admin: Address,
    pub proposal_id: u32,
}

/// Emitted by `cancel_admin_transfer_proposal` when the admin clears a
/// pending proposal without executing it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferCancelledEvent {
    pub admin: Address,
    pub proposal_id: u32,
}

/// Lifecycle status of an admin-transfer proposal record (issue #785).
///
/// `Pending` is the only state that accepts approvals or can execute.
/// `Executed` and `Cancelled` are terminal: the record stays on the ledger
/// with its approval bitmap cleared as the tombstone that makes its ID — and
/// every approval it ever held — permanently unusable, since the proposal ID
/// counter only ever moves forward.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdminProposalStatus {
    Pending,
    Executed,
    Cancelled,
}

/// Stored in `DataKey::AdminProposal(proposal_id)` by `propose_admin_transfer`
/// (issue #785).  This record is what a multisig approval is bound to:
/// `multisig_approve` only accepts an ID whose record exists and is still
/// `Pending`, the approval bitmap lives inside the record beside the
/// `new_admin` it would install, and `execute_admin_transfer` /
/// `cancel_admin_transfer_proposal` mark it terminal and clear the bitmap, so
/// neither the ID nor its approvals can ever be replayed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferProposal {
    pub proposal_id: u32,
    pub new_admin: Address,
    pub created_at: u64,
    pub status: AdminProposalStatus,
    pub approvals_bitmap: u32,
}

/// Emitted by `admin_pause_escrow` when the admin freezes normal operations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowPausedEvent {
    pub admin: Address,
}

/// Emitted by `admin_resume_escrow` when the admin lifts the pause and
/// restores normal operations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowResumedEvent {
    pub admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneTimeExtensionEvent {
    pub amount: i128,
    pub elapsed_seconds: i128,
    pub total_seconds: i128,
    pub freelancer_share: i128,
    pub client_refund: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentStreamingEvent {
    pub total_amount: i128,
    pub numerator: i128,
    pub denominator: i128,
    pub streamed_payout: i128,
    pub client_refund: i128,
}

/// Emitted by `payment_streaming_consent` once both the client's
/// and the freelancer's signatures have been collected and the streaming
/// split has been computed. The two addresses are included so an indexer can
/// audit *who* consented without re-reading job metadata.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentStreamingConsentEvent {
    pub milestone_index: u32,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    pub total_amount: i128,
    pub numerator: i128,
    pub denominator: i128,
    pub streamed_payout: i128,
    pub client_refund: i128,
}

/// Emitted by `time_extensions_consent` once both the client's
/// and the freelancer's signatures have been collected and the deadline
/// extension has been written to storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeExtConsentEvent {
    pub milestone_index: u32,
    pub client: Address,
    pub freelancer: Address,
    pub extra_seconds: u32,
    pub new_extension: u32,
    pub new_deadline: u64,
}

// ── emergency_pause events ──────────────────────────────────────────────────

/// Emitted by `emergency_pause_allocation` with the exact per-party
/// amounts. `total_amount` always equals the sum of `allocations`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPauseAllocationEvent {
    pub total_amount: i128,
    pub num_parties: u32,
    pub allocations: Vec<i128>,
}

// ── multisig_approval events ────────────────────────────────────────────────

/// Emitted by `multisig_approval_init` when the multisig signer set and
/// approval threshold are successfully registered (issue #455).
///
/// The initialisation is a one-time state transition: it is the *only* write
/// path for the multisig configuration (`DataKey::MultiSigConfig`), so
/// without an event the original configuration could only be recovered by
/// replaying the ledger.  Both payload fields therefore reconcile exactly with
/// what the call persisted — `signers` and `threshold` are the two fields of
/// the `MultiSigConfig` written under `DataKey::MultiSigConfig` (read back from
/// instance storage after the write).  Emitted only on the success path: every rejection
/// (`NotInitialized` / `Unauthorized` / `AlreadyInitialized` /
/// `MultiSigNoSigners` / `MultiSigTooManySigners` /
/// `MultiSigInvalidThreshold` / `MultiSigDuplicateSigner`) returns before the
/// publish, so a failed call publishes nothing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultisigApprovalInitEvent {
    /// Admin that authorised and applied the initialisation (the acting
    /// address, verified by `require_admin` against `DataKey::Admin`).
    pub admin: Address,
    /// Full signer set persisted in `DataKey::MultiSigConfig`.
    pub signers: Vec<Address>,
    /// Approval threshold persisted in `DataKey::MultiSigConfig`.
    pub threshold: u32,
}

/// Emitted by `multisig_admin_override_release` when the admin force-releases
/// a multisig-locked allocation to the freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultisigAdminOverrideReleaseEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
}

/// Emitted by `multisig_admin_override_refund` when the admin force-refunds
/// a multisig-locked allocation back to the client.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultisigAdminOverrideRefundEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub token: Address,
    pub amount: i128,
}

/// Emitted by `multisig_lock` when the admin takes the multisig execution
/// lock.  `locked` reconciles with the flag the call persisted under
/// `DataKey::MultisigLocked` and is always `true`, because taking the lock is
/// what the call does.  Emitted only on the success path — a rejected call
/// (`Unauthorized` / `NotInitialized`) publishes nothing.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultisigLockedEvent {
    /// Admin that authorised and applied the lock (the acting address).
    pub admin: Address,
    /// Lock flag persisted in instance storage; always `true` after a lock.
    pub locked: bool,
}

/// Emitted by `multisig_split_refund` when a split-refund allocation is
/// calculated between client and freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitRefundCalculatedEvent {
    pub client_refund: i128,
    pub freelancer_payout: i128,
    pub client_refund_bps: u32,
    pub freelancer_payout_bps: u32,
}

/// Emitted by `cancel_escrow_split_refund` when a cancel-specific split-refund
/// allocation is calculated.  Records the exact basis-point inputs alongside
/// the computed amounts so downstream indexers can audit every cancellation
/// distribution without querying contract storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelSplitRefundCalculatedEvent {
    pub client_refund: i128,
    pub freelancer_payout: i128,
    pub client_refund_bps: u32,
    pub freelancer_payout_bps: u32,
}

/// Emitted by `split_refund_net_distribution` when net split-refund distributions
/// and platform fees are calculated. Records the input amounts and resulting
/// net refund and fee shares so downstream indexers can reconstruct the operation
/// without replaying storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitRefundNetDistributionEvent {
    pub total_amount: i128,
    pub client_refund_bps: u32,
    pub freelancer_payout_bps: u32,
    pub client_net_refund: i128,
    pub client_fee_share: i128,
    pub freelancer_net_payout: i128,
    pub treasury_fee_share: i128,
}

/// Emitted by `upgrade` recording the outcome of a contract WASM upgrade.
/// Emitted by `set_admin_role`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminRoleChangedEvent {
    pub admin: Address,
    pub role: AdminRole,
    pub previous: Address,
    pub new_address: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgradedEvent {
    pub admin: Address,
    pub new_wasm_hash: BytesN<32>,
    pub version: u32,
}

pub type UpgradeEvent = ContractUpgradedEvent;

/// Emitted by `migrate` when stored entries were rewritten from
/// `from_version` to `to_version` (`SCHEMA_VERSION`).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigratedEvent {
    pub admin: Address,
    pub from_version: u32,
    pub to_version: u32,
}

#[contract]
pub struct MilestoneEscrow;

// Deferred pending coordinated migration to #[contractevent] — see
// escrow-backend's poller.ts, which reads the current event wire format.
#[allow(deprecated)]
#[contractimpl]
impl MilestoneEscrow {
    fn bump(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    fn load_admin(env: &Env) -> Result<Address, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    fn require_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        admin.require_auth();
        let stored_admin = Self::load_admin(env)?;
        if stored_admin != *admin {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    /// Variant of `require_admin` that reads `DataKey::Admin` from **instance**
    /// storage instead of persistent storage.
    ///
    /// During `initialize` the admin address is written to both
    /// `instance()` and `persistent()` storage (see `initialize`).  Callers
    /// that also write other instance-storage keys in the same call can use
    /// this helper so that both the admin read and the subsequent instance write
    /// touch a **single** ledger entry rather than two.
    ///
    /// # Errors
    /// * `NotInitialized` – The instance `Admin` key is absent (contract has
    ///   not been initialised).
    /// * `Unauthorized`   – `admin` does not match the stored admin.
    fn require_admin_from_instance(env: &Env, admin: &Address) -> Result<(), Error> {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        if stored_admin != *admin {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    /// Require `caller` to hold `role` (issue #761): the address assigned
    /// with `set_admin_role`, or the root admin while the role is unassigned.
    ///
    /// # Errors
    /// * `NotInitialized` – No admin is stored.
    /// * `Unauthorized`   – `caller` does not hold `role`.
    fn require_role(env: &Env, caller: &Address, role: AdminRole) -> Result<(), Error> {
        caller.require_auth();
        let instance = env.storage().instance();
        let holder: Address = match instance.get(&DataKey::AdminRole(role)) {
            Some(holder) => holder,
            None => instance.get(&DataKey::Admin).ok_or(Error::NotInitialized)?,
        };
        if holder != *caller {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    /// Reject while milestone `index`'s dispute is younger than the dispute
    /// timeout, so a resolver cannot pre-empt the arbiter (issue #761).
    fn assert_dispute_timeout_elapsed(env: &Env, index: u32) -> Result<(), Error> {
        let Some(info) = env
            .storage()
            .persistent()
            .get::<_, DisputeInfo>(&DataKey::DisputeInfo(index))
        else {
            return Ok(());
        };
        let timeout = env
            .storage()
            .instance()
            .get(&DataKey::DisputeTimeout)
            .unwrap_or(DISPUTE_TIMEOUT_SECONDS);
        let elapsed = env
            .ledger()
            .timestamp()
            .checked_sub(info.disputed_at)
            .ok_or(Error::DisputeTimeoutNotReached)?;
        if elapsed < timeout {
            return Err(Error::DisputeTimeoutNotReached);
        }
        Ok(())
    }

    /// Read a **cold configuration** entry (issue #772).
    ///
    /// The whole contract-instance entry is loaded on every invocation, so it
    /// keeps only what nearly every call needs (`Job`, pause and cancel flags,
    /// admin). Rarely used configuration — `WhitelistedTokens`,
    /// `MultiSigConfig`, `InterestYieldState` and `PlatformFeeAllocation` —
    /// lives under persistent keys and is read only by the functions that use
    /// it; `approve_milestone` no longer pays to load a 50-token whitelist or a
    /// 32-signer multisig set.
    ///
    /// A deployment upgraded from the old layout still holds these values in
    /// instance storage until the admin calls `migrate_cold_config`, so an
    /// absent persistent entry falls back to the instance copy. The instance
    /// entry is loaded by every call anyway, so the fallback costs no extra
    /// ledger entry, and the persistent copy always wins once written.
    ///
    /// **This function must contain only read operations.**
    fn cold_get<V: TryFromVal<Env, Val>>(env: &Env, key: &DataKey) -> Option<V> {
        env.storage()
            .persistent()
            .get(key)
            .or_else(|| env.storage().instance().get(key))
    }

    /// Presence check matching [`Self::cold_get`].
    fn cold_has(env: &Env, key: &DataKey) -> bool {
        env.storage().persistent().has(key) || env.storage().instance().has(key)
    }

    /// Move one cold-configuration entry from instance to persistent storage.
    /// A persistent copy written after the upgrade is newer, so it is kept and
    /// only the stale instance copy is dropped. Returns whether anything was
    /// found in instance storage.
    fn migrate_cold_entry<V>(env: &Env, key: &DataKey) -> bool
    where
        V: TryFromVal<Env, Val> + IntoVal<Env, Val>,
    {
        let instance = env.storage().instance();
        let Some(value) = instance.get::<_, V>(key) else {
            return false;
        };
        let persistent = env.storage().persistent();
        if !persistent.has(key) {
            persistent.set(key, &value);
        }
        instance.remove(key);
        true
    }

    /// Validation hook: verify that **both** the client and the freelancer
    /// recorded on the job have signed the current transaction.
    ///
    /// Both signatures are collected before any business logic runs.  A
    /// transaction carrying only one of the two signatures never reaches the
    /// caller's logic: the missing `require_auth()` panics at the host level,
    /// so a single-signature attempt reverts the whole invocation and no
    /// storage is mutated.
    ///
    /// Returns the loaded `JobMeta` so callers do not need a second instance
    /// read.
    ///
    /// # Errors
    /// * `NotInitialized` – Job metadata has never been written, so there is
    ///   no client/freelancer pair to collect signatures from.
    fn require_client_and_freelancer_consent(env: &Env) -> Result<JobMeta, Error> {
        let meta = Self::load_job_meta(env)?;

        // Order is irrelevant to correctness — both must succeed — but the
        // client is checked first to mirror `set_interest_yield_consent`.
        meta.client.require_auth();
        meta.freelancer.require_auth();

        Ok(meta)
    }

    /// The one "has `initialize` run?" predicate. `DataKey::Job` is written by
    /// every initialisation path and is what `load_job_meta` reads, so it is
    /// the marker; do not test `Admin` or `Version` ad hoc.
    fn is_initialized(env: &Env) -> bool {
        env.storage().instance().has(&DataKey::Job)
    }

    /// Reject the call before any other ledger access if the contract has not
    /// been initialised.
    fn require_initialized(env: &Env) -> Result<(), Error> {
        if !Self::is_initialized(env) {
            return Err(Error::NotInitialized);
        }
        Ok(())
    }

    /// Verify that the caller is either the stored client or freelancer for
    /// this escrow.  Used by `raise_dispute` to ensure only authorised parties
    /// can initiate a dispute.  Returns the loaded `JobMeta` on success so the
    /// caller does not need a second instance read.
    fn require_dispute_party(env: &Env, caller: &Address) -> Result<JobMeta, Error> {
        caller.require_auth();
        let meta = Self::load_job_meta(env)?;
        if meta.client != *caller && meta.freelancer != *caller {
            return Err(Error::Unauthorized);
        }
        Ok(meta)
    }

    /// Read the dispute flag for `index`.  Returns `false` when the flag
    /// was never written or has been evicted.
    #[allow(dead_code)]
    fn is_dispute_flag(env: &Env, index: u32) -> bool {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::DisputeFlag(index))
            .unwrap_or(false)
    }

    /// Write the dispute flag to temporary storage.  This is a cheap,
    /// short-lived signal that the milestone at `index` has been disputed.
    /// Callers that need to verify dispute status can read this temporary key
    /// rather than fetching the full persistent `Milestone` entry, reducing
    /// ledger footprint rent on the read path.
    fn store_dispute_flag(env: &Env, index: u32) {
        env.storage()
            .temporary()
            .set(&DataKey::DisputeFlag(index), &true);
    }

    /// Release the dispute lock for a given milestone index.  Called
    /// unconditionally after every `raise_dispute` attempt — success or
    /// failure — so the lock can never become permanently held.
    fn release_dispute_lock(env: &Env, milestone_index: u32) {
        env.storage()
            .temporary()
            .remove(&DataKey::DisputeLock(milestone_index));
    }

    fn ensure_not_paused(env: &Env) -> Result<(), Error> {
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused || Self::read_emergency_paused(env) {
            return Err(Error::Paused);
        }
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if cancel_locked {
            return Err(Error::EscrowLocked);
        }
        Ok(())
    }

    /// Return `Err(Error::TaxWithholdingInProgress)` when a tax withholding
    /// calculation is active so that state-modifying operations can block
    /// concurrent mutations.
    fn assert_tax_withholding_not_locked(env: &Env) -> Result<(), Error> {
        let locked: bool = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::TaxWithholdingExecutionLock)
            .unwrap_or(false);
        if locked {
            return Err(Error::TaxWithholdingInProgress);
        }
        Ok(())
    }

    /// Return `Err(Error::PlatformFeeAllocationInProgress)` when a platform
    /// fee allocation operation is active so that state-modifying operations
    /// can block concurrent mutations.
    fn assert_platform_fee_allocation_not_locked(env: &Env) -> Result<(), Error> {
        let locked: bool = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::PlatformFeeAllocationLock)
            .unwrap_or(false);
        if locked {
            return Err(Error::PlatformFeeAllocationInProgress);
        }
        Ok(())
    }

    /// Return `Err(Error::EmergencyPauseInProgress)` when an emergency pause
    /// operation is active so that state-modifying operations can block
    /// concurrent mutations.
    fn assert_emergency_pause_not_locked(env: &Env) -> Result<(), Error> {
        let locked: bool = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::EpLk)
            .unwrap_or(false);
        if locked {
            return Err(Error::EmergencyPauseInProgress);
        }
        Ok(())
    }

    /// Reject a pause-gated settlement while the contract token balance is
    /// zero, so a refund never attempts an empty transfer.
    fn assert_nonzero_balance(env: &Env, meta: &JobMeta) -> Result<(), Error> {
        let token_client = token::Client::new(env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::EmptyBalance);
        }
        Ok(())
    }

    /// Total amount credited to recipients via `pay_or_credit` and not yet
    /// withdrawn. Returns `0` when nothing has ever been credited.
    fn load_total_owed(env: &Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalOwed)
            .unwrap_or(0)
    }

    /// Contract token balance minus the credits still owed to recipients, i.e.
    /// the part of the balance that settlement paths may pay out.
    /// Drop a milestone's persistent deadline extension once it reaches a
    /// terminal state (#671). Checking first keeps the many milestones that
    /// were never extended from paying for a ledger write.
    fn clear_time_ext(env: &Env, milestone_index: u32) {
        let key = DataKey::TimeExt(milestone_index);
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    }

    fn available_balance(env: &Env, token_client: &token::Client) -> i128 {
        token_client
            .balance(&env.current_contract_address())
            .saturating_sub(Self::load_total_owed(env))
    }

    /// Pay `amount` of the escrowed token from the contract to `to`, or credit
    /// it to `DataKey::Owed(to)` when the transfer fails.
    ///
    /// For a classic asset wrapped by its SAC, a transfer to an account with no
    /// trustline (or a deauthorized or full one) fails and would revert the
    /// whole settlement, letting the recipient's account state block every
    /// payout and refund path. Instead the amount is credited and later claimed
    /// by the recipient through `withdraw`, so settlement always completes.
    ///
    /// A failure while the contract holds less than `amount` (net of credits
    /// already owed) is not converted into a credit, so no unbacked balance is
    /// ever owed.
    ///
    /// # Errors
    /// * `InsufficientBalance` – the transfer failed and the contract does not
    ///   hold `amount` beyond what is already owed.
    /// * `ArithmeticOverflow`  – the credited totals overflow `i128`.
    fn pay_or_credit(
        env: &Env,
        token_client: &token::Client,
        to: &Address,
        amount: &i128,
    ) -> Result<(), Error> {
        let contract = env.current_contract_address();
        if matches!(token_client.try_transfer(&contract, to, amount), Ok(Ok(()))) {
            return Ok(());
        }
        if Self::available_balance(env, token_client) < *amount {
            return Err(Error::InsufficientBalance);
        }

        let key = DataKey::Owed(to.clone());
        let owed = env
            .storage()
            .persistent()
            .get::<_, i128>(&key)
            .unwrap_or(0)
            .checked_add(*amount)
            .ok_or(Error::ArithmeticOverflow)?;
        let total_owed = Self::load_total_owed(env)
            .checked_add(*amount)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage().persistent().set(&key, &owed);
        env.storage()
            .instance()
            .set(&DataKey::TotalOwed, &total_owed);
        Ok(())
    }

    /// Return `Err(Error::TimeExtInProgress)` when a milestone
    /// time-extension split is mid-execution.
    fn assert_time_ext_not_locked(env: &Env) -> Result<(), Error> {
        let locked: bool = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::TimeExtExecutionLock)
            .unwrap_or(false);
        if locked {
            return Err(Error::TimeExtInProgress);
        }
        Ok(())
    }

    /// Return `Err(Error::PaymentStreamingInProgress)` when a payment-streaming
    /// split is mid-execution.
    fn assert_payment_streaming_not_locked(env: &Env) -> Result<(), Error> {
        let locked: bool = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::PaymentStreamingExecutionLock)
            .unwrap_or(false);
        if locked {
            return Err(Error::PaymentStreamingInProgress);
        }
        Ok(())
    }

    fn validate_fee_allocation(
        client_bps: u32,
        freelancer_bps: u32,
        treasury_bps: u32,
    ) -> Result<(), Error> {
        let total = client_bps
            .checked_add(freelancer_bps)
            .and_then(|v| v.checked_add(treasury_bps))
            .ok_or(Error::InvalidRatio)?;
        if total != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        if treasury_bps > MAX_TREASURY_FEE_BPS || client_bps > MAX_CLIENT_FEE_BPS {
            return Err(Error::FeeTooHigh);
        }
        Ok(())
    }

    /// Validate estimator inputs for `escrow_interest_yield`.
    ///
    /// Rejects zero/negative principal, rate, or duration immediately, and
    /// rejects rates above 100 % (`BPS_SCALE`) as an unsupported configuration.
    fn validate_interest_yield_params(
        principal: i128,
        annual_rate_bps: i128,
        duration_seconds: i128,
    ) -> Result<(), Error> {
        if principal <= 0 {
            return Err(Error::InvalidAmount);
        }
        if annual_rate_bps <= 0 {
            return Err(Error::InvalidAmount);
        }
        if annual_rate_bps > BPS_DENOMINATOR {
            return Err(Error::InvalidRatio);
        }
        if duration_seconds <= 0 {
            return Err(Error::InvalidAmount);
        }
        Ok(())
    }

    /// Validate stored yield-share configuration: both shares must be finite and
    /// sum exactly to `BPS_SCALE` (10_000).
    fn validate_interest_yield_share_config(
        client_share_bps: u32,
        freelancer_share_bps: u32,
    ) -> Result<(), Error> {
        let total = client_share_bps
            .checked_add(freelancer_share_bps)
            .ok_or(Error::InvalidRatio)?;
        if total != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }
        Ok(())
    }

    /// Validate an admin-configured annual yield rate in basis points.
    /// `0` is allowed (disables accrual); values above `BPS_SCALE` are rejected.
    fn validate_yield_rate_bps(rate_bps: u32) -> Result<(), Error> {
        if rate_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }
        Ok(())
    }

    /// Single source of truth for reading the interest/yield share
    /// configuration out of instance storage.
    ///
    /// Every caller — the public read paths `get_escrow_interest_yield` and
    /// `is_escrow_interest_yield_locked`, and the write-path precondition
    /// guards that only need to know whether the configuration is locked —
    /// delegates here, so `DataKey::InterestYieldState` is named in exactly one
    /// place and each invocation issues exactly one ledger read.
    ///
    /// **This function must contain only read operations.** It performs a
    /// single instance-storage `get` and must never `set`, `remove`, or
    /// `extend_ttl` any instance, persistent, or temporary entry, and must
    /// never publish an event: a read path that wrote state could silently
    /// rewrite the ledger or shorten the TTL of the entry it inspects.
    /// `get_escrow_interest_yield_no_mutation_tests` pins that guarantee by
    /// comparing whole-ledger snapshots (`Env::to_ledger_snapshot`) taken
    /// around the public read paths that call this helper.
    fn load_interest_yield_state(env: &Env) -> Result<EscrowInterestYieldState, Error> {
        Self::cold_get(env, &DataKey::InterestYieldState).ok_or(Error::NotInitialized)
    }

    /// Read `DataKey::PlatformFeeAllocation` (persistent, see `cold_get`).
    ///
    /// This is the single source of truth for the platform-fee allocation read
    /// path.  Every function that needs the stored value — including the public
    /// `get_platform_fee_allocation`, `calculate_platform_fee_split`, and the
    /// write-path helpers — delegates here so the storage key is referenced in
    /// exactly one place and each invocation issues exactly one ledger read.
    fn load_platform_fee_allocation(env: &Env) -> Result<PlatformFeeAllocation, Error> {
        Self::cold_get(env, &DataKey::PlatformFeeAllocation).ok_or(Error::NotInitialized)
    }

    /// Read the multisig deadlock flag. `MultisigLocked` is only written by
    /// `multisig_lock`, so an initialized escrow that was never locked has no
    /// entry and reads as `false`; only a contract without an instance `Admin`
    /// (never initialized) reports `NotInitialized`.
    ///
    /// **This function must contain only read operations.**
    fn load_multisig_locked(env: &Env) -> Result<bool, Error> {
        Self::require_initialized(env)?;
        Ok(env
            .storage()
            .instance()
            .get(&DataKey::MultisigLocked)
            .unwrap_or(false))
    }

    fn store_interest_yield_state(env: &Env, state: &EscrowInterestYieldState) {
        env.storage()
            .persistent()
            .set(&DataKey::InterestYieldState, state);
    }

    /// Calculate net distributions for a split refund by applying the platform
    /// fee allocation only to the freelancer's payout portion. The client's refund
    /// is fee-exempt.
    ///
    /// # Rounding Specification
    /// When basis-point calculations do not divide evenly:
    /// * Rounding Direction: Uses explicit round-to-nearest arithmetic (half rounds up)
    ///   via `split_round_nearest` (`(amount * bps + 5_000) / 10_000`).
    /// * Gross Split: `client_net_refund` is rounded to nearest stroop; `gross_payout`
    ///   receives the exact remainder (`total_amount - client_net_refund`), ensuring
    ///   no unit is created or destroyed.
    /// * Fee Deductions: `client_fee_share` and `treasury_fee_share` are rounded to
    ///   nearest stroop; `freelancer_net_payout` receives the remaining balance
    ///   (`gross_payout - client_fee_share - treasury_fee_share`).
    /// * Conservation Invariant: All four return values sum exactly to the input amount:
    ///   `client_net_refund + client_fee_share + freelancer_net_payout + treasury_fee_share == total_amount`.
    ///   Remainders are never silently discarded.
    ///
    /// # Parameters
    /// * `total_amount`          – Total amount to split; must be > 0.
    /// * `client_refund_bps`     – Client refund share in basis points (0–10 000).
    /// * `freelancer_payout_bps` – Freelancer payout share in basis points (0–10 000).
    ///                             The two BPS values must sum to `BPS_SCALE` (10 000).
    /// * `fee_allocation`        – Platform fee allocation applied to the freelancer's gross payout.
    ///
    /// # Returns
    /// A `SplitRefundFeeDistribution` detailing net amounts and fee shares.
    ///
    /// # Errors
    /// * `Paused`        – Contract is currently paused.
    /// * `InvalidAmount` – `total_amount` ≤ 0 or arithmetic underflow.
    /// * `InvalidRatio`  – `client_refund_bps + freelancer_payout_bps != 10_000`.
    ///
    /// # Validation order
    /// `total_amount` and the BPS ratio are validated before any storage read,
    /// so an invalid argument is rejected without touching the ledger; the
    /// admin and emergency pause checks run afterwards.
    pub fn split_refund_net_distribution(
        env: Env,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
        fee_allocation: PlatformFeeAllocation,
    ) -> Result<SplitRefundFeeDistribution, Error> {
        // Precondition checks before any storage access
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        // After preconditions pass, check authorization and contract state
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // 1. Calculate gross split with explicit round-to-nearest arithmetic.
        // Client receives the nearest stroop; freelancer payout receives the exact
        // remainder so client_net_refund + gross_payout == total_amount.
        let client_split =
            Self::split_round_nearest(total_amount, client_refund_bps as i128, BPS_SCALE as i128)?;
        let client_net_refund = client_split.first;
        let gross_payout = total_amount
            .checked_sub(client_net_refund)
            .ok_or(Error::InvalidAmount)?;

        // 2. Calculate fee shares from gross payout using explicit round-to-nearest.
        let client_fee_share = Self::split_round_nearest(
            gross_payout,
            fee_allocation.client_bps as i128,
            BPS_SCALE as i128,
        )?
        .first;

        let treasury_fee_share = Self::split_round_nearest(
            gross_payout,
            fee_allocation.treasury_bps as i128,
            BPS_SCALE as i128,
        )?
        .first;

        // 3. Freelancer net payout receives the remaining gross payout after fees.
        // Remainder is never discarded, ensuring:
        // client_net_refund + client_fee_share + freelancer_net_payout + treasury_fee_share == total_amount.
        let freelancer_net_payout = gross_payout
            .checked_sub(client_fee_share)
            .and_then(|v| v.checked_sub(treasury_fee_share))
            .ok_or(Error::InvalidAmount)?;

        let distribution = SplitRefundFeeDistribution {
            client_net_refund,
            client_fee_share,
            freelancer_net_payout,
            treasury_fee_share,
        };

        // 4. Emit a structured event carrying the inputs and the resulting state.
        env.events().publish(
            (symbol_short!("sprefnet"),),
            SplitRefundNetDistributionEvent {
                total_amount,
                client_refund_bps,
                freelancer_payout_bps,
                client_net_refund: distribution.client_net_refund,
                client_fee_share: distribution.client_fee_share,
                freelancer_net_payout: distribution.freelancer_net_payout,
                treasury_fee_share: distribution.treasury_fee_share,
            },
        );

        Ok(distribution)
    }

    fn ensure_interest_yield_unlocked(env: &Env) -> Result<(), Error> {
        let state = Self::load_interest_yield_state(env)?;
        if state.locked {
            return Err(Error::EscrowLocked);
        }
        Ok(())
    }

    /// Precondition guard for callers that are about to *create or replace* the
    /// interest/yield share configuration.
    ///
    /// `ensure_interest_yield_unlocked` maps a missing state to
    /// `NotInitialized`, which is what the read paths
    /// (`get_escrow_interest_yield`, `is_escrow_interest_yield_locked`) need.
    /// A write path cannot reuse it: the very first `set_escrow_interest_yield`
    /// call is *expected* to find no state and is supposed to create it. Such
    /// callers therefore had to pair a `has()` probe with the `ensure` call,
    /// costing two reads of the same instance entry.
    ///
    /// This helper collapses the pair into a single read, so a caller that
    /// replaces the configuration pays one ledger read instead of two:
    /// * absent state  – not locked, so the write is allowed to create it;
    /// * present, `locked == false` – writable;
    /// * present, `locked == true`  – the illegal source state, `EscrowLocked`.
    fn ensure_interest_yield_writable(env: &Env) -> Result<(), Error> {
        let state: Option<EscrowInterestYieldState> =
            Self::cold_get(env, &DataKey::InterestYieldState);
        if state.is_some_and(|state| state.locked) {
            return Err(Error::EscrowLocked);
        }
        Ok(())
    }

    /// Validate the inputs of a streaming ratio split and prove up front that
    /// the scaled product [`Self::split_round_nearest`] is about to compute is
    /// representable in `i128`.
    ///
    /// # Why the product is probed separately
    ///
    /// `split_round_nearest` performs its own `checked_mul` on `total *
    /// numerator`, so it can never panic or wrap — but it does so *inside* the
    /// caller's execution-locked section.  Probing the same product here, with
    /// `i128::checked_mul`, means an out-of-range `total_amount` is turned away
    /// with the typed `InvalidAmount` error **before** the caller takes its
    /// execution lock, so a doomed invocation writes no ledger entry at all
    /// rather than relying on invocation rollback to undo one.
    ///
    /// Every `i128` operation on the streaming-consent path is checked: the
    /// product here (`checked_mul`), and the product, the rounding bias and the
    /// remainder inside [`Self::split_round_nearest`] (`checked_mul`,
    /// `checked_add`, `checked_sub`).
    ///
    /// # Errors
    /// * `InvalidAmount` – `total_amount <= 0`, or `total_amount * numerator`
    ///   overflows `i128`.
    /// * `InvalidRatio`  – `denominator <= 0`, or `numerator` outside
    ///   `0..=denominator`.
    fn validate_streaming_ratio(
        total_amount: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<(), Error> {
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if denominator <= 0 {
            return Err(Error::InvalidRatio);
        }
        if numerator < 0 || numerator > denominator {
            return Err(Error::InvalidRatio);
        }
        // The only multiplication performed on this path.  Probing it with
        // `checked_mul` is what turns an `i128::MAX`-scale `total_amount` into
        // a typed `InvalidAmount` instead of a panic or a wrapped product.
        total_amount
            .checked_mul(numerator)
            .ok_or(Error::InvalidAmount)?;
        Ok(())
    }

    fn split_round_nearest(
        total: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        let (first, second) = math::split_ratio_nearest(total, numerator, denominator)?;
        Ok(RatioSplit { first, second })
    }

    /// Split `total_amount` across the three configured platform-fee ratios.
    ///
    /// # Overflow policy
    ///
    /// `total_amount` is caller-supplied, so the intermediate
    /// `total_amount * bps` products are reachable overflow candidates: a
    /// `total_amount` of `i128::MAX` against any non-zero weight cannot be
    /// represented. Every `i128` operation below is therefore a `checked_*`
    /// counterpart, and every failure maps to [`Error::ArithmeticOverflow`]
    /// rather than a panic (release builds abort on overflow) or a silent wrap.
    /// `checked_div` / `checked_rem` are used instead of `/` and `%` so that a
    /// zero `scale` would surface as the same typed error instead of a
    /// divide-by-zero trap; `scale` is the `BPS_SCALE` constant and can never
    /// be zero today, so this is defence in depth.
    ///
    /// # Atomicity
    ///
    /// All three shares are computed into a local `[i128; 3]` and only returned
    /// once every step has succeeded, so a failure on the last party cannot
    /// leave a partially-populated `PlatformFeeDistribution` behind. The
    /// helper also performs no storage writes; the only externally visible
    /// side effect of `calculate_platform_fee_split` is the `pf_split` event,
    /// which is published strictly after this helper returns `Ok`.
    ///
    /// # Errors
    ///
    /// * `InvalidAmount`      – `total_amount` is negative.
    /// * `ArithmeticOverflow` – any intermediate `i128` value is not
    ///   representable, including the `i128::MIN` input (whose magnitude has no
    ///   `i128` counterpart, so the whole-total distribution below cannot be
    ///   expressed).
    fn allocate_platform_fee(
        total_amount: i128,
        allocation: &PlatformFeeAllocation,
    ) -> Result<PlatformFeeDistribution, Error> {
        // `i128::MIN` is the single input for which the *magnitude* of the
        // total is not representable as an `i128`. Because this helper
        // distributes the entire total and asserts that the three shares sum
        // back to it, an `i128::MIN` total has no valid decomposition. It is
        // reported as an arithmetic overflow (rather than a range error) so the
        // failure mode is unambiguous; every other negative total remains a
        // plain `InvalidAmount`.
        if total_amount == i128::MIN {
            return Err(Error::ArithmeticOverflow);
        }
        if total_amount < 0 {
            return Err(Error::InvalidAmount);
        }

        let scale = BPS_SCALE as i128;
        let ratios = [
            allocation.client_bps as i128,
            allocation.freelancer_bps as i128,
            allocation.treasury_bps as i128,
        ];
        let mut amounts = [0_i128; 3];
        let mut remainders = [0_i128; 3];
        let mut allocated = 0_i128;

        for index in 0..3 {
            let weighted = total_amount
                .checked_mul(ratios[index])
                .ok_or(Error::ArithmeticOverflow)?;
            // `checked_div` / `checked_rem` also reject `i128::MIN / -1`, which
            // the plain operators trap on; `scale` is positive so neither can
            // trip, but going through the checked forms keeps the whole helper
            // free of panic paths.
            amounts[index] = weighted
                .checked_div(scale)
                .ok_or(Error::ArithmeticOverflow)?;
            remainders[index] = weighted
                .checked_rem(scale)
                .ok_or(Error::ArithmeticOverflow)?;
            allocated = allocated
                .checked_add(amounts[index])
                .ok_or(Error::ArithmeticOverflow)?;
        }

        // Largest-remainder allocation preserves every unit. Ties are resolved
        // by field order, making the result deterministic across runtimes.
        // The three weights always sum to `BPS_SCALE`, so `remaining` is at most
        // two and the loop below is bounded regardless of `total_amount`.
        let mut remaining = total_amount
            .checked_sub(allocated)
            .ok_or(Error::ArithmeticOverflow)?;
        while remaining > 0 {
            let mut best = 0_usize;
            for index in 1..3 {
                if remainders[index] > remainders[best] {
                    best = index;
                }
            }
            amounts[best] = amounts[best]
                .checked_add(1)
                .ok_or(Error::ArithmeticOverflow)?;
            // Retire this party so it cannot win a second residue unit. Every
            // live remainder is in `0..scale` and therefore strictly greater
            // than `i128::MIN`, so the sentinel can never be selected again.
            remainders[best] = i128::MIN;
            remaining = remaining.checked_sub(1).ok_or(Error::ArithmeticOverflow)?;
        }

        // Final conservation check. `allocated` plus the residue units handed
        // out above must reconstruct `total_amount` exactly; if it does not,
        // fail loudly rather than returning a split that does not sum.
        let distributed = amounts[0]
            .checked_add(amounts[1])
            .and_then(|v| v.checked_add(amounts[2]))
            .ok_or(Error::ArithmeticOverflow)?;
        if distributed != total_amount {
            return Err(Error::ArithmeticOverflow);
        }

        Ok(PlatformFeeDistribution {
            client_amount: amounts[0],
            freelancer_amount: amounts[1],
            treasury_amount: amounts[2],
        })
    }

    fn load_job_meta(env: &Env) -> Result<JobMeta, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)
    }

    fn store_job_meta(env: &Env, meta: &JobMeta) {
        env.storage().instance().set(&DataKey::Job, meta);
    }

    fn load_acceptance(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Acceptance)
            .unwrap_or(0)
    }

    /// Reject with `NotAccepted` unless both the freelancer and the arbiter
    /// have accepted the job.
    fn require_accepted(env: &Env) -> Result<(), Error> {
        if Self::load_acceptance(env) != 3 {
            return Err(Error::NotAccepted);
        }
        Ok(())
    }

    /// Record `bit` in the acceptance mask on behalf of `party` and publish
    /// the acceptance under `topic`.
    fn record_acceptance(env: &Env, party: Address, bit: u32, topic: Symbol) -> Result<(), Error> {
        let mask = Self::load_acceptance(env);
        if mask & bit != 0 {
            return Err(Error::InvalidStatus);
        }
        let new_mask = mask | bit;
        env.storage()
            .instance()
            .set(&DataKey::Acceptance, &new_mask);

        env.events().publish(
            (topic,),
            JobAcceptedEvent {
                party,
                acceptance_mask: new_mask,
            },
        );
        Ok(())
    }

    fn load_milestone(env: &Env, index: u32) -> Result<Milestone, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Milestone(index))
            .ok_or(Error::InvalidMilestone)
    }

    fn store_milestone(env: &Env, index: u32, milestone: &Milestone) {
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(index), milestone);
        env.storage().persistent().extend_ttl(
            &DataKey::Milestone(index),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );
        Self::bump(env);
    }

    fn load_funding_mode(env: &Env) -> FundingMode {
        env.storage()
            .instance()
            .get(&DataKey::FundingMode)
            .unwrap_or(FundingMode::Upfront)
    }

    fn require_escrow_has_funding(env: &Env, meta: &JobMeta) -> Result<(), Error> {
        if meta.funded {
            return Ok(());
        }
        if Self::load_funding_mode(env) == FundingMode::PerMilestone {
            for index in 0..meta.milestone_count {
                if Self::load_milestone(env, index)?.funded {
                    return Ok(());
                }
            }
        }
        Err(Error::NotFunded)
    }

    fn require_milestone_is_funded(
        env: &Env,
        meta: &JobMeta,
        milestone: &Milestone,
    ) -> Result<(), Error> {
        match Self::load_funding_mode(env) {
            FundingMode::Upfront => {
                if meta.funded {
                    Ok(())
                } else {
                    Err(Error::NotFunded)
                }
            }
            FundingMode::PerMilestone => {
                if Self::milestone_is_funded(meta, milestone) {
                    Ok(())
                } else {
                    Err(Error::NotFunded)
                }
            }
        }
    }

    /// A milestone counts as funded when the whole job is funded (Upfront, or
    /// PerMilestone once every milestone is funded) or when it was funded on
    /// its own. Upfront funding does not rewrite each milestone, which keeps
    /// `fund` at a constant number of ledger writes.
    fn milestone_is_funded(meta: &JobMeta, milestone: &Milestone) -> bool {
        meta.funded || milestone.funded
    }

    fn is_terminal_status(status: &MilestoneStatus) -> bool {
        matches!(
            status,
            MilestoneStatus::Released | MilestoneStatus::Refunded | MilestoneStatus::Split
        )
    }

    fn outstanding_escrow_obligations(env: &Env, meta: &JobMeta) -> Result<i128, Error> {
        let mut outstanding: i128 = 0;
        for index in 0..meta.milestone_count {
            let milestone = Self::load_milestone(env, index)?;
            if Self::is_terminal_status(&milestone.status) {
                continue;
            }
            let remaining = milestone
                .amount
                .checked_sub(milestone.released_amount)
                .ok_or(Error::ArithmeticOverflow)?;
            if remaining > 0 {
                outstanding = outstanding
                    .checked_add(remaining)
                    .ok_or(Error::ArithmeticOverflow)?;
            }
        }
        let yield_accrued: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0);
        if yield_accrued > 0 {
            outstanding = outstanding
                .checked_add(yield_accrued)
                .ok_or(Error::ArithmeticOverflow)?;
        }
        // Amounts credited to parties whose transfer failed (#755) are still
        // owed and must never be swept.
        outstanding = outstanding
            .checked_add(Self::load_total_owed(env))
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(outstanding)
    }

    /// Write the delivery timestamp to temporary storage.  Temporary entries
    /// are automatically evicted by the network after their TTL expires, which
    /// makes them the correct storage tier for single-use, deadline-scoped
    /// workflow state like the auto-release window.
    fn store_delivered_at(env: &Env, index: u32, timestamp: u64) {
        env.storage()
            .temporary()
            .set(&DataKey::DeliveredAt(index), &timestamp);
    }

    /// Read the delivery timestamp from temporary storage.  Returns `None` if
    /// the entry has already been evicted (TTL expired) or was never written.
    fn load_delivered_at(env: &Env, index: u32) -> Option<u64> {
        env.storage().temporary().get(&DataKey::DeliveredAt(index))
    }

    fn auto_release_deadline(
        meta: &JobMeta,
        delivered_at: u64,
        extension: u32,
    ) -> Result<u64, Error> {
        delivered_at
            .checked_add(meta.auto_release_seconds)
            .and_then(|deadline| deadline.checked_add(extension as u64))
            .ok_or(Error::ArithmeticOverflow)
    }

    /// Write the terminal approval flag to temporary storage.  This is a
    /// cheap, short-lived signal that the milestone at `index` has been fully
    /// released via `approve_milestone`.  Callers that only need to verify
    /// completion can read this temporary key rather than fetching the full
    /// persistent `Milestone` entry, reducing ledger footprint rent on the
    /// hot read path.
    fn store_milestone_released(env: &Env, index: u32) {
        env.storage()
            .temporary()
            .set(&DataKey::MilestoneReleased(index), &true);
    }

    fn load_time_extension(env: &Env, index: u32) -> u32 {
        env.storage()
            .temporary()
            .get(&DataKey::TimeExt(index))
            .or_else(|| env.storage().persistent().get(&DataKey::TimeExt(index)))
            .unwrap_or(0)
    }

    /// Check whether `approve_milestone` has marked the given milestone index
    /// as fully released via the temporary completion flag.  Returns `false`
    /// if the flag was never written or has been evicted.
    #[allow(dead_code)]
    fn is_milestone_released_flag(env: &Env, index: u32) -> bool {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::MilestoneReleased(index))
            .unwrap_or(false)
    }

    /// Persist the applied client-refund BPS under the compact temporary key
    /// `ArbitrationSplitBps(index)`.  Only a single `u32` is written — the
    /// freelancer share is always `BPS_SCALE - client_refund_bps` and is never
    /// stored separately.
    fn store_arbitration_split_bps(env: &Env, index: u32, client_refund_bps: u32) {
        env.storage()
            .temporary()
            .set(&DataKey::ArbitrationSplitBps(index), &client_refund_bps);
    }

    /// Read the compact arbitration-split BPS for `index` from temporary
    /// storage.  Returns `None` if the entry was never written or was evicted.
    fn load_arbitration_split_bps(env: &Env, index: u32) -> Option<u32> {
        env.storage()
            .temporary()
            .get(&DataKey::ArbitrationSplitBps(index))
    }

    /// Cheap presence check for whether an arbitration split has been applied
    /// to `index`, without loading the full persistent `Milestone` entry.
    #[allow(dead_code)]
    fn is_arbitration_split_applied(env: &Env, index: u32) -> bool {
        Self::load_arbitration_split_bps(env, index).is_some()
    }

    // ── pause guard ──────────────────────────────────────────────────────────

    /// Return `Err(Error::EscrowPaused)` when an admin pause is active so that
    /// every user-facing endpoint can call this as its first operation.
    fn assert_not_paused(env: &Env) -> Result<(), Error> {
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused {
            return Err(Error::Paused);
        }
        Ok(())
    }

    fn increment_reputation(env: &Env, address: &Address) {
        let key = DataKey::Reputation(address.clone());
        let current: u32 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(current + 1));
    }

    fn checked_add_amount(total: i128, amount: i128) -> Result<i128, Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        total.checked_add(amount).ok_or(Error::InvalidAmount)
    }

    /// Sum all milestone amounts, returning `Err(InvalidAmount)` if:
    /// - the list is empty (no milestones to escrow),
    /// - any individual amount is `≤ 0` (enforced by [`checked_add_amount`]),
    /// - the running total overflows `i128` (enforced by `i128::checked_add`
    ///   inside [`checked_add_amount`]).
    ///
    /// Every addition goes through `i128::checked_add`, so no unchecked
    /// arithmetic is performed regardless of input size or value.
    ///
    /// [`checked_add_amount`]: Self::checked_add_amount
    fn checked_initialize_total(milestone_amounts: &Vec<i128>) -> Result<i128, Error> {
        if milestone_amounts.is_empty() {
            return Err(Error::InvalidAmount);
        }

        let mut total_amount: i128 = 0;
        for amount in milestone_amounts.iter() {
            // checked_add_amount rejects amount ≤ 0 and uses i128::checked_add
            // to catch overflow, returning Err(InvalidAmount) in both cases.
            total_amount = Self::checked_add_amount(total_amount, amount)?;
        }

        Ok(total_amount)
    }

    fn validate_fund_amount(meta: &JobMeta) -> Result<i128, Error> {
        let total_amount = meta.total_amount;
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // `total_amount` is kept in step with the milestones by initialize,
        // add_milestone and reduce_milestone, so fund needs no milestone reads
        // (#730).

        Ok(total_amount)
    }

    fn validate_fund_client(env: &Env, client: &Address) -> Result<(), Error> {
        if client == &env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        Ok(())
    }

    fn validate_address(env: &Env, address: &Address) -> Result<(), Error> {
        let zero_account = Address::from_str(
            env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if address == &zero_account
            || address == &zero_contract
            || address == &env.current_contract_address()
        {
            return Err(Error::InvalidAddress);
        }

        Ok(())
    }

    /// Validate the target of an admin transfer (issue #771).
    ///
    /// On top of the zero sentinels, `validate_address` rejects the escrow's
    /// own address: the contract cannot sign an external transaction, so
    /// making it its own admin would leave every admin endpoint permanently
    /// unusable. Re-proposing the current admin is also refused — it is a
    /// no-op that would still cost a full multisig round and emit misleading
    /// transfer events.
    fn validate_new_admin(
        env: &Env,
        stored_admin: &Address,
        new_admin: &Address,
    ) -> Result<(), Error> {
        Self::validate_address(env, new_admin)?;
        if new_admin == stored_admin {
            return Err(Error::InvalidAddress);
        }
        Ok(())
    }

    fn assemble_job(env: &Env, meta: &JobMeta) -> Result<Job, Error> {
        let mut milestones = Vec::new(env);
        for i in 0..meta.milestone_count {
            let mut milestone = Self::load_milestone(env, i)?;
            milestone.funded = Self::milestone_is_funded(meta, &milestone);
            milestones.push_back(milestone);
        }
        Ok(Job {
            client: meta.client.clone(),
            freelancer: meta.freelancer.clone(),
            arbiter: meta.arbiter.clone(),
            token: meta.token.clone(),
            milestones,
            funded: meta.funded,
            auto_release_seconds: meta.auto_release_seconds,
        })
    }

    /// Initialize a new milestone escrow job.
    ///
    /// Sets up the client/freelancer/arbiter relationship, the settlement
    /// token, and the milestone schedule. Must be called exactly once; every
    /// other state-mutating endpoint checks for prior initialization and
    /// returns `NotInitialized` if this call was never made. The settlement
    /// token is automatically added to the whitelist, and the platform fee
    /// allocation defaults to 100 % freelancer / 0 % client / 0 % treasury.
    ///
    /// All integer arithmetic (summing `milestone_amounts`) is performed via
    /// the internal `checked_initialize_total` helper, which uses
    /// `i128::checked_add` on every addition and rejects non-positive amounts,
    /// so no arithmetic can overflow or wrap silently — a typed `InvalidAmount`
    /// error is returned instead.
    ///
    /// # Parameters
    /// * `admin`                – Address that will control admin-only
    ///                           endpoints (whitelist management, pause /
    ///                           resume, admin overrides, yield config).
    ///                           Must authorize the call.
    /// * `client`               – Address that funds the job and approves
    ///                           milestone releases.
    /// * `freelancer`           – Address that delivers milestones and
    ///                           receives payouts.
    /// * `arbiter`              – Address that resolves disputes via
    ///                           `resolve_dispute`.
    /// * `token`                – Settlement token contract address used for
    ///                           all deposits and payouts.
    /// * `auto_release_seconds` – Seconds after a milestone is marked
    ///                           delivered before it becomes eligible for
    ///                           `claim_auto_release`.  Must be non-zero.
    ///                           The default dispute timeout is 30 days; use
    ///                           `initialize_with_dispute_timeout` to set a
    ///                           custom timeout.
    /// * `milestone_amounts`    – Ordered list of token amounts owed per
    ///                           milestone.  Must be non-empty; every
    ///                           individual amount must be strictly positive
    ///                           (`> 0`).
    /// * `milestone_spec_hashes` – Ordered 32-byte hashes of the corresponding
    ///                            off-chain milestone descriptions.
    ///
    /// # Returns
    /// `Ok(())` on success.  At that point the following state has been
    /// committed atomically:
    /// * `DataKey::Job` (instance storage) – `JobMeta` with the supplied
    ///   parties, token, `auto_release_seconds`, milestone count, and the sum
    ///   of `milestone_amounts` as `total_amount`.
    /// * `DataKey::Milestone(i)` (persistent storage) – one `Milestone` entry
    ///   per element of `milestone_amounts`, each starting in
    ///   `MilestoneStatus::Pending` with `released_amount = 0`.
    /// * Each `Milestone.spec_hash` – the corresponding hash from
    ///   `milestone_spec_hashes`.
    /// * `DataKey::Admin` (instance **and** persistent storage) – the `admin`
    ///   address.
    /// * `DataKey::Version` (instance storage) – version marker `1u32`.
    /// * `DataKey::Ep` (instance storage) – emergency-pause flag set to
    ///   `false`.
    /// * `DataKey::PlatformFeeAllocation` (persistent storage) – fee allocation
    ///   defaulting to 100 % freelancer / 0 % client / 0 % treasury.
    /// * `DataKey::WhitelistedTokens` (persistent storage) – whitelist
    ///   initialized with `token` as its sole entry.
    ///
    /// An `"init"` event carrying an [`InitializedEvent`] is published after
    /// all state is written. It includes the full `milestone_amounts` vec and
    /// corresponding `milestone_spec_hashes`.
    ///
    /// On any error path the Soroban host rolls back all storage writes made
    /// during this invocation, so the contract remains in its uninitialized state
    /// and a subsequent valid call will succeed.
    ///
    /// # Errors
    /// Errors are returned in the order the corresponding checks appear in the
    /// function body.
    ///
    /// * [`Error::AlreadyInitialized`] – `DataKey::Job` is already present in
    ///   instance storage, meaning `initialize` has already completed
    ///   successfully (or a prior attempt wrote the reentrancy sentinel and the
    ///   transaction was not rolled back).  No state is mutated.
    ///
    /// * [`Error::InvalidAddress`] – Any address argument fails the internal
    ///   `validate_address` check, which rejects:
    ///   - The Stellar zero account
    ///     (`GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF`),
    ///   - The canonical Soroban zero contract address
    ///     (`CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4`),
    ///   - The escrow contract's own address
    ///     (`env.current_contract_address()`).
    ///   The check is applied in parameter order: `admin`, `client`,
    ///   `freelancer`, `arbiter`, `token`.
    ///
    /// * [`Error::InvalidParties`] – Two or more party addresses collide.
    ///   The escrow trust model requires four distinct addresses: the admin
    ///   (platform), client, freelancer, and arbiter (neutral dispute
    ///   resolver) must all be different.  Any of the following collisions
    ///   is rejected: `client == freelancer`, `arbiter == client`,
    ///   `arbiter == freelancer`, `admin == client`, `admin == freelancer`.
    ///   The check runs immediately after the zero-address validation so a
    ///   collision is caught before any milestone amount arithmetic runs.
    ///
    /// * [`Error::InvalidAmount`] – Any of the following conditions:
    ///   - `milestone_amounts` is empty (no milestones to escrow).
    ///   - `milestone_spec_hashes.len()` differs from
    ///     `milestone_amounts.len()`.
    ///   - Any individual milestone amount is `≤ 0` (zero or negative amounts
    ///     are not valid escrow values).
    ///   - The sum of all milestone amounts overflows `i128`.
    ///
    /// * [`Error::InvalidDuration`] – `auto_release_seconds` is `0` or exceeds
    ///   `MAX_AUTO_RELEASE_SECONDS` (10 years), which would disable the
    ///   auto-release window or risk arithmetic overflow in deadline
    ///   calculations.
    ///
    /// * [`Error::InvalidMilestone`] – An element of `milestone_amounts` could
    ///   not be retrieved by index during the milestone-storage loop.  In
    ///   practice this indicates an internal SDK-level error rather than a
    ///   caller mistake.
    ///
    /// Unit tests are built with a no-argument constructor instead (below), so
    /// they can register the contract and call the test-only `initialize`;
    /// `constructor_tests` deploys a release build to cover this one.
    #[cfg(not(test))]
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        settings: EscrowSettings,
    ) -> Result<(), Error> {
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            settings,
        )
    }

    /// Test builds only: lets unit tests register the contract without
    /// constructor arguments and initialise it through `initialize`.
    #[cfg(test)]
    pub fn __constructor(_env: Env) {}

    /// Shared body of `__constructor` and the test-only initialisers.
    ///
    /// # Errors
    /// * [`Error::InvalidRatio`] – `settings.arbiter_fee_bps` exceeds
    ///   `MAX_ARBITER_FEE_BPS`.
    /// * [`Error::InvalidDisputeTimeout`] – `settings.dispute_timeout_seconds`
    ///   is zero.
    /// * Every error documented on `__constructor`.
    #[allow(clippy::too_many_arguments)]
    fn initialize_with_settings(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        settings: EscrowSettings,
    ) -> Result<(), Error> {
        if settings.arbiter_fee_bps > MAX_ARBITER_FEE_BPS {
            return Err(Error::InvalidRatio);
        }
        Self::initialize_inner(
            env.clone(),
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            settings.dispute_timeout_seconds,
            milestone_amounts,
            milestone_spec_hashes,
        )?;
        if settings.funding_mode != FundingMode::Upfront {
            env.storage()
                .instance()
                .set(&DataKey::FundingMode, &settings.funding_mode);
        }
        if settings.arbiter_fee_bps > 0 {
            env.storage()
                .instance()
                .set(&DataKey::ArbiterFeeBps, &settings.arbiter_fee_bps);
        }
        Ok(())
    }

    /// The default settings: upfront funding, the standard dispute timeout
    /// and no arbiter fee.
    #[cfg(test)]
    fn default_settings() -> EscrowSettings {
        EscrowSettings {
            funding_mode: FundingMode::Upfront,
            dispute_timeout_seconds: DISPUTE_TIMEOUT_SECONDS,
            arbiter_fee_bps: 0,
        }
    }

    /// Test-only initialiser with default settings, for tests that register
    /// the contract without constructor arguments.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            Self::default_settings(),
        )
    }

    /// Test-only: `initialize` with a custom dispute timeout.
    ///
    /// After `dispute_timeout_seconds`, either the client or freelancer may
    /// call `escalate_dispute` to restore the milestone to its status before
    /// the dispute was raised. The timeout must be non-zero.
    ///
    /// # Errors
    /// * [`Error::InvalidDisputeTimeout`] – `dispute_timeout_seconds` is zero.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_with_dispute_timeout(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        dispute_timeout_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            EscrowSettings {
                dispute_timeout_seconds,
                ..Self::default_settings()
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn initialize_inner(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        dispute_timeout_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
    ) -> Result<(), Error> {
        // No `admin.require_auth()` here: the constructor runs atomically with
        // the deploy, so nobody can initialise the contract first, and the
        // factory (#666) must be able to name the platform admin without the
        // admin co-signing every job. The test-only initialisers below still
        // require it.
        if env.storage().instance().has(&DataKey::Job) {
            return Err(Error::AlreadyInitialized);
        }

        // Validate all inputs BEFORE writing any state.
        Self::validate_address(&env, &admin)?;
        Self::validate_address(&env, &client)?;
        Self::validate_address(&env, &freelancer)?;
        Self::validate_address(&env, &arbiter)?;
        Self::validate_address(&env, &token)?;

        // Party-distinctness check (issue #667): the escrow's protection
        // depends on four independent roles.  Collisions remove checks and
        // balances — e.g. client == arbiter lets the client resolve their own
        // disputes, and admin == client gives the client every admin override.
        if client == freelancer
            || arbiter == client
            || arbiter == freelancer
            || admin == client
            || admin == freelancer
        {
            return Err(Error::InvalidParties);
        }

        // milestone_count is u32 (Soroban Vec::len() returns u32) so no cast
        // is needed and there is no overflow risk on the count itself.
        let milestone_count = milestone_amounts.len();
        // All per-amount and running-total arithmetic is performed inside
        // checked_initialize_total via i128::checked_add (see its rustdoc).
        // Non-positive amounts and sum overflow both return Err(InvalidAmount).
        let total_amount = Self::checked_initialize_total(&milestone_amounts)?;
        if milestone_spec_hashes.len() != milestone_count {
            return Err(Error::InvalidAmount);
        }

        // auto_release_seconds must be non-zero and have a reasonable upper bound
        // to prevent overflow in deadline calculations (e.g. claim_auto_release).
        if auto_release_seconds == 0 || auto_release_seconds > MAX_AUTO_RELEASE_SECONDS {
            return Err(Error::InvalidDuration);
        }
        if dispute_timeout_seconds == 0 {
            return Err(Error::InvalidDisputeTimeout);
        }

        // Write a sentinel immediately to prevent reentrancy: any reentrant
        // call to `initialize` will now see `DataKey::Job` already present and
        // return `AlreadyInitialized` before touching any other state.
        // The sentinel is a zero-value `JobMeta` placeholder; the real meta
        // overwrites it at the end of this function once all validation has
        // passed and milestones have been stored.
        let created_at = env.ledger().timestamp();
        env.storage().instance().set(
            &DataKey::Job,
            &JobMeta {
                client: admin.clone(),
                freelancer: admin.clone(),
                arbiter: admin.clone(),
                token: admin.clone(),
                created_at,
                funded: false,
                auto_release_seconds: 0,
                milestone_count: 0,
                total_amount: 0,
            },
        );

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().persistent().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Ep, &false);
        env.storage().persistent().set(
            &DataKey::PlatformFeeAllocation,
            &PlatformFeeAllocation {
                client_bps: 0,
                freelancer_bps: BPS_SCALE,
                treasury_bps: 0,
                locked: false,
            },
        );

        let mut whitelist: Vec<Address> = Vec::new(&env);
        whitelist.push_back(token.clone());
        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedTokens, &whitelist);
        if dispute_timeout_seconds != DISPUTE_TIMEOUT_SECONDS {
            env.storage()
                .instance()
                .set(&DataKey::DisputeTimeout, &dispute_timeout_seconds);
        }

        for index in 0..milestone_count {
            let amount = milestone_amounts
                .get(index)
                .ok_or(Error::InvalidMilestone)?;
            let spec_hash = milestone_spec_hashes
                .get(index)
                .ok_or(Error::InvalidMilestone)?;
            Self::store_milestone(
                &env,
                index,
                &Milestone {
                    amount,
                    released_amount: 0,
                    status: MilestoneStatus::Pending,
                    delivered_at: 0,
                    funded: false,
                    spec_hash,
                    deliverable: BytesN::from_array(&env, &[0u8; 32]),
                },
            );
        }

        env.storage().instance().set(&DataKey::Version, &1u32);
        env.storage()
            .instance()
            .set(&DataKey::SchemaVersion, &SCHEMA_VERSION);

        let meta = JobMeta {
            client,
            freelancer,
            arbiter,
            token,
            created_at,
            funded: false,
            auto_release_seconds,
            milestone_count,
            total_amount,
        };

        Self::store_job_meta(&env, &meta);
        env.storage()
            .instance()
            .set(&DataKey::FundingMode, &FundingMode::Upfront);

        // Emit a structured initialization event so downstream indexers can
        // record all operational parameters from a single on-chain event without
        // having to query contract storage separately.
        env.events().publish(
            (
                symbol_short!("init"),
                meta.client.clone(),
                meta.freelancer.clone(),
                meta.arbiter.clone(),
            ),
            InitializedEvent {
                admin,
                client: meta.client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                auto_release_seconds: meta.auto_release_seconds,
                milestone_amounts,
                milestone_spec_hashes,
                total_amount: meta.total_amount,
                milestone_count: meta.milestone_count,
                created_at: meta.created_at,
            },
        );

        Ok(())
    }

    /// Test-only: `initialize` with an explicit funding mode.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_with_funding_mode(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        funding_mode: FundingMode,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            EscrowSettings {
                funding_mode,
                ..Self::default_settings()
            },
        )
    }

    /// Test-only: `initialize` with an arbiter fee (issue #738).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_with_arbiter_fee(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        arbiter_fee_bps: u32,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            EscrowSettings {
                arbiter_fee_bps,
                ..Self::default_settings()
            },
        )
    }

    /// The arbiter fee in basis points (issue #738); 0 when none was set.
    pub fn get_arbiter_fee_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ArbiterFeeBps)
            .unwrap_or(0)
    }

    /// Arbiter fee owed on a disputed `remaining` balance:
    /// `remaining * arbiter_fee_bps / 10_000`, rounded down and capped to
    /// `available` (issue #738).
    fn arbiter_fee_for(env: &Env, remaining: i128, available: i128) -> Result<i128, Error> {
        let fee_bps = Self::get_arbiter_fee_bps(env.clone());
        if fee_bps == 0 {
            return Ok(0);
        }
        let fee = remaining
            .checked_mul(fee_bps as i128)
            .ok_or(Error::ArithmeticOverflow)?
            / BPS_SCALE as i128;
        Ok(fee.min(available))
    }

    /// Transfer admin control of the contract to a new address.
    ///
    /// The new admin immediately gains access to all admin-only endpoints
    /// (whitelist management, pause/resume, emergency overrides, etc.); the
    /// previous admin loses that access.
    ///
    /// # Checks (in order)
    /// 1. `current_admin.require_auth()` — SDK-level signature check.
    /// 2. Contract must be initialised (`NotInitialized`).
    /// 3. `require_admin` — verified admin key matches `DataKey::Admin`.
    /// 4. No multisig approval regime may exist (`Unauthorized`) — issue #786:
    ///    once `multisig_approval_init` has registered signers, a
    ///    single-signature transfer would bypass them; the admin must use
    ///    `propose_admin_transfer` → approvals → `execute_admin_transfer`.
    /// 5. `new_admin` must not be a zero address, the contract itself, or the
    ///    current admin (`InvalidAddress`).
    /// 6. No multisig admin-transfer proposal may already be pending
    ///    (`AdminTransferPending`).
    ///
    /// # Parameters
    /// * `current_admin` – Must match the currently stored admin. Must
    ///                     authorize the call.
    /// * `new_admin`     – Address to become the new admin.
    ///
    /// # Errors
    /// * `NotInitialized`       – Contract has not been initialized.
    /// * `Unauthorized`         – `current_admin` is not the stored admin, or a
    ///                            multisig regime is configured (issue #786;
    ///                            use the multisig proposal flow instead).
    /// * `InvalidAddress`       – `new_admin` is a zero address, the contract
    ///                            itself, or the current admin.
    /// * `AdminTransferPending` – A multisig admin-transfer proposal is already
    ///                            pending; execute or cancel it first.
    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Auth + init guards first — a single require_auth so the host does not
        // abort on a duplicated auth requirement.
        current_admin.require_auth();
        // Footprint: a single read of DataKey::Admin serves both the
        // initialization guard and the authorization check. Previously this
        // touched the entry twice — a separate presence probe followed by
        // `load_admin()` (which also reads it). `load_admin` already returns
        // `NotInitialized` when the key is absent, so dropping the redundant
        // `has` keeps behavior identical (absent → NotInitialized, mismatch →
        // Unauthorized) while reading the entry only once.
        let stored_admin = Self::load_admin(&env)?;
        if stored_admin != current_admin {
            return Err(Error::Unauthorized);
        }

        // Issue #786: after `multisig_approval_init` the legacy single-signature
        // transfer path is closed.  It would let one key swap the admin without
        // any signer approval, silently bypassing the regime the signers were
        // registered for; every admin change on a multisig-configured contract
        // must go through propose → approve → execute, which is the flow the
        // signers audit.  `load_multisig_config` reads only, covering both the
        // consolidated entry and a pre-#456 legacy two-key layout.
        if Self::load_multisig_config(&env).is_some() {
            return Err(Error::Unauthorized);
        }

        Self::validate_new_admin(&env, &stored_admin, &new_admin)?;

        if env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
        {
            return Err(Error::AdminTransferPending);
        }

        env.storage().persistent().set(&DataKey::Admin, &new_admin);
        // Keep the instance copy in sync: `require_admin_from_instance`
        // authorizes against it, so a stale value would leave the old admin
        // in control of those endpoints.
        env.storage().instance().set(&DataKey::Admin, &new_admin);

        env.events().publish(
            (symbol_short!("admin"),),
            TransferAdminEvent {
                old_admin: current_admin,
                new_admin,
            },
        );

        Ok(())
    }

    /// Add a token contract address to the escrow's settlement-token whitelist.
    ///
    /// The whitelist controls which tokens are acceptable as settlement
    /// currencies. It is checked at funding time and any token in it may be
    /// used as the job's settlement token. Only the admin may modify the
    /// whitelist, and only before the escrow is funded.
    ///
    /// # Parameters
    /// * `admin` – Must match the stored admin address. Must authorize the
    ///             call (`admin.require_auth()` is called internally).
    /// * `token` – Token contract address to add to the whitelist. Must not
    ///             be a sentinel zero address or the escrow contract itself.
    ///
    /// # Returns
    /// `Ok(())` on success. At that point:
    /// * `token` has been appended to `DataKey::WhitelistedTokens` in
    ///   instance storage.
    /// * A `"wtok"` event carrying a [`TokenWhitelistedEvent`] has been
    ///   published with the acting `admin` and the newly whitelisted `token`.
    ///
    /// # Errors
    /// Errors are returned in the order the checks appear in the function body.
    ///
    /// * [`Error::NotInitialized`] – Returned from any of three call sites
    ///   inside the function:
    ///   1. `require_admin` fails to load `DataKey::Admin` from persistent
    ///      storage (the contract was never initialized).
    ///   2. `load_job_meta` fails to load `DataKey::JobMeta` from instance
    ///      storage (initialize was never called or storage was cleared).
    ///   3. The whitelist load fails to find `DataKey::WhitelistedTokens` in
    ///      instance storage (same root cause as case 2).
    ///   In practice, all three keys are written atomically by `initialize`,
    ///   so only case 1 is reachable after a fully successful initialization.
    ///
    /// * [`Error::Unauthorized`] – `admin` does not match the stored admin
    ///   address, or `admin.require_auth()` was not satisfied by the
    ///   transaction's authorization envelope.
    ///
    /// * [`Error::InvalidAddress`] – `token` is one of the sentinel invalid
    ///   addresses (checked immediately after the admin check, before any
    ///   storage reads for job metadata or the whitelist):
    ///   - The Stellar zero account
    ///     (`GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF`),
    ///   - The canonical Soroban zero contract
    ///     (`CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4`),
    ///   - The escrow contract's own address
    ///     (`env.current_contract_address()`).
    ///
    /// * [`Error::AlreadyFunded`] – The escrow has already been funded
    ///   (`JobMeta::funded` is `true`). Token whitelist changes are locked
    ///   once the client has deposited funds to prevent post-funding token
    ///   substitution attacks.
    ///
    /// * [`Error::TokenAlreadyWhitelisted`] – `token` is already present in
    ///   the whitelist. This check runs before the capacity check, so a
    ///   duplicate token is always reported as `TokenAlreadyWhitelisted` even
    ///   when the whitelist is at capacity.
    ///
    /// * [`Error::WhitelistFull`] – The whitelist already contains
    ///   `MAX_WHITELIST_SIZE` (50) entries. Adding another would exceed the
    ///   capacity cap. This guards against unbounded `Vec` growth and `u32`
    ///   length-counter overflow.
    pub fn add_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if token == zero_account || token == zero_contract {
            return Err(Error::InvalidAddress);
        }
        if token == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

        let mut whitelist: Vec<Address> =
            Self::cold_get(&env, &DataKey::WhitelistedTokens).ok_or(Error::NotInitialized)?;

        // Duplicate check runs before the capacity check so that a
        // full whitelist still reports TokenAlreadyWhitelisted (rather
        // than InvalidAmount) for a token that's already present.
        if whitelist.contains(&token) {
            return Err(Error::TokenAlreadyWhitelisted);
        }

        if whitelist.len() >= MAX_WHITELIST_SIZE {
            return Err(Error::WhitelistFull);
        }

        whitelist.push_back(token.clone());
        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        env.events().publish(
            (symbol_short!("wtok"),),
            TokenWhitelistedEvent { admin, token },
        );

        Ok(())
    }

    /// Removes a token from the escrow's whitelist.
    ///
    /// # Returns
    /// * `Ok(())`            - The token was successfully removed.
    ///
    /// # Errors
    /// * `NotInitialized`      - The contract has not been initialized (missing admin, job, or whitelisted tokens data).
    /// * `Unauthorized`        - The provided `admin` address does not match the stored admin address.
    /// * `AlreadyFunded`       - The job has already been funded, preventing further whitelist modifications.
    /// * `InvalidAddress`      - The provided `token` address is the zero account or the zero contract.
    /// * `TokenNotWhitelisted` - The whitelist is empty, or the provided `token` is not present in the whitelist.
    /// * `WhitelistMinimum`    - Removing the token would leave the whitelist empty (the contract requires at least one token to remain).
    pub fn remove_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

        // Kept (unlike a bare lookup miss) so the zero sentinels still report
        // `InvalidAddress` rather than `TokenNotWhitelisted`.
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if token == zero_account || token == zero_contract {
            return Err(Error::InvalidAddress);
        }

        let mut whitelist: Vec<Address> =
            Self::cold_get(&env, &DataKey::WhitelistedTokens).ok_or(Error::NotInitialized)?;

        let len = whitelist.len();
        if len == 0 {
            return Err(Error::TokenNotWhitelisted);
        }
        if len == 1 {
            return Err(Error::WhitelistMinimum);
        }

        let index = whitelist
            .first_index_of(&token)
            .ok_or(Error::TokenNotWhitelisted)?;

        // Swap-remove: move the last element into the vacated slot.
        let last = len - 1;
        if index != last {
            let last_elem = whitelist.get(last).ok_or(Error::TokenNotWhitelisted)?;
            whitelist.set(index, last_elem);
        }
        whitelist.pop_back();
        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        env.events().publish(
            (symbol_short!("wldel"),),
            TokenRemovedEvent { admin, token },
        );

        Ok(())
    }

    /// Returns whether `token` is present in the stored whitelist.
    ///
    /// Returns `false` if the whitelist has not been stored, is empty, or does
    /// not contain `token`. Returns `true` when it does. A full whitelist is
    /// checked the same way; its size does not otherwise affect the result.
    /// This read-only query requires no authorization.
    pub fn is_token_whitelisted(env: Env, token: Address) -> bool {
        Self::read_whitelist(&env).is_some_and(|whitelist| whitelist.contains(&token))
    }

    /// Return the list of whitelisted token addresses.
    ///
    /// Strictly read-only: performs a single `get` on instance storage and no
    /// writes to instance, persistent, or temporary storage. Keep it that way —
    /// the storage handle is only ever used through `Self::read_whitelist`.
    /// No `has` probe precedes the `get`: the `Option` it returns already
    /// distinguishes "never written" from "written", so the key is resolved
    /// exactly once per invocation (#484).
    ///
    /// # Errors
    /// * `NotInitialized` – The whitelist has never been written (the contract
    ///   has not been initialized). Returns a typed error rather than
    ///   panicking or defaulting to an empty vector.
    pub fn get_whitelisted_tokens(env: Env) -> Result<Vec<Address>, Error> {
        Self::read_whitelist(&env).ok_or(Error::NotInitialized)
    }

    /// Read-only accessor for `DataKey::WhitelistedTokens`.
    fn read_whitelist(env: &Env) -> Option<Vec<Address>> {
        Self::cold_get(env, &DataKey::WhitelistedTokens)
    }

    /// Deposit the full escrow amount into the contract.
    ///
    /// Transfers the sum of all milestone amounts from the client to the
    /// contract in a single token transfer. The `funded` flag is set before
    /// the transfer is executed to prevent reentrant double-funding. Must be
    /// called once, after `initialize` and before any milestone can be
    /// delivered or approved.
    ///
    /// # Parameters
    /// * `client` – Must match the job's stored client. Must authorize the
    ///              call.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `AlreadyFunded`    – The job has already been funded.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `InvalidAddress`   – `client` is a zero address.
    /// * `InvalidAmount`    – The total milestone amount is invalid (e.g.
    ///                        overflow or non-positive).
    pub fn fund(env: Env, client: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        Self::validate_fund_client(&env, &client)?;
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if Self::load_funding_mode(&env) != FundingMode::Upfront {
            return Err(Error::InvalidFundingMode);
        }
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let total_amount = Self::validate_fund_amount(&meta)?;

        // Update status BEFORE token transfer to ensure state is persisted
        // and prevent double-funding via reentrancy
        meta.funded = true;
        Self::store_job_meta(&env, &meta);
        env.storage()
            .instance()
            .set(&DataKey::FundedAt, &env.ledger().timestamp());

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, env.current_contract_address(), &total_amount);

        env.events().publish(
            (
                symbol_short!("fund"),
                meta.client.clone(),
                meta.freelancer.clone(),
                meta.arbiter.clone(),
            ),
            FundedEvent {
                client,
                total_amount,
            },
        );

        Ok(())
    }

    /// Fund a single milestone when the escrow uses `FundingMode::PerMilestone`.
    pub fn fund_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;

        Self::validate_fund_client(&env, &client)?;
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if Self::load_funding_mode(&env) != FundingMode::PerMilestone {
            return Err(Error::InvalidFundingMode);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.funded {
            return Err(Error::AlreadyFunded);
        }
        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        let before = token_client.balance(&env.current_contract_address());
        token_client.transfer(&client, env.current_contract_address(), &milestone.amount);
        let after = token_client.balance(&env.current_contract_address());
        let received = after.checked_sub(before).unwrap_or(0);
        if received != milestone.amount {
            return Err(Error::InsufficientBalance);
        }

        milestone.funded = true;
        Self::store_milestone(&env, milestone_index, &milestone);
        // The first funded milestone starts the acceptance window (#757).
        if !env.storage().instance().has(&DataKey::FundedAt) {
            env.storage()
                .instance()
                .set(&DataKey::FundedAt, &env.ledger().timestamp());
        }

        let mut all_funded = true;
        for index in 0..meta.milestone_count {
            if !Self::load_milestone(&env, index)?.funded {
                all_funded = false;
                break;
            }
        }
        if all_funded {
            meta.funded = true;
            Self::store_job_meta(&env, &meta);
        }

        env.events().publish(
            (symbol_short!("msfund"),),
            MilestoneFundedEvent {
                client,
                milestone_index,
                amount: milestone.amount,
                token: meta.token,
            },
        );

        Ok(())
    }

    /// Current funding mode. Missing storage defaults to `Upfront`.
    pub fn get_funding_mode(env: Env) -> Result<FundingMode, Error> {
        let _ = Self::load_job_meta(&env)?;
        Ok(Self::load_funding_mode(&env))
    }

    /// Append a new `Pending` milestone to a funded escrow (issue #737).
    ///
    /// Real freelance work changes scope after funding; without this endpoint
    /// the only ways to add work are cancelling the whole escrow or leaving
    /// the new deliverable unfunded.  Both parties must sign, and the client
    /// deposits `amount` into escrow in the same call, so the conservation
    /// invariant `total_amount == Î£ milestone.amounts` holds across the
    /// amendment: `total_amount` grows by exactly `amount` and the escrow
    /// token balance grows by exactly `amount`.
    ///
    /// # Authorization
    /// `client` and `freelancer` must both match the addresses stored on the
    /// job and both authorise the call — neither party can extend scope alone.
    ///
    /// # Parameters
    /// * `client`     – The job's client; pays the new milestone.
    /// * `freelancer` – The job's freelancer; consents to the extra scope.
    /// * `amount`     – New milestone amount, must be > 0.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked`     – Emergency pause or cancel lock is on.
    /// * `NotInitialized`              – Contract has not been initialized.
    /// * `Unauthorized`                – `client` / `freelancer` do not match
    ///                                    the stored job parties.
    /// * `NotFunded`                   – The escrow has not been funded yet;
    ///                                    extra milestones belong in the
    ///                                    `initialize` amount vector.
    /// * `InvalidAmount`               – `amount` ≤ 0, or `total_amount +
    ///                                    amount` overflows `i128`.
    /// * `TooManyMilestones`           – The job already carries
    ///                                    `MAX_MILESTONES` milestones.
    pub fn add_milestone(
        env: Env,
        client: Address,
        freelancer: Address,
        amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;

        let mut meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if meta.milestone_count >= MAX_MILESTONES {
            return Err(Error::TooManyMilestones);
        }
        let new_total = meta
            .total_amount
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;
        let new_index = meta.milestone_count;

        Self::store_milestone(
            &env,
            new_index,
            &Milestone {
                amount,
                released_amount: 0,
                status: MilestoneStatus::Pending,
                delivered_at: 0,
                funded: true,
                spec_hash: BytesN::from_array(&env, &[0u8; 32]),
                deliverable: BytesN::from_array(&env, &[0u8; 32]),
            },
        );
        // Persist amended state BEFORE the token transfer so a reentrant call
        // sees the updated count/total (mirrors `fund`).
        meta.milestone_count = new_index + 1;
        meta.total_amount = new_total;
        Self::store_job_meta(&env, &meta);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, env.current_contract_address(), &amount);

        env.events().publish(
            (symbol_short!("ms_add"),),
            MilestoneAddedEvent {
                client,
                freelancer,
                milestone_index: new_index,
                amount,
                total_amount: new_total,
                milestone_count: meta.milestone_count,
            },
        );

        Ok(())
    }

    /// Reduce the amount of a `Pending` milestone and refund the difference to
    /// the client (issue #737).
    ///
    /// Both parties must sign.  Only `Pending` milestones can be reduced: a
    /// `Delivered` or partially-released milestone has work behind it and its
    /// remaining balance is already committed to a release/dispute path.  The
    /// refund is exact, so the conservation invariant
    /// `total_amount == Î£ milestone.amounts` holds across the amendment.
    ///
    /// # Parameters
    /// * `client`          – The job's client; receives the refund.
    /// * `freelancer`      – The job's freelancer; consents to reduced scope.
    /// * `milestone_index` – Index of the milestone to reduce.
    /// * `new_amount`      – Replacement amount.  Must be > 0 and strictly
    ///                        below the milestone's current amount.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked`     – Emergency pause or cancel lock is on.
    /// * `NotInitialized`              – Contract has not been initialized.
    /// * `Unauthorized`                – `client` / `freelancer` do not match
    ///                                    the stored job parties.
    /// * `NotFunded`                   – Nothing was ever deposited to refund.
    /// * `InvalidMilestone`            – `milestone_index` is out of range.
    /// * `InvalidStatus`               – The milestone is not `Pending`.
    /// * `InvalidAmount`               – `new_amount` ≤ 0, or it is not
    ///                                    strictly below the current amount.
    /// * `InsufficientBalance`         – The escrow holds less than the refund
    ///                                    (defensive; unreachable while the
    ///                                    conservation invariant holds).
    pub fn reduce_milestone(
        env: Env,
        client: Address,
        freelancer: Address,
        milestone_index: u32,
        new_amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;

        let mut meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }
        if new_amount <= 0 || new_amount >= milestone.amount {
            return Err(Error::InvalidAmount);
        }
        let refund = milestone.amount - new_amount;

        // Credits owed to parties (`TotalOwed`) are not available to refund.
        let token_client = token::Client::new(&env, &meta.token);
        if Self::available_balance(&env, &token_client) < refund {
            return Err(Error::InsufficientBalance);
        }

        milestone.amount = new_amount;
        Self::store_milestone(&env, milestone_index, &milestone);
        // Persist amended state BEFORE the refund transfer so a reentrant
        // call never observes the old, larger amount (mirrors `fund`).
        meta.total_amount -= refund;
        Self::store_job_meta(&env, &meta);

        Self::pay_or_credit(&env, &token_client, &client, &refund)?;

        env.events().publish(
            (symbol_short!("ms_reduce"),),
            MilestoneReducedEvent {
                client,
                freelancer,
                milestone_index,
                old_amount: new_amount + refund,
                new_amount,
                refund,
                total_amount: meta.total_amount,
                milestone_count: meta.milestone_count,
            },
        );

        Ok(())
    }

    /// Accept the job as its freelancer, agreeing on-chain to the milestone
    /// amounts and auto-release window set at `initialize`.
    ///
    /// `mark_delivered` and the dispute endpoints require both this and
    /// `accept_arbiter`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `freelancer` does not match the job's freelancer.
    /// * `InvalidStatus`  – The freelancer has already accepted.
    pub fn accept_job(env: Env, freelancer: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        freelancer.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }
        Self::record_acceptance(&env, freelancer, 1, symbol_short!("acceptjob"))
    }

    /// Accept the job as its arbiter, agreeing to resolve its disputes.
    ///
    /// `mark_delivered` and the dispute endpoints require both this and
    /// `accept_job`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `arbiter` does not match the job's arbiter.
    /// * `InvalidStatus`  – The arbiter has already accepted.
    pub fn accept_arbiter(env: Env, arbiter: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        arbiter.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }
        Self::record_acceptance(&env, arbiter, 2, symbol_short!("acceptarb"))
    }

    /// Appoint a new arbiter, jointly authorized by the client and the
    /// freelancer (issue #731).
    ///
    /// Lets the two contracting parties swap an arbiter who has become
    /// unavailable (unreachable, unresponsive, or unwilling to act) without
    /// waiting on the arbiter's own signature or an admin override.  Both the
    /// client's and the freelancer's `require_auth()` are collected up front
    /// by `require_client_and_freelancer_consent`, mirroring
    /// `set_interest_yield_consent` and `payment_streaming_consent`: a
    /// transaction missing either signature panics at the host level before
    /// any storage is mutated, so neither party can ever unilaterally replace
    /// the arbiter.
    ///
    /// Distinct-parties validation mirrors `initialize` (issue #667): the new
    /// arbiter must differ from the stored client, the stored freelancer, and
    /// the currently recorded arbiter.  A replacement is also refused while
    /// any milestone is still `Disputed` — the sitting arbiter is the party
    /// authorised to conclude an open dispute (`resolve_dispute` and
    /// `apply_dispute_arbitration_split` both verify the stored
    /// `meta.arbiter`), so the swap may only happen once every dispute has
    /// been resolved, split, or timed out and escalated.  There is no
    /// escape hatch past this guard: this is the deliberate design, because a
    /// silent mid-dispute arbiter swap would let the parties steer the
    /// outcome.  Once the swap succeeds it is recorded in `JobMeta::arbiter`,
    /// so the very next dispute raise, resolution, or arbitration split is
    /// checked and settled against the new arbiter automatically — no
    /// additional plumbing is needed.  On success the `arbchg` event reports
    /// the `old` and `new` arbiter addresses for indexers.
    ///
    /// # Parameters
    /// * `new_arbiter` – The incoming arbiter. Must be a valid address
    ///                   distinct from every recorded party.
    ///
    /// # Errors
    /// * `MigrationRequired`– Storage predates this code; call `migrate`.
    /// * `Paused`           – The contract is emergency-paused or
    ///                        cancel-locked.
    /// * `InvalidAddress`   – `new_arbiter` is a zero address, the canonical
    ///                        Soroban zero contract address, or the escrow's
    ///                        own address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `InvalidParties`   – `new_arbiter` equals the stored client, the
    ///                        stored freelancer, or the current arbiter.
    /// * `DisputePending`   – At least one milestone is currently `Disputed`.
    pub fn replace_arbiter(env: Env, new_arbiter: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;

        // ── Input validation: reject the zero sentinels and the escrow's own
        // address before any authorization or ledger access. ────────────────
        Self::validate_address(&env, &new_arbiter)?;

        // ── Joint authorization: both the client's and the freelancer's
        // `require_auth()` fire here, before any further check or write. ──
        let mut meta = Self::require_client_and_freelancer_consent(&env)?;

        // ── Distinct-parties validation (mirrors `initialize`, #667): the
        // arbiter must stay independent of the client, the freelancer, and
        // the arbiter being replaced. Re-proposing the sitting arbiter is a
        // no-op that would still cost a full two-signature round and emit a
        // misleading replacement event, so it is refused like a collision. ──
        if new_arbiter == meta.client
            || new_arbiter == meta.freelancer
            || new_arbiter == meta.arbiter
        {
            return Err(Error::InvalidParties);
        }

        // ── Dispute-active guard: an arbiter cannot be swapped out from
        // under an open dispute. Scanned before any write so a refused call
        // leaves no ledger trace. ─────────────────────────────────────────
        for index in 0..meta.milestone_count {
            let milestone = Self::load_milestone(&env, index)?;
            if milestone.status == MilestoneStatus::Disputed {
                return Err(Error::DisputePending);
            }
        }

        let old_arbiter = meta.arbiter.clone();
        meta.arbiter = new_arbiter.clone();
        Self::store_job_meta(&env, &meta);

        env.events().publish(
            (symbol_short!("arbchg"),),
            ArbiterReplacedEvent {
                client: meta.client.clone(),
                freelancer: meta.freelancer.clone(),
                old: old_arbiter,
                new: new_arbiter,
            },
        );

        Ok(())
    }

    /// Rotate the freelancer identity and payout destination (issue #732).
    ///
    /// Both the current freelancer and the client must authorize, so someone
    /// holding only the freelancer's key cannot redirect payouts. The new
    /// address becomes the identity checked by freelancer-only methods and
    /// the recipient of every later payout. Credits already owed to the old
    /// address (`Owed`) stay withdrawable by it.
    ///
    /// # Errors
    /// * `NotInitialized` – Job metadata has not been initialized.
    /// * `InvalidAddress` – `new_freelancer` is a zero address, this contract,
    ///   or the current freelancer.
    /// * `InvalidParties` – `new_freelancer` collides with the client, the
    ///   arbiter or the admin.
    pub fn rotate_freelancer(env: Env, new_freelancer: Address) -> Result<(), Error> {
        let mut meta = Self::require_client_and_freelancer_consent(&env)?;
        Self::validate_address(&env, &new_freelancer)?;
        if new_freelancer == meta.freelancer || new_freelancer == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        let admin = Self::load_admin(&env)?;
        if new_freelancer == meta.client
            || new_freelancer == meta.arbiter
            || new_freelancer == admin
        {
            return Err(Error::InvalidParties);
        }

        let previous_freelancer = meta.freelancer.clone();
        meta.freelancer = new_freelancer.clone();
        Self::store_job_meta(&env, &meta);

        env.events().publish(
            (symbol_short!("freerot"), new_freelancer.clone()),
            FreelancerRotatedEvent {
                client: meta.client,
                previous_freelancer,
                new_freelancer,
            },
        );

        Ok(())
    }

    /// Refund the client in full when the job was not accepted by both the
    /// freelancer and the arbiter within `ACCEPTANCE_WINDOW_SECONDS` of
    /// funding.
    ///
    /// Every milestone that is not already `Released` or `Refunded` moves to
    /// `Refunded` and its remaining amount is paid to the client.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked` – The contract is emergency-paused or
    ///                               cancel-locked.
    /// * `NotInitialized`          – Contract has not been initialized.
    /// * `Unauthorized`            – `client` does not match the job's client.
    /// * `NotFunded`               – The escrow has not been funded yet.
    /// * `InvalidStatus`           – Both parties have accepted the job.
    /// * `DeadlineNotPassed`       – The acceptance window is still open.
    /// * `InvalidAmount`           – Nothing remains to refund.
    pub fn withdraw_unaccepted(env: Env, client: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        // Upfront: the whole job; PerMilestone: at least one funded milestone.
        Self::require_escrow_has_funding(&env, &meta)?;
        if Self::load_acceptance(&env) == 3 {
            return Err(Error::InvalidStatus);
        }

        let funded_at: u64 = env
            .storage()
            .instance()
            .get(&DataKey::FundedAt)
            .unwrap_or(meta.created_at);
        let deadline = funded_at
            .checked_add(ACCEPTANCE_WINDOW_SECONDS)
            .ok_or(Error::InvalidAmount)?;
        if env.ledger().timestamp() < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        let mut total_refunded: i128 = 0;
        for index in 0..meta.milestone_count {
            let mut milestone = Self::load_milestone(&env, index)?;
            // Only money that was deposited can be refunded: skip terminal
            // milestones and, under PerMilestone funding, unfunded ones.
            if Self::is_terminal_status(&milestone.status)
                || !Self::milestone_is_funded(&meta, &milestone)
            {
                continue;
            }
            let remaining = milestone
                .amount
                .checked_sub(milestone.released_amount)
                .ok_or(Error::InvalidAmount)?;
            if remaining > 0 {
                total_refunded = total_refunded
                    .checked_add(remaining)
                    .ok_or(Error::InvalidAmount)?;
                milestone.status = MilestoneStatus::Refunded;
                Self::store_milestone(&env, index, &milestone);
            }
        }

        if total_refunded <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.client, &total_refunded)?;

        env.events().publish(
            (symbol_short!("unaccref"),),
            UnacceptedWithdrawnEvent {
                client: meta.client,
                token: meta.token,
                amount: total_refunded,
            },
        );

        Ok(())
    }

    /// Mark a milestone as delivered by the freelancer.
    ///
    /// Moves the milestone from `Pending` to `Delivered` and records the
    /// ledger timestamp of delivery, which starts the clock for
    /// `extend_milestone_deadline` and `claim_auto_release`.
    ///
    /// # Parameters
    /// * `freelancer`      – Must match the job's stored freelancer. Must
    ///                       authorize the call.
    /// * `milestone_index` – Index of the milestone being delivered.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `freelancer` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `freelancer` does not match the job's
    ///                        freelancer.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidAmount`    – The milestone's amount is not positive.
    /// * `InvalidStatus`    – The milestone is not currently `Pending`.
    /// * `NotAccepted`      – The freelancer or arbiter has not accepted the
    ///                        job.
    ///
    /// Legacy path kept for migration: records an all-zero `deliverable`
    /// hash. Prefer `mark_delivered_with_hash`.
    pub fn mark_delivered(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        let zero_hash = BytesN::from_array(&env, &[0u8; 32]);
        Self::mark_delivered_with_hash(env, freelancer, milestone_index, zero_hash)
    }

    /// Mark a milestone as delivered and record a hash of the deliverable
    /// (e.g. the SHA-256 of the artifact or an IPFS CID digest) so an arbiter
    /// can tie the delivery claim to a concrete artifact.
    ///
    /// Same parameters, behaviour and errors as `mark_delivered`, plus:
    /// * `deliverable` – 32-byte hash of the delivered artifact. Stored on the
    ///                   milestone and emitted in `DeliveredEvent`.
    pub fn mark_delivered_with_hash(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
        deliverable: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        // Check for zero addresses (both account and contract types)
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if freelancer == zero_account || freelancer == zero_contract {
            return Err(Error::InvalidAddress);
        }
        freelancer.require_auth();

        let meta = Self::load_job_meta(&env)?;

        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }
        Self::require_accepted(&env)?;

        let delivered_at = env.ledger().timestamp();
        milestone.status = MilestoneStatus::Delivered;
        milestone.delivered_at = delivered_at;
        milestone.deliverable = deliverable.clone();
        Self::store_milestone(&env, milestone_index, &milestone);
        // Write the delivery timestamp to temporary storage so that
        // claim_auto_release and time_until_auto_release can read it from the
        // optimised temporary tier without touching the persistent Milestone entry.
        Self::store_delivered_at(&env, milestone_index, delivered_at);

        env.events().publish(
            (symbol_short!("deliver"), meta.freelancer.clone()),
            DeliveredEvent {
                milestone_index,
                freelancer: meta.freelancer,
                client: meta.client,
                delivered_at,
                status: MilestoneStatus::Delivered,
                amount: milestone.amount,
                deliverable,
            },
        );

        Ok(())
    }

    /// Ask the freelancer to revise a delivered milestone.
    ///
    /// Returns the milestone to `Pending`, clears `delivered_at` so the previous
    /// auto-release clock cannot fire, and increments `revision_count` up to
    /// `MAX_REVISIONS` (3). Accounting and funding are unchanged.
    pub fn request_revision(
        env: Env,
        client: Address,
        milestone_index: u32,
        note_hash: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        let current: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::RevCnt(milestone_index))
            .unwrap_or(0);
        if current >= MAX_REVISIONS {
            return Err(Error::MaxRevisions);
        }
        let revision_count = current + 1;
        env.storage()
            .persistent()
            .set(&DataKey::RevCnt(milestone_index), &revision_count);

        milestone.status = MilestoneStatus::Pending;
        milestone.delivered_at = 0;
        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .temporary()
            .remove(&DataKey::DeliveredAt(milestone_index));
        env.storage()
            .temporary()
            .remove(&DataKey::TimeExt(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

        env.events().publish(
            (symbol_short!("msrev"),),
            RevisionRequestedEvent {
                milestone_index,
                client,
                note_hash,
                revision_count,
            },
        );
        Ok(())
    }

    pub fn get_revision_count(env: Env, milestone_index: u32) -> Result<u32, Error> {
        let meta = Self::load_job_meta(&env)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        Ok(env
            .storage()
            .persistent()
            .get(&DataKey::RevCnt(milestone_index))
            .unwrap_or(0))
    }

    /// Extends the auto-release deadline for a Delivered milestone.
    pub fn extend_milestone_deadline(
        env: Env,
        client: Address,
        milestone_index: u32,
        extra_seconds: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::assert_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;
        // A deadline extension changes the freelancer's guaranteed release time;
        // require their explicit consent instead of allowing unilateral delay.
        meta.freelancer.require_auth();
        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        if extra_seconds == 0 {
            return Err(Error::InvalidExtension);
        }

        let current_extension = Self::load_time_extension(&env, milestone_index);
        let new_extension = current_extension
            .checked_add(extra_seconds)
            .ok_or(Error::InvalidExtension)?;
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let new_deadline = Self::auto_release_deadline(&meta, delivered_at, new_extension)?;
        env.storage()
            .persistent()
            .set(&DataKey::TimeExt(milestone_index), &new_extension);

        env.events().publish(
            (symbol_short!("extend"),),
            DeadlineExtendedEvent {
                milestone_index,
                client,
                extra_seconds,
                new_extension,
                new_deadline,
            },
        );

        Ok(())
    }

    /// Time-locked auto-release of a single milestone to the freelancer.
    ///
    /// # Gas complexity: O(1)
    ///
    /// This function performs a bounded, constant number of storage reads and
    /// writes regardless of the total milestone count:
    ///
    /// - 1× instance read  (`DataKey::Job` → `JobMeta`)
    /// - 1× temporary read (`DataKey::DeliveredAt(milestone_index)`)
    /// - 1× persistent read  (`DataKey::Milestone(milestone_index)`)
    /// - 1× persistent write (`DataKey::Milestone(milestone_index)`)
    /// - 1× token transfer
    ///
    /// No loop over all milestones is performed here.  Functions that do loop
    /// over all milestones (`assemble_job`) are
    /// intentionally not called from this hot path.
    ///
    /// The payout always goes to the job's freelancer, so the signature only
    /// attributes the claim; `trigger_auto_release` performs the same release
    /// without it.
    pub fn claim_auto_release(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if freelancer == zero_account || freelancer == zero_contract {
            return Err(Error::InvalidAddress);
        }
        freelancer.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }

        Self::release_overdue_milestone(&env, &meta, freelancer, milestone_index)
    }

    /// Permissionless auto-release of an overdue milestone (#740).
    ///
    /// Anyone (a keeper bot, a scheduled backend job, the freelancer, the
    /// client) may call this once a milestone's auto-release deadline has
    /// passed. No signature is required because the call cannot redirect
    /// funds: the remaining balance is always transferred to the job's stored
    /// freelancer, exactly as in `claim_auto_release`, and every other check
    /// (pause and execution guards, index, `Delivered` status, deadline,
    /// remaining > 0) is identical.
    ///
    /// # Parameters
    /// * `caller`          – Recorded in the `claim` event for attribution.
    ///                       It is **not** authenticated, so indexers must
    ///                       not treat it as proof of who submitted the
    ///                       transaction.
    /// * `milestone_index` – Index of the milestone to release.
    ///
    /// # Errors
    /// * `Paused`            – The contract is emergency-paused.
    /// * `NotInitialized`    – Contract has not been initialized.
    /// * `InvalidMilestone`  – `milestone_index` is out of range.
    /// * `InvalidStatus`     – The milestone is not currently `Delivered`.
    /// * `DeadlineNotPassed` – The auto-release deadline has not passed yet.
    /// * `InvalidAmount`     – The milestone's remaining balance is not
    ///                         positive.
    pub fn trigger_auto_release(
        env: Env,
        caller: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        let meta = Self::load_job_meta(&env)?;

        Self::release_overdue_milestone(&env, &meta, caller, milestone_index)
    }

    /// Shared body of `claim_auto_release` and `trigger_auto_release`: every
    /// milestone-level check, the state commit, and the payout to
    /// `meta.freelancer`. Callers run the guards and any caller checks first.
    fn release_overdue_milestone(
        env: &Env,
        meta: &JobMeta,
        caller: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        // CHECK 1: Validate index boundary.
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(env, milestone_index)?;
        Self::require_milestone_is_funded(env, meta, &milestone)?;

        // CHECK 2: Milestone must be in the Delivered state.  Any other status —
        // including Released (double-claim), Disputed, Refunded, Pending, or
        // PartiallyReleased — is rejected here, making the guard the sole
        // gatekeeper against double-execution and out-of-sequence calls.
        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        // CHECK 3: Read the delivery timestamp from temporary storage first
        //    (optimised ledger-footprint path).  Fall back to the value stored on
        //    the persistent Milestone entry so that entries written before this
        //    migration remain fully functional.
        let delivered_at =
            Self::load_delivered_at(env, milestone_index).unwrap_or(milestone.delivered_at);
        let extension = Self::load_time_extension(env, milestone_index);

        let deadline = Self::auto_release_deadline(meta, delivered_at, extension)?;
        let current = env.ledger().timestamp();
        if current < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        // CHECK 4: Compute remaining using checked subtraction so that corrupted
        //    or adversarially-crafted storage values (released_amount > amount)
        //    never produce a silent underflow.
        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        // EFFECT: Commit the terminal state to persistent storage BEFORE any
        //    external call (Checks-Effects-Interactions pattern).  Setting the
        //    status to Released here means a re-entrant or duplicate invocation
        //    will hit the `InvalidStatus` guard above on its next CHECK 2 and
        //    be rejected before it can touch the token contract.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(env, milestone_index, &milestone);
        Self::clear_time_ext(env, milestone_index);
        Self::increment_reputation(env, &meta.client);
        Self::increment_reputation(env, &meta.freelancer);

        // INTERACTION: Token transfer is the sole external call and executes only
        //    after all state mutations have been durably persisted.
        let token_client = token::Client::new(env, &meta.token);
        Self::pay_or_credit(env, &token_client, &meta.freelancer, &remaining)?;

        env.events().publish(
            (symbol_short!("claim"), meta.freelancer.clone()),
            ClaimedEvent {
                milestone_index,
                caller,
                freelancer: meta.freelancer.clone(),
                token: meta.token.clone(),
                amount: remaining,
            },
        );

        Ok(())
    }

    /// Compute the seconds remaining until automatic release of a milestone.
    ///
    /// This is a read-only query that does not mutate any ledger state. It calculates
    /// the time until the auto-release deadline based on:
    /// - The milestone's delivery timestamp (from temporary storage if available, else from persistent)
    /// - The configured auto-release delay from the job metadata
    /// - Any active time extension applied to the milestone
    ///
    /// # Return Value
    ///
    /// - **Positive value**: The number of seconds remaining until auto-release.
    ///   - At or near the deadline, this approaches 0.
    ///   - The deadline is: `delivered_at + auto_release_seconds + extension_seconds`.
    ///
    /// - **Zero or Negative**: Deadline has passed; auto-release is or was overdue.
    ///   - In populated state: Returns negative if `current > deadline`.
    ///   - In boundary state: Returns error if deadline or current timestamp overflows i64.
    ///
    /// - **Error**: Returned if:
    ///   - The deadline calculation overflows (deadline too far in future).
    ///   - The timestamp conversion to i64 fails.
    ///   - The subtraction overflows (current > deadline, rare in i64).
    ///   - The job metadata is missing (`NotInitialized`).
    ///   - The milestone is missing (`InvalidMilestone`).
    ///
    /// # Behavior in Edge Cases
    ///
    /// - Empty state (no milestone): Returns `InvalidMilestone`.
    /// - Boundary state (large extensions/timestamps): Returns error on overflow.
    /// - Expired milestone: Returns 0 or negative value (seconds past deadline).
    /// - Just-delivered (delivered_at = current): Returns approximately `auto_release_seconds + extension_seconds`.
    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> Result<i64, Error> {
        let meta = Self::load_job_meta(&env)?;
        let milestone = Self::load_milestone(&env, milestone_index)?;
        // Read delivery timestamp from temporary storage (optimised path) and
        // fall back to the persistent Milestone field for pre-migration entries.
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let extension = Self::load_time_extension(&env, milestone_index);
        let deadline = Self::auto_release_deadline(&meta, delivered_at, extension)?;
        let current = env.ledger().timestamp();
        let deadline_i64 = i64::try_from(deadline).map_err(|_| Error::ArithmeticOverflow)?;
        let current_i64 = i64::try_from(current).map_err(|_| Error::ArithmeticOverflow)?;
        deadline_i64
            .checked_sub(current_i64)
            .ok_or(Error::ArithmeticOverflow)
    }

    /// Release a partial payment for a delivered milestone.
    ///
    /// Transfers `amount` to the freelancer immediately. If the released
    /// total reaches the full milestone amount, the milestone transitions
    /// to `Released` and both parties' reputation is incremented; otherwise
    /// it moves to (or stays at) `PartiallyReleased` so further partial
    /// approvals can follow.
    ///
    /// # Parameters
    /// * `client`          – Must match the job's stored client. Must
    ///                       authorize the call.
    /// * `milestone_index` – Index of the milestone being paid out.
    /// * `amount`          – Amount to release now. Must be positive and no
    ///                       more than the milestone's remaining balance.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `client` is a zero address or the contract
    ///                        address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – The milestone is not `Delivered` or
    ///                        `PartiallyReleased`.
    /// * `InvalidAmount`    – `amount` is not positive, or exceeds the
    ///                        milestone's remaining balance.
    pub fn approve_partial(
        env: Env,
        client: Address,
        milestone_index: u32,
        amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        let zero_1 = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_2 = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if client == zero_1 || client == zero_2 || client == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if amount > remaining {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.freelancer, &amount)?;

        let mut updated_milestone = milestone;
        updated_milestone.released_amount = updated_milestone
            .released_amount
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;

        if updated_milestone.released_amount == updated_milestone.amount {
            updated_milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
            Self::clear_time_ext(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        } else {
            updated_milestone.status = MilestoneStatus::PartiallyReleased;
        }

        Self::store_milestone(&env, milestone_index, &updated_milestone);

        let event_remaining = updated_milestone
            .amount
            .checked_sub(updated_milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        env.events().publish(
            (symbol_short!("approve"), meta.client.clone()),
            ApprovedEvent {
                milestone_index,
                amount,
                released_amount: updated_milestone.released_amount,
                remaining: event_remaining,
                status: updated_milestone.status.clone(),
            },
        );

        Ok(())
    }

    /// Approve a delivered milestone and release its full remaining balance
    /// to the freelancer.
    ///
    /// Transfers the remaining amount owed, marks the milestone `Released`,
    /// and increments the reputation of both the client and freelancer.
    ///
    /// # Parameters
    /// * `client`          – Must match the job's stored client. Must
    ///                       authorize the call.
    /// * `milestone_index` – Index of the milestone to approve.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `client` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – The milestone is not currently `Delivered`.
    /// * `InvalidAmount`    – The milestone's remaining balance is not
    ///                        positive.
    pub fn approve_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if client == zero_account || client == zero_contract {
            return Err(Error::InvalidAddress);
        }

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.freelancer, &remaining)?;
        milestone.released_amount = milestone.amount;

        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::clear_time_ext(&env, milestone_index);
        Self::increment_reputation(&env, &meta.client);
        Self::increment_reputation(&env, &meta.freelancer);

        // Write a short-lived completion flag to temporary storage.  This is
        // transient workflow state: the milestone approval window is now
        // permanently closed, so this signal does not need to survive beyond
        // the TTL of the ledger entry.  Using temporary storage avoids the
        // higher rent cost of a persistent or instance entry for data that has
        // no long-term value.
        Self::store_milestone_released(&env, milestone_index);

        let event_remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;

        env.events().publish(
            (symbol_short!("approve"), meta.client.clone()),
            ApprovedEvent {
                milestone_index,
                amount: remaining,
                released_amount: milestone.released_amount,
                remaining: event_remaining,
                status: milestone.status.clone(),
            },
        );

        Ok(())
    }

    /// Withdraw the amount owed to `caller` from payouts or refunds that could
    /// not be transferred at settlement time (see `pay_or_credit`).
    ///
    /// The credit is cleared before the transfer. If the transfer still fails
    /// (e.g. the trustline is still missing) the whole call reverts and the
    /// credit is kept, so `caller` can retry once their account can receive.
    ///
    /// # Parameters
    /// * `caller` – Address with an owed balance. Must authorize the call.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `InvalidAmount`  – Nothing is owed to `caller`.
    pub fn withdraw(env: Env, caller: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;

        let key = DataKey::Owed(caller.clone());
        let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total_owed = Self::load_total_owed(&env)
            .checked_sub(amount)
            .ok_or(Error::ArithmeticOverflow)?;

        // CEI: clear the credit before the external transfer.
        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .set(&DataKey::TotalOwed, &total_owed);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &caller, &amount);

        env.events().publish(
            (symbol_short!("withdraw"),),
            WithdrawnEvent {
                recipient: caller,
                token: meta.token,
                amount,
            },
        );

        Ok(())
    }

    /// Approve several delivered milestones in one transaction (#741).
    ///
    /// Releases the full remaining balance of every listed milestone with a
    /// **single** token transfer of the summed amount, marks each one
    /// `Released`, increments both parties' reputation once per milestone
    /// (as `approve_milestone` would), and emits one `approve` event per
    /// milestone, in the order given.
    ///
    /// The call is all-or-nothing: every index is validated before any state
    /// is written, and any invalid, duplicated or ineligible index fails the
    /// whole call with nothing released.
    ///
    /// Unlike `approve_milestone`, a `PartiallyReleased` milestone is
    /// accepted too; its outstanding remainder is what gets released.
    ///
    /// # Parameters
    /// * `client`  – Must match the job's stored client. Must authorize the
    ///               call.
    /// * `indices` – Milestones to approve: 1 to `MAX_BATCH_APPROVALS` (10)
    ///               distinct indices.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `client` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `indices` is empty, longer than the cap,
    ///                        contains a duplicate, or contains an
    ///                        out-of-range index.
    /// * `InvalidStatus`    – A listed milestone is not `Delivered` or
    ///                        `PartiallyReleased`.
    /// * `InvalidAmount`    – A listed milestone's remaining balance is not
    ///                        positive, or the sum overflows.
    pub fn approve_milestones(env: Env, client: Address, indices: Vec<u32>) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if client == zero_account || client == zero_contract {
            return Err(Error::InvalidAddress);
        }

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        let count = indices.len();
        if count == 0 || count > MAX_BATCH_APPROVALS {
            return Err(Error::InvalidMilestone);
        }

        // CHECKS: validate every index and sum the payout before writing
        // anything, so a bad entry anywhere in the list leaves state untouched.
        let mut milestones: Vec<Milestone> = Vec::new(&env);
        let mut released: Vec<i128> = Vec::new(&env);
        let mut total: i128 = 0;
        for (position, milestone_index) in indices.iter().enumerate() {
            if milestone_index >= meta.milestone_count {
                return Err(Error::InvalidMilestone);
            }
            // `count` is capped at MAX_BATCH_APPROVALS, so this scan is bounded.
            for earlier in indices.iter().take(position) {
                if earlier == milestone_index {
                    return Err(Error::InvalidMilestone);
                }
            }

            let milestone = Self::load_milestone(&env, milestone_index)?;
            Self::require_milestone_is_funded(&env, &meta, &milestone)?;
            if milestone.status != MilestoneStatus::Delivered
                && milestone.status != MilestoneStatus::PartiallyReleased
            {
                return Err(Error::InvalidStatus);
            }
            let remaining = milestone
                .amount
                .checked_sub(milestone.released_amount)
                .ok_or(Error::InvalidAmount)?;
            if remaining <= 0 {
                return Err(Error::InvalidAmount);
            }
            total = total.checked_add(remaining).ok_or(Error::InvalidAmount)?;
            milestones.push_back(milestone);
            released.push_back(remaining);
        }

        // EFFECTS: commit every milestone's terminal state before the
        // transfer (Checks-Effects-Interactions).
        for (milestone_index, mut milestone) in indices.iter().zip(milestones.iter()) {
            milestone.released_amount = milestone.amount;
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone(&env, milestone_index, &milestone);
            Self::store_milestone_released(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        }

        // INTERACTION: one transfer for the whole batch.
        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.freelancer, &total)?;

        for (milestone_index, (milestone, amount)) in
            indices.iter().zip(milestones.iter().zip(released.iter()))
        {
            env.events().publish(
                (symbol_short!("approve"), meta.client.clone()),
                ApprovedEvent {
                    milestone_index,
                    amount,
                    released_amount: milestone.amount,
                    remaining: 0,
                    status: MilestoneStatus::Released,
                },
            );
        }

        Ok(())
    }

    /// Raise a dispute on a milestone, freezing it for arbitration.
    ///
    /// Either the client or the freelancer may call this. Moves the
    /// milestone to `Disputed`, from which only `resolve_dispute` (or a
    /// split via `apply_dispute_arbitration_split`) can move it forward.
    ///
    /// A dispute raised while the milestone is `Pending` is a *pre-delivery
    /// (scope) dispute*; one raised while `Delivered` or `PartiallyReleased`
    /// is a *delivery (quality) dispute*. Both freeze the milestone and share
    /// the same arbiter outcomes. See `SPEC.md` §"Dispute semantics" for
    /// the full per-status contract.
    ///
    /// # Parameters
    /// * `caller`          – Must be either the job's client or freelancer.
    ///                       Must authorize the call.
    /// * `milestone_index` – Index of the milestone being disputed.
    /// * `reason_hash`     – Commitment (e.g. SHA-256) to the off-chain
    ///                       statement or evidence bundle explaining the
    ///                       dispute. Stored with `disputed_at` and echoed in
    ///                       `DisputeRaisedEvent` so arbiters have an on-chain
    ///                       anchor for what the raiser claimed (#712).
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `caller` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `caller` is neither the client nor the
    ///                        freelancer.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – The milestone is not `Delivered` or
    ///                        `PartiallyReleased`.
    /// * `NotAccepted`      – The freelancer or arbiter has not accepted the
    ///                        job.
    pub fn raise_dispute(
        env: Env,
        caller: Address,
        milestone_index: u32,
        reason_hash: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;

        // ── Caller validation (before any storage write) ─────────────────
        // Reject zero addresses before touching ledger state so that no
        // storage entry is mutated on an invalid caller.
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }

        // require_dispute_party performs caller.require_auth() + verifies the
        // caller matches the stored client or freelancer in a single step.
        // Running this before the DisputeLock write ensures that neither an
        // unauthorized caller nor a wrong-party caller can cause any storage
        // mutation.
        let meta = Self::require_dispute_party(&env, &caller)?;

        // ── Re-entrancy lock ─────────────────────────────────────────────
        // The lock is acquired only after the caller is confirmed to be
        // authorized, so a rejected call leaves no trace in storage.
        if env
            .storage()
            .temporary()
            .has(&DataKey::DisputeLock(milestone_index))
        {
            return Err(Error::DisputeAlreadyRaised);
        }
        env.storage()
            .temporary()
            .set(&DataKey::DisputeLock(milestone_index), &true);

        let result = Self::raise_dispute_inner(&env, caller, milestone_index, reason_hash, meta);

        // Always release the lock regardless of success or failure.
        Self::release_dispute_lock(&env, milestone_index);

        result
    }

    /// Core dispute logic extracted so that the lock guard in
    /// `raise_dispute` wraps every path uniformly.  This function
    /// is never called directly — it exists only to keep the
    /// lock/release pairing in one place.
    ///
    /// `meta` is pre-validated by `raise_dispute` (zero-address check and
    /// `require_auth` already performed) so this function can proceed
    /// directly to business-logic checks.
    fn raise_dispute_inner(
        env: &Env,
        caller: Address,
        milestone_index: u32,
        reason_hash: BytesN<32>,
        meta: JobMeta,
    ) -> Result<(), Error> {
        Self::require_escrow_has_funding(env, &meta)?;

        // ── Input validation: index boundary check ───────────────────────
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        // Nothing can be disputed before both parties accepted the job (#757);
        // checked before the status so callers learn why.
        Self::require_accepted(env)?;
        let mut milestone = Self::load_milestone(env, milestone_index)?;
        Self::require_milestone_is_funded(env, &meta, &milestone)?;

        // ── Input validation: non-zero positive amount ───────────────────
        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Strict state machine (see SPEC.md "Dispute semantics"): a dispute
        // needs delivered work to arbitrate, so only Delivered (quality) and
        // PartiallyReleased (the unreleased remainder) may be disputed. Pending,
        // Disputed and the terminal statuses are rejected with InvalidStatus.
        match milestone.status {
            MilestoneStatus::Delivered | MilestoneStatus::PartiallyReleased => {}
            _ => return Err(Error::InvalidStatus),
        }

        let disputed_at = env.ledger().timestamp();
        env.storage().persistent().set(
            &DataKey::DisputeInfo(milestone_index),
            &DisputeInfo {
                disputed_at,
                previous_status: milestone.status.clone(),
                reason_hash: reason_hash.clone(),
                evidence: Vec::new(env),
            },
        );
        milestone.status = MilestoneStatus::Disputed;
        Self::store_milestone(env, milestone_index, &milestone);

        // Write a short-lived dispute flag to temporary storage so that callers
        // can verify dispute status without loading the full persistent
        // Milestone entry, reducing ledger footprint on the read path.
        Self::store_dispute_flag(env, milestone_index);

        env.events().publish(
            (symbol_short!("dispute"), caller.clone()),
            DisputeRaisedEvent {
                milestone_index,
                caller,
                milestone_amount: milestone.amount,
                reason_hash,
                disputed_at,
                new_status: MilestoneStatus::Disputed,
            },
        );

        Ok(())
    }

    /// Attach an evidence hash to an already-open dispute (#712).
    ///
    /// Either the client or the freelancer may anchor evidence while the
    /// milestone is `Disputed`. The hash is stored in the milestone's
    /// `DisputeInfo` record (bounded by `MAX_DISPUTE_EVIDENCE_ENTRIES`) and
    /// echoed in an `EvidenceSubmittedEvent`, so arbiters can reconstruct what
    /// each party submitted without trusting off-chain metadata.
    ///
    /// # Parameters
    /// * `caller`          – Must be either the job's client or freelancer.
    ///                       Must authorize the call.
    /// * `milestone_index` – Index of the disputed milestone.
    /// * `evidence_hash`   – Commitment to the off-chain evidence bundle.
    ///
    /// # Errors
    /// * `Paused`               – The contract is emergency-paused.
    /// * `InvalidAddress`       – `caller` is a zero address.
    /// * `NotInitialized`       – Contract has not been initialized.
    /// * `Unauthorized`         – `caller` is neither the client nor the
    ///                            freelancer.
    /// * `NotFunded`            – The escrow has not been funded yet.
    /// * `InvalidMilestone`     – `milestone_index` is out of range.
    /// * `InvalidStatus`        – The milestone is not `Disputed`.
    /// * `EvidenceLimitReached` – The dispute already holds
    ///                            `MAX_DISPUTE_EVIDENCE_ENTRIES` hashes.
    pub fn submit_evidence(
        env: Env,
        caller: Address,
        milestone_index: u32,
        evidence_hash: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;

        // Validate the caller before any storage read/write so an unauthorized
        // address neither pays for the dispute lookup nor mutates state.
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }
        let meta = Self::require_dispute_party(&env, &caller)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Disputed {
            return Err(Error::InvalidStatus);
        }

        let key = DataKey::DisputeInfo(milestone_index);
        let mut dispute_info: DisputeInfo = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::InvalidStatus)?;
        if dispute_info.evidence.len() >= MAX_DISPUTE_EVIDENCE_ENTRIES {
            return Err(Error::EvidenceLimitReached);
        }

        let submitted_at = env.ledger().timestamp();
        dispute_info.evidence.push_back(EvidenceRecord {
            submitter: caller.clone(),
            evidence_hash: evidence_hash.clone(),
            submitted_at,
        });
        env.storage().persistent().set(&key, &dispute_info);

        env.events().publish(
            (symbol_short!("evidence"),),
            EvidenceSubmittedEvent {
                milestone_index,
                caller,
                evidence_hash,
                submitted_at,
            },
        );

        Ok(())
    }

    /// Read the on-chain record for an open dispute (#712).
    ///
    /// Returns the raising party's `reason_hash`, the ledger timestamp of the
    /// raise (`disputed_at`), the status to restore on escalation, and every
    /// evidence hash submitted so far. Callers use this to anchor arbitration
    /// context without relying on off-chain metadata.
    ///
    /// # Errors
    /// * [`Error::InvalidStatus`] – No open dispute exists for the milestone.
    pub fn get_dispute_info(env: Env, milestone_index: u32) -> Result<DisputeInfo, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::DisputeInfo(milestone_index))
            .ok_or(Error::InvalidStatus)
    }

    /// Escalate an unresolved dispute after its configured timeout by
    /// restoring the milestone to the status it had before the dispute.
    ///
    /// Either escrow party may call this function. A restored Delivered
    /// milestone receives a fresh auto-release window starting at escalation.
    ///
    /// # Errors
    /// * [`Error::Paused`] – The contract is emergency-paused.
    /// * [`Error::InvalidAddress`] – `caller` is a zero address.
    /// * [`Error::NotInitialized`] – The escrow has not been initialized.
    /// * [`Error::Unauthorized`] – `caller` is not the client or freelancer.
    /// * [`Error::NotFunded`] – The escrow has not been funded.
    /// * [`Error::InvalidMilestone`] – `milestone_index` is out of range.
    /// * [`Error::InvalidStatus`] – The milestone is not disputed or its
    ///   dispute metadata is missing.
    /// * [`Error::DisputeTimeoutNotReached`] – The timeout has not elapsed.
    pub fn escalate_dispute(env: Env, caller: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;

        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }
        let meta = Self::require_dispute_party(&env, &caller)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Disputed {
            return Err(Error::InvalidStatus);
        }
        let dispute_info: DisputeInfo = env
            .storage()
            .persistent()
            .get(&DataKey::DisputeInfo(milestone_index))
            .ok_or(Error::InvalidStatus)?;
        let timeout = env
            .storage()
            .instance()
            .get(&DataKey::DisputeTimeout)
            .unwrap_or(DISPUTE_TIMEOUT_SECONDS);
        let escalated_at = env.ledger().timestamp();
        let elapsed = escalated_at
            .checked_sub(dispute_info.disputed_at)
            .ok_or(Error::DisputeTimeoutNotReached)?;
        if elapsed < timeout {
            return Err(Error::DisputeTimeoutNotReached);
        }

        milestone.status = dispute_info.previous_status.clone();
        if milestone.status == MilestoneStatus::Delivered {
            milestone.delivered_at = escalated_at;
            Self::store_delivered_at(&env, milestone_index, escalated_at);
        }
        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        env.storage()
            .temporary()
            .remove(&DataKey::DisputeFlag(milestone_index));

        env.events().publish(
            (symbol_short!("dispescal"),),
            DisputeEscalatedEvent {
                milestone_index,
                caller,
                disputed_at: dispute_info.disputed_at,
                escalated_at,
                restored_status: milestone.status,
            },
        );

        Ok(())
    }

    /// Resolve a disputed milestone by releasing its remaining balance to
    /// either the freelancer or the client.
    ///
    /// Only callable while the milestone is `Disputed`. The payout is capped
    /// at the contract's current token balance in case a shortfall exists.
    /// A full release increments both parties' reputation.
    ///
    /// # Parameters
    /// * `arbiter`                – Must match the job's stored arbiter.
    ///                              Must authorize the call.
    /// * `milestone_index`        – Index of the disputed milestone.
    /// * `release_to_freelancer`  – `true` releases the remaining balance to
    ///                              the freelancer (milestone → `Released`);
    ///                              `false` refunds it to the client
    ///                              (milestone → `Refunded`).
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `arbiter` is a zero address or the contract
    ///                        address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `arbiter` does not match the job's arbiter.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidStatus`    – The milestone is not currently `Disputed`.
    /// * `NotAccepted`      – The freelancer or arbiter has not accepted the
    ///                        job.
    /// * `InvalidAmount`    – The milestone's remaining balance is not positive.
    /// * `EmptyBalance`     – The contract's token balance is not positive.
    pub fn resolve_dispute(
        env: Env,
        arbiter: Address,
        milestone_index: u32,
        release_to_freelancer: bool,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if arbiter == zero_account
            || arbiter == zero_contract
            || arbiter == env.current_contract_address()
        {
            return Err(Error::InvalidAddress);
        }
        arbiter.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        // Strict state machine: resolve_dispute may only run while the
        // milestone is Disputed. Every other source status is rejected with
        // InvalidStatus before any payment or status mutation occurs.
        // Allowed transitions:
        //   Disputed → Released  (release_to_freelancer = true)
        //   Disputed → Refunded  (release_to_freelancer = false)
        match milestone.status {
            MilestoneStatus::Disputed => {}
            MilestoneStatus::Pending
            | MilestoneStatus::Delivered
            | MilestoneStatus::PartiallyReleased
            | MilestoneStatus::Released
            | MilestoneStatus::Refunded
            | MilestoneStatus::Split => return Err(Error::InvalidStatus),
        }
        Self::require_accepted(&env)?;

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = Self::available_balance(&env, &token_client);
        if contract_balance < remaining {
            return Err(Error::InsufficientBalance);
        }

        // Issue #738: the arbiter's fee comes out of the disputed balance
        // first; the winning party receives the rest. The balance check above
        // (#689) guarantees the whole remaining amount is available.
        let arbiter_fee_amount = Self::arbiter_fee_for(&env, remaining, contract_balance)?;
        let payout = remaining
            .checked_sub(arbiter_fee_amount)
            .ok_or(Error::InvalidAmount)?;
        if arbiter_fee_amount > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.arbiter, &arbiter_fee_amount)?;
        }
        if release_to_freelancer {
            milestone.released_amount = milestone
                .released_amount
                .checked_add(payout)
                .ok_or(Error::InvalidAmount)?;
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone(&env, milestone_index, &milestone);

            if payout > 0 {
                Self::pay_or_credit(&env, &token_client, &meta.freelancer, &payout)?;
            }
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        } else {
            milestone.status = MilestoneStatus::Refunded;
            Self::store_milestone(&env, milestone_index, &milestone);

            if payout > 0 {
                Self::pay_or_credit(&env, &token_client, &meta.client, &payout)?;
            }
        }
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

        env.events().publish(
            (symbol_short!("resolve"),),
            DisputeResolvedEvent {
                milestone_index,
                arbiter: meta.arbiter.clone(),
                // `amount` is what was owed before capping to the available
                // balance; `paid_amount` is what actually moved to the
                // winning party, excluding the arbiter fee.
                amount: remaining,
                paid_amount: payout,
                arbiter_fee_amount,
                released_to_freelancer: release_to_freelancer,
                status: milestone.status.clone(),
            },
        );

        Ok(())
    }

    // ── dispute_arbitration_split: storage-optimised key design ────────────
    //
    // Design rationale
    // ─────────────────
    // A naïve split-state layout would persist a full `RefundAllocation`
    // (2×i128 + 2×u32) under Address-bearing keys such as
    // `(arbiter: Address, milestone_index: u32)`.  On Soroban each Address
    // contributes ~32 bytes to the ledger key footprint.
    //
    // This implementation uses three optimisations to minimise bytes stored:
    //
    // 1. **Key is only `u32`** (`ArbitrationSplitBps(milestone_index)`) —
    //    no Address payload in the key.
    //
    // 2. **Value is a single `u32` BPS** — freelancer BPS is derived as
    //    `BPS_SCALE - client_refund_bps`, so the second BPS field and both
    //    i128 payout amounts are never written to storage (amounts live in
    //    the persistent `Milestone` entry already required for settlement).
    //
    // 3. **Temporary storage tier** — the compact BPS signal is auto-evicted
    //    after the dispute workflow ends rather than accruing persistent rent.
    //
    // Deterministic access: the same milestone index always maps to the same
    // key; reads never require scanning Address-keyed maps.

    /// Allocate a disputed amount into client refund vs freelancer payout by BPS.
    ///
    /// Uses floor division for the client share and assigns the remainder to the
    /// freelancer so the two legs always sum exactly to `total_amount` (no value
    /// is lost to rounding).
    fn allocate_refund_by_bps(
        total_amount: i128,
        client_refund_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        let (client_refund, freelancer_payout) =
            math::split_bps(total_amount, client_refund_bps, math::Rounding::Floor)?;

        Ok(RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps: BPS_SCALE - client_refund_bps,
        })
    }

    /// Pure refund-allocation algorithm for split-refund dispute claims.
    ///
    /// Split a disputed milestone amount between client and freelancer using
    /// arbiter-specified basis points.
    ///
    /// The arbiter decides how much of the escrowed `total_amount` the
    /// freelancer receives, expressed in basis points (1 bp = 0.01 %).
    /// The client receives the remainder.  Both values are guaranteed to sum
    /// exactly to `total_amount` because the client share is computed as
    /// `total_amount - freelancer_share` rather than independently.
    ///
    /// # Parameters
    /// * `total_amount`         – Total escrowed balance to distribute. Must be ≥ 0.
    /// * `freelancer_bps`       – Basis points awarded to the freelancer. Range: 0 – 10_000.
    ///                            0 → full refund to client, 10_000 → full release to freelancer.
    ///
    /// # Returns
    /// A [`RefundAllocation`] with:
    /// * `freelancer_payout`      = round_nearest(`total_amount` × `freelancer_bps` / 10_000)
    /// * `client_refund`          = `total_amount` − `freelancer_payout`
    /// * `freelancer_payout_bps`  = `freelancer_bps` (echoed for auditability)
    /// * `client_refund_bps`      = 10_000 − `freelancer_bps`
    ///
    /// # Errors
    /// * `InvalidAmount` – `total_amount` is negative, or an intermediate
    ///                     multiplication overflows `i128`.
    /// * `InvalidRatio`  – `freelancer_bps` exceeds 10_000.
    pub fn dispute_arbitration_split(
        _env: Env,
        total_amount: i128,
        freelancer_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // Guard: total must be non-negative.
        if total_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        // Guard: basis points must be within [0, 10_000].
        if freelancer_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        // Use the shared split_round_nearest primitive for consistent rounding.
        // numerator   = freelancer_bps
        // denominator = BPS_SCALE (10_000)
        let split =
            Self::split_round_nearest(total_amount, freelancer_bps as i128, BPS_SCALE as i128)?;

        let freelancer_payout = split.first;
        let client_refund = split.second;
        let client_refund_bps = BPS_SCALE
            .checked_sub(freelancer_bps)
            .ok_or(Error::InvalidRatio)?;

        Ok(RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps: freelancer_bps,
        })
    }

    /// Apply a BPS split-refund to a disputed milestone and transfer funds.
    ///
    /// Client receives `client_refund_bps` of the remaining balance; freelancer
    /// receives the remainder. Milestones end `Refunded` or `Released` for a
    /// one-sided outcome and `Split` when both parties receive a payout.
    ///
    /// After a successful apply, a compact temporary entry
    /// `ArbitrationSplitBps(milestone_index) → client_refund_bps` is written so
    /// downstream readers can confirm the applied split without loading a full
    /// `RefundAllocation` or an Address-keyed map.
    pub fn apply_dispute_arbitration_split(
        env: Env,
        arbiter: Address,
        milestone_index: u32,
        client_refund_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if arbiter == zero_account
            || arbiter == zero_contract
            || arbiter == env.current_contract_address()
        {
            return Err(Error::InvalidAddress);
        }
        arbiter.require_auth();

        let meta = Self::load_job_meta(&env)?;
        if meta.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if milestone.status != MilestoneStatus::Disputed {
            return Err(Error::InvalidStatus);
        }
        Self::require_accepted(&env)?;

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = Self::available_balance(&env, &token_client);
        if contract_balance < remaining {
            return Err(Error::InsufficientBalance);
        }

        // Issue #738: the arbiter's fee comes out of the disputed balance
        // first; the parties split what is left in the requested ratio. The
        // balance check above (#689) guarantees the whole remaining amount is
        // available, so neither leg is capped.
        let arbiter_fee_amount = Self::arbiter_fee_for(&env, remaining, contract_balance)?;
        let split_amount = remaining
            .checked_sub(arbiter_fee_amount)
            .ok_or(Error::InvalidAmount)?;
        let allocation = Self::allocate_refund_by_bps(split_amount, client_refund_bps)?;
        if arbiter_fee_amount > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.arbiter, &arbiter_fee_amount)?;
        }

        let client_refund = allocation.client_refund;
        let freelancer_payout = allocation.freelancer_payout;

        if client_refund > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.client, &client_refund)?;
        }
        if freelancer_payout > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.freelancer, &freelancer_payout)?;
        }

        milestone.released_amount = milestone
            .released_amount
            .checked_add(freelancer_payout)
            .ok_or(Error::InvalidAmount)?;

        if freelancer_payout == 0 {
            milestone.status = MilestoneStatus::Refunded;
        } else if client_refund > 0 {
            milestone.status = MilestoneStatus::Split;
        } else {
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        }

        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

        // Compact temporary signal: one u32 key + one u32 value (not a full
        // RefundAllocation, and not an Address-bearing composite key).
        Self::store_arbitration_split_bps(&env, milestone_index, client_refund_bps);

        let resolved = RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps: allocation.client_refund_bps,
            freelancer_payout_bps: allocation.freelancer_payout_bps,
        };

        let paid_amount = client_refund
            .checked_add(freelancer_payout)
            .ok_or(Error::InvalidAmount)?;

        env.events().publish(
            (symbol_short!("resolve"),),
            DisputeResolvedEvent {
                milestone_index,
                arbiter: meta.arbiter.clone(),
                amount: remaining,
                paid_amount,
                arbiter_fee_amount,
                released_to_freelancer: freelancer_payout > 0,
                status: milestone.status.clone(),
            },
        );

        // Dedicated structured record of this arbitration split: the acting
        // arbiter plus every value persisted by the call (transferred amounts,
        // the stored client_refund_bps, cumulative release, terminal status).
        env.events().publish(
            (symbol_short!("arbsplit"),),
            ArbitrationSplitAppliedEvent {
                arbiter: meta.arbiter.clone(),
                milestone_index,
                client: meta.client.clone(),
                freelancer: meta.freelancer.clone(),
                token: meta.token.clone(),
                client_refund,
                freelancer_payout,
                arbiter_fee_amount,
                client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
                released_amount: milestone.released_amount,
                status: milestone.status.clone(),
            },
        );

        Ok(resolved)
    }

    /// Initiate cancellation of the escrow, freezing it pending an admin
    /// override.
    ///
    /// The first caller (client or freelancer) records their approval in a
    /// persistent [`CancelApprovalRecord`] and emits
    /// `CancelApprovalRecordedEvent` with the round's `approved_at` timestamp.
    /// The `CancelLock` is **not** set on a single-signature call — both parties
    /// must call before the escrow is frozen.  This prevents either party from
    /// unilaterally locking the other out.
    ///
    /// Once the second party calls, both bits in the mask are set, the
    /// `CancelLock` is activated, the approval record is cleared, and the
    /// `CancelEscrowInitiatedEvent` is emitted.
    ///
    /// # Expiry (issue #713)
    /// An approval is only valid for [`CANCEL_APPROVAL_TTL_SECONDS`] (7 days).
    /// A call made at or after that age treats the stored approval as absent:
    /// it records a fresh first approval under the caller's own bit and
    /// restarts the TTL window at the current ledger timestamp instead of
    /// completing the lock.  This stops a forgotten approval from freezing the
    /// escrow months after the disagreement it expressed was resolved.
    ///
    /// # Parameters
    /// * `caller` – Must be either the job's client or freelancer.
    ///
    /// # Errors
    /// * `InvalidAddress` – `caller` is a zero address.
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `caller` is neither the client nor the freelancer.
    /// * `NotFunded`      – The escrow has not been funded yet.
    /// * `EscrowLocked`   – Both parties already approved; lock is already active.
    /// * `InvalidStatus`  – `caller` has already recorded an unexpired approval
    ///                      in this round (duplicate single-party call).
    pub fn cancel_escrow(env: Env, caller: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }

        // Reject if the contract is emergency-paused.
        let emergency_paused = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::Ep)
            .unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Reject a duplicate cancel — CancelLock already active.
        let already_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if already_locked {
            return Err(Error::EscrowLocked);
        }

        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if caller != meta.client && caller != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        // Boundary guard: cancelling against an empty escrow has no funds
        // to resolve, so block processing until the contract holds a
        // positive token balance.
        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        let now = env.ledger().timestamp();
        let bit = if caller == meta.client { 1u32 } else { 2u32 };
        let stored: Option<CancelApprovalRecord> =
            env.storage().instance().get(&DataKey::CancelApproval);
        // Approvals older than the TTL are treated as absent (issue #713): the
        // record is folded down to "no live approvals", so this call starts a
        // fresh round instead of completing a lock on stale intent.
        let live_mask = match stored {
            Some(record)
                if now.saturating_sub(record.approved_at) < CANCEL_APPROVAL_TTL_SECONDS =>
            {
                record.mask
            }
            _ => 0,
        };
        if live_mask & bit != 0 {
            return Err(Error::InvalidStatus);
        }

        let new_mask = live_mask | bit;
        if new_mask == 3 {
            env.storage().instance().remove(&DataKey::CancelApproval);
            env.storage().instance().set(&DataKey::CancelLock, &true);
            env.events().publish(
                (symbol_short!("cancel"),),
                CancelEscrowInitiatedEvent {
                    caller_is_client: caller == meta.client,
                    client: meta.client.clone(),
                    freelancer: meta.freelancer.clone(),
                    token: meta.token.clone(),
                    milestone_count: meta.milestone_count,
                    total_amount: meta.total_amount,
                    caller,
                },
            );
            return Ok(());
        }

        // `new_mask != 3` means no live approval survived, so this is the first
        // approval of a fresh round and the TTL window starts now.
        env.storage().instance().set(
            &DataKey::CancelApproval,
            &CancelApprovalRecord {
                mask: new_mask,
                approved_at: now,
            },
        );
        env.events().publish(
            (symbol_short!("cxlappr"),),
            CancelApprovalRecordedEvent {
                caller,
                approval_mask: new_mask,
                approved_at: now,
            },
        );

        Ok(())
    }

    /// Revoke a previously recorded cancellation approval before the final
    /// second-party lock is set.
    ///
    /// # Storage footprint
    /// The approval record is a [`CancelApprovalRecord`] (`mask` plus
    /// `approved_at`) under the instance key `DataKey::CancelApproval`.  A
    /// stored mask only ever holds a single party's bit (both bits together
    /// fire the lock and clear the record in `cancel_escrow`), so revoking
    /// always leaves the mask empty and the key is **removed** rather than
    /// rewritten as a zero mask.  After a successful revoke the instance
    /// storage is byte-identical to its state before the approval was first
    /// recorded.
    ///
    /// Revoking an approval that has already expired (#713) is still allowed:
    /// the bit is present in storage until something clears it, and removing it
    /// restores the pre-approval footprint.
    ///
    /// # Errors
    /// * `InvalidAddress` – `caller` is a zero address.
    /// * `NotInitialized` – Contract has not been initialized.  Checked before
    ///                      `require_auth`, so an uninitialized contract is
    ///                      rejected with a typed error, never a panic, and
    ///                      without touching storage.
    /// * `Unauthorized`   – `caller` is neither the client nor the freelancer.
    /// * `EscrowLocked`   – Both parties already approved; lock is active.
    /// * `InvalidStatus`  – `caller` has no recorded approval to revoke.
    pub fn revoke_cancel_approval(env: Env, caller: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }

        // Initialization guard runs before `require_auth` so an uninitialized
        // contract returns `NotInitialized` instead of failing on auth.
        let meta = Self::load_job_meta(&env)?;
        caller.require_auth();

        if caller != meta.client && caller != meta.freelancer {
            return Err(Error::Unauthorized);
        }

        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if cancel_locked {
            return Err(Error::EscrowLocked);
        }

        let bit = if caller == meta.client { 1u32 } else { 2u32 };
        let (current_mask, approved_at) = match env
            .storage()
            .instance()
            .get::<_, CancelApprovalRecord>(&DataKey::CancelApproval)
        {
            Some(record) => (record.mask, record.approved_at),
            None => (0u32, 0u64),
        };
        if current_mask & bit == 0 {
            return Err(Error::InvalidStatus);
        }

        let new_mask = current_mask & !bit;
        if new_mask == 0 {
            env.storage().instance().remove(&DataKey::CancelApproval);
        } else {
            env.storage().instance().set(
                &DataKey::CancelApproval,
                &CancelApprovalRecord {
                    mask: new_mask,
                    approved_at,
                },
            );
        }

        env.events().publish(
            (symbol_short!("cancelrev"),),
            CancelApprovalRevokedEvent {
                caller,
                approval_mask: new_mask,
            },
        );

        Ok(())
    }

    /// Admin emergency override: resolve a cancel-locked escrow by releasing
    /// all remaining milestone funds to the freelancer.
    ///
    /// When `cancel_escrow` is called by either party, the compact `C` lock is set
    /// that blocks all normal operations.  This endpoint lets the verified admin
    /// break the deadlock by force-releasing every non-terminal milestone to the
    /// freelancer in a single atomic transaction.
    ///
    /// # Checks (in order)
    /// 1. `admin.require_auth()` — SDK-level signature check.
    /// 2. `require_admin` — verified admin key matches `DataKey::Admin`.
    /// 3. Contract must be initialised (`NotInitialized`).
    /// 4. Escrow must be funded (`NotFunded`).
    /// 5. The compact `C` lock must be active (`InvalidStatus`).
    ///
    /// # Effects
    /// - Every milestone in a non-terminal status (`!Released && !Refunded`)
    ///   is moved to `Released` and its remaining balance is summed.
    /// - The total is transferred from the contract to the freelancer in a
    ///   single token call.
    /// - The compact `C` lock entry is removed so subsequent queries are unblocked.
    /// - `YieldAccrued` is removed (same as `admin_override_cancel_refund`).
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract not initialised.
    /// * `Unauthorized`    – Caller is not the verified admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidStatus`   – the compact `C` lock is not active.
    /// * `InvalidAmount`   – Total remaining balance is zero (nothing to pay out).
    pub fn admin_override_cancel_release(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        let (meta, total_released) = Self::settle_all_remaining(&env, &admin, true)?;

        env.events().publish(
            (symbol_short!("adcovls"),),
            AdminCancelOverrideReleaseEvent {
                admin,
                freelancer: meta.freelancer,
                token: meta.token,
                total_released,
            },
        );

        Ok(())
    }

    /// Admin emergency override: resolve a cancel-locked escrow by refunding
    /// all remaining milestone funds to the client.
    ///
    /// Mirror of `admin_override_cancel_release`, but transfers funds back to
    /// the client rather than the freelancer.  Use this when the client is
    /// entitled to a full refund (e.g. no work was delivered).
    ///
    /// # Checks (in order)
    /// 1. `admin.require_auth()` — SDK-level signature check.
    /// 2. `require_admin` — verified admin key matches `DataKey::Admin`.
    /// 3. Contract must be initialised (`NotInitialized`).
    /// 4. Escrow must be funded (`NotFunded`).
    /// 5. The compact `C` lock must be active (`InvalidStatus`).
    ///
    /// # Effects
    /// - Every milestone in a non-terminal status is moved to `Refunded` and
    ///   its remaining balance is summed.
    /// - The total is transferred from the contract to the client in a single
    ///   token call.
    /// - The compact `C` lock entry is removed.
    /// - `YieldAccrued` is removed.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract not initialised.
    /// * `Unauthorized`    – Caller is not the verified admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidStatus`   – the compact `C` lock is not active.
    /// * `InvalidAmount`   – Total remaining balance is zero (nothing to refund).
    pub fn admin_override_cancel_refund(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        let (meta, total_refunded) = Self::settle_all_remaining(&env, &admin, false)?;

        env.events().publish(
            (symbol_short!("adcovrf"),),
            AdminCancelOverrideRefundEvent {
                admin,
                client: meta.client,
                token: meta.token,
                total_refunded,
            },
        );

        Ok(())
    }

    /// Shared body of `admin_override_cancel_release` (`to_freelancer = true`)
    /// and `admin_override_cancel_refund` (`to_freelancer = false`).
    ///
    /// Requires the admin and an active cancel lock, settles every
    /// non-terminal milestone's remaining balance as `Released` or `Refunded`,
    /// removes the `CancelLock` and `YieldAccrued` entries (so neither path
    /// leaves a stale entry paying rent), then pays the total to the freelancer
    /// or client. Negative `amount` / `released_amount` fields are rejected on
    /// both paths (issue #386). Returns the job metadata and the total paid.
    fn settle_all_remaining(
        env: &Env,
        admin: &Address,
        to_freelancer: bool,
    ) -> Result<(JobMeta, i128), Error> {
        Self::require_role(env, admin, AdminRole::Resolver)?;

        // Only valid when a cancel lock is active.
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if !cancel_locked {
            return Err(Error::InvalidStatus);
        }

        let meta = Self::load_job_meta(env)?;
        Self::require_escrow_has_funding(env, &meta)?;

        let settled_status = if to_freelancer {
            MilestoneStatus::Released
        } else {
            MilestoneStatus::Refunded
        };

        // Walk every milestone; accumulate remaining balance and mark settled.
        let mut total: i128 = 0;
        for index in 0..meta.milestone_count {
            let mut milestone = Self::load_milestone(env, index)?;
            if !Self::milestone_is_funded(&meta, &milestone) {
                continue;
            }
            if Self::is_terminal_status(&milestone.status) {
                continue;
            }
            if milestone.status == MilestoneStatus::Disputed {
                Self::assert_dispute_timeout_elapsed(env, index)?;
            }
            // Guard: a malformed entry with a negative amount or
            // released_amount (e.g. i128::MIN) could yield a nonsensical
            // positive `remaining`; reject it so every exit path either pays a
            // valid positive total or returns Error::InvalidAmount.
            if milestone.amount < 0 || milestone.released_amount < 0 {
                return Err(Error::InvalidAmount);
            }
            let remaining = milestone
                .amount
                .checked_sub(milestone.released_amount)
                .ok_or(Error::InvalidAmount)?;
            if remaining > 0 {
                total = total.checked_add(remaining).ok_or(Error::InvalidAmount)?;
                // `released_amount` counts what the freelancer was paid, so a
                // refund leaves it as it was (#688).
                if to_freelancer {
                    milestone.released_amount = milestone.amount;
                }
                milestone.status = settled_status.clone();
                // Only the persistent Milestone entry is written; the temporary
                // MilestoneReleased flag is redundant here (issue #383).
                Self::store_milestone(env, index, &milestone);
                Self::clear_time_ext(env, index);
            }
        }

        if total <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: clear the lock and yield before the external transfer. Only an
        // existing yield entry is removed, so escrows without one pay no write.
        env.storage().instance().remove(&DataKey::CancelLock);
        if env.storage().persistent().has(&DataKey::YieldAccrued) {
            env.storage().persistent().remove(&DataKey::YieldAccrued);
        }

        let token_client = token::Client::new(env, &meta.token);
        let recipient = if to_freelancer {
            &meta.freelancer
        } else {
            &meta.client
        };
        Self::pay_or_credit(env, &token_client, recipient, &total)?;

        Ok((meta, total))
    }

    /// Upgrade the contract's WASM to `new_wasm_hash`.
    ///
    /// # Business rules
    /// Caller authorization and pause/lock preconditions are checked before
    /// any storage mutation or WASM upgrade, so a rejected call leaves the
    /// contract's storage and installed code untouched.
    ///
    /// # Errors
    /// * `NotInitialized` – Admin key has never been stored.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    /// * `Paused`         – The contract is currently emergency-paused.
    /// * `EscrowLocked`   – A cancel is in progress and holds the cancel lock.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::Upgrader)?;
        Self::ensure_not_paused(&env)?;

        env.deployer()
            .update_current_contract(ContractExecutable::Wasm(new_wasm_hash.clone()));

        // Single read-modify-write: replaces the separate get + set with one
        // storage operation on DataKey::Version, reducing the key's ledger
        // footprint from two accesses to one.
        let new_version: u32 = env
            .storage()
            .instance()
            .try_update(&DataKey::Version, |v: Option<u32>| {
                v.unwrap_or(1).checked_add(1).ok_or(Error::InvalidAmount)
            })?;

        env.events().publish(
            (symbol_short!("upgrade"),),
            ContractUpgradedEvent {
                admin,
                new_wasm_hash,
                version: new_version,
            },
        );

        Ok(())
    }

    /// Migrate a deployment upgraded from the old storage layout (issue #772).
    ///
    /// Moves `WhitelistedTokens`, `MultiSigConfig`, `InterestYieldState` and
    /// `PlatformFeeAllocation` out of the contract-instance entry, which every
    /// invocation loads, into persistent keys read only by the functions that
    /// use them. A multisig regime still stored under the pre-#456
    /// `MultiSigSigners` / `MultiSigThreshold` keys is folded into a persistent
    /// `MultiSigConfig` on the way.
    ///
    /// Call it once, right after `upgrade` to this version. Until then the
    /// contract keeps working, because every cold-config read falls back to the
    /// instance copy; the migration only shrinks the instance entry. It is
    /// idempotent: a second call finds nothing to move.
    ///
    /// Returns the number of instance entries that were moved or folded.
    ///
    /// # Errors
    /// * `NotInitialized` – Admin key has never been stored.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    pub fn migrate_cold_config(env: Env, admin: Address) -> Result<u32, Error> {
        Self::require_role(&env, &admin, AdminRole::Upgrader)?;
        Ok(Self::move_cold_config(&env))
    }

    /// Bring storage written by an older version of this contract up to
    /// `SCHEMA_VERSION`.
    ///
    /// # Upgrade procedure
    /// 1. `upgrade(admin, new_wasm_hash)` installs the new code.  If the new
    ///    code's `SCHEMA_VERSION` is higher than the stored
    ///    `schema_version()`, every state-changing endpoint now returns
    ///    `MigrationRequired`, so nothing can write entries in the old shape
    ///    or misread them.  Read-only getters and `upgrade` itself still work.
    /// 2. `migrate(admin)` rewrites the stored entries one schema step at a
    ///    time and records `SCHEMA_VERSION`.  Normal operation resumes.
    ///
    /// Steps:
    /// * `1 → 2`: every `Milestone` stored without `spec_hash` gains a zero
    ///   `spec_hash` (#767), and the whitelist, multisig, yield and fee
    ///   configuration moves to persistent storage (#772, as
    ///   `migrate_cold_config`).  Milestones that already carry a `spec_hash`
    ///   (deployments created after #767 but before the schema marker) are
    ///   left as they are.
    /// * `2 → 3`: every `Milestone` stored without `deliverable` gains a zero
    ///   `deliverable`.
    ///
    /// Returns the new schema version and emits `("migrate",)` with a
    /// `MigratedEvent`.
    ///
    /// # Errors
    /// * `NotInitialized`  – Admin key has never been stored.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `AlreadyMigrated` – Storage is already at `SCHEMA_VERSION`.
    pub fn migrate(env: Env, admin: Address) -> Result<u32, Error> {
        Self::require_role(&env, &admin, AdminRole::Upgrader)?;
        let from_version = Self::read_schema_version(&env);
        if from_version >= SCHEMA_VERSION {
            return Err(Error::AlreadyMigrated);
        }

        if from_version < 3 {
            Self::migrate_milestones_to_v3(&env)?;
        }
        if from_version < 2 {
            Self::move_cold_config(&env);
        }

        env.storage()
            .instance()
            .set(&DataKey::SchemaVersion, &SCHEMA_VERSION);
        env.events().publish(
            (symbol_short!("migrate"),),
            MigratedEvent {
                admin,
                from_version,
                to_version: SCHEMA_VERSION,
            },
        );
        Ok(SCHEMA_VERSION)
    }

    /// Schema 1/2 → 3 for milestones: add a zero `spec_hash` (#767) and a
    /// zero `deliverable` to every entry stored without them.
    fn migrate_milestones_to_v3(env: &Env) -> Result<(), Error> {
        let meta = Self::load_job_meta(env)?;
        let persistent = env.storage().persistent();
        let spec_hash_field = Symbol::new(env, "spec_hash");
        let deliverable_field = Symbol::new(env, "deliverable");
        let zero = BytesN::from_array(env, &[0u8; 32]);
        for index in 0..meta.milestone_count {
            let key = DataKey::Milestone(index);
            // Decode the field map first: each older shape only decodes as
            // its own struct.
            let fields: Map<Symbol, Val> = persistent.get(&key).ok_or(Error::InvalidMilestone)?;
            if fields.contains_key(deliverable_field.clone()) {
                continue;
            }
            let migrated = if fields.contains_key(spec_hash_field.clone()) {
                let old: MilestoneV2 = persistent.get(&key).ok_or(Error::InvalidMilestone)?;
                Milestone {
                    amount: old.amount,
                    released_amount: old.released_amount,
                    status: old.status,
                    delivered_at: old.delivered_at,
                    funded: old.funded,
                    spec_hash: old.spec_hash,
                    deliverable: zero.clone(),
                }
            } else {
                let old: MilestoneV1 = persistent.get(&key).ok_or(Error::InvalidMilestone)?;
                Milestone {
                    amount: old.amount,
                    released_amount: old.released_amount,
                    status: old.status,
                    delivered_at: old.delivered_at,
                    funded: old.funded,
                    spec_hash: zero.clone(),
                    deliverable: zero.clone(),
                }
            };
            persistent.set(&key, &migrated);
        }
        Ok(())
    }

    /// Body of `migrate_cold_config`, shared with `migrate`.  Returns the
    /// number of instance entries that were moved or folded.
    fn move_cold_config(env: &Env) -> u32 {
        let mut moved = 0u32;
        if Self::migrate_cold_entry::<Vec<Address>>(env, &DataKey::WhitelistedTokens) {
            moved += 1;
        }
        if Self::migrate_cold_entry::<MultiSigConfig>(env, &DataKey::MultiSigConfig) {
            moved += 1;
        }
        if Self::migrate_cold_entry::<EscrowInterestYieldState>(env, &DataKey::InterestYieldState) {
            moved += 1;
        }
        if Self::migrate_cold_entry::<PlatformFeeAllocation>(env, &DataKey::PlatformFeeAllocation) {
            moved += 1;
        }

        let instance = env.storage().instance();
        let legacy_signers: Option<Vec<Address>> = instance.get(&DataKey::MultiSigSigners);
        let legacy_threshold: Option<u32> = instance.get(&DataKey::MultiSigThreshold);
        if let (Some(signers), Some(threshold)) = (legacy_signers, legacy_threshold) {
            let persistent = env.storage().persistent();
            if !persistent.has(&DataKey::MultiSigConfig) {
                persistent.set(
                    &DataKey::MultiSigConfig,
                    &MultiSigConfig(signers, threshold),
                );
            }
            instance.remove(&DataKey::MultiSigSigners);
            instance.remove(&DataKey::MultiSigThreshold);
            moved += 1;
        }

        moved
    }

    /// Freeze the escrow: set `DataKey::Ep`, blocking every
    /// endpoint guarded by `ensure_not_paused`.
    ///
    /// # Business rules
    /// Bad setups are rejected before any state is written, each with a
    /// distinct error variant:
    ///
    /// 1. The contract must be initialised — `load_job_meta` returns
    ///    `NotInitialized` when no job is stored.  Pausing an uninitialised
    ///    contract would write a flag no endpoint could ever clear.
    /// 2. Both parties must sign: the supplied addresses must match the job's
    ///    stored `client` and `freelancer` (`Unauthorized`), and each must
    ///    authorise the call (`require_auth`).  Neither party -- nor the admin
    ///    -- can freeze the escrow alone; the admin's unilateral path is
    ///    `emergency_pause_admin_override`.
    /// 3. No pause transition may already be mid-execution
    ///    (`EmergencyPauseInProgress`).
    /// 4. The contract must not already be paused (`AlreadyPaused`).  A
    ///    redundant pause previously succeeded silently, which let an operator
    ///    believe they had taken fresh action during an incident when the
    ///    freeze was in fact already in place.
    ///
    /// # Errors
    /// * `NotInitialized`            – No job has been stored.
    /// * `Unauthorized`              – `client` / `freelancer` do not match the
    ///                                 addresses recorded on the job.
    /// * `EmergencyPauseInProgress`  – A pause transition is already running.
    /// * `AlreadyPaused`             – The contract is already paused.
    pub fn emergency_pause(env: Env, client: Address, freelancer: Address) -> Result<(), Error> {
        let meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        Self::assert_emergency_pause_not_locked(&env)?;

        // `load_job_meta` above already proved the contract is initialized, so
        // the pause-flag read cannot miss here; `?` keeps the path honest
        // rather than defaulting a missing flag to `false`.
        if Self::load_emergency_paused(&env)? {
            return Err(Error::AlreadyPaused);
        }

        env.storage().instance().set(&DataKey::EpLk, &true);
        env.storage().instance().set(&DataKey::Ep, &true);
        env.storage().instance().set(&DataKey::EpParty, &true);
        env.storage().instance().set(&DataKey::EpLk, &false);

        env.events().publish(
            (symbol_short!("empause"),),
            EmergencyPausedEvent { client, freelancer },
        );

        Ok(())
    }

    /// Lift an emergency freeze, restoring normal operation.
    ///
    /// # Business rules
    /// Mirrors `emergency_pause`: the contract must be initialised, the
    /// caller must be the stored admin, no transition may be mid-execution,
    /// and the contract must actually be paused.  Unpausing a running
    /// contract is rejected with `NotPaused` rather than silently succeeding,
    /// so a mistaken call is visible to the operator instead of reading as a
    /// completed recovery.
    ///
    /// # Errors
    /// * `NotInitialized`            – Admin key has never been stored.
    /// * `Unauthorized`              – `admin` is not the stored admin.
    /// * `NotPaused`                 – The contract is not currently paused.
    /// * `EmergencyPauseInProgress`  – A pause transition is already running.
    pub fn emergency_unpause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        // Reject the illegal source state before any further ledger access
        // (including the transition lock) so a mistaken unpause never mutates
        // storage.  The stored-admin check above already proved the contract is
        // initialized, so the pause-flag read cannot miss here.
        if !Self::load_emergency_paused(&env)? {
            return Err(Error::NotPaused);
        }

        Self::assert_emergency_pause_not_locked(&env)?;

        // Single write — no external call inside the transition body, so no
        // reentrancy lock is needed (mirrors emergency_pause_admin_override).
        env.storage().instance().set(&DataKey::Ep, &false);
        env.storage().instance().set(&DataKey::EpParty, &false);

        env.events().publish(
            (symbol_short!("emunpause"),),
            EmergencyUnpausedEvent {
                admin: admin.clone(),
            },
        );

        Ok(())
    }

    /// Jointly lift a pause that the client and freelancer previously set.
    /// Cannot clear an admin pause (`EpParty` must be true).
    pub fn emergency_unpause_parties(
        env: Env,
        client: Address,
        freelancer: Address,
    ) -> Result<(), Error> {
        let meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        if !Self::load_emergency_paused(&env)? {
            return Err(Error::NotPaused);
        }
        let party_pause = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::EpParty)
            .unwrap_or(false);
        if !party_pause {
            return Err(Error::Unauthorized);
        }

        Self::assert_emergency_pause_not_locked(&env)?;
        env.storage().instance().set(&DataKey::Ep, &false);
        env.storage().instance().set(&DataKey::EpParty, &false);

        env.events().publish(
            (symbol_short!("emunpause"),),
            EmergencyPartyUnpausedEvent { client, freelancer },
        );
        Ok(())
    }

    /// Override the emergency pause status to paused.
    ///
    /// # Effects
    /// Sets the `DataKey::Ep` to `true`. Emits an `EmergencyPauseAdminOverrideEvent`.
    ///
    /// # Returns
    /// * `Ok(())` on a successful override of the emergency pause state.
    ///
    /// # Errors
    /// * `NotInitialized` - Admin key has never been stored.
    /// * `Unauthorized` - Caller is not the verified admin.
    /// * `AlreadyPaused` - The contract is already paused.
    pub fn emergency_pause_admin_override(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        // Read the current state once; reject no-op transitions so the caller
        // can distinguish a successful override from a mistaken duplicate call.
        let current = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::Ep)
            .unwrap_or(false);

        if current {
            return Err(Error::AlreadyPaused);
        }

        // Single write — no external call, no EmergencyPauseLock needed.
        env.storage().instance().set(&DataKey::Ep, &true);
        env.storage().instance().set(&DataKey::EpParty, &false);

        env.events().publish(
            (symbol_short!("emoverrid"),),
            EmergencyPauseAdminOverrideEvent {
                admin: admin.clone(),
                paused: true,
                previous: current,
            },
        );

        Ok(())
    }

    /// Report whether the emergency-pause flag (`DataKey::Ep`) is set.
    ///
    /// This is a **pure read**: it never writes to instance, persistent, or
    /// temporary storage, emits no events, and requires no authorisation, so
    /// any caller may invoke it at any time — including while the contract is
    /// itself emergency-paused.
    ///
    /// `initialize` writes `DataKey::Ep = false` as part of its state commit
    /// (see its rustdoc), so the key is present for every initialized contract
    /// and the reported flag is always the value last written by
    /// `emergency_pause`, `emergency_unpause`, or
    /// `emergency_pause_admin_override`.
    ///
    /// # Return values
    ///
    /// | State                                          | Return value |
    /// |------------------------------------------------|--------------|
    /// | `initialize` never completed successfully       | `Err(Error::NotInitialized)` |
    /// | Initialized, never paused                       | `Ok(false)` |
    /// | Initialized, frozen by `emergency_pause` / `emergency_pause_admin_override` | `Ok(true)` |
    /// | Initialized, released by `emergency_unpause`    | `Ok(false)` |
    ///
    /// # Errors
    /// * `NotInitialized` – `DataKey::Ep` is absent from instance storage,
    ///   which is the case only for contracts on which `initialize` has not
    ///   yet been successfully invoked.  Callers can therefore distinguish
    ///   "not paused" (`Ok(false)`) from "not set up yet"
    ///   (`Err(NotInitialized)`) instead of reading a defaulted `false` for
    ///   both.
    ///
    /// No other error is possible: the function takes no arguments, touches no
    /// token balance, and does not inspect the pause-transition lock
    /// (`DataKey::EpLk`) — a pause transition that is mid-execution in the
    /// same transaction is still reported by whatever value is currently
    /// stored.
    ///
    /// Read-only: exactly one instance-storage `get` on `DataKey::Ep`; the
    /// whole ledger is byte-identical before and after the call (enforced by
    /// `read_path_tests`).
    pub fn is_emergency_paused(env: Env) -> Result<bool, Error> {
        Self::load_emergency_paused(&env)
    }

    /// Pure-read inner implementation for `is_emergency_paused`.
    ///
    /// Extracted as a named private helper so that:
    /// * the public entry-point stays one line, making an accidental write
    ///   immediately obvious in diff review, and
    /// * the internal guards (`emergency_pause`, `emergency_unpause`,
    ///   `emergency_pause_claim_refund`) can share the same read path without
    ///   re-spelling the storage key or cloning `Env`.
    ///
    /// Returns `Err(Error::NotInitialized)` when the pause flag has never been
    /// written.  A single instance-storage read serves as both the
    /// initialization guard and the value lookup, so the function touches
    /// exactly one ledger entry.
    ///
    /// **This function must contain only read operations.**
    fn load_emergency_paused(env: &Env) -> Result<bool, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Ep)
            .ok_or(Error::NotInitialized)
    }

    /// Infallible read of the emergency-pause flag for internal guards
    /// (`ensure_not_paused`): absent → `false`, so a guard on an
    /// uninitialized contract falls through to its own initialization check.
    ///
    /// **This function must contain only read operations.**
    fn read_emergency_paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::Ep)
            .unwrap_or(false)
    }

    /// Sets the global platform fee allocation in basis points (BPS).
    ///
    /// # Effects
    /// Updates the `PlatformFeeAllocation` stored at `DataKey::PlatformFeeAllocation`.
    /// Sets the `DataKey::PlatformFeeAllocationLock` to prevent re-entrant or concurrent updates.
    /// Emits a `PlatformFeeAllocationSetEvent`.
    ///
    /// ## Storage-footprint note
    ///
    /// This function uses `require_admin_from_instance` rather than the
    /// standard `require_admin` helper so that the admin verification read
    /// (`DataKey::Admin`, instance) and all subsequent instance reads/writes
    /// (`PlatformFeeAllocationLock`, `PlatformFeeAllocation`, `EpLk`) touch
    /// the **same single ledger entry** (instance storage) instead of two
    /// (persistent + instance).
    ///
    /// # Returns
    /// * `Ok(())` on a successful update of the platform fee allocation.
    ///
    /// # Errors
    /// * `NotInitialized` - Admin key has never been stored.
    /// * `Unauthorized` - Caller is not the verified admin.
    /// * `PlatformFeeAllocationInProgress` - A platform fee update lock is currently active.
    /// * `EmergencyPauseInProgress` - An emergency pause lock is currently active.
    /// * `InvalidRatio` - The sum of the BPS values does not equal 10,000 (BPS_SCALE).
    /// * `FeeTooHigh` - The treasury or client BPS exceeds the maximum allowed limits.
    /// * `InvalidStatus` - The allocation is locked.
    pub fn set_platform_fee_allocation(
        env: Env,
        admin: Address,
        client_bps: u32,
        freelancer_bps: u32,
        treasury_bps: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Both the Admin read and all subsequent instance reads/writes touch
        // instance storage only, so the whole function uses a single ledger entry.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        // Precondition: validate illegal source state (locked allocation) before any ledger write
        // Ensures InvalidStatus is returned with no storage mutation when allocation is locked
        if let Ok(current) = Self::load_platform_fee_allocation(&env) {
            if current.locked {
                return Err(Error::InvalidStatus);
            }
        }
        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &true);

        let result = (|| {
            let current: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;

            if current.locked {
                return Err(Error::InvalidStatus);
            }

            env.storage().persistent().set(
                &DataKey::PlatformFeeAllocation,
                &PlatformFeeAllocation {
                    client_bps,
                    freelancer_bps,
                    treasury_bps,
                    locked: false,
                },
            );

            // Emit a structured event so downstream indexers can track
            // every platform-fee configuration change without polling storage.
            env.events().publish(
                (symbol_short!("pf_set"),),
                PlatformFeeAllocationSetEvent {
                    admin: admin.clone(),
                    client_bps,
                    freelancer_bps,
                    treasury_bps,
                },
            );

            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &false);

        result
    }

    /// Lock the current platform-fee allocation, preventing further non-override
    /// modifications.  The admin's configuration (client_bps, freelancer_bps,
    /// treasury_bps) is persisted with `locked = true` so that subsequent calls
    /// to non-override endpoints are rejected until an admin override clears the
    /// lock.
    ///
    /// # Returns
    /// * `Ok(())` – the allocation was successfully locked.
    /// * `Err(Error::Unauthorized)` – the caller is not the contract admin.
    /// * `Err(Error::PlatformFeeAllocationInProgress)` – a platform-fee
    ///   allocation is already locked.
    /// * `Err(Error::EmergencyPauseInProgress)` – an emergency-pause transition
    ///   is active.
    /// * `Err(Error::NotInitialized)` – the contract has not been initialized.
    ///
    /// # Errors
    /// * `Error::Unauthorized` – caller is not the admin (via `require_admin`).
    /// * `Error::PlatformFeeAllocationInProgress` – lock is already set (via
    ///   `assert_platform_fee_allocation_not_locked`).
    /// * `Error::EmergencyPauseInProgress` – emergency pause is in progress (via
    ///   `assert_emergency_pause_not_locked`).
    /// * `Error::NotInitialized` – no platform-fee allocation exists (inside the
    ///   execution lock guard).
    pub fn lock_platform_fee_allocation(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Perform all authorization and precondition checks before any ledger writes
        // so unauthorized callers and illegal source states are rejected without
        // mutating any storage entry.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;

        // Load the current allocation early to validate it exists
        let mut current: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;
        // Locking twice is a mistake worth surfacing, not a silent no-op.
        if current.locked {
            return Err(Error::InvalidStatus);
        }

        // All checks passed; now proceed with mutation under re-entrancy guard
        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &true);

        let result = (|| {
            // Emit a structured event so downstream indexers can track
            // lock state changes without polling storage.
            env.events().publish(
                (symbol_short!("pf_lock"),),
                PlatformFeeAllocationLockedEvent {
                    admin: admin.clone(),
                    client_bps: current.client_bps,
                    freelancer_bps: current.freelancer_bps,
                    treasury_bps: current.treasury_bps,
                },
            );

            current.locked = true;
            env.storage()
                .persistent()
                .set(&DataKey::PlatformFeeAllocation, &current);
            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &false);

        result
    }

    /// Admin override that replaces a *locked* platform-fee allocation and
    /// unlocks it in the same step.
    ///
    /// Storage footprint: the call writes exactly one instance-storage map
    /// key, `DataKey::PlatformFeeAllocation`. Unlike `set_platform_fee_allocation`
    /// / `lock_platform_fee_allocation`, it does not take the
    /// `PlatformFeeAllocationLock` re-entrancy guard: the body performs a
    /// single unconditional `set` with no external or cross-contract calls
    /// between validation and the write, so there is no re-entrancy window to
    /// protect, and omitting the guard's set-true / set-false pair keeps the
    /// invocation to a single written key.
    pub fn pf_alloc_admin_override(
        env: Env,
        admin: Address,
        client_bps: u32,
        freelancer_bps: u32,
        treasury_bps: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        let current: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;

        if !current.locked {
            return Err(Error::InvalidStatus);
        }

        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;

        env.storage().persistent().set(
            &DataKey::PlatformFeeAllocation,
            &PlatformFeeAllocation {
                client_bps,
                freelancer_bps,
                treasury_bps,
                locked: false,
            },
        );

        // Emit a structured event so downstream indexers can track
        // admin override changes without polling storage.
        env.events().publish(
            (symbol_short!("pf_ovr"),),
            PlatformFeeAllocationOverrideEvent {
                admin: admin.clone(),
                client_bps,
                freelancer_bps,
                treasury_bps,
                locked: false,
            },
        );

        Ok(())
    }

    /// Return the current platform-fee allocation (persistent storage).
    ///
    /// This is a **pure read** — it never writes to any storage tier, emits no
    /// events, and performs no authentication checks.  Any caller may invoke it
    /// at any time, including while the contract is emergency-paused.
    ///
    /// # Return values
    ///
    /// | State | Return value |
    /// |-------|-------------|
    /// | Contract not yet initialized (`initialize` never called) | `Err(Error::NotInitialized)` |
    /// | After `initialize`, before `set_platform_fee_allocation` | `Ok(PlatformFeeAllocation { client_bps: 0, freelancer_bps: 10_000, treasury_bps: 0, locked: false })` |
    /// | After `set_platform_fee_allocation` or `pf_alloc_admin_override` | `Ok(PlatformFeeAllocation { …, locked: false })` |
    /// | After `lock_platform_fee_allocation` | `Ok(PlatformFeeAllocation { …, locked: true })` |
    ///
    /// `initialize` writes a default allocation of
    /// `{ client_bps: 0, freelancer_bps: 10_000, treasury_bps: 0, locked: false }`
    /// to instance storage, so the key is always present once the contract is
    /// initialized.  The only way to get `Err(NotInitialized)` is to call this
    /// function before `initialize` has ever succeeded.
    ///
    /// For every `Ok` variant the three basis-point fields satisfy
    /// `client_bps + freelancer_bps + treasury_bps == 10_000` (= `BPS_SCALE`);
    /// this invariant is enforced by `initialize`, `set_platform_fee_allocation`,
    /// and `pf_alloc_admin_override` before writing, so it always holds for
    /// any value this function can return.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotInitialized`] (code 2) when
    /// `DataKey::PlatformFeeAllocation` is absent from storage, which
    /// is the case only for contracts on which `initialize` has not yet been
    /// successfully invoked.
    ///
    /// No other error is possible: the function does not validate its
    /// arguments, touch token balances, check pause state, or require any
    /// authorisation.
    ///
    /// INVARIANT: this function must never write to instance, persistent, or
    /// temporary storage (directly or transitively), and must never emit
    /// events. It exists purely to expose the current allocation to callers.
    /// A regression test (`platform_fee_allocation_no_mutation_tests`) takes
    /// a full ledger snapshot before and after invoking this function and
    /// asserts the two are identical, so any future edit that introduces a
    /// storage write here will fail CI. Do not add `.set(`, `.remove(`,
    /// `.extend_ttl(`, or `env.events().publish(` calls to this function or
    /// to `Self::load_platform_fee_allocation`.
    ///
    /// The `Err(Error::NotInitialized)` row of the table above is pinned by
    /// `get_platform_fee_allocation_not_initialized_tests`: that suite asserts a
    /// freshly registered contract — `initialize` never called — receives this
    /// *typed* contract error (code 2) from `get_platform_fee_allocation`, from
    /// `calculate_platform_fee_split`, and from the `set` / `lock` / override
    /// writers. It distinguishes the typed error from a host trap and from a
    /// defaulted allocation, so a future edit that replaced the storage miss
    /// with an `unwrap()`/`unwrap_or_default()` would fail CI rather than
    /// silently hand callers a fee split the admin never configured.
    pub fn get_platform_fee_allocation(env: Env) -> Result<PlatformFeeAllocation, Error> {
        Self::load_platform_fee_allocation(&env)
    }

    /// Split an amount according to the configured platform-fee ratios.
    ///
    /// Each component is calculated with checked integer arithmetic. Any
    /// units left after flooring are assigned by largest remainder, so the
    /// three returned amounts always sum exactly to `total_amount`.
    ///
    /// # Overflow safety
    ///
    /// Every `i128` step performed by `Self::allocate_platform_fee` is a
    /// `checked_*` counterpart, so no arithmetic in this function can panic
    /// (the release profile sets `overflow-checks = true` and `panic = "abort"`,
    /// which would abort the whole transaction) or silently wrap into a
    /// negative, nonsensical split.
    ///
    /// Because `total_amount` is fully caller-supplied, the intermediate
    /// `total_amount * bps` products are reachable overflow candidates — a
    /// total of `i128::MAX` multiplied by any non-zero basis-point weight
    /// exceeds `i128::MAX`. Such inputs are rejected with
    /// [`Error::ArithmeticOverflow`] instead.
    ///
    /// # Atomicity
    ///
    /// The endpoint is a **pure calculation**: it reads the allocation, returns
    /// the distribution, and performs no storage write. The `pf_split` event is
    /// published strictly *after* every arithmetic step has succeeded, so a
    /// rejected input leaves the ledger byte-for-byte unchanged — no event, no
    /// storage mutation, and no partially-populated `PlatformFeeDistribution`
    /// is ever observed. A regression test
    /// (`platform_fee_split_overflow_tests`) snapshots the whole ledger before
    /// and after each `i128::MAX` / `i128::MIN` invocation and asserts the two
    /// snapshots are identical.
    ///
    /// # Errors
    ///
    /// * `NotInitialized`      – the contract has not been initialised, so no
    ///   allocation is stored.
    /// * `InvalidAmount`       – `total_amount` is negative (but not
    ///   `i128::MIN`).
    /// * `ArithmeticOverflow`  – `total_amount` is `i128::MIN`, or any
    ///   intermediate value is not representable as an `i128`.
    pub fn calculate_platform_fee_split(
        env: Env,
        total_amount: i128,
    ) -> Result<PlatformFeeDistribution, Error> {
        let allocation: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;
        let distribution = Self::allocate_platform_fee(total_amount, &allocation)?;

        // Emit a structured event so downstream indexers can audit the
        // per-party split without re-querying contract storage.
        env.events().publish(
            (symbol_short!("pf_split"),),
            PlatformFeeSplitCalculatedEvent {
                total_amount,
                client_amount: distribution.client_amount,
                freelancer_amount: distribution.freelancer_amount,
                treasury_amount: distribution.treasury_amount,
            },
        );

        Ok(distribution)
    }

    /// Calculate a nearest-rounded split of a total between a streamed payout
    /// and a client refund.
    ///
    /// # Authorization and preconditions
    /// The guard block runs at the top of this function, before any ledger
    /// entry is read or written by the calculation itself:
    ///
    /// * The escrow must be initialised (`Self::require_initialized`), so the
    ///   split is anchored to a real job rather than a fresh instance.
    /// * No other payment-streaming split may be mid-execution and the escrow
    ///   must be neither administratively nor emergency paused.
    /// * Both the client and the freelancer recorded on the job must sign
    ///   (`Self::require_client_and_freelancer_consent`), so a
    ///   single-signature attempt reverts before the split arithmetic runs.
    ///
    /// # Returns
    /// `RatioSplit.first` is the streamed payout; `RatioSplit.second` is the
    /// client refund. The two values sum exactly to `total_amount`.
    ///
    /// # Errors
    /// * `NotInitialized` – The contract has never been initialised, so there
    ///   is no job metadata to anchor the split to.
    /// * `PaymentStreamingInProgress` – Another payment-streaming split is
    ///   mid-execution.
    /// * `Paused` – The escrow is administratively or emergency paused.
    /// * `InvalidAmount` – `total_amount` is not positive, or checked
    ///   arithmetic overflows while calculating the split.
    /// * `InvalidRatio` – `denominator` is not positive, or `numerator` is
    ///   outside `0..=denominator`.
    ///
    /// Authentication, preconditions and input validation all complete before
    /// the execution lock is written, so rejected calls leave no storage entry
    /// or event behind.
    pub fn payment_streaming_milestones(
        env: Env,
        total_amount: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        Self::ensure_migrated(&env)?;
        // Precondition: the escrow must be initialized so the split is anchored
        // to a real job rather than a fresh or uninitialized instance. This is
        // the same guard used by every other mutating endpoint.
        Self::require_initialized(&env)?;

        // Reject illegal source states — a split already mid-execution or a
        // paused escrow — before any further ledger access, so a mistaken
        // call never mutates storage.
        Self::assert_payment_streaming_not_locked(&env)?;
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Authorization and precondition checks run BEFORE any ledger entry is
        // written. Both the client and the freelancer must have signed the
        // transaction, so a single-signature attempt can never reach the split
        // arithmetic and no state is mutated.
        let _meta = Self::require_client_and_freelancer_consent(&env)?;

        // Reject invalid inputs BEFORE the execution lock is acquired, so a
        // rejected invocation writes no ledger entry and publishes no event.
        Self::validate_streaming_ratio(total_amount, numerator, denominator)?;
        let split = Self::split_round_nearest(total_amount, numerator, denominator)?;

        // Acquire execution lock so concurrent state mutations observe the
        // in-progress status while the successful calculation is published.
        env.storage()
            .instance()
            .set(&DataKey::PaymentStreamingExecutionLock, &true);

        env.events().publish(
            (symbol_short!("p_stream"),),
            PaymentStreamingEvent {
                total_amount,
                numerator,
                denominator,
                streamed_payout: split.first,
                client_refund: split.second,
            },
        );

        env.storage()
            .instance()
            .remove(&DataKey::PaymentStreamingExecutionLock);

        Ok(split)
    }

    /// Stream a partial payment from a milestone's escrowed balance to the
    /// freelancer with **dual consent**: both the client and the freelancer
    /// must independently sign the transaction.
    ///
    /// Unlike `payment_streaming_milestones` which is a pure calculator, this
    /// endpoint actually transfers tokens: `numerator/denominator` of the
    /// milestone's remaining balance is released to the freelancer, and the
    /// rest stays in the escrow.  The milestone transitions to
    /// `PartiallyReleased` (or `Released` if the full amount is streamed).
    ///
    /// # Parameters
    /// * `milestone_index` – Index of the milestone to stream from.
    /// * `numerator`        – Streamed portion; must satisfy 0 ≤ n ≤ denominator.
    /// * `denominator`      – Ratio denominator; must be > 0.
    ///
    /// # Returns
    /// A `RatioSplit` where `first` is the streamed payout and `second` is the
    /// remaining balance.  The two always sum to the remaining amount exactly.
    ///
    /// # Errors
    /// * `NotInitialized`  – Job metadata missing, so no signers are known.
    /// * `Paused`           – The escrow is administratively or emergency paused.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – Milestone is not `Delivered` or `PartiallyReleased`.
    /// * `InvalidAmount`    – Remaining balance ≤ 0, or arithmetic overflow.
    /// * `InvalidRatio`     – `denominator` ≤ 0, or `numerator` outside
    ///   `0..=denominator`.
    pub fn payment_streaming_consent(
        env: Env,
        milestone_index: u32,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        Self::ensure_migrated(&env)?;
        // Reject illegal source state before any further ledger access.
        Self::assert_payment_streaming_not_locked(&env)?;
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Collect both signatures; returns NotInitialized before any
        // lock write if Job metadata is missing.
        let meta = Self::require_client_and_freelancer_consent(&env)?;

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Validate ratio inputs before touching storage.
        Self::validate_streaming_ratio(remaining, numerator, denominator)?;

        env.storage()
            .instance()
            .set(&DataKey::PaymentStreamingExecutionLock, &true);

        let result = (|| {
            let split = Self::split_round_nearest(remaining, numerator, denominator)?;

            let token_client = token::Client::new(&env, &meta.token);
            let contract_addr = env.current_contract_address();

            if split.first > 0 {
                token_client.transfer(&contract_addr, &meta.freelancer, &split.first);
            }

            milestone.released_amount = milestone
                .released_amount
                .checked_add(split.first)
                .ok_or(Error::InvalidAmount)?;

            if milestone.released_amount >= milestone.amount {
                milestone.status = MilestoneStatus::Released;
                Self::store_milestone_released(&env, milestone_index);
                Self::increment_reputation(&env, &meta.client);
                Self::increment_reputation(&env, &meta.freelancer);
            } else {
                milestone.status = MilestoneStatus::PartiallyReleased;
            }

            Self::store_milestone(&env, milestone_index, &milestone);

            env.events().publish(
                (symbol_short!("p_strcns"),),
                PaymentStreamingConsentEvent {
                    milestone_index,
                    client: meta.client.clone(),
                    freelancer: meta.freelancer.clone(),
                    token: meta.token.clone(),
                    total_amount: remaining,
                    numerator,
                    denominator,
                    streamed_payout: split.first,
                    client_refund: split.second,
                },
            );

            Ok(split)
        })();

        env.storage()
            .instance()
            .remove(&DataKey::PaymentStreamingExecutionLock);

        result
    }

    /// Allocate a milestone's escrowed amount between two parties (typically
    /// client and freelancer) using a high-precision ratio that reflects how
    /// much of the extended deadline has been used.
    ///
    /// # Design
    /// When a client extends a milestone's auto-release deadline, the elapsed
    /// portion of the extended window can be used to derive a fair split of the
    /// milestone amount:
    ///
    /// ```text
    /// freelancer_share = round_nearest(amount × elapsed_seconds / total_seconds)
    /// client_refund    = amount − freelancer_share
    /// ```
    ///
    /// The arithmetic uses `split_round_nearest` which adds `denominator/2`
    /// before the final division so that the freelancer receives the rounded
    /// share rather than always the floor, preventing systematic value loss
    /// through repeated rounding.  The two halves always sum to `amount` exactly.
    ///
    /// While the split runs, `TimeExtExecutionLock` is held so
    /// concurrent state-modifying endpoints observe the in-progress status and
    /// reject rather than interleave mutations.
    ///
    /// # Parameters
    /// * `amount`           – Total escrowed amount to split.  Must be ≥ 0.
    ///                        Zero is allowed (returns two zeros).
    /// * `elapsed_seconds`  – Time already elapsed in the extension window.
    ///                        Must satisfy 0 ≤ elapsed_seconds ≤ total_seconds.
    /// * `total_seconds`    – Full length of the extension window.  Must be > 0.
    ///
    /// # Returns
    /// A `RatioSplit` where:
    /// * `first`  = freelancer portion (rounded to nearest stroop)
    /// * `second` = client refund (remainder, guarantees first + second == amount)
    ///
    /// # Errors
    /// * `InvalidAmount`  – `amount` is negative, or an intermediate checked
    ///                      multiplication overflows.
    /// * `InvalidRatio`   – `total_seconds` is zero, `elapsed_seconds` is
    ///                      negative, or `elapsed_seconds > total_seconds`.
    pub fn milestone_time_extensions(
        env: Env,
        amount: i128,
        elapsed_seconds: i128,
        total_seconds: i128,
    ) -> Result<RatioSplit, Error> {
        Self::ensure_migrated(&env)?;
        // Precondition: the escrow must be initialized so the split is anchored
        // to a real job rather than a fresh or uninitialized instance. This is
        // the same guard used by every other mutating endpoint.
        Self::require_initialized(&env)?;

        // Authorization and precondition checks run BEFORE any ledger entry is
        // read or written. Both the client and the freelancer must have signed
        // the transaction, so a single-signature attempt can never reach the
        // split arithmetic and no state is mutated.
        let _meta = Self::require_client_and_freelancer_consent(&env)?;

        Self::time_extension_split(&env, amount, elapsed_seconds, total_seconds)
    }

    /// The split behind `milestone_time_extensions`, without its
    /// authorization. `admin_override_streaming_release` calls this directly
    /// after its own admin check: an override must not need the signatures of
    /// the client and freelancer it overrides.
    fn time_extension_split(
        env: &Env,
        amount: i128,
        elapsed_seconds: i128,
        total_seconds: i128,
    ) -> Result<RatioSplit, Error> {
        // Reject illegal source states BEFORE the execution lock is acquired, so
        // an invalid amount or time range can never observe or mutate on-chain
        // state. These are validation-only errors and require no ledger write.
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if total_seconds <= 0 {
            return Err(Error::InvalidRatio);
        }
        if elapsed_seconds < 0 || elapsed_seconds > total_seconds {
            return Err(Error::InvalidRatio);
        }

        env.storage()
            .instance()
            .set(&DataKey::TimeExtExecutionLock, &true);

        let result = (|| {
            // Delegate to the single shared high-precision split primitive.
            // split_round_nearest(total, numerator, denominator) computes:
            //   first  = round_nearest(total × numerator / denominator)
            //   second = total − first
            // Here numerator = elapsed_seconds, denominator = total_seconds.
            let split = Self::split_round_nearest(amount, elapsed_seconds, total_seconds)?;

            env.events().publish(
                (symbol_short!("m_ext"),),
                MilestoneTimeExtensionEvent {
                    amount,
                    elapsed_seconds,
                    total_seconds,
                    freelancer_share: split.first,
                    client_refund: split.second,
                },
            );

            Ok(split)
        })();

        env.storage()
            .instance()
            .remove(&DataKey::TimeExtExecutionLock);

        result
    }

    /// Extend the auto-release deadline for a Delivered milestone with
    /// **dual consent**: both the client and the freelancer must independently
    /// sign the transaction.
    ///
    /// Unlike `extend_milestone_deadline` which requires only the client's
    /// signature, this endpoint requires both parties' auth.  When both
    /// parties agree to an extension, the cumulative extension is written to
    /// `DataKey::TimeExt(milestone_index)` so that `time_until_auto_release`
    /// and `claim_auto_release` reflect the new deadline.
    ///
    /// # Parameters
    /// * `milestone_index` – Index of the milestone to extend.
    /// * `extra_seconds`   – Additional seconds to add to the deadline. Must
    ///                        be > 0.
    ///
    /// # Errors
    /// * `NotInitialized`    – Job metadata missing, so no signers are known.
    /// * `Paused`            – The escrow is administratively or emergency paused.
    /// * `InvalidMilestone`  – `milestone_index` is out of range.
    /// * `InvalidStatus`     – Milestone is not `Delivered` or `PartiallyReleased`.
    /// * `InvalidExtension`   – `extra_seconds` is 0, or the cumulative
    ///                          extension overflows `u32`.
    pub fn time_extensions_consent(
        env: Env,
        milestone_index: u32,
        extra_seconds: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Reject illegal source state before collecting signatures so a
        // mistaken call never mutates storage.
        Self::assert_not_paused(&env)?;
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Collect both signatures; returns NotInitialized before any
        // lock write if Job metadata is missing.
        let meta = Self::require_client_and_freelancer_consent(&env)?;

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        if extra_seconds == 0 {
            return Err(Error::InvalidExtension);
        }

        let current_extension = Self::load_time_extension(&env, milestone_index);
        let new_extension = current_extension
            .checked_add(extra_seconds)
            .ok_or(Error::InvalidExtension)?;
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let new_deadline = Self::auto_release_deadline(&meta, delivered_at, new_extension)?;

        env.storage()
            .persistent()
            .set(&DataKey::TimeExt(milestone_index), &new_extension);

        env.events().publish(
            (symbol_short!("m_extcns"),),
            TimeExtConsentEvent {
                milestone_index,
                client: meta.client,
                freelancer: meta.freelancer,
                extra_seconds,
                new_extension,
                new_deadline,
            },
        );

        Ok(())
    }

    /// Split `total_amount` into per-party shares without changing contract state.
    ///
    /// The result uses the largest-remainder (Hare quota) method. This
    /// calculation performs no authorization, storage writes, token transfers,
    /// or event publication. Callers must separately execute any transfers.
    ///
    /// # Parameters
    /// * `total_amount` – Amount to split.  Must be > 0.
    /// * `ratios`       – Per-party weights, index-aligned with the returned
    ///                    vector.  Only their relative sizes matter, so they
    ///                    need not be normalised.  Must be non-empty, at most
    ///                    `MAX_MULTISIG_RATIO_COUNT` (255) entries long, with
    ///                    no negative entry, and must sum to > 0 without
    ///                    overflowing `i128`.
    ///
    /// # Returns
    /// `Ok(allocations)` — a `Vec<i128>` with **exactly `ratios.len()`
    /// entries, index-aligned with `ratios`**, where `allocations[i]` is party
    /// `i`'s share of `total_amount`:
    ///
    /// * **Conservation** – `Σ allocations == total_amount` exactly, for every
    ///   valid input.  No value is lost and none is created.
    /// * **Non-negative** – every entry is ≥ 0.  A party weighted `0` receives
    ///   exactly `0`.
    /// * **Bounded error** – entry `i` is either
    ///   `floor(total_amount × ratios[i] / Σratios)` or that floor `+ 1`, so a
    ///   party is never rounded below its floor share.  The indivisible
    ///   residue units (at most `ratios.len() − 1` of them) go to the parties
    ///   with the largest fractional remainders.
    /// * **Determinism** – remainder ties are broken by lowest index, so
    ///   identical inputs always produce the identical vector.
    ///
    /// # Validation order
    /// Checks run in the order below and a call violating more than one fails
    /// with the first match:
    ///
    /// 1. `total_amount` ≤ 0                → `InvalidAmount`
    /// 2. `ratios` empty                    → `InvalidRatio`
    /// 3. `ratios.len()` > 255              → `TooManyParties`
    /// 4. negative entry, overflowing sum, or `Σratios` ≤ 0 → `InvalidRatio`
    /// 5. allocation arithmetic overflow    → `InvalidAmount`
    ///
    /// # Errors
    /// * `InvalidAmount`  – `total_amount` ≤ 0; or an `i128` checked operation
    ///   inside the allocation maths (weighted product, running base sum, residue
    ///   subtraction, per-entry increment) is unrepresentable.
    /// * `InvalidRatio`   – `ratios` is empty; any entry is negative; `Σratios`
    ///   overflows `i128`; or `Σratios` ≤ 0 (every entry zero).
    /// * `TooManyParties` – `ratios` is longer than `MAX_MULTISIG_RATIO_COUNT`.
    ///
    /// Arithmetic overflow maps to `InvalidAmount` / `InvalidRatio`.
    pub fn compute_ratio_allocation(
        env: Env,
        total_amount: i128,
        ratios: Vec<i128>,
    ) -> Result<Vec<i128>, Error> {
        if ratios.len() > MAX_MULTISIG_RATIO_COUNT {
            return Err(Error::TooManyParties);
        }
        math::allocate_weights(&env, total_amount, &ratios)
    }

    // ── multisig approval: storage-optimised key design ────────────────────
    //
    // Design rationale
    // ─────────────────
    // Traditional multisig implementations store approval state as individual
    // `(Address, ProposalId) → bool` entries, which is expensive on Soroban
    // because each Address contributes ~32 bytes to the ledger key footprint.
    //
    // This implementation uses three optimisations to minimise bytes stored:
    //
    // 1. **Signer set and threshold are stored once** (persistent storage) in a
    //    single consolidated `MultiSigConfig` entry — rather than one key per
    //    signer, and rather than the two separate `MultiSigSigners` /
    //    `MultiSigThreshold` keys used before issue #456.
    //
    // 2. **Approval tracking uses a compact u32 bitmap**, one bit per signer
    //    by its index in the signers vec, eliminating the Address overhead
    //    from every approval entry.  Up to 32 signers are supported per
    //    proposal.
    //
    // 3. **The bitmap lives inside the proposal record** —
    //    `AdminProposal(proposal_id)`, written by `propose_admin_transfer`
    //    (issue #785) — beside the `new_admin`, creation timestamp and
    //    lifecycle status it belongs to.  That placement is what binds an
    //    approval to one specific live proposal instead of to a free-floating
    //    ID: the record must exist and be `Pending` before any bit can be
    //    set, and executing or cancelling clears the bits in the same write
    //    that consumes the record.  The former temporary
    //    `MultiSigApproval(proposal_id)` entry is no longer written (its
    //    `DataKey` variant is kept only so existing discriminants stay
    //    stable): a temporary entry could be evicted mid-flight, silently
    //    dropping collected approvals, and it carried no tie to the
    //    proposal's contents.
    //
    // Issue #457 audited the *write* path of `multisig_approve` in the
    // temporary-bitmap layout and found one redundant storage access of its
    // day — an unconditional bitmap write that re-wrote an unchanged value on
    // every duplicate approval.  The fix survives the move to the record: a
    // duplicate approval writes no contract storage key at all.  See
    // `multisig_approve`.

    const MAX_MULTISIG_SIGNERS: u32 = 32;

    /// Validates multisig signer list and approval threshold before persistence.
    ///
    /// Rejects empty or oversized signer sets, thresholds outside `1..=signer_count`,
    /// and duplicate signer addresses.
    fn validate_multisig_setup(signers: &Vec<Address>, threshold: u32) -> Result<(), Error> {
        let count = signers.len();
        if count == 0 {
            return Err(Error::MultiSigNoSigners);
        }
        if count > Self::MAX_MULTISIG_SIGNERS {
            return Err(Error::MultiSigTooManySigners);
        }
        if threshold == 0 || threshold > count {
            return Err(Error::MultiSigInvalidThreshold);
        }

        let mut i = 0u32;
        while i < count {
            let mut j = i + 1;
            while j < count {
                let a = signers.get(i).ok_or(Error::MultiSigNoSigners)?;
                let b = signers.get(j).ok_or(Error::MultiSigNoSigners)?;
                if a == b {
                    return Err(Error::MultiSigDuplicateSigner);
                }
                j += 1;
            }
            i += 1;
        }

        Ok(())
    }

    /// Load the multisig signer set and threshold.
    ///
    /// Reads the consolidated `DataKey::MultiSigConfig` entry (issue #456).
    /// A contract initialised before that change and then upgraded still holds
    /// the two halves under the legacy `MultiSigSigners` / `MultiSigThreshold`
    /// keys, so those are read as a fallback; without it such a contract would
    /// report `NotInitialized` for an existing multisig regime.  The legacy
    /// keys live in the contract-instance entry, which every call loads
    /// anyway, so the fallback adds no ledger entry to the footprint.
    /// `migrate_cold_config` folds them into the persistent
    /// `MultiSigConfig` (issue #772).
    ///
    /// **This function must contain only read operations.**
    fn load_multisig_config(env: &Env) -> Option<MultiSigConfig> {
        if let Some(config) = Self::cold_get(env, &DataKey::MultiSigConfig) {
            return Some(config);
        }
        let instance = env.storage().instance();
        let signers: Vec<Address> = instance.get(&DataKey::MultiSigSigners)?;
        let threshold: u32 = instance.get(&DataKey::MultiSigThreshold)?;
        Some(MultiSigConfig(signers, threshold))
    }

    /// Initialise a multisig approval regime with a fixed set of signers and
    /// the required approval threshold.  Must be called exactly once.
    ///
    /// On success the call publishes a `MultisigApprovalInitEvent` under the
    /// `msiginit` topic carrying the acting admin and the values that were
    /// actually persisted.  A rejected call (`NotInitialized` /
    /// `Unauthorized` / `AlreadyInitialized` / `MultiSig*`) publishes nothing.
    ///
    /// # Storage footprint (issue #456)
    /// Two reductions, both measured by
    /// `multisig_approval_init_footprint_tests`:
    ///
    /// 1. The signer set and the threshold are persisted as **one**
    ///    `DataKey::MultiSigConfig` entry, so the call writes a single distinct
    ///    multisig storage key instead of the two separate `MultiSigSigners` /
    ///    `MultiSigThreshold` entries used before, and the contract-instance
    ///    ledger entry ends up 28 bytes smaller for a three-signer set.  The
    ///    already-initialised guard probes that same consolidated key — it is
    ///    the only key this call writes, so it *is* the initialisation
    ///    condition.
    /// 2. Authorization uses `require_admin_from_instance` rather than
    ///    `require_admin`, so the admin verification read lands on the same
    ///    instance entry as every other access of this call instead of adding
    ///    the persistent `Admin` entry to the invocation footprint — the same
    ///    consolidation already landed for `multisig_lock` (#460),
    ///    `set_escrow_interest_yield` (#463), `set_platform_fee_allocation`
    ///    (#472) and `admin_resume_escrow` (#449).  `initialize` writes both
    ///    `Admin` copies atomically and every admin-transfer path keeps them in
    ///    sync, so the check is logically identical; a contract whose only
    ///    `Admin` copy is the instance one is accepted, exactly like those
    ///    endpoints.
    pub fn multisig_approval_init(
        env: Env,
        admin: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Either layout means the regime already exists: a contract upgraded
        // from before issue #456 still holds its signer set under the legacy
        // `MultiSigSigners` key, and must not be re-initialisable.
        if Self::cold_has(&env, &DataKey::MultiSigConfig)
            || env.storage().instance().has(&DataKey::MultiSigSigners)
        {
            return Err(Error::AlreadyInitialized);
        }

        Self::validate_multisig_setup(&signers, threshold)?;

        let signers_clone = signers.clone();
        let config = MultiSigConfig(signers, threshold);
        env.storage()
            .persistent()
            .set(&DataKey::MultiSigConfig, &config);

        // Structured event for indexers / auditors (issue #455).  The two
        // payload fields are taken from the local variables that were just
        // written to storage, so the event reconciles exactly with the state this
        // call persisted rather than with the raw inputs.  Nothing fallible
        // follows the publish: every rejection above (`NotInitialized`,
        // `Unauthorized`, `AlreadyInitialized`, `MultiSigNoSigners`,
        // `MultiSigTooManySigners`, `MultiSigInvalidThreshold`,
        // `MultiSigDuplicateSigner`) returns before this point, so a
        // successful call carries exactly one `msiginit` event and a rejected
        // call carries none.
        env.events().publish(
            (symbol_short!("msiginit"),),
            MultisigApprovalInitEvent {
                admin,
                signers: signers_clone,
                threshold,
            },
        );

        Ok(())
    }

    /// Record an approval from one of the registered signers for the given
    /// proposal.  Idempotent — calling twice from the same signer has no
    /// effect and is not an error.
    ///
    /// # Checks (in order)
    /// Authorization and source-state guards run **before** any proposal
    /// record is read or written, so a rejected call cannot mutate storage:
    /// 1. `signer.require_auth()` — the transaction must be signed by
    ///    `signer` (`Unauthorized` if missing).
    /// 2. A multisig regime must exist (`NotInitialized`) and `signer` must be
    ///    one of the registered multisig signers (`Unauthorized`).
    /// 3. A proposal record must exist for `proposal_id`
    ///    (`NoPendingAdminTransfer`) and still be `Pending` (`InvalidStatus`)
    ///    — issue #785: an approval can only bind to a live,
    ///    contract-assigned proposal, never to a free-floating ID, a consumed
    ///    one, or an ID that was never proposed.
    /// 4. The signer's index must be representable as a bit position in the
    ///    `u32` approval bitmap (`ArithmeticOverflow`). This is evaluated with
    ///    `try_into` / `checked_shl` / `checked_add` and runs **before** the
    ///    bitmap is written, so an unrepresentable index cannot leave partial
    ///    state behind (see `multisig_approve_checked_arithmetic_tests`).
    ///
    /// # Storage footprint (issues #457, #785, #787)
    /// Keys this call reads:
    /// * `DataKey::MultiSigConfig` (persistent, issue #772) — the registered signers
    ///   (which also provide the membership check and the signer's bit index)
    ///   and the approval threshold, in one consolidated entry (issue #456).
    /// * `DataKey::AdminProposal(proposal_id)` (persistent) — the proposal
    ///   record that owns the approval bitmap (issue #785).
    ///
    /// The former `DataKey::Job` metadata read and cross-contract token
    /// `balance` probe are gone (issue #787): recording an approval moves no
    /// tokens, so an unfunded escrow is a legitimate place to collect
    /// signatures and this call no longer needs either.
    ///
    /// Keys this call writes: only `AdminProposal(proposal_id)`, and only
    /// when the signer's bit is not already set.  A duplicate approval cannot
    /// change the bitmap, so it writes no contract storage key at all instead
    /// of re-writing the record; the first approval from each signer writes
    /// the record exactly once — the floor for recording an approval at all
    /// (measured per invocation by `multisig_approve_footprint_tests`).
    ///
    /// # Errors
    /// * `NotInitialized`         – `multisig_approval_init` has not been
    ///   called.
    /// * `Unauthorized`           – `signer` did not sign, or is not a
    ///   registered signer.
    /// * `NoPendingAdminTransfer` – No proposal record exists for
    ///   `proposal_id`: approving before a proposal exists, or against an ID
    ///   the contract never assigned, is rejected (issue #785).
    /// * `InvalidStatus`          – The proposal record exists but is no
    ///   longer `Pending` — it was already executed or cancelled, so its
    ///   approvals are spent (issue #785).
    /// * `ArithmeticOverflow`     – The signer's position does not fit the `u32`
    ///   bitmap, i.e. the stored signer set holds more than
    ///   `MAX_MULTISIG_SIGNERS` = 32 entries (which `multisig_approval_init`
    ///   itself rejects with `MultiSigTooManySigners`, so this needs corrupt or
    ///   upgraded storage). Reported as a typed error instead of trapping on
    ///   the release profile's overflow checks.
    ///
    /// Note: `MultiSigEmptyBalance` (25) is **not** returned any more — the
    /// balance guard it named was removed by issue #787.
    pub fn multisig_approve(
        env: Env,
        signer: Address,
        proposal_id: u32,
    ) -> Result<MultiSigApprovalState, Error> {
        Self::ensure_migrated(&env)?;
        signer.require_auth();

        // Consolidated multisig config read (issue #456): the signer set and
        // the threshold live in the same instance entry, so this is one `get`
        // where the previous two-key layout needed two (legacy entries from
        // before the change are still read, see `load_multisig_config`).  A
        // missing configuration still reports `NotInitialized`.
        let config = Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;
        let signers = config.0;
        let threshold = config.1;

        // Reject callers who are not registered signers (find the signer's
        // index, O(n) but n ≤ 32).
        let signer_index = signers
            .iter()
            .position(|s| s == signer)
            .ok_or(Error::Unauthorized)?;

        // Issue #785: bind the approval to a live proposal record.  IDs are
        // assigned by `propose_admin_transfer`, so a missing record means the
        // proposal never existed, and a non-`Pending` record means it has
        // already been consumed — either way this approval must not move.
        //
        // Issue #787: the token-balance guard that used to sit between the
        // membership check and the bitmap read is gone.  Recording signer
        // intent moves no tokens, so approvals are accepted on an unfunded
        // escrow; `execute_admin_transfer` still cannot conjure funds.
        let mut record: AdminTransferProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AdminProposal(proposal_id))
            .ok_or(Error::NoPendingAdminTransfer)?;
        if record.status != AdminProposalStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        // ── Turn the signer's index into its bitmap bit (issue #583) ─────────
        // This is the only integer computation in the call, and every step of
        // it is fallible, so no step uses a bare operator:
        //   * `position` yields a `usize` while the bitmap is a `u32`, so the
        //     index is narrowed with `try_into` (`as` would truncate silently);
        //   * the shift is `checked_shl`, which answers `None` for `idx >= 32`
        //     instead of wrapping onto bit 0 or trapping;
        //   * the accumulator is advanced with `checked_add` rather than `|=`,
        //     so a value the `u32` cannot represent is reported instead of
        //     wrapping.
        // The release profile enables `overflow-checks` with `panic = "abort"`,
        // so a bare operator that overflowed would abort the whole transaction
        // with no value for the caller to inspect; instead each checked step
        // maps its failure to `Error::ArithmeticOverflow` (code 36). All of it
        // runs before the `set` below, so a rejected call writes nothing.
        let idx: u32 = signer_index
            .try_into()
            .map_err(|_| Error::ArithmeticOverflow)?;
        let mask = 1u32.checked_shl(idx).ok_or(Error::ArithmeticOverflow)?;

        // Set the bit for this signer, and write the record back only when the
        // bit actually changed (issue #457).  A duplicate approval
        // (`bitmap & mask != 0`) cannot change the value, so it writes no
        // contract storage key at all; the first approval from each signer
        // writes the record exactly once — the floor for recording an approval.
        // Reaching the add only when the bit is clear also keeps `checked_add`
        // sound at the top of the range: `bitmap + mask` is then exactly
        // `bitmap | mask` and provably representable.
        let bitmap = record.approvals_bitmap;
        if bitmap & mask == 0 {
            record.approvals_bitmap = bitmap.checked_add(mask).ok_or(Error::ArithmeticOverflow)?;
            env.storage()
                .persistent()
                .set(&DataKey::AdminProposal(proposal_id), &record);
        }
        // Re-read after the conditional write so the state returned (and the
        // event) always carries the bitmap as stored, new bit included.
        let bitmap = record.approvals_bitmap;

        let approvals = bitmap.count_ones();
        let approved = approvals >= threshold;

        env.events().publish(
            (symbol_short!("msigappr"),),
            MultiSigApprovedEvent {
                proposal_id,
                signer,
                approvals,
                threshold,
                approved,
                bitmap,
            },
        );

        Ok(MultiSigApprovalState {
            approved,
            approvals,
            threshold,
            bitmap,
        })
    }

    /// Query whether a proposal has reached the required approval threshold.
    ///
    /// # Returns
    /// A `MultiSigApprovalState` built from the persistent `MultiSigConfig`
    /// (its threshold field) and the `AdminProposal(proposal_id)` record's
    /// approval bitmap (issue #785).  An ID with no record reads as an empty
    /// bitmap (`approvals == 0`), and so does an executed or cancelled
    /// proposal — its record keeps the ID observable while its cleared bitmap
    /// reports that its approvals are spent.
    ///
    /// # Guarantees
    /// * **Read-only.**  Exactly two `get`s (instance config, proposal
    ///   record), via `read_multisig_approval`.  No `set`, `remove`,
    ///   `extend_ttl`, or event publish in instance, persistent, or temporary
    ///   storage — in particular it never re-writes the record or bumps its
    ///   TTL.  The whole ledger is byte-identical before and after the call
    ///   (enforced by `read_path_tests`).
    /// * Requires no authorization.
    ///
    /// # Errors
    /// * `NotInitialized` – `multisig_approval_init` has not been called.
    pub fn is_multisig_approved(
        env: Env,
        proposal_id: u32,
    ) -> Result<MultiSigApprovalState, Error> {
        Self::read_multisig_approval(&env, proposal_id)
    }

    /// Read path behind `is_multisig_approved`.
    ///
    /// **This function must contain only read operations.**
    fn read_multisig_approval(env: &Env, proposal_id: u32) -> Result<MultiSigApprovalState, Error> {
        // One instance `get` for the consolidated signer-set/threshold entry
        // (issue #456).  The proposal record below is the only other storage
        // read; nothing here writes.
        let config = Self::load_multisig_config(env).ok_or(Error::NotInitialized)?;
        let threshold = config.1;

        // Issue #785: the bitmap now lives in the proposal record.  Pre-upgrade
        // temporary `MultiSigApproval` bitmaps are no longer consulted — an ID
        // whose record never existed (or whose approvals were collected before
        // the upgrade) reads as zero, so approvals are re-collected against a
        // freshly proposed record.
        let record: Option<AdminTransferProposal> = env
            .storage()
            .persistent()
            .get(&DataKey::AdminProposal(proposal_id));
        let bitmap: u32 = record.map(|r| r.approvals_bitmap).unwrap_or(0);

        let approvals = bitmap.count_ones();

        Ok(MultiSigApprovalState {
            approved: approvals >= threshold,
            approvals,
            threshold,
            bitmap,
        })
    }

    // ── multisig admin transfer: transaction status lock ──────────────────
    //
    // `propose_admin_transfer` / `execute_admin_transfer` /
    // `cancel_admin_transfer_proposal` build a status lock on top of the
    // generic multisig approval bitmap above: once a transfer is proposed,
    // `DataKey::PendingAdminTransfer` is set and no further proposal can be
    // created until the pending one executes or is explicitly cancelled by
    // the admin. This prevents the signer approvals already being collected
    // for one `new_admin` from being silently redirected mid-flight by a
    // second, overlapping proposal.

    /// Assign one of the narrower administrative roles (issue #761) to
    /// `new_address`. Root admin only. Passing the root admin's own address
    /// returns the role to the default (held by the root admin).
    /// Emits `rolechg`.
    ///
    /// # Errors
    /// * `MigrationRequired` – Storage predates this code; call `migrate`.
    /// * `NotInitialized`    – No admin is stored.
    /// * `Unauthorized`      – `admin` is not the root admin.
    /// * `InvalidAddress`    – `new_address` is a zero address or this
    ///   contract.
    pub fn set_admin_role(
        env: Env,
        admin: Address,
        role: AdminRole,
        new_address: Address,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin_from_instance(&env, &admin)?;
        Self::validate_address(&env, &new_address)?;

        let instance = env.storage().instance();
        let key = DataKey::AdminRole(role);
        let previous: Address = instance.get(&key).unwrap_or(admin.clone());
        if new_address == admin {
            instance.remove(&key);
        } else {
            instance.set(&key, &new_address);
        }

        env.events().publish(
            (symbol_short!("rolechg"),),
            AdminRoleChangedEvent {
                admin,
                role,
                previous,
                new_address,
            },
        );
        Ok(())
    }

    /// The address currently holding `role`: its assignee, or the root admin.
    ///
    /// # Errors
    /// * `NotInitialized` – No admin is stored.
    pub fn get_admin_role(env: Env, role: AdminRole) -> Result<Address, Error> {
        let instance = env.storage().instance();
        match instance.get(&DataKey::AdminRole(role)) {
            Some(holder) => Ok(holder),
            None => instance.get(&DataKey::Admin).ok_or(Error::NotInitialized),
        }
    }

    /// Propose a new admin via the multisig approval workflow.
    ///
    /// The proposal ID is **assigned by the contract** (issue #785): the
    /// persistent `DataKey::AdminProposalCounter` advances by one on every
    /// proposal, so no caller can choose an ID to collide with approvals
    /// already collected elsewhere, and an ID — along with the record and
    /// approvals it consumed — can never be reused.  Both the full proposal
    /// record (`DataKey::AdminProposal(id)`, holding `new_admin`, the
    /// creation timestamp, the `Pending` status, and the approval bitmap) and
    /// the `PendingAdminTransfer` lock/mirror are written atomically.
    ///
    /// # Errors
    /// * `NotInitialized`       – Contract not initialised.
    /// * `Unauthorized`         – Caller is not the stored admin.
    /// * `InvalidAddress`       – `new_admin` is a zero address, the contract
    ///   itself, or the current admin.
    /// * `AdminTransferPending` – A proposal is already pending; execute or
    ///   cancel it before proposing another.
    /// * `ArithmeticOverflow`   – The proposal counter has no representable
    ///   successor (2^32 proposals have been created); nothing is written.
    pub fn propose_admin_transfer(
        env: Env,
        admin: Address,
        new_admin: Address,
    ) -> Result<u32, Error> {
        Self::ensure_migrated(&env)?;
        // Storage-footprint note: `DataKey::Version` (instance) and
        // `DataKey::Admin` (persistent) are only ever written together, in a
        // single atomic `initialize` call — a failed `initialize` reverts the
        // whole invocation, so one can never be present without the other.
        // `load_admin` alone is therefore a complete "is this contract
        // initialised" check (it already backs this same guarantee in
        // `execute_admin_transfer`), so a separate `require_initialized`
        // call — and the extra `Version` ledger entry it touches — is
        // redundant here. Auth is still checked only *after* this guard, to
        // preserve the existing behaviour of rejecting an uninitialised
        // contract before requiring any signature.
        let stored_admin = Self::load_admin(&env)?;
        admin.require_auth();
        if stored_admin != admin {
            return Err(Error::Unauthorized);
        }

        Self::validate_new_admin(&env, &stored_admin, &new_admin)?;

        if env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
        {
            return Err(Error::AdminTransferPending);
        }

        // Issue #785: assign the ID ourselves.  `checked_add` reports an
        // exhausted counter as a typed error instead of trapping under the
        // release profile's overflow checks, and the counter write lands in
        // the same invocation as the two record writes below, so the assigned
        // ID can never be handed out twice.
        let proposal_id = Self::open_proposal_record(&env, &new_admin)?;
        env.storage().persistent().set(
            &DataKey::PendingAdminTransfer,
            &PendingAdminTransfer {
                new_admin: new_admin.clone(),
                proposal_id,
            },
        );

        env.events().publish(
            (symbol_short!("adminprp"),),
            AdminTransferProposedEvent {
                admin,
                new_admin,
                proposal_id,
            },
        );

        Ok(proposal_id)
    }

    /// Assign the next proposal ID and open its `Pending` record with an
    /// empty approval bitmap (issue #785).  `checked_add` reports an exhausted
    /// counter as `ArithmeticOverflow`, and the counter and record are written
    /// in the same invocation, so an ID is never handed out twice.  `subject`
    /// is stored as the record's `new_admin`: the incoming admin for a
    /// transfer, the proposing admin for a signer-set change.
    fn open_proposal_record(env: &Env, subject: &Address) -> Result<u32, Error> {
        let persistent = env.storage().persistent();
        let proposal_id: u32 = persistent
            .get(&DataKey::AdminProposalCounter)
            .unwrap_or(0u32)
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;
        persistent.set(&DataKey::AdminProposalCounter, &proposal_id);
        persistent.set(
            &DataKey::AdminProposal(proposal_id),
            &AdminTransferProposal {
                proposal_id,
                new_admin: subject.clone(),
                created_at: env.ledger().timestamp(),
                status: AdminProposalStatus::Pending,
                approvals_bitmap: 0,
            },
        );
        Ok(proposal_id)
    }

    /// Execute a pending admin transfer once its multisig proposal has
    /// reached the configured approval threshold. Any caller may trigger
    /// execution — the safety guarantee comes from the collected signer
    /// approvals, not caller identity — but nothing happens unless the
    /// proposal **record's** approval bitmap (issue #785) reports the
    /// threshold met.  On success the proposal is consumed: the record is
    /// marked `Executed` with its bitmap cleared, so neither the ID nor its
    /// approvals can ever run again.
    ///
    /// # Errors
    /// * `NotInitialized`          – Contract has never been initialised, so
    ///   there is no admin to transfer from.
    /// * `NoPendingAdminTransfer`  – No transfer is currently proposed.
    /// * `InvalidStatus`           – The pending transfer has no proposal
    ///   record (written before issue #785 landed, so its approvals cannot be
    ///   verified — cancel it and re-propose), or the record is not `Pending`.
    /// * `MultiSigThresholdNotMet` – Approvals collected so far are below the
    ///   required threshold.
    pub fn execute_admin_transfer(env: Env) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Precondition guard: reject an uninitialised contract (no admin ever
        // stored, so nothing can be transferred) with `NotInitialized` before
        // any pending-transfer or approval storage is read or written.
        let old_admin = Self::load_admin(&env)?;

        let pending: PendingAdminTransfer = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .ok_or(Error::NoPendingAdminTransfer)?;

        // Issue #785: execution is bound to the proposal record for exactly
        // this pending ID.  The record carries the approvals next to the
        // `new_admin` they authorize installing, so approvals collected for
        // any other proposal can never satisfy this one, and a legacy pending
        // entry with no record (pre-upgrade) cannot be executed at all.
        let mut record: AdminTransferProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AdminProposal(pending.proposal_id))
            .ok_or(Error::InvalidStatus)?;
        if record.status != AdminProposalStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        // Threshold comes from the same consolidated config the approvals were
        // collected against (issue #456); a contract without a multisig
        // regime has no threshold to satisfy.
        let config = Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;
        let approvals = record.approvals_bitmap.count_ones();
        if approvals < config.1 {
            return Err(Error::MultiSigThresholdNotMet);
        }

        // `propose_admin_transfer` now refuses the current admin (issue #771),
        // but a same-admin proposal recorded before that check shipped may
        // still be pending after an upgrade. Such a proposal does not need to
        // rewrite the Admin ledger entry; it still emits the event and clears
        // the pending proposal.
        if old_admin != pending.new_admin {
            env.storage()
                .persistent()
                .set(&DataKey::Admin, &pending.new_admin);
            // Keep the instance copy read by `require_admin_from_instance`
            // in sync with the persistent one.
            env.storage()
                .instance()
                .set(&DataKey::Admin, &pending.new_admin);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::PendingAdminTransfer);

        // Consume the proposal: terminal status + cleared bitmap (issue #785).
        // The record itself stays as the tombstone that keeps this ID — and
        // every approval it held — permanently unusable.
        record.status = AdminProposalStatus::Executed;
        record.approvals_bitmap = 0;
        env.storage()
            .persistent()
            .set(&DataKey::AdminProposal(pending.proposal_id), &record);

        env.events().publish(
            (symbol_short!("adminexc"),),
            AdminTransferExecutedEvent {
                old_admin,
                new_admin: pending.new_admin,
                proposal_id: pending.proposal_id,
            },
        );

        Ok(())
    }

    /// Cancel a pending admin-transfer proposal, clearing the lock so a new
    /// one can be proposed. Only the current admin may cancel.  The proposal
    /// record is consumed alongside the lock — marked `Cancelled` with its
    /// approval bitmap cleared (issue #785) — so the cancelled ID and every
    /// approval it collected are spent and cannot be revived by a later
    /// proposal (IDs are never reused).
    ///
    /// # Errors
    /// * `NotInitialized`         – Contract not initialised.
    /// * `Unauthorized`           – Caller is not the stored admin.
    /// * `NoPendingAdminTransfer` – Nothing is currently pending.
    pub fn cancel_admin_transfer_proposal(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin(&env, &admin)?;

        let pending: PendingAdminTransfer = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .ok_or(Error::NoPendingAdminTransfer)?;

        env.storage()
            .persistent()
            .remove(&DataKey::PendingAdminTransfer);

        // Consume the proposal record when one exists (issue #785).  A pending
        // entry written before the record existed has nothing to mark — it is
        // simply cleared here so the admin can re-propose under a fresh,
        // record-backed ID.
        let record_key = DataKey::AdminProposal(pending.proposal_id);
        if let Some(mut record) = env
            .storage()
            .persistent()
            .get::<DataKey, AdminTransferProposal>(&record_key)
        {
            record.status = AdminProposalStatus::Cancelled;
            record.approvals_bitmap = 0;
            env.storage().persistent().set(&record_key, &record);
        }

        env.events().publish(
            (symbol_short!("admincxl"),),
            AdminTransferCancelledEvent {
                admin,
                proposal_id: pending.proposal_id,
            },
        );

        Ok(())
    }

    // ── multisig signer-set rotation ───────────────────────────────────────
    //
    // `multisig_approval_init` can only run once, so rotating signers (a
    // lost key, a departing operator) or changing the threshold goes through
    // the same proposal/approval flow as an admin transfer: the admin
    // proposes, the contract assigns the proposal ID and opens its record
    // (#785), the *current* signers approve it up to the current threshold,
    // and anyone executes.
    //
    // Approval bitmaps index the signer vec, so bit `i` names a different
    // address after a rotation.  Executing a rotation therefore clears the
    // approvals of a pending admin-transfer proposal; the new signers have to
    // approve it again.

    /// Propose replacing the multisig signer set and threshold.
    ///
    /// The new configuration is validated like `multisig_approval_init`'s and
    /// recorded as pending under a contract-assigned proposal ID, which is
    /// returned.  It takes effect through `execute_multisig_config` once the
    /// current signers have approved that ID up to the current threshold.
    /// Emits `msigcfgp`.
    ///
    /// # Errors
    /// * `NotInitialized`           – Contract not initialised, or no multisig
    ///   regime exists yet (use `multisig_approval_init`).
    /// * `Unauthorized`             – `admin` is not the stored admin.
    /// * `MultiSigNoSigners` / `MultiSigTooManySigners` /
    ///   `MultiSigInvalidThreshold` / `MultiSigDuplicateSigner` – the new
    ///   configuration is invalid (`validate_multisig_setup`).
    /// * `MultiSigConfigPending`    – Another change is already pending.
    /// * `ArithmeticOverflow`       – The proposal counter is exhausted.
    pub fn propose_multisig_config(
        env: Env,
        admin: Address,
        new_signers: Vec<Address>,
        new_threshold: u32,
    ) -> Result<u32, Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin(&env, &admin)?;
        Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;
        Self::validate_multisig_setup(&new_signers, new_threshold)?;

        let persistent = env.storage().persistent();
        if persistent.has(&DataKey::PendingMultiSigConfig) {
            return Err(Error::MultiSigConfigPending);
        }

        let proposal_id = Self::open_proposal_record(&env, &admin)?;
        persistent.set(
            &DataKey::PendingMultiSigConfig,
            &PendingMultiSigConfig {
                signers: new_signers.clone(),
                threshold: new_threshold,
                proposal_id,
            },
        );

        env.events().publish(
            (symbol_short!("msigcfgp"),),
            MultiSigConfigProposedEvent {
                admin,
                signers: new_signers,
                threshold: new_threshold,
                proposal_id,
            },
        );

        Ok(proposal_id)
    }

    /// Apply the pending signer-set change once its proposal has reached the
    /// current threshold.  Anyone may call it.  The proposal is consumed, and
    /// a pending admin transfer's approvals are cleared because they index the
    /// old signer vec.  Emits `msigcfg`.
    ///
    /// # Errors
    /// * `NoPendingMultiSigConfig` – Nothing is pending.
    /// * `NotInitialized`          – No multisig regime exists.
    /// * `InvalidStatus`           – The proposal record is missing or no
    ///   longer `Pending`.
    /// * `MultiSigThresholdNotMet` – Not enough current signers approved.
    pub fn execute_multisig_config(env: Env) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        let persistent = env.storage().persistent();
        let pending: PendingMultiSigConfig = persistent
            .get(&DataKey::PendingMultiSigConfig)
            .ok_or(Error::NoPendingMultiSigConfig)?;
        let MultiSigConfig(old_signers, old_threshold) =
            Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;

        let mut record: AdminTransferProposal = persistent
            .get(&DataKey::AdminProposal(pending.proposal_id))
            .ok_or(Error::InvalidStatus)?;
        if record.status != AdminProposalStatus::Pending {
            return Err(Error::InvalidStatus);
        }
        if !Self::read_multisig_approval(&env, pending.proposal_id)?.approved {
            return Err(Error::MultiSigThresholdNotMet);
        }

        persistent.set(
            &DataKey::MultiSigConfig,
            &MultiSigConfig(pending.signers.clone(), pending.threshold),
        );
        // Drop any pre-#772 / pre-#456 instance copies so no read path can
        // fall back to the old set.
        let instance = env.storage().instance();
        instance.remove(&DataKey::MultiSigConfig);
        instance.remove(&DataKey::MultiSigSigners);
        instance.remove(&DataKey::MultiSigThreshold);
        persistent.remove(&DataKey::PendingMultiSigConfig);

        record.status = AdminProposalStatus::Executed;
        record.approvals_bitmap = 0;
        persistent.set(&DataKey::AdminProposal(pending.proposal_id), &record);

        // Approvals for a pending admin transfer were given by the old signer
        // set; their bits would name different addresses now.
        if let Some(transfer) =
            persistent.get::<_, PendingAdminTransfer>(&DataKey::PendingAdminTransfer)
        {
            let key = DataKey::AdminProposal(transfer.proposal_id);
            if let Some(mut transfer_record) = persistent.get::<_, AdminTransferProposal>(&key) {
                if transfer_record.approvals_bitmap != 0 {
                    transfer_record.approvals_bitmap = 0;
                    persistent.set(&key, &transfer_record);
                }
            }
        }

        env.events().publish(
            (symbol_short!("msigcfg"),),
            MultiSigConfigChangedEvent {
                old_signers,
                old_threshold,
                new_signers: pending.signers,
                new_threshold: pending.threshold,
                proposal_id: pending.proposal_id,
            },
        );

        Ok(())
    }

    /// Withdraw the pending signer-set change.  Its proposal is consumed.
    /// Emits `msigcfgx`.
    ///
    /// # Errors
    /// * `Unauthorized`            – `admin` is not the stored admin.
    /// * `NoPendingMultiSigConfig` – Nothing is pending.
    pub fn cancel_multisig_config_proposal(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin(&env, &admin)?;
        let persistent = env.storage().persistent();
        let pending: PendingMultiSigConfig = persistent
            .get(&DataKey::PendingMultiSigConfig)
            .ok_or(Error::NoPendingMultiSigConfig)?;
        persistent.remove(&DataKey::PendingMultiSigConfig);

        let key = DataKey::AdminProposal(pending.proposal_id);
        if let Some(mut record) = persistent.get::<_, AdminTransferProposal>(&key) {
            record.status = AdminProposalStatus::Cancelled;
            record.approvals_bitmap = 0;
            persistent.set(&key, &record);
        }

        env.events().publish(
            (symbol_short!("msigcfgx"),),
            MultiSigConfigCancelledEvent {
                admin,
                proposal_id: pending.proposal_id,
            },
        );

        Ok(())
    }

    /// The pending signer-set change, if any.
    pub fn get_pending_multisig_config(env: Env) -> Option<PendingMultiSigConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::PendingMultiSigConfig)
    }

    /// Return the currently pending admin-transfer proposal, if any.
    ///
    /// # Errors
    /// * `NotInitialized` – The contract has not been initialized (no admin
    ///   key is present in storage).  Callers must handle this case rather
    ///   than relying on a silent `None` return.
    ///
    /// # Read-only contract
    ///
    /// This function **must never write to any storage tier** (instance,
    /// persistent, or temporary).  It is a pure read: the only operations
    /// permitted on `env.storage()` are:
    /// * one `.persistent().get(…)` on `DataKey::PendingAdminTransfer` — the
    ///   payload, resolved exactly once per invocation; and
    /// * at most one `.persistent().has(…)` presence probe on `DataKey::Admin`
    ///   — the initialization guard, and only on the no-proposal path.
    ///
    /// A call that finds a pending proposal therefore touches a single ledger
    /// entry; the `Admin` entry is consulted only to tell "initialized, nothing
    /// pending" apart from "never initialized" (#496).
    ///
    /// Any future edit that introduces a `.set(…)`, `.remove(…)`,
    /// `.bump(…)`, or equivalent mutating call breaks this invariant and
    /// **must be rejected in code review**.  The snapshot-identity tests in
    /// `get_pending_admin_transfer_tests.rs` enforce this property
    /// automatically: a full ledger snapshot taken immediately before and
    /// immediately after calling this function must be byte-identical.
    pub fn get_pending_admin_transfer(env: Env) -> Result<Option<PendingAdminTransfer>, Error> {
        Self::read_pending_admin_transfer(&env)
    }

    /// Pure-read inner implementation for `get_pending_admin_transfer`.
    ///
    /// Extracted as a named private helper so that:
    /// * the public entry-point stays one line, making accidental write
    ///   additions immediately obvious in diff review, and
    /// * internal callers (tests, guards) can call the same read path
    ///   without re-spelling the storage key.
    ///
    /// Returns `Err(Error::NotInitialized)` when no admin key is stored.
    ///
    /// **This function must contain only read operations.**
    fn read_pending_admin_transfer(env: &Env) -> Result<Option<PendingAdminTransfer>, Error> {
        let storage = env.storage().persistent();

        // Resolve the payload key exactly once per invocation.  A stored
        // proposal is proof enough that `initialize` ran, so the
        // initialization guard below is skipped entirely on this path and the
        // call touches a single storage entry (#496).
        let pending: Option<PendingAdminTransfer> = storage.get(&DataKey::PendingAdminTransfer);
        if pending.is_some() {
            return Ok(pending);
        }

        // No proposal: distinguish "initialized, nothing pending" from "never
        // initialized" via the shared initialisation predicate.
        Self::require_initialized(env)?;
        Ok(None)
    }

    /// Return the contract's code version.
    ///
    /// # Returns
    /// The `u32` stored under the instance key `DataKey::Version`:
    /// * `1` after `initialize` (which writes the marker `1u32`).
    /// * Incremented by exactly one on every successful `upgrade`.
    /// * `1` when the key is absent (the contract has not been initialized),
    ///   i.e. the version of the code that shipped before any upgrade.
    ///
    /// # Guarantees
    /// * **Read-only.**  Performs a single instance-storage `get` and nothing
    ///   else: no `set`, `remove`, `extend_ttl`, or event publish in instance,
    ///   persistent, or temporary storage.  The whole ledger is byte-identical
    ///   before and after the call (enforced by `version_tests`).
    /// * Requires no authorization and is safe to call on an uninitialized
    ///   contract.
    ///
    /// # Errors
    /// None.  `version` is infallible: it returns a bare `u32`, and the only
    /// absent-state case (uninitialized contract) maps to the default `1`
    /// rather than an error.
    pub fn version(env: Env) -> u32 {
        Self::read_version(&env)
    }

    /// Read path behind `version`.  Takes `&Env` and returns a plain value so
    /// callers cannot thread a mutation through it.
    ///
    /// **This function must contain only read operations.**
    fn read_version(env: &Env) -> u32 {
        env.storage().instance().get(&DataKey::Version).unwrap_or(1)
    }

    /// Return the storage layout version (see `SCHEMA_VERSION`).
    ///
    /// Lower than `SCHEMA_VERSION` after an `upgrade` until the admin calls
    /// `migrate`; state-changing endpoints return `MigrationRequired` until
    /// then.  Read-only; `1` on deployments that predate the marker.
    pub fn schema_version(env: Env) -> u32 {
        Self::read_schema_version(&env)
    }

    fn read_schema_version(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::SchemaVersion)
            .unwrap_or(1)
    }

    /// Reject state-changing calls until `migrate` has brought storage up to
    /// `SCHEMA_VERSION`.  An uninitialized contract passes, so its endpoints
    /// keep reporting `NotInitialized`.
    fn ensure_migrated(env: &Env) -> Result<(), Error> {
        let instance = env.storage().instance();
        match instance.get::<_, u32>(&DataKey::SchemaVersion) {
            Some(version) if version >= SCHEMA_VERSION => Ok(()),
            Some(_) => Err(Error::MigrationRequired),
            // `DataKey::Version` is the initialisation marker.
            None if instance.has(&DataKey::Version) => Err(Error::MigrationRequired),
            None => Ok(()),
        }
    }

    /// Return a full snapshot of the current job, including every
    /// milestone's status and amounts.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    pub fn get_job(env: Env) -> Result<Job, Error> {
        let meta = Self::load_job_meta(&env)?;
        Self::assemble_job(&env, &meta)
    }

    /// Return one milestone without assembling the rest of the job (issue
    /// #733): reads the job metadata and exactly one milestone entry,
    /// whatever the milestone count.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `InvalidMilestone` – `index` is outside the job's milestone range.
    pub fn get_milestone(env: Env, index: u32) -> Result<Milestone, Error> {
        let meta = Self::load_job_meta(&env)?;
        if index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        let mut milestone = Self::load_milestone(&env, index)?;
        milestone.funded = Self::milestone_is_funded(&meta, &milestone);
        Ok(milestone)
    }

    /// Number of milestones in the job (issue #733).
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    pub fn milestone_count(env: Env) -> Result<u32, Error> {
        Ok(Self::load_job_meta(&env)?.milestone_count)
    }

    /// Return everything needed to render the escrow in a single read: the
    /// job (as `get_job`), the admin, both pause flags, the cancellation
    /// lock and approval mask, each milestone's time extension and
    /// auto-release deadline, the platform-fee allocation and the version.
    ///
    /// Several of these (admin, cancel state, per-milestone extensions) have
    /// no other getter, and the rest would otherwise cost one simulation each.
    ///
    /// Read-only: no storage write, TTL extension or event (enforced by
    /// `get_escrow_state_no_mutation_tests`).
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    pub fn get_escrow_state(env: Env) -> Result<EscrowState, Error> {
        let meta = Self::load_job_meta(&env)?;
        let job = Self::assemble_job(&env, &meta)?;
        let admin = Self::load_admin(&env)?;

        let instance = env.storage().instance();
        let paused = EscrowPausedState {
            emergency: instance.get(&DataKey::Ep).unwrap_or(false),
            admin: instance.get(&DataKey::Paused).unwrap_or(false),
        };
        let cancel = EscrowCancelState {
            locked: instance.get(&DataKey::CancelLock).unwrap_or(false),
            // An approval older than CANCEL_APPROVAL_TTL_SECONDS no longer
            // counts (#713), so it reads as no approval here too.
            approval_mask: instance
                .get::<_, CancelApprovalRecord>(&DataKey::CancelApproval)
                .filter(|record| {
                    env.ledger().timestamp().saturating_sub(record.approved_at)
                        < CANCEL_APPROVAL_TTL_SECONDS
                })
                .map(|record| record.mask)
                .unwrap_or(0),
        };

        let mut milestone_timing = Vec::new(&env);
        for (index, milestone) in job.milestones.iter().enumerate() {
            let index = index as u32;
            let extension_seconds = Self::load_time_extension(&env, index);
            // Auto-release only applies to Delivered milestones
            // (claim_auto_release), so only they have a deadline.
            let deadline = if milestone.status == MilestoneStatus::Delivered {
                let delivered_at =
                    Self::load_delivered_at(&env, index).unwrap_or(milestone.delivered_at);
                Some(Self::auto_release_deadline(
                    &meta,
                    delivered_at,
                    extension_seconds,
                )?)
            } else {
                None
            };
            milestone_timing.push_back(MilestoneTiming {
                extension_seconds,
                deadline,
            });
        }

        let fee_allocation: Option<PlatformFeeAllocation> =
            Self::cold_get(&env, &DataKey::PlatformFeeAllocation);
        Ok(EscrowState {
            job,
            admin,
            paused,
            cancel,
            milestone_timing,
            fee_allocation_set: fee_allocation.is_some(),
            fee_allocation: fee_allocation.unwrap_or(PlatformFeeAllocation {
                client_bps: 0,
                freelancer_bps: 0,
                treasury_bps: 0,
                locked: false,
            }),
            version: Self::read_version(&env),
        })
    }

    /// Recover tokens that are not backing outstanding escrow obligations.
    ///
    /// Authorized callers: the job client or the admin. Excess escrow-token
    /// balance is returned to the client. Non-escrow tokens are returned in
    /// full to the client after confirming they are not the settlement token.
    pub fn sweep_excess(env: Env, caller: Address, token: Address) -> Result<i128, Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;
        let admin = Self::load_admin(&env)?;
        if caller != meta.client && caller != admin {
            return Err(Error::Unauthorized);
        }

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());
        let excess = if token == meta.token {
            let obligations = Self::outstanding_escrow_obligations(&env, &meta)?;
            if balance < obligations {
                return Err(Error::InsufficientBalance);
            }
            balance
                .checked_sub(obligations)
                .ok_or(Error::ArithmeticOverflow)?
        } else {
            balance
        };
        if excess <= 0 {
            return Err(Error::NothingToSweep);
        }

        token_client.transfer(&env.current_contract_address(), &meta.client, &excess);
        env.events().publish(
            (symbol_short!("sweep"),),
            ExcessSweptEvent {
                token,
                amount: excess,
                recipient: meta.client,
            },
        );
        Ok(excess)
    }

    /// Return the off-chain description hash committed for one milestone.
    pub fn get_milestone_spec_hash(env: Env, index: u32) -> Result<BytesN<32>, Error> {
        let meta = Self::load_job_meta(&env)?;
        if index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        Ok(Self::load_milestone(&env, index)?.spec_hash)
    }

    /// Return the reputation counter for an address.
    ///
    /// Read-only: performs no storage writes, TTL extensions or event
    /// publications. Each storage key is touched at most once per call — one
    /// existence check on `DataKey::Admin` and one read of
    /// `DataKey::Reputation(address)`. `reputation_no_mutation_tests` pins this
    /// with whole-ledger snapshots taken around the call (#479); any `set`,
    /// `remove` or `extend_ttl` added here fails that suite.
    ///
    /// # Returns
    /// * **Populated** – `Ok(n)` where `n` is the stored counter, i.e. the
    ///   number of completed jobs (full releases) the address has been a
    ///   client or freelancer on.
    /// * **Empty** – `Ok(0)` when no `Reputation` entry exists for `address`.
    ///   An absent entry and a zero counter are indistinguishable.
    /// * **Boundary** – the value is a `u32`, so the result lies in
    ///   `0..=u32::MAX`. This getter never saturates or wraps; the counter is
    ///   incremented with plain `+ 1` in `increment_reputation`, so a counter
    ///   already at `u32::MAX` would overflow on the *next increment* (panic
    ///   with overflow checks enabled), not in this read.
    ///
    /// # Errors
    /// * `NotInitialized` - Contract has not been initialized.
    pub fn get_reputation(env: Env, address: Address) -> Result<u32, Error> {
        Self::require_initialized(&env)?;
        Ok(env
            .storage()
            .persistent()
            .get(&DataKey::Reputation(address))
            .unwrap_or(0))
    }

    // ── escrow_interest_yield: estimator + share-config validation ────────────

    /// Estimate the interest yield that would accrue on an escrowed balance
    /// over a given duration, using a simple-interest model.
    ///
    /// # Authorization and preconditions
    ///
    /// The guard block runs at the top of this function, before any
    /// calculation or event publish, so an unauthorized or uninitialized call
    /// is rejected before any work is done:
    ///
    /// 1. **Authorization** — `admin.require_auth()` inside `require_admin`
    ///    makes the host reject a missing signature before the contract body
    ///    executes. The stored-admin equality check then rejects a
    ///    validly-signed non-admin with `Unauthorized`.
    /// 2. **Initialized state** — `require_initialized` ensures the contract
    ///    has been set up, so the estimator is always anchored to a real job.
    ///
    /// # Parameters
    /// * `admin`             – Caller; must be the stored contract admin.
    /// * `principal`         – Balance (token stroops) on which interest is
    ///                         calculated. Must be > 0.
    /// * `annual_rate_bps`   – Annual interest rate in basis points
    ///                         (1 bp = 0.01 %). Must satisfy
    ///                         `0 < annual_rate_bps ≤ 10_000`.
    /// * `duration_seconds`  – Accrual window in seconds. Must be > 0.
    ///
    /// # Formula
    /// ```text
    /// yield = principal * annual_rate_bps * duration_seconds
    ///         / (10_000 * SECONDS_PER_YEAR)
    /// ```
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has never been initialised.
    /// * `Unauthorized`    – `admin` does not match the stored admin.
    /// * `InvalidAmount`   – Zero/negative principal, rate, or duration, or an
    ///                       intermediate checked multiplication overflows.
    /// * `InvalidRatio`   – `annual_rate_bps` exceeds 10_000 (unsupported).
    pub fn escrow_interest_yield(
        env: Env,
        admin: Address,
        principal: i128,
        annual_rate_bps: i128,
        duration_seconds: i128,
    ) -> Result<i128, Error> {
        // Authorization: admin.require_auth() runs inside require_admin, so a
        // transaction missing the admin signature is rejected by the host
        // before this body executes at all. The equality check then rejects a
        // validly-signed non-admin. Both guards run before any calculation or
        // event publish, so an unauthorized caller learns nothing about the
        // configuration.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        // Precondition: the escrow must be initialized so the estimator is
        // anchored to a real job rather than a fresh or uninitialized instance.
        Self::require_initialized(&env)?;

        Self::validate_interest_yield_params(principal, annual_rate_bps, duration_seconds)?;

        let numerator = principal
            .checked_mul(annual_rate_bps)
            .ok_or(Error::InvalidAmount)?
            .checked_mul(duration_seconds)
            .ok_or(Error::InvalidAmount)?;

        let denominator = BPS_DENOMINATOR
            .checked_mul(SECONDS_PER_YEAR)
            .ok_or(Error::InvalidAmount)?;

        let yield_amount = numerator
            .checked_div(denominator)
            .ok_or(Error::InvalidAmount)?;

        // Publish exactly once on the success path (after validation and the
        // checked arithmetic), so every error path returns before emitting.
        env.events().publish(
            (symbol_short!("intyield"),),
            EscrowInterestYieldEvent {
                principal,
                annual_rate_bps,
                duration_seconds,
                yield_amount,
            },
        );

        Ok(yield_amount)
    }

    /// Initialize or update interest/yield share configuration (unlocked by
    /// default on first write). Rejects invalid share totals and modifications
    /// while an execution lock is held.
    ///
    /// This is the admin-only path, so it is only open **before the escrow is
    /// funded** (issue #776). Once the client has deposited, the split is part
    /// of the parties' economics and can only change through
    /// `set_interest_yield_consent`, which requires the client and freelancer
    /// to sign as well.
    ///
    /// ## Guard order
    ///
    /// Every guard runs before this function reads the payload ledger entry or
    /// performs its single write, in this order:
    ///
    /// 1. **Authorization** — `require_admin_from_instance` performs
    ///    `admin.require_auth()` (so a transaction that omits the admin
    ///    signature is rejected by the host before the contract body executes)
    ///    and then compares against the stored admin.  Being first, an
    ///    unauthorized caller learns nothing about the configuration.
    /// 2. **Illegal source state** — a funded escrow is refused with
    ///    `AlreadyFunded` (the admin may not move yield between the parties
    ///    after they committed funds), then `ensure_interest_yield_writable`
    ///    rejects a locked configuration with `EscrowLocked`.  This runs *before* the
    ///    argument validation below so the state guard is authoritative: a
    ///    caller that is refused because the configuration is frozen for
    ///    execution always gets `EscrowLocked`, never `InvalidRatio`, and so
    ///    cannot use the error to probe the guard's state.
    /// 3. **Argument validation** — `validate_interest_yield_share_config` is
    ///    pure (no ledger access), so a malformed ratio is rejected without
    ///    touching the ledger at all.
    ///
    /// Because the guards are exhaustive, the only way to reach the write is
    /// with an authenticated admin, an unlocked configuration, and a ratio that
    /// sums to `BPS_SCALE`; every rejection path returns above the write and
    /// therefore leaves the ledger byte-for-byte unchanged.
    ///
    /// ## Storage-footprint note
    ///
    /// The admin check and the funded check both read the contract-instance
    /// entry (`require_admin_from_instance`, `DataKey::Job`), so together they
    /// add a single ledger entry. `InterestYieldState` is cold configuration in
    /// persistent storage (issue #772); the precondition guard issues exactly
    /// one read of it before the single write.
    ///
    /// # Errors
    /// Listed in evaluation order; a call that violates more than one guard
    /// fails with the first match.
    /// * `NotInitialized` – Contract admin key is missing.
    /// * `Unauthorized`   – Caller is not the stored admin.
    /// * `AlreadyFunded`  – The escrow has been funded; use
    ///   `set_interest_yield_consent` instead.
    /// * `EscrowLocked`   – Configuration is locked for execution.
    /// * `InvalidRatio`   – Shares do not sum to exactly 10_000 bps.
    pub fn set_escrow_interest_yield(
        env: Env,
        admin: Address,
        client_share_bps: u32,
        freelancer_share_bps: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Authorization: `admin.require_auth()` inside the helper makes the
        // host reject a missing signature before this body runs at all; the
        // equality check then rejects a validly-signed non-admin.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        // Illegal source state: after funding the split is the parties'
        // economics, so the admin alone may no longer change it (issue #776).
        // `Job` lives in instance storage, the same entry as the admin read.
        let funded = env
            .storage()
            .instance()
            .get::<_, JobMeta>(&DataKey::Job)
            .is_some_and(|meta| meta.funded);
        if funded {
            return Err(Error::AlreadyFunded);
        }

        // Illegal source state: a locked configuration may not be replaced
        // until an admin clears the lock. Checked before the pure argument
        // validation so the freeze takes precedence over the ratio, and in a
        // single read of the instance entry.
        Self::ensure_interest_yield_writable(&env)?;

        // Argument validation: pure, so a bad ratio is rejected without any
        // further ledger access.
        Self::validate_interest_yield_share_config(client_share_bps, freelancer_share_bps)?;

        Self::store_interest_yield_state(
            &env,
            &EscrowInterestYieldState {
                client_share_bps,
                freelancer_share_bps,
                locked: false,
            },
        );
        // Publish structured event carrying acting address and resulting values
        // Reconciles exactly with persisted state; emitted only on success path
        env.events().publish(
            (symbol_short!("yldset"),),
            EscrowInterestYieldSetEvent {
                admin: admin.clone(),
                client_share_bps,
                freelancer_share_bps,
                locked: false,
            },
        );
        Ok(())
    }

    /// Update interest/yield share configuration with **dual consent**: both
    /// the client and the freelancer must independently authorize the
    /// transaction, in addition to the admin.
    ///
    /// `set_escrow_interest_yield` lets the platform admin set the split alone,
    /// but only before funding. After funding this is the only way to change
    /// it, so the parties' economics cannot be rewritten without them: a compromised or malicious admin key alone cannot move
    /// funds between client and freelancer here, because `client.require_auth()`
    /// and `freelancer.require_auth()` must both succeed. If either party's
    /// signature is missing from the transaction, the host-level auth check
    /// panics before any state is touched — a single-signature attempt never
    /// reaches the validation logic below, let alone mutates storage.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract admin key or job metadata missing.
    /// * `Unauthorized`   – `admin` does not match the stored admin.
    /// * `InvalidRatio`   – Shares do not sum to exactly 10 000 bps.
    /// * `EscrowLocked`   – Configuration is locked for execution.
    pub fn set_interest_yield_consent(
        env: Env,
        admin: Address,
        client_share_bps: u32,
        freelancer_share_bps: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        let meta = Self::load_job_meta(&env)?;

        // Both parties must independently sign this transaction. A missing
        // signature causes require_auth to panic at the host level.
        meta.client.require_auth();
        meta.freelancer.require_auth();

        Self::validate_interest_yield_share_config(client_share_bps, freelancer_share_bps)?;

        // Consolidate storage reads: load state once and check lock status
        // instead of calling has() followed by load_interest_yield_state().
        if let Ok(state) = Self::load_interest_yield_state(&env) {
            if state.locked {
                return Err(Error::EscrowLocked);
            }
        }

        Self::store_interest_yield_state(
            &env,
            &EscrowInterestYieldState {
                client_share_bps,
                freelancer_share_bps,
                locked: false,
            },
        );

        env.events().publish(
            (symbol_short!("yldcons"),),
            EscrowInterestYieldConsentSetEvent {
                admin,
                client: meta.client,
                freelancer: meta.freelancer,
                client_share_bps,
                freelancer_share_bps,
            },
        );

        Ok(())
    }

    /// Lock interest/yield share state during pending execution.
    ///
    /// Once locked, `set_escrow_interest_yield`, `set_interest_yield_consent`,
    /// and `interest_yield_split_refund` are all rejected until the admin, the
    /// client and the freelancer jointly call `unlock_escrow_interest_yield`.
    ///
    /// A `yldlock` / `EscrowInterestYieldLockedEvent` is published once the new
    /// state is durable so indexers and auditors get an immutable record of the
    /// lock without polling storage. The event mirrors the persisted state
    /// field-for-field: the two BPS values are the shares the lock froze and
    /// `locked` is `true`.
    ///
    /// ## Guards (issue #464)
    ///
    /// Both guards run before this function performs its single ledger write, in
    /// this order:
    ///
    /// 1. **Authorization** — `require_admin_from_instance` performs
    ///    `admin.require_auth()` (so a transaction that omits the admin
    ///    signature is rejected by the host before the contract body executes)
    ///    and then compares against the stored admin.  Being first, an
    ///    unauthorized caller learns nothing about the lock: it cannot
    ///    distinguish “already locked” from “not configured” on an escrow it
    ///    is not allowed to administer.
    /// 2. **Illegal source state** — an absent `InterestYieldState` entry is
    ///    `NotInitialized`, and a configuration that is *already* locked is
    ///    `InvalidStatus`.  Re-locking is refused rather than silently
    ///    no-opping, so an operator (or an indexer replaying the call) can never
    ///    mistake a redundant call for one that took fresh action.  This mirrors
    ///    the guard `unlock_escrow_interest_yield` applies to unlocking an
    ///    unlocked configuration, and is why this endpoint cannot reuse
    ///    `ensure_interest_yield_writable` — that helper deliberately maps an
    ///    absent state to “writable” so the first configuration write can
    ///    create it, whereas `lock` has no create path.
    ///
    /// Because the guards are exhaustive, the only way to reach the write is with
    /// an authenticated admin and a configured-but-unlocked configuration; every
    /// rejection path returns above the write and therefore leaves the ledger
    /// unchanged.
    ///
    /// ## Storage-footprint note
    ///
    /// Like `set_escrow_interest_yield`, this function uses
    /// `require_admin_from_instance` rather than the standard `require_admin`
    /// helper, so the admin verification read lands on the contract-instance
    /// entry every call loads anyway rather than adding the persistent `Admin`
    /// entry; the `InterestYieldState` read/write is on its own persistent key
    /// (issue #772).
    ///
    /// # Errors
    /// Listed in evaluation order; a call that violates more than one guard
    /// fails with the first match.
    /// * `NotInitialized` – Contract admin key or interest/yield state missing.
    /// * `Unauthorized`    – `admin` does not match the stored admin.
    /// * `InvalidStatus`   – The interest/yield configuration is already locked.
    pub fn lock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Guard 1 — authorization. `admin.require_auth()` inside the helper makes
        // the host reject a missing signature before this body runs at all; the
        // equality check then rejects a validly-signed non-admin.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        // Guard 2 — illegal source state. One read covers both cases: a missing
        // entry is `NotInitialized`, and an entry that is already locked is the
        // illegal source state this endpoint must refuse before writing.
        let mut state = Self::load_interest_yield_state(&env)?;
        if state.locked {
            return Err(Error::InvalidStatus);
        }

        state.locked = true;
        Self::store_interest_yield_state(&env, &state);

        // Emitted after the write so the payload can only describe state that
        // is already durable; no fallible step follows, so a success always
        // carries exactly one event and a failure carries none.
        env.events().publish(
            (symbol_short!("yldlock"),),
            EscrowInterestYieldLockedEvent {
                admin,
                client_share_bps: state.client_share_bps,
                freelancer_share_bps: state.freelancer_share_bps,
                locked: state.locked,
            },
        );

        Ok(())
    }

    /// Clear the execution lock so share configuration can be modified again.
    ///
    /// Unlocking requires the same consent as changing the shares through
    /// `set_interest_yield_consent`: the admin **and** both the client and the
    /// freelancer must sign (issue #776). Otherwise the admin could undo its own
    /// lock at will and the lock would commit to nothing.
    ///
    /// # Errors
    /// * `NotInitialized` - Contract admin key, job metadata or interest/yield
    ///   state missing.
    /// * `Unauthorized` - Caller is not the stored admin.
    /// * `InvalidStatus` - Interest/yield lock is not active (already unlocked).
    pub fn unlock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        Self::require_client_and_freelancer_consent(&env)?;
        let mut state = Self::load_interest_yield_state(&env)?;
        if !state.locked {
            return Err(Error::InvalidStatus);
        }
        state.locked = false;
        Self::store_interest_yield_state(&env, &state);
        // Publish structured event carrying acting address and resulting values
        // Reconciles exactly with persisted state; emitted only on success path
        env.events().publish(
            (symbol_short!("yldunlock"),),
            EscrowInterestYieldUnlockedEvent {
                admin: admin.clone(),
                client_share_bps: state.client_share_bps,
                freelancer_share_bps: state.freelancer_share_bps,
                locked: state.locked,
            },
        );
        Ok(())
    }

    /// Return whether the interest/yield share configuration is locked.
    pub fn is_escrow_interest_yield_locked(env: Env) -> Result<bool, Error> {
        Ok(Self::load_interest_yield_state(&env)?.locked)
    }

    /// Calculate a split-refund allocation between client and freelancer for
    /// yield/interest claims.
    ///
    /// Given a total amount and basis-point ratios for each party, this
    /// function computes how much should be refunded to the client and how
    /// much should be paid to the freelancer.  The ratios must sum to
    /// exactly `BPS_SCALE` (10 000).  The client share is rounded to the
    /// nearest stroop and the freelancer receives the exact remainder so no
    /// value is lost to integer division.
    ///
    /// # Parameters
    /// * `total_amount`          – Total amount to split; must be > 0.
    /// * `client_refund_bps`     – Client's refund share in basis points.
    /// * `freelancer_payout_bps` – Freelancer's payout share in basis points.
    ///
    /// # Returns
    /// A `RefundAllocation` whose two amounts sum to `total_amount` exactly.
    ///
    /// # Errors
    /// * `Paused`        – Contract is currently paused.
    /// * `EscrowLocked`  – Interest yield state is locked.
    /// * `InvalidRatio`  – Ratios do not sum to `BPS_SCALE`.
    /// * `InvalidAmount` – `total_amount` ≤ 0 or arithmetic overflow.
    pub fn interest_yield_split_refund(
        env: Env,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // Precondition: reject illegal source state if contract is paused.
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Precondition: reject illegal source state if interest yield state is locked.
        if Self::cold_has(&env, &DataKey::InterestYieldState) {
            Self::ensure_interest_yield_unlocked(&env)?;
        }

        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        let client_split =
            Self::split_round_nearest(total_amount, client_refund_bps as i128, BPS_SCALE as i128)?;

        let freelancer_payout = total_amount
            .checked_sub(client_split.first)
            .ok_or(Error::InvalidAmount)?;

        let allocation = RefundAllocation {
            client_refund: client_split.first,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        };

        env.events().publish(
            (symbol_short!("iyspltref"),),
            SplitRefundCalculatedEvent {
                client_refund: allocation.client_refund,
                freelancer_payout: allocation.freelancer_payout,
                client_refund_bps: allocation.client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
            },
        );

        Ok(allocation)
    }

    /// Return the stored interest/yield share configuration.
    ///
    /// # Read-only
    /// This is a pure query over instance storage: it delegates to
    /// `Self::load_interest_yield_state`, which performs exactly one `get` of
    /// `DataKey::InterestYieldState`. It must never `set`, `remove`, or
    /// `extend_ttl` any instance, persistent, or temporary entry, and must
    /// never publish an event — a read that wrote state could silently rewrite
    /// the ledger or shorten the TTL of the entry it inspects. It requires no
    /// authorization and is safe to call while the escrow is paused.
    ///
    /// INVARIANT: `get_escrow_interest_yield_no_mutation_tests` takes a full
    /// `Env::to_ledger_snapshot` immediately before and after the call — plus a
    /// per-tier fingerprint of instance, persistent, and temporary storage —
    /// and asserts the ledger is byte-identical, so any future edit that adds
    /// `.set(`, `.remove(`, `.extend_ttl(`, or `env.events().publish(` here or
    /// in the shared `load_interest_yield_state` helper fails CI. Do not add
    /// storage-writing calls to either function.
    ///
    /// # Returns
    /// * `Ok(EscrowInterestYieldState)` – the configuration last written by
    ///   `set_escrow_interest_yield` / `set_interest_yield_consent`, with
    ///   `locked` as last set by `lock_escrow_interest_yield` /
    ///   `unlock_escrow_interest_yield`. The two share fields satisfy
    ///   `client_share_bps + freelancer_share_bps == BPS_SCALE` (10_000),
    ///   because both writers validate that before storing.
    ///
    /// # Errors
    /// * `NotInitialized` – Configuration has never been set.
    pub fn get_escrow_interest_yield(env: Env) -> Result<EscrowInterestYieldState, Error> {
        Self::load_interest_yield_state(&env)
    }
}

// `all_event_tuples` below collects into a `std::vec::Vec`; this crate is
// `no_std`, so std has to be linked explicitly for the test build.
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

// ── escrow_interest_yield: admin emergency override endpoints ─────────────────
//
// Design rationale
// ─────────────────
// In rare operational conditions (e.g. a client or freelancer becoming
// unresponsive, a key being compromised, or yield accounting needing manual
// correction) the platform admin must be able to resolve a locked escrow
// without depending on the normal multi-party workflow.  These endpoints are
// intentionally narrow in scope:
//
//   • Every function requires a fresh `admin.require_auth()` and then verifies
//     the supplied address against the persisted `DataKey::Admin` value, so no
//     other address — including the client, freelancer, or arbiter — can ever
//     invoke them.
//
//   • Overrides are not gated on milestone status; the admin can act on a
//     milestone in ANY state (Pending, Delivered, PartiallyReleased, Disputed,
//     etc.) so that genuinely stuck escrows can always be resolved.
//
//   • Every action emits a structured on-chain event so that off-chain
//     indexers, auditors, and the parties involved receive an immutable record
//     of what happened and who authorised it.

// Deferred pending coordinated migration to #[contractevent] — see
// escrow-backend's poller.ts, which reads the current event wire format.
#[allow(deprecated)]
#[contractimpl]
impl MilestoneEscrow {
    // ── yield-rate management ─────────────────────────────────────────────────

    /// Set the annual yield rate for the escrow in basis points (1 bp = 0.01 %).
    ///
    /// # Parameters
    /// * `admin`       – Must match `DataKey::Admin`; a fresh signature is
    ///                   required on every call.
    /// * `rate_bps`    – New annual rate.  Capped at 10 000 (= 100 %).
    ///                   Pass `0` to disable yield accrual.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised yet (job metadata
    ///                      absent); checked before any auth or storage access.
    /// * `Paused`         – Contract is currently paused; yield-rate mutations
    ///                      are rejected while operations are suspended.
    /// * `Unauthorized`   – `admin` does not match the stored admin key.
    /// * `InvalidRatio`   – `rate_bps` exceeds 10 000.
    pub fn admin_set_yield_rate(env: Env, admin: Address, rate_bps: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Precondition 1: contract must be fully initialised (job metadata
        // must exist).  This check runs before any auth or storage mutation so
        // that callers on an uninitialised contract receive `NotInitialized`
        // rather than a less informative error.
        Self::load_job_meta(&env)?;

        // Precondition 2: reject calls while the contract is paused.  The
        // yield-rate is part of the active financial configuration; mutating
        // it while operations are suspended could silently affect the next
        // accrual cycle once the pause is lifted.
        //
        // Both pause flags count. `assert_not_paused` only reads
        // `DataKey::Paused`, which `admin_pause_escrow` sets; the emergency
        // pause is a separate, stronger freeze recorded under `DataKey::Ep`.
        // Letting a yield-rate change through during an emergency pause would
        // make the weaker of the two freezes the stricter one.
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Authorization: `require_admin` performs `admin.require_auth()` plus
        // the stored-key equality check.  Placed after the stateless
        // preconditions so that an unauthorized caller on an uninitialised or
        // paused contract receives the precondition error, not Unauthorized,
        // which would leak information about the admin key.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        Self::validate_yield_rate_bps(rate_bps)?;

        let old_rate_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldConfig)
            .map(|config: YieldConfig| config.yield_rate)
            .unwrap_or(0);

        env.storage().persistent().set(
            &DataKey::YieldConfig,
            &YieldConfig {
                yield_rate: rate_bps,
            },
        );

        env.events().publish(
            (symbol_short!("yldrate"),),
            YieldRateSetEvent {
                admin,
                old_rate_bps,
                new_rate_bps: rate_bps,
            },
        );

        Ok(())
    }

    /// Manually accrue interest for a specific milestone and record it in the
    /// running `YieldAccrued` total.
    ///
    /// The `accrued_amount` argument is the admin-specified interest figure for
    /// this accrual event (e.g. the result of an off-chain calculation).  It is
    /// added to the on-chain `YieldAccrued` accumulator via checked arithmetic
    /// to prevent overflow.
    ///
    /// ## Storage-footprint note
    ///
    /// This function authorizes through `require_admin_from_instance` rather
    /// than the standard `require_admin` helper, so the admin verification read
    /// (`DataKey::Admin`, instance) and the job-metadata read it performs right
    /// after (`DataKey::Job`, instance) share the **same single ledger entry**.
    ///
    /// That drops the separate persistent `DataKey::Admin` entry from the call:
    /// one invocation now touches two ledger entries — the instance entry
    /// (`Admin` + `Job`) and the persistent `YieldAccrued` accumulator — instead
    /// of three.  The instance copy of `DataKey::Admin` is a complete mirror of
    /// the persistent one: `initialize` writes both, and both admin-transfer
    /// paths (`transfer_admin` and `execute_admin_transfer`) keep them in sync,
    /// so authorizing against the instance copy cannot accept a stale admin.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Index of the milestone to which yield is attributed.
    /// * `accrued_amount`  – Interest amount to book; must be > 0.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidAmount`   – `accrued_amount` ≤ 0 or the running total would
    ///                       overflow `i128`.
    pub fn admin_accrue_yield(
        env: Env,
        admin: Address,
        milestone_index: u32,
        accrued_amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Authorize from instance storage so the admin read shares a ledger
        // entry with the `DataKey::Job` read below (see the rustdoc note).
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        let meta = Self::load_job_meta(&env)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        if accrued_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let current_total: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0);

        let new_total = current_total
            .checked_add(accrued_amount)
            .ok_or(Error::InvalidAmount)?;

        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &new_total);

        env.events().publish(
            (symbol_short!("yldacc"),),
            YieldAccruedEvent {
                admin,
                milestone_index,
                accrued_amount,
                total_accrued: new_total,
            },
        );

        Ok(())
    }

    // ── emergency override transfers ──────────────────────────────────────────

    /// Force-release a locked milestone directly to the freelancer, bypassing
    /// the normal `mark_delivered` → `approve_milestone` flow.
    ///
    /// This is the remedy for an escrow where normal settlement is impossible:
    /// the milestone must be `Disputed` (arbiter is unreachable or timed out),
    /// or the cancel lock must be active (`CancelLock == true`).  Using this
    /// outside those conditions — e.g. on a `Pending` or `Delivered` milestone
    /// that has no open dispute — returns `InvalidStatus`.
    ///
    /// The milestone is moved to `Released` and a full token transfer is
    /// executed to the freelancer.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `Paused`          – Contract is administratively paused.
    /// * `NotFunded`       – Escrow has not been funded; nothing to release.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`, or
    ///                       it is not in `Disputed` status and no cancel lock
    ///                       is active.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0, or the subtraction
    ///                       `amount − released_amount` overflows `i128`
    ///                       (e.g. when `released_amount > amount`).  All
    ///                       arithmetic uses checked operations so no input
    ///                       can cause a panic or silent integer wrap.
    pub fn admin_override_release(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Reject calls while the contract is administratively paused so that
        // a suspended escrow cannot have funds moved through the override path.
        Self::assert_not_paused(&env)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        // Terminal states have already settled funds — no double-spend.
        if Self::is_terminal_status(&milestone.status) {
            return Err(Error::InvalidStatus);
        }

        // Gate: only intervene when the normal settlement path is genuinely
        // blocked.  The milestone must be Disputed (arbiter is unreachable or
        // the dispute timed out without resolution), or the two-party cancel
        // lock must be active.  Overriding a Pending, Delivered, or
        // PartiallyReleased milestone that has no open dispute bypasses the
        // arbiter entirely and makes the admin a unilateral custodian.
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if milestone.status != MilestoneStatus::Disputed && !cancel_locked {
            return Err(Error::InvalidStatus);
        }

        // Use checked_sub so that any i128 overflow (e.g. released_amount >
        // amount, or extreme values such as i128::MIN / i128::MAX) returns
        // Error::InvalidAmount rather than panicking or wrapping silently.
        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);
        Self::store_milestone_released(&env, milestone_index);

        // Reset accrued yield on emergency override
        if env
            .storage()
            .persistent()
            .get::<_, i128>(&DataKey::YieldAccrued)
            .unwrap_or(0)
            != 0
        {
            env.storage()
                .persistent()
                .set(&DataKey::YieldAccrued, &0_i128);
        }

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.freelancer, &remaining)?;

        env.events().publish(
            (symbol_short!("admovrls"),),
            AdminOverrideReleaseEvent {
                admin,
                milestone_index,
                freelancer: meta.freelancer,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

    /// Force-refund a locked milestone back to the client, bypassing the normal
    /// dispute/resolution flow.
    ///
    /// Use this when normal settlement is impossible: the milestone must be
    /// `Disputed` (arbiter is unreachable or the dispute timed out), or the
    /// two-party cancel lock must be active.  Using this outside those
    /// conditions — e.g. on a `Pending` or `Delivered` milestone that has no
    /// open dispute — returns `InvalidStatus`.
    ///
    /// The milestone is moved to `Refunded` and a full token transfer is
    /// executed back to the client.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `Paused`          – Contract is paused.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`, or
    ///                       it is not in `Disputed` status and no cancel lock
    ///                       is active.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0, or the subtraction
    ///                       `amount − released_amount` overflows `i128`
    ///                       (e.g. when `released_amount > amount`).  All
    ///                       arithmetic uses checked operations so no input
    ///                       can cause a panic or silent integer wrap.
    pub fn admin_override_refund(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Reject illegal source state before any job/milestone ledger I/O.
        Self::assert_not_paused(&env)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        if Self::is_terminal_status(&milestone.status) {
            return Err(Error::InvalidStatus);
        }

        // Gate: only intervene when the normal settlement path is genuinely
        // blocked.  The milestone must be Disputed (arbiter is unreachable or
        // the dispute timed out without resolution), or the two-party cancel
        // lock must be active.  Overriding a Pending, Delivered, or
        // PartiallyReleased milestone that has no open dispute bypasses the
        // arbiter entirely and makes the admin a unilateral custodian.
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if milestone.status != MilestoneStatus::Disputed && !cancel_locked {
            return Err(Error::InvalidStatus);
        }

        // Use checked_sub so that any i128 overflow (e.g. released_amount >
        // amount, or extreme values such as i128::MIN / i128::MAX) returns
        // Error::InvalidAmount rather than panicking or wrapping silently.
        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call.
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

        // Reset accrued yield on emergency override — only write when the
        // stored value is non-zero to avoid an unnecessary ledger mutation.
        if env
            .storage()
            .persistent()
            .get::<_, i128>(&DataKey::YieldAccrued)
            .unwrap_or(0)
            != 0
        {
            env.storage()
                .persistent()
                .set(&DataKey::YieldAccrued, &0_i128);
        }

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.client, &remaining)?;

        env.events().publish(
            (symbol_short!("admovrf"),),
            AdminOverrideRefundEvent {
                admin,
                milestone_index,
                client: meta.client,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

    /// Emergency admin resolution for a milestone stuck in `Disputed` status,
    /// using the streaming/time-extension proportional split
    /// (`milestone_time_extensions` / `payment_streaming_milestones`) instead
    /// of the all-or-nothing `admin_override_release` / `admin_override_refund`.
    ///
    /// Intended for the case where the arbiter is unreachable and the normal
    /// `resolve_dispute` / `apply_dispute_arbitration_split` flow cannot
    /// proceed: the admin attests how much of an extension window
    /// (`elapsed_seconds` of `total_seconds`) had elapsed and the remaining
    /// balance is split proportionally between freelancer and client in a
    /// single settlement.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone; must currently be `Disputed`.
    /// * `elapsed_seconds` – Portion of the extension window already elapsed.
    /// * `total_seconds`   – Full length of the extension window.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is not currently `Disputed`.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0, or arithmetic overflow.
    /// * `InvalidRatio`    – `total_seconds` ≤ 0, or `elapsed_seconds` is
    ///                       negative or exceeds `total_seconds`.
    pub fn admin_override_streaming_release(
        env: Env,
        admin: Address,
        milestone_index: u32,
        elapsed_seconds: i128,
        total_seconds: i128,
    ) -> Result<RatioSplit, Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if milestone.status != MilestoneStatus::Disputed {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let split = Self::time_extension_split(&env, remaining, elapsed_seconds, total_seconds)?;

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = Self::available_balance(&env, &token_client);
        if contract_balance < remaining {
            return Err(Error::InsufficientBalance);
        }

        let client_refund = split.second;
        let freelancer_payout = split.first;

        if client_refund > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.client, &client_refund)?;
        }
        if freelancer_payout > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.freelancer, &freelancer_payout)?;
        }

        milestone.released_amount = milestone
            .released_amount
            .checked_add(freelancer_payout)
            .ok_or(Error::InvalidAmount)?;

        if freelancer_payout == 0 {
            milestone.status = MilestoneStatus::Refunded;
        } else if client_refund > 0 {
            milestone.status = MilestoneStatus::Split;
        } else {
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        }

        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

        env.events().publish(
            (symbol_short!("admstrm"),),
            AdminOverrideStreamingReleaseEvent {
                admin,
                milestone_index,
                client: meta.client,
                freelancer: meta.freelancer,
                token: meta.token,
                client_refund,
                freelancer_payout,
            },
        );

        Ok(RatioSplit {
            first: freelancer_payout,
            second: client_refund,
        })
    }

    // ── pause / resume ────────────────────────────────────────────────────────

    /// Pause the escrow, blocking all normal user-facing endpoints.
    ///
    /// After this call, `fund`, `mark_delivered`, `approve_milestone`,
    /// `approve_partial`, `claim_auto_release`, `raise_dispute`, and
    /// `resolve_dispute` all return `EscrowPaused` until the admin calls
    /// `admin_resume_escrow`.  Admin-prefixed endpoints (including this one)
    /// remain fully operational during a pause.
    ///
    /// Calling this on an already-paused escrow is rejected with `AlreadyPaused`
    /// rather than silently succeeding, so an operator cannot mistake a
    /// redundant call for having taken fresh action.
    ///
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    /// * `AlreadyPaused`  – The escrow is already paused.
    /// * `EmergencyPauseInProgress` – A pause/resume transition is already
    ///   in flight; the escrow cannot be re-paused mid-transition.
    pub fn admin_pause_escrow(env: Env, admin: Address) -> Result<(), Error> {
        // Authorization: only the stored admin may pause the escrow.  Any
        // other caller is rejected with `Unauthorized`, and a contract that
        // has never been initialised with `NotInitialized`, before any ledger
        // entry is read or written.
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        // Precondition: reject an illegal source state — an emergency
        // pause/resume transition already in flight — before any write.
        Self::assert_emergency_pause_not_locked(&env)?;

        // Reject redundant pause calls so an operator cannot mistake a
        // redundant call for having taken fresh action.
        let already_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if already_paused {
            return Err(Error::AlreadyPaused);
        }

        env.storage().instance().set(&DataKey::Paused, &true);

        env.events()
            .publish((symbol_short!("pause"),), EscrowPausedEvent { admin });

        Ok(())
    }

    /// Resume a previously paused escrow, re-enabling all normal user-facing
    /// endpoints.
    ///
    /// Resuming an escrow that is not paused is rejected with `NotPaused`
    /// rather than silently succeeding, so a mistaken call is visible instead
    /// of reading as a completed recovery.
    ///
    /// # Checks (in order)
    /// 1. `admin.require_auth()` — SDK-level signature check.
    /// 2. `require_admin_from_instance` — verified admin key matches the
    ///    instance copy of `DataKey::Admin` (see the storage-footprint note
    ///    on the function body).
    /// 3. Contract must currently be paused (`NotPaused`).
    /// 4. No pause transition may already be mid-execution
    ///    (`EmergencyPauseInProgress`).
    ///
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin`.
    ///
    /// # Errors
    /// * `NotInitialized`           – Contract has not been initialised.
    /// * `Unauthorized`             – `admin` is not the stored admin.
    /// * `NotPaused`                – The escrow is not currently paused.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    pub fn admin_resume_escrow(env: Env, admin: Address) -> Result<(), Error> {
        // Storage-footprint note (issue #449): authorize against the *instance*
        // copy of `DataKey::Admin` via `require_admin_from_instance` instead of
        // probing the persistent copy twice (`has` + `load_admin`'s `get`).
        // `initialize` writes both copies atomically and every admin-transfer
        // path keeps them in sync, so this is the same logical check — but now
        // the admin read and every `Paused` / `EpLk` read and write below land
        // on the single instance ledger entry, reducing the call from two
        // distinct ledger entries touched to one.
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        let currently_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if !currently_paused {
            return Err(Error::NotPaused);
        }

        Self::assert_emergency_pause_not_locked(&env)?;

        env.storage().instance().set(&DataKey::EpLk, &true);

        let result = (|| {
            env.storage().instance().set(&DataKey::Paused, &false);

            env.events().publish(
                (symbol_short!("resume"),),
                EscrowResumedEvent {
                    admin: admin.clone(),
                },
            );

            Ok(())
        })();

        env.storage().instance().set(&DataKey::EpLk, &false);

        result
    }

    // ── tax recipient configuration (issue #788) ──────────────────────────────

    /// Configure the address that receives withheld tax.
    ///
    /// Must be in place **before** `tax_withholding_deductions` records a
    /// withholding — that endpoint refuses without a recipient
    /// (`InvalidStatus`), so a `tax_amount` can never be created that has no
    /// destination to be settled to.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract not initialised.
    /// * `Unauthorized`    – Caller is not the stored admin.
    /// * `InvalidAddress`  – `recipient` is a zero address.
    pub fn set_tax_recipient(env: Env, admin: Address, recipient: Address) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if recipient == zero_account || recipient == zero_contract {
            return Err(Error::InvalidAddress);
        }

        env.storage()
            .persistent()
            .set(&DataKey::TaxRecipient, &recipient);

        env.events().publish(
            (symbol_short!("taxrcpt"),),
            TaxRecipientSetEvent { admin, recipient },
        );

        Ok(())
    }

    /// Return the configured tax recipient, if any (issue #788).
    ///
    /// Pure read: no authorization, no writes — safe for indexers and UIs to
    /// poll.  `None` means no recipient has ever been configured (or the
    /// stored value was cleared), in which case `tax_withholding_deductions`
    /// will refuse to withhold.
    pub fn get_tax_recipient(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::TaxRecipient)
    }

    /// Resolve the address that must receive milestone `milestone_index`'s
    /// withheld tax when a settlement path runs (issue #788): the recipient
    /// captured when the tax was withheld, falling back to the current global
    /// configuration for records written before that capture existed.
    ///
    /// With neither present the tax has no destination, so the caller fails
    /// with `InvalidStatus` instead of paying a party that was never entitled
    /// to it or stranding the amount inside the contract.
    ///
    /// **This function must contain only read operations.**
    fn tax_settlement_recipient(env: &Env, milestone_index: u32) -> Result<Address, Error> {
        if let Some(recipient) = env
            .storage()
            .persistent()
            .get::<DataKey, Address>(&DataKey::TaxWithholdingRecipient(milestone_index))
        {
            return Ok(recipient);
        }
        env.storage()
            .persistent()
            .get(&DataKey::TaxRecipient)
            .ok_or(Error::InvalidStatus)
    }

    /// Publish the additive `taxpaid` event for a non-zero tax transfer
    /// (issue #788).  Kept in one place so all three settlement paths record
    /// the payout identically; a nil withholding moves nothing and therefore
    /// publishes nothing.
    fn publish_tax_paid(
        env: &Env,
        milestone_index: u32,
        recipient: &Address,
        token: &Address,
        tax_amount: i128,
    ) {
        if tax_amount > 0 {
            env.events().publish(
                (symbol_short!("taxpaid"),),
                TaxPaidEvent {
                    milestone_index,
                    recipient: recipient.clone(),
                    token: token.clone(),
                    tax_amount,
                },
            );
        }
    }

    // ── tax_withholding_deductions ────────────────────────────────────────────

    /// The single tax-withholding calculation behind both
    /// `tax_withholding_deductions` and `admin_tax_withholding_deductions`.
    ///
    /// Taxes the milestone's **remaining** balance (`amount - released_amount`)
    /// with round-to-nearest (`split_round_nearest`), and only for milestones
    /// still in `Pending` / `Delivered` / `PartiallyReleased`. Reads only; the
    /// caller decides whether to persist or emit.
    ///
    /// # Errors
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidRatio`     – `tax_rate_bps > 10_000`.
    /// * `InvalidStatus`    – milestone is Released, Refunded, Split or Disputed.
    /// * `InvalidAmount`    – empty contract balance, no remaining balance, or
    ///                        arithmetic overflow.
    fn compute_tax_withholding(
        env: &Env,
        meta: &JobMeta,
        milestone_index: u32,
        tax_rate_bps: u32,
    ) -> Result<TaxWithholdingRecord, Error> {
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(env, milestone_index)?;

        // Only Pending/Delivered/PartiallyReleased milestones may have tax
        // withheld — mirrors the state machine `raise_dispute_inner` and
        // `resolve_dispute` already enforce elsewhere in this file. A
        // Disputed milestone's funds are frozen pending arbitration. An
        // exhaustive match means a future new `MilestoneStatus` variant fails
        // to compile here instead of silently falling through as allowed.
        match milestone.status {
            MilestoneStatus::Pending
            | MilestoneStatus::Delivered
            | MilestoneStatus::PartiallyReleased => {}
            MilestoneStatus::Released
            | MilestoneStatus::Refunded
            | MilestoneStatus::Split
            | MilestoneStatus::Disputed => return Err(Error::InvalidStatus),
        }

        let token_client = token::Client::new(env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        let gross_amount = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if gross_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if tax_rate_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        // tax_amount = round_nearest(gross × rate / 10_000)
        let tax_split =
            Self::split_round_nearest(gross_amount, tax_rate_bps as i128, BPS_SCALE as i128)?;

        Ok(TaxWithholdingRecord {
            gross_amount,
            tax_amount: tax_split.first,
            net_amount: tax_split.second,
            tax_rate_bps,
        })
    }

    /// Compute and record a tax withholding deduction for a specific milestone.
    ///
    /// Calculates the tax owed on the milestone's remaining gross balance using
    /// the supplied `tax_rate_bps`, writes the result to
    /// `DataKey::TaxWithholdingLock(milestone_index)` and emits an event.
    /// Both the client and freelancer must authorize the calculation.
    /// The milestone is left in its current state so the normal approval flow
    /// remains intact; the admin override endpoints read the stored record to
    /// resolve any locked condition.
    ///
    /// # Parameters
    /// * `milestone_index` – Target milestone (must be in a non-terminal status).
    /// * `tax_rate_bps`    – Tax rate in basis points (0–10 000).  Zero is
    ///                       accepted and records a nil withholding.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract not initialised.
    /// * `NotFunded`        – Escrow not yet funded.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – Milestone is not Pending/Delivered/PartiallyReleased
    ///                        (i.e. it is Released, Refunded, or Disputed —
    ///                        disputed funds are frozen pending `resolve_dispute`).
    /// * `InvalidRatio`     – `tax_rate_bps > 10_000`.
    /// * `InvalidAmount`    – Remaining balance is zero or arithmetic overflow.
    pub fn tax_withholding_deductions(
        env: Env,
        milestone_index: u32,
        tax_rate_bps: u32,
    ) -> Result<TaxWithholdingRecord, Error> {
        Self::ensure_migrated(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        let meta = Self::load_job_meta(&env)?;

        meta.client.require_auth();
        meta.freelancer.require_auth();

        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        if tax_rate_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        let record = Self::compute_tax_withholding(&env, &meta, milestone_index, tax_rate_bps)?;
        let TaxWithholdingRecord {
            gross_amount,
            tax_amount,
            net_amount,
            ..
        } = record.clone();

        // Issue #788: a withholding cannot be created without a destination
        // that will later receive `tax_amount`.  At withholding time only the
        // global `DataKey::TaxRecipient` must be present (the per-milestone
        // capture is written below).  Without it, refuse with `InvalidStatus`
        // rather than stranding funds.  A zero-tax record strands nothing, so
        // it does not need a recipient.
        if tax_amount > 0
            && env
                .storage()
                .persistent()
                .get::<DataKey, Address>(&DataKey::TaxRecipient)
                .is_none()
        {
            return Err(Error::InvalidStatus);
        }

        // Persist the record so admin override endpoints can read it without
        // recomputing tax arithmetic.
        env.storage()
            .persistent()
            .set(&DataKey::TaxWithholdingLock(milestone_index), &record);

        // Capture the recipient that will receive this tax (issue #788).
        // Reading the global recipient at this point is fine, since we just
        // rejected if it were absent; persist a per-milestone snapshot so
        // settlement paths can resolve the recipient even if it is later
        // changed (and to cover legacy records whose per-milestone key was
        // never written, the settlement lookup falls back to the global one).
        if let Some(recipient) = env
            .storage()
            .persistent()
            .get::<DataKey, Address>(&DataKey::TaxRecipient)
        {
            env.storage().persistent().set(
                &DataKey::TaxWithholdingRecipient(milestone_index),
                &recipient,
            );
        }

        env.events().publish(
            (symbol_short!("taxwith"),),
            TaxWithholdingAppliedEvent {
                milestone_index,
                gross_amount,
                tax_amount,
                net_amount,
                tax_rate_bps,
            },
        );

        Ok(record)
    }

    /// Refund distribution pathway for a **split-refund claim against a
    /// tax-withheld amount**.
    ///
    /// `tax_withholding_deductions` reduces a milestone's remaining gross to a
    /// single `net_amount`, and `admin_override_tax_release` hands that whole
    /// net amount to the freelancer.  Neither endpoint answers the question a
    /// *split* claim asks: if the escrow is unwinding and the withheld balance
    /// has to be shared, how much of it does each party actually receive?
    ///
    /// # Algorithm — proportional withholding
    ///
    /// The tax is attributed in the same ratio as the gross it was taken from,
    /// so each party bears its own share of the withholding rather than the tax
    /// landing entirely on one side:
    ///
    /// ```text
    /// client_gross      = round_nearest(gross × client_bps / 10_000)
    /// client_tax        = round_nearest(tax   × client_bps / 10_000)
    /// client_refund     = client_gross − client_tax
    /// freelancer_gross  = gross − client_gross
    /// freelancer_tax    = tax   − client_tax
    /// freelancer_payout = freelancer_gross − freelancer_tax
    /// ```
    ///
    /// # Conservation invariants
    ///
    /// * `client_refund + freelancer_payout == gross − tax` exactly — every
    ///   stroop that survives withholding is attributed, and none is created or
    ///   destroyed by the two independent roundings.
    /// * Both legs are `>= 0`: a party's share of the tax can never exceed its
    ///   share of the gross, because `tax <= gross` and both are apportioned by
    ///   the same `round_nearest` pass over the same ratio.
    /// * Every subtraction goes through `i128::checked_sub`, and every
    ///   multiplication through `i128::checked_mul` (inside
    ///   [`Self::split_round_nearest`]), so an out-of-range input yields a
    ///   typed error rather than a panic or a wrapped amount.
    ///
    /// [`Self::split_round_nearest`]: Self::split_round_nearest
    ///
    /// # Parameters
    /// * `gross_amount`          – Pre-tax balance. Must be > 0.
    /// * `tax_amount`            – Amount withheld out of `gross_amount`. Must
    ///                              satisfy `0 <= tax_amount <= gross_amount`.
    /// * `client_refund_bps`     – Client's share in basis points (0–10 000).
    /// * `freelancer_payout_bps` – Freelancer's share in basis points
    ///                              (0–10 000); the two must sum to `BPS_SCALE`.
    ///
    /// # Returns
    /// A [`RefundAllocation`] whose `client_refund` and `freelancer_payout` are
    /// both **post-tax** amounts summing to `gross_amount − tax_amount`.
    ///
    /// # Errors
    /// * `InvalidAmount` – `gross_amount` ≤ 0, `tax_amount` < 0,
    ///   `tax_amount > gross_amount`, or arithmetic overflow.
    /// * `InvalidRatio`  – `client_refund_bps + freelancer_payout_bps ≠ 10_000`,
    ///   or the `u32` addition overflows.
    fn allocate_withholding_refund(
        gross_amount: i128,
        tax_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        if gross_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if tax_amount < 0 || tax_amount > gross_amount {
            return Err(Error::InvalidAmount);
        }

        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        // Apportion the gross and the withheld tax over the same ratio so both
        // legs of the split carry the same rounding direction.
        let client_gross =
            Self::split_round_nearest(gross_amount, client_refund_bps as i128, BPS_SCALE as i128)?
                .first;
        let client_tax =
            Self::split_round_nearest(tax_amount, client_refund_bps as i128, BPS_SCALE as i128)?
                .first;

        let freelancer_gross = gross_amount
            .checked_sub(client_gross)
            .ok_or(Error::InvalidAmount)?;
        let freelancer_tax = tax_amount
            .checked_sub(client_tax)
            .ok_or(Error::InvalidAmount)?;

        let client_refund = client_gross
            .checked_sub(client_tax)
            .ok_or(Error::InvalidAmount)?;
        let freelancer_payout = freelancer_gross
            .checked_sub(freelancer_tax)
            .ok_or(Error::InvalidAmount)?;

        Ok(RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        })
    }

    /// Calculate the split-refund distribution pathway for a tax-withheld
    /// amount: how much of `gross_amount − tax_amount` the client receives and
    /// how much the freelancer receives, with the withheld tax attributed to
    /// both parties in the same ratio it was taken in.
    ///
    /// This is a pure calculator — it moves no tokens and writes no ledger
    /// entry — so it can be used to preview a split-refund claim against a
    /// withheld balance before anything is committed.  It is the calculator
    /// counterpart of `Self::allocate_withholding_refund`, which holds the
    /// arithmetic.
    ///
    /// Emits [`TaxWithholdingSplitRefundEvent`] on success carrying the gross,
    /// tax, net and both post-tax legs.
    ///
    /// # Parameters
    /// * `gross_amount`          – Pre-tax balance. Must be > 0.
    /// * `tax_amount`            – Withheld amount. Must satisfy
    ///                              `0 <= tax_amount <= gross_amount`.
    /// * `client_refund_bps`     – Client's share in basis points (0–10 000).
    /// * `freelancer_payout_bps` – Freelancer's share in basis points
    ///                              (0–10 000); the two must sum to 10 000.
    ///
    /// # Returns
    /// A [`RefundAllocation`] with:
    /// * `client_refund`         = the client's post-tax share,
    /// * `freelancer_payout`     = the freelancer's post-tax share,
    /// * `client_refund_bps`     = echoed input,
    /// * `freelancer_payout_bps` = echoed input.
    ///
    /// `client_refund + freelancer_payout` always equals
    /// `gross_amount − tax_amount` exactly.
    ///
    /// # Errors
    /// * `InvalidAmount` – `gross_amount` ≤ 0, `tax_amount` < 0,
    ///   `tax_amount > gross_amount`, or arithmetic overflow.
    /// * `InvalidRatio`  – `client_refund_bps + freelancer_payout_bps ≠ 10_000`,
    ///   or the `u32` addition overflows.
    pub fn tax_withholding_split_refund(
        env: Env,
        gross_amount: i128,
        tax_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        let allocation = Self::allocate_withholding_refund(
            gross_amount,
            tax_amount,
            client_refund_bps,
            freelancer_payout_bps,
        )?;

        let net_amount = gross_amount
            .checked_sub(tax_amount)
            .ok_or(Error::InvalidAmount)?;

        env.events().publish(
            (symbol_short!("twspltref"),),
            TaxWithholdingSplitRefundEvent {
                gross_amount,
                tax_amount,
                net_amount,
                client_refund: allocation.client_refund,
                freelancer_payout: allocation.freelancer_payout,
                client_refund_bps: allocation.client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
            },
        );

        Ok(allocation)
    }

    /// Admin emergency override: resolve a tax-locked milestone by releasing
    /// the net (post-tax) amount to the freelancer.
    ///
    /// Reads the `TaxWithholdingRecord` written by `tax_withholding_deductions`,
    /// transfers `net_amount` to the freelancer, marks the milestone `Released`,
    /// and removes the lock entry.
    ///
    /// Only the verified admin key can call this function.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract not initialised.
    /// * `Unauthorized`     – Caller is not the verified admin.
    /// * `NotFunded`        – Escrow not yet funded.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – No tax-withholding lock exists for this milestone,
    ///                        or the milestone is already terminal.
    /// * `InvalidAmount`    – Net amount is zero.
    pub fn admin_override_tax_release(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_initialized(&env)?;

        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let record: TaxWithholdingRecord = env
            .storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(milestone_index))
            .ok_or(Error::InvalidStatus)?;

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if Self::is_terminal_status(&milestone.status) {
            return Err(Error::InvalidStatus);
        }
        if record.net_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Remove the lock entry.
        env.storage()
            .persistent()
            .remove(&DataKey::TaxWithholdingLock(milestone_index));

        let token_client = token::Client::new(&env, &meta.token);
        // Transfer net to freelancer and tax to the configured recipient (issue #788).
        Self::pay_or_credit(&env, &token_client, &meta.freelancer, &record.net_amount)?;
        if record.tax_amount > 0 {
            let recipient = Self::tax_settlement_recipient(&env, milestone_index)?;
            Self::pay_or_credit(&env, &token_client, &recipient, &record.tax_amount)?;
            Self::publish_tax_paid(
                &env,
                milestone_index,
                &recipient,
                &meta.token,
                record.tax_amount,
            );
        }

        env.events().publish(
            (symbol_short!("adtxrls"),),
            AdminOverrideTaxReleaseEvent {
                admin,
                milestone_index,
                freelancer: meta.freelancer,
                token: meta.token,
                net_amount: record.net_amount,
                tax_amount: record.tax_amount,
            },
        );

        Ok(())
    }

    /// Admin emergency override: resolve a tax-locked milestone by refunding
    /// the client **net** of withheld tax (`gross − tax`) and paying
    /// `tax_amount` to the configured tax recipient (issue #788).
    ///
    /// Reads the `TaxWithholdingRecord`, transfers `gross_amount - tax_amount`
    /// to the client, transfers `tax_amount` to the tax recipient, marks the
    /// milestone `Refunded`, and removes the lock entry.  This preserves the
    /// invariant that `client refund + freelancer payout + tax payment = gross`
    /// in settlement (and here freelancer receives nothing, so
    /// `client_net + tax = gross - tax + tax = gross`).
    ///
    /// Only the verified admin key can call this function.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract not initialised.
    /// * `Unauthorized`     – Caller is not the verified admin.
    /// * `NotFunded`        – Escrow not yet funded.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – No tax-withholding lock exists for this milestone,
    ///                        or the milestone is already terminal; also returned
    ///                        if no tax recipient can be resolved (issue #788).
    /// * `InvalidAmount`    – Gross amount is zero.
    pub fn admin_override_tax_refund(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        let record: TaxWithholdingRecord = env
            .storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(milestone_index))
            .ok_or(Error::InvalidStatus)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if Self::is_terminal_status(&milestone.status) {
            return Err(Error::InvalidStatus);
        }
        if record.gross_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call.
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Remove the lock entry.
        env.storage()
            .persistent()
            .remove(&DataKey::TaxWithholdingLock(milestone_index));

        let token_client = token::Client::new(&env, &meta.token);
        let client_net = record.gross_amount - record.tax_amount;
        if client_net > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.client, &client_net)?;
        }
        if record.tax_amount > 0 {
            let recipient = Self::tax_settlement_recipient(&env, milestone_index)?;
            Self::pay_or_credit(&env, &token_client, &recipient, &record.tax_amount)?;
            Self::publish_tax_paid(
                &env,
                milestone_index,
                &recipient,
                &meta.token,
                record.tax_amount,
            );
        }

        env.events().publish(
            (symbol_short!("adtxrfd"),),
            AdminOverrideTaxRefundEvent {
                admin,
                milestone_index,
                client: meta.client,
                token: meta.token,
                gross_amount: record.gross_amount,
            },
        );

        Ok(())
    }

    /// Admin emergency override: resolve a tax-locked milestone by paying a
    /// **split** of the withheld balance to both parties, each net of their own
    /// share of the withheld tax.
    ///
    /// This is the settlement counterpart of
    /// `Self::tax_withholding_split_refund`: the calculator decides the
    /// distribution, this function moves the money. The gross and the withheld
    /// tax are apportioned over the same ratio, so the two transfers always add
    /// up to exactly `gross_amount − tax_amount` and the tax is never paid out
    /// twice.
    ///
    /// Only the verified admin key can call this function.
    ///
    /// # Parameters
    /// * `admin`                – Verified admin address (authorized caller).
    /// * `milestone_index`      – Milestone whose tax lock is being settled.
    /// * `client_refund_bps`    – Client's share in basis points.
    /// * `freelancer_payout_bps`– Freelancer's share in basis points; the two
    ///                             must sum to `BPS_SCALE`.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract not initialised.
    /// * `Unauthorized`     – Caller is not the verified admin.
    /// * `NotFunded`        – Escrow not yet funded.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – No tax-withholding lock exists for this milestone,
    ///                        or the milestone is already terminal.
    /// * `InvalidAmount`    – Gross amount is zero, or arithmetic overflow.
    /// * `InvalidRatio`     – The two BPS values do not sum to `BPS_SCALE`.
    pub fn admin_override_tax_split_refund(
        env: Env,
        admin: Address,
        milestone_index: u32,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        let record: TaxWithholdingRecord = env
            .storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(milestone_index))
            .ok_or(Error::InvalidStatus)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if Self::is_terminal_status(&milestone.status) {
            return Err(Error::InvalidStatus);
        }
        if record.gross_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Ratio is validated (and the overflow-probed) before any state is
        // committed, so an illegal ratio leaves the tax lock exactly as it was.
        let allocation = Self::allocate_withholding_refund(
            record.gross_amount,
            record.tax_amount,
            client_refund_bps,
            freelancer_payout_bps,
        )?;

        // CEI: commit state before any external call.
        milestone.released_amount = milestone
            .released_amount
            .checked_add(allocation.client_refund)
            .and_then(|sum| sum.checked_add(allocation.freelancer_payout))
            .ok_or(Error::InvalidAmount)?;
        milestone.status = if allocation.freelancer_payout == 0 {
            MilestoneStatus::Refunded
        } else {
            MilestoneStatus::Released
        };
        Self::store_milestone(&env, milestone_index, &milestone);

        // Remove the lock entry.
        env.storage()
            .persistent()
            .remove(&DataKey::TaxWithholdingLock(milestone_index));

        let token_client = token::Client::new(&env, &meta.token);
        if allocation.client_refund > 0 {
            Self::pay_or_credit(&env, &token_client, &meta.client, &allocation.client_refund)?;
        }
        if allocation.freelancer_payout > 0 {
            Self::pay_or_credit(
                &env,
                &token_client,
                &meta.freelancer,
                &allocation.freelancer_payout,
            )?;
        }
        if record.tax_amount > 0 {
            let recipient = Self::tax_settlement_recipient(&env, milestone_index)?;
            Self::pay_or_credit(&env, &token_client, &recipient, &record.tax_amount)?;
            Self::publish_tax_paid(
                &env,
                milestone_index,
                &recipient,
                &meta.token,
                record.tax_amount,
            );
        }

        env.events().publish(
            (symbol_short!("adtwsplt"),),
            AdminOverrideTaxSplitRefundEvent {
                admin,
                milestone_index,
                client: meta.client,
                freelancer: meta.freelancer,
                token: meta.token,
                gross_amount: record.gross_amount,
                tax_amount: record.tax_amount,
                client_refund: allocation.client_refund,
                freelancer_payout: allocation.freelancer_payout,
                client_refund_bps: allocation.client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
            },
        );

        Ok(())
    }

    // ── read-only query ───────────────────────────────────────────────────────

    /// Return a snapshot of the current yield and pause state.
    ///
    /// All fields are safe to call even before any admin has set a yield rate
    /// (defaults to zero) or paused the contract (defaults to `false`).
    ///
    /// # Read-only
    /// This is a pure query: it only *reads* the persistent `DataKey::YieldConfig`
    /// and `DataKey::YieldAccrued` entries and the instance `DataKey::Paused`
    /// entry. It must never `set`, `remove`, or `extend_ttl` any instance,
    /// persistent, or temporary entry, and must never publish an event — a
    /// read that wrote state could silently rewrite the ledger or shorten the
    /// TTL of the entries it inspects. `get_yield_info_no_mutation_tests` pins
    /// that guarantee by taking a full `Env::to_ledger_snapshot` immediately
    /// before and after the call and asserting the two are byte-identical.
    ///
    /// # Returns `(rate_bps, total_accrued, is_paused)`
    ///
    /// | Field           | Type   | Description                                   |
    /// |-----------------|--------|-----------------------------------------------|
    /// | `rate_bps`      | `u32`  | Current annual yield rate in basis points.    |
    /// | `total_accrued` | `i128` | Cumulative yield booked via `admin_accrue_yield`. |
    /// | `is_paused`     | `bool` | Whether normal operations are currently paused. |
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    pub fn get_yield_info(env: Env) -> Result<(u32, i128, bool), Error> {
        // Verify the contract is initialized before returning state.
        Self::load_job_meta(&env)?;

        let rate_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldConfig)
            .map(|config: YieldConfig| config.yield_rate)
            .unwrap_or(0);

        let total_accrued: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0);

        let is_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);

        Ok((rate_bps, total_accrued, is_paused))
    }

    /// Calculate tax withholding deductions for a milestone payout (admin).
    ///
    /// Admin-gated, read-only counterpart of `tax_withholding_deductions`: it
    /// runs the same shared calculation (`compute_tax_withholding` — remaining
    /// balance, round-to-nearest, same status checks) so both entry points
    /// report identical numbers, but it does not persist a
    /// `TaxWithholdingRecord` and emits `TaxWithholdingDeductionsEvent`.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone for tax calculation.
    /// * `tax_rate_bps`    – Tax rate in basis points (1 bp = 0.01 %).
    ///                       Must be ≤ 10 000 (≤ 100 %).
    ///
    /// # Returns
    /// `(gross_amount, tax_amount, net_amount)` reflecting the split.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract has not been initialised.
    /// * `Unauthorized`     – `admin` does not match the stored admin key.
    /// * `NotFunded`        – Escrow has not been funded.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidRatio`     – `tax_rate_bps` exceeds 10 000.
    /// * `InvalidStatus`    – Milestone is Released, Refunded, Split or Disputed.
    /// * `InvalidAmount`    – No remaining balance, empty contract balance or overflow.
    pub fn admin_tax_withholding_deductions(
        env: Env,
        admin: Address,
        milestone_index: u32,
        tax_rate_bps: u32,
    ) -> Result<(i128, i128, i128), Error> {
        // Authorization: only the stored admin may invoke this endpoint.  Any
        // caller that is not the stored admin is rejected with `Unauthorized`,
        // and a contract that has never been initialised is rejected with
        // `NotInitialized`, before any ledger entry is read or written.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;

        // Precondition guards: reject illegal source states (out-of-range
        // milestone, non-positive milestone amount, tax rate above full scale,
        // empty contract balance) with their specific typed error before any
        // ledger entry is written.
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let TaxWithholdingRecord {
            gross_amount,
            tax_amount,
            net_amount,
            ..
        } = Self::compute_tax_withholding(&env, &meta, milestone_index, tax_rate_bps)?;

        // Emit structured event for indexers.
        env.events().publish(
            (symbol_short!("taxwh"),),
            TaxWithholdingDeductionsEvent {
                admin,
                milestone_index,
                gross_amount,
                tax_amount,
                net_amount,
                tax_rate_bps,
            },
        );

        Ok((gross_amount, tax_amount, net_amount))
    }
}

// ── multisig_approval: admin emergency override & split-refund endpoints ────
//
// Design rationale
// ─────────────────
// In multi-signature escrow workflows, deadlocks can arise when one or more
// signers become unresponsive or keys are compromised.  These endpoints give
// the platform admin the ability to resolve locked multisig conditions
// unilaterally while emitting immutable on-chain events for auditability.
//
//   • Every admin function requires a fresh `admin.require_auth()` and then
//     verifies the supplied address against `DataKey::Admin`, so no other
//     address can invoke them.
//
//   • The `multisig_split_refund` helper implements refund distribution
//     pathways for split-refund claims, returning a `RefundAllocation`
//     struct that downstream code can use to execute proportional transfers
//     between client and freelancer.
//
//   • Every action emits a structured on-chain event so that off-chain
//     indexers, auditors, and the parties involved receive an immutable record.

// Deferred pending coordinated migration to #[contractevent] — see
// escrow-backend's poller.ts, which reads the current event wire format.
#[allow(deprecated)]
#[contractimpl]
impl MilestoneEscrow {
    // ── emergency multisig overrides ──────────────────────────────────────────

    /// Force-release a multisig-locked milestone directly to the freelancer.
    ///
    /// Use this when a multisig approval workflow is deadlocked (e.g. a
    /// required signer is unresponsive) and the admin must resolve the
    /// escrow without depending on the normal multi-party approval flow.
    /// The milestone is moved to `Released` and a full token transfer is
    /// executed to the freelancer.  The `MultisigLocked` flag is cleared.
    ///
    /// # Checks (in order)
    /// 1. `admin.require_auth()` — SDK-level signature check.
    /// 2. `require_admin` — verified admin key matches `DataKey::Admin`.
    /// 3. `MultisigLocked` must be active (`InvalidStatus`).
    /// 4. Escrow must be funded (`NotFunded`).
    /// 5. `milestone_index` must be in range (`InvalidMilestone`).
    /// 6. Milestone must not already be terminal (`InvalidStatus`).
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `InvalidStatus`   – `MultisigLocked` is not active, or the milestone
    ///                       is already `Released` / `Refunded`.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0.
    pub fn multisig_admin_override_release(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Only valid when a multisig lock is active — reject before any
        // JobMeta / milestone ledger reads.
        let multisig_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::MultisigLocked)
            .unwrap_or(false);
        if !multisig_locked {
            return Err(Error::InvalidStatus);
        }

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        // Terminal states have already settled funds — no double-spend.
        if Self::is_terminal_status(&milestone.status) {
            return Err(Error::InvalidStatus);
        }

        // Reject pathological operands before any arithmetic (issue #395).
        // `amount` / `released_amount` are signed i128 values read from
        // storage; guarding them here, alongside the checked_sub and
        // `remaining <= 0` guards below, guarantees no input — including
        // `i128::MAX` / `i128::MIN` — can cause a wrap or an unhandled panic.
        if milestone.amount < 0 || milestone.released_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        if milestone.released_amount > milestone.amount {
            return Err(Error::InvalidAmount);
        }
        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call. Only `Milestone(index)`
        // (persistent) is written here. Unlike the normal approval path, we
        // deliberately skip the `MilestoneReleased(index)` temporary flag:
        // nothing reads it on this override path (`is_milestone_released_flag`
        // is dead code), and the terminal `Released` status already persisted on
        // the milestone is the authoritative completion signal. Skipping it
        // keeps the ledger footprint of this call to two distinct keys
        // (`Milestone(index)` + `MultisigLocked`), matching the already-lean
        // `multisig_admin_override_refund` path.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Clear the multisig lock flag now that the deadlock is resolved.
        env.storage()
            .instance()
            .set(&DataKey::MultisigLocked, &false);

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.freelancer, &remaining)?;

        env.events().publish(
            (symbol_short!("msadmrel"),),
            MultisigAdminOverrideReleaseEvent {
                admin,
                milestone_index,
                freelancer: meta.freelancer,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

    /// Force-refund a multisig-locked milestone back to the client.
    ///
    /// Use this when a multisig approval workflow is deadlocked and the admin
    /// must return funds to the client without depending on the normal
    /// multi-party resolution flow.  The milestone is moved to `Refunded`
    /// and a full token transfer is executed back to the client.  The
    /// `MultisigLocked` flag is cleared.
    ///
    /// # Checks (in order)
    /// Authorization and source-state guards run **before** any job or
    /// milestone ledger entry is read or written, so a rejected call cannot
    /// mutate storage:
    /// 1. `require_admin` — caller must be the stored admin (`Unauthorized`
    ///    / `NotInitialized`).
    /// 2. `MultisigLocked` must be active (`InvalidStatus`).
    /// 3. Escrow must be funded (`NotFunded`).
    /// 4. `milestone_index` must be in range (`InvalidMilestone`).
    /// 5. Milestone must not already be `Released` or `Refunded`
    ///    (`InvalidStatus`).
    /// 6. Amount arithmetic is fully checked — pathological `i128` operands
    ///    (negative `amount` / `released_amount`, or `released_amount` beyond
    ///    `amount`, including `i128::MAX` / `i128::MIN`) return `InvalidAmount`
    ///    without panic or wrap.
    /// 7. Remaining balance must be > 0 (`InvalidAmount`).
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `InvalidStatus`   – Multisig workflow is not locked, or the
    ///                       milestone is already `Released` / `Refunded`.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0.
    pub fn multisig_admin_override_refund(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Reject illegal source state before any job/milestone ledger I/O.
        let multisig_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::MultisigLocked)
            .unwrap_or(false);
        if !multisig_locked {
            return Err(Error::InvalidStatus);
        }

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        if Self::is_terminal_status(&milestone.status) {
            return Err(Error::InvalidStatus);
        }

        // Reject pathological operands before any arithmetic (issue #395).
        // `amount` / `released_amount` are signed i128 values read from
        // storage; guarding them here, alongside the checked_sub and
        // `remaining <= 0` guards below, guarantees no input — including
        // `i128::MAX` / `i128::MIN` — can cause a wrap or an unhandled panic.
        if milestone.amount < 0 || milestone.released_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        if milestone.released_amount > milestone.amount {
            return Err(Error::InvalidAmount);
        }
        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call.
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Clear the multisig lock flag now that the deadlock is resolved.
        env.storage().instance().remove(&DataKey::MultisigLocked);

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.client, &remaining)?;

        env.events().publish(
            (symbol_short!("msadmref"),),
            MultisigAdminOverrideRefundEvent {
                admin,
                milestone_index,
                client: meta.client,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

    // ── split-refund distribution ─────────────────────────────────────────────

    /// Calculate a split-refund allocation between client and freelancer.
    ///
    /// Given a total amount and basis-point ratios for each party, this
    /// function computes how much should be refunded to the client and how
    /// much should be paid to the freelancer.  The ratios must sum to
    /// exactly `BPS_SCALE` (10 000).
    ///
    /// This is a pure computation (no storage access) that can be called
    /// by off-chain clients to preview split-refund outcomes before
    /// executing on-chain transfers.
    ///
    /// # Parameters
    /// * `env`                  – Soroban environment.
    /// * `admin`                – Must match `DataKey::Admin`.
    /// * `total_amount`         – Total amount to split.
    /// * `client_refund_bps`    – Client's refund share in basis points.
    /// * `freelancer_payout_bps`– Freelancer's payout share in basis points.
    ///
    /// # Returns
    /// A `RefundAllocation` struct with computed amounts and the basis-point
    /// ratios that were used.
    ///
    /// # Errors
    /// * `NotInitialized`– Contract has not been initialized.
    /// * `Unauthorized`  – `admin` is not the verified admin.
    /// * `InvalidStatus` – Multisig workflow is not locked.
    /// * `InvalidRatio`  – Ratios do not sum to `BPS_SCALE`.
    /// * `InvalidAmount` – `total_amount` ≤ 0 or arithmetic overflow.
    pub fn multisig_split_refund(
        env: Env,
        admin: Address,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        let multisig_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::MultisigLocked)
            .unwrap_or(false);
        if !multisig_locked {
            return Err(Error::InvalidStatus);
        }
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        // Use the existing split_round_nearest to compute client refund.
        let client_split =
            Self::split_round_nearest(total_amount, client_refund_bps as i128, BPS_SCALE as i128)?;

        // freelancer_payout = total_amount - client_refund
        let freelancer_payout = total_amount
            .checked_sub(client_split.first)
            .ok_or(Error::InvalidAmount)?;

        let allocation = RefundAllocation {
            client_refund: client_split.first,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        };

        env.events().publish(
            (symbol_short!("splitref"),),
            SplitRefundCalculatedEvent {
                client_refund: allocation.client_refund,
                freelancer_payout: allocation.freelancer_payout,
                client_refund_bps: allocation.client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
            },
        );

        Ok(allocation)
    }

    /// Implement refund distribution pathways for split-refund claims during
    /// an emergency pause.
    ///
    /// # Guard order (#524)
    ///
    /// Every guard runs before the arguments are validated and before any
    /// arithmetic, and each one only *reads* the instance entry, so a rejected
    /// call writes no ledger entry and publishes no event:
    /// 1. `caller.require_auth()`, then the caller must be the stored admin,
    ///    client or freelancer (`NotInitialized` / `Unauthorized`).
    /// 2. No pause transition may be half-applied (`EmergencyPauseInProgress`).
    /// 3. The escrow must actually be emergency-paused (`NotPaused`) — this
    ///    endpoint describes the refund split *while frozen*, so producing a
    ///    figure for a running escrow is an illegal source state.
    ///
    /// # Conservation (#526)
    ///
    /// `client_refund + freelancer_payout == total_amount` for every positive
    /// `total_amount` up to `i128::MAX` and every ratio summing to
    /// `BPS_SCALE`.  See [`Self::emergency_split_exact`].
    ///
    /// # Parameters
    /// * `caller`               – Admin, client or freelancer. Must authorize.
    /// * `total_amount`         – Total amount to split.
    /// * `client_refund_bps`    – Client's refund share in basis points.
    /// * `freelancer_payout_bps`– Freelancer's payout share in basis points.
    ///
    /// # Returns
    /// A `RefundAllocation` struct with computed amounts and the basis-point
    /// ratios that were used.
    ///
    /// # Errors
    /// * `NotInitialized`           – The escrow has not been initialized.
    /// * `Unauthorized`             – `caller` is not the admin, client or
    ///   freelancer.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    /// * `NotPaused`                – The escrow is not emergency-paused.
    /// * `InvalidAmount`            – `total_amount` ≤ 0.
    /// * `InvalidRatio`             – Ratios do not sum to `BPS_SCALE`.
    pub fn emergency_pause_split_refund(
        env: Env,
        caller: Address,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // ── authorization: before any other ledger read
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        if caller != meta.client && caller != meta.freelancer && admin.as_ref() != Some(&caller) {
            return Err(Error::Unauthorized);
        }

        // ── source state: a settled emergency pause, not a transition
        Self::assert_emergency_pause_not_locked(&env)?;
        if !Self::read_emergency_paused(&env) {
            return Err(Error::NotPaused);
        }

        let allocation =
            Self::emergency_split_exact(total_amount, client_refund_bps, freelancer_payout_bps)?;

        env.events().publish(
            (symbol_short!("epspltref"),),
            SplitRefundCalculatedEvent {
                client_refund: allocation.client_refund,
                freelancer_payout: allocation.freelancer_payout,
                client_refund_bps: allocation.client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
            },
        );

        Ok(allocation)
    }

    /// Conserving two-party split used by `emergency_pause_split_refund`.
    ///
    /// The client leg is `round_nearest(total × client_bps / BPS_SCALE)`,
    /// computed without ever forming the `total × client_bps` product: with
    /// `total = q × BPS_SCALE + r`,
    ///
    /// ```text
    /// round_nearest(total × bps / S) = q × bps + ⌊(r × bps + S/2) / S⌋
    /// ```
    ///
    /// `q × bps ≤ total` and `r × bps < S²`, so no intermediate can overflow
    /// for any `total` up to `i128::MAX`; the result is identical to the
    /// direct formula wherever that one does not overflow.  The second term is
    /// at most `r`, so `client_refund ≤ total` and the freelancer leg — the
    /// exact remainder — is never negative.  The sum is re-checked before
    /// returning so a future edit that breaks conservation fails loudly.
    fn emergency_split_exact(
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        let scale = BPS_SCALE as i128;
        let bps = client_refund_bps as i128;
        let quotient = total_amount / scale;
        let remainder = total_amount % scale;
        let client_refund = quotient
            .checked_mul(bps)
            .and_then(|whole| whole.checked_add((remainder * bps + scale / 2) / scale))
            .ok_or(Error::ArithmeticOverflow)?;
        let freelancer_payout = total_amount
            .checked_sub(client_refund)
            .ok_or(Error::ArithmeticOverflow)?;

        if freelancer_payout < 0
            || client_refund.checked_add(freelancer_payout) != Some(total_amount)
        {
            return Err(Error::ArithmeticOverflow);
        }

        Ok(RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        })
    }

    /// Compute a split-refund allocation for a cancelled escrow.
    ///
    /// Defines refund distribution pathways for split-refund claims that arise
    /// specifically from a `cancel_escrow` initiation.  The function is a pure
    /// calculator — it does not transfer tokens or mutate storage — so callers
    /// can safely invoke it to preview the allocation before committing to an
    /// admin override.
    ///
    /// The arithmetic uses `split_round_nearest` so the client share is rounded
    /// to nearest rather than always floored, and `client_refund + freelancer_payout`
    /// always equals `total_amount` exactly.
    ///
    /// # Guard order
    ///
    /// The source-state guards run **first**, before the amount and ratio
    /// validation and before any arithmetic, so an illegal source state is
    /// rejected before the function reads or writes anything else.  They only
    /// read the instance entry, so a rejected call touches no second ledger
    /// entry and mutates nothing:
    /// 1. `EscrowLocked` – a cancellation is already in flight
    ///   (`DataKey::CancelLock`), so the refund split it will settle is not yet
    ///   knowable; also covers an emergency pause (`DataKey::Ep`).
    /// 2. `Paused` – an `admin_pause_escrow` pause (`DataKey::Paused`) is in
    ///   force, so no cancellation-path figure should be produced.
    ///
    /// Both guards read a missing key as "not set", so an uninitialised
    /// contract still answers as a pure calculator.
    ///
    /// # Caller authorization
    ///
    /// This endpoint deliberately takes no caller parameter: it is an
    /// unauthenticated *preview*, not a settlement path, so no signature is
    /// required and an escrow that has not been initialized can still be asked
    /// what a ratio works out to (see
    /// `test_cancel_escrow_split_refund_works_without_initialization`).
    /// Because it moves no tokens and writes no ledger entry, authorization
    /// cannot be bypassed by calling it — the only way to actually settle a
    /// split refund on the cancellation path is the admin-gated
    /// `Self::cancel_escrow_claim_refund`, and moving funds outright is
    /// `Self::admin_override_cancel_refund`.  The source-state guards above
    /// are therefore the complete set of conditions under which the preview is
    /// allowed to answer; the guards deliberately mirror the claim's, which
    /// requires a cancellation to be in flight.
    ///
    /// # Parameters
    /// * `total_amount`          – Total escrowed balance to distribute. Must be > 0.
    /// * `client_refund_bps`     – Client's share in basis points (0–10 000).
    /// * `freelancer_payout_bps` – Freelancer's share in basis points (0–10 000).
    ///   The two BPS values must sum to exactly 10 000.
    ///
    /// # Returns
    /// A `RefundAllocation` with:
    /// * `client_refund`         = round_nearest(`total_amount` × `client_refund_bps` / 10_000)
    /// * `freelancer_payout`     = `total_amount` − `client_refund`
    /// * `client_refund_bps`     = echoed input
    /// * `freelancer_payout_bps` = echoed input
    ///
    /// # Errors
    /// * `EscrowLocked`  – A cancellation is already in flight, or an emergency
    ///                     pause is active.
    /// * `Paused`        – The contract is administratively paused.
    /// * `InvalidAmount` – `total_amount` ≤ 0 or arithmetic overflow.
    /// * `InvalidRatio`  – `client_refund_bps + freelancer_payout_bps ≠ 10_000`,
    ///                     or either value overflows `u32` on addition.
    pub fn cancel_escrow_split_refund(
        env: Env,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // Source-state guards run before any validation or arithmetic. Both
        // read only the instance entry, so an illegal source state is rejected
        // without a single ledger write.
        Self::ensure_not_paused(&env)?;
        Self::assert_not_paused(&env)?;

        let allocation =
            Self::split_refund_allocation(total_amount, client_refund_bps, freelancer_payout_bps)?;

        env.events().publish(
            (symbol_short!("cxlspref"),),
            CancelSplitRefundCalculatedEvent {
                client_refund: allocation.client_refund,
                freelancer_payout: allocation.freelancer_payout,
                client_refund_bps: allocation.client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
            },
        );

        Ok(allocation)
    }

    /// Pure two-party split arithmetic shared by `cancel_escrow_split_refund`
    /// and its admin-gated claim counterpart, so the preview and the claim can
    /// never disagree about how a ratio is rounded.
    ///
    /// The client share is computed with round-nearest arithmetic and the
    /// freelancer receives the exact remainder, so the two legs always sum to
    /// `total_amount`.
    fn split_refund_allocation(
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        let client_split =
            Self::split_round_nearest(total_amount, client_refund_bps as i128, BPS_SCALE as i128)?;

        let freelancer_payout = total_amount
            .checked_sub(client_split.first)
            .ok_or(Error::InvalidAmount)?;

        Ok(RefundAllocation {
            client_refund: client_split.first,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        })
    }

    /// Admin-gated split-refund claim on the cancellation path — the authorized
    /// counterpart to the unauthenticated `cancel_escrow_split_refund`
    /// calculator.
    ///
    /// The calculator and the claim are deliberately complementary:
    /// `cancel_escrow_split_refund` refuses to answer while a cancellation is
    /// in flight (`EscrowLocked`), and this endpoint refuses to answer unless
    /// one *is* in flight.  That matches `admin_override_cancel_refund`, which
    /// also requires the cancel lock to be active.
    ///
    /// # Checks (in order)
    /// 1. `admin.require_auth()` and the stored admin must match
    ///    (`NotInitialized` / `Unauthorized`).
    /// 2. A cancellation must be in flight (`InvalidStatus`).
    /// 3. The escrow must be initialized, funded and hold a non-zero balance
    ///    (`NotInitialized` / `NotFunded` / `EmptyBalance`).
    /// 4. `total_amount` > 0 and the shares sum to `BPS_SCALE`
    ///    (`InvalidAmount` / `InvalidRatio`).
    ///
    /// Every check runs before any arithmetic, so a rejected claim mutates no
    /// ledger entry.
    ///
    /// # Returns
    /// A `RefundAllocation` whose two amounts sum to `total_amount` exactly.
    ///
    /// # Errors
    /// * `NotInitialized` – No admin key has ever been stored, or no job.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    /// * `InvalidStatus`  – No cancellation is currently in flight.
    /// * `NotFunded`      – The escrow has not been funded.
    /// * `EmptyBalance`   – The contract holds no tokens to distribute.
    /// * `InvalidAmount`  – `total_amount` ≤ 0, or overflow.
    /// * `InvalidRatio`   – Shares do not sum to 10 000 bps.
    pub fn cancel_escrow_claim_refund(
        env: Env,
        admin: Address,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if !cancel_locked {
            return Err(Error::InvalidStatus);
        }

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        Self::assert_nonzero_balance(&env, &meta)?;

        let allocation =
            Self::split_refund_allocation(total_amount, client_refund_bps, freelancer_payout_bps)?;

        env.events().publish(
            (symbol_short!("cxlspref"),),
            CancelSplitRefundCalculatedEvent {
                client_refund: allocation.client_refund,
                freelancer_payout: allocation.freelancer_payout,
                client_refund_bps: allocation.client_refund_bps,
                freelancer_payout_bps: allocation.freelancer_payout_bps,
            },
        );

        Ok(allocation)
    }

    /// Admin-gated split-refund claim that may only run **while the contract
    /// is actually frozen**.
    ///
    /// `emergency_pause_split_refund` lets any escrow party (admin, client or
    /// freelancer) ask "what would this split be?" while the escrow is frozen.
    /// This endpoint is the admin-only operational counterpart: it enforces
    /// the business rules that must hold
    /// before an emergency refund is settled, rejecting each bad setup with a
    /// distinct error variant before any arithmetic runs.
    ///
    /// # Business rules (checked in this order, each before any later read)
    /// 1. `admin.require_auth()` runs first, before any ledger read.  The
    ///    contract must be initialised and the caller must be the stored admin
    ///    (`NotInitialized` / `Unauthorized`).
    /// 2. No pause transition mid-execution (`EmergencyPauseInProgress`) — a
    ///    refund must not be computed against a half-applied freeze.
    /// 3. The contract **is** paused (`NotPaused`).  Settling an emergency
    ///    refund on a running escrow would bypass the normal release and
    ///    dispute paths.
    /// 4. `total_amount` > 0 (`InvalidAmount`) and the two shares sum to
    ///    exactly `BPS_SCALE` (`InvalidRatio`).
    /// 5. The split is computed with checked `i128` arithmetic only
    ///    (`ArithmeticOverflow`).
    /// 6. The contract holds a non-zero balance (`EmptyBalance`) that covers
    ///    `total_amount` (`InsufficientBalance`).
    ///
    /// # Storage footprint
    /// The endpoint is read-only.  Every key it reads — `Admin`, `EpLk`, `Ep`
    /// and `Job` — lives in **instance** storage, so the whole call touches a
    /// single contract ledger entry (plus the token balance read).  The admin
    /// check uses `require_admin_from_instance` rather than `require_admin`,
    /// which drops the separate persistent `Admin` entry from the footprint.
    /// The token balance is fetched once and reused for both the balance
    /// guards and the event's `remaining_balance`.
    ///
    /// # Events
    /// On success only, publishes an [`EmergencyPauseClaimRefundEvent`] under
    /// the topics `(symbol_short!("epclaim"), admin)`.
    ///
    /// # Returns
    /// A `RefundAllocation` whose two amounts sum to `total_amount` exactly.
    ///
    /// # Errors
    /// * `NotInitialized`           – Admin key has never been stored.
    /// * `Unauthorized`             – `admin` is not the stored admin.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    /// * `NotPaused`                – The contract is not frozen.
    /// * `InvalidAmount`            – `total_amount` ≤ 0.
    /// * `InvalidRatio`             – Shares do not sum to 10 000 bps.
    /// * `ArithmeticOverflow`       – The split overflowed `i128`.
    /// * `EmptyBalance`             – The contract token balance is zero, so
    ///   there is nothing to settle.
    /// * `InsufficientBalance`      – `total_amount` exceeds the contract
    ///   token balance.
    pub fn emergency_pause_claim_refund(
        env: Env,
        admin: Address,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // ── preconditions (#532): auth and contract state before anything else
        Self::require_role(&env, &admin, AdminRole::Resolver)?;
        Self::assert_emergency_pause_not_locked(&env)?;

        // `require_admin` already proved an admin key is stored, so the
        // pause-flag read cannot miss here.
        if !Self::load_emergency_paused(&env)? {
            return Err(Error::NotPaused);
        }

        // ── input validation (pure, no ledger access)
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        // ── checked split (#533): client share rounds to nearest, freelancer
        // receives the exact remainder so the legs always sum to the total.
        let scale = BPS_SCALE as i128;
        let client_refund = total_amount
            .checked_mul(client_refund_bps as i128)
            .and_then(|scaled| scaled.checked_add(scale / 2))
            .and_then(|scaled| scaled.checked_div(scale))
            .ok_or(Error::ArithmeticOverflow)?;
        let freelancer_payout = total_amount
            .checked_sub(client_refund)
            .ok_or(Error::ArithmeticOverflow)?;

        // ── balance guards: one token read, reused for the event (#535)
        let meta = Self::load_job_meta(&env)?;
        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::EmptyBalance);
        }
        let remaining_balance = contract_balance
            .checked_sub(total_amount)
            .ok_or(Error::ArithmeticOverflow)?;
        if remaining_balance < 0 {
            return Err(Error::InsufficientBalance);
        }

        // ── success-only event (#534)
        env.events().publish(
            (symbol_short!("epclaim"), admin),
            EmergencyPauseClaimRefundEvent {
                claimant: meta.client,
                refund_amount: client_refund,
                freelancer_payout,
                remaining_balance,
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        })
    }

    /// Divide a frozen escrow balance across an arbitrary number of parties
    /// without losing value to rounding.
    ///
    /// # Why plain division is not enough
    /// Allocating `total × weightᵢ / Σweights` with truncating division rounds
    /// every party down, so the shares sum to *less* than `total`.  The
    /// shortfall is at most `n − 1` stroops per call, but it is systematic:
    /// the same party sizes lose value every time, and the residue is stranded
    /// in the contract with no owner.
    ///
    /// # Algorithm — largest remainder (Hare quota)
    /// ```text
    /// weightedᵢ = total × weightᵢ
    /// baseáµ¢     = weightedáµ¢ / Î£weights      (floor)
    /// remáµ¢      = weightedáµ¢ % Î£weights      (exact fractional part, scaled)
    /// residue   = total − Σbaseᵢ            (0 ≤ residue < n)
    /// ```
    /// The `residue` indivisible units are then handed out one at a time to
    /// the parties with the largest `remáµ¢`, each party receiving at most one.
    /// This is exact rather than approximate: `remáµ¢` is the true numerator of
    /// the discarded fraction, so the units go to whoever was rounded down
    /// hardest.
    ///
    /// # Guarantees
    /// * **Conservation** – `Σallocations == total_amount` exactly, for every
    ///   input.  No value is lost and none is created.
    /// * **Bounded error** – each `allocationᵢ` is within one unit of the
    ///   exact rational share `total × weightᵢ / Σweights`; it is never more
    ///   than one unit below it, so no party is systematically rounded down.
    /// * **Determinism** – ties in `remᵢ` are broken by lowest index, so the
    ///   same inputs always produce the same vector.
    /// * **Zero weights** – a party weighted `0` receives exactly `0`; its
    ///   remainder is also `0`, so it never wins a residue unit ahead of a
    ///   party with a real fractional claim.
    ///
    /// # Guard order (#536)
    ///
    /// Every guard runs *before* the weight vector is looked at and before any
    /// arithmetic, and each one only reads instance storage, so a rejected
    /// call writes no ledger entry and publishes no event:
    ///
    /// 1. `caller.require_auth()`.
    /// 2. The caller must be the stored admin, the job's client or the job's
    ///    freelancer — otherwise `NotInitialized` (no job recorded) or
    ///    `Unauthorized`. The allocation describes how a frozen balance is
    ///    shared, so it is a participant-only figure; an arbitrary address may
    ///    not have the contract compute it.
    /// 3. No pause transition may be half-applied (`EmergencyPauseInProgress`).
    /// 4. The escrow must actually be emergency-paused (`NotPaused`) — this
    ///    endpoint describes the split *while frozen*, so producing a figure
    ///    for a running escrow is an illegal source state.
    ///
    /// Authorization deliberately precedes the source-state guards so an
    /// unauthorized caller learns nothing about the escrow's pause status.
    ///
    /// # Validation order
    /// The total and the weight vector are fully validated *before* the first
    /// division by `Î£weights` is attempted, so a malformed vector can never
    /// reach the arithmetic and can never trap:
    /// 1. `total_amount` ≤ 0                    → `InvalidAmount`
    /// 2. `weights` empty, or over the cap      → `InvalidAllocationWeights`
    /// 3. any `weight` < 0                      → `InvalidAllocationWeights`
    /// 4. `Σweights` overflowing, or `≤ 0`      → `InvalidAllocationWeights`
    ///
    /// Step 3 is a per-entry scan, not a property of the total: a vector such
    /// as `[5, -1, 6]` sums to `10`, so the sum check alone would let a
    /// negative share cancel itself out of the divisor and hand that party a
    /// negative allocation.  Step 4 is what keeps `weighted / weight_sum` from
    /// ever being a division by zero.
    ///
    /// # Parameters
    /// * `caller`        – Admin, client or freelancer. Must authorize.
    /// * `total_amount` – Amount to divide; must be > 0.
    /// * `weights`      – Per-party weights.  Need not sum to any particular
    ///   scale; only their ratios matter.  Must be non-empty, at most
    ///   `MAX_EMERGENCY_ALLOCATION_PARTIES` long, non-negative, and sum to > 0.
    ///
    /// # Returns
    /// A `Vec<i128>` of per-party amounts, index-aligned with `weights`.
    ///
    /// # Errors
    /// * `NotInitialized`           – No job has been recorded.
    /// * `Unauthorized`             – `caller` is not the admin, client or
    ///   freelancer.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    /// * `NotPaused`                – The escrow is not emergency-paused.
    /// * `InvalidAmount`             – `total_amount` ≤ 0, or arithmetic overflow.
    /// * `InvalidAllocationWeights`  – `weights` empty, over the cap, negative,
    ///   or summing to zero.
    pub fn emergency_pause_allocation(
        env: Env,
        caller: Address,
        total_amount: i128,
        weights: Vec<i128>,
    ) -> Result<Vec<i128>, Error> {
        // ── authorization: before any other ledger read
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        if caller != meta.client && caller != meta.freelancer && admin.as_ref() != Some(&caller) {
            return Err(Error::Unauthorized);
        }

        // ── source state: a settled emergency pause, not a transition
        Self::assert_emergency_pause_not_locked(&env)?;
        if !Self::read_emergency_paused(&env) {
            return Err(Error::NotPaused);
        }

        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if weights.is_empty() || weights.len() > MAX_EMERGENCY_ALLOCATION_PARTIES {
            return Err(Error::InvalidAllocationWeights);
        }

        let mut weight_sum: i128 = 0;
        for weight in weights.iter() {
            if weight < 0 {
                return Err(Error::InvalidAllocationWeights);
            }
            weight_sum = weight_sum
                .checked_add(weight)
                .ok_or(Error::InvalidAllocationWeights)?;
        }

        if weight_sum <= 0 {
            return Err(Error::InvalidAllocationWeights);
        }

        let mut allocations: Vec<i128> = Vec::new(&env);
        let mut remainders: Vec<i128> = Vec::new(&env);
        let mut allocated_total: i128 = 0;

        for weight in weights.iter() {
            let weighted = total_amount
                .checked_mul(weight)
                .ok_or(Error::InvalidAmount)?;

            allocations.push_back(weighted / weight_sum);
            remainders.push_back(weighted % weight_sum);

            allocated_total = allocated_total
                .checked_add(weighted / weight_sum)
                .ok_or(Error::InvalidAmount)?;
        }

        // `residue` is strictly less than the number of parties, because each
        // discarded fraction is < 1 unit. The loop below therefore runs at
        // most `MAX_EMERGENCY_ALLOCATION_PARTIES` times.
        let residue = total_amount
            .checked_sub(allocated_total)
            .ok_or(Error::InvalidAmount)?;

        for _ in 0..residue {
            let mut best_index: u32 = 0;
            let mut best_remainder: i128 = i128::MIN;

            // Strict `>` keeps the lowest index on a tie, making the result
            // deterministic across identical inputs.
            for (idx, rem) in remainders.iter().enumerate() {
                if rem > best_remainder {
                    best_remainder = rem;
                    best_index = idx as u32;
                }
            }

            let current = allocations.get(best_index).ok_or(Error::InvalidAmount)?;
            allocations.set(
                best_index,
                current.checked_add(1).ok_or(Error::InvalidAmount)?,
            );

            // Retire this party so it cannot win a second residue unit.
            remainders.set(best_index, i128::MIN);
        }

        let num_parties = allocations.len();
        env.events().publish(
            (symbol_short!("epalloc"),),
            EmergencyPauseAllocationEvent {
                total_amount,
                num_parties,
                allocations: allocations.clone(),
            },
        );

        Ok(allocations)
    }

    /// Lock the multisig approval workflow, preventing further normal
    /// operations until an admin override resolves the deadlock.
    ///
    /// This is called internally by multisig-related functions when a
    /// deadlock condition is detected.  Only the stored admin can invoke
    /// the corresponding override endpoints.
    ///
    /// A `mslock` / `MultisigLockedEvent` is published once the flag is
    /// durable so indexers and auditors get an immutable record of the lock
    /// instead of polling storage.  The event mirrors the persisted state
    /// field-for-field: `locked` is the value written under
    /// `DataKey::MultisigLocked`.
    ///
    /// The lock is *taken* here and *cleared* by the override endpoints
    /// (`multisig_admin_override_release` / `multisig_admin_override_refund`),
    /// which already publish their own events, so a replay of `mslock` can
    /// never be confused with a release.
    ///
    /// ## Storage-footprint note
    ///
    /// This function deliberately uses `require_admin_from_instance` rather
    /// than the standard `require_admin` helper.  Both the admin verification
    /// read (`DataKey::Admin`) and the lock write (`DataKey::MultisigLocked`)
    /// therefore target **instance** storage, meaning a single invocation
    /// touches exactly **one** ledger entry instead of two (persistent + instance).
    /// Publishing the event does not alter that: events are contract logs, not
    /// ledger entries.
    ///
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin` (instance storage).
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    /// * `InvalidStatus`  – The multisig workflow is already locked.
    /// * `EmergencyPauseInProgress` – An emergency pause lock is currently active.
    pub fn multisig_lock(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Authorization check at top - require auth and admin role first
        Self::require_role(&env, &admin, AdminRole::Resolver)?;
        // Precondition guards before any ledger write: validate illegal source state
        // If already locked, return InvalidStatus with no storage mutation
        if env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::MultisigLocked)
            .unwrap_or(false)
        {
            return Err(Error::InvalidStatus);
        }
        Self::assert_emergency_pause_not_locked(&env)?;
        // Both the Admin read and the MultisigLocked write are in instance
        // storage, so the whole function touches a single ledger entry.
        env.storage()
            .instance()
            .set(&DataKey::MultisigLocked, &true);

        // Emitted after the write so the payload can only describe a flag that
        // is already durable; no fallible step follows, so a successful call
        // carries exactly one event and a rejected call carries none.
        env.events().publish(
            (symbol_short!("mslock"),),
            MultisigLockedEvent {
                admin,
                locked: true,
            },
        );

        Ok(())
    }

    /// Check whether the multisig workflow is currently locked.
    ///
    /// Returns `true` if the `MultisigLocked` flag is set, meaning normal
    /// multisig operations are blocked until an admin override resolves the
    /// deadlock, and `false` for an initialized escrow that was never locked.
    ///
    /// # Errors
    /// * `NotInitialized` – The contract has not been initialized.
    pub fn is_multisig_locked(env: Env) -> Result<bool, Error> {
        Self::load_multisig_locked(&env)
    }
}
