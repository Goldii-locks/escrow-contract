#![no_std]

use soroban_sdk::contracttype;

/// Shared with `milestone-escrow` and `escrow-factory` so the factory's
/// constructor arguments cannot drift from the escrow's ABI.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FundingMode {
    Upfront,
    PerMilestone,
}

/// Per-escrow settings fixed at deployment by `__constructor` (issue #665).
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EscrowSettings {
    /// `Upfront` funds the whole job at once; `PerMilestone` funds each
    /// milestone separately (issue #766).
    pub funding_mode: FundingMode,
    /// Seconds after which a dispute may be escalated (issue #784).
    pub dispute_timeout_seconds: u64,
    /// Arbiter fee on dispute resolution, in basis points, at most
    /// `MAX_ARBITER_FEE_BPS` (issue #738). 0 means no fee.
    pub arbiter_fee_bps: u32,
}
