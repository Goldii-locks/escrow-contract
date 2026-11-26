#[allow(unused_imports)]
use crate::{
    events::*,
    math,
    types::*,
    MilestoneEscrow,
};
#[allow(unused_imports)]
use soroban_sdk::{
    contractimpl, symbol_short, token, Address, BytesN, ContractExecutable, Env, IntoVal, Map,
    String, Symbol, TryFromVal, Val, Vec,
};

#[allow(deprecated)]
#[contractimpl]
impl MilestoneEscrow {
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
    /// * `ReviewWindowClosed` – The milestone is `Delivered` and its
    ///                        auto-release deadline has passed.
    pub fn raise_dispute(
        env: Env,
        caller: Address,
        milestone_index: u32,
        reason_hash: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;

        // ── Caller validation (before any storage write) ─────────────────
        // Reject zero addresses before touching ledger state so that no
        // storage entry is mutated on an invalid caller.
        Self::validate_address(&env, &caller)?;

        // require_dispute_party performs caller.require_auth() + verifies the
        // caller matches the stored client or freelancer in a single step.
        let meta = Self::require_dispute_party(&env, &caller)?;
        Self::raise_dispute_inner(&env, caller, milestone_index, reason_hash, meta)
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
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;

        // Validate the caller before any storage read/write so an unauthorized
        // address neither pays for the dispute lookup nor mutates state.
        Self::validate_address(&env, &caller)?;
        let meta = Self::require_dispute_party(&env, &caller)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
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
    /// Returns the raising party's `reason_hash`, who raised the dispute
    /// (`raised_by`), the ledger timestamp of the raise (`disputed_at`), the
    /// status to restore on escalation, and every evidence hash submitted so
    /// far. Callers use this to anchor arbitration
    /// context without relying on off-chain metadata.
    ///
    /// # Errors
    /// * [`Error::InvalidStatus`] – No open dispute exists for the milestone.
    pub fn get_dispute_info(env: Env, milestone_index: u32) -> Result<DisputeInfo, Error> {
        let key = DataKey::DisputeInfo(milestone_index);
        let persistent = env.storage().persistent();
        let fields: Map<Symbol, Val> = persistent.get(&key).ok_or(Error::InvalidStatus)?;
        if fields.contains_key(Symbol::new(&env, "raised_by")) {
            return persistent.get(&key).ok_or(Error::InvalidStatus);
        }
        // Schema-3 record read between `upgrade` and `migrate`: getters keep
        // working, with the same client placeholder `migrate` would write.
        let old: DisputeInfoV3 = persistent.get(&key).ok_or(Error::InvalidStatus)?;
        let meta = Self::load_job_meta(&env)?;
        Ok(DisputeInfo {
            disputed_at: old.disputed_at,
            previous_status: old.previous_status,
            reason_hash: old.reason_hash,
            evidence: old.evidence,
            raised_by: meta.client,
        })
    }

    /// Escalate an unresolved dispute after its configured timeout by
    /// restoring the milestone to the status it had before the dispute.
    ///
    /// Either escrow party may call this function. A restored Delivered
    /// milestone keeps its original auto-release deadline, so a dispute and
    /// escalation loop cannot extend the review window.
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

        Self::validate_address(&env, &caller)?;
        let meta = Self::require_dispute_party(&env, &caller)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
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
        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));

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
    /// * `decision_hash`          – 32-byte commitment to the arbiter's
    ///                              written decision (#1015). The document
    ///                              itself stays off-chain (escrow-backend
    ///                              storage, keyed by this hash); the contract
    ///                              only stores/echoes the hash in the `resolve`
    ///                              event. Not validated on-chain — any
    ///                              32 bytes are accepted, exactly like
    ///                              `raise_dispute`'s `reason_hash`.
    ///
    /// # Notes
    /// The `DisputeInfo` record is deleted by this call (existing behaviour),
    /// so the decision hash is anchored by the emitted `resolve` event rather
    /// than persisted in dispute storage.
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
        decision_hash: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::validate_address(&env, &arbiter)?;
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
                Self::pay_freelancer_with_fee(&env, &meta, &token_client, &payout)?;
            }
            Self::increment_reputation(&env, &meta.client, milestone_index);
            Self::increment_reputation(&env, &meta.freelancer, milestone_index);
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
                decision_hash,
            },
        );

        Ok(())
    }

    /// Pure refund-allocation algorithm for split-refund dispute claims.
    ///
    /// Split a disputed milestone amount between client and freelancer using
    /// the arbiter-specified `client_refund_bps`. This is the exact same
    /// allocation (same parameter, same floor rounding) used by
    /// `apply_dispute_arbitration_split`, so a UI can preview here and execute
    /// there without a one-stroop surprise.
    ///
    /// # Parameters
    /// * `total_amount`      – Total escrowed balance to distribute. Must be ≥ 0.
    /// * `client_refund_bps` – Basis points awarded to the client (0 – 10_000).
    ///                         The freelancer receives the remainder.
    ///
    /// # Returns
    /// A [`RefundAllocation`]. Rounding direction is documented once on
    /// [`Self::allocate_refund_by_bps`].
    ///
    /// # Errors
    /// * `InvalidAmount` – `total_amount` is negative, or an intermediate
    ///                     multiplication overflows `i128`.
    /// * `InvalidRatio`  – `client_refund_bps` exceeds 10_000.
    pub fn dispute_arbitration_split(
        _env: Env,
        total_amount: i128,
        client_refund_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        if total_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        if client_refund_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }
        Self::allocate_refund_by_bps(total_amount, client_refund_bps)
    }

    /// Apply a BPS split-refund to a disputed milestone and transfer funds.
    ///
    /// Client receives `client_refund_bps` of the remaining balance; freelancer
    /// receives the remainder. Milestones end `Refunded` or `Released` for a
    /// one-sided outcome and `Split` when both parties receive a payout.
    ///
    /// `decision_hash` is a 32-byte commitment to the arbiter's written
    /// decision (#1015). The document itself stays off-chain (escrow-backend
    /// storage, keyed by this hash); the contract only echoes the hash in the
    /// `resolve` and `arbsplit` events. It is not validated on-chain — any 32
    /// bytes are accepted, exactly like `raise_dispute`'s `reason_hash`. The
    /// `DisputeInfo` record is deleted by this call (existing behaviour), so
    /// the event is where the commitment survives.
    pub fn apply_dispute_arbitration_split(
        env: Env,
        arbiter: Address,
        milestone_index: u32,
        client_refund_bps: u32,
        decision_hash: BytesN<32>,
    ) -> Result<RefundAllocation, Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::validate_address(&env, &arbiter)?;
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
            Self::pay_freelancer_with_fee(&env, &meta, &token_client, &freelancer_payout)?;
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
            Self::increment_reputation(&env, &meta.client, milestone_index);
            Self::increment_reputation(&env, &meta.freelancer, milestone_index);
        }

        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

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
                decision_hash: decision_hash.clone(),
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
                decision_hash,
            },
        );

        Ok(resolved)
    }

}

