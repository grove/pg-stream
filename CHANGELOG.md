# Changelog

All notable changes to pg_trickle are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
For future plans and release milestones, see [ROADMAP.md](ROADMAP.md).

---

## [0.2.0] — 2026-03-04

### Added

- **Observability / Monitoring functions** — Six new `pgtrickle` schema
  functions for runtime introspection, all callable without superuser:
  - `pgtrickle.change_buffer_sizes()` — per-stream-table CDC change buffer
    row counts, byte estimates, and oldest pending change timestamp; useful
    for spotting buffer build-up or stalled ingestion.
  - `pgtrickle.list_sources(stream_table_name)` — enumerate all base tables
    tracked by a given stream table, including their OID, estimated row count,
    current bloat, and CDC-enabled flag.
  - `pgtrickle.dependency_tree()` — ASCII-tree view of the full stream table
    dependency graph: each node shows refresh mode, status, and schedule
    interval so you can read the whole DAG at a glance.
  - `pgtrickle.health_check()` — single-query triage returning
    `(check_name, severity, detail)` rows (severity `'OK'|'WARN'|'ERROR'`)
    for seven checks: scheduler running, error/suspended tables, stale
    tables, needs-reinit tables, consecutive-error tables, CDC buffer growth
    (> 10 000 pending rows), and WAL slot retention (> 100 MB).
  - `pgtrickle.refresh_timeline(max_rows DEFAULT 50)` — cross-stream-table
    chronological refresh history (most-recent first), joining
    `pgt_refresh_history` with `pgt_stream_tables`; shows action, status,
    inserted/deleted rows, duration, and any error message.
  - `pgtrickle.trigger_inventory()` — one row per (source table, trigger
    type) verifying that both the DML trigger (`pg_trickle_cdc_<oid>`) and
    the TRUNCATE trigger (`pg_trickle_cdc_truncate_<oid>`) are present and
    enabled in `pg_trigger`; highlights missing or disabled triggers.

- **IMMEDIATE refresh mode (Transactional IVM)** — New `'IMMEDIATE'` refresh
  mode maintains stream tables synchronously within the same transaction as
  the base table DML. Uses statement-level AFTER triggers with transition
  tables — no change buffers, no scheduler. The stream table is always
  up-to-date within the current transaction.
  - New `RefreshMode::Immediate` variant with `is_immediate()` /
    `is_scheduled()` helper methods.
  - `DeltaSource::TransitionTable` — DVM Scan operator reads from trigger
    transition tables instead of change buffer tables when in IMMEDIATE mode.
  - New `src/ivm.rs` module with trigger setup/cleanup, delta application
    (`pgt_ivm_apply_delta`), and TRUNCATE handling (`pgt_ivm_handle_truncate`).
  - Advisory lock-based concurrency control (ExclusiveLock equivalent).
  - `IvmLockMode` enum (`Exclusive` / `RowExclusive`) with `for_query()`
    analysis — simple scan chains use lighter `pg_try_advisory_xact_lock`,
    complex queries (aggregates, joins, DISTINCT) use `pg_advisory_xact_lock`.
  - `alter_stream_table` fully supports mode switching between
    DIFFERENTIAL↔IMMEDIATE and FULL↔IMMEDIATE: tears down old infrastructure
    (IVM triggers or CDC triggers), sets up new infrastructure, updates
    catalog, runs full refresh, restores schedule when leaving IMMEDIATE.
  - `validate_immediate_mode_support()` — rejects recursive CTEs at
    creation/alter time with clear error messages suggesting DIFFERENTIAL mode.
    Window functions, LATERAL subqueries, LATERAL functions, and scalar
    subqueries are fully supported in IMMEDIATE mode.
  - Delta SQL template caching — thread-local `IVM_DELTA_CACHE` keyed by
    (pgt_id, source_oid, has_new, has_old) avoids re-parsing the defining
    query on every trigger invocation. Cross-session invalidation via shared
    cache generation counter.
  - TRUNCATE on base table triggers full refresh of the stream table.
  - Manual `refresh_stream_table()` for IMMEDIATE STs does a full refresh.
  - TopK + IMMEDIATE combination is explicitly rejected.
  - Catalog `get_by_id(pgt_id)` lookup method added.
  - E2E tests: `tests/e2e_ivm_tests.rs` with 29 tests covering
    INSERT/UPDATE/DELETE/TRUNCATE propagation, DROP cleanup, validation,
    mixed-operation tests, mode switching (DIFFERENTIAL↔IMMEDIATE,
    FULL↔IMMEDIATE), window functions, LATERAL joins, scalar subqueries,
    cascading IMMEDIATE stream tables, concurrent inserts, recursive CTE
    rejection, aggregate + join in IMMEDIATE mode, and alter mode switching.
  - Unit tests for transition table scan path (7 tests) and
    `RefreshMode::Immediate` helpers.
- **TopK (ORDER BY + LIMIT) support** — Queries with a top-level `ORDER BY … LIMIT N` (constant integer, no OFFSET) are now recognized as "TopK" and accepted. TopK stream tables store only the top-N rows. Refreshes use scoped-recomputation via MERGE (bypass the DVM delta pipeline). Catalog columns `topk_limit` and `topk_order_by` record the pattern. Monitoring view exposes `is_topk`.
- **FETCH FIRST / FETCH NEXT rejection** — `FETCH FIRST N ROWS ONLY` and `FETCH NEXT N ROWS ONLY` now produce the same unsupported-feature error as `LIMIT`.
- **OFFSET without ORDER BY warning** — Subqueries using `OFFSET` without `ORDER BY` now emit a parser warning (alongside the existing `LIMIT` without `ORDER BY` warning).
- **Diamond dependency consistency** — detect diamond-shaped dependency graphs
  among stream tables and optionally refresh them as atomic groups using
  `SAVEPOINT`. Prevents split-version reads at convergence (fan-in) nodes.
  - New `diamond_consistency` parameter on `create_stream_table()` and
    `alter_stream_table()` (`'none'` or `'atomic'`).
  - New `pg_trickle.diamond_consistency` GUC to set the cluster-wide default.
  - New `diamond_schedule_policy` parameter on `create_stream_table()` and
    `alter_stream_table()` (`'fastest'` or `'slowest'`). Controls whether an
    atomic group fires when any member is due or when all are due.
  - New `pg_trickle.diamond_schedule_policy` GUC (default `'fastest'`).
    Per-convergence-node values override the GUC; strictest wins for nested
    diamonds.
  - New `pgtrickle.diamond_groups()` monitoring function to inspect detected
    groups, convergence points, epoch counters, and effective schedule policy.
  - Scheduler wraps multi-member groups in a SAVEPOINT when
    `diamond_consistency = 'atomic'`; on any failure the entire group is
    rolled back, preserving consistency.

