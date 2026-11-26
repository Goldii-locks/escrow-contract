# Releasing

This document describes how to cut a release of the Milestone Escrow Contract, publish its TypeScript bindings, and record deployments.

## Version Bump Rules

Versions follow [Semantic Versioning](https://semver.org/) (`vX.Y.Z`):

- **Major (`X.0.0`)**: Breaking changes to the contract's public interface (function signatures, parameter/return types, error codes, or event schemas) or on-chain storage layout (`DataKey` / `SCHEMA_VERSION` changes requiring `migrate`).
- **Minor (`0.Y.0`)**: Backwards-compatible additions such as new contract endpoints, new optional/non-breaking features, or new events that do not alter existing signatures or storage layouts.
- **Patch (`0.0.Z`)**: Backwards-compatible bug fixes, security patches, documentation updates, or internal refactors that preserve the contract interface and storage layout.

## Release Checklist

1. **Update `CHANGELOG.md`**:
   - Move the items under `## [Unreleased]` into a new dated section: `## [X.Y.Z] - YYYY-MM-DD`.
   - Leave an empty `## [Unreleased]` heading at the top for subsequent changes.
   - Ensure any breaking changes or migration notes (`SCHEMA_VERSION` / `migrate`) are clearly called out.

2. **Build the reproducible WASM and record the hash**:
   ```bash
   scripts/build-reproducible.sh
   ```
   - This produces `dist/milestone_escrow.wasm`, `dist/milestone_escrow.wasm.sha256`, and `dist/build-info.json` inside the pinned Docker toolchain.
   - Record the SHA-256 hash from `dist/milestone_escrow.wasm.sha256` in the new `CHANGELOG.md` release section.
   - Commit the `CHANGELOG.md` update to `main`.

3. **Tag and push `vX.Y.Z`**:
   ```bash
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin main --tags
   ```
   - Pushing a `v*` tag triggers the `typescript-bindings` and `publish-bindings` jobs in [`.github/workflows/ci.yml`](.github/workflows/ci.yml), which build `@goldii-locks/milestone-escrow-bindings` at version `X.Y.Z` and publish it to GitHub Packages.
   - Verify the package is visible in GitHub Packages and that `reproducible-wasm` CI summary matches the recorded SHA-256 hash.

4. **Update deployment records (`deployments/*.json`)**:
   - When the release is deployed to a network (e.g., `testnet` or `mainnet`), update `deployments/<network>.json` (such as [`deployments/testnet.json`](deployments/testnet.json)) with:
     - `contract_id`: the deployed contract address
     - `wasm_sha256`: the SHA-256 hash of the deployed WASM
     - `source_commit`: the tagged release commit SHA from `dist/build-info.json`
     - `build`: toolchain metadata from `dist/build-info.json`
