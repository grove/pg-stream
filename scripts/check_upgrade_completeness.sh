#!/usr/bin/env bash
# =============================================================================
# Validate that all SQL objects in the pgrx-generated full install script
# are also covered by the hand-authored upgrade script.
#
# This catches the class of bug where a new #[pg_extern] annotation is added
# in Rust (which pgrx auto-includes in the full install SQL) but the engineer
# forgets to add the corresponding CREATE OR REPLACE FUNCTION to the upgrade
# script — causing ALTER EXTENSION UPDATE to silently skip the function.
#
# Usage:
#   scripts/check_upgrade_completeness.sh <from_version> <to_version>
#   scripts/check_upgrade_completeness.sh 0.1.3 0.2.0
#
# Prerequisites:
#   - cargo pgrx package must have been run (or the generated SQL must exist)
#   - OR: sql/archive/pg_trickle--<from_version>.sql must exist as baseline
#
# Exit code 0 = all objects covered, 1 = missing objects found, 2 = setup error
# =============================================================================
set -uo pipefail

FROM_VERSION="${1:?Usage: $0 <from_version> <to_version>}"
TO_VERSION="${2:?Usage: $0 <from_version> <to_version>}"

# Allow overriding the package output directory (CI vs local may differ)
PKG_DIR="${PG_TRICKLE_PKG_DIR:-}"

UPGRADE_SQL="sql/pg_trickle--${FROM_VERSION}--${TO_VERSION}.sql"
TMPDIR="${TMPDIR:-/tmp}/pg_trickle_upgrade_check.$$"
mkdir -p "$TMPDIR"
trap 'rm -rf "$TMPDIR"' EXIT

# ── Locate the full install SQL for the NEW version ─────────────────────
find_full_sql() {
    local version="$1"

    # 1. Try the pgrx package output (standard cargo pgrx package location)
    if [[ -n "$PKG_DIR" ]]; then
        local candidate="$PKG_DIR/pg_trickle--${version}.sql"
        [[ -f "$candidate" ]] && echo "$candidate" && return
    fi

    # 2. Search target/ for the generated SQL
    local found
    found=$(find target/ -name "pg_trickle--${version}.sql" -type f 2>/dev/null | head -1)
    if [[ -n "$found" ]]; then echo "$found"; return; fi

    # 3. Check sql/archive/
    if [[ -f "sql/archive/pg_trickle--${version}.sql" ]]; then
        echo "sql/archive/pg_trickle--${version}.sql"
        return
    fi

    return 1
}

FULL_SQL_NEW=$(find_full_sql "$TO_VERSION") || {
    echo "ERROR: Cannot find full install script for version ${TO_VERSION}."
    echo "       Run 'cargo pgrx package' first, or place it in sql/archive/."
    exit 2
}
echo "Full install SQL (new): $FULL_SQL_NEW"

# ── Locate the full install SQL for the OLD version (baseline) ──────────
FULL_SQL_OLD=$(find_full_sql "$FROM_VERSION" 2>/dev/null) || true
if [[ -n "$FULL_SQL_OLD" ]]; then
    echo "Full install SQL (old): $FULL_SQL_OLD"
else
    echo "INFO: No baseline SQL found for ${FROM_VERSION}."
    echo "      Will check that ALL functions in the new install SQL"
    echo "      are present in the upgrade script."
fi

# ── Validate upgrade script exists ──────────────────────────────────────
if [[ ! -f "$UPGRADE_SQL" ]]; then
    echo "ERROR: Upgrade script not found: $UPGRADE_SQL"
    exit 2
fi
echo "Upgrade script:         $UPGRADE_SQL"
echo ""

ERRORS=0

# ── Helper: extract function names from a SQL file ──────────────────────
# Handles both "CREATE FUNCTION" and "CREATE OR REPLACE FUNCTION"
# with optional extra whitespace (pgrx generates double spaces sometimes)
extract_functions() {
    local sqlfile="$1"
    # Match: CREATE [OR REPLACE] FUNCTION pgtrickle."name"
    grep -oE 'CREATE\s+(OR\s+REPLACE\s+)?FUNCTION\s+pgtrickle\."[^"]+"' "$sqlfile" 2>/dev/null \
        | sed -E 's/.*pgtrickle\."([^"]+)".*/\1/' \
        | sort -u
}

