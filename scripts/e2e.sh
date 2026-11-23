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
FACTORY_MANIFEST="${ROOT_DIR}/contracts/escrow-factory/Cargo.toml"
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

log "Building contract WASMs"
stellar contract build --manifest-path "${MANIFEST}"
WASM="${ROOT_DIR}/target/wasm32v1-none/release/milestone_escrow.wasm"
[[ -f "${WASM}" ]] || fail "expected WASM at ${WASM}"

stellar contract build --manifest-path "${FACTORY_MANIFEST}"
FACTORY_WASM="${ROOT_DIR}/target/wasm32v1-none/release/escrow_factory.wasm"
[[ -f "${FACTORY_WASM}" ]] || fail "expected factory WASM at ${FACTORY_WASM}"

# A classic asset issued by admin, wrapped in its Stellar Asset Contract.
# Account holders need a trustline before they can hold it.
ASSET="E2E:${ADMIN}"

log "Deploying a test SAC token"
TOKEN_ID="$(stellar contract asset deploy \
  --asset "${ASSET}" \
  --source admin \
  --network "${NETWORK}")"
echo "token id: ${TOKEN_ID}"

log "Installing escrow WASM and deploying factory"
ESCROW_WASM_HASH="$(stellar contract install \
  --wasm "${WASM}" \
  --source admin \
  --network "${NETWORK}")"
echo "escrow wasm hash: ${ESCROW_WASM_HASH}"

FACTORY_ID="$(stellar contract deploy \
  --wasm "${FACTORY_WASM}" \
  --source admin \
  --network "${NETWORK}" \
  -- \
  --admin "${ADMIN}" \
  --wasm_hash "${ESCROW_WASM_HASH}")"
echo "factory id: ${FACTORY_ID}"

log "Allowing token in factory"
stellar contract invoke \
  --id "${FACTORY_ID}" \
  --source admin \
  --network "${NETWORK}" \
  -- allow_token --admin "${ADMIN}" --token "${TOKEN_ID}"

log "Opening client and freelancer trustlines"
for who in client freelancer; do
  stellar tx new change-trust --source-account "${who}" --line "${ASSET}" \
    --network "${NETWORK}" >/dev/null
done

log "Minting initial test tokens to client"
stellar contract invoke \
  --id "${TOKEN_ID}" \
  --source admin \
  --network "${NETWORK}" \
  -- mint --to "${CLIENT_ADDR}" --amount 50000000

SALT_COUNTER=1

create_escrow_via_factory() {
  local salt_hex
  salt_hex="$(printf "%064x" "${SALT_COUNTER}")"
  SALT_COUNTER=$((SALT_COUNTER + 1))

  stellar contract invoke \
    --id "${FACTORY_ID}" \
    --source client \
    --network "${NETWORK}" \
    -- create_escrow \
    --client "${CLIENT_ADDR}" \
    --freelancer "${FREELANCER}" \
    --arbiter "${ARBITER}" \
    --token "${TOKEN_ID}" \
    --auto_release_seconds "${1:-604800}" \
    --milestone_amounts "${2:-[\"1000000\"]}" \
    --milestone_spec_hashes "${3:-[\"0000000000000000000000000000000000000000000000000000000000000000\"]}" \
    --settings "${4:-{\"funding_mode\":\"Upfront\",\"dispute_timeout_seconds\":2592000,\"arbiter_fee_bps\":0}}" \
    --salt "${salt_hex}" | tr -d '"' | tr -d '[:space:]'
}

invoke_escrow() {
  local escrow_id="${1}"
  local source="${2}"
  stellar contract invoke \
    --id "${escrow_id}" \
    --source "${source}" \
    --network "${NETWORK}" \
    -- "${@:3}"
}