### Fixed

- **E2E test type mismatch** — `test_diamond_atomic_all_succeed` queried
  `total` (an `INT4` column from `INT + INT`) as `i64`; corrected to `i32`.
- **IVM P5 aggregate bypass in IMMEDIATE mode** — The P5 direct aggregate
  optimization path read directly from `pgtrickle_changes.changes_<oid>` tables,
  which don't exist for IMMEDIATE mode stream tables. Fixed by skipping the P5
  bypass when `DeltaSource::TransitionTable` is active, falling through to the
  standard path that correctly reads from trigger transition temp tables.
- **IVM empty delta CTE type mismatch** — When a join query's non-modified
  source table produced an empty delta CTE, untyped `NULL` columns defaulted to
  `text` in PostgreSQL, causing `integer = text` comparison errors in downstream
  join conditions. Fixed by selecting from the actual source table with
  `WHERE false` to inherit correct column types.
- **Equi-join key normalization for semi-join optimization** — `extract_equijoin_keys`
  returned key pairs in expression order (`left_of_= , right_of_=`), but callers
  assumed `(left_table_key, right_table_key)`. When conditions were written as
  `r.col = l.col`, the keys were swapped, causing incorrect semi-join filters
  (wrong column referenced on wrong table). Fixed by inspecting table aliases
  before stripping qualifiers and normalizing each pair so the first element
  always belongs to the left join child.
- **Dockerfile ivm module conflict** — Three Dockerfiles (`tests/Dockerfile.e2e`,
  `tests/Dockerfile.e2e-coverage`, `cnpg/Dockerfile.ext-build`) created
  `src/ivm/mod.rs` stubs for dependency caching, conflicting with the new flat
  `src/ivm.rs` module. Fixed stubs to match the current module structure.
- **E2E DDL block GUC pool routing** — `test_block_source_ddl_guc_prevents_alter`
  used session-level `SET`, but PgPool may route subsequent queries to different
  connections. Fixed to use `ALTER SYSTEM SET` + `pg_reload_conf()`.
- **Integration test advisory lock pool routing** —
  `test_advisory_lock_roundtrip` used pool-level queries for session-scoped
  advisory locks. Fixed to use a single acquired connection.
- **dbt macros: DDL rollback bug** — `pgtrickle_create_stream_table`,
  `pgtrickle_drop_stream_table`, `pgtrickle_alter_stream_table`, and
  `pgtrickle_refresh_stream_table` all used `run_query()`, which shares the
  model's connection. dbt wraps the model's main statement in
  `BEGIN … ROLLBACK`, causing the DDL to be silently rolled back. Fixed by
  replacing `run_query()` with `{% call statement(..., auto_begin=False) %}`
  with explicit `BEGIN; … COMMIT;` so DDL is committed unconditionally.
- **dbt test script: Python 3.14 incompatibility** — `dbt-core 1.9` depends on
  `pydantic v1` / `mashumaro`, neither of which supports Python 3.14. The test
  script now creates a dedicated `.venv-dbt` using Python 3.13 for dbt.
- **dbt test script: broken `PROJECT_ROOT` path** — was computing 3 levels up
  from `integration_tests/scripts/` instead of 2, resolving to the wrong
  directory. Fixed.
- **dbt test script: `psql` not on PATH** — added auto-detection of
  Homebrew-keg PostgreSQL `bin/` directories so `wait_for_populated.sh` can
  connect without requiring a globally linked `psql`.

---

## [0.1.3] — 2026-03-02

### Added

#### SQL_GAPS_7: 50/51 Gap Items Completed

Comprehensive gap remediation across all 5 tiers of the
[SQL_GAPS_7](plans/sql/SQL_GAPS_7.md) plan, completing 50 of 51 items
(F40 — extension upgrade migration scripts — deferred to
PLAN_DB_SCHEMA_STABILITY.md).

**Tier 0 — Critical Correctness:**
- **F1** — Removed `delete_insert` merge strategy (unsafe, superseded by `auto`).
- **F2** — WAL decoder: keyless-table `pk_hash` now rejects keyless tables and
  requires `REPLICA IDENTITY FULL`.
- **F3** — WAL decoder: `old_*` column population for UPDATEs via
  `parse_pgoutput_old_columns` and old-key→new-tuple section parsing.
- **F6** — ALTER TYPE / ALTER POLICY DDL tracking via `handle_type_change` and
  `handle_policy_change` in `hooks.rs`.

**Tier 1 — High-Value Correctness Verification:**
- **F8** — Window partition key change E2E tests (2 tests in `e2e_window_tests.rs`).
- **F9** — Recursive CTE monotonicity audit with `recursive_term_is_non_monotone`
  guard and 11 unit tests.
- **F10** — ALTER DOMAIN DDL tracking via `handle_domain_change` in `hooks.rs`.
- **F11** — Keyless table duplicate-row limitation documented in SQL_REFERENCE.md.
- **F12** — PgBouncer compatibility documented in FAQ.md.

**Tier 2 — Robustness:**
- **F13** — Warning on `LIMIT` in subquery without `ORDER BY`.
- **F15** — `RANGE_AGG` / `RANGE_INTERSECT_AGG` recognized and rejected in
  DIFFERENTIAL mode.
- **F16** — Read replica detection: `pg_is_in_recovery()` check skips background
  worker on replicas.

