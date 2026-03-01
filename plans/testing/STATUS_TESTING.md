# pg_trickle — Testing & Coverage Status

What we've learned about testing the pg_trickle extension, the current state of tests and coverage, and what can be improved.

---

## 1. Current Test Inventory

| Category | Tests | Runner | Requires Docker? |
|----------|------:|--------|:----------------:|
| **Unit tests** (in `src/`) | 854 | `just test-unit` | No |
| **Integration tests** (in `tests/`) | 101 | `just test-integration` | Yes (bare PG 18.x containers) |
| **E2E tests** (in `tests/e2e_*.rs`) | 340 | `just test-e2e` | Yes (custom Docker image with extension) |
| **Benchmarks** | 16 | `just test-e2e` (ignored) | Yes |
| **Total** | **1,311** | `just test-all` | |

### Unit Tests by Module

| File | Tests | Focus |
|------|------:|-------|
| `src/dvm/parser.rs` | 237 | SQL parsing, OpTree construction, expression handling, view definition sanitization |
| `src/dvm/operators/aggregate.rs` | 106 | Aggregate differentiation, merge expressions, delta CTEs |
| `src/api.rs` | 82 | Pure helper functions (`quote_identifier`, `find_top_level_keyword`, `classify_relkind`, etc.) |
| `src/dvm/operators/recursive_cte.rs` | 59 | Recursive CTE SQL generation, cascade detection |
| `src/refresh.rs` | 44 | Refresh action resolution, LSN placeholder substitution |
| `src/dvm/diff.rs` | 34 | DiffContext, CTE name generation, dispatch |
| `src/dvm/mod.rs` | 28 | Delta template resolution, union splitting, scan-chain detection |
| `src/dag.rs` | 26 | Topological sort, cycle detection, lag resolution |
| `src/dvm/operators/join.rs` | 20 | Inner join differentiation, equijoin key extraction |
| `src/dvm/operators/scan.rs` | 18 | Scan differentiation, PK detection, hash expressions |
| `src/dvm/operators/lateral_subquery.rs` | 18 | Lateral subquery support |
| `src/dvm/operators/join_common.rs` | 18 | Shared join helpers |
| `src/dvm/operators/lateral_function.rs` | 16 | Lateral function differentiation |
| `src/dvm/operators/except.rs` | 16 | EXCEPT operator differentiation |
| `src/version.rs` | 15 | Versioning, epoch conversion, frontier operations |
| `src/dvm/operators/intersect.rs` | 14 | INTERSECT operator differentiation |
| Others (19 files) | 128 | filter, distinct, union_all, window, project, outer_join, cte_scan, subquery, error, hash, cdc, monitor, scheduler, hooks, semi_join, anti_join, full_join, scalar_subquery |

### E2E Tests by File

| File | Tests | Focus |
|------|------:|-------|
| `e2e_cte_tests.rs` | 68 | Recursive/non-recursive CTE parsing and IVM |
| `e2e_expression_tests.rs` | 29 | Expression handling in defining queries |
| `e2e_create_tests.rs` | 29 | `create_stream_table()` validation, catalog writes |
| `e2e_refresh_tests.rs` | 26 | Refresh modes (FULL, DIFFERENTIAL), staleness, correctness |
| `e2e_error_tests.rs` | 26 | Error handling, invalid inputs, rejection messages |
| `e2e_window_tests.rs` | 18 | Window function parsing and IVM |
| `e2e_lateral_tests.rs` | 16 | LATERAL joins |
| `e2e_bench_tests.rs` | 16 | Performance benchmarks (marked `#[ignore]`) |
| `e2e_lateral_subquery_tests.rs` | 12 | LATERAL subquery patterns |
| `e2e_property_tests.rs` | 11 | Deterministic property-based correctness |
| `e2e_coverage_parser_tests.rs` | 11 | Parser edge cases for coverage |
| `e2e_smoke_tests.rs` | 10 | Basic extension functionality |
| `e2e_coverage_error_tests.rs` | 10 | Error path coverage |
| `e2e_alter_tests.rs` | 10 | ALTER operations |
| `e2e_lifecycle_tests.rs` | 9 | Stream table lifecycle state machine |
| `e2e_cdc_tests.rs` | 9 | CDC triggers, change buffers |
| `e2e_bgworker_tests.rs` | 9 | Background worker scheduling |
| `e2e_drop_tests.rs` | 8 | Drop cascades, cleanup |
| `e2e_monitoring_tests.rs` | 6 | Stats, lag, alerts |
| `e2e_ddl_event_tests.rs` | 4 | DDL event hook handling |
| `e2e_concurrent_tests.rs` | 3 | Advisory locks, concurrent refresh |