# ── Helper: extract view names from a SQL file ─────────────────────────
extract_views() {
    local sqlfile="$1"
    grep -oE 'CREATE\s+(OR\s+REPLACE\s+)?VIEW\s+pgtrickle\.[a-z_]+' "$sqlfile" 2>/dev/null \
        | sed -E 's/.*pgtrickle\.([a-z_]+)/\1/' \
        | sort -u
}

# ── Helper: extract event trigger names from a SQL file ─────────────────
extract_event_triggers() {
    local sqlfile="$1"
    grep -oE 'CREATE\s+EVENT\s+TRIGGER\s+[a-z_]+' "$sqlfile" 2>/dev/null \
        | sed -E 's/CREATE\s+EVENT\s+TRIGGER\s+//' \
        | sort -u
}

# ── Helper: extract table names from a SQL file ─────────────────────────
extract_tables() {
    local sqlfile="$1"
    grep -oE 'CREATE\s+TABLE\s+(IF\s+NOT\s+EXISTS\s+)?pgtrickle\.[a-z_]+' "$sqlfile" 2>/dev/null \
        | sed -E 's/.*pgtrickle\.([a-z_]+)/\1/' \
        | sort -u
}

# ── Helper: extract column names for a named pgtrickle table ────────────
# Prints one lowercase column name per line from the CREATE TABLE block.
# Uses grep -n to locate the block start, then awk to parse until ");".
extract_table_columns_for() {
    local sqlfile="$1"
    local tablename="$2"
    local lineno
    lineno=$(grep -n "CREATE TABLE.*pgtrickle\.$tablename" "$sqlfile" 2>/dev/null | head -1 | cut -d: -f1)
    [[ -z "$lineno" ]] && return
    tail -n +"$((lineno + 1))" "$sqlfile" | head -200 | awk '
        /^[[:space:]]*\);/ { exit }
        /^[[:space:]]+[A-Za-z_]/ {
            gsub(/^[[:space:]]+/, "")
            split($0, parts, /[[:space:]]/)
            col = tolower(parts[1])
            if (col != "constraint" && col != "check" && col != "primary" &&
                col != "foreign" && col != "unique" && col != "exclude") {
                print col
            }
        }
    ' | sort -u
}

# ── Helper: extract index names from a SQL file ─────────────────────────
extract_indexes() {
    local sqlfile="$1"
    grep -oE 'CREATE\s+(UNIQUE\s+)?INDEX\s+(IF\s+NOT\s+EXISTS\s+)?[a-z_][a-z0-9_]*' "$sqlfile" 2>/dev/null \
        | sed -E 's/CREATE\s+(UNIQUE\s+)?INDEX\s+(IF\s+NOT\s+EXISTS\s+)?//' \
        | sort -u
}

# ═══════════════════════════════════════════════════════════════════════
# CHECK 1: Functions
# ═══════════════════════════════════════════════════════════════════════
echo "━━━ Check 1: SQL Functions ━━━"

extract_functions "$FULL_SQL_NEW" > "$TMPDIR/new_functions.txt"
extract_functions "$UPGRADE_SQL"  > "$TMPDIR/upgrade_functions.txt"

if [[ -n "$FULL_SQL_OLD" ]]; then
    extract_functions "$FULL_SQL_OLD" > "$TMPDIR/old_functions.txt"
else
    : > "$TMPDIR/old_functions.txt"
fi

# Functions that are NEW (in new full SQL but not in old full SQL)
comm -23 "$TMPDIR/new_functions.txt" "$TMPDIR/old_functions.txt" > "$TMPDIR/added_functions.txt"

