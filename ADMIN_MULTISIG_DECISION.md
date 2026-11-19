# Admin Multisig Decision

**Status:** Adopt native admin authorization for new mainnet deployments; retain the in-contract multisig until a coordinated migration is approved.

**Decision date:** 2026-10-17

## Context

Admin entry points in the contract authorize through `admin.require_auth()` and compare the supplied address with the stored `DataKey::Admin` ([authorization helpers](contracts/milestone-escrow/src/lib.rs)). This supports Stellar account authorization, including account signers and thresholds, without contract-specific multisig checks on each admin method.

The contract also maintains its own signer configuration, approval records, admin-transfer proposals, multisig locks, overrides, and related events. That is a separate authorization system with a larger state and behavior surface. The frontend currently has a generic Soroban transaction builder in [`app/lib`](https://github.com/Goldii-locks/escrow-frontend/tree/main/app/lib) of [escrow-frontend](https://github.com/Goldii-locks/escrow-frontend); it does not contain multisig transaction assembly or signature-collection hooks.

## Comparison

| Option | Security | User experience | Cost |
|---|---|---|---|
| Classic Stellar M-of-N account as admin | Native account thresholds are enforced by Stellar for every `require_auth()` call. No escrow-specific signer or approval checks are needed. | Signers approve the actual admin transaction through compatible wallets or a coordinated transaction-signing flow. Requires frontend and wallet support. | Adds signatures and their verification overhead to admin transactions, but avoids escrow approval transactions, approval storage, and custom multisig state reads/writes. Measure with the chosen signer count and wallet flow. |
| Smart-wallet contract as admin | `__check_auth` can enforce custom policy, but introduces another contract and custom authorization logic that must be audited and maintained. | Can offer policy-specific workflows, but requires compatible wallet tooling and contract deployment/maintenance. | Adds smart-wallet authorization execution and its own operational costs; measure against the selected policy. |
| Existing in-contract multisig | Bespoke signer, proposal, and approval logic creates a larger contract-specific attack surface and does not automatically protect every admin endpoint. | Requires contract-specific calls and UI state; approvals are separate from signing the actual admin action. | Adds contract storage, reads/writes, and often extra transactions for configuration and approvals. |

## Decision And Rollout

Use a classic Stellar M-of-N account as the platform admin for new mainnet deployments. It is the simplest option that applies the platform's native threshold authorization to every contract admin action. Prefer a smart wallet only if a concrete policy requirement cannot be met by account signers and thresholds.

Do **not** remove the current in-contract multisig in this change. Removal needs an explicit issue-thread decision and coordinated frontend support; neither is represented in this workspace. The existing frontend has no assembly hooks to migrate, and removing public methods/events or stored-key variants without an instance/client compatibility plan would be a breaking change. This is a staged migration decision, not approval to rely on the custom multisig for production security.

Before removing the legacy system:

1. Agree the migration and deprecation plan in the owning issue thread.
2. Implement frontend assembly and collection of all required Stellar account signatures for admin transactions; document supported wallets and failure/retry behavior.
3. Use the classic M-of-N account address as `DataKey::Admin` for new deployments and verify representative admin calls through normal `require_auth()`.
4. Confirm no deployed instance or supported client depends on the legacy methods, events, or state. Preserve serialized key compatibility where live storage requires it.
5. Remove the obsolete contract configuration, approval/proposal/lock paths, and events in a separately reviewed breaking change, with migration and regression tests.

If the chosen admin is instead a smart-wallet contract, add a test wallet whose `__check_auth` rejects one signature and accepts a valid two-signature authorization, and exercise it against `admin_override_release` before removing the in-contract multisig.

## Removal Tracking Plan

Tracking issue: [#1014](https://github.com/Goldii-locks/escrow-contract/issues/1014). Each step below gets its own issue, linked here when it is opened, and is done only when that issue is closed. The legacy surface in scope is `multisig_approval_init`, `multisig_approve`, `is_multisig_approved`, the proposal and config rotation functions, `multisig_lock`, `is_multisig_locked`, and the `multisig_*` overrides that do not consult the multisig (#691), together with their events, error variants, tests, docs and bindings.

| Step | Work | Owner | Issue |
|---|---|---|---|
| 1 | Confirm with the backend and frontend owners that no client calls the legacy functions. The frontend has no multisig flow today. | Backend and frontend owners | To be opened under #1014 |
| 2 | Document a classic M-of-N admin account setup and a signing runbook. | Contract lead | To be opened under #1014 |
| 3 | Deploy testnet with such an account as admin and exercise every admin call. | Release manager | To be opened under #1014 |
| 4 | Enumerate the storage keys that live deployments hold, and decide how `migrate` handles them. | Contract lead | To be opened under #1014 |
| 5 | Remove the functions, events and error variants in one breaking release, keeping `DataKey` discriminants stable. | Contract lead | To be opened under #1014 |

Steps 1-4 are prerequisites for step 5.