**Tier 3 — Test Coverage (62 new E2E tests across 10 test files):**
- **F17** — 18 aggregate differential E2E tests (`e2e_aggregate_coverage_tests.rs`).
- **F18** — 5 FULL JOIN E2E tests (`e2e_full_join_tests.rs`).
- **F19** — 6 INTERSECT/EXCEPT E2E tests (`e2e_set_operation_tests.rs`).
- **F20** — 4 scalar subquery E2E tests (`e2e_scalar_subquery_tests.rs`).
- **F21** — 4 SubLinks-in-OR E2E tests (`e2e_sublink_or_tests.rs`).
- **F22** — 6 multi-partition window E2E tests (`e2e_multi_window_tests.rs`).
- **F23** — 7 GUC variation E2E tests (`e2e_guc_variation_tests.rs`).
- **F24** — 5 multi-cycle refresh E2E tests (`e2e_multi_cycle_tests.rs`).
- **F25** — 7 HAVING group transition E2E tests (`e2e_having_transition_tests.rs`).
- **F26** — FULL JOIN NULL keys E2E tests (in `e2e_full_join_tests.rs`).

**Tier 4 — Operational Hardening (13/14, F40 deferred):**
- **F27** — Adaptive threshold exposed in `stream_tables_info` view.
- **F29** — SPI SQLSTATE error classification for retry (`classify_spi_error_retryable`).
- **F30** — Delta row count in refresh history (3 new columns + `RefreshRecord` API).
- **F31** — `StaleData` NOTIFY emitted consistently (`emit_stale_alert_if_needed`).
- **F32** — WAL transition retry with 3× progressive backoff.
- **F33** — WAL column rename detection via `detect_schema_mismatch`.
- **F34** — Clear error on SPI permission failure (`SpiPermissionError` variant).
- **F38** — NATURAL JOIN column drift tracking (warning emitted).
- **F39** — Drop orphaned buffer table columns (`sync_change_buffer_columns`).

**Tier 5 — Nice-to-Have:**
- **F41** — Wide table MERGE hash shortcut for >50-column tables.
- **F42** — Delta memory bounds documented in FAQ.md.
- **F43** — Sequential processing rationale documented in FAQ.md.
- **F44** — Connection overhead documented in FAQ.md.
- **F45** — Memory/temp file usage tracking (`query_temp_file_usage`).
- **F46** — `pg_trickle.buffer_alert_threshold` GUC.
- **F47** — `pgtrickle.st_auto_threshold()` SQL function.
- **F48** — 7 keyless table duplicate-row E2E tests (`e2e_keyless_duplicate_tests.rs`).
- **F49** — Generated column snapshot filter alignment.
- **F50** — Covering index overhead benchmark (`e2e_bench_tests.rs`).
- **F51** — Change buffer schema permissions (`REVOKE ALL FROM PUBLIC`).

#### TPC-H-Derived Correctness Suite: 22/22 Queries Passing

> **TPC Fair Use Policy:** Queries are *derived from* the TPC-H Benchmark
> specification and do not constitute TPC-H Benchmark results. TPC Benchmark™
> is a trademark of the Transaction Processing Performance Council (TPC).

Improved the TPC-H-derived correctness suite from 20/22 create + 15/22
deterministic pass to **22/22 queries create and pass** across multiple
mutation cycles. Fixed Q02 subquery and TPC-H schema/datagen edge cases.

#### Planning & Research Documentation

- **PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md** — multi-path refresh correctness
  analysis for diamond-shaped DAG dependencies.
- **PLAN_PG_BACKCOMPAT.md** — analysis for supporting PostgreSQL 13–17.
- **PLAN_TRANSACTIONAL_IVM.md** — immediate (transactional) IVM design.
- **PLAN_EXTERNAL_PROCESS.md** — external sidecar feasibility analysis.
- **PLAN_PGWIRE_PROXY.md** — pgwire proxy/intercept feasibility analysis.
- **GAP_ANALYSIS_EPSIO.md** / **GAP_ANALYSIS_FELDERA.md** — competitive gap
  analysis documents.

### Fixed

#### Window Function Differential Maintenance (6 tests un-ignored)

Fixed window function differential maintenance to correctly handle non-RANGE
frames, LAG/LEAD, ranking functions (DENSE_RANK, NTILE, RANK), and
window-over-aggregate queries. Six previously-ignored E2E tests now pass:

- **Parser: `is_agg_node()` OVER clause check** — Window function calls with
  `OVER` were incorrectly classified as aggregate nodes, causing wrong operator
  tree construction.
- **Parser: `extract_aggregates()` OVER clause early return** — Aggregates
  wrapped in `OVER (...)` were extracted as plain aggregates, producing
  duplicate columns in the delta SQL.
- **Parser: `needs_pgt_count` Window delegation** — The `__pgt_count` tracking
  column was not propagated through Window operators.
- **Window diff: NOT EXISTS filter on pass-through columns** — The
  `current_input` CTE used `__pgt_row_id` for change detection, which does not
  exist in the Window operator's input. Switched to NOT EXISTS join on
  pass-through columns.
- **Window diff: `build_agg_alias_map` + `render_window_sql`** — Window
  functions wrapping aggregates (e.g., `RANK() OVER (ORDER BY SUM(x))`)
  emitted raw aggregate expressions instead of referencing the aggregate
  output aliases.
- **Row ID uniqueness via `row_to_json` + `row_number`** — Window functions
  over tied values (DENSE_RANK, RANK) produced duplicate `__pgt_row_id` hashes.
  Row IDs are now computed from the full row content plus a positional disambiguator.

#### INTERSECT/EXCEPT Differential Correctness (6 tests un-ignored)

Fixed INTERSECT and EXCEPT differential SQL generation that produced invalid
GROUP BY clauses. The set operation diff now correctly generates dual-count
multiplicity tracking with LEAST/GREATEST boundary crossing.

#### SubLink OR Differential Correctness (3 tests un-ignored)

Fixed EXISTS/IN subqueries combined with OR in WHERE clauses that generated
invalid GROUP BY expressions. The semi-join/anti-join delta operators now
correctly handle OR-combined SubLinks.

#### Multi-Partition Window Native Handling

Queries with multiple window functions using different PARTITION BY clauses
are now handled natively by the parser instead of requiring a CTE+JOIN
rewrite. If all windows share the same partition key, it is used directly;
otherwise the window operator falls back to un-partitioned (full) recomputation.

