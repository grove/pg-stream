# Full E2E Test Suite — Deep Evaluation Report

> **Date:** 2025-03-16
> **Scope:** 18 full-E2E-only test files (222 tests, ~11,000 lines) requiring
>            the custom Docker image with the compiled extension
> **Goal:** Assess coverage confidence and identify mitigations to harden the suite

---

## Implementation Status

> **Updated:** 2026-03-17
> **Branch:** `test-evals-full-e2e`

### Completed Mitigations

| Priority | Item | Status | Files Changed |
|----------|------|--------|---------------|
| P0-1 | WAL CDC data capture multiset assertions | ✅ Done | `e2e_wal_cdc_tests.rs` |
| P0-2 | Partition tests multiset assertions | ✅ Done | `e2e_partition_tests.rs` |
| P0-3 | DDL event post-reinit data assertions | ✅ Done | `e2e_ddl_event_tests.rs` |
| P0-4 | Circular ST convergence data assertions | ✅ Done | `e2e_circular_tests.rs` |
| P1-1 | Fix RLS superuser bypass in test | ✅ Done | `e2e_rls_tests.rs` |
| P1-2 | Add multiset to append-only fallback tests | ✅ Done | `e2e_append_only_tests.rs` |
| P1-3 | Add multiset to cascade regression tests 3 and 6 | ✅ Done | `e2e_cascade_regression_tests.rs` |
| P1-4 | Add multiset to bootstrap gating refresh tests 12 and 17 | ✅ Done | `e2e_bootstrap_gating_tests.rs` |
| P2-1 | Benchmark smoke assertions | ✅ Done | `e2e_bench_tests.rs` |
| P2-2 | Add multiset after ALTER QUERY | ✅ Done | `e2e_alter_query_tests.rs` |
| P2-3 | Upgrade survival multiset | ✅ Done | `e2e_upgrade_tests.rs` |
| P2-4 | Non-convergence guaranteed divergence | ✅ Done | `e2e_circular_tests.rs` |
| P3-1 | Cascade ad-hoc to multiset | ✅ Done | `e2e_cascade_regression_tests.rs` |
| P3-2 | DELETE/UPDATE in bootstrap gating | ✅ Done | `e2e_bootstrap_gating_tests.rs` |
| P3-3 | Standardize bgworker multiset | ✅ Done | `e2e_bgworker_tests.rs` |

#### P0-1 Details (WAL CDC)
Added `assert_st_matches_query` to four tests:
- `test_wal_cdc_captures_insert` — verifies all inserted rows decoded correctly
- `test_wal_cdc_captures_update` — verifies update reflected via WAL pipeline
- `test_wal_cdc_captures_delete` — verifies only kept rows remain
- `test_wal_fallback_on_missing_slot` — verifies no data loss after fallback

#### P0-2 Details (Partitions)
Added `assert_st_matches_query` to six tests:
- `test_partition_range_full_refresh` — row-level correctness for RANGE + FULL
- `test_partition_range_differential_refresh` — correctness after I/U/D across partitions
- `test_partition_list_source` — aggregated result correctness for LIST partition
- `test_partition_hash_source` — no row loss/corruption for HASH partition
- `test_partition_with_aggregation` — full GROUP BY result over both partitions
- `test_partition_differential_with_aggregation` — GROUP BY result after cross-partition INSERT

#### P0-3 Details (DDL Events)
Added post-reinit data assertions to five tests:
- `test_function_change_marks_st_for_reinit` — refreshes after replacement, verifies new function body applies
- `test_add_column_on_source_st_still_functional` — multiset after ADD COLUMN refresh
- `test_add_column_unused_st_survives_refresh` — multiset verifies unused column excluded
- `test_drop_unused_column_st_survives` — multiset after DROP COLUMN refresh
- `test_alter_column_type_triggers_reinit` — refreshes after type change, verifies correct data

#### P0-4 Details (Circular)
Added to `test_circular_monotone_cycle_converges`:
- Row count assertion: ≥6 pairs for transitive closure of 3-node chain
- Existence assertion: pair `(1,4)` must exist — requires 2+ fixpoint iterations

#### P1-1 Details (RLS)
Fixed `test_rls_on_stream_table_filters_reads`:
- Uses `db.pool.begin()` + `SET LOCAL ROLE rls_reader` in a transaction
- Asserts `count = 2` (only tenant_id=10 rows visible) as restricted role
- Existing superuser assertion `count = 4` retained

#### P1-2 Details (Append-Only)
Added `assert_st_matches_query` to three tests:
- `test_append_only_fallback_on_delete` — verifies row absent after DELETE + MERGE fallback
- `test_append_only_fallback_on_update` — verifies no stale old-value rows remain
- `test_alter_enable_append_only` — verifies correct data after INSERT via append-only path

#### P1-3 Details (Cascade Regression)
Added `assert_st_matches_query` to two tests:
- `test_st_on_st_cascade_propagates_delete` — compares `order_report` against its defining query post-DELETE
- `test_three_layer_cascade_insert_propagates` — compares `big_categories` against `category_flags WHERE is_big = true` post-INSERT

#### P1-4 Details (Bootstrap Gating)
Added `assert_st_matches_query` to two tests:
- `test_manual_refresh_works_through_full_lifecycle` — verifies all 3 rows correct after full gate/ungate/re-gate cycle
- `test_manual_refresh_not_blocked_by_gate` — verifies both rows correct after gated manual refresh

### Remaining Work

| Priority | Item | Status |
|----------|------|--------|
| P2-1 | Add smoke correctness check to benchmarks (32 tests) | Not started |
| P2-2 | Add ALTER QUERY + DML cycle tests | Not started |
| P2-3 | Add upgrade chain data validation | Not started |
| P2-4 | Add non-convergence test with guaranteed divergence | Not started |
| P3-1 | Consolidate cascade value checks to multiset | Not started |
| P3-2 | Add DELETE/UPDATE to bootstrap gating tests | Not started |
| P3-3 | Standardise bgworker test assertions | Not started |

---

## Table of Contents

