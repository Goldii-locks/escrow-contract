use crate::types::{AdminRole, MilestoneStatus};
use soroban_sdk::{contracttype, Address, BytesN, Vec};

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

/// Emitted by `increase_milestone` when both parties consent to raising a
/// `Pending` milestone's amount (issue #1020).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneIncreasedEvent {
    pub client: Address,
    pub freelancer: Address,
    pub milestone_index: u32,
    pub old_amount: i128,
    pub new_amount: i128,
    /// Contract-level total after the increase.
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
    /// Commitment to the arbiter's written decision (#1015). The contract
    /// never sees the decision itself, only this 32-byte hash of it; the
    /// document lives in escrow-backend storage, keyed by this hash. Echoed
    /// from `resolve_dispute` / `apply_dispute_arbitration_split` so the
    /// on-chain resolution is cryptographically anchored to the reasoning
    /// that produced it. The `DisputeInfo` record is deleted when the
    /// dispute closes, so this event is where the commitment survives.
    pub decision_hash: BytesN<32>,
}

/// Immutable record of a successful `apply_dispute_arbitration_split` call.
///
/// Every field reconciles exactly with the state the call persisted:
/// `client_refund` / `freelancer_payout` are the amounts actually transferred
/// (capped to the contract balance), `client_refund_bps` is the BPS the arbiter
/// applied, `released_amount` is the
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
    /// Basis points of the disputed balance awarded to the client.
    pub client_refund_bps: u32,
    /// Basis points awarded to the freelancer (`10_000 - client_refund_bps`).
    pub freelancer_payout_bps: u32,
    /// Cumulative amount released on the milestone after this split.
    pub released_amount: i128,
    /// Terminal milestone status after the split.
    pub status: MilestoneStatus,
    /// Commitment to the arbiter's written decision (#1015). Same value the
    /// call echoes in the accompanying `resolve` event; the written decision
    /// itself stays off-chain in escrow-backend storage, keyed by this hash.
    pub decision_hash: BytesN<32>,
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

/// Emitted whenever a reputation score changes, so indexers can follow score
/// history without polling `get_reputation` for every address after a release.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReputationChangedEvent {
    /// Address whose score changed (client or freelancer).
    pub address: Address,
    /// Score stored after the increment.
    pub new_score: u32,
    /// Milestone whose full release caused the change.
    pub milestone_index: u32,
}

/// Emitted on every freelancer payout that charges a treasury fee (#674).
/// Published by `apply_treasury_update` when the fee treasury changes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreasuryUpdatedEvent {
    pub treasury: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeChargedEvent {
    pub recipient: Address,
    pub gross_amount: i128,
    pub fee_amount: i128,
    pub treasury: Address,
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

/// Emitted by `reclaim_overdue` when the client takes back a `Pending`
/// milestone whose delivery deadline (plus grace) has passed.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverdueReclaimedEvent {
    pub client: Address,
    pub milestone_index: u32,
    pub due_at: u64,
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
/// `paused` is the new value written to `DataKey::Paused` and `previous` is
/// whether any pause was in effect before. Emitted only on the success path.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPauseAdminOverrideEvent {
    /// Admin that authorised and applied the override (the acting address).
    pub admin: Address,
    /// Pause flag persisted by this call (`DataKey::Paused`).
    pub paused: bool,
    /// Pause flag that was in effect immediately before this call.
    pub previous: bool,
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

/// Emitted by `emergency_pause_allocation` with the exact per-party
/// amounts. `total_amount` always equals the sum of `allocations`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPauseAllocationEvent {
    pub total_amount: i128,
    pub num_parties: u32,
    pub allocations: Vec<i128>,
}

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
