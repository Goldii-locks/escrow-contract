#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, BytesN, Env,
    Vec,
};

/// Maximum number of tokens that may be held in the whitelist at any one time.
/// `add_whitelisted_token` enforces this cap before calling `push_back` so
/// that the internal `u32` length counter of the Soroban `Vec` can never
/// overflow regardless of how many times the function is invoked.
const MAX_WHITELIST_SIZE: u32 = 50;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AlreadyFunded = 3,
    NotFunded = 4,
    Unauthorized = 5,
    InvalidMilestone = 6,
    InvalidStatus = 7,
    TokenNotWhitelisted = 8,
    TokenAlreadyWhitelisted = 9,
    InvalidAmount = 10,
    DeadlineNotPassed = 11,
    InvalidAddress = 12,
    Paused = 13,
    InvalidRatio = 14,
    InvalidExtension = 15,
    EscrowLocked = 16,
}

const BPS_SCALE: u32 = 10_000;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MilestoneStatus {
    Pending,
    Delivered,
    PartiallyReleased,
    Released,
    Disputed,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Milestone {
    pub amount: i128,
    pub released_amount: i128,
    pub status: MilestoneStatus,
    pub delivered_at: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Job {
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub milestones: Vec<Milestone>,
    pub funded: bool,
    pub auto_release_seconds: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
struct JobMeta {
    client: Address,
    freelancer: Address,
    arbiter: Address,
    token: Address,
    funded: bool,
    auto_release_seconds: u64,
    milestone_count: u32,
    total_amount: i128,
}

/// Result of a split-refund allocation between client and freelancer.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundAllocation {
    pub client_refund: i128,
    pub freelancer_payout: i128,
    pub client_refund_bps: u32,
    pub freelancer_payout_bps: u32,
}

#[contracttype]
pub enum DataKey {
    Job,
    Milestone(u32),
    Admin,
    Version,
    WhitelistedTokens,
    EmergencyPaused,
    PlatformFeeAllocation,
    /// Temporary key: records the ledger timestamp at which a milestone was
    /// marked delivered.  Written by `mark_delivered`, consumed by
    /// `claim_auto_release` and `time_until_auto_release`.  Uses temporary
    /// storage because it is single-use, deadline-scoped workflow state whose
    /// ledger footprint cost should not persist beyond the auto-release window.
    DeliveredAt(u32),
    /// Temporary key: written by `approve_milestone` when a milestone reaches
    /// the terminal `Released` state via a full approval.  Acts as a cheap
    /// short-lived completion signal so callers can confirm terminal state
    /// without loading the full persistent `Milestone` entry.  Uses temporary
    /// storage because the signal is transient: once the milestone is released,
    /// the approval workflow for that milestone is permanently closed and this
    /// flag has no further use.
    MilestoneReleased(u32),
    Reputation(Address),
    // ── escrow_interest_yield admin-override keys ────────────────────────────
    /// Persistent: annual yield rate expressed in basis points (1 bp = 0.01 %).
    /// Range 0–10 000 (0 %–100 %).  Written by `admin_set_yield_rate`, read by
    /// `get_yield_info` and `admin_accrue_yield`.
    YieldRateBps,
    /// Persistent: total interest (in token stroops) accrued so far by the
    /// admin via `admin_accrue_yield`.  Reset to zero on admin override release
    /// or refund so downstream indexers can detect a fresh yield cycle.
    YieldAccrued,
    /// Persistent: boolean flag set to `true` by `admin_pause_escrow` and
    /// cleared by `admin_resume_escrow`.  When `true`, the guard in
    /// `assert_not_paused` blocks all normal user-facing endpoints (fund,
    /// mark_delivered, approve_milestone, approve_partial, claim_auto_release,
    /// raise_dispute, resolve_dispute) so that an emergency admin investigation
    /// cannot be interfered with.
    Paused,
    MilestoneTimeExtension(u32),
    CancelLock,
    // ── multisig approval compact storage keys ─────────────────────────────
    /// The full list of registered multisig signers (instance storage, written
    /// once by `multisig_approval_init`).  Stored as a single `Vec<Address>`
    /// rather than N individual keys to minimise read overhead and total bytes.
    MultiSigSigners,
    /// The minimum number of approvals required for a multisig decision.
    /// Written once during initialisation, read on every approval check.
    MultiSigThreshold,
    /// Transient approval-bitmap for a given proposal index.  Uses **temporary**
    /// storage so the ledger footprint does not persist beyond the proposal
    /// lifecycle.  Each bit position corresponds to a signer index in the
    /// `MultiSigSigners` vec; a set bit means that signer has approved.
    /// The `u32` value is treated as a bitset, supporting up to 32 signers.
    /// Key type: `u32` (the proposal index) — significantly smaller than a
    /// composite `(Address, u32)` alternative.
    MultiSigApproval(u32),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitializedEvent {
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub auto_release_seconds: u64,
    pub milestone_amounts: Vec<i128>,
    pub total_amount: i128,
    pub milestone_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundedEvent {
    pub contract_id: Address,
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub total_amount: i128,
    pub milestone_count: u32,
    pub auto_release_seconds: u64,
    pub funded: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveredEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub client: Address,
    pub delivered_at: u64,
    pub status: MilestoneStatus,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadlineExtendedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub extra_seconds: u64,
    pub new_extension: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
    pub released_amount: i128,
    pub remaining: i128,
    pub status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeRaisedEvent {
    pub milestone_index: u32,
    pub caller: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeResolvedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub arbiter: Address,
    pub client: Address,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
    pub released_to_freelancer: bool,
    pub status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFeeAllocation {
    pub client_bps: u32,
    pub freelancer_bps: u32,
    pub treasury_bps: u32,
    pub locked: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoReleasedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub client: Address,
    pub token: Address,
    pub amount: i128,
    pub delivered_at: u64,
    pub released_at: u64,
    pub auto_release_seconds: u64,
}
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferAdminEvent {
    pub old_admin: Address,
    pub new_admin: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenWhitelistedEvent {
    pub admin: Address,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenRemovedEvent {
    pub admin: Address,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RatioSplit {
    pub first: i128,
    pub second: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedEvent {
    pub contract_id: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelEscrowInitiatedEvent {
    pub contract_id: Address,
    pub caller: Address,
}


// ── escrow_interest_yield admin-override events ──────────────────────────────

/// Emitted by `admin_set_yield_rate` whenever the admin updates the annual
/// yield rate.  Downstream indexers can track the full rate-change history
/// by replaying these events in ledger order.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldRateSetEvent {
    pub admin: Address,
    pub old_rate_bps: u32,
    pub new_rate_bps: u32,
}

/// Emitted by `admin_accrue_yield` each time the admin books interest against
/// the escrowed balance.  `accrued_amount` is the incremental interest for
/// this call; `total_accrued` is the running total stored in `YieldAccrued`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YieldAccruedEvent {
    pub admin: Address,
    pub milestone_index: u32,
    pub accrued_amount: i128,
    pub total_accrued: i128,
}

/// Emitted by `admin_override_release` when the admin force-releases a locked
/// milestone directly to the freelancer, bypassing the normal approval flow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideReleaseEvent {
    pub admin: Address,
    pub contract_id: Address,
    pub milestone_index: u32,
    pub freelancer: Address,
    pub token: Address,
    pub amount: i128,
}

/// Emitted by `admin_override_refund` when the admin force-refunds a locked
/// milestone back to the client, bypassing the normal dispute flow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminOverrideRefundEvent {
    pub admin: Address,
    pub contract_id: Address,
    pub milestone_index: u32,
    pub client: Address,
    pub token: Address,
    pub amount: i128,
}

/// Result of checking whether a multisig proposal has reached the threshold.
/// Returned by `is_multisig_approved` to give callers both the boolean
/// decision and the raw approval bitmap for off-chain inspection.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiSigApprovalState {
    pub approved: bool,
    pub approvals: u32,
    pub threshold: u32,
    pub bitmap: u32,
}

/// Emitted by `admin_pause_escrow` when the admin freezes normal operations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowPausedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `admin_resume_escrow` when the admin lifts the pause and
/// restores normal operations.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowResumedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `emergency_pause` when the admin pauses the escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPausedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `emergency_unpause` when the admin unpauses the escrow.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyUnpausedEvent {
    pub admin: Address,
    pub contract_id: Address,
}

/// Emitted by `emergency_pause_admin_override` when the admin overrides the pause state.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmergencyPauseAdminOverrideEvent {
    pub admin: Address,
    pub contract_id: Address,
    pub paused: bool,
}


#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentStreamingEvent {
    pub total_amount: i128,
    pub numerator: i128,
    pub denominator: i128,
    pub streamed_payout: i128,
    pub client_refund: i128,
}

#[contract]
pub struct MilestoneEscrow;

#[contractimpl]
impl MilestoneEscrow {
    fn load_admin(env: &Env) -> Result<Address, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    fn require_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        admin.require_auth();
        let stored_admin = Self::load_admin(env)?;
        if stored_admin != *admin {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    fn ensure_not_paused(env: &Env) -> Result<(), Error> {
        let paused = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::EmergencyPaused)
            .unwrap_or(false);
        if paused {
            return Err(Error::Paused);
        }
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

    fn validate_fee_allocation(
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
        Ok(())
    }

    fn split_round_nearest(
        total: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        if total < 0 || numerator < 0 || denominator <= 0 || numerator > denominator {
            return Err(Error::InvalidRatio);
        }

        let scaled = total.checked_mul(numerator).ok_or(Error::InvalidAmount)?;
        let half = denominator / 2;
        let rounded = scaled.checked_add(half).ok_or(Error::InvalidAmount)? / denominator;

        if rounded > total {
            return Err(Error::InvalidAmount);
        }

        Ok(RatioSplit {
            first: rounded,
            second: total.checked_sub(rounded).ok_or(Error::InvalidAmount)?,
        })
    }

    fn load_job_meta(env: &Env) -> Result<JobMeta, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)
    }

    fn store_job_meta(env: &Env, meta: &JobMeta) {
        env.storage().instance().set(&DataKey::Job, meta);
    }

    fn load_milestone(env: &Env, index: u32) -> Result<Milestone, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Milestone(index))
            .ok_or(Error::InvalidMilestone)
    }

    fn store_milestone(env: &Env, index: u32, milestone: &Milestone) {
        env.storage()
            .persistent()
            .set(&DataKey::Milestone(index), milestone);
    }

    /// Write the delivery timestamp to temporary storage.  Temporary entries
    /// are automatically evicted by the network after their TTL expires, which
    /// makes them the correct storage tier for single-use, deadline-scoped
    /// workflow state like the auto-release window.
    fn store_delivered_at(env: &Env, index: u32, timestamp: u64) {
        env.storage()
            .temporary()
            .set(&DataKey::DeliveredAt(index), &timestamp);
    }

    /// Read the delivery timestamp from temporary storage.  Returns `None` if
    /// the entry has already been evicted (TTL expired) or was never written.
    fn load_delivered_at(env: &Env, index: u32) -> Option<u64> {
        env.storage().temporary().get(&DataKey::DeliveredAt(index))
    }

    /// Write the terminal approval flag to temporary storage.  This is a
    /// cheap, short-lived signal that the milestone at `index` has been fully
    /// released via `approve_milestone`.  Callers that only need to verify
    /// completion can read this temporary key rather than fetching the full
    /// persistent `Milestone` entry, reducing ledger footprint rent on the
    /// hot read path.
    fn store_milestone_released(env: &Env, index: u32) {
        env.storage()
            .temporary()
            .set(&DataKey::MilestoneReleased(index), &true);
    }

    fn load_time_extension(env: &Env, index: u32) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::MilestoneTimeExtension(index))
            .unwrap_or(0)
    }



    /// Check whether `approve_milestone` has marked the given milestone index
    /// as fully released via the temporary completion flag.  Returns `false`
    /// if the flag was never written or has been evicted.
    #[allow(dead_code)]
    fn is_milestone_released_flag(env: &Env, index: u32) -> bool {
        env.storage()
            .temporary()
            .get::<_, bool>(&DataKey::MilestoneReleased(index))
            .unwrap_or(false)
    }

    // ── pause guard ──────────────────────────────────────────────────────────

    /// Return `Err(Error::EscrowPaused)` when an admin pause is active so that
    /// every user-facing endpoint can call this as its first operation.
    fn assert_not_paused(env: &Env) -> Result<(), Error> {
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused {
            return Err(Error::Paused);
        }
        Ok(())
    }

    fn increment_reputation(env: &Env, address: &Address) {
        let key = DataKey::Reputation(address.clone());
        let current: u32 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(current + 1));
    }

    fn checked_add_amount(total: i128, amount: i128) -> Result<i128, Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        total.checked_add(amount).ok_or(Error::InvalidAmount)
    }

    fn checked_initialize_total(milestone_amounts: &Vec<i128>) -> Result<i128, Error> {
        if milestone_amounts.len() == 0 {
            return Err(Error::InvalidAmount);
        }

        let mut total_amount: i128 = 0;
        for amount in milestone_amounts.iter() {
            total_amount = Self::checked_add_amount(total_amount, amount)?;
        }

        Ok(total_amount)
    }

    fn checked_job_total(env: &Env, meta: &JobMeta) -> Result<i128, Error> {
        let mut total_amount: i128 = 0;

        for index in 0..meta.milestone_count {
            let milestone = Self::load_milestone(env, index)?;
            total_amount = Self::checked_add_amount(total_amount, milestone.amount)?;
        }

        if total_amount != meta.total_amount {
            return Err(Error::InvalidAmount);
        }

        Ok(total_amount)
    }

    fn validate_fund_amount(env: &Env, meta: &JobMeta) -> Result<i128, Error> {
        let total_amount = Self::checked_job_total(env, meta)?;
        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        Ok(total_amount)
    }

    fn validate_fund_client(env: &Env, client: &Address) -> Result<(), Error> {
        if client == &env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        Ok(())
    }

    fn validate_address(env: &Env, address: &Address) -> Result<(), Error> {
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

    fn assemble_job(env: &Env, meta: &JobMeta) -> Result<Job, Error> {
        let mut milestones = Vec::new(env);
        for i in 0..meta.milestone_count {
            milestones.push_back(Self::load_milestone(env, i)?);
        }
        Ok(Job {
            client: meta.client.clone(),
            freelancer: meta.freelancer.clone(),
            arbiter: meta.arbiter.clone(),
            token: meta.token.clone(),
            milestones,
            funded: meta.funded,
            auto_release_seconds: meta.auto_release_seconds,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
    ) -> Result<(), Error> {
        admin.require_auth();

        if env.storage().instance().has(&DataKey::Job) {
            return Err(Error::AlreadyInitialized);
        }

        Self::validate_address(&env, &admin)?;
        Self::validate_address(&env, &client)?;
        Self::validate_address(&env, &freelancer)?;
        Self::validate_address(&env, &arbiter)?;
        Self::validate_address(&env, &token)?;

        let milestone_count = milestone_amounts.len();
        if milestone_count == 0 {
            return Err(Error::InvalidAmount);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::EmergencyPaused, &false);
        env.storage().instance().set(
            &DataKey::PlatformFeeAllocation,
            &PlatformFeeAllocation {
                client_bps: 0,
                freelancer_bps: BPS_SCALE,
                treasury_bps: 0,
                locked: false,
            },
        );

        let mut whitelist: Vec<Address> = Vec::new(&env);
        whitelist.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);
        if auto_release_seconds == 0 {
            return Err(Error::InvalidAmount);
        }

        let mut total_amount: i128 = 0;
        for index in 0..milestone_count {
            let amount = milestone_amounts
                .get(index)
                .ok_or(Error::InvalidMilestone)?;
            total_amount = Self::checked_add_amount(total_amount, amount)?;
            Self::store_milestone(
                &env,
                index,
                &Milestone {
                    amount,
                    released_amount: 0,
                    status: MilestoneStatus::Pending,
                    delivered_at: 0,
                },
            );
        }

        env.storage().persistent().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Version, &1u32);

        let mut whitelist: Vec<Address> = Vec::new(&env);
        whitelist.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        let meta = JobMeta {
            client,
            freelancer,
            arbiter,
            token,
            funded: false,
            auto_release_seconds,
            milestone_count,
            total_amount,
        };

        Self::store_job_meta(&env, &meta);

        // Emit a structured initialization event so downstream indexers can
        // record all operational parameters from a single on-chain event without
        // having to query contract storage separately.
        env.events().publish(
            (symbol_short!("init"),),
            InitializedEvent {
                client: meta.client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                auto_release_seconds: meta.auto_release_seconds,
                milestone_amounts,
                total_amount: meta.total_amount,
                milestone_count: meta.milestone_count,
            },
        );

        Ok(())
    }

    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &current_admin)?;

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if current_admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        env.storage().persistent().set(&DataKey::Admin, &new_admin);

        env.events().publish(
            (symbol_short!("admin"),),
            TransferAdminEvent {
                old_admin: current_admin,
                new_admin,
            },
        );

        Ok(())
    }

    pub fn add_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if token == zero_account || token == zero_contract {
            return Err(Error::InvalidAddress);
        }
        if token == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

        let mut whitelist: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)?;

        // Duplicate check runs before the capacity check so that a
        // full whitelist still reports TokenAlreadyWhitelisted (rather
        // than InvalidAmount) for a token that's already present.
        if whitelist.contains(&token) {
            return Err(Error::TokenAlreadyWhitelisted);
        }

        if whitelist.len() >= MAX_WHITELIST_SIZE {
            return Err(Error::InvalidAmount);
        }

        whitelist.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        env.events().publish(
            (symbol_short!("wtok"),),
            TokenWhitelistedEvent { admin, token },
        );

        Ok(())
    }

    pub fn remove_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let meta = Self::load_job_meta(&env)?;
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }

        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if token == zero_account || token == zero_contract {
            return Err(Error::InvalidAddress);
        }

        let mut whitelist: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)?;

        let whitelist_len = whitelist.len();
        if whitelist_len == 0 {
            return Err(Error::TokenNotWhitelisted);
        }

        let post_removal_len = whitelist_len.checked_sub(1).ok_or(Error::InvalidAmount)?;
        if post_removal_len == 0 {
            return Err(Error::InvalidAmount);
        }

        if !whitelist.contains(&token) {
            return Err(Error::TokenNotWhitelisted);
        }

        if let Some(index) = whitelist.iter().position(|t| t == token) {
            let last = whitelist.len() - 1;
            if (index as u32) != last {
                let last_elem = whitelist.get(last).unwrap();
                whitelist.set(index as u32, last_elem);
            }
            whitelist.pop_back();
            env.storage()
                .instance()
                .set(&DataKey::WhitelistedTokens, &whitelist);

            env.events().publish(
                (symbol_short!("wldel"),),
                TokenRemovedEvent { admin, token },
            );

            Ok(())
        } else {
            Err(Error::TokenNotWhitelisted)
        }
    }

    pub fn is_token_whitelisted(env: Env, token: Address) -> bool {
        if let Some(whitelist) = env
            .storage()
            .instance()
            .get::<_, Vec<Address>>(&DataKey::WhitelistedTokens)
        {
            whitelist.contains(&token)
        } else {
            false
        }
    }

    pub fn get_whitelisted_tokens(env: Env) -> Result<Vec<Address>, Error> {
        env.storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)
    }

    pub fn fund(env: Env, client: Address) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::validate_fund_client(&env, &client)?;
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if meta.funded {
            return Err(Error::AlreadyFunded);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let total_amount = Self::validate_fund_amount(&env, &meta)?;

        // Update status BEFORE token transfer to ensure state is persisted
        // and prevent double-funding via reentrancy
        meta.funded = true;
        Self::store_job_meta(&env, &meta);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, &env.current_contract_address(), &total_amount);

        env.events().publish(
            (symbol_short!("fund"),),
            FundedEvent {
                contract_id: env.current_contract_address(),
                client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                total_amount,
                milestone_count: meta.milestone_count,
                auto_release_seconds: meta.auto_release_seconds,
                funded: meta.funded,
            },
        );

        Ok(())
    }

    pub fn mark_delivered(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        // Check for zero addresses (both account and contract types)
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if freelancer == zero_account || freelancer == zero_contract {
            return Err(Error::InvalidAddress);
        }
        freelancer.require_auth();

        let meta = Self::load_job_meta(&env)?;

        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        let delivered_at = env.ledger().timestamp();
        milestone.status = MilestoneStatus::Delivered;
        milestone.delivered_at = delivered_at;
        Self::store_milestone(&env, milestone_index, &milestone);
        // Write the delivery timestamp to temporary storage so that
        // claim_auto_release and time_until_auto_release can read it from the
        // optimised temporary tier without touching the persistent Milestone entry.
        Self::store_delivered_at(&env, milestone_index, delivered_at);

        env.events().publish(
            (symbol_short!("deliver"),),
            DeliveredEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                freelancer: meta.freelancer,
                client: meta.client,
                delivered_at,
                status: MilestoneStatus::Delivered,
                amount: milestone.amount,
            },
        );

        Ok(())
    }

    /// Extends the auto-release deadline for a Delivered milestone.
    pub fn extend_milestone_deadline(
        env: Env,
        client: Address,
        milestone_index: u32,
        extra_seconds: u64,
    ) -> Result<(), Error> {
        Self::assert_not_paused(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Delivered && milestone.status != MilestoneStatus::PartiallyReleased {
            return Err(Error::InvalidStatus);
        }

        if extra_seconds == 0 {
            return Err(Error::InvalidExtension);
        }

        let current_extension = Self::load_time_extension(&env, milestone_index);
        let new_extension = current_extension.checked_add(extra_seconds).ok_or(Error::InvalidExtension)?;

        env.storage()
            .persistent()
            .set(&DataKey::MilestoneTimeExtension(milestone_index), &new_extension);

        env.events().publish(
            (symbol_short!("extend"),),
            DeadlineExtendedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                client,
                extra_seconds,
                new_extension,
            },
        );

        Ok(())
    }

    /// Time-locked auto-release of a single milestone to the freelancer.
    ///
    /// # Gas complexity: O(1)
    ///
    /// This function performs a bounded, constant number of storage reads and
    /// writes regardless of the total milestone count:
    ///
    /// - 1Ã— instance read  (`DataKey::Job` â†’ `JobMeta`)
    /// - 1Ã— temporary read (`DataKey::DeliveredAt(milestone_index)`)
    /// - 1Ã— persistent read  (`DataKey::Milestone(milestone_index)`)
    /// - 1Ã— persistent write (`DataKey::Milestone(milestone_index)`)
    /// - 1Ã— token transfer
    ///
    /// No loop over all milestones is performed here.  Functions that do loop
    /// over all milestones (`checked_job_total`, `assemble_job`) are
    /// intentionally not called from this hot path.
    pub fn claim_auto_release(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if freelancer == zero_account || freelancer == zero_contract {
            return Err(Error::InvalidAddress);
        }
        freelancer.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }

        // CHECK 1: Validate index boundary.
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        // CHECK 2: Milestone must be in the Delivered state.  Any other status â€”
        // including Released (double-claim), Disputed, Refunded, Pending, or
        // PartiallyReleased â€” is rejected here, making the guard the sole
        // gatekeeper against double-execution and out-of-sequence calls.
        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        // CHECK 3: Validate auto_release_seconds is non-zero.
        if meta.auto_release_seconds == 0 {
            return Err(Error::InvalidAmount);
        }

        // CHECK 4: Read the delivery timestamp from temporary storage first
        //    (optimised ledger-footprint path).  Fall back to the value stored on
        //    the persistent Milestone entry so that entries written before this
        //    migration remain fully functional.
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let extension = Self::load_time_extension(&env, milestone_index);

        let deadline = delivered_at
            .checked_add(meta.auto_release_seconds)
            .and_then(|d| d.checked_add(extension))
            .ok_or(Error::InvalidAmount)?;
        let current = env.ledger().timestamp();
        if current < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        // CHECK 5: Compute remaining using checked subtraction so that corrupted
        //    or adversarially-crafted storage values (released_amount > amount)
        //    never produce a silent underflow.
        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        // EFFECT: Commit the terminal state to persistent storage BEFORE any
        //    external call (Checks-Effects-Interactions pattern).  Setting the
        //    status to Released here means a re-entrant or duplicate invocation
        //    will hit the `InvalidStatus` guard above on its next CHECK 2 and
        //    be rejected before it can touch the token contract.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::increment_reputation(&env, &meta.client);
        Self::increment_reputation(&env, &meta.freelancer);

        // INTERACTION: Token transfer is the sole external call and executes only
        //    after all state mutations have been durably persisted.
        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );

        env.events().publish(
            (symbol_short!("claim"),),
            ClaimedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                freelancer: meta.freelancer,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> i64 {
        let meta = Self::load_job_meta(&env).unwrap();
        let milestone = Self::load_milestone(&env, milestone_index).unwrap();
        // Read delivery timestamp from temporary storage (optimised path) and
        // fall back to the persistent Milestone field for pre-migration entries.
        let delivered_at =
            Self::load_delivered_at(&env, milestone_index).unwrap_or(milestone.delivered_at);
        let extension = Self::load_time_extension(&env, milestone_index);
        let deadline = delivered_at + meta.auto_release_seconds + extension;
        let current = env.ledger().timestamp();
        (deadline as i64) - (current as i64)
    }

    pub fn approve_partial(
        env: Env,
        client: Address,
        milestone_index: u32,
        amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        let zero_1 = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_2 = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if client == zero_1 || client == zero_2 || client == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if amount > remaining {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &meta.freelancer, &amount);

        let mut updated_milestone = milestone;
        updated_milestone.released_amount = updated_milestone
            .released_amount
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;

        if updated_milestone.released_amount == updated_milestone.amount {
            updated_milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        } else {
            updated_milestone.status = MilestoneStatus::PartiallyReleased;
        }

        Self::store_milestone(&env, milestone_index, &updated_milestone);

        let event_remaining = updated_milestone
            .amount
            .checked_sub(updated_milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        env.events().publish(
            (symbol_short!("approve"),),
            ApprovedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                client: meta.client,
                freelancer: meta.freelancer,
                token: meta.token,
                amount,
                released_amount: updated_milestone.released_amount,
                remaining: event_remaining,
                status: updated_milestone.status.clone(),
            },
        );

        Ok(())
    }

    pub fn approve_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if client == zero_account || client == zero_contract {
            return Err(Error::InvalidAddress);
        }

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );
        milestone.released_amount = milestone.amount;

        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::increment_reputation(&env, &meta.client);
        Self::increment_reputation(&env, &meta.freelancer);

        // Write a short-lived completion flag to temporary storage.  This is
        // transient workflow state: the milestone approval window is now
        // permanently closed, so this signal does not need to survive beyond
        // the TTL of the ledger entry.  Using temporary storage avoids the
        // higher rent cost of a persistent or instance entry for data that has
        // no long-term value.
        Self::store_milestone_released(&env, milestone_index);

        let event_remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;

        env.events().publish(
            (symbol_short!("approve"),),
            ApprovedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                client: meta.client,
                freelancer: meta.freelancer,
                token: meta.token,
                amount: remaining,
                released_amount: milestone.released_amount,
                remaining: event_remaining,
                status: milestone.status.clone(),
            },
        );

        Ok(())
    }

    pub fn raise_dispute(env: Env, caller: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        // Check for zero addresses (both account and contract types)
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != caller && meta.freelancer != caller {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        // Strict state machine: only Pending, Delivered, or PartiallyReleased
        // may transition to Disputed. All other statuses (Released, Refunded,
        // Disputed) are rejected.
        match milestone.status {
            MilestoneStatus::Pending
            | MilestoneStatus::Delivered
            | MilestoneStatus::PartiallyReleased => {}
            _ => return Err(Error::InvalidStatus),
        }

        milestone.status = MilestoneStatus::Disputed;
        Self::store_milestone(&env, milestone_index, &milestone);

        env.events().publish(
            (symbol_short!("dispute"),),
            DisputeRaisedEvent {
                milestone_index,
                caller,
            },
        );

        Ok(())
    }

    pub fn resolve_dispute(
        env: Env,
        arbiter: Address,
        milestone_index: u32,
        release_to_freelancer: bool,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );

        if arbiter == zero_account
            || arbiter == zero_contract
            || arbiter == env.current_contract_address()
        {
            return Err(Error::InvalidAddress);
        }
        arbiter.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

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

        let token_client = token::Client::new(&env, &meta.token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance <= 0 {
            return Err(Error::InvalidAmount);
        }

        if release_to_freelancer {
            let payout = remaining.min(contract_balance);
            if payout > 0 {
                token_client.transfer(&env.current_contract_address(), &meta.freelancer, &payout);
                milestone.released_amount = milestone
                    .released_amount
                    .checked_add(payout)
                    .ok_or(Error::InvalidAmount)?;
            }
            milestone.status = MilestoneStatus::Released;
            Self::increment_reputation(&env, &meta.client);
            Self::increment_reputation(&env, &meta.freelancer);
        } else {
            let refund = remaining.min(contract_balance);
            if refund > 0 {
                token_client.transfer(&env.current_contract_address(), &meta.client, &refund);
            }
            milestone.status = MilestoneStatus::Refunded;
        }

        Self::store_milestone(&env, milestone_index, &milestone);

        env.events().publish(
            (symbol_short!("resolve"),),
            DisputeResolvedEvent {
                contract_id: env.current_contract_address(),
                milestone_index,
                arbiter: meta.arbiter.clone(),
                client: meta.client.clone(),
                freelancer: meta.freelancer.clone(),
                token: meta.token.clone(),
                amount: remaining,
                released_to_freelancer: release_to_freelancer,
                status: milestone.status.clone(),
            },
        );

        Ok(())
    }

    pub fn cancel_escrow(env: Env, caller: Address) -> Result<(), Error> {
        let zero_account = Address::from_str(
            &env,
            "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
        );
        let zero_contract = Address::from_str(
            &env,
            "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
        );
        if caller == zero_account || caller == zero_contract {
            return Err(Error::InvalidAddress);
        }

        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if caller != meta.client && caller != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        if !meta.funded {
            return Err(Error::NotFunded);
        }

        env.storage().instance().set(&DataKey::CancelLock, &true);

        env.events().publish(
            (symbol_short!("cancel"),),
            CancelEscrowInitiatedEvent {
                contract_id: env.current_contract_address(),
                caller,
            },
        );

        Ok(())
    }

    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        env.deployer().update_current_contract_wasm(new_wasm_hash);

        let current: u32 = env.storage().instance().get(&DataKey::Version).unwrap_or(1);
        env.storage()
            .instance()
            .set(&DataKey::Version, &(current + 1));

        Ok(())
    }

    pub fn emergency_pause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::EmergencyPaused, &true);

        env.events().publish(
            (symbol_short!("empause"),),
            EmergencyPausedEvent {
                admin: admin.clone(),
                contract_id: env.current_contract_address(),
            },
        );

        Ok(())
    }

    pub fn emergency_unpause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::EmergencyPaused, &false);

        env.events().publish(
            (symbol_short!("emunpause"),),
            EmergencyUnpausedEvent {
                admin: admin.clone(),
                contract_id: env.current_contract_address(),
            },
        );

        Ok(())
    }

    pub fn emergency_pause_admin_override(
        env: Env,
        admin: Address,
        paused: bool,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let current = env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::EmergencyPaused)
            .unwrap_or(false);

        if current == paused {
            return Err(Error::InvalidStatus);
        }

        env.storage()
            .instance()
            .set(&DataKey::EmergencyPaused, &paused);

        env.events().publish(
            (symbol_short!("emoverrid"),),
            EmergencyPauseAdminOverrideEvent {
                admin: admin.clone(),
                contract_id: env.current_contract_address(),
                paused,
            },
        );

        Ok(())
    }

    pub fn is_emergency_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::EmergencyPaused)
            .unwrap_or(false)
    }

    pub fn set_platform_fee_allocation(
        env: Env,
        admin: Address,
        client_bps: u32,
        freelancer_bps: u32,
        treasury_bps: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;

        let current: PlatformFeeAllocation = env
            .storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)?;

        if current.locked {
            return Err(Error::InvalidStatus);
        }

        env.storage().instance().set(
            &DataKey::PlatformFeeAllocation,
            &PlatformFeeAllocation {
                client_bps,
                freelancer_bps,
                treasury_bps,
                locked: false,
            },
        );
        Ok(())
    }

    pub fn lock_platform_fee_allocation(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let mut current: PlatformFeeAllocation = env
            .storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)?;
        current.locked = true;
        env.storage()
            .instance()
            .set(&DataKey::PlatformFeeAllocation, &current);
        Ok(())
    }

    pub fn pf_alloc_admin_override(
        env: Env,
        admin: Address,
        client_bps: u32,
        freelancer_bps: u32,
        treasury_bps: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        let current: PlatformFeeAllocation = env
            .storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)?;

        if !current.locked {
            return Err(Error::InvalidStatus);
        }

        Self::validate_fee_allocation(client_bps, freelancer_bps, treasury_bps)?;
        env.storage().instance().set(
            &DataKey::PlatformFeeAllocation,
            &PlatformFeeAllocation {
                client_bps,
                freelancer_bps,
                treasury_bps,
                locked: false,
            },
        );
        Ok(())
    }

    pub fn get_platform_fee_allocation(env: Env) -> Result<PlatformFeeAllocation, Error> {
        env.storage()
            .instance()
            .get(&DataKey::PlatformFeeAllocation)
            .ok_or(Error::NotInitialized)
    }

    pub fn payment_streaming_milestones(
        env: Env,
        total_amount: i128,
        numerator: i128,
        denominator: i128,
    ) -> Result<RatioSplit, Error> {
        if total_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        if denominator <= 0 {
            return Err(Error::InvalidRatio);
        }
        if numerator < 0 || numerator > denominator {
            return Err(Error::InvalidRatio);
        }

        let split = Self::split_round_nearest(total_amount, numerator, denominator)?;

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

    pub fn multisig_transfer_admin(
        env: Env,
        total_amount: i128,
        ratios: Vec<i128>,
    ) -> Result<Vec<i128>, Error> {
        if total_amount < 0 || ratios.is_empty() {
            return Err(Error::InvalidRatio);
        }

        let mut ratio_sum: i128 = 0;
        for ratio in ratios.iter() {
            if ratio < 0 {
                return Err(Error::InvalidRatio);
            }
            ratio_sum = ratio_sum.checked_add(ratio).ok_or(Error::InvalidRatio)?;
        }

        if ratio_sum <= 0 {
            return Err(Error::InvalidRatio);
        }

        let mut allocations: Vec<i128> = Vec::new(&env);
        let mut remainders: Vec<i128> = Vec::new(&env);
        let mut allocated_total: i128 = 0;

        for ratio in ratios.iter() {
            let weighted = total_amount
                .checked_mul(ratio)
                .ok_or(Error::InvalidAmount)?;
            let base = weighted / ratio_sum;
            let rem = weighted % ratio_sum;

            allocations.push_back(base);
            remainders.push_back(rem);
            allocated_total = allocated_total
                .checked_add(base)
                .ok_or(Error::InvalidAmount)?;
        }

        let remaining = total_amount
            .checked_sub(allocated_total)
            .ok_or(Error::InvalidAmount)?;

        for _ in 0..remaining {
            let mut best_index: u32 = 0;
            let mut best_remainder: i128 = i128::MIN;

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
            remainders.set(best_index, i128::MIN);
        }

        Ok(allocations)
    }

    // ── multisig approval: storage-optimised key design ────────────────────
    //
    // Design rationale
    // ─────────────────
    // Traditional multisig implementations store approval state as individual
    // `(Address, ProposalId) → bool` entries, which is expensive on Soroban
    // because each Address contributes ~32 bytes to the ledger key footprint.
    //
    // This implementation uses three optimisations to minimise bytes stored:
    //
    // 1. **Signer list is stored once** (instance storage) under a single
    //    `MultiSigSigners` key rather than storing individual key-value pairs
    //    per signer.
    //
    // 2. **Approval tracking uses a compact u32 bitmap** in temporary storage
    //    under `MultiSigApproval(proposal_id)`.  Each bit represents one signer
    //    by its index in the signers vec, eliminating the Address overhead from
    //    every approval entry.  Up to 32 signers are supported per proposal.
    //
    // 3. **Temporary storage tier** is used for the bitmap so that the ledger
    //    footprint is automatically evicted once the proposal lifecycle ends,
    //    rather than persisting indefinitely.

    const MAX_MULTISIG_SIGNERS: u32 = 32;

    /// Initialise a multisig approval regime with a fixed set of signers and
    /// the required approval threshold.  Must be called exactly once.
    pub fn multisig_approval_init(
        env: Env,
        admin: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;

        if env.storage().instance().has(&DataKey::MultiSigSigners) {
            return Err(Error::AlreadyInitialized);
        }

        let count = signers.len();
        if count == 0 || count > Self::MAX_MULTISIG_SIGNERS {
            return Err(Error::InvalidAmount);
        }
        if threshold == 0 || threshold > count {
            return Err(Error::InvalidAmount);
        }

        env.storage()
            .instance()
            .set(&DataKey::MultiSigSigners, &signers);
        env.storage()
            .instance()
            .set(&DataKey::MultiSigThreshold, &threshold);

        Ok(())
    }

    /// Record an approval from one of the registered signers for the given
    /// proposal.  Idempotent — calling twice from the same signer has no
    /// effect and is not an error.
    pub fn multisig_approve(
        env: Env,
        signer: Address,
        proposal_id: u32,
    ) -> Result<MultiSigApprovalState, Error> {
        signer.require_auth();

        let signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigSigners)
            .ok_or(Error::NotInitialized)?;

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .ok_or(Error::NotInitialized)?;

        // Find the signer's index in the list (O(n) but n ≤ 32).
        let signer_index = signers
            .iter()
            .position(|s| s == signer)
            .ok_or(Error::Unauthorized)?;

        // Read the current bitmap from temporary storage (default: 0 = no approvals).
        let mut bitmap: u32 = env
            .storage()
            .temporary()
            .get(&DataKey::MultiSigApproval(proposal_id))
            .unwrap_or(0);

        // Set the bit for this signer (idempotent).
        let idx: u32 = signer_index.try_into().map_err(|_| Error::InvalidAmount)?;
        let mask = 1u32.checked_shl(idx).ok_or(Error::InvalidAmount)?;
        bitmap |= mask;

        // Write the updated bitmap back to temporary storage.
        env.storage()
            .temporary()
            .set(&DataKey::MultiSigApproval(proposal_id), &bitmap);

        let approvals = bitmap.count_ones();
        let approved = approvals >= threshold;

        Ok(MultiSigApprovalState {
            approved,
            approvals,
            threshold,
            bitmap,
        })
    }

    /// Query whether a proposal has reached the required approval threshold.
    /// Pure read — does not require auth and does not mutate state.
    pub fn is_multisig_approved(env: Env, proposal_id: u32) -> Result<MultiSigApprovalState, Error> {
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .ok_or(Error::NotInitialized)?;

        let bitmap: u32 = env
            .storage()
            .temporary()
            .get(&DataKey::MultiSigApproval(proposal_id))
            .unwrap_or(0);

        let approvals = bitmap.count_ones();

        Ok(MultiSigApprovalState {
            approved: approvals >= threshold,
            approvals,
            threshold,
            bitmap,
        })
    }

    pub fn version(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::Version).unwrap_or(1)
    }

    pub fn get_job(env: Env) -> Result<Job, Error> {
        let meta = Self::load_job_meta(&env)?;
        Self::assemble_job(&env, &meta)
    }

    pub fn get_reputation(env: Env, address: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Reputation(address))
            .unwrap_or(0)
    }
}

