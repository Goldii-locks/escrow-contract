# Authorization Requirements

Every state-changing public entry point on `MilestoneEscrow` and its required
signer(s).  The rows have matching tests in
`src/tests/auth_requirements_tests.rs`, except for entry points added after it
(`rotate_freelancer`, `replace_arbiter`, the signer-set rotation calls,
`migrate`, milestone amendment, `set_tax_recipient`), whose auth is tested in
their own test modules.

Functions that name a role (issue #761) need the address `set_admin_role`
assigned to that role, or the root admin while the role is unassigned.

Read-only / pure-calculation functions (no storage writes, no token transfers)
are listed at the bottom as **No auth required**.

| `pub fn` | Required signer(s) | Auth helper |
|---|---|---|
| `__constructor` | none: runs once, at deploy, with the deployer's arguments | `initialize_inner` |
| `initialize` | `admin` | `admin.require_auth()` |
| `initialize_with_arbiter_fee` | `admin` | `admin.require_auth()` |
| `initialize_with_dispute_timeout` | `admin` | `admin.require_auth()` |
| `initialize_with_funding_mode` | `admin` | `admin.require_auth()` |
| `set_admin_role` | root `admin` | `require_admin_from_instance` |
| `transfer_admin` | `current_admin` | `current_admin.require_auth()` |
| `add_whitelisted_token` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `remove_whitelisted_token` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `upgrade` | `Upgrader` role (root `admin` by default) | `require_role(AdminRole::Upgrader)` |
| `fund` | `client` | `client.require_auth()` |
| `fund_milestone` | `client` | `client.require_auth()` |
| `accept_job` | `freelancer` | `freelancer.require_auth()` |
| `accept_arbiter` | `arbiter` | `arbiter.require_auth()` |
| `withdraw_unaccepted` | `client` | `client.require_auth()` |
| `mark_delivered` | `freelancer` | `freelancer.require_auth()` |
| `request_revision` | `client` | `client.require_auth()` |
| `extend_milestone_deadline` | `client` **and** `freelancer` (both) | `client.require_auth()` + `meta.freelancer.require_auth()` (#670) |
| `claim_auto_release` | `freelancer` | `freelancer.require_auth()` |
| `approve_partial` | `client` | `client.require_auth()` |
| `approve_milestone` | `client` | `client.require_auth()` |
| `withdraw` | `caller` (any address with owed balance) | `caller.require_auth()` |
| `withdraw_to` | `caller` (any address with owed balance) | `caller.require_auth()` |
| `raise_dispute` | `caller` (client **or** freelancer) | `require_dispute_party` |
| `escalate_dispute` | `caller` (client **or** freelancer) | `require_dispute_party` |
| `resolve_dispute` | `arbiter` | `arbiter.require_auth()` |
| `apply_dispute_arbitration_split` | `arbiter` | `arbiter.require_auth()` |
| `cancel_escrow` | `caller` (client **or** freelancer) | `caller.require_auth()` |
| `revoke_cancel_approval` | `caller` (client **or** freelancer) | `caller.require_auth()` |
| `admin_override_cancel_release` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `admin_override_cancel_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `cancel_escrow_claim_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `emergency_pause` | `client` **and** `freelancer` (both) | `client.require_auth()` + `freelancer.require_auth()` |
| `emergency_unpause` | `Operator` role (root `admin` by default) | `require_role(AdminRole::Operator)` |
| `emergency_unpause_parties` | `client` **and** `freelancer` (both) | `client.require_auth()` + `freelancer.require_auth()` |
| `emergency_pause_admin_override` | `Operator` role (root `admin` by default) | `require_role(AdminRole::Operator)` |
| `emergency_pause_split_refund` | `caller` (client **or** freelancer **or** admin) | `caller.require_auth()` |
| `emergency_pause_claim_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `emergency_pause_allocation` | `caller` (client **or** freelancer **or** admin) | `caller.require_auth()` |
| `set_platform_fee_allocation` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `lock_platform_fee_allocation` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `pf_alloc_admin_override` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `payment_streaming_milestones` | `client` **and** `freelancer` (both) | `require_client_and_freelancer_consent` |
| `payment_streaming_consent` | `client` **and** `freelancer` (both) | `require_client_and_freelancer_consent` |
| `milestone_time_extensions` | `client` **and** `freelancer` (both) | `require_client_and_freelancer_consent` |
| `time_extensions_consent` | `client` **and** `freelancer` (both) | `require_client_and_freelancer_consent` |
| `multisig_approval_init` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `multisig_approve` | `signer` (must be registered multisig signer) | `signer.require_auth()` |
| `multisig_admin_override_release` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `multisig_admin_override_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `multisig_split_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `multisig_lock` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `propose_admin_transfer` | `admin` | `admin.require_auth()` (inline) |
| `cancel_admin_transfer_proposal` | `admin` | `require_admin` |
| `sweep_excess` | `caller` (client **or** admin) | `caller.require_auth()` |
| `escrow_interest_yield` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `set_escrow_interest_yield` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `set_interest_yield_consent` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `lock_escrow_interest_yield` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `unlock_escrow_interest_yield` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `admin_set_yield_rate` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `admin_accrue_yield` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `admin_override_release` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `admin_override_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `admin_override_streaming_release` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `admin_pause_escrow` | `Operator` role (root `admin` by default) | `require_role(AdminRole::Operator)` |
| `admin_resume_escrow` | `Operator` role (root `admin` by default) | `require_role(AdminRole::Operator)` |
| `tax_withholding_deductions` | `client` **and** `freelancer` (both) | `meta.client.require_auth()` + `meta.freelancer.require_auth()` |
| `admin_override_tax_release` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `admin_override_tax_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `admin_override_tax_split_refund` | `Resolver` role (root `admin` by default) | `require_role(AdminRole::Resolver)` |
| `admin_tax_withholding_deductions` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `set_tax_recipient` | `FeeManager` role (root `admin` by default) | `require_role(AdminRole::FeeManager)` |
| `migrate` | `Upgrader` role (root `admin` by default) | `require_role(AdminRole::Upgrader)` |
| `propose_multisig_config` | `admin` | `require_admin` |
| `cancel_multisig_config_proposal` | `admin` | `require_admin` |
| `execute_multisig_config` | none: controlled by the current signers' approvals of the proposal | — |
| `add_milestone` | `client` **and** `freelancer` (both) | `client.require_auth()` + `freelancer.require_auth()` |
| `reduce_milestone` | `client` **and** `freelancer` (both) | `client.require_auth()` + `freelancer.require_auth()` |
| `increase_milestone` | `client` **and** `freelancer` (both) | `client.require_auth()` + `freelancer.require_auth()` |
| `approve_milestones` | `client` | `client.require_auth()` |
| `rotate_freelancer` | `client` **and** `freelancer` (both) | `require_client_and_freelancer_consent` |
| `replace_arbiter` | `client` **and** `freelancer` (both) | `require_client_and_freelancer_consent` |
| `submit_evidence` | `caller` (the client or the freelancer) | `require_dispute_party` |
| `trigger_auto_release` | none: pays the stored freelancer only, once the deadline has passed | — |

## No auth required (read-only / pure calculation)

| `pub fn` | Notes |
|---|---|
| `is_token_whitelisted` | Pure read |
| `get_whitelisted_tokens` | Pure read |
| `get_funding_mode` | Pure read |
| `get_revision_count` | Pure read |
| `time_until_auto_release` | Pure read |
| `dispute_arbitration_split` | Pure calculation (no storage write) |
| `execute_admin_transfer` | Controlled by multisig threshold, no caller auth |
| `is_emergency_paused` | Pure read |
| `get_platform_fee_allocation` | Pure read |
| `calculate_platform_fee_split` | Pure calculation |
| `compute_ratio_allocation` | Pure calculation |
| `cancel_escrow_split_refund` | Pure arithmetic + state guards |
| `emergency_pause_split_refund` (calculator path) | Caller auth but no privileged role required beyond being client/freelancer/admin |
| `is_multisig_approved` | Pure read |
| `get_pending_admin_transfer` | Pure read |
| `version` | Pure read |
| `get_job` | Pure read |
| `get_milestone_spec_hash` | Pure read |
| `get_reputation` | Pure read |
| `is_escrow_interest_yield_locked` | Pure read |
| `interest_yield_split_refund` | Pure arithmetic + state guards |
| `get_escrow_interest_yield` | Pure read |
| `get_yield_info` | Pure read |
| `tax_withholding_split_refund` | Pure arithmetic |
| `split_refund_net_distribution` | Pure arithmetic + state guards |
| `is_multisig_locked` | Pure read |
| `get_arbiter_fee_bps` | Pure read |
| `get_dispute_info` | Pure read |
| `get_escrow_state` | Pure read |
| `get_milestone` | Pure read |
| `milestone_count` | Pure read |
| `get_pending_multisig_config` | Pure read |
| `get_tax_recipient` | Pure read |
| `schema_version` | Pure read |
| `get_admin_role` | Pure read |
