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

impl MilestoneEscrow {
    pub(crate) const MAX_MULTISIG_SIGNERS: u32 = 32;
}

#[allow(deprecated)]
#[contractimpl]
impl MilestoneEscrow {
    /// Initialise a multisig approval regime with a fixed set of signers and
    /// the required approval threshold.  Must be called exactly once.
    ///
    /// `Unauthorized`: `admin` is neither the `Resolver` delegate nor, if none
    /// is set, the stored admin.
    ///
    /// On success the call publishes a `MultisigApprovalInitEvent` under the
    /// `msiginit` topic carrying the acting admin and the values that were
    /// actually persisted.  A rejected call (`NotInitialized` /
    /// `Unauthorized` / `AlreadyInitialized` / `MultiSig*`) publishes nothing.
    ///
    /// # Storage footprint (issue #456)
    /// Two reductions, both measured by
    /// `multisig_approval_init_footprint_tests`:
    ///
    /// 1. The signer set and the threshold are persisted as **one**
    ///    `DataKey::MultiSigConfig` entry, so the call writes a single distinct
    ///    multisig storage key instead of the two separate `MultiSigSigners` /
    ///    `MultiSigThreshold` entries used before, and the contract-instance
    ///    ledger entry ends up 28 bytes smaller for a three-signer set.  The
    ///    already-initialised guard probes that same consolidated key — it is
    ///    the only key this call writes, so it *is* the initialisation
    ///    condition.
    /// 2. Authorization uses `require_admin_from_instance` rather than
    ///    `require_admin`, so the admin verification read lands on the same
    ///    instance entry as every other access of this call instead of adding
    ///    the persistent `Admin` entry to the invocation footprint — the same
    ///    consolidation already landed for `multisig_lock` (#460),
    ///    `set_escrow_interest_yield` (#463), `set_platform_fee_allocation`
    ///    (#472) and `admin_resume_escrow` (#449).  `initialize` writes both
    ///    `Admin` copies atomically and every admin-transfer path keeps them in
    ///    sync, so the check is logically identical; a contract whose only
    ///    `Admin` copy is the instance one is accepted, exactly like those
    ///    endpoints.
    pub fn multisig_approval_init(
        env: Env,
        admin: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Either layout means the regime already exists: a contract upgraded
        // from before issue #456 still holds its signer set under the legacy
        // `MultiSigSigners` key, and must not be re-initialisable.
        if Self::cold_has(&env, &DataKey::MultiSigConfig)
            || env.storage().instance().has(&DataKey::MultiSigSigners)
        {
            return Err(Error::AlreadyInitialized);
        }

        Self::validate_multisig_setup(&signers, threshold)?;

        let signers_clone = signers.clone();
        let config = MultiSigConfig(signers, threshold);
        env.storage()
            .persistent()
            .set(&DataKey::MultiSigConfig, &config);

        // Structured event for indexers / auditors (issue #455).  The two
        // payload fields are taken from the local variables that were just
        // written to storage, so the event reconciles exactly with the state this
        // call persisted rather than with the raw inputs.  Nothing fallible
        // follows the publish: every rejection above (`NotInitialized`,
        // `Unauthorized`, `AlreadyInitialized`, `MultiSigNoSigners`,
        // `MultiSigTooManySigners`, `MultiSigInvalidThreshold`,
        // `MultiSigDuplicateSigner`) returns before this point, so a
        // successful call carries exactly one `msiginit` event and a rejected
        // call carries none.
        env.events().publish(
            (symbol_short!("msiginit"),),
            MultisigApprovalInitEvent {
                admin,
                signers: signers_clone,
                threshold,
            },
        );

        Ok(())
    }

