# Milestone Escrow Contract

A Soroban smart contract on Stellar that enables trustless milestone-based escrow between a client, freelancer, and arbiter.

## Overview

This contract allows a client to fund a job broken into milestones. Funds are locked in the contract and released per milestone only when the client approves delivery. Disputes can be raised by either party and resolved by a designated arbiter.

## Contract Functions

| Function | Caller | Description |
|---|---|---|
| `initialize` | Anyone | Set up job with parties, token, and milestone amounts |
| `fund` | Client | Deposit total amount into contract |
| `mark_delivered` | Freelancer | Mark a milestone as delivered |
| `approve_milestone` | Client | Release funds for a delivered milestone |
| `raise_dispute` | Client or Freelancer | Freeze a milestone for arbitration |
| `resolve_dispute` | Arbiter | Release to freelancer or refund to client |
| `get_job` | Anyone | View current job state |

## Milestone States

Pending → Delivered → Released

↓

Disputed → Released (arbiter favors freelancer)

→ Refunded (arbiter favors client)

## Prerequisites

- https://rustup.rs/  1.79+
- https://developers.stellar.org/docs/smart-contracts/getting-started/setup
- wasm32 target: rustup target add wasm32-unknown-unknown

## Build

```bash
cargo build --release --target wasm32-unknown-unknown
```

## Test

```bash
cargo test
```

## Deploy (Testnet)

```bash
soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/milestone_escrow.wasm \
  --network testnet \
  --source <your-account>
```

## License

MIT

## Deployed Contract

| Network | Contract ID |
|---|---|
| Testnet | `CBBRYWY6ROXCM6AHP4COM3AL6UDPTY66FXF43Q7PNEIPU53RZOGHBYP3` |

Explorer: `https://stellar.expert/explorer/testnet/contract/CBBRYWY6ROXCM6AHP4COM3AL6UDPTY66FXF43Q7PNEIPU53RZOGHBYP3`