# New functions that are MISSING from the upgrade script
comm -23 "$TMPDIR/added_functions.txt" "$TMPDIR/upgrade_functions.txt" > "$TMPDIR/missing_functions.txt"

NEW_FUNC_COUNT=$(wc -l < "$TMPDIR/added_functions.txt" | tr -d ' ')
MISSING_FUNC_COUNT=$(wc -l < "$TMPDIR/missing_functions.txt" | tr -d ' ')

if [[ "$MISSING_FUNC_COUNT" -gt 0 ]]; then
    echo "  ERROR: ${MISSING_FUNC_COUNT} new function(s) missing from upgrade script:"
    while IFS= read -r fn; do
        echo "    - pgtrickle.\"${fn}\""
    done < "$TMPDIR/missing_functions.txt"
    ERRORS=$((ERRORS + MISSING_FUNC_COUNT))
else
    echo "  OK: ${NEW_FUNC_COUNT} new function(s) all covered."
fi

# ═══════════════════════════════════════════════════════════════════════
# CHECK 2: Views
# ═══════════════════════════════════════════════════════════════════════
echo "━━━ Check 2: Views ━━━"

extract_views "$FULL_SQL_NEW" > "$TMPDIR/new_views.txt"
extract_views "$UPGRADE_SQL"  > "$TMPDIR/upgrade_views.txt"

if [[ -n "$FULL_SQL_OLD" ]]; then
    extract_views "$FULL_SQL_OLD" > "$TMPDIR/old_views.txt"
else
    : > "$TMPDIR/old_views.txt"
fi

comm -23 "$TMPDIR/new_views.txt" "$TMPDIR/old_views.txt" > "$TMPDIR/added_views.txt"
comm -23 "$TMPDIR/added_views.txt" "$TMPDIR/upgrade_views.txt" > "$TMPDIR/missing_views.txt"

NEW_VIEW_COUNT=$(wc -l < "$TMPDIR/added_views.txt" | tr -d ' ')
MISSING_VIEW_COUNT=$(wc -l < "$TMPDIR/missing_views.txt" | tr -d ' ')

if [[ "$MISSING_VIEW_COUNT" -gt 0 ]]; then
    echo "  ERROR: ${MISSING_VIEW_COUNT} new view(s) missing from upgrade script:"
    while IFS= read -r v; do
        echo "    - pgtrickle.${v}"
    done < "$TMPDIR/missing_views.txt"
    ERRORS=$((ERRORS + MISSING_VIEW_COUNT))
else
    echo "  OK: ${NEW_VIEW_COUNT} new view(s) all covered."
fi

# ═══════════════════════════════════════════════════════════════════════
# CHECK 3: Event Triggers
# ═══════════════════════════════════════════════════════════════════════
echo "━━━ Check 3: Event Triggers ━━━"

extract_event_triggers "$FULL_SQL_NEW" > "$TMPDIR/new_triggers.txt"
extract_event_triggers "$UPGRADE_SQL"  > "$TMPDIR/upgrade_triggers.txt"

if [[ -n "$FULL_SQL_OLD" ]]; then
    extract_event_triggers "$FULL_SQL_OLD" > "$TMPDIR/old_triggers.txt"
else
    : > "$TMPDIR/old_triggers.txt"
fi

comm -23 "$TMPDIR/new_triggers.txt" "$TMPDIR/old_triggers.txt" > "$TMPDIR/added_triggers.txt"
comm -23 "$TMPDIR/added_triggers.txt" "$TMPDIR/upgrade_triggers.txt" > "$TMPDIR/missing_triggers.txt"

NEW_TRIG_COUNT=$(wc -l < "$TMPDIR/added_triggers.txt" | tr -d ' ')
MISSING_TRIG_COUNT=$(wc -l < "$TMPDIR/missing_triggers.txt" | tr -d ' ')

if [[ "$MISSING_TRIG_COUNT" -gt 0 ]]; then
    echo "  ERROR: ${MISSING_TRIG_COUNT} new event trigger(s) missing from upgrade script:"
    while IFS= read -r t; do
        echo "    - ${t}"
    done < "$TMPDIR/missing_triggers.txt"
    ERRORS=$((ERRORS + MISSING_TRIG_COUNT))