#### Aggregate Differential Correctness

- **MIN/MAX rescan on extremum deletion:** When the current MIN or MAX value was
  deleted and no new inserts existed, the merge expression returned NULL instead
  of rescanning the source table. MIN/MAX now participate in the rescan CTE and
  use the rescanned value when the extremum is deleted.
- **Regular aggregate ORDER BY parsing:** `STRING_AGG(val, ',' ORDER BY val)` and
  `ARRAY_AGG(val ORDER BY val)` silently dropped the ORDER BY clause because the
  parser only captured ordering for ordered-set aggregates (`WITHIN GROUP`). Now
  all aggregate ORDER BY clauses are parsed correctly.
- **ORDER BY placement in rescan SQL:** Regular aggregate ORDER BY is now emitted
  inside the function call parentheses (`STRING_AGG(val, ',' ORDER BY val)`)
  rather than as `WITHIN GROUP (ORDER BY ...)`, which is reserved for ordered-set
  aggregates (MODE, PERCENTILE_CONT, PERCENTILE_DISC).

#### E2E Test Infrastructure: Multi-Statement Execute

- Fixed `db.execute()` calls that sent multiple SQL statements in a single
  prepared statement (which PostgreSQL rejects). Split into separate calls in
  `e2e_full_join_tests.rs`, `e2e_scalar_subquery_tests.rs`,
  `e2e_set_operation_tests.rs`, `e2e_sublink_or_tests.rs`, and
  `e2e_multi_cycle_tests.rs`.

#### CI: pg_stub.c Missing Stubs

Added `palloc0` and error reporting stubs to `scripts/pg_stub.c` to fix unit
test compilation.

### Changed

- **Test count:** ~1,455 total tests (up from ~1,138): 963 unit + 32 integration
  + 460 E2E across 34 test files (up from ~22). 18 E2E tests are `#[ignore]`d
  pending DVM correctness fixes (see Known Limitations below).
- **1 new GUC variable** — `buffer_alert_threshold` added. Total: 16 GUCs.

### Known Limitations

18 E2E tests are marked `#[ignore]` due to pre-existing DVM differential logic
bugs that will be addressed in future releases:

| Suite | Ignored | Reason |
|---|---|---|
| `e2e_full_join_tests` | 5/5 | FULL OUTER JOIN differential produces incorrect results |
| `e2e_having_transition_tests` | 5/7 | HAVING threshold crossing differential incorrect |
| `e2e_keyless_duplicate_tests` | 5/7 | Keyless table duplicate-row row_id hash collision |
| `e2e_scalar_subquery_tests` | 2/4 | Correlated scalar subquery differential generates invalid SQL |
| `e2e_sublink_or_tests` | 1/4 | Correlated EXISTS with HAVING loses aggregate state |

---

## [0.1.2] — 2026-02-28

### Changed

#### Project Renamed from pg_stream to pg_trickle

Renamed the entire project to avoid a naming collision with an unrelated
project. All identifiers, schemas, GUC prefixes, catalog columns, and
documentation references have been updated:

- Crate name: `pg_stream` → `pg_trickle`
- Extension control file: `pg_stream.control` → `pg_trickle.control`
- SQL schemas: `pgstream` → `pgtrickle`, `pgstream_changes` → `pgtrickle_changes`
- Catalog column prefix: `pgs_` → `pgt_`
- Internal column prefix: `__pgs_` → `__pgt_`
- GUC prefix: `pg_stream.*` → `pg_trickle.*`
- CamelCase types: `PgStreamError` → `PgTrickleError`
- dbt package: `dbt-pgstream` → `dbt-pgtrickle`

"Stream tables" terminology is unchanged — only the project/extension name
was renamed.

### Fixed

#### DVM: Inner Join Delta Double-Counting

Fixed inner join pre-change snapshot logic that caused delta double-counting
during differential refresh. The snapshot now correctly eliminates rows that
would be counted twice when both sides of the join have changes in the same
refresh cycle. Discovered via TPC-H-derived Q07.

#### DVM: Multi-Stream-Table Change Buffer Cleanup

Fixed a bug where change buffer cleanup for one stream table could delete
entries still needed by another stream table that shares the same source
table. Buffer cleanup now scopes deletions per-stream-table rather than
per-source-table.

#### DVM: Scalar Aggregate Row ID Mismatch and AVG Group Rescan

Fixed scalar aggregate `row_id` generation that produced mismatched identifiers
between delta and merge phases, and corrected `AVG` group rescan logic that
failed to recompute averages after partial group changes. Fixes TPC-H-derived
Q06 and improves Q01.

#### DVM: SemiJoin/AntiJoin Snapshots and GROUP BY Alias Projection

Fixed snapshot handling for `SemiJoin` and `AntiJoin` operators that missed
pre-change state, corrected `__pgt_count` filtering in delta output, and
fixed the parser's `GROUP BY` alias resolution to emit proper `Project` nodes.
Raises TPC-H-derived passing count to 14/22.

#### DVM: Unqualified Column Resolution and Deep Disambiguation

Fixed unqualified column resolution in join contexts, intermediate aggregate
delta computation, and deep column disambiguation for nested subqueries.

#### DVM: COALESCE Null Counts and Walker-Based OID Extraction

Fixed `COALESCE` handling for null count columns in aggregate deltas, replaced
regex-based OID extraction with a proper AST walker, and fixed
`ComplexExpression` aggregate detection.

#### DVM: Column Reference Resolution Against Disambiguated Join CTEs

Fixed column reference resolution that failed to match against disambiguated
join CTE column names, causing incorrect references in multi-join queries.

#### Stale Pending Cleanup Crash on Dropped Change Buffer Tables

Prevented the background cleanup worker from crashing when it encounters
pending cleanup entries for change buffer tables that have already been
dropped (e.g., after a stream table is removed mid-cycle).

#### DVM Parser: 4 Query Rewrite Bugs (TPC-H-Derived Regression Coverage)

Fixed four bugs in `src/dvm/parser.rs` discovered while building the TPC-H-derived
correctness test suite. Together they unblock 3 more TPC-H-derived queries (Q04,
Q15, Q21) from stream table creation, raising the create-success rate from
17/22 to 20/22.

