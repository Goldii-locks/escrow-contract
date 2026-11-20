use soroban_sdk::{contracterror, contracttype, Address, BytesN, Vec};

/// Maximum number of ratio slots accepted by `compute_ratio_allocation`.
/// A multisig setup with more than this many signers is operationally
/// unreasonable and would impose unbounded per-transaction CPU costs; the cap
/// guarantees that the nested loop over `ratios` iterates at most
/// `MAX_MULTISIG_RATIO_COUNT` times both during validation and during
/// the largest-remainder allocation phase.
pub(crate) const MAX_MULTISIG_RATIO_COUNT: u32 = 255;

/// Maximum number of tokens that may be held in the whitelist at any one time.
/// `add_whitelisted_token` enforces this cap before calling `push_back` so
/// that the internal `u32` length counter of the Soroban `Vec` can never
/// overflow regardless of how many times the function is invoked.
pub(crate) const MAX_WHITELIST_SIZE: u32 = 50;

/// Maximum number of parties that may share an emergency-pause allocation.
/// `emergency_pause_allocation` runs a nested loop over the weight
/// vector during the largest-remainder phase, so the cap bounds its worst-case
/// CPU cost and keeps the `u32` party counter from overflowing.
pub(crate) const MAX_EMERGENCY_ALLOCATION_PARTIES: u32 = 255;

/// Default window before either party may return an unresolved dispute to its
/// pre-dispute milestone state.
pub const DISPUTE_TIMEOUT_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Minimum allowed `dispute_timeout_seconds` (1 day), so the arbiter always
/// gets time to act before either party can escalate.
pub const MIN_DISPUTE_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;

/// Maximum allowed `dispute_timeout_seconds` (180 days), so escalation and
/// admin settlement of a disputed milestone stay reachable.
pub const MAX_DISPUTE_TIMEOUT_SECONDS: u64 = 180 * 24 * 60 * 60;

/// Maximum number of milestones `approve_milestones` accepts in one call.
/// Validation does a duplicate scan over the index list, so the cap bounds
/// its worst-case CPU cost along with the per-milestone storage writes.
pub(crate) const MAX_BATCH_APPROVALS: u32 = 10;

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
    /// mid-execution and holds `the former reentrancy guard`.
    TaxWithholdingInProgress = 26,
    /// A guarded endpoint was called while a platform-fee allocation is
    /// mid-execution and holds `the former reentrancy guard`.
    PlatformFeeAllocationInProgress = 27,
    /// A guarded endpoint was called while an emergency pause transition is
    /// mid-execution and holds `the former reentrancy guard`.
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
    /// mid-execution and holds `the former reentrancy guard`.
    TimeExtInProgress = 33,
    /// A guarded endpoint was called while `payment_streaming_milestones` is
    /// mid-execution and holds `the former reentrancy guard`.
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
    /// A dispute timeout must be between `MIN_DISPUTE_TIMEOUT_SECONDS` and
    /// `MAX_DISPUTE_TIMEOUT_SECONDS`.
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
    /// `raise_dispute` or `request_revision` was called on a `Delivered`
    /// milestone whose auto-release deadline has already passed; the
    /// freelancer is entitled to `claim_auto_release`.
    ReviewWindowClosed = 56,
    /// A non-zero milestone due date was not in the future at the time
    /// `set_milestone_due_dates` was called.
    InvalidDeadline = 57,
}

pub(crate) const MAX_REVISIONS: u32 = 3;

/// Maximum number of evidence hashes a single dispute may accumulate through
/// `submit_evidence`.  The cap bounds the persistent `DisputeInfo` value so a
/// party cannot grow it without limit (and therefore cannot inflate the
/// ledger-rent footprint of the dispute record) by submitting repeatedly.
pub const MAX_DISPUTE_EVIDENCE_ENTRIES: u32 = 10;

pub(crate) const MAX_TREASURY_FEE_BPS: u32 = 2000;

pub(crate) const MAX_CLIENT_FEE_BPS: u32 = 5000;

/// Upper bound on the number of milestones a job may carry, including
/// milestones appended after funding by `add_milestone` (issue #737).
/// Keeps `get_job` and every whole-set iteration bounded
/// so the ledger footprint of amendment endpoints stays predictable.
pub(crate) const MAX_MILESTONES: u32 = 50;

/// Seconds after funding during which the freelancer and arbiter may accept
/// the job. Once it has passed without both acceptances, the client can
/// reclaim the escrow with `withdraw_unaccepted`.
pub(crate) const ACCEPTANCE_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Seconds after a milestone's `due_at` before the client may reclaim it with
/// `reclaim_overdue`, so a delivery landing just after the deadline still
/// counts.
pub const OVERDUE_GRACE_SECONDS: u64 = 24 * 60 * 60;

pub(crate) const BPS_SCALE: u32 = 10_000;

/// Timelock for treasury address changes (#674): seven days.
pub(crate) const TREASURY_UPDATE_DELAY_SECONDS: u64 = 7 * 24 * 60 * 60;

pub(crate) const TTL_THRESHOLD: u32 = 30 * 24 * 60 * 5;

pub(crate) const TTL_EXTEND_TO: u32 = 365 * 24 * 60 * 5;

/// Maximum arbiter fee in basis points (1,000 = 10 %), issue #738.
pub(crate) const MAX_ARBITER_FEE_BPS: u32 = 1_000;

