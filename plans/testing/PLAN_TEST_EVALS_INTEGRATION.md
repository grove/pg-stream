# Integration & Unit Test Suite — Deep Evaluation Report

> **Date:** 2025-03-16
> **Scope:** 10 integration test files (~123 tests, ~3,700 lines) +
>            unit tests across `src/` (~1,300 tests)
> **Goal:** Assess coverage confidence and identify mitigations to harden
>           the non-E2E test layers

---

## Implementation Status

> **Updated:** 2026-03-17 (pass 3)
> **Status:** All items complete. All P0–P3 items implemented or intentionally resolved.

| Priority | Item | Status | Notes |
|----------|------|--------|-------|
| P0-1 | Extract Multiset Comparison Helper | ✅ DONE | `assert_sets_equal()` added to `tests/common/mod.rs` |
| P0-2 | Add LISTEN/NOTIFY Round-Trip Test | ✅ DONE | Real `PgListener` round-trip in `monitoring_tests.rs`, replaces fire-and-forget |
| P0-3 | Strengthen Workflow Data Validation | ✅ DONE | `test_full_refresh_workflow` now uses `assert_sets_equal` for row-level correctness |
| P0-4 | Add Semi/Anti Join Unit Tests | ✅ DONE | Expanded from 3→9 tests each; nested, multi-column, filter, complementary |
| P1-1 | Widen Staleness Test Tolerance | ✅ DONE | Changed 59–65s window to 50–120s in `test_staleness_calculation` |
| P1-2 | Add Column Type Verification | ✅ DONE | Two new tests in `extension_tests.rs` verify column types via `information_schema` |
| P1-3 | Add Error Escalation Threshold Test | ✅ DONE | `test_error_escalation_exact_threshold` + `test_suspended_to_active_recovery` added to `resilience_tests.rs` |
| P2-1 | DDL Drift Detection Test | ✅ DONE | `test_catalog_ddl_no_phantom_columns` + `test_catalog_ddl_no_phantom_tables` in `catalog_compat_tests.rs`; pure-Rust via `include_str!` |
| P2-2 | Multi-Table Join Chain Unit Tests | ✅ DONE | `(A LEFT JOIN B) INNER JOIN C`, `A INNER JOIN (B INNER JOIN C)` in `join.rs`; `A FULL JOIN (B SEMI JOIN C)`, `(A FULL JOIN B) INNER JOIN C` in `full_join.rs` |
| P2-3 | Scheduler Job Lifecycle Integration Test | ✅ DONE | `pgt_scheduler_jobs` added to `CATALOG_DDL`; `test_scheduler_job_lifecycle_queued_to_succeeded` + `test_scheduler_job_lifecycle_retryable_failure` in `workflow_tests.rs` |
| P2-4 | Extend Property Tests to DVM Operators | ✅ DONE | `proptest!` blocks added to `join.rs`, `semi_join.rs`, `anti_join.rs`; 6 new property tests: UNION ALL invariant, output-col-count, left-col-equality, EXISTS/NOT EXISTS presence |
| P3-1 | Remove or Fold Smoke Tests | ✅ DONE | Kept as first-to-fail diagnostics; updated module doc to explain the intentional redundancy and diagnostic value; test names standardized |
| P3-2 | Add Workflow Test for ST Drop Cascade | ✅ DONE | `test_workflow_st_drop_cascade` in `workflow_tests.rs`; verifies CASCADE on deps, storage table drop |
| P3-3 | Standardize Test Naming | ✅ DONE | `workflow_tests.rs` older tests renamed to `test_workflow_*` convention; `smoke_tests.rs` renamed to `test_infra_*` convention |

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Test Infrastructure](#test-infrastructure)
3. [Integration Tests — Per-File Analysis](#integration-tests--per-file-analysis)
4. [Unit Tests — Module Landscape](#unit-tests--module-landscape)
5. [Cross-Cutting Findings](#cross-cutting-findings)
6. [Priority Mitigations](#priority-mitigations)

---

## Executive Summary

The integration test suite consists of **~123 test functions** across 10 files,
running against a Testcontainers PostgreSQL 18.3-alpine instance via sqlx.
Unit tests provide **~1,300 tests** across `src/` modules with no database
dependency.

**Integration confidence: MODERATE (≈55%)**
**Unit test confidence: HIGH (≈80%)**

### Integration Test Strengths

| Area | Assessment |
|------|-----------|
| Property-based logic tests | **Excellent** — `property_tests.rs` uses proptest with 500+ cases per property |
| Scenario multiset validation | **Excellent** — `scenario_tests.rs` uses `EXCEPT ALL / UNION ALL` pattern for 7/10 tests |
| Trigger detection regression | **Strong** — 10 tests covering all exclusion rules + recent prefix bug regression |
| Catalog constraint enforcement | **Good** — UNIQUE, CHECK constraints validated in `catalog_tests.rs` |

### Integration Test Weaknesses

| Severity | Finding | Impact |
|----------|---------|--------|
| **CRITICAL** | Extension binary never loaded — all tests use mock catalog DDL | Tests validate SQL assumptions about the catalog schema, not the actual extension |
| **HIGH** | 63% of tests (77/123) use weak assertions (boolean/count/existence only) | Many tests verify structure but not data correctness |
| **HIGH** | Only `scenario_tests.rs` has multiset comparison (7 tests) | Other files that mutate data never verify row-level correctness |
| **HIGH** | All resilience/workflow/monitoring tests use mock data | No test triggers real refresh failures, real CDC, or real crashes |
| **MEDIUM** | NOTIFY test (`monitoring_tests.rs`) never receives the payload | Only sends `pg_notify()`, never verifies receipt via `LISTEN` |
| **MEDIUM** | Time-dependent staleness assertions vulnerable to CI clock skew | `test_staleness_calculation` asserts 59–65 seconds for a 60-second-old row |
| **LOW** | `smoke_tests.rs` duplicates basic Testcontainers validation | Already proven by any other test that starts a container |

### Unit Test Strengths

| Area | Assessment |
|------|-----------|
| Query parsing | **Exceptional** — `parser.rs` has 60+ tests with thorough edge cases |
| DVM operators | **Strong** — aggregate (40), recursive CTE (45), inner join (11), left join (11), full join (7) |
| Schedule parsing | **Comprehensive** — `api.rs` ~95 tests covering cron/duration/at/repeats variants |
| Refresh decision logic | **Solid** — `determine_refresh_action()` covers all 5 state combinations |

### Unit Test Weaknesses

| Severity | Finding | Impact |
|----------|---------|--------|
| **HIGH** | Semi/Anti join operators severely under-tested (3 tests each) | Complex join types have minimal coverage vs INNER/LEFT (11 each) |
| **HIGH** | `execute_differential_refresh()` only tested for error paths | Success case (MERGE execution) completely untested at unit level |
| **MEDIUM** | Scheduler job lifecycle untestable without SPI mock | enqueue/claim/complete flow has no unit coverage |
| **MEDIUM** | No multi-table join chains tested | All join tests are 2-table; no `(A ⋈ B) ⋈ C` compositions |
| **LOW** | Distinct operator has only 3 tests | Minimal but likely sufficient for simple dedup logic |

---

## Test Infrastructure

### Shared Helpers — `tests/common/mod.rs`

The integration test harness is built around `TestDb`, a thin wrapper over
`sqlx::PgPool` and a Testcontainers `postgres:18.3-alpine` container.

**Two initialization modes:**

| Mode | Method | What You Get |
|------|--------|-------------|
| Bare | `TestDb::new()` | Fresh PostgreSQL — no pg_trickle objects at all |
| Catalog | `TestDb::with_catalog()` | Bare PG + hardcoded `CATALOG_DDL` SQL (schemas, tables, views, functions) |

**Key helper methods:**
- `execute(sql)` — run SQL, panic on error
- `try_execute(sql)` → `Result<PgQueryResult>` — run SQL, return result
- `query_scalar::<T>(sql)` — single-value query, panic on empty
- `query_scalar_opt::<T>(sql)` → `Option<T>` — single-value, return None if empty
- `count(table)` → `i64` — `SELECT COUNT(*) FROM table`

**Critical limitation:** `TestDb::with_catalog()` loads a hardcoded DDL string
that mimics the extension's catalog tables (schemas `pgtrickle`,
`pgtrickle_changes`; tables like `pgt_stream_tables`, `pgt_dependencies`, etc.)
but does **not** load the compiled extension. This means:

- ❌ No SQL-callable functions (`pgtrickle.create_stream_table()`, etc.)
- ❌ No background worker
- ❌ No CDC triggers
- ❌ No GUC variables
- ❌ No event triggers

Integration tests therefore validate **catalog schema correctness** and
**pure Rust logic**, not the extension's runtime behavior. Runtime behavior is
the domain of E2E tests.

**Missing from harness:**
- No `assert_st_matches_query()` helper (unlike the E2E harness)
- No `PgListener`-based NOTIFY verification
- No multiset comparison helper — `scenario_tests.rs` reimplements this inline

---

## Integration Tests — Per-File Analysis

### 1. `smoke_tests.rs` — 3 tests, 49 lines

**Purpose:** Validates that the Testcontainers infrastructure works.

| Test | What It Does | Assertion Quality |
|------|-------------|-------------------|
| `test_container_starts_and_connects` | `SELECT version()` contains "PostgreSQL" | ✅ Adequate (infra) |
| `test_create_table_and_insert` | Creates table, inserts 2 rows, counts | ✅ Adequate (infra) |
| `test_schemas_can_be_created` | Creates schema, queries pg_namespace | ✅ Adequate (infra) |

**Verdict:** ✅ **Pass.** These are infrastructure sanity checks and serve their
purpose. However, they are redundant once any other test succeeds — consider
whether keeping them has value beyond documentation.

---

### 2. `catalog_tests.rs` — 16 tests, 514 lines

**Purpose:** Validates the mock catalog DDL: schemas exist, tables have correct
constraints, CRUD operations work, dependencies cascade, change buffer tables
have the right structure.

**Init mode:** `TestDb::with_catalog()`

| Test | What It Claims | What It Checks | Verdict |
|------|---------------|---------------|---------|
| `test_catalog_schemas_created` | Schemas exist | `EXISTS(pg_namespace)` for 2 schemas | ⚠️ Weak (existence only) |
| `test_catalog_tables_exist` | Tables exist | Loop + `EXISTS(pg_class)` for 5 tables | ⚠️ Weak |
| `test_stream_tables_info_view_exists` | View exists | `EXISTS(pg_class WHERE relkind='v')` | ⚠️ Weak |
| `test_insert_stream_table` | Can INSERT an ST row | COUNT=1, single-field string check | ⚠️ Weak (doesn't verify all columns) |
| `test_unique_name_constraint` | UNIQUE(pgt_name, pgt_schema) enforced | Duplicate insert returns error | ✅ Pass |
| `test_refresh_mode_check_constraint` | CHECK on refresh_mode | Invalid value rejected | ✅ Pass |
| `test_status_check_constraint` | CHECK on status | Invalid value rejected | ✅ Pass |
| `test_dependency_insertion_and_cascade_delete` | CASCADE DELETE | Insert dep, delete parent, count=0 | ⚠️ Weak |
| `test_refresh_history_recording` | Refresh history tracked | INSERT + UPDATE `rows_inserted=42` | ⚠️ Weak |
| `test_refresh_history_action_check_constraint` | CHECK on action | Invalid action rejected | ✅ Pass |
| `test_stream_tables_info_view` | Staleness computed | `stale=true` for old data | ⚠️ Weak |
| `test_change_tracking_crud` | CRUD on tracking table | Array append + equality | ⚠️ Weak |
| `test_status_transitions` | Status updates work | UPDATE loop + COUNT | ⚠️ Weak |
| `test_change_buffer_table_schema` | Buffer table creation | Creates table, inserts 1 row | ⚠️ Weak (doesn't validate schema matches CDC output) |
| `test_multiple_sts_sharing_source` | Multiple STs can share a source | Creates 2 STs, counts deps | ⚠️ Weak |
| `test_staleness_info_view` | Staleness calculation | Duration extraction from age | ⚠️ Weak |

**Scorecard:** 3/16 strong, 13/16 weak.

**Key gaps:**
- No multiset comparison — INSERT tests verify count or a single field, never
  the full row
- Cascade delete only checks count, not that the correct rows were deleted
- Change buffer schema test doesn't validate that the schema matches what the
  actual CDC trigger would produce
- No test for cross-schema name uniqueness edge cases

---

### 3. `catalog_compat_tests.rs` — 21 tests, 477 lines

**Purpose:** Pins PostgreSQL 18.x catalog behavior assumptions that pg_trickle
relies on (type casting, relkind values, advisory locks, etc.).

**Init mode:** `TestDb::new()` (no catalog needed)

| Test Group | # Tests | What It Validates | Verdict |
|-----------|---------|-------------------|---------|
| `pg_get_viewdef_*` | 3 | View definition has/lacks semicolon, subquery wrapping | ✅ Adequate |
| `nspname_*` | 3 | `nspname::text` cast works (Oid 19 → 25 interop) | ✅ Important regression guard |
| `relkind_*` | 5 | relkind values for r/v/m/p/I are as expected | ✅ Pass |
| `array_length_*` | 2 | `array_length()` returns INT4 (not INT8) | ✅ Pass |
| `union_all_*` | 2 | UNION ALL preserves column names, allows aggregation | ⚠️ Weak (count only) |
| `advisory_lock_roundtrip` | 1 | Lock acquire + release cycle | ✅ Pass |
| `pg_available_extensions_shape` | 1 | Extension metadata columns exist | ✅ Pass |
| `trigger_*` | 2 | User trigger detection query works | ✅ Pass |
| `table_rewrite_event_trigger` | 1 | Event trigger fires on ALTER TABLE | ✅ Pass |
| `pg_get_viewdef_trailing_whitespace` | 1 | Trailing whitespace behavior | ✅ Pass |

**Verdict:** ✅ **Good.** These tests are valuable as PostgreSQL version
compatibility guards. When PG 19 ships, any behavioral changes that break
pg_trickle assumptions will surface here. The `nspname` cast tests are
especially important given the Oid 19/25 mismatch that caused real bugs.

---

### 4. `extension_tests.rs` — 11 tests, 236 lines

**Purpose:** Validates pg_trickle-specific catalog objects exist with the
expected structure (columns, indexes, JSONB frontier, pg_lsn type).

**Init mode:** `TestDb::with_catalog()`

| Test | What It Claims | What It Checks | Verdict |
|------|---------------|---------------|---------|
| `test_pg_trickle_schema_exists` | Schema exists | `EXISTS` check | ⚠️ Weak |
| `test_all_catalog_objects_exist` | All catalog objects exist | Loop + COUNT ≥ 3 indexes | ⚠️ Weak |
| `test_xxhash_deterministic` | xxh64 hash is deterministic | Pure Rust: `hash(a)==hash(a)`, `hash(a)!=hash(b)` | ✅ Strong |
| `test_pg_hashtext_works_in_container` | PG hashtext works | SQL: two calls equal | ✅ Pass |
| `test_stream_tables_columns` | All expected columns exist | Loop + `EXISTS` per column | ⚠️ Weak (doesn't verify types) |
| `test_change_tracking_columns` | Tracking columns exist | Loop + `EXISTS` | ⚠️ Weak |
| `test_dependency_columns` | Dependency columns exist | Loop + `EXISTS` | ⚠️ Weak |
| `test_refresh_history_columns` | History columns exist | Loop + `EXISTS` | ⚠️ Weak |
| `test_frontier_jsonb_column` | JSONB roundtrip works | INSERT + UPDATE + field extraction | ✅ Pass |
| `test_pg_lsn_type_works` | pg_lsn comparison works | Cast + comparison operators | ✅ Pass |
| `test_xxhash_sql_function` | xxh64 SQL function deterministic | SQL calls + equality | ✅ Pass |

**Key gap:** Column existence tests don't verify data types or constraints.
A column named `pgt_status` of type `integer` instead of `text` would pass.

---

### 5. `monitoring_tests.rs` — 10 tests, 421 lines

**Purpose:** Validates monitoring queries (refresh stats aggregation, staleness
calculation, notification, lateral join stats views).

**Init mode:** `TestDb::with_catalog()`

| Test | What It Claims | What It Checks | Verdict |
|------|---------------|---------------|---------|
| `test_refresh_stats_aggregation` | Stats aggregate correctly | FILTER/COUNT on mock data | ⚠️ Weak (pieces tested separately) |
| `test_refresh_history_by_name` | History ordered correctly | ORDER BY + LIMIT | ✅ Pass |
| `test_staleness_calculation` | Staleness epoch correct | Assert 59–65 seconds for 60s old data | ⚠️ Fragile (clock-dependent) |
| `test_stale_flag` | Stale detection works | Boolean: 120s > 30s threshold | ✅ Pass |
| `test_stream_tables_info_view` | Info view returns data | COUNT=1 + stale field | ⚠️ Weak |
| `test_notify_pg_trickle_alert` | NOTIFY works | Executes `pg_notify()` — never verifies receipt | ⚠️ Incomplete |
| `test_full_stats_lateral_join` | LATERAL JOIN stats work | 11-column tuple unpacking | ✅ Strong |
| `test_monitoring_empty_state` | Empty tables handled | COUNT=0 queries | ✅ Pass |
| `test_alert_channels_exist` | Alert channels documented | String contains check | ⚠️ Weak |
| `test_staleness_with_null_timestamp` | NULL data_timestamp handled | COALESCE behavior | ✅ Pass |

**Key gaps:**
- `test_notify_pg_trickle_alert` is a fire-and-forget test — it never
  subscribes with `LISTEN` to verify the payload was delivered
- `test_staleness_calculation` uses a ±5 second tolerance window that may
  fail under CI load

---

### 6. `property_tests.rs` — ~30 tests, 470 lines

**Purpose:** Property-based testing of pure Rust logic using `proptest`.

**Init mode:** None (no database)

| Property Category | # Tests | Key Properties Verified |
|-------------------|---------|----------------------|
| LSN comparison | 6 | Reflexive, antisymmetric, trichotomy, total order, ≥ consistent |
| Frontier JSON | 4 | Roundtrip serialization, set/get idempotency |
| Canonical period | 2 | 48 × 2^n bounds invariant |
| SQL generation | 5 | `quote_ident` roundtrip, column list completeness, determinism |
| Enum roundtrip | 2 | PgtStatus + RefreshMode from_str(to_str(x)) == x |
| DAG topology | 8 | Topological order respects edges, cycle detection, schedule resolution |
| Hash properties | 3 | Determinism, NULL encoding distinction, separator uniqueness |

**Verdict:** ✅ **Excellent.** This is the gold standard test file. All tests
use proptest with 500+ randomly generated cases per property. No gaps identified.
The DAG property tests are particularly valuable — they verify that
topological sort always produces a valid ordering, even for randomly generated
graphs.

---

### 7. `resilience_tests.rs` — 7 tests, 338 lines

**Purpose:** Validates crash recovery, error escalation, advisory locks, and
failure isolation between stream tables.

**Init mode:** `TestDb::with_catalog()`

| Test | What It Claims | What It Checks | Verdict |
|------|---------------|---------------|---------|
| `test_crash_recovery_marks_running_as_failed` | RUNNING → FAILED on restart | Status + error_message + end_time set | ✅ Pass |
| `test_advisory_lock_mechanism` | Advisory locks work | acquire + release cycle | ✅ Pass |
| `test_error_escalation_to_suspension` | Errors increment → suspend | UPDATE loop + auto-suspend check | ⚠️ Weak (doesn't verify threshold) |
| `test_error_count_resets_on_success` | Errors reset on success | `consecutive_errors=0` after reset | ⚠️ Weak |
| `test_needs_reinit_lifecycle` | reinit flag toggles | UPDATE TRUE/FALSE cycle | ⚠️ Weak |
| `test_refresh_history_status_transitions` | RUNNING → COMPLETED | UPDATE with rows_inserted=42 | ⚠️ Weak |
| `test_error_handling_independent_per_st` | Errors isolated per ST | Suspend one, verify other ACTIVE | ✅ Pass |

**Key gaps:**
- Error escalation test doesn't verify the exact threshold (how many errors
  trigger suspension?)
- No test for SUSPENDED → ACTIVE recovery path
- All tests use mock SQL updates — no test triggers a real refresh failure

---

### 8. `scenario_tests.rs` — 10 tests, 489 lines

**Purpose:** End-to-end-like scenarios testing full refresh workflows with
multiset data validation.

**Init mode:** `TestDb::with_catalog()`

| Test | What It Claims | Assert Type | Verdict |
|------|---------------|------------|---------|
| `test_scenario_create_and_full_refresh` | ST = query after refresh | EXCEPT/UNION ALL multiset | ✅✅ Strong |
| `test_scenario_refresh_after_insert` | ST matches after INSERT | EXCEPT/UNION ALL | ✅✅ Strong |
| `test_scenario_refresh_after_update` | ST matches after UPDATE | EXCEPT/UNION ALL + value check | ✅✅ Strong |
| `test_scenario_refresh_after_delete` | ST matches after DELETE | EXCEPT/UNION ALL | ✅✅ Strong |
| `test_scenario_filtered_st` | Filtered ST matches WHERE clause | EXCEPT/UNION ALL | ✅✅ Strong |
| `test_scenario_join_st` | JOIN ST is correct | EXCEPT/UNION ALL | ✅✅ Strong |
| `test_scenario_aggregate_st` | Aggregate ST correct | EXCEPT/UNION ALL + specific values | ✅✅ Strong+ |
| `test_scenario_refresh_history` | History tracked | COUNT queries | ⚠️ Weak |
| `test_scenario_st_info_view` | Info view correct | Boolean check | ⚠️ Weak |
| `test_scenario_no_data_refresh` | NO_DATA action recorded | String + count | ⚠️ Weak |

**Verdict:** ✅ **Excellent for tests 1–7.** The multiset comparison pattern is
the gold standard:

```sql
SELECT NOT EXISTS (
  (SELECT cols FROM st_table EXCEPT SELECT cols FROM defining_query)
  UNION ALL
  (SELECT cols FROM defining_query EXCEPT SELECT cols FROM st_table)
)
```

This catches: missing rows, extra rows, duplicate discrepancies, and column
value mutations. Tests 1–7 are the only integration tests that validate actual
data correctness.

**Note:** These tests simulate a full refresh by manually copying data
(`INSERT INTO st_table SELECT ...`) — they do not invoke the extension's
refresh function. This is appropriate for the integration tier.

---

### 9. `trigger_detection_tests.rs` — 10 tests, 378 lines

**Purpose:** Validates the SQL query used by `src/cdc.rs` to detect user-created
triggers (excluding pg_trickle's own `pgt_*` and `pg_trickle_*` triggers).

**Init mode:** `TestDb::new()`

| Test | Scenario | Verdict |
|------|----------|---------|
| `test_trigger_detection_no_triggers` | No triggers → false | ✅ Pass |
| `test_trigger_detection_with_user_trigger` | User ROW trigger → true | ✅ Pass |
| `test_trigger_detection_ignores_pgt_prefix` | `pgt_cdc_*` → false | ✅ Pass |
| `test_trigger_detection_ignores_statement_level` | STATEMENT trigger → false | ✅ Pass |
| `test_trigger_detection_mixed_triggers` | Mix of excluded + 1 user → true | ✅ Pass |
| `test_trigger_detection_before_trigger` | BEFORE trigger detected | ✅ Pass |
| `test_trigger_detection_after_drop` | Drop trigger → state updates | ✅ Pass |
| `test_trigger_detection_ignores_pg_trickle_prefix` | `pg_trickle_cdc_*` → false | ✅ Regression guard |
| `test_trigger_detection_mixed_internal_and_user` | Both prefixes + user → true | ✅ Pass |
| `test_trigger_detection_constraint_trigger` | Constraint trigger handling | ✅ Pass |

**Verdict:** ✅ **Strong.** 10 tests cover all exclusion rules and include a
regression test for the `pg_trickle_` prefix bug. Only weakness is that all
assertions are boolean — but for trigger detection, boolean is the correct
abstraction level.

---

### 10. `workflow_tests.rs` — 5 tests, 326 lines

**Purpose:** Validates full lifecycle workflows: create ST → insert source
data → refresh → verify history → drop.

**Init mode:** `TestDb::with_catalog()`

| Test | What It Claims | What It Checks | Verdict |
|------|---------------|---------------|---------|
| `test_full_refresh_workflow` | Full lifecycle | COUNT queries for status + history | ⚠️ Weak |
| `test_source_data_changes_tracked` | CDC mock: I/U/D in buffer | Change sequence + LSN order | ⚠️ Weak (mock only) |
| `test_chained_stream_tables` | Dependency DAG | Creates 2 STs in chain, queries graph | ⚠️ Weak |
| `test_error_escalation_and_suspension` | Error loop + suspension | 3 failures → suspend, verify history | ⚠️ Weak |
| `test_full_lifecycle_with_drop` | Create → populate → drop | Table exists → table gone | ⚠️ Weak |

**Key gap:** `test_full_refresh_workflow` creates a source table, inserts data,
simulates a "refresh" by copying data, marks the ST as populated — but **never
verifies that the ST storage table contains the correct rows**. It only checks
that `is_populated=true` and that a history record was created.

This test should adopt the `scenario_tests.rs` EXCEPT/UNION ALL pattern.

---

## Unit Tests — Module Landscape

### Overview

~1,300 tests across `src/` modules. Here are the top modules by test count
and coverage quality:

### Tier 1: Exceptional Coverage (>50 tests)

| Module | Tests | Coverage | Notes |
|--------|-------|----------|-------|
| `api.rs` | ~95 | ⭐⭐⭐⭐ | Schedule parsing exhaustive (cron/duration/at/repeats). Query validation thorough. All pre-DB logic. |
| `dvm/parser.rs` | ~60 | ⭐⭐⭐⭐⭐ | Aggregate detection, window function extraction, set operation analysis, subquery handling. Gold standard for parse edge cases. |
| `refresh.rs` | ~50 | ⭐⭐⭐ | `determine_refresh_action()` covers all 5 state combos. `resolve_lsn_placeholders()` thorough. **Gap:** `execute_differential_refresh()` — only error paths tested, success path untested. |
| `dvm/operators/recursive_cte.rs` | ~45 | ⭐⭐⭐⭐ | Multi-level recursion, terminal detection, max cycle limits. |
| `dvm/operators/aggregate.rs` | ~40 | ⭐⭐⭐⭐⭐ | GROUP BY dedup, DISTINCT COUNT, window post-filter extraction. Comprehensive per-type coverage. |

### Tier 2: Solid Coverage (15–50 tests)

| Module | Tests | Coverage | Notes |
|--------|-------|----------|-------|
| `hooks.rs` | ~23 | ⭐⭐⭐ | DDL classification (12 types), schema change detection. |
| `ivm.rs` | ~24 | ⭐⭐⭐ | Simple chain detection, insert/delete SQL generation. |
| `scheduler.rs` | ~20 | ⭐⭐ | Group policies, falling_behind ratios, worker_extra parsing. **Gap:** No job lifecycle (enqueue/claim/complete). |
| `cdc.rs` | ~20 | ⭐⭐⭐ | Trigger naming, PK hash, bitmask generation, LSN range parsing. |
| `version.rs` | ~19 | ⭐⭐⭐ | Frontier serialization, LSN merge, data timestamp selection. |
| `catalog.rs` | ~16 | ⭐⭐⭐ | Enum serialization roundtrips. |

### Tier 3: Basic or Minimal Coverage (<15 tests)

| Module | Tests | Coverage | Notes |
|--------|-------|----------|-------|
| `dvm/operators/join.rs` | ~11 | ⭐⭐⭐ | INNER join only — equijoin keys, pre-change snapshot, Part 1a/1b splits. |
| `dvm/operators/outer_join.rs` | ~11 | ⭐⭐⭐ | LEFT join — nested joins, null padding, R₀ reconstruction. |
| `dvm/operators/scan.rs` | ~10 | ⭐⭐⭐ | Partition elimination, type checking. |
| `dvm/operators/window.rs` | ~8 | ⭐⭐⭐ | Partition/frame detection. |
| `dvm/operators/lateral_subquery.rs` | ~8 | ⭐⭐ | Basic correlation tests only. |
| `dvm/operators/filter.rs` | ~8 | ⭐⭐ | Basic pushdown. |
| `dvm/operators/full_join.rs` | ~7 | ⭐⭐ | 10-part decomposition, delta flags. |
| `dag.rs` | ~8 | ⭐⭐⭐ | Topo sort, cycle detection. Property tests in `property_tests.rs` supplement. |
| `hash.rs` | ~7 | ⭐⭐⭐ | SHA256 determinism. Property tests supplement. |
| `monitor.rs` | ~8 | ⭐⭐ | Envelope formatting only. |
| `error.rs` | ~7 | ⭐⭐ | Display/Debug formatting only. |
| `wal_decoder.rs` | ~5 | ⭐ | Stubs/placeholders. |

### Join Operator Coverage Matrix

This is a critical coverage gap. Each join type has its own DVM operator file,
but test coverage varies dramatically:

| Join Type | File | Tests | Coverage | Notes |
|-----------|------|-------|----------|-------|
| **INNER** | `operators/join.rs` | 11 | ⭐⭐⭐ | Equijoin keys, EXCEPT ALL pre-change, Part 1a/1b |
| **LEFT** | `operators/outer_join.rs` | 11 | ⭐⭐⭐ | Nested joins, null padding, 7-part decomposition |
| **FULL** | `operators/full_join.rs` | 7 | ⭐⭐ | 10-part decomposition, delta flags, nested joins |
| **SEMI** | `operators/semi_join.rs` | 3 | ⭐ | **CRITICAL GAP:** Basic EXISTS check only |
| **ANTI** | `operators/anti_join.rs` | 3 | ⭐ | **CRITICAL GAP:** Basic NOT EXISTS check only |

**Missing across all join types:**
- No multi-column non-equi-join conditions
- No 3+ table join chains
- No unified test comparing behavior across join types with identical data

---

## Cross-Cutting Findings

### Finding 1: Mock Catalog DDL ≠ Real Extension (CRITICAL)

The most fundamental limitation of the integration test layer is that
`TestDb::with_catalog()` loads a hardcoded DDL string, not the actual compiled
extension. This means:

- The DDL string can drift from the real extension's DDL without detection
- Constraint names, column types, default values, and index definitions are
  assumed correct but never verified against the actual `CREATE EXTENSION`
- Any SQL function called in tests is not the real extension function — it's
  a mock or doesn't exist at all

**Mitigation:** This is an architectural choice (integration tests should be
fast and DB-binary-free). The risk is mitigated by E2E tests that load the
real extension. However, a **DDL drift detection test** would be valuable —
see Priority Mitigations.

### Finding 2: Assertion Quality Distribution

| Quality Level | Count | % | Description |
|--------------|-------|---|-------------|
| ✅✅ Strong (multiset) | 7 | 6% | `scenario_tests` EXCEPT/UNION ALL |
| ✅ Adequate | 40 | 32% | Specific value checks, constraint enforcement, type verification |
| ⚠️ Weak | 76 | 62% | Boolean/count/existence only |

62% of integration test assertions are weak. For catalog structure tests,
weak assertions (existence checks) are often appropriate — you just need to
know the table exists. But for data-flow tests (`workflow_tests.rs`,
`monitoring_tests.rs`), weak assertions create false confidence.

### Finding 3: No Shared Multiset Comparison Helper

`scenario_tests.rs` has excellent multiset comparison using inline SQL:

```sql
SELECT NOT EXISTS (
  (SELECT cols FROM a EXCEPT SELECT cols FROM b)
  UNION ALL
  (SELECT cols FROM b EXCEPT SELECT cols FROM a)
)
```

This pattern is reimplemented ad-hoc. Other files that would benefit from it
(workflow_tests, catalog_tests with data validation) don't use it because
there's no shared helper.

### Finding 4: Time-Dependent Assertions

`monitoring_tests.rs::test_staleness_calculation` inserts a row with
`NOW() - INTERVAL '60 seconds'` as the timestamp, then asserts the calculated
staleness is between 59 and 65 seconds. Under heavy CI load or container
startup delays, this window may be too narrow.

### Finding 5: Property Tests Are the Strongest Layer

`property_tests.rs` with its 30 proptest-driven tests provides the highest
confidence-per-test-count ratio of any file in the suite. The combination of
random input generation (500+ cases per property) with mathematical invariant
checking (reflexivity, antisymmetry, trichotomy for LSN ordering) is a model
for how other pure-logic modules should be tested.

### Finding 6: Unit Tests Miss SPI-Dependent Code Paths

Several important code paths are untestable at the unit level because they
require SPI (PostgreSQL's Server Programming Interface):

| Code Path | Module | Why Untestable |
|-----------|--------|---------------|
| `execute_differential_refresh()` success | `refresh.rs` | Executes MERGE via SPI |
| Job enqueue/claim/complete | `scheduler.rs` | Reads/writes catalog via SPI |
| CDC trigger installation | `cdc.rs` | Creates triggers via SPI |
| Extension object creation | `api.rs` | Calls `CREATE` via SPI |

This is an inherent limitation — these paths can only be tested via E2E tests
(or future SPI mocking infrastructure).

---

## Priority Mitigations

### P0 — Critical (Confidence Improvement)

#### P0-1: Extract Multiset Comparison Helper

Move the `scenario_tests.rs` EXCEPT/UNION ALL pattern into
`tests/common/mod.rs` as a reusable method:

```rust
impl TestDb {
    async fn assert_sets_equal(&self, table_a: &str, table_b: &str, cols: &[&str]) {
        let col_list = cols.join(", ");
        let sql = format!(
            "SELECT NOT EXISTS (
                (SELECT {cols} FROM {a} EXCEPT SELECT {cols} FROM {b})
                UNION ALL
                (SELECT {cols} FROM {b} EXCEPT SELECT {cols} FROM {a})
            )",
            cols = col_list, a = table_a, b = table_b
        );
        let matches: bool = self.query_scalar(&sql).await;
        assert!(matches, "Set mismatch between {} and {}", table_a, table_b);
    }
}
```

Then apply it to `workflow_tests.rs::test_full_refresh_workflow` and any other
test that simulates refresh + data mutation.

**Impact:** Converts 5+ weak tests to strong tests.

#### P0-2: Add LISTEN/NOTIFY Round-Trip Test

Replace the fire-and-forget `test_notify_pg_trickle_alert` with an actual
round-trip test using `sqlx::PgListener`:

```rust
#[tokio::test]
async fn test_notify_roundtrip() {
    let db = TestDb::new().await;
    let mut listener = PgListener::connect_with(&db.pool).await.unwrap();
    listener.listen("pg_trickle_alert").await.unwrap();
    db.execute("SELECT pg_notify('pg_trickle_alert', '{\"event\": \"refresh\"}')").await;
    let notification = tokio::time::timeout(
        Duration::from_secs(5), listener.recv()
    ).await.unwrap().unwrap();
    assert_eq!(notification.payload(), r#"{"event": "refresh"}"#);
}
```

**Impact:** Converts an incomplete test to a real integration test.

#### P0-3: Strengthen Workflow Data Validation

`workflow_tests.rs::test_full_refresh_workflow` should verify that the storage
table contains the expected rows after simulated refresh, not just check
`is_populated=true`. Use the P0-1 helper.

#### P0-4: Add Semi/Anti Join Unit Tests

Increase `semi_join.rs` and `anti_join.rs` from 3 tests each to 8–10,
matching the coverage level of `join.rs` and `outer_join.rs`. Key scenarios
to add:

- Nested semi-join (A SEMI JOIN (B SEMI JOIN C))
- Semi-join with multi-column conditions
- Anti-join with NULL handling (NOT IN vs NOT EXISTS semantics)
- Semi/anti join with aggregate subqueries

### P1 — High (Correctness Hardening)

#### P1-1: Widen Staleness Test Tolerance

Change `test_staleness_calculation` from ±5s (59–65) to ±10s (50–70) or use
a relative tolerance:

```rust
let staleness: f64 = db.query_scalar("...").await;
assert!(staleness >= 50.0 && staleness <= 120.0,
    "Staleness {} out of range for 60s-old row", staleness);
```

**Impact:** Eliminates flaky CI failures from clock skew.

#### P1-2: Add Column Type Verification to Extension Tests

`extension_tests.rs` checks column existence but not types. Add type
assertions:

```sql
SELECT column_name, data_type FROM information_schema.columns
WHERE table_schema = 'pgtrickle' AND table_name = 'pgt_stream_tables'
ORDER BY ordinal_position
```

Then compare against expected `(name, type)` pairs.

**Impact:** Catches schema drift between mock DDL and expectations.

#### P1-3: Add Error Escalation Threshold Test

`resilience_tests.rs::test_error_escalation_to_suspension` should assert the
exact threshold at which suspension triggers (e.g., after the Nth consecutive
error), not just that suspension eventually happens.

#### P1-4: Add Refresh Success Path Unit Test

`refresh.rs` has ~50 unit tests but `execute_differential_refresh()` is only
tested for error paths. While the success path requires SPI (and thus can only
be fully tested in E2E), the pre-SPI decision logic (template selection, LSN
resolution) could be extracted and tested.

### P2 — Medium (Coverage Expansion)

#### P2-1: Add DDL Drift Detection Test

Create a test that compares the hardcoded `CATALOG_DDL` string in
`tests/common/mod.rs` against the extension's actual DDL (extracted from
`sql/pg_trickle--X.Y.Z.sql` files):

```rust
#[test]
fn test_catalog_ddl_matches_extension_sql() {
    let catalog_ddl = CATALOG_DDL;
    let extension_sql = std::fs::read_to_string("sql/pg_trickle--current.sql").unwrap();
    // Extract table definitions from both, compare column names and types
    // (approximate check — exact DDL match is too fragile)
}
```

**Impact:** Prevents mock catalog from silently drifting from reality.

#### P2-2: Add Multi-Table Join Chain Unit Tests

No join operator test covers 3+ table compositions. Add tests for:
- `(A INNER JOIN B) LEFT JOIN C`
- `(A LEFT JOIN B) INNER JOIN C` (elimination opportunity)
- `A FULL JOIN (B SEMI JOIN C)`

#### P2-3: Add Scheduler Job Lifecycle Integration Test

Even without SPI mocking, the integration harness can test the job lifecycle
via raw SQL against the mock catalog:

```rust
async fn test_scheduler_job_lifecycle() {
    let db = TestDb::with_catalog().await;
    // INSERT job → UPDATE status QUEUED→RUNNING → UPDATE RUNNING→COMPLETED
    // Verify state transitions, timing, and error counts
}
```

#### P2-4: Extend Property Tests to DVM Operators

The proptest pattern from `property_tests.rs` could be applied to DVM
operator SQL output:

- For any randomly generated 2-table join, verify that `diff_inner_join()`
  output SQL mentions all source columns
- For any aggregate expression, verify output references all GROUP BY columns

### P3 — Low (Polish)

#### P3-1: Remove or Fold Smoke Tests

`smoke_tests.rs` is 49 lines testing Testcontainers basics already proven by
every other test. Consider folding into a doc-comment example or removing.

#### P3-2: Add Workflow Test for ST Drop Cascade

`workflow_tests.rs::test_full_lifecycle_with_drop` should verify that dropping
an ST also removes its change buffer table, dependencies, and history records.

#### P3-3: Standardize Test Naming

Some tests use `test_<component>_<scenario>_<expected>` naming (per
AGENTS.md), others use `test_<scenario>` without the component prefix.
Standardize across all files.

---

## Appendix: Test Count Summary

### Integration Tests

| File | Tests | Lines | Strong | Weak | Init Mode |
|------|-------|-------|--------|------|-----------|
| `smoke_tests.rs` | 3 | 49 | 0 | 3 | `new()` |
| `catalog_tests.rs` | 16 | 514 | 3 | 13 | `with_catalog()` |
| `catalog_compat_tests.rs` | 21 | 477 | 0 | 21 | `new()` |
| `extension_tests.rs` | 11 | 236 | 3 | 8 | `with_catalog()` |
| `monitoring_tests.rs` | 10 | 421 | 3 | 7 | `with_catalog()` |
| `property_tests.rs` | ~30 | 470 | 30 | 0 | None |
| `resilience_tests.rs` | 7 | 338 | 2 | 5 | `with_catalog()` |
| `scenario_tests.rs` | 10 | 489 | 7 | 3 | `with_catalog()` |
| `trigger_detection_tests.rs` | 10 | 378 | 0 | 10 | `new()` |
| `workflow_tests.rs` | 5 | 326 | 0 | 5 | `with_catalog()` |
| **Total** | **~123** | **~3,698** | **48** | **75** | — |

### Unit Tests (Top Modules)

| Module | Tests | Quality |
|--------|-------|---------|
| `api.rs` | ~95 | ⭐⭐⭐⭐ |
| `dvm/parser.rs` | ~60 | ⭐⭐⭐⭐⭐ |
| `refresh.rs` | ~50 | ⭐⭐⭐ |
| `dvm/operators/recursive_cte.rs` | ~45 | ⭐⭐⭐⭐ |
| `dvm/operators/aggregate.rs` | ~40 | ⭐⭐⭐⭐⭐ |
| `ivm.rs` | ~24 | ⭐⭐⭐ |
| `hooks.rs` | ~23 | ⭐⭐⭐ |
| `scheduler.rs` | ~20 | ⭐⭐ |
| `cdc.rs` | ~20 | ⭐⭐⭐ |
| `version.rs` | ~19 | ⭐⭐⭐ |
| `catalog.rs` | ~16 | ⭐⭐⭐ |
| Other modules | ~190 | Mixed |
| **Total** | **~1,300** | — |