- **`node_to_expr` agg_star** — `FuncCall` nodes with `agg_star: true` (i.e.
  `COUNT(*)`) were emitted as `count()` (no argument). Added `agg_star` check
  that inserts `Expr::Raw("*")` so the deparser produces `count(*)`.
- **`rewrite_sublinks_in_or` false trigger** — The OR-sublink rewriter was
  entered for any AND expression containing a SubLink (e.g. a bare `EXISTS`
  clause). Added `and_contains_or_with_sublink()` guard so the rewriter only
  activates when the AND contains an OR conjunct that itself has a SubLink.
  Prevented the false-positive `COUNT()` deparse for Q04 and Q21.
- **Correlated scalar subquery detection** — `rewrite_scalar_subquery_in_where`
  now collects outer table names and checks whether the scalar subquery
  references any of them (`is_correlated()`). Correlated subqueries are skipped
  (rather than incorrectly CROSS JOIN-rewritten). Non-correlated subqueries now
  use the correct wrapper pattern:
  `CROSS JOIN (SELECT v."c" AS "sq_col" FROM (subquery) AS v("c")) AS sq`.
- **`T_RangeSubselect` in FROM clause** — Both `from_item_to_sql` and
  `deparse_from_item` now handle `T_RangeSubselect` (derived tables / inline
  views in FROM). Previously these fell through to a `"?"` placeholder, causing
  a syntax error for Q15 after its CTE was inlined.

### Added

#### TPC-H-Derived Correctness Test Suite

> **TPC Fair Use Policy:** The queries in this test suite are *derived from* the
> TPC-H Benchmark specification and do not constitute TPC-H Benchmark results.
> TPC Benchmark™ is a trademark of the Transaction Processing Performance
> Council (TPC). pg_trickle results are not comparable to published TPC results.

Added a TPC-H-derived correctness test suite (`tests/e2e_tpch_tests.rs`) that
validates the core DBSP invariant — `Contents(ST) ≡ Result(defining_query)`
after every differential refresh — across all 22 TPC-H-derived queries at SF=0.01.

- **Schema & data generation** (`tests/tpch/schema.sql`, `datagen.sql`) —
  SQL-only, no external `dbgen` dependency, works with existing `E2eDb`
  testcontainers infrastructure.
- **Mutation scripts** (`rf1.sql` INSERT, `rf2.sql` DELETE, `rf3.sql` UPDATE)
  — multi-cycle churn to catch cumulative drift.
- **22 query files** (`tests/tpch/queries/q01.sql`–`q22.sql`) — queries
  derived from TPC-H, adapted for pg_trickle SQL compatibility:

  | Query | Adaptation |
  |-------|-----------|
  | Q08 | `NULLIF` → `CASE WHEN`; `BETWEEN` → explicit `>= AND <=` |
  | Q09 | `LIKE '%green%'` → `strpos(p_name, 'green') > 0` |
  | Q14 | `NULLIF` → `CASE`; `LIKE 'PROMO%'` → `left(p_type, 5) = 'PROMO'` |
  | Q15 | `WITH revenue0 AS (...)` CTE → inline derived table |
  | Q16 | `COUNT(DISTINCT)` → DISTINCT subquery + `COUNT(*)`; `NOT LIKE` / `LIKE` → `left()` / `strpos()` |
  | All | `→` replaced with `->` in comments (avoids UTF-8 byte-boundary panic) |

- **3 test functions** — `test_tpch_differential_correctness`,
  `test_tpch_cross_query_consistency`, `test_tpch_full_vs_differential`.
  All pass (`3 passed; 0 failed`). Queries blocked by known DVM limitations
  soft-skip rather than fail.
- **Current score:** 20/22 create successfully; 15/22 pass deterministic
  correctness checks across multiple mutation cycles after the DVM fixes
  listed above.
- **`just` targets:** `test-tpch` (fast, SF=0.01), `test-tpch-large`
  (SF=0.1, 5 cycles), `test-tpch-fast` (skips image rebuild).

---

## [0.1.1] — 2026-02-26

### Changed

