#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, contracterror,
    Address, Env, Vec, token,
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
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MilestoneStatus {
    Pending,
    Delivered,
    Released,
    Disputed,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Milestone {
    pub amount: i128,
    pub status: MilestoneStatus,
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
}

#[contracttype]
pub enum DataKey {
    Job,
}

#[contract]
pub struct MilestoneEscrow;

#[contractimpl]
impl MilestoneEscrow {
    pub fn initialize(
        env: Env,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        milestone_amounts: Vec<i128>,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Job) {
            return Err(Error::AlreadyInitialized);
        }

        let mut milestones: Vec<Milestone> = Vec::new(&env);
        for amount in milestone_amounts.iter() {
            milestones.push_back(Milestone {
                amount,
                status: MilestoneStatus::Pending,
            });
        }

        let job = Job {
            client,
            freelancer,
            arbiter,
            token,
            milestones,
            funded: false,
        };

        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn fund(env: Env, client: Address) -> Result<(), Error> {
        client.require_auth();
        let mut job: Job = env.storage().instance().get(&DataKey::Job)
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

    pub fn mark_delivered(env: Env, freelancer: Address, milestone_index: u32) -> Result<(), Error> {
        freelancer.require_auth();
        let mut job: Job = env.storage().instance().get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }
        if !job.funded {
            return Err(Error::NotFunded);
        }

        let mut milestone = job.milestones.get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }

        milestone.status = MilestoneStatus::Delivered;
        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn approve_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        client.require_auth();
        let mut job: Job = env.storage().instance().get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.client != client {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job.milestones.get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }

        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&env.current_contract_address(), &job.freelancer, &milestone.amount);

        milestone.status = MilestoneStatus::Released;
        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn raise_dispute(env: Env, caller: Address, milestone_index: u32) -> Result<(), Error> {
        caller.require_auth();
        let mut job: Job = env.storage().instance().get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.client != caller && job.freelancer != caller {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job.milestones.get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Pending && milestone.status != MilestoneStatus::Delivered {
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
        let mut job: Job = env.storage().instance().get(&DataKey::Job)
            .ok_or(Error::NotInitialized)?;

        if job.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }

        let mut milestone = job.milestones.get(milestone_index)
            .ok_or(Error::InvalidMilestone)?;

        if milestone.status != MilestoneStatus::Disputed {
            return Err(Error::InvalidStatus);
        }

        let token_client = token::Client::new(&env, &job.token);
        if release_to_freelancer {
            token_client.transfer(&env.current_contract_address(), &job.freelancer, &milestone.amount);
            milestone.status = MilestoneStatus::Released;
        } else {
            token_client.transfer(&env.current_contract_address(), &job.client, &milestone.amount);
            milestone.status = MilestoneStatus::Refunded;
        }

        job.milestones.set(milestone_index, milestone);
        env.storage().instance().set(&DataKey::Job, &job);
        Ok(())
    }

    pub fn get_job(env: Env) -> Result<Job, Error> {
        env.storage().instance().get(&DataKey::Job)
            .ok_or(Error::NotInitialized)
    }
}

mod test;