else
    echo "  OK: ${NEW_TRIG_COUNT} new event trigger(s) all covered."
fi

# ═══════════════════════════════════════════════════════════════════════
# CHECK 4: Tables
# ═══════════════════════════════════════════════════════════════════════
echo "━━━ Check 4: Tables ━━━"

extract_tables "$FULL_SQL_NEW" > "$TMPDIR/new_tables.txt"
extract_tables "$UPGRADE_SQL"  > "$TMPDIR/upgrade_tables.txt"

if [[ -n "$FULL_SQL_OLD" ]]; then
    extract_tables "$FULL_SQL_OLD" > "$TMPDIR/old_tables.txt"
else
    : > "$TMPDIR/old_tables.txt"
fi

comm -23 "$TMPDIR/new_tables.txt" "$TMPDIR/old_tables.txt" > "$TMPDIR/added_tables.txt"
comm -23 "$TMPDIR/added_tables.txt" "$TMPDIR/upgrade_tables.txt" > "$TMPDIR/missing_tables.txt"

NEW_TABLE_COUNT=$(wc -l < "$TMPDIR/added_tables.txt" | tr -d ' ')
MISSING_TABLE_COUNT=$(wc -l < "$TMPDIR/missing_tables.txt" | tr -d ' ')

if [[ "$MISSING_TABLE_COUNT" -gt 0 ]]; then
    echo "  ERROR: ${MISSING_TABLE_COUNT} new table(s) missing from upgrade script:"
    while IFS= read -r table; do
        echo "    - pgtrickle.${table}"
    done < "$TMPDIR/missing_tables.txt"
    ERRORS=$((ERRORS + MISSING_TABLE_COUNT))
else
    echo "  OK: ${NEW_TABLE_COUNT} new table(s) all covered."
fi

# ═══════════════════════════════════════════════════════════════════════
# CHECK 5: Indexes
# ═══════════════════════════════════════════════════════════════════════
echo "━━━ Check 5: Indexes ━━━"

extract_indexes "$FULL_SQL_NEW" > "$TMPDIR/new_indexes.txt"
extract_indexes "$UPGRADE_SQL"  > "$TMPDIR/upgrade_indexes.txt"

if [[ -n "$FULL_SQL_OLD" ]]; then
    extract_indexes "$FULL_SQL_OLD" > "$TMPDIR/old_indexes.txt"
else
    : > "$TMPDIR/old_indexes.txt"
fi

comm -23 "$TMPDIR/new_indexes.txt" "$TMPDIR/old_indexes.txt" > "$TMPDIR/added_indexes.txt"
comm -23 "$TMPDIR/added_indexes.txt" "$TMPDIR/upgrade_indexes.txt" > "$TMPDIR/missing_indexes.txt"

NEW_INDEX_COUNT=$(wc -l < "$TMPDIR/added_indexes.txt" | tr -d ' ')
MISSING_INDEX_COUNT=$(wc -l < "$TMPDIR/missing_indexes.txt" | tr -d ' ')

if [[ "$MISSING_INDEX_COUNT" -gt 0 ]]; then
    echo "  ERROR: ${MISSING_INDEX_COUNT} new index(es) missing from upgrade script:"
    while IFS= read -r idx; do
        echo "    - ${idx}"
    done < "$TMPDIR/missing_indexes.txt"
    ERRORS=$((ERRORS + MISSING_INDEX_COUNT))
else
    echo "  OK: ${NEW_INDEX_COUNT} new index(es) all covered."
fi

# ═══════════════════════════════════════════════════════════════════════
# CHECK 6: Column drift in existing tables
# Detect new columns added to existing pgtrickle tables that are not
# covered by ALTER TABLE ... ADD COLUMN in the upgrade script.
# ═══════════════════════════════════════════════════════════════════════
echo "━━━ Check 6: Column drift in existing tables ━━━"

