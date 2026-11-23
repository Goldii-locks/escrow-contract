#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, xdr::ToXdr, Address, Bytes,
    BytesN, ContractExecutable, Env, Vec,
};

// The escrow's constructor ABI lives in one crate so it cannot drift here.
pub use escrow_types::{EscrowSettings, FundingMode};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    Unauthorized = 3,
    TokenNotAllowed = 4,
    InvalidAmount = 5,
    InvalidDisputeTimeout = 6,
    ArithmeticOverflow = 7,
    NoPendingWasmHash = 8,
    InvalidWasmHash = 9,
    // Renumbered from 7 in #1080 because #1079 already owns 7-9 in the stack.
    Paused = 10,
}

/// Mirrors `milestone_escrow::MIN_DISPUTE_TIMEOUT_SECONDS` (1 day).
pub const MIN_DISPUTE_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;
/// Mirrors `milestone_escrow::MAX_DISPUTE_TIMEOUT_SECONDS` (180 days).
pub const MAX_DISPUTE_TIMEOUT_SECONDS: u64 = 180 * 24 * 60 * 60;
/// A proposed escrow WASM hash becomes active this long after it is proposed.
/// Seven days matches the escrow's treasury-update delay.
pub const WASM_HASH_DELAY: u64 = 7 * 24 * 60 * 60;

#[contracttype]
enum DataKey {
    Admin,
    WasmHash,
    AllowedTokens,
    PendingWasmHash,
    Paused,
}

/// Payload of the factory's `tokallow` event. The token is in the data rather
/// than a topic so indexers decode it from the payload.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenAllowedEvent {
    pub token: Address,
}

/// A hash proposed by `propose_wasm_hash` and not yet active. Also returned by
/// the `get_pending_wasm_hash` view.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingWasmHash {
    pub hash: BytesN<32>,
    pub effective_at: u64,
}

/// Payload of the factory's `whashprp` event.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmHashProposedEvent {
    pub hash: BytesN<32>,
    pub effective_at: u64,
}

/// Payload of the factory's `whashcxl` event.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmHashCancelledEvent {
    pub hash: BytesN<32>,
    pub effective_at: u64,
}

/// Payload of the factory's `whashact` event, published by the first
/// `create_escrow` at or after `effective_at`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmHashActivatedEvent {
    pub hash: BytesN<32>,
    pub effective_at: u64,
}

