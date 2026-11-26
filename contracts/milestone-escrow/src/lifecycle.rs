#[allow(unused_imports)]
use crate::{
    events::*,
    math,
    types::*,
    MilestoneEscrow,
};
#[allow(unused_imports)]
use soroban_sdk::{
    contractimpl, symbol_short, token, Address, BytesN, ContractExecutable, Env, IntoVal, Map,
    String, Symbol, TryFromVal, Val, Vec,
};

#[allow(deprecated)]
#[contractimpl]
impl MilestoneEscrow {
    /// Initialize a new milestone escrow job.
    ///
    /// Sets up the client/freelancer/arbiter relationship, the settlement
    /// token, and the milestone schedule. Must be called exactly once; every
    /// other state-mutating endpoint checks for prior initialization and
    /// returns `NotInitialized` if this call was never made. The settlement
    /// token is automatically added to the whitelist, and the platform fee
    /// allocation defaults to 100 % freelancer / 0 % client / 0 % treasury.
    ///
    /// All integer arithmetic (summing `milestone_amounts`) is performed via
    /// the internal `checked_initialize_total` helper, which uses
    /// `i128::checked_add` on every addition and rejects non-positive amounts,
    /// so no arithmetic can overflow or wrap silently — a typed `InvalidAmount`
    /// error is returned instead.
    ///
    /// # Parameters
    /// * `admin`                – Address that will control admin-only
    ///                           endpoints (whitelist management, pause /
    ///                           resume, admin overrides, yield config).
    ///                           Must authorize the call.
    /// * `client`               – Address that funds the job and approves
    ///                           milestone releases.
    /// * `freelancer`           – Address that delivers milestones and
    ///                           receives payouts.
    /// * `arbiter`              – Address that resolves disputes via
    ///                           `resolve_dispute`.
    /// * `token`                – Settlement token contract address used for
    ///                           all deposits and payouts.
    /// * `auto_release_seconds` – Seconds after a milestone is marked
    ///                           delivered before it becomes eligible for
    ///                           `claim_auto_release`.  Must be non-zero.
    ///                           The default dispute timeout is 30 days; use
    ///                           `initialize_with_dispute_timeout` to set a
    ///                           custom timeout.
    /// * `milestone_amounts`    – Ordered list of token amounts owed per
    ///                           milestone.  Must be non-empty; every
    ///                           individual amount must be strictly positive
    ///                           (`> 0`).
    /// * `milestone_spec_hashes` – Ordered 32-byte hashes of the corresponding
    ///                            off-chain milestone descriptions.
    ///
    /// # Returns
    /// `Ok(())` on success.  At that point the following state has been
    /// committed atomically:
    /// * `DataKey::Job` (instance storage) – `JobMeta` with the supplied
    ///   parties, token, `auto_release_seconds`, milestone count, and the sum
    ///   of `milestone_amounts` as `total_amount`.
    /// * `DataKey::Milestone(i)` (persistent storage) – one `Milestone` entry
    ///   per element of `milestone_amounts`, each starting in
    ///   `MilestoneStatus::Pending` with `released_amount = 0`.
    /// * Each `Milestone.spec_hash` – the corresponding hash from
    ///   `milestone_spec_hashes`.
    /// * `DataKey::Admin` (instance storage only) – the `admin`
    ///   address.
    /// * `DataKey::Version` (instance storage) – version marker `1u32`.
    /// * `DataKey::Ep` (instance storage) – emergency-pause flag set to
    ///   `false`.
    /// * `DataKey::PlatformFeeAllocation` (persistent storage) – fee allocation
    ///   defaulting to 100 % freelancer / 0 % client / 0 % treasury.
    /// * `DataKey::WhitelistedTokens` (persistent storage) – whitelist
    ///   initialized with `token` as its sole entry.
    ///
    /// An `"init"` event carrying an [`InitializedEvent`] is published after
    /// all state is written. It includes the full `milestone_amounts` vec and
    /// corresponding `milestone_spec_hashes`.
    ///
    /// On any error path the Soroban host rolls back all storage writes made
    /// during this invocation, so the contract remains in its uninitialized state
    /// and a subsequent valid call will succeed.
    ///
    /// # Errors
    /// Errors are returned in the order the corresponding checks appear in the
    /// function body.
    ///
    /// * [`Error::AlreadyInitialized`] – `DataKey::Job` is already present in
    ///   instance storage, meaning `initialize` has already completed
    ///   successfully (or a prior attempt wrote the reentrancy sentinel and the
    ///   transaction was not rolled back).  No state is mutated.
    ///
    /// * [`Error::InvalidAddress`] – Any address argument fails the internal
    ///   `validate_address` check, which rejects:
    ///   - The Stellar zero account,
    ///   - The canonical Soroban zero contract address,
    ///   - The escrow contract's own address
    ///     (`env.current_contract_address()`).
    ///   The check is applied in parameter order: `admin`, `client`,
    ///   `freelancer`, `arbiter`, `token`.
    ///
    /// * [`Error::InvalidParties`] – Two or more party addresses collide.
    ///   The escrow trust model requires four distinct addresses: the admin
    ///   (platform), client, freelancer, and arbiter (neutral dispute
    ///   resolver) must all be different.  Any of the following collisions
    ///   is rejected: `client == freelancer`, `arbiter == client`,
    ///   `arbiter == freelancer`, `admin == client`, `admin == freelancer`.
    ///   The check runs immediately after the zero-address validation so a
    ///   collision is caught before any milestone amount arithmetic runs.
    ///
    /// * [`Error::InvalidAmount`] – Any of the following conditions:
    ///   - `milestone_amounts` is empty (no milestones to escrow).
    ///   - `milestone_spec_hashes.len()` differs from
    ///     `milestone_amounts.len()`.
    ///   - Any individual milestone amount is `≤ 0` (zero or negative amounts
    ///     are not valid escrow values).
    ///   - The sum of all milestone amounts overflows `i128`.
    ///
    /// * [`Error::InvalidDuration`] – `auto_release_seconds` is `0` or exceeds
    ///   `MAX_AUTO_RELEASE_SECONDS` (10 years), which would disable the
    ///   auto-release window or risk arithmetic overflow in deadline
    ///   calculations.
    ///
    /// * [`Error::InvalidMilestone`] – An element of `milestone_amounts` could
    ///   not be retrieved by index during the milestone-storage loop.  In
    ///   practice this indicates an internal SDK-level error rather than a
    ///   caller mistake.
    ///
    /// Unit tests are built with a no-argument constructor instead (below), so
    /// they can register the contract and call the test-only `initialize`;
    /// `constructor_tests` deploys a release build to cover this one.
    #[cfg(not(test))]
    #[allow(clippy::too_many_arguments)]
    pub fn __constructor(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        settings: EscrowSettings,
    ) -> Result<(), Error> {
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            settings,
        )
    }

    /// Test builds only: lets unit tests register the contract without
    /// constructor arguments and initialise it through `initialize`.
    #[cfg(test)]
    pub fn __constructor(_env: Env) {}

    /// Test-only initialiser with default settings, for tests that register
    /// the contract without constructor arguments.
    #[cfg(test)]
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
        milestone_spec_hashes: Vec<BytesN<32>>,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            Self::default_settings(),
        )
    }

    /// Test-only: `initialize` with a custom dispute timeout.
    ///
    /// After `dispute_timeout_seconds`, either the client or freelancer may
    /// call `escalate_dispute` to restore the milestone to its status before
    /// the dispute was raised. The timeout must be within
    /// `MIN_DISPUTE_TIMEOUT_SECONDS..=MAX_DISPUTE_TIMEOUT_SECONDS`.
    ///
    /// # Errors
    /// * [`Error::InvalidDisputeTimeout`] – `dispute_timeout_seconds` is out
    ///   of range.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_with_dispute_timeout(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        dispute_timeout_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            EscrowSettings {
                dispute_timeout_seconds,
                ..Self::default_settings()
            },
        )
    }

    /// Test-only: `initialize` with an explicit funding mode.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_with_funding_mode(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        funding_mode: FundingMode,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            EscrowSettings {
                funding_mode,
                ..Self::default_settings()
            },
        )
    }

    /// Test-only: `initialize` with an arbiter fee (issue #738).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_with_arbiter_fee(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        arbiter_fee_bps: u32,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::initialize_with_settings(
            env,
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            milestone_amounts,
            milestone_spec_hashes,
            EscrowSettings {
                arbiter_fee_bps,
                ..Self::default_settings()
            },
        )
    }

    /// The arbiter fee in basis points (issue #738); 0 when none was set.
    pub fn get_arbiter_fee_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ArbiterFeeBps)
            .unwrap_or(0)
    }

    /// The single settlement token fixed for this escrow at creation (issue #680).
    ///
    /// Read-only replacement for the legacy per-escrow whitelist: token
    /// admission is enforced platform-wide by the factory's allow-list, not by
    /// the escrow's whitelist, so callers should use this getter when they need
    /// the currency a job actually settles in.
    pub fn token(env: Env) -> Result<Address, Error> {
        Ok(Self::load_job_meta(&env)?.token)
    }

    /// Deposit the full escrow amount into the contract.
    ///
    /// Transfers the sum of all milestone amounts from the client to the
    /// contract in a single token transfer. The `funded` flag is set before
    /// the transfer is executed to prevent reentrant double-funding. Must be
    /// called once, after `initialize` and before any milestone can be
    /// delivered or approved.
    ///
    /// # Parameters
    /// * `client` – Must match the job's stored client. Must authorize the
    ///              call.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `AlreadyFunded`    – The job has already been funded.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `InvalidAddress`   – `client` is a zero address.
    /// * `InvalidAmount`    – The total milestone amount is invalid (e.g.
    ///                        overflow or non-positive).
    pub fn fund(env: Env, client: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::validate_fund_client(&env, &client)?;
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if Self::load_funding_mode(&env) != FundingMode::Upfront {
            return Err(Error::InvalidFundingMode);
        }
        if meta.funded {
            return Err(Error::AlreadyFunded);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }

        let total_amount = Self::validate_fund_amount(&meta)?;

        // Update status BEFORE token transfer to ensure state is persisted
        // and prevent double-funding via reentrancy
        meta.funded = true;
        Self::store_job_meta(&env, &meta);
        env.storage()
            .instance()
            .set(&DataKey::FundedCount, &meta.milestone_count);
        env.storage()
            .instance()
            .set(&DataKey::FundedAt, &env.ledger().timestamp());

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, env.current_contract_address(), &total_amount);

        env.events().publish(
            (
                symbol_short!("fund"),
                meta.client.clone(),
                meta.freelancer.clone(),
                meta.arbiter.clone(),
            ),
            FundedEvent {
                client,
                total_amount,
            },
        );

        Ok(())
    }

    /// Fund a single milestone when the escrow uses `FundingMode::PerMilestone`.
    pub fn fund_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;

        Self::validate_fund_client(&env, &client)?;
        client.require_auth();
        let mut meta = Self::load_job_meta(&env)?;

        if Self::load_funding_mode(&env) != FundingMode::PerMilestone {
            return Err(Error::InvalidFundingMode);
        }
        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.funded {
            return Err(Error::AlreadyFunded);
        }
        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        // Read before this milestone is marked funded: the legacy fallback
        // counts stored flags, and this milestone is still unfunded.
        let funded_count = Self::load_funded_count(&env, &meta)?
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;

        let token_client = token::Client::new(&env, &meta.token);
        let before = token_client.balance(&env.current_contract_address());
        token_client.transfer(&client, env.current_contract_address(), &milestone.amount);
        let after = token_client.balance(&env.current_contract_address());
        let received = after.checked_sub(before).unwrap_or(0);
        if received != milestone.amount {
            return Err(Error::InsufficientBalance);
        }

        milestone.funded = true;
        Self::store_milestone(&env, milestone_index, &milestone);
        // The first funded milestone starts the acceptance window (#757).
        if !env.storage().instance().has(&DataKey::FundedAt) {
            env.storage()
                .instance()
                .set(&DataKey::FundedAt, &env.ledger().timestamp());
        }

        env.storage()
            .instance()
            .set(&DataKey::FundedCount, &funded_count);
        if funded_count == meta.milestone_count {
            meta.funded = true;
            Self::store_job_meta(&env, &meta);
        }

        env.events().publish(
            (
                symbol_short!("msfund"),
                client.clone(),
                meta.freelancer.clone(),
            ),
            MilestoneFundedEvent {
                client,
                milestone_index,
                amount: milestone.amount,
                token: meta.token,
            },
        );

        Ok(())
    }

    /// Current funding mode. Missing storage defaults to `Upfront`.
    pub fn get_funding_mode(env: Env) -> Result<FundingMode, Error> {
        let _ = Self::load_job_meta(&env)?;
        Ok(Self::load_funding_mode(&env))
    }

    /// Append a new `Pending` milestone to a funded escrow (issue #737).
    ///
    /// Real freelance work changes scope after funding; without this endpoint
    /// the only ways to add work are cancelling the whole escrow or leaving
    /// the new deliverable unfunded.  Both parties must sign, and the client
    /// deposits `amount` into escrow in the same call, so the conservation
    /// invariant `total_amount == Î£ milestone.amounts` holds across the
    /// amendment: `total_amount` grows by exactly `amount` and the escrow
    /// token balance grows by exactly `amount`.
    ///
    /// # Authorization
    /// `client` and `freelancer` must both match the addresses stored on the
    /// job and both authorise the call — neither party can extend scope alone.
    ///
    /// # Parameters
    /// * `client`     – The job's client; pays the new milestone.
    /// * `freelancer` – The job's freelancer; consents to the extra scope.
    /// * `amount`     – New milestone amount, must be > 0.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked`     – Emergency pause or cancel lock is on.
    /// * `NotInitialized`              – Contract has not been initialized.
    /// * `Unauthorized`                – `client` / `freelancer` do not match
    ///                                    the stored job parties.
    /// * `NotFunded`                   – The escrow has not been funded yet;
    ///                                    extra milestones belong in the
    ///                                    `initialize` amount vector.
    /// * `InvalidAmount`               – `amount` ≤ 0, or `total_amount +
    ///                                    amount` overflows `i128`.
    /// * `TooManyMilestones`           – The job already carries
    ///                                    `MAX_MILESTONES` milestones.
    pub fn add_milestone(
        env: Env,
        client: Address,
        freelancer: Address,
        amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;

        let mut meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        if !meta.funded {
            return Err(Error::NotFunded);
        }
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if meta.milestone_count >= MAX_MILESTONES {
            return Err(Error::TooManyMilestones);
        }
        let new_total = meta
            .total_amount
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;
        let new_index = meta.milestone_count;

        Self::store_milestone(
            &env,
            new_index,
            &Milestone {
                amount,
                released_amount: 0,
                status: MilestoneStatus::Pending,
                delivered_at: 0,
                funded: true,
                spec_hash: BytesN::from_array(&env, &[0u8; 32]),
                deliverable: BytesN::from_array(&env, &[0u8; 32]),
            },
        );
        // Persist amended state BEFORE the token transfer so a reentrant call
        // sees the updated count/total (mirrors `fund`).
        meta.milestone_count = new_index + 1;
        meta.total_amount = new_total;
        Self::store_job_meta(&env, &meta);
        // The job is funded, so every milestone, including this one, holds
        // its deposit (#999).
        env.storage()
            .instance()
            .set(&DataKey::FundedCount, &meta.milestone_count);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&client, env.current_contract_address(), &amount);

        env.events().publish(
            (symbol_short!("ms_add"), client.clone(), freelancer.clone()),
            MilestoneAddedEvent {
                client,
                freelancer,
                milestone_index: new_index,
                amount,
                total_amount: new_total,
                milestone_count: meta.milestone_count,
            },
        );

        Ok(())
    }

    /// Reduce the amount of a `Pending` milestone and refund the difference to
    /// the client (issue #737).
    ///
    /// Both parties must sign.  Only `Pending` milestones can be reduced: a
    /// `Delivered` or partially-released milestone has work behind it and its
    /// remaining balance is already committed to a release/dispute path.  The
    /// refund is exact, so the conservation invariant
    /// `total_amount == Î£ milestone.amounts` holds across the amendment.
    ///
    /// # Parameters
    /// * `client`          – The job's client; receives the refund.
    /// * `freelancer`      – The job's freelancer; consents to reduced scope.
    /// * `milestone_index` – Index of the milestone to reduce.
    /// * `new_amount`      – Replacement amount.  Must be > 0 and strictly
    ///                        below the milestone's current amount.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked`     – Emergency pause or cancel lock is on.
    /// * `NotInitialized`              – Contract has not been initialized.
    /// * `Unauthorized`                – `client` / `freelancer` do not match
    ///                                    the stored job parties.
    /// * `NotFunded`                   – Nothing was ever deposited to refund.
    /// * `InvalidMilestone`            – `milestone_index` is out of range.
    /// * `InvalidStatus`               – The milestone is not `Pending`.
    /// * `InvalidAmount`               – `new_amount` ≤ 0, or it is not
    ///                                    strictly below the current amount.
    /// * `InsufficientBalance`         – The escrow holds less than the refund
    ///                                    (defensive; unreachable while the
    ///                                    conservation invariant holds).
    pub fn reduce_milestone(
        env: Env,
        client: Address,
        freelancer: Address,
        milestone_index: u32,
        new_amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;

        let mut meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

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
        if new_amount <= 0 || new_amount >= milestone.amount {
            return Err(Error::InvalidAmount);
        }
        let refund = milestone.amount - new_amount;

        // Credits owed to parties (`TotalOwed`) are not available to refund.
        let token_client = token::Client::new(&env, &meta.token);
        if Self::available_balance(&env, &token_client) < refund {
            return Err(Error::InsufficientBalance);
        }

        milestone.amount = new_amount;
        Self::store_milestone(&env, milestone_index, &milestone);
        // Persist amended state BEFORE the refund transfer so a reentrant
        // call never observes the old, larger amount (mirrors `fund`).
        meta.total_amount -= refund;
        Self::store_job_meta(&env, &meta);

        Self::pay_or_credit(&env, &token_client, &client, &refund)?;

        env.events().publish(
            (
                symbol_short!("ms_reduce"),
                client.clone(),
                freelancer.clone(),
            ),
            MilestoneReducedEvent {
                client,
                freelancer,
                milestone_index,
                old_amount: new_amount + refund,
                new_amount,
                refund,
                total_amount: meta.total_amount,
                milestone_count: meta.milestone_count,
            },
        );

        Ok(())
    }

    /// Raise the amount of a `Pending` milestone (issue #1020).
    ///
    /// Both parties must sign.  Only `Pending` milestones can be raised, as
    /// with `reduce_milestone`.  On a funded milestone the client deposits the
    /// difference in the same call and the received amount is checked, so the
    /// conservation invariant `total_amount == Σ milestone.amounts` holds
    /// across the amendment.  In per-milestone mode an unfunded milestone only
    /// has its stored amount changed; `fund_milestone` later collects it.
    ///
    /// # Parameters
    /// * `client`          – The job's client; pays the difference.
    /// * `freelancer`      – The job's freelancer; consents to the larger scope.
    /// * `milestone_index` – Index of the milestone to raise.
    /// * `new_amount`      – Replacement amount.  Must be strictly above the
    ///                        milestone's current amount.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked`     – Emergency pause or cancel lock is on.
    /// * `NotInitialized`              – Contract has not been initialized.
    /// * `Unauthorized`                – `client` / `freelancer` do not match
    ///                                    the stored job parties.
    /// * `InvalidMilestone`            – `milestone_index` is out of range.
    /// * `NotFunded`                   – An upfront escrow has not been funded
    ///                                    yet.
    /// * `InvalidStatus`               – The milestone is not `Pending`.
    /// * `InvalidAmount`               – `new_amount` is not strictly above the
    ///                                    current amount, or `total_amount`
    ///                                    overflows `i128`.
    /// * `InsufficientBalance`         – The escrow received less than the
    ///                                    difference.
    pub fn increase_milestone(
        env: Env,
        client: Address,
        freelancer: Address,
        milestone_index: u32,
        new_amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;

        let mut meta = Self::load_job_meta(&env)?;
        if client != meta.client || freelancer != meta.freelancer {
            return Err(Error::Unauthorized);
        }
        client.require_auth();
        freelancer.require_auth();

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        let funded = Self::milestone_is_funded(&meta, &milestone);
        if !funded && Self::load_funding_mode(&env) != FundingMode::PerMilestone {
            return Err(Error::NotFunded);
        }
        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }
        if new_amount <= milestone.amount {
            return Err(Error::InvalidAmount);
        }
        let old_amount = milestone.amount;
        let difference = new_amount
            .checked_sub(old_amount)
            .ok_or(Error::ArithmeticOverflow)?;
        let new_total = meta
            .total_amount
            .checked_add(difference)
            .ok_or(Error::InvalidAmount)?;

        milestone.amount = new_amount;
        Self::store_milestone(&env, milestone_index, &milestone);
        // Persist amended state BEFORE the token transfer so a reentrant call
        // sees the updated amount/total (mirrors `fund`).
        meta.total_amount = new_total;
        Self::store_job_meta(&env, &meta);

        if funded {
            let token_client = token::Client::new(&env, &meta.token);
            let before = token_client.balance(&env.current_contract_address());
            token_client.transfer(&client, env.current_contract_address(), &difference);
            let after = token_client.balance(&env.current_contract_address());
            let received = after.checked_sub(before).unwrap_or(0);
            if received != difference {
                return Err(Error::InsufficientBalance);
            }
        }

        env.events().publish(
            (symbol_short!("ms_incr"), client.clone(), freelancer.clone()),
            MilestoneIncreasedEvent {
                client,
                freelancer,
                milestone_index,
                old_amount,
                new_amount,
                total_amount: meta.total_amount,
                milestone_count: meta.milestone_count,
            },
        );

        Ok(())
    }

    /// Accept the job as its freelancer, agreeing on-chain to the milestone
    /// amounts and auto-release window set at `initialize`.
    ///
    /// `mark_delivered` and the dispute endpoints require both this and
    /// `accept_arbiter`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `freelancer` does not match the job's freelancer.
    /// * `InvalidStatus`  – The freelancer has already accepted.
    pub fn accept_job(env: Env, freelancer: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        freelancer.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }
        Self::record_acceptance(&env, freelancer, meta.client, 1, symbol_short!("acceptjob"))
    }

    /// Accept the job as its arbiter, agreeing to resolve its disputes.
    ///
    /// `mark_delivered` and the dispute endpoints require both this and
    /// `accept_job`.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `arbiter` does not match the job's arbiter.
    /// * `InvalidStatus`  – The arbiter has already accepted.
    pub fn accept_arbiter(env: Env, arbiter: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        arbiter.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.arbiter != arbiter {
            return Err(Error::Unauthorized);
        }
        Self::record_acceptance(&env, arbiter, meta.client, 2, symbol_short!("acceptarb"))
    }

    /// Appoint a new arbiter, jointly authorized by the client and the
    /// freelancer (issue #731).
    ///
    /// Lets the two contracting parties swap an arbiter who has become
    /// unavailable (unreachable, unresponsive, or unwilling to act) without
    /// waiting on the arbiter's own signature or an admin override.  Both the
    /// client's and the freelancer's `require_auth()` are collected up front
    /// by `require_client_and_freelancer_consent`, mirroring
    /// `set_interest_yield_consent` and `payment_streaming_consent`: a
    /// transaction missing either signature panics at the host level before
    /// any storage is mutated, so neither party can ever unilaterally replace
    /// the arbiter.
    ///
    /// Distinct-parties validation mirrors `initialize` (issue #667): the new
    /// arbiter must differ from the stored client, the stored freelancer, and
    /// the currently recorded arbiter.  A replacement is also refused while
    /// any milestone is still `Disputed` — the sitting arbiter is the party
    /// authorised to conclude an open dispute (`resolve_dispute` and
    /// `apply_dispute_arbitration_split` both verify the stored
    /// `meta.arbiter`), so the swap may only happen once every dispute has
    /// been resolved, split, or timed out and escalated.  There is no
    /// escape hatch past this guard: this is the deliberate design, because a
    /// silent mid-dispute arbiter swap would let the parties steer the
    /// outcome.  Once the swap succeeds it is recorded in `JobMeta::arbiter`,
    /// so the very next dispute raise, resolution, or arbitration split is
    /// checked and settled against the new arbiter automatically — no
    /// additional plumbing is needed.  On success the `arbchg` event reports
    /// the `old` and `new` arbiter addresses for indexers.
    ///
    /// # Parameters
    /// * `new_arbiter` – The incoming arbiter. Must be a valid address
    ///                   distinct from every recorded party.
    ///
    /// # Errors
    /// * `MigrationRequired`– Storage predates this code; call `migrate`.
    /// * `Paused`           – The contract is emergency-paused or
    ///                        cancel-locked.
    /// * `InvalidAddress`   – `new_arbiter` is a zero address, the canonical
    ///                        Soroban zero contract address, or the escrow's
    ///                        own address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `InvalidParties`   – `new_arbiter` equals the stored client, the
    ///                        stored freelancer, or the current arbiter.
    /// * `DisputePending`   – At least one milestone is currently `Disputed`.
    pub fn replace_arbiter(env: Env, new_arbiter: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;

        // ── Input validation: reject the zero sentinels and the escrow's own
        // address before any authorization or ledger access. ────────────────
        Self::validate_address(&env, &new_arbiter)?;

        // ── Joint authorization: both the client's and the freelancer's
        // `require_auth()` fire here, before any further check or write. ──
        let mut meta = Self::require_client_and_freelancer_consent(&env)?;

        // ── Distinct-parties validation (mirrors `initialize`, #667): the
        // arbiter must stay independent of the client, the freelancer, and
        // the arbiter being replaced. Re-proposing the sitting arbiter is a
        // no-op that would still cost a full two-signature round and emit a
        // misleading replacement event, so it is refused like a collision. ──
        if new_arbiter == meta.client
            || new_arbiter == meta.freelancer
            || new_arbiter == meta.arbiter
        {
            return Err(Error::InvalidParties);
        }

        // ── Dispute-active guard: an arbiter cannot be swapped out from
        // under an open dispute. Scanned before any write so a refused call
        // leaves no ledger trace. ─────────────────────────────────────────
        for index in 0..meta.milestone_count {
            let milestone = Self::load_milestone(&env, index)?;
            if milestone.status == MilestoneStatus::Disputed {
                return Err(Error::DisputePending);
            }
        }

        let old_arbiter = meta.arbiter.clone();
        meta.arbiter = new_arbiter.clone();
        Self::store_job_meta(&env, &meta);

        env.events().publish(
            (symbol_short!("arbchg"), new_arbiter.clone()),
            ArbiterReplacedEvent {
                client: meta.client.clone(),
                freelancer: meta.freelancer.clone(),
                old: old_arbiter,
                new: new_arbiter,
            },
        );

        Ok(())
    }

    /// Rotate the freelancer identity and payout destination (issue #732).
    ///
    /// Both the current freelancer and the client must authorize, so someone
    /// holding only the freelancer's key cannot redirect payouts. The new
    /// address becomes the identity checked by freelancer-only methods and
    /// the recipient of every later payout. Credits already owed to the old
    /// address (`Owed`) stay withdrawable by it.
    ///
    /// # Errors
    /// * `NotInitialized` – Job metadata has not been initialized.
    /// * `InvalidAddress` – `new_freelancer` is a zero address, this contract,
    ///   or the current freelancer.
    /// * `InvalidParties` – `new_freelancer` collides with the client, the
    ///   arbiter or the admin.
    pub fn rotate_freelancer(env: Env, new_freelancer: Address) -> Result<(), Error> {
        let mut meta = Self::require_client_and_freelancer_consent(&env)?;
        Self::validate_address(&env, &new_freelancer)?;
        if new_freelancer == meta.freelancer || new_freelancer == env.current_contract_address() {
            return Err(Error::InvalidAddress);
        }

        let admin = Self::load_admin(&env)?;
        if new_freelancer == meta.client
            || new_freelancer == meta.arbiter
            || new_freelancer == admin
        {
            return Err(Error::InvalidParties);
        }

        let previous_freelancer = meta.freelancer.clone();
        meta.freelancer = new_freelancer.clone();
        Self::store_job_meta(&env, &meta);

        env.events().publish(
            (symbol_short!("freerot"), new_freelancer.clone()),
            FreelancerRotatedEvent {
                client: meta.client,
                previous_freelancer,
                new_freelancer,
            },
        );

        Ok(())
    }

    /// Refund the client in full when the job was not accepted by both the
    /// freelancer and the arbiter within `ACCEPTANCE_WINDOW_SECONDS` of
    /// funding.
    ///
    /// Every milestone that is not already `Released` or `Refunded` moves to
    /// `Refunded` and its remaining amount is paid to the client.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked` – The contract is emergency-paused or
    ///                               cancel-locked.
    /// * `NotInitialized`          – Contract has not been initialized.
    /// * `Unauthorized`            – `client` does not match the job's client.
    /// * `NotFunded`               – The escrow has not been funded yet.
    /// * `InvalidStatus`           – Both parties have accepted the job.
    /// * `DeadlineNotPassed`       – The acceptance window is still open.
    /// * `InvalidAmount`           – Nothing remains to refund.
    pub fn withdraw_unaccepted(env: Env, client: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        // Upfront: the whole job; PerMilestone: at least one funded milestone.
        Self::require_escrow_has_funding(&env, &meta)?;
        if Self::load_acceptance(&env) == 3 {
            return Err(Error::InvalidStatus);
        }

        let funded_at: u64 = env
            .storage()
            .instance()
            .get(&DataKey::FundedAt)
            .unwrap_or(meta.created_at);
        let deadline = funded_at
            .checked_add(ACCEPTANCE_WINDOW_SECONDS)
            .ok_or(Error::InvalidAmount)?;
        if env.ledger().timestamp() < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        let mut total_refunded: i128 = 0;
        for index in 0..meta.milestone_count {
            let mut milestone = Self::load_milestone(&env, index)?;
            // Only money that was deposited can be refunded: skip terminal
            // milestones and, under PerMilestone funding, unfunded ones.
            if Self::is_terminal_status(&milestone.status)
                || !Self::milestone_is_funded(&meta, &milestone)
            {
                continue;
            }
            let remaining = milestone
                .amount
                .checked_sub(milestone.released_amount)
                .ok_or(Error::InvalidAmount)?;
            if remaining > 0 {
                total_refunded = total_refunded
                    .checked_add(remaining)
                    .ok_or(Error::InvalidAmount)?;
                milestone.status = MilestoneStatus::Refunded;
                Self::store_milestone(&env, index, &milestone);
            }
        }

        if total_refunded <= 0 {
            return Err(Error::InvalidAmount);
        }

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.client, &total_refunded)?;

        env.events().publish(
            (
                symbol_short!("unaccref"),
                meta.client.clone(),
                meta.freelancer.clone(),
            ),
            UnacceptedWithdrawnEvent {
                client: meta.client,
                token: meta.token,
                amount: total_refunded,
            },
        );

        Ok(())
    }

    /// Set a delivery deadline per milestone, index-aligned with the job's
    /// milestones.  `0` means no deadline.  Only allowed before any funding
    /// and before the freelancer accepts, so the deadlines are part of the
    /// terms the freelancer accepts.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `Unauthorized`   – `client` does not match the job's client.
    /// * `AlreadyFunded`  – The escrow (or any milestone) is already funded.
    /// * `InvalidStatus`  – The freelancer has already accepted the job.
    /// * `InvalidAmount`  – `due_dates.len()` differs from the milestone count.
    /// * `InvalidDeadline` – A non-zero due date is at or before the current
    ///                       ledger timestamp (`0` still clears a deadline).
    pub fn set_milestone_due_dates(
        env: Env,
        client: Address,
        due_dates: Vec<u64>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if Self::require_escrow_has_funding(&env, &meta).is_ok() {
            return Err(Error::AlreadyFunded);
        }
        if Self::load_acceptance(&env) & 1 != 0 {
            return Err(Error::InvalidStatus);
        }
        if due_dates.len() != meta.milestone_count {
            return Err(Error::InvalidAmount);
        }
        // Validate every date before writing any, so a rejected call cannot
        // leave a partially updated set of deadlines behind (#924).
        let now = env.ledger().timestamp();
        for due_at in due_dates.iter() {
            if due_at != 0 && due_at <= now {
                return Err(Error::InvalidDeadline);
            }
        }
        for (index, due_at) in due_dates.iter().enumerate() {
            let key = DataKey::DueAt(index as u32);
            if due_at == 0 {
                env.storage().persistent().remove(&key);
            } else {
                env.storage().persistent().set(&key, &due_at);
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            }
        }
        Ok(())
    }

    /// Delivery deadline of milestone `index`; `0` when it has none.
    ///
    /// # Errors
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `InvalidMilestone` – `index` is out of range.
    pub fn get_milestone_due_at(env: Env, index: u32) -> Result<u64, Error> {
        let meta = Self::load_job_meta(&env)?;
        if index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        Ok(Self::load_due_at(&env, index))
    }

    /// Refund a `Pending` milestone to the client once its delivery deadline
    /// plus `OVERDUE_GRACE_SECONDS` has passed without delivery.  The
    /// milestone becomes `Refunded`.  A late `mark_delivered` stays possible
    /// until this is called.
    ///
    /// # Errors
    /// * `Paused` / `EscrowLocked` – The contract is emergency-paused or
    ///                               cancel-locked.
    /// * `NotInitialized`    – Contract has not been initialized.
    /// * `Unauthorized`      – `client` does not match the job's client.
    /// * `InvalidMilestone`  – `milestone_index` is out of range.
    /// * `InvalidStatus`     – The milestone is not `Pending`.
    /// * `NotFunded`         – The milestone has not been funded.
    /// * `DeadlineNotPassed` – The milestone has no deadline, or deadline plus
    ///                         grace has not passed.
    /// * `InvalidAmount`     – Nothing remains to refund.
    pub fn reclaim_overdue(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if env
            .storage()
            .persistent()
            .get::<_, u32>(&DataKey::RevCnt(milestone_index))
            .unwrap_or(0)
            > 0
        {
            return Err(Error::InvalidStatus);
        }

        let due_at = Self::load_due_at(&env, milestone_index);
        if due_at == 0 {
            return Err(Error::DeadlineNotPassed);
        }
        let deadline = due_at
            .checked_add(OVERDUE_GRACE_SECONDS)
            .ok_or(Error::ArithmeticOverflow)?;
        if env.ledger().timestamp() <= deadline {
            return Err(Error::DeadlineNotPassed);
        }

        let remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        if remaining <= 0 {
            return Err(Error::InvalidAmount);
        }
        milestone.status = MilestoneStatus::Refunded;
        Self::store_milestone(&env, milestone_index, &milestone);

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.client, &remaining)?;

        env.events().publish(
            (
                symbol_short!("overdue"),
                meta.client.clone(),
                meta.freelancer.clone(),
            ),
            OverdueReclaimedEvent {
                client: meta.client,
                milestone_index,
                due_at,
                amount: remaining,
            },
        );

        Ok(())
    }

    /// Mark a milestone as delivered by the freelancer.
    ///
    /// Moves the milestone from `Pending` to `Delivered` and records the
    /// ledger timestamp of delivery, which starts the clock for
    /// `extend_milestone_deadline` and `claim_auto_release`.
    ///
    /// # Parameters
    /// * `freelancer`      – Must match the job's stored freelancer. Must
    ///                       authorize the call.
    /// * `milestone_index` – Index of the milestone being delivered.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `freelancer` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `freelancer` does not match the job's
    ///                        freelancer.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidAmount`    – The milestone's amount is not positive.
    /// * `InvalidStatus`    – The milestone is not currently `Pending`.
    /// * `NotAccepted`      – The freelancer or arbiter has not accepted the
    ///                        job.
    ///
    /// Legacy path kept for migration: records an all-zero `deliverable`
    /// hash. Prefer `mark_delivered_with_hash`.
    pub fn mark_delivered(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        let zero_hash = BytesN::from_array(&env, &[0u8; 32]);
        Self::mark_delivered_with_hash(env, freelancer, milestone_index, zero_hash)
    }

    /// Mark a milestone as delivered and record a hash of the deliverable
    /// (e.g. the SHA-256 of the artifact or an IPFS CID digest) so an arbiter
    /// can tie the delivery claim to a concrete artifact.
    ///
    /// Same parameters, behaviour and errors as `mark_delivered`, plus:
    /// * `deliverable` – 32-byte hash of the delivered artifact. Stored on the
    ///                   milestone and emitted in `DeliveredEvent`.
    pub fn mark_delivered_with_hash(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
        deliverable: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        // Check for zero addresses (both account and contract types)
        Self::validate_address(&env, &freelancer)?;
        freelancer.require_auth();

        let meta = Self::load_job_meta(&env)?;

        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

        if milestone.amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if milestone.status != MilestoneStatus::Pending {
            return Err(Error::InvalidStatus);
        }
        Self::require_accepted(&env)?;

        let delivered_at = env.ledger().timestamp();
        milestone.status = MilestoneStatus::Delivered;
        milestone.delivered_at = delivered_at;
        milestone.deliverable = deliverable.clone();
        Self::store_milestone(&env, milestone_index, &milestone);

        env.events().publish(
            (symbol_short!("deliver"), meta.freelancer.clone()),
            DeliveredEvent {
                milestone_index,
                freelancer: meta.freelancer,
                client: meta.client,
                delivered_at,
                status: MilestoneStatus::Delivered,
                amount: milestone.amount,
                deliverable,
            },
        );

        Ok(())
    }

    /// Ask the freelancer to revise a delivered milestone.
    ///
    /// Returns the milestone to `Pending`, clears `delivered_at` so the previous
    /// auto-release clock cannot fire, and increments `revision_count` up to
    /// `MAX_REVISIONS` (3). Accounting and funding are unchanged. Returns
    /// `ReviewWindowClosed` once the auto-release deadline has passed.
    pub fn request_revision(
        env: Env,
        client: Address,
        milestone_index: u32,
        note_hash: BytesN<32>,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        client.require_auth();
        let meta = Self::load_job_meta(&env)?;
        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        if milestone.status != MilestoneStatus::Delivered {
            return Err(Error::InvalidStatus);
        }
        Self::assert_review_window_open(&env, &meta, milestone_index, &milestone)?;

        let current: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::RevCnt(milestone_index))
            .unwrap_or(0);
        if current >= MAX_REVISIONS {
            return Err(Error::MaxRevisions);
        }
        let revision_count = current + 1;
        env.storage()
            .persistent()
            .set(&DataKey::RevCnt(milestone_index), &revision_count);

        milestone.status = MilestoneStatus::Pending;
        milestone.delivered_at = 0;
        Self::store_milestone(&env, milestone_index, &milestone);
        env.storage()
            .temporary()
            .remove(&DataKey::TimeExt(milestone_index));
        Self::clear_time_ext(&env, milestone_index);

        env.events().publish(
            (
                symbol_short!("msrev"),
                client.clone(),
                meta.freelancer.clone(),
            ),
            RevisionRequestedEvent {
                milestone_index,
                client,
                note_hash,
                revision_count,
            },
        );
        Ok(())
    }

    pub fn get_revision_count(env: Env, milestone_index: u32) -> Result<u32, Error> {
        let meta = Self::load_job_meta(&env)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        Ok(env
            .storage()
            .persistent()
            .get(&DataKey::RevCnt(milestone_index))
            .unwrap_or(0))
    }

    /// Extends the auto-release deadline for a Delivered milestone.
    pub fn extend_milestone_deadline(
        env: Env,
        client: Address,
        milestone_index: u32,
        extra_seconds: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
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
        // A deadline extension changes the freelancer's guaranteed release time;
        // require their explicit consent instead of allowing unilateral delay.
        meta.freelancer.require_auth();
        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        if extra_seconds == 0 {
            return Err(Error::InvalidExtension);
        }

        let current_extension = Self::load_time_extension(&env, milestone_index);
        let new_extension = current_extension
            .checked_add(extra_seconds)
            .ok_or(Error::InvalidExtension)?;
        let delivered_at = milestone.delivered_at;
        let new_deadline = Self::auto_release_deadline(&meta, delivered_at, new_extension)?;
        env.storage()
            .persistent()
            .set(&DataKey::TimeExt(milestone_index), &new_extension);

        env.events().publish(
            (
                symbol_short!("extend"),
                client.clone(),
                meta.freelancer.clone(),
            ),
            DeadlineExtendedEvent {
                milestone_index,
                client,
                extra_seconds,
                new_extension,
                new_deadline,
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
    /// - 1× persistent read  (`DataKey::Milestone(milestone_index)`)
    /// - 1× persistent write (`DataKey::Milestone(milestone_index)`)
    /// - 1× token transfer
    ///
    /// No loop over all milestones is performed here.  Functions that do loop
    /// over all milestones (`assemble_job`) are
    /// intentionally not called from this hot path.
    ///
    /// The payout always goes to the job's freelancer, so the signature only
    /// attributes the claim; `trigger_auto_release` performs the same release
    /// without it.
    pub fn claim_auto_release(
        env: Env,
        freelancer: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::validate_address(&env, &freelancer)?;
        freelancer.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.freelancer != freelancer {
            return Err(Error::Unauthorized);
        }

        Self::release_overdue_milestone(&env, &meta, freelancer, milestone_index)
    }

    /// Permissionless auto-release of an overdue milestone (#740).
    ///
    /// Anyone (a keeper bot, a scheduled backend job, the freelancer, the
    /// client) may call this once a milestone's auto-release deadline has
    /// passed. No signature is required because the call cannot redirect
    /// funds: the remaining balance is always transferred to the job's stored
    /// freelancer, exactly as in `claim_auto_release`, and every other check
    /// (pause and execution guards, index, `Delivered` status, deadline,
    /// remaining > 0) is identical.
    ///
    /// # Parameters
    /// * `caller`          – Recorded in the `claim` event for attribution.
    ///                       It is **not** authenticated, so indexers must
    ///                       not treat it as proof of who submitted the
    ///                       transaction.
    /// * `milestone_index` – Index of the milestone to release.
    ///
    /// # Errors
    /// * `Paused`            – The contract is emergency-paused.
    /// * `NotInitialized`    – Contract has not been initialized.
    /// * `InvalidMilestone`  – `milestone_index` is out of range.
    /// * `InvalidStatus`     – The milestone is not currently `Delivered`.
    /// * `DeadlineNotPassed` – The auto-release deadline has not passed yet.
    /// * `InvalidAmount`     – The milestone's remaining balance is not
    ///                         positive.
    pub fn trigger_auto_release(
        env: Env,
        caller: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        let meta = Self::load_job_meta(&env)?;

        Self::release_overdue_milestone(&env, &meta, caller, milestone_index)
    }

    /// Compute the seconds remaining until automatic release of a milestone.
    ///
    /// This is a read-only query that does not mutate any ledger state. It calculates
    /// the time until the auto-release deadline based on:
    /// - The milestone's delivery timestamp (from temporary storage if available, else from persistent)
    /// - The configured auto-release delay from the job metadata
    /// - Any active time extension applied to the milestone
    ///
    /// # Return Value
    ///
    /// - **Positive value**: The number of seconds remaining until auto-release.
    ///   - At or near the deadline, this approaches 0.
    ///   - The deadline is: `delivered_at + auto_release_seconds + extension_seconds`.
    ///
    /// - **Zero or Negative**: Deadline has passed; auto-release is or was overdue.
    ///   - In populated state: Returns negative if `current > deadline`.
    ///   - In boundary state: Returns error if deadline or current timestamp overflows i64.
    ///
    /// - **Error**: Returned if:
    ///   - The deadline calculation overflows (deadline too far in future).
    ///   - The timestamp conversion to i64 fails.
    ///   - The subtraction overflows (current > deadline, rare in i64).
    ///   - The job metadata is missing (`NotInitialized`).
    ///   - The milestone index is out of range or the milestone is missing
    ///     (`InvalidMilestone`).
    ///   - The milestone is not `Delivered` or `PartiallyReleased`
    ///     (`InvalidStatus`).
    ///
    /// # Behavior in Edge Cases
    ///
    /// - Out-of-range index: Returns `InvalidMilestone`.
    /// - Undelivered / settled milestone: Returns `InvalidStatus` instead of a
    ///   huge negative "overdue" value.
    /// - Boundary state (large extensions/timestamps): Returns error on overflow.
    /// - Expired milestone: Returns 0 or negative value (seconds past deadline).
    /// - Just-delivered (delivered_at = current): Returns approximately `auto_release_seconds + extension_seconds`.
    pub fn time_until_auto_release(env: Env, milestone_index: u32) -> Result<i64, Error> {
        let meta = Self::load_job_meta(&env)?;
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        let milestone = Self::load_milestone(&env, milestone_index)?;
        // Only a claimable delivery has an auto-release deadline. Pending,
        // Released, Refunded and Disputed milestones have delivered_at == 0 or
        // a stale value, which would otherwise yield a nonsense overdue result.
        match milestone.status {
            MilestoneStatus::Delivered | MilestoneStatus::PartiallyReleased => {}
            _ => return Err(Error::InvalidStatus),
        }
        // Read delivery timestamp from temporary storage (optimised path) and
        // fall back to the persistent Milestone field for pre-migration entries.
        let delivered_at = milestone.delivered_at;
        let extension = Self::load_time_extension(&env, milestone_index);
        let deadline = Self::auto_release_deadline(&meta, delivered_at, extension)?;
        let current = env.ledger().timestamp();
        let deadline_i64 = i64::try_from(deadline).map_err(|_| Error::ArithmeticOverflow)?;
        let current_i64 = i64::try_from(current).map_err(|_| Error::ArithmeticOverflow)?;
        deadline_i64
            .checked_sub(current_i64)
            .ok_or(Error::ArithmeticOverflow)
    }

    /// Release a partial payment for a delivered milestone.
    ///
    /// Transfers `amount` to the freelancer immediately. If the released
    /// total reaches the full milestone amount, the milestone transitions
    /// to `Released` and both parties' reputation is incremented; otherwise
    /// it moves to (or stays at) `PartiallyReleased` so further partial
    /// approvals can follow.
    ///
    /// # Parameters
    /// * `client`          – Must match the job's stored client. Must
    ///                       authorize the call.
    /// * `milestone_index` – Index of the milestone being paid out.
    /// * `amount`          – Amount to release now. Must be positive and no
    ///                       more than the milestone's remaining balance.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `client` is a zero address or the contract
    ///                        address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – The milestone is not `Delivered` or
    ///                        `PartiallyReleased`.
    /// * `InvalidAmount`    – `amount` is not positive, or exceeds the
    ///                        milestone's remaining balance.
    pub fn approve_partial(
        env: Env,
        client: Address,
        milestone_index: u32,
        amount: i128,
    ) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::validate_address(&env, &client)?;

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;

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
        Self::pay_freelancer_with_fee(&env, &meta, &token_client, &amount)?;

        let mut updated_milestone = milestone;
        updated_milestone.released_amount = updated_milestone
            .released_amount
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;

        if updated_milestone.released_amount == updated_milestone.amount {
            updated_milestone.status = MilestoneStatus::Released;
            Self::clear_time_ext(&env, milestone_index);
            Self::increment_reputation(&env, &meta.client, milestone_index);
            Self::increment_reputation(&env, &meta.freelancer, milestone_index);
        } else {
            updated_milestone.status = MilestoneStatus::PartiallyReleased;
        }

        Self::store_milestone(&env, milestone_index, &updated_milestone);

        let event_remaining = updated_milestone
            .amount
            .checked_sub(updated_milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;
        env.events().publish(
            (symbol_short!("approve"), meta.client.clone()),
            ApprovedEvent {
                milestone_index,
                amount,
                released_amount: updated_milestone.released_amount,
                remaining: event_remaining,
                status: updated_milestone.status.clone(),
            },
        );

        Ok(())
    }

    /// Approve a delivered milestone and release its full remaining balance
    /// to the freelancer.
    ///
    /// Transfers the remaining amount owed, marks the milestone `Released`,
    /// and increments the reputation of both the client and freelancer.
    ///
    /// # Parameters
    /// * `client`          – Must match the job's stored client. Must
    ///                       authorize the call.
    /// * `milestone_index` – Index of the milestone to approve.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `client` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `milestone_index` is out of range.
    /// * `InvalidStatus`    – The milestone is not currently `Delivered`.
    /// * `InvalidAmount`    – The milestone's remaining balance is not
    ///                        positive.
    pub fn approve_milestone(env: Env, client: Address, milestone_index: u32) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        Self::validate_address(&env, &client)?;

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(&env, milestone_index)?;
        Self::require_milestone_is_funded(&env, &meta, &milestone)?;
        // #673: a partial approval leaves the milestone PartiallyReleased;
        // the client must still be able to approve the remaining balance in
        // one call. Other statuses remain invalid here.
        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
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

        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_freelancer_with_fee(&env, &meta, &token_client, &remaining)?;
        milestone.released_amount = milestone.amount;

        milestone.status = MilestoneStatus::Released;
        Self::store_milestone(&env, milestone_index, &milestone);
        Self::clear_time_ext(&env, milestone_index);
        Self::increment_reputation(&env, &meta.client, milestone_index);
        Self::increment_reputation(&env, &meta.freelancer, milestone_index);

        let event_remaining = milestone
            .amount
            .checked_sub(milestone.released_amount)
            .ok_or(Error::InvalidAmount)?;

        env.events().publish(
            (symbol_short!("approve"), meta.client.clone()),
            ApprovedEvent {
                milestone_index,
                amount: remaining,
                released_amount: milestone.released_amount,
                remaining: event_remaining,
                status: milestone.status.clone(),
            },
        );

        Ok(())
    }

    /// Withdraw the amount owed to `caller` from payouts or refunds that could
    /// not be transferred at settlement time (see `pay_or_credit`).
    ///
    /// The credit is cleared before the transfer. If the transfer still fails
    /// (e.g. the trustline is still missing) the whole call reverts and the
    /// credit is kept, so `caller` can retry once their account can receive.
    ///
    /// # Parameters
    /// * `caller` – Address with an owed balance. Must authorize the call.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `InvalidAmount`  – Nothing is owed to `caller`.
    pub fn withdraw(env: Env, caller: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;

        let key = DataKey::Owed(caller.clone());
        let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total_owed = Self::load_total_owed(&env)
            .checked_sub(amount)
            .ok_or(Error::ArithmeticOverflow)?;

        // CEI: clear the credit before the external transfer.
        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .set(&DataKey::TotalOwed, &total_owed);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &caller, &amount);

        let counterparty = if caller == meta.client {
            meta.freelancer.clone()
        } else {
            meta.client.clone()
        };
        env.events().publish(
            (symbol_short!("withdraw"), caller.clone(), counterparty),
            WithdrawnEvent {
                recipient: caller,
                token: meta.token,
                amount,
            },
        );

        Ok(())
    }

    /// Withdraw the amount owed to `caller` to a different `recipient`
    /// address (#1012).
    ///
    /// Credits exist because a transfer to `caller` failed. When that cannot
    /// be fixed (e.g. the issuer deauthorized the account), `caller` can still
    /// sign and send the credit somewhere it can be received. Otherwise this
    /// behaves like `withdraw`: the credit is cleared before the transfer and
    /// the whole call reverts, keeping the credit, if the transfer fails.
    ///
    /// Like `withdraw`, the `withdraw` event's topics are `caller` and the
    /// counterparty; `recipient` is in its body.
    ///
    /// # Parameters
    /// * `caller`    – Address with an owed balance. Must authorize the call.
    /// * `recipient` – Address that receives the owed balance.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `InvalidAddress` – `recipient` is a zero address or the contract.
    /// * `InvalidAmount`  – Nothing is owed to `caller`.
    pub fn withdraw_to(env: Env, caller: Address, recipient: Address) -> Result<(), Error> {
        Self::ensure_migrated(&env)?;
        caller.require_auth();
        Self::validate_address(&env, &recipient)?;
        let meta = Self::load_job_meta(&env)?;

        let key = DataKey::Owed(caller.clone());
        let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let total_owed = Self::load_total_owed(&env)
            .checked_sub(amount)
            .ok_or(Error::ArithmeticOverflow)?;

        // CEI: clear the credit before the external transfer.
        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .set(&DataKey::TotalOwed, &total_owed);

        let token_client = token::Client::new(&env, &meta.token);
        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        let counterparty = if caller == meta.client {
            meta.freelancer.clone()
        } else {
            meta.client.clone()
        };
        env.events().publish(
            (symbol_short!("withdraw"), caller, counterparty),
            WithdrawnEvent {
                recipient,
                token: meta.token,
                amount,
            },
        );

        Ok(())
    }

    /// Approve several delivered milestones in one transaction (#741).
    ///
    /// Releases the full remaining balance of every listed milestone with a
    /// **single** token transfer of the summed amount, marks each one
    /// `Released`, increments both parties' reputation once per milestone
    /// (as `approve_milestone` would), and emits one `approve` event per
    /// milestone, in the order given.
    ///
    /// The call is all-or-nothing: every index is validated before any state
    /// is written, and any invalid, duplicated or ineligible index fails the
    /// whole call with nothing released.
    ///
    /// Unlike `approve_milestone`, a `PartiallyReleased` milestone is
    /// accepted too; its outstanding remainder is what gets released.
    ///
    /// # Parameters
    /// * `client`  – Must match the job's stored client. Must authorize the
    ///               call.
    /// * `indices` – Milestones to approve: 1 to `MAX_BATCH_APPROVALS` (10)
    ///               distinct indices.
    ///
    /// # Errors
    /// * `Paused`           – The contract is emergency-paused.
    /// * `InvalidAddress`   – `client` is a zero address.
    /// * `NotInitialized`   – Contract has not been initialized.
    /// * `Unauthorized`     – `client` does not match the job's client.
    /// * `NotFunded`        – The escrow has not been funded yet.
    /// * `InvalidMilestone` – `indices` is empty, longer than the cap,
    ///                        contains a duplicate, or contains an
    ///                        out-of-range index.
    /// * `InvalidStatus`    – A listed milestone is not `Delivered` or
    ///                        `PartiallyReleased`.
    /// * `InvalidAmount`    – A listed milestone's remaining balance is not
    ///                        positive, or the sum overflows.
    pub fn approve_milestones(env: Env, client: Address, indices: Vec<u32>) -> Result<(), Error> {
        Self::ensure_not_paused(&env)?;
        Self::validate_address(&env, &client)?;

        client.require_auth();
        let meta = Self::load_job_meta(&env)?;

        if meta.client != client {
            return Err(Error::Unauthorized);
        }
        Self::require_escrow_has_funding(&env, &meta)?;

        let count = indices.len();
        if count == 0 || count > MAX_BATCH_APPROVALS {
            return Err(Error::InvalidMilestone);
        }

        // CHECKS: validate every index and sum the payout before writing
        // anything, so a bad entry anywhere in the list leaves state untouched.
        let mut milestones: Vec<Milestone> = Vec::new(&env);
        let mut released: Vec<i128> = Vec::new(&env);
        let mut total: i128 = 0;
        for (position, milestone_index) in indices.iter().enumerate() {
            if milestone_index >= meta.milestone_count {
                return Err(Error::InvalidMilestone);
            }
            // `count` is capped at MAX_BATCH_APPROVALS, so this scan is bounded.
            for earlier in indices.iter().take(position) {
                if earlier == milestone_index {
                    return Err(Error::InvalidMilestone);
                }
            }

            let milestone = Self::load_milestone(&env, milestone_index)?;
            Self::require_milestone_is_funded(&env, &meta, &milestone)?;
            if milestone.status != MilestoneStatus::Delivered
                && milestone.status != MilestoneStatus::PartiallyReleased
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
            total = total.checked_add(remaining).ok_or(Error::InvalidAmount)?;
            milestones.push_back(milestone);
            released.push_back(remaining);
        }

        // EFFECTS: commit every milestone's terminal state before the
        // transfer (Checks-Effects-Interactions).
        for (milestone_index, mut milestone) in indices.iter().zip(milestones.iter()) {
            milestone.released_amount = milestone.amount;
            milestone.status = MilestoneStatus::Released;
            Self::store_milestone(&env, milestone_index, &milestone);
            Self::increment_reputation(&env, &meta.client, milestone_index);
            Self::increment_reputation(&env, &meta.freelancer, milestone_index);
        }

        // INTERACTION: one transfer for the whole batch.
        let token_client = token::Client::new(&env, &meta.token);
        Self::pay_or_credit(&env, &token_client, &meta.freelancer, &total)?;

        for (milestone_index, (milestone, amount)) in
            indices.iter().zip(milestones.iter().zip(released.iter()))
        {
            env.events().publish(
                (symbol_short!("approve"), meta.client.clone()),
                ApprovedEvent {
                    milestone_index,
                    amount,
                    released_amount: milestone.amount,
                    remaining: 0,
                    status: MilestoneStatus::Released,
                },
            );
        }

        Ok(())
    }

    /// Split `total_amount` into per-party shares without changing contract state.
    ///
    /// The result uses the largest-remainder (Hare quota) method. This
    /// calculation performs no authorization, storage writes, token transfers,
    /// or event publication. Callers must separately execute any transfers.
    ///
    /// # Parameters
    /// * `total_amount` – Amount to split.  Must be > 0.
    /// * `ratios`       – Per-party weights, index-aligned with the returned
    ///                    vector.  Only their relative sizes matter, so they
    ///                    need not be normalised.  Must be non-empty, at most
    ///                    `MAX_MULTISIG_RATIO_COUNT` (255) entries long, with
    ///                    no negative entry, and must sum to > 0 without
    ///                    overflowing `i128`.
    ///
    /// # Returns
    /// `Ok(allocations)` — a `Vec<i128>` with **exactly `ratios.len()`
    /// entries, index-aligned with `ratios`**, where `allocations[i]` is party
    /// `i`'s share of `total_amount`:
    ///
    /// * **Conservation** – `Σ allocations == total_amount` exactly, for every
    ///   valid input.  No value is lost and none is created.
    /// * **Non-negative** – every entry is ≥ 0.  A party weighted `0` receives
    ///   exactly `0`.
    /// * **Bounded error** – entry `i` is either
    ///   `floor(total_amount × ratios[i] / Σratios)` or that floor `+ 1`, so a
    ///   party is never rounded below its floor share.  The indivisible
    ///   residue units (at most `ratios.len() − 1` of them) go to the parties
    ///   with the largest fractional remainders.
    /// * **Determinism** – remainder ties are broken by lowest index, so
    ///   identical inputs always produce the identical vector.
    ///
    /// # Validation order
    /// Checks run in the order below and a call violating more than one fails
    /// with the first match:
    ///
    /// 1. `total_amount` ≤ 0                → `InvalidAmount`
    /// 2. `ratios` empty                    → `InvalidRatio`
    /// 3. `ratios.len()` > 255              → `TooManyParties`
    /// 4. negative entry, overflowing sum, or `Σratios` ≤ 0 → `InvalidRatio`
    /// 5. allocation arithmetic overflow    → `InvalidAmount`
    ///
    /// # Errors
    /// * `InvalidAmount`  – `total_amount` ≤ 0; or an `i128` checked operation
    ///   inside the allocation maths (weighted product, running base sum, residue
    ///   subtraction, per-entry increment) is unrepresentable.
    /// * `InvalidRatio`   – `ratios` is empty; any entry is negative; `Σratios`
    ///   overflows `i128`; or `Σratios` ≤ 0 (every entry zero).
    /// * `TooManyParties` – `ratios` is longer than `MAX_MULTISIG_RATIO_COUNT`.
    ///
    /// Arithmetic overflow maps to `InvalidAmount` / `InvalidRatio`.
    pub fn compute_ratio_allocation(
        env: Env,
        total_amount: i128,
        ratios: Vec<i128>,
    ) -> Result<Vec<i128>, Error> {
        if ratios.len() > MAX_MULTISIG_RATIO_COUNT {
            return Err(Error::TooManyParties);
        }
        math::allocate_weights(&env, total_amount, &ratios)
    }

    /// Return the version of the code that is actually running.
    ///
    /// The value is the compile-time `CONTRACT_VERSION` from Cargo.toml and is
    /// also embedded in the WASM as `contractmeta!(key = "version", ...)`.
    /// Unlike the old implementation it does not read the `DataKey::Version`
    /// storage marker: that marker counted upgrades (and even incremented on a
    /// downgrade), so two deployments of identical code could report different
    /// values. `DataKey::Version` is retained solely as a migration marker.
    ///
    /// # Guarantees
    /// * **Read-only.** No storage access at all: no `set`, `remove`,
    ///   `extend_ttl`, or event publish in any storage durability.
    /// * Requires no authorization and is safe to call on an uninitialized
    ///   contract.
    ///
    /// # Errors
    /// None. `version` is infallible.
    pub fn version(env: Env) -> String {
        String::from_str(&env, CONTRACT_VERSION)
    }

    /// Return the storage layout version (see `SCHEMA_VERSION`).
    ///
    /// Lower than `SCHEMA_VERSION` after an `upgrade` until the admin calls
    /// `migrate`; state-changing endpoints return `MigrationRequired` until
    /// then.  Read-only; `1` on deployments that predate the marker.
    pub fn schema_version(env: Env) -> u32 {
        Self::read_schema_version(&env)
    }

    /// Return a full snapshot of the current job, including every
    /// milestone's status and amounts.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    pub fn get_job(env: Env) -> Result<Job, Error> {
        let meta = Self::load_job_meta(&env)?;
        Self::assemble_job(&env, &meta)
    }

    /// Return one milestone without assembling the rest of the job (issue
    /// #733): reads the job metadata and exactly one milestone entry,
    /// whatever the milestone count.
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    /// * `InvalidMilestone` – `index` is outside the job's milestone range.
    pub fn get_milestone(env: Env, index: u32) -> Result<Milestone, Error> {
        let meta = Self::load_job_meta(&env)?;
        if index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        let mut milestone = Self::load_milestone(&env, index)?;
        milestone.funded = Self::milestone_is_funded(&meta, &milestone);
        Ok(milestone)
    }

    /// Number of milestones in the job (issue #733).
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    pub fn milestone_count(env: Env) -> Result<u32, Error> {
        Ok(Self::load_job_meta(&env)?.milestone_count)
    }

    /// Return everything needed to render the escrow in a single read: the
    /// job (as `get_job`), the admin, both pause flags, the cancellation
    /// lock and approval mask, each milestone's time extension and
    /// auto-release deadline, the platform-fee allocation and the version.
    ///
    /// Several of these (admin, cancel state, per-milestone extensions) have
    /// no other getter, and the rest would otherwise cost one simulation each.
    ///
    /// Read-only: no storage write, TTL extension or event (enforced by
    /// `get_escrow_state_no_mutation_tests`).
    ///
    /// # Errors
    /// * `NotInitialized` – Contract has not been initialized.
    pub fn get_escrow_state(env: Env) -> Result<EscrowState, Error> {
        let meta = Self::load_job_meta(&env)?;
        let job = Self::assemble_job(&env, &meta)?;
        let admin = Self::load_admin(&env)?;

        let instance = env.storage().instance();
        let paused = EscrowPausedState {
            emergency: instance.get(&DataKey::Ep).unwrap_or(false),
            admin: instance.get(&DataKey::Paused).unwrap_or(false),
        };
        let cancel = EscrowCancelState {
            locked: instance.get(&DataKey::CancelLock).unwrap_or(false),
            // An approval older than CANCEL_APPROVAL_TTL_SECONDS no longer
            // counts (#713), so it reads as no approval here too.
            approval_mask: instance
                .get::<_, CancelApprovalRecord>(&DataKey::CancelApproval)
                .filter(|record| {
                    env.ledger().timestamp().saturating_sub(record.approved_at)
                        < CANCEL_APPROVAL_TTL_SECONDS
                })
                .map(|record| record.mask)
                .unwrap_or(0),
        };

        let mut milestone_timing = Vec::new(&env);
        for (index, milestone) in job.milestones.iter().enumerate() {
            let index = index as u32;
            let extension_seconds = Self::load_time_extension(&env, index);
            // Auto-release only applies to Delivered milestones
            // (claim_auto_release), so only they have a deadline.
            let deadline = if milestone.status == MilestoneStatus::Delivered {
                let delivered_at = milestone.delivered_at;
                Some(Self::auto_release_deadline(
                    &meta,
                    delivered_at,
                    extension_seconds,
                )?)
            } else {
                None
            };
            milestone_timing.push_back(MilestoneTiming {
                extension_seconds,
                deadline,
            });
        }

        let fee_allocation: Option<PlatformFeeAllocation> =
            Self::cold_get(&env, &DataKey::PlatformFeeAllocation);
        Ok(EscrowState {
            job,
            admin,
            paused,
            cancel,
            milestone_timing,
            fee_allocation_set: fee_allocation.is_some(),
            fee_allocation: fee_allocation.unwrap_or(PlatformFeeAllocation {
                client_bps: 0,
                freelancer_bps: 0,
                treasury_bps: 0,
                locked: false,
            }),
            version: String::from_str(&env, CONTRACT_VERSION),
        })
    }

    /// Recover tokens that are not backing outstanding escrow obligations.
    ///
    /// Authorized callers: the job client or the admin. Excess escrow-token
    /// balance is returned to the client. Non-escrow tokens are returned in
    /// full to the client after confirming they are not the settlement token.
    pub fn sweep_excess(env: Env, caller: Address, token: Address) -> Result<i128, Error> {
        Self::ensure_migrated(&env)?;
        Self::ensure_not_paused(&env)?;
        caller.require_auth();
        let meta = Self::load_job_meta(&env)?;
        let admin = Self::load_admin(&env)?;
        if caller != meta.client && caller != admin {
            return Err(Error::Unauthorized);
        }

        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());
        let excess = if token == meta.token {
            let obligations = Self::outstanding_escrow_obligations(&env, &meta)?;
            if balance < obligations {
                return Err(Error::InsufficientBalance);
            }
            balance
                .checked_sub(obligations)
                .ok_or(Error::ArithmeticOverflow)?
        } else {
            balance
        };
        if excess <= 0 {
            return Err(Error::NothingToSweep);
        }

        token_client.transfer(&env.current_contract_address(), &meta.client, &excess);
        env.events().publish(
            (
                symbol_short!("sweep"),
                meta.client.clone(),
                meta.freelancer.clone(),
            ),
            ExcessSweptEvent {
                token,
                amount: excess,
                recipient: meta.client,
            },
        );
        Ok(excess)
    }

    /// Return the off-chain description hash committed for one milestone.
    pub fn get_milestone_spec_hash(env: Env, index: u32) -> Result<BytesN<32>, Error> {
        let meta = Self::load_job_meta(&env)?;
        if index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }
        Ok(Self::load_milestone(&env, index)?.spec_hash)
    }

    /// Return the reputation counter for an address.
    ///
    /// Read-only: performs no storage writes, TTL extensions or event
    /// publications. Each storage key is touched at most once per call — one
    /// existence check on `DataKey::Admin` and one read of
    /// `DataKey::Reputation(address)`. `reputation_no_mutation_tests` pins this
    /// with whole-ledger snapshots taken around the call (#479); any `set`,
    /// `remove` or `extend_ttl` added here fails that suite.
    ///
    /// # Returns
    /// * **Populated** – `Ok(n)` where `n` is the stored counter, i.e. the
    ///   number of completed jobs (full releases) the address has been a
    ///   client or freelancer on.
    /// * **Empty** – `Ok(0)` when no `Reputation` entry exists for `address`.
    ///   An absent entry and a zero counter are indistinguishable.
    /// * **Boundary** – the value is a `u32`, so the result lies in
    ///   `0..=u32::MAX`. This getter never saturates or wraps; the counter is
    ///   incremented with plain `+ 1` in `increment_reputation`, so a counter
    ///   already at `u32::MAX` would overflow on the *next increment* (panic
    ///   with overflow checks enabled), not in this read.
    ///
    /// # Errors
    /// * `NotInitialized` - Contract has not been initialized.
    pub fn get_reputation(env: Env, address: Address) -> Result<u32, Error> {
        Self::require_initialized(&env)?;
        Ok(env
            .storage()
            .persistent()
            .get(&DataKey::Reputation(address))
            .unwrap_or(0))
    }

}