1. [Implementation Status](#implementation-status)
2. [Executive Summary](#executive-summary)
3. [Test Infrastructure](#test-infrastructure)
4. [Per-File Analysis](#per-file-analysis)
5. [Cross-Cutting Findings](#cross-cutting-findings)
6. [Priority Mitigations](#priority-mitigations)
7. [Appendix: Coverage Matrix](#appendix-coverage-matrix)
5. [Priority Mitigations](#priority-mitigations)
6. [Appendix: Coverage Matrix](#appendix-coverage-matrix)

---

## Executive Summary

The full E2E test suite consists of **222 test functions** across 18 files
(~11,000 lines). These tests require the custom Docker image built from
`tests/Dockerfile.e2e` with the compiled extension, background worker,
`shared_preload_libraries`, and GUC support. They run via `just test-e2e`
(CI: push to main + daily schedule + manual dispatch; **skipped on PRs**).

**Confidence level: MODERATE (≈65%)**

### Strength Distribution

| Verdict | Files | Tests | % of Total |
|---------|-------|-------|-----------|
| STRONG | 4 | 40 | 18% |
| ADEQUATE | 9 | 122 | 55% |
| WEAK | 5 | 60 | 27% |

### Files Using `assert_st_matches_query` (Multiset Comparison)

| File | Calls | Tests w/ Multiset |
|------|-------|-------------------|
| `e2e_differential_gaps_tests` | 39 | 13/13 (100%) |
| `e2e_multi_cycle_tests` | 21 | 6/9 (67%) |
| `e2e_guc_variation_tests` | 10 | 8/13 (62%) |
| `e2e_dag_autorefresh_tests` | 8 | 4/5 (80%) |
| `e2e_bgworker_tests` | 2 | 2/9 (22%) |
| `e2e_user_trigger_tests` | 2 | 2/11 (18%) |
| `e2e_alter_query_tests` | 1 | 1/15 (7%) |
| `e2e_upgrade_tests` | 1 | 1/14 (7%) |
| **8 files with ZERO** | 0 | 0/138 (0%) |
| **TOTAL** | **84** | **37/222 (17%)** |

**83% of full-E2E tests do NOT use multiset comparison for data correctness.**

### Strengths

| Area | Assessment |
|------|-----------|
| UDA + nested OR differential gaps | **Exceptional** — 13/13 tests with multiset, full DML cycles |
| Multi-cycle cumulative correctness | **Strong** — 5+ DML cycles with multiset at each checkpoint |
| DAG autorefresh cascades | **Strong** — 3-4 layer topologies with multiset at all layers |
| GUC variation correctness | **Strong** — 8 GUC configurations validated with multiset |
| DDL event detection | **Good** — 14 tests covering ADD/DROP/ALTER column, function changes, RENAME |
| Bootstrap gating lifecycle | **Good** — 18 tests covering full gate → ungate → re-gate cycle |

### Weaknesses

| Severity | Finding | Impact |
|----------|---------|--------|
| **CRITICAL** | 10 files (138 tests) have ZERO multiset comparison | Data corruption undetectable in partition, RLS, WAL CDC, circular, DDL event, append-only, bootstrap gating, cascade regression, bench, and ergonomics tests |
| **HIGH** | Partition tests rely on `db.count()` only | All 5 partition types (RANGE/LIST/HASH + aggregation) unverified for row correctness |
| **HIGH** | WAL CDC data capture tests use count only | WAL INSERT/UPDATE/DELETE correctness never verified at row level |
| **HIGH** | Circular ST data correctness never verified | Cycle convergence could produce wrong data; only metadata (scc_id, status) checked |
| **MEDIUM** | Cascade regression tests miss multiset on 3-layer chains | Test 6 (3-layer) only counts; tests 2, 7 use partial data checks |
| **MEDIUM** | Benchmark tests (32) have zero correctness assertions | Performance measured on potentially incorrect results |
| **MEDIUM** | RLS tests don't verify row-level filtering | Test 3 runs as superuser (bypasses RLS); no restricted-user query |
| **LOW** | Ergonomics tests are metadata-only | By design — API contract tests, not data tests |

---

## Test Infrastructure

### Full E2E Docker Image

**Docker image:** Built from `tests/Dockerfile.e2e`, includes:
- PostgreSQL 18.x with the compiled `pg_trickle` extension
- `shared_preload_libraries = 'pg_trickle'` configured
- Background worker active
- All GUCs available

**Test harness:** `tests/e2e/mod.rs` provides `TestDb` with:
- `create_st()` / `refresh_st()` / `drop_st()` — extension function wrappers
- `assert_st_matches_query(st_name, query)` — EXCEPT-based multiset comparison
  that auto-discovers columns, handles json→text casts, and filters internal
  `__pgt_*` columns. Supports EXCEPT/INTERSECT set-operation visibility filters.
- `wait_for_scheduler()` — polls until background worker completes a refresh
- Full `sqlx::PgPool` access for arbitrary SQL

### Why These Tests Need the Full Image

These 18 files test capabilities that require the compiled extension binary:
- Background worker / scheduler (bgworker, dag_autorefresh)
- GUC variables (guc_variation, bootstrap_gating)
- DDL event triggers (ddl_event)
- WAL-based CDC with logical replication (wal_cdc)
- Extension upgrade paths (upgrade)
- Row-level security interaction (rls)
- Partition ATTACH/DETACH triggers (partition)
- Circular dependency / SCC detection (circular)
- Append-only optimization (append_only)
- User-defined trigger interaction (user_trigger)
- CDC benchmarks (bench)

---

## Per-File Analysis

### 1. `e2e_alter_query_tests.rs` — 578 lines, 15 tests

**Purpose:** Validates ALTER QUERY operations (changing a stream table's
defining query in-place).

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_alter_query_same_schema` | Same-schema query change with WHERE clause | ✅ **STRONG** — `assert_st_matches_query` |
| `test_alter_query_same_schema_differential` | ALTER on DIFFERENTIAL mode ST | ⚠️ Count only |
| `test_alter_query_add_column` | Adding a column to the query | ⚠️ Spot-checks one value |
| `test_alter_query_remove_column` | Removing a column | ⚠️ Column existence only |
| `test_alter_query_type_change_compatible` | INT → BIGINT type change | ⚠️ Status + count |
| `test_alter_query_type_change_incompatible` | INT → TEXT triggers rebuild | ⚠️ OID changed, count only |
| `test_alter_query_change_sources` | Change to different source tables | ⚠️ Dependency count only |
| `test_alter_query_remove_source` | Remove a source dependency | ⚠️ Dependency check |
| `test_alter_query_pgt_count_transition` | Flat → aggregate query transition | ⚠️ Count only |
| `test_alter_query_with_mode_change` | Simultaneous query + mode change | ⚠️ Status + count |
| `test_alter_query_invalid_query` | Invalid query rejected | ✅ Error path |
| `test_alter_query_cycle_detection` | Cyclic deps rejected | ✅ Error path |
| `test_alter_query_view_inlining` | Views inlined in catalog | ⚠️ Catalog check |
| `test_alter_query_oid_stable_same_schema` | OID preserved for same-schema ALTER | ✅ OID comparison |
| `test_alter_query_catalog_updated` | Catalog query updated | ✅ Query text comparison |

**Verdict: ADEQUATE**

**Gaps:**
- Only 1/15 tests uses multiset comparison
- After ALTER to aggregate/join queries, data correctness not verified
- No ALTER + DML cycle (INSERT → ALTER → refresh → verify)

---

### 2. `e2e_append_only_tests.rs` — 342 lines, 10 tests

**Purpose:** Validates the append-only optimization (INSERT-only fast path)
and fallback to MERGE on UPDATE/DELETE.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_append_only_basic_insert_path` | Flag set, row count correct | ⚠️ Count only |
| `test_append_only_data_correctness` | Multi-cycle correctness | ⚠️ SUM aggregate only |
| `test_append_only_fallback_on_delete` | DELETE triggers fallback to MERGE | ⚠️ Flag check + count |
| `test_append_only_fallback_on_update` | UPDATE triggers fallback | ⚠️ Spot-checks one value |
| `test_alter_enable_append_only` | ALTER to enable append_only | ⚠️ Flag + count |
| `test_append_only_rejected_for_full_mode` | FULL mode rejects append_only | ✅ Error validation |
| `test_append_only_rejected_for_immediate_mode` | IMMEDIATE mode rejects | ✅ Error validation |
| `test_append_only_rejected_for_keyless_source` | Keyless table rejects | ✅ Error validation |
| `test_alter_append_only_rejected_for_full_mode` | ALTER rejects on FULL | ✅ Error validation |
| `test_append_only_no_data_cycle` | No-data cycle is idempotent | ⚠️ Count only |

**Verdict: ADEQUATE**

**Key gap:** Zero multiset comparisons. After fallback from append-only to
MERGE, data correctness should be verified with `assert_st_matches_query`.
Test 2 uses SUM for basic verification but can't detect wrong individual rows.

---

### 3. `e2e_bench_tests.rs` — 2,156 lines, 32 tests (all `#[ignore]`)

**Purpose:** Performance benchmarks measuring refresh latency across query
types (scan, filter, aggregate, join, window, lateral, CTE, UNION), sizes
(10K–100K rows), and change rates (1%–50%).

All 32 tests are `#[ignore]`-gated and timer-based. They measure TPS, p50/p99
latency, and overhead percentages.

| Test Category | Count | Assertion Type |
|--------------|-------|---------------|
| Scan benchmarks | 9 | ⚠️ Timing only |
| Filter/aggregate/join/window benchmarks | 12 | ⚠️ Timing only |
| No-data refresh latency | 1 | ⚠️ avg < 10ms target |
| Index overhead | 1 | ⚠️ Overhead % |
| CDC trigger overhead | 2 | ⚠️ Timing comparison |
| Statement vs row CDC | 2 | ⚠️ Timing comparison |
| Concurrent writers | 1 | ⚠️ Throughput |
| Full matrix sweeps | 4 | ⚠️ Timing aggregation |

**Verdict: WEAK (by design — benchmarks, not correctness tests)**

**Gap:** No data correctness assertions anywhere. Row counts are logged but
never asserted. If a DVM bug causes incorrect results, benchmarks will still
report normal timing.

**Recommendation:** Add a smoke-test assertion at the end of each benchmark
variant: after the final cycle, call `assert_st_matches_query` once. This
adds negligible overhead to the benchmark but catches correctness regressions.

---

### 4. `e2e_bgworker_tests.rs` — 570 lines, 9 tests

**Purpose:** Validates the background worker / scheduler: extension loading,
GUC registration, auto-refresh, differential mode, history records, catalog
metadata updates.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_extension_loads_with_shared_preload` | Extension present in pg_extension | ✅ Setup validation |
| `test_gucs_registered` | 8 GUC defaults correct | ✅ 8 SHOW comparisons |
| `test_gucs_can_be_altered` | GUCs changeable via ALTER SYSTEM | ✅ 5 ALTER + SHOW |
| `test_auto_refresh_within_schedule` | Scheduler fires within threshold | ⚠️ Count only |
| `test_auto_refresh_differential_mode` | Differential auto-refresh correct | ✅ **STRONG** — `assert_st_matches_query` |
| `test_scheduler_writes_refresh_history` | History records created | ⚠️ History count |
| `test_auto_refresh_differential_with_cdc` | CDC + differential auto-refresh | ✅ **STRONG** — `assert_st_matches_query` |
| `test_scheduler_refreshes_multiple_healthy_sts` | Multiple STs refreshed in one tick | ⚠️ Count checks |
| `test_auto_refresh_updates_catalog_metadata` | Timestamps and error counts updated | ⚠️ Metadata checks |

**Verdict: ADEQUATE**

**Strengths:** Tests 5 and 7 use multiset comparison for real correctness.
GUC validation thorough.

**Gaps:** Tests 4 and 8 (auto-refresh count, multiple STs) should use multiset.

---

### 5. `e2e_bootstrap_gating_tests.rs` — 637 lines, 18 tests

**Purpose:** Validates the bootstrap gating feature (source gates that block
scheduler refreshes during initial data loads).

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_gate_source_inserts_gate_record` | Gate record created | ⚠️ Metadata |
| `test_source_gates_returns_gated_source` | Function returns gated source | ⚠️ Metadata |
| `test_ungate_source_clears_gate` | Ungate sets gated=false | ⚠️ Metadata |
| `test_gate_source_is_idempotent` | Double-gate produces one record | ⚠️ Count |
| `test_regate_after_ungate` | Re-gate after ungate works | ⚠️ Metadata |
| `test_gate_source_nonexistent_table_errors` | Nonexistent table → error | ✅ Error path |
| `test_source_gates_empty_by_default` | No gates initially | ⚠️ Count |
| `test_multiple_sources_gated` | Multiple sources can be gated | ⚠️ Count |
| `test_idempotent_gate_refreshes_timestamp` | Double-gate refreshes gated_at | ⚠️ Timestamp |
| `test_idempotent_gate_preserves_state` | Double-gate preserves state | ⚠️ Metadata |
| `test_regate_lifecycle_clears_ungated_at` | Re-gate clears ungated_at | ⚠️ Metadata |
| `test_manual_refresh_works_through_full_lifecycle` | Manual refresh through gate cycle | ⚠️ Count (1→2→3) |
| `test_bootstrap_gate_status_returns_expected_columns` | Status function columns | ⚠️ Column check |
| `test_bootstrap_gate_status_ungated_duration` | Duration for ungated sources | ⚠️ Metadata |
| `test_bootstrap_gate_status_affected_stream_tables` | Affected STs listed | ⚠️ String contains |
| `test_bootstrap_gate_status_empty_by_default` | No gate status initially | ⚠️ Count |
| `test_manual_refresh_not_blocked_by_gate` | Manual refresh bypasses gates | ⚠️ Count |
| `test_scheduler_logs_skip_when_source_gated` | Scheduler SKIPs gated sources | ✅ History action/status |

**Verdict: ADEQUATE**

**Gaps:** Zero multiset comparisons. Tests 12 and 17 (manual refresh) should
verify data content, not just count increments.

---

### 6. `e2e_cascade_regression_tests.rs` — 796 lines, 8 tests

**Purpose:** Regression tests for ST-on-ST cascade behavior: propagation of
INSERT/UPDATE/DELETE through chained stream tables, zero-row refresh timestamp
stability, and correct dependency type tracking.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_cdc_triggers_not_counted_as_user_triggers` | CDC trigger exclusion in detection query | ✅ Before/after logic |
| `test_st_on_st_cascade_propagates_insert` | INSERT cascades through ST chain | ✅ Value comparison (300→450) |
| `test_st_on_st_cascade_propagates_delete` | DELETE cascades through ST chain | ⚠️ EXISTS check only |
| `test_zero_row_differential_preserves_data_timestamp` | 0-row refresh doesn't bump timestamp | ✅ **STRONG** — timestamp equality regression |
| `test_no_spurious_cascade_after_noop_upstream_refresh` | No-op upstream doesn't cascade | ✅ **STRONG** — timestamp stability |
| `test_three_layer_cascade_insert_propagates` | 3-layer INSERT cascade | ⚠️ Count only |
| `test_three_layer_cascade_update_propagates` | 3-layer UPDATE cascade | ✅ Category value comparison |
| `test_st_on_st_dependency_is_stream_table_type` | Dependency recorded as STREAM_TABLE | ✅ Type string comparison |

**Verdict: ADEQUATE to STRONG**

**Strengths:** Tests 2, 4, 5, 7 have genuine data validation (value comparisons,
timestamp equality). Regression-focused.

**Gaps:**
- Zero use of `assert_st_matches_query` — tests do ad-hoc data checks
- Test 3 (DELETE cascade) only checks EXISTS, not full data
- Test 6 (3-layer INSERT) only checks count

---

### 7. `e2e_circular_tests.rs` — 562 lines, 6 tests

**Purpose:** Validates circular/cyclic stream table dependencies using SCC
(strongly connected component) detection, monotonicity checks, convergence,
and drop cleanup.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_circular_monotone_cycle_converges` | Monotone cycle creation + SCC ID | ⚠️ Metadata only |
| `test_circular_nonmonotone_cycle_rejected` | Non-monotone cycle rejected | ✅ Error message |
| `test_circular_convergence_records_iterations` | Iteration count recorded | ⚠️ iterations ≥ 1 (loose) |
| `test_circular_nonconvergence_error_status` | Max iterations → ERROR | ⚠️ Status check (timing-sensitive) |
| `test_circular_drop_member_clears_scc_id` | Drop member clears SCC IDs | ⚠️ Metadata |
| `test_circular_default_rejects_cycles` | allow_circular=false rejects | ✅ Error message |

**Verdict: WEAK**

**Critical gap:** Zero multiset comparisons. All 6 tests validate only metadata
(scc_id, status, iteration count) — none verify that the cyclic stream tables
actually contain correct data after convergence. A cycle that converges to the
wrong fixed point would pass all tests.

---

### 8. `e2e_dag_autorefresh_tests.rs` — 449 lines, 5 tests

**Purpose:** Validates automatic scheduler-driven refresh through multi-layer
DAG topologies.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_autorefresh_3_layer_cascade` | 3-layer cascade auto-refresh | ✅ **STRONG** — `assert_st_matches_query` at all 3 layers |
| `test_autorefresh_diamond_cascade` | Diamond topology auto-refresh | ✅ **STRONG** — multiset on L2 |
| `test_autorefresh_calculated_schedule` | CALCULATED schedule triggers | ✅ **STRONG** — multiset after L1 refresh |
| `test_autorefresh_no_spurious_3_layer` | No spurious cascades on no-op | ✅ Timestamp stability |
| `test_autorefresh_staggered_schedules` | Staggered schedules converge | ✅ **STRONG** — multiset at all 3 layers |

**Verdict: STRONG**

**Exemplary file.** 4/5 tests use `assert_st_matches_query` for full multiset
comparison at every layer of the DAG. Test 4 (no-spurious) appropriately uses
timestamp stability rather than data comparison.

---

### 9. `e2e_ddl_event_tests.rs` — 608 lines, 14 tests

**Purpose:** Validates DDL event trigger reactions: what happens to stream
tables when source tables are altered (ADD/DROP/ALTER column, RENAME, DROP
table, function changes, index creation).

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_drop_source_fires_event_trigger` | DROP source → ST error/cleanup | ⚠️ Status/count |
| `test_alter_source_fires_event_trigger` | ALTER source → ST remains | ⚠️ Count only |
| `test_drop_st_storage_by_sql` | DROP storage → catalog cleanup | ⚠️ Count only |
| `test_rename_source_table` | RENAME source → refresh fails | ✅ Error path |
| `test_function_change_marks_st_for_reinit` | Function change → needs_reinit | ⚠️ Flag check |
| `test_drop_function_marks_st_for_reinit` | DROP function → needs_reinit | ⚠️ Flag check |
| `test_add_column_on_source_st_still_functional` | ADD column (unused) → ST OK | ⚠️ Count only |
| `test_add_column_unused_st_survives_refresh` | ADD + UPDATE → ST refreshes | ⚠️ Count + spot value |
| `test_drop_unused_column_st_survives` | DROP column (unused) → ST OK | ⚠️ Status + count |
| `test_alter_column_type_triggers_reinit` | ALTER TYPE → needs_reinit | ⚠️ Flag check |
| `test_create_index_on_source_is_benign` | CREATE INDEX → no reinit | ⚠️ Flag + count |
| `test_drop_source_with_multiple_downstream_sts` | DROP with 2+ downstream STs | ⚠️ Status checks |
| `test_block_source_ddl_guc_prevents_alter` | block_source_ddl=on blocks ALTER | ✅ Error + DML works |
| `test_add_column_on_joined_source_st_survives` | ADD column on joined source | ⚠️ Status + count |

**Verdict: WEAK**

**Critical gap:** Zero multiset comparisons across all 14 tests. After DDL
changes (ADD/DROP/ALTER column, function replacement), stream table data is
never verified. Tests confirm metadata flags (needs_reinit, status) but not
whether the data is correct after the DDL-triggered reinit/refresh.

---

### 10. `e2e_differential_gaps_tests.rs` — 526 lines, 13 tests

**Purpose:** Validates DVM differential refresh for features that previously
had gaps: user-defined aggregates (UDAs) and nested OR with EXISTS sublinks.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_uda_simple_differential` | UDA INSERT/DELETE/UPDATE cycles | ✅ **STRONG** — multiset after each DML |
| `test_uda_combined_with_builtin` | UDA + COUNT/SUM together | ✅ **STRONG** — multiset |
| `test_uda_auto_mode_resolves_to_differential` | AUTO mode resolves correctly | ✅ **STRONG** — mode + multiset |
| `test_uda_multiple_in_same_query` | Multiple UDAs in one query | ✅ **STRONG** — multiset |
| `test_nested_or_two_exists` | OR with 2 EXISTS sublinks | ✅ **STRONG** — multiset after each DML |
| `test_nested_or_mixed_and_or_under_or` | OR(a OR (b AND EXISTS)) | ✅ **STRONG** — multiset |
| `test_nested_or_cdc_cycle` | Complex OR+EXISTS + full CDC cycle | ✅ **STRONG** — multiset after I/U/D |
| `test_nested_or_demorgan_not_and` | De Morgan NOT(AND+sublink) | ✅ **STRONG** — multiset after I/U/D |
| `test_nested_or_demorgan_and_prefix` | AND prefix + NOT(AND+sublink) | ✅ **STRONG** — multiset |
| `test_uda_with_filter_clause` | UDA with FILTER(WHERE ...) | ✅ **STRONG** — multiset |
| `test_uda_with_order_by_in_agg` | UDA with ORDER BY in aggregate | ✅ **STRONG** — multiset |
| `test_uda_schema_qualified` | Schema-qualified UDA | ✅ **STRONG** — multiset |
| `test_uda_insert_delete_update_full_cycle` | Full lifecycle: I→U→D→revival | ✅ **STRONG** — multiset after each of 6 ops |

**Verdict: STRONG — EXEMPLARY**

**All 13 tests** use `assert_st_matches_query` for full multiset comparison.
Full DML cycles (INSERT, UPDATE, DELETE) with verification at each step. This
is the gold standard for the test suite.

---

### 11. `e2e_guc_variation_tests.rs` — 430 lines, 13 tests

**Purpose:** Validates that non-default GUC configurations produce correct
results.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_guc_prepared_statements_off` | prepared_statements=OFF | ✅ **STRONG** — multiset |
| `test_guc_merge_planner_hints_off` | merge_planner_hints=OFF | ✅ **STRONG** — multiset |
| `test_guc_cleanup_use_truncate_off` | cleanup_use_truncate=OFF | ✅ **STRONG** — multiset |
| `test_guc_merge_work_mem_mb_custom` | merge_work_mem_mb=16 | ✅ **STRONG** — multiset |
| `test_guc_block_source_ddl_on` | block_source_ddl=ON prevents DDL | ✅ **STRONG** — error + multiset |
| `test_guc_differential_max_change_ratio_zero` | max_change_ratio=0.0 | ✅ **STRONG** — mode + multiset |
| `test_guc_combined_non_default` | Multiple GUCs at once | ✅ **STRONG** — multiset |
| `test_guc_max_grouping_set_branches_rejects_over_limit` | CUBE limit exceeded | ✅ Error validation |
| `test_guc_max_grouping_set_branches_allows_within_limit` | CUBE within limit | ⚠️ Creation only |
| `test_guc_max_grouping_set_branches_raised_allows_large_cube` | Raised CUBE limit | ⚠️ Creation only |
| `test_guc_foreign_table_polling_off_rejects_differential` | Foreign table polling rejected | ✅ Error validation |
| `test_guc_foreign_table_polling_full_mode_no_guc_needed` | Foreign table FULL mode | ⚠️ Creation only |
| `test_guc_foreign_table_polling_on_allows_differential` | Foreign table polling enabled | ✅ **STRONG** — multiset after I/D |

**Verdict: STRONG**

**8/13 tests** use multiset comparison. The 5 without it are boundary/error
tests where creation success/failure is the primary assertion. Minor gap:
CUBE limit tests only verify creation, not query result correctness.

---

### 12. `e2e_multi_cycle_tests.rs` — 534 lines, 9 tests

**Purpose:** Validates cumulative correctness across multiple refresh cycles
with different DML operations and cache behaviors.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_multi_cycle_aggregate_differential` | 5 cycles: I→U→D→mixed→no-op | ✅ **STRONG** — multiset after each |
| `test_multi_cycle_join_differential` | 4 JOIN cycles with left/right DML | ✅ **STRONG** — multiset after each |
| `test_multi_cycle_window_differential` | 5 INSERT + 2 DELETE cycles | ✅ **STRONG** — multiset after each |
| `test_multi_cycle_prepared_statement_cache` | 7 cycles, cache survives | ✅ **STRONG** — multiset after each |
| `test_prepared_statements_cleared_after_cache_invalidation` | Cache invalidated on ALTER | ⚠️ Scalar total + cache count |
| `test_multi_cycle_group_elimination_revival` | Group elimination + revival | ✅ **STRONG** — multiset after each |
| `test_ec16_function_body_change_marks_reinit` | Function change → reinit + correct data | ✅ Explicit sum validation (60→70→108) |
| `test_ec16_function_change_full_refresh_recovery` | Function change recovery | ✅ Explicit sum validation (215→836) |
| `test_ec16_no_functions_unaffected` | Unchanged STs unaffected | ⚠️ Flag + count |

**Verdict: STRONG**

**6/9 tests** use multiset comparison with multi-step DML cycles. The EC-16
tests use explicit sum validation which is adequate for verifying new function
logic is applied.

---

### 13. `e2e_partition_tests.rs` — 554 lines, 9 tests

**Purpose:** Validates stream tables built on partitioned source tables
(RANGE, LIST, HASH) and on foreign tables via postgres_fdw.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_partition_range_full_refresh` | RANGE partition + FULL | ⚠️ Count only |
| `test_partition_range_differential_refresh` | RANGE + INSERT/UPDATE/DELETE cycle | ⚠️ Count checks |
| `test_partition_list_source` | LIST partition | ⚠️ Count only |
| `test_partition_hash_source` | HASH partition | ⚠️ Count only |
| `test_partition_attach_triggers_reinit` | ATTACH → needs_reinit | ⚠️ Flag + count |
| `test_partition_detach_triggers_reinit` | DETACH → needs_reinit | ⚠️ Flag + count |
| `test_foreign_table_full_refresh_works` | Foreign table via postgres_fdw | ⚠️ Count only |
| `test_partition_with_aggregation` | Partitioned + GROUP BY | ⚠️ Scalar sum |
| `test_partition_differential_with_aggregation` | Partitioned + GROUP BY + INSERT | ⚠️ Scalar sum |

**Verdict: WEAK**

**Zero multiset comparisons.** All 9 tests rely on `db.count()` or scalar
aggregate checks. Test 2 has a full INSERT/UPDATE/DELETE cycle but never
verifies the actual row content.

---

### 14. `e2e_phase4_ergonomics_tests.rs` — 577 lines, 20 tests

**Purpose:** Validates API ergonomics: manual refresh history, quick_health
view, `create_if_not_exists()`, schedule defaults, removed GUCs, ALTER warnings.

| Test Group | Count | What It Validates | Assertion Quality |
|-----------|-------|-------------------|-------------------|
| ERG-D (refresh history) | 3 | `initiated_by='MANUAL'`, status/end_time | ⚠️ Metadata |
| ERG-E (quick_health) | 3 | View returns correct status | ⚠️ Metadata |
| COR-2 (create_if_not_exists) | 3 | Idempotent creation | ⚠️ Count/status |
| ERG-T1 (schedule defaults) | 5 | 'calculated' default, NULL rejection | ✅ Error + metadata |
| ERG-T2 (removed GUCs) | 2 | Old GUCs properly missing | ✅ Error validation |
| ERG-T3 (ALTER warnings) | 4 | Warnings emitted on mode/query changes | ⚠️ Notice text |

**Verdict: ADEQUATE (by design — API contract tests, not data tests)**

These tests are appropriately metadata-focused. They test the API surface,
not data correctness. No multiset comparison needed.

---

### 15. `e2e_rls_tests.rs` — 453 lines, 9 tests

**Purpose:** Validates Row-Level Security interaction with stream tables:
RLS on source, RLS on ST, change buffer security, trigger SECURITY DEFINER,
and DDL event detection for RLS changes.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_rls_on_source_does_not_filter_stream_table` | RLS on source → ST sees all rows | ⚠️ Count only |
| `test_rls_on_source_differential_mode` | RLS + DIFFERENTIAL + INSERT cycle | ⚠️ Count only |
| `test_rls_on_stream_table_filters_reads` | RLS policy on ST (superuser) | ⚠️ Count only |
| `test_rls_on_stream_table_immediate_mode` | IMMEDIATE + RLS on ST | ⚠️ Count only |
| `test_change_buffer_rls_disabled` | relrowsecurity=false on buffer | ⚠️ Boolean check |
| `test_ivm_trigger_functions_security_definer` | Triggers are SECURITY DEFINER | ⚠️ Boolean + search_path |
| `test_enable_rls_on_source_triggers_reinit` | ENABLE RLS → needs_reinit | ⚠️ Flag check |
| `test_disable_rls_on_source_triggers_reinit` | DISABLE RLS → needs_reinit | ⚠️ Flag check |
| `test_force_rls_on_source_triggers_reinit` | FORCE RLS → needs_reinit | ⚠️ Flag check |

**Verdict: WEAK**

**Zero multiset comparisons.** All tests use count or flag assertions.

**Significant gap:** Test 3 (`test_rls_on_stream_table_filters_reads`) claims
to test RLS filtering but runs as superuser, who bypasses RLS by default.
The test should query as a restricted role to verify that RLS actually filters
rows.

---

### 16. `e2e_upgrade_tests.rs` — 871 lines, 14 tests (7 active, 7 `#[ignore]`)

**Purpose:** Validates extension upgrade paths: schema stability, round-trip
(DROP + CREATE), version consistency, and upgrade chain survival.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_upgrade_catalog_schema_stability` | 31 expected columns present | ✅ **STRONG** — column list |
| `test_upgrade_catalog_indexes_present` | Expected indexes exist | ⚠️ EXISTS checks |
| `test_upgrade_drop_recreate_roundtrip` | DROP CASCADE + CREATE round-trip | ✅ **STRONG** — `assert_st_matches_query` |
| `test_upgrade_extension_version_consistency` | Version matches | ✅ String comparison |
| `test_upgrade_dependencies_schema_stability` | Dependencies schema stable | ⚠️ Column list |
| `test_upgrade_event_triggers_installed` | Event triggers exist | ⚠️ EXISTS |
| `test_upgrade_monitoring_views_present` | Views queryable | ⚠️ Queryability |
| `test_upgrade_chain_new_functions_exist` | (#[ignore]) Functions callable | ⚠️ Existence |
| `test_upgrade_chain_stream_tables_survive` | (#[ignore]) STs survive upgrade | ⚠️ Count only |
| `test_upgrade_chain_views_queryable` | (#[ignore]) Views work post-upgrade | ⚠️ Queryability |
| `test_upgrade_chain_event_triggers_present` | (#[ignore]) Triggers exist | ⚠️ EXISTS |
| `test_upgrade_chain_version_consistency` | (#[ignore]) Version correct | ⚠️ String |
| `test_upgrade_chain_function_parity_with_fresh_install` | (#[ignore]) Function count matches | ⚠️ Count |
| `test_upgrade_schema_additions_from_sql` | All SQL scripts parsed + verified | ✅ **STRONG** — regex-based |

**Verdict: ADEQUATE**

**Strength:** Test 3 (round-trip) uses `assert_st_matches_query`. Test 14
(SQL script verification) is comprehensive.

**Gap:** The 7 `#[ignore]` upgrade chain tests only use count/existence — none
verify data correctness post-upgrade.

---

### 17. `e2e_user_trigger_tests.rs` — 649 lines, 11 tests

**Purpose:** Validates user-defined trigger interaction with stream table
refresh: audit triggers, GUC control, BEFORE trigger modification, and
MERGE vs explicit DML path selection.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_explicit_dml_insert` | Audit on INSERT: NEW captured | ⚠️ Audit field-level |
| `test_explicit_dml_update` | Audit on UPDATE: OLD/NEW captured | ⚠️ Audit field-level |
| `test_explicit_dml_delete` | Audit on DELETE: OLD captured | ⚠️ Audit field-level |
| `test_explicit_dml_no_op_skip` | IS DISTINCT FROM prevents no-op trigger | ⚠️ Count check |
| `test_no_trigger_uses_merge` | No triggers → MERGE path + correct data | ✅ **STRONG** — `assert_st_matches_query` |
| `test_trigger_audit_trail` | Mixed I/U/D + audit + data correctness | ✅ **STRONG** — multiset + audit counts |
| `test_guc_off_suppresses_triggers` | GUC 'off' → audit empty | ⚠️ Audit emptiness |
| `test_guc_auto_detects_triggers` | GUC 'auto' → triggers fire | ⚠️ Audit count |
| `test_guc_on_alias_detects_triggers` | Deprecated 'on' alias works | ⚠️ Audit count |
| `test_full_refresh_suppresses_triggers` | FULL refresh → no row triggers | ⚠️ Audit emptiness |
| `test_before_trigger_modifies_new` | BEFORE trigger modifies NEW value | ⚠️ Scalar value |

**Verdict: ADEQUATE to STRONG**

**Tests 5 and 6** use multiset comparison — test 6 is especially good, combining
audit trail validation with data correctness.

---

### 18. `e2e_wal_cdc_tests.rs` — 729 lines, 17 tests

**Purpose:** Validates WAL-based CDC (logical replication): mode transitions,
INSERT/UPDATE/DELETE capture, fallback to triggers, cleanup on DROP, keyless
table handling, and health checks.

| Test | What It Validates | Assertion Quality |
|------|-------------------|-------------------|
| `test_wal_auto_is_default_cdc_mode` | Default GUC = 'auto' | ⚠️ String |
| `test_wal_level_is_logical` | Container has wal_level=logical | ⚠️ String |
| `test_explicit_wal_override_transitions_even_with_global_trigger` | Force WAL despite trigger GUC | ⚠️ Mode check |
| `test_explicit_trigger_override_blocks_wal_transition` | Force TRIGGER prevents WAL | ⚠️ Mode check |
| `test_wal_transition_lifecycle` | TRIGGER→TRANSITIONING→WAL + slot/pub | ⚠️ Mode + infrastructure |
| `test_wal_cdc_captures_insert` | INSERT captured via WAL | ⚠️ Count only |
| `test_wal_cdc_captures_update` | UPDATE captured via WAL | ⚠️ Count + scalar |
| `test_wal_cdc_captures_delete` | DELETE captured via WAL | ⚠️ Count only |
| `test_trigger_mode_no_wal_transition` | cdc_mode='trigger' stays trigger | ⚠️ Mode check |
| `test_wal_fallback_on_missing_slot` | Slot dropped → fallback + data survives | ⚠️ Mode + count |
| `test_wal_cleanup_on_drop` | DROP ST → slot + pub cleaned | ⚠️ Infrastructure |
| `test_wal_keyless_table_stays_on_triggers` | Keyless → stays trigger | ⚠️ Mode check |
| `test_ec18_check_cdc_health_shows_trigger_for_stuck_auto` | EC-18: keyless auto → TRIGGER | ⚠️ Health check |
| `test_ec18_health_check_ok_with_trigger_auto_sources` | EC-18: no errors for trigger auto | ⚠️ Count |
| `test_ec34_check_cdc_health_detects_missing_slot` | EC-34: missing slot alert + fallback | ⚠️ Alert + mode + count |
| `test_ec19_wal_keyless_without_replica_identity_full_rejected` | Keyless + no RIF rejected | ✅ Error validation |
| `test_ec19_wal_keyless_with_replica_identity_full_accepted` | Keyless + RIF accepted | ⚠️ Mode check |

**Verdict: ADEQUATE for CDC mode transitions, WEAK for WAL data correctness**

**Critical gap:** Zero multiset comparisons. Tests 6–8 (INSERT/UPDATE/DELETE
via WAL CDC) only verify count or scalar values — they never verify the actual
captured data matches the source. A WAL decoding bug that produces wrong
column values would pass all tests.

---

## Cross-Cutting Findings

### Finding 1: Multiset Comparison Usage is Bimodal

The suite splits sharply into two camps:

**Files with strong multiset coverage (≥60%):**
- `e2e_differential_gaps_tests` — 13/13 (100%)
- `e2e_dag_autorefresh_tests` — 4/5 (80%)
- `e2e_multi_cycle_tests` — 6/9 (67%)
- `e2e_guc_variation_tests` — 8/13 (62%)

**Files with weak/no multiset coverage (≤22%):**
- `e2e_ddl_event_tests` — 0/14 (0%)
- `e2e_circular_tests` — 0/6 (0%)
- `e2e_partition_tests` — 0/9 (0%)
- `e2e_rls_tests` — 0/9 (0%)
- `e2e_wal_cdc_tests` — 0/17 (0%)
- `e2e_append_only_tests` — 0/10 (0%)
- `e2e_bootstrap_gating_tests` — 0/18 (0%)
- `e2e_bench_tests` — 0/32 (0%)
- `e2e_cascade_regression_tests` — 0/8 (0%) (though uses ad-hoc value checks)
- `e2e_bgworker_tests` — 2/9 (22%)

This suggests the multiset pattern was adopted partway through development.
Files written earlier or focused on infrastructure tend to lack it.

### Finding 2: Count-Only Tests Create False Confidence

62 tests use `db.count()` as their primary data assertion. This catches:
- ✅ Missing rows (count too low)
- ✅ Duplicate rows (count too high)

But cannot catch:
- ❌ Wrong column values
- ❌ Wrong row composition (right count, wrong data)
- ❌ NULL corruption
- ❌ Type coercion bugs

For example, a partition test that verifies `count = 3` would pass even if all
three rows have incorrect values derived from the wrong partition.

### Finding 3: WAL CDC Data Path is Unvalidated

The 17 WAL CDC tests thoroughly validate mode transitions (TRIGGER → WAL),
infrastructure (slots, publications), and fallback behavior. But the actual
data path — whether WAL-decoded INSERTs/UPDATEs/DELETEs produce correct
stream table content — is verified with counts only.

This is a significant blind spot because WAL decoding involves complex binary
parsing of the replication stream, and a subtle bug could produce wrong values
that pass all count assertions.

### Finding 4: DDL Event Tests Missing Post-Reinit Validation

When a DDL change (ALTER COLUMN TYPE, function replacement, RLS change) marks
a stream table as `needs_reinit`, the tests verify:
- ✅ The `needs_reinit` flag is set
- ⚠️ The reinit can execute (sometimes)
- ❌ The data after reinit is correct (never)

This means the DDL detection works, but whether the recovery path produces
correct data is untested at the full E2E level.

### Finding 5: RLS Test Has a Superuser Bypass Flaw

`test_rls_on_stream_table_filters_reads` intends to verify that RLS filters
rows when querying a stream table. However, it appears to run queries as the
superuser, who bypasses RLS by default. The test should:
1. Create a restricted role
2. Enable RLS on the stream table
3. Query as the restricted role
4. Verify filtered results

### Finding 6: Benchmark Tests as Silent Correctness Regression Vector

The 32 benchmark tests (`#[ignore]`) exercise all major query types (scan,
filter, aggregate, join, window, lateral, CTE, UNION) with real DML cycles
and multi-cycle refreshes. Yet none assert data correctness. These tests are
actually exercising the most complex code paths in the DVM engine — adding a
single `assert_st_matches_query` call at the end of each benchmark would be
extremely high-value with negligible performance impact.

---

## Priority Mitigations

### P0 — Critical (Data Integrity Gaps)

#### P0-1: Add Multiset Comparison to WAL CDC Data Tests

Tests 6–8 (`captures_insert`, `captures_update`, `captures_delete`) should
verify data correctness after WAL-captured changes:

```rust
// Current (WEAK):
let count: i64 = db.count("wal_st").await;
assert_eq!(count, 3);

// Proposed (STRONG):
db.assert_st_matches_query("wal_st", "SELECT id, val FROM wal_source").await;
```

Also add multiset to test 10 (fallback) and test 15 (EC-34 missing slot).

**Impact:** 5 tests converted from weak to strong. Validates the entire WAL
decoding → change buffer → differential refresh pipeline.

#### P0-2: Add Multiset to Partition Tests

All non-foreign-table tests should use `assert_st_matches_query`:

```rust
// For each partition type (RANGE, LIST, HASH):
db.assert_st_matches_query("part_st", "SELECT id, val FROM part_source").await;

// For aggregation tests:
db.assert_st_matches_query("part_agg_st",
    "SELECT region, SUM(amount) FROM part_sales GROUP BY region"
).await;
```

**Impact:** 7 tests converted. Validates partition pruning doesn't corrupt
results.

#### P0-3: Add Multiset to DDL Event Post-Reinit Tests

After setting `needs_reinit` and triggering reinit, verify data:

```rust
// After function change + reinit:
db.refresh_st("fn_st").await; // triggers reinit
db.assert_st_matches_query("fn_st", "SELECT id, my_func(val) FROM source").await;

// After ALTER COLUMN TYPE + reinit:
db.refresh_st("col_st").await;
db.assert_st_matches_query("col_st", "SELECT id, val::new_type FROM source").await;
```

**Impact:** 4–6 tests improved. Validates that DDL recovery produces correct data.

#### P0-4: Add Data Verification to Circular ST Tests

After cycle convergence, verify actual data content:

```rust
db.assert_st_matches_query("cyc_a",
    "SELECT DISTINCT src, dst FROM expected_transitive_closure"
).await;
```

**Impact:** 2 tests improved. Validates convergence correctness, not just
convergence detection.

### P1 — High (Coverage Hardening)

#### P1-1: Fix RLS Superuser Bypass in Test

Add a restricted role and query as that role:

```rust
db.execute("CREATE ROLE rls_reader").await;
db.execute("GRANT SELECT ON rls_st TO rls_reader").await;
db.execute("SET ROLE rls_reader").await;
let count: i64 = db.count("rls_st").await;
assert_eq!(count, expected_filtered_count);
db.execute("RESET ROLE").await;
```

**Impact:** Validates actual RLS filtering, not just that RLS is enabled.

#### P1-2: Add Multiset to Append-Only Fallback Tests

After fallback from append-only to MERGE:

```rust
db.assert_st_matches_query("ao_st", "SELECT id, val FROM ao_source").await;
```

**Impact:** 3 tests improved. Validates fallback produces correct data.

#### P1-3: Add Multiset to Cascade Regression Tests

Tests 3 and 6 (DELETE cascade, 3-layer INSERT) should use multiset:

```rust
// 3-layer cascade:
db.assert_st_matches_query("l3_st",
    "SELECT id, val * 2 + 10 FROM base_source"
).await;
```

**Impact:** 2 tests improved.

#### P1-4: Add Multiset to Bootstrap Gating Refresh Tests

Tests 12 and 17 (manual refresh through gate lifecycle):

```rust
db.assert_st_matches_query("gated_st", "SELECT id, val FROM gated_source").await;
```

**Impact:** 2 tests improved.

### P2 — Medium (Completeness)

#### P2-1: Add Smoke Correctness Check to Benchmarks

At the end of each benchmark variant, add one `assert_st_matches_query`:

```rust
// After final benchmark cycle:
db.assert_st_matches_query(&st_name, &defining_query).await;
```

This adds ~50ms per benchmark but catches DVM correctness regressions during
performance testing.

**Impact:** 32 tests gain correctness assertion. Extremely high value.

#### P2-2: Add ALTER QUERY + DML Cycle Tests

`e2e_alter_query_tests` needs tests that:
1. Create ST, populate with data
2. ALTER QUERY to join/aggregate
3. Refresh
4. Verify with `assert_st_matches_query`

Currently, ALTER tests verify schema changes succeed but not data correctness
for complex query transformations.

#### P2-3: Add Upgrade Chain Data Validation

The 7 `#[ignore]` upgrade chain tests should add `assert_st_matches_query`
after verifying STs survive the upgrade:

```rust
// After upgrade:
db.assert_st_matches_query("pre_upgrade_st",
    "SELECT id, val FROM pre_upgrade_source"
).await;
```

#### P2-4: Add Non-Convergence Test with Guaranteed Divergence

`test_circular_nonconvergence_error_status` should use DML that guarantees
divergence (e.g., monotonically increasing counts) rather than relying on
timing.

### P3 — Low (Polish)

#### P3-1: Consolidate Cascade Value Checks to Multiset

`e2e_cascade_regression_tests` uses ad-hoc value comparisons (amount "450",
categories ["X", "Y"]). Replace with `assert_st_matches_query` for consistency
with the rest of the suite.

#### P3-2: Add DELETE/UPDATE to Bootstrap Gating Tests

Current gating tests only INSERT. Add UPDATE and DELETE during the gate →
ungate → re-gate lifecycle.

#### P3-3: Standardize bgworker Test Assertions

Tests 4 and 8 (auto-refresh within schedule, multiple STs) use count only.
Add multiset comparison for consistency.

---

## Appendix: Coverage Matrix

### Full E2E Files: Summary Table

| File | Lines | Tests | Multiset Calls | Multiset % | DML Cycle? | Verdict |
|------|-------|-------|---------------|------------|-----------|---------|
| `e2e_differential_gaps_tests` | 526 | 13 | 39 | 100% | ✅ Full I/U/D | **STRONG** |
| `e2e_dag_autorefresh_tests` | 449 | 5 | 8 | 80% | ✅ Insert cycle | **STRONG** |
| `e2e_multi_cycle_tests` | 534 | 9 | 21 | 67% | ✅ Full I/U/D | **STRONG** |
| `e2e_guc_variation_tests` | 430 | 13 | 10 | 62% | ✅ Insert/delete | **STRONG** |
| `e2e_cascade_regression_tests` | 796 | 8 | 0 | 0%* | ✅ I/U/D | **ADEQUATE** |
| `e2e_bgworker_tests` | 570 | 9 | 2 | 22% | ✅ Insert | **ADEQUATE** |
| `e2e_user_trigger_tests` | 649 | 11 | 2 | 18% | ✅ Full I/U/D | **ADEQUATE** |
| `e2e_alter_query_tests` | 578 | 15 | 1 | 7% | ⚠️ Limited | **ADEQUATE** |
| `e2e_upgrade_tests` | 871 | 14 | 1 | 7% | ⚠️ Round-trip | **ADEQUATE** |
| `e2e_bootstrap_gating_tests` | 637 | 18 | 0 | 0% | ⚠️ Insert only | **ADEQUATE** |
| `e2e_phase4_ergonomics_tests` | 577 | 20 | 0 | N/A | ❌ Metadata | **ADEQUATE** |
| `e2e_append_only_tests` | 342 | 10 | 0 | 0% | ⚠️ Insert + fallback | **ADEQUATE** |
| `e2e_ddl_event_tests` | 608 | 14 | 0 | 0% | ⚠️ DDL only | **WEAK** |
| `e2e_wal_cdc_tests` | 729 | 17 | 0 | 0% | ⚠️ Single DML | **WEAK** |
| `e2e_partition_tests` | 554 | 9 | 0 | 0% | ⚠️ Limited I/U/D | **WEAK** |
| `e2e_circular_tests` | 562 | 6 | 0 | 0% | ❌ No DML verify | **WEAK** |
| `e2e_rls_tests` | 453 | 9 | 0 | 0% | ⚠️ Insert only | **WEAK** |
| `e2e_bench_tests` | 2,156 | 32 | 0 | 0% | ✅ Multi-cycle | **WEAK** |
| **TOTAL** | **~11,021** | **222** | **84** | **17%** | — | — |

\* `e2e_cascade_regression_tests` uses ad-hoc value checks instead of `assert_st_matches_query`.

### Assertion Type Distribution

| Assertion Type | Test Count | % |
|---------------|-----------|---|
| `assert_st_matches_query` (multiset) | 37 | 17% |
| Explicit value comparison | 12 | 5% |
| Error path validation | 22 | 10% |
| Metadata / flag / status | 68 | 31% |
| Count only (`db.count()`) | 62 | 28% |
| Timing / benchmark | 32 | 14% |
| **Total** | **222** | — |

### Feature Coverage by Test File

| Feature | Test File(s) | Coverage Level |
|---------|-------------|---------------|
| Differential refresh (core) | differential_gaps, multi_cycle | ✅ Strong |
| DAG cascade + autorefresh | dag_autorefresh | ✅ Strong |
| GUC configurability | guc_variation | ✅ Strong |
| ALTER QUERY operations | alter_query | ⚠️ Adequate |
| Background worker / scheduler | bgworker | ⚠️ Adequate |
| Bootstrap gating | bootstrap_gating | ⚠️ Adequate |
| User-defined triggers | user_trigger | ⚠️ Adequate |
| Extension upgrade paths | upgrade | ⚠️ Adequate |
| ST-on-ST cascades | cascade_regression | ⚠️ Adequate |
| Append-only optimization | append_only | ⚠️ Adequate |
| API ergonomics | phase4_ergonomics | ⚠️ Adequate (metadata) |
| WAL-based CDC | wal_cdc | ❌ Weak (data path) |
| Partitioned tables | partition | ❌ Weak |
| DDL event reactions | ddl_event | ❌ Weak (post-reinit) |
| Circular dependencies | circular | ❌ Weak |
| Row-Level Security | rls | ❌ Weak |
| Performance benchmarks | bench | ❌ Weak (no correctness) |
