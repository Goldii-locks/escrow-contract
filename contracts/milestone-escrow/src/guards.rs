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
impl MilestoneEscrow {
    pub(crate) fn require_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        admin.require_auth();
        let stored_admin = Self::load_admin(env)?;
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
    pub(crate) fn require_role(env: &Env, caller: &Address, role: AdminRole) -> Result<(), Error> {
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
    pub(crate) fn assert_dispute_timeout_elapsed(env: &Env, index: u32) -> Result<(), Error> {
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
    pub(crate) fn require_client_and_freelancer_consent(env: &Env) -> Result<JobMeta, Error> {
        let meta = Self::load_job_meta(env)?;

        // Order is irrelevant to correctness — both must succeed — but the
        // client is checked first to mirror `set_interest_yield_consent`.
        meta.client.require_auth();
        meta.freelancer.require_auth();

        Ok(meta)
    }

    /// Fee changes are only unilateral before funding. Once the parties have
    /// committed funds, the allocation is frozen into `JobMeta` and any change
    /// needs both parties' signatures (issue #694).
    pub(crate) fn require_fee_allocation_change_allowed(env: &Env) -> Result<(), Error> {
        let meta = Self::load_job_meta(env)?;
        if meta.funded {
            Self::require_client_and_freelancer_consent(env)?;
        }
        Ok(())
    }

    /// Reject the call before any other ledger access if the contract has not
    /// been initialised.
    pub(crate) fn require_initialized(env: &Env) -> Result<(), Error> {
        if !Self::is_initialized(env) {
            return Err(Error::NotInitialized);
        }
        Ok(())
    }

    /// Verify that the caller is either the stored client or freelancer for
    /// this escrow.  Used by `raise_dispute` to ensure only authorised parties
    /// can initiate a dispute.  Returns the loaded `JobMeta` on success so the
    /// caller does not need a second instance read.
    pub(crate) fn require_dispute_party(env: &Env, caller: &Address) -> Result<JobMeta, Error> {
        caller.require_auth();
        let meta = Self::load_job_meta(env)?;
        if meta.client != *caller && meta.freelancer != *caller {
            return Err(Error::Unauthorized);
        }
        Ok(meta)
    }

    pub(crate) fn ensure_not_paused(env: &Env) -> Result<(), Error> {
        Self::assert_not_paused(env)?;
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

    /// Reject a pause-gated settlement while the contract token balance is
    /// zero, so a refund never attempts an empty transfer.
    pub(crate) fn assert_nonzero_balance(env: &Env, meta: &JobMeta) -> Result<(), Error> {
        let token_client = token::Client::new(env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::EmptyBalance);
        }
        Ok(())
    }

    pub(crate) fn validate_fee_allocation(
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
    #[cfg(test)]
    pub(crate) fn validate_interest_yield_params(
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
    #[cfg(test)]
    pub(crate) fn validate_interest_yield_share_config(
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
    #[cfg(test)]
    pub(crate) fn validate_yield_rate_bps(rate_bps: u32) -> Result<(), Error> {
        if rate_bps > BPS_SCALE {
            return Err(Error::InvalidRatio);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn ensure_interest_yield_unlocked(env: &Env) -> Result<(), Error> {
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
    #[cfg(test)]
    pub(crate) fn ensure_interest_yield_writable(env: &Env) -> Result<(), Error> {
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
    pub(crate) fn validate_streaming_ratio(
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

    /// Reject with `NotAccepted` unless both the freelancer and the arbiter
    /// have accepted the job.
    pub(crate) fn require_accepted(env: &Env) -> Result<(), Error> {
        if Self::load_acceptance(env) != 3 {
            return Err(Error::NotAccepted);
        }
        Ok(())
    }

    pub(crate) fn require_escrow_has_funding(env: &Env, meta: &JobMeta) -> Result<(), Error> {
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

    pub(crate) fn require_milestone_is_funded(
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
    pub(crate) fn milestone_is_funded(meta: &JobMeta, milestone: &Milestone) -> bool {
        meta.funded || milestone.funded
    }

    pub(crate) fn is_terminal_status(status: &MilestoneStatus) -> bool {
        matches!(
            status,
            MilestoneStatus::Released | MilestoneStatus::Refunded | MilestoneStatus::Split
        )
    }

    /// Reject client review actions on a `Delivered` milestone once its
    /// auto-release deadline has passed, so the expired window cannot be
    /// undone before the freelancer claims.
    pub(crate) fn assert_review_window_open(
        env: &Env,
        meta: &JobMeta,
        milestone_index: u32,
        milestone: &Milestone,
    ) -> Result<(), Error> {
        if milestone.status != MilestoneStatus::Delivered {
            return Ok(());
        }
        let delivered_at = milestone.delivered_at;
        let extension = Self::load_time_extension(env, milestone_index);
        let deadline = Self::auto_release_deadline(meta, delivered_at, extension)?;
        if env.ledger().timestamp() >= deadline {
            return Err(Error::ReviewWindowClosed);
        }
        Ok(())
    }

    /// Return `Err(Error::Paused)` when the admin pause or the party
    /// emergency pause is active (see `is_paused`) so that every user-facing
    /// endpoint can call this as its first operation.
    pub(crate) fn assert_not_paused(env: &Env) -> Result<(), Error> {
        if Self::is_paused(env) {
            return Err(Error::Paused);
        }
        Ok(())
    }

    pub(crate) fn checked_add_amount(total: i128, amount: i128) -> Result<i128, Error> {
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
    pub(crate) fn checked_initialize_total(milestone_amounts: &Vec<i128>) -> Result<i128, Error> {
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

    pub(crate) fn validate_fund_amount(meta: &JobMeta) -> Result<i128, Error> {
        let total_amount = meta.total_amount;
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // `total_amount` is kept in step with the milestones by initialize,
        // add_milestone and reduce_milestone, so fund needs no milestone reads
        // (#730).

        Ok(total_amount)
    }

    pub(crate) fn validate_fund_client(env: &Env, client: &Address) -> Result<(), Error> {
        if client == &env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        Ok(())
    }

    pub(crate) fn validate_address(env: &Env, address: &Address) -> Result<(), Error> {
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
    pub(crate) fn validate_new_admin(
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

    /// Validates multisig signer list and approval threshold before persistence.
    ///
    /// Rejects empty or oversized signer sets, thresholds outside `1..=signer_count`,
    /// and duplicate signer addresses.
    pub(crate) fn validate_multisig_setup(signers: &Vec<Address>, threshold: u32) -> Result<(), Error> {
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

    /// Reject state-changing calls until `migrate` has brought storage up to
    /// `SCHEMA_VERSION`.  An uninitialized contract passes, so its endpoints
    /// keep reporting `NotInitialized`.
    pub(crate) fn ensure_migrated(env: &Env) -> Result<(), Error> {
        let instance = env.storage().instance();
        match instance.get::<_, u32>(&DataKey::SchemaVersion) {
            Some(version) if version >= SCHEMA_VERSION => Ok(()),
            Some(_) => Err(Error::MigrationRequired),
            // `DataKey::Version` is the initialisation marker.
            None if instance.has(&DataKey::Version) => Err(Error::MigrationRequired),
            None => Ok(()),
        }
    }

}
