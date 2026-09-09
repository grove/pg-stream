#!/usr/bin/env python3
"""Validate the frozen v0.98 qualification contract."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "tests/release/v0.98-qualification.json"
VERSION = "0.98.0"
EXACT_VERSION = re.compile(r"^\d+\.\d+\.\d+$")
SUITE_STATUSES = {"passed", "failed", "skipped", "unavailable", "stale", "historical"}
BLOCKER_STATUSES = {"open", "closed", "mitigated", "accepted"}


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def reject_placeholders(value: Any, path: str, errors: list[str]) -> None:
    if value is None:
        fail(errors, f"{path} must not be null")
    elif isinstance(value, str):
        if not value.strip():
            fail(errors, f"{path} must not be empty")
        if "tbd" in value.lower() or "*" in value:
            fail(errors, f"{path} contains a placeholder or wildcard")
        if path.endswith("version") or path.endswith("source_versions[]"):
            if not EXACT_VERSION.fullmatch(value):
                fail(errors, f"{path} must be an exact x.y.z version")
    elif isinstance(value, dict):
        for key, child in value.items():
            reject_placeholders(child, f"{path}.{key}", errors)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            reject_placeholders(child, f"{path}[{index}]", errors)


def require_numeric_thresholds(items: Any, name: str, errors: list[str]) -> None:
    if not isinstance(items, list) or not items:
        fail(errors, f"{name} must contain at least one budget")
        return
    for index, item in enumerate(items):
        if not isinstance(item, dict):
            fail(errors, f"{name}[{index}] must be an object")
            continue
        threshold = item.get("threshold")
        if not isinstance(threshold, (int, float)) or isinstance(threshold, bool):
            fail(errors, f"{name}[{index}].threshold must be numeric")


def main() -> int:
    errors: list[str] = []
    try:
        contract = json.loads(CONTRACT.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"v0_98_qualification_gate: FAILED: {error}")
        return 1

    reject_placeholders(contract, "contract", errors)
    if contract.get("contract_version") != 1:
        fail(errors, "contract_version must be 1")
    if contract.get("release_version") != VERSION:
        fail(errors, f"release_version must be {VERSION}")

    source_versions = contract.get("source_versions")
    if not isinstance(source_versions, list) or source_versions != ["0.97.0"]:
        fail(errors, "source_versions must contain exactly 0.97.0")

    required_suites = contract.get("required_suites")
    if not isinstance(required_suites, list) or not required_suites:
        fail(errors, "required_suites must be non-empty")
        required_suites = []
    suite_ids: list[str] = []
    for index, suite in enumerate(required_suites):
        if not isinstance(suite, dict):
            fail(errors, f"required_suites[{index}] must be an object")
            continue
        suite_id = suite.get("id")
        if isinstance(suite_id, str):
            suite_ids.append(suite_id)
        if not isinstance(suite_id, str) or not suite_id:
            fail(errors, f"required_suites[{index}].id is missing")
        if not isinstance(suite.get("command"), str) or not suite["command"].strip():
            fail(errors, f"required_suites[{index}].command is missing")
        if suite.get("required") is not True or suite.get("enabled") is not True:
            fail(errors, f"required suite {suite_id!r} must be required and enabled")
        if suite.get("expected_status", "passed") not in SUITE_STATUSES:
            fail(errors, f"required suite {suite_id!r} has an unknown expected status")
        if suite.get("expected_status", "passed") != "passed":
            fail(errors, f"required suite {suite_id!r} must require passed evidence")
    if len(suite_ids) != len(set(suite_ids)):
        fail(errors, "required_suites contains duplicate identifiers")

    capabilities = contract.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        fail(errors, "capabilities must be non-empty")
        capabilities = []
    for index, capability in enumerate(capabilities):
        if not isinstance(capability, dict):
            fail(errors, f"capabilities[{index}] must be an object")
            continue
        if capability.get("status") not in {"experimental", "stable", "unsupported"}:
            fail(errors, f"capability {capability.get('id')!r} has an unknown status")
        if not isinstance(capability.get("enabled"), bool):
            fail(errors, f"capability {capability.get('id')!r} must declare enabled")

    artifacts = contract.get("artifacts")
    if not isinstance(artifacts, list) or not artifacts:
        fail(errors, "artifacts must be non-empty")
    else:
        for index, artifact in enumerate(artifacts):
            if not isinstance(artifact, dict):
                fail(errors, f"artifacts[{index}] must be an object")
                continue
            if artifact.get("version") != VERSION:
                fail(errors, f"artifact {artifact.get('id')!r} has the wrong version")
            if artifact.get("postgresql_major") != 18:
                fail(errors, f"artifact {artifact.get('id')!r} must target PostgreSQL 18")

    soak = contract.get("soak", {})
    longevity = contract.get("longevity", {})
    if soak.get("duration_hours") != 72:
        fail(errors, "soak.duration_hours must be exactly 72")
    if longevity.get("duration_days") != 7:
        fail(errors, "longevity.duration_days must be exactly 7")
    for name, item in (("soak", soak), ("longevity", longevity)):
        if item.get("required") is not True or item.get("enabled") is not True:
            fail(errors, f"{name} must be required and enabled")
        if not isinstance(item.get("command"), str) or not item["command"].strip():
            fail(errors, f"{name}.command is missing")
        if item.get("id") not in suite_ids:
            fail(errors, f"{name}.id must also be a required suite")

    require_numeric_thresholds(contract.get("performance_budgets"), "performance_budgets", errors)
    require_numeric_thresholds(contract.get("growth_budgets"), "growth_budgets", errors)

    blockers = contract.get("blockers")
    if not isinstance(blockers, list):
        fail(errors, "blockers must be a list")
        blockers = []
    blocker_ids = []
    for index, blocker in enumerate(blockers):
        if not isinstance(blocker, dict):
            fail(errors, f"blockers[{index}] must be an object")
            continue
        blocker_id = blocker.get("id")
        if isinstance(blocker_id, str):
            blocker_ids.append(blocker_id)
        if blocker.get("severity") not in {"P0", "P1", "P2", "P3"}:
            fail(errors, f"blocker {blocker.get('id')!r} has an unknown severity")
        if blocker.get("status") not in BLOCKER_STATUSES:
            fail(errors, f"blocker {blocker.get('id')!r} has an unknown status")
        for field in ("affected_stable_capability", "verifying_suite"):
            if not isinstance(blocker.get(field), str) or not blocker[field].strip():
                fail(errors, f"blocker {blocker.get('id')!r} is missing {field}")
        if blocker.get("severity") in {"P0", "P1"} and blocker.get("status") == "open":
            fail(errors, f"open {blocker['severity']} blocker: {blocker.get('id')}")
    if len(blocker_ids) != len(set(blocker_ids)):
        fail(errors, "blockers contains duplicate identifiers")

    identifiers = suite_ids + [item.get("id") for item in capabilities]
    identifiers += [item.get("id") for item in contract.get("performance_budgets", [])]
    identifiers += [item.get("id") for item in contract.get("growth_budgets", [])]
    identifiers += [item.get("id") for item in blockers]
    identifiers = [identifier for identifier in identifiers if isinstance(identifier, str)]
    if len(identifiers) != len(set(identifiers)):
        fail(errors, "contract identifiers must be unique across all sections")

    if errors:
        print("v0_98_qualification_gate: FAILED")
        print("\n".join(f"- {error}" for error in errors))
        return 1
    print("v0_98_qualification_gate: passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
