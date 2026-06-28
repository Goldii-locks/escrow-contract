#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env, Vec,
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
}

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

#[contracttype]
pub enum DataKey {
    Job,
    Milestone(u32),
    Admin,
    WhitelistedTokens,
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
}

#[contracttype]
pub struct InitializedEvent {
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
    pub milestone_amounts: Vec<i128>,
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
pub struct DisputeRaisedEvent {
    pub milestone_index: u32,
}

#[contracttype]
pub struct DisputeResolvedEvent {
    pub milestone_index: u32,
    pub released_to_freelancer: bool,
}

#[contract]
pub struct MilestoneEscrow;

#[contractimpl]
impl MilestoneEscrow {
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
        env.storage()
            .temporary()
            .get(&DataKey::DeliveredAt(index))
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

    fn checked_add_amount(total: i128, amount: i128) -> Result<i128, Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        total.checked_add(amount).ok_or(Error::InvalidAmount)
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

    fn validate_fund_client(env: &Env, client: &Address) -> Result<(), Error> {
        if client == &env.current_contract_address() {
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
        if env.storage().instance().has(&DataKey::Job) {
            return Err(Error::AlreadyInitialized);
        }

        let milestone_count = milestone_amounts.len();
        let mut total_amount: i128 = 0;
        for amount in milestone_amounts.iter() {
            total_amount = Self::checked_add_amount(total_amount, amount)?;
        }

        env.storage().instance().set(&DataKey::Admin, &admin);

        let mut whitelist: Vec<Address> = Vec::new(&env);
        whitelist.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        for (index, amount) in milestone_amounts.iter().enumerate() {
            Self::store_milestone(
                &env,
                index as u32,
                &Milestone {
                    amount,
                    released_amount: 0,
                    status: MilestoneStatus::Pending,
                    delivered_at: 0,
                },
            );
        }

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
        Ok(())
    }

    pub fn transfer_admin(
        env: Env,
        current_admin: Address,
        new_admin: Address,
    ) -> Result<(), Error> {
        current_admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if current_admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        env.storage().instance().set(&DataKey::Admin, &new_admin);
        Ok(())
    }

    pub fn add_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let mut whitelist: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)?;

        if whitelist.contains(&token) {
            return Err(Error::TokenAlreadyWhitelisted);
        }

        // Guard against integer overflow on the Vec's internal u32 length
        // counter.  If the whitelist is already at capacity, reject the
        // addition with InvalidAmount rather than letting push_back overflow.
        if whitelist.len() >= MAX_WHITELIST_SIZE {
            return Err(Error::InvalidAmount);
        }

