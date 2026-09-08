#!/usr/bin/env python3
"""Offline release gate for durable typed output deltas."""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
VERSION = "0.95.0"


def main() -> int:
    errors = []
    for path in (
        "roadmap/v0.95.0.md",
        "sql/pg_trickle--0.94.0--0.95.0.sql",
        "sql/archive/pg_trickle--0.95.0.sql",
    ):
        if not (ROOT / path).is_file():
            errors.append(f"missing v0.95 artifact: {path}")
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    if f'version = "{VERSION}"' not in cargo:
        errors.append(f"Cargo.toml does not declare {VERSION}")
    source = (ROOT / "src/api/output_delta.rs").read_text(encoding="utf-8")
    for marker in (
        "register_output_delta_consumer",
        "output_delta_batches",
        "FULL_INVALIDATION",
        "ack_output_delta_resnapshot",
    ):
        if marker not in source:
            errors.append(f"output-delta API missing marker: {marker}")
    migration = ROOT / "sql/pg_trickle--0.94.0--0.95.0.sql"
    if migration.is_file() and "register_output_delta_consumer" not in migration.read_text(encoding="utf-8"):
        errors.append("migration does not install output-delta consumers")
    tests = source
    if not re.search(r"\bfn\s+test_delta_batch_mode_fails_closed", tests):
        errors.append("missing output-delta fail-closed unit test")
    if errors:
        print("v0_95_release_gate: FAILED")
        print("\n".join(f"- {error}" for error in errors))
        return 1
    print("v0_95_release_gate: passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