/// Seconds in a standard Gregorian year (365 days). Used by the simple-interest
/// estimator in `escrow_interest_yield`.
#[cfg(test)]
pub(crate) const SECONDS_PER_YEAR: i128 = 31_536_000;

/// Basis-point denominator for interest math (`1 bp = 1 / 10_000`).
#[cfg(test)]
pub(crate) const BPS_DENOMINATOR: i128 = 10_000;

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
/// * `4` – `DisputeInfo::raised_by` (#997), the party that called
///   `raise_dispute`.  Disputes already open at the upgrade have no recorded
///   raiser; `migrate` stamps them with the job's client (see `migrate`).
/// * `5` – one admin pause flag: `DataKey::Paused`. `DataKey::Ep` holds only
///   the party emergency pause.
pub const SCHEMA_VERSION: u32 = 5;

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
pub(crate) struct MilestoneV1 {
    pub(crate) amount: i128,
    pub(crate) released_amount: i128,
    pub(crate) status: MilestoneStatus,
    pub(crate) delivered_at: u64,
    pub(crate) funded: bool,
}

/// `Milestone` as stored by schema version 2, before `deliverable`.
/// Only `migrate` reads it.
#[contracttype]
#[derive(Clone, Debug)]
pub(crate) struct MilestoneV2 {
    pub(crate) amount: i128,
    pub(crate) released_amount: i128,
    pub(crate) status: MilestoneStatus,
    pub(crate) delivered_at: u64,
    pub(crate) funded: bool,
    pub(crate) spec_hash: BytesN<32>,
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
    /// The escrow party (client or freelancer) that called `raise_dispute`
    /// (#997).  Disputes that were already open when the escrow migrated from
    /// schema 3 carry the job's client here, because the raiser was never
    /// recorded; for those, the original `dispute` event's `caller` topic is
    /// authoritative.
    pub raised_by: Address,
}

/// `DisputeInfo` as stored by schema version 3, before `raised_by`.
/// Only `migrate` and `get_dispute_info` read it.
#[contracttype]
#[derive(Clone, Debug)]
pub(crate) struct DisputeInfoV3 {
    pub(crate) disputed_at: u64,
    pub(crate) previous_status: MilestoneStatus,
    pub(crate) reason_hash: BytesN<32>,
    pub(crate) evidence: Vec<EvidenceRecord>,
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

/// Both pauses. `emergency` is the party pause, `DataKey::Ep`
/// (`emergency_pause`); `admin` is the single admin pause, `DataKey::Paused`
/// (`admin_pause_escrow`).
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
    /// Code version of the WASM serving this read (`CONTRACT_VERSION`).
    pub version: String,
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
pub(crate) struct JobMeta {
    pub(crate) client: Address,
    pub(crate) freelancer: Address,
    pub(crate) arbiter: Address,
    pub(crate) token: Address,
    pub(crate) created_at: u64,
    pub(crate) funded: bool,
    pub(crate) auto_release_seconds: u64,
    pub(crate) milestone_count: u32,
    pub(crate) total_amount: i128,
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
    /// `upgrade`, `migrate`.
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
    /// Retired (#1001): no longer read or written; `Milestone.delivered_at`
    /// is the only delivery timestamp. Kept for discriminant stability.
    DeliveredAt(u32),
    /// Retired (#1001): no longer read or written; the persisted milestone
    /// status is the completion signal. Kept for discriminant stability.
    MilestoneReleased(u32),
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
    /// Retired (#1001): no longer read or written; the persisted milestone
    /// status is the dispute signal. Kept for discriminant stability.
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
    /// Retired (#1001): no longer read or written; the applied split is
    /// reported by `ArbitrationSplitAppliedEvent`. Kept for discriminant
    /// stability.
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
    /// Instance: number of milestones that hold their own deposit (#999).
    /// `fund_milestone` advances it by one, so deciding whether the job is
    /// fully funded is a comparison with `milestone_count` instead of a scan
    /// over every milestone.  `fund` sets it to `milestone_count` and
    /// `add_milestone` keeps it equal to the new count, so a funded job
    /// always satisfies `FundedCount == milestone_count`.  `reduce_milestone`
    /// leaves it alone: a reduced milestone stays funded.  There is no
    /// milestone-removal endpoint; one would have to decrement it for every
    /// funded milestone it removes.  Escrows created before this key existed
    /// have no entry; `load_funded_count` counts their milestones once and
    /// the next `fund_milestone` stores the result.
    FundedCount,
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
    /// Persistent: delivery deadline (ledger timestamp) of milestone `u32`,
    /// set by `set_milestone_due_dates`.  Absent or `0` means no deadline.
    DueAt(u32),
    /// Instance: address receiving the configured treasury share of payouts.
    /// Absent means the admin address is used, so existing deployments keep
    /// working without a migration.
    Treasury,
    /// Instance: admin-timelocked treasury change awaiting execution.
    PendingTreasury,
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
pub struct PendingTreasury {
    pub address: Address,
    pub effective_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeDistribution {
    pub client_amount: i128,
    pub freelancer_amount: i128,
    pub treasury_amount: i128,
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

/// Consolidated yield configuration stored under `DataKey::YieldConfig`.
/// Bundles all yield-related settings into a single struct-valued ledger
/// entry so `admin_set_yield_rate` touches one key instead of several.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldConfig {
    pub yield_rate: u32,
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