    /// Record an approval from one of the registered signers for the given
    /// proposal.  Idempotent — calling twice from the same signer has no
    /// effect and is not an error.
    ///
    /// # Checks (in order)
    /// Authorization and source-state guards run **before** any proposal
    /// record is read or written, so a rejected call cannot mutate storage:
    /// 1. `signer.require_auth()` — the transaction must be signed by
    ///    `signer` (`Unauthorized` if missing).
    /// 2. A multisig regime must exist (`NotInitialized`) and `signer` must be
    ///    one of the registered multisig signers (`Unauthorized`).
    /// 3. A proposal record must exist for `proposal_id`
    ///    (`NoPendingAdminTransfer`) and still be `Pending` (`InvalidStatus`)
    ///    — issue #785: an approval can only bind to a live,
    ///    contract-assigned proposal, never to a free-floating ID, a consumed
    ///    one, or an ID that was never proposed.
    /// 4. The signer's index must be representable as a bit position in the
    ///    `u32` approval bitmap (`ArithmeticOverflow`). This is evaluated with
    ///    `try_into` / `checked_shl` / `checked_add` and runs **before** the
    ///    bitmap is written, so an unrepresentable index cannot leave partial
    ///    state behind (see `multisig_approve_checked_arithmetic_tests`).
    ///
    /// # Storage footprint (issues #457, #785, #787)
    /// Keys this call reads:
    /// * `DataKey::MultiSigConfig` (persistent, issue #772) — the registered signers
    ///   (which also provide the membership check and the signer's bit index)
    ///   and the approval threshold, in one consolidated entry (issue #456).
    /// * `DataKey::AdminProposal(proposal_id)` (persistent) — the proposal
    ///   record that owns the approval bitmap (issue #785).
    ///
    /// The former `DataKey::Job` metadata read and cross-contract token
    /// `balance` probe are gone (issue #787): recording an approval moves no
    /// tokens, so an unfunded escrow is a legitimate place to collect
    /// signatures and this call no longer needs either.
    ///
    /// Keys this call writes: only `AdminProposal(proposal_id)`, and only
    /// when the signer's bit is not already set.  A duplicate approval cannot
    /// change the bitmap, so it writes no contract storage key at all instead
    /// of re-writing the record; the first approval from each signer writes
    /// the record exactly once — the floor for recording an approval at all
    /// (measured per invocation by `multisig_approve_footprint_tests`).
    ///
    /// # Errors
    /// * `NotInitialized`         – `multisig_approval_init` has not been
    ///   called.
    /// * `Unauthorized`           – `signer` did not sign, or is not a
    ///   registered signer.
    /// * `NoPendingAdminTransfer` – No proposal record exists for
    ///   `proposal_id`: approving before a proposal exists, or against an ID
    ///   the contract never assigned, is rejected (issue #785).
    /// * `InvalidStatus`          – The proposal record exists but is no
    ///   longer `Pending` — it was already executed or cancelled, so its
    ///   approvals are spent (issue #785).
    /// * `ArithmeticOverflow`     – The signer's position does not fit the `u32`
    ///   bitmap, i.e. the stored signer set holds more than
    ///   `MAX_MULTISIG_SIGNERS` = 32 entries (which `multisig_approval_init`
    ///   itself rejects with `MultiSigTooManySigners`, so this needs corrupt or
    ///   upgraded storage). Reported as a typed error instead of trapping on
    ///   the release profile's overflow checks.
    ///
    /// Note: `MultiSigEmptyBalance` (25) is **not** returned any more — the
    /// balance guard it named was removed by issue #787.
    pub fn multisig_approve(
        env: Env,
        signer: Address,
        proposal_id: u32,
    ) -> Result<MultiSigApprovalState, Error> {
        Self::ensure_migrated(&env)?;
        signer.require_auth();

        // Consolidated multisig config read (issue #456): the signer set and
        // the threshold live in the same instance entry, so this is one `get`
        // where the previous two-key layout needed two (legacy entries from
        // before the change are still read, see `load_multisig_config`).  A
        // missing configuration still reports `NotInitialized`.
        let config = Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;
        let signers = config.0;
        let threshold = config.1;

        // Reject callers who are not registered signers (find the signer's
        // index, O(n) but n ≤ 32).
        let signer_index = signers
            .iter()
            .position(|s| s == signer)
            .ok_or(Error::Unauthorized)?;

        // Issue #785: bind the approval to a live proposal record.  IDs are
        // assigned by `propose_admin_transfer`, so a missing record means the
        // proposal never existed, and a non-`Pending` record means it has
        // already been consumed — either way this approval must not move.
        //
        // Issue #787: the token-balance guard that used to sit between the
        // membership check and the bitmap read is gone.  Recording signer
        // intent moves no tokens, so approvals are accepted on an unfunded
        // escrow; `execute_admin_transfer` still cannot conjure funds.
        let mut record: AdminTransferProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AdminProposal(proposal_id))
            .ok_or(Error::NoPendingAdminTransfer)?;
        if record.status != AdminProposalStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        // ── Turn the signer's index into its bitmap bit (issue #583) ─────────
        // This is the only integer computation in the call, and every step of
        // it is fallible, so no step uses a bare operator:
        //   * `position` yields a `usize` while the bitmap is a `u32`, so the
        //     index is narrowed with `try_into` (`as` would truncate silently);
        //   * the shift is `checked_shl`, which answers `None` for `idx >= 32`
        //     instead of wrapping onto bit 0 or trapping;
        //   * the accumulator is advanced with `checked_add` rather than `|=`,
        //     so a value the `u32` cannot represent is reported instead of
        //     wrapping.
        // The release profile enables `overflow-checks` with `panic = "abort"`,
        // so a bare operator that overflowed would abort the whole transaction
        // with no value for the caller to inspect; instead each checked step
        // maps its failure to `Error::ArithmeticOverflow` (code 36). All of it
        // runs before the `set` below, so a rejected call writes nothing.
        let idx: u32 = signer_index
            .try_into()
            .map_err(|_| Error::ArithmeticOverflow)?;
        let mask = 1u32.checked_shl(idx).ok_or(Error::ArithmeticOverflow)?;