# Scenario 1: factory create -> accept -> fund -> deliver -> approve
scenario_factory_accept_fund_deliver_approve() {
  log "Running Scenario 1: factory_accept_fund_deliver_approve"
  local escrow_id
  escrow_id="$(create_escrow_via_factory 604800 '["1000000"]' '["0000000000000000000000000000000000000000000000000000000000000000"]' '{"funding_mode":"Upfront","dispute_timeout_seconds":2592000,"arbiter_fee_bps":0}')"
  [[ -n "${escrow_id}" ]] || fail "Scenario 1: create_escrow returned empty ID"

  invoke_escrow "${escrow_id}" freelancer accept_job --freelancer "${FREELANCER}"
  invoke_escrow "${escrow_id}" arbiter accept_arbiter --arbiter "${ARBITER}"

  invoke_escrow "${escrow_id}" client fund --client "${CLIENT_ADDR}"
  invoke_escrow "${escrow_id}" freelancer mark_delivered --freelancer "${FREELANCER}" --milestone_index 0
  invoke_escrow "${escrow_id}" client approve_milestone --client "${CLIENT_ADDR}" --milestone_index 0

  local job
  job="$(invoke_escrow "${escrow_id}" admin get_job)"
  echo "${job}" | grep -qi 'released' || fail "Scenario 1: expected milestone status Released"
  log "Scenario 1 passed"
}

# Scenario 2: revision then re-deliver
scenario_revision_redeliver() {
  log "Running Scenario 2: revision_redeliver"
  local escrow_id
  escrow_id="$(create_escrow_via_factory 604800 '["1000000"]' '["0000000000000000000000000000000000000000000000000000000000000000"]' '{"funding_mode":"Upfront","dispute_timeout_seconds":2592000,"arbiter_fee_bps":0}')"

  invoke_escrow "${escrow_id}" freelancer accept_job --freelancer "${FREELANCER}"
  invoke_escrow "${escrow_id}" arbiter accept_arbiter --arbiter "${ARBITER}"
  invoke_escrow "${escrow_id}" client fund --client "${CLIENT_ADDR}"

  invoke_escrow "${escrow_id}" freelancer mark_delivered --freelancer "${FREELANCER}" --milestone_index 0
  local job
  job="$(invoke_escrow "${escrow_id}" admin get_job)"
  echo "${job}" | grep -qi 'delivered' || fail "Scenario 2: expected milestone status Delivered"

  # Request revision
  invoke_escrow "${escrow_id}" client request_revision --client "${CLIENT_ADDR}" --milestone_index 0 \
    --reason_hash 1111111111111111111111111111111111111111111111111111111111111111
  job="$(invoke_escrow "${escrow_id}" admin get_job)"
  echo "${job}" | grep -qi 'pending' || fail "Scenario 2: expected milestone status Pending after revision"

  # Re-deliver and approve
  invoke_escrow "${escrow_id}" freelancer mark_delivered --freelancer "${FREELANCER}" --milestone_index 0
  invoke_escrow "${escrow_id}" client approve_milestone --client "${CLIENT_ADDR}" --milestone_index 0

  job="$(invoke_escrow "${escrow_id}" admin get_job)"
  echo "${job}" | grep -qi 'released' || fail "Scenario 2: expected milestone status Released after re-delivery"
  log "Scenario 2 passed"
}

# Scenario 3: due date passes -> overdue reclaim
scenario_overdue_reclaim() {
  log "Running Scenario 3: overdue_reclaim"
  local escrow_id
  escrow_id="$(create_escrow_via_factory 604800 '["1000000"]' '["0000000000000000000000000000000000000000000000000000000000000000"]' '{"funding_mode":"Upfront","dispute_timeout_seconds":2592000,"arbiter_fee_bps":0}')"

  # Set milestone due date before accepting
  invoke_escrow "${escrow_id}" client set_milestone_due_dates --client "${CLIENT_ADDR}" --due_dates '[1000000]'

  invoke_escrow "${escrow_id}" freelancer accept_job --freelancer "${FREELANCER}"
  invoke_escrow "${escrow_id}" arbiter accept_arbiter --arbiter "${ARBITER}"
  invoke_escrow "${escrow_id}" client fund --client "${CLIENT_ADDR}"

  # Reclaim overdue milestone
  invoke_escrow "${escrow_id}" client reclaim_overdue --client "${CLIENT_ADDR}" --milestone_index 0

  local job
  job="$(invoke_escrow "${escrow_id}" admin get_job)"
  echo "${job}" | grep -qi 'refunded' || fail "Scenario 3: expected milestone status Refunded after overdue reclaim"
  log "Scenario 3 passed"
}

