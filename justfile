# pg_trickle — project commands
# https://github.com/casey/just

set dotenv-load := false

# Default PostgreSQL major version
pg := "18"

# List available recipes
[group: "help"]
default:
    @just --list --unsorted

# ── Build ─────────────────────────────────────────────────────────────────

# Compile the extension (debug)
[group: "build"]
build:
    cargo build --features pg{{pg}}

# Compile the extension (release)
[group: "build"]
build-release:
    cargo build --release --features pg{{pg}}

# ── Lint & Format ─────────────────────────────────────────────────────────

# Format source code
[group: "lint"]
fmt:
    cargo fmt

# Check formatting only (no files changed)
[group: "lint"]
fmt-check:
    cargo fmt -- --check

# Lint with clippy (warnings as errors)
[group: "lint"]
clippy:
    cargo clippy --all-targets --features pg{{pg}} -- -D warnings

# Check formatting and run clippy
[group: "lint"]
lint: fmt-check clippy

# ── Tests ─────────────────────────────────────────────────────────────────

# Run pure-Rust unit tests (no Docker needed)
[group: "test"]
test-unit:
    ./scripts/run_unit_tests.sh pg{{pg}}

# Run integration tests (requires Docker)
[group: "test"]
test-integration:
    cargo test \
        --test catalog_tests \
        --test catalog_compat_tests \
        --test extension_tests \
        --test monitoring_tests \
        --test smoke_tests \
        --test resilience_tests \
        --test scenario_tests \
        --test trigger_detection_tests \
        --test workflow_tests \
        --test property_tests \
        -- --test-threads=1

# Build the E2E Docker test image
[group: "test"]
build-e2e-image:
    ./tests/build_e2e_image.sh

# Run E2E tests (rebuilds Docker image first)
[group: "test"]
test-e2e: build-e2e-image
    cargo test --test 'e2e_*' -- --test-threads=1

# Run E2E tests, skip Docker image rebuild
[group: "test"]
test-e2e-fast:
    cargo test --test 'e2e_*' -- --test-threads=1

# Run tests via pgrx against a pgrx-managed postgres
[group: "test"]
test-pgrx:
    cargo pgrx test pg{{pg}}

# Run all test tiers: unit + integration + E2E + pgrx
[group: "test"]
test-all: test-unit test-integration test-e2e test-pgrx

# ── TPC-H Tests ───────────────────────────────────────────────────────────

# Run TPC-H correctness tests at SF-0.01 (~2 min, rebuilds Docker image)
[group: "tpch"]
test-tpch: build-e2e-image
    cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture

# Run TPC-H tests, skip Docker image rebuild
[group: "tpch"]
test-tpch-fast:
    cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture

# Run TPC-H tests at larger scale: SF-0.1 (~5 min, rebuilds Docker image)
[group: "tpch"]
test-tpch-large: build-e2e-image
    TPCH_SCALE=0.1 cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture

# ── dbt Tests ─────────────────────────────────────────────────────────────

# Run dbt-pgtrickle integration tests (builds Docker image)
[group: "dbt"]
test-dbt:
    ./dbt-pgtrickle/integration_tests/scripts/run_dbt_tests.sh

# Run dbt tests, skip Docker image rebuild
[group: "dbt"]
test-dbt-fast:
    ./dbt-pgtrickle/integration_tests/scripts/run_dbt_tests.sh --skip-build

# ── Upgrade Tests ─────────────────────────────────────────────────────────

# Validate upgrade script covers all new SQL objects (no Docker needed)
[group: "upgrade"]
check-upgrade from to:
    scripts/check_upgrade_completeness.sh {{from}} {{to}}

# Build the upgrade Docker image for testing FROM→TO migrations
[group: "upgrade"]
build-upgrade-image from="0.1.3" to="0.2.1": build-e2e-image
    ./tests/build_e2e_upgrade_image.sh {{from}} {{to}}

# Run upgrade E2E tests (builds base + upgrade Docker images first)
[group: "upgrade"]
test-upgrade from="0.1.3" to="0.2.1": (build-upgrade-image from to)
    PGS_E2E_IMAGE=pg_trickle_upgrade_e2e:latest \
    PGS_UPGRADE_FROM={{from}} PGS_UPGRADE_TO={{to}} \
        cargo test --test e2e_upgrade_tests -- --ignored --test-threads=1 --nocapture

# ── Benchmarks ────────────────────────────────────────────────────────────

# Run all criterion benchmarks
[group: "bench"]
bench:
    ./scripts/run_benchmarks.sh

# Run diff-operator benchmarks only
[group: "bench"]
bench-diff:
    ./scripts/run_benchmarks.sh diff_operators

# Run benchmarks with Bencher-compatible JSON output
[group: "bench"]
bench-bencher:
    ./scripts/run_benchmarks.sh -- --output-format bencher

# ── Coverage ──────────────────────────────────────────────────────────────

# Generate HTML + LCOV coverage report
[group: "coverage"]
coverage:
    ./scripts/coverage.sh

# Generate LCOV report only (for CI upload)
[group: "coverage"]
coverage-lcov:
    ./scripts/coverage.sh --lcov

# Print coverage summary to terminal
[group: "coverage"]
coverage-text:
    ./scripts/coverage.sh --text

# Run E2E tests with coverage instrumentation (rebuilds Docker image)
[group: "coverage"]
coverage-e2e:
    ./scripts/e2e-coverage.sh

# E2E coverage, skip Docker image rebuild
[group: "coverage"]
coverage-e2e-fast:
    ./scripts/e2e-coverage.sh --skip-build

# ── pgrx ──────────────────────────────────────────────────────────────────

# Install the extension into the pgrx-managed postgres
[group: "pgrx"]
install:
    cargo pgrx install --features pg{{pg}}

# Open a pgrx postgres session with the extension loaded
[group: "pgrx"]
run:
    cargo pgrx run pg{{pg}}

# Package the extension for distribution
[group: "pgrx"]
package:
    cargo pgrx package --features pg{{pg}}

# ── Docker ────────────────────────────────────────────────────────────────

# Build the CNPG extension image (scratch-based, for Image Volumes)
[group: "docker"]
docker-build:
    docker build -t pg_trickle-ext:latest -f cnpg/Dockerfile.ext-build .

# Build the E2E Docker image (alias for build-e2e-image)
[group: "docker"]
docker-build-e2e:
    ./tests/build_e2e_image.sh

# ── Documentation ─────────────────────────────────────────────────────────

# Build the mdBook documentation site → book/
[group: "docs"]
docs-build:
    mdbook build

# Serve docs locally with live-reload at http://localhost:3000
[group: "docs"]
docs-serve:
    mdbook serve --open

# ── Housekeeping ──────────────────────────────────────────────────────────

# Remove build artifacts
[group: "housekeeping"]
clean:
    cargo clean

# Full CI check: lint + unit + integration + E2E
[group: "housekeeping"]
ci: lint test-unit test-integration test-e2e
