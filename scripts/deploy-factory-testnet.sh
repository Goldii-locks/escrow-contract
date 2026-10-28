#!/usr/bin/env bash
# Deploy escrow-factory on Stellar testnet, so the app can create escrows
# through `create_escrow` (one escrow contract per job).
#
# 1. Builds the milestone escrow reproducibly (scripts/build-reproducible.sh)
#    and uploads it; the factory deploys every new escrow from that hash.
# 2. Builds and deploys the factory with (admin, escrow wasm hash).
# 3. Allows each token in ALLOWED_TOKENS for new escrows (signed by ADMIN_SOURCE).
# 4. Appends the deployment to deployments/testnet-factory.json.
#
# Then set the factory ID as the frontend's FACTORY_CONTRACT_ID repo variable
# and as FACTORY_CONTRACT_ID in the backend's backend.env.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
for tool in docker git stellar python3; do
  command -v "$tool" >/dev/null 2>&1 || fail "required tool '$tool' not found on PATH"
done

: "${SOURCE_ACCOUNT:?Set SOURCE_ACCOUNT to the stellar-cli key name paying for deployment}"
: "${ADMIN_ADDRESS:?Set ADMIN_ADDRESS to the platform admin Stellar address (admin of every escrow)}"
: "${ADMIN_SOURCE:?Set ADMIN_SOURCE to the stellar-cli key name for ADMIN_ADDRESS (signs allow_token)}"
: "${ALLOWED_TOKENS:?Set ALLOWED_TOKENS to a comma-separated list of token contract IDs}"

NETWORK=testnet
ESCROW_WASM="$ROOT_DIR/dist/milestone_escrow.wasm"
FACTORY_WASM="$ROOT_DIR/target/wasm32v1-none/release/escrow_factory.wasm"
DEPLOYMENTS="$ROOT_DIR/deployments/testnet-factory.json"
# Testnet and mainnet reject contract code above contract_max_size_bytes.
MAX_WASM_BYTES="${MAX_WASM_BYTES:-131072}"

printf 'Building reproducible escrow WASM with the pinned Docker toolchain...\n'
scripts/build-reproducible.sh
[[ -s "$ESCROW_WASM" ]] || fail "reproducible build did not produce $ESCROW_WASM"
size=$(wc -c <"$ESCROW_WASM")
(( size <= MAX_WASM_BYTES )) \
  || fail "escrow WASM is $size bytes, over the network's $MAX_WASM_BYTES-byte contract size limit; it cannot be uploaded"

printf 'Building escrow-factory WASM...\n'
stellar contract build --manifest-path contracts/escrow-factory/Cargo.toml
[[ -s "$FACTORY_WASM" ]] || fail "factory build did not produce $FACTORY_WASM"

printf 'Uploading escrow WASM to %s...\n' "$NETWORK"
ESCROW_WASM_HASH="$(stellar contract upload --wasm "$ESCROW_WASM" --source "$SOURCE_ACCOUNT" --network "$NETWORK")"
[[ -n "$ESCROW_WASM_HASH" ]] || fail 'stellar contract upload returned an empty hash'
printf 'Escrow WASM hash: %s\n' "$ESCROW_WASM_HASH"

printf 'Deploying escrow-factory...\n'
FACTORY_ID="$(stellar contract deploy --wasm "$FACTORY_WASM" --source "$SOURCE_ACCOUNT" --network "$NETWORK" -- \
  --admin "$ADMIN_ADDRESS" \
  --wasm_hash "$ESCROW_WASM_HASH")"
[[ -n "$FACTORY_ID" ]] || fail 'stellar contract deploy returned an empty contract ID'
printf 'Factory ID: %s\n' "$FACTORY_ID"

IFS=',' read -r -a tokens <<<"$ALLOWED_TOKENS"
for token in "${tokens[@]}"; do
  token="$(echo "$token" | tr -d '[:space:]')"
  [[ -n "$token" ]] || continue
  printf 'Allowing token %s...\n' "$token"
  stellar contract invoke --id "$FACTORY_ID" --source "$ADMIN_SOURCE" --network "$NETWORK" -- \
    allow_token --admin "$ADMIN_ADDRESS" --token "$token" >/dev/null
done

DEPLOY_DATE="$(date -u +%Y-%m-%d)"
SOURCE_COMMIT="$(git rev-parse HEAD)"
python3 - "$DEPLOYMENTS" "$DEPLOY_DATE" "$FACTORY_ID" "$ESCROW_WASM_HASH" "$ADMIN_ADDRESS" "$SOURCE_COMMIT" "$ALLOWED_TOKENS" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
date, factory_id, escrow_wasm_hash, admin, commit, tokens = sys.argv[2:]
records = json.loads(path.read_text(encoding="utf-8")) if path.exists() else []
records.append({
    "date": date,
    "network": "testnet",
    "factory_id": factory_id,
    "escrow_wasm_hash": escrow_wasm_hash,
    "admin": admin,
    "source_commit": commit,
    "allowed_tokens": [t.strip() for t in tokens.split(",") if t.strip()],
})
path.write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
PY

printf 'Deployment recorded in %s\n' "$DEPLOYMENTS"
printf '\nNext: set FACTORY_CONTRACT_ID=%s as the escrow-frontend repo variable\n' "$FACTORY_ID"
printf 'and in the backend server'"'"'s backend.env, then redeploy both.\n'
