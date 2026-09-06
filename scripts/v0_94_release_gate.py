#!/usr/bin/env python3
"""Offline release gate for strict transactional graph refresh."""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
VERSION = "0.94.0"

def main() -> int:
    errors = []
    required = (
        "roadmap/v0.94.0.md",
        "sql/pg_trickle--0.93.0--0.94.0.sql",
        "sql/archive/pg_trickle--0.94.0.sql",
    )
    for path in required:
        if not (ROOT / path).is_file():
            errors.append(f"missing v0.94 artifact: {path}")
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    if f'version = "{VERSION}"' not in cargo:
        errors.append(f"Cargo.toml does not declare {VERSION}")
    source = (ROOT / "src/api/integration.rs").read_text(encoding="utf-8")
    for marker in ("refresh_graph_strict", "PGT_EXT_CONTRACT_MISMATCH", "source_boundary"):
        if marker not in source:
            errors.append(f"integration API missing marker: {marker}")
    migration = ROOT / "sql/pg_trickle--0.93.0--0.94.0.sql"
    if migration.is_file() and "refresh_graph_strict" not in migration.read_text(encoding="utf-8"):
        errors.append("migration does not install refresh_graph_strict")
    tests = "\n".join(p.read_text(encoding="utf-8") for p in (ROOT / "src").rglob("*.rs"))
    for name in ("test_graph_contract_canonicalizes_closure_and_order", "test_external_graph_authorization_fails_closed"):
        if not re.search(rf"\bfn\s+{name}\b", tests):
            errors.append(f"missing graph contract test: {name}")
    if errors:
        print("v0_94_release_gate: FAILED")
        print("\n".join(f"- {error}" for error in errors))
        return 1
    print("v0_94_release_gate: passed")
    return 0

if __name__ == "__main__":
    sys.exit(main())
