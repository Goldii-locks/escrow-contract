#!/usr/bin/env python3
"""Fail if a committed golden snapshot has no test function behind it, or if
any test_snapshots/ file is tracked by git.

Soroban's per-test snapshots under test_snapshots/ are gitignored; the only
committed snapshots are the few golden ones in golden_snapshots/, each
written by a test function of the same name (golden_snapshot_tests.rs).
When such a test is renamed or deleted its file would stay behind and
nothing would notice, so CI fails on it instead.
"""
import glob
import os
import re
import subprocess
import sys

SRC = "contracts/milestone-escrow/src/**/*.rs"
SNAPSHOTS = "contracts/milestone-escrow/golden_snapshots/*.json"


def check_no_tracked_test_snapshots() -> int:
    """Return 1 if any test_snapshots/ path is tracked by git."""
    result = subprocess.run(
        ["git", "ls-files", "--error-unmatch", "--", "*/test_snapshots/*"],
        capture_output=True,
        text=True,
    )
    # git ls-files exits 0 when at least one path matched (i.e. is tracked)
    if result.returncode == 0 and result.stdout.strip():
        tracked = [line for line in result.stdout.splitlines() if line.strip()]
        print(f"test_snapshots: {len(tracked)} tracked file(s) found (should be gitignored)\n")
        for path in tracked:
            print(f"  {path}")
        print(
            "\nAdd contracts/escrow-factory/test_snapshots/ (and any other "
            "test_snapshots/ directory) to .gitignore and run "
            "`git rm -r --cached <path>` to untrack them."
        )
        return 1
    return 0


def main() -> int:
    errors = check_no_tracked_test_snapshots()

    source = ""
    for path in glob.glob(SRC, recursive=True):
        with open(path, encoding="utf-8", errors="replace") as handle:
            source += handle.read()

    defined = set(re.findall(r"\bfn\s+([A-Za-z0-9_]+)", source))

    orphans = []
    for path in sorted(glob.glob(SNAPSHOTS)):
        name = re.sub(r"\.json$", "", os.path.basename(path))
        if name not in defined:
            orphans.append((path, name))

    if not orphans:
        print("golden_snapshots: no orphans")
    else:
        errors += 1
        print(f"golden_snapshots: {len(orphans)} orphaned snapshot(s)\n")
        for path, name in orphans:
            print(f"  {path}\n      no `fn {name}` in {SRC}")
        print(
            "\nEither the test was renamed (delete the stale snapshot) or it was "
            "removed (restore it, or delete the snapshot deliberately)."
        )

    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
