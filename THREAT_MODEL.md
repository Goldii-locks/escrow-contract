# Threat Model

This document describes who can do what in the milestone escrow contract
(`contracts/milestone-escrow`), what each party is trusted with, what is at risk
and what is not protected. It is meant for users deciding whether to trust the
platform with funds and for auditors scoping a review.

Keep it updated as the linked issues are resolved. The transition-level detail
(every entry point, its signers and its effects) lives in
[`contracts/milestone-escrow/SPEC.md`](contracts/milestone-escrow/SPEC.md).

## Assets at risk

- **Escrowed tokens.** The full job amount is deposited by `fund` and held by
  the contract until each milestone is released, refunded or split.
- **Owed balances.** Payouts that could not be transferred at settlement time
  (for example a classic asset whose recipient has no trustline) are held as
  `Owed(address)` credits until the recipient calls `withdraw`.
- **Job integrity.** Party addresses, milestone amounts and statuses, and the
  events indexers use to display activity and reputation.
- **Contract code.** A single admin-controlled upgrade replaces all of the
  above logic.

## Actors and capabilities

### Admin

The admin is the platform. It is set by `initialize` and can be replaced with
`transfer_admin` or `propose_admin_transfer` / `execute_admin_transfer`.

| Capability | Entry points | Mitigation or status |
|---|---|---|
| Upgrade the contract code instantly | `upgrade` | No timelock or delay; blocked only while emergency paused or cancel locked. The admin can therefore replace the logic holding every escrowed token. See [#668](https://github.com/Goldii-locks/escrow-contract/issues/668), [#718](https://github.com/Goldii-locks/escrow-contract/issues/718), [#773](https://github.com/Goldii-locks/escrow-contract/issues/773). |
| Override any milestone | `admin_override_release`, `admin_override_refund`, `admin_override_streaming_release`, `admin_override_cancel_release`, `admin_override_cancel_refund`, `multisig_admin_override_release`, `multisig_admin_override_refund`, `admin_override_tax_release`, `admin_override_tax_refund`, `admin_override_tax_split_refund` | Releases to the freelancer or refunds to the client on any milestone that is not `Released` or `Refunded`, with no dispute, cancellation or delay required. Funds can only go to the recorded client or freelancer, never to the admin. Every override publishes an event. See [#687](https://github.com/Goldii-locks/escrow-contract/issues/687), [#691](https://github.com/Goldii-locks/escrow-contract/issues/691), [#684](https://github.com/Goldii-locks/escrow-contract/issues/684), [#689](https://github.com/Goldii-locks/escrow-contract/issues/689). |
| Change fees after funding | `set_platform_fee_allocation`, `lock_platform_fee_allocation`, `pf_alloc_admin_override` | No funded check, and the lock can be undone by the admin override. The allocation is not applied to any payout today, so changing it moves no funds. See [#694](https://github.com/Goldii-locks/escrow-contract/issues/694), [#674](https://github.com/Goldii-locks/escrow-contract/issues/674). |
| Pause and unpause | `admin_pause_escrow`, `admin_resume_escrow`, `emergency_unpause`, `emergency_pause_admin_override` | The admin pause does not block the normal workflow endpoints; the admin can set or clear the emergency pause at will, and only the admin can clear a pause the parties set. See [#664](https://github.com/Goldii-locks/escrow-contract/issues/664), [#726](https://github.com/Goldii-locks/escrow-contract/issues/726), [#759](https://github.com/Goldii-locks/escrow-contract/issues/759). |
| Control the token whitelist | `add_whitelisted_token`, `remove_whitelisted_token` | Allowed only before funding. `initialize` whitelists the job's own token, so the list does not restrict anything. See [#680](https://github.com/Goldii-locks/escrow-contract/issues/680). |
| Control yield | `set_escrow_interest_yield`, `lock_escrow_interest_yield`, `unlock_escrow_interest_yield`, `admin_set_yield_rate`, `admin_accrue_yield` | The admin can change yield shares without the parties' consent (`set_interest_yield_consent` is the consented alternative). Accrued yield is a recorded number that is never funded or paid. See [#776](https://github.com/Goldii-locks/escrow-contract/issues/776), [#693](https://github.com/Goldii-locks/escrow-contract/issues/693). |
| Manage the multisig and admin succession | `multisig_approval_init`, `multisig_lock`, `transfer_admin`, `propose_admin_transfer`, `cancel_admin_transfer_proposal` | `transfer_admin` replaces the admin directly without the multisig flow. See [#786](https://github.com/Goldii-locks/escrow-contract/issues/786), [#715](https://github.com/Goldii-locks/escrow-contract/issues/715), [#761](https://github.com/Goldii-locks/escrow-contract/issues/761), [#762](https://github.com/Goldii-locks/escrow-contract/issues/762). |
| Become admin of a fresh deployment | `initialize` | `initialize` is a separate call after deployment, so whoever calls it first becomes admin. See [#665](https://github.com/Goldii-locks/escrow-contract/issues/665), [#667](https://github.com/Goldii-locks/escrow-contract/issues/667). |

### Arbiter

| Capability | Entry points | Mitigation or status |
|---|---|---|
| Resolve disputes | `resolve_dispute`, `apply_dispute_arbitration_split` | Acts only on `Disputed` milestones and can only pay the recorded client and freelancer. There is no timeout if the arbiter never acts and no way to replace the arbiter; the admin overrides are the only fallback. See [#784](https://github.com/Goldii-locks/escrow-contract/issues/784), [#731](https://github.com/Goldii-locks/escrow-contract/issues/731), [#738](https://github.com/Goldii-locks/escrow-contract/issues/738). |
| Accept the job | `accept_arbiter` | The arbiter is named by the job creator but must accept on-chain before disputes can be raised or resolved; an unaccepted job can be reclaimed by the client after the acceptance window. |

### Client and freelancer

| Capability | Entry points | Mitigation or status |
|---|---|---|
| Normal workflow | `fund`, `mark_delivered`, `approve_partial`, `approve_milestone`, `claim_auto_release`, `extend_milestone_deadline` | Each call requires the matching party's signature and the documented source status (see `SPEC.md`). |
| Consent paths | `emergency_pause`, `set_interest_yield_consent`, `payment_streaming_consent`, `time_extensions_consent`, `tax_withholding_deductions` | Require both the client's and the freelancer's signatures in the same transaction. |
| Cancel | `cancel_escrow`, `revoke_cancel_approval` | Both parties must approve; the escrow is then cancel locked and the admin decides whether funds go to the client or the freelancer. A one-sided approval never expires. See [#713](https://github.com/Goldii-locks/escrow-contract/issues/713), [#727](https://github.com/Goldii-locks/escrow-contract/issues/727). |
| Dispute | `raise_dispute` | Either party can freeze a `Pending`, `Delivered` or `PartiallyReleased` milestone; only the arbiter or the admin can unfreeze it. See [#774](https://github.com/Goldii-locks/escrow-contract/issues/774), [#712](https://github.com/Goldii-locks/escrow-contract/issues/712). |
| Block settlement through account state | n/a | Payouts that fail (e.g. a removed trustline) are credited to the recipient and claimed with `withdraw`, so a party cannot block settlement by making their account unable to receive. |
| Accept or ignore a job | `accept_job`, `withdraw_unaccepted` | The freelancer must accept the job on-chain before delivering, agreeing to the milestone amounts and auto-release window. If the freelancer or arbiter does not accept within 7 days of funding, the client can reclaim the full escrow. |

### Multisig signers

| Capability | Entry points | Mitigation or status |
|---|---|---|
| Approve proposal IDs | `multisig_approve` | Approvals are keyed by an opaque `u32` proposal ID that is not bound to the action it authorises, can be reused across admin transfers, live in temporary storage and can expire, and are refused while the escrow balance is zero. They gate only `execute_admin_transfer`. See [#785](https://github.com/Goldii-locks/escrow-contract/issues/785), [#716](https://github.com/Goldii-locks/escrow-contract/issues/716), [#787](https://github.com/Goldii-locks/escrow-contract/issues/787), [#691](https://github.com/Goldii-locks/escrow-contract/issues/691). |

### Anyone

| Capability | Entry points | Mitigation or status |
|---|---|---|
| Call calculation endpoints and emit events | `split_refund_net_distribution`, `calculate_platform_fee_split`, `escrow_interest_yield`, `interest_yield_split_refund`, `tax_withholding_split_refund`, `cancel_escrow_split_refund` | Several need no signature and publish events carrying caller-supplied values, which indexers can record as real activity. Indexers should only trust events from state-changing entry points. See [#676](https://github.com/Goldii-locks/escrow-contract/issues/676), [#692](https://github.com/Goldii-locks/escrow-contract/issues/692). |
| Execute an approved admin transfer | `execute_admin_transfer` | Succeeds only once the pending proposal has reached the multisig threshold. |
| Send tokens directly to the contract | token `transfer` | Tokens not deposited through `fund` are not tracked by any milestone and cannot be recovered. See [#760](https://github.com/Goldii-locks/escrow-contract/issues/760). |

### Admin succession and role separation

- **Two-step admin transfer (#953).** `transfer_admin` and
  `execute_admin_transfer` only propose; the stored admin changes when the
  proposed address signs `accept_admin` within 7 days. A typo or an address
  without a usable key can no longer take the admin out of reach, and an
  unclaimed proposal expires instead of staying redeemable. The current admin
  keeps full authority, including `cancel_admin_transfer`, until acceptance.
  Residual risk: a compromised admin can still propose a transfer to itself or
  an accomplice, which then completes on acceptance; the multisig regime
  (#786) is the control for that.
- **Parties never hold admin roles (#954).** `set_admin_role` rejects the
  client, freelancer and arbiter, and `rotate_freelancer` / `replace_arbiter`
  reject role holders, so a party cannot pause, re-fee, override or upgrade its
  own escrow through a delegated role.
- **Reputation (#952).** The client is credited only for releases it approved,
  so an auto-release it failed to review or a dispute it lost cannot inflate
  its score. Reputation is a counter, not an access control: nothing in the
  contract grants rights based on it.

## Trust assumptions

1. **The admin is trusted not to steal.** The admin can upgrade the code
   instantly and override any open milestone. Override payouts can only go to the
   recorded client or freelancer, but an upgrade can change that. Users who do
   not trust the platform operator should not use the contract until an upgrade
   timelock exists ([#668](https://github.com/Goldii-locks/escrow-contract/issues/668)).
2. **The arbiter is trusted to act, and to act fairly.** An absent arbiter
   leaves disputed funds locked until the admin overrides
   ([#784](https://github.com/Goldii-locks/escrow-contract/issues/784)).
3. **The settlement token behaves like a standard Soroban token.** Balances and
   transfers are taken at face value; fee-on-transfer, rebasing or malicious
   token contracts are not supported.
4. **Indexers read events from state-changing entry points only.** Events from
   calculation endpoints are not evidence of activity
   ([#676](https://github.com/Goldii-locks/escrow-contract/issues/676)).
5. **`initialize` is called by the intended admin immediately after deployment**
   ([#665](https://github.com/Goldii-locks/escrow-contract/issues/665)).

## Factory-created escrow admin pinning

`create_escrow` pins each escrow to the factory's admin address at deploy time.
After that, each escrow's admin is independent: rotating the factory admin does
not affect existing escrows, and incident response after a key compromise
requires one `propose_admin_transfer → approvals → execute_admin_transfer`
sequence per escrow.

**Decision (adopted, see [ADMIN_MULTISIG_DECISION.md](ADMIN_MULTISIG_DECISION.md)):**
Use a classic Stellar M-of-N account as the platform admin address. Rotating
the signer set on that account (via `stellar transaction sign` with the new
key set, or the account's own multisig) updates all existing escrows at once
without touching any escrow contract, because every escrow's `require_auth()`
call is satisfied by the same account address. No on-chain escrow transaction
is needed for a key rotation.

Operational runbook for key compromise:

1. Revoke the compromised signing key on the M-of-N Stellar account.
2. Add a replacement key with at least the same threshold weight.
3. Verify the new key set on testnet before updating mainnet.
4. No escrow contract calls are needed; all existing escrows immediately
   recognise the updated signer set.

The alternative — storing the factory address in each escrow and reading the
current admin via a cross-contract call — was not chosen because it adds a
cross-contract read on every admin call, makes escrows dependent on factory
liveness, and complicates the existing `require_auth()` pattern. The M-of-N
account approach achieves the same rotation effect with no contract changes.

## Out of scope

- Off-chain components: the backend indexer, the frontend
  ([escrow-frontend](https://github.com/Goldii-locks/escrow-frontend)), and how
  milestone descriptions and deliverables are agreed.
- Key management and compromise of any party's signing keys.
- Stellar network, Soroban host and token contract correctness.
- Economic disputes about the quality of delivered work, beyond the on-chain
  dispute and override paths.
- Denial of service through resource limits ([#700](https://github.com/Goldii-locks/escrow-contract/issues/700), [#764](https://github.com/Goldii-locks/escrow-contract/issues/764)).
