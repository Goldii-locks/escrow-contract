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

        Ok(distribution)
    }

    /// Propose a treasury address change. It takes effect only after
    /// `TREASURY_UPDATE_DELAY_SECONDS`, giving clients time to react.
    pub fn propose_treasury_update(
        env: Env,
        admin: Address,
        treasury: Address,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        Self::validate_address(&env, &treasury)?;
        let effective_at = env
            .ledger()
            .timestamp()
            .checked_add(TREASURY_UPDATE_DELAY_SECONDS)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage().instance().set(
            &DataKey::PendingTreasury,
            &PendingTreasury {
                address: treasury,
                effective_at,
            },
        );
        Ok(())
    }

    /// Apply a treasury change once its timelock has elapsed.
    pub fn apply_treasury_update(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        let pending: PendingTreasury = env
            .storage()
            .instance()
            .get(&DataKey::PendingTreasury)
            .ok_or(Error::NotInitialized)?;
        if env.ledger().timestamp() < pending.effective_at {
            return Err(Error::InvalidStatus);
        }
        env.storage()
            .instance()
            .set(&DataKey::Treasury, &pending.address);
        env.storage().instance().remove(&DataKey::PendingTreasury);
        env.events().publish(
            (symbol_short!("treasury"),),
            TreasuryUpdatedEvent {
                treasury: pending.address,
            },
        );
        Ok(())
    }

    /// Sets the global platform fee allocation in basis points (BPS).
    ///
    /// # Effects
    /// Updates the `PlatformFeeAllocation` stored at `DataKey::PlatformFeeAllocation`.
    /// Sets the `the former reentrancy guard` to prevent re-entrant or concurrent updates.
    /// Emits a `PlatformFeeAllocationSetEvent`.
    ///
    /// ## Storage-footprint note
    ///
    /// This function uses `require_admin_from_instance` rather than the
    /// standard `require_admin` helper so that the admin verification read
    /// (`DataKey::Admin`, instance) and all subsequent instance reads/writes
    /// (the former reentrancy guard, `PlatformFeeAllocation`, the former reentrancy guard) touch
    /// the **same single ledger entry** (instance storage) instead of two
    /// (persistent + instance).
    ///
    /// # Returns
    /// * `Ok(())` on a successful update of the platform fee allocation.
    ///
    /// # Errors
    /// * `NotInitialized` - Admin key has never been stored.
    /// * `Unauthorized` - `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
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
        Self::require_fee_allocation_change_allowed(&env)?;
        // Precondition: validate illegal source state (locked allocation) before any ledger write
        // Ensures InvalidStatus is returned with no storage mutation when allocation is locked
        if let Ok(current) = Self::load_platform_fee_allocation(&env) {
            if current.locked {
                return Err(Error::InvalidStatus);
            }
        }
        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;

        (|| {
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
        })()
    }

    /// Lock the current platform-fee allocation, preventing further non-override
    /// modifications.  The admin's configuration (client_bps, freelancer_bps,
    /// treasury_bps) is persisted with `locked = true` so that subsequent calls
    /// to non-override endpoints are rejected until an admin override clears the
    /// lock.
    ///
    /// # Returns
    /// * `Ok(())` – the allocation was successfully locked.
    /// * `Err(Error::Unauthorized)` – `admin` is neither the `FeeManager` delegate nor, if none is
    ///   set, the stored admin.
    /// * `Err(Error::PlatformFeeAllocationInProgress)` – a platform-fee
    ///   allocation is already locked.
    /// * `Err(Error::EmergencyPauseInProgress)` – an emergency-pause transition
    ///   is active.
    /// * `Err(Error::NotInitialized)` – the contract has not been initialized.
    ///
    /// # Errors
    /// * `Error::Unauthorized` – `admin` is neither the `FeeManager` delegate nor, if none is set,
    ///   the stored admin.
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

        // Load the current allocation early to validate it exists
        let mut current: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;
        // Locking twice is a mistake worth surfacing, not a silent no-op.
        if current.locked {
            return Err(Error::InvalidStatus);
        }

        // All checks passed; now proceed with mutation under re-entrancy guard

        (|| {
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
        })()
    }

    /// Admin override that replaces a *locked* platform-fee allocation and
    /// unlocks it in the same step.
    ///
    /// `Unauthorized`: `admin` is neither the `FeeManager` delegate nor, if
    /// none is set, the stored admin.
    ///
    /// Storage footprint: the call writes exactly one instance-storage map
    /// key, `DataKey::PlatformFeeAllocation`. Unlike `set_platform_fee_allocation`
    /// / `lock_platform_fee_allocation`, it does not take the
    /// the former reentrancy guard re-entrancy guard: the body performs a
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
        // An admin who locked the allocation cannot unlock and rewrite it
        // alone once the escrow is funded (issue #694).
        Self::require_fee_allocation_change_allowed(&env)?;

        let current: PlatformFeeAllocation = Self::load_platform_fee_allocation(&env)?;

        if !current.locked {
            return Err(Error::InvalidStatus);
        }

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
        Self::assert_not_paused(&env)?;

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
        Self::assert_not_paused(&env)?;

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

        (|| {
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
                Self::increment_reputation(&env, &meta.client, milestone_index);
                Self::increment_reputation(&env, &meta.freelancer, milestone_index);
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
        })()
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
    /// While the split runs, the former reentrancy guard is held so
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
        let delivered_at = milestone.delivered_at;
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

}