### Integration Tests (Non-E2E)

These run against bare PostgreSQL 18.x containers without the extension loaded, manually creating catalog schemas:

| File | Tests | Focus |
|------|------:|-------|
| `catalog_tests.rs` | 15 | Catalog CRUD via SQL |
| `catalog_compat_tests.rs` | 13 | PG18 catalog behavior: `pg_get_viewdef` semicolons, `nspname::text` cast, `relkind` values, `array_length` types, UNION ALL column consistency |
| `scenario_tests.rs` | 10 | Multi-step workflow scenarios |
| `monitoring_tests.rs` | 8 | Monitoring views and functions |
| `resilience_tests.rs` | 7 | Error recovery, edge cases |
| `extension_tests.rs` | 7 | Extension install/uninstall |
| `workflow_tests.rs` | 4 | End-to-end workflows |
| `smoke_tests.rs` | 3 | Basic smoke tests |

---

## 2. Testing Architecture

### Three-Tier Test Strategy

```
┌─────────────────────────────────────────────────────┐
│  Tier 1: Unit Tests (854 tests)                     │
│  - Pure Rust, no database needed                    │
│  - Tests OpTree construction, SQL generation,       │
│    differentiation logic, DAG algorithms            │
│  - Runs in <5 seconds                               │
│  - cargo test --lib                                 │
├─────────────────────────────────────────────────────┤
│  Tier 2: Integration Tests (101 tests)              │
│  - Bare PG 18.x containers via Testcontainers       │
│  - Tests SQL-level data model, catalog schema,      │
│    and PG version-specific catalog behavior          │
│  - Does NOT load the compiled extension             │
│  - Runs in ~2-3 minutes                             │
├─────────────────────────────────────────────────────┤
│  Tier 3: E2E Tests (340 tests)                      │
│  - Custom Docker image with compiled extension      │
│  - Tests full SQL API via CREATE EXTENSION          │
│  - Exercises CDC triggers, event hooks, refresh,    │
│    background workers, monitoring views             │
│  - Runs in ~10-15 minutes (serial, one container    │
│    per test)                                        │
└─────────────────────────────────────────────────────┘
```

### E2E Infrastructure

The E2E tests use a **custom Docker image** (`pg_trickle_e2e:latest`) built via a multi-stage Dockerfile:

1. **Stage 1 (builder):** Installs Rust, `cargo-pgrx`, compiles the extension against PG 18 headers
2. **Stage 2 (runtime):** Copies `.so`, `.control`, `.sql` into a clean `postgres:18.x` image with `shared_preload_libraries = 'pg_trickle'`

Build the image:
```bash
./tests/build_e2e_image.sh
```

The `E2eDb` test harness (in `tests/e2e/mod.rs`) starts one container per test via `testcontainers::GenericImage`, connects via `sqlx::PgPool`, and provides helpers for extension setup, SQL execution, and assertions.

### Property-Based Testing

The `e2e_property_tests.rs` file implements 11 deterministic property-based correctness tests. The key invariant tested:

> For every stream table, at every data timestamp:
> `Contents(ST) = Result(defining_query)` (multiset equality)

Each test uses a **deterministic SplitMix64 PRNG** seeded per test, applies randomized DML (INSERT/UPDATE/DELETE mix) across 5 cycles of 15 initial rows, and verifies the invariant after each cycle. On failure, the seed is printed for reproduction.

---

## 3. Code Coverage

### Coverage History

| Milestone | Unit Tests | Lines Covered | Coverage |
|-----------|-----------|--------------|----------|
| Initial baseline | 189 | 2,888 / 9,139 | **31.6%** |
| After Coverage Wave 1 (4 phases) | 476 | 7,525 / 12,117 | **62.1%** |
| Current (post SQL support expansion) | 854 | ~8,500+ / ~13,500 | **~63%** (est.) |

### The Coverage Ceiling

Unit test coverage has hit a **structural ceiling at ~63%**. Of the ~4,600 uncovered lines:

| Category | Uncovered Lines | % of Uncovered | Unit Testable? |
|----------|---------------:|---------------:|:--------------:|
| **DB-only** (SPI, pg_sys FFI, GUC, shmem, bg workers) | ~3,940 | 86% | No |
| **Parser** (pg_sys::raw_parser tree walking) | ~1,012 | 22% | No |
| **Pure Rust edge branches** | ~165 | 4% | Yes (diminishing returns) |

