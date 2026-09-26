#![no_std]
// Reentrancy guards throughout this contract use the `let result = (|| { … })();`
// idiom: set the lock, run the body, release the lock, then return the body's
// result. clippy::redundant_closure_call suggests flattening these to a plain
// block, which is NOT equivalent -- inside a block, `?` and `return` exit the
// enclosing function and skip the lock release below, leaving the escrow
// permanently locked. The closure boundary is what makes the release
// unconditional, so the lint is disabled here deliberately.
#![allow(clippy::redundant_closure_call)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, BytesN,
    ContractExecutable, Env, Symbol, Vec,
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
}

const MAX_TREASURY_FEE_BPS: u32 = 2000;
const MAX_CLIENT_FEE_BPS: u32 = 5000;

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

/// Result of a split-refund fee distribution, detailing the net amounts and fee shares.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitRefundFeeDistribution {
    pub client_net_refund: i128,
    pub client_fee_share: i128,
    pub freelancer_net_payout: i128,
    pub treasury_fee_share: i128,
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
    /// Temporary key: cumulative extension seconds applied to a Delivered
    /// milestone. Written by `extend_milestone_deadline`, read by
    /// `claim_auto_release` and `time_until_auto_release`. Uses temporary
    /// storage because the extension is deadline-scoped workflow state whose
    /// ledger footprint cost should not persist beyond the auto-release window.
    ///
    /// Short key `TimeExt` (7 chars vs 22) to minimise on-ledger symbol bytes.
    TimeExt(u32),
    /// Instance key for the cancel_escrow lock.
    CancelLock,
    /// Persistent: bitmask recording which parties have approved a pending
    /// cancel-escrow request.  Bit 0 (value 1) = client has approved;
    /// bit 1 (value 2) = freelancer has approved.  Written by `cancel_escrow`
    /// when a party signals intent, cleared when both bits are set (lock fires)
    /// or when `revoke_cancel_approval` removes a party's bit.
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
    pub extra_seconds: u32,
    pub new_extension: u32,
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
    /// The amount of the milestone being disputed, in stroops.
    pub milestone_amount: i128,
    /// The resulting milestone status after raising the dispute (always `Disputed`).
    pub new_status: MilestoneStatus,
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
    /// Address of this escrow contract.
    pub contract_id: Address,
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
    /// Basis points of the disputed balance awarded to the client. Matches the
    /// value persisted under `ArbitrationSplitBps(milestone_index)`.
    pub client_refund_bps: u32,
    /// Basis points awarded to the freelancer (`10_000 - client_refund_bps`).
    pub freelancer_payout_bps: u32,
    /// Cumulative amount released on the milestone after this split.
    pub released_amount: i128,
    /// Terminal milestone status after the split (`Refunded` or `Released`).
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
    pub contract_id: Address,
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

/// Emitted by `cancel_escrow` when a party records their cancellation approval
/// but the counter-party has not yet approved (single-signature stage).
/// Lets indexers track partial cancel intent without treating it as a finalized
/// cancel.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelApprovalRecordedEvent {
    pub contract_id: Address,
    pub caller: Address,
    /// Current bitmask after recording this approval.
    /// bit 0 = client approved, bit 1 = freelancer approved.
    pub approval_mask: u32,
}

/// Emitted by `revoke_cancel_approval` when a party withdraws their
/// cancellation approval before the lock fires.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelApprovalRevokedEvent {
    pub contract_id: Address,
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

/// Emitted by `emergency_pause` when the escrow is paused by the client and freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPausedEvent {
    pub client: Address,
    pub freelancer: Address,
    pub contract_id: Address,
}

/// Emitted by `emergency_unpause` when the admin unpauses the escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyUnpausedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `emergency_pause_claim_refund` on its success path only.
///
/// Published under the topics
/// `(Symbol("emergency_pause_claim_refund"), admin)` so indexers can track
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
    pub contract_id: Address,
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
    pub contract_id: Address,
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

/// Emitted by `admin_override_tax_split_refund` when the admin settles a
/// tax-locked milestone with a split refund. `client_refund` and
/// `freelancer_payout` are the amounts actually transferred (each net of its
/// share of `tax_amount`), so the two legs always sum to
/// `gross_amount − tax_amount` and the milestone's tax lock has been cleared.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideTaxSplitRefundEvent {
    pub admin: Address,
    pub contract_id: Address,
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

/// Emitted by `time_extensions_consent` once both the client's
/// and the freelancer's signatures have been collected and the time-extension
/// split has been computed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeExtConsentEvent {
    pub client: Address,
    pub freelancer: Address,
    pub amount: i128,
    pub elapsed_seconds: i128,
    pub total_seconds: i128,
    pub freelancer_share: i128,
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

/// Emitted by `upgrade` recording the outcome of a contract WASM upgrade.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractUpgradedEvent {
    pub admin: Address,
    pub new_wasm_hash: BytesN<32>,
    pub version: u32,
}

pub type UpgradeEvent = ContractUpgradedEvent;

#[contract]
pub struct MilestoneEscrow;

