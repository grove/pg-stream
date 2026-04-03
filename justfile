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

# Build the Docker Hub image (PostgreSQL 18 with pg_trickle pre-installed)
[group: "build"]
build-hub:
    docker build -t pgtrickle/pg_trickle:0.14.0-pg18 -f Dockerfile.hub .

# Build the Docker Hub image with 'latest' tag
[group: "build"]
build-hub-latest:
    docker build -t pgtrickle/pg_trickle:latest -f Dockerfile.hub .

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

# Audit unsafe block counts against the committed baseline (.unsafe-baseline)
[group: "lint"]
unsafe-inventory:
    bash scripts/unsafe_inventory.sh

# ── Tests ─────────────────────────────────────────────────────────────────

# Run pure-Rust unit tests (no Docker needed)
[group: "test"]
test-unit:
    ./scripts/run_unit_tests.sh pg{{pg}}

# Run DVM execution-backed integration tests with pg_stub (macOS + Linux)
# Requires Docker for testcontainers Postgres.
[group: "test"]
test-dvm:
    ./scripts/run_dvm_integration_tests.sh pg{{pg}}

# Run integration tests (requires Docker)
[group: "test"]
test-integration:
    cargo nextest run \
        --test catalog_tests \
        --test catalog_compat_tests \
        --test extension_tests \
        --test monitoring_tests \
        --test smoke_tests \
        --test resilience_tests \
        --test scenario_tests \
        --test trigger_detection_tests \
        --test workflow_tests \
        --test property_tests
    just test-tui-commands

# Run TUI command contract tests against stub functions (requires Docker)
[group: "test"]
test-tui-commands:
    cargo test -p pgtrickle-tui --bin pgtrickle test_cmd_ -- --test-threads=4

# Build the pre-compiled builder base image (Rust + cargo-pgrx + pgrx init).
# Only needed once, or when upgrading the Rust toolchain or pgrx version.
[group: "test"]
build-builder-image:
    docker build -t pg_trickle_builder:pg18 -f tests/Dockerfile.builder .

# Build the E2E Docker test image (auto-builds builder image if absent)
[group: "test"]
build-e2e-image:
    ./tests/build_e2e_image.sh

# Run E2E tests (rebuilds Docker image first)
[group: "test"]
test-e2e: build-e2e-image
    ./scripts/run_e2e_tests.sh --test 'e2e_*' 

# Run E2E tests, skip Docker image rebuild
[group: "test"]
test-e2e-fast:
    ./scripts/run_e2e_tests.sh --test 'e2e_*' 

# Run E2E tests with parallel refresh mode enabled (rebuilds Docker image first)
[group: "test"]
test-e2e-parallel: build-e2e-image
    PGT_PARALLEL_MODE=on ./scripts/run_e2e_tests.sh --test 'e2e_*' 

# Run E2E tests with parallel refresh mode enabled, skip Docker image rebuild
[group: "test"]
test-e2e-parallel-fast:
    PGT_PARALLEL_MODE=on ./scripts/run_e2e_tests.sh --test 'e2e_*' 

# Package the extension for light-E2E tests (cargo pgrx package)
[group: "test"]
package-extension:
    bash ./scripts/run_light_e2e_tests.sh --package-only

# Run light-E2E tests (stock postgres container, no custom Docker image).
# On macOS the runner builds Linux package artifacts in the Docker builder image.
[group: "test"]
test-light-e2e:
    bash ./scripts/run_light_e2e_tests.sh --package

# Run light-E2E tests, skip extension packaging
[group: "test"]
test-light-e2e-fast:
    bash ./scripts/run_light_e2e_tests.sh

# Run tests via pgrx against a pgrx-managed postgres
[group: "test"]
test-pgrx:
    cargo pgrx test pg{{pg}}

# Run all test tiers: unit + integration + E2E + pgrx
[group: "test"]
test-all: test-unit test-integration test-e2e test-pgrx

# Run PgBouncer compatibility E2E tests (requires E2E image + Docker)
[group: "test"]
test-pgbouncer: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_pgbouncer_tests

# Run PgBouncer tests, skip Docker image rebuild
[group: "test"]
test-pgbouncer-fast:
    ./scripts/run_e2e_tests.sh --test e2e_pgbouncer_tests

