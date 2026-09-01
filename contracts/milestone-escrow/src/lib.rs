#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, BytesN, Env,
    Vec,
};

/// Maximum number of ratio slots that may be passed to `multisig_transfer_admin`.
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

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AlreadyFunded = 3,
    NotFunded = 4,
    Unauthorized = 5,
    InvalidMilestone = 6,
    InvalidStatus = 7,
    TokenNotWhitelisted = 8,
    TokenAlreadyWhitelisted = 9,
    InvalidAmount = 10,
    DeadlineNotPassed = 11,
    InvalidAddress = 12,
    Paused = 13,
    InvalidRatio = 14,
    InvalidExtension = 15,
    EscrowLocked = 16,
    MultiSigNoSigners = 17,
    MultiSigTooManySigners = 18,
    MultiSigInvalidThreshold = 19,
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
    /// A multisig payout was attempted while the contract token balance is
    /// zero, so there is nothing to allocate across the signer ratios.
    MultiSigEmptyBalance = 25,
    /// A guarded endpoint was called while `tax_withholding_deductions` is
    /// mid-execution and holds `DataKey::TaxWithholdingExecutionLock`.
    TaxWithholdingInProgress = 26,
    /// A guarded endpoint was called while a platform-fee allocation is
    /// mid-execution and holds `DataKey::PlatformFeeAllocationLock`.
    PlatformFeeAllocationInProgress = 27,
    /// A guarded endpoint was called while an emergency pause transition is
    /// mid-execution and holds `DataKey::EmergencyPauseLock`.
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
}

const BPS_SCALE: u32 = 10_000;

/// Seconds in a standard Gregorian year (365 days). Used by the simple-interest
/// estimator in `escrow_interest_yield`.
const SECONDS_PER_YEAR: i128 = 31_536_000;

