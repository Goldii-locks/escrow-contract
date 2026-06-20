#![no_std]
use soroban_sdk::{contract, contracterror, contractimpl, contracttype, token, Address, Env, Vec};

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
pub enum DataKey {
    Job,
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
pub struct DeliveredEvent {
    pub milestone_index: u32,
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

        let mut milestones: Vec<Milestone> = Vec::new(&env);
        for amount in milestone_amounts.iter() {
            milestones.push_back(Milestone {
                amount,
                released_amount: 0,
                status: MilestoneStatus::Pending,
                delivered_at: 0,
            });
        }

        let job = Job {
            client,
            freelancer,
            arbiter,
            token,
            milestones,
            funded: false,
            auto_release_seconds,
        };

        env.storage().instance().set(&DataKey::Job, &job);
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
        client.require_auth();
        let mut job: Job = env
            .storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.funded {
            return Err(Error::AlreadyFunded);
        }
        if job.client != client {
            return Err(Error::Unauthorized);
        }

        let total: i128 = job.milestones.iter().map(|m| m.amount).sum();
        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&client, &env.current_contract_address(), &total);

        job.funded = true;
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn mark_delivered(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        freelancer.require_auth();
        let mut job: Job = env
            .storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }
        if !job.funded {
            return Err(Error::NotFunded);
        }

        let mut milestone = job
            .milestones
            .get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        milestone.status = MilestoneStatus::Delivered;
        milestone.delivered_at = env.ledger().timestamp();
        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn claim_auto_release(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        freelancer.require_auth();
        let mut job: Job = env
            .storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job
            .milestones
            .get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        let deadline = milestone.delivered_at + job.auto_release_seconds;
        let current = env.ledger().timestamp();
        if current < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        let remaining = milestone.amount - milestone.released_amount;
        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&env.current_contract_address(), &job.freelancer, &remaining);

        milestone.released_amount = milestone.amount;
        milestone.status = MilestoneStatus::Released;
        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> i64 {
        let job: Job = env.storage().instance().get(&DataKey::Job).unwrap();
        let milestone = job.milestones.get(milestone_index).unwrap();
        let deadline = milestone.delivered_at + job.auto_release_seconds;
        let current = env.ledger().timestamp();
        (deadline as i64) - (current as i64)
    }

    pub fn approve_partial(
        env: Env,
        client: Address,
        milestone_index: u32,
        amount: i128,
    ) -> Result<(), Error> {
        client.require_auth();
        let mut job: Job = env
            .storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.client != client {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job
            .milestones
            .get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

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

        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&env.current_contract_address(), &job.freelancer, &amount);

        milestone.released_amount += amount;

        if milestone.released_amount == milestone.amount {
            milestone.status = MilestoneStatus::Released;
        } else {
            milestone.status = MilestoneStatus::PartiallyReleased;
        }

        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn approve_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        client.require_auth();
        let mut job: Job = env
            .storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.client != client {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job
            .milestones
            .get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone.amount - milestone.released_amount;
        if remaining > 0 {
            let token_client = token::Client::new(&env, &job.token);
            token_client.transfer(&env.current_contract_address(), &job.freelancer, &remaining);
            milestone.released_amount = milestone.amount;
        }

        milestone.status = MilestoneStatus::Released;
        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn raise_dispute(env: Env, caller: Address, milestone_index: u32) -> Result<(), Error> {
        caller.require_auth();
        let mut job: Job = env
            .storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.client != caller && job.freelancer != caller {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job
            .milestones
            .get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Pending
            && milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        milestone.status = MilestoneStatus::Disputed;
        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn resolve_dispute(
        env: Env,
        arbiter: Address,
        milestone_index: u32,
        release_to_freelancer: bool,
    ) -> Result<(), Error> {
        arbiter.require_auth();
        let mut job: Job = env
            .storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job
            .milestones
            .get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Disputed {
            return Err(Error::InvalidStatus);
        }

        let remaining = milestone.amount - milestone.released_amount;
        let token_client = token::Client::new(&env, &job.token);
        if release_to_freelancer {
            if remaining > 0 {
                token_client.transfer(&env.current_contract_address(), &job.freelancer, &remaining);
                milestone.released_amount = milestone.amount;
            }
            milestone.status = MilestoneStatus::Released;
        } else {
            if remaining > 0 {
                token_client.transfer(&env.current_contract_address(), &job.client, &remaining);
            }
            milestone.status = MilestoneStatus::Refunded;
        }

        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn get_job(env: Env) -> Result<Job, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Job)
            .ok_or(Error::NotInitialized)
    }
}

mod test;
