#!/usr/bin/env python3
"""Tests for deploy-testnet input validation (unit + end-to-end bash execution)."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPTS_DIR = Path(__file__).resolve().parent
VALIDATOR_PATH = SCRIPTS_DIR / "validate-deploy-env.py"

SPEC = importlib.util.spec_from_file_location("validate_deploy_env", VALIDATOR_PATH)
assert SPEC is not None and SPEC.loader is not None
validator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(validator)


def make_strkey(prefix: str, fill: str) -> str:
    return prefix + (fill * 55)


VALID_ESCROW_ENV = {
    "SOURCE_ACCOUNT": "deployer",
    "ADMIN_ADDRESS": make_strkey("G", "A"),
    "CLIENT_ADDRESS": make_strkey("G", "B"),
    "FREELANCER_ADDRESS": make_strkey("G", "C"),
    "ARBITER_ADDRESS": make_strkey("G", "D"),
    "TOKEN_ADDRESS": make_strkey("C", "E"),
    "AUTO_RELEASE_SECONDS": "604800",
    "MILESTONE_AMOUNTS": '["10000000"]',
    "MILESTONE_SPEC_HASHES": '["' + ("a1" * 32) + '"]',
    "FUNDING_MODE": "Upfront",
    "DISPUTE_TIMEOUT_SECONDS": "2592000",
    "ARBITER_FEE_BPS": "0",
}

VALID_FACTORY_ENV = {
    "SOURCE_ACCOUNT": "deployer",
    "ADMIN_ADDRESS": make_strkey("G", "A"),
    "ADMIN_SOURCE": "admin-key",
    "ALLOWED_TOKENS": make_strkey("C", "B"),
    "MAX_WASM_BYTES": "131072",
}


def find_bash() -> str | None:
    if os.name == "nt":
        git_bash = Path(r"C:\Program Files\Git\bin\bash.exe")
        if git_bash.is_file():
            return str(git_bash)
    return shutil.which("bash")


class ValidatorUnitTests(unittest.TestCase):
    def test_valid_escrow_passes(self) -> None:
        validator.validate_escrow_env(dict(VALID_ESCROW_ENV))

    def test_valid_escrow_numeric_amounts_and_permilestone(self) -> None:
        env = dict(VALID_ESCROW_ENV)
        env["MILESTONE_AMOUNTS"] = "[10000000, 25000000]"
        env["MILESTONE_SPEC_HASHES"] = '["' + ("00" * 32) + '", "' + ("ff" * 32) + '"]'
        env["FUNDING_MODE"] = "PerMilestone"
        env["ARBITER_FEE_BPS"] = "1000"
        env["DISPUTE_TIMEOUT_SECONDS"] = "86400"
        env["AUTO_RELEASE_SECONDS"] = str(validator.MAX_AUTO_RELEASE_SECONDS)
        validator.validate_escrow_env(env)

    def test_admin_may_equal_arbiter(self) -> None:
        env = dict(VALID_ESCROW_ENV)
        env["ARBITER_ADDRESS"] = env["ADMIN_ADDRESS"]
        validator.validate_escrow_env(env)

    def test_rejects_bad_funding_mode(self) -> None:
        env = dict(VALID_ESCROW_ENV)
        env["FUNDING_MODE"] = "upfront"
        with self.assertRaisesRegex(validator.ValidationError, "FUNDING_MODE"):
            validator.validate_escrow_env(env)

    def test_rejects_out_of_range_arbiter_fee_bps(self) -> None:
        for bad in ("-1", "1001", "abc", "10 "):
            env = dict(VALID_ESCROW_ENV)
            env["ARBITER_FEE_BPS"] = bad
            with self.assertRaisesRegex(validator.ValidationError, "ARBITER_FEE_BPS"):
                validator.validate_escrow_env(env)

    def test_rejects_out_of_range_dispute_timeout(self) -> None:
        for bad in ("0", "86399", "15552001", "not-an-int"):
            env = dict(VALID_ESCROW_ENV)
            env["DISPUTE_TIMEOUT_SECONDS"] = bad
            with self.assertRaisesRegex(
                validator.ValidationError, "DISPUTE_TIMEOUT_SECONDS"
            ):
                validator.validate_escrow_env(env)

    def test_rejects_out_of_range_auto_release(self) -> None:
        for bad in ("0", "-5", str(validator.MAX_AUTO_RELEASE_SECONDS + 1), "1e5"):
            env = dict(VALID_ESCROW_ENV)
            env["AUTO_RELEASE_SECONDS"] = bad
            with self.assertRaisesRegex(
                validator.ValidationError, "AUTO_RELEASE_SECONDS"
            ):
                validator.validate_escrow_env(env)

    def test_rejects_bad_milestone_amounts(self) -> None:
        bad_values = (
            "not-json",
            "{}",
            "[]",
            "[0]",
            "[-1]",
            '["0"]',
            "[true]",
            "[1.5]",
            f"[{validator.I128_MAX}, 1]",
            "[" + ",".join(["1"] * 51) + "]",
        )
        for bad in bad_values:
            env = dict(VALID_ESCROW_ENV)
            env["MILESTONE_AMOUNTS"] = bad
            with self.assertRaisesRegex(validator.ValidationError, "MILESTONE_AMOUNTS"):
                validator.validate_escrow_env(env)

    def test_rejects_bad_milestone_spec_hashes(self) -> None:
        bad_values = (
            "not-json",
            "{}",
            "[]",
            '["0x' + ("ab" * 32) + '"]',
            '["' + ("ab" * 31) + '"]',
            '["' + ("zz" * 32) + '"]',
            "[123]",
        )
        for bad in bad_values:
            env = dict(VALID_ESCROW_ENV)
            env["MILESTONE_SPEC_HASHES"] = bad
            with self.assertRaisesRegex(
                validator.ValidationError, "MILESTONE_SPEC_HASHES"
            ):
                validator.validate_escrow_env(env)

    def test_rejects_mismatched_milestone_lengths(self) -> None:
        env = dict(VALID_ESCROW_ENV)
        env["MILESTONE_AMOUNTS"] = '["10000000", "20000000"]'
        with self.assertRaisesRegex(
            validator.ValidationError, "MILESTONE_AMOUNTS.*MILESTONE_SPEC_HASHES"
        ):
            validator.validate_escrow_env(env)

    def test_rejects_bad_addresses_and_distinct_parties(self) -> None:
        for var in ("ADMIN_ADDRESS", "CLIENT_ADDRESS", "FREELANCER_ADDRESS", "ARBITER_ADDRESS"):
            env = dict(VALID_ESCROW_ENV)
            env[var] = "X" + ("A" * 55)
            with self.assertRaisesRegex(validator.ValidationError, var):
                validator.validate_escrow_env(env)

        env = dict(VALID_ESCROW_ENV)
        env["TOKEN_ADDRESS"] = make_strkey("G", "E")
        with self.assertRaisesRegex(validator.ValidationError, "TOKEN_ADDRESS"):
            validator.validate_escrow_env(env)

        for left, right in (
            ("CLIENT_ADDRESS", "FREELANCER_ADDRESS"),
            ("ARBITER_ADDRESS", "CLIENT_ADDRESS"),
            ("ARBITER_ADDRESS", "FREELANCER_ADDRESS"),
            ("ADMIN_ADDRESS", "CLIENT_ADDRESS"),
            ("ADMIN_ADDRESS", "FREELANCER_ADDRESS"),
        ):
            env = dict(VALID_ESCROW_ENV)
            env[right] = env[left]
            with self.assertRaisesRegex(validator.ValidationError, left):
                validator.validate_escrow_env(env)

    def test_valid_factory_passes(self) -> None:
        env = dict(VALID_FACTORY_ENV)
        env["ALLOWED_TOKENS"] = f"{make_strkey('C', 'B')},{make_strkey('C', 'C')}"
        validator.validate_factory_env(env)

    def test_rejects_bad_factory_inputs(self) -> None:
        bad_cases = (
            ("ADMIN_ADDRESS", "G" + ("A" * 54), "ADMIN_ADDRESS"),
            ("ALLOWED_TOKENS", "", "ALLOWED_TOKENS"),
            ("ALLOWED_TOKENS", f"{make_strkey('C', 'B')},", "ALLOWED_TOKENS"),
            ("ALLOWED_TOKENS", make_strkey("G", "B"), "ALLOWED_TOKENS"),
            ("MAX_WASM_BYTES", "0", "MAX_WASM_BYTES"),
            ("MAX_WASM_BYTES", "128KB", "MAX_WASM_BYTES"),
        )
        for key, val, expected_msg in bad_cases:
            env = dict(VALID_FACTORY_ENV)
            env[key] = val
            with self.assertRaisesRegex(validator.ValidationError, expected_msg):
                validator.validate_factory_env(env)


class DeployScriptE2ETests(unittest.TestCase):
    def setUp(self) -> None:
        self.bash = find_bash()
        if not self.bash:
            self.skipTest("bash executable not found")

        self.tmpdir = Path(tempfile.mkdtemp(prefix="escrow-deploy-val-"))
        self.addCleanup(shutil.rmtree, self.tmpdir, True)

        self.repo = self.tmpdir / "repo"
        scripts_copy = self.repo / "scripts"
        scripts_copy.mkdir(parents=True)
        for name in (
            "deploy-testnet.sh",
            "deploy-factory-testnet.sh",
            "validate-deploy-env.py",
        ):
            shutil.copy2(SCRIPTS_DIR / name, scripts_copy / name)

        self.build_marker = self.tmpdir / "BUILD_RAN"
        build_stub = scripts_copy / "build-reproducible.sh"
        # Deliberately fail right after marking so stubbed execution stops at build.
        build_stub.write_text(
            f"#!/usr/bin/env bash\ntouch '{self.build_marker.as_posix()}'\nexit 99\n",
            encoding="utf-8",
            newline="\n",
        )
        build_stub.chmod(0o755)

        self.bin_dir = self.tmpdir / "bin"
        self.bin_dir.mkdir()
        for tool in ("stellar", "docker"):
            stub = self.bin_dir / tool
            stub.write_text("#!/usr/bin/env bash\nexit 0\n", encoding="utf-8", newline="\n")
            stub.chmod(0o755)

        py_exe = Path(sys.executable).as_posix()
        py_stub = self.bin_dir / "python3"
        py_stub.write_text(
            f"#!/usr/bin/env bash\nexec '{py_exe}' \"$@\"\n",
            encoding="utf-8",
            newline="\n",
        )
        py_stub.chmod(0o755)

    def _run_script(self, script_name: str, extra_env: dict[str, str]) -> subprocess.CompletedProcess[str]:
        if self.build_marker.exists():
            self.build_marker.unlink()
        env = os.environ.copy()
        env.update(extra_env)
        env["PATH"] = f"{self.bin_dir}{os.pathsep}{env.get('PATH', '')}"
        return subprocess.run(
            [self.bash, str(self.repo / "scripts" / script_name)],
            cwd=str(self.repo),
            env=env,
            capture_output=True,
            text=True,
        )

    def test_deploy_testnet_rejects_bad_input_before_build(self) -> None:
        bad_env = dict(VALID_ESCROW_ENV)
        bad_env["FUNDING_MODE"] = "InvalidMode"
        res = self._run_script("deploy-testnet.sh", bad_env)
        self.assertEqual(res.returncode, 1, res.stderr)
        self.assertIn("FUNDING_MODE", res.stderr)
        self.assertFalse(
            self.build_marker.exists(),
            "build-reproducible.sh must not run when validation fails",
        )

    def test_deploy_testnet_valid_input_reaches_build(self) -> None:
        res = self._run_script("deploy-testnet.sh", dict(VALID_ESCROW_ENV))
        self.assertEqual(res.returncode, 99, res.stderr)
        self.assertTrue(
            self.build_marker.exists(),
            "valid deploy-testnet.sh invocation should proceed to build-reproducible.sh",
        )

    def test_deploy_factory_rejects_bad_input_before_build(self) -> None:
        bad_env = dict(VALID_FACTORY_ENV)
        bad_env["ALLOWED_TOKENS"] = make_strkey("G", "B")
        res = self._run_script("deploy-factory-testnet.sh", bad_env)
        self.assertEqual(res.returncode, 1, res.stderr)
        self.assertIn("ALLOWED_TOKENS", res.stderr)
        self.assertFalse(
            self.build_marker.exists(),
            "build-reproducible.sh must not run when factory validation fails",
        )

    def test_deploy_factory_valid_input_reaches_build(self) -> None:
        res = self._run_script("deploy-factory-testnet.sh", dict(VALID_FACTORY_ENV))
        self.assertEqual(res.returncode, 99, res.stderr)
        self.assertTrue(
            self.build_marker.exists(),
            "valid deploy-factory-testnet.sh invocation should proceed to build-reproducible.sh",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