/// Payload of the factory's `created` event. The party addresses are indexed
/// as topics (`created`, `client`, `freelancer`, `arbiter`) so RPC `getEvents`
/// can filter by participant; the data records everything needed to audit how
/// the escrow was configured, including the exact settings and escrow WASM hash
/// used for the deployment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowCreatedEvent {
    pub escrow: Address,
    pub token: Address,
    /// Sum of all milestone amounts, in the token's smallest unit.
    pub total_amount: i128,
    pub milestone_count: u32,
    pub auto_release_seconds: u64,
    pub settings: EscrowSettings,
    /// Escrow WASM hash the factory held when it deployed this escrow.
    pub wasm_hash: BytesN<32>,
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
        env.storage().instance().set(&DataKey::Paused, &false);
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
            env.events()
                .publish((symbol_short!("tokallow"),), TokenAllowedEvent { token });
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

    /// Stop new escrows from being deployed. Existing escrows are unaffected.
    /// Calling this while already paused is a no-op.
    pub fn pause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        if Self::paused(&env) {
            return Ok(());
        }
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish((symbol_short!("paused"),), ());
        Ok(())
    }

    /// Allow new escrows to be deployed again. Calling this while not paused
    /// is a no-op.
    pub fn unpause(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        if !Self::paused(&env) {
            return Ok(());
        }
        env.storage().instance().set(&DataKey::Paused, &false);
        env.events().publish((symbol_short!("unpaused"),), ());
        Ok(())
    }

    /// Propose a new escrow WASM hash. It becomes active only after
    /// `WASM_HASH_DELAY`, giving clients time to react to an upgrade.
    ///
    /// `wasm` is the bytecode the hash refers to. Soroban SDK 28 has no view
    /// for checking whether a hash is already in the ledger, so uploading the
    /// bytes here and comparing the resulting hash is the only way to reject
    /// a typo before it can brick every later `create_escrow` call.
    pub fn propose_wasm_hash(
        env: Env,
        admin: Address,
        wasm_hash: BytesN<32>,
        wasm: Bytes,
    ) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let uploaded: BytesN<32> = env.deployer().upload_contract_wasm(wasm);
        if uploaded != wasm_hash {
            return Err(Error::InvalidWasmHash);
        }
        let effective_at = env
            .ledger()
            .timestamp()
            .checked_add(WASM_HASH_DELAY)
            .ok_or(Error::ArithmeticOverflow)?;
        let pending = PendingWasmHash {
            hash: wasm_hash.clone(),
            effective_at,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingWasmHash, &pending);
        env.events().publish(
            (symbol_short!("whashprp"),),
            WasmHashProposedEvent {
                hash: wasm_hash,
                effective_at,
            },
        );
        Ok(())
    }

    /// Cancel a pending WASM hash proposal. The current hash remains active.
    pub fn cancel_wasm_hash_proposal(env: Env, admin: Address) -> Result<(), Error> {
        Self::require_admin(&env, &admin)?;
        let pending: PendingWasmHash = env
            .storage()
            .instance()
            .get(&DataKey::PendingWasmHash)
            .ok_or(Error::NoPendingWasmHash)?;
        env.storage().instance().remove(&DataKey::PendingWasmHash);
        env.events().publish(
            (symbol_short!("whashcxl"),),
            WasmHashCancelledEvent {
                hash: pending.hash,
                effective_at: pending.effective_at,
            },
        );
        Ok(())
    }

    /// The pending WASM hash proposal, if any. Returns
    /// `Error::NoPendingWasmHash` when no proposal is outstanding.
    pub fn get_pending_wasm_hash(env: Env) -> Result<PendingWasmHash, Error> {
        env.storage()
            .instance()
            .get(&DataKey::PendingWasmHash)
            .ok_or(Error::NoPendingWasmHash)
    }

    /// Deploy a new milestone-escrow contract.
    ///
    /// The platform admin becomes the escrow admin so the platform retains
    /// override capabilities regardless of who the client is.
    /// `salt` must be unique per job for a given client. The actual deployment
    /// salt is `sha256(client.to_xdr() || salt)`, so two clients reusing the
    /// same salt cannot front-run each other for the same escrow address.
    /// Frontends can predict the address with `escrow_address`.
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

        if Self::paused(&env) {
            return Err(Error::Paused);
        }

        if milestone_amounts.is_empty() {
            return Err(Error::InvalidAmount);
        }
        if !(MIN_DISPUTE_TIMEOUT_SECONDS..=MAX_DISPUTE_TIMEOUT_SECONDS)
            .contains(&settings.dispute_timeout_seconds)
        {
            return Err(Error::InvalidDisputeTimeout);
        }

        // Summed up front for the `created` event; an overflowing total can
        // never be funded, so it is rejected like any other invalid amount.
        let milestone_count = milestone_amounts.len();
        let mut total_amount: i128 = 0;
        for amount in milestone_amounts.iter() {
            total_amount = total_amount
                .checked_add(amount)
                .ok_or(Error::InvalidAmount)?;
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

        let current_hash: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::WasmHash)
            .ok_or(Error::NotInitialized)?;

        // A proposal is used only after its timelock has elapsed. Applying it
        // here (rather than in a separate privileged call) means the stored
        // hash always matches the code used by the next deployment, and the
        // `created` event can report the exact hash that was deployed.
        let wasm_hash = if let Some(pending) = env
            .storage()
            .instance()
            .get::<_, PendingWasmHash>(&DataKey::PendingWasmHash)
        {
            if env.ledger().timestamp() >= pending.effective_at {
                env.storage()
                    .instance()
                    .set(&DataKey::WasmHash, &pending.hash);
                env.storage().instance().remove(&DataKey::PendingWasmHash);
                env.events().publish(
                    (symbol_short!("whashact"),),
                    WasmHashActivatedEvent {
                        hash: pending.hash.clone(),
                        effective_at: pending.effective_at,
                    },
                );
                pending.hash
            } else {
                current_hash
            }
        } else {
            current_hash
        };

        // Deploying runs the escrow's __constructor with these arguments, so
        // the escrow is initialised in the same call that creates it.
        let deploy_salt = Self::deploy_salt(&env, &client, &salt);
        let escrow: Address = env
            .deployer()
            .with_current_contract(deploy_salt)
            .deploy_contract(
                ContractExecutable::Wasm(wasm_hash.clone()),
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
            (symbol_short!("created"), client, freelancer, arbiter),
            EscrowCreatedEvent {
                escrow: escrow.clone(),
                token,
                total_amount,
                milestone_count,
                auto_release_seconds,
                settings,
                wasm_hash,
            },
        );

        Ok(escrow)
    }

    /// Predict the escrow address for a `(client, salt)` pair without
    /// deploying. Uses the same derivation as `create_escrow`.
    pub fn escrow_address(env: Env, client: Address, salt: BytesN<32>) -> Address {
        let deploy_salt = Self::deploy_salt(&env, &client, &salt);
        env.deployer()
            .with_current_contract(deploy_salt)
            .deployed_address()
    }

    /// Deployment salt: `sha256(client.to_xdr() || salt)`. Binding the client
    /// into the salt prevents an attacker from claiming a predictable address
    /// with their own `create_escrow` call before the intended client's call.
    fn deploy_salt(env: &Env, client: &Address, salt: &BytesN<32>) -> BytesN<32> {
        let mut preimage = client.clone().to_xdr(env);
        preimage.append(&salt.clone().to_xdr(env));
        env.crypto().sha256(&preimage).to_bytes()
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

    /// Whether new escrow deployments are currently blocked.
    pub fn is_paused(env: Env) -> bool {
        Self::paused(&env)
    }

    fn paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
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
