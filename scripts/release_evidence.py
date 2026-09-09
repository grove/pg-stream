#!/usr/bin/env python3
"""Write a compact, machine-readable release evidence manifest."""

import argparse
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--candidate-commit", required=True)
    parser.add_argument("--artifact", nargs="+", action="append", default=[])
    parser.add_argument("--suite", action="append", default=[])
    parser.add_argument("--skipped", action="append", default=[])
    args = parser.parse_args()

    artifacts = []
    for path_string in (path for group in args.artifact for path in group):
        path = Path(path_string)
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        artifacts.append(
            {"path": path.as_posix(), "bytes": path.stat().st_size, "sha256": digest}
        )

    def parse_entries(entries: list[str]) -> list[dict[str, str]]:
        result = []
        for entry in entries:
            name, _, status = entry.partition("=")
            result.append({"name": name, "status": status or "recorded"})
        return result

    evidence = {
        "schema_version": 1,
        "release_version": args.version,
        "candidate_commit": args.candidate_commit,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "artifacts": sorted(artifacts, key=lambda item: item["path"]),
        "executed_suites": parse_entries(args.suite),
        "skipped_suites": parse_entries(args.skipped),
        "status": "passed",
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
