#!/usr/bin/env python3
"""Write candidate-bound release evidence from structured suite results."""

from __future__ import annotations

import argparse
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path


RESULT_STATES = {"passed", "failed", "skipped", "unavailable", "stale", "historical"}


def parse_result(entry: str, *, default_status: str | None = None) -> dict[str, str]:
    name, separator, value = entry.partition("=")
    if not name or (not separator and default_status is None):
        raise ValueError(f"suite result must be NAME=STATUS: {entry!r}")
    status = default_status or value
    if status not in RESULT_STATES:
        raise ValueError(f"unknown suite status {status!r} for {name!r}")
    result = {"name": name, "status": status}
    if default_status is not None and separator and value:
        result["reason"] = value
    return result


def required_suite_ids(qualification: Path | None) -> list[str]:
    if qualification is None:
        return []
    contract = json.loads(qualification.read_text(encoding="utf-8"))
    return [suite["id"] for suite in contract.get("required_suites", []) if suite.get("required")]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--candidate-commit", required=True)
    parser.add_argument("--qualification", type=Path)
    parser.add_argument("--capability-manifest", type=Path)
    parser.add_argument("--artifact", nargs="+", action="append", default=[])
    parser.add_argument("--suite", action="append", default=[])
    parser.add_argument("--skipped", action="append", default=[])
    parser.add_argument("--required-suite", action="append", default=[])
    args = parser.parse_args()

    try:
        artifacts = []
        for path_string in (path for group in args.artifact for path in group):
            path = Path(path_string)
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            artifacts.append(
                {"path": path.as_posix(), "bytes": path.stat().st_size, "sha256": digest}
            )

        results = [parse_result(entry) for entry in args.suite]
        results.extend(parse_result(entry, default_status="skipped") for entry in args.skipped)
        names = [result["name"] for result in results]
        if len(names) != len(set(names)):
            raise ValueError("duplicate suite result identifier")

        required = set(args.required_suite or required_suite_ids(args.qualification))
        actual = {result["name"]: result for result in results}
        missing = sorted(required - actual.keys())
        incomplete = sorted(
            name for name in required if name in actual and actual[name]["status"] != "passed"
        )
        status = "passed" if not missing and not incomplete else "blocked"

        capability_manifest = None
        if args.capability_manifest is not None:
            manifest = json.loads(args.capability_manifest.read_text(encoding="utf-8"))
            manifest_version = manifest.get("manifest_version")
            manifest_release = manifest.get("release_version")
            if not isinstance(manifest_version, int) or not isinstance(manifest_release, str):
                raise ValueError("capability manifest is missing version metadata")
            capability_manifest = {
                "version": manifest_version,
                "release_version": manifest_release,
                "sha256": hashlib.sha256(args.capability_manifest.read_bytes()).hexdigest(),
            }

        evidence = {
            "schema_version": 2,
            "release_version": args.version,
            "candidate_commit": args.candidate_commit,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "artifacts": sorted(artifacts, key=lambda item: item["path"]),
            "executed_suites": sorted(results, key=lambda item: item["name"]),
            "required_suites": sorted(required),
            "missing_required_suites": missing,
            "incomplete_required_suites": incomplete,
            "status": status,
        }
        if capability_manifest is not None:
            evidence["capability_manifest"] = capability_manifest
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
        if status != "passed":
            raise ValueError(
                "required release suites are incomplete: "
                + ", ".join(missing + incomplete)
            )
    except (OSError, ValueError, json.JSONDecodeError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