# ── Pipeline DAG Tests ───────────────────────────────────────────────────

# Run multi-level DAG pipeline tests (rebuilds Docker image)
[group: "test"]
test-pipeline: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_pipeline_dag_tests --no-capture

# Run pipeline DAG tests, skip Docker image rebuild
[group: "test"]
test-pipeline-fast:
    ./scripts/run_e2e_tests.sh --test e2e_pipeline_dag_tests --no-capture

# ── TPC-H Tests ───────────────────────────────────────────────────────────

# Run TPC-H correctness tests at SF-0.01 (~2 min, rebuilds Docker image)
[group: "tpch"]
test-tpch: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_tpch_tests --run-ignored all --no-capture

# Run TPC-H tests, skip Docker image rebuild
# TPCH_CYCLES=2     — 2 mutations cycles per query (33% fewer than default 3)
# TPCH_CHURN_CYCLES=20 — keep sustained-churn test fast
# --skip test_tpch_performance_comparison — benchmarking only, covered by differential_correctness
[group: "tpch"]
test-tpch-fast:
    TPCH_CYCLES=2 TPCH_CHURN_CYCLES=20 ./scripts/run_e2e_tests.sh --test e2e_tpch_tests --run-ignored all --no-capture -E 'not test(test_tpch_performance_comparison)'

# Run TPC-H tests at larger scale: SF-0.1 (~5 min, rebuilds Docker image)
[group: "tpch"]
test-tpch-large: build-e2e-image
    TPCH_SCALE=0.1 ./scripts/run_e2e_tests.sh --test e2e_tpch_tests --run-ignored all --no-capture

# Run TPC-H as a performance benchmark (SF-0.01, TPCH_BENCH=1, ~5 min, rebuilds Docker image).
# Emits [TPCH_BENCH] structured lines and a per-query median/P95/MERGE% summary table.
# Warm-up cycles are discarded before measurement (controlled by WARMUP_CYCLES, default 2).
[group: "tpch"]
bench-tpch: build-e2e-image
    TPCH_BENCH=1 ./scripts/run_e2e_tests.sh --test e2e_tpch_tests --run-ignored all --no-capture -E 'test(test_tpch_performance_comparison)'

# Run TPC-H benchmark, skip Docker image rebuild.
[group: "tpch"]
bench-tpch-fast:
    TPCH_BENCH=1 ./scripts/run_e2e_tests.sh --test e2e_tpch_tests --run-ignored all --no-capture -E 'test(test_tpch_performance_comparison)'

# Run TPC-H benchmark at larger scale: SF-0.1, TPCH_CYCLES=5, warm-up=2 (~15 min, rebuilds image).
[group: "tpch"]
bench-tpch-large: build-e2e-image
    TPCH_BENCH=1 TPCH_SCALE=0.1 TPCH_CYCLES=5 ./scripts/run_e2e_tests.sh --test e2e_tpch_tests --run-ignored all --no-capture -E 'test(test_tpch_performance_comparison)'

# DI-10: Run TPC-H benchmark at SF=1 (~1 GB data, 60-180 min).
# Validates that DVM improvements hold at realistic OLAP scale.
# Gate v0.13.0 release on 22/22 queries at SF=1.
# CI: manual dispatch only (4h timeout); local runs require ~2 GB Docker volume.
[group: "tpch"]
bench-tpch-sf1: build-e2e-image
    TPCH_BENCH=1 TPCH_SCALE=1 TPCH_CYCLES=3 ./scripts/run_e2e_tests.sh --test e2e_tpch_tests --run-ignored all --no-capture -E 'test(test_tpch_performance_comparison)'

# ── Correctness Gate (Phase 9) ────────────────────────────────────────────

# G17-SOAK: Long-running stability soak test (default 10 min, rebuilds Docker image)
[group: "test"]
test-soak: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_soak_tests --run-ignored all --no-capture

# G17-SOAK: Quick soak test (2 minutes, skip Docker rebuild)
[group: "test"]
test-soak-short:
    SOAK_DURATION_SECS=120 ./scripts/run_e2e_tests.sh --test e2e_soak_tests --run-ignored all --no-capture

