# Common build, test, snapshot and deploy commands for milestone-escrow.
# Run `just --list` to see every recipe.

set shell := ["bash", "-euo", "-c"]

manifest := "contracts/milestone-escrow/Cargo.toml"

# Run the full test suite (all targets)
test:
    cargo test --all-targets

# Run the factory contract tests
test-factory:
    cargo test --manifest-path {{manifest}} --all-targets

# Run the ignored (slow / network) tests
ignored:
    cargo test --all-targets -- --ignored

# Check formatting
fmt:
    cargo fmt --all -- --check

# Run clippy with warnings denied
clippy:
    cargo clippy --all-targets -- -D warnings

# Build the optimized contract WASM into dist/optimized
wasm:
    stellar contract build --optimize --out-dir dist/optimized

# Report the size of the built WASM
wasm-size:
    ls -l dist/optimized/*.wasm

# Run the golden snapshot tests (commit the regenerated snapshots afterwards)
golden:
    cargo test golden_snapshot_tests

# Run the cost baseline tests with output
costs:
    cargo test cost_tests::main_entrypoint_cost_baselines -- --nocapture

# Build a reproducible WASM for a given commit
repro commit:
    bash scripts/build-reproducible.sh {{commit}}

# Run the end-to-end lifecycle test (requires docker + stellar CLI)
e2e:
    bash scripts/e2e.sh

# Generate the TypeScript bindings
bindings:
    stellar contract bindings typescript --wasm dist/optimized/*.wasm --output-dir bindings
