#!/usr/bin/env bash
# Build milestone_escrow.wasm and escrow_factory.wasm reproducibly (issues
# #773 and #941).
#
# Both contracts are compiled inside a pinned container
# (build/reproducible.Dockerfile) from a clean export of a git commit, so two
# machines building the same commit get byte-identical wasm and the same
# SHA-256. Those hashes are what `stellar contract info` / the explorer report
# for deployed contracts, so they tie deployments back to reviewed source.
#
# Usage:
#   scripts/build-reproducible.sh                     # build HEAD
#   scripts/build-reproducible.sh <commit>            # build any commit, tag or branch
#   scripts/build-reproducible.sh --worktree          # build uncommitted changes (the
#                                                     # hash then maps to no commit, and
#                                                     # on Windows CRLF checkouts it will
#                                                     # not match a Linux build)
#   scripts/build-reproducible.sh --check-testdata [<commit>]
#                                                     # also verify the committed
#                                                     # upgrade-test fixture wasm by
#                                                     # rebuilding it from the commit
#                                                     # that last updated that fixture
#
# Output (in dist/):
#   milestone_escrow.wasm
#   milestone_escrow.wasm.sha256   "<sha256>  milestone_escrow.wasm"
#   escrow_factory.wasm
#   escrow_factory.wasm.sha256     "<sha256>  escrow_factory.wasm"
#   build-info.json                commit, both hashes and pinned tool versions
#
# Requires: docker, git, sha256sum (or shasum), cmp (for --check-testdata).
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

dockerfile=build/reproducible.Dockerfile
manifest=contracts/milestone-escrow/Cargo.toml
wasm_rel=target/wasm32v1-none/release/milestone_escrow.wasm
factory_manifest=contracts/escrow-factory/Cargo.toml
factory_wasm_rel=target/wasm32v1-none/release/escrow_factory.wasm
testdata_manifest=contracts/milestone-escrow/testdata/escrow_v2/Cargo.toml
testdata_wasm_rel=contracts/milestone-escrow/testdata/escrow_v2/target/wasm32v1-none/release/escrow_v2.wasm
testdata_wasm=contracts/milestone-escrow/testdata/escrow_v2.wasm
out_dir=dist

check_testdata=0
# The flag is accepted before the optional commit/worktree argument.
while [ "${1:-}" = "--check-testdata" ]; do
  check_testdata=1
  shift
done

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Git Bash on Windows rewrites /container/paths in arguments; docker needs them
# verbatim, and needs the host side of a bind mount as a Windows path.
export MSYS_NO_PATHCONV=1
host_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else echo "$1"; fi
}

src=$(mktemp -d)
fixture_src=$(mktemp -d)
trap 'rm -rf "$src" "$fixture_src"' EXIT

if [ "${1:-}" = "--worktree" ]; then
  commit="$(git rev-parse HEAD)-dirty"
  echo "Building the working tree (tracked + untracked, not ignored) on top of ${commit%-dirty}"
  git ls-files -z --cached --others --exclude-standard \
    | while IFS= read -r -d '' f; do [ -e "$f" ] && printf '%s\0' "$f"; done \
    | tar --null -cf - -T - | tar -xf - -C "$src"
else
  commit=$(git rev-parse --verify "${1:-HEAD}^{commit}")
  echo "Building commit $commit"
  # Export the blobs exactly as committed. Without this, core.autocrlf on a
  # Windows clone would hand the compiler CRLF sources, and Soroban embeds
  # function doc comments in the wasm's contract spec, so the hash would differ.
  git -c core.autocrlf=false -c core.eol=lf archive "$commit" | tar -xf - -C "$src"
fi