86% of remaining uncovered code **requires a running PostgreSQL instance**. It uses `Spi::connect()`, `pg_sys::raw_parser()`, `BackgroundWorkerBuilder`, GUC registration, event trigger context, or `#[pg_extern]` entry points.

### DB-Only Code That Cannot Be Unit Tested

| File | Uncovered Lines | Why |
|------|---------------:|-----|
| `src/dvm/parser.rs` | ~1,012 | `pg_sys::raw_parser()` FFI parse tree walking |
| `src/api.rs` | ~662 | `#[pg_extern]` + SPI orchestration |
| `src/catalog.rs` | ~464 | 100% SPI queries |
| `src/scheduler.rs` | ~380 | Background worker + SPI |
| `src/monitor.rs` | ~358 | `#[pg_extern]` + SPI |
| `src/refresh.rs` | ~315 | SPI (pure logic already tested) |
| `src/hooks.rs` | ~275 | Event triggers + SPI |
| `src/cdc.rs` | ~224 | SPI CREATE/DROP + triggers |
| `src/dvm/mod.rs` | ~181 | Calls `parse_defining_query()` |
| Others | ~121 | GUC, shmem, pg_extern wrappers |

### E2E Coverage Infrastructure (Implemented)

To break through the 63% ceiling, an **instrumented E2E coverage** pipeline has been built:

1. **`tests/Dockerfile.e2e-coverage`** — Builds the extension with LLVM coverage instrumentation (`-C instrument-coverage`)
2. **`scripts/e2e-coverage.sh`** — Full pipeline: build instrumented image → run E2E tests → extract profraw → merge with unit coverage → generate report
3. **`tests/e2e/mod.rs`** — Supports `PGS_E2E_IMAGE` env var for image override and `PGS_E2E_COVERAGE_DIR` for volume-mounting profraw output

Projected combined coverage (unit + E2E): **75–85%**.

### Running Coverage

```bash
# Unit test coverage (HTML + LCOV)
just coverage

# Terminal summary only
just coverage-text

# E2E instrumented coverage (full pipeline)
just coverage-e2e

# E2E coverage, skip Docker rebuild
just coverage-e2e-fast
```

---

## 4. Key Lessons Learned

### 4.1 The IVM Engine Is Highly Unit-Testable

The biggest win was discovering that the **entire DVM (differential view maintenance) subsystem** — 11 operator files, the diff engine, and the parser's pure logic — is fully testable without a database. These files do string-based SQL construction from `OpTree` structs. Going from 0% to 90%+ coverage on the operator files required only constructing test `OpTree`s and asserting on the generated SQL.

**Pattern used across all operator tests:**
```rust
let mut ctx = test_ctx();  // DiffContext with dummy frontiers
let tree = OpTree::Scan { /* ... */ };
let result = diff_scan(&mut ctx, &tree).unwrap();
assert!(result.cte_sql.contains("expected SQL fragment"));
```

### 4.2 Property Tests Found Real Bugs

The 11 deterministic property tests in `e2e_property_tests.rs` caught a **correctness bug** in the DELETE+INSERT merge strategy. When the strategy evaluated the delta query twice (once for DELETE, once for INSERT), aggregate/DISTINCT queries that LEFT JOIN back to the stream table to read `__pgt_count` saw modified data between the DELETE and INSERT, causing incorrect results.

This was not caught by hand-written E2E tests because they used known data patterns. The randomized DML from property tests triggered the edge case.

**Lesson:** Property-based tests with randomized DML are essential for IVM correctness, not just a nice-to-have.

### 4.3 E2E Test Isolation Is Critical

Each E2E test gets its **own PostgreSQL container**. Early experiments with shared containers led to test interference (leftover stream tables, triggers, catalog state). The overhead of per-test containers (~1-2 seconds startup) is worth the isolation guarantee.

**Trade-off:** Running 340 E2E tests serially with per-test containers takes ~10-15 minutes. Parallelism is possible but requires sufficient Docker resources.

### 4.4 The Docker Image Build Is the Bottleneck

Building the E2E Docker image (compiling Rust + pgrx inside a container) takes **5-15 minutes** depending on cache state. This is the dominant cost in the development cycle. Mitigations:

