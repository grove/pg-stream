# Changelog

All notable changes to pg_stream are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

### Added

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
  strategy. 37 aggregate function variants total (up from 25).
- **Mixed `UNION` / `UNION ALL` (S6)** — nested set operations with different
  `ALL` flags are now parsed correctly.
- **Column snapshot + schema fingerprint (S7)** — `pgs_dependencies` stores a
  JSONB column snapshot and SHA-256 fingerprint for each source table. DDL
  change detection uses a 3-tier fast path: fingerprint → snapshot → legacy
  `columns_used` fallback.
- **`pg_stream.block_source_ddl` GUC (S8)** — when `true`, column-affecting
  DDL on tracked source tables is blocked with an ERROR instead of marking
  stream tables for reinit.
- **`NATURAL JOIN` support (S9)** — common columns are resolved at parse time
  and an explicit equi-join condition is synthesized. Supports INNER, LEFT,
  RIGHT, and FULL NATURAL JOIN variants. Previously rejected.
- **Keyless table support (S10)** — source tables without a primary key now
  work correctly. CDC triggers compute an all-column content hash for row
  identity. Consistent `__pgs_row_id` between full and delta refreshes.
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
  See `docs/research/CUSTOM_SQL_SYNTAX.md`.
- **Native syntax plan** — tiered strategy: Tier 1 (function API, existing),
  Tier 1.5 (`CALL` procedure wrappers), Tier 2 (`CREATE MATERIALIZED VIEW ...
  WITH (pgstream.stream = true)` via `ProcessUtility_hook`). See
  `plans/sql/PLAN_NATIVE_SYNTAX.md`.

#### Hybrid CDC — Automatic Trigger → WAL Transition
- **Hybrid CDC architecture** — stream tables now start with lightweight
  row-level triggers for zero-config setup and can automatically transition to
  WAL-based (logical replication) capture for lower write-side overhead. The
  transition is controlled by the `pg_stream.cdc_mode` GUC (`trigger` / `auto`
  / `wal`).
- **WAL decoder background worker** — dedicated worker that polls logical
  replication slots and writes decoded changes into the same change buffer
  tables used by triggers, ensuring a uniform format for the DVM engine.
- **Transition orchestration** — transparent three-step process: create
  replication slot, wait for decoder catch-up, drop trigger. Falls back to
  triggers automatically if the decoder does not catch up within the timeout.
- **CDC health monitoring** — new `pgstream.check_cdc_health()` function
  returns per-source CDC mode, slot lag, confirmed LSN, and alerts.
- **CDC transition notifications** — `NOTIFY pg_stream_cdc_transition` emits
  JSON payloads when sources transition between CDC modes.
- **New GUCs** — `pg_stream.cdc_mode` and `pg_stream.wal_transition_timeout`.
- **Catalog extension** — `pgs_dependencies` table gains `cdc_mode`,
  `slot_name`, `decoder_confirmed_lsn`, and `transition_started_at` columns.

#### User-Defined Triggers on Stream Tables
- **User trigger support in DIFFERENTIAL mode** — user-created `AFTER` triggers
  on stream tables now fire correctly during differential refresh via explicit
  per-row DML (INSERT/UPDATE/DELETE) instead of bulk MERGE.
- **FULL refresh trigger handling** — user triggers are suppressed during FULL
  refresh with `DISABLE TRIGGER USER` and a `NOTIFY pgstream_refresh` is
  emitted so listeners know when to re-query.
- **Trigger detection** — `has_user_triggers()` automatically detects
  user-defined triggers on storage tables at refresh time.
- **DDL warning** — `CREATE TRIGGER` on a stream table emits a notice explaining
  the trigger semantics and the `pg_stream.user_triggers` GUC.
- **New GUC** — `pg_stream.user_triggers` (`auto` / `on` / `off`) controls
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
- **Keyless table support** — tables without primary keys use content hashing.
- **Volatile function detection** — rejected in DIFFERENTIAL, warned for stable.
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
- **Monitoring views** — `pgstream.stream_tables_info` and
  `pgstream.pg_stat_stream_tables`.
- **NOTIFY alerting** — `pg_stream_alert` channel broadcasts stale, suspended,
  reinitialize, slot lag, refresh completed/failed events.

#### Infrastructure
- **Row ID hashing** — `pg_stream_hash()` and `pg_stream_hash_multi()` using
  xxHash (xxh64) for deterministic row identity.
- **DDL event tracking** — `ALTER TABLE` and `DROP TABLE` on source tables
  automatically set `needs_reinit` on affected stream tables.
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
- 896 unit tests, 22 E2E test suites (Testcontainers + custom Docker image).
- Property-based tests, integration tests, resilience tests.
- Column snapshot and schema fingerprint-based DDL change detection.

### Known Limitations

- `TABLESAMPLE`, `LIMIT` / `OFFSET`, `FOR UPDATE` / `FOR SHARE` — rejected
  with clear error messages.
- Window functions inside expressions (CASE, COALESCE, arithmetic) — rejected.
- Circular stream table dependencies (cycles) — not yet supported.
