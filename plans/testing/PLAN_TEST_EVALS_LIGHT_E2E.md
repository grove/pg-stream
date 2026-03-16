# Light E2E Test Suite — Deep Evaluation Report

> **Date:** 2025-03-16
> **Scope:** All 47 files in `scripts/run_light_e2e_tests.sh` LIGHT_E2E_TESTS
> **Goal:** Assess coverage confidence and identify mitigations to harden the suite

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Harness Architecture & Limitations](#harness-architecture--limitations)
3. [Cross-Cutting Findings](#cross-cutting-findings)
4. [Per-File Analysis](#per-file-analysis)
5. [Priority Mitigations](#priority-mitigations)
6. [Appendix: Summary Table](#appendix-summary-table)

---

## Executive Summary

The Light E2E suite comprises **47 test files** with approximately **~670 test
functions** running against a stock `postgres:18.3` container with bind-mounted
extension artifacts. No background worker, scheduler, or
`shared_preload_libraries` is available.

**Confidence level: MODERATE-HIGH (≈75%).**

The suite excels at **differential correctness validation** — the majority of
DVM-critical tests use `assert_st_matches_query()` to compare stream table
contents against re-executing the defining query. Multi-layer DAG pipelines,
aggregate coverage, HAVING transitions, and multi-cycle stress tests are
gold-standard.

However, significant gaps undermine confidence:

| Severity | Finding | Impact |
|----------|---------|--------|
| **CRITICAL** | `light.rs` uses `EXCEPT` (set) not `EXCEPT ALL` (multiset) in `assert_st_matches_query` | Duplicate-row bugs invisible in light E2E |
| **CRITICAL** | `rows_from_tests` verifies only row counts, never contents | Rewrite bugs pass silently |
| **HIGH** | `expression_tests` (41 tests) uses row counts only, no `assert_st_matches_query` | Expression evaluation errors invisible |
| **HIGH** | `ivm_tests` (26 tests) uses row counts only, no `assert_st_matches_query` | Core IVM correctness undertested |
| **HIGH** | `getting_started_tests` — no `assert_st_matches_query` on full results | Spurious/missing rows missed |
| **MEDIUM** | `monitoring_tests` — existence checks only, no value validation | Staleness/history never verified |
| **MEDIUM** | `create_or_replace_tests` — zero error-path tests | Failure modes untested |
| **MEDIUM** | `concurrent_tests` — smoke tests ("doesn't crash"), no correctness | Race conditions pass silently |

---

## Harness Architecture & Limitations

**Harness file:** `tests/e2e/light.rs`
**Feature gate:** `--features light-e2e` (selected in `tests/e2e/mod.rs`)
**Runner:** `scripts/run_light_e2e_tests.sh`

### What Light E2E provides
- Stock `postgres:18.3` Docker container via testcontainers
- Extension loaded via `CREATE EXTENSION IF NOT EXISTS pg_trickle CASCADE`
- Bind-mounted `.so` and `.control` from `cargo pgrx package` output
- Full SQL API surface: `create_stream_table()`, `refresh_stream_table()`,
  `drop_stream_table()`, `alter_stream_table()`, etc.
- CDC triggers installed and functional
- Manual refresh works

### What Light E2E cannot test
- **Background worker / scheduler** — no `shared_preload_libraries`
- **Auto-refresh** — `wait_for_auto_refresh()` always returns `false`
- **Scheduler gating** — `wait_for_scheduler()` always returns `false`
- **GUC availability not guaranteed** — `ALTER SYSTEM SET pg_trickle.*` may fail
- **WAL-level features** — WAL decoder not loaded

### Critical harness defect: EXCEPT vs EXCEPT ALL

The `assert_st_matches_query()` implementation in `tests/e2e/light.rs`
(lines 563–577) uses plain `EXCEPT` for set comparison:

```sql
(SELECT cols FROM st_table EXCEPT (defining_query))
UNION ALL
((defining_query) EXCEPT SELECT cols FROM st_table)
```

The full E2E harness in `tests/e2e/mod.rs` was fixed in PR #208 to use
`EXCEPT ALL` for proper **multiset** (bag) equality. Light.rs was missed.

**Impact:** Any test where the stream table produces the right set of
*distinct* rows but incorrect *multiplicities* will pass in light E2E but
fail in full E2E. This directly affects:
- `e2e_keyless_duplicate_tests` — tests duplicate-row correctness
- `e2e_set_operation_tests` — tests INTERSECT ALL / EXCEPT ALL
- Any aggregate test where GROUP BY accidentally deduplicates

**Mitigation:** Apply the same `EXCEPT ALL` fix to `tests/e2e/light.rs`.
This is the single highest-priority fix.

### Tests gated out of light E2E

Only `e2e_watermark_gating_tests.rs` has `#[cfg(not(feature = "light-e2e"))]`
annotations, gating 3 scheduler-dependent tests:
- `test_scheduler_skips_misaligned_watermark`
- `test_scheduler_resumes_after_watermark_alignment`
- `test_scheduler_respects_tolerance`

All other tests in the 47-file allowlist execute unconditionally.

---

## Cross-Cutting Findings

### 1. `assert_st_matches_query` adoption is inconsistent

| Category | Files using it extensively | Files NOT using it |
|----------|--------------------------|-------------------|
| DVM core (aggregates, HAVING, set ops, subqueries) | ✅ 12 files | — |
| DAG pipelines (pipeline, multi-cycle, mixed-mode) | ✅ 4 files | — |
| Expression evaluation | — | ❌ `expression_tests` (41 tests) |
| Core IVM loop | — | ❌ `ivm_tests` (26 tests) |
| CRUD operations | — | ❌ `create_tests`, `alter_tests` |
| Tutorial walkthrough | — | ❌ `getting_started_tests` |
| TopK queries | — | ❌ `topk_tests` (60+ tests, uses counts/min/max) |
| Monitoring/meta | — | ❌ `monitoring_tests`, `phase4_ergonomics_tests` |
| Lateral subqueries | — | ❌ `lateral_subquery_tests` |
| ROWS FROM rewriting | — | ❌ `rows_from_tests` |

### 2. Error-path testing is sparse

Most files test only the happy path. Files with meaningful error-path coverage:
- `e2e_error_tests` — dedicated error file (but messages rarely verified)
- `e2e_guard_trigger_tests` — 4 error tests with message checks ✅
- `e2e_watermark_gating_tests` — rejection tests with message checks ✅
- `e2e_topk_tests` — 3 rejection tests ✅
- `e2e_phase4_ergonomics_tests` — GUC removal, NULL schedule ✅

Files with ZERO error-path tests:
`create_or_replace_tests`, `cdc_tests`, `cte_tests`, `expression_tests`,
`ivm_tests`, `lateral_tests`, `lateral_subquery_tests`, `full_join_tests`,
`window_tests`, `multi_window_tests`, `set_operation_tests`,
`scalar_subquery_tests`, `sublink_or_tests`, `all_subquery_tests`,
`keyless_duplicate_tests`, `rows_from_tests`, `pipeline_dag_tests`,
`mixed_mode_dag_tests`, `multi_cycle_dag_tests`, `snapshot_consistency_tests`,
`aggregate_coverage_tests`, `having_transition_tests`,
`differential_gaps_tests`

### 3. Row-count-only assertions hide bugs

Several test files rely exclusively on row counts (e.g., `assert_eq!(count, 5)`)
without checking actual content. If a query returns the right number of rows but
with wrong values, these tests pass. Affected files:
- **`rows_from_tests`** — 6 tests, all count-only (CRITICAL)
- **`expression_tests`** — 41 tests, almost all count-only (HIGH)
- **`ivm_tests`** — 26 tests, count + existence only (HIGH)
- **`lateral_tests` (FULL mode)** — 5 tests, count-only (MEDIUM)

### 4. Smoke-test-level files

These files verify "it doesn't crash" rather than "it produces correct results":
- **`concurrent_tests`** — tests concurrent refresh doesn't deadlock/crash
- **`smoke_tests`** — basic infrastructure verification
- **`coverage_error_tests`** (partially) — some tests verify error occurs but
  not content

---

## Per-File Analysis

### e2e_smoke_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~5 |
| Primary assertion | Infrastructure checks |
| `assert_st_matches_query` | 0 |
| Error paths | Some |
| Risk | LOW |

**What it tests:** Extension loads, `CREATE EXTENSION` succeeds, basic smoke
signals that the harness is functional.

**Assessment:** Appropriate for its role. No mitigations needed — this is
correctly a minimal infrastructure check.

---

### e2e_create_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~30 |
| Primary assertion | Catalog entry verification, status checks |
| `assert_st_matches_query` | ZERO |
| Error paths | Some (invalid inputs) |
| Risk | MEDIUM |

**What it tests:** Stream table creation with various SQL constructs. Verifies
catalog entries, refresh modes, schedule storage, status values.

**Assessment:** Tests verify that creation *succeeds* and catalog metadata is
correct, but never verify that the stream table *contents* match the defining
query after initial load.

**Mitigations:**
1. Add `assert_st_matches_query()` after creation for at least the 5 most
   complex queries (JOINs, aggregates, CTEs).
2. Add a test that creates with an invalid query (e.g., referencing
   non-existent table) and verifies the error message.

---

### e2e_drop_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~20 |
| Primary assertion | Catalog cleanup, trigger removal, storage removal |
| `assert_st_matches_query` | N/A (tests removal, not content) |
| Error paths | Some (drop non-existent) |
| Risk | LOW |

**What it tests:** Drop semantics: catalog entry removal, trigger cleanup,
storage table removal, IF EXISTS, CASCADE (DAG dependencies).

**Assessment:** Solid. Verifies the full cleanup lifecycle. No content
verification needed since the purpose is deletion.

---

### e2e_alter_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | Catalog field changes |
| `assert_st_matches_query` | ZERO |
| Error paths | Some (invalid mode transitions) |
| Risk | MEDIUM |

**What it tests:** `ALTER STREAM TABLE` for schedule, refresh mode, query
changes. Verifies catalog metadata updates.

**Assessment:** Tests check that catalog columns are updated but never verify
that altered stream tables still produce correct results after refresh. For
example, after changing refresh mode from FULL to DIFFERENTIAL, no test checks
that DIFFERENTIAL refresh works correctly.

**Mitigations:**
1. After each `ALTER` that changes refresh mode or query, add a DML → refresh →
   `assert_st_matches_query()` cycle.
2. Add test for ALTER on a stream table with existing data — verify data is
   preserved (or re-initialized) correctly.

---

### e2e_create_or_replace_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~10 |
| Primary assertion | No-op when exists, create when missing |
| `assert_st_matches_query` | ZERO |
| Error paths | ZERO |
| Risk | MEDIUM-HIGH |

**What it tests:** `CREATE OR REPLACE STREAM TABLE` semantics — creates when
absent, silently succeeds when present.

**Assessment:** Tests verify the API contract (no-op vs. create) but:
- Never verify that the original definition is preserved when "replace" is a
  no-op (could silently overwrite).
- No error-path tests at all.
- No test that replaces with a *different* query and verifies behavior.

**Mitigations:**
1. After the no-op case, query the catalog to verify the original
   `defining_query` is still stored.
2. Add at least one error-path test (e.g., replace with invalid SQL).
3. Add a test that verifies behavior when the defining query actually differs.

---

### e2e_error_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~25 |
| Primary assertion | `is_err()` checks |
| `assert_st_matches_query` | ZERO (error-only file) |
| Error paths | ALL |
| Risk | MEDIUM |

**What it tests:** Error paths for unsupported SQL constructs, invalid
parameters, constraint violations.

**Assessment:** Most tests check `result.is_err()` but don't verify error
*message content*. Some tests have been fixed in PR #208 to check messages,
but many remain message-free. There are also some tests that check `is_err()`
on queries that might be supported now (stale rejection checks).

**Mitigations:**
1. Add `.contains("expected substring")` checks to remaining `is_err()` tests.
2. Audit which "rejected" queries are now actually supported and remove/update
   stale tests.

---

### e2e_lifecycle_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | Status transitions, catalog state |
| `assert_st_matches_query` | 1 test |
| Error paths | Some (invalid transitions) |
| Risk | MEDIUM |

**What it tests:** Full lifecycle: CREATE → REFRESH → ALTER → DROP. Status
transitions (EMPTY → ACTIVE → ERROR → etc.).

**Assessment:** Good coverage of the lifecycle state machine but relies mostly
on status field checks rather than content validation.

**Mitigations:**
1. Add `assert_st_matches_query()` after each REFRESH step to verify content
   correctness, not just status.

---

### e2e_refresh_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~30 |
| Primary assertion | `assert_st_matches_query` extensively |
| `assert_st_matches_query` | 16+ uses |
| Error paths | Some |
| Risk | LOW |

**What it tests:** Full and differential refresh correctness after INSERT,
UPDATE, DELETE on source tables. Multi-step DML cycles, no-change refreshes,
multiple source tables.

**Assessment:** **Excellent.** One of the strongest files in the suite. Heavy
use of `assert_st_matches_query()` with multi-step DML operations.

---

### e2e_cdc_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | Change buffer contents, trigger existence |
| `assert_st_matches_query` | Some |
| Error paths | ZERO |
| Risk | MEDIUM |

**What it tests:** CDC trigger installation, change buffer population,
INSERT/UPDATE/DELETE capture, TRUNCATE handling.

**Assessment:** Good validation that CDC triggers capture changes correctly.
Missing error-path tests (e.g., what happens when source table is dropped
while triggers exist).

**Mitigations:**
1. Add a test for source-table DDL while CDC triggers are active.
2. Add `assert_st_matches_query()` after each CDC-captured operation.

---

### e2e_stmt_cdc_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | Bit-level trigger verification |
| `assert_st_matches_query` | Some |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Statement-level CDC trigger behavior. Verifies that triggers
fire on correct DML operations, with granular bit-level trigger type checks.

**Assessment:** **Very strong** — tests validate trigger configuration at a
lower level than most other files.

---

### e2e_expression_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~41 |
| Primary assertion | **Row counts only** |
| `assert_st_matches_query` | ZERO |
| Error paths | ZERO |
| Risk | **HIGH** |

**What it tests:** Expression evaluation in stream tables: arithmetic, string
functions, CASE/WHEN, COALESCE, NULLIF, type casts, date functions, array
operations, JSON operations, GREATEST/LEAST.

**Assessment:** **Critically weak.** 41 tests all verify row counts but never
check that expression *values* are correct. For example, a test for
`UPPER(name)` might check that 5 rows exist but not that the values are
actually uppercased. If the DVM passes through source values unchanged, all
tests pass.

**Mitigations (HIGH PRIORITY):**
1. Add `assert_st_matches_query()` to every expression test. This single
   change would transform 41 smoke tests into 41 correctness tests.
2. At minimum, add specific value assertions to the 10 most complex expression
   tests (CASE, COALESCE, JSON operations, array operations).

---

### e2e_property_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | Custom `assert_invariant` with EXCEPT ALL |
| `assert_st_matches_query` | Custom equivalent |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Property-based invariants on stream tables. Uses a custom
`assert_invariant` helper that performs EXCEPT ALL comparison.

**Assessment:** **Excellent.** Implements its own multiset comparison (EXCEPT
ALL). This is the gold standard — these tests would catch duplicate-row bugs
even in light E2E (bypassing the light.rs EXCEPT issue).

---

### e2e_cte_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~71 |
| Primary assertion | `assert_st_matches_query` extensively |
| `assert_st_matches_query` | Extensive |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Common Table Expressions: simple CTEs, recursive CTEs,
multi-CTE, CTE with aggregation, CTE with JOINs, CTE referenced multiple
times, CTE with window functions.

**Assessment:** **Very thorough.** 71 tests with heavy `assert_st_matches_query`
usage. The largest single test file in the suite.

**Mitigations:**
1. Add a few error-path tests (e.g., recursive CTE without termination
   condition, CTE with name collision).

---

### e2e_ivm_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~26 |
| Primary assertion | **Row counts and existence** |
| `assert_st_matches_query` | ZERO |
| Error paths | ZERO |
| Risk | **HIGH** |

**What it tests:** Core incremental view maintenance loop: INSERT → refresh →
verify, UPDATE → refresh → verify, DELETE → refresh → verify. Multiple source
tables, JOINs, aggregates.

**Assessment:** **Critically undertested for its importance.** These are the
core IVM tests — they should be the most rigorous in the suite. Instead, they
rely on row counts and existence checks. If the IVM engine produces rows with
wrong values (e.g., stale aggregates, wrong JOIN results), these tests pass.

**Mitigations (HIGH PRIORITY):**
1. Add `assert_st_matches_query()` after every refresh in every test.
2. This is arguably the highest-impact single change in the entire suite — 26
   tests instantly become correctness validators.

---

### e2e_concurrent_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~8 |
| Primary assertion | "Doesn't crash" |
| `assert_st_matches_query` | ZERO |
| Error paths | Some (expected conflicts) |
| Risk | MEDIUM |

**What it tests:** Concurrent refresh behavior — two refreshes at the same
time, concurrent DDL, etc. Verifies no deadlocks or panics.

**Assessment:** By nature, concurrent tests are difficult to make fully
deterministic. The "doesn't crash" approach is acceptable for detecting
deadlocks but misses correctness issues (e.g., lost updates under
concurrency).

**Mitigations:**
1. After each concurrent operation, add `assert_st_matches_query()` to verify
   that the final state is correct regardless of execution order.

---

### e2e_coverage_error_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | `is_err()` checks |
| `assert_st_matches_query` | ZERO |
| Error paths | ALL |
| Risk | MEDIUM |

**What it tests:** Error coverage for edge cases in the parser and DVM engine.
Includes cycle detection, unsupported constructs, and boundary conditions.

**Assessment:** The cycle detection test creates a chain A → B → C → A but
doesn't actually verify that the error mentions "cycle" or includes the
participating tables.

**Mitigations:**
1. Add error message content checks to all `is_err()` assertions.
2. Verify cycle detection error includes the cycle path.

---

### e2e_coverage_parser_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~20 |
| Primary assertion | Parse success/failure, source table detection |
| `assert_st_matches_query` | ZERO |
| Error paths | Some |
| Risk | LOW |

**What it tests:** Parser coverage: source table extraction, schema
qualification, edge cases in SQL parsing.

**Assessment:** Appropriate for parser-level tests. These don't need
content validation — they test metadata extraction.

---

### e2e_diamond_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~12 |
| Primary assertion | `assert_st_matches_query` in some tests |
| `assert_st_matches_query` | Partial |
| Error paths | ZERO |
| Risk | MEDIUM |

**What it tests:** Diamond dependency patterns where two intermediate stream
tables feed a single downstream. Tests none/auto/atomic diamond consistency
modes.

**Assessment:** Good coverage of the core diamond scenario. Some tests use
`assert_st_matches_query()` but not all.

**Mitigations:**
1. Add `assert_st_matches_query()` to all diamond tests after refresh.
2. Add test for diamond with three or more parents.

---

### e2e_dag_operations_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | Extensive |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** DAG-aware operations: cascade refresh, topological ordering,
multi-level refresh propagation.

**Assessment:** **Strong.** Uses `assert_st_matches_query()` as primary check.

---

### e2e_dag_topology_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~10 |
| Primary assertion | Topological ordering checks |
| `assert_st_matches_query` | Some |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** DAG topology correctness: dependency ordering, refresh
sequencing, level computation.

**Assessment:** Good. Tests the graph structure correctly.

---

### e2e_dag_error_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~10 |
| Primary assertion | Status checks, retry behavior |
| `assert_st_matches_query` | ZERO |
| Error paths | Partial |
| Risk | MEDIUM |

**What it tests:** DAG error handling: refresh failure propagation, error
recovery, consecutive error tracking.

**Assessment:** Tests check error status propagation but don't verify the
`consecutive_errors` catalog column value or that recovery actually
re-materializes correct data.

**Mitigations:**
1. After error recovery, add `assert_st_matches_query()` to verify correct
   re-materialization.
2. Assert `consecutive_errors` count value, not just status.

---

### e2e_dag_concurrent_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~8 |
| Primary assertion | Retry loop success |
| `assert_st_matches_query` | ZERO |
| Error paths | Some (expected conflicts) |
| Risk | MEDIUM |

**What it tests:** Concurrent DAG refresh operations — multiple refresh calls
on overlapping DAGs.

**Assessment:** Uses retry loops that mask CDC lag issues. If a refresh fails
due to timing, the test retries, which means intermittent failures are hidden.

**Mitigations:**
1. After retry-loop success, add `assert_st_matches_query()` to verify correctness.
2. Log retry counts to detect flakiness trends.

---

### e2e_dag_immediate_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~15 |
| Primary assertion | Immediate propagation checks |
| `assert_st_matches_query` | Some |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** IMMEDIATE mode in DAG pipelines: DML on source tables
propagates immediately through the DAG without explicit refresh.

**Assessment:** Good. Tests verify immediate propagation works.

---

### e2e_keyless_duplicate_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 8 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 7/8 |
| Error paths | ZERO |
| Risk | MEDIUM |

**What it tests:** Keyless (no PRIMARY KEY) tables with duplicate rows under
differential refresh. Uses ctid-based row identity.

**Assessment:** Strong use of `assert_st_matches_query()`. However, **in light
E2E, the EXCEPT-based comparison (not EXCEPT ALL) means duplicate-row
correctness is NOT actually verified**. A bug that returns 1 copy instead of 3
identical copies would pass.

**Mitigations:**
1. Fix `light.rs` to use `EXCEPT ALL` (fixes all keyless duplicate tests).
2. Add explicit count assertions for duplicate multiplicities as defense-in-depth.

---

### e2e_lateral_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 16 |
| Primary assertion | Counts (FULL), `assert_st_matches_query` (DIFF) |
| `assert_st_matches_query` | 9/16 |
| Error paths | ZERO |
| Risk | MEDIUM |

**What it tests:** LATERAL set-returning functions (jsonb_array_elements,
jsonb_each, unnest) in FULL and DIFFERENTIAL modes.

**Assessment:** DIFFERENTIAL tests are strong (9 use `assert_st_matches_query`).
FULL mode tests (5) only check counts — if an SRF returned wrong values but
correct count, tests pass.

**Mitigations:**
1. Add `assert_st_matches_query()` to the 5 FULL mode tests.
2. Add test for non-empty → empty array transition.

---

### e2e_lateral_subquery_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 13 |
| Primary assertion | Specific values + counts |
| `assert_st_matches_query` | ZERO |
| Error paths | ZERO |
| Risk | **HIGH** |

**What it tests:** LATERAL subqueries: correlated subqueries returning multiple
rows, LIMIT, LEFT JOIN LATERAL.

**Assessment:** Relies on manual assertions (counts, specific values, boolean
checks). Without `assert_st_matches_query()`, if the stream table accidentally
includes extra rows that the manual checks don't look for, tests pass.

**Mitigations:**
1. Add `assert_st_matches_query()` to all 13 tests.
2. Particularly important for LEFT JOIN LATERAL where NULL semantics matter.

---

### e2e_full_join_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 6 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 6/6 (100%) |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** FULL OUTER JOIN differential correctness including NULL join
keys and row migration.

**Assessment:** **Excellent.** 100% `assert_st_matches_query` coverage.

**Mitigations:**
1. Add edge case: delete all rows from one side.
2. Add edge case: multiple unmatched rows on both sides simultaneously.

---

### e2e_window_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 24 |
| Primary assertion | Values + `assert_st_matches_query` |
| `assert_st_matches_query` | 4/24 |
| Error paths | Light (acceptance checks) |
| Risk | MEDIUM |

**What it tests:** Window functions: ROW_NUMBER, RANK, DENSE_RANK, SUM OVER,
LAG, LEAD. Nested window expressions (EC-03). Partition key changes (G1.2).

**Assessment:** Mixed. Complex tests (partition key changes, multiple
partitions) use `assert_st_matches_query()`. Simpler tests and EC-03 rewrite
acceptance tests only check `result.is_ok()` without verifying output.

**Mitigations:**
1. EC-03 rewrite acceptance tests (tests 13–17): if they now test acceptance
   instead of rejection, add `assert_st_matches_query()` to verify the rewrite
   produces correct output.
2. Add `assert_st_matches_query()` to FULL mode tests.

---

### e2e_multi_window_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 7 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 7/7 (100%) |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Multiple window functions with different PARTITION BY/ORDER
BY, frame clauses (ROWS/RANGE), LAG/LEAD, ranking functions.

**Assessment:** **Excellent.** 100% `assert_st_matches_query` coverage.

**Mitigations:**
1. Add test with LAG/LEAD returning NULL (first/last row in partition).
2. Add test with ties in ranking functions.

---

### e2e_set_operation_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 7 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 7/7 (100%) |
| Error paths | ZERO |
| Risk | MEDIUM |

**What it tests:** INTERSECT / EXCEPT differential correctness including ALL
variants, multi-way chains, multi-column operations.

**Assessment:** Good `assert_st_matches_query` coverage, but **the light.rs
EXCEPT bug means INTERSECT ALL / EXCEPT ALL multiplicity is not actually
verified** — the assert itself uses EXCEPT (not EXCEPT ALL).

**Mitigations:**
1. Fix `light.rs` EXCEPT → EXCEPT ALL (critical).
2. Add test with NULL values in set operations.
3. Add three-way+ chain test.

---

### e2e_topk_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~60 |
| Primary assertion | Counts, min/max, catalog checks |
| `assert_st_matches_query` | ZERO |
| Error paths | YES (3 rejection tests) |
| Risk | MEDIUM |

**What it tests:** TopK queries (ORDER BY … LIMIT N) in FULL, DIFFERENTIAL,
and IMMEDIATE modes. Catalog metadata, OFFSET, FETCH FIRST, rejection cases.

**Assessment:** Comprehensive test count (60+) but relies on count/min/max
assertions rather than full content comparison. This is partially acceptable
since TopK has specific semantics (correct top-N, not full set), but missing
`assert_st_matches_query()` means the actual row contents are unverified.

**Mitigations:**
1. Add `assert_st_matches_query()` to at least the 10 most critical TopK tests
   (basic creation, DIFFERENTIAL refresh, IMMEDIATE mode).
2. Verify IMMEDIATE mode tests actually trigger immediate propagation (not just
   explicit refresh fallback).

---

### e2e_scalar_subquery_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 4 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 4/4 (100%) |
| Error paths | ZERO |
| Risk | LOW-MEDIUM |

**What it tests:** Scalar subqueries in SELECT, WHERE, and correlated positions.

**Assessment:** Strong assertion quality. Limited scope — only 4 tests.

**Mitigations:**
1. Add tests for: scalar in JOIN ON, multiple scalar subqueries in one query,
   scalar returning multiple rows (should error).

---

### e2e_sublink_or_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 4 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 4/4 (100%) |
| Error paths | ZERO |
| Risk | LOW-MEDIUM |

**What it tests:** Sublink expressions (EXISTS, NOT EXISTS, IN) combined with OR.

**Assessment:** Strong assertion quality. Limited scope — no NOT IN, ANY/ALL,
nested sublinks.

**Mitigations:**
1. Add NOT IN test.
2. Add nested sublink test (EXISTS with inner IN).

---

### e2e_all_subquery_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 10 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 7/10 |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** ALL (subquery) support with various operators, NULL handling,
empty subquery edge cases.

**Assessment:** Strong. Tests all major operators (<, >, >=, =, <>) and NULL
semantics.

**Mitigations:**
1. Add test for NULL in outer value (not just inner subquery).
2. Add FULL mode parity tests.

---

### e2e_aggregate_coverage_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 18 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 18/18 (100%) |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Every supported aggregate function: SUM, AVG, COUNT, MIN,
MAX, DISTINCT variants, STRING_AGG, ARRAY_AGG, BOOL_AND/OR/EVERY, BIT_AND/OR,
JSON(B)_AGG, JSON(B)_OBJECT_AGG, PERCENTILE_CONT/DISC, MODE.

**Assessment:** **Gold standard.** 100% `assert_st_matches_query` coverage with
I/U/D cycles on every aggregate type.

---

### e2e_having_transition_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 7 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 7/7 (100%) |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** HAVING threshold transitions: groups appearing/disappearing,
oscillation, row migration between groups.

**Assessment:** **Excellent.** Tests the critical path where groups cross
HAVING thresholds, which is notoriously hard to get right in IVM.

---

### e2e_view_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 16 |
| Primary assertion | Mixed (`assert_st_matches_query` + counts) |
| `assert_st_matches_query` | ~50% |
| Error paths | YES (matview rejection, DROP VIEW) |
| Risk | MEDIUM |

**What it tests:** View inlining, nested views, materialized view rejection,
DDL hooks (CREATE OR REPLACE VIEW, DROP VIEW), TRUNCATE propagation.

**Assessment:** Good breadth. Main gap: after DDL events (CREATE OR REPLACE
VIEW, DROP VIEW), tests verify status but don't verify refresh correctness
post-event.

**Mitigations:**
1. After `CREATE OR REPLACE VIEW`, verify a refresh with
   `assert_st_matches_query()` uses the new view definition.
2. Add `assert_st_matches_query()` to TRUNCATE propagation test.
3. Test view containing UNION.

---

### e2e_monitoring_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 6 |
| Primary assertion | **Existence checks only** |
| `assert_st_matches_query` | ZERO |
| Error paths | ZERO |
| Risk | **MEDIUM-HIGH** |

**What it tests:** Monitoring views: `pgt_status()`, `stream_tables_info`,
`pg_stat_stream_tables`, staleness detection, refresh history.

**Assessment:** **Weak.** Tests verify that monitoring views exist and are
queryable but never validate that the values are correct. Staleness test doesn't
advance time to verify stale=true. Refresh history test doesn't check column
values.

**Mitigations:**
1. `test_stale_detection`: After refreshing, wait > schedule interval and verify
   `stale = true`. (May require light-E2E workaround since no scheduler.)
2. `test_refresh_history_records`: Verify `initiated_by`, `status`,
   `start_time`, `end_time` values, not just column existence.
3. `test_pg_stat_stream_tables_view`: After manual refresh, verify
   `total_refreshes` > 0.

---

### e2e_getting_started_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 8 |
| Primary assertion | Specific hardcoded values |
| `assert_st_matches_query` | ZERO |
| Error paths | ZERO |
| Risk | **HIGH** |

**What it tests:** Tutorial walkthrough from GETTING_STARTED.md: recursive CTE,
LEFT JOIN + GROUP BY, cascading refreshes across 3 layers, DROP cleanup.

**Assessment:** Tests verify specific expected values (e.g., "Alice's salary
total should be 580000") but never check that the **entire result set** matches.
Spurious extra rows or missing rows would pass. Also, `test_getting_started_step7_drop_in_order`
doesn't verify base tables remain intact after dropping stream tables.

**Mitigations (HIGH PRIORITY):**
1. Add `assert_st_matches_query()` at the end of each step to verify full
   result set correctness.
2. After DROP, verify source tables still exist with original data.
3. Add a negative test: DROP in wrong order should fail gracefully.

---

### e2e_guard_trigger_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 5 |
| Primary assertion | Error messages + `assert_st_matches_query` |
| `assert_st_matches_query` | 1/5 |
| Error paths | 4/5 |
| Risk | LOW |

**What it tests:** Guard triggers blocking direct DML (INSERT/UPDATE/DELETE/
TRUNCATE) on stream tables.

**Assessment:** **Well-structured.** 4 error tests with message validation +
1 success test with correctness check.

---

### e2e_phase4_ergonomics_tests.rs

| Metric | Value |
|--------|-------|
| Tests | ~20 |
| Primary assertion | Mixed (catalog, warnings, status) |
| `assert_st_matches_query` | ZERO |
| Error paths | Several |
| Risk | MEDIUM |

**What it tests:** Ergonomic features: refresh history, quick_health view,
create_if_not_exists, calculated schedule, removed GUCs, ALTER warnings.

**Assessment:** Warning tests use `.contains()` substring matching which is
fragile to wording changes. `create_if_not_exists` gap: doesn't verify original
query is preserved after no-op call.

**Mitigations:**
1. After `create_if_not_exists` no-op, query catalog to verify original
   `defining_query`.
2. Consider making warning checks more resilient (check key words rather than
   exact phrases).

---

### e2e_pipeline_dag_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 18 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | Most tests |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Realistic multi-level DAG pipelines: Nexmark auction (3
levels), E-commerce analytics (4 levels), IoT telemetry (3 levels).

**Assessment:** **Exceptional.** The most realistic integration tests in the
suite. `assert_st_matches_query()` at every layer after every mutation.

---

### e2e_mixed_mode_dag_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 5 |
| Primary assertion | `assert_st_matches_query` |
| `assert_st_matches_query` | 5/5 (100%) |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Mixed FULL/DIFFERENTIAL/IMMEDIATE modes in DAG pipelines.

**Assessment:** **Strong.** Tests mode transitions and mixed cascade behavior.

**Mitigations:**
1. `test_mixed_immediate_leaf` does explicit refresh as fallback but doesn't
   confirm IMMEDIATE trigger actually fired. Add validation that no
   explicit refresh was needed.

---

### e2e_multi_cycle_dag_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 6 |
| Primary assertion | `assert_st_matches_query` (intensive) |
| `assert_st_matches_query` | 60+ calls across tests |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Multi-cycle stress tests: 10 INSERT cycles, 5 mixed DML
cycles, no-op drift detection, group elimination/revival, bulk mutation (100
INSERTs + 50 UPDATEs + 30 DELETEs), diamond multi-cycle.

**Assessment:** **Gold standard for stress testing.** `assert_pipeline_correct()`
helper calls `assert_st_matches_query()` on all layers after every cycle.

---

### e2e_rows_from_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 6 |
| Primary assertion | **Row counts only** |
| `assert_st_matches_query` | ZERO |
| Error paths | ZERO |
| Risk | **CRITICAL** |

**What it tests:** `ROWS FROM(f1(), f2(), ...)` rewriting: multi-unnest merge,
mixed SRF handling.

**Assessment:** **The weakest test file in the entire suite.** All 6 tests
verify only row counts. If the rewriting is broken and produces NULL values in
all columns, tests still pass (correct count, wrong values). No error paths.

**Mitigations (CRITICAL PRIORITY):**
1. Add `assert_st_matches_query()` to all 6 tests.
2. Add specific value assertions for NULL pairing behavior.
3. Add error-path test for invalid SRF combinations.

---

### e2e_snapshot_consistency_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 5 |
| Primary assertion | `assert_st_matches_query` + cross-source invariants |
| `assert_st_matches_query` | Most tests |
| Error paths | ZERO |
| Risk | LOW |

**What it tests:** Cross-source snapshot consistency: overlapping sources,
diamond convergence, interleaved mutations.

**Assessment:** **Very strong.** Adds cross-source invariant checks on top of
standard `assert_st_matches_query()`.

---

### e2e_watermark_gating_tests.rs

| Metric | Value |
|--------|-------|
| Tests | 26 (23 light + 3 full-only) |
| Primary assertion | Mixed (CRUD, status, `assert_st_matches_query`) |
| `assert_st_matches_query` | Some |
| Error paths | YES (rejections) |
| Risk | LOW-MEDIUM |

**What it tests:** Watermark advancement, monotonicity, groups, tolerance,
alignment detection, ST gating. Scheduler tests gated to full E2E only.

**Assessment:** Good coverage of watermark API. Light-mode tests cover CRUD
and gating. Scheduler-level verification only in full E2E (acceptable given
light-E2E limitations).

**Mitigations:**
1. Add boundary test for tolerance (exactly at tolerance limit).

---

## Priority Mitigations

### P0 — Critical (should block next release)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 1 | Fix `tests/e2e/light.rs` `assert_st_matches_query` to use `EXCEPT ALL` | All keyless/set-op tests gain multiset verification | Small |
| 2 | Add `assert_st_matches_query` to `rows_from_tests` (6 tests) | 6 smoke tests → 6 correctness tests | Small |
| 3 | Add `assert_st_matches_query` to `expression_tests` (41 tests) | 41 smoke tests → 41 correctness tests | Medium |
| 4 | Add `assert_st_matches_query` to `ivm_tests` (26 tests) | Core IVM loop fully validated | Medium |

### P1 — High (should address soon)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 5 | Add `assert_st_matches_query` to `getting_started_tests` | Tutorial walkthrough fully validated | Small |
| 6 | Add `assert_st_matches_query` to `lateral_subquery_tests` | 13 tests gain full verification | Small |
| 7 | Add `assert_st_matches_query` to `topk_tests` (10 most critical) | Top-N content verified, not just counts | Medium |
| 8 | Add error-path tests to `create_or_replace_tests` | Currently zero error tests | Small |
| 9 | Add error message checks to `error_tests` | Most tests only check `is_err()` | Medium |

### P2 — Medium (address during regular maintenance)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 10 | Harden `monitoring_tests` with value validation | Staleness/history actually verified | Medium |
| 11 | Add `assert_st_matches_query` to `create_tests` (5 complex cases) | Creation correctness verified | Small |
| 12 | Add `assert_st_matches_query` to `alter_tests` post-alter | Altered STs verified correct | Small |
| 13 | Verify `consecutive_errors` in `dag_error_tests` | Error tracking validated | Small |
| 14 | Add `assert_st_matches_query` to `concurrent_tests` post-op | Concurrency correctness verified | Small |
| 15 | Add correctness check after DDL hooks in `view_tests` | Post-reinit correctness verified | Small |

### P3 — Low (backlog)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 16 | Add error-path tests to CTE, expression, lateral files | Error handling coverage | Medium |
| 17 | Add NULL edge cases to set_operation, window, lateral | Edge case coverage | Medium |
| 18 | Add FETCH NEXT syntax test to topk | SQL compliance coverage | Small |
| 19 | Add multi-row unmatched FULL JOIN test | Edge case coverage | Small |
| 20 | Add LAG/LEAD NULL test to multi_window | Edge case coverage | Small |

---

## Appendix: Summary Table

| File | Tests | Assertion Quality | `assert_st_matches_query` | Error Paths | Risk |
|------|-------|-------------------|---------------------------|-------------|------|
| smoke | ~5 | Infrastructure | 0% | Some | LOW |
| create | ~30 | Catalog only | 0% | Some | MEDIUM |
| drop | ~20 | Cleanup checks | N/A | Some | LOW |
| alter | ~15 | Catalog only | 0% | Some | MEDIUM |
| create_or_replace | ~10 | No-op checks | 0% | **ZERO** | MED-HIGH |
| error | ~25 | `is_err()` | 0% | ALL | MEDIUM |
| lifecycle | ~15 | Status checks | ~7% | Some | MEDIUM |
| refresh | ~30 | **Excellent** | ~53% | Some | LOW |
| cdc | ~15 | Buffer checks | Partial | **ZERO** | MEDIUM |
| stmt_cdc | ~15 | Bit-level | Partial | **ZERO** | LOW |
| expression | ~41 | **Counts only** | **0%** | **ZERO** | **HIGH** |
| property | ~15 | Custom EXCEPT ALL | Custom 100% | **ZERO** | LOW |
| cte | ~71 | **Excellent** | Extensive | **ZERO** | LOW |
| ivm | ~26 | **Counts only** | **0%** | **ZERO** | **HIGH** |
| concurrent | ~8 | Smoke only | 0% | Some | MEDIUM |
| coverage_error | ~15 | `is_err()` | 0% | ALL | MEDIUM |
| coverage_parser | ~20 | Parse checks | 0% | Some | LOW |
| diamond | ~12 | Partial | Partial | **ZERO** | MEDIUM |
| dag_operations | ~15 | **Excellent** | Extensive | **ZERO** | LOW |
| dag_topology | ~10 | Ordering | Some | **ZERO** | LOW |
| dag_error | ~10 | Status checks | 0% | Partial | MEDIUM |
| dag_concurrent | ~8 | Retry-loop | 0% | Some | MEDIUM |
| dag_immediate | ~15 | Immediate checks | Some | **ZERO** | LOW |
| keyless_duplicate | 8 | **Excellent** | 88% | **ZERO** | MEDIUM* |
| lateral | 16 | Mixed | 56% | **ZERO** | MEDIUM |
| lateral_subquery | 13 | Manual only | **0%** | **ZERO** | **HIGH** |
| full_join | 6 | **Excellent** | **100%** | **ZERO** | LOW |
| window | 24 | Mixed | 17% | Light | MEDIUM |
| multi_window | 7 | **Excellent** | **100%** | **ZERO** | LOW |
| set_operation | 7 | **Excellent** | **100%** | **ZERO** | MEDIUM* |
| topk | ~60 | Counts/min/max | 0% | YES | MEDIUM |
| scalar_subquery | 4 | **Excellent** | **100%** | **ZERO** | LOW-MED |
| sublink_or | 4 | **Excellent** | **100%** | **ZERO** | LOW-MED |
| all_subquery | 10 | **Excellent** | 70% | **ZERO** | LOW |
| aggregate_coverage | 18 | **Gold standard** | **100%** | **ZERO** | LOW |
| having_transition | 7 | **Excellent** | **100%** | **ZERO** | LOW |
| view | 16 | Mixed | ~50% | YES | MEDIUM |
| monitoring | 6 | **Existence only** | 0% | **ZERO** | **MED-HIGH** |
| getting_started | 8 | Hardcoded values | **0%** | **ZERO** | **HIGH** |
| guard_trigger | 5 | **Strong** | 20% | YES (4/5) | LOW |
| phase4_ergonomics | ~20 | Mixed | 0% | Several | MEDIUM |
| pipeline_dag | 18 | **Exceptional** | Most | **ZERO** | LOW |
| mixed_mode_dag | 5 | **Excellent** | **100%** | **ZERO** | LOW |
| multi_cycle_dag | 6 | **Gold standard** | 60+ calls | **ZERO** | LOW |
| rows_from | 6 | **Counts only** | **0%** | **ZERO** | **CRITICAL** |
| snapshot_consistency | 5 | **Excellent** | Most | **ZERO** | LOW |
| watermark_gating | 26 | Mixed | Some | YES | LOW-MED |

\* `keyless_duplicate` and `set_operation` are marked MEDIUM because the light.rs
EXCEPT (vs EXCEPT ALL) bug means their multiset assertions are weaker than
intended.

---

*End of report.*
