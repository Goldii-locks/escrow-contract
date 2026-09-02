#!/usr/bin/env python3
"""Fail if a committed test snapshot has no test function behind it.

Soroban writes a snapshot per test into test_snapshots/. When a test is
renamed or deleted the file stays, and nothing notices -- 261 had built up
this way, most of them left behind by a commit that cut test.rs from 143
test functions to 86 while claiming to add coverage. An orphan is harmless
on its own, but a pile of them hides the one that means a test was lost.
"""
import glob
import os
import re
import sys

SRC = "contracts/milestone-escrow/src/**/*.rs"
SNAPSHOTS = "contracts/milestone-escrow/test_snapshots/**/*.json"


def main() -> int:
    source = ""
    for path in glob.glob(SRC, recursive=True):
        with open(path, encoding="utf-8", errors="replace") as handle:
            source += handle.read()

    defined = set(re.findall(r"\bfn\s+([A-Za-z0-9_]+)", source))

    orphans = []
    for path in sorted(glob.glob(SNAPSHOTS, recursive=True)):
        name = re.sub(r"\.\d+\.json$", "", os.path.basename(path))
        if name not in defined:
            orphans.append((path, name))

    if not orphans:
        print("test_snapshots: no orphans")
        return 0

    print(f"test_snapshots: {len(orphans)} orphaned snapshot(s)\n")
    for path, name in orphans:
        print(f"  {path}\n      no `fn {name}` in {SRC}")
    print(
        "\nEither the test was renamed (delete the stale snapshot) or it was "
        "removed (restore it, or delete the snapshot deliberately)."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
