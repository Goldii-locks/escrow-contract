# Contributing to Milestone Escrow Contract

Thank you for your interest in contributing! This document outlines the process for making contributions.

## Getting Started

1. Fork the repository on GitHub
2. Clone your forked repo locally
3. Install dependencies: `cargo build`
4. Run tests to verify your setup: `cargo test`

## Making Changes

1. Create a new branch for your feature/fix: `git checkout -b my-feature`
2. Make your changes to the codebase
3. Write tests for new functionality (if applicable)
4. Verify all tests pass: `cargo test --all-targets`
5. Ensure the code builds: `cargo build --release`
6. If your change adds, removes or alters a state transition, its required signers, its token movements or its event, update [`contracts/milestone-escrow/SPEC.md`](contracts/milestone-escrow/SPEC.md) in the same PR

## Code Style

Follow Rust's standard code style and use `cargo fmt` before committing:
```bash
cargo fmt
```

## End-to-end tests

Unit tests run against the Soroban test host (`cargo test`). For a full
integration check, `scripts/e2e.sh` builds the contract, deploys it to a
throwaway local Stellar network (Docker required) and exercises the funded →
delivered → dispute → refund lifecycle:

```bash
bash scripts/e2e.sh
```

It requires `cargo`, `docker`, and the `stellar` CLI. The same script runs
nightly (and on demand) via the `E2E` GitHub Actions workflow.

### Toolchain pins (issue #961)

| Pin | Value | Where |
|---|---|---|
| Local network image | `stellar/quickstart:v695-b1655.1-latest`: Protocol 30, stellar-core v30.0.0, stellar-rpc v30.0.1 | `QUICKSTART_TAG` in `scripts/e2e.sh` and `.github/workflows/e2e.yml` |
| `stellar-cli` | 30.0.1 | `.github/workflows/ci.yml`, `e2e.yml`, `mutants.yml` |
| `soroban-sdk` | 28.0.0 | `Cargo.toml` |

Why these values:

- The CLI has to match the RPC. Quickstart's RPC 30 builds (from 2026-10-22)
  failed every simulation sent by stellar-cli 28 with
  `HostError: Error(Context, InternalError)`, so E2E was held on the RPC 29.0.1
  image `v678-b1525.1-latest`. With the CLI on 30.0.1, the suite runs on a
  Protocol 30 image again, which is what testnet and mainnet upgrade to next.
- The SDK stays on 28. A protocol-30 host runs protocol-28 contracts, but a
  contract built with soroban-sdk 29 or later is rejected by a protocol-28
  network ("contract protocol number is newer than host"). Move the SDK only
  when testnet and mainnet run the newer protocol.
- Quickstart's tags move daily. Keep the image pinned, and move it, the CLI,
  and escrow-backend's `e2e-nightly.yml` in one change, so the contract and
  backend nightly suites run against the same network.
- The reproducible build (`build/reproducible.Dockerfile`) pins its own
  stellar-cli 28.0.0 by release checksum. Changing it changes the deployed
  wasm's bytes (the CLI writes its version into the wasm metadata), so it
  moves in a separate, deliberate release change.

`escrow-backend`'s `.github/workflows/e2e-nightly.yml` must use the same
`QUICKSTART_TAG` and stellar-cli version. That file lives in the backend repo;
update it there in a change linked to issue #961.

## Documentation requirements

Three references are checked in CI and must stay in sync with the contract:

- `SPEC.md` describes milestone states and dispute semantics.
- `EVENTS.md` lists every emitted event topic; the `event_docs_sync_tests`
  module fails if a `symbol_short!` topic is added without a matching row.
- `CHANGELOG.md` records released versions, deployed WASM hashes, and user-visible
  ABI changes. Any PR that adds, removes, or alters user-visible ABI or contract
  behavior must update `CHANGELOG.md` in the same PR.

## Submitting Changes

1. Commit your changes with a clear, descriptive message: `git commit -m "feat: add XYZ functionality"`
2. Push your branch to your fork: `git push origin my-feature`
3. Open a pull request (PR) on the main repository
4. Your PR will be reviewed and merged after approval

## Reporting Issues

If you find a bug or have a feature request, please open an issue on GitHub with clear details:
- Steps to reproduce the problem (if bug)
- Expected behavior
- Actual behavior
- System information (Rust version, OS, etc.)

## Code of Conduct

We expect all contributors to be respectful and inclusive in their interactions.