- Docker layer caching (separate `cargo fetch` layer before source copy)
- `just test-e2e-fast` skips the rebuild when iterating on tests
- CI caches the builder layer

### 4.5 Integration Tests (Tier 2) Are Valuable for Catalog Assumptions

The integration tests that run against bare PostgreSQL (without the extension) were initially viewed as marginal once E2E tests existed. However, the **catalog compatibility tests** (`catalog_compat_tests.rs`) proved their value by catching PostgreSQL version-specific behavior differences (see §4.7). These tests run fast (~40s), don't require the extension Docker image, and serve as **early-warning canaries** for PG version upgrades.

The original 54 tests that simulate catalog behavior by manually creating schemas still have overlap with E2E tests, but the newer catalog compatibility tests fill a unique niche.

### 4.6 Benchmark Tests Need Careful Interpretation

Benchmark results show high variance between runs (see [STATUS_PERFORMANCE.md](../performance/STATUS_PERFORMANCE.md)). Cycle 1 is always cold (cache miss, first plan). The "INCR c1" and "INCR 2+" columns in later benchmark snapshots are more informative than simple averages.

### 4.7 E2E Failures Exposed Systemic Gaps (Session 2026-02)

A full E2E test run uncovered **~86 failures** across multiple categories. All were fixable, but the root causes reveal important lessons about what unit tests and integration tests *cannot* catch in a pgrx extension:

#### Root Cause 1: pgrx Type Oid Mismatch (9 tests)

`resolve_rangevar_schema()` queried `pg_namespace.nspname` (PostgreSQL `name` type, Oid 19) and decoded it via `.get::<String>()` which expects `text` (Oid 25). This worked in earlier pgrx versions but broke with stricter type checking.

**Lesson:** Always cast `name`-typed columns to `::text` in SPI queries. This is now documented as a mandatory coding convention in `AGENTS.md`. A dedicated integration test (`test_nspname_text_cast_works`) now verifies this assumption.

#### Root Cause 2: CTE Names Aren't in pg_class (63 tests)

`resolve_rangevar_schema()` assumed every `RangeVar` in a FROM clause references a real `pg_class` entry. CTE names (e.g., `WITH all_people AS ... FROM all_people`) aren't in `pg_class`, causing an empty `SpiTupleTable` iterator panic.

**Lesson:** Catalog lookups must return `Option` rather than panicking on empty results. CTE names, subquery aliases, and function-call ranges are valid FROM sources that don't exist in `pg_class`. The fix changed the return type from `Result<String>` to `Result<Option<String>>` and callers now skip `None` values.

#### Root Cause 3: PostgreSQL 18 Behavior Change (13 tests)

`pg_get_viewdef()` in PostgreSQL 18 returns view definitions **with a trailing semicolon**. When embedded as `(SELECT ... ;)` inside a subquery, this produces a syntax error. This is a pure PG version regression invisible to unit tests.

**Lesson:** Any function that wraps catalog output in SQL (e.g., embedding a view definition in a subquery) must sanitize the output. The fix was extracted as a pure function `strip_view_definition_suffix()` with 6 unit tests. An integration test (`test_pg_get_viewdef_returns_trimmed_definition`) now verifies this behavior on the actual PG version.

#### Root Cause 4: Stale Tests After Feature Implementation (7 tests)

Tests that expected rejection of NATURAL JOIN, DISTINCT ON, and GROUPING SETS/CUBE/ROLLUP were not updated when these features were implemented as auto-rewrites in a prior session.

**Lesson:** When implementing a feature that changes error→success behavior, the corresponding rejection tests must be updated in the same commit. A pre-commit checklist item has been added to `AGENTS.md`.

#### Root Cause 5: Incomplete relkind Handling (1 test)

`classify_source_relation()` mapped materialized views (`relkind='m'`) to `"TABLE"`, causing CDC trigger installation attempts that fail on matviews.

**Lesson:** The `relkind` → source-type mapping was interleaved with SPI calls, making it impossible to unit-test. Extracting the pure function `classify_relkind()` enabled 7 unit tests covering all relkind variants. An integration test (`test_relkind_for_materialized_view`) verifies the actual PG relkind value.

#### Root Cause 6: INT4/INT8 Type Mismatch (1 test)

`array_length()` returns `integer` (INT4), but a test expected `bigint` (INT8) when comparing columns. An integration test (`test_array_length_returns_int4`) now documents this.

#### Impact and Prevention

