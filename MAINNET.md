# Mainnet Readiness Checklist

Mainnet is **not ready** until every gate below is checked and its evidence is attached to the release record. The owner roles are accountable; name the individual owner in the release PR before work starts. A checked box requires a link to the merged change, issue, audit record, or deployment evidence.

| Done | Gate | Accountable owner | Evidence / tracking |
|---|---|---|---|
| [ ] | Close and verify every P1 contract issue in this launch-blocker batch. | Contract lead | [P1 issue gate](#p1-contract-issue-gate): [#674](https://github.com/Goldii-locks/escrow-contract/issues/674), [#694](https://github.com/Goldii-locks/escrow-contract/issues/694), [#989](https://github.com/Goldii-locks/escrow-contract/issues/989), [#956](https://github.com/Goldii-locks/escrow-contract/issues/956), [#929](https://github.com/Goldii-locks/escrow-contract/issues/929), [#923](https://github.com/Goldii-locks/escrow-contract/issues/923), [#972](https://github.com/Goldii-locks/escrow-contract/issues/972), [#994](https://github.com/Goldii-locks/escrow-contract/issues/994), [#973](https://github.com/Goldii-locks/escrow-contract/issues/973) |
| [ ] | Schedule an external audit or complete it; document scope, auditor, status, and report location. | Security lead | [Audit scope](#audit-scope); no audit tracking issue exists yet |
| [ ] | Verify `THREAT_MODEL.md`, `SPEC.md` and `contracts/milestone-escrow/SPEC.md` describe the shipped contract. | Contract lead | [Required design documents](#required-design-documents) |
| [ ] | Review fee caps, TTLs, timeouts, and maximum milestone count against launch requirements. | Protocol owner | [Constants review](#constants-review): [#674](https://github.com/Goldii-locks/escrow-contract/issues/674), [#956](https://github.com/Goldii-locks/escrow-contract/issues/956), [#972](https://github.com/Goldii-locks/escrow-contract/issues/972) |
| [ ] | Assign and verify admin key custody and the authority for each operational role. | Security lead | [Admin keys and roles](#admin-keys-and-roles): [#761](https://github.com/Goldii-locks/escrow-contract/issues/761), [#668](https://github.com/Goldii-locks/escrow-contract/issues/668) |
| [ ] | Dry-run deployment on testnet using the exact WASM artifact intended for mainnet; record its hash. | Release manager | [Deployment evidence](#deployment-evidence) and [testnet deploy procedure](README.md#deploy-testnet): [#942](https://github.com/Goldii-locks/escrow-contract/issues/942) |
| [ ] | Publish and rehearse the incident runbook for pausing, communicating, and upgrading. | Operations lead | [Incident runbook](#incident-runbook): [#973](https://github.com/Goldii-locks/escrow-contract/issues/973), [#994](https://github.com/Goldii-locks/escrow-contract/issues/994), [#932](https://github.com/Goldii-locks/escrow-contract/issues/932) |

## P1 Contract Issue Gate

Close each applicable P1 issue, merge its fix, and link passing regression tests before launch. Each row links the specific issues it depends on; check their state in the issue tracker before marking a row done.

| Done | Blocker | Owner | Issue tracking |
|---|---|---|---|
| [ ] | Apply the platform fee allocation at settlement and give it a treasury address; stop the admin changing the allocation after funding or undoing its lock. | Contract lead | [#674](https://github.com/Goldii-locks/escrow-contract/issues/674) (open), [#694](https://github.com/Goldii-locks/escrow-contract/issues/694) (open) |
| [ ] | Make admin tax settlements after a partial release pay only from the settled milestone. [#689](https://github.com/Goldii-locks/escrow-contract/issues/689) was closed without fixing this. | Contract lead | [#989](https://github.com/Goldii-locks/escrow-contract/issues/989) (open) |
| [ ] | Extend the TTL of every stored entry, not only `Milestone` and the escrow instance, and test entries near their expiry boundary. Milestone and instance TTL extension landed in [#669](https://github.com/Goldii-locks/escrow-contract/issues/669). | Contract lead | [#956](https://github.com/Goldii-locks/escrow-contract/issues/956) (open), [#929](https://github.com/Goldii-locks/escrow-contract/issues/929) (open, factory instance) |
| [ ] | Remove instant unilateral admin upgrades; require the approved delay or multisig control. [#668](https://github.com/Goldii-locks/escrow-contract/issues/668) is closed; `upgrade` is gated by the `Upgrader` role ([#761](https://github.com/Goldii-locks/escrow-contract/issues/761)) and takes effect in the same call, with no delay. | Contract lead | [#668](https://github.com/Goldii-locks/escrow-contract/issues/668) (closed), [#761](https://github.com/Goldii-locks/escrow-contract/issues/761) (closed) |
| [x] | Resolve the one-job-per-contract limitation or explicitly approve and document that launch constraint. | Contract lead | [#666](https://github.com/Goldii-locks/escrow-contract/issues/666) (closed): [`contracts/escrow-factory`](contracts/escrow-factory) deploys one escrow per job |
| [x] | Replace the release-candidate Soroban SDK with a supported stable release and rerun validation. | Release manager | [#703](https://github.com/Goldii-locks/escrow-contract/issues/703) (closed): the workspace pins `soroban-sdk = "28.0.0"` in [`Cargo.toml`](Cargo.toml) |
| [ ] | Ensure pause state blocks every settlement path, including approval, release, refund, and dispute resolution. `admin_pause_escrow` blocks fund, approve, claim and dispute since [#664](https://github.com/Goldii-locks/escrow-contract/issues/664). | Contract lead | [#923](https://github.com/Goldii-locks/escrow-contract/issues/923) (open), [#972](https://github.com/Goldii-locks/escrow-contract/issues/972) (open), [#994](https://github.com/Goldii-locks/escrow-contract/issues/994) (open) |
| [ ] | Allow `upgrade` while the escrow is paused, so an incident can be paused and then patched. | Contract lead | [#973](https://github.com/Goldii-locks/escrow-contract/issues/973) (open) |

## Audit Scope

The audit request and report should cover, at minimum:

- Authorization, admin transfers, upgrade authority, and multisig thresholds.
- Reentrancy protections and state transitions across fund, approval, auto-release, refunds, and disputes.
- Fee calculation, caps, rounding, and payout allocation.
- Persistent and temporary storage TTL behavior and recovery of expired entries.
- Pause enforcement on every value-moving entry point.
- Arithmetic bounds, milestone limits, and token interactions.

Record the auditor, scope version, scheduled/completed dates, findings, and remediation status in the release record. Owner: Security lead.

## Required Design Documents

- [x] Merge `THREAT_MODEL.md`. Owner: Security lead. Evidence: [`THREAT_MODEL.md`](THREAT_MODEL.md).
- [x] Merge a contract specification. Owner: Contract lead. Evidence: [`SPEC.md`](SPEC.md) (observable behaviour) and [`contracts/milestone-escrow/SPEC.md`](contracts/milestone-escrow/SPEC.md) (state transitions, signers and events).
- [ ] Verify `THREAT_MODEL.md` covers assets, trust boundaries, actors, attack surfaces, and mitigations for the release commit. Owner: Security lead. Reference: [audit scope](#audit-scope).
- [ ] Verify both specifications cover the deployed interface, state machine, fee behavior, limits, and admin controls for the release commit. Owner: Contract lead. Reference: [documented contract functions](README.md#contract-functions).

All three documents must match the release commit and be linked from the release record.

## Constants Review

Review the source constants and configuration in [`contracts/milestone-escrow/src/lib.rs`](contracts/milestone-escrow/src/lib.rs) against `SPEC.md`. Record the decision and reviewer for each:

- [ ] Fee rate and fee caps, including confirmation that the fee allocation is applied and correctly allocated at settlement. Owner: Protocol owner. Reference: [contract configuration](contracts/milestone-escrow/src/lib.rs), [#674](https://github.com/Goldii-locks/escrow-contract/issues/674).
- [ ] Persistent and temporary storage TTLs, renewal triggers, and expiry behavior. Owner: Contract lead. Reference: [contract storage and TTL logic](contracts/milestone-escrow/src/lib.rs), [#956](https://github.com/Goldii-locks/escrow-contract/issues/956).
- [ ] Auto-release and other configured timeouts, including boundary and overflow behavior. Owner: Contract lead. Reference: [contract timeout logic](contracts/milestone-escrow/src/lib.rs), [#972](https://github.com/Goldii-locks/escrow-contract/issues/972).
  - Dispute timeout: `dispute_timeout_seconds` must lie within `MIN_DISPUTE_TIMEOUT_SECONDS` (1 day) and `MAX_DISPUTE_TIMEOUT_SECONDS` (180 days), enforced by the escrow constructor and mirrored in the factory (`InvalidDisputeTimeout` outside the range). The floor gives the arbiter time to act before escalation; the cap keeps `escalate_dispute` and `settle_all_remaining` reachable if the arbiter disappears (#992).
- [ ] Maximum milestone count and the resource cost at that limit. Owner: Protocol owner. Reference: [contract limits](contracts/milestone-escrow/src/lib.rs).

## Admin Keys and Roles

Follow the [admin multisig decision and migration plan](ADMIN_MULTISIG_DECISION.md).

- [ ] Store admin and upgrade keys in hardware-backed custody or an approved multisig; document recovery and rotation. Owner: Security lead. Reference: [admin controls](contracts/milestone-escrow/src/lib.rs).
- [ ] Assign named custodians and thresholds for contract upgrades, emergency pause/unpause, fee configuration, and routine administration. The `Upgrader`, `Operator`, `FeeManager` and `Resolver` roles from [#761](https://github.com/Goldii-locks/escrow-contract/issues/761) can each be assigned with `set_admin_role`. Owner: Operations lead. Reference: [administrative roles](README.md#administrative-roles), [`AUTH.md`](contracts/milestone-escrow/AUTH.md).
- [ ] Verify least privilege, independent approvals for upgrades, and a tested recovery path. Owner: Security lead. Reference: [audit scope](#audit-scope).

Keep key material out of this repository and the release record. Record role assignments and verification evidence in the organization’s access-controlled operations document.

## Deployment Evidence

Use the [testnet deployment procedure](README.md#deploy-testnet) to rehearse the release. The dry-run evidence must include:

- [ ] Release commit and the SHA-256 hash of the exact WASM bytes proposed for mainnet. Owner: Release manager. Reference: [testnet deploy procedure](README.md#deploy-testnet).
- [ ] Testnet deployment transaction, resulting contract ID, deployer identity, and verification that the deployed WASM hash matches. Owner: Release manager. Reference: [current testnet deployment record](README.md#deployed-contract), [#942](https://github.com/Goldii-locks/escrow-contract/issues/942). Reproducible builds landed in [#773](https://github.com/Goldii-locks/escrow-contract/issues/773).
- [ ] Confirmation that the mainnet release will use the same reviewed artifact, with no rebuild or unreviewed configuration changes. Owner: Contract lead. Reference: [deployment evidence](#deployment-evidence).

Attach the evidence to the release record and have a second person verify the hash before mainnet submission.

## Incident Runbook

Publish the full operational runbook in the organization’s operations documentation and link it from the release record. It must include these rehearsed steps:

1. **Pause:** identify the authorized pause signers, pause all affected settlement paths, verify the on-chain state, and document conditions for resuming. The escrow has two independent admin pause switches ([#994](https://github.com/Goldii-locks/escrow-contract/issues/994)); the factory has none ([#932](https://github.com/Goldii-locks/escrow-contract/issues/932)).
2. **Communicate:** notify affected users and internal responders through approved channels; state impact, current safeguards, and the next update time without exposing secrets.
3. **Upgrade:** assess and review a fix, obtain required multisig or delayed approvals, deploy the verified artifact, validate behavior on testnet, and record the mainnet transaction and WASM hash. Then call `migrate` if the new code raises `SCHEMA_VERSION`. `upgrade` currently refuses to run while the escrow is paused ([#973](https://github.com/Goldii-locks/escrow-contract/issues/973)).

- [ ] Record responder contacts, escalation thresholds, communication templates, and pause/upgrade authority. Owner: Operations lead. Reference: [incident runbook](#incident-runbook).
- [ ] Rehearse pause, communication, and upgrade procedures and record results. Owner: Operations lead. Reference: [incident runbook](#incident-runbook).