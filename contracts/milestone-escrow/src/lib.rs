#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env, Vec,
};

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
pub struct FundedEvent {
    pub total_amount: i128,
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
pub struct ApprovedEvent {
    pub milestone_index: u32,
    pub amount: i128,
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

    /// Initializes the escrow contract with all job parties and milestone configuration.
    ///
    /// Must be called exactly once before any other function. Sets the admin, whitelists
    /// the token, and creates one [`Milestone`] per entry in `milestone_amounts`.
    ///
    /// # Parameters
    /// - `admin`: Address granted token-whitelist management rights.
    /// - `client`: Address of the party funding the job and approving milestones.
    /// - `freelancer`: Address of the party doing the work and receiving payments.
    /// - `arbiter`: Address empowered to resolve disputes.
    /// - `token`: SAC token address used for all payments; added to the whitelist automatically.
    /// - `auto_release_seconds`: Seconds after `mark_delivered` before the freelancer can
    ///   self-release without client approval via [`claim_auto_release`].
    /// - `milestone_amounts`: Ordered list of per-milestone payment amounts (in token stroops).
    ///
    /// # Errors
    /// - [`Error::AlreadyInitialized`] if the contract has already been set up.
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

        env.storage().instance().set(&DataKey::Admin, &admin);

        let mut whitelist: Vec<Address> = Vec::new(&env);
        whitelist.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);

        let milestone_count = milestone_amounts.len();
        let mut total_amount: i128 = 0;
        for (index, amount) in milestone_amounts.iter().enumerate() {
            total_amount += amount;
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

    /// Adds a token address to the whitelist of accepted payment tokens.
    ///
    /// # Parameters
    /// - `admin`: Must match the admin set during [`initialize`]; requires auth.
    /// - `token`: Token address to whitelist.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `admin` does not match the stored admin.
    /// - [`Error::TokenAlreadyWhitelisted`] if the token is already in the whitelist.
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

        whitelist.push_back(token);
        env.storage()
            .instance()
            .set(&DataKey::WhitelistedTokens, &whitelist);
        Ok(())
    }

    /// Removes a token address from the whitelist of accepted payment tokens.
    ///
    /// # Parameters
    /// - `admin`: Must match the admin set during [`initialize`]; requires auth.
    /// - `token`: Token address to remove.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `admin` does not match the stored admin.
    /// - [`Error::TokenNotWhitelisted`] if the token is not currently in the whitelist.
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

    /// Returns `true` if `token` is in the whitelist, `false` otherwise (including when
    /// the contract is uninitialized).
    ///
    /// # Parameters
    /// - `token`: Token address to check.
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

