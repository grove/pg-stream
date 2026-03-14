# Changelog

All notable changes to pg_trickle are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
For future plans and release milestones, see [ROADMAP.md](ROADMAP.md).

---

## [Unreleased]

---

## [0.5.0] — 2026-03-13

### Added

#### Row-Level Security (RLS) Support

Stream tables now work correctly with PostgreSQL's Row-Level Security feature,
which lets you control which rows different users can see.

- **Refreshes always see all data.** When a stream table is refreshed, it
  computes the full result regardless of RLS policies on the source tables.
  This matches how PostgreSQL's built-in materialized views work. You then
  add RLS policies directly on the stream table to control who can read what.
- **Internal tables are protected.** The internal change-tracking tables used
  by pg_trickle are shielded from RLS interference, so refreshes won't
  silently fail if you turn on RLS at the schema level.
- **Real-time (IMMEDIATE) mode secured.** Triggers that keep stream tables
  updated in real time now run with elevated privileges and a locked-down
  search path, preventing data corruption or security bypasses.
- **RLS changes are detected automatically.** If you enable, disable, or force
  RLS on a source table, pg_trickle detects the change and marks affected
  stream tables for a full rebuild.
- **New tutorial.** Step-by-step guide for setting up per-tenant RLS policies
  on stream tables (see `docs/tutorials/ROW_LEVEL_SECURITY.md`).

#### Source Gating for Bulk Loads

New pause/resume mechanism for large data imports. When you're loading a big
batch of data into a source table, you can temporarily "gate" it to prevent
the background scheduler from triggering refreshes mid-load. Once the load is
done, ungate it and everything catches up in a single refresh.

- **`gate_source('my_table')`** — pauses automatic refreshes for any stream
  table that depends on `my_table`.
- **`ungate_source('my_table')`** — resumes automatic refreshes. All changes
  made during the gate are picked up in the next refresh cycle.
- **`source_gates()`** — shows which source tables are currently gated, when
  they were gated, and by whom.
- **Manual refresh still works.** Even while a source is gated, you can
  explicitly call `refresh_stream_table()` if needed.
- Gating is idempotent — calling `gate_source()` twice is safe, and gating a
  source that's already gated is a no-op.

#### Append-Only Fast Path

Significant performance improvement for tables that only receive INSERTs
(event logs, audit trails, time-series data, etc.). When you mark a stream
table as `append_only`, refreshes skip the expensive merge logic (checking
for deletes, updates, and row comparisons) and use a simple, fast insert.

- **How to use:** Pass `append_only => true` when creating or altering a
  stream table.
- **Safe fallback.** If a DELETE or UPDATE is detected on a source table, the
  extension automatically falls back to the standard refresh path and logs a
  warning. It won't silently produce wrong results.
- **Restrictions.** Append-only mode requires DIFFERENTIAL refresh mode and
  source tables with primary keys.

#### Usability Improvements

- **Manual refresh history.** When you manually call `refresh_stream_table()`,
  the result (success or failure, timing, rows affected) is now recorded in
  the refresh history, just like scheduled refreshes.
- **`quick_health` view.** A single-row health summary showing how many stream
  tables you have, how many are in error or stale, whether the scheduler is
  running, and an overall status (`OK`, `WARNING`, `CRITICAL`). Easy to plug
  into monitoring dashboards.
- **`create_stream_table_if_not_exists()`.** A convenience function that does
  nothing if the stream table already exists, instead of raising an error.
  Makes migration scripts and deployment automation simpler.

#### Smooth Upgrade from 0.4.0

- Existing installations can upgrade with
  `ALTER EXTENSION pg_trickle UPDATE TO '0.5.0'`. All new features (source
  gating, append-only mode, quick health view, and the new convenience
  functions) are included in the upgrade script.
- The upgrade has been verified with automated tests that confirm all 40 SQL
  objects survive the upgrade intact.

---

## [0.4.0] — 2026-03-12

