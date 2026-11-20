#!/usr/bin/env python3
"""Fail if an ignored Rust test does not cite an OPEN GitHub issue.

Every `#[ignore]` under contracts/ must carry a reason that references at least
one issue as `#<number>`, and at least one cited issue must be open in the
repository (verified against the GitHub API, not just by pattern matching).

Environment:
  GITHUB_REPOSITORY  owner/name to look issues up in (default: Goldii-locks/escrow-contract)
  GITHUB_TOKEN       optional but recommended (raises the API rate limit)
  GITHUB_API_URL     API base (default: https://api.github.com)

Exit codes: 0 all ignores valid; 1 invalid ignore found;
            2 issue status could not be verified (API failure / rate limit).
            Unverifiable status never passes silently.
"""
import glob
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

ROOT = "contracts"
DEFAULT_REPO = "Goldii-locks/escrow-contract"
IGNORE_ATTR = re.compile(r'#\[\s*ignore\s*(?:=\s*"((?:[^"\\]|\\.)*)")?\s*\]')
ISSUE_REF = re.compile(r"(?<![\w&])#(\d+)\b")
MAX_RETRIES = 3
MAX_WAIT_SECONDS = 60


class VerificationError(Exception):
    """Issue state could not be determined."""


def find_ignores(root=ROOT):
    """Yield (path, line, reason_or_None) for every #[ignore] attribute."""
    for path in sorted(glob.glob(os.path.join(root, "**", "*.rs"), recursive=True)):
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
        for m in IGNORE_ATTR.finditer(text):
            line = text.count("\n", 0, m.start()) + 1
            # Skip attributes inside comments/doc comments (e.g. ```ignore fences).
            line_start = text.rfind("\n", 0, m.start()) + 1
            if text[line_start:m.start()].lstrip().startswith("//"):
                continue
            yield path, line, m.group(1)


def fetch_issue_state(repo, number, token=None, api=None, opener=urllib.request.urlopen, sleep=time.sleep):
    """Return 'open', 'closed', 'pull_request' or 'missing'. Raise VerificationError otherwise."""
    api = (api or os.environ.get("GITHUB_API_URL") or "https://api.github.com").rstrip("/")
    url = f"{api}/repos/{repo}/issues/{number}"
    headers = {"Accept": "application/vnd.github+json", "User-Agent": "check-ignored-tests"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    for attempt in range(MAX_RETRIES + 1):
        try:
            with opener(urllib.request.Request(url, headers=headers), timeout=30) as resp:
                data = json.load(resp)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return "missing"
            rate_limited = e.code == 429 or (
                e.code == 403 and e.headers.get("X-RateLimit-Remaining") == "0"
            )
            if rate_limited or e.code >= 500:
                wait = None
                if e.headers.get("Retry-After"):
                    wait = int(e.headers["Retry-After"])
                elif e.headers.get("X-RateLimit-Reset"):
                    wait = max(0, int(e.headers["X-RateLimit-Reset"]) - int(time.time())) + 1
                if wait is None:
                    wait = 2 ** attempt
                if attempt < MAX_RETRIES and wait <= MAX_WAIT_SECONDS:
                    sleep(wait)
                    continue
                kind = "rate limited" if rate_limited else f"server error {e.code}"
                hint = " (set GITHUB_TOKEN for a higher limit)" if rate_limited and not token else ""
                raise VerificationError(f"#{number}: {kind}{hint}") from e
            raise VerificationError(f"#{number}: HTTP {e.code} from GitHub API") from e
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, OSError) as e:
            if attempt < MAX_RETRIES:
                sleep(2 ** attempt)
                continue
            raise VerificationError(f"#{number}: GitHub API unreachable ({e})") from e
        if "pull_request" in data:
            return "pull_request"
        state = data.get("state")
        if state not in ("open", "closed"):
            raise VerificationError(f"#{number}: unexpected API response")
        return state
    raise VerificationError(f"#{number}: retries exhausted")


def check(ignores, lookup):
    """Return (problems, errors). `lookup(number)` -> state or raises VerificationError."""
    problems, errors, cache = [], [], {}
    for path, line, reason in ignores:
        where = f"{path}:{line}"
        if not reason:
            problems.append(f"{where}: #[ignore] has no reason; cite an open issue as #<number>")
            continue
        numbers = sorted({int(n) for n in ISSUE_REF.findall(reason)})
        if not numbers:
            problems.append(f"{where}: ignore reason cites no issue (#<number>): {reason!r}")
            continue
        states = {}
        for n in numbers:
            if n not in cache:
                try:
                    cache[n] = lookup(n)
                except VerificationError as e:
                    cache[n] = e
            if isinstance(cache[n], VerificationError):
                errors.append(f"{where}: cannot verify {cache[n]}")
            else:
                states[n] = cache[n]
        if any(isinstance(cache[n], VerificationError) for n in numbers):
            continue
        if "open" not in states.values():
            detail = ", ".join(f"#{n} is {s}" for n, s in states.items())
            problems.append(f"{where}: ignore reason must cite an open issue ({detail})")
    return problems, errors


def main():
    repo = os.environ.get("GITHUB_REPOSITORY") or DEFAULT_REPO
    token = os.environ.get("GITHUB_TOKEN")
    ignores = list(find_ignores())
    problems, errors = check(ignores, lambda n: fetch_issue_state(repo, n, token))
    for msg in problems + errors:
        print(f"error: {msg}", file=sys.stderr)
    if errors:
        print("Issue status could not be verified; failing rather than passing silently.", file=sys.stderr)
        return 2
    if problems:
        return 1
    print(f"OK: {len(ignores)} ignored test(s), each cites an open issue in {repo}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
