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

# 3. CI upgrade matrix latest 'to:' must match current version
# The upgrade-e2e-tests job uses a strategy matrix; extract the last 'to:' entry.
CI_TO=$(grep -E '^\s+to:\s+' .github/workflows/ci.yml | sed 's/.*"\([^"]*\)".*/\1/' | tail -1)
if [[ "$CI_TO" == "$VERSION" ]]; then
    echo "  OK  ci.yml upgrade matrix latest to = $CI_TO"
else
    echo "  FAIL ci.yml upgrade matrix latest to is '$CI_TO', expected '$VERSION'"
    PASS=false
fi

# 4. CI upgrade-check chain ends at current version
LAST_CHECK=$(grep 'check_upgrade_completeness.sh' .github/workflows/ci.yml | tail -1)
if echo "$LAST_CHECK" | grep -q "$VERSION"; then
    echo "  OK  ci.yml upgrade-check chain ends at $VERSION"
else
    echo "  FAIL ci.yml upgrade-check chain does not end at $VERSION"
    echo "       Last line: $LAST_CHECK"
    PASS=false
fi

# 5. justfile build-upgrade-image and test-upgrade defaults match
JF_TO=$(grep -E '^(build-upgrade-image|test-upgrade)' justfile | sed 's/.*to="\([^"]*\)".*/\1/' | sort -u)
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
