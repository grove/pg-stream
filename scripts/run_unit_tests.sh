#!/usr/bin/env bash
# scripts/run_unit_tests.sh — Run unit tests on macOS 26+ with pg_stub
#
# macOS 26 (Tahoe) changed dyld to eagerly resolve all flat-namespace symbols
# at binary load time.  pgrx extensions reference PostgreSQL server symbols
# (e.g. CacheMemoryContext, SPI_connect) that are only available inside the
# postgres process.  Pure-Rust unit tests never call those symbols, but the
# test binary still links them — which crashes on load.
#
# Workaround:
#   1. Compile a tiny C stub library (libpg_stub.dylib) that provides
#      NULL/no-op definitions for every PostgreSQL symbol.
#   2. Compile the test binary with `--no-run`.
#   3. Run the binary with DYLD_INSERT_LIBRARIES pointing to the stub.
#
# On Linux (or older macOS where the original lazy binding still works),
# we skip the stub and run `cargo test` directly.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
STUB_SRC="$SCRIPT_DIR/pg_stub.c"
STUB_LIB="$PROJECT_DIR/target/libpg_stub.dylib"
FEATURES="${1:-pg18}"

# ── Helper: needs_stub ────────────────────────────────────────────────────
needs_stub() {
    [[ "$(uname)" == "Darwin" ]] || return 1

    # Detect macOS major version (26 = Tahoe).  Versions ≥ 26 need the stub.
    local macos_ver
    macos_ver="$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)"
    [[ "${macos_ver:-0}" -ge 26 ]]
}

# ── Helper: ensure_stub ───────────────────────────────────────────────────
ensure_stub() {
    if [[ ! -f "$STUB_LIB" ]] || [[ "$STUB_SRC" -nt "$STUB_LIB" ]]; then
        echo "Building libpg_stub.dylib ..."
        mkdir -p "$(dirname "$STUB_LIB")"
        cc -shared -o "$STUB_LIB" "$STUB_SRC" \
           -install_name @rpath/libpg_stub.dylib 2>&1
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────
cd "$PROJECT_DIR"

if needs_stub; then
    ensure_stub

    # Compile test binary without running it and capture the executable path.
    # cargo prints: "Executable unittests src/lib.rs (target/debug/deps/pg_stream-HASH)"
    echo "Compiling unit tests ..."
    CARGO_OUTPUT=$(cargo test --lib --features "$FEATURES" --no-run 2>&1)
    echo "$CARGO_OUTPUT"

    # Extract the binary path from cargo output
    TEST_BIN=$(echo "$CARGO_OUTPUT" \
               | grep -oE 'target/debug/deps/pg_stream-[a-f0-9]+' \
               | head -1)

    if [[ -n "$TEST_BIN" ]]; then
        TEST_BIN="$PROJECT_DIR/$TEST_BIN"
    fi

    if [[ -z "$TEST_BIN" ]] || [[ ! -x "$TEST_BIN" ]]; then
        # Fallback: pick the newest executable pg_stream- binary
        TEST_BIN=$(find "$PROJECT_DIR/target/debug/deps" \
                        -maxdepth 1 -name 'pg_stream-*' -type f -perm +111 \
                        2>/dev/null \
                   | xargs ls -t 2>/dev/null \
                   | head -1)
    fi

    if [[ -z "$TEST_BIN" ]]; then
        echo "ERROR: Could not find the test binary in target/debug/deps/" >&2
        exit 1
    fi

    echo "Running: $(basename "$TEST_BIN") (with libpg_stub.dylib)"
    DYLD_INSERT_LIBRARIES="$STUB_LIB" "$TEST_BIN" "${@:2}"
else
    # Linux or older macOS — standard path
    cargo test --lib --features "$FEATURES" "${@:2}"
fi
