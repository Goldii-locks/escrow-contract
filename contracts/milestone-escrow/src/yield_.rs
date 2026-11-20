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
    /// Estimate the interest yield that would accrue on an escrowed balance
    /// over a given duration, using a simple-interest model.
    ///
    /// # Authorization and preconditions
    ///
    /// The guard block runs at the top of this function, before any
    /// calculation or event publish, so an unauthorized or uninitialized call
    /// is rejected before any work is done:
    ///
    /// 1. **Authorization** — `admin.require_auth()` inside `require_admin`
    ///    makes the host reject a missing signature before the contract body
    ///    executes. The stored-admin equality check then rejects a
    ///    validly-signed non-admin with `Unauthorized`.
    /// 2. **Initialized state** — `require_initialized` ensures the contract
    ///    has been set up, so the estimator is always anchored to a real job.
    ///
    /// # Parameters
    /// * `admin`             – Caller; must hold the `FeeManager` role (root admin by default).
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
    /// * `NotInitialized` – Contract has never been initialised.
    /// * `Unauthorized`    – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidAmount`   – Zero/negative principal, rate, or duration, or an
    ///                       intermediate checked multiplication overflows.
    /// * `InvalidRatio`   – `annual_rate_bps` exceeds 10_000 (unsupported).
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn escrow_interest_yield(
        env: Env,
        admin: Address,
        principal: i128,
        annual_rate_bps: i128,
        duration_seconds: i128,
    ) -> Result<i128, Error> {
        // Authorization: admin.require_auth() runs inside require_role, so a
        // transaction missing the admin signature is rejected by the host
        // before this body executes at all. The role check then rejects a
        // validly-signed caller that is neither the `FeeManager` delegate nor,
        // if none is set, the stored admin. Both guards run before any
        // calculation or event publish, so an unauthorized caller learns
        // nothing about the configuration.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        // Precondition: the escrow must be initialized so the estimator is
        // anchored to a real job rather than a fresh or uninitialized instance.
        Self::require_initialized(&env)?;

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

        Ok(yield_amount)
    }

    /// Initialize or update interest/yield share configuration (unlocked by
    /// default on first write). Rejects invalid share totals and modifications
    /// while an execution lock is held.
    ///
    /// This is the admin-only path, so it is only open **before the escrow is
    /// funded** (issue #776). Once the client has deposited, the split is part
    /// of the parties' economics and can only change through
    /// `set_interest_yield_consent`, which requires the client and freelancer
    /// to sign as well.
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
    /// 2. **Illegal source state** — a funded escrow is refused with
    ///    `AlreadyFunded` (the admin may not move yield between the parties
    ///    after they committed funds), then `ensure_interest_yield_writable`
    ///    rejects a locked configuration with `EscrowLocked`.  This runs *before* the
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
    /// The admin check and the funded check both read the contract-instance
    /// entry (`require_admin_from_instance`, `DataKey::Job`), so together they
    /// add a single ledger entry. `InterestYieldState` is cold configuration in
    /// persistent storage (issue #772); the precondition guard issues exactly
    /// one read of it before the single write.
    ///
    /// # Errors
    /// Listed in evaluation order; a call that violates more than one guard
    /// fails with the first match.
    /// * `NotInitialized` – Contract admin key is missing.
    /// * `Unauthorized`   – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `AlreadyFunded`  – The escrow has been funded; use
    ///   `set_interest_yield_consent` instead.
    /// * `EscrowLocked`   – Configuration is locked for execution.
    /// * `InvalidRatio`   – Shares do not sum to exactly 10_000 bps.
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn set_escrow_interest_yield(
        env: Env,
        admin: Address,
        client_share_bps: u32,
        freelancer_share_bps: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Authorization: `admin.require_auth()` inside the helper makes the
        // host reject a missing signature before this body runs at all; the
        // equality check then rejects a validly-signed non-admin.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        // Illegal source state: after funding the split is the parties'
        // economics, so the admin alone may no longer change it (issue #776).
        // `Job` lives in instance storage, the same entry as the admin read.
        let funded = env
            .storage()
            .instance()
            .get::<_, JobMeta>(&DataKey::Job)
            .is_some_and(|meta| meta.funded);
        if funded {
            return Err(Error::AlreadyFunded);
        }

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
    /// `set_escrow_interest_yield` lets the platform admin set the split alone,
    /// but only before funding. After funding this is the only way to change
    /// it, so the parties' economics cannot be rewritten without them: a compromised or malicious admin key alone cannot move
    /// funds between client and freelancer here, because `client.require_auth()`
    /// and `freelancer.require_auth()` must both succeed. If either party's
    /// signature is missing from the transaction, the host-level auth check
    /// panics before any state is touched — a single-signature attempt never
    /// reaches the validation logic below, let alone mutates storage.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract admin key or job metadata missing.
    /// * `Unauthorized`   – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidRatio`   – Shares do not sum to exactly 10 000 bps.
    /// * `EscrowLocked`   – Configuration is locked for execution.
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn set_interest_yield_consent(
        env: Env,
        admin: Address,
        client_share_bps: u32,
        freelancer_share_bps: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        let meta = Self::load_job_meta(&env)?;

        // Both parties must independently sign this transaction. A missing
        // signature causes require_auth to panic at the host level.
        meta.client.require_auth();
        meta.freelancer.require_auth();

        Self::validate_interest_yield_share_config(client_share_bps, freelancer_share_bps)?;

        // Consolidate storage reads: load state once and check lock status
        // instead of calling has() followed by load_interest_yield_state().
        if let Ok(state) = Self::load_interest_yield_state(&env) {
            if state.locked {
                return Err(Error::EscrowLocked);
            }
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
    /// and `interest_yield_split_refund` are all rejected until the admin, the
    /// client and the freelancer jointly call `unlock_escrow_interest_yield`.
    ///
    /// A `yldlock` / `EscrowInterestYieldLockedEvent` is published once the new
    /// state is durable so indexers and auditors get an immutable record of the
    /// lock without polling storage. The event mirrors the persisted state
    /// field-for-field: the two BPS values are the shares the lock froze and
    /// `locked` is `true`.
    ///
    /// ## Guards (issue #464)
    ///
    /// Both guards run before this function performs its single ledger write, in
    /// this order:
    ///
    /// 1. **Authorization** — `require_admin_from_instance` performs
    ///    `admin.require_auth()` (so a transaction that omits the admin
    ///    signature is rejected by the host before the contract body executes)
    ///    and then compares against the stored admin.  Being first, an
    ///    unauthorized caller learns nothing about the lock: it cannot
    ///    distinguish “already locked” from “not configured” on an escrow it
    ///    is not allowed to administer.
    /// 2. **Illegal source state** — an absent `InterestYieldState` entry is
    ///    `NotInitialized`, and a configuration that is *already* locked is
    ///    `InvalidStatus`.  Re-locking is refused rather than silently
    ///    no-opping, so an operator (or an indexer replaying the call) can never
    ///    mistake a redundant call for one that took fresh action.  This mirrors
    ///    the guard `unlock_escrow_interest_yield` applies to unlocking an
    ///    unlocked configuration, and is why this endpoint cannot reuse
    ///    `ensure_interest_yield_writable` — that helper deliberately maps an
    ///    absent state to “writable” so the first configuration write can
    ///    create it, whereas `lock` has no create path.
    ///
    /// Because the guards are exhaustive, the only way to reach the write is with
    /// an authenticated admin and a configured-but-unlocked configuration; every
    /// rejection path returns above the write and therefore leaves the ledger
    /// unchanged.
    ///
    /// ## Storage-footprint note
    ///
    /// Like `set_escrow_interest_yield`, this function uses
    /// `require_admin_from_instance` rather than the standard `require_admin`
    /// helper, so the admin verification read lands on the contract-instance
    /// entry every call loads anyway rather than adding the persistent `Admin`
    /// entry; the `InterestYieldState` read/write is on its own persistent key
    /// (issue #772).
    ///
    /// # Errors
    /// Listed in evaluation order; a call that violates more than one guard
    /// fails with the first match.
    /// * `NotInitialized` – Contract admin key or interest/yield state missing.
    /// * `Unauthorized`    – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidStatus`   – The interest/yield configuration is already locked.
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn lock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Guard 1 — authorization. `admin.require_auth()` inside the helper makes
        // the host reject a missing signature before this body runs at all; the
        // equality check then rejects a validly-signed non-admin.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

        // Guard 2 — illegal source state. One read covers both cases: a missing
        // entry is `NotInitialized`, and an entry that is already locked is the
        // illegal source state this endpoint must refuse before writing.
        let mut state = Self::load_interest_yield_state(&env)?;
        if state.locked {
            return Err(Error::InvalidStatus);
        }

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
    /// Unlocking requires the same consent as changing the shares through
    /// `set_interest_yield_consent`: the admin **and** both the client and the
    /// freelancer must sign (issue #776). Otherwise the admin could undo its own
    /// lock at will and the lock would commit to nothing.
    ///
    /// # Errors
    /// * `NotInitialized` - Contract admin key, job metadata or interest/yield
    ///   state missing.
    /// * `Unauthorized` - `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidStatus` - Interest/yield lock is not active (already unlocked).
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn unlock_escrow_interest_yield(env: Env, admin: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
        Self::require_client_and_freelancer_consent(&env)?;
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
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
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
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn interest_yield_split_refund(
        env: Env,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // Precondition: reject illegal source state if contract is paused.
        Self::assert_not_paused(&env)?;

        // Precondition: reject illegal source state if interest yield state is locked.
        if Self::cold_has(&env, &DataKey::InterestYieldState) {
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

        Ok(allocation)
    }

    /// Return the stored interest/yield share configuration.
    ///
    /// # Read-only
    /// This is a pure query over instance storage: it delegates to
    /// `Self::load_interest_yield_state`, which performs exactly one `get` of
    /// `DataKey::InterestYieldState`. It must never `set`, `remove`, or
    /// `extend_ttl` any instance, persistent, or temporary entry, and must
    /// never publish an event — a read that wrote state could silently rewrite
    /// the ledger or shorten the TTL of the entry it inspects. It requires no
    /// authorization and is safe to call while the escrow is paused.
    ///
    /// INVARIANT: `get_escrow_interest_yield_no_mutation_tests` takes a full
    /// `Env::to_ledger_snapshot` immediately before and after the call — plus a
    /// per-tier fingerprint of instance, persistent, and temporary storage —
    /// and asserts the ledger is byte-identical, so any future edit that adds
    /// `.set(`, `.remove(`, `.extend_ttl(`, or `env.events().publish(` here or
    /// in the shared `load_interest_yield_state` helper fails CI. Do not add
    /// storage-writing calls to either function.
    ///
    /// # Returns
    /// * `Ok(EscrowInterestYieldState)` – the configuration last written by
    ///   `set_escrow_interest_yield` / `set_interest_yield_consent`, with
    ///   `locked` as last set by `lock_escrow_interest_yield` /
    ///   `unlock_escrow_interest_yield`. The two share fields satisfy
    ///   `client_share_bps + freelancer_share_bps == BPS_SCALE` (10_000),
    ///   because both writers validate that before storing.
    ///
    /// # Errors
    /// * `NotInitialized` – Configuration has never been set.
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn get_escrow_interest_yield(env: Env) -> Result<EscrowInterestYieldState, Error> {
        Self::load_interest_yield_state(&env)
    }

    /// Set the annual yield rate for the escrow in basis points (1 bp = 0.01 %).
    ///
    /// # Parameters
    /// * `admin`       – Must hold the `FeeManager` role (root admin by default);
    ///                   a fresh signature is required on every call.
    /// * `rate_bps`    – New annual rate.  Capped at 10 000 (= 100 %).
    ///                   Pass `0` to disable yield accrual.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised yet (job metadata
    ///                      absent); checked before any auth or storage access.
    /// * `Paused`         – Contract is currently paused; yield-rate mutations
    ///                      are rejected while operations are suspended.
    /// * `Unauthorized`   – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidRatio`   – `rate_bps` exceeds 10 000.
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn admin_set_yield_rate(env: Env, admin: Address, rate_bps: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Precondition 1: contract must be fully initialised (job metadata
        // must exist).  This check runs before any auth or storage mutation so
        // that callers on an uninitialised contract receive `NotInitialized`
        // rather than a less informative error.
        Self::load_job_meta(&env)?;

        // Precondition 2: reject calls while the contract is paused.  The
        // yield-rate is part of the active financial configuration; mutating
        // it while operations are suspended could silently affect the next
        // accrual cycle once the pause is lifted.
        Self::assert_not_paused(&env)?;

        // Authorization: `require_role` performs `admin.require_auth()` plus
        // the `FeeManager` role check.  Placed after the stateless
        // preconditions so that an unauthorized caller on an uninitialised or
        // paused contract receives the precondition error, not Unauthorized,
        // which would leak information about the admin key.
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;
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
    /// * `admin`           – Must hold the `FeeManager` role (root admin by default).
    /// * `milestone_index` – Index of the milestone to which yield is attributed.
    /// * `accrued_amount`  – Interest amount to book; must be > 0.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is neither the `FeeManager` delegate nor, if none is set, the
    ///   stored admin.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidAmount`   – `accrued_amount` ≤ 0 or the running total would
    ///                       overflow `i128`.
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
    pub fn admin_accrue_yield(
        env: Env,
        admin: Address,
        milestone_index: u32,
        accrued_amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        // Authorize from instance storage so the admin read shares a ledger
        // entry with the `DataKey::Job` read below (see the rustdoc note).
        Self::require_role(&env, &admin, AdminRole::FeeManager)?;

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

    /// Return a snapshot of the current yield and pause state.
    ///
    /// All fields are safe to call even before any admin has set a yield rate
    /// (defaults to zero) or paused the contract (defaults to `false`).
    ///
    /// # Read-only
    /// This is a pure query: it only *reads* the persistent `DataKey::YieldConfig`
    /// and `DataKey::YieldAccrued` entries and the instance pause flags. It must never `set`, `remove`, or `extend_ttl` any instance,
    /// persistent, or temporary entry, and must never publish an event — a
    /// read that wrote state could silently rewrite the ledger or shorten the
    /// TTL of the entries it inspects. `get_yield_info_no_mutation_tests` pins
    /// that guarantee by taking a full `Env::to_ledger_snapshot` immediately
    /// before and after the call and asserting the two are byte-identical.
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
    /// Kept only for the legacy unit-test suite; the yield subsystem is not
    /// part of the shipped ABI (issue #693).
    #[cfg(test)]
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

        let is_paused = Self::is_paused(&env);

        Ok((rate_bps, total_accrued, is_paused))
    }

}
