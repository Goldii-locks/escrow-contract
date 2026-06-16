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

## Code Style

Follow Rust's standard code style and use `cargo fmt` before committing:
```bash
cargo fmt
```

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