# G17-MDB: Multi-database scheduler isolation test (rebuilds Docker image)
[group: "test"]
test-mdb: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_mdb_tests --run-ignored all --no-capture

# G17-MDB: Multi-database test, skip Docker image rebuild
[group: "test"]
test-mdb-fast:
    ./scripts/run_e2e_tests.sh --test e2e_mdb_tests --run-ignored all --no-capture

# Run Phase 9 external correctness gate (rebuilds Docker image, ~5-10 min)
# Five TPC-H-derived queries in DIFFERENTIAL mode; zero tolerance for failures.
[group: "test"]
test-correctness-gate: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_correctness_gate_tests --no-capture

# Run Phase 9 correctness gate, skip Docker image rebuild
[group: "test"]
test-correctness-gate-fast:
    ./scripts/run_e2e_tests.sh --test e2e_correctness_gate_tests --no-capture

# ── SQLancer Fuzzing (Phase 4) ─────────────────────────────────────────────

# Run SQLancer crash + equivalence oracle (rebuilds Docker image first).
# Controls: SQLANCER_CASES (default 200), SQLANCER_SEED (hex), SQLANCER_JAR.
[group: "sqlancer"]
sqlancer: build-e2e-image
    bash scripts/run_sqlancer.sh

# Run SQLancer oracle, skip Docker image rebuild
[group: "sqlancer"]
sqlancer-fast:
    bash scripts/run_sqlancer.sh

# Run only the Rust crash + equivalence oracle (no Java SQLancer tool).
# Faster than `just sqlancer`; suitable for PR spot-checks.
[group: "sqlancer"]
sqlancer-rust-only: build-e2e-image
    SKIP_JAVA_ORACLE=1 bash scripts/run_sqlancer.sh

# Run Rust oracle, skip Docker image rebuild
[group: "sqlancer"]
sqlancer-rust-only-fast:
    SKIP_JAVA_ORACLE=1 bash scripts/run_sqlancer.sh

# ── dbt Tests ─────────────────────────────────────────────────────────────

# Run dbt-pgtrickle integration tests (builds Docker image)
[group: "dbt"]
test-dbt:
    ./dbt-pgtrickle/integration_tests/scripts/run_dbt_tests.sh

# Run dbt tests, skip Docker image rebuild
[group: "dbt"]
test-dbt-fast:
    ./dbt-pgtrickle/integration_tests/scripts/run_dbt_tests.sh --skip-build

# Run the dbt Getting Started example project against a local pg_trickle container
[group: "dbt"]
test-dbt-getting-started:
    ./examples/dbt_getting_started/scripts/run_example.sh

# Run the dbt Getting Started example, skip Docker image rebuild
[group: "dbt"]
test-dbt-getting-started-fast:
    SKIP_BUILD=1 ./examples/dbt_getting_started/scripts/run_example.sh

# ── Upgrade Tests ─────────────────────────────────────────────────────────

# Validate upgrade script covers all new SQL objects (no Docker needed)
[group: "upgrade"]
check-upgrade from to:
    scripts/check_upgrade_completeness.sh {{from}} {{to}}