| Category | Tests Fixed | Prevention Measure |
|----------|--------:|--------------------|
| Oid 19/25 type mismatch | 9 | `::text` cast convention in AGENTS.md + integration test |
| CTE name in pg_class | 63 | `Option` return pattern + integration test |
| PG18 viewdef semicolons | 13 | `strip_view_definition_suffix()` pure function + 6 unit tests + integration test |
| Stale rejection tests | 7 | Pre-commit checklist in AGENTS.md |
| Matview relkind handling | 1 | `classify_relkind()` pure function + 7 unit tests + integration test |
| INT4/INT8 type mismatch | 1 | Integration test documenting `array_length` return type |
| **Total** | **~86** (reduced to 4 pre-existing) | 13 new integration tests + 13 new unit tests |

The 4 remaining failures are **pre-existing** (CDC partition data mismatch, CTE multi-ref self-join, recursive CTE `row_data` column, CASE-in-WHERE deparsing) and are tracked separately.

---

## 5. What Can Be Improved

### 5.1 CI Integration for E2E Coverage (Not Started)

**Priority: High.** The instrumented E2E coverage pipeline (`scripts/e2e-coverage.sh`) works locally but is not integrated into CI. A GitHub Actions workflow should:

- Build the coverage-instrumented Docker image
- Run E2E tests with profraw extraction
- Merge unit + E2E profdata
- Upload combined coverage to Codecov
- Track the combined metric on every PR

This would provide **visibility into the 75-85% combined coverage** on every commit, gating PRs that reduce coverage.

### 5.2 More Property Tests

**Priority: High.** Currently 11 property tests cover: SUM, COUNT, AVG, DISTINCT, MIN/MAX, filter, join+aggregate, scan, multi-aggregate, string_agg, and array_agg. Missing coverage:

| Missing Scenario | Why It Matters |
|------------------|---------------|
| **Window functions** | Complex SQL generation, PARTITION BY + ORDER BY |
| **CTEs (recursive and non-recursive)** | Multi-step differentiation |
| **LATERAL joins** | Recently added, complex delta logic |
| **EXCEPT / INTERSECT** | Set operations with count tracking |
| **Multi-source joins** (3+ tables) | Bilinear join rule with >2 inputs |
| **HAVING clause** | Post-aggregation filtering |
| **Composite primary keys** | Hash computation differences |

Each new property test would exercise the full pipeline: CDC trigger → change buffer → delta query generation → MERGE → correctness verification.

### 5.3 Schema Evolution E2E Tests

**Priority: Medium.** The `e2e_ddl_event_tests.rs` file has only 4 tests. Known untested scenarios:

- ADD COLUMN on a monitored source table → trigger rebuild
- DROP COLUMN on a monitored source table → error handling
- ALTER COLUMN TYPE on a source → type mismatch detection
- RENAME TABLE of a source → OID tracking
- CREATE INDEX on a source → no-op verification
- DROP source TABLE with multiple downstream stream tables

### 5.4 Concurrent Refresh Stress Tests

**Priority: Medium.** Only 3 tests in `e2e_concurrent_tests.rs`. Could add:

- Multiple stream tables on the same source, refreshed concurrently
- Concurrent DML on the source while refresh is running
- Advisory lock timeout and retry behavior under contention
- Background worker scheduling with overlapping refresh windows

### 5.5 Error Recovery Tests

**Priority: Medium.** The `e2e_error_tests.rs` file focuses on input validation errors. Missing:

- Refresh failure mid-MERGE (simulate disk full / connection drop)
- Background worker crash and restart
- Extension upgrade (`ALTER EXTENSION pg_trickle UPDATE`)
- Out-of-memory during large delta materialization
- Transaction abort during `create_stream_table()` → verify cleanup

### 5.6 Write-Side Overhead Benchmarks

**Priority: Low.** A benchmark plan exists in [`PLAN_TRIGGERS_OVERHEAD.md`](../performance/PLAN_TRIGGERS_OVERHEAD.md) to measure CDC trigger impact on DML throughput (INSERT/UPDATE/DELETE ops/sec with and without triggers). This would complement the existing refresh benchmarks by quantifying the **write-side cost** of monitoring a source table. Not yet implemented.

### 5.7 Expand Integration Tests as Version-Compatibility Canaries

**Priority: Medium** (upgraded from Low). The `catalog_compat_tests.rs` file (13 tests) demonstrated that integration tests have a unique role: they run **without building the extension Docker image** (~40s vs ~5-15 min) and catch PostgreSQL version-specific behavior changes early.

