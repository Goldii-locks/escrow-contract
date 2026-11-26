#!/usr/bin/env bash
#
# scripts/e2e.sh — local end-to-end smoke test.
#
# Builds the milestone-escrow contract, deploys it to a throwaway local
# Stellar network, and drives the funded -> delivered -> disputed -> refunded
# path, asserting the resulting on-chain state.
#
# Requirements:
#   * stellar-cli (https://developers.stellar.org/docs/tools/developer-tools/cli)
#   * Docker (the local network runs in a container)
#
# Usage:
#   ./scripts/e2e.sh
#
# Environment overrides:
#   NETWORK_NAME   local network name (default: milestone-e2e)
#   KEEP_NETWORK=1 leave the local network running on exit (for debugging)
#   QUICKSTART_TAG stellar/quickstart image tag (default: v678-b1525.1-latest)

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${ROOT_DIR}/contracts/milestone-escrow/Cargo.toml"
NETWORK="${NETWORK:-local}"
NETWORK_NAME="${NETWORK_NAME:-milestone-e2e}"
KEEP_NETWORK="${KEEP_NETWORK:-0}"
# Pinned: the quickstart image moves daily. Its RPC 30.0.0 builds (from
# 2026-10-22) fail every simulation from stellar-cli 28 with
# "HostError: Error(Context, InternalError)"; this one runs RPC 29.0.1.
QUICKSTART_TAG="${QUICKSTART_TAG:-v678-b1525.1-latest}"

log() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
fail() { printf '\n\033[1;31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

require() {
  command -v "$1" >/dev/null 2>&1 || fail "required tool '$1' not found on PATH"
}

cleanup() {
  if [[ "${KEEP_NETWORK}" != "1" ]]; then
    log "Stopping local Stellar network"
    stellar container stop "${NETWORK_NAME}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

log "Checking prerequisites"
require cargo
require docker
require stellar
stellar --version

# `--limits unlimited`: the optimized escrow wasm is larger than the
# 131,072-byte contract_max_size_bytes that testnet and mainnet enforce, so
# under the default limits the deploy below fails with TxSorobanInvalid.
# This test exercises the contract's behaviour; it does not prove the wasm
# fits on a public network.
log "Starting throwaway local network"
stellar container start "${NETWORK}" --name "${NETWORK_NAME}" \
  --image-tag-override "${QUICKSTART_TAG}" --limits unlimited

# The container returns before its RPC and friendbot are serving. Deploying
# straight away fails with "client error (Connect)", so wait for both.
log "Waiting for the local RPC to report healthy"
for _ in $(seq 1 120); do
  if stellar network health --network "${NETWORK}" 2>&1 | grep -qi healthy; then
    break
  fi
  sleep 5
done
stellar network health --network "${NETWORK}" 2>&1 | grep -qi healthy \
  || fail "local RPC did not become healthy"

log "Creating and funding test identities"
for who in admin client freelancer arbiter; do
  stellar keys generate "${who}" --overwrite >/dev/null
  funded=0
  for _ in $(seq 1 60); do
    if stellar keys fund "${who}" --network "${NETWORK}" >/dev/null 2>&1; then
      funded=1
      break
    fi
    sleep 5
  done
  [[ "${funded}" == "1" ]] || fail "friendbot did not fund '${who}'"
done
ADMIN="$(stellar keys address admin)"
CLIENT_ADDR="$(stellar keys address client)"
FREELANCER="$(stellar keys address freelancer)"
ARBITER="$(stellar keys address arbiter)"

log "Building contract WASM"
stellar contract build --manifest-path "${MANIFEST}"
WASM="${ROOT_DIR}/target/wasm32v1-none/release/milestone_escrow.wasm"
[[ -f "${WASM}" ]] || fail "expected WASM at ${WASM}"

# A classic asset issued by admin, wrapped in its Stellar Asset Contract.
# Account holders need a trustline before they can hold it.
ASSET="E2E:${ADMIN}"

log "Deploying a test SAC token"
TOKEN_ID="$(stellar contract asset deploy \
  --asset "${ASSET}" \
  --source admin \
  --network "${NETWORK}")"
echo "token id: ${TOKEN_ID}"

log "Deploying contract with constructor args"
CONTRACT_ID="$(stellar contract deploy \
  --wasm "${WASM}" \
  --source admin \
  --network "${NETWORK}" \
  -- \
  --admin "${ADMIN}" \
  --client "${CLIENT_ADDR}" \
  --freelancer "${FREELANCER}" \
  --arbiter "${ARBITER}" \
  --token "${TOKEN_ID}" \
  --auto_release_seconds 604800 \
  --milestone_amounts '["1000000"]' \
  --milestone_spec_hashes '["0000000000000000000000000000000000000000000000000000000000000000"]' \
  --settings '{"funding_mode":"Upfront","dispute_timeout_seconds":2592000,"arbiter_fee_bps":0}')"
echo "contract id: ${CONTRACT_ID}"

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${1}" \
    --network "${NETWORK}" \
    -- "${@:2}"
}

# Work is gated on both the freelancer and the arbiter accepting the job.
log "Freelancer and arbiter accept the job"
invoke freelancer accept_job --freelancer "${FREELANCER}"
invoke arbiter accept_arbiter --arbiter "${ARBITER}"

log "Opening client and freelancer trustlines"
for who in client freelancer; do
  stellar tx new change-trust --source-account "${who}" --line "${ASSET}" \
    --network "${NETWORK}" >/dev/null
done

log "Minting test tokens to the client"
stellar contract invoke \
  --id "${TOKEN_ID}" \
  --source admin \
  --network "${NETWORK}" \
  -- mint --to "${CLIENT_ADDR}" --amount 1000000

log "Funding escrow"
invoke client fund --client "${CLIENT_ADDR}"

log "Marking milestone delivered"
invoke freelancer mark_delivered --freelancer "${FREELANCER}" --milestone_index 0

log "Raising a dispute"
invoke client raise_dispute --caller "${CLIENT_ADDR}" --milestone_index 0 \
  --reason_hash 2121212121212121212121212121212121212121212121212121212121212121

log "Resolving dispute as refund to client"
invoke arbiter resolve_dispute \
  --arbiter "${ARBITER}" \
  --milestone_index 0 \
  --release_to_freelancer false \
  --decision_hash 3333333333333333333333333333333333333333333333333333333333333333

log "Asserting final milestone status is Refunded"
JOB="$(invoke admin get_job)"
echo "${JOB}"
echo "${JOB}" | grep -qi 'refunded' || fail "expected milestone status Refunded after dispute resolution"

log "E2E passed"
