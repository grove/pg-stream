#!/usr/bin/env bash
# =============================================================================
# E2E Coverage Script for pg_trickle
#
# Builds a coverage-instrumented Docker image, runs E2E tests against it,
# extracts profraw files, and generates combined unit + E2E coverage reports.
#
# Usage:
#   ./scripts/e2e-coverage.sh              # Full pipeline (build + test + report)
#   ./scripts/e2e-coverage.sh --skip-build # Reuse existing coverage image
#   ./scripts/e2e-coverage.sh --skip-test  # Only merge/report (profraw already extracted)
#
# Prerequisites:
#   - Docker
#   - cargo-llvm-cov
#   - llvm-tools-preview rustup component
#
# Output:
#   coverage/e2e/                — E2E profraw files
#   coverage/e2e/merged.profdata — Merged E2E profdata
#   coverage/combined/           — Combined unit + E2E report
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
COVERAGE_DIR="${PROJECT_ROOT}/coverage"
E2E_COV_DIR="${COVERAGE_DIR}/e2e"
COMBINED_DIR="${COVERAGE_DIR}/combined"
COV_IMAGE="pg_trickle_e2e_cov:latest"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info()  { echo -e "${GREEN}[e2e-cov]${NC} $*"; }
warn()  { echo -e "${YELLOW}[e2e-cov]${NC} $*"; }
error() { echo -e "${RED}[e2e-cov]${NC} $*" >&2; }

# ── Parse arguments ──────────────────────────────────────────────────────
SKIP_BUILD=false
SKIP_TEST=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=true; shift ;;
        --skip-test)  SKIP_TEST=true; shift ;;
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

# ── Check prerequisites ─────────────────────────────────────────────────
info "Checking prerequisites..."

if ! command -v docker &>/dev/null; then
    error "Docker is required. Install it from https://docker.com"
    exit 1
fi

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

# ── Step 1: Build coverage-instrumented Docker image ────────────────────
if [[ "${SKIP_BUILD}" == "false" ]]; then
    info "Building coverage-instrumented Docker image..."
    docker build \
        -t "${COV_IMAGE}" \
        -f tests/Dockerfile.e2e-coverage \
        "${PROJECT_ROOT}"
    info "Image built: ${COV_IMAGE}"
else
    info "Skipping Docker build (--skip-build)"
    if ! docker image inspect "${COV_IMAGE}" &>/dev/null; then
        error "Coverage image '${COV_IMAGE}' not found. Run without --skip-build first."
        exit 1
    fi
fi

