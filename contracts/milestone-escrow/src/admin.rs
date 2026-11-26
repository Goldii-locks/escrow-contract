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
    /// 4. No multisig approval regime may exist (`Unauthorized`) — issue #786:
    ///    once `multisig_approval_init` has registered signers, a
    ///    single-signature transfer would bypass them; the admin must use
    ///    `propose_admin_transfer` → approvals → `execute_admin_transfer`.
    /// 5. `new_admin` must not be a zero address, the contract itself, or the
    ///    current admin (`InvalidAddress`).
    /// 6. No multisig admin-transfer proposal may already be pending
    ///    (`AdminTransferPending`).
    ///
    /// # Parameters
    /// * `current_admin` – Must match the currently stored admin. Must
    ///                     authorize the call.
    /// * `new_admin`     – Address to become the new admin.
    ///
    /// # Errors
    /// * `NotInitialized`       – Contract has not been initialized.
    /// * `Unauthorized`         – `current_admin` is not the stored admin, or a
    ///                            multisig regime is configured (issue #786;
    ///                            use the multisig proposal flow instead).
    /// * `InvalidAddress`       – `new_admin` is a zero address, the contract
    ///                            itself, or the current admin.
    /// * `AdminTransferPending` – A multisig admin-transfer proposal is already
    ///                            pending; execute or cancel it first.
    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Auth + init guards first — a single require_auth so the host does not
        // abort on a duplicated auth requirement.
        current_admin.require_auth();
        // Footprint: a single read of DataKey::Admin serves both the
        // initialization guard and the authorization check. Previously this
        // touched the entry twice — a separate presence probe followed by
        // `load_admin()` (which also reads it). `load_admin` already returns
        // `NotInitialized` when the key is absent, so dropping the redundant
        // `has` keeps behavior identical (absent → NotInitialized, mismatch →
        // Unauthorized) while reading the entry only once.
        let stored_admin = Self::load_admin(&env)?;
        if stored_admin != current_admin {
            return Err(Error::Unauthorized);
        }

        // Issue #786: after `multisig_approval_init` the legacy single-signature
        // transfer path is closed.  It would let one key swap the admin without
        // any signer approval, silently bypassing the regime the signers were
        // registered for; every admin change on a multisig-configured contract
        // must go through propose → approve → execute, which is the flow the
        // signers audit.  `load_multisig_config` reads only, covering both the
        // consolidated entry and a pre-#456 legacy two-key layout.
        if Self::load_multisig_config(&env).is_some() {
            return Err(Error::Unauthorized);
        }

        Self::validate_new_admin(&env, &stored_admin, &new_admin)?;

        if env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
        {
            return Err(Error::AdminTransferPending);
        }

        // Single source of truth: instance storage only (#679).
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
    /// * `admin` – Must hold the `FeeManager` role (root admin by default). Must authorize the
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
    ///   1. `require_admin` fails to load `DataKey::Admin` from instance
    ///      storage (the contract was never initialized).
    ///   2. `load_job_meta` fails to load `DataKey::JobMeta` from instance
    ///      storage (initialize was never called or storage was cleared).
    ///   3. The whitelist load fails to find `DataKey::WhitelistedTokens` in
    ///      instance storage (same root cause as case 2).
    ///   In practice, all three keys are written atomically by `initialize`,
    ///   so only case 1 is reachable after a fully successful initialization.
    ///
    /// * [`Error::Unauthorized`] – `admin` is neither the `FeeManager` delegate
    ///   nor, if none is set, the stored admin, or `admin.require_auth()` was
    ///   not satisfied by the transaction's authorization envelope.
    ///
    /// * [`Error::InvalidAddress`] – `token` is one of the sentinel invalid
    ///   addresses (checked immediately after the admin check, before any
    ///   storage reads for job metadata or the whitelist):
    ///   - The Stellar zero account,
    ///   - The canonical Soroban zero contract,
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
    /// * [`Error::WhitelistFull`] – The whitelist already contains
    ///   `MAX_WHITELIST_SIZE` (50) entries. Adding another would exceed the
    ///   capacity cap. This guards against unbounded `Vec` growth and `u32`
    ///   length-counter overflow.
    pub fn add_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        Self::validate_address(&env, &token)?;
        if token == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

        let mut whitelist: Vec<Address> =
            Self::cold_get(&env, &DataKey::WhitelistedTokens).ok_or(Error::NotInitialized)?;

        // Duplicate check runs before the capacity check so that a
        // full whitelist still reports TokenAlreadyWhitelisted (rather
        // than InvalidAmount) for a token that's already present.
        if whitelist.contains(&token) {
            return Err(Error::TokenAlreadyWhitelisted);
        }

        if whitelist.len() >= MAX_WHITELIST_SIZE {
            return Err(Error::WhitelistFull);
        }

        whitelist.push_back(token.clone());
        env.storage()
            .persistent()
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
    /// * `Unauthorized`        - `admin` is neither the `FeeManager` delegate nor, if none is set,
    ///   the stored admin.
    /// * `AlreadyFunded`       - The job has already been funded, preventing further whitelist modifications.
    /// * `InvalidAddress`      - The provided `token` address is the zero account or the zero contract.
    /// * `TokenNotWhitelisted` - The whitelist is empty, or the provided `token` is not present in the whitelist.
    /// * `WhitelistMinimum`    - Removing the token would leave the whitelist empty (the contract requires at least one token to remain).
    pub fn remove_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

        // Kept (unlike a bare lookup miss) so the zero sentinels still report
        // `InvalidAddress` rather than `TokenNotWhitelisted`.
        Self::validate_address(&env, &token)?;

        let mut whitelist: Vec<Address> =
            Self::cold_get(&env, &DataKey::WhitelistedTokens).ok_or(Error::NotInitialized)?;

        let len = whitelist.len();
        if len == 0 {
            return Err(Error::TokenNotWhitelisted);
        }
        if len == 1 {
            return Err(Error::WhitelistMinimum);
        }

        let index = whitelist
            .first_index_of(&token)
            .ok_or(Error::TokenNotWhitelisted)?;

        // Swap-remove: move the last element into the vacated slot.
        let last = len - 1;
        if index != last {
            let last_elem = whitelist.get(last).ok_or(Error::TokenNotWhitelisted)?;
            whitelist.set(index, last_elem);
        }
        whitelist.pop_back();
        env.storage()
            .persistent()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        env.events().publish(
            (symbol_short!("wldel"),),
            TokenRemovedEvent { admin, token },
        );

        Ok(())
    }

    /// Returns whether `token` is present in the stored whitelist.
    ///
    /// Returns `false` if the whitelist has not been stored, is empty, or does
    /// not contain `token`. Returns `true` when it does. A full whitelist is
    /// checked the same way; its size does not otherwise affect the result.
    /// This read-only query requires no authorization.
    pub fn is_token_whitelisted(env: Env, token: Address) -> bool {
        Self::read_whitelist(&env).is_some_and(|whitelist| whitelist.contains(&token))
    }

    /// Return the list of whitelisted token addresses.
    ///
    /// Strictly read-only: performs a single `get` on instance storage and no
    /// writes to instance, persistent, or temporary storage. Keep it that way —
    /// the storage handle is only ever used through `Self::read_whitelist`.
    /// No `has` probe precedes the `get`: the `Option` it returns already
    /// distinguishes "never written" from "written", so the key is resolved
    /// exactly once per invocation (#484).
    ///
    /// # Errors
    /// * `NotInitialized` – The whitelist has never been written (the contract
    ///   has not been initialized). Returns a typed error rather than
    ///   panicking or defaulting to an empty vector.
    pub fn get_whitelisted_tokens(env: Env) -> Result<Vec<Address>, Error> {
        Self::read_whitelist(&env).ok_or(Error::NotInitialized)
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
    /// * `Unauthorized`   – `admin` is neither the `Upgrader` delegate nor, if none is set, the
    ///   stored admin.
    /// * `Paused`         – The contract is currently emergency-paused.
    /// * `EscrowLocked`   – A cancel is in progress and holds the cancel lock.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::Upgrader)?;
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

    /// Bring storage written by an older version of this contract up to
    /// `SCHEMA_VERSION`.
    ///
    /// # Upgrade procedure
    /// 1. `upgrade(admin, new_wasm_hash)` installs the new code.  If the new
    ///    code's `SCHEMA_VERSION` is higher than the stored
    ///    `schema_version()`, every state-changing endpoint now returns
    ///    `MigrationRequired`, so nothing can write entries in the old shape
    ///    or misread them.  Read-only getters and `upgrade` itself still work.
    /// 2. `migrate(admin)` rewrites the stored entries one schema step at a
    ///    time and records `SCHEMA_VERSION`.  Normal operation resumes.
    ///
    /// Migration note (#679): before this change the admin was written to both
    /// instance and persistent storage. The persistent `DataKey::Admin` copy is
    /// no longer read or written by any code path; the instance copy is the
    /// single source of truth. Contracts deployed with both copies need no
    /// state rewrite: a stale persistent value is inert and cannot grant or
    /// revoke authority, and every admin-changing path now updates exactly one
    /// entry, so no stale-authority window remains.
    ///
    /// Steps:
    /// * `1 → 2`: every `Milestone` stored without `spec_hash` gains a zero
    ///   `spec_hash` (#767), and the whitelist, multisig, yield and fee
    ///   configuration moves to persistent storage (#772).  Milestones that
    ///   already carry a `spec_hash` (deployments created after #767 but
    ///   before the schema marker) are left as they are.
    /// * `2 → 3`: every `Milestone` stored without `deliverable` gains a zero
    ///   `deliverable`.
    /// * `3 → 4`: every `DisputeInfo` stored without `raised_by` gains one
    ///   (#997).  The raiser of a dispute that is already open was never
    ///   stored, so it is set to the job's client as a placeholder; the
    ///   `dispute` event for that dispute names the real caller.  Disputes
    ///   raised after the upgrade record their actual raiser.
    /// * `4 → 5`: an admin-set emergency pause (`DataKey::Ep` without
    ///   `DataKey::EpParty`) becomes the admin pause `DataKey::Paused`.
    ///
    /// Returns the new schema version and emits `("migrate",)` with a
    /// `MigratedEvent`.
    ///
    /// # Errors
    /// * `NotInitialized`  – Admin key has never been stored.
    /// * `Unauthorized`    – `admin` is neither the `Upgrader` delegate nor, if none is set, the
    ///   stored admin.
    /// * `AlreadyMigrated` – Storage is already at `SCHEMA_VERSION`.
    pub fn migrate(env: Env, admin: Address) -> Result<u32, Error> {
        Self::require_role(&env, &admin, AdminRole::Upgrader)?;
        let from_version = Self::read_schema_version(&env);
        if from_version >= SCHEMA_VERSION {
            return Err(Error::AlreadyMigrated);
        }

        if from_version < 3 {
            Self::migrate_milestones_to_v3(&env)?;
        }
        if from_version < 4 {
            Self::migrate_disputes_to_v4(&env)?;
        }
        if from_version < 2 {
            Self::move_cold_config(&env);
        }
        if from_version < 5 {
            Self::migrate_admin_pause_to_v5(&env);
        }

        env.storage()
            .instance()
            .set(&DataKey::SchemaVersion, &SCHEMA_VERSION);
        env.events().publish(
            (symbol_short!("migrate"),),
            MigratedEvent {
                admin,
                from_version,
                to_version: SCHEMA_VERSION,
            },
        );
        Ok(SCHEMA_VERSION)
    }

    /// Assign one of the narrower administrative roles (issue #761) to
    /// `new_address`. Root admin only. Passing the root admin's own address
    /// returns the role to the default (held by the root admin).
    /// Emits `rolechg`.
    ///
    /// # Errors
    /// * `MigrationRequired` – Storage predates this code; call `migrate`.
    /// * `NotInitialized`    – No admin is stored.
    /// * `Unauthorized`      – `admin` is not the root admin.
    /// * `InvalidAddress`    – `new_address` is a zero address or this
    ///   contract.
    pub fn set_admin_role(
        env: Env,
        admin: Address,
        role: AdminRole,
        new_address: Address,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin(&env, &admin)?;
        Self::validate_address(&env, &new_address)?;

        let instance = env.storage().instance();
        let key = DataKey::AdminRole(role);
        let previous: Address = instance.get(&key).unwrap_or(admin.clone());
        if new_address == admin {
            instance.remove(&key);
        } else {
            instance.set(&key, &new_address);
        }

        env.events().publish(
            (symbol_short!("rolechg"),),
            AdminRoleChangedEvent {
                admin,
                role,
                previous,
                new_address,
            },
        );
        Ok(())
    }

    /// The address currently holding `role`: its assignee, or the root admin.
    ///
    /// # Errors
    /// * `NotInitialized` – No admin is stored.
    pub fn get_admin_role(env: Env, role: AdminRole) -> Result<Address, Error> {
        let instance = env.storage().instance();
        match instance.get(&DataKey::AdminRole(role)) {
            Some(holder) => Ok(holder),
            None => instance.get(&DataKey::Admin).ok_or(Error::NotInitialized),
        }
    }

    /// Propose a new admin via the multisig approval workflow.
    ///
    /// The proposal ID is **assigned by the contract** (issue #785): the
    /// persistent `DataKey::AdminProposalCounter` advances by one on every
    /// proposal, so no caller can choose an ID to collide with approvals
    /// already collected elsewhere, and an ID — along with the record and
    /// approvals it consumed — can never be reused.  Both the full proposal
    /// record (`DataKey::AdminProposal(id)`, holding `new_admin`, the
    /// creation timestamp, the `Pending` status, and the approval bitmap) and
    /// the `PendingAdminTransfer` lock/mirror are written atomically.
    ///
    /// # Errors
    /// * `NotInitialized`       – Contract not initialised.
    /// * `Unauthorized`         – Caller is not the stored admin.
    /// * `InvalidAddress`       – `new_admin` is a zero address, the contract
    ///   itself, or the current admin.
    /// * `AdminTransferPending` – A proposal is already pending; execute or
    ///   cancel it before proposing another.
    /// * `ArithmeticOverflow`   – The proposal counter has no representable
    ///   successor (2^32 proposals have been created); nothing is written.
    pub fn propose_admin_transfer(
        env: Env,
        admin: Address,
        new_admin: Address,
    ) -> Result<u32, Error> {
        Self::ensure_migrated(&env)?;
        // Storage-footprint note: `DataKey::Version` and `DataKey::Admin`
        // (both instance) are only ever written together in a single atomic
        // `initialize` call — a failed `initialize` reverts the whole
        // invocation, so one can never be present without the other.
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

        Self::validate_new_admin(&env, &stored_admin, &new_admin)?;

        if env
            .storage()
            .persistent()
            .has(&DataKey::PendingAdminTransfer)
        {
            return Err(Error::AdminTransferPending);
        }

        // Issue #785: assign the ID ourselves.  `checked_add` reports an
        // exhausted counter as a typed error instead of trapping under the
        // release profile's overflow checks, and the counter write lands in
        // the same invocation as the two record writes below, so the assigned
        // ID can never be handed out twice.
        let proposal_id = Self::open_proposal_record(&env, &new_admin)?;
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

        Ok(proposal_id)
    }

    /// Execute a pending admin transfer once its multisig proposal has
    /// reached the configured approval threshold. Any caller may trigger
    /// execution — the safety guarantee comes from the collected signer
    /// approvals, not caller identity — but nothing happens unless the
    /// proposal **record's** approval bitmap (issue #785) reports the
    /// threshold met.  On success the proposal is consumed: the record is
    /// marked `Executed` with its bitmap cleared, so neither the ID nor its
    /// approvals can ever run again.
    ///
    /// # Errors
    /// * `NotInitialized`          – Contract has never been initialised, so
    ///   there is no admin to transfer from.
    /// * `NoPendingAdminTransfer`  – No transfer is currently proposed.
    /// * `InvalidStatus`           – The pending transfer has no proposal
    ///   record (written before issue #785 landed, so its approvals cannot be
    ///   verified — cancel it and re-propose), or the record is not `Pending`.
    /// * `MultiSigThresholdNotMet` – Approvals collected so far are below the
    ///   required threshold.
    pub fn execute_admin_transfer(env: Env) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Precondition guard: reject an uninitialised contract (no admin ever
        // stored, so nothing can be transferred) with `NotInitialized` before
        // any pending-transfer or approval storage is read or written.
        let old_admin = Self::load_admin(&env)?;

        let pending: PendingAdminTransfer = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .ok_or(Error::NoPendingAdminTransfer)?;

        // Issue #785: execution is bound to the proposal record for exactly
        // this pending ID.  The record carries the approvals next to the
        // `new_admin` they authorize installing, so approvals collected for
        // any other proposal can never satisfy this one, and a legacy pending
        // entry with no record (pre-upgrade) cannot be executed at all.
        let mut record: AdminTransferProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AdminProposal(pending.proposal_id))
            .ok_or(Error::InvalidStatus)?;
        if record.status != AdminProposalStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        // Threshold comes from the same consolidated config the approvals were
        // collected against (issue #456); a contract without a multisig
        // regime has no threshold to satisfy.
        let config = Self::load_multisig_config(&env).ok_or(Error::NotInitialized)?;
        let approvals = record.approvals_bitmap.count_ones();
        if approvals < config.1 {
            return Err(Error::MultiSigThresholdNotMet);
        }

        // `propose_admin_transfer` now refuses the current admin (issue #771),
        // but a same-admin proposal recorded before that check shipped may
        // still be pending after an upgrade. Such a proposal does not need to
        // rewrite the Admin ledger entry; it still emits the event and clears
        // the pending proposal.
        if old_admin != pending.new_admin {
            // Single source of truth: instance storage only (#679).
            env.storage()
                .instance()
                .set(&DataKey::Admin, &pending.new_admin);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::PendingAdminTransfer);

        // Consume the proposal: terminal status + cleared bitmap (issue #785).
        // The record itself stays as the tombstone that keeps this ID — and
        // every approval it held — permanently unusable.
        record.status = AdminProposalStatus::Executed;
        record.approvals_bitmap = 0;
        env.storage()
            .persistent()
            .set(&DataKey::AdminProposal(pending.proposal_id), &record);

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
    /// one can be proposed. Only the current admin may cancel.  The proposal
    /// record is consumed alongside the lock — marked `Cancelled` with its
    /// approval bitmap cleared (issue #785) — so the cancelled ID and every
    /// approval it collected are spent and cannot be revived by a later
    /// proposal (IDs are never reused).
    ///
    /// # Errors
    /// * `NotInitialized`         – Contract not initialised.
    /// * `Unauthorized`           – Caller is not the stored admin.
    /// * `NoPendingAdminTransfer` – Nothing is currently pending.
    pub fn cancel_admin_transfer_proposal(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_admin(&env, &admin)?;

        let pending: PendingAdminTransfer = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdminTransfer)
            .ok_or(Error::NoPendingAdminTransfer)?;

        env.storage()
            .persistent()
            .remove(&DataKey::PendingAdminTransfer);

        // Consume the proposal record when one exists (issue #785).  A pending
        // entry written before the record existed has nothing to mark — it is
        // simply cleared here so the admin can re-propose under a fresh,
        // record-backed ID.
        let record_key = DataKey::AdminProposal(pending.proposal_id);
        if let Some(mut record) = env
            .storage()
            .persistent()
            .get::<DataKey, AdminTransferProposal>(&record_key)
        {
            record.status = AdminProposalStatus::Cancelled;
            record.approvals_bitmap = 0;
            env.storage().persistent().set(&record_key, &record);
        }

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
    /// * one `.persistent().get(…)` on `DataKey::PendingAdminTransfer` — the
    ///   payload, resolved exactly once per invocation; and
    /// * at most one `.persistent().has(…)` presence probe on `DataKey::Admin`
    ///   — the initialization guard, and only on the no-proposal path.
    ///
    /// A call that finds a pending proposal therefore touches a single ledger
    /// entry; the `Admin` entry is consulted only to tell "initialized, nothing
    /// pending" apart from "never initialized" (#496).
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

    /// Force-release a locked milestone directly to the freelancer, bypassing
    /// the normal `mark_delivered` → `approve_milestone` flow.
    ///
    /// This is the remedy for an escrow where normal settlement is impossible:
    /// the milestone must be `Disputed` (arbiter is unreachable or timed out),
    /// or the cancel lock must be active (`CancelLock == true`).  Using this
    /// outside those conditions — e.g. on a `Pending` or `Delivered` milestone
    /// that has no open dispute — returns `InvalidStatus`.
    ///
    /// The milestone is moved to `Released` and a full token transfer is
    /// executed to the freelancer.
    ///
    /// # Parameters
    /// * `admin`           – Must hold the `Resolver` role (root admin by default).
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
    /// * `Paused`          – Contract is administratively paused.
    /// * `NotFunded`       – Escrow has not been funded; nothing to release.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`, or
    ///                       it is not in `Disputed` status and no cancel lock
    ///                       is active.
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
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Reject calls while the contract is administratively paused so that
        // a suspended escrow cannot have funds moved through the override path.
        Self::assert_not_paused(&env)?;

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

        // Gate: only intervene when the normal settlement path is genuinely
        // blocked.  The milestone must be Disputed (arbiter is unreachable or
        // the dispute timed out without resolution), or the two-party cancel
        // lock must be active.  Overriding a Pending, Delivered, or
        // PartiallyReleased milestone that has no open dispute bypasses the
        // arbiter entirely and makes the admin a unilateral custodian.
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if milestone.status != MilestoneStatus::Disputed && !cancel_locked {
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
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

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
        Self::pay_freelancer_with_fee(&env, &meta, &token_client, &remaining)?;

        env.events().publish(
            (symbol_short!("admovrls"),),
            AdminOverrideReleaseEvent {
                admin,
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
    /// Use this when normal settlement is impossible: the milestone must be
    /// `Disputed` (arbiter is unreachable or the dispute timed out), or the
    /// two-party cancel lock must be active.  Using this outside those
    /// conditions — e.g. on a `Pending` or `Delivered` milestone that has no
    /// open dispute — returns `InvalidStatus`.
    ///
    /// The milestone is moved to `Refunded` and a full token transfer is
    /// executed back to the client.
    ///
    /// # Parameters
    /// * `admin`           – Must hold the `Resolver` role (root admin by default).
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
    /// * `Paused`          – Contract is paused.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`, or
    ///                       it is not in `Disputed` status and no cancel lock
    ///                       is active.
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
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // Reject illegal source state before any job/milestone ledger I/O.
        Self::assert_not_paused(&env)?;

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

        // Gate: only intervene when the normal settlement path is genuinely
        // blocked.  The milestone must be Disputed (arbiter is unreachable or
        // the dispute timed out without resolution), or the two-party cancel
        // lock must be active.  Overriding a Pending, Delivered, or
        // PartiallyReleased milestone that has no open dispute bypasses the
        // arbiter entirely and makes the admin a unilateral custodian.
        let cancel_locked = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::CancelLock)
            .unwrap_or(false);
        if milestone.status != MilestoneStatus::Disputed && !cancel_locked {
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
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeInfo(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

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
        Self::pay_or_credit(&env, &token_client, &meta.client, &remaining)?;

        env.events().publish(
            (symbol_short!("admovrf"),),
            AdminOverrideRefundEvent {
                admin,
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
    /// * `admin`           – Must hold the `Resolver` role (root admin by default).
    /// * `milestone_index` – Target milestone; must currently be `Disputed`.
    /// * `elapsed_seconds` – Portion of the extension window already elapsed.
    /// * `total_seconds`   – Full length of the extension window.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is neither the `Resolver` delegate nor, if none is set, the
    ///   stored admin.
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
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        let meta = Self::load_job_meta(&env)?;
        Self::require_escrow_has_funding(&env, &meta)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
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

        let split = Self::time_extension_split(&env, remaining, elapsed_seconds, total_seconds)?;

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = Self::available_balance(&env, &token_client);
        if contract_balance < remaining {
            return Err(Error::InsufficientBalance);
        }

        let client_refund = split.second;
        let freelancer_payout = split.first;

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

        env.events().publish(
            (symbol_short!("admstrm"),),
            AdminOverrideStreamingReleaseEvent {
                admin,
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

    /// Pause the escrow, blocking all normal user-facing endpoints.
    ///
    /// After this call, `fund`, `mark_delivered`, `approve_milestone`,
    /// `approve_partial`, `claim_auto_release`, `raise_dispute`, and
    /// `resolve_dispute` all return `EscrowPaused` until the admin calls
    /// `admin_resume_escrow`.  Admin-prefixed endpoints (including this one)
    /// remain fully operational during a pause.
    ///
    /// Calling this on an already-paused escrow is rejected with `AlreadyPaused`
    /// rather than silently succeeding, so an operator cannot mistake a
    /// redundant call for having taken fresh action.
    ///
    /// # Parameters
    /// * `admin` – Must hold the `Operator` role (root admin by default).
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is neither the `Operator` delegate nor, if none is set, the
    ///   stored admin.
    /// * `AlreadyPaused`  – The escrow is already paused.
    /// * `EmergencyPauseInProgress` – A pause/resume transition is already
    ///   in flight; the escrow cannot be re-paused mid-transition.
    pub fn admin_pause_escrow(env: Env, admin: Address) -> Result<(), Error> {
        // Authorization: only the `Operator` delegate (or, if none is set, the
        // stored admin) may pause the escrow.  Any other caller is rejected
        // with `Unauthorized`, and a contract that has never been initialised
        // with `NotInitialized`, before any ledger entry is read or written.
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        // Precondition: reject an illegal source state — an emergency
        // pause/resume transition already in flight — before any write.

        // Reject redundant pause calls so an operator cannot mistake a
        // redundant call for having taken fresh action.
        let already_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if already_paused {
            return Err(Error::AlreadyPaused);
        }

        env.storage().instance().set(&DataKey::Paused, &true);

        env.events()
            .publish((symbol_short!("pause"),), EscrowPausedEvent { admin });

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
    /// * `admin` – Must hold the `Operator` role (root admin by default).
    ///
    /// # Errors
    /// * `NotInitialized`           – Contract has not been initialised.
    /// * `Unauthorized`             – `admin` is neither the `Operator` delegate nor, if none is
    ///   set, the stored admin.
    /// * `NotPaused`                – The escrow is not currently paused.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    pub fn admin_resume_escrow(env: Env, admin: Address) -> Result<(), Error> {
        // Storage-footprint note (issue #449): authorize against the *instance*
        // copy of `DataKey::Admin` via `require_admin_from_instance` instead of
        // probing the persistent copy twice (`has` + `load_admin`'s `get`).
        // `initialize` writes both copies atomically and every admin-transfer
        // path keeps them in sync, so this is the same logical check — but now
        // the admin read and every `Paused` / the former reentrancy guard read and write below land
        // on the single instance ledger entry, reducing the call from two
        // distinct ledger entries touched to one.
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        let currently_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if !currently_paused {
            return Err(Error::NotPaused);
        }

        (|| {
            env.storage().instance().set(&DataKey::Paused, &false);

            env.events().publish(
                (symbol_short!("resume"),),
                EscrowResumedEvent {
                    admin: admin.clone(),
                },
            );

            Ok(())
        })()
    }

}

#[allow(deprecated)]
impl MilestoneEscrow {
    /// Schema 3 → 4: move an admin-set emergency pause (`Ep` set while
    /// `EpParty` is not) onto the single admin pause flag, `Paused`.
    pub(crate) fn migrate_admin_pause_to_v5(env: &Env) {
        let instance = env.storage().instance();
        let ep: bool = instance.get(&DataKey::Ep).unwrap_or(false);
        let ep_party: bool = instance.get(&DataKey::EpParty).unwrap_or(false);
        if ep && !ep_party {
            instance.set(&DataKey::Paused, &true);
            instance.set(&DataKey::Ep, &false);
        }
    }

    /// Schema 1/2 → 3 for milestones: add a zero `spec_hash` (#767) and a
    /// zero `deliverable` to every entry stored without them.
    pub(crate) fn migrate_milestones_to_v3(env: &Env) -> Result<(), Error> {
        let meta = Self::load_job_meta(env)?;
        let persistent = env.storage().persistent();
        let spec_hash_field = Symbol::new(env, "spec_hash");
        let deliverable_field = Symbol::new(env, "deliverable");
        let zero = BytesN::from_array(env, &[0u8; 32]);
        for index in 0..meta.milestone_count {
            let key = DataKey::Milestone(index);
            // Decode the field map first: each older shape only decodes as
            // its own struct.
            let fields: Map<Symbol, Val> = persistent.get(&key).ok_or(Error::InvalidMilestone)?;
            if fields.contains_key(deliverable_field.clone()) {
                continue;
            }
            let migrated = if fields.contains_key(spec_hash_field.clone()) {
                let old: MilestoneV2 = persistent.get(&key).ok_or(Error::InvalidMilestone)?;
                Milestone {
                    amount: old.amount,
                    released_amount: old.released_amount,
                    status: old.status,
                    delivered_at: old.delivered_at,
                    funded: old.funded,
                    spec_hash: old.spec_hash,
                    deliverable: zero.clone(),
                }
            } else {
                let old: MilestoneV1 = persistent.get(&key).ok_or(Error::InvalidMilestone)?;
                Milestone {
                    amount: old.amount,
                    released_amount: old.released_amount,
                    status: old.status,
                    delivered_at: old.delivered_at,
                    funded: old.funded,
                    spec_hash: zero.clone(),
                    deliverable: zero.clone(),
                }
            };
            persistent.set(&key, &migrated);
        }
        Ok(())
    }

    /// Schema 3 → 4 for disputes: add `raised_by` (the job's client, as a
    /// placeholder) to every open `DisputeInfo` stored without it (#997).
    /// Records that already carry the field are left untouched.
    pub(crate) fn migrate_disputes_to_v4(env: &Env) -> Result<(), Error> {
        let meta = Self::load_job_meta(env)?;
        let persistent = env.storage().persistent();
        let raised_by_field = Symbol::new(env, "raised_by");
        for index in 0..meta.milestone_count {
            let key = DataKey::DisputeInfo(index);
            let Some(fields) = persistent.get::<_, Map<Symbol, Val>>(&key) else {
                continue;
            };
            if fields.contains_key(raised_by_field.clone()) {
                continue;
            }
            let old: DisputeInfoV3 = persistent.get(&key).ok_or(Error::InvalidStatus)?;
            persistent.set(
                &key,
                &DisputeInfo {
                    disputed_at: old.disputed_at,
                    previous_status: old.previous_status,
                    reason_hash: old.reason_hash,
                    evidence: old.evidence,
                    raised_by: meta.client.clone(),
                },
            );
        }
        Ok(())
    }

    /// Move `WhitelistedTokens`, `MultiSigConfig`, `InterestYieldState` and
    /// `PlatformFeeAllocation` out of the contract-instance entry into
    /// persistent keys, folding a pre-#456 `MultiSigSigners` /
    /// `MultiSigThreshold` pair into `MultiSigConfig` (issue #772).  Run by
    /// `migrate` for the `1 → 2` step.  Returns the number of instance entries
    /// that were moved or folded.
    pub(crate) fn move_cold_config(env: &Env) -> u32 {
        let mut moved = 0u32;
        if Self::migrate_cold_entry::<Vec<Address>>(env, &DataKey::WhitelistedTokens) {
            moved += 1;
        }
        if Self::migrate_cold_entry::<MultiSigConfig>(env, &DataKey::MultiSigConfig) {
            moved += 1;
        }
        if Self::migrate_cold_entry::<EscrowInterestYieldState>(env, &DataKey::InterestYieldState) {
            moved += 1;
        }
        if Self::migrate_cold_entry::<PlatformFeeAllocation>(env, &DataKey::PlatformFeeAllocation) {
            moved += 1;
        }

        let instance = env.storage().instance();
        let legacy_signers: Option<Vec<Address>> = instance.get(&DataKey::MultiSigSigners);
        let legacy_threshold: Option<u32> = instance.get(&DataKey::MultiSigThreshold);
        if let (Some(signers), Some(threshold)) = (legacy_signers, legacy_threshold) {
            let persistent = env.storage().persistent();
            if !persistent.has(&DataKey::MultiSigConfig) {
                persistent.set(
                    &DataKey::MultiSigConfig,
                    &MultiSigConfig(signers, threshold),
                );
            }
            instance.remove(&DataKey::MultiSigSigners);
            instance.remove(&DataKey::MultiSigThreshold);
            moved += 1;
        }

        moved
    }

    /// Assign the next proposal ID and open its `Pending` record with an
    /// empty approval bitmap (issue #785).  `checked_add` reports an exhausted
    /// counter as `ArithmeticOverflow`, and the counter and record are written
    /// in the same invocation, so an ID is never handed out twice.  `subject`
    /// is stored as the record's `new_admin`: the incoming admin for a
    /// transfer, the proposing admin for a signer-set change.
    pub(crate) fn open_proposal_record(env: &Env, subject: &Address) -> Result<u32, Error> {
        let persistent = env.storage().persistent();
        let proposal_id: u32 = persistent
            .get(&DataKey::AdminProposalCounter)
            .unwrap_or(0u32)
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;
        persistent.set(&DataKey::AdminProposalCounter, &proposal_id);
        persistent.set(
            &DataKey::AdminProposal(proposal_id),
            &AdminTransferProposal {
                proposal_id,
                new_admin: subject.clone(),
                created_at: env.ledger().timestamp(),
                status: AdminProposalStatus::Pending,
                approvals_bitmap: 0,
            },
        );
        Ok(proposal_id)
    }

}
