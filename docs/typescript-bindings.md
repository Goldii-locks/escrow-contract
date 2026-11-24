# TypeScript bindings (issue #744)

The escrow contract's ABI is published as generated TypeScript bindings so the
frontend and backend stop hand-writing ScVal encodings (e.g.
`{ type: "u64", value }` argument mappers) that silently break on any contract
signature change.

## How they are produced

The `typescript-bindings` job in `.github/workflows/ci.yml` runs for every
build on `main` (and every PR):

1. `stellar contract build` produces `milestone_escrow.wasm`.
2. `stellar contract bindings typescript --wasm ... --output-dir bindings/ts`
   generates a typed client from the wasm's contract spec.
3. `npm pkg set` names the package `@goldii-locks/milestone-escrow-bindings`
   and sets its version, then `npm run build` compiles it.
4. The package is uploaded as the `milestone-escrow-bindings-ts-<sha>` run
   artifact.

On a `v*` tag, the `publish-bindings` job publishes the same package to GitHub
Packages (`https://npm.pkg.github.com`) with the tag version, e.g.
`@goldii-locks/milestone-escrow-bindings@1.2.3`. Main builds get
`0.0.0-main.<run>` artifact builds (not published).

> The package carries no contract id. The escrow is deployed per job, so
> consumers pass the deployed contract id when constructing the client.

## Consuming from escrow-frontend

```bash
# .npmrc (repo root):
@goldii-locks:registry=https://npm.pkg.github.com
//npm.pkg.github.com/:_authToken=${NPM_TOKEN}   # any GitHub PAT with read:packages
```

```ts
import { Client } from "@goldii-locks/milestone-escrow-bindings";

const escrow = new Client({
  contractId: deployedContractId,
  networkPassphrase,
  rpcUrl,
  publicKey,
  signTransaction, // from the connected wallet
});

// Instead of hand-building the ScVal arguments for `initialize`:
const tx = await escrow.initialize({
  admin, client, freelancer, arbiter, token,
  auto_release_seconds: autoReleaseSeconds,
  milestone_amounts: milestoneAmounts,
  milestone_spec_hashes: specHashes,
});
await tx.signAndSend();
```

Reads are typed too: `(await escrow.get_job()).result` returns the generated `Job`
structure — `milestones: Vec<Milestone>`, `status: MilestoneStatus`, etc. — so
decoding is no longer hand-rolled.

## Consuming from escrow-backend

The backend's build-tx mapper (`src/routes/jobs.ts`) and its ScVal decoding of
`get_job` / `get_whitelisted_tokens` should use the generated `Client` and
types from the same package rather than hand-writing `{ type: "u64", value }`
encoders. The generated `MilestoneStatus` / `Job`
union types mirror the contract's `#[contracttype]` definitions exactly, which
is what the broken decoders were missing.

## Regenerating locally

```bash
stellar contract build --manifest-path contracts/milestone-escrow/Cargo.toml
stellar contract bindings typescript \
  --wasm target/wasm32v1-none/release/milestone_escrow.wasm \
  --output-dir bindings/ts --overwrite
(cd bindings/ts && npm install && npm run build)
```

## Switching the clients over

This repo only *produces* the bindings. Track the client-side migration in the
client repos:

- escrow-frontend: replace the hand-written `initialize` encoding in
  `app/create/page.tsx` and all other raw-ScVal call sites with the package.
- escrow-backend: replace the build-tx mapper in `src/routes/jobs.ts` and the
  hand-rolled `get_job` / `get_whitelisted_tokens` decoders with the package.

A follow-up issue in each client repo should link back to this repo's
`typescript-bindings` CI job and pin the released version they migrate to.

## Schema 4 changes (#997, #1007)

Regenerate the bindings after upgrading; the generated types change in two places:

- `DisputeInfo` (returned by `get_dispute_info`) has a new required field,
  `raised_by: string`, the address that called `raise_dispute`. Code that builds
  a `DisputeInfo` by hand (for example in test fixtures) must set it. Disputes
  that were open when an escrow migrated from schema 3 carry the job's client
  here as a placeholder; use the `dispute` event's `caller` topic for those.
- The `escrow-factory` `EscrowCreatedEvent` has the new shape `{ escrow, token,
  total_amount, milestone_count, auto_release_seconds, settings, wasm_hash }`,
  and its party addresses are event topics rather than data fields. See
  "Escrow factory events" in `EVENTS.md`.
- Error codes are contract-scoped (issue #1082): this contract uses 1-43, while
  `escrow-factory` renumbered its codes into 1001-1006 so no factory code
  equals an escrow code. A client decoding `Error(Contract, #)` must know
  which contract failed; the factory's codes are listed in the README's
  "Factory error codes" table.