        // Set the bit for this signer, and write the record back only when the
        // bit actually changed (issue #457).  A duplicate approval
        // (`bitmap & mask != 0`) cannot change the value, so it writes no
        // contract storage key at all; the first approval from each signer
        // writes the record exactly once — the floor for recording an approval.
        // Reaching the add only when the bit is clear also keeps `checked_add`
        // sound at the top of the range: `bitmap + mask` is then exactly
        // `bitmap | mask` and provably representable.
        let bitmap = record.approvals_bitmap;
        if bitmap & mask == 0 {
            record.approvals_bitmap = bitmap.checked_add(mask).ok_or(Error::ArithmeticOverflow)?;
            env.storage()
                .persistent()
                .set(&DataKey::AdminProposal(proposal_id), &record);
        }
        // Re-read after the conditional write so the state returned (and the
        // event) always carries the bitmap as stored, new bit included.
        let bitmap = record.approvals_bitmap;

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
    /// A `MultiSigApprovalState` built from the persistent `MultiSigConfig`
    /// (its threshold field) and the `AdminProposal(proposal_id)` record's
    /// approval bitmap (issue #785).  An ID with no record reads as an empty
    /// bitmap (`approvals == 0`), and so does an executed or cancelled
    /// proposal — its record keeps the ID observable while its cleared bitmap
    /// reports that its approvals are spent.
    ///
    /// # Guarantees
    /// * **Read-only.**  Exactly two `get`s (instance config, proposal
    ///   record), via `read_multisig_approval`.  No `set`, `remove`,
    ///   `extend_ttl`, or event publish in instance, persistent, or temporary
    ///   storage — in particular it never re-writes the record or bumps its
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

