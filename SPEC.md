# Milestone Escrow — Contract Specification

This document is the normative reference for the observable behaviour of the
`milestone-escrow` contract (`contracts/milestone-escrow/src/lib.rs`).
Where this document and the code disagree, the code is authoritative; please
open an issue so the two can be reconciled.

## Milestone lifecycle

Each milestone carries a `MilestoneStatus`:

| Status              | Meaning                                                              |
| ------------------- | -------------------------------------------------------------------- |
| `Pending`           | Funded, not yet delivered.                                           |
| `Delivered`         | The freelancer has marked the milestone delivered.                   |
| `PartiallyReleased` | Some, but not all, of the milestone has been released to the freelancer. |
| `Released`          | Fully paid out to the freelancer. Terminal.                          |
| `Refunded`          | Fully refunded to the client. Terminal.                              |
| `Disputed`          | Frozen for arbitration.                                              |
| `Split`             | Settled by an arbiter-directed split. Terminal.                      |

## Funding modes

The escrow supports two funding modes.

### Upfront (default)

`initialize` always uses `FundingMode::Upfront`. The client calls `fund(client)` once and transfers the sum of every milestone amount. No milestone may be delivered or settled until that full deposit succeeds. Calling `fund_milestone` in this mode returns `InvalidFundingMode`.

### PerMilestone

`initialize_with_funding_mode(..., FundingMode::PerMilestone)` lets the client fund each milestone independently with `fund_milestone(client, milestone_index)`.

- Only the selected milestone amount is transferred.
- Duplicate funding of the same milestone returns `AlreadyFunded`.
- `fund` is rejected with `InvalidFundingMode`.
- A milestone that is not funded cannot be delivered, approved, disputed, released, refunded, or auto-released (`NotFunded`).
- Unrelated unfunded milestones do not block work on a funded milestone.
- `get_job` exposes each milestone's `funded` flag. `get_funding_mode` returns the mode.

Funding is recorded only after the client balance is confirmed sufficient, so a failed transfer does not mark a milestone funded.

## Conservation

A milestone can never release more than its funded amount. Partial approvals, refunds, and dispute splits operate only against that remaining funded balance.

## Revision workflow

After a milestone is `Delivered`, the authorized client may call
`request_revision(client, milestone_index, note_hash)`.

- The milestone returns to `Pending`.
- `delivered_at` is cleared (persistent and temporary), so the previous
  auto-release window cannot fire.
- `revision_count` increments by one. `MAX_REVISIONS` is 3.
- Further revision requests after the cap return `MaxRevisions`; the client
  must approve, partially approve, or dispute.
- Once the auto-release deadline has passed, `request_revision` returns
  `ReviewWindowClosed`.
- The on-chain event `msrev` carries `milestone_index`, `note_hash`, and
  `revision_count`. The note itself is not stored.
- A later `mark_delivered` writes a fresh `delivered_at` and starts a new
  auto-release window.
- Revision does not change funding, released amounts, or escrow balances.

## Excess asset recovery

`sweep_excess(caller, token)` lets the **client or admin** recover assets that
are not required to settle the escrow.

- Escrow token: sweepable amount is `contract_balance - outstanding_obligations`.
  Obligations are each non-terminal milestone's `amount - released_amount`
  (Pending, Delivered, PartiallyReleased, Disputed), plus any accrued yield.
  Terminal statuses (`Released`, `Refunded`, `Split`) are excluded so they
  cannot be double-counted after settlement.
- If the contract balance is below obligations, the call returns
  `InsufficientBalance` and transfers nothing.
- Zero excess returns `NothingToSweep`.
- Other tokens: the full contract balance is recoverable (they are not used
  for milestone settlement).
- Excess is always sent to the client. Callers cannot pick another recipient.
- Emits `sweep` with token, amount, and recipient.

## Emergency pause control

- `emergency_pause(client, freelancer)`: both parties must authorize. Sets a
  party-owned pause (`EpParty = true`).
- `emergency_unpause_parties(client, freelancer)`: both parties must authorize
  to lift a pause **they jointly created**. One party acting alone is
  `Unauthorized`. This path cannot lift an admin pause.
