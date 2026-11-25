#!/usr/bin/env python3
"""Validate deployment environment variables before building or uploading contracts."""

from __future__ import annotations

import json
import os
import re
import sys

MAX_MILESTONES = 50
MAX_AUTO_RELEASE_SECONDS = 10 * 365 * 24 * 60 * 60  # 315_360_000
MIN_DISPUTE_TIMEOUT_SECONDS = 86_400
MAX_DISPUTE_TIMEOUT_SECONDS = 15_552_000
MAX_ARBITER_FEE_BPS = 1_000
I128_MAX = (1 << 127) - 1
VALID_FUNDING_MODES = ("Upfront", "PerMilestone")

INT_RE = re.compile(r"^-?[0-9]+$")
UINT_RE = re.compile(r"^[0-9]+$")
HEX32_RE = re.compile(r"^[0-9a-fA-F]{64}$")
ADDRESS_RE = re.compile(r"^[GC][A-Z2-7]{55}$")
CONTRACT_ID_RE = re.compile(r"^C[A-Z2-7]{55}$")


class ValidationError(ValueError):
    """Raised when an environment variable fails deploy-time validation."""


def parse_int_range(
    name: str,
    raw: str,
    minimum: int,
    maximum: int | None = None,
) -> int:
    """Parse a canonical base-10 integer string and enforce inclusive bounds."""
    if not INT_RE.fullmatch(raw):
        raise ValidationError(f"{name}: expected integer, got {raw!r}")
    value = int(raw)
    if value < minimum:
        raise ValidationError(f"{name}: must be >= {minimum}, got {value}")
    if maximum is not None and value > maximum:
        raise ValidationError(f"{name}: must be <= {maximum}, got {value}")
    return value


def validate_address(name: str, raw: str, *, contract_only: bool = False) -> str:
    """Validate a 56-character Stellar StrKey (G... account or C... contract)."""
    pattern = CONTRACT_ID_RE if contract_only else ADDRESS_RE
    kind = "contract ID (C..., 56 StrKey chars)" if contract_only else "Stellar address (G... or C..., 56 StrKey chars)"
    if not pattern.fullmatch(raw):
        raise ValidationError(f"{name}: expected {kind}, got {raw!r}")
    return raw


def parse_milestone_amounts(raw: str) -> list[int]:
    """Parse MILESTONE_AMOUNTS as a non-empty JSON array (1..50) of positive ints."""
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ValidationError(f"MILESTONE_AMOUNTS: invalid JSON ({exc.msg})") from exc

    if not isinstance(parsed, list):
        raise ValidationError("MILESTONE_AMOUNTS: must be a JSON array")
    if not 1 <= len(parsed) <= MAX_MILESTONES:
        raise ValidationError(
            f"MILESTONE_AMOUNTS: expected 1..{MAX_MILESTONES} milestones, got {len(parsed)}"
        )

    amounts: list[int] = []
    total = 0
    for idx, item in enumerate(parsed):
        if isinstance(item, bool):
            raise ValidationError(
                f"MILESTONE_AMOUNTS: element [{idx}] must be a positive integer, got bool"
            )
        if isinstance(item, int):
            val = item
        elif isinstance(item, str) and UINT_RE.fullmatch(item):
            val = int(item)
        else:
            raise ValidationError(
                f"MILESTONE_AMOUNTS: element [{idx}] must be a positive integer or digit string, got {item!r}"
            )
        if val <= 0:
            raise ValidationError(
                f"MILESTONE_AMOUNTS: element [{idx}] must be > 0, got {val}"
            )
        total += val
        if total > I128_MAX:
            raise ValidationError(
                f"MILESTONE_AMOUNTS: total milestone sum exceeds i128::MAX ({I128_MAX})"
            )
        amounts.append(val)
    return amounts


