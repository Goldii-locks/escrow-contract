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

## Ignored tests

Every `#[ignore]` must carry a reason that cites a tracking issue as `#<number>`,
e.g. `#[ignore = "blocked on #123: ..."]`. `scripts/check-ignored-tests.py`
(run in CI) scans `contracts/**/*.rs` and, for each cited number, asks the
GitHub API (`GET /repos/{GITHUB_REPOSITORY}/issues/{n}`) for its state. It
fails when:

- the `#[ignore]` has no reason, or the reason cites no `#<number>`;
- none of the cited numbers is an **open issue** (closed issues, nonexistent
  numbers and pull requests do not count).

Verification failures never pass silently: network errors, unexpected HTTP
statuses and exhausted rate limits make the script exit `2`. Rate-limit and 5xx
responses are retried (honouring `Retry-After` / `X-RateLimit-Reset`) only when
the wait is at most 60 seconds. Each issue is looked up individually, so no
pagination is needed. Set `GITHUB_TOKEN` locally for a higher rate limit;
`GITHUB_REPOSITORY` overrides the repository (default `Goldii-locks/escrow-contract`).
When the underlying bug is fixed, remove the `#[ignore]` rather than closing
the issue first. Offline unit tests: `python3 scripts/test_check_ignored_tests.py`.

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
