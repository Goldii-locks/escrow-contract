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
    /// Configure the address that receives withheld tax.
    ///
    /// Must be in place **before** `tax_withholding_deductions` records a
    /// withholding — that endpoint refuses without a recipient
    /// (`InvalidStatus`), so a `tax_amount` can never be created that has no
    /// destination to be settled to.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract not initialised.
    /// * `Unauthorized`    – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidAddress`  – `recipient` is a zero address.
    pub fn set_tax_recipient(env: Env, admin: Address, recipient: Address) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        Self::validate_address(&env, &recipient)?;

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
        _env: Env,
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
    /// * `Unauthorized`     – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
    /// * `Unauthorized`     – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
    /// * `Unauthorized`     – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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

    /// Calculate tax withholding deductions for a milestone payout (admin).
    ///
    /// Admin-gated, read-only counterpart of `tax_withholding_deductions`: it
    /// runs the same shared calculation (`compute_tax_withholding` — remaining
    /// balance, round-to-nearest, same status checks) so both entry points
    /// report identical numbers, but it does not persist a
    /// `TaxWithholdingRecord` and emits `TaxWithholdingDeductionsEvent`.
    ///
    /// # Parameters
    /// * `admin`           – Must hold the `FeeManager` role (root admin by default).
    /// * `milestone_index` – Target milestone for tax calculation.
    /// * `tax_rate_bps`    – Tax rate in basis points (1 bp = 0.01 %).
    ///                       Must be ≤ 10 000 (≤ 100 %).
    ///
    /// # Returns
    /// `(gross_amount, tax_amount, net_amount)` reflecting the split.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract has not been initialised.
    /// * `Unauthorized`     – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
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
        // Authorization: only the `FeeManager` delegate (or, if none is set,
        // the stored admin) may invoke this endpoint.  Any other caller is
        // rejected with `Unauthorized`, and a contract that has never been
        // initialised is rejected with `NotInitialized`, before any ledger
        // entry is read or written.
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

#[allow(deprecated)]
impl MilestoneEscrow {
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
    pub(crate) fn tax_settlement_recipient(env: &Env, milestone_index: u32) -> Result<Address, Error> {
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
    pub(crate) fn publish_tax_paid(
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
    pub(crate) fn compute_tax_withholding(
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
    pub(crate) fn allocate_withholding_refund(
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

}