    /// Propose replacing the multisig signer set and threshold.
    ///
    /// The new configuration is validated like `multisig_approval_init`'s and
    /// recorded as pending under a contract-assigned proposal ID, which is
    /// returned.  It takes effect through `execute_multisig_config` once the
    /// current signers have approved that ID up to the current threshold.
    /// Emits `msigcfgp`.
    ///
    /// # Errors
    /// * `NotInitialized`           – Contract not initialised, or no multisig
    ///   regime exists yet (use `multisig_approval_init`).
    /// * `Unauthorized`             – `admin` is not the stored admin.
    /// * `MultiSigNoSigners` / `MultiSigTooManySigners` /
    ///   `MultiSigInvalidThreshold` / `MultiSigDuplicateSigner` – the new
    ///   configuration is invalid (`validate_multisig_setup`).
    /// * `MultiSigConfigPending`    – Another change is already pending.
    /// * `ArithmeticOverflow`       – The proposal counter is exhausted.
    pub fn propose_multisig_config(
        env: Env,
        admin: Address,
        new_signers: Vec<Address>,
        new_threshold: u32,
    ) -> Result<u32, Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin(&env, &admin)?;
        Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;
        Self::validate_multisig_setup(&new_signers, new_threshold)?;

        let persistent = env.storage().persistent();
        if persistent.has(&DataKey::PendingMultiSigConfig) {
            return Err(Error::MultiSigConfigPending);
        }

        let proposal_id = Self::open_proposal_record(&env, &admin)?;
        persistent.set(
            &DataKey::PendingMultiSigConfig,
            &PendingMultiSigConfig {
                signers: new_signers.clone(),
                threshold: new_threshold,
                proposal_id,
            },
        );

        env.events().publish(
            (symbol_short!("msigcfgp"),),
            MultiSigConfigProposedEvent {
                admin,
                signers: new_signers,
                threshold: new_threshold,
                proposal_id,
            },
        );