#[allow(deprecated)]
impl MilestoneEscrow {
    pub(crate) fn available_balance(env: &Env, token_client: &token::Client) -> i128 {
        token_client
            .balance(&env.current_contract_address())
            .saturating_sub(Self::load_total_owed(env))
    }

    pub(crate) fn pay_or_credit(
        env: &Env,
        token_client: &token::Client,
        to: &Address,
        amount: &i128,
    ) -> Result<(), Error> {
        let contract = env.current_contract_address();
        if matches!(token_client.try_transfer(&contract, to, amount), Ok(Ok(()))) {
            return Ok(());
        }
        if Self::available_balance(env, token_client) < *amount {
            return Err(Error::InsufficientBalance);
        }

        let key = DataKey::Owed(to.clone());
        let owed = env
            .storage()
            .persistent()
            .get::<_, i128>(&key)
            .unwrap_or(0)
            .checked_add(*amount)
            .ok_or(Error::ArithmeticOverflow)?;
        let total_owed = Self::load_total_owed(env)
            .checked_add(*amount)
            .ok_or(Error::ArithmeticOverflow)?;
        env.storage().persistent().set(&key, &owed);
        env.storage()
            .instance()
            .set(&DataKey::TotalOwed, &total_owed);
        Ok(())
    }

    pub(crate) fn outstanding_escrow_obligations(env: &Env, meta: &JobMeta) -> Result<i128, Error> {
        let mut outstanding: i128 = 0;
        for index in 0..meta.milestone_count {
            let milestone = Self::load_milestone(env, index)?;
            if Self::is_terminal_status(&milestone.status) {
                continue;
            }
            let remaining = milestone
                .amount
                .checked_sub(milestone.released_amount)
                .ok_or(Error::ArithmeticOverflow)?;
            if remaining > 0 {
                outstanding = outstanding
                    .checked_add(remaining)
                    .ok_or(Error::ArithmeticOverflow)?;
            }
        }
        let yield_accrued: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::YieldAccrued)
            .unwrap_or(0);
        if yield_accrued > 0 {
            outstanding = outstanding
                .checked_add(yield_accrued)
                .ok_or(Error::ArithmeticOverflow)?;
        }
        // Amounts credited to parties whose transfer failed (#755) are still
        // owed and must never be swept.
        outstanding = outstanding
            .checked_add(Self::load_total_owed(env))
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(outstanding)
    }

    pub(crate) fn auto_release_deadline(
        meta: &JobMeta,
        delivered_at: u64,
        extension: u32,
    ) -> Result<u64, Error> {
        delivered_at
            .checked_add(meta.auto_release_seconds)
            .and_then(|deadline| deadline.checked_add(extension as u64))
            .ok_or(Error::ArithmeticOverflow)
    }

    pub(crate) fn increment_reputation(env: &Env, address: &Address, milestone_index: u32) {
        let key = DataKey::Reputation(address.clone());
        let current: u32 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_score = current + 1;
        env.storage().persistent().set(&key, &new_score);
        env.events().publish(
            (symbol_short!("rep"),),
            ReputationChangedEvent {
                address: address.clone(),
                new_score,
                milestone_index,
            },
        );
    }

    pub(crate) fn assemble_job(env: &Env, meta: &JobMeta) -> Result<Job, Error> {
        let mut milestones = Vec::new(env);
        for i in 0..meta.milestone_count {
            let mut milestone = Self::load_milestone(env, i)?;
            milestone.funded = Self::milestone_is_funded(meta, &milestone);
            milestones.push_back(milestone);
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

    /// Shared body of `__constructor` and the test-only initialisers.
    ///
    /// # Errors
    /// * [`Error::InvalidRatio`] – `settings.arbiter_fee_bps` exceeds
    ///   `MAX_ARBITER_FEE_BPS`.
    /// * [`Error::InvalidDisputeTimeout`] – `settings.dispute_timeout_seconds`
    ///   is outside `MIN_DISPUTE_TIMEOUT_SECONDS..=MAX_DISPUTE_TIMEOUT_SECONDS`.
    /// * Every error documented on `__constructor`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn initialize_with_settings(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
        settings: EscrowSettings,
    ) -> Result<(), Error> {
        if settings.arbiter_fee_bps > MAX_ARBITER_FEE_BPS {
            return Err(Error::InvalidRatio);
        }
        Self::initialize_inner(
            env.clone(),
            admin,
            client,
            freelancer,
            arbiter,
            token,
            auto_release_seconds,
            settings.dispute_timeout_seconds,
            milestone_amounts,
            milestone_spec_hashes,
        )?;
        if settings.funding_mode != FundingMode::Upfront {
            env.storage()
                .instance()
                .set(&DataKey::FundingMode, &settings.funding_mode);
        }
        if settings.arbiter_fee_bps > 0 {
            env.storage()
                .instance()
                .set(&DataKey::ArbiterFeeBps, &settings.arbiter_fee_bps);
        }
        Ok(())
    }

    /// The default settings: upfront funding, the standard dispute timeout
    /// and no arbiter fee.
    #[cfg(test)]
    pub(crate) fn default_settings() -> EscrowSettings {
        EscrowSettings {
            funding_mode: FundingMode::Upfront,
            dispute_timeout_seconds: DISPUTE_TIMEOUT_SECONDS,
            arbiter_fee_bps: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn initialize_inner(
        env: Env,
        admin: Address,
        client: Address,
        freelancer: Address,
        arbiter: Address,
        token: Address,
        auto_release_seconds: u64,
        dispute_timeout_seconds: u64,
        milestone_amounts: Vec<i128>,
        milestone_spec_hashes: Vec<BytesN<32>>,
    ) -> Result<(), Error> {
        // No `admin.require_auth()` here: the constructor runs atomically with
        // the deploy, so nobody can initialise the contract first, and the
        // factory (#666) must be able to name the platform admin without the
        // admin co-signing every job. The test-only initialisers below still
        // require it.
        if env.storage().instance().has(&DataKey::Job) {
            return Err(Error::AlreadyInitialized);
        }

        // Validate all inputs BEFORE writing any state.
        Self::validate_address(&env, &admin)?;
        Self::validate_address(&env, &client)?;
        Self::validate_address(&env, &freelancer)?;
        Self::validate_address(&env, &arbiter)?;
        Self::validate_address(&env, &token)?;

        // Party-distinctness check (issue #667): the escrow's protection
        // depends on four independent roles.  Collisions remove checks and
        // balances — e.g. client == arbiter lets the client resolve their own
        // disputes, and admin == client gives the client every admin override.
        if client == freelancer
            || arbiter == client
            || arbiter == freelancer
            || admin == client
            || admin == freelancer
        {
            return Err(Error::InvalidParties);
        }

        // milestone_count is u32 (Soroban Vec::len() returns u32) so no cast
        // is needed and there is no overflow risk on the count itself.
        let milestone_count = milestone_amounts.len();
        if milestone_count > MAX_MILESTONES {
            return Err(Error::InvalidMilestone);
        }
        // All per-amount and running-total arithmetic is performed inside
        // checked_initialize_total via i128::checked_add (see its rustdoc).
        // Non-positive amounts and sum overflow both return Err(InvalidAmount).
        let total_amount = Self::checked_initialize_total(&milestone_amounts)?;
        if milestone_spec_hashes.len() != milestone_count {
            return Err(Error::InvalidAmount);
        }

        // auto_release_seconds must be non-zero and have a reasonable upper bound
        // to prevent overflow in deadline calculations (e.g. claim_auto_release).
        if auto_release_seconds == 0 || auto_release_seconds > MAX_AUTO_RELEASE_SECONDS {
            return Err(Error::InvalidDuration);
        }
        if !(MIN_DISPUTE_TIMEOUT_SECONDS..=MAX_DISPUTE_TIMEOUT_SECONDS)
            .contains(&dispute_timeout_seconds)
        {
            return Err(Error::InvalidDisputeTimeout);
        }

        // Write a sentinel immediately to prevent reentrancy: any reentrant
        // call to `initialize` will now see `DataKey::Job` already present and
        // return `AlreadyInitialized` before touching any other state.
        // The sentinel is a zero-value `JobMeta` placeholder; the real meta
        // overwrites it at the end of this function once all validation has
        // passed and milestones have been stored.
        let created_at = env.ledger().timestamp();
        env.storage().instance().set(
            &DataKey::Job,
            &JobMeta {
                client: admin.clone(),
                freelancer: admin.clone(),
                arbiter: admin.clone(),
                token: admin.clone(),
                created_at,
                funded: false,
                auto_release_seconds: 0,
                milestone_count: 0,
                total_amount: 0,
            },
        );

        // Single source of truth: instance storage only (#679).
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Ep, &false);
        env.storage().persistent().set(
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
            .persistent()
            .set(&DataKey::WhitelistedTokens, &whitelist);
        if dispute_timeout_seconds != DISPUTE_TIMEOUT_SECONDS {
            env.storage()
                .instance()
                .set(&DataKey::DisputeTimeout, &dispute_timeout_seconds);
        }

        for index in 0..milestone_count {
            let amount = milestone_amounts
                .get(index)
                .ok_or(Error::InvalidMilestone)?;
            let spec_hash = milestone_spec_hashes
                .get(index)
                .ok_or(Error::InvalidMilestone)?;
            Self::store_milestone(
                &env,
                index,
                &Milestone {
                    amount,
                    released_amount: 0,
                    status: MilestoneStatus::Pending,
                    delivered_at: 0,
                    funded: false,
                    spec_hash,
                    deliverable: BytesN::from_array(&env, &[0u8; 32]),
                },
            );
        }

        env.storage().instance().set(&DataKey::Version, &1u32);
        env.storage()
            .instance()
            .set(&DataKey::SchemaVersion, &SCHEMA_VERSION);
        env.storage().instance().set(&DataKey::FundedCount, &0u32);

        let meta = JobMeta {
            client,
            freelancer,
            arbiter,
            token,
            created_at,
            funded: false,
            auto_release_seconds,
            milestone_count,
            total_amount,
        };

        Self::store_job_meta(&env, &meta);
        env.storage()
            .instance()
            .set(&DataKey::FundingMode, &FundingMode::Upfront);

        // Emit a structured initialization event so downstream indexers can
        // record all operational parameters from a single on-chain event without
        // having to query contract storage separately.
        env.events().publish(
            (
                symbol_short!("init"),
                meta.client.clone(),
                meta.freelancer.clone(),
                meta.arbiter.clone(),
            ),
            InitializedEvent {
                admin,
                client: meta.client,
                freelancer: meta.freelancer,
                arbiter: meta.arbiter,
                token: meta.token,
                auto_release_seconds: meta.auto_release_seconds,
                milestone_amounts,
                milestone_spec_hashes,
                total_amount: meta.total_amount,
                milestone_count: meta.milestone_count,
                created_at: meta.created_at,
            },
        );

        Ok(())
    }

    /// Arbiter fee owed on a disputed `remaining` balance:
    /// `remaining * arbiter_fee_bps / 10_000`, rounded down and capped to
    /// `available` (issue #738).
    pub(crate) fn arbiter_fee_for(env: &Env, remaining: i128, available: i128) -> Result<i128, Error> {
        let fee_bps = Self::get_arbiter_fee_bps(env.clone());
        if fee_bps == 0 {
            return Ok(0);
        }
        let fee = remaining
            .checked_mul(fee_bps as i128)
            .ok_or(Error::ArithmeticOverflow)?
            / BPS_SCALE as i128;
        Ok(fee.min(available))
    }

    /// Shared body of `claim_auto_release` and `trigger_auto_release`: every
    /// milestone-level check, the state commit, and the payout to
    /// `meta.freelancer`. Callers run the guards and any caller checks first.
    pub(crate) fn release_overdue_milestone(
        env: &Env,
        meta: &JobMeta,
        caller: Address,
        milestone_index: u32,
    ) -> Result<(), Error> {
        // CHECK 1: Validate index boundary.
        if milestone_index >= meta.milestone_count {
            return Err(Error::InvalidMilestone);
        }

        let mut milestone = Self::load_milestone(env, milestone_index)?;
        Self::require_milestone_is_funded(env, meta, &milestone)?;

        // CHECK 2: Delivered and PartiallyReleased are both eligible. A
        // partial payment must not switch off the client-silence protection:
        // the freelancer can claim the remaining balance after the deadline.
        // The deadline still runs from the original `delivered_at` (no clock
        // reset on partial payment), so a partial payment cannot postpone it.
        // Released (double-claim), Disputed, Refunded and Pending are rejected,
        // making this guard the gatekeeper against out-of-sequence calls.
        if milestone.status != MilestoneStatus::Delivered
            && milestone.status != MilestoneStatus::PartiallyReleased
        {
            return Err(Error::InvalidStatus);
        }

        // CHECK 3: Read the delivery timestamp from the Milestone entry.
        let delivered_at = milestone.delivered_at;
        let extension = Self::load_time_extension(env, milestone_index);

        let deadline = Self::auto_release_deadline(meta, delivered_at, extension)?;
        let current = env.ledger().timestamp();
        if current < deadline {
            return Err(Error::DeadlineNotPassed);
        }

        // CHECK 4: Compute remaining using checked subtraction so that corrupted
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
        Self::store_milestone(env, milestone_index, &milestone);
        Self::clear_time_ext(env, milestone_index);
        Self::increment_reputation(env, &meta.client, milestone_index);
        Self::increment_reputation(env, &meta.freelancer, milestone_index);

        // INTERACTION: Token transfer is the sole external call and executes only
        //    after all state mutations have been durably persisted.
        let token_client = token::Client::new(env, &meta.token);
        Self::pay_or_credit(env, &token_client, &meta.freelancer, &remaining)?;

        env.events().publish(
            (symbol_short!("claim"), meta.freelancer.clone()),
            ClaimedEvent {
                milestone_index,
                caller,
                freelancer: meta.freelancer.clone(),
                token: meta.token.clone(),
                amount: remaining,
            },
        );

        Ok(())
    }

}
