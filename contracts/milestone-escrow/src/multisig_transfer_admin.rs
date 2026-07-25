use crate::{DataKey, Error, MilestoneEscrow};
use soroban_sdk::{contracttype, symbol_short, Address, Env, Vec};

pub const PROPOSAL_TTL_SECONDS: u64 = 604_800; // 7 days

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferProposal {
    pub new_admins: Vec<Address>,
    pub new_threshold: u32,
    pub approvals: Vec<Address>,
    pub proposed_at: u64,
}

pub fn get_admins(env: &Env) -> Result<Vec<Address>, Error> {
    env.storage()
        .persistent()
        .get(&DataKey::Admins)
        .ok_or(Error::NotInitialized)
}

pub fn get_admin_threshold(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::AdminThreshold)
        .unwrap_or(1)
}

pub fn propose_admin_transfer(
    env: Env,
    proposer: Address,
    new_admins: Vec<Address>,
    new_threshold: u32,
) -> Result<(), Error> {
    proposer.require_auth();

    // 1. Proposer Authorization
    let current_admins = get_admins(&env)?;
    if !current_admins.contains(&proposer) {
        return Err(Error::Unauthorized);
    }

    // 2. Validate New Admin Addresses & Threshold
    if new_admins.len() == 0 || new_threshold == 0 || new_threshold > new_admins.len() {
        return Err(Error::InvalidAmount);
    }
    for admin in new_admins.iter() {
        MilestoneEscrow::validate_address(&env, &admin)?;
    }

    // 3. Prevent conflicting active proposals (unless expired)
    if env.storage().persistent().has(&DataKey::AdminProposal) {
        let existing: AdminTransferProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AdminProposal)
            .unwrap();
        let current_time = env.ledger().timestamp();
        if current_time < existing.proposed_at + PROPOSAL_TTL_SECONDS {
            return Err(Error::ProposalPending);
        }
    }

    // 4. Create Proposal (Proposer is the first approval)
    let mut approvals = Vec::new(&env);
    approvals.push_back(proposer.clone());

    let proposal = AdminTransferProposal {
        new_admins: new_admins.clone(),
        new_threshold,
        approvals: approvals.clone(),
        proposed_at: env.ledger().timestamp(),
    };

    // 5. Check if threshold is already met (e.g. 1-of-1 proposing a new set).
    //    If so, execute immediately instead of leaving a pending proposal that
    //    would block any subsequent propose call with ProposalPending.
    let current_threshold = get_admin_threshold(&env);
    if approvals.len() >= current_threshold {
        // Execute immediately
        if let Some(first_new_admin) = proposal.new_admins.get(0) {
            env.storage()
                .persistent()
                .set(&DataKey::Admin, &first_new_admin);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Admins, &proposal.new_admins);
        env.storage()
            .persistent()
            .set(&DataKey::AdminThreshold, &proposal.new_threshold);
        // No proposal stored — remove any stale entry just in case
        if env.storage().persistent().has(&DataKey::AdminProposal) {
            env.storage().persistent().remove(&DataKey::AdminProposal);
        }

        env.events().publish(
            (symbol_short!("exec_adm"),),
            (proposal.new_admins, proposal.new_threshold),
        );
    } else {
        env.storage()
            .persistent()
            .set(&DataKey::AdminProposal, &proposal);

        // Emit proposal event
        env.events().publish(
            (symbol_short!("prop_adm"), proposer),
            (proposal.proposed_at,),
        );
    }

    Ok(())
}

pub fn approve_admin_transfer(env: Env, approver: Address) -> Result<(), Error> {
    approver.require_auth();

    // 1. Voter Authorization
    let current_admins = get_admins(&env)?;
    if !current_admins.contains(&approver) {
        return Err(Error::Unauthorized);
    }

    // 2. Load Active Proposal
    let mut proposal: AdminTransferProposal = env
        .storage()
        .persistent()
        .get(&DataKey::AdminProposal)
        .ok_or(Error::ProposalNotFound)?;

    // 3. Expiration Check
    let current_time = env.ledger().timestamp();
    if current_time >= proposal.proposed_at + PROPOSAL_TTL_SECONDS {
        env.storage().persistent().remove(&DataKey::AdminProposal);
        return Err(Error::ProposalExpired);
    }

    // 4. Double-voting Check
    if proposal.approvals.contains(&approver) {
        return Err(Error::AlreadyApproved);
    }

    // 5. Add Approval (Effect)
    proposal.approvals.push_back(approver.clone());

    // 6. Check Threshold
    let threshold = get_admin_threshold(&env);
    if proposal.approvals.len() >= threshold {
        // Threshold met! Execute the transfer.
        if let Some(first_new_admin) = proposal.new_admins.get(0) {
            env.storage()
                .persistent()
                .set(&DataKey::Admin, &first_new_admin);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Admins, &proposal.new_admins);
        env.storage()
            .persistent()
            .set(&DataKey::AdminThreshold, &proposal.new_threshold);
        env.storage().persistent().remove(&DataKey::AdminProposal);

        // Emit completion event
        env.events().publish(
            (symbol_short!("exec_adm"),),
            (proposal.new_admins, proposal.new_threshold),
        );
    } else {
        // Save updated approvals list
        env.storage()
            .persistent()
            .set(&DataKey::AdminProposal, &proposal);

        // Emit approval event
        env.events().publish(
            (symbol_short!("appr_adm"), approver),
            (proposal.approvals.len(),),
        );
    }

    Ok(())
}

pub fn revoke_admin_approval(env: Env, admin: Address) -> Result<(), Error> {
    admin.require_auth();

    // 1. Voter Authorization
    let current_admins = get_admins(&env)?;
    if !current_admins.contains(&admin) {
        return Err(Error::Unauthorized);
    }

    // 2. Load Active Proposal
    let mut proposal: AdminTransferProposal = env
        .storage()
        .persistent()
        .get(&DataKey::AdminProposal)
        .ok_or(Error::ProposalNotFound)?;

    // 3. Expiration Check
    let current_time = env.ledger().timestamp();
    if current_time >= proposal.proposed_at + PROPOSAL_TTL_SECONDS {
        env.storage().persistent().remove(&DataKey::AdminProposal);
        return Err(Error::ProposalExpired);
    }

    // 4. Find and remove approval
    if let Some(index) = proposal.approvals.iter().position(|a| a == admin) {
        proposal.approvals.remove(index as u32);
        env.storage()
            .persistent()
            .set(&DataKey::AdminProposal, &proposal);
        Ok(())
    } else {
        Err(Error::Unauthorized)
    }
}