: > "$TMPDIR/missing_columns.txt"
MISSING_COL_COUNT=0
NEW_COL_TOTAL=0

# All ADD COLUMN column-names mentioned in the upgrade script
# Extract the trailing identifier (the column name) from each match
grep -oiE 'ADD COLUMN (IF NOT EXISTS )?[a-z_][a-z0-9_]*' "$UPGRADE_SQL" 2>/dev/null \
    | grep -oE '[a-zA-Z_][a-zA-Z0-9_]*$' \
    | tr '[:upper:]' '[:lower:]' \
    | sort -u > "$TMPDIR/upgrade_add_columns.txt"

if [[ -n "$FULL_SQL_OLD" ]]; then
    while IFS= read -r table; do
        # Only check tables present in the OLD SQL too (existing tables, not new ones)
        if grep -q "pgtrickle\.$table" "$FULL_SQL_OLD" 2>/dev/null; then
            extract_table_columns_for "$FULL_SQL_NEW" "$table" > "$TMPDIR/cols_new_${table}.txt"
            extract_table_columns_for "$FULL_SQL_OLD" "$table" > "$TMPDIR/cols_old_${table}.txt"
            comm -23 "$TMPDIR/cols_new_${table}.txt" "$TMPDIR/cols_old_${table}.txt" > "$TMPDIR/cols_added_${table}.txt"
            while IFS= read -r col; do
                NEW_COL_TOTAL=$((NEW_COL_TOTAL + 1))
                if ! grep -qx "$col" "$TMPDIR/upgrade_add_columns.txt"; then
                    echo "pgtrickle.${table}.${col}" >> "$TMPDIR/missing_columns.txt"
                    MISSING_COL_COUNT=$((MISSING_COL_COUNT + 1))
                fi
            done < "$TMPDIR/cols_added_${table}.txt"
        fi
    done < <(extract_tables "$FULL_SQL_NEW")
fi

if [[ "$MISSING_COL_COUNT" -gt 0 ]]; then
    echo "  ERROR: ${MISSING_COL_COUNT} new column(s) missing ADD COLUMN in upgrade script:"
    while IFS= read -r entry; do
        echo "    - ${entry}"
    done < "$TMPDIR/missing_columns.txt"
    ERRORS=$((ERRORS + MISSING_COL_COUNT))
else
    echo "  OK: ${NEW_COL_TOTAL} new column(s) in existing tables all covered."
fi

# ═══════════════════════════════════════════════════════════════════════
# Summary
# ═══════════════════════════════════════════════════════════════════════
echo ""
if [[ "$ERRORS" -gt 0 ]]; then
    echo "FAILED: ${ERRORS} object(s) missing from upgrade script."
    echo ""
    echo "Fix: Add the missing CREATE OR REPLACE statements to:"
    echo "     ${UPGRADE_SQL}"
    echo ""
    echo "See plans/sql/PLAN_UPGRADE_MIGRATIONS.md §11 for the full checklist."
    exit 1
else
    echo "PASSED: All new SQL objects are covered by the upgrade script."
    echo "  Functions:      $(wc -l < "$TMPDIR/new_functions.txt" | tr -d ' ') total (${NEW_FUNC_COUNT} new)"
    echo "  Views:          $(wc -l < "$TMPDIR/new_views.txt" | tr -d ' ') total (${NEW_VIEW_COUNT} new)"
    echo "  Event triggers: $(wc -l < "$TMPDIR/new_triggers.txt" | tr -d ' ') total (${NEW_TRIG_COUNT} new)"
    echo "  Tables:         $(wc -l < "$TMPDIR/new_tables.txt" | tr -d ' ') total (${NEW_TABLE_COUNT} new)"
    echo "  Indexes:        $(wc -l < "$TMPDIR/new_indexes.txt" | tr -d ' ') total (${NEW_INDEX_COUNT} new)"
    echo "  Column drift:   ${NEW_COL_TOTAL} new column(s) in existing tables, all covered."
    exit 0
fi
