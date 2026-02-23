#!/usr/bin/env bash
# =============================================================================
# Code Coverage Script for pg_stream
#
# Generates code coverage reports for unit tests using cargo-llvm-cov.
#
# Usage:
#   ./scripts/coverage.sh              # HTML + LCOV report (default)
#   ./scripts/coverage.sh --html       # HTML report only
#   ./scripts/coverage.sh --lcov       # LCOV report only (for CI upload)
#   ./scripts/coverage.sh --text       # Terminal summary only
#
# Prerequisites:
#   - Rust nightly or stable with llvm-tools-preview component
#   - cargo-llvm-cov (installed automatically if missing)
#
# Output:
#   coverage/html/       — HTML report (open coverage/html/index.html)
#   coverage/lcov.info   — LCOV report (for Codecov / Coveralls upload)
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COVERAGE_DIR="${PROJECT_ROOT}/coverage"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info()  { echo -e "${GREEN}[coverage]${NC} $*"; }
warn()  { echo -e "${YELLOW}[coverage]${NC} $*"; }
error() { echo -e "${RED}[coverage]${NC} $*" >&2; }

# ── Parse arguments ──────────────────────────────────────────────────────
FORMAT="all"  # default: generate both HTML and LCOV
while [[ $# -gt 0 ]]; do
    case "$1" in
        --html) FORMAT="html"; shift ;;
        --lcov) FORMAT="lcov"; shift ;;
        --text) FORMAT="text"; shift ;;
        --e2e)  FORMAT="e2e"; shift ;;
        --combined) FORMAT="combined"; shift ;;
        --help|-h)
            head -20 "$0" | grep '^#' | sed 's/^# \?//'
            exit 0
            ;;
        *)
            error "Unknown option: $1"
            exit 1
            ;;
    esac
done

cd "${PROJECT_ROOT}"

# ── Check / install prerequisites ────────────────────────────────────────
info "Checking prerequisites..."

# Ensure llvm-tools-preview component is installed
if ! rustup component list --installed | grep -q llvm-tools; then
    info "Installing llvm-tools-preview..."
    rustup component add llvm-tools-preview
fi

# Ensure cargo-llvm-cov is installed
if ! command -v cargo-llvm-cov &>/dev/null; then
    info "Installing cargo-llvm-cov..."
    cargo install cargo-llvm-cov
fi

# ── Delegate to e2e-coverage.sh for E2E / combined modes ────────────────
if [[ "${FORMAT}" == "e2e" ]]; then
    info "Delegating to e2e-coverage.sh..."
    exec "${SCRIPT_DIR}/e2e-coverage.sh"
fi

if [[ "${FORMAT}" == "combined" ]]; then
    info "Delegating to e2e-coverage.sh (combined unit + E2E)..."
    exec "${SCRIPT_DIR}/e2e-coverage.sh"
fi

# ── Clean previous coverage data ────────────────────────────────────────
info "Cleaning previous coverage data..."
cargo llvm-cov clean --workspace

mkdir -p "${COVERAGE_DIR}"

# ── Ignore patterns for generated / external code ────────────────────────
# Exclude pgrx-generated macros, build scripts, test helpers
IGNORE_REGEX='(pgrx_generated|build\.rs|tests/common)'

# ── Run unit tests with coverage instrumentation ────────────────────────
info "Running unit tests with coverage instrumentation..."

# Common flags for cargo-llvm-cov
COMMON_FLAGS=(
    --features pg18
    --lib
    --ignore-filename-regex "${IGNORE_REGEX}"
)

case "${FORMAT}" in
    html)
        cargo llvm-cov "${COMMON_FLAGS[@]}" \
            --html \
            --output-dir "${COVERAGE_DIR}/html"
        info "HTML report: ${COVERAGE_DIR}/html/index.html"
        ;;
    lcov)
        cargo llvm-cov "${COMMON_FLAGS[@]}" \
            --lcov \
            --output-path "${COVERAGE_DIR}/lcov.info"
        info "LCOV report: ${COVERAGE_DIR}/lcov.info"
        ;;
    text)
        cargo llvm-cov "${COMMON_FLAGS[@]}"
        ;;
    all)
        # Generate LCOV first (for CI), then HTML (for browsing)
        cargo llvm-cov "${COMMON_FLAGS[@]}" \
            --lcov \
            --output-path "${COVERAGE_DIR}/lcov.info"
        info "LCOV report: ${COVERAGE_DIR}/lcov.info"

        cargo llvm-cov "${COMMON_FLAGS[@]}" \
            --html \
            --output-dir "${COVERAGE_DIR}/html" \
            --no-clean
        info "HTML report: ${COVERAGE_DIR}/html/index.html"
        ;;
esac

info "Coverage complete!"

# ── Open HTML report on macOS ────────────────────────────────────────────
if [[ "${FORMAT}" == "html" || "${FORMAT}" == "all" ]] && [[ "$(uname)" == "Darwin" ]]; then
    read -rp "Open HTML report in browser? [y/N] " answer
    if [[ "${answer}" =~ ^[Yy]$ ]]; then
        open "${COVERAGE_DIR}/html/index.html"
    fi
fi
