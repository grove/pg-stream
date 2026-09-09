#!/usr/bin/env python3
"""Fail closed when the v0.97 release surfaces drift apart."""

import json
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.97.0"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"v0.97 release gate failed: {message}")


def main() -> None:
    with (ROOT / "Cargo.toml").open("rb") as handle:
        cargo_version = tomllib.load(handle)["package"]["version"]
    require(cargo_version == VERSION, f"Cargo.toml is {cargo_version}")

    meta = json.loads((ROOT / "META.json").read_text(encoding="utf-8"))
    require(meta["version"] == VERSION, "META.json top-level version drift")
    require(meta["provides"]["pg_trickle"]["version"] == VERSION, "META.json provides version drift")
    require((ROOT / f"sql/archive/pg_trickle--{VERSION}.sql").is_file(), "full SQL archive missing")
    require((ROOT / "sql/pg_trickle--0.96.0--0.97.0.sql").is_file(), "upgrade migration missing")

    manifest = json.loads(
        (ROOT / "docs/upgrade-support-manifest.json").read_text(encoding="utf-8")
    )
    require(manifest["latest_released_source_version"] == VERSION, "upgrade manifest is stale")
    require(VERSION in manifest["supported_source_versions"], "release absent from support manifest")
    roadmap = (ROOT / "roadmap/v0.97.0.md").read_text(encoding="utf-8")
    require("> **Status:** Released" in roadmap, "roadmap release status is stale")
    roadmap_index = (ROOT / "ROADMAP.md").read_text(encoding="utf-8")
    require("[v0.97.0]" in roadmap_index and "✅ Released" in roadmap_index, "roadmap index is stale")

    subprocess.run(
        [sys.executable, str(ROOT / "scripts/check_monitoring_contract.py")],
        cwd=ROOT,
        check=True,
    )
    print("v0.97 release gate passed")


if __name__ == "__main__":
    main()