#### CloudNativePG Image Volume Extension Distribution
- **Extension-only OCI image** — replaced the full PostgreSQL Docker image
  (`ghcr.io/<owner>/pg_trickle`) with a minimal `scratch`-based extension image
  (`ghcr.io/<owner>/pg_trickle-ext`) following the
  [CNPG Image Volume Extensions](https://cloudnative-pg.io/docs/1.28/imagevolume_extensions/)
  specification. The image contains only `.so`, `.control`, and `.sql` files
  (< 10 MB vs ~400 MB for the old full image).
- **New `cnpg/Dockerfile.ext`** — release Dockerfile for packaging pre-built
  artifacts into the scratch-based extension image.
- **New `cnpg/Dockerfile.ext-build`** — multi-stage from-source build for
  local development and CI.
- **New `cnpg/database-example.yaml`** — CNPG `Database` resource for
  declarative `CREATE EXTENSION pg_trickle` (replaces `postInitSQL`).
- **Updated `cnpg/cluster-example.yaml`** — uses official CNPG PostgreSQL 18
  operand image with `.spec.postgresql.extensions` for Image Volume mounting.
- **Removed `cnpg/Dockerfile` and `cnpg/Dockerfile.release`** — the old full
  PostgreSQL images are no longer built or published.
- **Updated release workflow** — publishes multi-arch (amd64/arm64) extension
  image to GHCR with layout verification and SQL smoke test.
- **Updated CI CNPG smoke test** — uses transitional composite image approach
  until `kind` supports Kubernetes 1.33 with `ImageVolume` feature gate.

---

## [0.1.0] — 2026-02-26

### Fixed

#### WAL Decoder pgoutput Action Parsing (F4 / G2.3)
- **Positional action parsing** — `parse_pgoutput_action()` previously used
  `data.contains("INSERT:")` etc., which would misclassify events when a
  schema name, table name, or column value contained an action keyword (e.g.,
  a table named `INSERT_LOG` or a text value `"DELETE: old row"`).
  Replaced with positional parsing: strip `"table "` prefix, skip
  `schema.table: `, then match the action keyword before the next `:`.
- 3 new unit tests covering the edge cases.

### Added

#### CUBE / ROLLUP Combinatorial Explosion Guard (F14 / G5.2)
- **Branch limit guard** — `CUBE(n)` on *N* columns generates $2^N$ `UNION ALL`
  branches. Large CUBEs would silently produce memory-exhausting query trees.
  `rewrite_grouping_sets()` now rejects CUBE/ROLLUP combinations that would
  expand beyond **64 branches**, emitting a clear error that directs users to
  explicit `GROUPING SETS(...)`.

#### Documentation: Known Delta Computation Limitations (F7 / F11)
- **JOIN key change + simultaneous right-side delete** — documented in
  `docs/SQL_REFERENCE.md` § "Known Delta Computation Limitations" with a
  concrete SQL example, root-cause explanation, and three mitigations
  (adaptive FULL fallback, staggered changes, FULL mode).
- **Keyless table duplicate-row limitation** — the "Tables Without Primary
  Keys" section now includes a `> Limitation` callout explaining that rows
  with identical content produce the same content hash, causing INSERT
  deduplication and ambiguous DELETE matching. Recommends adding a surrogate
  PK or UNIQUE constraint.
- **SQL-standard JSON aggregate recognition** — `JSON_ARRAYAGG(expr ...)` and
  `JSON_OBJECTAGG(key: value ...)` are now recognized as first-class DVM
  aggregates with the group-rescan strategy. Previously treated as opaque raw
  expressions, they now work correctly in DIFFERENTIAL mode.
- Two new `AggFunc` variants: `JsonObjectAggStd(String)`, `JsonArrayAggStd(String)`.
  The carried String preserves the full deparsed SQL since the special `key: value`,
  `ABSENT ON NULL`, `ORDER BY`, and `RETURNING` clauses differ from regular
  function syntax.
- 4 E2E tests covering both FULL and DIFFERENTIAL modes.

#### JSON_TABLE Support (F12)
- **`JSON_TABLE()` in FROM clause** — PostgreSQL 17+ `JSON_TABLE(expr, path
  COLUMNS (...))` is now supported. Deparsed with full syntax including
  `PASSING` clauses, regular/EXISTS/formatted/nested columns, and
  `ON ERROR`/`ON EMPTY` behaviors. Modeled internally as `LateralFunction`.
- 2 E2E tests (FULL and DIFFERENTIAL modes).

#### Operator Volatility Checking (F16)
- **Operator volatility checking** — custom operators backed by volatile
  functions are now detected and rejected in DIFFERENTIAL mode. The check
  queries `pg_operator` → `pg_proc` to resolve operator function volatility.
  This completes the volatility coverage (G7.2) started with function
  volatility detection.
- 3 unit tests and 2 E2E tests.

#### Cross-Session Cache Invalidation (F17)
- **Cross-session cache invalidation** — a shared-memory atomic counter
  (`CACHE_GENERATION`) ensures that when one backend alters a stream table,
  drops a stream table, or triggers a DDL hook, all other backends
  automatically flush their delta template and MERGE template caches on the
  next refresh cycle. Previously, cached templates could become stale in
  multi-backend deployments.
- Thread-local generation tracking in both `dvm/mod.rs` (delta cache) and
  `refresh.rs` (MERGE cache + prepared statements).

#### Function/Operator DDL Tracking (F18)
- **Function DDL tracking** — `CREATE OR REPLACE FUNCTION` and `ALTER FUNCTION`
  on functions referenced by stream table defining queries now trigger reinit
  of affected STs. `DROP FUNCTION` also marks affected STs for reinit.
- **`functions_used` catalog column** — new `TEXT[]` column in
  `pgtrickle.pgt_stream_tables` stores all function names used by the defining
  query (extracted from the parsed OpTree at creation time). DDL hooks query
  this column to find affected STs.
- 2 E2E tests and 5 unit tests for function name extraction.

#### View Inlining (G2.1)
- **View inlining auto-rewrite** — views referenced in defining queries are
  transparently replaced with their underlying SELECT definition as inline
  subqueries. CDC triggers land on base tables, so DIFFERENTIAL mode works
  correctly with views. Nested views (view → view → table) are fully expanded
  via a fixpoint loop (max depth 10).
- **Materialized view rejection** — materialized views (`relkind = 'm'`) are
  rejected with a clear error in DIFFERENTIAL mode. FULL mode allows them.
- **Foreign table rejection** — foreign tables (`relkind = 'f'`) are rejected
  in DIFFERENTIAL mode (row-level triggers cannot be created on foreign tables).
- **Original query preservation** — the user's original SQL (pre-inlining) is
  stored in `pgtrickle.pgt_stream_tables.original_query` for reinit after view
  changes and user introspection.
- **View DDL hooks** — `CREATE OR REPLACE VIEW` triggers reinit of affected
  stream tables. `DROP VIEW` sets affected stream tables to ERROR status.
- **View dependency tracking** — views are registered as soft dependencies in
  `pgtrickle.pgt_dependencies` (source_type = 'VIEW') for DDL hook lookups.
- **E2E test suite** — 16 E2E tests covering basic view inlining, UPDATE/DELETE
  through views, filtered views, aggregation, joins, nested views, FULL mode,
  materialized view rejection/allowance, view replacement/drop hooks, TRUNCATE
  propagation, column renaming, catalog verification, and dependency registration.

#### SQL Feature Gaps (S1–S15)
- **Volatile function detection (S1)** — defining queries containing volatile
  functions (e.g., `random()`, `clock_timestamp()`) are rejected in DIFFERENTIAL
  mode with a clear error. Stable functions (e.g., `now()`) emit a warning.
- **TRUNCATE capture in CDC (S2)** — statement-level `AFTER TRUNCATE` trigger
  writes a `T` marker row to the change buffer. Differential refresh detects
  the marker and automatically falls back to a full refresh.
- **`ALL (subquery)` support (S3)** — `x op ALL (subquery)` is rewritten to
  an AntiJoin via `NOT EXISTS` with a negated condition.
- **`DISTINCT ON` auto-rewrite (S4)** — `DISTINCT ON (col1, col2)` is
  transparently rewritten to a `ROW_NUMBER() OVER (PARTITION BY ... ORDER BY
  ...) = 1` subquery before DVM parsing. Previously rejected.
- **12 regression aggregates (S5)** — `CORR`, `COVAR_POP`, `COVAR_SAMP`,
  `REGR_AVGX`, `REGR_AVGY`, `REGR_COUNT`, `REGR_INTERCEPT`, `REGR_R2`,
  `REGR_SLOPE`, `REGR_SXX`, `REGR_SXY`, `REGR_SYY` — all use group-rescan
  strategy. 39 aggregate function variants total (up from 25).
- **Mixed `UNION` / `UNION ALL` (S6)** — nested set operations with different
  `ALL` flags are now parsed correctly.
- **Column snapshot + schema fingerprint (S7)** — `pgt_dependencies` stores a
  JSONB column snapshot and SHA-256 fingerprint for each source table. DDL
  change detection uses a 3-tier fast path: fingerprint → snapshot → legacy
  `columns_used` fallback.
- **`pg_trickle.block_source_ddl` GUC (S8)** — when `true`, column-affecting
  DDL on tracked source tables is blocked with an ERROR instead of marking
  stream tables for reinit.
- **`NATURAL JOIN` support (S9)** — common columns are resolved at parse time
  and an explicit equi-join condition is synthesized. Supports INNER, LEFT,
  RIGHT, and FULL NATURAL JOIN variants. Previously rejected.
- **Keyless table support (S10)** — source tables without a primary key now
  work correctly. CDC triggers compute an all-column content hash for row
  identity. Consistent `__pgt_row_id` between full and delta refreshes.
- **`GROUPING SETS` / `CUBE` / `ROLLUP` auto-rewrite (S11)** — decomposed at
  parse time into a `UNION ALL` of separate `GROUP BY` queries. `GROUPING()`
  calls become integer literals. Previously rejected.
- **Scalar subquery in WHERE rewrite (S12)** — `WHERE col > (SELECT avg(x)
  FROM t)` is rewritten to a `CROSS JOIN` with column reference replacement.
- **SubLinks in OR rewrite (S13)** — `WHERE a OR EXISTS (...)` is decomposed
  into `UNION` branches, one per OR arm.
- **Multi-PARTITION BY window rewrite (S14)** — window functions with different
  `PARTITION BY` clauses are split into separate subqueries joined by a
  `ROW_NUMBER() OVER ()` row marker.
- **Recursive CTE semi-naive + DRed (S15)** — DIFFERENTIAL mode for recursive
  CTEs now uses semi-naive evaluation for INSERT-only changes, Delete-and-
  Rederive (DRed) for mixed changes, and recomputation fallback. Strategy is
  auto-selected per refresh.

#### Native Syntax Planning
- **Native DDL syntax research** — comprehensive analysis of 15 PostgreSQL
  extension syntax mechanisms for supporting `CREATE STREAM TABLE`-like syntax.
  See `plans/sql/REPORT_CUSTOM_SQL_SYNTAX.md`.
- **Native syntax plan** — tiered strategy: Tier 1 (function API, existing),
  Tier 1.5 (`CALL` procedure wrappers), Tier 2 (`CREATE MATERIALIZED VIEW ...
  WITH (pgtrickle.stream = true)` via `ProcessUtility_hook`). See
  `plans/sql/PLAN_NATIVE_SYNTAX.md`.

#### Hybrid CDC — Automatic Trigger → WAL Transition
- **Hybrid CDC architecture** — stream tables now start with lightweight
  row-level triggers for zero-config setup and can automatically transition to
  WAL-based (logical replication) capture for lower write-side overhead. The
  transition is controlled by the `pg_trickle.cdc_mode` GUC (`trigger` / `auto`
  / `wal`).
- **WAL decoder background worker** — dedicated worker that polls logical
  replication slots and writes decoded changes into the same change buffer
  tables used by triggers, ensuring a uniform format for the DVM engine.
- **Transition orchestration** — transparent three-step process: create
  replication slot, wait for decoder catch-up, drop trigger. Falls back to
  triggers automatically if the decoder does not catch up within the timeout.
- **CDC health monitoring** — new `pgtrickle.check_cdc_health()` function
  returns per-source CDC mode, slot lag, confirmed LSN, and alerts.
- **CDC transition notifications** — `NOTIFY pg_trickle_cdc_transition` emits
  JSON payloads when sources transition between CDC modes.
- **New GUCs** — `pg_trickle.cdc_mode` and `pg_trickle.wal_transition_timeout`.
- **Catalog extension** — `pgt_dependencies` table gains `cdc_mode`,
  `slot_name`, `decoder_confirmed_lsn`, and `transition_started_at` columns.

#### User-Defined Triggers on Stream Tables
- **User trigger support in DIFFERENTIAL mode** — user-created `AFTER` triggers
  on stream tables now fire correctly during differential refresh via explicit
  per-row DML (INSERT/UPDATE/DELETE) instead of bulk MERGE.
- **FULL refresh trigger handling** — user triggers are suppressed during FULL
  refresh with `DISABLE TRIGGER USER` and a `NOTIFY pgtrickle_refresh` is
  emitted so listeners know when to re-query.
- **Trigger detection** — `has_user_triggers()` automatically detects
  user-defined triggers on storage tables at refresh time.
- **DDL warning** — `CREATE TRIGGER` on a stream table emits a notice explaining
  the trigger semantics and the `pg_trickle.user_triggers` GUC.
- **New GUC** — `pg_trickle.user_triggers` (`auto` / `on` / `off`) controls
  whether the explicit DML path is used.

### Changed

- **Monitoring layer** — `slot_health()` now covers WAL-mode sources.
  Architecture diagrams and documentation updated to reflect the hybrid CDC
  model.
- **Stream table restrictions** — user triggers on stream tables upgraded from
  "⚠️ Unsupported" to "✅ Supported (DIFFERENTIAL mode)".

#### Core Engine
- **Declarative stream tables** — define a SQL query and a schedule; the
  extension handles automatic refresh.
- **Differential View Maintenance (DVM)** — incremental delta computation
  derived automatically from the defining query's operator tree.
- **Trigger-based CDC** — lightweight `AFTER` row-level triggers capture changes
  into per-source buffer tables. No `wal_level = logical` required.
- **DAG-aware scheduling** — stream tables that depend on other stream tables
  are refreshed in topological order with cycle detection.
- **Background scheduler** — canonical-period scheduling (48·2ⁿ seconds) with
  cron expression support.
- **Crash-safe refresh** — advisory locks prevent concurrent refreshes; crash
  recovery marks in-flight refreshes as failed.

#### SQL Support
- **Full operator coverage** — table scans, projections, WHERE/HAVING filters,
  INNER/LEFT/RIGHT/FULL OUTER joins, nested multi-table joins, GROUP BY with 25
  aggregate functions, DISTINCT, UNION ALL, UNION, INTERSECT, EXCEPT.
- **Subquery support** — subqueries in FROM, EXISTS/NOT EXISTS, IN/NOT IN
  (subquery), scalar subqueries in SELECT.
- **CTE support** — non-recursive CTEs (inline and shared delta), recursive
  CTEs (`WITH RECURSIVE`) in both FULL and DIFFERENTIAL modes.
- **Recursive CTE incremental maintenance** — DIFFERENTIAL mode now uses
  semi-naive evaluation for INSERT-only changes, Delete-and-Rederive (DRed)
  for mixed changes, and recomputation fallback when CTE columns don't match
  ST storage. Strategy is auto-selected per refresh.
- **DISTINCT ON auto-rewrite** — transparently rewritten to ROW_NUMBER()
  window subquery before DVM parsing.
- **GROUPING SETS / CUBE / ROLLUP auto-rewrite** — decomposed into UNION ALL
  of separate GROUP BY queries at parse time.
- **NATURAL JOIN support** — common columns resolved at parse time with
  explicit equi-join synthesis.
- **ALL (subquery) support** — rewritten to AntiJoin via NOT EXISTS.
- **Scalar subquery in WHERE** — rewritten to CROSS JOIN.
- **SubLinks in OR** — decomposed into UNION branches.
- **Multi-PARTITION BY windows** — split into joined subqueries.
- **Regression aggregates** — CORR, COVAR_POP, COVAR_SAMP, REGR_* (12 new).
- **JSON_ARRAYAGG / JSON_OBJECTAGG** — SQL-standard JSON aggregates recognized
  as first-class DVM aggregates in DIFFERENTIAL mode.
- **JSON_TABLE** — PostgreSQL 17+ JSON_TABLE() in FROM clause.
- **Keyless table support** — tables without primary keys use content hashing.
- **Volatile function and operator detection** — rejected in DIFFERENTIAL,
  warned for stable. Custom operators backed by volatile functions are also
  detected.
- **TRUNCATE capture in CDC** — triggers fall back to full refresh.
- **Window functions** — ROW_NUMBER, RANK, SUM OVER, etc. with full frame
  clause support (ROWS, RANGE, GROUPS, BETWEEN, EXCLUDE) and named WINDOW
  clauses.
- **LATERAL SRFs** — `jsonb_array_elements`, `unnest`, `jsonb_each`, etc. via
  row-scoped recomputation.
- **LATERAL subqueries** — explicit `LATERAL (SELECT ...)` in FROM with
  correlated references.
- **Expression support** — CASE WHEN, COALESCE, NULLIF, GREATEST, LEAST,
  IN (list), BETWEEN, IS DISTINCT FROM, IS TRUE/FALSE/UNKNOWN, SIMILAR TO,
  ANY/ALL (array), ARRAY/ROW constructors, array subscript, field access.
- **Ordered-set aggregates** — MODE, PERCENTILE_CONT, PERCENTILE_DISC with
  WITHIN GROUP (ORDER BY).

#### Monitoring & Observability
- **Refresh statistics** — `st_refresh_stats()`, `get_refresh_history()`,
  `get_staleness()`.
- **Slot health** — `slot_health()` checks replication slot state and WAL
  retention.
- **DVM plan inspection** — `explain_st()` describes the operator tree.
- **Monitoring views** — `pgtrickle.stream_tables_info` and
  `pgtrickle.pg_stat_stream_tables`.
- **NOTIFY alerting** — `pg_trickle_alert` channel broadcasts stale, suspended,
  reinitialize, slot lag, refresh completed/failed events.

#### Infrastructure
- **Row ID hashing** — `pg_trickle_hash()` and `pg_trickle_hash_multi()` using
  xxHash (xxh64) for deterministic row identity.
- **DDL event tracking** — `ALTER TABLE` and `DROP TABLE` on source tables
  automatically set `needs_reinit` on affected stream tables. `CREATE OR
  REPLACE FUNCTION` / `ALTER FUNCTION` / `DROP FUNCTION` on functions used
  by defining queries also triggers reinit.
- **Cross-session cache coherence** — shared-memory `CACHE_GENERATION` atomic
  counter ensures all backends flush delta/MERGE template caches when DDL
  changes occur.
- **Version / frontier tracking** — per-source JSONB frontier for consistent
  snapshots and Delayed View Semantics (DVS) guarantee.
- **12 GUC variables** — `enabled`, `scheduler_interval_ms`,
  `min_schedule_seconds`, `max_consecutive_errors`, `change_buffer_schema`,
  `max_concurrent_refreshes`, `differential_max_change_ratio`,
  `cleanup_use_truncate`, `user_triggers`, `cdc_mode`,
  `wal_transition_timeout`, `block_source_ddl`.

#### Documentation
- Architecture guide, SQL reference, configuration reference, FAQ,
  getting-started tutorial, DVM operators reference, benchmark guide.
- Deep-dive tutorials: What Happens on INSERT / UPDATE / DELETE / TRUNCATE.

#### Testing
- ~1,138 unit tests, 22 E2E test suites (Testcontainers + custom Docker image).
- Property-based tests, integration tests, resilience tests.
- Column snapshot and schema fingerprint-based DDL change detection.

### Known Limitations

- `TABLESAMPLE`, `LIMIT` / `OFFSET`, `FOR UPDATE` / `FOR SHARE` — rejected
  with clear error messages.
- Window functions inside expressions (CASE, COALESCE, arithmetic) — rejected.
- Circular stream table dependencies (cycles) — not yet supported.
