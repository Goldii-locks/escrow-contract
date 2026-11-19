# Mainnet Readiness Checklist

Mainnet is **not ready** until every gate below is checked and its evidence is attached to the release record. The owner roles are accountable; name the individual owner in the release PR before work starts. A checked box requires a link to the merged change, issue, audit record, or deployment evidence.

| Done | Gate | Accountable owner | Evidence / tracking |
|---|---|---|---|
| [ ] | Close and verify every P1 contract issue in this launch-blocker batch. | Contract lead | [P1 issue gate](#p1-contract-issue-gate) |
| [ ] | Schedule an external audit or complete it; document scope, auditor, status, and report location. | Security lead | [Audit scope](#audit-scope) |
| [ ] | Merge `THREAT_MODEL.md` and `SPEC.md`; verify both describe the shipped contract. | Contract lead | [Required design documents](#required-design-documents) |
| [ ] | Review fee caps, TTLs, timeouts, and maximum milestone count against launch requirements. | Protocol owner | [Constants review](#constants-review) |
| [ ] | Assign and verify admin key custody and the authority for each operational role. | Security lead | [Admin keys and roles](#admin-keys-and-roles) |
| [ ] | Dry-run deployment on testnet using the exact WASM artifact intended for mainnet; record its hash. | Release manager | [Deployment evidence](#deployment-evidence) and [testnet deploy procedure](README.md#deploy-testnet) |
| [ ] | Publish and rehearse the incident runbook for pausing, communicating, and upgrading. | Operations lead | [Incident runbook](#incident-runbook) |

## P1 Contract Issue Gate

Close each applicable P1 issue, merge its fix, and link passing regression tests before launch. These are the blocker areas identified for this release; confirm the authoritative P1 list in the project's issue tracker rather than treating search results as complete.

| Done | Blocker | Owner | Issue tracking |
|---|---|---|---|
| [ ] | Apply the announced fee to settlement and verify the configured 1% fee is correctly allocated. | Contract lead | [Search issues for fees](https://github.com/search?q=is%3Aissue+%22fees+are+never+applied%22&type=issues) |
| [ ] | Extend required storage TTLs and test entries near their expiry boundary. | Contract lead | [Search issues for TTL](https://github.com/search?q=is%3Aissue+milestone+escrow+TTL&type=issues) |
| [ ] | Remove instant unilateral admin upgrades; require the approved delay or multisig control. | Contract lead | [Search issues for upgrades](https://github.com/search?q=is%3Aissue+milestone+escrow+admin+upgrade&type=issues) |
| [ ] | Resolve the one-job-per-contract limitation or explicitly approve and document that launch constraint. | Contract lead | [Search issues for multiple jobs](https://github.com/search?q=is%3Aissue+milestone+escrow+multiple+jobs&type=issues) |
| [ ] | Replace the release-candidate Soroban SDK with a supported stable release and rerun validation. | Release manager | [Search issues for SDK](https://github.com/search?q=is%3Aissue+milestone+escrow+Soroban+SDK&type=issues) |
| [ ] | Ensure pause state blocks every settlement path, including approval, release, refund, and dispute resolution. | Contract lead | [Search issues for pause and settlement](https://github.com/search?q=is%3Aissue+milestone+escrow+pause+settlement&type=issues) |

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

- [ ] Merge `THREAT_MODEL.md` with assets, trust boundaries, actors, attack surfaces, and mitigations. Owner: Security lead. Reference: [audit scope](#audit-scope).
- [ ] Merge `SPEC.md` with the deployed interface, state machine, fee behavior, limits, and admin controls. Owner: Contract lead. Reference: [documented contract functions](README.md#contract-functions).

Both documents must match the release commit and be linked from the release record. They do not exist in this repository yet; do not mark this gate complete until the documents are added and merged.

## Constants Review

Review the source constants and configuration in [`contracts/milestone-escrow/src/lib.rs`](contracts/milestone-escrow/src/lib.rs) against `SPEC.md`. Record the decision and reviewer for each:

- [ ] Fee rate and fee caps, including confirmation that the advertised 1% fee is enforceable and correctly allocated. Owner: Protocol owner. Reference: [contract configuration](contracts/milestone-escrow/src/lib.rs).
- [ ] Persistent and temporary storage TTLs, renewal triggers, and expiry behavior. Owner: Contract lead. Reference: [contract storage and TTL logic](contracts/milestone-escrow/src/lib.rs).
- [ ] Auto-release and other configured timeouts, including boundary and overflow behavior. Owner: Contract lead. Reference: [contract timeout logic](contracts/milestone-escrow/src/lib.rs).
- [ ] Maximum milestone count and the resource cost at that limit. Owner: Protocol owner. Reference: [contract limits](contracts/milestone-escrow/src/lib.rs).

## Admin Keys and Roles

Follow the [admin multisig decision and migration plan](ADMIN_MULTISIG_DECISION.md).

**Factory admin pinning:** `create_escrow` pins each deployed escrow to the
factory's admin address at creation time. Use a classic Stellar M-of-N account
as the platform admin so that rotating the signer set on that account updates
all existing escrows without any per-escrow transaction. See the runbook in
[THREAT_MODEL.md](THREAT_MODEL.md#factory-created-escrow-admin-pinning).

- [ ] Store admin and upgrade keys in hardware-backed custody or an approved multisig; document recovery and rotation. Owner: Security lead. Reference: [admin controls](contracts/milestone-escrow/src/lib.rs).
- [ ] Assign named custodians and thresholds for contract upgrades, emergency pause/unpause, fee configuration, and routine administration. Owner: Operations lead. Reference: [role requirements](#admin-keys-and-roles).
- [ ] Verify least privilege, independent approvals for upgrades, and a tested recovery path. Owner: Security lead. Reference: [audit scope](#audit-scope).

Keep key material out of this repository and the release record. Record role assignments and verification evidence in the organization’s access-controlled operations document.

## Deployment Evidence

Use the [testnet deployment procedure](README.md#deploy-testnet) to rehearse the release. The dry-run evidence must include:

- [ ] Release commit and the SHA-256 hash of the exact WASM bytes proposed for mainnet. Owner: Release manager. Reference: [testnet deploy procedure](README.md#deploy-testnet).
- [ ] Testnet deployment transaction, resulting contract ID, deployer identity, and verification that the deployed WASM hash matches. Owner: Release manager. Reference: [current testnet deployment record](README.md#deployed-contract).
- [ ] Confirmation that the mainnet release will use the same reviewed artifact, with no rebuild or unreviewed configuration changes. Owner: Contract lead. Reference: [deployment evidence](#deployment-evidence).

Attach the evidence to the release record and have a second person verify the hash before mainnet submission.

## Incident Runbook

Publish the full operational runbook in the organization’s operations documentation and link it from the release record. It must include these rehearsed steps:

1. **Pause:** identify the authorized pause signers, pause all affected settlement paths, verify the on-chain state, and document conditions for resuming.
2. **Communicate:** notify affected users and internal responders through approved channels; state impact, current safeguards, and the next update time without exposing secrets.
3. **Upgrade:** assess and review a fix, obtain required multisig or delayed approvals, deploy the verified artifact, validate behavior on testnet, and record the mainnet transaction and WASM hash.

- [ ] Record responder contacts, escalation thresholds, communication templates, and pause/upgrade authority. Owner: Operations lead. Reference: [incident runbook](#incident-runbook).
- [ ] Rehearse pause, communication, and upgrade procedures and record results. Owner: Operations lead. Reference: [incident runbook](#incident-runbook).