    /// Returns the full list of whitelisted token addresses.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    pub fn get_whitelisted_tokens(env: Env) -> Result<Vec<Address>, Error> {
        env.storage()
            .instance()
            .get(&DataKey::WhitelistedTokens)
            .ok_or(Error::NotInitialized)
    }

    /// Transfers the total milestone sum from the client into the contract.
    ///
    /// Must be called by the client before any work can be delivered. Requires a token
    /// approval (allowance) from `client` to the contract address for `total_amount`.
    ///
    /// # Parameters
    /// - `client`: Must match the client set during [`initialize`]; requires auth.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::AlreadyFunded`] if the contract has already been funded.
    /// - [`Error::Unauthorized`] if `client` does not match the stored client.
    pub fn fund(env: Env, client: Address) -> Result<(), Error> {
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if meta.funded {
            return Err(Error::AlreadyFunded);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, &env.current_contract_address(), &meta.total_amount);

        meta.funded = true;
        Self::store_job_meta(&env, &meta);
        Ok(())
    }

    /// Marks a milestone as delivered, starting the auto-release countdown.
    ///
    /// Transitions the milestone from `Pending` → `Delivered` and records
    /// `delivered_at` from the current ledger timestamp. Emits a `deliver` event.
    ///
    /// # Parameters
    /// - `freelancer`: Must match the freelancer set during [`initialize`]; requires auth.
    /// - `milestone_index`: Zero-based index of the milestone to mark delivered.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `freelancer` does not match the stored freelancer.
    /// - [`Error::NotFunded`] if the contract has not been funded yet.
    /// - [`Error::InvalidMilestone`] if `milestone_index` is out of range.
    /// - [`Error::InvalidStatus`] if the milestone is not in `Pending` status.
    pub fn mark_delivered(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
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

        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        let delivered_at = env.ledger().timestamp();
        milestone.status = MilestoneStatus::Delivered;
        milestone.delivered_at = delivered_at;
        Self::store_milestone(&env, milestone_index, &milestone);

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

    /// Releases a delivered milestone to the freelancer without client approval once the
    /// auto-release deadline has passed (`delivered_at + auto_release_seconds`).
    ///
    /// Transitions the milestone to `Released` and transfers any unreleased amount to the
    /// freelancer.
    ///
    /// # Parameters
    /// - `freelancer`: Must match the freelancer set during [`initialize`]; requires auth.
    /// - `milestone_index`: Zero-based index of the milestone to auto-release.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `freelancer` does not match the stored freelancer.
    /// - [`Error::InvalidMilestone`] if `milestone_index` is out of range.
    /// - [`Error::InvalidStatus`] if the milestone is not in `Delivered` status.
    /// - [`Error::DeadlineNotPassed`] if the auto-release deadline has not yet elapsed.
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

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        let deadline = milestone.delivered_at + meta.auto_release_seconds;
        let current = env.ledger().timestamp();
        if current < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        let remaining = milestone.amount - milestone.released_amount;
        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(
            &env.current_contract_address(),
            &meta.freelancer,
            &remaining,
        );

        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Ok(())
    }

    /// Returns the number of seconds remaining until the auto-release deadline for a milestone.
    ///
    /// A negative value means the deadline has already passed and [`claim_auto_release`] can
    /// be called. Panics if the contract is uninitialized or `milestone_index` is invalid.
    ///
    /// # Parameters
    /// - `milestone_index`: Zero-based index of the milestone to query.
    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> i64 {
        let meta = Self::load_job_meta(&env).unwrap();
        let milestone = Self::load_milestone(&env, milestone_index).unwrap();
        let deadline = milestone.delivered_at + meta.auto_release_seconds;
        let current = env.ledger().timestamp();
        (deadline as i64) - (current as i64)
    }

    /// Releases a partial amount from a delivered (or partially-released) milestone to the freelancer.
    ///
    /// Transitions the milestone to `PartiallyReleased` if funds remain, or `Released` if the
    /// full amount has now been paid.
    ///
    /// # Parameters
    /// - `client`: Must match the client set during [`initialize`]; requires auth.
    /// - `milestone_index`: Zero-based index of the milestone to partially approve.
    /// - `amount`: Token amount (in stroops) to release; must be > 0 and ≤ unreleased remainder.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `client` does not match the stored client.
    /// - [`Error::InvalidMilestone`] if `milestone_index` is out of range.
    /// - [`Error::InvalidStatus`] if the milestone is not in `Delivered` or `PartiallyReleased` status.
    /// - [`Error::InvalidAmount`] if `amount` is ≤ 0 or exceeds the unreleased remainder.
    pub fn approve_partial(
        env: Env,
        client: Address,
        milestone_index: u32,
        amount: i128,
    ) -> Result<(), Error> {
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let remaining = milestone.amount - milestone.released_amount;
        if amount > remaining {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &meta.freelancer, &amount);

        milestone.released_amount += amount;

        if milestone.released_amount == milestone.amount {
            milestone.status = MilestoneStatus::Released;
        } else {
            milestone.status = MilestoneStatus::PartiallyReleased;
        }

        Self::store_milestone(&env, milestone_index, &milestone);
        Ok(())
    }

    /// Releases the full remaining balance of a delivered milestone to the freelancer.
    ///
    /// Transitions the milestone to `Released` and transfers all unreleased funds to the
    /// freelancer in one step.
    ///
    /// # Parameters
    /// - `client`: Must match the client set during [`initialize`]; requires auth.
    /// - `milestone_index`: Zero-based index of the milestone to approve.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `client` does not match the stored client.
    /// - [`Error::InvalidMilestone`] if `milestone_index` is out of range.
    /// - [`Error::InvalidStatus`] if the milestone is not in `Delivered` or `PartiallyReleased` status.
    pub fn approve_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;

        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone.amount - milestone.released_amount;
        if remaining > 0 {
            let token_client = token::Client::new(&env, &meta.token);
            token_client.transfer(
                &env.current_contract_address(),
                &meta.freelancer,
                &remaining,
            );
            milestone.released_amount = milestone.amount;
        }

        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Ok(())
    }

    /// Freezes a milestone for arbitration by transitioning it to `Disputed` status.
    ///
    /// Either the client or freelancer may raise a dispute on a milestone that is
    /// `Pending`, `Delivered`, or `PartiallyReleased`. Once disputed, only the arbiter
    /// can unblock it via [`resolve_dispute`].
    ///
    /// # Parameters
    /// - `caller`: Must be either the client or freelancer; requires auth.
    /// - `milestone_index`: Zero-based index of the milestone to dispute.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `caller` is neither the client nor the freelancer.
    /// - [`Error::InvalidMilestone`] if `milestone_index` is out of range.
    /// - [`Error::InvalidStatus`] if the milestone is already `Released`, `Refunded`, or `Disputed`.
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

    /// Resolves a disputed milestone, releasing funds to either the freelancer or the client.
    ///
    /// Only callable by the arbiter. If `release_to_freelancer` is `true`, the remaining
    /// balance is sent to the freelancer and the milestone transitions to `Released`.
    /// Otherwise, the remaining balance is returned to the client and the milestone
    /// transitions to `Refunded`.
    ///
    /// # Parameters
    /// - `arbiter`: Must match the arbiter set during [`initialize`]; requires auth.
    /// - `milestone_index`: Zero-based index of the disputed milestone to resolve.
    /// - `release_to_freelancer`: `true` to pay the freelancer; `false` to refund the client.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    /// - [`Error::Unauthorized`] if `arbiter` does not match the stored arbiter.
    /// - [`Error::InvalidMilestone`] if `milestone_index` is out of range.
    /// - [`Error::InvalidStatus`] if the milestone is not in `Disputed` status.
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

    /// Returns the full [`Job`] state, including all milestones and their current statuses.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] if the contract has not been initialized.
    pub fn get_job(env: Env) -> Result<Job, Error> {
        let meta = Self::load_job_meta(&env)?;
        Self::assemble_job(&env, &meta)
    }
}

mod test;