### Added

#### Parallel Refresh (opt-in)

Stream tables can now be refreshed in parallel, using multiple background
workers instead of processing them one at a time. This can dramatically reduce
end-to-end refresh latency when you have many independent stream tables.

- **Off by default.** Set `pg_trickle.parallel_refresh_mode = 'on'` to enable.
  Use `'dry_run'` to preview what the scheduler would do without changing
  behavior.
- **Automatic dependency awareness.** The scheduler figures out which stream
  tables can safely refresh at the same time and which must wait for others.
  Stream tables connected by real-time (IMMEDIATE) triggers are always
  refreshed together to prevent race conditions.
- **Atomic groups.** When a group of stream tables must succeed or fail
  together (e.g. diamond dependencies), all members are wrapped in a single
  transaction — if one fails, the whole group rolls back cleanly.
- **Worker pool controls:**
  - `pg_trickle.max_dynamic_refresh_workers` (default 4) — cluster-wide cap on
    concurrent refresh workers.
  - `pg_trickle.max_concurrent_refreshes` — per-database dispatch cap.
- **Monitoring:**
  - `worker_pool_status()` — shows how many workers are active and the current
    limits.
  - `parallel_job_status(max_age_seconds)` — lists recent and active refresh
    jobs with timing and status.
  - `health_check()` now warns when the worker pool is saturated or the job
    queue is backing up.
- **Self-healing.** On startup, the scheduler automatically cleans up orphaned
  jobs and reclaims leaked worker slots from previous crashes.

#### Statement-Level CDC Triggers

Change tracking triggers have been upgraded from row-level to statement-level,
reducing write-side overhead for bulk INSERT and UPDATE operations. This is
now the default for all new and existing stream tables. A benchmark harness is
included so you can measure the difference on your own hardware.

#### dbt Getting Started Example

New `examples/dbt_getting_started/` project with a complete, runnable dbt
example showing org-chart seed data, staging views, and three stream table
models. Includes an automated test script.

### Fixed

#### Refresh Lock Not Released After Errors

Fixed a bug where `refresh_stream_table()` could get permanently stuck after
a PostgreSQL error (e.g. running out of temp file space). The internal lock
was session-level and survived transaction rollback, causing all future
refreshes for that stream table to report "another refresh is already in
progress". Refresh locks are now transaction-level, so they are automatically
released when the transaction ends — whether it succeeds or fails.

#### dbt Integration Fixes

- Fixed query quoting in dbt macros that broke when queries contained single
  quotes.
- Fixed `schedule = none` in dbt being incorrectly mapped to SQL NULL.
- Fixed view inlining when the same view was referenced with different aliases.

### Changed

- Updated to PostgreSQL 18.3 across CI and test infrastructure.
- Dependency updates: `tokio` 1.49 → 1.50 and several GitHub Actions bumps.

---

## [0.3.0] — 2026-03-11

This is a correctness and hardening release. No new SQL functions, tables, or
views were added — all changes are in the compiled extension code.
`ALTER EXTENSION pg_trickle UPDATE` is safe and a no-op for schema objects.

### Fixed

#### Incremental Correctness Fixes

All 18 previously-disabled correctness tests have been re-enabled (0
remaining). The following query patterns now produce correct results during
incremental (non-full) refreshes:

- **HAVING clause threshold crossing.** Queries with `HAVING` filters (e.g.
  `HAVING SUM(amount) > 100`) now produce correct totals when groups cross
  the threshold. Previously, a group gaining enough rows to meet the condition
  would show only the newly added values instead of the correct total.

- **FULL OUTER JOIN.** Five bugs affecting incremental updates for
  `FULL OUTER JOIN` queries are fixed: mismatched row identifiers, incorrect
  handling of compound GROUP BY expressions like
  `COALESCE(left.col, right.col)`, and wrong NULL handling for SUM aggregates.