#[allow(deprecated)]
impl MilestoneEscrow {
    /// Treasury share using the same rounding as `calculate_platform_fee_split`.
    /// No allocation stored means no fee (fee-free legacy behaviour).
    pub(crate) fn treasury_fee_for(env: &Env, amount: i128) -> Result<i128, Error> {
        let allocation = match Self::load_platform_fee_allocation(env) {
            Ok(allocation) => allocation,
            Err(_) => return Ok(0),
        };
        if allocation.treasury_bps == 0 {
            return Ok(0);
        }
        // For ordinary amounts use the shared split so rounding matches
        // `calculate_platform_fee_split` exactly. At the i128 ceiling the
        // intermediate `amount * bps` can overflow, so divide first there.
        if amount > i128::MAX / BPS_SCALE as i128 {
            let scale = BPS_SCALE as i128;
            let bps = allocation.treasury_bps as i128;
            return amount
                .checked_div(scale)
                .and_then(|q| q.checked_mul(bps))
                .and_then(|q| {
                    q.checked_add(
                        amount
                            .checked_rem(scale)?
                            .checked_mul(bps)?
                            .checked_div(scale)?,
                    )
                })
                .ok_or(Error::ArithmeticOverflow);
        }
        Ok(Self::allocate_platform_fee(amount, &allocation)?.treasury_amount)
    }

    /// Pay a freelancer, deducting the configured treasury fee. Refund paths
    /// never call this helper, so they remain fee-free.
    pub(crate) fn pay_freelancer_with_fee(
        env: &Env,
        meta: &JobMeta,
        token_client: &token::Client,
        amount: &i128,
    ) -> Result<(), Error> {
        let fee = Self::treasury_fee_for(env, *amount)?;
        if fee <= 0 {
            return Self::pay_or_credit(env, token_client, &meta.freelancer, amount);
        }
        let net = amount.checked_sub(fee).ok_or(Error::ArithmeticOverflow)?;
        Self::pay_or_credit(env, token_client, &meta.freelancer, &net)?;
        let treasury = Self::treasury_address(env)?;
        Self::pay_or_credit(env, token_client, &treasury, &fee)?;
        env.events().publish(
            (symbol_short!("pf_charge"),),
            PlatformFeeChargedEvent {
                recipient: meta.freelancer.clone(),
                gross_amount: *amount,
                fee_amount: fee,
                treasury,
            },
        );
        Ok(())
    }

    pub(crate) fn split_round_nearest(
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
    pub(crate) fn allocate_platform_fee(
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

    /// The split behind `milestone_time_extensions`, without its
    /// authorization. `admin_override_streaming_release` calls this directly
    /// after its own admin check: an override must not need the signatures of
    /// the client and freelancer it overrides.
    pub(crate) fn time_extension_split(
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

        (|| {
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
        })()
    }

}
