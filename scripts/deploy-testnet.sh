#!/usr/bin/env bash
# Build, upload and deploy the milestone escrow on Stellar testnet. The escrow
# is initialised atomically by its constructor (issue #665), and the deployment
# is appended to deployments/testnet.json (issue #745).
#
# Schema (issue #987): every record in testnet.json is an object with the
# fields { date, network, contract, contract_id, wasm_sha256, source_commit,
# build, admin }. The file is an array so multiple deployments accumulate.
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
BUILD_INFO="$ROOT_DIR/dist/build-info.json"
DEPLOYMENTS="$ROOT_DIR/deployments/testnet.json"

# ── Dirty-tree guard (issue #987) ────────────────────────────────────────────
# A deploy with uncommitted changes cannot be reproduced from its recorded
# commit. Refuse unless ALLOW_DIRTY=1 is set, in which case we append "-dirty"
# to the commit hash so the record is honest.
if ! git diff --quiet || ! git diff --cached --quiet; then
  if [[ "${ALLOW_DIRTY:-0}" != "1" ]]; then
    fail "working tree has uncommitted changes. Commit or stash them, or set ALLOW_DIRTY=1."
  fi
  printf 'WARNING: deploying with uncommitted changes (ALLOW_DIRTY=1)\n' >&2
  DIRTY_SUFFIX="-dirty"
else
  DIRTY_SUFFIX=""
fi

printf 'Building reproducible WASM with the pinned Docker toolchain...\n'
scripts/build-reproducible.sh
[[ -s "$WASM" ]] || fail "reproducible build did not produce $WASM"

# ── Hash assertion (issue #987) ──────────────────────────────────────────────
# The reproducible build writes its SHA-256 to dist/build-info.json. We read
# it here so we can compare against the hash the network returns after upload.
REPRODUCIBLE_SHA256=""
if [[ -s "$BUILD_INFO" ]]; then
  REPRODUCIBLE_SHA256="$(python3 -c "
import json, pathlib, sys
info = json.loads(pathlib.Path(sys.argv[1]).read_text())
print(info.get('wasm_sha256', ''))
" "$BUILD_INFO")"
fi

printf 'Uploading WASM to %s...\n' "$NETWORK"
WASM_HASH="$(stellar contract upload --wasm "$WASM" --source "$SOURCE_ACCOUNT" --network "$NETWORK")"
[[ -n "$WASM_HASH" ]] || fail 'stellar contract upload returned an empty hash'
printf 'WASM hash: %s\n' "$WASM_HASH"

# Assert hash match if we have a reproducible reference.
if [[ -n "$REPRODUCIBLE_SHA256" ]] && [[ "$WASM_HASH" != "$REPRODUCIBLE_SHA256" ]]; then
  fail "uploaded WASM hash ($WASM_HASH) does not match the reproducible build ($REPRODUCIBLE_SHA256). The build may have used different sources."
fi

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
SOURCE_COMMIT="$(git rev-parse HEAD)${DIRTY_SUFFIX}"
BUILD_TAG=""
if [[ -s "$BUILD_INFO" ]]; then
  BUILD_TAG="$(python3 -c "
import json, pathlib, sys
info = json.loads(pathlib.Path(sys.argv[1]).read_text())
print(info.get('build_tag', '') or info.get('build', '') or '')
" "$BUILD_INFO")"
fi

python3 - "$DEPLOYMENTS" "$DEPLOY_DATE" "$WASM_HASH" "$CONTRACT_ID" "$ADMIN_ADDRESS" "$SOURCE_COMMIT" "$BUILD_TAG" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
date, wasm_sha256, contract_id, admin, source_commit, build = sys.argv[2:]
# Migrate existing single-object record to the array schema (issue #987).
if path.exists():
    previous = json.loads(path.read_text(encoding="utf-8"))
    records = previous if isinstance(previous, list) else [previous]
else:
    records = []
records.append({
    "date": date,
    "network": "testnet",
    "contract": "milestone-escrow",
    "contract_id": contract_id,
    "wasm_sha256": wasm_sha256,
    "source_commit": source_commit,
    "build": build if build else None,
    "admin": admin,
})
path.write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
PY

printf 'Deployment recorded in %s\n' "$DEPLOYMENTS"
