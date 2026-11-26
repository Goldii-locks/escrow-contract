#!/usr/bin/env bash
# Build, upload and deploy the milestone escrow on Stellar testnet. The escrow
# is initialised atomically by its constructor (issue #665), and the deployment
# is appended to deployments/testnet.json (issue #745).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
for tool in docker git stellar python3; do
  command -v "$tool" >/dev/null 2>&1 || fail "required tool '$tool' not found on PATH"
done

: "${SOURCE_ACCOUNT:?Set SOURCE_ACCOUNT to the stellar-cli key name paying for deployment}"
: "${ADMIN_ADDRESS:?Set ADMIN_ADDRESS to the escrow admin Stellar address}"
: "${CLIENT_ADDRESS:?Set CLIENT_ADDRESS}"
: "${FREELANCER_ADDRESS:?Set FREELANCER_ADDRESS}"
: "${ARBITER_ADDRESS:?Set ARBITER_ADDRESS}"
: "${TOKEN_ADDRESS:?Set TOKEN_ADDRESS to the token contract ID}"
: "${AUTO_RELEASE_SECONDS:?Set AUTO_RELEASE_SECONDS to a positive integer}"
: "${MILESTONE_AMOUNTS:?Set MILESTONE_AMOUNTS to a JSON array of positive integer amounts}"
: "${MILESTONE_SPEC_HASHES:?Set MILESTONE_SPEC_HASHES to a JSON array of 32-byte hex hashes}"
FUNDING_MODE="${FUNDING_MODE:-Upfront}"                    # Upfront or PerMilestone
DISPUTE_TIMEOUT_SECONDS="${DISPUTE_TIMEOUT_SECONDS:-2592000}" # 30 days
ARBITER_FEE_BPS="${ARBITER_FEE_BPS:-0}"                     # 0-1000
SETTINGS="{\"funding_mode\":\"${FUNDING_MODE}\",\"dispute_timeout_seconds\":${DISPUTE_TIMEOUT_SECONDS},\"arbiter_fee_bps\":${ARBITER_FEE_BPS}}"

NETWORK=testnet
WASM="$ROOT_DIR/dist/milestone_escrow.wasm"
DEPLOYMENTS="$ROOT_DIR/deployments/testnet.json"

printf 'Building reproducible WASM with the pinned Docker toolchain...\n'
scripts/build-reproducible.sh
[[ -s "$WASM" ]] || fail "reproducible build did not produce $WASM"

printf 'Uploading WASM to %s...\n' "$NETWORK"
WASM_HASH="$(stellar contract upload --wasm "$WASM" --source "$SOURCE_ACCOUNT" --network "$NETWORK")"
[[ -n "$WASM_HASH" ]] || fail 'stellar contract upload returned an empty hash'
printf 'WASM hash: %s\n' "$WASM_HASH"

printf 'Deploying with constructor arguments...\n'
CONTRACT_ID="$(stellar contract deploy --wasm-hash "$WASM_HASH" --source "$SOURCE_ACCOUNT" --network "$NETWORK" -- \
  --admin "$ADMIN_ADDRESS" \
  --client "$CLIENT_ADDRESS" \
  --freelancer "$FREELANCER_ADDRESS" \
  --arbiter "$ARBITER_ADDRESS" \
  --token "$TOKEN_ADDRESS" \
  --auto_release_seconds "$AUTO_RELEASE_SECONDS" \
  --milestone_amounts "$MILESTONE_AMOUNTS" \
  --milestone_spec_hashes "$MILESTONE_SPEC_HASHES" \
  --settings "$SETTINGS")"
[[ -n "$CONTRACT_ID" ]] || fail 'stellar contract deploy returned an empty contract ID'
printf 'Contract ID: %s\n' "$CONTRACT_ID"

DEPLOY_DATE="$(date -u +%Y-%m-%d)"
python3 - "$DEPLOYMENTS" "$DEPLOY_DATE" "$WASM_HASH" "$CONTRACT_ID" "$ADMIN_ADDRESS" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
date, wasm_hash, contract_id, admin = sys.argv[2:]
if path.exists():
    previous = json.loads(path.read_text(encoding="utf-8"))
    # Preserve the repository's original single-record format as history.
    records = previous if isinstance(previous, list) else [previous]
else:
    records = []
records.append({
    "date": date,
    "network": "testnet",
    "wasm_hash": wasm_hash,
    "contract_id": contract_id,
    "admin": admin,
})
path.write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
PY

printf 'Deployment recorded in %s\n' "$DEPLOYMENTS"
