#!/usr/bin/env python3
"""Verify a PGXN zip archive contains required files."""

from __future__ import annotations

import sys
import zipfile
from pathlib import PurePosixPath


def main() -> int:
    if len(sys.argv) != 2:
        print("Usage: verify_pgxn_archive.py <archive.zip>", file=sys.stderr)
        return 2

    archive = sys.argv[1]

    try:
        with zipfile.ZipFile(archive) as zf:
            names = zf.namelist()
    except FileNotFoundError:
        print(f"Error: archive not found: {archive}", file=sys.stderr)
        return 1
    except zipfile.BadZipFile:
        print(f"Error: invalid zip archive: {archive}", file=sys.stderr)
        return 1

    # git archive adds a versioned prefix (e.g. pg_trickle-0.9.0/META.json).
    has_meta = any(PurePosixPath(name).name == "META.json" for name in names)

    if not has_meta:
        print("Error: META.json not found in the archive.")
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
