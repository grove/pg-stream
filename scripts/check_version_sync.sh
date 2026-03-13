#!/usr/bin/env bash
# check_version_sync.sh — verify all version references match Cargo.toml
set -euo pipefail

PASS=true
VERSION=$(cargo metadata --no-deps --format-version 1 | \
    python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])")
echo "Cargo version: $VERSION"

# 1. Archive SQL for the current version must exist
ARCHIVE="sql/archive/pg_trickle--${VERSION}.sql"
if [[ -f "$ARCHIVE" ]]; then
    echo "  OK  archive SQL exists: $ARCHIVE"
else
    echo "  FAIL archive SQL missing: $ARCHIVE"
    echo "       Run: PATH=\"/opt/homebrew/opt/postgresql@18/bin:\$PATH\" cargo pgrx package --features pg18"
    echo "       Then: cp target/release/pg_trickle-pg18/**/*.sql sql/archive/pg_trickle--${VERSION}.sql"
    PASS=false
fi

# 2. Upgrade script from the previous step must exist
UPGRADE_SQL=$(ls sql/pg_trickle--*--${VERSION}.sql 2>/dev/null | head -1)
if [[ -n "$UPGRADE_SQL" ]]; then
    echo "  OK  upgrade script exists: $UPGRADE_SQL"
else
    echo "  FAIL no upgrade script ending in --${VERSION}.sql found in sql/"
    PASS=false
fi

# 3. CI upgrade E2E uses auto-discovery (upgrade-e2e-prepare job with fromJson matrix)
if grep -q 'upgrade-e2e-prepare' .github/workflows/ci.yml && \
   grep -q 'fromJson(needs.upgrade-e2e-prepare.outputs.matrix)' .github/workflows/ci.yml; then
    echo "  OK  ci.yml upgrade matrix latest to = $VERSION"
else
    echo "  FAIL ci.yml upgrade-e2e-tests does not use auto-discovery prepare job"
    PASS=false
fi

# 4. CI upgrade-check uses auto-discovery loop (not hardcoded versions)
if grep -q 'check_upgrade_completeness.sh' .github/workflows/ci.yml && \
   grep -q 'for f in sql/pg_trickle--\*--\*.sql' .github/workflows/ci.yml; then
    echo "  OK  ci.yml upgrade-check chain ends at $VERSION"
else
    echo "  FAIL ci.yml upgrade-check does not use auto-discovery loop"
    PASS=false
fi

# 5. justfile build-upgrade-image and test-upgrade defaults match
# Use word-boundary anchors so test-upgrade-all (which auto-discovers) is excluded.
JF_TO=$(grep -E '^(build-upgrade-image|test-upgrade) ' justfile | sed 's/.*to="\([^"]*\)".*/\1/' | sort -u)
BAD_JF=$(echo "$JF_TO" | grep -v "^${VERSION}$" || true)
if [[ -z "$BAD_JF" ]]; then
    echo "  OK  justfile upgrade defaults = $VERSION"
else
    echo "  FAIL justfile upgrade defaults: $BAD_JF (expected $VERSION)"
    PASS=false
fi

# 6. Test fallback defaults in e2e_upgrade_tests.rs match
BAD_TESTS=$(grep 'unwrap_or(' tests/e2e_upgrade_tests.rs | grep -v "\"${VERSION}\"" || true)
if [[ -z "$BAD_TESTS" ]]; then
    echo "  OK  e2e_upgrade_tests.rs PGS_UPGRADE_TO fallbacks = $VERSION"
else
    echo "  FAIL e2e_upgrade_tests.rs has stale fallback(s):"
    echo "$BAD_TESTS" | sed 's/^/       /'
    PASS=false
fi

if $PASS; then
    echo ""
    echo "All version checks passed for v${VERSION}."
else
    echo ""
    echo "One or more version checks FAILED. Fix the issues above."
    exit 1
fi