- **EXISTS with HAVING subqueries.** Queries using
  `WHERE EXISTS(... GROUP BY ... HAVING ...)` now work correctly — the inner
  GROUP BY and HAVING were previously being silently discarded.

- **Correlated scalar subqueries.** Correlated subqueries in SELECT like
  `(SELECT MAX(e.salary) FROM emp e WHERE e.dept_id = d.id)` are now
  automatically rewritten into LEFT JOINs so the incremental engine can
  handle them correctly.

#### Background Worker Detection on PostgreSQL 18

Fixed a bug where `health_check()` and the scheduler reported zero active
workers on PostgreSQL 18 due to a column name change in system views.

#### Scheduler Stability

Fixed a loop where the scheduler launcher could get stuck retrying failed
database probes indefinitely instead of backing off properly.

### Added

#### Security Tooling

Added static security analysis to the CI pipeline:

- **GitHub CodeQL** — automated security scanning across all Rust source files.
  First scan: zero findings.
- **`cargo deny`** — enforces a license allow-list and flags unmaintained or
  yanked dependencies.
- **Semgrep** — custom rules that flag potentially dangerous patterns such as
  dynamic SQL construction and privilege escalation. Advisory-only (does not
  block merges).
- **Unsafe block inventory** — CI tracks the count of unsafe code blocks per
  file and fails if any file exceeds its baseline, preventing unreviewed
  growth of low-level code.

## [0.2.3] — 2026-03-09

### Added

- **Unsafe function detection.** Queries using non-deterministic functions like
  `random()` or `clock_timestamp()` are now rejected when creating incremental
  stream tables, because they can't produce reliable results. Functions like
  `now()` that return the same value within a transaction are allowed with a
  warning.

- **Per-table change tracking mode.** You can now choose how each stream table
  tracks changes (`'auto'`, `'trigger'`, or `'wal'`) via the `cdc_mode`
  parameter on `create_stream_table()` and `alter_stream_table()`, instead of
  relying only on the global setting.

- **CDC status view.** New `pgtrickle.pgt_cdc_status` view shows the change
  tracking mode, replication slot, and transition status for every source
  table in one place.

- **Configurable WAL lag thresholds.** The warning and critical thresholds for
  replication slot lag are now configurable via
  `pg_trickle.slot_lag_warning_threshold_mb` (default 100 MB) and
  `pg_trickle.slot_lag_critical_threshold_mb` (default 1024 MB), instead of
  being hard-coded.

- **`pg_trickle_dump` backup tool.** New standalone CLI that exports all your
  stream table definitions as replayable SQL, ordered by dependency. Useful
  for backups before upgrades or migrations.

- **Upgrade path.** `ALTER EXTENSION pg_trickle UPDATE` picks up all new
  features from this release.

### Changed

- After a full refresh, WAL replication slots are now advanced to the current
  position, preventing unnecessary WAL accumulation and false lag alarms.
- Change buffers are now flushed after a full refresh, fixing a cycle where
  the scheduler would alternate endlessly between incremental and full
  refreshes on bulk-loaded tables.
- IMMEDIATE mode now correctly rejects explicit WAL CDC requests with a clear
  error, since real-time mode uses its own trigger mechanism.
- The `pg_trickle.user_triggers` setting is simplified to `auto` and `off`.
  The old `on` value still works as an alias for `auto`.
- CI pipelines are faster on PRs — only essential tests run; the full suite
  runs on merge and daily schedule.

---

## [0.2.2] — 2026-03-08

### Added

- **Change a stream table's query.** `alter_stream_table` now accepts a
  `query` parameter, so you can change what a stream table computes without
  dropping and recreating it. If the new query's columns are compatible, the
  underlying storage table is preserved — existing views, policies, and
  publications continue to work.

- **AUTO refresh mode (new default).** Stream tables now default to `AUTO`
  mode, which uses fast incremental updates when the query supports it and
  automatically falls back to a full recompute when it doesn't. You no longer
  need to think about whether your query is "incremental-compatible" — just
  create the stream table and it picks the best strategy.