fixture_commit=""
if [ "$check_testdata" -eq 1 ]; then
  # The fixture is a frozen "v2" artifact, not the escrow at HEAD. Rebuild it
  # from the commit that committed the wasm so a later escrow change does not
  # silently invalidate the upgrade test (issue #940).
  fixture_commit=$(git log -1 --format=%H -- "$testdata_wasm")
  [ -n "$fixture_commit" ] || {
    echo "ERROR: no commit found for $testdata_wasm" >&2
    exit 1
  }
  echo "Checking upgrade-test fixture from commit $fixture_commit"
  git -c core.autocrlf=false -c core.eol=lf archive "$fixture_commit" | tar -xf - -C "$fixture_src"
fi

image="milestone-escrow-build:$(sha256 "$dockerfile" | cut -c1-12)"
if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "Building pinned toolchain image $image"
  docker build --platform linux/amd64 -t "$image" -f "$dockerfile" build
fi

# Generate the in-container build steps instead of nesting shell quoting in
# `docker run sh -c`. One docker run shares the pinned toolchain and cargo cache
# across both contracts (and the fixture when requested).
cat > "$src/build-in-container.sh" <<EOF
#!/bin/sh
set -eu
trap 'chown -R $(id -u):$(id -g) /build /fixture' EXIT
echo 'Building milestone_escrow.wasm...'
stellar contract build --manifest-path $manifest --locked
echo 'Building escrow_factory.wasm...'
stellar contract build --manifest-path $factory_manifest --locked
EOF
if [ "$check_testdata" -eq 1 ]; then
  cat >> "$src/build-in-container.sh" <<EOF
cd /fixture
echo 'Building escrow_v2.wasm fixture...'
stellar contract build --manifest-path $testdata_manifest --locked
EOF
fi

docker run --rm --platform linux/amd64 \
  -v "$(host_path "$src"):/build" \
  -v "$(host_path "$fixture_src"):/fixture" \
  "$image" sh /build/build-in-container.sh

if [ "$check_testdata" -eq 1 ]; then
  if ! cmp -s "$fixture_src/$testdata_wasm_rel" "$testdata_wasm"; then
    cat >&2 <<'EOF'
ERROR: committed contracts/milestone-escrow/testdata/escrow_v2.wasm differs from
the rebuild with the pinned toolchain. Rebuild and commit it with:

stellar contract build --manifest-path contracts/milestone-escrow/testdata/escrow_v2/Cargo.toml
cp contracts/milestone-escrow/testdata/escrow_v2/target/wasm32v1-none/release/escrow_v2.wasm \
   contracts/milestone-escrow/testdata/escrow_v2.wasm
EOF
    exit 1
  fi
  echo "upgrade-test fixture matches the pinned-container rebuild"
fi

mkdir -p "$out_dir"
cp "$src/$wasm_rel" "$out_dir/milestone_escrow.wasm"
hash=$(sha256 "$out_dir/milestone_escrow.wasm")
echo "$hash  milestone_escrow.wasm" > "$out_dir/milestone_escrow.wasm.sha256"
cp "$src/$factory_wasm_rel" "$out_dir/escrow_factory.wasm"
factory_hash=$(sha256 "$out_dir/escrow_factory.wasm")
echo "$factory_hash  escrow_factory.wasm" > "$out_dir/escrow_factory.wasm.sha256"

rust_version=$(docker run --rm --platform linux/amd64 "$image" rustc --version)
cli_version=$(docker run --rm --platform linux/amd64 "$image" stellar --version | head -n1)
cat > "$out_dir/build-info.json" <<EOF
{
  "source_commit": "$commit",
  "wasm": "milestone_escrow.wasm",
  "wasm_sha256": "$hash",
  "factory": "escrow-factory",
  "factory_wasm": "escrow_factory.wasm",
  "factory_sha256": "$factory_hash",
  "rustc": "$rust_version",
  "stellar_cli": "$cli_version",
  "dockerfile_sha256": "$(sha256 "$dockerfile")"
}
EOF

echo
echo "source commit:      $commit"
echo "escrow wasm sha256: $hash"
echo "factory wasm sha256: $factory_hash"
echo "written to:         $out_dir/"
