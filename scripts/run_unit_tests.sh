#!/usr/bin/env bash
# scripts/run_unit_tests.sh — Run unit tests with pg_stub preloaded
#
# pgrx extensions reference PostgreSQL server symbols (e.g.
# CurrentMemoryContext, SPI_connect) that are only available inside the
# postgres process.  Pure-Rust unit tests never call those symbols, but the
# test binary still links them.
#
# On macOS 26+ (Tahoe) dyld eagerly resolves all flat-namespace symbols at
# load time, causing an immediate crash.  On newer Linux toolchains the
# linker may use --no-as-needed / -z now, which has the same effect.
#
# Workaround (all platforms):
#   1. Compile a tiny C stub library that provides NULL/no-op definitions
#      for every PostgreSQL symbol the binary references.
#   2. Compile the test binary with `--no-run`.
#   3. Run the binary with LD_PRELOAD (Linux) or DYLD_INSERT_LIBRARIES
#      (macOS) pointing to the stub.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
STUB_SRC="$SCRIPT_DIR/pg_stub.c"
FEATURES="${1:-pg18}"

OS="$(uname)"
case "$OS" in
    Darwin)
        STUB_LIB="$PROJECT_DIR/target/libpg_stub.dylib"
        STUB_CC_FLAGS="-shared -install_name @rpath/libpg_stub.dylib"
        PRELOAD_VAR="DYLD_INSERT_LIBRARIES"
        ;;
    *)
        STUB_LIB="$PROJECT_DIR/target/libpg_stub.so"
        STUB_CC_FLAGS="-shared -fPIC"
        PRELOAD_VAR="LD_PRELOAD"
        ;;
esac

# ── Helper: ensure_stub ───────────────────────────────────────────────────
ensure_stub() {
    if [[ ! -f "$STUB_LIB" ]] || [[ "$STUB_SRC" -nt "$STUB_LIB" ]]; then
        echo "Building $(basename "$STUB_LIB") ..."
        mkdir -p "$(dirname "$STUB_LIB")"
        # shellcheck disable=SC2086
        cc $STUB_CC_FLAGS -o "$STUB_LIB" "$STUB_SRC" 2>&1
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────
cd "$PROJECT_DIR"

ensure_stub

# Compile test binary without running it and capture the executable path.
echo "Compiling unit tests ..."
CARGO_OUTPUT=$(cargo test --lib --features "$FEATURES" --no-run 2>&1)
echo "$CARGO_OUTPUT"

# Extract the binary path from cargo output.
# cargo prints: "Executable unittests src/lib.rs (target/debug/deps/pg_trickle-HASH)"
TEST_BIN=$(echo "$CARGO_OUTPUT" \
           | grep -oE 'target/debug/deps/pg_trickle-[a-f0-9]+' \
           | head -1)

if [[ -n "$TEST_BIN" ]]; then
    TEST_BIN="$PROJECT_DIR/$TEST_BIN"
fi

if [[ -z "${TEST_BIN:-}" ]] || [[ ! -x "$TEST_BIN" ]]; then
    # Fallback: pick the newest executable pg_trickle- binary
    if [[ "$OS" == "Darwin" ]]; then
        TEST_BIN=$(find "$PROJECT_DIR/target/debug/deps" \
                        -maxdepth 1 -name 'pg_trickle-*' -type f -perm +111 \
                        2>/dev/null \
                   | xargs ls -t 2>/dev/null \
                   | head -1)
    else
        TEST_BIN=$(find "$PROJECT_DIR/target/debug/deps" \
                        -maxdepth 1 -name 'pg_trickle-*' -type f -executable \
                        2>/dev/null \
                   | xargs ls -t 2>/dev/null \
                   | head -1)
    fi
fi

if [[ -z "${TEST_BIN:-}" ]]; then
    echo "ERROR: Could not find the test binary in target/debug/deps/" >&2
    exit 1
fi

echo "Running: $(basename "$TEST_BIN") (with $(basename "$STUB_LIB"))"
export "$PRELOAD_VAR"="$STUB_LIB"
"$TEST_BIN" "${@:2}"
