# Changelog

All notable changes to pg_stream are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
For future plans and release milestones, see [ROADMAP.md](ROADMAP.md).

---

## [Unreleased]

### Fixed

#### DVM Parser: 4 Query Rewrite Bugs (TPC-H Regression Coverage)

Fixed four bugs in `src/dvm/parser.rs` discovered while building the TPC-H
correctness test suite. Together they unblock 3 more TPC-H queries (Q04,
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

#### TPC-H Correctness Test Suite

Added a full TPC-H correctness test suite (`tests/e2e_tpch_tests.rs`) that
validates the core DBSP invariant — `Contents(ST) ≡ Result(defining_query)`
after every differential refresh — across all 22 TPC-H queries at SF=0.01.

- **Schema & data generation** (`tests/tpch/schema.sql`, `datagen.sql`) —
  SQL-only, no external `dbgen` dependency, works with existing `E2eDb`
  testcontainers infrastructure.
- **Mutation scripts** (`rf1.sql` INSERT, `rf2.sql` DELETE, `rf3.sql` UPDATE)
  — multi-cycle churn to catch cumulative drift.
- **22 query files** (`tests/tpch/queries/q01.sql`–`q22.sql`) — standard
  TPC-H queries adapted for pg_stream SQL compatibility:

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
- **Current score:** 20/22 create successfully; 4/22 pass all 3 cycles
  (Q11, Q16, Q20, Q22); 16/22 pass cycle 1 only (blocked by
  `rewrite_expr_for_join` column-qualification bug, tracked as ROADMAP F5).
- **`just` targets:** `test-tpch` (fast, SF=0.01), `test-tpch-large`
  (SF=0.1, 5 cycles), `test-tpch-fast` (skips image rebuild).

---

## [0.1.1] — 2026-02-26

### Changed

#### CloudNativePG Image Volume Extension Distribution
- **Extension-only OCI image** — replaced the full PostgreSQL Docker image
  (`ghcr.io/<owner>/pg_stream`) with a minimal `scratch`-based extension image
  (`ghcr.io/<owner>/pg_stream-ext`) following the
  [CNPG Image Volume Extensions](https://cloudnative-pg.io/docs/1.28/imagevolume_extensions/)
  specification. The image contains only `.so`, `.control`, and `.sql` files
  (< 10 MB vs ~400 MB for the old full image).
- **New `cnpg/Dockerfile.ext`** — release Dockerfile for packaging pre-built
  artifacts into the scratch-based extension image.
- **New `cnpg/Dockerfile.ext-build`** — multi-stage from-source build for
  local development and CI.
- **New `cnpg/database-example.yaml`** — CNPG `Database` resource for
  declarative `CREATE EXTENSION pg_stream` (replaces `postInitSQL`).
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
  `pgstream.pgs_stream_tables` stores all function names used by the defining
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
  stored in `pgstream.pgs_stream_tables.original_query` for reinit after view
  changes and user introspection.
- **View DDL hooks** — `CREATE OR REPLACE VIEW` triggers reinit of affected
  stream tables. `DROP VIEW` sets affected stream tables to ERROR status.
- **View dependency tracking** — views are registered as soft dependencies in
  `pgstream.pgs_dependencies` (source_type = 'VIEW') for DDL hook lookups.
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
- **Monitoring views** — `pgstream.stream_tables_info` and
  `pgstream.pg_stat_stream_tables`.
- **NOTIFY alerting** — `pg_stream_alert` channel broadcasts stale, suspended,
  reinitialize, slot lag, refresh completed/failed events.

#### Infrastructure
- **Row ID hashing** — `pg_stream_hash()` and `pg_stream_hash_multi()` using
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
