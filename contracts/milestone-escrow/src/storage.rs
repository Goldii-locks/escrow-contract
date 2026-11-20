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
    pub(crate) fn bump(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    /// Load the single source of truth for the admin: the **instance** entry.
    ///
    /// Pre-#679 deployments also carry a legacy persistent `DataKey::Admin`
    /// copy. It is deliberately ignored here; see the migration note on
    /// `migrate`.
    pub(crate) fn load_admin(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
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
    /// instance storage until the admin calls `migrate`, so an
    /// absent persistent entry falls back to the instance copy. The instance
    /// entry is loaded by every call anyway, so the fallback costs no extra
    /// ledger entry, and the persistent copy always wins once written.
    ///
    /// **This function must contain only read operations.**
    pub(crate) fn cold_get<V: TryFromVal<Env, Val>>(env: &Env, key: &DataKey) -> Option<V> {
        env.storage()
            .persistent()
            .get(key)
            .or_else(|| env.storage().instance().get(key))
    }

    /// Presence check matching [`Self::cold_get`].
    pub(crate) fn cold_has(env: &Env, key: &DataKey) -> bool {
        env.storage().persistent().has(key) || env.storage().instance().has(key)
    }

    /// Move one cold-configuration entry from instance to persistent storage.
    /// A persistent copy written after the upgrade is newer, so it is kept and
    /// only the stale instance copy is dropped. Returns whether anything was
    /// found in instance storage.
    pub(crate) fn migrate_cold_entry<V>(env: &Env, key: &DataKey) -> bool
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

    /// The one "has `initialize` run?" predicate. `DataKey::Job` is written by
    /// every initialisation path and is what `load_job_meta` reads, so it is
    /// the marker; do not test `Admin` or `Version` ad hoc.
    pub(crate) fn is_initialized(env: &Env) -> bool {
        env.storage().instance().has(&DataKey::Job)
    }

    /// Total amount credited to recipients via `pay_or_credit` and not yet
    /// withdrawn. Returns `0` when nothing has ever been credited.
    pub(crate) fn load_total_owed(env: &Env) -> i128 {
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
    pub(crate) fn clear_time_ext(env: &Env, milestone_index: u32) {
        let key = DataKey::TimeExt(milestone_index);
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
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
    /// Treasury receiving platform fees. Defaults to the admin until an
    /// admin-timelocked update is applied, so old deployments need no migration.
    pub(crate) fn treasury_address(env: &Env) -> Result<Address, Error> {
        match env.storage().instance().get(&DataKey::Treasury) {
            Some(treasury) => Ok(treasury),
            None => Self::load_admin(env),
        }
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
    #[cfg(test)]
    pub(crate) fn load_interest_yield_state(env: &Env) -> Result<EscrowInterestYieldState, Error> {
        Self::cold_get(env, &DataKey::InterestYieldState).ok_or(Error::NotInitialized)
    }

    /// Read `DataKey::PlatformFeeAllocation` (persistent, see `cold_get`).
    ///
    /// This is the single source of truth for the platform-fee allocation read
    /// path.  Every function that needs the stored value — including the public
    /// `get_platform_fee_allocation`, `calculate_platform_fee_split`, and the
    /// write-path helpers — delegates here so the storage key is referenced in
    /// exactly one place and each invocation issues exactly one ledger read.
    pub(crate) fn load_platform_fee_allocation(env: &Env) -> Result<PlatformFeeAllocation, Error> {
        Self::cold_get(env, &DataKey::PlatformFeeAllocation).ok_or(Error::NotInitialized)
    }

    /// Read the multisig deadlock flag. `MultisigLocked` is only written by
    /// `multisig_lock`, so an initialized escrow that was never locked has no
    /// entry and reads as `false`; only a contract without an instance `Admin`
    /// (never initialized) reports `NotInitialized`.
    ///
    /// **This function must contain only read operations.**
    pub(crate) fn load_multisig_locked(env: &Env) -> Result<bool, Error> {
        Self::require_initialized(env)?;
        Ok(env
            .storage()
            .instance()
            .get(&DataKey::MultisigLocked)
            .unwrap_or(false))
    }

    #[cfg(test)]
    pub(crate) fn store_interest_yield_state(env: &Env, state: &EscrowInterestYieldState) {
        env.storage()
            .persistent()
            .set(&DataKey::InterestYieldState, state);
    }

    pub(crate) fn load_job_meta(env: &Env) -> Result<JobMeta, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)
    }

    pub(crate) fn store_job_meta(env: &Env, meta: &JobMeta) {
        env.storage().instance().set(&DataKey::Job, meta);
    }

    pub(crate) fn load_acceptance(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Acceptance)
            .unwrap_or(0)
    }

    /// Record `bit` in the acceptance mask on behalf of `party` and publish
    /// the acceptance under `topic`.
    pub(crate) fn record_acceptance(
        env: &Env,
        party: Address,
        client: Address,
        bit: u32,
        topic: Symbol,
    ) -> Result<(), Error> {
        let mask = Self::load_acceptance(env);
        if mask & bit != 0 {
            return Err(Error::InvalidStatus);
        }
        let new_mask = mask | bit;
        env.storage()
            .instance()
            .set(&DataKey::Acceptance, &new_mask);

        env.events().publish(
            (topic, party.clone(), client),
            JobAcceptedEvent {
                party,
                acceptance_mask: new_mask,
            },
        );
        Ok(())
    }

    /// Number of milestones holding their own deposit (see
    /// `DataKey::FundedCount`).  O(1) once the counter exists; escrows that
    /// predate it fall back to counting `Milestone::funded` once.
    pub(crate) fn load_funded_count(env: &Env, meta: &JobMeta) -> Result<u32, Error> {
        if let Some(count) = env
            .storage()
            .instance()
            .get::<_, u32>(&DataKey::FundedCount)
        {
            return Ok(count);
        }
        let mut count = 0u32;
        for index in 0..meta.milestone_count {
            if Self::load_milestone(env, index)?.funded {
                count += 1;
            }
        }
        Ok(count)
    }

    pub(crate) fn load_milestone(env: &Env, index: u32) -> Result<Milestone, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Milestone(index))
            .ok_or(Error::InvalidMilestone)
    }

    pub(crate) fn store_milestone(env: &Env, index: u32, milestone: &Milestone) {
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

    pub(crate) fn load_funding_mode(env: &Env) -> FundingMode {
        env.storage()
            .instance()
            .get(&DataKey::FundingMode)
            .unwrap_or(FundingMode::Upfront)
    }

    pub(crate) fn load_time_extension(env: &Env, index: u32) -> u32 {
        env.storage()
            .temporary()
            .get(&DataKey::TimeExt(index))
            .or_else(|| env.storage().persistent().get(&DataKey::TimeExt(index)))
            .unwrap_or(0)
    }

    /// Read-only accessor for `DataKey::WhitelistedTokens`.
    pub(crate) fn read_whitelist(env: &Env) -> Option<Vec<Address>> {
        Self::cold_get(env, &DataKey::WhitelistedTokens)
    }

    pub(crate) fn load_due_at(env: &Env, index: u32) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::DueAt(index))
            .unwrap_or(0)
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
    /// Returns `Err(Error::NotInitialized)` when the emergency-pause flag has
    /// never been written; otherwise the pause state from `is_paused`.  Both
    /// reads land on the instance entry, so the function touches exactly one
    /// ledger entry.
    ///
    /// **This function must contain only read operations.**
    pub(crate) fn load_emergency_paused(env: &Env) -> Result<bool, Error> {
        if !env.storage().instance().has(&DataKey::Ep) {
            return Err(Error::NotInitialized);
        }
        Ok(Self::is_paused(env))
    }

    /// The one pause-state check: `true` while the admin pause
    /// (`DataKey::Paused`) or the party emergency pause (`DataKey::Ep`) is
    /// active.  Absent flags read as `false`, so a guard on an uninitialized
    /// contract falls through to its own initialization check.
    ///
    /// **This function must contain only read operations.**
    pub(crate) fn is_paused(env: &Env) -> bool {
        let instance = env.storage().instance();
        instance.get::<_, bool>(&DataKey::Paused).unwrap_or(false)
            || instance.get::<_, bool>(&DataKey::Ep).unwrap_or(false)
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
    /// `migrate` folds them into the persistent
    /// `MultiSigConfig` (issue #772).
    ///
    /// **This function must contain only read operations.**
    pub(crate) fn load_multisig_config(env: &Env) -> Option<MultiSigConfig> {
        if let Some(config) = Self::cold_get(env, &DataKey::MultiSigConfig) {
            return Some(config);
        }
        let instance = env.storage().instance();
        let signers: Vec<Address> = instance.get(&DataKey::MultiSigSigners)?;
        let threshold: u32 = instance.get(&DataKey::MultiSigThreshold)?;
        Some(MultiSigConfig(signers, threshold))
    }

    /// Read path behind `is_multisig_approved`.
    ///
    /// **This function must contain only read operations.**
    pub(crate) fn read_multisig_approval(env: &Env, proposal_id: u32) -> Result<MultiSigApprovalState, Error> {
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
    pub(crate) fn read_pending_admin_transfer(env: &Env) -> Result<Option<PendingAdminTransfer>, Error> {
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

    pub(crate) fn read_schema_version(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::SchemaVersion)
            .unwrap_or(1)
    }

}