/// Basis-point denominator for interest math (`1 bp = 1 / 10_000`).
const BPS_DENOMINATOR: i128 = 10_000;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MilestoneStatus {
    Pending,
    Delivered,
    PartiallyReleased,
    Released,
    Disputed,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Milestone {
    pub amount: i128,
    pub released_amount: i128,
    pub status: MilestoneStatus,
    pub delivered_at: u64,
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

#[contracttype]
#[derive(Clone, Debug)]
struct JobMeta {
    client: Address,
    freelancer: Address,
    arbiter: Address,
    token: Address,
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

#[contracttype]
pub enum DataKey {
    Job,
    Milestone(u32),
    Admin,
    Version,
    WhitelistedTokens,
    EmergencyPaused,
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
    /// Persistent: annual yield rate expressed in basis points (1 bp = 0.01 %).
    /// Range 0–10 000 (0 %–100 %).  Written by `admin_set_yield_rate`, read by
    /// `get_yield_info` and `admin_accrue_yield`.
    YieldRateBps,
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
    MilestoneTimeExtension(u32),
    CancelLock,
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
    /// The full list of registered multisig signers (instance storage, written
    /// once by `multisig_approval_init`).  Stored as a single `Vec<Address>`
    /// rather than N individual keys to minimise read overhead and total bytes.
    MultiSigSigners,
    /// The minimum number of approvals required for a multisig decision.
    /// Written once during initialisation, read on every approval check.
    MultiSigThreshold,
    /// Transient approval-bitmap for a given proposal index.  Uses **temporary**
    /// storage so the ledger footprint does not persist beyond the proposal
    /// lifecycle.  Each bit position corresponds to a signer index in the
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
    // Appended at the end of `DataKey` so existing variant discriminants stay
    // stable (serialization compatibility for already-written ledger entries).
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
    EmergencyPauseLock,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitializedEvent {
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub auto_release_seconds: u64,
    pub milestone_amounts: Vec<i128>,
    pub total_amount: i128,
    pub milestone_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundedEvent {
    pub contract_id: Address,
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub total_amount: i128,
    pub milestone_count: u32,
    pub auto_release_seconds: u64,
    pub funded: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveredEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub client: Address,
    pub delivered_at: u64,
    pub status: MilestoneStatus,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadlineExtendedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub extra_seconds: u64,
    pub new_extension: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    /// Gross milestone amount (before any partial releases).
    pub amount: i128,
    /// Cumulative amount released including this approval.
    pub released_amount: i128,
    /// Remaining balance after this approval (always 0 on a full approval).
    pub remaining: i128,
    pub status: MilestoneStatus,
    /// Total number of milestones in the contract.
    pub milestone_count: u32,
    /// Contract-level total amount across all milestones.
    pub total_amount: i128,
    /// Auto-release window configured for this escrow.
    pub auto_release_seconds: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeRaisedEvent {
    pub milestone_index: u32,
    pub caller: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolvedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub arbiter: Address,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    /// Amount owed on the milestone at the time of resolution (before
    /// capping to the contract's available balance).
    pub amount: i128,
    /// Amount actually transferred to the freelancer or refunded to the
    /// client. May be less than `amount` if the contract balance was
    /// insufficient to cover the full owed amount.
    pub paid_amount: i128,
    pub released_to_freelancer: bool,
    pub status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaxWithholdingDeductionsEvent {
    pub admin: Address,
    pub contract_id: Address,
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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoReleasedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub client: Address,
    pub token: Address,
    pub amount: i128,
    pub delivered_at: u64,
    pub released_at: u64,
    pub auto_release_seconds: u64,
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
    pub contract_id: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelEscrowInitiatedEvent {
    pub contract_id: Address,
    pub caller: Address,
}

/// Emitted by `admin_override_cancel_release` when the admin resolves a locked
/// cancel state by releasing all remaining funds to the freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminCancelOverrideReleaseEvent {
    pub admin: Address,
    pub contract_id: Address,
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
    pub contract_id: Address,
    pub client: Address,
    pub token: Address,
    pub total_refunded: i128,
}

/// Emitted by `emergency_pause` when the admin pauses the escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPausedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `emergency_unpause` when the admin unpauses the escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyUnpausedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `emergency_pause_admin_override` when the admin overrides the pause state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPauseAdminOverrideEvent {
    pub admin: Address,
    pub contract_id: Address,
    pub paused: bool,
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
    pub contract_id: Address,
    pub milestone_index: u32,
    pub gross_amount: i128,
    pub tax_amount: i128,
    pub net_amount: i128,
    pub tax_rate_bps: u32,
}

/// Emitted by `admin_override_tax_release` when the admin resolves a
/// tax-locked milestone by releasing the net amount to the freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideTaxReleaseEvent {
    pub admin: Address,
    pub contract_id: Address,
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
    pub contract_id: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub token: Address,
    pub gross_amount: i128,
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
    pub contract_id: Address,
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
    pub contract_id: Address,
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

/// Emitted by `admin_override_streaming_release` when the admin proportionally
/// settles a `Disputed` milestone using the streaming/time-extension split.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideStreamingReleaseEvent {
    pub admin: Address,
    pub contract_id: Address,
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

/// Emitted by `admin_pause_escrow` when the admin freezes normal operations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowPausedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `admin_resume_escrow` when the admin lifts the pause and
/// restores normal operations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowResumedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

// ── NEW EVENTS ─────────────────────────────────────────────

#[contracttype]
pub struct WhitelistedTokenAddedEvent {
    pub token: Address,
}

#[contracttype]
pub struct WhitelistedTokenRemovedEvent {
    pub token: Address,
}

#[contracttype]
pub struct PartialReleaseApprovedEvent {
    pub milestone_index: u32,
    pub amount: i128,
}

#[contracttype]
pub struct AutoReleaseClaimedEvent {
    pub milestone_index: u32,
    pub amount: i128,
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
    pub client: Address,
    pub freelancer: Address,
    pub total_amount: i128,
    pub numerator: i128,
    pub denominator: i128,
    pub streamed_payout: i128,
    pub client_refund: i128,
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

/// Emitted by `multisig_admin_override_release` when the admin force-releases
/// a multisig-locked allocation to the freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultisigAdminOverrideReleaseEvent {
    pub admin: Address,
    pub contract_id: Address,
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
    pub contract_id: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub token: Address,
    pub amount: i128,
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

/// Emitted by `multisig_transfer_admin` after a successful proportional
/// allocation of `total_amount` across all ratio entries.  Downstream
/// indexers can use this event to audit every admin-triggered multi-party
/// transfer without querying contract storage directly.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigTransferAdminEvent {
    /// The total amount that was distributed.
    pub total_amount: i128,
    /// The number of parties the amount was split between.
    pub num_parties: u32,
    /// The resulting allocation per party, in the same order as the input
    /// ratios.  Guaranteed to sum exactly to `total_amount`.
    pub allocations: Vec<i128>,
}

#[contract]
pub struct MilestoneEscrow;

#[contractimpl]
impl MilestoneEscrow {
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
        let paused = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::EmergencyPaused)
            .unwrap_or(false);
        if paused {
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
            .get::<_, bool>(&DataKey::EmergencyPauseLock)
            .unwrap_or(false);
        if locked {
            return Err(Error::EmergencyPauseInProgress);
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

    fn load_interest_yield_state(env: &Env) -> Result<EscrowInterestYieldState, Error> {
        env.storage()
            .instance()
            .get(&DataKey::InterestYieldState)
            .ok_or(Error::NotInitialized)
    }

    fn store_interest_yield_state(env: &Env, state: &EscrowInterestYieldState) {
        env.storage()
            .instance()
            .set(&DataKey::InterestYieldState, state);
    }

    fn ensure_interest_yield_unlocked(env: &Env) -> Result<(), Error> {
        let state = Self::load_interest_yield_state(env)?;
        if state.locked {
            return Err(Error::EscrowLocked);
        }
        Ok(())
    }

    fn split_round_nearest(
        total: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        if total < 0 || numerator < 0 || denominator <= 0 || numerator > denominator {
            return Err(Error::InvalidRatio);
        }

        let scaled = total.checked_mul(numerator).ok_or(Error::InvalidAmount)?;
        let half = denominator / 2;
        let rounded = scaled.checked_add(half).ok_or(Error::InvalidAmount)? / denominator;

        if rounded > total {
            return Err(Error::InvalidAmount);
        }

        Ok(RatioSplit {
            first: rounded,
            second: total.checked_sub(rounded).ok_or(Error::InvalidAmount)?,
        })
    }

    fn allocate_platform_fee(
        total_amount: i128,
        allocation: &PlatformFeeAllocation,
    ) -> Result<PlatformFeeDistribution, Error> {
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
                .ok_or(Error::InvalidAmount)?;
            amounts[index] = weighted / scale;
            remainders[index] = weighted % scale;
            allocated = allocated
                .checked_add(amounts[index])
                .ok_or(Error::InvalidAmount)?;
        }

        // Largest-remainder allocation preserves every unit. Ties are resolved
        // by field order, making the result deterministic across runtimes.
        let mut remaining = total_amount
            .checked_sub(allocated)
            .ok_or(Error::InvalidAmount)?;
        while remaining > 0 {
            let mut best = 0_usize;
            for index in 1..3 {
                if remainders[index] > remainders[best] {
                    best = index;
                }
            }
            amounts[best] = amounts[best].checked_add(1).ok_or(Error::InvalidAmount)?;
            remainders[best] = -1;
            remaining -= 1;
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

    fn load_time_extension(env: &Env, index: u32) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::MilestoneTimeExtension(index))
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

    #[allow(dead_code)]
    fn checked_initialize_total(milestone_amounts: &Vec<i128>) -> Result<i128, Error> {
        if milestone_amounts.is_empty() {
            return Err(Error::InvalidAmount);
        }

        let mut total_amount: i128 = 0;
        for amount in milestone_amounts.iter() {
            total_amount = Self::checked_add_amount(total_amount, amount)?;
        }

        Ok(total_amount)
    }

    fn checked_job_total(env: &Env, meta: &JobMeta) -> Result<i128, Error> {
        let mut total_amount: i128 = 0;

        for index in 0..meta.milestone_count {
            let milestone = Self::load_milestone(env, index)?;
            total_amount = Self::checked_add_amount(total_amount, milestone.amount)?;
        }

        if total_amount != meta.total_amount {
            return Err(Error::InvalidAmount);
        }

        Ok(total_amount)
    }

    fn validate_fund_amount(env: &Env, meta: &JobMeta) -> Result<i128, Error> {
        let total_amount = Self::checked_job_total(env, meta)?;
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

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

    fn assemble_job(env: &Env, meta: &JobMeta) -> Result<Job, Error> {
        let mut milestones = Vec::new(env);
        for i in 0..meta.milestone_count {
            milestones.push_back(Self::load_milestone(env, i)?);
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
    /// token, and the milestone schedule. Must be called exactly once before
    /// any other endpoint (aside from read-only queries) will succeed. The
    /// escrow token is automatically added to the whitelist, and the
    /// platform fee allocation defaults to 100% freelancer / 0% client / 0%
    /// treasury.
    ///
    /// # Parameters
    /// * `admin`                 – Address that will control admin-only
    ///                             endpoints (whitelist, pause, overrides).
    ///                             Must authorize the call.
    /// * `client`                – Address that funds the job and approves
    ///                             milestone releases.
    /// * `freelancer`            – Address that delivers milestones and
    ///                             receives payouts.
    /// * `arbiter`                – Address that resolves disputes.
    /// * `token`                 – Settlement token contract address.
    /// * `auto_release_seconds`  – Seconds after delivery before a milestone
    ///                             becomes eligible for `claim_auto_release`.
    ///                             Must be non-zero.
    /// * `milestone_amounts`     – Amount owed for each milestone, in order.
    ///
    /// # Errors
    /// * `AlreadyInitialized` – The contract has already been initialized.
    /// * `InvalidAddress`     – Any of `admin`, `client`, `freelancer`,
    ///                          `arbiter`, or `token` is a zero address.
    /// * `InvalidAmount`      – `auto_release_seconds` is zero, or the total
    ///                          of `milestone_amounts` overflows.
    /// * `InvalidMilestone`   – `milestone_amounts` could not be read at a
    ///                          given index.
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
    ) -> Result<(), Error> {
        admin.require_auth();

        if env.storage().instance().has(&DataKey::Job) {
            return Err(Error::AlreadyInitialized);
        }

        // Write a sentinel immediately to prevent reentrancy: any reentrant
        // call to `initialize` will now see `DataKey::Job` already present and
        // return `AlreadyInitialized` before touching any other state.
        // The sentinel is a zero-value `JobMeta` placeholder; the real meta
        // overwrites it at the end of this function once all validation has
        // passed and milestones have been stored.
        env.storage().instance().set(
            &DataKey::Job,
            &JobMeta {
                client: admin.clone(),
                freelancer: admin.clone(),
                arbiter: admin.clone(),
                token: admin.clone(),
                funded: false,
                auto_release_seconds: 0,
                milestone_count: 0,
                total_amount: 0,
            },
        );

        Self::validate_address(&env, &admin)?;
        Self::validate_address(&env, &client)?;
        Self::validate_address(&env, &freelancer)?;
        Self::validate_address(&env, &arbiter)?;
        Self::validate_address(&env, &token)?;

        let milestone_count = milestone_amounts.len();
        let total_amount = Self::checked_initialize_total(&milestone_amounts)?;

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::EmergencyPaused, &false);
        env.storage().instance().set(
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
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);
        if auto_release_seconds == 0 {
            return Err(Error::InvalidAmount);
        }

        for index in 0..milestone_count {
            let amount = milestone_amounts
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
                },
            );
        }

        env.storage().persistent().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Version, &1u32);

        let mut whitelist: Vec<Address> = Vec::new(&env);
        whitelist.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        let meta = JobMeta {
            client,
            freelancer,
            arbiter,
            token,
            funded: false,
            auto_release_seconds,
            milestone_count,
            total_amount,
        };

        Self::store_job_meta(&env, &meta);

        // Emit a structured initialization event so downstream indexers can
        // record all operational parameters from a single on-chain event without
        // having to query contract storage separately.
        env.events().publish(
            (symbol_short!("init"),),
            InitializedEvent {
                client: meta.client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                auto_release_seconds: meta.auto_release_seconds,
                milestone_amounts,
                total_amount: meta.total_amount,
                milestone_count: meta.milestone_count,
            },
        );

        Ok(())
    }

    /// Transfer admin control of the contract to a new address.
    ///
    /// The new admin immediately gains access to all admin-only endpoints
    /// (whitelist management, pause/resume, emergency overrides, etc.); the
    /// previous admin loses that access.
    ///
    /// # Parameters
    /// * `current_admin` – Must match the currently stored admin. Must
    ///                     authorize the call.
    /// * `new_admin`     – Address to become the new admin.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `current_admin` is not the stored admin.
    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &current_admin)?;

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if current_admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        env.storage().persistent().set(&DataKey::Admin, &new_admin);

        env.events().publish(
            (symbol_short!("admin"),),
            TransferAdminEvent {
                old_admin: current_admin,
                new_admin,
            },
        );

        Ok(())
    }

    pub fn add_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

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

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

        let mut whitelist: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)?;

        // Duplicate check runs before the capacity check so that a
        // full whitelist still reports TokenAlreadyWhitelisted (rather
        // than InvalidAmount) for a token that's already present.
        if whitelist.contains(&token) {
            return Err(Error::TokenAlreadyWhitelisted);
        }

        if whitelist.len() >= MAX_WHITELIST_SIZE {
            return Err(Error::InvalidAmount);
        }

        whitelist.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        env.events().publish(
            (symbol_short!("wtok"),),
            TokenWhitelistedEvent { admin, token },
        );

        Ok(())
    }

    pub fn remove_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

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

        let mut whitelist: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)?;

        let whitelist_len = whitelist.len();
        if whitelist_len == 0 {
            return Err(Error::TokenNotWhitelisted);
        }

        let post_removal_len = whitelist_len.checked_sub(1).ok_or(Error::InvalidAmount)?;
        if post_removal_len == 0 {
            return Err(Error::InvalidAmount);
        }

        if !whitelist.contains(&token) {
            return Err(Error::TokenNotWhitelisted);
        }

        if let Some(index) = whitelist.iter().position(|t| t == token) {
            let last = whitelist.len() - 1;
            if (index as u32) != last {
                let last_elem = whitelist.get(last).unwrap();
                whitelist.set(index as u32, last_elem);
            }
            whitelist.pop_back();
            env.storage()
                .instance()
                .set(&DataKey::WhitelistedTokens, &whitelist);

            env.events().publish(
                (symbol_short!("wldel"),),
                TokenRemovedEvent { admin, token },
            );

            Ok(())
        } else {
            Err(Error::TokenNotWhitelisted)
        }
    }

    pub fn is_token_whitelisted(env: Env, token: Address) -> bool {
        if let Some(whitelist) = env
            .storage()
            .instance()
            .get::<_, Vec<Address>>(&DataKey::WhitelistedTokens)
        {
            whitelist.contains(&token)
        } else {
            false
        }
    }

    pub fn get_whitelisted_tokens(env: Env) -> Result<Vec<Address>, Error> {
        env.storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)
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
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::validate_fund_client(&env, &client)?;
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if meta.funded {
            return Err(Error::AlreadyFunded);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let total_amount = Self::validate_fund_amount(&env, &meta)?;

        // Update status BEFORE token transfer to ensure state is persisted
        // and prevent double-funding via reentrancy
        meta.funded = true;
        Self::store_job_meta(&env, &meta);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, &env.current_contract_address(), &total_amount);

        env.events().publish(
            (symbol_short!("fund"),),
            FundedEvent {
                contract_id: env.current_contract_address(),
                client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                total_amount,
                milestone_count: meta.milestone_count,
                auto_release_seconds: meta.auto_release_seconds,
                funded: meta.funded,
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
    pub fn mark_delivered(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
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
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        let delivered_at = env.ledger().timestamp();
        milestone.status = MilestoneStatus::Delivered;
        milestone.delivered_at = delivered_at;
        Self::store_milestone(&env, milestone_index, &milestone);
        // Write the delivery timestamp to temporary storage so that
        // claim_auto_release and time_until_auto_release can read it from the
        // optimised temporary tier without touching the persistent Milestone entry.
        Self::store_delivered_at(&env, milestone_index, delivered_at);

        env.events().publish(
            (symbol_short!("deliver"),),
            DeliveredEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                freelancer: meta.freelancer,
                client: meta.client,
                delivered_at,
                status: MilestoneStatus::Delivered,
                amount: milestone.amount,
            },
        );

        Ok(())
    }

    /// Extends the auto-release deadline for a Delivered milestone.
    pub fn extend_milestone_deadline(
        env: Env,
        client: Address,
        milestone_index: u32,
        extra_seconds: u64,
    ) -> Result<(), Error> {
        Self::assert_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }

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

        env.storage().persistent().set(
            &DataKey::MilestoneTimeExtension(milestone_index),
            &new_extension,
        );

        env.events().publish(
            (symbol_short!("extend"),),
            DeadlineExtendedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                client,
                extra_seconds,
                new_extension,
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
    /// - 1Ã— instance read  (`DataKey::Job` â†’ `JobMeta`)
    /// - 1Ã— temporary read (`DataKey::DeliveredAt(milestone_index)`)
    /// - 1Ã— persistent read  (`DataKey::Milestone(milestone_index)`)
    /// - 1Ã— persistent write (`DataKey::Milestone(milestone_index)`)
    /// - 1Ã— token transfer
    ///
    /// No loop over all milestones is performed here.  Functions that do loop
    /// over all milestones (`checked_job_total`, `assemble_job`) are
    /// intentionally not called from this hot path.
    pub fn claim_auto_release(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
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

        // CHECK 1: Validate index boundary.
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        // CHECK 2: Milestone must be in the Delivered state.  Any other status â€”
        // including Released (double-claim), Disputed, Refunded, Pending, or
        // PartiallyReleased â€” is rejected here, making the guard the sole
        // gatekeeper against double-execution and out-of-sequence calls.
        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        // CHECK 3: Validate auto_release_seconds is non-zero.
        if meta.auto_release_seconds == 0 {
            return Err(Error::InvalidAmount);
        }

        // CHECK 4: Read the delivery timestamp from temporary storage first
        //    (optimised ledger-footprint path).  Fall back to the value stored on
        //    the persistent Milestone entry so that entries written before this
        //    migration remain fully functional.
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let extension = Self::load_time_extension(&env, milestone_index);

        let deadline = delivered_at
            .checked_add(meta.auto_release_seconds)
            .and_then(|d| d.checked_add(extension))
            .ok_or(Error::InvalidAmount)?;
        let current = env.ledger().timestamp();
        if current < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        // CHECK 5: Compute remaining using checked subtraction so that corrupted
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
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::increment_reputation(&env, &meta.client);
        Self::increment_reputation(&env, &meta.freelancer);

        // INTERACTION: Token transfer is the sole external call and executes only
        //    after all state mutations have been durably persisted.
        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );

        env.events().publish(
            (symbol_short!("claim"),),
            ClaimedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                freelancer: meta.freelancer,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> i64 {
        let meta = Self::load_job_meta(&env).unwrap();
        let milestone = Self::load_milestone(&env, milestone_index).unwrap();
        // Read delivery timestamp from temporary storage (optimised path) and
        // fall back to the persistent Milestone field for pre-migration entries.
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let extension = Self::load_time_extension(&env, milestone_index);
        let deadline = delivered_at + meta.auto_release_seconds + extension;
        let current = env.ledger().timestamp();
        (deadline as i64) - (current as i64)
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
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
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
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;

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
        token_client.transfer(&env.current_contract_address(), &meta.freelancer, &amount);

        let mut updated_milestone = milestone;
        updated_milestone.released_amount = updated_milestone
            .released_amount
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;

        if updated_milestone.released_amount == updated_milestone.amount {
            updated_milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
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
            (symbol_short!("approve"),),
            ApprovedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                client: meta.client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                amount,
                released_amount: updated_milestone.released_amount,
                remaining: event_remaining,
                status: updated_milestone.status.clone(),
                milestone_count: meta.milestone_count,
                total_amount: meta.total_amount,
                auto_release_seconds: meta.auto_release_seconds,
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
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
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

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );
        milestone.released_amount = milestone.amount;

        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
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
            (symbol_short!("approve"),),
            ApprovedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                client: meta.client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                amount: remaining,
                released_amount: milestone.released_amount,
                remaining: event_remaining,
                status: milestone.status.clone(),
                milestone_count: meta.milestone_count,
                total_amount: meta.total_amount,
                auto_release_seconds: meta.auto_release_seconds,
            },
        );

        Ok(())
    }

    /// Raise a dispute on a milestone, freezing it for arbitration.
    ///
    /// Either the client or the freelancer may call this. Moves the
    /// milestone to `Disputed`, from which only `resolve_dispute` (or a
    /// split via `apply_dispute_arbitration_split`) can move it forward.
    ///
    /// # Parameters
    /// * `caller`          – Must be either the job's client or freelancer.
    ///                       Must authorize the call.
    /// * `milestone_index` – Index of the milestone being disputed.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `caller` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `caller` is neither the client nor the
    ///                        freelancer.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – The milestone is not `Pending`, `Delivered`,
    ///                        or `PartiallyReleased`.
    pub fn raise_dispute(env: Env, caller: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        // ── Re-entrancy lock ─────────────────────────────────────────────
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

        let result = Self::raise_dispute_inner(&env, caller, milestone_index);

        // Always release the lock regardless of success or failure.
        Self::release_dispute_lock(&env, milestone_index);

        result
    }

    /// Core dispute logic extracted so that the lock guard in
    /// `raise_dispute` wraps every path uniformly.  This function
    /// is never called directly — it exists only to keep the
    /// lock/release pairing in one place.
    fn raise_dispute_inner(env: &Env, caller: Address, milestone_index: u32) -> Result<(), Error> {
        // Check for zero addresses (both account and contract types)
        let zero_account = Address::from_str(
            env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }

        // require_dispute_party performs caller.require_auth() + verifies the
        // caller matches the stored client or freelancer in a single step.
        let meta = Self::require_dispute_party(env, &caller)?;

        if !meta.funded {
            return Err(Error::NotFunded);
        }

        // ── Input validation: index boundary check ───────────────────────
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(env, milestone_index)?;

        // ── Input validation: non-zero positive amount ───────────────────
        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Strict state machine: only Pending, Delivered, or PartiallyReleased
        // may transition to Disputed. All other statuses (Released, Refunded,
        // Disputed) are rejected.
        match milestone.status {
            MilestoneStatus::Pending
            | MilestoneStatus::Delivered
            | MilestoneStatus::PartiallyReleased => {}
            _ => return Err(Error::InvalidStatus),
        }

        milestone.status = MilestoneStatus::Disputed;
        Self::store_milestone(env, milestone_index, &milestone);

        // Write a short-lived dispute flag to temporary storage so that callers
        // can verify dispute status without loading the full persistent
        // Milestone entry, reducing ledger footprint on the read path.
        Self::store_dispute_flag(&env, milestone_index);

        env.events().publish(
            (symbol_short!("dispute"),),
            DisputeRaisedEvent {
                milestone_index,
                caller,
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
    /// * `InvalidAmount`    – The milestone's remaining balance, or the
    ///                        contract's token balance, is not positive.
    pub fn resolve_dispute(
        env: Env,
        arbiter: Address,
        milestone_index: u32,
        release_to_freelancer: bool,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::assert_tax_withholding_not_locked(&env)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
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
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

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
            | MilestoneStatus::Refunded => return Err(Error::InvalidStatus),
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        let payout = remaining.min(contract_balance);
        if release_to_freelancer {
            milestone.released_amount = milestone
                .released_amount
                .checked_add(payout)
                .ok_or(Error::InvalidAmount)?;
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone(&env, milestone_index, &milestone);

            if payout > 0 {
                token_client.transfer(&env.current_contract_address(), &meta.freelancer, &payout);
            }
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        } else {
            milestone.status = MilestoneStatus::Refunded;
            Self::store_milestone(&env, milestone_index, &milestone);

            if payout > 0 {
                token_client.transfer(&env.current_contract_address(), &meta.client, &payout);
            }
        }

        env.events().publish(
            (symbol_short!("resolve"),),
            DisputeResolvedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                arbiter: meta.arbiter.clone(),
                client: meta.client.clone(),
                freelancer: meta.freelancer.clone(),
                token: meta.token.clone(),
                // `amount` is what was owed before capping to the available
                // balance; `paid_amount` is what actually moved.
                amount: remaining,
                paid_amount: payout,
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
        if total_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        if client_refund_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        let scale = BPS_SCALE as i128;
        let client_refund = total_amount
            .checked_mul(client_refund_bps as i128)
            .ok_or(Error::InvalidAmount)?
            / scale;
        let freelancer_payout = total_amount
            .checked_sub(client_refund)
            .ok_or(Error::InvalidAmount)?;

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
        let client_refund_bps = BPS_SCALE - freelancer_bps;

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
    /// receives the remainder. Milestone ends `Refunded` when the freelancer
    /// share is zero, otherwise `Released`.
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
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
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

        let allocation = Self::allocate_refund_by_bps(remaining, client_refund_bps)?;

        let token_client = token::Client::new(&env, &meta.token);
        let contract_addr = env.current_contract_address();
        let contract_balance = token_client.balance(&contract_addr);
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Cap transfers to available contract balance while preserving the
        // proportional split intent (client first, then freelancer remainder).
        let client_refund = allocation.client_refund.min(contract_balance);
        let freelancer_cap = contract_balance
            .checked_sub(client_refund)
            .ok_or(Error::InvalidAmount)?;
        let freelancer_payout = allocation.freelancer_payout.min(freelancer_cap);

        if client_refund > 0 {
            token_client.transfer(&contract_addr, &meta.client, &client_refund);
        }
        if freelancer_payout > 0 {
            token_client.transfer(&contract_addr, &meta.freelancer, &freelancer_payout);
        }

        milestone.released_amount = milestone
            .released_amount
            .checked_add(freelancer_payout)
            .ok_or(Error::InvalidAmount)?;

        if freelancer_payout == 0 {
            milestone.status = MilestoneStatus::Refunded;
        } else {
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        }

        Self::store_milestone(&env, milestone_index, &milestone);

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
                contract_id: env.current_contract_address(),
                milestone_index,
                arbiter: meta.arbiter.clone(),
                client: meta.client.clone(),
                freelancer: meta.freelancer.clone(),
                token: meta.token.clone(),
                amount: remaining,
                paid_amount,
                released_to_freelancer: freelancer_payout > 0,
                status: milestone.status.clone(),
            },
        );

        Ok(resolved)
    }

    /// Calculate a cancellation allocation between the client and freelancer.
    ///
    /// The client share is rounded to the nearest stroop and the freelancer
    /// receives the exact remainder, so no value is lost to integer division.
    /// The ratios must sum to exactly `BPS_SCALE`.
    pub fn cancel_escrow_split_refund(
        _env: Env,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        if total_amount < 0 {
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

        Ok(RefundAllocation {
            client_refund: client_split.first,
            freelancer_payout: client_split.second,
            client_refund_bps,
            freelancer_payout_bps,
        })
    }

    /// Initiate cancellation of the escrow, freezing it pending an admin
    /// override.
    ///
    /// Either the client or the freelancer may call this. Sets a
    /// `CancelLock` that blocks normal operations until the admin resolves
    /// it via `admin_override_cancel_release` or
    /// `admin_override_cancel_refund`. This function itself does not move
    /// any funds.
    ///
    /// # Parameters
    /// * `caller` – Must be either the job's client or freelancer. Must
    ///              authorize the call.
    ///
    /// # Errors
    /// * `InvalidAddress` – `caller` is a zero address.
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `caller` is neither the client nor the
    ///                      freelancer.
    /// * `NotFunded`      – The escrow has not been funded yet.
    pub fn cancel_escrow(env: Env, caller: Address) -> Result<(), Error> {
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

        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if caller != meta.client && caller != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        // Boundary guard: cancelling against an empty escrow has no funds
        // to resolve, so block processing until the contract holds a
        // positive token balance.
        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        env.storage().instance().set(&DataKey::CancelLock, &true);

        env.events().publish(
            (symbol_short!("cancel"),),
            CancelEscrowInitiatedEvent {
                contract_id: env.current_contract_address(),
                caller,
            },
        );

        Ok(())
    }

    /// Admin emergency override: resolve a cancel-locked escrow by releasing
    /// all remaining milestone funds to the freelancer.
    ///
    /// When `cancel_escrow` is called by either party, a `CancelLock` is set
    /// that blocks all normal operations.  This endpoint lets the verified admin
    /// break the deadlock by force-releasing every non-terminal milestone to the
    /// freelancer in a single atomic transaction.
    ///
    /// # Checks (in order)
    /// 1. `admin.require_auth()` — SDK-level signature check.
    /// 2. `require_admin` — verified admin key matches `DataKey::Admin`.
    /// 3. Contract must be initialised (`NotInitialized`).
    /// 4. Escrow must be funded (`NotFunded`).
    /// 5. `CancelLock` must be active (`InvalidStatus`).
    ///
    /// # Effects
    /// - Every milestone in a non-terminal status (`!Released && !Refunded`)
    ///   is moved to `Released` and its remaining balance is summed.
    /// - The total is transferred from the contract to the freelancer in a
    ///   single token call.
    /// - `CancelLock` is cleared so subsequent queries are unblocked.
    /// - `YieldAccrued` is reset to zero (matches the pattern used by
    ///   `admin_override_release` / `admin_override_refund`).
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract not initialised.
    /// * `Unauthorized`    – Caller is not the verified admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidStatus`   – `CancelLock` is not active.
    /// * `InvalidAmount`   – Total remaining balance is zero (nothing to pay out).
    pub fn admin_override_cancel_release(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        // Only valid when a cancel lock is active.
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if !cancel_locked {
            return Err(Error::InvalidStatus);
        }

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        // Walk every milestone; accumulate remaining balance and mark Released.
        let mut total_released: i128 = 0;
        for index in 0..meta.milestone_count {
            let mut milestone = Self::load_milestone(&env, index)?;
            if milestone.status == MilestoneStatus::Released
                || milestone.status == MilestoneStatus::Refunded
            {
                continue;
            }
            let remaining = milestone
                .amount
                .checked_sub(milestone.released_amount)
                .ok_or(Error::InvalidAmount)?;
            if remaining > 0 {
                total_released = total_released
                    .checked_add(remaining)
                    .ok_or(Error::InvalidAmount)?;
                milestone.released_amount = milestone.amount;
                milestone.status = MilestoneStatus::Released;
                // Write only the persistent Milestone entry.  The temporary
                // MilestoneReleased flag is omitted here: it is a hot-read
                // optimisation for the approve_milestone path and is redundant
                // in this admin-override code path because the persistent
                // status already carries the Released state.  Skipping it
                // reduces the number of distinct ledger keys written by this
                // function by one per updated milestone (issue #383).
                Self::store_milestone(&env, index, &milestone);
            }
        }

        if total_released <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: clear the lock and reset yield before the external transfer.
        env.storage().instance().set(&DataKey::CancelLock, &false);
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &0_i128);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &total_released,
        );

        env.events().publish(
            (symbol_short!("adcovls"),),
            AdminCancelOverrideReleaseEvent {
                admin,
                contract_id: env.current_contract_address(),
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
    /// 5. `CancelLock` must be active (`InvalidStatus`).
    ///
    /// # Effects
    /// - Every milestone in a non-terminal status is moved to `Refunded` and
    ///   its remaining balance is summed.
    /// - The total is transferred from the contract to the client in a single
    ///   token call.
    /// - `CancelLock` is cleared.
    /// - `YieldAccrued` is reset to zero.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract not initialised.
    /// * `Unauthorized`    – Caller is not the verified admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidStatus`   – `CancelLock` is not active.
    /// * `InvalidAmount`   – Total remaining balance is zero (nothing to refund).
    pub fn admin_override_cancel_refund(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        // Only valid when a cancel lock is active.
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if !cancel_locked {
            return Err(Error::InvalidStatus);
        }

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        // Walk every milestone; accumulate remaining balance and mark Refunded.
        let mut total_refunded: i128 = 0;
        for index in 0..meta.milestone_count {
            let mut milestone = Self::load_milestone(&env, index)?;
            if milestone.status == MilestoneStatus::Released
                || milestone.status == MilestoneStatus::Refunded
            {
                continue;
            }
            let remaining = {
                // Guard: both fields must be non-negative before arithmetic.
                // A malformed entry with a negative amount or released_amount
                // (e.g. i128::MIN) could yield a nonsensical positive
                // `remaining` after wrapping; rejecting here keeps the
                // guarantee that every exit path either refunds a valid
                // positive total or returns Error::InvalidAmount (issue #386).
                if milestone.amount < 0 || milestone.released_amount < 0 {
                    return Err(Error::InvalidAmount);
                }
                milestone
                    .amount
                    .checked_sub(milestone.released_amount)
                    .ok_or(Error::InvalidAmount)?
            };
            if remaining > 0 {
                total_refunded = total_refunded
                    .checked_add(remaining)
                    .ok_or(Error::InvalidAmount)?;
                milestone.released_amount = milestone.amount;
                milestone.status = MilestoneStatus::Refunded;
                Self::store_milestone(&env, index, &milestone);
            }
        }

        if total_refunded <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: clear the lock and reset yield before the external transfer.
        env.storage().instance().set(&DataKey::CancelLock, &false);
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &0_i128);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.client,
            &total_refunded,
        );

        env.events().publish(
            (symbol_short!("adcovrf"),),
            AdminCancelOverrideRefundEvent {
                admin,
                contract_id: env.current_contract_address(),
                client: meta.client,
                token: meta.token,
                total_refunded,
            },
        );

        Ok(())
    }

    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        env.deployer().update_current_contract_wasm(new_wasm_hash);

        let current: u32 = env.storage().instance().get(&DataKey::Version).unwrap_or(1);
        env.storage()
            .instance()
            .set(&DataKey::Version, &(current + 1));

        Ok(())
    }

    /// Freeze the escrow: set `DataKey::EmergencyPaused`, blocking every
    /// endpoint guarded by `ensure_not_paused`.
    ///
    /// # Business rules
    /// Bad setups are rejected before any state is written, each with a
    /// distinct error variant:
    ///
    /// 1. The contract must be initialised — `require_admin` loads the stored
    ///    admin key and returns `NotInitialized` when it is absent.  Pausing
    ///    an uninitialised contract would write a flag no endpoint could ever
    ///    clear through the normal admin path.
    /// 2. The caller must be the stored admin, both at the SDK level
    ///    (`admin.require_auth()`) and by key comparison (`Unauthorized`).
    /// 3. No pause transition may already be mid-execution
    ///    (`EmergencyPauseInProgress`).
    /// 4. The contract must not already be paused (`AlreadyPaused`).  A
    ///    redundant pause previously succeeded silently, which let an operator
    ///    believe they had taken fresh action during an incident when the
    ///    freeze was in fact already in place.
    ///
    /// # Errors
    /// * `NotInitialized`            – Admin key has never been stored.
    /// * `Unauthorized`              – `admin` is not the stored admin.
    /// * `EmergencyPauseInProgress`  – A pause transition is already running.
    /// * `AlreadyPaused`             – The contract is already paused.
    pub fn emergency_pause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        Self::assert_emergency_pause_not_locked(&env)?;

        if Self::is_emergency_paused(env.clone()) {
            return Err(Error::AlreadyPaused);
        }

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &true);

        let result = (|| {
            env.storage()
                .instance()
                .set(&DataKey::EmergencyPaused, &true);
            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &false);

        if result.is_ok() {
            env.events().publish(
                (symbol_short!("empause"),),
                EmergencyPausedEvent {
                    admin: admin.clone(),
                    contract_id: env.current_contract_address(),
                },
            );
        }

        result
    }

    /// Lift an emergency freeze, restoring normal operation.
    ///
    /// # Business rules
    /// Mirrors [`emergency_pause`]: the contract must be initialised, the
    /// caller must be the stored admin, no transition may be mid-execution,
    /// and the contract must actually be paused.  Unpausing a running
    /// contract is rejected with `NotPaused` rather than silently succeeding,
    /// so a mistaken call is visible to the operator instead of reading as a
    /// completed recovery.
    ///
    /// # Errors
    /// * `NotInitialized`            – Admin key has never been stored.
    /// * `Unauthorized`              – `admin` is not the stored admin.
    /// * `EmergencyPauseInProgress`  – A pause transition is already running.
    /// * `NotPaused`                 – The contract is not currently paused.
    pub fn emergency_unpause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        Self::assert_emergency_pause_not_locked(&env)?;

        if !Self::is_emergency_paused(env.clone()) {
            return Err(Error::NotPaused);
        }

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &true);

        let result = (|| {
            env.storage()
                .instance()
                .set(&DataKey::EmergencyPaused, &false);
            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &false);

        if result.is_ok() {
            env.events().publish(
                (symbol_short!("emunpause"),),
                EmergencyUnpausedEvent {
                    admin: admin.clone(),
                    contract_id: env.current_contract_address(),
                },
            );
        }

        result
    }

    pub fn emergency_pause_admin_override(
        env: Env,
        admin: Address,
        paused: bool,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let current = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::EmergencyPaused)
            .unwrap_or(false);

        if current == paused {
            return Err(Error::InvalidStatus);
        }

        Self::assert_emergency_pause_not_locked(&env)?;

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &true);

        let result = (|| {
            env.storage()
                .instance()
                .set(&DataKey::EmergencyPaused, &paused);
            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &false);

        if result.is_ok() {
            env.events().publish(
                (symbol_short!("emoverrid"),),
                EmergencyPauseAdminOverrideEvent {
                    admin: admin.clone(),
                    contract_id: env.current_contract_address(),
                    paused,
                },
            );
        }

        result
    }

    pub fn is_emergency_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::EmergencyPaused)
            .unwrap_or(false)
    }

    pub fn set_platform_fee_allocation(
        env: Env,
        admin: Address,
        client_bps: u32,
        freelancer_bps: u32,
        treasury_bps: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &true);

        let result = (|| {
            let current: PlatformFeeAllocation = env
                .storage()
                .instance()
                .get(&DataKey::PlatformFeeAllocation)
                .ok_or(Error::NotInitialized)?;

            if current.locked {
                return Err(Error::InvalidStatus);
            }

            env.storage().instance().set(
                &DataKey::PlatformFeeAllocation,
                &PlatformFeeAllocation {
                    client_bps,
                    freelancer_bps,
                    treasury_bps,
                    locked: false,
                },
            );
            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &false);

        result
    }

    pub fn lock_platform_fee_allocation(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &true);

        let result = (|| {
            let mut current: PlatformFeeAllocation = env
                .storage()
                .instance()
                .get(&DataKey::PlatformFeeAllocation)
                .ok_or(Error::NotInitialized)?;
            current.locked = true;
            env.storage()
                .instance()
                .set(&DataKey::PlatformFeeAllocation, &current);
            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &false);

        result
    }

    pub fn pf_alloc_admin_override(
        env: Env,
        admin: Address,
        client_bps: u32,
        freelancer_bps: u32,
        treasury_bps: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let current: PlatformFeeAllocation = env
            .storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)?;

        if !current.locked {
            return Err(Error::InvalidStatus);
        }

        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &true);

        let result = (|| {
            env.storage().instance().set(
                &DataKey::PlatformFeeAllocation,
                &PlatformFeeAllocation {
                    client_bps,
                    freelancer_bps,
                    treasury_bps,
                    locked: false,
                },
            );
            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &false);

        result
    }

    pub fn get_platform_fee_allocation(env: Env) -> Result<PlatformFeeAllocation, Error> {
        env.storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)
    }

    /// Split an amount according to the configured platform-fee ratios.
    ///
    /// Each component is calculated with checked integer arithmetic. Any
    /// units left after flooring are assigned by largest remainder, so the
    /// three returned amounts always sum exactly to `total_amount`.
    pub fn calculate_platform_fee_split(
        env: Env,
        total_amount: i128,
    ) -> Result<PlatformFeeDistribution, Error> {
        let allocation: PlatformFeeAllocation = env
            .storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)?;
        Self::allocate_platform_fee(total_amount, &allocation)
    }

    pub fn payment_streaming_milestones(
        env: Env,
        total_amount: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        // Guard: reject zero or negative totals so that streaming operations
        // are never initiated on an empty balance.  A zero total would
        // distribute nothing to either party and signals a misconfigured or
        // already-drained escrow.
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if denominator <= 0 {
            return Err(Error::InvalidRatio);
        }
        if numerator < 0 || numerator > denominator {
            return Err(Error::InvalidRatio);
        }

        let split = Self::split_round_nearest(total_amount, numerator, denominator)?;

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

        Ok(split)
    }

    /// Compute a streaming milestone split that requires **dual consent**:
    /// both the client and the freelancer must independently sign the
    /// transaction.
    ///
    /// `payment_streaming_milestones` is an unauthenticated calculator — any
    /// caller may ask it what a given ratio works out to.  This endpoint is
    /// the consent-gated counterpart, for deployments that want a streaming
    /// settlement to be agreed by both parties before it is computed and
    /// recorded on-chain.
    ///
    /// # Signature collection
    /// [`require_client_and_freelancer_consent`] calls `require_auth()` on the
    /// client address and then on the freelancer address, both taken from the
    /// stored job metadata rather than from caller-supplied arguments.  If
    /// either signature is missing from the transaction the host-level auth
    /// check panics before any ratio validation runs, so a single-signature
    /// attempt reverts the invocation entirely.  Neither party can be
    /// impersonated by passing a different address, because no address is
    /// accepted as a parameter.
    ///
    /// # Parameters
    /// * `total_amount` – Total streaming amount; must be > 0.
    /// * `numerator`    – Streamed portion; must satisfy 0 ≤ n ≤ denominator.
    /// * `denominator`  – Ratio denominator; must be > 0.
    ///
    /// # Returns
    /// A `RatioSplit` where `first` is the streamed payout and `second` is the
    /// client refund.  The two always sum to `total_amount` exactly.
    ///
    /// # Errors
    /// * `NotInitialized` – Job metadata missing, so no signers are known.
    /// * `InvalidAmount`  – `total_amount` ≤ 0.
    /// * `InvalidRatio`   – `denominator` ≤ 0, or `numerator` outside
    ///   `0..=denominator`.
    pub fn payment_streaming_consent(
        env: Env,
        total_amount: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        // Collect both signatures first: an unauthorised caller must not be
        // able to probe the validation rules below.
        let meta = Self::require_client_and_freelancer_consent(&env)?;

        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if denominator <= 0 {
            return Err(Error::InvalidRatio);
        }
        if numerator < 0 || numerator > denominator {
            return Err(Error::InvalidRatio);
        }

        let split = Self::split_round_nearest(total_amount, numerator, denominator)?;

        env.events().publish(
            (symbol_short!("p_strcns"),),
            PaymentStreamingConsentEvent {
                client: meta.client,
                freelancer: meta.freelancer,
                total_amount,
                numerator,
                denominator,
                streamed_payout: split.first,
                client_refund: split.second,
            },
        );

        Ok(split)
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
        // Guard: a zero or negative balance means there is nothing left in
        // this milestone to distribute.  Operations on an empty balance would
        // produce a split of (0, 0) which is a no-op and signals a
        // misconfigured or already-drained escrow.
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Reject nonsensical time inputs before the generic ratio guard so
        // callers get a precise error code for time-specific misuse.
        if total_seconds <= 0 {
            return Err(Error::InvalidRatio);
        }
        if elapsed_seconds < 0 || elapsed_seconds > total_seconds {
            return Err(Error::InvalidRatio);
        }

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
    }

    pub fn multisig_transfer_admin(
        env: Env,
        admin: Address,
        total_amount: i128,
        ratios: Vec<i128>,
    ) -> Result<Vec<i128>, Error> {
        // Only the stored admin may trigger a multi-party transfer.
        Self::require_admin(&env, &admin)?;

        // Guard: reject zero or negative totals so that a multisig transfer
        // cannot be initiated against an empty or invalid balance.  A zero
        // total would distribute nothing and signals a drained or
        // misconfigured escrow.
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if ratios.is_empty() {
            return Err(Error::InvalidRatio);
        }

        if ratios.len() > MAX_MULTISIG_RATIO_COUNT {
            return Err(Error::InvalidAmount);
        }

        let mut ratio_sum: i128 = 0;
        for ratio in ratios.iter() {
            if ratio < 0 {
                return Err(Error::InvalidRatio);
            }
            ratio_sum = ratio_sum.checked_add(ratio).ok_or(Error::InvalidRatio)?;
        }

        if ratio_sum <= 0 {
            return Err(Error::InvalidRatio);
        }

        let mut allocations: Vec<i128> = Vec::new(&env);
        let mut remainders: Vec<i128> = Vec::new(&env);
        let mut allocated_total: i128 = 0;

        for ratio in ratios.iter() {
            let weighted = total_amount
                .checked_mul(ratio)
                .ok_or(Error::InvalidAmount)?;
            let base = weighted / ratio_sum;
            let rem = weighted % ratio_sum;

            allocations.push_back(base);
            remainders.push_back(rem);
            allocated_total = allocated_total
                .checked_add(base)
                .ok_or(Error::InvalidAmount)?;
        }

        let remaining = total_amount
            .checked_sub(allocated_total)
            .ok_or(Error::InvalidAmount)?;

        for _ in 0..remaining {
            let mut best_index: u32 = 0;
            let mut best_remainder: i128 = i128::MIN;

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
            remainders.set(best_index, i128::MIN);
        }

        // Emit a structured event so downstream indexers can audit every
        // multi-party admin transfer without reading contract storage directly.
        let num_parties = allocations.len();
        env.events().publish(
            (symbol_short!("msigtrx"),),
            MultiSigTransferAdminEvent {
                total_amount,
                num_parties,
                allocations: allocations.clone(),
            },
        );

        Ok(allocations)
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
    // 1. **Signer list is stored once** (instance storage) under a single
    //    `MultiSigSigners` key rather than storing individual key-value pairs
    //    per signer.
    //
    // 2. **Approval tracking uses a compact u32 bitmap** in temporary storage
    //    under `MultiSigApproval(proposal_id)`.  Each bit represents one signer
    //    by its index in the signers vec, eliminating the Address overhead from
    //    every approval entry.  Up to 32 signers are supported per proposal.
    //
    // 3. **Temporary storage tier** is used for the bitmap so that the ledger
    //    footprint is automatically evicted once the proposal lifecycle ends,
    //    rather than persisting indefinitely.

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
                if signers.get(i).unwrap() == signers.get(j).unwrap() {
                    return Err(Error::MultiSigDuplicateSigner);
                }
                j += 1;
            }
            i += 1;
        }

        Ok(())
    }

    /// Initialise a multisig approval regime with a fixed set of signers and
    /// the required approval threshold.  Must be called exactly once.
    pub fn multisig_approval_init(
        env: Env,
        admin: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        if env.storage().instance().has(&DataKey::MultiSigSigners) {
            return Err(Error::AlreadyInitialized);
        }

        Self::validate_multisig_setup(&signers, threshold)?;

        env.storage()
            .instance()
            .set(&DataKey::MultiSigSigners, &signers);
        env.storage()
            .instance()
            .set(&DataKey::MultiSigThreshold, &threshold);

        Ok(())
    }

    /// Record an approval from one of the registered signers for the given
    /// proposal.  Idempotent — calling twice from the same signer has no
    /// effect and is not an error.
    pub fn multisig_approve(
        env: Env,
        signer: Address,
        proposal_id: u32,
    ) -> Result<MultiSigApprovalState, Error> {
        signer.require_auth();

        // Boundary guard: an approval collected against an empty escrow has
        // no funds behind it, so block processing until the contract holds
        // a positive token balance.
        let meta = Self::load_job_meta(&env)?;
        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::MultiSigEmptyBalance);
        }

        let signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigSigners)
            .ok_or(Error::NotInitialized)?;

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .ok_or(Error::NotInitialized)?;

        // Find the signer's index in the list (O(n) but n ≤ 32).
        let signer_index = signers
            .iter()
            .position(|s| s == signer)
            .ok_or(Error::Unauthorized)?;

        // Read the current bitmap from temporary storage (default: 0 = no approvals).
        let mut bitmap: u32 = env
            .storage()
            .temporary()
            .get(&DataKey::MultiSigApproval(proposal_id))
            .unwrap_or(0);

        // Set the bit for this signer (idempotent).
        let idx: u32 = signer_index.try_into().map_err(|_| Error::InvalidAmount)?;
        let mask = 1u32.checked_shl(idx).ok_or(Error::InvalidAmount)?;
        bitmap |= mask;

        // Write the updated bitmap back to temporary storage.
        env.storage()
            .temporary()
            .set(&DataKey::MultiSigApproval(proposal_id), &bitmap);

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
    /// Pure read — does not require auth and does not mutate state.
    pub fn is_multisig_approved(
        env: Env,
        proposal_id: u32,
    ) -> Result<MultiSigApprovalState, Error> {
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .ok_or(Error::NotInitialized)?;

        let bitmap: u32 = env
            .storage()
            .temporary()
            .get(&DataKey::MultiSigApproval(proposal_id))
            .unwrap_or(0);

        let approvals = bitmap.count_ones();

        Ok(MultiSigApprovalState {
            approved: approvals >= threshold,
            approvals,
            threshold,
            bitmap,
        })
    }

    // ── multisig_transfer_admin: transaction status lock ───────────────────
    //
    // `propose_admin_transfer` / `execute_admin_transfer` /
    // `cancel_admin_transfer_proposal` build a status lock on top of the
    // generic multisig approval bitmap above: once a transfer is proposed,
    // `DataKey::PendingAdminTransfer` is set and no further proposal can be
    // created until the pending one executes or is explicitly cancelled by
    // the admin. This prevents the signer approvals already being collected
    // for one `new_admin` from being silently redirected mid-flight by a
    // second, overlapping proposal.

    /// Propose a new admin via the multisig approval workflow.
    ///
    /// # Errors
    /// * `NotInitialized`       – Contract not initialised.
    /// * `Unauthorized`         – Caller is not the stored admin.
    /// * `InvalidAddress`       – `new_admin` is a zero address.
    /// * `AdminTransferPending` – A proposal is already pending; execute or
    ///   cancel it before proposing another.
    pub fn propose_admin_transfer(
        env: Env,
        admin: Address,
        new_admin: Address,
        proposal_id: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if new_admin == zero_account || new_admin == zero_contract {
            return Err(Error::InvalidAddress);
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
        {
            return Err(Error::AdminTransferPending);
        }

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

        Ok(())
    }

    /// Execute a pending admin transfer once its multisig proposal has
    /// reached the configured approval threshold. Any caller may trigger
    /// execution — the safety guarantee comes from the collected signer
    /// approvals, not caller identity — but nothing happens unless
    /// `is_multisig_approved` reports the threshold met.
    ///
    /// # Errors
    /// * `NoPendingAdminTransfer`  – No transfer is currently proposed.
    /// * `NotInitialized`          – Multisig has not been initialised.
    /// * `MultiSigThresholdNotMet` – Approvals collected so far are below the
    ///   required threshold.
    pub fn execute_admin_transfer(env: Env) -> Result<(), Error> {
        let pending: PendingAdminTransfer = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .ok_or(Error::NoPendingAdminTransfer)?;

        let state = Self::is_multisig_approved(env.clone(), pending.proposal_id)?;
        if !state.approved {
            return Err(Error::MultiSigThresholdNotMet);
        }

        let old_admin = Self::load_admin(&env)?;
        env.storage()
            .persistent()
            .set(&DataKey::Admin, &pending.new_admin);
        env.storage()
            .persistent()
            .remove(&DataKey::PendingAdminTransfer);

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
    /// one can be proposed. Only the current admin may cancel.
    ///
    /// # Errors
    /// * `NotInitialized`         – Contract not initialised.
    /// * `Unauthorized`           – Caller is not the stored admin.
    /// * `NoPendingAdminTransfer` – Nothing is currently pending.
    pub fn cancel_admin_transfer_proposal(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let pending: PendingAdminTransfer = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .ok_or(Error::NoPendingAdminTransfer)?;

        env.storage()
            .persistent()
            .remove(&DataKey::PendingAdminTransfer);

        env.events().publish(
            (symbol_short!("admincxl"),),
            AdminTransferCancelledEvent {
                admin,
                proposal_id: pending.proposal_id,
            },
        );

        Ok(())
    }

    /// Return the currently pending admin-transfer proposal, if any.
    pub fn get_pending_admin_transfer(env: Env) -> Option<PendingAdminTransfer> {
        env.storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
    }

    pub fn version(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::Version).unwrap_or(1)
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

    pub fn get_reputation(env: Env, address: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Reputation(address))
            .unwrap_or(0)
    }

    // ── escrow_interest_yield: estimator + share-config validation ────────────

    /// Estimate the interest yield that would accrue on an escrowed balance
    /// over a given duration, using a simple-interest model.
    ///
    /// # Parameters
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
    /// * `InvalidAmount` – Zero/negative principal, rate, or duration, or an
    ///                     intermediate checked multiplication overflows.
    /// * `InvalidRatio`  – `annual_rate_bps` exceeds 10_000 (unsupported).
    pub fn escrow_interest_yield(
        _env: Env,
        principal: i128,
        annual_rate_bps: i128,
        duration_seconds: i128,
    ) -> Result<i128, Error> {
        Self::validate_interest_yield_params(principal, annual_rate_bps, duration_seconds)?;

        let numerator = principal
            .checked_mul(annual_rate_bps)
            .ok_or(Error::InvalidAmount)?
            .checked_mul(duration_seconds)
            .ok_or(Error::InvalidAmount)?;

        let denominator = BPS_DENOMINATOR
            .checked_mul(SECONDS_PER_YEAR)
            .ok_or(Error::InvalidAmount)?;

        numerator
            .checked_div(denominator)
            .ok_or(Error::InvalidAmount)
    }

    /// Initialize or update interest/yield share configuration (unlocked by
    /// default on first write). Rejects invalid share totals and modifications
    /// while an execution lock is held.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract admin key is missing.
    /// * `Unauthorized`   – Caller is not the stored admin.
    /// * `InvalidRatio`   – Shares do not sum to exactly 10_000 bps.
    /// * `EscrowLocked`   – Configuration is locked for execution.
    pub fn set_escrow_interest_yield(
        env: Env,
        admin: Address,
        client_share_bps: u32,
        freelancer_share_bps: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        Self::validate_interest_yield_share_config(client_share_bps, freelancer_share_bps)?;

        if env.storage().instance().has(&DataKey::InterestYieldState) {
            Self::ensure_interest_yield_unlocked(&env)?;
        }

        Self::store_interest_yield_state(
            &env,
            &EscrowInterestYieldState {
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
    /// `set_escrow_interest_yield` lets the platform admin unilaterally
    /// reallocate yield between the two parties. This endpoint exists for
    /// deployments that want to remove that single point of trust for
    /// share changes: a compromised or malicious admin key alone cannot move
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
        Self::require_admin(&env, &admin)?;
        let meta = Self::load_job_meta(&env)?;

        // Both parties must independently sign this transaction. A missing
        // signature causes require_auth to panic at the host level.
        meta.client.require_auth();
        meta.freelancer.require_auth();

        Self::validate_interest_yield_share_config(client_share_bps, freelancer_share_bps)?;

        if env.storage().instance().has(&DataKey::InterestYieldState) {
            Self::ensure_interest_yield_unlocked(&env)?;
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
    pub fn lock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let mut state = Self::load_interest_yield_state(&env)?;
        state.locked = true;
        Self::store_interest_yield_state(&env, &state);
        Ok(())
    }

    /// Clear the execution lock so share configuration can be modified again.
    pub fn unlock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let mut state = Self::load_interest_yield_state(&env)?;
        state.locked = false;
        Self::store_interest_yield_state(&env, &state);
        Ok(())
    }

    /// Return whether the interest/yield share configuration is locked.
    pub fn is_escrow_interest_yield_locked(env: Env) -> Result<bool, Error> {
        Ok(Self::load_interest_yield_state(&env)?.locked)
    }

    /// Return the stored interest/yield share configuration.
    ///
    /// # Errors
    /// * `NotInitialized` – Configuration has never been set.
    pub fn get_escrow_interest_yield(env: Env) -> Result<EscrowInterestYieldState, Error> {
        Self::load_interest_yield_state(&env)
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_emergency_pause;
#[cfg(test)]
mod test_payment_streaming_milestones;
#[cfg(test)]
mod admin_override_cancel_tests;
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
    /// * `NotInitialized`   – Contract has not been initialised yet.
    /// * `Unauthorized`     – `admin` does not match the stored admin key.
    /// * `YieldRateInvalid` – `rate_bps` exceeds 10 000.
    pub fn admin_set_yield_rate(env: Env, admin: Address, rate_bps: u32) -> Result<(), Error> {
        // `require_admin` performs `admin.require_auth()` + stored-key check.
        Self::require_admin(&env, &admin)?;
        Self::validate_yield_rate_bps(rate_bps)?;

        let old_rate_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldRateBps)
            .unwrap_or(0);

        env.storage()
            .persistent()
            .set(&DataKey::YieldRateBps, &rate_bps);

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
        Self::require_admin(&env, &admin)?;

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
    /// This is the primary remedy for an escrow where the client is
    /// unresponsive or has lost their key after the freelancer has completed
    /// the work.  The milestone is moved to `Released` and a full token
    /// transfer is executed.
    ///
    /// The override works on any non-terminal milestone status (Pending,
    /// Delivered, PartiallyReleased, Disputed).  Calling it on an already
    /// `Released` or `Refunded` milestone — where the funds have already left
    /// the contract — returns `InvalidStatus` to prevent a double-spend.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `NotFunded`       – Escrow has not been funded; nothing to release.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0 (sanity guard).
    pub fn admin_override_release(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        // Terminal states have already settled funds — no double-spend.
        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
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

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::store_milestone_released(&env, milestone_index);

        // Reset accrued yield on emergency override
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &0_i128);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );

        env.events().publish(
            (symbol_short!("admovrls"),),
            AdminOverrideReleaseEvent {
                admin,
                contract_id: env.current_contract_address(),
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
    /// Use this when the freelancer is unresponsive, the work was never
    /// delivered, or the arbiter cannot be reached.  The milestone is moved to
    /// `Refunded` and a full token transfer is executed back to the client.
    ///
    /// Like `admin_override_release`, this operates on any non-terminal status
    /// and returns `InvalidStatus` for already-settled milestones.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0 (sanity guard).
    pub fn admin_override_refund(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
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

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Reset accrued yield on emergency override
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &0_i128);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &meta.client, &remaining);

        env.events().publish(
            (symbol_short!("admovrf"),),
            AdminOverrideRefundEvent {
                admin,
                contract_id: env.current_contract_address(),
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
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
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

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let split = Self::milestone_time_extensions(
            env.clone(),
            remaining,
            elapsed_seconds,
            total_seconds,
        )?;

        let token_client = token::Client::new(&env, &meta.token);
        let contract_addr = env.current_contract_address();
        let contract_balance = token_client.balance(&contract_addr);
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Cap transfers to available contract balance while preserving the
        // proportional split intent (client first, then freelancer remainder).
        let client_refund = split.second.min(contract_balance);
        let freelancer_cap = contract_balance
            .checked_sub(client_refund)
            .ok_or(Error::InvalidAmount)?;
        let freelancer_payout = split.first.min(freelancer_cap);

        if client_refund > 0 {
            token_client.transfer(&contract_addr, &meta.client, &client_refund);
        }
        if freelancer_payout > 0 {
            token_client.transfer(&contract_addr, &meta.freelancer, &freelancer_payout);
        }

        milestone.released_amount = milestone
            .released_amount
            .checked_add(freelancer_payout)
            .ok_or(Error::InvalidAmount)?;

        if freelancer_payout == 0 {
            milestone.status = MilestoneStatus::Refunded;
        } else {
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        }

        Self::store_milestone(&env, milestone_index, &milestone);

        env.events().publish(
            (symbol_short!("admstrm"),),
            AdminOverrideStreamingReleaseEvent {
                admin,
                contract_id: contract_addr,
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
    /// Calling this on an already-paused escrow is a no-op (idempotent) so
    /// that automated retry logic cannot produce an error.
    ///
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    pub fn admin_pause_escrow(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &true);

        let result = (|| {
            let already_paused: bool = env
                .storage()
                .instance()
                .get(&DataKey::Paused)
                .unwrap_or(false);

            env.storage().instance().set(&DataKey::Paused, &true);

            if !already_paused {
                env.events().publish(
                    (symbol_short!("pause"),),
                    EscrowPausedEvent {
                        admin: admin.clone(),
                        contract_id: env.current_contract_address(),
                    },
                );
            }

            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &false);

        result
    }

    /// Resume a previously paused escrow, re-enabling all normal user-facing
    /// endpoints.
    ///
    /// Calling this on an escrow that is not paused is a no-op (idempotent).
    ///
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    pub fn admin_resume_escrow(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &true);

        let result = (|| {
            let currently_paused: bool = env
                .storage()
                .instance()
                .get(&DataKey::Paused)
                .unwrap_or(false);

            env.storage().instance().set(&DataKey::Paused, &false);

            if currently_paused {
                env.events().publish(
                    (symbol_short!("resume"),),
                    EscrowResumedEvent {
                        admin: admin.clone(),
                        contract_id: env.current_contract_address(),
                    },
                );
            }

            Ok(())
        })();

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPauseLock, &false);

        result
    }

    // ── tax_withholding_deductions ────────────────────────────────────────────

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
    /// * `InvalidStatus`    – Milestone is already terminal (Released/Refunded).
    /// * `InvalidRatio`     – `tax_rate_bps > 10_000`.
    /// * `InvalidAmount`    – Remaining balance is zero or arithmetic overflow.
    pub fn tax_withholding_deductions(
        env: Env,
        milestone_index: u32,
        tax_rate_bps: u32,
    ) -> Result<TaxWithholdingRecord, Error> {
        let meta = Self::load_job_meta(&env)?;

        meta.client.require_auth();
        meta.freelancer.require_auth();

        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        if tax_rate_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
        {
            return Err(Error::InvalidStatus);
        }

        let token_client = token::Client::new(&env, &meta.token);
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

        // tax_amount = round_nearest(gross × rate / 10_000)
        let tax_split =
            Self::split_round_nearest(gross_amount, tax_rate_bps as i128, BPS_SCALE as i128)?;
        let tax_amount = tax_split.first;
        let net_amount = tax_split.second;

        let record = TaxWithholdingRecord {
            gross_amount,
            tax_amount,
            net_amount,
            tax_rate_bps,
        };

        // Persist the record so admin override endpoints can read it without
        // recomputing tax arithmetic.
        env.storage()
            .persistent()
            .set(&DataKey::TaxWithholdingLock(milestone_index), &record);

        env.events().publish(
            (symbol_short!("taxwith"),),
            TaxWithholdingAppliedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                gross_amount,
                tax_amount,
                net_amount,
                tax_rate_bps,
            },
        );

        Ok(record)
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
        if !env.storage().persistent().has(&DataKey::Admin) {
            return Err(Error::NotInitialized);
        }

        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let record: TaxWithholdingRecord = env
            .storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(milestone_index))
            .ok_or(Error::InvalidStatus)?;

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
        {
            return Err(Error::InvalidStatus);
        }
        if record.net_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::store_milestone_released(&env, milestone_index);

        // Remove the lock entry.
        env.storage()
            .persistent()
            .remove(&DataKey::TaxWithholdingLock(milestone_index));

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &record.net_amount,
        );

        env.events().publish(
            (symbol_short!("adtxrls"),),
            AdminOverrideTaxReleaseEvent {
                admin,
                contract_id: env.current_contract_address(),
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
    /// the gross amount to the client.
    ///
    /// Reads the `TaxWithholdingRecord`, transfers `gross_amount` to the client,
    /// marks the milestone `Refunded`, and removes the lock entry.
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
    /// * `InvalidAmount`    – Gross amount is zero.
    pub fn admin_override_tax_refund(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let record: TaxWithholdingRecord = env
            .storage()
            .persistent()
            .get(&DataKey::TaxWithholdingLock(milestone_index))
            .ok_or(Error::InvalidStatus)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
        {
            return Err(Error::InvalidStatus);
        }
        if record.gross_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Remove the lock entry.
        env.storage()
            .persistent()
            .remove(&DataKey::TaxWithholdingLock(milestone_index));

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.client,
            &record.gross_amount,
        );

        env.events().publish(
            (symbol_short!("adtxrfd"),),
            AdminOverrideTaxRefundEvent {
                admin,
                contract_id: env.current_contract_address(),
                milestone_index,
                client: meta.client,
                token: meta.token,
                gross_amount: record.gross_amount,
            },
        );

        Ok(())
    }

    // ── read-only query ───────────────────────────────────────────────────────

    /// Return a snapshot of the current yield and pause state.    ///
    /// All fields are safe to call even before any admin has set a yield rate
    /// (defaults to zero) or paused the contract (defaults to `false`).
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
            .get(&DataKey::YieldRateBps)
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
    /// Distinct from `tax_withholding_deductions`, which records a
    /// `TaxWithholdingRecord` per milestone for the normal approval flow. This
    /// admin-gated variant computes and returns the split directly and emits
    /// `TaxWithholdingDeductionsEvent`. Both arrived from separate PRs under
    /// the same name; this one carries the `admin_` prefix used by the other
    /// admin-gated endpoints.
    ///
    /// This function sets a lock (`TaxWithholdingExecutionLock`) while executing to
    /// prevent concurrent state mutations.  The lock is automatically cleared
    /// when the calculation completes, ensuring that normal escrow operations
    /// remain blocked only for the duration of the tax calculation.
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
    /// * `InvalidAmount`    – Milestone amount is ≤ 0 or arithmetic overflow.
    pub fn admin_tax_withholding_deductions(
        env: Env,
        admin: Address,
        milestone_index: u32,
        tax_rate_bps: u32,
    ) -> Result<(i128, i128, i128), Error> {
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Acquire lock before any state reads to prevent concurrent mutations.
        env.storage()
            .instance()
            .set(&DataKey::TaxWithholdingExecutionLock, &true);

        // Ensure lock is released even if the function returns early due to error.
        // We use a defer-like pattern by clearing the lock before returning.
        let result = (|| {
            if milestone_index >= meta.milestone_count {
                return Err(Error::InvalidMilestone);
            }

            let milestone = Self::load_milestone(&env, milestone_index)?;

            if milestone.amount <= 0 {
                return Err(Error::InvalidAmount);
            }

            if tax_rate_bps > 10_000 {
                return Err(Error::InvalidRatio);
            }

            let gross_amount = milestone.amount;
            let tax_amount = (gross_amount * (tax_rate_bps as i128)) / (BPS_SCALE as i128);
            let net_amount = gross_amount - tax_amount;

            if net_amount < 0 {
                return Err(Error::InvalidAmount);
            }

            Ok((gross_amount, tax_amount, net_amount))
        })();

        // Release lock regardless of success or failure.  Remove the key so a
        // stale `false` entry does not remain on the ledger.
        env.storage()
            .instance()
            .remove(&DataKey::TaxWithholdingExecutionLock);

        let (gross_amount, tax_amount, net_amount) = result?;

        // Emit structured event for indexers.
        env.events().publish(
            (symbol_short!("taxwh"),),
            TaxWithholdingDeductionsEvent {
                admin,
                contract_id: env.current_contract_address(),
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
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0.
    pub fn multisig_admin_override_release(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        // Terminal states have already settled funds — no double-spend.
        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
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

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::store_milestone_released(&env, milestone_index);

        // Clear the multisig lock flag now that the deadlock is resolved.
        env.storage()
            .instance()
            .set(&DataKey::MultisigLocked, &false);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );

        env.events().publish(
            (symbol_short!("msadmrel"),),
            MultisigAdminOverrideReleaseEvent {
                admin,
                contract_id: env.current_contract_address(),
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
    /// 6. Remaining balance must be > 0 (`InvalidAmount`).
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
        Self::require_admin(&env, &admin)?;

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
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
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

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Clear the multisig lock flag now that the deadlock is resolved.
        env.storage()
            .instance()
            .set(&DataKey::MultisigLocked, &false);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &meta.client, &remaining);

        env.events().publish(
            (symbol_short!("msadmref"),),
            MultisigAdminOverrideRefundEvent {
                admin,
                contract_id: env.current_contract_address(),
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
        Self::require_admin(&env, &admin)?;

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
    /// # Parameters
    /// * `env`                  – Soroban environment (used only for event emission).
    /// * `total_amount`         – Total amount to split.
    /// * `client_refund_bps`    – Client's refund share in basis points.
    /// * `freelancer_payout_bps`– Freelancer's payout share in basis points.
    ///
    /// # Returns
    /// A `RefundAllocation` struct with computed amounts and the basis-point
    /// ratios that were used.
    ///
    /// # Errors
    /// * `InvalidRatio` – Ratios do not sum to `BPS_SCALE`.
    /// * `InvalidAmount`– `total_amount` ≤ 0 or arithmetic overflow.
    pub fn emergency_pause_split_refund(
        env: Env,
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

    /// Admin-gated split-refund claim that may only run **while the contract
    /// is actually frozen**.
    ///
    /// `emergency_pause_split_refund` is an unauthenticated calculator that
    /// answers "what would this split be?" at any time.  This endpoint is the
    /// operational counterpart: it enforces the business rules that must hold
    /// before an emergency refund is settled, rejecting each bad setup with a
    /// distinct error variant before any arithmetic runs.
    ///
    /// # Business rules
    /// 1. Contract initialised and caller is the stored admin
    ///    (`NotInitialized` / `Unauthorized`).
    /// 2. No pause transition mid-execution (`EmergencyPauseInProgress`) — a
    ///    refund must not be computed against a half-applied freeze.
    /// 3. The contract **is** paused (`NotPaused`).  Settling an emergency
    ///    refund on a running escrow would bypass the normal release and
    ///    dispute paths.
    /// 4. `total_amount` > 0 (`InvalidAmount`) and the two shares sum to
    ///    exactly `BPS_SCALE` (`InvalidRatio`).
    ///
    /// # Returns
    /// A `RefundAllocation` whose two amounts sum to `total_amount` exactly.
    ///
    /// # Errors
    /// * `NotInitialized`           – Admin key has never been stored.
    /// * `Unauthorized`             – `admin` is not the stored admin.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    /// * `NotPaused`                – The contract is not frozen.
    /// * `InvalidAmount`            – `total_amount` ≤ 0, or overflow.
    /// * `InvalidRatio`             – Shares do not sum to 10 000 bps.
    pub fn emergency_pause_claim_refund(
        env: Env,
        admin: Address,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        Self::require_admin(&env, &admin)?;
        Self::assert_emergency_pause_not_locked(&env)?;

        if !Self::is_emergency_paused(env.clone()) {
            return Err(Error::NotPaused);
        }

        Self::emergency_pause_split_refund(
            env,
            total_amount,
            client_refund_bps,
            freelancer_payout_bps,
        )
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
    /// baseᵢ     = weightedᵢ / Σweights      (floor)
    /// remᵢ      = weightedᵢ % Σweights      (exact fractional part, scaled)
    /// residue   = total − Σbaseᵢ            (0 ≤ residue < n)
    /// ```
    /// The `residue` indivisible units are then handed out one at a time to
    /// the parties with the largest `remᵢ`, each party receiving at most one.
    /// This is exact rather than approximate: `remᵢ` is the true numerator of
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
    /// # Parameters
    /// * `total_amount` – Amount to divide; must be > 0.
    /// * `weights`      – Per-party weights.  Need not sum to any particular
    ///   scale; only their ratios matter.  Must be non-empty, at most
    ///   `MAX_EMERGENCY_ALLOCATION_PARTIES` long, non-negative, and sum to > 0.
    ///
    /// # Returns
    /// A `Vec<i128>` of per-party amounts, index-aligned with `weights`.
    ///
    /// # Errors
    /// * `InvalidAmount`             – `total_amount` ≤ 0, or arithmetic overflow.
    /// * `InvalidAllocationWeights`  – `weights` empty, over the cap, negative,
    ///   or summing to zero.
    pub fn emergency_pause_allocation(
        env: Env,
        total_amount: i128,
        weights: Vec<i128>,
    ) -> Result<Vec<i128>, Error> {
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
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    pub fn multisig_lock(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::MultisigLocked, &true);
        Ok(())
    }

    /// Check whether the multisig workflow is currently locked.
    ///
    /// Returns `true` if the `MultisigLocked` flag is set, meaning normal
    /// multisig operations are blocked until an admin override resolves the
    /// deadlock.
    pub fn is_multisig_locked(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::MultisigLocked)
            .unwrap_or(false)
    }
}
