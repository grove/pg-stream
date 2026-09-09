#!/usr/bin/env python3
"""Validate the v0.96 release surfaces and checked-in artifacts."""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent

required_files = (
    "roadmap/v0.96.0.md",
    "roadmap/v0.96.0.md-full.md",
    "plans/PLAN_0_96_0.md",
    "sql/pg_trickle--0.95.0--0.96.0.sql",
    "sql/archive/pg_trickle--0.96.0.sql",
    "docs/ERRORS.md",
)
missing = [path for path in required_files if not (ROOT / path).is_file()]

sources = "\n".join(
    (ROOT / path).read_text(encoding="utf-8")
    for path in (
        "src/api/release_096.rs",
        "src/lib.rs",
        "sql/pg_trickle--0.95.0--0.96.0.sql",
        "sql/archive/pg_trickle--0.96.0.sql",
    )
)
needles = (
    "active_profile",
    "disk_usage",
    "error_catalog",
    "pg_stat_progress_pgtrickle",
    "pgtrickle_reader",
    "pgtrickle_operator",
    "pgtrickle_admin",
    "FORECAST_AND_REACT",
)
missing_contracts = [needle for needle in needles if needle not in sources]

roadmap = (ROOT / "ROADMAP.md").read_text(encoding="utf-8")
if not re.search(r"\[v0\.96\.0\].*?✅ Released", roadmap):
    missing_contracts.append("ROADMAP.md v0.96.0 released status")

if missing or missing_contracts:
    for path in missing:
        print(f"ERROR: missing release artifact: {path}")
    for contract in missing_contracts:
        print(f"ERROR: missing v0.96 contract: {contract}")
    sys.exit(1)

print("v0.96 release gate passed")
