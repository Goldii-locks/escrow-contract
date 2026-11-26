# TypeScript bindings (issues #744, #965)

The escrow and escrow-factory contracts' ABIs are published as generated
TypeScript bindings so the frontend and backend stop hand-writing ScVal
encodings (e.g. `{ type: "u64", value }` argument mappers) that silently break
on any contract signature change.

| Package | Generated from | Use it for |
|---|---|---|
| `@goldii-locks/milestone-escrow-bindings` | `milestone_escrow.wasm` | Every call on a deployed escrow (`fund`, `approve_milestone`, `get_job`, …) |
| `@goldii-locks/escrow-factory-bindings` | `escrow_factory.wasm` | Creating escrows (`create_escrow`) and factory administration (`allow_token`, `get_allowed_tokens`, `set_wasm_hash`, …) |

## How they are produced

The `typescript-bindings` job in `.github/workflows/ci.yml` runs for every
build on `main` (and every PR):

1. `stellar contract build` produces `milestone_escrow.wasm` and, from
   `contracts/escrow-factory`, `escrow_factory.wasm`.
2. One version is resolved for the run (see [Versioning](#versioning)).
3. `stellar contract bindings typescript --wasm ... --output-dir ...` generates
   a typed client from each wasm's contract spec: `bindings/ts` for the escrow,
   `bindings/factory-ts` for the factory.
4. `npm pkg set` names the packages `@goldii-locks/milestone-escrow-bindings`
   and `@goldii-locks/escrow-factory-bindings` and sets the shared version,
   then `npm run build` compiles each.
5. The packages are uploaded as the `milestone-escrow-bindings-ts-<sha>` and
   `escrow-factory-bindings-ts-<sha>` run artifacts. The job summary lists both
   with their version.

On a `v*` tag, the `publish-bindings` job downloads both artifacts, checks that
both packages carry the tag's version, and publishes both to GitHub Packages
(`https://npm.pkg.github.com`), e.g.
`@goldii-locks/milestone-escrow-bindings@1.2.3` and
`@goldii-locks/escrow-factory-bindings@1.2.3`. Main builds get
`0.0.0-main.<run>` artifact builds (not published).

> Neither package carries a contract id. The escrow is deployed per job and
> the factory per network, so consumers pass the deployed contract id when
> constructing each client.

## Versioning

The two packages are always versioned together: one CI run computes one
version (the tag without its `v`, or `0.0.0-main.<run>` on main) and stamps it
on both, and the publish job refuses to publish if they differ. A given
version of the factory package therefore describes the factory built from the
same commit as the escrow package of that version. That matters because
`create_escrow` passes `EscrowSettings` straight into the escrow's
constructor: the factory's `EscrowSettings` and `FundingMode` mirror the
escrow's and must encode identically. Depend on both at the same version:

```json
{
  "dependencies": {
    "@goldii-locks/milestone-escrow-bindings": "1.2.3",
    "@goldii-locks/escrow-factory-bindings": "1.2.3"
  }
}
```

## Creating an escrow through the factory

`create_escrow` takes the struct, enum and `BytesN<32>` vector types the
backend currently encodes by hand. The generated client types them:

```ts
// Illustrative; the generated client's exact argument shapes are authoritative.
import { Client as FactoryClient } from "@goldii-locks/escrow-factory-bindings";
import { Client as EscrowClient } from "@goldii-locks/milestone-escrow-bindings";

const factory = new FactoryClient({
  contractId: factoryContractId, // NEXT_PUBLIC_FACTORY_CONTRACT_ID / FACTORY_CONTRACT_ID
  networkPassphrase,
  rpcUrl,
  publicKey,
  signTransaction,
});

const tx = await factory.create_escrow({
  client, freelancer, arbiter, token,
  auto_release_seconds: autoReleaseSeconds,
  milestone_amounts: milestoneAmounts,              // bigint[] (i128)
  milestone_spec_hashes: specHashes,                // Buffer[] (Vec<BytesN<32>>), 32 bytes each
  settings: {
    funding_mode: { tag: "Upfront", values: undefined }, // FundingMode
    dispute_timeout_seconds: disputeTimeoutSeconds,      // bigint (u64)
    arbiter_fee_bps: 0,                                  // u32
  },
  salt,                                             // Buffer (BytesN<32>)
});
const { result: escrowId } = await tx.signAndSend();

// The returned address is a milestone-escrow contract:
const escrow = new EscrowClient({ contractId: escrowId, networkPassphrase, rpcUrl });
```

The exact `EscrowSettings` fields are whatever the factory's
`#[contracttype] struct EscrowSettings` declares at that version; the
generated type is authoritative.

Factory reads are typed the same way, e.g.
`(await factory.get_allowed_tokens()).result` is `string[]`.

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

# Factory (issue #965)
stellar contract build --manifest-path contracts/escrow-factory/Cargo.toml
stellar contract bindings typescript \
  --wasm target/wasm32v1-none/release/escrow_factory.wasm \
  --output-dir bindings/factory-ts --overwrite
(cd bindings/factory-ts && npm install && npm run build)
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