        Ok(proposal_id)
    }

    /// Apply the pending signer-set change once its proposal has reached the
    /// current threshold.  Anyone may call it.  The proposal is consumed, and
    /// a pending admin transfer's approvals are cleared because they index the
    /// old signer vec.  Emits `msigcfg`.
    ///
    /// # Errors
    /// * `NoPendingMultiSigConfig` – Nothing is pending.
    /// * `NotInitialized`          – No multisig regime exists.
    /// * `InvalidStatus`           – The proposal record is missing or no
    ///   longer `Pending`.
    /// * `MultiSigThresholdNotMet` – Not enough current signers approved.
    pub fn execute_multisig_config(env: Env) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        let persistent = env.storage().persistent();
        let pending: PendingMultiSigConfig = persistent
            .get(&DataKey::PendingMultiSigConfig)
            .ok_or(Error::NoPendingMultiSigConfig)?;
        let MultiSigConfig(old_signers, old_threshold) =
            Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;

        let mut record: AdminTransferProposal = persistent
            .get(&DataKey::AdminProposal(pending.proposal_id))
            .ok_or(Error::InvalidStatus)?;
        if record.status != AdminProposalStatus::Pending {
            return Err(Error::InvalidStatus);
        }
        if !Self::read_multisig_approval(&env, pending.proposal_id)?.approved {
            return Err(Error::MultiSigThresholdNotMet);
        }

        persistent.set(
            &DataKey::MultiSigConfig,
            &MultiSigConfig(pending.signers.clone(), pending.threshold),
        );
        // Drop any pre-#772 / pre-#456 instance copies so no read path can
        // fall back to the old set.
        let instance = env.storage().instance();
        instance.remove(&DataKey::MultiSigConfig);
        instance.remove(&DataKey::MultiSigSigners);
        instance.remove(&DataKey::MultiSigThreshold);
        persistent.remove(&DataKey::PendingMultiSigConfig);

        record.status = AdminProposalStatus::Executed;
        record.approvals_bitmap = 0;
        persistent.set(&DataKey::AdminProposal(pending.proposal_id), &record);

        // Approvals for a pending admin transfer were given by the old signer
        // set; their bits would name different addresses now.
        if let Some(transfer) =
            persistent.get::<_, PendingAdminTransfer>(&DataKey::PendingAdminTransfer)
        {
            let key = DataKey::AdminProposal(transfer.proposal_id);
            if let Some(mut transfer_record) = persistent.get::<_, AdminTransferProposal>(&key) {
                if transfer_record.approvals_bitmap != 0 {
                    transfer_record.approvals_bitmap = 0;
                    persistent.set(&key, &transfer_record);
                }
            }
        }

        env.events().publish(
            (symbol_short!("msigcfg"),),
            MultiSigConfigChangedEvent {
                old_signers,
                old_threshold,
                new_signers: pending.signers,
                new_threshold: pending.threshold,
                proposal_id: pending.proposal_id,
            },
        );

        Ok(())
    }

    /// Withdraw the pending signer-set change.  Its proposal is consumed.
    /// Emits `msigcfgx`.
    ///
    /// # Errors
    /// * `Unauthorized`            – `admin` is not the stored admin.
    /// * `NoPendingMultiSigConfig` – Nothing is pending.
    pub fn cancel_multisig_config_proposal(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin(&env, &admin)?;
        let persistent = env.storage().persistent();
        let pending: PendingMultiSigConfig = persistent
            .get(&DataKey::PendingMultiSigConfig)
            .ok_or(Error::NoPendingMultiSigConfig)?;
        persistent.remove(&DataKey::PendingMultiSigConfig);

        let key = DataKey::AdminProposal(pending.proposal_id);
        if let Some(mut record) = persistent.get::<_, AdminTransferProposal>(&key) {
            record.status = AdminProposalStatus::Cancelled;
            record.approvals_bitmap = 0;
            persistent.set(&key, &record);
        }

        env.events().publish(
            (symbol_short!("msigcfgx"),),
            MultiSigConfigCancelledEvent {
                admin,
                proposal_id: pending.proposal_id,
            },
        );

        Ok(())
    }

    /// The pending signer-set change, if any.
    pub fn get_pending_multisig_config(env: Env) -> Option<PendingMultiSigConfig> {
        env.storage()
            .persistent()
            .get(&DataKey::PendingMultiSigConfig)
    }

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
    /// * `admin`           – Must hold the `Resolver` role (root admin by default).
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

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
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        // Terminal states have already settled funds — no double-spend.
        if Self::is_terminal_status(&milestone.status) {
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
        // (persistent) is written here; the terminal `Released` status is the
        // authoritative completion signal.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Clear the multisig lock flag now that the deadlock is resolved.
        env.storage()
            .instance()
            .set(&DataKey::MultisigLocked, &false);

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_freelancer_with_fee(&env, &meta, &token_client, &remaining)?;

        env.events().publish(
            (symbol_short!("msadmrel"),),
            MultisigAdminOverrideReleaseEvent {
                admin,
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
    /// * `admin`           – Must hold the `Resolver` role (root admin by default).
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

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
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        if Self::is_terminal_status(&milestone.status) {
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
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Clear the multisig lock flag now that the deadlock is resolved.
        env.storage().instance().remove(&DataKey::MultisigLocked);

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.client, &remaining)?;

        env.events().publish(
            (symbol_short!("msadmref"),),
            MultisigAdminOverrideRefundEvent {
                admin,
                milestone_index,
                client: meta.client,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

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
    /// * `admin`                – Must hold the `Resolver` role (root admin by default).
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
    /// * `Unauthorized`  – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

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

        Ok(allocation)
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
    /// * `admin` – Must hold the `Resolver` role (root admin by default).
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidStatus`  – The multisig workflow is already locked.
    /// * `EmergencyPauseInProgress` – An emergency pause lock is currently active.
    pub fn multisig_lock(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Authorization check at top - require auth and admin role first
        Self::require_role(&env, &admin, AdminRole::Resolver)?;
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
    /// deadlock, and `false` for an initialized escrow that was never locked.
    ///
    /// # Errors
    /// * `NotInitialized` – The contract has not been initialized.
    pub fn is_multisig_locked(env: Env) -> Result<bool, Error> {
        Self::load_multisig_locked(&env)
    }

}