# ── Step 2: Run E2E tests with coverage image ───────────────────────────
if [[ "${SKIP_TEST}" == "false" ]]; then
    # Create the profraw output directory
    mkdir -p "${E2E_COV_DIR}"
    rm -f "${E2E_COV_DIR}"/*.profraw

    info "Running E2E tests against coverage-instrumented image..."
    info "  PGS_E2E_IMAGE=${COV_IMAGE}"
    info "  PGS_E2E_COVERAGE_DIR=${E2E_COV_DIR}"

    # The E2eDb harness reads these env vars:
    # - PGS_E2E_IMAGE: overrides the Docker image name
    # - PGS_E2E_COVERAGE_DIR: enables /coverage volume mount
    PGS_E2E_IMAGE="${COV_IMAGE}" \
    PGS_E2E_COVERAGE_DIR="${E2E_COV_DIR}" \
        cargo test --test 'e2e_*' --features pg18 -- --test-threads=1

    # Count profraw files
    PROFRAW_COUNT=$(find "${E2E_COV_DIR}" -name '*.profraw' 2>/dev/null | wc -l | tr -d ' ')
    info "Collected ${PROFRAW_COUNT} profraw files from E2E tests"

    if [[ "${PROFRAW_COUNT}" -eq 0 ]]; then
        warn "No profraw files collected! Coverage instrumentation may not be working."
        warn "Check that the Docker image was built with RUSTFLAGS='-C instrument-coverage'"
        exit 1
    fi
else
    info "Skipping E2E test run (--skip-test)"
fi

# ── Step 3: Merge profraw → profdata ────────────────────────────────────
info "Merging profraw files..."

# Find the host llvm-profdata binary (from rustup's llvm-tools-preview)
LLVM_PROFDATA=""
SYSROOT="$(rustc --print sysroot 2>/dev/null || true)"
if [[ -n "${SYSROOT}" ]]; then
    LLVM_PROFDATA="$(find "${SYSROOT}" -name llvm-profdata -type f 2>/dev/null | head -1)"
fi

if [[ -z "${LLVM_PROFDATA}" ]]; then
    # Fallback: check PATH
    LLVM_PROFDATA="$(command -v llvm-profdata 2>/dev/null || true)"
fi

if [[ -z "${LLVM_PROFDATA}" ]]; then
    error "llvm-profdata not found. Install with: rustup component add llvm-tools-preview"
    exit 1
fi

info "Using llvm-profdata: ${LLVM_PROFDATA}"

# Merge E2E profraw files into a single profdata
"${LLVM_PROFDATA}" merge -sparse \
    "${E2E_COV_DIR}"/*.profraw \
    -o "${E2E_COV_DIR}/merged.profdata"

info "E2E profdata: ${E2E_COV_DIR}/merged.profdata"

# ── Step 4: Generate unit test profdata ─────────────────────────────────
info "Running unit tests with coverage instrumentation..."

# Use cargo-llvm-cov to run unit tests and export profdata
IGNORE_REGEX='(pgrx_generated|build\.rs|tests/common)'

cargo llvm-cov clean --workspace
cargo llvm-cov --no-report \
    --features pg18 \
    --lib \
    --ignore-filename-regex "${IGNORE_REGEX}"

# Find the unit test profdata generated by cargo-llvm-cov
UNIT_PROFDATA="$(find "${PROJECT_ROOT}/target" -name '*.profdata' -newer "${PROJECT_ROOT}/Cargo.toml" 2>/dev/null | head -1)"

if [[ -z "${UNIT_PROFDATA}" ]]; then
    warn "Could not locate unit test profdata. Generating E2E-only report."
    UNIT_PROFDATA=""
fi

# ── Step 5: Merge unit + E2E profdata ───────────────────────────────────
mkdir -p "${COMBINED_DIR}"

MERGE_INPUTS=("${E2E_COV_DIR}/merged.profdata")
if [[ -n "${UNIT_PROFDATA}" ]]; then
    MERGE_INPUTS+=("${UNIT_PROFDATA}")
    info "Merging unit + E2E profdata..."
else
    info "Using E2E profdata only..."
fi

"${LLVM_PROFDATA}" merge -sparse \
    "${MERGE_INPUTS[@]}" \
    -o "${COMBINED_DIR}/combined.profdata"

info "Combined profdata: ${COMBINED_DIR}/combined.profdata"

# ── Step 6: Generate combined coverage report ───────────────────────────
info "Generating combined coverage reports..."

# Find the .so / .dylib built with coverage instrumentation.
# cargo-llvm-cov builds the instrumented binary in its own target dir;
# we need the one from the unit test run (host-side instrumented library).
INSTRUMENTED_LIB="$(find "${PROJECT_ROOT}/target" -name 'libpg_trickle*.so' -o -name 'libpg_trickle*.dylib' 2>/dev/null | head -1)"

# Find llvm-cov binary
LLVM_COV=""
if [[ -n "${SYSROOT}" ]]; then
    LLVM_COV="$(find "${SYSROOT}" -name llvm-cov -type f 2>/dev/null | head -1)"
fi
if [[ -z "${LLVM_COV}" ]]; then
    LLVM_COV="$(command -v llvm-cov 2>/dev/null || true)"
fi

if [[ -n "${INSTRUMENTED_LIB}" ]] && [[ -n "${LLVM_COV}" ]]; then
    # Text summary
    "${LLVM_COV}" report \
        --instr-profile="${COMBINED_DIR}/combined.profdata" \
        --object "${INSTRUMENTED_LIB}" \
        --sources "${PROJECT_ROOT}/src/" \
        2>/dev/null || warn "llvm-cov report failed (binary mismatch between host and Docker build is expected)"

    # HTML report
    mkdir -p "${COMBINED_DIR}/html"
    "${LLVM_COV}" show \
        --instr-profile="${COMBINED_DIR}/combined.profdata" \
        --object "${INSTRUMENTED_LIB}" \
        --sources "${PROJECT_ROOT}/src/" \
        --format=html \
        --output-dir="${COMBINED_DIR}/html" \
        2>/dev/null || warn "llvm-cov show (html) failed"

    info "Combined HTML report: ${COMBINED_DIR}/html/index.html"
else
    if [[ -z "${INSTRUMENTED_LIB}" ]]; then
        warn "Could not find instrumented library — skipping llvm-cov report."
        warn "The profraw/profdata files are still available for manual analysis."
    fi
    if [[ -z "${LLVM_COV}" ]]; then
        warn "llvm-cov not found — skipping report generation."
    fi
fi

# ── Summary ─────────────────────────────────────────────────────────────
echo ""
info "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
info "  E2E Coverage Pipeline Complete"
info ""
info "  profraw files:   ${E2E_COV_DIR}/*.profraw"
info "  E2E profdata:    ${E2E_COV_DIR}/merged.profdata"
info "  Combined:        ${COMBINED_DIR}/combined.profdata"
if [[ -d "${COMBINED_DIR}/html" ]]; then
    info "  HTML report:     ${COMBINED_DIR}/html/index.html"
fi
info "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# ── Open HTML report on macOS ────────────────────────────────────────────
if [[ -d "${COMBINED_DIR}/html" ]] && [[ "$(uname)" == "Darwin" ]]; then
    read -rp "Open HTML report in browser? [y/N] " answer
    if [[ "${answer}" =~ ^[Yy]$ ]]; then
        open "${COMBINED_DIR}/html/index.html"
    fi
fi
