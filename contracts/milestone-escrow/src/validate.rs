use soroban_sdk::{contracttype, Address, Env};

use crate::errors::ContractError;
use crate::storage::DataKey;

/// Storage key used exclusively by the admin module.
#[contracttype]
#[derive(Clone)]
pub enum AdminKey {
    /// Tracks whether the contract has been paused by an admin.
    Paused,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the current admin address, or `NotInitialized` if the contract
/// has not been set up yet.
pub fn get_admin(env: &Env) -> Result<Address, ContractError> {
    let config: crate::storage::Config = env
        .storage()
        .persistent()
        .get(&DataKey::Config)
        .ok_or(ContractError::NotInitialized)?;
    Ok(config.admin)
}

/// Guard for every privileged function.
///
/// 1. Requires `caller` to authorise the current invocation (`require_auth`).
/// 2. Compares `caller` against the admin stored in config.
/// 3. Returns `Unauthorized` if they do not match, `NotInitialized` if the
///    contract has never been set up.
///
/// Usage — place at the very top of every admin-gated contract function:
///
/// ```ignore
/// pub fn some_admin_fn(env: Env, caller: Address) -> Result<(), ContractError> {
///     admin::only_admin(&env, &caller)?;
///     // ... privileged logic
/// }
/// ```
pub fn only_admin(env: &Env, caller: &Address) -> Result<(), ContractError> {
    caller.require_auth();
    let admin = get_admin(env)?;
    if *caller != admin {
        return Err(ContractError::Unauthorized);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pause / unpause
// ---------------------------------------------------------------------------

/// Pauses the contract. `caller` must be the admin.
pub fn pause(env: &Env, caller: &Address) -> Result<(), ContractError> {
    only_admin(env, caller)?;
    env.storage().instance().set(&AdminKey::Paused, &true);
    Ok(())
}

/// Unpauses the contract. `caller` must be the admin.
pub fn unpause(env: &Env, caller: &Address) -> Result<(), ContractError> {
    only_admin(env, caller)?;
    env.storage().instance().set(&AdminKey::Paused, &false);
    Ok(())
}

/// Returns `true` when the contract is currently paused.
pub fn is_paused(env: &Env) -> bool {
    env.storage()
        .instance()
        .get::<AdminKey, bool>(&AdminKey::Paused)
        .unwrap_or(false)
}

/// Returns `ContractPaused` when the contract is paused.
pub fn assert_not_paused(env: &Env) -> Result<(), ContractError> {
    if is_paused(env) {
        Err(ContractError::ContractPaused)
    } else {
        Ok(())
    }
}