        whitelist.push_back(token);
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);
        Ok(())
    }

    pub fn remove_whitelisted_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let mut whitelist: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)?;

        if let Some(index) = whitelist.iter().position(|t| t == token) {
            whitelist.remove(index as u32);
            env.storage()
                .instance()
                .set(&DataKey::WhitelistedTokens, &whitelist);
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
        Self::validate_fund_client(&env, &client)?;
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if meta.funded {
            return Err(Error::AlreadyFunded);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let total_amount = meta.total_amount;
        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, &env.current_contract_address(), &total_amount);

        meta.funded = true;
        Self::store_job_meta(&env, &meta);

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

    /// Time-locked auto-release of a single milestone to the freelancer.
    ///
    /// # Gas complexity: O(1)
    ///
    /// This function performs a bounded, constant number of storage reads and
    /// writes regardless of the total milestone count:
    ///
    /// - 1× instance read  (`DataKey::Job` → `JobMeta`)
    /// - 1× temporary read (`DataKey::DeliveredAt(milestone_index)`)
    /// - 1× persistent read  (`DataKey::Milestone(milestone_index)`)
    /// - 1× persistent write (`DataKey::Milestone(milestone_index)`)
    /// - 1× token transfer
    ///
    /// No loop over all milestones is performed here.  Functions that do loop
    /// over all milestones (`checked_job_total`, `assemble_job`) are
    /// intentionally not called from this hot path.
    pub fn claim_auto_release(
    env: Env,
    freelancer: Address,
    milestone_index: u32,
) -> Result<(), Error> {
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

    // CHECK 2: Milestone must be in the Delivered state.  Any other status —
    // including Released (double-claim), Disputed, Refunded, Pending, or
    // PartiallyReleased — is rejected here, making the guard the sole
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
    let delivered_at = Self::load_delivered_at(&env, milestone_index)
        .unwrap_or(milestone.delivered_at);

    let deadline = delivered_at
        .checked_add(meta.auto_release_seconds)
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

    // INTERACTION: Token transfer is the sole external call and executes only
    //    after all state mutations have been durably persisted.
    let token_client = token::Client::new(&env, &meta.token);
    token_client.transfer(
        &env.current_contract_address(),
        &meta.freelancer,
        &remaining,
    );

    Ok(())
}

    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> i64 {
        let meta = Self::load_job_meta(&env).unwrap();
        let milestone = Self::load_milestone(&env, milestone_index).unwrap();
        // Read delivery timestamp from temporary storage (optimised path) and
        // fall back to the persistent Milestone field for pre-migration entries.
        let delivered_at = Self::load_delivered_at(&env, milestone_index)
            .unwrap_or(milestone.delivered_at);
        let deadline = delivered_at + meta.auto_release_seconds;
        let current = env.ledger().timestamp();
        (deadline as i64) - (current as i64)
    }

    pub fn approve_partial(
        env: Env,
        client: Address,
        milestone_index: u32,
        amount: i128,
    ) -> Result<(), Error> {
        let zero_1 = Address::from_str(&env, "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF");
        let zero_2 = Address::from_str(&env, "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4");
        if client == zero_1 || client == zero_2 || client == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
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

        let remaining = milestone.amount.checked_sub(milestone.released_amount).ok_or(Error::InvalidAmount)?;
        if amount > remaining {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &meta.freelancer, &amount);

        let mut updated_milestone = milestone;
        updated_milestone.released_amount = updated_milestone.released_amount.checked_add(amount).ok_or(Error::InvalidAmount)?;

        if updated_milestone.released_amount == updated_milestone.amount {
            updated_milestone.status = MilestoneStatus::Released;
            Self::store_milestone_released(&env, milestone_index);
        } else {
            updated_milestone.status = MilestoneStatus::PartiallyReleased;
        }

        Self::store_milestone(&env, milestone_index, &updated_milestone);

        let event_remaining = updated_milestone.amount.checked_sub(updated_milestone.released_amount).ok_or(Error::InvalidAmount)?;
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
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone.amount.checked_sub(milestone.released_amount).ok_or(Error::InvalidAmount)?;
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
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != caller && meta.freelancer != caller {
            return Err(Error::Unauthorized);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status != MilestoneStatus::Pending
            && milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        milestone.status = MilestoneStatus::Disputed;
        Self::store_milestone(&env, milestone_index, &milestone);
        Ok(())
    }

    pub fn resolve_dispute(
        env: Env,
        arbiter: Address,
        milestone_index: u32,
        release_to_freelancer: bool,
    ) -> Result<(), Error> {
        arbiter.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status != MilestoneStatus::Disputed {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone.amount - milestone.released_amount;
        let token_client = token::Client::new(&env, &meta.token);
        if release_to_freelancer {
            if remaining > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &meta.freelancer,
                    &remaining,
                );
                milestone.released_amount = milestone.amount;
            }
            milestone.status = MilestoneStatus::Released;
        } else {
            if remaining > 0 {
                token_client.transfer(&env.current_contract_address(), &meta.client, &remaining);
            }
            milestone.status = MilestoneStatus::Refunded;
        }

        Self::store_milestone(&env, milestone_index, &milestone);
        Ok(())
    }

    pub fn get_job(env: Env) -> Result<Job, Error> {
        let meta = Self::load_job_meta(&env)?;
        Self::assemble_job(&env, &meta)
    }
}

mod test;