Candidates for new catalog compatibility tests:

| Test | What It Catches |
|------|-----------------|
| `pg_get_viewdef` for complex views (CTEs, LATERAL, window functions) | Semicolon/formatting changes per PG version |
| `pg_catalog.pg_proc` volatility column type | Type changes in system catalogs |
| `current_setting('search_path')` parsing with `$user` | search_path edge cases |
| `relkind` for partitioned indexes (`relkind='I'`) | New relkind values in PG18+ |
| `information_schema` vs `pg_catalog` type name differences | ORM-like assumptions |

### 5.8 Systematic "Pure Function Extraction" for SPI Code

**Priority: Medium.** The E2E failure investigation (§4.7) showed that bugs in SPI-dependent code are invisible to unit tests. The pattern that works:

1. **Extract pure decision logic** from SPI-calling functions into standalone functions
2. **Unit-test the pure function** exhaustively (all branches)
3. **Keep the SPI glue** thin and tested only via E2E

Examples already done:
- `classify_relkind(relkind: &str) -> String` — extracted from `classify_source_relation()`
- `strip_view_definition_suffix(raw: &str) -> String` — extracted from `get_view_definition()`

Candidates for future extraction:

| SPI Function | Extractable Logic |
|-------------|-------------------|
| `resolve_rangevar_schema()` | Schema selection from search_path ordering |
| `check_source_volatility()` | Volatility classification rules |
| `build_change_buffer_table_sql()` | DDL SQL construction from column metadata |
| `compute_refresh_action()` | Decision tree for FULL vs DIFFERENTIAL vs SKIP |

### 5.9 Fuzz Testing for the Parser

**Priority: Low.** The SQL parser (`src/dvm/parser.rs`, ~13,000 lines) handles arbitrary SQL from `pg_sys::raw_parser()`. Fuzz testing with `cargo-fuzz` or `proptest` could uncover:

- Panics on unexpected parse tree shapes
- Incorrect SQL generation for exotic syntax
- Memory safety issues in unsafe FFI boundaries

This requires a running PostgreSQL instance (for `pg_sys::raw_parser()`), so it would need to be done as an E2E fuzzing harness.

---

## 6. Test Commands Reference

```bash
# ── Unit tests (fast, no Docker) ──
just test-unit                    # 854 tests, ~5 seconds

# ── Integration tests (bare PG containers) ──
just test-integration             # 101 tests, ~3-4 minutes

# ── E2E tests (requires Docker image) ──
just build-e2e-image              # Build Docker image (~5-15 min first time)
just test-e2e                     # 340 tests, ~10-15 min (rebuilds image)
just test-e2e-fast                # Skip image rebuild

# ── All tests ──
just test-all                     # Unit + integration + E2E + pgrx

# ── Benchmarks ──
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_full_matrix

# ── Coverage ──
just coverage                     # Unit coverage (HTML + LCOV)
just coverage-text                # Unit coverage (terminal)
just coverage-e2e                 # E2E instrumented coverage (full pipeline)
just coverage-e2e-fast            # Skip Docker image rebuild

# ── Lint ──
just fmt                          # Format code
just lint                         # Format check + clippy
```

---

## 7. Summary

| Aspect | Status | Notes |
|--------|--------|-------|
| Unit tests | **854 tests, ~63% coverage** | Structural ceiling — 86% of remaining uncovered lines need a DB |
| E2E tests | **340 tests** | Comprehensive, exercising full SQL API (4 pre-existing failures tracked) |
| Integration tests | **101 tests** | Now includes 13 catalog compatibility canaries for PG version behavior |
| Property tests | **11 tests** | Found real correctness bugs; need expansion |
| Benchmarks | **16 benchmarks** | Refresh performance matrix, trigger overhead planned |
| E2E coverage infra | **Built, not in CI** | Projected 75-85% combined coverage |
| CI coverage tracking | **Not started** | Highest-priority improvement |
| Pure function extraction | **In progress** | `classify_relkind`, `strip_view_definition_suffix` extracted; more candidates identified |
| Fuzz testing | **Not started** | Parser is the primary target |

The testing strategy is mature for an extension at this stage. The main gaps are: **(1)** CI integration for combined E2E coverage, **(2)** more property tests for recently added SQL features, **(3)** schema evolution and concurrent stress tests, and **(4)** systematic extraction of pure functions from SPI-dependent code to bring more logic under unit test coverage.