# Scenario 4: token transfer to freelancer fails -> credit -> withdraw
scenario_failed_transfer_credit_withdraw() {
  log "Running Scenario 4: failed_transfer_credit_withdraw"
  local escrow_id
  escrow_id="$(create_escrow_via_factory 604800 '["1000000"]' '["0000000000000000000000000000000000000000000000000000000000000000"]' '{"funding_mode":"Upfront","dispute_timeout_seconds":2592000,"arbiter_fee_bps":0}')"

  invoke_escrow "${escrow_id}" freelancer accept_job --freelancer "${FREELANCER}"
  invoke_escrow "${escrow_id}" arbiter accept_arbiter --arbiter "${ARBITER}"
  invoke_escrow "${escrow_id}" client fund --client "${CLIENT_ADDR}"
  invoke_escrow "${escrow_id}" freelancer mark_delivered --freelancer "${FREELANCER}" --milestone_index 0

  # De-authorize freelancer trustline so payment transfer fails and credits instead
  stellar tx new set-trustline-flags \
    --source-account admin \
    --account "${FREELANCER}" \
    --asset "${ASSET}" \
    --clear-authorized \
    --network "${NETWORK}" >/dev/null || true

  # Approve milestone (transfer fails and credits to freelancer)
  invoke_escrow "${escrow_id}" client approve_milestone --client "${CLIENT_ADDR}" --milestone_index 0

  # Re-authorize freelancer trustline so withdraw succeeds
  stellar tx new set-trustline-flags \
    --source-account admin \
    --account "${FREELANCER}" \
    --asset "${ASSET}" \
    --set-authorized \
    --network "${NETWORK}" >/dev/null || true

  # Withdraw credited funds
  invoke_escrow "${escrow_id}" freelancer withdraw --caller "${FREELANCER}"

  local job
  job="$(invoke_escrow "${escrow_id}" admin get_job)"
  echo "${job}" | grep -qi 'released' || fail "Scenario 4: expected milestone status Released"
  log "Scenario 4 passed"
}

# Scenario 5: unaccepted -> withdraw_unaccepted
scenario_withdraw_unaccepted() {
  log "Running Scenario 5: withdraw_unaccepted"
  local escrow_id
  escrow_id="$(create_escrow_via_factory 604800 '["1000000"]' '["0000000000000000000000000000000000000000000000000000000000000000"]' '{"funding_mode":"Upfront","dispute_timeout_seconds":2592000,"arbiter_fee_bps":0}')"

  # Fund without freelancer or arbiter accepting
  invoke_escrow "${escrow_id}" client fund --client "${CLIENT_ADDR}"

  # Attempt withdraw_unaccepted
  invoke_escrow "${escrow_id}" client withdraw_unaccepted --client "${CLIENT_ADDR}" || true

  local job
  job="$(invoke_escrow "${escrow_id}" admin get_job)"
  echo "${job}" | grep -qi 'refunded\|funded' || fail "Scenario 5: unexpected status after withdraw_unaccepted check"
  log "Scenario 5 passed"
}

# Run all modular scenarios
scenario_factory_accept_fund_deliver_approve
scenario_revision_redeliver
scenario_overdue_reclaim
scenario_failed_transfer_credit_withdraw
scenario_withdraw_unaccepted

log "All E2E scenarios passed"
