#!/usr/bin/env bash
# Build milestone_escrow.wasm reproducibly (issue #773).
#
# The contract is compiled inside a pinned container (build/reproducible.Dockerfile)
# from a clean export of a git commit, so two machines building the same commit
# get byte-identical wasm and the same SHA-256. That hash is what
# `stellar contract info` / the explorer report for a deployed contract, so it
# ties a deployment back to reviewed source.
#
# Usage:
#   scripts/build-reproducible.sh            # build HEAD
#   scripts/build-reproducible.sh <commit>   # build any commit, tag or branch
#   scripts/build-reproducible.sh --worktree # build uncommitted changes (the
#                                            # hash then maps to no commit, and
#                                            # on Windows CRLF checkouts it will
#                                            # not match a Linux build)
#
# Output (in dist/):
#   milestone_escrow.wasm
#   milestone_escrow.wasm.sha256   "<sha256>  milestone_escrow.wasm"
#   build-info.json                commit, hash and pinned tool versions
#
# Requires: docker, git, sha256sum (or shasum).
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

dockerfile=build/reproducible.Dockerfile
manifest=contracts/milestone-escrow/Cargo.toml
wasm_rel=target/wasm32v1-none/release/milestone_escrow.wasm
out_dir=dist

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
trap 'rm -rf "$src"' EXIT

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

image="milestone-escrow-build:$(sha256 "$dockerfile" | cut -c1-12)"
if ! docker image inspect "$image" >/dev/null 2>&1; then
  echo "Building pinned toolchain image $image"
  docker build --platform linux/amd64 -t "$image" -f "$dockerfile" build
fi

docker run --rm --platform linux/amd64 \
  -v "$(host_path "$src"):/build" \
  "$image" \
  sh -c "stellar contract build --manifest-path $manifest --locked; status=\$?; chown -R $(id -u):$(id -g) /build; exit \$status"

mkdir -p "$out_dir"
cp "$src/$wasm_rel" "$out_dir/milestone_escrow.wasm"
hash=$(sha256 "$out_dir/milestone_escrow.wasm")
echo "$hash  milestone_escrow.wasm" > "$out_dir/milestone_escrow.wasm.sha256"

rust_version=$(docker run --rm --platform linux/amd64 "$image" rustc --version)
cli_version=$(docker run --rm --platform linux/amd64 "$image" stellar --version | head -n1)
cat > "$out_dir/build-info.json" <<EOF
{
  "source_commit": "$commit",
  "wasm": "milestone_escrow.wasm",
  "wasm_sha256": "$hash",
  "rustc": "$rust_version",
  "stellar_cli": "$cli_version",
  "dockerfile_sha256": "$(sha256 "$dockerfile")"
}
EOF

echo
echo "source commit: $commit"
echo "wasm sha256:   $hash"
echo "written to:    $out_dir/"
