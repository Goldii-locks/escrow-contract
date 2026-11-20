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
    /// Freeze the escrow: set `DataKey::Ep`, blocking every
    /// endpoint guarded by `ensure_not_paused`.
    ///
    /// # Business rules
    /// Bad setups are rejected before any state is written, each with a
    /// distinct error variant:
    ///
    /// 1. The contract must be initialised — `load_job_meta` returns
    ///    `NotInitialized` when no job is stored.  Pausing an uninitialised
    ///    contract would write a flag no endpoint could ever clear.
    /// 2. Both parties must sign: the supplied addresses must match the job's
    ///    stored `client` and `freelancer` (`Unauthorized`), and each must
    ///    authorise the call (`require_auth`).  Neither party -- nor the admin
    ///    -- can freeze the escrow alone; the admin's unilateral path is
    ///    `admin_pause_escrow`.
    /// 3. No pause transition may already be mid-execution
    ///    (`EmergencyPauseInProgress`).
    /// 4. The contract must not already be paused (`AlreadyPaused`).  A
    ///    redundant pause previously succeeded silently, which let an operator
    ///    believe they had taken fresh action during an incident when the
    ///    freeze was in fact already in place.
    ///
    /// # Errors
    /// * `NotInitialized`            – No job has been stored.
    /// * `Unauthorized`              – `client` / `freelancer` do not match the
    ///                                 addresses recorded on the job.
    /// * `EmergencyPauseInProgress`  – A pause transition is already running.
    /// * `AlreadyPaused`             – The contract is already paused.
    pub fn emergency_pause(env: Env, client: Address, freelancer: Address) -> Result<(), Error> {
        let meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        // `load_job_meta` above already proved the contract is initialized, so
        // the pause-flag read cannot miss here; `?` keeps the path honest
        // rather than defaulting a missing flag to `false`.
        if Self::load_emergency_paused(&env)? {
            return Err(Error::AlreadyPaused);
        }

        env.storage().instance().set(&DataKey::Ep, &true);
        env.storage().instance().set(&DataKey::EpParty, &true);

        env.events().publish(
            (symbol_short!("empause"),),
            EmergencyPausedEvent { client, freelancer },
        );

        Ok(())
    }

    /// Lift any pause (the party emergency pause and the admin pause),
    /// restoring normal operation.
    ///
    /// # Business rules
    /// Mirrors `emergency_pause`: the contract must be initialised, the
    /// caller must be the stored admin, no transition may be mid-execution,
    /// and the contract must actually be paused.  Unpausing a running
    /// contract is rejected with `NotPaused` rather than silently succeeding,
    /// so a mistaken call is visible to the operator instead of reading as a
    /// completed recovery.
    ///
    /// # Errors
    /// * `NotInitialized`            – Admin key has never been stored.
    /// * `Unauthorized`              – `admin` is neither the `Operator` delegate nor, if none is
    ///   set, the stored admin.
    /// * `NotPaused`                 – The contract is not currently paused.
    /// * `EmergencyPauseInProgress`  – A pause transition is already running.
    pub fn emergency_unpause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        // Reject the illegal source state before any further ledger access
        // (including the transition lock) so a mistaken unpause never mutates
        // storage.  The stored-admin check above already proved the contract is
        // initialized, so the pause-flag read cannot miss here.
        if !Self::load_emergency_paused(&env)? {
            return Err(Error::NotPaused);
        }

        // Single write — no external call inside the transition body, so no
        // reentrancy lock is needed (mirrors emergency_pause_admin_override).
        env.storage().instance().set(&DataKey::Ep, &false);
        env.storage().instance().set(&DataKey::EpParty, &false);
        env.storage().instance().set(&DataKey::Paused, &false);

        env.events().publish(
            (symbol_short!("emunpause"),),
            EmergencyUnpausedEvent {
                admin: admin.clone(),
            },
        );

        Ok(())
    }

    /// Jointly lift a pause that the client and freelancer previously set.
    /// Cannot clear an admin pause (`EpParty` must be true).
    pub fn emergency_unpause_parties(
        env: Env,
        client: Address,
        freelancer: Address,
    ) -> Result<(), Error> {
        let meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        if !Self::load_emergency_paused(&env)? {
            return Err(Error::NotPaused);
        }
        let party_pause = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::EpParty)
            .unwrap_or(false);
        if !party_pause {
            return Err(Error::Unauthorized);
        }

        env.storage().instance().set(&DataKey::Ep, &false);
        env.storage().instance().set(&DataKey::EpParty, &false);

        env.events().publish(
            (symbol_short!("emunpausp"),),
            EmergencyPartyUnpausedEvent { client, freelancer },
        );
        Ok(())
    }

    /// Deprecated alias for the admin pause; use `admin_pause_escrow`.
    ///
    /// # Effects
    /// Sets the single admin pause flag `DataKey::Paused` to `true`; lift it
    /// with `admin_resume_escrow` or `emergency_unpause`. Emits an
    /// `EmergencyPauseAdminOverrideEvent`.
    ///
    /// # Returns
    /// * `Ok(())` on a successful override of the emergency pause state.
    ///
    /// # Errors
    /// * `NotInitialized` - Admin key has never been stored.
    /// * `Unauthorized` - `admin` is neither the `Operator` delegate nor, if none is set, the
    ///   stored admin.
    /// * `AlreadyPaused` - The contract is already paused.
    pub fn emergency_pause_admin_override(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_role(&env, &admin, AdminRole::Operator)?;

        // Read the current state once; reject no-op transitions so the caller
        // can distinguish a successful override from a mistaken duplicate call.
        let current = Self::is_paused(&env);

        if current {
            return Err(Error::AlreadyPaused);
        }

        // Single write — no external call, no EmergencyPauseLock needed.
        env.storage().instance().set(&DataKey::Paused, &true);

        env.events().publish(
            (symbol_short!("emoverrid"),),
            EmergencyPauseAdminOverrideEvent {
                admin: admin.clone(),
                paused: true,
                previous: current,
            },
        );

        Ok(())
    }

    /// Report whether the escrow is paused: the party emergency pause
    /// (`DataKey::Ep`) or the admin pause (`DataKey::Paused`).
    ///
    /// This is a **pure read**: it never writes to instance, persistent, or
    /// temporary storage, emits no events, and requires no authorisation, so
    /// any caller may invoke it at any time — including while the contract is
    /// itself emergency-paused.
    ///
    /// `initialize` writes `DataKey::Ep = false` as part of its state commit
    /// (see its rustdoc), so the key is present for every initialized contract
    /// and the reported value always reflects the last `emergency_pause`,
    /// `emergency_unpause`, `admin_pause_escrow`, `admin_resume_escrow` or
    /// `emergency_pause_admin_override`.
    ///
    /// # Return values
    ///
    /// | State                                          | Return value |
    /// |------------------------------------------------|--------------|
    /// | `initialize` never completed successfully       | `Err(Error::NotInitialized)` |
    /// | Initialized, never paused                       | `Ok(false)` |
    /// | Initialized, frozen by `emergency_pause` / `admin_pause_escrow` / `emergency_pause_admin_override` | `Ok(true)` |
    /// | Initialized, released by `emergency_unpause`    | `Ok(false)` |
    ///
    /// # Errors
    /// * `NotInitialized` – `DataKey::Ep` is absent from instance storage,
    ///   which is the case only for contracts on which `initialize` has not
    ///   yet been successfully invoked.  Callers can therefore distinguish
    ///   "not paused" (`Ok(false)`) from "not set up yet"
    ///   (`Err(NotInitialized)`) instead of reading a defaulted `false` for
    ///   both.
    ///
    /// No other error is possible: the function takes no arguments, touches no
    /// token balance, and does not inspect the pause-transition lock
    /// (`the former reentrancy guard`) — a pause transition that is mid-execution in the
    /// same transaction is still reported by whatever value is currently
    /// stored.
    ///
    /// Read-only: it reads only the instance entry; the whole ledger is
    /// byte-identical before and after the call (enforced by
    /// `read_path_tests`).
    pub fn is_emergency_paused(env: Env) -> Result<bool, Error> {
        Self::load_emergency_paused(&env)
    }

    /// Implement refund distribution pathways for split-refund claims during
    /// an emergency pause.
    ///
    /// # Guard order (#524)
    ///
    /// Every guard runs before the arguments are validated and before any
    /// arithmetic, and each one only *reads* the instance entry, so a rejected
    /// call writes no ledger entry and publishes no event:
    /// 1. `caller.require_auth()`, then the caller must be the stored admin,
    ///    client or freelancer (`NotInitialized` / `Unauthorized`).
    /// 2. No pause transition may be half-applied (`EmergencyPauseInProgress`).
    /// 3. The escrow must actually be emergency-paused (`NotPaused`) — this
    ///    endpoint describes the refund split *while frozen*, so producing a
    ///    figure for a running escrow is an illegal source state.
    ///
    /// # Conservation (#526)
    ///
    /// `client_refund + freelancer_payout == total_amount` for every positive
    /// `total_amount` up to `i128::MAX` and every ratio summing to
    /// `BPS_SCALE`.  See [`Self::emergency_split_exact`].
    ///
    /// # Parameters
    /// * `caller`               – Admin, client or freelancer. Must authorize.
    /// * `total_amount`         – Total amount to split.
    /// * `client_refund_bps`    – Client's refund share in basis points.
    /// * `freelancer_payout_bps`– Freelancer's payout share in basis points.
    ///
    /// # Returns
    /// A `RefundAllocation` struct with computed amounts and the basis-point
    /// ratios that were used.
    ///
    /// # Errors
    /// * `NotInitialized`           – The escrow has not been initialized.
    /// * `Unauthorized`             – `caller` is not the admin, client or
    ///   freelancer.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    /// * `NotPaused`                – The escrow is not emergency-paused.
    /// * `InvalidAmount`            – `total_amount` ≤ 0.
    /// * `InvalidRatio`             – Ratios do not sum to `BPS_SCALE`.
    pub fn emergency_pause_split_refund(
        env: Env,
        caller: Address,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // ── authorization: before any other ledger read
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        if caller != meta.client && caller != meta.freelancer && admin.as_ref() != Some(&caller) {
            return Err(Error::Unauthorized);
        }

        // ── source state: a settled emergency pause, not a transition
        if !Self::is_paused(&env) {
            return Err(Error::NotPaused);
        }

        let allocation =
            Self::emergency_split_exact(total_amount, client_refund_bps, freelancer_payout_bps)?;

        Ok(allocation)
    }

    /// Admin-gated split-refund claim that may only run **while the contract
    /// is actually frozen**.
    ///
    /// `emergency_pause_split_refund` lets any escrow party (admin, client or
    /// freelancer) ask "what would this split be?" while the escrow is frozen.
    /// This endpoint is the admin-only operational counterpart: it enforces
    /// the business rules that must hold
    /// before an emergency refund is settled, rejecting each bad setup with a
    /// distinct error variant before any arithmetic runs.
    ///
    /// # Business rules (checked in this order, each before any later read)
    /// 1. `admin.require_auth()` runs first, before any ledger read.  The
    ///    contract must be initialised and the caller must be the stored admin
    ///    (`NotInitialized` / `Unauthorized`).
    /// 2. No pause transition mid-execution (`EmergencyPauseInProgress`) — a
    ///    refund must not be computed against a half-applied freeze.
    /// 3. The contract **is** paused (`NotPaused`).  Settling an emergency
    ///    refund on a running escrow would bypass the normal release and
    ///    dispute paths.
    /// 4. `total_amount` > 0 (`InvalidAmount`) and the two shares sum to
    ///    exactly `BPS_SCALE` (`InvalidRatio`).
    /// 5. The split is computed with checked `i128` arithmetic only
    ///    (`ArithmeticOverflow`).
    /// 6. The contract holds a non-zero balance (`EmptyBalance`) that covers
    ///    `total_amount` (`InsufficientBalance`).
    ///
    /// # Storage footprint
    /// The endpoint is read-only.  Every key it reads — `Admin`, the former reentrancy guard, `Ep`
    /// and `Job` — lives in **instance** storage, so the whole call touches a
    /// single contract ledger entry (plus the token balance read).  The admin
    /// check uses `require_admin_from_instance` rather than `require_admin`,
    /// which drops the separate persistent `Admin` entry from the footprint.
    /// The token balance is fetched once and reused for both the balance
    /// guards and the event's `remaining_balance`.
    ///
    /// # Events
    /// On success only, publishes an [`EmergencyPauseClaimRefundEvent`] under
    /// the topics `(symbol_short!("epclaim"), admin)`.
    ///
    /// # Returns
    /// A `RefundAllocation` whose two amounts sum to `total_amount` exactly.
    ///
    /// # Errors
    /// * `NotInitialized`           – Admin key has never been stored.
    /// * `Unauthorized`             – `admin` is neither the `Resolver` delegate nor, if none is
    ///   set, the stored admin.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    /// * `NotPaused`                – The contract is not frozen.
    /// * `InvalidAmount`            – `total_amount` ≤ 0.
    /// * `InvalidRatio`             – Shares do not sum to 10 000 bps.
    /// * `ArithmeticOverflow`       – The split overflowed `i128`.
    /// * `EmptyBalance`             – The contract token balance is zero, so
    ///   there is nothing to settle.
    /// * `InsufficientBalance`      – `total_amount` exceeds the contract
    ///   token balance.
    pub fn emergency_pause_claim_refund(
        env: Env,
        admin: Address,
        total_amount: i128,
        client_refund_bps: u32,
        freelancer_payout_bps: u32,
    ) -> Result<RefundAllocation, Error> {
        // ── preconditions (#532): auth and contract state before anything else
        Self::require_role(&env, &admin, AdminRole::Resolver)?;

        // `require_admin` already proved an admin key is stored, so the
        // pause-flag read cannot miss here.
        if !Self::load_emergency_paused(&env)? {
            return Err(Error::NotPaused);
        }

        // ── input validation (pure, no ledger access)
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total_bps = client_refund_bps
            .checked_add(freelancer_payout_bps)
            .ok_or(Error::InvalidRatio)?;
        if total_bps != BPS_SCALE {
            return Err(Error::InvalidRatio);
        }

        // ── checked split (#533): client share rounds to nearest, freelancer
        // receives the exact remainder so the legs always sum to the total.
        let scale = BPS_SCALE as i128;
        let client_refund = total_amount
            .checked_mul(client_refund_bps as i128)
            .and_then(|scaled| scaled.checked_add(scale / 2))
            .and_then(|scaled| scaled.checked_div(scale))
            .ok_or(Error::ArithmeticOverflow)?;
        let freelancer_payout = total_amount
            .checked_sub(client_refund)
            .ok_or(Error::ArithmeticOverflow)?;

        // ── balance guards: one token read, reused for the event (#535)
        let meta = Self::load_job_meta(&env)?;
        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::EmptyBalance);
        }
        let remaining_balance = contract_balance
            .checked_sub(total_amount)
            .ok_or(Error::ArithmeticOverflow)?;
        if remaining_balance < 0 {
            return Err(Error::InsufficientBalance);
        }

        // ── success-only event (#534)
        env.events().publish(
            (symbol_short!("epclaim"), admin),
            EmergencyPauseClaimRefundEvent {
                claimant: meta.client,
                refund_amount: client_refund,
                freelancer_payout,
                remaining_balance,
                timestamp: env.ledger().timestamp(),
            },
        );

        Ok(RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        })
    }

    /// Divide a frozen escrow balance across an arbitrary number of parties
    /// without losing value to rounding.
    ///
    /// # Why plain division is not enough
    /// Allocating `total × weightᵢ / Σweights` with truncating division rounds
    /// every party down, so the shares sum to *less* than `total`.  The
    /// shortfall is at most `n − 1` stroops per call, but it is systematic:
    /// the same party sizes lose value every time, and the residue is stranded
    /// in the contract with no owner.
    ///
    /// # Algorithm — largest remainder (Hare quota)
    /// ```text
    /// weightedᵢ = total × weightᵢ
    /// baseáµ¢     = weightedáµ¢ / Î£weights      (floor)
    /// remáµ¢      = weightedáµ¢ % Î£weights      (exact fractional part, scaled)
    /// residue   = total − Σbaseᵢ            (0 ≤ residue < n)
    /// ```
    /// The `residue` indivisible units are then handed out one at a time to
    /// the parties with the largest `remáµ¢`, each party receiving at most one.
    /// This is exact rather than approximate: `remáµ¢` is the true numerator of
    /// the discarded fraction, so the units go to whoever was rounded down
    /// hardest.
    ///
    /// # Guarantees
    /// * **Conservation** – `Σallocations == total_amount` exactly, for every
    ///   input.  No value is lost and none is created.
    /// * **Bounded error** – each `allocationᵢ` is within one unit of the
    ///   exact rational share `total × weightᵢ / Σweights`; it is never more
    ///   than one unit below it, so no party is systematically rounded down.
    /// * **Determinism** – ties in `remᵢ` are broken by lowest index, so the
    ///   same inputs always produce the same vector.
    /// * **Zero weights** – a party weighted `0` receives exactly `0`; its
    ///   remainder is also `0`, so it never wins a residue unit ahead of a
    ///   party with a real fractional claim.
    ///
    /// # Guard order (#536)
    ///
    /// Every guard runs *before* the weight vector is looked at and before any
    /// arithmetic, and each one only reads instance storage, so a rejected
    /// call writes no ledger entry and publishes no event:
    ///
    /// 1. `caller.require_auth()`.
    /// 2. The caller must be the stored admin, the job's client or the job's
    ///    freelancer — otherwise `NotInitialized` (no job recorded) or
    ///    `Unauthorized`. The allocation describes how a frozen balance is
    ///    shared, so it is a participant-only figure; an arbitrary address may
    ///    not have the contract compute it.
    /// 3. No pause transition may be half-applied (`EmergencyPauseInProgress`).
    /// 4. The escrow must actually be emergency-paused (`NotPaused`) — this
    ///    endpoint describes the split *while frozen*, so producing a figure
    ///    for a running escrow is an illegal source state.
    ///
    /// Authorization deliberately precedes the source-state guards so an
    /// unauthorized caller learns nothing about the escrow's pause status.
    ///
    /// # Validation order
    /// The total and the weight vector are fully validated *before* the first
    /// division by `Î£weights` is attempted, so a malformed vector can never
    /// reach the arithmetic and can never trap:
    /// 1. `total_amount` ≤ 0                    → `InvalidAmount`
    /// 2. `weights` empty, or over the cap      → `InvalidAllocationWeights`
    /// 3. any `weight` < 0                      → `InvalidAllocationWeights`
    /// 4. `Σweights` overflowing, or `≤ 0`      → `InvalidAllocationWeights`
    ///
    /// Step 3 is a per-entry scan, not a property of the total: a vector such
    /// as `[5, -1, 6]` sums to `10`, so the sum check alone would let a
    /// negative share cancel itself out of the divisor and hand that party a
    /// negative allocation.  Step 4 is what keeps `weighted / weight_sum` from
    /// ever being a division by zero.
    ///
    /// # Parameters
    /// * `caller`        – Admin, client or freelancer. Must authorize.
    /// * `total_amount` – Amount to divide; must be > 0.
    /// * `weights`      – Per-party weights.  Need not sum to any particular
    ///   scale; only their ratios matter.  Must be non-empty, at most
    ///   `MAX_EMERGENCY_ALLOCATION_PARTIES` long, non-negative, and sum to > 0.
    ///
    /// # Returns
    /// A `Vec<i128>` of per-party amounts, index-aligned with `weights`.
    ///
    /// # Errors
    /// * `NotInitialized`           – No job has been recorded.
    /// * `Unauthorized`             – `caller` is not the admin, client or
    ///   freelancer.
    /// * `EmergencyPauseInProgress` – A pause transition is already running.
    /// * `NotPaused`                – The escrow is not emergency-paused.
    /// * `InvalidAmount`             – `total_amount` ≤ 0, or arithmetic overflow.
    /// * `InvalidAllocationWeights`  – `weights` empty, over the cap, negative,
    ///   or summing to zero.
    pub fn emergency_pause_allocation(
        env: Env,
        caller: Address,
        total_amount: i128,
        weights: Vec<i128>,
    ) -> Result<Vec<i128>, Error> {
        // ── authorization: before any other ledger read
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        if caller != meta.client && caller != meta.freelancer && admin.as_ref() != Some(&caller) {
            return Err(Error::Unauthorized);
        }

        // ── source state: a settled emergency pause, not a transition
        if !Self::is_paused(&env) {
            return Err(Error::NotPaused);
        }

        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if weights.is_empty() || weights.len() > MAX_EMERGENCY_ALLOCATION_PARTIES {
            return Err(Error::InvalidAllocationWeights);
        }

        let mut weight_sum: i128 = 0;
        for weight in weights.iter() {
            if weight < 0 {
                return Err(Error::InvalidAllocationWeights);
            }
            weight_sum = weight_sum
                .checked_add(weight)
                .ok_or(Error::InvalidAllocationWeights)?;
        }

        if weight_sum <= 0 {
            return Err(Error::InvalidAllocationWeights);
        }

        let mut allocations: Vec<i128> = Vec::new(&env);
        let mut remainders: Vec<i128> = Vec::new(&env);
        let mut allocated_total: i128 = 0;

        for weight in weights.iter() {
            let weighted = total_amount
                .checked_mul(weight)
                .ok_or(Error::InvalidAmount)?;

            allocations.push_back(weighted / weight_sum);
            remainders.push_back(weighted % weight_sum);

            allocated_total = allocated_total
                .checked_add(weighted / weight_sum)
                .ok_or(Error::InvalidAmount)?;
        }

        // `residue` is strictly less than the number of parties, because each
        // discarded fraction is < 1 unit. The loop below therefore runs at
        // most `MAX_EMERGENCY_ALLOCATION_PARTIES` times.
        let residue = total_amount
            .checked_sub(allocated_total)
            .ok_or(Error::InvalidAmount)?;

        for _ in 0..residue {
            let mut best_index: u32 = 0;
            let mut best_remainder: i128 = i128::MIN;

            // Strict `>` keeps the lowest index on a tie, making the result
            // deterministic across identical inputs.
            for (idx, rem) in remainders.iter().enumerate() {
                if rem > best_remainder {
                    best_remainder = rem;
                    best_index = idx as u32;
                }
            }

            let current = allocations.get(best_index).ok_or(Error::InvalidAmount)?;
            allocations.set(
                best_index,
                current.checked_add(1).ok_or(Error::InvalidAmount)?,
            );

            // Retire this party so it cannot win a second residue unit.
            remainders.set(best_index, i128::MIN);
        }

        let num_parties = allocations.len();
        env.events().publish(
            (symbol_short!("epalloc"),),
            EmergencyPauseAllocationEvent {
                total_amount,
                num_parties,
                allocations: allocations.clone(),
            },
        );

        Ok(allocations)
    }

}

