#!/usr/bin/env python3
"""Validate the v0.101 exact-relational-state release contract."""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.101.0"
MANIFEST = ROOT / "docs/capability-manifest.json"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"v0.101 release gate failed: {message}")


def main() -> None:
    with (ROOT / "Cargo.toml").open("rb") as handle:
        require(
            tomllib.load(handle)["package"]["version"] == VERSION,
            "Cargo.toml version drift",
        )

    meta = json.loads((ROOT / "META.json").read_text(encoding="utf-8"))
    require(meta["version"] == VERSION, "META.json top-level version drift")
    require(
        meta["provides"]["pg_trickle"]["version"] == VERSION,
        "META.json provides version drift",
    )
    require(
        (ROOT / f"sql/archive/pg_trickle--{VERSION}.sql").is_file(),
        "full SQL archive missing",
    )
    require(
        (ROOT / "sql/pg_trickle--0.100.0--0.101.0.sql").is_file(),
        "upgrade migration missing",
    )

    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    require(manifest["manifest_version"] == 1, "manifest version must be 1")
    require(manifest["release_version"] == VERSION, "manifest release version drift")
    capabilities = {item["id"]: item for item in manifest["capabilities"]}
    require(
        capabilities["trigger-capture"]["enabled"] is True,
        "trigger capture must stay enabled",
    )
    require(
        capabilities["graph-v1"]["enabled"] is False,
        "Graph V1 must stay disabled by default",
    )
    require(
        capabilities["delta-v1"]["enabled"] is False,
        "Delta V1 must stay disabled",
    )

    roadmap = (ROOT / "roadmap/v0.101.0.md").read_text(encoding="utf-8")
    require("> **Status:** Released" in roadmap, "roadmap release status is stale")
    roadmap_index = (ROOT / "ROADMAP.md").read_text(encoding="utf-8")
    require(
        "[v0.101.0]" in roadmap_index and "✅ Released" in roadmap_index,
        "roadmap index is stale",
    )
    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    require("0.101.0" in changelog, "CHANGELOG.md is missing the release entry")

    subprocess.run(
        [sys.executable, str(ROOT / "scripts/generate_capability_manifest.py"), "--check"],
        cwd=ROOT,
        check=True,
    )
    print("v0.101 release gate passed")


if __name__ == "__main__":
    main()