#[allow(deprecated)]
impl MilestoneEscrow {
    /// Core dispute logic extracted so that the lock guard in
    /// `raise_dispute` wraps every path uniformly.  This function
    /// is never called directly — it exists only to keep the
    /// lock/release pairing in one place.
    ///
    /// `meta` is pre-validated by `raise_dispute` (zero-address check and
    /// `require_auth` already performed) so this function can proceed
    /// directly to business-logic checks.
    pub(crate) fn raise_dispute_inner(
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
        Self::assert_review_window_open(env, &meta, milestone_index, &milestone)?;

        let disputed_at = env.ledger().timestamp();
        env.storage().persistent().set(
            &DataKey::DisputeInfo(milestone_index),
            &DisputeInfo {
                disputed_at,
                previous_status: milestone.status.clone(),
                reason_hash: reason_hash.clone(),
                evidence: Vec::new(env),
                raised_by: caller.clone(),
            },
        );
        milestone.status = MilestoneStatus::Disputed;
        Self::store_milestone(env, milestone_index, &milestone);

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

    /// Allocate a disputed amount into client refund vs freelancer payout by BPS.
    ///
    /// This is the single allocation helper for arbitration splits. Rounding is
    /// defined once here: the **client leg is rounded down** (floor) and the
    /// **freelancer receives the exact remainder**, so the two legs always sum
    /// to `total_amount` (no value is lost to rounding). Both the read-only
    /// preview and the payout path call this helper, so they cannot disagree.
    pub(crate) fn allocate_refund_by_bps(
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

}
