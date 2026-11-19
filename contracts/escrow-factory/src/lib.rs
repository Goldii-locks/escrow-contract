#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN,
    ContractExecutable, Env, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    Unauthorized = 3,
    TokenNotAllowed = 4,
    InvalidAmount = 5,
}

#[contracttype]
enum DataKey {
    Admin,
    WasmHash,
    AllowedTokens,
}

/// Mirrors `milestone_escrow::FundingMode`; the two encode identically.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FundingMode {
    Upfront,
    PerMilestone,
}

/// Mirrors `milestone_escrow::EscrowSettings`, the escrow constructor's last
/// argument; the two encode identically.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EscrowSettings {
    pub funding_mode: FundingMode,
    pub dispute_timeout_seconds: u64,
    pub arbiter_fee_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCreatedEvent {
    pub escrow: Address,
    pub client: Address,
    pub freelancer: Address,
    pub arbiter: Address,
    pub token: Address,
}

#[contract]
pub struct EscrowFactory;

// Events keep the escrow's `publish` wire format (see milestone-escrow).
#[allow(deprecated)]
#[contractimpl]
impl EscrowFactory {
    /// Store the platform admin and the milestone-escrow wasm hash at deploy
    /// time, so no one can initialise a freshly deployed factory first (the
    /// escrow's own #665 fix, applied here).
    pub fn __constructor(env: Env, admin: Address, wasm_hash: BytesN<32>) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::WasmHash, &wasm_hash);
        env.storage()
            .instance()
            .set(&DataKey::AllowedTokens, &Vec::<Address>::new(&env));
    }

    /// Allow a token to be used in escrows created by this factory.
    pub fn allow_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let mut tokens = Self::load_tokens(&env)?;
        if !tokens.contains(&token) {
            tokens.push_back(token.clone());
            env.storage()
                .instance()
                .set(&DataKey::AllowedTokens, &tokens);
            env.events().publish((symbol_short!("allowed"), token), ());
        }
        Ok(())
    }

    /// Remove a token from the factory's allowlist. Removing an absent token
    /// is a no-op. Existing escrows are unaffected: each escrow stores its own
    /// token, so only future `create_escrow` calls are restricted.
    pub fn disallow_token(env: Env, admin: Address, token: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let mut tokens = Self::load_tokens(&env)?;
        if let Some(index) = tokens.first_index_of(&token) {
            tokens.remove(index);
            env.storage()
                .instance()
                .set(&DataKey::AllowedTokens, &tokens);
        }
        env.events()
            .publish((symbol_short!("disallowd"), token), ());
        Ok(())
    }

    /// Update the wasm hash (e.g. after an escrow contract upgrade).
    pub fn set_wasm_hash(env: Env, admin: Address, wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        env.storage().instance().set(&DataKey::WasmHash, &wasm_hash);
        Ok(())
    }

    /// Deploy a new milestone-escrow contract.
    ///
    /// The platform admin becomes the escrow admin so the platform retains
    /// override capabilities regardless of who the client is.
    /// `salt` must be unique per (client, job) to derive a deterministic address.
    #[allow(clippy::too_many_arguments)]
    pub fn create_escrow(
        env: Env,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        settings: EscrowSettings,
        salt: BytesN<32>,
    ) -> Result<Address, Error> {
        client.require_auth();

        if milestone_amounts.is_empty() {
            return Err(Error::InvalidAmount);
        }

        let tokens = Self::load_tokens(&env)?;
        if !tokens.contains(&token) {
            return Err(Error::TokenNotAllowed);
        }

        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        let wasm_hash: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::WasmHash)
            .ok_or(Error::NotInitialized)?;

        // Deploying runs the escrow's __constructor with these arguments, so
        // the escrow is initialised in the same call that creates it.
        let escrow: Address = env.deployer().with_current_contract(salt).deploy_contract(
            ContractExecutable::Wasm(wasm_hash),
            (
                admin.clone(),
                client.clone(),
                freelancer.clone(),
                arbiter.clone(),
                token.clone(),
                auto_release_seconds,
                milestone_amounts,
                milestone_spec_hashes,
                settings,
            ),
        );

        env.events().publish(
            (symbol_short!("created"),),
            EscrowCreatedEvent {
                escrow: escrow.clone(),
                client,
                freelancer,
                arbiter,
                token,
            },
        );

        Ok(escrow)
    }

    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    pub fn get_wasm_hash(env: Env) -> Result<BytesN<32>, Error> {
        env.storage()
            .instance()
            .get(&DataKey::WasmHash)
            .ok_or(Error::NotInitialized)
    }

    pub fn get_allowed_tokens(env: Env) -> Result<Vec<Address>, Error> {
        Self::load_tokens(&env)
    }

    fn require_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        admin.require_auth();
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        if stored != *admin {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    fn load_tokens(env: &Env) -> Result<Vec<Address>, Error> {
        env.storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .ok_or(Error::NotInitialized)
    }
}

#[cfg(test)]
mod test;