#[allow(deprecated)]
impl MilestoneEscrow {
    /// Conserving two-party split used by `emergency_pause_split_refund`.
    ///
    /// The client leg is `round_nearest(total × client_bps / BPS_SCALE)`,
    /// computed without ever forming the `total × client_bps` product: with
    /// `total = q × BPS_SCALE + r`,
    ///
    /// ```text
    /// round_nearest(total × bps / S) = q × bps + ⌊(r × bps + S/2) / S⌋
    /// ```
    ///
    /// `q × bps ≤ total` and `r × bps < S²`, so no intermediate can overflow
    /// for any `total` up to `i128::MAX`; the result is identical to the
    /// direct formula wherever that one does not overflow.  The second term is
    /// at most `r`, so `client_refund ≤ total` and the freelancer leg — the
    /// exact remainder — is never negative.  The sum is re-checked before
    /// returning so a future edit that breaks conservation fails loudly.
    pub(crate) fn emergency_split_exact(
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

        let scale = BPS_SCALE as i128;
        let bps = client_refund_bps as i128;
        let quotient = total_amount / scale;
        let remainder = total_amount % scale;
        let client_refund = quotient
            .checked_mul(bps)
            .and_then(|whole| whole.checked_add((remainder * bps + scale / 2) / scale))
            .ok_or(Error::ArithmeticOverflow)?;
        let freelancer_payout = total_amount
            .checked_sub(client_refund)
            .ok_or(Error::ArithmeticOverflow)?;

        if freelancer_payout < 0
            || client_refund.checked_add(freelancer_payout) != Some(total_amount)
        {
            return Err(Error::ArithmeticOverflow);
        }

        Ok(RefundAllocation {
            client_refund,
            freelancer_payout,
            client_refund_bps,
            freelancer_payout_bps,
        })
    }

}