// Deferred pending coordinated migration to #[contractevent] — see
// escrow-backend's poller.ts, which reads the current event wire format.
#[allow(deprecated)]
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

    /// Reject the call before any other ledger access if the contract has not
    /// been initialised. `initialize` is the only path that sets
    /// `DataKey::Version`, so its presence is the initialisation marker.
    fn require_initialized(env: &Env) -> Result<(), Error> {
        if !env.storage().instance().has(&DataKey::Version) {
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
        if Self::read_emergency_paused(env) {
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

    fn load_interest_yield_state(env: &Env) -> Result<EscrowInterestYieldState, Error> {
        env.storage()
            .instance()
            .get(&DataKey::InterestYieldState)
            .ok_or(Error::NotInitialized)
    }

    /// Read `DataKey::PlatformFeeAllocation` from instance storage.
    ///
    /// This is the single source of truth for the platform-fee allocation read
    /// path.  Every function that needs the stored value — including the public
    /// `get_platform_fee_allocation`, `calculate_platform_fee_split`, and the
    /// write-path helpers — delegates here so the storage key is referenced in
    /// exactly one place and each invocation issues exactly one ledger read.
    fn load_platform_fee_allocation(env: &Env) -> Result<PlatformFeeAllocation, Error> {
        env.storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)
    }

    fn store_interest_yield_state(env: &Env, state: &EscrowInterestYieldState) {
        env.storage()
            .instance()
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
    pub fn split_refund_net_distribution(
        env: Env,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
        fee_allocation: PlatformFeeAllocation,
    ) -> Result<SplitRefundFeeDistribution, Error> {
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
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
            env.storage().instance().get(&DataKey::InterestYieldState);
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
    /// * `milestone_amounts`    – Ordered list of token amounts owed per
    ///                           milestone.  Must be non-empty; every
    ///                           individual amount must be strictly positive
    ///                           (`> 0`).
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
    /// * `DataKey::Admin` (instance **and** persistent storage) – the `admin`
    ///   address.
    /// * `DataKey::Version` (instance storage) – version marker `1u32`.
    /// * `DataKey::Ep` (instance storage) – emergency-pause flag set to
    ///   `false`.
    /// * `DataKey::PlatformFeeAllocation` (instance storage) – fee allocation
    ///   defaulting to 100 % freelancer / 0 % client / 0 % treasury.
    /// * `DataKey::WhitelistedTokens` (instance storage) – whitelist
    ///   initialized with `token` as its sole entry.
    ///
    /// An `"init"` event carrying an [`InitializedEvent`] is published after
    /// all state is written.  The event includes all party addresses,
    /// `auto_release_seconds`, the full `milestone_amounts` vec, the computed
    /// `total_amount`, and `milestone_count`.
    ///
    /// On any error path the Soroban host rolls back all storage writes made
    /// during this invocation, including the reentrancy sentinel written at
    /// entry, so the contract remains in its uninitialized state and a
    /// subsequent valid call will succeed.
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
    /// * [`Error::InvalidAmount`] – Any of the following conditions:
    ///   - `milestone_amounts` is empty (no milestones to escrow).
    ///   - Any individual milestone amount is `≤ 0` (zero or negative amounts
    ///     are not valid escrow values).
    ///   - The sum of all milestone amounts overflows `i128`.
    ///   - `auto_release_seconds` is `0` (zero disables the auto-release
    ///     window entirely, which is not a valid configuration).
    ///     Note: the `auto_release_seconds` check runs *after* the milestone
    ///     amount validation in the current implementation.
    ///
    /// * [`Error::InvalidMilestone`] – An element of `milestone_amounts` could
    ///   not be retrieved by index during the milestone-storage loop.  In
    ///   practice this indicates an internal SDK-level error rather than a
    ///   caller mistake.
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

        // milestone_count is u32 (Soroban Vec::len() returns u32) so no cast
        // is needed and there is no overflow risk on the count itself.
        let milestone_count = milestone_amounts.len();
        // All per-amount and running-total arithmetic is performed inside
        // checked_initialize_total via i128::checked_add (see its rustdoc).
        // Non-positive amounts and sum overflow both return Err(InvalidAmount).
        let total_amount = Self::checked_initialize_total(&milestone_amounts)?;

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Ep, &false);
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
    /// # Checks (in order)
    /// 1. `current_admin.require_auth()` — SDK-level signature check.
    /// 2. Contract must be initialised (`NotInitialized`).
    /// 3. `require_admin` — verified admin key matches `DataKey::Admin`.
    /// 4. `new_admin` must not be a zero address (`InvalidAddress`).
    /// 5. No multisig admin-transfer proposal may already be pending
    ///    (`AdminTransferPending`).
    ///
    /// # Parameters
    /// * `current_admin` – Must match the currently stored admin. Must
    ///                     authorize the call.
    /// * `new_admin`     – Address to become the new admin.
    ///
    /// # Errors
    /// * `NotInitialized`       – Contract has not been initialized.
    /// * `Unauthorized`         – `current_admin` is not the stored admin.
    /// * `InvalidAddress`       – `new_admin` is a zero address.
    /// * `AdminTransferPending` – A multisig admin-transfer proposal is already
    ///                            pending; execute or cancel it first.
    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), Error> {
        // Auth + init guards first — a single require_auth so the host does not
        // abort on a duplicated auth requirement.
        current_admin.require_auth();
        // Footprint: a single read of DataKey::Admin serves both the
        // initialization guard and the authorization check. Previously this
        // touched the entry twice — `has(&DataKey::Admin)` followed by
        // `load_admin()` (which also reads it). `load_admin` already returns
        // `NotInitialized` when the key is absent, so dropping the redundant
        // `has` keeps behavior identical (absent → NotInitialized, mismatch →
        // Unauthorized) while reading the entry only once.
        let stored_admin = Self::load_admin(&env)?;
        if stored_admin != current_admin {
            return Err(Error::Unauthorized);
        }

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
    /// * [`Error::InvalidAmount`] – The whitelist already contains
    ///   `MAX_WHITELIST_SIZE` (50) entries. Adding another would exceed the
    ///   capacity cap. This guards against unbounded `Vec` growth and `u32`
    ///   length-counter overflow.
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
    /// * `InvalidAmount`       - Removing the token would leave the whitelist empty (the contract requires at least one token to remain).
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

    /// Return the list of whitelisted token addresses.
    ///
    /// Strictly read-only: performs a single `get` on instance storage and no
    /// writes to instance, persistent, or temporary storage. Keep it that way —
    /// the storage handle is only ever used through [`Self::read_whitelist`].
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
        env.storage().instance().get(&DataKey::WhitelistedTokens)
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
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_payment_streaming_not_locked(&env)?;
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
        token_client.transfer(&client, env.current_contract_address(), &total_amount);

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
        extra_seconds: u32,
    ) -> Result<(), Error> {
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
        env.storage()
            .temporary()
            .set(&DataKey::TimeExt(milestone_index), &new_extension);

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

        // CHECK 1: Validate index boundary.
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        // CHECK 2: Milestone must be in the Delivered state.  Any other status —
        // including Released (double-claim), Disputed, Refunded, Pending, or
        // PartiallyReleased — is rejected here, making the guard the sole
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
            .and_then(|d| d.checked_add(extension as u64))
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

    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> Result<i64, Error> {
        let meta = Self::load_job_meta(&env).unwrap();
        let milestone = Self::load_milestone(&env, milestone_index).unwrap();
        // Read delivery timestamp from temporary storage (optimised path) and
        // fall back to the persistent Milestone field for pre-migration entries.
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let extension = Self::load_time_extension(&env, milestone_index);
        let deadline = delivered_at
            .checked_add(meta.auto_release_seconds)
            .and_then(|d| d.checked_add(extension as u64))
            .ok_or(Error::InvalidAmount)?;
        let current = env.ledger().timestamp();
        let deadline_i64 = i64::try_from(deadline).map_err(|_| Error::InvalidAmount)?;
        let current_i64 = i64::try_from(current).map_err(|_| Error::InvalidAmount)?;
        deadline_i64
            .checked_sub(current_i64)
            .ok_or(Error::InvalidAmount)
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

        let result = Self::raise_dispute_inner(&env, caller, milestone_index, meta);

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
        meta: JobMeta,
    ) -> Result<(), Error> {
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
        Self::store_dispute_flag(env, milestone_index);

        env.events().publish(
            (symbol_short!("dispute"),),
            DisputeRaisedEvent {
                milestone_index,
                caller,
                milestone_amount: milestone.amount,
                new_status: MilestoneStatus::Disputed,
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

        // Dedicated structured record of this arbitration split: the acting
        // arbiter plus every value persisted by the call (transferred amounts,
        // the stored client_refund_bps, cumulative release, terminal status).
        env.events().publish(
            (symbol_short!("arbsplit"),),
            ArbitrationSplitAppliedEvent {
                contract_id: env.current_contract_address(),
                arbiter: meta.arbiter.clone(),
                milestone_index,
                client: meta.client.clone(),
                freelancer: meta.freelancer.clone(),
                token: meta.token.clone(),
                client_refund,
                freelancer_payout,
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
    /// persistent `CancelApproval` bitmask and emits `CancelApprovalRecordedEvent`.
    /// The `CancelLock` is **not** set on a single-signature call — both parties
    /// must call before the escrow is frozen.  This prevents either party from
    /// unilaterally locking the other out.
    ///
    /// Once the second party calls, both bits in the mask are set, the
    /// `CancelLock` is activated, the approval record is cleared, and the
    /// `CancelEscrowInitiatedEvent` is emitted.
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
    /// * `InvalidStatus`  – `caller` has already recorded their approval in this
    ///                      round (duplicate single-party call).
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

        let bit = if caller == meta.client { 1u32 } else { 2u32 };
        let current_mask = env
            .storage()
            .instance()
            .get::<_, u32>(&DataKey::CancelApproval)
            .unwrap_or(0);
        if current_mask & bit != 0 {
            return Err(Error::InvalidStatus);
        }

        let new_mask = current_mask | bit;
        if new_mask == 3 {
            env.storage().instance().remove(&DataKey::CancelApproval);
            env.storage().instance().set(&DataKey::CancelLock, &true);
            env.events().publish(
                (symbol_short!("cancel"),),
                CancelEscrowInitiatedEvent {
                    contract_id: env.current_contract_address(),
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

        env.storage()
            .instance()
            .set(&DataKey::CancelApproval, &new_mask);
        env.events().publish(
            (symbol_short!("cxlappr"),),
            CancelApprovalRecordedEvent {
                contract_id: env.current_contract_address(),
                caller,
                approval_mask: new_mask,
            },
        );

        Ok(())
    }

    /// Revoke a previously recorded cancellation approval before the final
    /// second-party lock is set.
    ///
    /// # Storage footprint
    /// The approval record is a compact `u32` bitmask under the instance key
    /// `DataKey::CancelApproval`.  A stored mask only ever holds a single
    /// party's bit (both bits together fire the lock and clear the record in
    /// `cancel_escrow`), so revoking always leaves the mask empty and the key
    /// is **removed** rather than rewritten as a zero mask.  After a
    /// successful revoke the instance storage is byte-identical to its state
    /// before the approval was first recorded.
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
        let current_mask = env
            .storage()
            .instance()
            .get::<_, u32>(&DataKey::CancelApproval)
            .unwrap_or(0);
        if current_mask & bit == 0 {
            return Err(Error::InvalidStatus);
        }

        let new_mask = current_mask & !bit;
        if new_mask == 0 {
            env.storage().instance().remove(&DataKey::CancelApproval);
        } else {
            env.storage()
                .instance()
                .set(&DataKey::CancelApproval, &new_mask);
        }

        env.events().publish(
            (symbol_short!("cancelrev"),),
            CancelApprovalRevokedEvent {
                contract_id: env.current_contract_address(),
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
    /// - The compact `C` lock is cleared so subsequent queries are unblocked.
    /// - `YieldAccrued` is reset to zero (matches the pattern used by
    ///   `admin_override_release` / `admin_override_refund`).
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract not initialised.
    /// * `Unauthorized`    – Caller is not the verified admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidStatus`   – the compact `C` lock is not active.
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
    /// 5. The compact `C` lock must be active (`InvalidStatus`).
    ///
    /// # Effects
    /// - Every milestone in a non-terminal status is moved to `Refunded` and
    ///   its remaining balance is summed.
    /// - The total is transferred from the contract to the client in a single
    ///   token call.
    /// - The compact `C` lock is cleared.
    /// - `YieldAccrued` is reset to zero.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract not initialised.
    /// * `Unauthorized`    – Caller is not the verified admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidStatus`   – the compact `C` lock is not active.
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
        env.storage().instance().remove(&DataKey::CancelLock);
        if env.storage().persistent().has(&DataKey::YieldAccrued) {
            env.storage().persistent().remove(&DataKey::YieldAccrued);
        }

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
        Self::require_admin(&env, &admin)?;
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

        let result = (|| {
            env.storage().instance().set(&DataKey::Ep, &true);
            Ok(())
        })();

        env.storage().instance().set(&DataKey::EpLk, &false);

        if result.is_ok() {
            env.events().publish(
                (symbol_short!("empause"),),
                EmergencyPausedEvent {
                    client,
                    freelancer,
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
    /// * `NotPaused`                 – The contract is not currently paused.
    /// * `EmergencyPauseInProgress`  – A pause transition is already running.
    pub fn emergency_unpause(env: Env, admin: Address) -> Result<(), Error> {
        // Auth + init guards first — a single require_auth so the host does not
        // abort on a duplicated auth requirement.
        admin.require_auth();
        if !env.storage().persistent().has(&DataKey::Admin) {
            return Err(Error::NotInitialized);
        }
        let stored_admin = Self::load_admin(&env)?;
        if stored_admin != admin {
            return Err(Error::Unauthorized);
        }

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

        env.events().publish(
            (symbol_short!("emunpause"),),
            EmergencyUnpausedEvent {
                admin: admin.clone(),
                contract_id: env.current_contract_address(),
            },
        );

        Ok(())
    }

    /// Override the emergency pause status.
    ///
    /// # Effects
    /// Sets the `DataKey::Ep` to the provided `paused` boolean. Emits an `EmergencyPauseAdminOverrideEvent`.
    ///
    /// # Returns
    /// * `Ok(())` on a successful override of the emergency pause state.
    ///
    /// # Errors
    /// * `NotInitialized` - Admin key has never been stored.
    /// * `Unauthorized` - Caller is not the verified admin.
    /// * `InvalidStatus` - `paused` matches the contract's current emergency pause state.
    pub fn emergency_pause_admin_override(
        env: Env,
        admin: Address,
        paused: bool,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        // Read the current state once; reject no-op transitions so the caller
        // can distinguish a successful override from a mistaken duplicate call.
        let current = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::Ep)
            .unwrap_or(false);

        if current == paused {
            return Err(Error::InvalidStatus);
        }

        // Single write — no external call, no EmergencyPauseLock needed.
        env.storage().instance().set(&DataKey::Ep, &paused);

        env.events().publish(
            (symbol_short!("emoverrid"),),
            EmergencyPauseAdminOverrideEvent {
                admin: admin.clone(),
                contract_id: env.current_contract_address(),
                paused,
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
    /// | Initialized, frozen by `emergency_pause` / `emergency_pause_admin_override(_, true)` | `Ok(true)` |
    /// | Initialized, released by `emergency_unpause` / `emergency_pause_admin_override(_, false)` | `Ok(false)` |
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
        // Both the Admin read and all subsequent instance reads/writes touch
        // instance storage only, so the whole function uses a single ledger entry.
        Self::require_admin_from_instance(&env, &admin)?;
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

            env.storage().instance().set(
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
        Self::require_admin(&env, &admin)?;
        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;

        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocationLock, &true);

        let result = (|| {
            let mut current: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;

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
                .instance()
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
        Self::require_admin(&env, &admin)?;

        let current: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;

        if !current.locked {
            return Err(Error::InvalidStatus);
        }

        Self::assert_platform_fee_allocation_not_locked(&env)?;
        Self::assert_emergency_pause_not_locked(&env)?;
        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;

        env.storage().instance().set(
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
                contract_id: env.current_contract_address(),
                client_bps,
                freelancer_bps,
                treasury_bps,
                locked: false,
            },
        );

        Ok(())
    }

    /// Return the current platform-fee allocation stored in instance storage.
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
    /// `DataKey::PlatformFeeAllocation` is absent from instance storage, which
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
    /// Every `i128` step performed by [`Self::allocate_platform_fee`] is a
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

    pub fn payment_streaming_milestones(
        env: Env,
        total_amount: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        // Acquire execution lock so concurrent state mutations observe the
        // in-progress status and bail rather than interleaving.
        env.storage()
            .instance()
            .set(&DataKey::PaymentStreamingExecutionLock, &true);

        let result = (|| {
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
        })();

        env.storage()
            .instance()
            .remove(&DataKey::PaymentStreamingExecutionLock);

        result
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
    /// # Checked arithmetic
    /// Every `i128` operation this endpoint performs is checked, so an overflow
    /// surfaces as a typed `Error::InvalidAmount` rather than a panic or a
    /// silent wrap:
    /// * `total_amount × numerator` is probed with `i128::checked_mul` in
    ///   [`Self::validate_streaming_ratio`] **before** the
    ///   `PaymentStreamingExecutionLock` is taken, so a rejected invocation
    ///   writes no ledger entry.
    /// * The same product, the `denominator / 2` rounding bias and the
    ///   `total − rounded` remainder are all `checked_mul` / `checked_add` /
    ///   `checked_sub` inside [`Self::split_round_nearest`].
    ///
    /// # Errors
    /// * `NotInitialized` – Job metadata missing, so no signers are known.
    /// * `InvalidAmount`  – `total_amount` ≤ 0, or `total_amount × numerator`
    ///   overflows `i128`.
    /// * `InvalidRatio`   – `denominator` ≤ 0, or `numerator` outside
    ///   `0..=denominator`.
    ///
    /// [`Self::validate_streaming_ratio`]: Self::validate_streaming_ratio
    /// [`Self::split_round_nearest`]: Self::split_round_nearest
    pub fn payment_streaming_consent(
        env: Env,
        total_amount: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        // Validate pure inputs before any storage access so invalid
        // parameters never touch the ledger (footprint reduction for
        // failure cases).
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if denominator <= 0 {
            return Err(Error::InvalidRatio);
        }
        if numerator < 0 || numerator > denominator {
            return Err(Error::InvalidRatio);
        }

        // Reject illegal source state before any further ledger access
        // (including the execution lock) so a mistaken call never
        // mutates storage. This also keeps the success-path footprint
        // to a single instance ledger entry (Job + lock).
        Self::assert_payment_streaming_not_locked(&env)?;
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Collect both signatures; returns NotInitialized before any
        // lock write if Job metadata is missing.
        let meta = Self::require_client_and_freelancer_consent(&env)?;

        // Validate every input — including the overflow domain of the scaled
        // product — before touching storage.  A rejected invocation therefore
        // takes no execution lock and leaves no partial write behind.
        Self::validate_streaming_ratio(total_amount, numerator, denominator)?;

        env.storage()
            .instance()
            .set(&DataKey::PaymentStreamingExecutionLock, &true);

        let result = (|| {
            // `validate_streaming_ratio` already proved the scaled product is
            // representable, and `split_round_nearest` re-checks every
            // operation, so the arithmetic below cannot overflow.
            let split = Self::split_round_nearest(total_amount, numerator, denominator)?;

            env.events().publish(
                (symbol_short!("p_strcns"),),
                PaymentStreamingConsentEvent {
                    client: meta.client.clone(),
                    freelancer: meta.freelancer.clone(),
                    total_amount,
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
        // Authorization: the caller must have signed this transaction. The
        // SDK aborts the call before any ledger access if the signature is
        // missing, so an unsigned attempt is rejected outright.
        env.current_contract_address().require_auth();

        // Precondition: the escrow must be initialized so the split is anchored
        // to a real job rather than a fresh or uninitialized instance. This is
        // the same guard used by every other mutating endpoint.
        Self::require_initialized(&env)?;

        // Authorization and precondition checks run BEFORE any ledger entry is
        // read or written. Both the client and the freelancer must have signed
        // the transaction, so a single-signature attempt can never reach the
        // split arithmetic and no state is mutated.
        let _meta = Self::require_client_and_freelancer_consent(&env)?;

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

    /// Compute a milestone time-extension split that requires **dual consent**:
    /// both the client and the freelancer must independently sign the
    /// transaction.
    ///
    /// The split is only computed after the caller has been authenticated and
    /// the escrow has been initialised; a transaction carrying only one of the
    /// two signatures never reaches the split arithmetic, so a single-signature
    /// attempt reverts the whole invocation and no state is mutated.
    ///
    /// A transaction carrying only one of the two signatures never reaches the
    /// split arithmetic: the missing `require_auth()` panics at the host level,
    /// so a single-signature attempt reverts the whole invocation.
    ///
    /// # Errors
    /// * `NotInitialized` – Job metadata missing, so no signers are known.
    /// * `InvalidAmount`  – `amount` ≤ 0, or arithmetic overflow.
    /// * `InvalidRatio`   – Invalid elapsed/total seconds.
    pub fn time_extensions_consent(
        env: Env,
        amount: i128,
        elapsed_seconds: i128,
        total_seconds: i128,
    ) -> Result<RatioSplit, Error> {
        // Validate pure inputs before any storage access so invalid
        // parameters never touch the ledger (footprint reduction for
        // failure cases and to ensure overflow is caught via checked
        // ops rather than panicking).
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if total_seconds <= 0 {
            return Err(Error::InvalidRatio);
        }
        if elapsed_seconds < 0 || elapsed_seconds > total_seconds {
            return Err(Error::InvalidRatio);
        }

        // Reject illegal source state before any further ledger access
        // (including the execution lock) so a mistaken call never
        // mutates storage.
        Self::assert_time_ext_not_locked(&env)?;
        Self::assert_not_paused(&env)?;
        let emergency_paused: bool = env.storage().instance().get(&DataKey::Ep).unwrap_or(false);
        if emergency_paused {
            return Err(Error::Paused);
        }

        // Collect both signatures; returns NotInitialized before any
        // lock write if Job metadata is missing.
        let meta = Self::require_client_and_freelancer_consent(&env)?;

        env.storage()
            .instance()
            .set(&DataKey::TimeExtExecutionLock, &true);

        let result = (|| {
            let split = Self::split_round_nearest(amount, elapsed_seconds, total_seconds)?;

            env.events().publish(
                (symbol_short!("m_extcns"),),
                TimeExtConsentEvent {
                    client: meta.client.clone(),
                    freelancer: meta.freelancer.clone(),
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
    ///
    /// # Checks (in order)
    /// Authorization and source-state guards run **before** any job or
    /// token ledger entry is read or written, so a rejected call cannot
    /// mutate storage:
    /// 1. `signer.require_auth()` — the transaction must be signed by
    ///    `signer` (`Unauthorized` if missing).
    /// 2. `signer` must be one of the registered multisig signers
    ///    (`Unauthorized`).
    /// 3. Contract token balance must be > 0 (`MultiSigEmptyBalance`).
    ///
    /// # Errors
    /// * `NotInitialized`       – `multisig_approval_init` has not been called.
    /// * `Unauthorized`         – `signer` did not sign, or is not a
    ///   registered signer.
    /// * `MultiSigEmptyBalance` – Contract token balance is ≤ 0.
    pub fn multisig_approve(
        env: Env,
        signer: Address,
        proposal_id: u32,
    ) -> Result<MultiSigApprovalState, Error> {
        signer.require_auth();

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

        // Reject callers who are not registered signers before touching any
        // job/token ledger entry (find the signer's index, O(n) but n ≤ 32).
        let signer_index = signers
            .iter()
            .position(|s| s == signer)
            .ok_or(Error::Unauthorized)?;

        // Boundary guard: an approval collected against an empty escrow has
        // no funds behind it, so block processing until the contract holds
        // a positive token balance.
        let meta = Self::load_job_meta(&env)?;
        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::MultiSigEmptyBalance);
        }

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
    ///
    /// # Returns
    /// A `MultiSigApprovalState` built from the instance `MultiSigThreshold`
    /// and the temporary `MultiSigApproval(proposal_id)` bitmap.  An unknown
    /// or expired proposal reads as an empty bitmap (`approvals == 0`).
    ///
    /// # Guarantees
    /// * **Read-only.**  Exactly two `get`s (instance threshold, temporary
    ///   bitmap), via `read_multisig_approval`.  No `set`, `remove`,
    ///   `extend_ttl`, or event publish in instance, persistent, or temporary
    ///   storage — in particular it never re-writes the bitmap or bumps its
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
    /// * `NotInitialized`          – Contract has never been initialised, so
    ///   there is no admin to transfer from.
    /// * `NoPendingAdminTransfer`  – No transfer is currently proposed.
    /// * `MultiSigThresholdNotMet` – Approvals collected so far are below the
    ///   required threshold.
    pub fn execute_admin_transfer(env: Env) -> Result<(), Error> {
        // Precondition guard: reject an uninitialised contract (no admin ever
        // stored, so nothing can be transferred) with `NotInitialized` before
        // any pending-transfer or approval storage is read or written.
        let old_admin = Self::load_admin(&env)?;

        let pending: PendingAdminTransfer = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .ok_or(Error::NoPendingAdminTransfer)?;

        let state = Self::is_multisig_approved(env.clone(), pending.proposal_id)?;
        if !state.approved {
            return Err(Error::MultiSigThresholdNotMet);
        }

        // A proposal that keeps the current admin does not need to rewrite the
        // Admin ledger entry. This preserves the event and clears the pending
        // proposal while avoiding a redundant storage write.
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
    /// * one `.persistent().get(…)` on `DataKey::Admin` (initialization
    ///   guard), and
    /// * one `.persistent().get(…)` on `DataKey::PendingAdminTransfer`.
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
        // Guard: reject calls on an uninitialized contract.  We check for the
        // admin key because it is the canonical "has initialize() been called?"
        // signal used by every other guarded endpoint (load_admin,
        // load_job_meta, cancel_admin_transfer_proposal, etc.).
        Self::load_admin(env)?;
        Ok(env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer))
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

    /// Return a full snapshot of the current job, including every
    /// milestone's status and amounts.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    pub fn get_job(env: Env) -> Result<Job, Error> {
        let meta = Self::load_job_meta(&env)?;
        Self::assemble_job(&env, &meta)
    }

    /// Return the reputation counter for an address.
    ///
    /// Read-only: performs no storage writes. Each storage key is touched at
    /// most once per call — one existence check on `DataKey::Admin` and one
    /// read of `DataKey::Reputation(address)`.
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
        let storage = env.storage().persistent();
        // `has` avoids deserializing the admin address we never use.
        if !storage.has(&DataKey::Admin) {
            return Err(Error::NotInitialized);
        }
        Ok(storage.get(&DataKey::Reputation(address)).unwrap_or(0))
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
        env: Env,
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
    /// 2. **Illegal source state** — `ensure_interest_yield_writable` rejects a
    ///    locked configuration with `EscrowLocked`.  This runs *before* the
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
    /// This function uses `require_admin_from_instance` rather than the
    /// standard `require_admin` helper so that the admin verification read
    /// (`DataKey::Admin`, instance) and all `InterestYieldState` reads/writes
    /// (`DataKey::InterestYieldState`, instance) touch the **same single
    /// ledger entry** (instance storage) instead of two (persistent + instance).
    /// The precondition guard also issues exactly one read of
    /// `InterestYieldState`, so an update costs two reads total rather than
    /// three.
    ///
    /// # Errors
    /// Listed in evaluation order; a call that violates more than one guard
    /// fails with the first match.
    /// * `NotInitialized` – Contract admin key is missing.
    /// * `Unauthorized`   – Caller is not the stored admin.
    /// * `EscrowLocked`   – Configuration is locked for execution.
    /// * `InvalidRatio`   – Shares do not sum to exactly 10_000 bps.
    pub fn set_escrow_interest_yield(
        env: Env,
        admin: Address,
        client_share_bps: u32,
        freelancer_share_bps: u32,
    ) -> Result<(), Error> {
        // Authorization: `admin.require_auth()` inside the helper makes the
        // host reject a missing signature before this body runs at all; the
        // equality check then rejects a validly-signed non-admin. Both the
        // Admin read and all InterestYieldState reads/writes are in instance
        // storage, so the whole function touches a single ledger entry.
        Self::require_admin_from_instance(&env, &admin)?;

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
    ///
    /// Once locked, `set_escrow_interest_yield`, `set_interest_yield_consent`,
    /// and `interest_yield_split_refund` are all rejected until an admin calls
    /// `unlock_escrow_interest_yield`.
    ///
    /// A `yldlock` / `EscrowInterestYieldLockedEvent` is published once the new
    /// state is durable so indexers and auditors get an immutable record of the
    /// lock without polling storage. The event mirrors the persisted state
    /// field-for-field: the two BPS values are the shares the lock froze and
    /// `locked` is `true`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract admin key or interest/yield state missing.
    /// * `Unauthorized` – `admin` does not match the stored admin.
    pub fn lock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let mut state = Self::load_interest_yield_state(&env)?;
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
    /// # Errors
    /// * `NotInitialized` - Contract admin key or interest/yield state missing.
    /// * `Unauthorized` - Caller is not the stored admin.
    /// * `InvalidStatus` - Interest/yield lock is not active (already unlocked).
    pub fn unlock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
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
        if env.storage().instance().has(&DataKey::InterestYieldState) {
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

#[cfg(test)]
mod admin_accrue_yield_footprint_tests;
#[cfg(test)]
mod admin_accrue_yield_tests;
#[cfg(test)]
mod admin_override_cancel_tests;
#[cfg(test)]
mod admin_override_streaming_release_tests;
#[cfg(test)]
mod admin_set_yield_rate_tests;
#[cfg(test)]
mod cancel_admin_transfer_tests;
#[cfg(test)]
mod cancel_escrow_split_refund_guards_tests;
#[cfg(test)]
mod get_pending_admin_transfer_tests;
#[cfg(test)]
mod interest_yield_consent_tests;
#[cfg(test)]
mod is_emergency_paused_not_initialized_tests;
mod multisig_lock_auth_tests;
#[cfg(test)]
mod payment_streaming_consent_arithmetic_tests;
#[cfg(test)]
mod reputation_tests;
mod set_escrow_interest_yield_event_tests;
#[cfg(test)]
mod set_platform_fee_allocation_auth_tests;
#[cfg(test)]
mod tax_withholding_split_refund_tests;
#[cfg(test)]
mod test;
#[cfg(test)]
mod test_emergency_pause;
#[cfg(test)]
mod test_payment_streaming_milestones;
#[cfg(test)]
mod unlock_escrow_interest_yield_event_tests;

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
        Self::require_admin(&env, &admin)?;
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
        // Authorize from instance storage so the admin read shares a ledger
        // entry with the `DataKey::Job` read below (see the rustdoc note).
        Self::require_admin_from_instance(&env, &admin)?;

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
    /// * `Paused`          – Contract is paused.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`.
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
        Self::require_admin(&env, &admin)?;

        // Reject illegal source state before any job/milestone ledger I/O.
        Self::assert_not_paused(&env)?;

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
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

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
    /// * `EmergencyPauseInProgress` – A pause/resume transition is already
    ///   in flight; the escrow cannot be re-paused mid-transition.
    pub fn admin_pause_escrow(env: Env, admin: Address) -> Result<(), Error> {
        // Authorization: only the stored admin may pause the escrow.  Any
        // other caller is rejected with `Unauthorized`, and a contract that
        // has never been initialised with `NotInitialized`, before any ledger
        // entry is read or written.
        Self::require_admin(&env, &admin)?;

        // Precondition: reject an illegal source state — an emergency
        // pause/resume transition already in flight — before any write.
        Self::assert_emergency_pause_not_locked(&env)?;

        // Idempotent no-op: when the escrow is already paused, return without
        // touching any storage, so a redundant call mutates nothing.
        let already_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if already_paused {
            return Ok(());
        }

        env.storage().instance().set(&DataKey::Paused, &true);

        env.events().publish(
            (symbol_short!("pause"),),
            EscrowPausedEvent {
                admin,
                contract_id: env.current_contract_address(),
            },
        );

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
        Self::require_admin_from_instance(&env, &admin)?;

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
                    contract_id: env.current_contract_address(),
                },
            );

            Ok(())
        })();

        env.storage().instance().set(&DataKey::EpLk, &false);

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
        Self::assert_tax_withholding_not_locked(&env)?;
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

        // Only Pending/Delivered/PartiallyReleased milestones may have tax
        // withheld — mirrors the state machine `raise_dispute_inner` and
        // `resolve_dispute` already enforce elsewhere in this file. A
        // Disputed milestone's funds are frozen pending arbitration, so
        // computing (and persisting) a tax split for it here would let this
        // entry point move money around a dispute the same way `resolve_dispute`
        // is meant to gate exclusively. An exhaustive match (rather than the
        // two equality checks this replaced) also means a future new
        // `MilestoneStatus` variant fails to compile here instead of silently
        // falling through as allowed.
        match milestone.status {
            MilestoneStatus::Pending
            | MilestoneStatus::Delivered
            | MilestoneStatus::PartiallyReleased => {}
            MilestoneStatus::Released | MilestoneStatus::Refunded | MilestoneStatus::Disputed => {
                return Err(Error::InvalidStatus)
            }
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
    /// counterpart of [`Self::allocate_withholding_refund`], which holds the
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

    /// Admin emergency override: resolve a tax-locked milestone by paying a
    /// **split** of the withheld balance to both parties, each net of their own
    /// share of the withheld tax.
    ///
    /// This is the settlement counterpart of
    /// [`Self::tax_withholding_split_refund`]: the calculator decides the
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

        let contract_addr = env.current_contract_address();
        let token_client = token::Client::new(&env, &meta.token);
        if allocation.client_refund > 0 {
            token_client.transfer(&contract_addr, &meta.client, &allocation.client_refund);
        }
        if allocation.freelancer_payout > 0 {
            token_client.transfer(
                &contract_addr,
                &meta.freelancer,
                &allocation.freelancer_payout,
            );
        }

        env.events().publish(
            (symbol_short!("adtwsplt"),),
            AdminOverrideTaxSplitRefundEvent {
                admin,
                contract_id: contract_addr,
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
    /// Distinct from `tax_withholding_deductions`, which records a
    /// `TaxWithholdingRecord` per milestone for the normal approval flow. This
    /// admin-gated variant computes and returns the split directly and emits
    /// `TaxWithholdingDeductionsEvent`. Both arrived from separate PRs under
    /// the same name; this one carries the `admin_` prefix used by the other
    /// admin-gated endpoints.
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
        // Authorization: only the stored admin may invoke this endpoint.  Any
        // caller that is not the stored admin is rejected with `Unauthorized`,
        // and a contract that has never been initialised is rejected with
        // `NotInitialized`, before any ledger entry is read or written.
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        // Precondition guards: reject illegal source states (out-of-range
        // milestone, non-positive milestone amount, tax rate above full scale,
        // empty contract balance) with their specific typed error before any
        // ledger entry is written.
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if tax_rate_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        let gross_amount = milestone.amount;
        let tax_amount = gross_amount
            .checked_mul(tax_rate_bps as i128)
            .ok_or(Error::InvalidAmount)?
            .checked_div(BPS_SCALE as i128)
            .ok_or(Error::InvalidAmount)?;
        let net_amount = gross_amount
            .checked_sub(tax_amount)
            .ok_or(Error::InvalidAmount)?;

        if net_amount < 0 {
            return Err(Error::InvalidAmount);
        }

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
        // Auth + init guards first — a single require_auth so the host does not
        // abort on a duplicated auth requirement.
        admin.require_auth();
        if !env.storage().persistent().has(&DataKey::Admin) {
            return Err(Error::NotInitialized);
        }
        let stored_admin = Self::load_admin(&env)?;
        if stored_admin != admin {
            return Err(Error::Unauthorized);
        }

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
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Clear the multisig lock flag now that the deadlock is resolved.
        env.storage().instance().remove(&DataKey::MultisigLocked);

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
    /// [`Self::cancel_escrow_claim_refund`], and moving funds outright is
    /// [`Self::admin_override_cancel_refund`].  The source-state guards above
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
    /// one *is* in flight.  That is the same split of responsibilities
    /// `emergency_pause_split_refund` / `emergency_pause_claim_refund` already
    /// use, and it matches `admin_override_cancel_refund`, which also requires
    /// the cancel lock to be active.
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
        Self::require_admin(&env, &admin)?;

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
    /// `emergency_pause_split_refund` is an unauthenticated calculator that
    /// answers "what would this split be?" at any time.  This endpoint is the
    /// operational counterpart: it enforces the business rules that must hold
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
    /// the topics `(Symbol("emergency_pause_claim_refund"), admin)`.
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
        Self::require_admin_from_instance(&env, &admin)?;
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
            (Symbol::new(&env, "emergency_pause_claim_refund"), admin),
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
    /// # Validation order
    /// The total and the weight vector are fully validated *before* the first
    /// division by `Σweights` is attempted, so a malformed vector can never
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
        // Authorization check at top - require auth and admin role first
        Self::require_admin_from_instance(&env, &admin)?;
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
    /// deadlock.
    pub fn is_multisig_locked(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::MultisigLocked)
            .unwrap_or(false)
    }
}