- **Version mismatch warning.** The background scheduler now warns if the
  installed extension version doesn't match the compiled library, making it
  easier to spot a half-finished upgrade.

- **ORDER BY + LIMIT + OFFSET.** You can now page through top-N results, e.g.
  `ORDER BY revenue DESC LIMIT 10 OFFSET 20` to get the third page of
  top earners.

- **Real-time mode: recursive queries.** `WITH RECURSIVE` queries (e.g.
  org-chart hierarchies) now work in IMMEDIATE mode. A depth limit (default
  100) prevents infinite loops.

- **Real-time mode: top-N queries.** `ORDER BY ... LIMIT N` queries now work
  in IMMEDIATE mode — the top-N rows are recomputed on every data change.
  Maximum N is controlled by `pg_trickle.ivm_topk_max_limit` (default 1000).

- **Foreign table support.** Stream tables can now use foreign tables as
  sources. Changes are detected by comparing snapshots since foreign tables
  don't support triggers. Enable with `pg_trickle.foreign_table_polling = on`.

- **Documentation reorganization.** Configuration and SQL reference docs are
  reorganized around practical workflows. New sections cover DDL-during-refresh
  behavior, standby/replica limitations, and PgBouncer constraints.

### Changed

- Default refresh mode changed from `'DIFFERENTIAL'` to `'AUTO'`.
- Default schedule changed from `'1m'` to `'calculated'` (automatic).
- Default change tracking mode changed from `'trigger'` to `'auto'` — WAL-based
  tracking starts automatically when available, with trigger-based as fallback.

---

## [0.2.1] — 2026-03-05

### Added

- **Safe upgrades.** New upgrade infrastructure ensures that
  `ALTER EXTENSION pg_trickle UPDATE` works correctly. A CI check detects
  missing functions or views in upgrade scripts, and automated tests verify
  that stream tables survive version-to-version upgrades intact. See
  [docs/UPGRADING.md](docs/UPGRADING.md) for the upgrade guide.

- **ORDER BY + LIMIT + OFFSET.** You can now create stream tables over paged
  results, like "the second page of the top-100 products by revenue"
  (`ORDER BY revenue DESC LIMIT 100 OFFSET 100`).

- **`'calculated'` schedule.** Instead of passing SQL `NULL` to request
  automatic scheduling, you can now write `schedule => 'calculated'`. Passing
  `NULL` now gives a helpful error message.

- **Documentation expansion.** Six new pages in the online book covering dbt
  integration, contributing guidelines, security policy, release process, and
  research comparisons with other projects.

- **Better warnings and safety checks:**
  - Warning when a source table lacks a primary key (duplicate rows are
    handled safely but less efficiently).
  - Warning when using `SELECT *` (new columns added later can break
    incremental updates).
  - Alert when the refresh queue is falling behind (> 80% capacity).
  - Guard triggers prevent accidental direct writes to stream table storage.
  - Automatic fallback from WAL to trigger-based change tracking when the
    replication slot disappears.
  - Nested window functions and complex `WHERE` clauses with `EXISTS` are now
    handled automatically.

- **Change buffer partitioning.** For high-throughput tables, change buffers
  can now be partitioned so that processed data is dropped efficiently.

- **Column pruning.** The incremental engine now skips source columns not used
  in the query, reducing I/O for wide tables.

### Changed

- Default `schedule` changed from `'1m'` to `'calculated'` (automatic).
- Minimum schedule interval lowered from 60 s to 1 s.
- Cluster-wide diamond consistency settings removed; per-table settings remain
  and now default to `'atomic'` / `'fastest'`.

### Fixed

- The 0.1.3 → 0.2.0 upgrade script was accidentally a no-op, silently
  skipping 11 new functions. Fixed.
- Queries combining `WITH` (CTEs) and `UNION ALL` now parse correctly.

---

