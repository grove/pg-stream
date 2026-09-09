#!/usr/bin/env python3
"""Keep the stable v0.96 operational error catalog documented."""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "src/api/release_096.rs"
DOCS = ROOT / "docs/ERRORS.md"

source = SOURCE.read_text(encoding="utf-8")
catalog = source[source.index("pub fn error_catalog()"):source.index("/// Return the v0.96")]
entries = re.findall(
    r'\(\s*"([A-Z][A-Z0-9_]+)"\.to_string\(\),\s*"([0-9A-Z]{5})"\.to_string\(\),',
    catalog,
)
docs = DOCS.read_text(encoding="utf-8")

errors = []
if not entries:
    errors.append("error_catalog() has no stable entries")
for error_id, sqlstate in entries:
    if f"`{error_id}`" not in docs:
        errors.append(f"{error_id} is missing from docs/ERRORS.md")
    if f"`{sqlstate}`" not in docs:
        errors.append(f"{error_id} SQLSTATE {sqlstate} is missing from docs/ERRORS.md")

if errors:
    for error in errors:
        print(f"ERROR: {error}")
    sys.exit(1)

print(f"stable error identifier check passed ({len(entries)} entries)")
