# pg_stream — project commands
# https://github.com/casey/just

set dotenv-load := false

# Default PostgreSQL major version
pg := "18"

# ── Help ──────────────────────────────────────────────────────────────────

# List available recipes
default:
    @just --list --unsorted

# ── Build ─────────────────────────────────────────────────────────────────

# Compile the extension (debug)
build:
    cargo build --features pg{{pg}}

# Compile the extension (release)
build-release:
    cargo build --release --features pg{{pg}}

# ── Lint & Format ─────────────────────────────────────────────────────────

# Run cargo fmt
fmt:
    cargo fmt

# Check formatting without modifying files
fmt-check:
    cargo fmt -- --check

# Run clippy with warnings as errors
clippy:
    cargo clippy --all-targets --features pg{{pg}} -- -D warnings

# Run both fmt check and clippy
lint: fmt-check clippy

# ── Tests ─────────────────────────────────────────────────────────────────

# Run unit tests (lib only, no containers needed)
test-unit:
    cargo test --lib --features pg{{pg}}

# Run integration tests (requires Docker for testcontainers)
test-integration:
    cargo test \
        --test catalog_tests \
        --test extension_tests \
        --test monitoring_tests \
        --test smoke_tests \
        --test resilience_tests \
        --test scenario_tests \
        --test trigger_detection_tests \
        --test workflow_tests \
        --test property_tests \
        -- --test-threads=1

# Build the E2E Docker image
build-e2e-image:
    ./tests/build_e2e_image.sh

# Run E2E tests (requires E2E Docker image)
test-e2e: build-e2e-image
    cargo test --test 'e2e_*' -- --test-threads=1

# Run E2E tests without rebuilding the Docker image
test-e2e-fast:
    cargo test --test 'e2e_*' -- --test-threads=1

# Run pgrx-managed tests
test-pgrx:
    cargo pgrx test pg{{pg}}

# Run all tests (unit + integration + E2E + pgrx)
test-all: test-unit test-integration test-e2e test-pgrx

# ── dbt Tests ─────────────────────────────────────────────────────────────

# Run dbt-pgstream integration tests locally (builds Docker image)
test-dbt:
    ./dbt-pgstream/integration_tests/scripts/run_dbt_tests.sh

# Run dbt tests without rebuilding the Docker image
test-dbt-fast:
    ./dbt-pgstream/integration_tests/scripts/run_dbt_tests.sh --skip-build

# ── Benchmarks ────────────────────────────────────────────────────────────

# Run all criterion benchmarks
bench:
    cargo bench --bench refresh_bench --bench diff_operators

# Run only the diff-operator benchmarks
bench-diff:
    cargo bench --bench diff_operators

# Run benchmarks with Bencher-compatible output
bench-bencher:
    cargo bench --bench refresh_bench --bench diff_operators -- --output-format bencher

# ── Coverage ──────────────────────────────────────────────────────────────

# Generate code coverage report (HTML + LCOV)
coverage:
    ./scripts/coverage.sh

# Generate LCOV coverage report only (for CI)
coverage-lcov:
    ./scripts/coverage.sh --lcov

# Show coverage summary in terminal
coverage-text:
    ./scripts/coverage.sh --text

# Run E2E tests with coverage instrumentation and generate combined report
coverage-e2e:
    ./scripts/e2e-coverage.sh

# Run E2E coverage, skip Docker image rebuild (reuse existing)
coverage-e2e-fast:
    ./scripts/e2e-coverage.sh --skip-build

# ── pgrx ──────────────────────────────────────────────────────────────────

# Install the extension into the pgrx-managed PG instance
install:
    cargo pgrx install --features pg{{pg}}

# Start a pgrx-managed PostgreSQL session with the extension loaded
run:
    cargo pgrx run pg{{pg}}

# Package the extension for distribution
package:
    cargo pgrx package --features pg{{pg}}

# ── Docker ────────────────────────────────────────────────────────────────

# Build the CNPG production Docker image
docker-build:
    docker build -t pg_stream:latest -f cnpg/Dockerfile .

# Build the E2E test Docker image
docker-build-e2e:
    ./tests/build_e2e_image.sh

# ── Housekeeping ──────────────────────────────────────────────────────────

# Remove build artifacts
clean:
    cargo clean

# Full CI-style check (fmt + clippy + unit + integration + E2E)
ci: lint test-unit test-integration test-e2e