- `emergency_unpause(admin)`: admin recovery for any pause.
- `admin_pause_escrow(admin)` / `admin_resume_escrow(admin)`: the single admin
  pause (`Paused`). A party unpause cannot lift it.
- `emergency_pause_admin_override(admin)`: deprecated alias that sets the admin
  pause (`Paused`).
- There are two pauses, admin and party, and every entry point checks them
  through one helper; either one blocks the guarded endpoints.
- Unpausing a running escrow returns `NotPaused`.
- Pause state is per contract instance; unpausing one escrow does not affect
  another.

## Reputation

`get_reputation(address)` returns a count of milestones that ended well for
that address. Each increment emits a `rep` event (`ReputationChangedEvent`:
`address`, `new_score`, `milestone_index`, #736). Scores only increase;
no negative signal is tracked.

Rules (#952):

- **Freelancer:** +1 whenever a milestone reaches `Released` with its whole
  amount paid to the freelancer, by any path: `approve_milestone`,
  `approve_milestones`, the final `approve_partial`, `claim_auto_release` /
  `trigger_auto_release`, `resolve_dispute` in the freelancer's favour, an
  arbitration split that pays the freelancer everything,
  `payment_streaming_consent`, and `admin_override_streaming_release`.
- **Client:** +1 only when the client explicitly approved that release:
  `approve_milestone`, each milestone released by `approve_milestones`, the
  `approve_partial` call that releases the rest of a milestone, and a
  `payment_streaming_consent` the client co-signed that releases it in full.
- **No client credit** when the client did not approve: an overdue
  auto-release (the client failed to review in time), a dispute the arbiter
  decided for the freelancer (the client lost), an arbitration split, or an
  admin override. Refunds, splits that return money to the client, and
  partial releases credit nobody.

The client score therefore measures approvals the client gave, and the
freelancer score measures milestones the freelancer was paid for.

## Admin transfers

Admin hand-over is two-step on both paths (#953):

1. **Propose.** `transfer_admin(current_admin, new_admin)` (single admin), or
   `execute_admin_transfer()` once a multisig proposal has reached its
   threshold, records a `PendingAdminAcceptance` and emits `adm_prop` with
   `expires_at` = now + `ADMIN_ACCEPTANCE_WINDOW_SECONDS` (7 days). The stored
   admin does **not** change; the current admin keeps every power.
2. **Accept.** `accept_admin(new_admin)`, signed by `new_admin` itself, at or
   before `expires_at`, installs `new_admin` as the stored admin, clears the
   hand-over, and emits `adm_acc` followed by `admin` (`TransferAdminEvent`).
   Any other address is `Unauthorized`; after `expires_at` it is
   `AdminTransferExpired`.

A hand-over awaiting acceptance can end two other ways:

- **Cancel.** `cancel_admin_transfer(admin)` by the current admin removes it
  and emits `adm_cncl`. A cancelled multisig hand-over must be proposed and
  approved again.
- **Expire.** After `expires_at`, `expire_admin_transfer()` (anyone) removes
  it and emits `adm_exp`. A new `transfer_admin` or `propose_admin_transfer`
  also clears a lapsed hand-over first, emitting `adm_exp`.

While an unexpired hand-over is pending, `transfer_admin` and
`propose_admin_transfer` return `AdminTransferPending`, so at most one
transfer is in flight. A legacy same-admin multisig proposal (recorded before
#771) changes nobody's authority and completes in `execute_admin_transfer`
without an acceptance step. `get_pending_admin_acceptance()` returns the
pending hand-over.

Why: a one-step transfer to a mistyped, lost or contract address cannot be
undone, because the old admin loses control in the same call. Requiring the
new admin's own signature proves the key works before authority moves, and the
expiry stops an unclaimed proposal from being accepted much later.

## Admin roles and job parties

`set_admin_role` refuses to delegate `Upgrader`, `Operator`, `FeeManager` or
`Resolver` to the job's client, freelancer or arbiter (`InvalidParties`,
#954). Returning a role to the admin is always allowed. `rotate_freelancer` and
`replace_arbiter` refuse an incoming address that currently holds a delegated
role (`InvalidParties`). A party with an admin role could pause, re-fee,
override payouts on, or upgrade the escrow it is a party to, so the two sets
never overlap.

## Dispute semantics

A dispute is raised with `raise_dispute(caller, milestone_index, reason_hash)`.
Either the client or the freelancer may raise it, and the caller must authorize
the call. `reason_hash` is a 32-byte commitment to the off-chain statement that
explains the dispute; it is stored on-chain and echoed in the `dispute` event so
arbiters can anchor the claim without trusting off-chain metadata (#712).
Raise-able statuses and their intended meaning:

| Status at raise time | Allowed? | Meaning                                                                 |
| -------------------- | -------- | ----------------------------------------------------------------------- |
| `Pending`            | No       | Nothing has been delivered to arbitrate — returns `InvalidStatus` (#774, #784). |
| `Delivered`          | Yes      | **Delivery (quality) dispute** on submitted work.                        |
| `PartiallyReleased`  | Yes      | **Remainder dispute** over the not-yet-released balance.                 |
| `Disputed`           | No       | Already disputed — returns `InvalidStatus` (see `DisputeAlreadyRaised` for the lock path). |
| `Released`           | No       | Terminal — returns `InvalidStatus`.                                      |
| `Refunded`           | No       | Terminal — returns `InvalidStatus`.                                      |
| `Split`              | No       | Terminal — returns `InvalidStatus`.                                      |

Notes:

- Raising a dispute moves the milestone to `Disputed` and freezes it. While
  `Disputed`, `mark_delivered` is refused (`mark_delivered` only accepts
  `Pending`), so the way forward is the arbiter, or escalation after the
  dispute timeout (below).
- The arbiter resolves a `Disputed` milestone with one of:
  - `resolve_dispute(release_to_freelancer = true)` → `Released`
  - `resolve_dispute(release_to_freelancer = false)` → `Refunded`
  - `apply_dispute_arbitration_split(client_refund_bps)` → `Split` (full
    refund becomes `Refunded`, full freelancer award becomes `Released`).
  Both closing calls also take a 32-byte `decision_hash`, a commitment to the
  arbiter's written decision that stays off-chain in escrow-backend storage,
  keyed by that hash. The hash is echoed in the `resolve` event (and `arbsplit`
  for a split); it changes neither the outcome nor the fund distribution
  (#1015).
- While a milestone is `Disputed`, either party may anchor evidence with
  `submit_evidence(caller, milestone_index, evidence_hash)`. Each hash is
  appended to the dispute record (at most `MAX_DISPUTE_EVIDENCE_ENTRIES` = 10,
  then `EvidenceLimitReached`), timestamped, and published as an `evidence`
  event. `get_dispute_info` returns the reason hash, `raised_by` (the party
  that called `raise_dispute`, #997), `disputed_at`, the pre-dispute status
  and every evidence entry; the record is deleted when the
  dispute closes, so evidence never outlives the dispute it belongs to (#712).
- If the arbiter has not acted within the escrow's dispute timeout (30 days
  by default, set with `initialize_with_dispute_timeout`), either party may
  call `escalate_dispute` to return the milestone to its pre-dispute status;
  a restored `Delivered` milestone keeps its original auto-release deadline
  (`delivered_at` is not reset), so a dispute → escalate → dispute loop cannot
  extend the review window.
- Once a `Delivered` milestone's auto-release deadline has passed,
  `raise_dispute` returns `ReviewWindowClosed`; the freelancer may claim.
- A freelancer who never delivers is handled by the escrow delivery deadline
  and the cancellation / auto-release paths — `raise_dispute` is not the
  mechanism for non-delivery.
- When a dispute is rejected, no milestone or storage state is mutated.

## Events

See [`EVENTS.md`](EVENTS.md) for the full topic reference. Every topic emitted
by the contract is listed there, and the `event_docs_sync_tests` suite fails
the build if a topic is added to the source without a matching row.

## Related

- [`README.md`](README.md) — build and test instructions.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — contribution workflow, including the
  end-to-end test harness.
