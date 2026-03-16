# Changelog

All notable changes to pg_trickle are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
For future plans and release milestones, see [ROADMAP.md](ROADMAP.md).

## Table of Contents

<!-- TOC start -->
- [Unreleased](#unreleased)
- [0.7.0 — 2026-03-16](#070--2026-03-16)
- [0.6.0 — 2026-03-14](#060--2026-03-14)
- [0.5.0 — 2026-03-13](#050--2026-03-13)
- [0.4.0 — 2026-03-12](#040--2026-03-12)
- [0.3.0 — 2026-03-11](#030--2026-03-11)
- [0.2.3 — 2026-03-09](#023--2026-03-09)
- [0.2.2 — 2026-03-08](#022--2026-03-08)
- [0.2.1 — 2026-03-05](#021--2026-03-05)
- [0.2.0 — 2026-03-04](#020--2026-03-04)
- [0.1.3 — 2026-03-02](#013--2026-03-02)
- [0.1.2 — 2026-02-28](#012--2026-02-28)
- [0.1.1 — 2026-02-26](#011--2026-02-26)
- [0.1.0 — 2026-02-26](#010--2026-02-26)
<!-- TOC end -->

---

## [Unreleased]

No unreleased changes yet.

---

## [0.7.0] — 2026-03-16

0.7.0 makes pg_trickle easier to trust in real-world data pipelines. The big
theme of this release is fewer surprises: the scheduler can now wait for late
arriving source data, some circular pipelines can run safely instead of being
blocked, more queries stay on incremental refresh, and the system does a better
job of deciding when incremental work is no longer worth it.

### Added

#### Multi-source data can wait until it is actually ready

pg_trickle can now delay a refresh until related source tables have all caught
up to roughly the same point in time. This is useful for ETL jobs where, for
example, `orders` arrives before `order_lines` and refreshing too early would
produce a half-finished report.

- New watermark APIs: `advance_watermark(source, watermark)`,
  `create_watermark_group(name, sources[], tolerance_secs)`, and
  `drop_watermark_group(name)`.
- New status helpers: `watermarks()`, `watermark_groups()`, and
  `watermark_status()`.
- The scheduler now skips gated refreshes when grouped sources are too far
  apart and records the reason in refresh history.
- New catalog tables store per-source watermarks and watermark group
  definitions.
- 28 end-to-end tests cover normal operation, bad input, tolerance windows,
  and scheduler behavior.

#### Some circular pipelines can now run safely

Stream tables that depend on each other in a loop are no longer always blocked.
If the cycle is monotone and uses DIFFERENTIAL mode, pg_trickle can now keep
refreshing the group until it stops changing.

- Circular refreshes run to a fixed point, with `pg_trickle.max_fixpoint_iterations`
  as a safety limit.
- Cycle creation and ALTER validation now check that every member is safe for
  convergence before allowing the loop.
- `pgtrickle.pgt_status()` now reports `scc_id`, and
  `pgtrickle.pgt_scc_status()` shows per-cycle-group status.
- `pgtrickle.pgt_stream_tables` now tracks `last_fixpoint_iterations` so it is
  easier to spot slow or unstable cycles.
- 6 end-to-end tests cover convergence, rejection of unsafe cycles,
  non-convergence handling, and cleanup.

#### More queries stay on incremental refresh

Several query shapes that used to fall back to FULL refresh, or fail outright,
now keep working in DIFFERENTIAL and AUTO mode.

- User-defined aggregates created with `CREATE AGGREGATE` now work through the
  existing group-rescan strategy, including common extension-provided
  aggregates.
- More complex `OR` plus subquery patterns are now rewritten correctly,
  including cases that need De Morgan normalization and multiple rewrite passes.
- The rewrite pipeline has a guardrail to stop runaway branch explosion.
- A dedicated 14-test end-to-end suite covers these previously missing cases.

#### Easier packaging ahead of 1.0

The release also adds infrastructure that makes evaluation and future
distribution simpler.

- `Dockerfile.hub` and a dedicated CI workflow can build and smoke-test a
  ready-to-run PostgreSQL 18 image with pg_trickle preinstalled.
- `META.json` adds PGXN package metadata with `release_status: "testing"`.
- CNPG smoke testing is now part of the documented pre-1.0 packaging story.

### Improved

#### Refresh strategy and performance decisions are smarter

The scheduler and refresh engine now make better choices when incremental work
is likely to help and back off sooner when it is not.

- Wide tables now use xxh64-based change detection instead of slower MD5-based
  comparisons.
- Aggregate stream tables can skip expensive incremental work and jump straight
  to FULL refresh when the pending change set is obviously too large.
- Strategy selection now combines a change-ratio signal with recent refresh
  history, which helps on workloads with uneven batch sizes.
- DAG levels are extracted explicitly, enabling level-parallel refresh
  scheduling.
- Small internal hot paths such as column-list building and LSN comparison were
  tightened to remove avoidable allocations.

#### Benchmarking is much easier to use and compare

The performance toolchain was expanded so regressions are easier to spot and
large-scale behavior is easier to study.

- Benchmarks now support per-cycle output, optional `EXPLAIN ANALYZE` capture,
  larger 1M-row runs, and more stable Criterion settings.
- New tooling covers cross-run comparison, concurrent writers, and extra query
  shapes such as window, lateral, CTE, and `UNION ALL` workloads.
- `just bench-docker` makes it easier to run Criterion inside the builder image
  when local linking is awkward.

### Changed

#### Internal low-level code is much safer to audit

This release cuts the amount of low-level `unsafe` Rust in half without
changing behavior.

- Unsafe blocks were reduced by 51%, from 1,309 to 641.
- Repeated patterns were consolidated into a small set of documented helper
  functions.
- 37 internal functions no longer need to be marked `unsafe`.
- Existing unit tests continued to pass unchanged after the refactor.

---

## [0.6.0] — 2026-03-14

### Added

#### Idempotent DDL (`create_or_replace`)

New one-call function for deploying stream tables without worrying about
whether they already exist. Replaces the old "check if it exists, then drop
and recreate" pattern.

- **`create_or_replace_stream_table()`** — a single function that does the
  right thing automatically:
  - **Creates** the stream table if it doesn't exist yet.
  - **Does nothing** if the stream table already exists with the same query
    and settings (logs an INFO so you know it was a no-op).
  - **Updates settings** (schedule, refresh mode, etc.) if only config changed.
  - **Replaces the query** if the defining query changed — including
    automatic schema migration and a full refresh.
- **dbt uses it automatically.** The `stream_table` materialization now calls
  `create_or_replace_stream_table()` when running against pg_trickle 0.6.0+,
  with automatic fallback for older versions.
- **Whitespace-insensitive.** Cosmetic SQL differences (extra spaces, tabs,
  newlines) are correctly treated as no-ops — won't trigger unnecessary
  rebuilds.

#### dbt Integration Enhancements

- **Check stream table health from dbt.** New `pgtrickle_stream_table_status()`
  macro returns whether a stream table is healthy, stale, erroring, or paused.
  Pair it with the new built-in `stream_table_healthy` test in your
  `schema.yml` to fail CI when a stream table is behind or broken.
- **Refresh everything in the right order.** New `refresh_all_stream_tables`
  run-operation refreshes all dbt-managed stream tables in dependency order.
  Run it after `dbt run` and before `dbt test` in your CI pipeline.

#### Partitioned Source Tables

Stream tables now work with PostgreSQL's declarative table partitioning —
RANGE, LIST, and HASH partitioned tables all work as sources out of the box.

- **Changes in any partition are captured automatically.** CDC triggers fire
  on the parent table so inserts, updates, and deletes in any child partition
  are picked up.
- **ATTACH PARTITION triggers automatic rebuild.** When you attach a new
  partition, pg_trickle detects the structural change and rebuilds affected
  stream tables to include the new partition's pre-existing data.
- **WAL mode works with partitions.** Publications are configured with
  `publish_via_partition_root = true`, so all partitions report changes under
  the parent table's identity.
- **New tutorial** covering partitioned source tables, ATTACH/DETACH behavior,
  and known caveats (`docs/tutorials/PARTITIONED_TABLES.md`).

#### Circular Dependency Foundation

Lays the groundwork for stream tables that reference each other in a cycle
(A → B → A). The actual cyclic refresh execution is planned for v0.7.0 —
this release adds the detection, validation, and safety infrastructure.

- **Cycle detection.** pg_trickle can now identify groups of stream tables
  that form circular dependencies.
- **Safety checks at creation time.** Queries that can't safely participate
  in a cycle (those using aggregates, EXCEPT, window functions, or NOT EXISTS)
  are rejected with a clear error explaining why.
- **New settings:**
  - `pg_trickle.allow_circular` (default: off) — master switch for circular
    dependencies.
  - `pg_trickle.max_fixpoint_iterations` (default: 100) — prevents runaway
    loops.

#### Source Gating Improvements

- **`bootstrap_gate_status()` function.** Shows which sources are currently
  gated, when they were gated, how long the gate has been active, and which
  stream tables are waiting. Useful for debugging "why isn't my stream table
  refreshing?"
- **ETL coordination cookbook.** SQL Reference now includes five step-by-step
  recipes for common bulk-load patterns.

#### More SQL Patterns Supported

Two query patterns that previously required workarounds now just work:

- **Window functions inside expressions.** Queries like
  `CASE WHEN ROW_NUMBER() OVER (...) = 1 THEN 'top' ELSE 'other' END` or
  `COALESCE(SUM() OVER (...), 0)` are now accepted and produce correct
  results. Use **FULL** refresh mode for these queries — incremental
  (DIFFERENTIAL) refresh of window-in-expression patterns is not yet
  supported. Previously, the query was rejected entirely at creation time.

- **`ALL (subquery)` comparisons.** Queries like
  `WHERE price < ALL (SELECT price FROM competitors)` are now accepted in
  both FULL and DIFFERENTIAL modes. Supports all comparison operators
  (`>`, `>=`, `<`, `<=`, `=`, `<>`) and correctly handles NULL values per
  the SQL standard.

#### Operational Safety Improvements

- **Function changes detected automatically.** If a stream table's query
  calls a user-defined function and you update that function with
  `CREATE OR REPLACE FUNCTION`, pg_trickle detects the change and
  automatically rebuilds the stream table on the next cycle. No manual
  intervention needed.

- **WAL mode explains why it isn't activating.** When `cdc_mode = 'auto'`
  and the system stays on trigger-based tracking, the scheduler now
  periodically logs the exact reason (e.g., "`wal_level` is not `logical`")
  and `check_cdc_health()` reports the current mode so you can diagnose the
  issue.

- **WAL + keyless tables rejected early.** Creating a stream table with
  `cdc_mode = 'wal'` on a table that has no primary key and no
  `REPLICA IDENTITY FULL` is now rejected at creation time with a clear
  error — instead of silently producing incomplete results later.

- **Automatic recovery after backup/restore.** When a PostgreSQL server is
  restored from `pg_basebackup`, WAL replication slots are lost. pg_trickle
  now detects the missing slot, automatically falls back to trigger-based
  tracking, and logs a WARNING so you know what happened.

#### Documentation

- **ALL (subquery) worked example** in the SQL Reference with sample data
  and expected results.
- **Window-in-expression documentation** showing before/after examples of
  the automatic rewrite.
- **Foreign table sources tutorial** — step-by-step guide for using
  `postgres_fdw` foreign tables as stream table sources.

### Fixed

- **`create_or_replace` whitespace handling.** Extra spaces, tabs, and
  newlines in queries no longer trigger unnecessary rebuilds.
- **`create_or_replace` schema incompatibility detection.** Incompatible
  column type changes (e.g., text → integer) are now properly detected
  and handled.

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

### Breaking Changes

These behavioural changes shipped in v0.4.0. They improve usability but may
require action from users upgrading from v0.3.0.

- **Schedule default changed from `'1m'` to `'calculated'`.**
  `create_stream_table` now defaults to `schedule => 'calculated'`, which
  auto-computes the refresh interval from downstream dependents instead of
  refreshing every 1 minute. If you relied on the implicit 1-minute default,
  explicitly pass `schedule => '1m'` to preserve the old behaviour.

- **`NULL` schedule input rejected.** Passing `schedule => NULL` to
  `create_stream_table` now returns an error. Use `schedule => 'calculated'`
  instead — it's explicit and self-documenting.

- **Diamond GUCs removed.** The cluster-wide GUCs
  `pg_trickle.diamond_consistency` and `pg_trickle.diamond_schedule_policy`
  have been removed. Diamond behaviour is now controlled per-table via
  parameters on `create_stream_table()` / `alter_stream_table()`:
  `diamond_consistency => 'atomic'`, `diamond_schedule_policy => 'slowest'`.

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

Major hardening release with 50 improvements across correctness, robustness,
operational safety, and test coverage.

### Added

- **DDL change tracking expanded.** `ALTER TYPE`, `ALTER POLICY`, and
  `ALTER DOMAIN` on source tables are now detected and trigger a rebuild of
  affected stream tables. Previously only column changes were tracked.
- **Recursive query safety guard.** Recursive CTEs (`WITH RECURSIVE`) are now
  checked for non-monotonic terms that could produce incorrect incremental
  results.
- **Read replica awareness.** The background scheduler detects when it's
  running on a read replica and skips refresh work, preventing errors.
- **Range aggregates rejected.** `RANGE_AGG` and `RANGE_INTERSECT_AGG` are
  now properly rejected in incremental mode with a clear error.
- **Refresh history: row counts.** Refresh history now records how many rows
  were inserted, updated, and deleted in each refresh cycle.
- **Change buffer alerts.** New `pg_trickle.buffer_alert_threshold` setting
  lets you configure when to be warned about growing change buffers.
- **`st_auto_threshold()` function.** Shows the current adaptive threshold
  that decides when to switch between incremental and full refresh.
- **Wide table optimization.** Tables with more than 50 columns use a hash
  shortcut during refresh merges, improving performance.
- **Change buffer security.** Internal change buffer tables are no longer
  accessible to `PUBLIC`.
- **Documentation.** PgBouncer compatibility, keyless table limitations, delta
  memory bounds, sequential processing rationale, and connection overhead are
  all now documented in the FAQ.

#### TPC-H Correctness Suite: 22/22 Queries Passing

The TPC-H-derived correctness test suite (22 industry-standard analytical
queries) now passes completely across multiple rounds of data changes. This
validates that incremental refreshes produce identical results to full
recomputation for complex real-world query patterns.

### Fixed

#### Window Function Correctness

Fixed incremental maintenance of window functions (ROW_NUMBER, RANK,
DENSE_RANK, NTILE, LAG/LEAD, SUM OVER, etc.) to correctly handle:
- Non-RANGE frame types
- Ranking functions over tied values
- Window functions wrapping aggregates (e.g. `RANK() OVER (ORDER BY SUM(x))`)
- Multiple window functions with different PARTITION BY clauses

#### INTERSECT / EXCEPT Correctness

Fixed incremental maintenance of `INTERSECT` and `EXCEPT` queries that
produced wrong results due to invalid SQL generation.

#### EXISTS / IN with OR Correctness

Fixed `EXISTS` and `IN` subqueries combined with `OR` in WHERE clauses that
produced wrong results.

#### Aggregate Correctness

- `MIN` / `MAX` now correctly rescan the source table when the current
  minimum or maximum value is deleted.
- `STRING_AGG(... ORDER BY ...)` and `ARRAY_AGG(... ORDER BY ...)` no longer
  silently drop the ORDER BY clause.

---

## [0.1.2] — 2026-02-28

### Changed

#### Project Renamed from pg_stream to pg_trickle

Renamed the entire project from **pg_stream** to **pg_trickle** to avoid a
naming collision with an unrelated project. If you were using the old name,
all configuration prefixes changed from `pg_stream.*` to `pg_trickle.*`, and
the SQL schemas changed from `pgstream` to `pgtrickle`. The "stream tables"
terminology is unchanged.

### Fixed

Fixed numerous incremental computation bugs discovered while building a
comprehensive correctness test suite based on all 22 TPC-H analytical queries:

- **Inner join double-counting.** When both sides of a join had changes in
  the same refresh cycle, some rows were counted twice.
- **Shared source cleanup.** Cleaning up processed changes for one stream
  table could accidentally delete entries still needed by another stream
  table sharing the same source.
- **Scalar aggregate identity mismatch.** Queries like `SELECT SUM(amount)
  FROM orders` could produce mismatched row identifiers between the
  incremental and merge phases. AVG also failed to recompute correctly
  after partial group changes.
- **EXISTS / NOT EXISTS snapshots.** Incremental maintenance of `EXISTS` and
  `NOT EXISTS` subqueries missed pre-change state, producing wrong results.
- **Column resolution in complex joins.** Several fixes for column name
  resolution in multi-table joins and nested subqueries.
- **COUNT(*) rendering.** `COUNT(*)` was sometimes rendered as `COUNT()`
  (missing the star), causing SQL errors.
- **Subquery rewriting.** Several subquery patterns (correlated vs
  non-correlated scalar subqueries, derived tables in FROM) were incorrectly
  rewritten, blocking certain queries from being created.
- **Cleanup worker crash.** The background cleanup worker no longer crashes
  when it encounters entries for stream tables that were dropped mid-cycle.

### Added

#### TPC-H Correctness Test Suite

Added a comprehensive correctness test suite based on all 22 TPC-H analytical
queries. These tests verify that incremental refreshes produce identical
results to a full recompute after INSERT, DELETE, and UPDATE mutations.
20 of 22 queries can be created as stream tables; 15 pass full correctness
checks at this point (improved to 22/22 in v0.1.3).

---

## [0.1.1] — 2026-02-26

### Changed

#### CloudNativePG Extension Image

Replaced the full PostgreSQL Docker image (~400 MB) with a minimal
extension-only image (< 10 MB) following the CloudNativePG Image Volume
Extensions specification. This means faster pulls and less disk usage in
Kubernetes deployments. The image contains just the extension files —
no full PostgreSQL server.

---

## [0.1.0] — 2026-02-26

Initial release of pg_trickle — a PostgreSQL extension that keeps query results
automatically up to date as your data changes.

### Core Concept

Define a SQL query and a schedule. pg_trickle creates a **stream table** that
stores the query's results and keeps them fresh — either on a schedule
(every N seconds) or in real time. When data in your source tables changes,
only the affected rows are recomputed instead of re-running the entire query.

### What You Can Do

- **Create stream tables** from `SELECT` queries — joins, aggregates,
  subqueries, CTEs, window functions, set operations, and more.
- **Automatic refresh** — a background scheduler refreshes stream tables in
  dependency order. You can also trigger refreshes manually.
- **Incremental updates** — the engine automatically figures out how to update
  only the rows that changed, instead of recomputing everything. This works
  for most query patterns including multi-table joins and aggregates.
- **Views as sources** — views referenced in your query are automatically
  expanded so change tracking works on the underlying tables.
- **Tables without primary keys** — supported via content hashing. Tables with
  primary keys get better performance.
- **Hybrid change tracking** — starts with lightweight triggers (no special
  PostgreSQL configuration needed). Can automatically switch to WAL-based
  tracking for lower overhead when `wal_level = logical` is available.
- **Multi-database support** — the scheduler automatically discovers all
  databases on the server where the extension is installed.
- **User triggers on stream tables** — your own `AFTER` triggers on stream
  tables fire correctly during incremental refreshes.
- **DDL awareness** — `ALTER TABLE`, `DROP TABLE`, `CREATE OR REPLACE
  FUNCTION`, and other DDL on source tables or functions used in your query
  are detected and handled automatically.

### SQL Support

Broad coverage of SQL features:

- **Joins:** INNER, LEFT, RIGHT, FULL OUTER, NATURAL, LATERAL subqueries,
  LATERAL set-returning functions (`unnest`, `jsonb_array_elements`, etc.)
- **Aggregates:** 39 functions including COUNT, SUM, AVG, MIN, MAX,
  STRING_AGG, ARRAY_AGG, JSON_ARRAYAGG, JSON_OBJECTAGG, statistical
  regression functions (CORR, COVAR_*, REGR_*), and ordered-set aggregates
  (MODE, PERCENTILE_CONT, PERCENTILE_DISC)
- **Window functions:** ROW_NUMBER, RANK, DENSE_RANK, NTILE, LAG, LEAD,
  SUM OVER, etc. with full frame clause support
- **Set operations:** UNION, UNION ALL, INTERSECT, EXCEPT
- **Subqueries:** in FROM, EXISTS/NOT EXISTS, IN/NOT IN, scalar subqueries
- **CTEs:** `WITH` and `WITH RECURSIVE`
- **Special syntax:** DISTINCT, DISTINCT ON, GROUPING SETS / CUBE / ROLLUP,
  CASE WHEN, COALESCE, JSON_TABLE (PostgreSQL 17+)
- **Unsafe function detection:** queries using non-deterministic functions
  like `random()` are rejected with a clear error

### Monitoring

- `explain_st()` — shows the incremental computation plan
- `st_refresh_stats()`, `get_refresh_history()`, `get_staleness()` — refresh
  performance and status
- `slot_health()` — WAL replication slot health
- `check_cdc_health()` — change tracking health per source table
- `stream_tables_info` and `pg_stat_stream_tables` views
- NOTIFY alerts for stale data, errors, and refresh events

### Documentation

- Architecture guide, SQL reference, configuration reference, FAQ,
  getting-started tutorial, and deep-dive tutorials.

### Known Limitations

- `TABLESAMPLE`, `LIMIT` / `OFFSET`, `FOR UPDATE` / `FOR SHARE` — not yet
  supported (clear error messages).
- Window functions inside expressions (e.g. `CASE WHEN ROW_NUMBER() ...`) —
  not yet supported.
- Circular stream table dependencies — not yet supported.
