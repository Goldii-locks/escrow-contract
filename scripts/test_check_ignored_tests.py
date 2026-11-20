#!/usr/bin/env python3
"""Unit tests for check-ignored-tests.py (no network). Run: python3 scripts/test_check_ignored_tests.py"""
import importlib.util
import io
import os
import tempfile
import unittest
import urllib.error

spec = importlib.util.spec_from_file_location(
    "cit", os.path.join(os.path.dirname(__file__), "check-ignored-tests.py")
)
cit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cit)

STATES = {1: "open", 2: "closed", 3: "pull_request"}


def lookup(n):
    return STATES.get(n, "missing")


def run(reason):
    return cit.check([("f.rs", 1, reason)], lookup)


class CheckTests(unittest.TestCase):
    def test_no_issue_number(self):
        self.assertEqual(len(run("flaky")[0]), 1)

    def test_bare_ignore(self):
        self.assertEqual(len(run(None)[0]), 1)

    def test_closed_issue(self):
        self.assertIn("closed", run("until #2 lands")[0][0])

    def test_nonexistent_issue(self):
        self.assertIn("missing", run("until #99 lands")[0][0])

    def test_pull_request_is_not_an_issue(self):
        self.assertEqual(len(run("see #3")[0]), 1)

    def test_open_issue(self):
        self.assertEqual(run("until #1 lands"), ([], []))

    def test_one_open_among_several(self):
        self.assertEqual(run("#2 and #1"), ([], []))

    def test_api_failure_is_error_not_pass(self):
        def boom(n):
            raise cit.VerificationError(f"#{n}: unreachable")

        problems, errors = cit.check([("f.rs", 1, "see #1")], boom)
        self.assertEqual(problems, [])
        self.assertEqual(len(errors), 1)


def http_error(code, headers=None):
    return urllib.error.HTTPError("u", code, "x", headers or {}, io.BytesIO(b""))


class FetchTests(unittest.TestCase):
    def fetch(self, responses):
        calls = []

        def opener(req, timeout=None):
            r = responses[min(len(calls), len(responses) - 1)]
            calls.append(req)
            if isinstance(r, Exception):
                raise r
            return io.BytesIO(r.encode())

        return (lambda: cit.fetch_issue_state("o/r", 1, "t", "http://x", opener, lambda s: None)), calls

    def test_open_and_pr(self):
        f, _ = self.fetch(['{"state":"open"}'])
        self.assertEqual(f(), "open")
        f, _ = self.fetch(['{"state":"open","pull_request":{}}'])
        self.assertEqual(f(), "pull_request")

    def test_404_missing(self):
        f, _ = self.fetch([http_error(404)])
        self.assertEqual(f(), "missing")

    def test_rate_limit_retries_then_succeeds(self):
        f, calls = self.fetch([http_error(403, {"X-RateLimit-Remaining": "0", "Retry-After": "1"}), '{"state":"closed"}'])
        self.assertEqual(f(), "closed")
        self.assertEqual(len(calls), 2)

    def test_rate_limit_exhausted_raises(self):
        f, _ = self.fetch([http_error(429, {"Retry-After": "1"})])
        with self.assertRaises(cit.VerificationError):
            f()

    def test_long_rate_limit_raises_without_waiting(self):
        f, calls = self.fetch([http_error(403, {"X-RateLimit-Remaining": "0", "Retry-After": "3600"})])
        with self.assertRaises(cit.VerificationError):
            f()
        self.assertEqual(len(calls), 1)

    def test_403_without_rate_limit_raises(self):
        f, _ = self.fetch([http_error(403, {"X-RateLimit-Remaining": "5"})])
        with self.assertRaises(cit.VerificationError):
            f()

    def test_network_error_raises(self):
        f, _ = self.fetch([urllib.error.URLError("down")])
        with self.assertRaises(cit.VerificationError):
            f()

    def test_garbage_body_raises(self):
        f, _ = self.fetch(["not json"])
        with self.assertRaises(cit.VerificationError):
            f()


class ScanTests(unittest.TestCase):
    def test_find_ignores_skips_comments(self):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "a.rs"), "w") as fh:
                fh.write('//! ```ignore\n#[test]\n#[ignore = "see #1"]\nfn a(){}\n#[ignore]\nfn b(){}\n')
            found = list(cit.find_ignores(d))
        self.assertEqual([(l, r) for _, l, r in found], [(3, "see #1"), (5, None)])


if __name__ == "__main__":
    unittest.main()