def parse_milestone_hashes(raw: str) -> list[str]:
    """Parse MILESTONE_SPEC_HASHES as a non-empty JSON array (1..50) of 64-hex strings."""
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ValidationError(f"MILESTONE_SPEC_HASHES: invalid JSON ({exc.msg})") from exc

    if not isinstance(parsed, list):
        raise ValidationError("MILESTONE_SPEC_HASHES: must be a JSON array")
    if not 1 <= len(parsed) <= MAX_MILESTONES:
        raise ValidationError(
            f"MILESTONE_SPEC_HASHES: expected 1..{MAX_MILESTONES} hashes, got {len(parsed)}"
        )

    hashes: list[str] = []
    for idx, item in enumerate(parsed):
        if not isinstance(item, str) or not HEX32_RE.fullmatch(item):
            raise ValidationError(
                f"MILESTONE_SPEC_HASHES: element [{idx}] must be a 64-character hex string, got {item!r}"
            )
        hashes.append(item)
    return hashes


def validate_escrow_env(env: dict[str, str]) -> None:
    """Validate all inputs required by scripts/deploy-testnet.sh."""
    addresses = {
        name: validate_address(name, env.get(name, ""))
        for name in (
            "ADMIN_ADDRESS",
            "CLIENT_ADDRESS",
            "FREELANCER_ADDRESS",
            "ARBITER_ADDRESS",
        )
    }
    validate_address("TOKEN_ADDRESS", env.get("TOKEN_ADDRESS", ""), contract_only=True)

    # Matches milestone-escrow initialize() party distinctness (admin == arbiter is allowed).
    forbidden_pairs = (
        ("CLIENT_ADDRESS", "FREELANCER_ADDRESS"),
        ("ARBITER_ADDRESS", "CLIENT_ADDRESS"),
        ("ARBITER_ADDRESS", "FREELANCER_ADDRESS"),
        ("ADMIN_ADDRESS", "CLIENT_ADDRESS"),
        ("ADMIN_ADDRESS", "FREELANCER_ADDRESS"),
    )
    for left, right in forbidden_pairs:
        if addresses[left] == addresses[right]:
            raise ValidationError(f"{left} and {right} must be distinct addresses")

    parse_int_range(
        "AUTO_RELEASE_SECONDS",
        env.get("AUTO_RELEASE_SECONDS", ""),
        1,
        MAX_AUTO_RELEASE_SECONDS,
    )

    funding_mode = env.get("FUNDING_MODE", "")
    if funding_mode not in VALID_FUNDING_MODES:
        raise ValidationError(
            f"FUNDING_MODE: expected one of {VALID_FUNDING_MODES}, got {funding_mode!r}"
        )

    parse_int_range(
        "DISPUTE_TIMEOUT_SECONDS",
        env.get("DISPUTE_TIMEOUT_SECONDS", ""),
        MIN_DISPUTE_TIMEOUT_SECONDS,
        MAX_DISPUTE_TIMEOUT_SECONDS,
    )
    parse_int_range(
        "ARBITER_FEE_BPS",
        env.get("ARBITER_FEE_BPS", ""),
        0,
        MAX_ARBITER_FEE_BPS,
    )

    amounts = parse_milestone_amounts(env.get("MILESTONE_AMOUNTS", ""))
    hashes = parse_milestone_hashes(env.get("MILESTONE_SPEC_HASHES", ""))
    if len(amounts) != len(hashes):
        raise ValidationError(
            f"MILESTONE_AMOUNTS ({len(amounts)}) and MILESTONE_SPEC_HASHES ({len(hashes)}) must have the same length"
        )


def validate_factory_env(env: dict[str, str]) -> None:
    """Validate all inputs required by scripts/deploy-factory-testnet.sh."""
    validate_address("ADMIN_ADDRESS", env.get("ADMIN_ADDRESS", ""))

    raw_tokens = env.get("ALLOWED_TOKENS", "")
    if not raw_tokens:
        raise ValidationError("ALLOWED_TOKENS: must contain at least one contract ID")
    tokens = raw_tokens.split(",")
    for idx, token in enumerate(tokens):
        if not token:
            raise ValidationError(
                f"ALLOWED_TOKENS: empty token contract ID at position {idx}"
            )
        validate_address(f"ALLOWED_TOKENS[{idx}]", token, contract_only=True)

    parse_int_range("MAX_WASM_BYTES", env.get("MAX_WASM_BYTES", ""), 1)


def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[1] not in ("escrow", "factory"):
        print("Usage: validate-deploy-env.py <escrow|factory>", file=sys.stderr)
        return 2

    env = dict(os.environ)
    try:
        if argv[1] == "escrow":
            validate_escrow_env(env)
        else:
            validate_factory_env(env)
    except ValidationError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
