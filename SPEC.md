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
- `emergency_pause_admin_override(admin, paused)`: admin-only force pause or
  unpause. Clears `EpParty` so a later party unpause cannot undo an admin freeze.
- Unpausing a running escrow returns `NotPaused`.
- Pause state is per contract instance; unpausing one escrow does not affect
  another.

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
  event. `get_dispute_info` returns the reason hash, `disputed_at`, the
  pre-dispute status and every evidence entry; the record is deleted when the
  dispute closes, so evidence never outlives the dispute it belongs to (#712).
- If the arbiter has not acted within the escrow's dispute timeout (30 days
  by default, set with `initialize_with_dispute_timeout`), either party may
  call `escalate_dispute` to return the milestone to its pre-dispute status;
  a restored `Delivered` milestone gets a fresh auto-release window.
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
