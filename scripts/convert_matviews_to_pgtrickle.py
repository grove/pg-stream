#!/usr/bin/env python3
"""Convert CREATE/REFRESH MATERIALIZED VIEW statements to pg_trickle stream tables.

Each materialized view becomes a stream table with the same name.
Indexes are kept as-is; REFRESH calls become refresh_stream_table().
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

_IDENT = r'(?:"[^"]+"|[A-Za-z_]\w*)'


def _esc(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def _dollar_tag(query: str) -> str:
    tag = "$pgtrickle_q$"
    n = 0
    while tag in query:
        n += 1
        tag = f"$pgtrickle_q_{n}$"
    return tag


def _unquote(name: str) -> str:
    """Strip SQL double-quotes: '"foo"' -> 'foo', 'bar' -> 'bar'."""
    if name.startswith('"') and name.endswith('"'):
        return name[1:-1]
    return name


# ── The three regexes ───────────────────────────────────────────────────────

_CREATE_RE = re.compile(
    r"(CREATE\s+(?:OR\s+REPLACE\s+)?MATERIALIZED\s+VIEW\s+"
    r"(?:IF\s+NOT\s+EXISTS\s+)?)"
    rf"({_IDENT})"
    r"\s+AS\s+"
    r"(.*?)"
    r"(?:\s+WITH\s+(NO\s+)?DATA)?"
    r"\s*;",
    re.IGNORECASE | re.DOTALL,
)

_REFRESH_RE = re.compile(
    r"REFRESH\s+MATERIALIZED\s+VIEW\s+(?:CONCURRENTLY\s+)?"
    rf"({_IDENT})"
    r"(?:\s+WITH\s+(?:NO\s+)?DATA)?\s*;",
    re.IGNORECASE | re.DOTALL,
)


# ── Conversion: three re.sub passes ────────────────────────────────────────

def convert(sql: str) -> tuple[str, int, int]:
    creates = refreshes = 0

    def on_create(m: re.Match) -> str:
        nonlocal creates
        creates += 1
        header, name, query, no_data = m.group(1), m.group(2), m.group(3).rstrip(), bool(m.group(4))
        upper = header.upper()
        if "OR REPLACE" in upper:
            fn = "create_or_replace_stream_table"
        elif "IF NOT EXISTS" in upper:
            fn = "create_stream_table_if_not_exists"
        else:
            fn = "create_stream_table"

        plain = _unquote(name)
        tag = _dollar_tag(query)

        lines = [f"SELECT pgtrickle.{fn}(", f"    name => {_esc(plain)},", f"    query => {tag}", query]
        if no_data:
            lines += [f"{tag},", "    initialize => false"]
        else:
            lines.append(tag)
        lines.append(");")
        return "\n".join(lines)

    def on_refresh(m: re.Match) -> str:
        nonlocal refreshes
        refreshes += 1
        name = m.group(1)
        plain = _unquote(name)
        return f"SELECT pgtrickle.refresh_stream_table({_esc(plain)});"

    sql = _CREATE_RE.sub(on_create, sql)
    sql = _REFRESH_RE.sub(on_refresh, sql)
    return sql, creates, refreshes


# ── CLI ─────────────────────────────────────────────────────────────────────

def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("input", type=Path, help="Input SQL file.")
    p.add_argument("-o", "--output", type=Path, help="Output file (default: stdout).")
    p.add_argument("--in-place", action="store_true", help="Overwrite input file.")
    args = p.parse_args()

    if args.in_place and args.output:
        print("Error: --in-place and --output are mutually exclusive.", file=sys.stderr)
        return 2
    if not args.input.exists():
        print(f"Error: {args.input} not found.", file=sys.stderr)
        return 1

    result, c, r = convert(args.input.read_text("utf-8"))
    if args.in_place:
        args.input.write_text(result, "utf-8")
    elif args.output:
        args.output.write_text(result, "utf-8")
    else:
        sys.stdout.write(result)

    print(f"Converted {c} CREATE and {r} REFRESH MATERIALIZED VIEW statement(s).", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