mod test;

// ── escrow_interest_yield: admin emergency override endpoints ─────────────────
//
// Design rationale
// ─────────────────
// In rare operational conditions (e.g. a client or freelancer becoming
// unresponsive, a key being compromised, or yield accounting needing manual
// correction) the platform admin must be able to resolve a locked escrow
// without depending on the normal multi-party workflow.  These endpoints are
// intentionally narrow in scope:
//
//   • Every function requires a fresh `admin.require_auth()` and then verifies
//     the supplied address against the persisted `DataKey::Admin` value, so no
//     other address — including the client, freelancer, or arbiter — can ever
//     invoke them.
//
//   • Overrides are not gated on milestone status; the admin can act on a
//     milestone in ANY state (Pending, Delivered, PartiallyReleased, Disputed,
//     etc.) so that genuinely stuck escrows can always be resolved.
//
//   • Every action emits a structured on-chain event so that off-chain
//     indexers, auditors, and the parties involved receive an immutable record
//     of what happened and who authorised it.

#[contractimpl]
impl MilestoneEscrow {
    // ── yield-rate management ─────────────────────────────────────────────────

    /// Set the annual yield rate for the escrow in basis points (1 bp = 0.01 %).
    ///
    /// # Parameters
    /// * `admin`       – Must match `DataKey::Admin`; a fresh signature is
    ///                   required on every call.
    /// * `rate_bps`    – New annual rate.  Capped at 10 000 (= 100 %).
    ///                   Pass `0` to disable yield accrual.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract has not been initialised yet.
    /// * `Unauthorized`     – `admin` does not match the stored admin key.
    /// * `YieldRateInvalid` – `rate_bps` exceeds 10 000.
    pub fn admin_set_yield_rate(env: Env, admin: Address, rate_bps: u32) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        if rate_bps > 10_000 {
            return Err(Error::InvalidRatio);
        }

