#!/usr/bin/env bash
# Deploy escrow-factory on Stellar testnet, so the app can create escrows
# through `create_escrow` (one escrow contract per job).
#
# 1. Builds both contracts reproducibly (scripts/build-reproducible.sh) with the
#    pinned container; the factory deploys every new escrow from that escrow
#    wasm hash.
# 2. Deploys the factory with (admin, escrow wasm hash) and allows each token
#    in ALLOWED_TOKENS for new escrows (signed by ADMIN_SOURCE).
# 3. Appends the deployment to deployments/testnet-factory.json with the factory
#    contract id, wasm hash, source commit, full build info and allowed tokens,
#    so the deployment can be reverified from source (issues #941 and #942).
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
FACTORY_WASM="$ROOT_DIR/dist/escrow_factory.wasm"
BUILD_INFO="$ROOT_DIR/dist/build-info.json"
DEPLOYMENTS="$ROOT_DIR/deployments/testnet-factory.json"
# Testnet and mainnet reject contract code above contract_max_size_bytes.
MAX_WASM_BYTES="${MAX_WASM_BYTES:-131072}"

printf 'Building reproducible escrow and factory WASM with the pinned Docker toolchain...\n'
scripts/build-reproducible.sh
[[ -s "$ESCROW_WASM" ]] || fail "reproducible build did not produce $ESCROW_WASM"
[[ -s "$FACTORY_WASM" ]] || fail "reproducible build did not produce $FACTORY_WASM"
[[ -s "$BUILD_INFO" ]] || fail "reproducible build did not produce $BUILD_INFO"
for wasm in "$ESCROW_WASM" "$FACTORY_WASM"; do
  size=$(wc -c <"$wasm")
  (( size <= MAX_WASM_BYTES )) \
    || fail "$wasm is $size bytes, over the network's $MAX_WASM_BYTES-byte contract size limit; it cannot be uploaded"
done

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
python3 - "$DEPLOYMENTS" "$BUILD_INFO" "$FACTORY_WASM" "$DEPLOY_DATE" "$FACTORY_ID" "$ESCROW_WASM_HASH" "$ADMIN_ADDRESS" "$ALLOWED_TOKENS" <<'PY'
import hashlib
import json
import pathlib
import sys

deployments, build_info, factory_wasm = (pathlib.Path(p) for p in sys.argv[1:4])
date, factory_id, escrow_wasm_hash, admin, tokens = (
    value.strip() for value in sys.argv[4:]
)
build = json.loads(build_info.read_text(encoding="utf-8"))
for key in ("source_commit", "wasm_sha256", "factory_sha256", "factory_wasm"):
    if not build.get(key):
        raise SystemExit(f"{build_info} is missing {key}; refusing to record an incomplete deployment")
if build["wasm_sha256"] != escrow_wasm_hash.strip():
    raise SystemExit(
        f"uploaded escrow wasm hash {escrow_wasm_hash} does not match build-info {build['wasm_sha256']}"
    )
local_factory_sha = hashlib.sha256(factory_wasm.read_bytes()).hexdigest()
if local_factory_sha != build["factory_sha256"]:
    raise SystemExit(
        f"factory wasm hash {local_factory_sha} does not match build-info {build['factory_sha256']}"
    )
if deployments.exists():
    previous = json.loads(deployments.read_text(encoding="utf-8"))
    records = previous if isinstance(previous, list) else [previous]
else:
    records = []
records.append({
    "date": date,
    "network": "testnet",
    "factory_id": factory_id,
    "contract_id": factory_id,
    "wasm_sha256": build["factory_sha256"],
    "factory_wasm": build["factory_wasm"],
    "factory_sha256": build["factory_sha256"],
    "escrow_wasm_hash": escrow_wasm_hash.strip(),
    "admin": admin,
    "source_commit": build["source_commit"],
    "build": build,
    "allowed_tokens": [t.strip() for t in tokens.split(",") if t.strip()],
})
deployments.write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
PY

printf 'Deployment recorded in %s\n' "$DEPLOYMENTS"
printf '\nNext: set FACTORY_CONTRACT_ID=%s as the escrow-frontend repo variable\n' "$FACTORY_ID"
printf 'and in the backend server'"'"'s backend.env, then redeploy both.\n'