# Validate all upgrade scripts cover their new SQL objects (no Docker needed)
[group: "upgrade"]
check-upgrade-all:
    #!/usr/bin/env bash
    set -euo pipefail
    current_version=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    pairs=()
    for f in sql/pg_trickle--*--*.sql; do
        base=$(basename "$f" .sql)
        from=${base#pg_trickle--}
        to=${from#*--}
        from=${from%%--*}
        pairs+=("$from $to")
    done
    # Verify the chain reaches the current Cargo.toml version
    last_to=$(printf '%s\n' "${pairs[@]}" | awk '{print $2}' | sort -V | tail -1)
    if [[ "$last_to" != "$current_version" ]]; then
        echo "ERROR: Latest upgrade script target ($last_to) does not match Cargo.toml version ($current_version)."
        echo "       Did you forget to create sql/pg_trickle--${last_to}--${current_version}.sql?"
        exit 1
    fi
    echo "Found ${#pairs[@]} upgrade step(s) ending at v${current_version}"
    failed=0
    for pair in "${pairs[@]}"; do
        from=${pair%% *}
        to=${pair##* }
        echo ""
        echo "━━━ Checking upgrade: ${from} → ${to} ━━━"
        if ! scripts/check_upgrade_completeness.sh "$from" "$to"; then
            failed=1
        fi
    done
    if [[ $failed -ne 0 ]]; then
        echo ""
        echo "FAILED: One or more upgrade completeness checks failed."
        exit 1
    fi
    echo ""
    echo "All ${#pairs[@]} upgrade step(s) passed completeness checks."

# Build the upgrade Docker image for testing FROM→TO migrations
[group: "upgrade"]
build-upgrade-image from="0.7.0" to="0.15.0": build-e2e-image
    ./tests/build_e2e_upgrade_image.sh {{from}} {{to}}

# Run upgrade E2E tests (builds base + upgrade Docker images first)
[group: "upgrade"]
test-upgrade from="0.7.0" to="0.15.0": (build-upgrade-image from to)
    PGS_E2E_IMAGE=pg_trickle_upgrade_e2e:latest \
    PGS_UPGRADE_FROM={{from}} PGS_UPGRADE_TO={{to}} \
        ./scripts/run_e2e_tests.sh --test e2e_upgrade_tests --run-ignored all --no-capture

# Run upgrade E2E tests for every adjacent version pair and the full chain
# (builds the base E2E image once, then an upgrade image per pair)
[group: "upgrade"]
test-upgrade-all: build-e2e-image
    #!/usr/bin/env bash
    set -euo pipefail
    current_version=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    pairs=()
    for f in sql/pg_trickle--*--*.sql; do
        base=$(basename "$f" .sql)
        from=${base#pg_trickle--}
        to=${from#*--}
        from=${from%%--*}
        pairs+=("$from $to")
    done
    # Verify the chain reaches the current Cargo.toml version
    last_to=$(printf '%s\n' "${pairs[@]}" | awk '{print $2}' | sort -V | tail -1)
    if [[ "$last_to" != "$current_version" ]]; then
        echo "ERROR: Latest upgrade script target ($last_to) does not match Cargo.toml version ($current_version)."
        echo "       Did you forget to create sql/pg_trickle--${last_to}--${current_version}.sql?"
        exit 1
    fi
    # Also test the full chain from oldest archive to current version
    oldest=$(ls sql/archive/pg_trickle--*.sql | sed 's/.*--\(.*\)\.sql/\1/' | sort -V | head -1)
    if [[ "$oldest" != "$current_version" ]]; then
        pairs+=("$oldest $current_version")
    fi
    echo "Will test ${#pairs[@]} upgrade path(s) ending at v${current_version}"
    failed=0
    for pair in "${pairs[@]}"; do
        from=${pair%% *}
        to=${pair##* }
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "  Testing upgrade: ${from} → ${to}"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        ./tests/build_e2e_upgrade_image.sh "$from" "$to"
        if ! PGS_E2E_IMAGE=pg_trickle_upgrade_e2e:latest \
             PGS_UPGRADE_FROM="$from" PGS_UPGRADE_TO="$to" \
             ./scripts/run_e2e_tests.sh --test e2e_upgrade_tests --run-ignored all --no-capture; then
            echo "FAILED: upgrade ${from} → ${to}"
            failed=1
        fi
    done
    if [[ $failed -ne 0 ]]; then
        echo ""
        echo "FAILED: One or more upgrade tests failed."
        exit 1
    fi
    echo ""
    echo "All ${#pairs[@]} upgrade path(s) passed."

# Check that all version-related files and references are in sync with Cargo.toml
[group: "upgrade"]
check-version-sync:
    ./scripts/check_version_sync.sh

# ── Benchmarks ────────────────────────────────────────────────────────────

# Run all criterion benchmarks
[group: "bench"]
bench:
    ./scripts/run_benchmarks.sh

# Run database-level E2E benchmark suite (rebuilds Docker image)
[group: "bench"]
test-bench-e2e: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_bench_tests --features pg18 --run-ignored all --no-capture

# Run E2E benchmarks, skip Docker image rebuild
[group: "bench"]
test-bench-e2e-fast:
    ./scripts/run_e2e_tests.sh --test e2e_bench_tests --features pg18 --run-ignored all --no-capture

# Run DAG topology benchmark suite (rebuilds Docker image)
[group: "bench"]
test-dag-bench: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_dag_bench_tests --features pg18 --run-ignored all --no-capture

# Run DAG topology benchmarks, skip Docker image rebuild
[group: "bench"]
test-dag-bench-fast:
    ./scripts/run_e2e_tests.sh --test e2e_dag_bench_tests --features pg18 --run-ignored all --no-capture

# Run diff-operator benchmarks only
[group: "bench"]
bench-diff:
    ./scripts/run_benchmarks.sh diff_operators

# Run benchmarks with Bencher-compatible JSON output
[group: "bench"]
bench-bencher:
    ./scripts/run_benchmarks.sh -- --output-format bencher

# Run Criterion benchmarks inside the E2E Docker builder (for environments
# where local pg_stub linking fails, e.g. missing PG server symbols)
[group: "bench"]
bench-docker: build-e2e-image
    #!/usr/bin/env bash
    set -euo pipefail
    IMAGE="${BUILDER_IMAGE:-pg_trickle_builder:pg18}"
    echo "Running Criterion benchmarks inside Docker ($IMAGE)..."
    docker run --rm -t \
        -v "$(pwd)":/workspace \
        -w /workspace \
        "$IMAGE" \
        bash -c 'cargo bench --features pg18 2>&1'

# Compare two benchmark JSON result files (I-4)
[group: "bench"]
bench-compare baseline candidate:
    ./scripts/bench_compare.sh {{baseline}} {{candidate}}

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

# ── Release ───────────────────────────────────────────────────────────────

# Package the extension into a zip archive and upload it to PGXN
[group: "release"]
pgxn-publish:
    #!/usr/bin/env bash
    set -euo pipefail
    
    VERSION=$(jq -r '.version' META.json)
    if [ -z "$VERSION" ] || [ "$VERSION" = "null" ]; then
        echo "Error: Could not read version from META.json"
        exit 1
    fi
    
    ARCHIVE="pg_trickle-${VERSION}.zip"
    echo "Creating PGXN archive: $ARCHIVE"
    git archive --format zip --prefix="pg_trickle-${VERSION}/" -o "$ARCHIVE" HEAD
    
    echo "Verifying archive contents..."
    python3 scripts/verify_pgxn_archive.py "$ARCHIVE"
    
    if [ -z "${PGXN_USERNAME:-}" ] || [ -z "${PGXN_PASSWORD:-}" ]; then
        echo "Error: missing PGXN credentials."
        echo "Set PGXN_USERNAME and PGXN_PASSWORD environment variables."
        exit 1
    fi
    
    echo "Uploading to PGXN as '${PGXN_USERNAME}'..."
    HTTP_STATUS=$(curl --silent --show-error \
        --output /tmp/pgxn_response.txt --write-out "%{http_code}" \
        -F "archive=@${ARCHIVE};type=application/zip" \
        -u "${PGXN_USERNAME}:${PGXN_PASSWORD}" \
        "https://manager.pgxn.org/upload")
    
    echo "PGXN responded with HTTP $HTTP_STATUS"
    
    # PGXN Manager returns 200 on success or 3xx redirect-after-POST.
    if [ "$HTTP_STATUS" -ge 200 ] && [ "$HTTP_STATUS" -lt 400 ]; then
        echo "Successfully uploaded pg_trickle-$VERSION to PGXN!"
    elif [ "$HTTP_STATUS" = "409" ] && grep -q "already exists" /tmp/pgxn_response.txt; then
        echo "pg_trickle-$VERSION already exists on PGXN — nothing to do."
    else
        echo "Error: PGXN upload failed with HTTP $HTTP_STATUS"
        echo "Response body:"
        cat /tmp/pgxn_response.txt
        echo ""
        if [ "$HTTP_STATUS" = "401" ]; then
            echo "Hint: HTTP 401 means authentication failed."
            echo "Verify PGXN_USERNAME and PGXN_PASSWORD secrets are correct."
            echo "Register at https://manager.pgxn.org/account/register if needed."
        fi
        exit 1
    fi

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
