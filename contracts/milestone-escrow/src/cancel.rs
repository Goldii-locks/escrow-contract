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
        Self::validate_address(&env, &caller)?;

        // Reject if the contract is paused.
        Self::assert_not_paused(&env)?;

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

        let counterparty = if caller == meta.client {
            meta.freelancer.clone()
        } else {
            meta.client.clone()
        };
        let new_mask = live_mask | bit;
        if new_mask == 3 {
            env.storage().instance().remove(&DataKey::CancelApproval);
            env.storage().instance().set(&DataKey::CancelLock, &true);
            env.events().publish(
                (symbol_short!("cancel"), caller.clone(), counterparty),
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
            (symbol_short!("cxlappr"), caller.clone(), counterparty),
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
        Self::validate_address(&env, &caller)?;

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

        let counterparty = if caller == meta.client {
            meta.freelancer.clone()
        } else {
            meta.client.clone()
        };
        env.events().publish(
            (symbol_short!("cancelrev"), caller.clone(), counterparty),
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
    /// * `Unauthorized`    – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
    /// * `Unauthorized`    – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
    ///   knowable.
    /// 2. `Paused` – the admin pause or the party emergency pause is in
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

        Ok(allocation)
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
    /// * `Unauthorized`   – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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

}

#[allow(deprecated)]
impl MilestoneEscrow {
    /// Shared body of `admin_override_cancel_release` (`to_freelancer = true`)
    /// and `admin_override_cancel_refund` (`to_freelancer = false`).
    ///
    /// Requires the admin and an active cancel lock, settles every
    /// non-terminal milestone's remaining balance as `Released` or `Refunded`,
    /// removes the `CancelLock` and `YieldAccrued` entries (so neither path
    /// leaves a stale entry paying rent), then pays the total to the freelancer
    /// or client. Negative `amount` / `released_amount` fields are rejected on
    /// both paths (issue #386). Returns the job metadata and the total paid.
    pub(crate) fn settle_all_remaining(
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
                // Only the persistent Milestone entry is written.
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

    /// Pure two-party split arithmetic shared by `cancel_escrow_split_refund`
    /// and its admin-gated claim counterpart, so the preview and the claim can
    /// never disagree about how a ratio is rounded.
    ///
    /// The client share is computed with round-nearest arithmetic and the
    /// freelancer receives the exact remainder, so the two legs always sum to
    /// `total_amount`.
    pub(crate) fn split_refund_allocation(
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

}
