# Milestone Escrow — Specification

## Overview

The milestone escrow contract holds client funds and releases them to a freelancer
as milestones are completed. A job is created with a list of milestones; each
milestone has an amount and a due date. The client funds the escrow, the
freelancer submits work, and the client approves (releasing funds), refunds, or
splits each milestone.

## Status

- `Open` — the escrow is active and accepts mutations.
- `Closed` — the escrow is finalised; all mutating entry points return
  `EscrowClosed`.

## Entry points

### `close_escrow(caller)`

Finalises a completed job. Callable by the client, the freelancer, or anyone.

Succeeds only when all of the following hold:

- every milestone is terminal (`Released`, `Refunded`, or `Split`);
- `TotalOwed == 0`;
- no proposal is pending.

On success it:

- sweeps any excess escrow-token balance to the client;
- sets the escrow status to `Closed`;
- publishes a `closed` event;
- stops extending TTLs for persistent entries (milestones, revision counts,
  due dates, reputation).

After close, every mutating entry point returns `EscrowClosed`. Getters such as
`get_escrow_state` continue to work.

## Errors

- `InvalidStatus` — a milestone is not terminal, or credits are still owed.
- `ProposalPending` — a proposal is still pending.
- `EscrowClosed` — a mutating entry point was called after `close_escrow`.

## Acceptance criteria

- Tests: close fails with a non-terminal milestone or owed credits. After close,
  mutators fail and getters still work.