        let old_rate_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldRateBps)
            .unwrap_or(0);

        env.storage()
            .persistent()
            .set(&DataKey::YieldRateBps, &rate_bps);

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
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Index of the milestone to which yield is attributed.
    /// * `accrued_amount`  – Interest amount to book; must be > 0.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidAmount`   – `accrued_amount` ≤ 0 or the running total would
    ///                       overflow `i128`.
    pub fn admin_accrue_yield(
        env: Env,
        admin: Address,
        milestone_index: u32,
        accrued_amount: i128,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

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

    // ── emergency override transfers ──────────────────────────────────────────

    /// Force-release a locked milestone directly to the freelancer, bypassing
    /// the normal `mark_delivered` → `approve_milestone` flow.
    ///
    /// This is the primary remedy for an escrow where the client is
    /// unresponsive or has lost their key after the freelancer has completed
    /// the work.  The milestone is moved to `Released` and a full token
    /// transfer is executed.
    ///
    /// The override works on any non-terminal milestone status (Pending,
    /// Delivered, PartiallyReleased, Disputed).  Calling it on an already
    /// `Released` or `Refunded` milestone — where the funds have already left
    /// the contract — returns `InvalidStatus` to prevent a double-spend.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `NotFunded`       – Escrow has not been funded; nothing to release.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0 (sanity guard).
    pub fn admin_override_release(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        // Terminal states have already settled funds — no double-spend.
        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
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

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::store_milestone_released(&env, milestone_index);

        // Reset accrued yield on emergency override
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &0_i128);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );

        env.events().publish(
            (symbol_short!("admovrls"),),
            AdminOverrideReleaseEvent {
                admin,
                contract_id: env.current_contract_address(),
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
    /// Use this when the freelancer is unresponsive, the work was never
    /// delivered, or the arbiter cannot be reached.  The milestone is moved to
    /// `Refunded` and a full token transfer is executed back to the client.
    ///
    /// Like `admin_override_release`, this operates on any non-terminal status
    /// and returns `InvalidStatus` for already-settled milestones.
    ///
    /// # Parameters
    /// * `admin`           – Must match `DataKey::Admin`.
    /// * `milestone_index` – Target milestone.
    ///
    /// # Errors
    /// * `NotInitialized`  – Contract has not been initialised.
    /// * `Unauthorized`    – `admin` is not the stored admin.
    /// * `NotFunded`       – Escrow has not been funded.
    /// * `InvalidMilestone`– `milestone_index` is out of range.
    /// * `InvalidStatus`   – Milestone is already `Released` or `Refunded`.
    /// * `InvalidAmount`   – Remaining balance is ≤ 0 (sanity guard).
    pub fn admin_override_refund(
        env: Env,
        admin: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        let meta = Self::load_job_meta(&env)?;
        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status == MilestoneStatus::Released
            || milestone.status == MilestoneStatus::Refunded
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

        // CEI: commit state before external call.
        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        // Reset accrued yield on emergency override
        env.storage()
            .persistent()
            .set(&DataKey::YieldAccrued, &0_i128);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &meta.client, &remaining);

        env.events().publish(
            (symbol_short!("admovrf"),),
            AdminOverrideRefundEvent {
                admin,
                contract_id: env.current_contract_address(),
                milestone_index,
                client: meta.client,
                token: meta.token,
                amount: remaining,
            },
        );

        Ok(())
    }

    // ── pause / resume ────────────────────────────────────────────────────────

    /// Pause the escrow, blocking all normal user-facing endpoints.
    ///
    /// After this call, `fund`, `mark_delivered`, `approve_milestone`,
    /// `approve_partial`, `claim_auto_release`, `raise_dispute`, and
    /// `resolve_dispute` all return `EscrowPaused` until the admin calls
    /// `admin_resume_escrow`.  Admin-prefixed endpoints (including this one)
    /// remain fully operational during a pause.
    ///
    /// Calling this on an already-paused escrow is a no-op (idempotent) so
    /// that automated retry logic cannot produce an error.
    ///
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    pub fn admin_pause_escrow(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        let already_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);

        env.storage().instance().set(&DataKey::Paused, &true);

        // Only emit the event the first time so indexers can count distinct
        // pause transitions without double-counting idempotent re-pauses.
        if !already_paused {
            env.events().publish(
                (symbol_short!("pause"),),
                EscrowPausedEvent {
                    admin,
                    contract_id: env.current_contract_address(),
                },
            );
        }

        Ok(())
    }

    /// Resume a previously paused escrow, re-enabling all normal user-facing
    /// endpoints.
    ///
    /// Calling this on an escrow that is not paused is a no-op (idempotent).
    ///
    /// # Parameters
    /// * `admin` – Must match `DataKey::Admin`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialised.
    /// * `Unauthorized`   – `admin` is not the stored admin.
    pub fn admin_resume_escrow(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;

        let currently_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);

        env.storage().instance().set(&DataKey::Paused, &false);

        if currently_paused {
            env.events().publish(
                (symbol_short!("resume"),),
                EscrowResumedEvent {
                    admin,
                    contract_id: env.current_contract_address(),
                },
            );
        }

        Ok(())
    }

    // ── read-only query ───────────────────────────────────────────────────────

    /// Return a snapshot of the current yield and pause state.
    ///
    /// All fields are safe to call even before any admin has set a yield rate
    /// (defaults to zero) or paused the contract (defaults to `false`).
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
    pub fn get_yield_info(env: Env) -> Result<(u32, i128, bool), Error> {
        // Verify the contract is initialized before returning state.
        Self::load_job_meta(&env)?;

        let rate_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldRateBps)
            .unwrap_or(0);

        let total_accrued: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0);

        let is_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);

        Ok((rate_bps, total_accrued, is_paused))
    }
}
