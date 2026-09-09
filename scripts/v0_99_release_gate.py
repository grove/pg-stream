#!/usr/bin/env python3
"""Fail closed when the v0.99 capability contract drifts."""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.99.0"
MANIFEST = ROOT / "docs/capability-manifest.json"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"v0.99 release gate failed: {message}")


def main() -> None:
    with (ROOT / "Cargo.toml").open("rb") as handle:
        require(tomllib.load(handle)["package"]["version"] == VERSION, "Cargo.toml version drift")

    meta = json.loads((ROOT / "META.json").read_text(encoding="utf-8"))
    require(meta["version"] == VERSION, "META.json top-level version drift")
    require(meta["provides"]["pg_trickle"]["version"] == VERSION, "META.json provides version drift")
    require((ROOT / f"sql/archive/pg_trickle--{VERSION}.sql").is_file(), "full SQL archive missing")
    require((ROOT / "sql/pg_trickle--0.98.1--0.99.0.sql").is_file(), "upgrade migration missing")

    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    require(manifest["manifest_version"] == 1, "manifest version must be 1")
    require(manifest["release_version"] == VERSION, "manifest release version drift")
    capabilities = {item["id"]: item for item in manifest["capabilities"]}
    require(capabilities["trigger-capture"]["enabled"] is True, "trigger capture must stay enabled")
    for capability_id, target in (("graph-v1", "v0.100.0"), ("delta-v1", "v0.104.0")):
        item = capabilities[capability_id]
        require(item["enabled"] is False, f"{capability_id} must remain disabled")
        require(item["status"] == "experimental", f"{capability_id} status drift")
        require(item["reason_code"] == "PGT_EXT_CAPABILITY_DISABLED", f"{capability_id} reason drift")
        require(item["target"] == target, f"{capability_id} target drift")
    require(capabilities["wal-capture"]["reason_code"] == "PGT_EXT_CDC_UNAVAILABLE", "WAL reason drift")
    require(all(item["test"] for item in manifest["examples"]), "manifest example test missing")

    for relative in (
        "README.md",
        "docs/DVM_SUPPORT_MATRIX.md",
        "docs/SQL_REFERENCE.md",
        "docs/CONFIGURATION.md",
        "docs/integrations/citus.md",
    ):
        text = (ROOT / relative).read_text(encoding="utf-8")
        require("capability-manifest.json" in text, f"{relative} does not link the manifest")

    roadmap = (ROOT / "roadmap/v0.99.0.md").read_text(encoding="utf-8")
    require("> **Status:** Released" in roadmap, "roadmap release status is stale")
    roadmap_index = (ROOT / "ROADMAP.md").read_text(encoding="utf-8")
    require("[v0.99.0]" in roadmap_index and "✅ Released" in roadmap_index, "roadmap index is stale")

    subprocess.run(
        [sys.executable, str(ROOT / "scripts/generate_capability_manifest.py"), "--check"],
        cwd=ROOT,
        check=True,
    )
    print("v0.99 release gate passed")


if __name__ == "__main__":
    main()