## [0.2.0] — 2026-03-04

### Added

- **Monitoring & health checks.** Six new functions for inspecting your stream
  tables at runtime (no superuser required):
  - `change_buffer_sizes()` — shows how much pending change data each stream
    table has queued up.
  - `list_sources(name)` — lists all base tables that feed a given stream
    table, with row counts and size estimates.
  - `dependency_tree()` — displays an ASCII tree of how your stream tables
    depend on each other.
  - `health_check()` — quick system triage that checks whether the scheduler
    is running, flags tables in error or stale, and warns about large change
    buffers or WAL lag.
  - `refresh_timeline()` — recent refresh history across all stream tables,
    showing timing, row counts, and any errors.
  - `trigger_inventory()` — verifies that all required change-tracking
    triggers are in place and enabled.

- **IMMEDIATE refresh mode (real-time updates).** New `'IMMEDIATE'` mode keeps
  stream tables updated within the same transaction as your data changes.
  There's no delay — the stream table reflects changes the instant they happen.
  Supports window functions, LATERAL joins, scalar subqueries, and aggregate
  queries. You can switch between IMMEDIATE and other modes at any time using
  `alter_stream_table`.

- **Top-N queries (ORDER BY + LIMIT).** Queries like
  `SELECT ... ORDER BY score DESC LIMIT 10` are now supported. The stream
  table stores only the top N rows and updates efficiently.

- **Diamond dependency consistency.** When multiple stream tables share common
  sources and feed into the same downstream table (a "diamond" pattern), they
  can now be refreshed as an atomic group — either all succeed or all roll
  back. This prevents inconsistent reads at convergence points. Controlled via
  the `diamond_consistency` parameter (default: `'atomic'`).

- **Multi-database auto-discovery.** The background scheduler now automatically
  finds and services all databases on the server where pg_trickle is installed.
  No manual `pg_trickle.database` configuration required — just install the
  extension and the scheduler discovers it.

### Fixed

- Fixed IMMEDIATE mode incorrectly trying to read from change buffer tables
  (which don't exist in that mode) for certain aggregate queries.
- Fixed type mismatches when join queries had unchanged source tables producing
  empty change sets.
- Fixed join condition column order being swapped when the right-side table was
  written first in the `ON` clause (e.g. `ON r.id = l.id`).
- Fixed dbt macros silently rolling back stream table creation because dbt
  wraps statements in a `ROLLBACK` by default.
- Fixed `LIMIT ALL` being incorrectly rejected as an unsupported LIMIT clause.
- Fixed false "query may produce incorrect incremental results" warnings on
  simple arithmetic like `depth + 1` or `path || name`.
- Fixed auto-created indexes using the wrong column name when the query had a
  column alias (e.g. `SELECT id AS department_id`).

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
  + 460 E2E across 34 test files (up from ~22). At release, 18 E2E tests were
  `#[ignore]`d pending DVM correctness fixes (reduced to 8 in later releases;
  see [Unreleased] Known Limitations).
- **1 new GUC variable** — `buffer_alert_threshold` added. Total: 16 GUCs.

### Known Limitations

> **Note:** Five HAVING tests (listed below) were subsequently fixed in the
> next release. See [Unreleased] Known Limitations for the current state.

18 E2E tests were marked `#[ignore]` at v0.2.3 release due to pre-existing DVM
differential logic bugs:

| Suite | Ignored | Status |
|---|---|---|
| `e2e_full_join_tests` | 5/5 | Still open — FULL OUTER JOIN differential |
| `e2e_having_transition_tests` | 5/7 | **Fixed** — see [Unreleased] Fixed section |
| `e2e_keyless_duplicate_tests` | 5/7 | **Fixed** — un-ignored as part of F48 |
| `e2e_scalar_subquery_tests` | 2/4 | Still open — correlated subquery diff |
| `e2e_sublink_or_tests` | 1/4 | Still open — correlated EXISTS with HAVING |

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
