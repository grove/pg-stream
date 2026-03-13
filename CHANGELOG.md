# Changelog

All notable changes to pg_trickle are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
For future plans and release milestones, see [ROADMAP.md](ROADMAP.md).

---

## [Unreleased]

### Added

#### Row-Level Security (RLS) Hardening — Phase 1 (v0.5.0)

Security context hardening for stream tables. Stream tables now follow the
same RLS model as PostgreSQL's `MATERIALIZED VIEW`: the refresh always
materializes the full, unfiltered result set. Access control is applied at
read time via RLS policies on the stream table itself.

- **R2: Change buffer RLS disabled.** Change buffer tables
  (`pgtrickle_changes.changes_*`) now explicitly `DISABLE ROW LEVEL SECURITY`
  after creation, preventing CDC trigger insert failures if RLS is enabled
  at the schema level.
- **R3: Manual refresh bypasses RLS.** `refresh_stream_table()` now executes
  `SET LOCAL row_security = off` to ensure the defining query always produces
  the full result set regardless of the calling user's RLS policies. The
  scheduler (already superuser) also sets this as defence-in-depth.
- **R4: IVM triggers are SECURITY DEFINER.** All IMMEDIATE-mode IVM trigger
  functions are now created with `SECURITY DEFINER` and a locked
  `SET search_path = pg_catalog, pgtrickle, pgtrickle_changes` to prevent
  partial-visibility delta corruption and search_path hijacking.
- **R1: RLS semantics documented.** New "Row-Level Security" sections in
  `SQL_REFERENCE.md` and `FAQ.md` covering source-table RLS bypass, stream-table
  RLS patterns, SECURITY DEFINER rationale, and a per-tenant policy example.
- **R5/R7/R8: E2E tests.** New `e2e_rls_tests.rs` with 6 tests covering:
  RLS on source table (FULL + DIFFERENTIAL), RLS on stream table, IMMEDIATE
  mode + RLS, change buffer RLS verification, and SECURITY DEFINER verification.

#### Row-Level Security (RLS) DDL Tracking — Phase 2 (v0.5.0)

- **R9: ENABLE/DISABLE RLS detection.** The DDL event trigger hook now detects
  `ALTER TABLE ... ENABLE ROW LEVEL SECURITY`, `DISABLE ROW LEVEL SECURITY`,
  `FORCE ROW LEVEL SECURITY`, and `NO FORCE ROW LEVEL SECURITY` on source
  tables. Affected stream tables are marked for reinit. The column snapshot
  now includes RLS state (`rls_enabled`, `rls_forced`) so the schema
  fingerprint changes when RLS is toggled.
- **R10: E2E tests for RLS DDL tracking.** Three new tests in
  `e2e_rls_tests.rs` covering ENABLE RLS, DISABLE RLS, and FORCE RLS
  triggering reinit on downstream stream tables.
- **R6: RLS tutorial.** New `docs/tutorials/ROW_LEVEL_SECURITY.md` showing
  per-tenant RLS policies on stream tables with step-by-step examples.

#### Bootstrap Source Gating — Phase 3 (v0.5.0)

New operational control for bulk-load workflows. When loading large amounts
of historical data into a source table, gate it first so the scheduler does
not fire mid-load, then ungate it to pick up all the data in a single refresh.

- **BOOT-1: `pgtrickle.pgt_source_gates` catalog table.** Tracks which source
  tables are currently gated (`source_relid`, `gated`, `gated_at`, `ungated_at`,
  `gated_by`).
- **BOOT-2: `pgtrickle.gate_source(source TEXT)`.** Marks a source table as
  gated (UPSERT into `pgt_source_gates`) and emits a `pg_notify` on
  `pgtrickle_source_gate` so the scheduler can react immediately.
- **BOOT-3: `pgtrickle.ungate_source(source TEXT)`.** Clears the gate
  (`gated = false`, records `ungated_at`) and emits the same notify. The
  `pgtrickle.source_gates()` table function returns the current gate status
  for all registered sources, with schema name, gated flag, timestamps, and
  the gating actor.
- **BOOT-4: Scheduler skip integration.** At the start of every
  refresh-worker invocation the scheduler loads all currently gated source
  OIDs. If any direct source of a stream table is gated the refresh is
  skipped and a `SKIP` / `SKIPPED` record is written to
  `pgtrickle.pgt_refresh_history` with `initiated_by = 'SCHEDULER'`.
  Manual `refresh_stream_table()` calls are not affected — operators can always
  force a refresh out-of-band.
- **BOOT-5: E2E tests.** New `e2e_bootstrap_gating_tests.rs` with 9 tests
  covering: catalog insert/read, ungate, idempotency, re-gate, non-existent
  table error, multiple simultaneous gates, manual refresh not blocked by
  gate, and scheduler SKIP + resume flow.
- **Upgrade script.** `sql/pg_trickle--0.4.0--0.5.0.sql` adds the
  `pgt_source_gates` table for incremental upgrades.

#### Append-Only INSERT Fast Path — Phase 5 (v0.5.0)

Performance optimization for event-sourced / append-only workloads. Stream
tables marked as append-only skip the full MERGE pipeline (DELETE, UPDATE,
IS DISTINCT FROM checks) and use a simple `INSERT … SELECT` from the delta,
reducing per-refresh latency significantly.

- **A-3a: `append_only` parameter.** `create_stream_table()` and
  `alter_stream_table()` accept a new `append_only` boolean (default `false`).
  When `true`, differential refreshes use a direct INSERT instead of MERGE.
- **Catalog: `is_append_only` column.** New `BOOLEAN NOT NULL DEFAULT FALSE`
  column on `pgtrickle.pgt_stream_tables`.
- **CDC heuristic fallback.** At the start of each differential refresh, the
  engine checks the change buffers for DELETE or UPDATE actions. If any are
  found, `is_append_only` is reverted to `false` automatically and the
  current refresh falls through to the standard MERGE path. A warning is
  logged.
- **Validation.** `append_only` is rejected for FULL, IMMEDIATE, and keyless
  source stream tables at both creation and ALTER time.
- **E2E tests.** New `e2e_append_only_tests.rs` with 9 tests covering:
  basic INSERT path, data correctness, DELETE/UPDATE fallback, ALTER toggle,
  FULL/IMMEDIATE/keyless rejection, and no-data cycles.
- **Unit tests.** `build_append_only_insert_sql()` SQL builder tests.
- **Upgrade script.** `sql/pg_trickle--0.4.0--0.5.0.sql` adds
  `is_append_only` column.



## [0.4.0] — 2026-03-12

### Added

#### Parallel Refresh — Phase 0 + Phase 1 (Foundation)

Infrastructure for true parallel refresh within a database. This is the first
step toward dispatching refresh work to multiple dynamic background workers
(Phases 2–7 follow). No runtime behavior change — the sequential refresh path
remains the default.

- **New GUCs:**
  - `pg_trickle.parallel_refresh_mode` (`off` | `dry_run` | `on`, default `off`)
    Controls whether the scheduler computes and dispatches parallel execution units.
  - `pg_trickle.max_dynamic_refresh_workers` (default `4`)
    Cluster-wide cap on concurrent pg_trickle refresh workers.
- **Execution Unit DAG** (`ExecutionUnitDag` in `dag.rs`):
  - `ExecutionUnit`, `ExecutionUnitId`, `ExecutionUnitKind` types.
  - Transforms `StDag` consistency groups into schedulable execution units.
  - Conservative IMMEDIATE-trigger closure collapsing — IMMEDIATE-connected
    stream tables are merged into a single execution unit to prevent unsafe
    cross-worker interactions.
  - Ready-queue computation and topological ordering.
- **Dry-run mode:** When `parallel_refresh_mode = 'dry_run'`, the scheduler
  logs execution units, dispatch order, and ready-queue contents without
  changing any runtime behavior.
- **Updated `max_concurrent_refreshes` GUC description** — now documented as
  the per-database dispatch cap (takes effect when parallel mode is enabled).
- **10 new unit tests** covering singleton, diamond, IMMEDIATE closure, mixed
  graph, empty DAG, and summary/log formatting.

#### Parallel Refresh — Phase 2 + Phase 3 (Job Table & Worker Entry Point)

Job dispatch infrastructure and dynamic background worker entry point for
parallel refresh. Still behind `parallel_refresh_mode = 'off'` (default).

- **New catalog table:** `pgtrickle.pgt_scheduler_jobs` with 15 columns
  (job_id, dag_version, unit_key, unit_kind, member_pgt_ids, root_pgt_id,
  status, scheduler_pid, worker_pid, attempt_no, enqueued_at, started_at,
  finished_at, outcome_detail, retryable) and 3 indexes.
- **Migration SQL:** `pg_trickle--0.3.0--0.4.0.sql` for existing installations.
- **Catalog CRUD:** `JobStatus` enum (Queued/Running/Succeeded/RetryableFailed/
  PermanentFailed/Cancelled) and `SchedulerJob` struct with enqueue, claim,
  complete, cancel, get_by_id, cancel_orphaned_jobs, prune_completed,
  has_inflight_job operations.
- **Shared-memory token pool:** `ACTIVE_REFRESH_WORKERS` (AtomicU32) with
  CAS-based `try_acquire_worker_token()` / `release_worker_token()`.
  `RECONCILE_EPOCH` (AtomicU64) for coordinator synchronization.
- **Dynamic refresh worker:** `pg_trickle_refresh_worker_main` entry point
  spawned via `BackgroundWorkerBuilder::load_dynamic()`. Workers claim a job,
  validate DAG version, execute the refresh unit, persist outcome, and
  release the worker token on exit.
- **Worker spawning:** `spawn_refresh_worker(db_name, job_id)` packs database
  name and job ID into bgw_extra ("db\0id" format).
- **Coordinator reconciliation:** `reconcile_parallel_state()` runs at
  scheduler startup to cancel orphaned jobs, correct leaked worker tokens
  against live `pg_stat_activity`, and prune old completed jobs.
- **15 new unit tests** for JobStatus (7) and parse_worker_extra (8).

#### Parallel Refresh — Phase 4 (Coordinator Dispatch Loop)

Coordinator ready-queue dispatch loop that replaces the inline sequential
refresh path when `parallel_refresh_mode = 'on'`. This is the core scheduling
engine for true parallel refresh.

- **Coordinator dispatch state** (`ParallelDispatchState`, `UnitDispatchState`
  in `scheduler.rs`): in-memory ready queue, per-unit upstream tracking,
  in-flight job tracking, DAG-version-aware rebuilds.
- **Ready-queue dispatch** (`parallel_dispatch_tick`): 3-step dispatch loop —
  poll completed jobs → build ready queue in topological order → dispatch
  within per-db and cluster-wide budgets.
- **Downstream readiness release**: on worker completion, immediately
  decrements `remaining_upstreams` on downstream units, enabling dispatch in
  the same tick.
- **Transaction-split spawning**: jobs enqueued inside SPI transaction, workers
  spawned after commit to avoid job-visibility races.
- **Dynamic poll interval**: 200 ms during active dispatch (in-flight jobs),
  reverts to normal `scheduler_interval_ms` when idle.
- **Stable unit keys** (`ExecutionUnit::stable_key()` in `dag.rs`):
  deterministic `s:<id>`, `a:<ids>`, `i:<ids>` keys for deduplication across
  DAG rebuilds.
- **8 new unit tests** for `stable_key` (4) and `ParallelDispatchState` /
  `is_unit_due` (4). Total: 1233 unit tests.

See [PLAN_PARALLELISM.md](plans/sql/PLAN_PARALLELISM.md) for the full design.

#### Parallel Refresh — Phase 5 (Composite Unit Execution)

Proper atomic-group and IMMEDIATE-closure execution inside dynamic refresh
workers, replacing the Phase 3 serial placeholder.

- **`execute_worker_atomic_group()`**: wraps all group members in a C-level
  sub-transaction (`BeginInternalSubTransaction`). On any member failure the
  entire group is rolled back (`RollbackAndReleaseCurrentSubTransaction`);
  on success the sub-transaction is committed
  (`ReleaseCurrentSubTransaction`). Retains all-or-nothing semantics.
- **`execute_worker_immediate_closure()`**: refreshes only the root stream
  table. Downstream IMMEDIATE-mode triggers fire synchronously within the
  same worker transaction, so no explicit member iteration is needed.
- **Worker dispatch routing**: the worker entry point now routes
  `atomic_group` and `immediate_closure` to their dedicated functions
  instead of the former serial `execute_worker_composite()` placeholder.
- Coordinator already prevents double-scheduling of unit members via the
  `ExecutionUnitDag` membership index.

See [PLAN_PARALLELISM.md](plans/sql/PLAN_PARALLELISM.md) for the full design.

#### Parallel Refresh — Phase 6 (Observability and Tuning)

Monitoring functions and health checks for the parallel refresh subsystem.

- **`pgtrickle.worker_pool_status()`**: single-row function returning active
  workers, cluster-wide budget (`max_workers`), per-db dispatch cap
  (`per_db_cap`), and current `parallel_mode`.
- **`pgtrickle.parallel_job_status(max_age_seconds)`**: table function listing
  active and recently completed scheduler jobs with job ID, unit key/kind,
  status, member count, attempt number, scheduler/worker PIDs, timestamps,
  and computed `duration_ms`.
- **`health_check()` extended** with two new checks (gated on
  `parallel_refresh_mode != off`):
  - `worker_pool` — WARN when all worker tokens are in use (saturation).
  - `job_queue` — WARN when > 10 jobs are queued (backlog building up).

See [PLAN_PARALLELISM.md](plans/sql/PLAN_PARALLELISM.md) for the full design.

#### Parallel Refresh — Phase 7 (Rollout and Default Change)

Documentation, CI validation, and rollout gating for the parallel refresh
feature. All seven implementation phases are now complete.

- **GUC documentation** (`docs/CONFIGURATION.md`): Full reference for
  `parallel_refresh_mode`, `max_dynamic_refresh_workers`, and updated
  `max_concurrent_refreshes` (now documented as the per-database dispatch
  cap). Includes worker-budget planning formula and tuning guidance.
- **Architecture documentation** (`docs/ARCHITECTURE.md`): New "Parallel
  Refresh" subsection describing the execution-unit DAG, ready queue,
  dynamic worker lifecycle, and how it relates to `max_worker_processes`.
- **CI coverage**: E2E test suite now runs a second pass with
  `PGT_PARALLEL_MODE=on` to validate correctness under parallel dispatch.
  The test harness (`tests/e2e/mod.rs`) reads the environment variable and
  applies `ALTER SYSTEM SET pg_trickle.parallel_refresh_mode` accordingly.
- **Default remains `off`**: The feature is gated behind the
  `parallel_refresh_mode` GUC through initial releases. Defaulting to `on`
  is deferred until real-world operational evidence is collected.

See [PLAN_PARALLELISM.md](plans/sql/PLAN_PARALLELISM.md) for the full design.

#### Statement-Level CDC Triggers — B3 Write-Side Benchmark Harness

Added a benchmark harness to measure the write-side overhead reduction from
statement-level CDC triggers (v0.4.0 default) vs the legacy row-level triggers.

- **`bench_stmt_vs_row_cdc_matrix`** (new `#[ignore]` test in `e2e_bench_tests.rs`):
  Full 3×3×2 matrix — narrow/medium/wide table widths × bulk INSERT/UPDATE
  + single-row INSERT × row/statement CDC mode. Prints avg/p50/p95 timings
  and per-cell speedup ratios. Expected runtime: 15–20 min.
- **`bench_stmt_vs_row_cdc_quick`** (new `#[ignore]` test): Narrow table,
  bulk INSERT only, 5 iterations. Completes in ~60 s; useful as a quick smoke-
  check.
- Both tests use `alter_system_set_and_wait()` to switch `cdc_trigger_mode`
  between runs and output a summary with per-width bulk-DML reduction
  percentages and a single-row neutrality check (±10% target).
- ROADMAP B3 and PLAN_PERFORMANCE_PART_9 §B-3 marked ✅ Done.

#### dbt Getting Started Example

- **New `examples/dbt_getting_started/` project** — end-to-end dbt example
  with org-chart seed data, staging views, and three stream table models
  (`department_tree`, `department_stats`, `department_report`). Includes
  automated test script (`just test-dbt-getting-started`) that builds the
  E2E Docker image, runs the full dbt flow, and validates stream table
  population and dbt data tests.

--- — 2026-03-11

### Fixed

#### TPC-H Test Infrastructure Hardening

Fixed three root causes of TPC-H benchmark test failures and hangs
(`e2e-test-failure-part-6`, PR #157):

- **Scheduler `'calculated'` schedule causes test lock contention** — The
  `'calculated'` schedule string stores `NULL` in the catalog and maps to
  `ScheduleMode::Calculated` (auto-refresh on any pending CDC changes), not
  "no auto-refresh" as earlier comments assumed. During the RF mutation phase
  all 22 stream tables accumulate CDC changes; the scheduler starts a single
  background transaction refreshing all 22 STs (~5 minutes), blocking the
  test's explicit refreshes and causing `test_tpch_cross_query_consistency`
  to effectively hang. Fixed by using `'24h'` schedules in
  `test_tpch_cross_query_consistency` and `test_tpch_sustained_churn`
  (time-based schedules never fire within the test window), raising
  `pg_trickle.scheduler_interval_ms` to 60 000 ms in `new_bench()`, and
  adding a 60-second `lock_timeout` to `try_refresh_st()`. Result:
  `test_tpch_cross_query_consistency` completes in ~152 s (was 5+ min / hung).

- **Advisory lock not released on transaction abort** (`src/api.rs`) —
  `pg_try_advisory_lock` is session-level: it survives transaction rollback.
  When `execute_manual_refresh()` triggered a PostgreSQL error (e.g.
  `temp_file_limit exceeded`), pgrx aborted the transaction and the subsequent
  `pg_advisory_unlock()` SPI call silently no-oped (no SPI call executes in
  an aborted transaction). The lock remained held on the pooled connection,
  causing all subsequent `refresh_stream_table()` calls for that stream table
  to return `RefreshSkipped — another refresh is already in progress`. Fixed
  by replacing with `pg_try_advisory_xact_lock`: PostgreSQL automatically
  releases transaction-level advisory locks at transaction end (commit or
  rollback). Explicit unlock call removed.

- **Bench container memory limits** (`tests/e2e/mod.rs`) — `work_mem` raised
  from 64 MB to 256 MB to reduce sort/hash spills for multi-join delta CTEs
  at SF=0.01; Docker SHM raised from 256 MB to 512 MB. `temp_file_limit`
  kept at 4 GB: raising to 16 GB caused q05 (5-table join) to write 16 GB
  before aborting, slowing cycles ~4×; fast-fail at 4 GB is preferable for
  known-DVM-limited queries.

#### HAVING Clause Differential Correctness (5 tests un-ignored)

Fixed two DVM bugs that caused HAVING threshold-crossing transitions to produce
incorrect stream table contents in DIFFERENTIAL mode:

- **`COUNT(*)` in HAVING predicate rewrite:** PostgreSQL parses `COUNT(*)` in
  HAVING as `FuncCall { agg_star: true, args: [] }`, but the expression
  normalizer converts it to `FuncCall { func_name: "count", args: [Raw("*")] }`.
  `rewrite_having_expr` checked `args.is_empty()` to detect CountStar, which
  was `false` for the normalized `[Raw("*")]` form. The unmatched call leaked
  into the WHERE clause of the HAVING-filter CTE, producing a "syntax error at
  or near `*`" PostgreSQL error. Fixed by extending the CountStar check to also
  accept a single `Raw("*")` argument. Same fix applied in
  `extract_aggregates_from_expr_inner`.

- **Threshold crossing upward — missing aggregate baseline:** When a group is
  below the HAVING threshold it is absent from the stream table (ST). The
  algebraic merge used `COALESCE(st.col, 0)` as the pre-existing aggregate
  base, which evaluates to `0` for absent groups. This made the new aggregate
  value equal to the per-cycle delta only, ignoring all pre-existing source
  rows. For example, a group with 10 existing rows getting 15 new rows would
  produce `SUM = 15` instead of `25`. Fixed by forcing a full re-aggregation
  rescan when `ctx.having_filter = true`: for groups where `st.col IS NULL`
  (new groups entering the ST), the rescan CTE's re-aggregated value is used
  instead of `0 + delta`. Existing groups (already in ST) continue to use the
  algebraic merge path.

Five previously-ignored E2E tests in `tests/e2e_having_transition_tests.rs`
now run without `#[ignore]`:

- `test_group_enters_having_threshold` — group crosses upward via SUM
- `test_group_exits_having_threshold` — group drops below threshold
- `test_having_count_star_threshold` — `COUNT(*) > N` threshold crossing
- `test_having_with_multiple_groups` — multiple groups transitioning simultaneously
- `test_having_threshold_with_nulls` — NULL-handling in HAVING aggregate

#### FULL OUTER JOIN Differential Correctness (5 tests un-ignored)

Fixed five DVM bugs that caused `FULL OUTER JOIN` stream tables to produce
incorrect results in DIFFERENTIAL mode:

- **`project.rs` — row-id mismatch:** FULL refresh computes `__pgt_row_id` as
  `hash(output_columns)`. Differential for FullJoin was passing through raw
  part row-ids (PK hashes or `0::BIGINT`) instead. Fixed by adding
  `OpTree::FullJoin { .. }` to the `is_join_child` match in `diff_project` so
  the row-id is always recomputed from output columns.

- **`aggregate.rs` — compound GROUP BY expression resolution:** `resolve_group_col`
  called `resolve_col_for_child` which only handles `ColumnRef` atoms. A
  compound expression such as `COALESCE(l.dept, r.dept)` hit the fallback
  `strip_qualifier()` path producing `COALESCE(dept, dept)` (ambiguous column
  error). Fixed by calling `resolve_expr_for_child` so FuncCall arguments are
  recursively resolved against child CTE column names.

- **`aggregate.rs` — compound GROUP BY expression quoting:** GROUP BY clause
  building and the delta SELECT used `quote_ident(expr_sql)` for resolved
  expressions, turning `COALESCE(l__dept, r__dept)` into the identifier
  `"COALESCE(l__dept, r__dept)"`. Added `col_ref_or_sql_expr` helper:
  simple bare names are quoted; expressions containing `(` are emitted as-is.

- **`aggregate.rs` — SUM NULL semantics over FULL JOIN:** After a
  matched→unmatched row transition, `COALESCE(old,0) + COALESCE(ins,0) −
  COALESCE(del,0)` evaluates to `0` even when all remaining group rows have
  `NULL` for the aggregated column — PostgreSQL `SUM` of NULLs should be
  `NULL`. Fixed by building a group-rescan CTE for non-DISTINCT `SUM`
  aggregates when the child tree contains a `FullJoin`, and using the rescanned
  value in the merge formula when `has_rescan = true`.

- **`aggregate.rs` — rescan CTE SELECT list for FuncCall group-by columns:**
  `build_rescan_cte` (and `build_intermediate_agg_delta`) selected the
  output-name string as a bare column identifier when `expr_sql == output_name`.
  For `COALESCE(…)` group-by columns, PostgreSQL interpreted `"COALESCE(…)"` as
  a column name rather than a function call. Fixed by adding an
  `!expr_sql.contains('(')` guard so FuncCall expressions use `expr AS alias`
  form.

Five previously-ignored E2E tests in `tests/e2e_full_join_tests.rs` now run
without `#[ignore]`:

- `test_full_join_basic_differential` — basic matched/unmatched transitions
- `test_full_join_null_keys_differential` — NULL join keys
- `test_full_join_key_update_migration` — key value updates
- `test_full_join_with_aggregate_differential` — SUM over FULL JOIN with COALESCE GROUP BY
- `test_full_join_multi_column_key_differential` — composite join keys

#### Correlated EXISTS with HAVING Differential Correctness (1 test un-ignored)

Fixed three DVM bugs that caused `EXISTS(... GROUP BY ... HAVING ...)` stream
tables to produce incorrect results in DIFFERENTIAL mode:

- **`parser.rs` — `parse_exists_sublink` ignored GROUP BY / HAVING:** The
  EXISTS sublink parser discarded GROUP BY and HAVING from the inner subquery,
  treating `EXISTS(SELECT 1 FROM T WHERE T.k=outer.k GROUP BY T.k HAVING agg>N)`
  as a plain "does any row exist?" check. Fixed by detecting `GROUP BY`/`HAVING`
  in the inner SELECT and restructuring to:
  `SemiJoin(cond=outer.k=sub.T.k, right=Subquery(Filter(HAVING, Aggregate(GROUP BY, Scan(T)))))`.
  New helpers `collect_tree_source_aliases`, `split_exists_correlation`, and
  `try_extract_exists_corr_pair` support the correlation-predicate extraction
  (7 new unit tests).

- **`parser.rs` — row ID mismatch for `Project(SemiJoin)` trees:** For a
  defining query `SELECT c.name FROM eh_cust WHERE EXISTS(...)`,
  `row_id_key_columns()` returned `None` when `aliases.len() ≠ child.output_columns().len()`.
  FULL refresh fell to the `row_to_json(sub)::TEXT || '/' || row_number()` hash,
  while differential passed through `hash_multi(left_cols)` from `diff_semi_join`.
  The MERGE `ON st.__pgt_row_id = d.__pgt_row_id` never matched existing rows,
  so `DELETE` mutations were silently discarded. Fixed by returning
  `Some(aliases.clone())` for `SemiJoin`/`AntiJoin` children — the same
  treatment already given to `LateralFunction`/`LateralSubquery`.

- **`project.rs` — `diff_project` row-id recomputation for SemiJoin children:**
  `diff_project` must recompute `__pgt_row_id` from the projected output columns
  for `SemiJoin`/`AntiJoin` children (matching the FULL refresh formula).
  Added `is_semijoin_child` detection alongside the existing `is_lateral_child`
  branch so both share the `build_hash_expr(projected_cols)` recomputation path.

One previously-ignored E2E test in `tests/e2e_sublink_or_tests.rs` now runs
without `#[ignore]`:

- `test_exists_with_having_in_subquery_differential` — EXISTS with GROUP BY /
  HAVING threshold crossing (insert brings group above threshold; delete drops
  it back below)

#### Correlated Scalar Subquery Differential Correctness (2 tests un-ignored)

Fixed correlated scalar subqueries in the SELECT list by decorrelating them
into LEFT JOINs before DVM parsing:

- **`parser.rs` — `rewrite_correlated_scalar_in_select` rewrite:** Correlated
  scalar subqueries (e.g., `(SELECT MAX(e.salary) FROM emp e WHERE e.dept_id =
  d.id)`) were not supported by the DVM's `ScalarSubquery` operator. Added a
  pre-parser rewrite that detects correlation conditions, extracts them into
  JOIN-ON clauses, and rewrites the subquery as a `LEFT JOIN` with GROUP BY
  aggregation. The rewrite runs before `parse_defining_query` so the DVM tree
  sees a standard LEFT JOIN.

- **`parser.rs` — inner table qualifier leak:** The rewrite initially preserved
  inner table qualifiers (`MAX(e.salary)`). The DVM's intermediate aggregate
  delta engine wraps FROM in EXCEPT ALL subqueries where the original inner
  alias is invisible. Fixed by stripping inner table qualifiers during the
  rewrite (`strip_qualifier()`), producing `MAX(salary)`.

Two previously-ignored E2E tests in `tests/e2e_scalar_subquery_tests.rs` now
run without `#[ignore]`:

- `test_correlated_scalar_subquery_differential` — correlated MAX in SELECT
  with insert/delete of top earner and new department
- `test_scalar_subquery_null_result_differential` — correlated SUM with
  NULL-producing category that has no lookup match

#### Backend Worker Detection Fix (PG 18)

Fixed a critical bug where `health_check()` and the scheduler's active-worker
detection returned zero results on PostgreSQL 18. In PG 18,
`BackgroundWorkerBuilder::new(name)` sets `bgw_name` which maps to
`pg_stat_activity.backend_type`, not `application_name`. All queries checking
for `'pg_trickle scheduler'` and `'pg_trickle launcher'` were using
`application_name` and finding zero matches.

Fixed in `src/scheduler.rs` (launcher detection), `src/monitor.rs`
(`health_check()` scheduler count), `tests/e2e/mod.rs` (`wait_for_scheduler()`),
and `tests/e2e_bgworker_tests.rs` (diagnostic dump queries).

#### Scheduler Launcher Thundering-Herd Prevention

Fixed an infinite back-off loop where concurrent test databases calling
`create_stream_table()` each bumped the DAG rebuild signal, which previously
called `last_attempt.clear()` on every bump. This wiped all per-database
back-off timers (including `postgres`), causing the launcher to immediately
re-probe, fail (extension not yet installed), and reset the timer — an
infinite loop preventing the 15 s retry TTL from ever expiring.

Fixed by retaining `last_attempt` entries whose elapsed time is still within
`retry_ttl` on a DAG signal. Only entries older than `retry_ttl` (already
expired) are evicted, so recently-failed probes stay protected by back-off.

#### dbt Getting Started Fixes

- **dbt macro: query quoting breakage** — `dbt.string_literal(query)` wraps
  values in single quotes without escaping internal quotes. Queries containing
  single-quoted strings (e.g., `' > '` in string concatenation) broke the SQL
  parsing, causing PostgreSQL to misinterpret the function call arguments.
  Fixed by using PostgreSQL dollar-quoting (`$pgtrickle$...$pgtrickle$`) in
  both `create_stream_table` and `alter_stream_table` macros.
- **dbt macro: `schedule = none` mapped to SQL NULL** — The Rust function
  rejects NULL for the schedule parameter and expects `'calculated'` instead.
  Fixed by mapping dbt `none` to `'calculated'` in the create macro.
- **View inlining: alias lost when same view used with different aliases** —
  When a query referenced the same view multiple times with different aliases
  (e.g., `FROM stg_departments` and `FROM stg_departments d`), the inliner
  always used the first occurrence's alias. Fixed by using the explicit alias
  from each RangeVar being inlined.
- **`wait_for_populated.sh`: schema-qualified name mismatch** — The script
  queried `WHERE pgt_name = 'public.table'` but the catalog stores unqualified
  names. Fixed by matching on `pgt_schema || '.' || pgt_name`.

### Changed

- **Test count:** All 18 previously-ignored DVM-correctness E2E tests have been
  re-enabled (0 remaining). The 5 HAVING tests, 5 keyless-table duplicate tests
  (already un-ignored before this release), 5 FULL JOIN tests, 1 EXISTS+HAVING
  test, and 2 correlated scalar subquery tests account for the full recovery.

- **PostgreSQL 18.1 → 18.3** — CI pipelines, Dockerfiles, and the test harness
  have been updated to use `postgres:18.3`. This is a maintenance release of
  PostgreSQL 18 with bug fixes and no API changes.

- **Dependency updates** — `tokio` 1.49 → 1.50; GitHub Actions updated:
  `docker/build-push-action` 6→7, `actions/cache` 4→5,
  `docker/login-action` 3→4, `docker/setup-buildx-action` 3→4,
  `actions/setup-python` 5→6.

---

### Added

- **`test_tpch_immediate_correctness`** — new TPC-H test that validates the
  IMMEDIATE mode IVM trigger path end-to-end, verifying that stream tables
  maintained via statement-level AFTER triggers produce the same results as
  a full recompute after RF1/RF2/RF3 mutations.

- **SAST — Narrow `rust.panic-in-sql-path` scope** (`sast-review-2` branch):
  Semgrep cannot detect `#[cfg(test)]` block boundaries inside Rust files,
  causing the `rust.panic-in-sql-path` rule to fire on every `.unwrap()` /
  `.expect()` / `panic!()` call inside inline test modules — 351 false-positive
  alerts (numbers 48–398). Fixed by adding `paths.exclude: [src/dvm/**,
  src/bin/**]` to the rule. `src/dvm/**` contains the DVM computation engine
  with large inline test modules; `src/bin/**` is the standalone CLI, not the
  PostgreSQL extension. All 351 alerts triaged and dismissed as `false positive`.
  10 genuine production hits (`expect("unreachable after error!()")` idiom in
  `monitor.rs`, `api.rs`, `wal_decoder.rs`) are known safe and tracked as Phase
  4 cleanup (replace with `unreachable!()`).

- **SAST Phase 2 + 3 — Privilege-context rules and unsafe inventory**
  (`sast-review-1` branch):

  - **Phase 2 — Extension-specific Semgrep rules** (`.semgrep/pg_trickle.yml`):
    - `sql.row-security.disabled` — flags `SET LOCAL row_security = off` in
      `src/**` and `sql/**`. Alerts reviewers when the extension disables RLS,
      which can bypass per-user policies in a `SECURITY DEFINER` context.
    - `sql.set-role.present` — flags `SET ROLE` / `RESET ROLE` patterns. Role
      transitions in extension code can widen or narrow privileges unexpectedly.
    - `rust.panic-in-sql-path` — flags `.unwrap()`, `.expect(…)`, and
      `panic!(…)` in `src/**`. These crash the PostgreSQL backend process if
      reached from a SQL-callable function. Hits ~37 existing callsites
      (mostly `expect("unreachable after error!()")` idiom in `monitor.rs` and
      `api.rs`); all advisory, documented as Phase 2 triage backlog.
    - Updated `sql.security-definer.present` message to explicitly require
      `SET search_path = schema, pg_catalog, pg_temp` alongside every
      `SECURITY DEFINER` declaration. Prevents search-path hijacking.
    - All three rules are advisory (WARNING/INFO, SARIF upload only, no CI gate).
    - No current hits in source — rules are proactive, will fire when these
      patterns are added during the RLS and privilege-hardening work in v0.3.0.

  - **Phase 3 — Unsafe block inventory** (`scripts/unsafe_inventory.sh`):
    - New `scripts/unsafe_inventory.sh` counts `unsafe {` blocks per `.rs` file
      and compares against the committed baseline in `.unsafe-baseline`.
    - `.unsafe-baseline` records the 2026-03-10 snapshot: 1309 total `unsafe {`
      blocks across 6 files (`api.rs`:10, `dvm/parser.rs`:1286, `lib.rs`:1,
      `scheduler.rs`:5, `shmem.rs`:3, `wal_decoder.rs`:4).
    - New `.github/workflows/unsafe-inventory.yml` runs the script on PRs that
      touch `src/**`. Posts a per-file delta table to `GITHUB_STEP_SUMMARY`.
      Fails if any file exceeds its baseline count.
    - New `just unsafe-inventory` recipe for local use.
    - `clippy::undocumented_unsafe_blocks` deferred: `src/dvm/parser.rs` has
      1286 mechanical pgrx FFI blocks needing a `#![allow]` first.

  - **CI workflow trigger tightening** (`.github/workflows/codeql.yml` and
    `semgrep.yml`): removed `pull_request` trigger from both CodeQL and
    Semgrep. CodeQL output lands in the Security tab only (no inline PR
    annotations), so running it on every PR added ~25 min cost with no
    faster feedback. Semgrep is advisory-only and never blocks merge.
    Both workflows still run on push-to-main and weekly Monday schedule.

- **SAST / static security analysis baseline** — new security tooling on the
  `codeql-workflow` branch, now merged:
  - **GitHub CodeQL** (`.github/workflows/codeql.yml`) — runs 16 Rust security
    queries (SQL injection, path injection, SSRF, cleartext logging, weak
    crypto, uncontrolled allocation, invalid pointer access, and more) on every
    PR and push to `main`. First scan completed with **zero findings** across
    all 115 Rust source files. Uses `build-mode: none` (the only mode supported
    for Rust) and the `v4` action.
  - **`cargo deny`** (`.github/workflows/dependency-policy.yml` + `deny.toml`)
    — enforces an explicit license allow-list (Apache-2.0, MIT, BSD-2-Clause,
    BSD-3-Clause, ISC, Unlicense, Zlib, BSL-1.0, Unicode-3.0), blocks unknown
    registries and git sources, and surfaces unmaintained/yanked crates as
    warnings. Duplicate-version skews from upstream pgrx / testcontainers
    version mismatches are suppressed via `skip` entries.
  - **Semgrep** (`.github/workflows/semgrep.yml` + `.semgrep/pg_trickle.yml`)
    — repo-specific rules flagging dynamic SQL passed to `Spi::run` /
    `Spi::get_*` and `SECURITY DEFINER` occurrences. Advisory-only (SARIF
    upload, no CI failure) until rules are tuned.
  - **`plans/testing/PLAN_SAST.md`** — full SAST strategy document including
    threat model, five-phase rollout plan, Semgrep rules roadmap, unsafe/FFI
    review policy, CI posture table, and a **Security Newbie Checklist** for
    reviewers unfamiliar with extension-specific security patterns.

- **TPC-H test suite enhancements (T1–T6)** — second wave of TPC-H correctness
  coverage, building on the 22/22 passing DIFFERENTIAL baseline:
  - **T1 — `__pgt_count` guard** in `assert_tpch_invariant`: detects
    over-retraction bugs (negative multiplicity) before the EXCEPT ALL check,
    so they surface even when extra and missing rows cancel out. Applies
    automatically to all existing TPC-H tests.
  - **T2 — Skip-set regression guard** in `test_tpch_differential_correctness`
    and `test_tpch_immediate_correctness`: any query newly skipped that is not
    in the per-mode allowlist causes the test to fail with a clear diagnostic.
    Prevents silent regressions as DVM evolves.
  - **T3 — `test_tpch_immediate_rollback`**: verifies that a rolled-back DML
    transaction leaves an IMMEDIATE-mode stream table in exactly the same state
    as before. Covers q01, q06, q03, q05 across RF1/RF2/RF3 mutation types.
    Uses sqlx's `pool.begin()` + `txn.rollback()` to span real PostgreSQL
    transactions.
  - **T4 — `test_tpch_differential_vs_immediate`**: side-by-side comparison of
    DIFFERENTIAL and IMMEDIATE modes. For each query that succeeds in both
    modes, verifies that both STs produce identical results after shared RF
    mutations. Catches cases where both incremental paths agree with each other
    but diverge from ground truth in the same way.
  - **T5 — `test_tpch_single_row_mutations`**: validates the IVM trigger path
    for single-row INSERT, UPDATE, and DELETE (1-row `NEW TABLE`/`OLD TABLE`)
    on q01, q06, q03 in IMMEDIATE mode. Includes three new SQL fixtures:
    `tests/tpch/single_row_insert.sql`, `single_row_update.sql`,
    `single_row_delete.sql` (fixed order key 9999991 to avoid data collisions).
  - **T6 — `tests/e2e_tpch_dag_tests.rs`** — new test file with two DAG tests:
    - `test_tpch_dag_chain`: two-level DAG (Q01 as level-0, filtered projection
      as level-1). Refreshes in topological order and asserts both STs after
      each RF cycle.
    - `test_tpch_dag_multi_parent`: multi-parent fan-in (Q01 + Q06 as level-0,
      aggregated UNION ALL as level-1). Validates the DAG refresh scheduler
      with two independent parent STs sharing a downstream dependent.

## [0.2.3] — 2026-03-09

### Added

- **Non-deterministic function handling** — defining queries are now
  scanned for function and operator volatility before stream table creation.
  VOLATILE functions and custom operators are rejected in `DIFFERENTIAL` and
  `IMMEDIATE` modes, while STABLE functions continue with a warning. E2E
  coverage verifies volatile rejection, stable-function warnings,
  immutable-function acceptance, and nested volatile detection inside `WHERE`
  expressions.

- **Per-table `cdc_mode` override (G1)** — `pgtrickle.create_stream_table(...)`
  and `pgtrickle.alter_stream_table(...)` now accept an optional `cdc_mode`
  parameter (`'auto'`, `'trigger'`, or `'wal'`). The requested override is
  stored in `pgtrickle.pgt_stream_tables.requested_cdc_mode`, dbt exposes the
  same setting, and WAL transition eligibility is now resolved per source from
  the effective requests of all deferred stream tables that depend on it.

- **`pgtrickle.pgt_cdc_status` view (G5)** — new convenience view that joins
  `pgt_dependencies`, `pgt_stream_tables`, `pg_class`, and `pg_namespace` to
  expose per-source CDC state in one place. Columns: `pgt_schema`, `pgt_name`,
  `source_relid`, `source_name`, `source_schema`, `cdc_mode`, `slot_name`,
  `decoder_confirmed_lsn`, `transition_started_at`. Useful for tracking
  in-progress TRIGGER→WAL transitions.

- **`cdc_modes` column in `pgtrickle.pg_stat_stream_tables` (G5)** — text
  array of distinct CDC modes across all TABLE-type sources of each stream
  table (e.g. `{wal}`, `{trigger,wal}`, `{transitioning,wal}`). Populated via
  a correlated subquery on `pgt_dependencies`.

- **`0.2.2 -> 0.2.3` upgrade SQL** — added
  `sql/pg_trickle--0.2.2--0.2.3.sql` so `ALTER EXTENSION pg_trickle UPDATE`
  picks up the `requested_cdc_mode` catalog column, the updated
  `create_stream_table` / `alter_stream_table` signatures, the
  `pgtrickle.pgt_cdc_status` view, and the revised
  `pgtrickle.pg_stat_stream_tables` definition with `cdc_modes`.

- **Configurable WAL slot lag thresholds** — Added
  `pg_trickle.slot_lag_warning_threshold_mb` (default 100 MB) and
  `pg_trickle.slot_lag_critical_threshold_mb` (default 1024 MB). The
  scheduler now emits `slot_lag_warning` alerts on `pg_trickle_alert`,
  `pgtrickle.health_check()` uses the warning threshold, and
  `pgtrickle.check_cdc_health()` uses the critical threshold instead of
  hard-coded values.

- **`pg_trickle_dump` backup tool** — Added a standalone `pg_trickle_dump`
  CLI that connects over pgwire, topologically orders stream tables by
  dependency, and emits replayable SQL using
  `pgtrickle.create_stream_table(...)` plus follow-up status restoration.
  This gives operators a concrete pre-upgrade / pre-rollback export path
  instead of relying on manual catalog queries.

### Documentation

- Clarified volatility semantics across the SQL reference, DVM operator docs,
  README support matrix, roadmap, and implementation plan. The docs now match
  PostgreSQL volatility categories: `now()` / `current_timestamp` are treated
  as STABLE, while `random()` / `clock_timestamp()` / `gen_random_uuid()` are
  treated as VOLATILE.

### Changed

- **WAL slot advancement after FULL refresh (G3)** — `wal_decoder.rs` gains a
  new `advance_slot_to_current()` helper that calls
  `pg_replication_slot_advance($1, pg_current_wal_lsn())` (skips gracefully if
  the slot does not exist). The shared `post_full_refresh_cleanup()` helper in
  `refresh.rs` calls it for every WAL/TRANSITIONING dependency after each FULL
  refresh, preventing WAL segment bloat and replication-lag false alarms.

- **Change buffer flush after adaptive FULL fallback (G4)** — the
  `post_full_refresh_cleanup()` helper (see G3 above) also calls
  `cleanup_change_buffers_by_frontier()` after every FULL refresh. This
  eliminates the change-ratio ping-pong cycle where bulk-loaded tables caused
  the AUTO scheduler to alternate between DIFFERENTIAL and FULL indefinitely.
  The helper is invoked from `scheduler.rs` after `store_frontier()` in the
  `Full`, `Reinitialize`, and empty-prev-frontier `Differential` arms, and from
  the adaptive fallback path inside `execute_differential_refresh()`.

- **Differential refresh now rejects missing baselines defensively** —
  `execute_differential_refresh()` now returns a user error if it is invoked
  for an unpopulated stream table or without a previous frontier. Manual
  refresh still falls back to FULL for `initialize => false` stream tables,
  and unit plus E2E coverage now lock in both the low-level guard and the
  public fallback behavior.

- **IMMEDIATE/WAL CDC interaction clarified** — `create_stream_table()` and
  `alter_stream_table()` now emit an INFO message when
  `pg_trickle.cdc_mode = 'wal'` is in effect but the requested
  `refresh_mode = 'IMMEDIATE'`. IMMEDIATE mode continues to use
  statement-level IVM triggers and does not install CDC triggers or create WAL
  replication slots. Explicit per-table `cdc_mode => 'wal'` requests are now
  rejected for `IMMEDIATE` mode with a clear user error, while the global-GUC
  path continues to log INFO and proceed with IVM triggers. Unit tests and E2E
  coverage now lock in both behaviors.

- **Prepared statement invalidation completed** — backends now explicitly
  `DEALLOCATE` tracked `__pgt_merge_*` prepared statements when the shared
  cache generation advances, closing the remaining path where cross-backend
  invalidation cleared local bookkeeping without releasing the PostgreSQL-side
  prepared plan.

- **`pg_trickle.user_triggers` simplified** — the canonical modes are now
  `auto` and `off`. The legacy `on` value remains accepted as a deprecated
  compatibility alias for `auto`, so existing configs keep working without
  preserving the redundant runtime branch.

- **CI test pyramid rebalanced** — PRs now run a faster three-tier gate:
  Linux unit tests, integration tests, and a curated Light E2E tier split
  across three shards against stock `postgres:18.3`. The heavier full E2E,
  TPC-H, dbt, CNPG smoke, and extra-platform unit jobs remain off the PR
  critical path and continue to run on push-to-main, schedule, or manual
  dispatch. The shared CI setup action now caches the `cargo-pgrx` binary,
  cutting Linux setup time substantially on warm runs.

- **Test harness hardening for shared PostgreSQL E2E runs** — the shared
  full-E2E harness now resets `ALTER SYSTEM` state during teardown,
  terminates lingering scheduler/client backends before cleanup, and
  serializes shared-`postgres` resets so parallel test processes do not fight
  over the same database state.

- **Windows CI restores the cached `cargo-pgrx` toolchain reliably again** —
  the setup action now handles the cached binary path correctly on Windows,
  and the test harness changes that accompanied the fix avoid shared-postgres
  reset deadlocks in E2E runs.

---

## [0.2.2] — 2026-03-08

### Added

- **ALTER QUERY** — `alter_stream_table` now accepts a `query` parameter to
  change the defining query of an existing stream table. The function validates
  the new query, classifies schema changes (same, compatible, or incompatible),
  migrates the storage table accordingly, updates catalog entries and
  dependencies, performs ALTER-aware cycle detection, and runs a full refresh.
  Compatible schema changes preserve the storage table OID (views, policies,
  and publications remain valid).

- **AUTO refresh mode** — New default refresh mode for `create_stream_table`.
  AUTO uses differential maintenance when the query supports it and
  automatically falls back to FULL when it doesn't (unsupported constructs,
  materialized views, or DVM parse failures). Explicit `'DIFFERENTIAL'`
  retains strict error behavior.

- **Version mismatch check** — The background scheduler now compares the
  compiled shared library version against the SQL-installed extension version
  at startup. A WARNING is logged if they differ, helping users detect stale
  `.so` installs after `ALTER EXTENSION pg_trickle UPDATE`.

- **FAQ upgrade section** — Expanded the Deployment & Operations section of
  the FAQ with upgrade instructions, version mismatch detection, and stream
  table preservation guidance. Cross-links to the full
  [Upgrading Guide](docs/UPGRADING.md).

- **ORDER BY + LIMIT + OFFSET (Paged TopK)** — `OFFSET` is now supported in
  defining queries with `ORDER BY ... LIMIT N OFFSET M`. Nine E2E tests
  validate paging, catalog metadata storage, aggregate queries, and rejection
  of invalid patterns (OFFSET without LIMIT, dynamic OFFSET, negative OFFSET).
  The `topk_offset` catalog column was pre-provisioned in v0.2.1.

- **IMMEDIATE mode — WITH RECURSIVE (IM1)** — `WITH RECURSIVE` queries are now
  fully supported in IMMEDIATE refresh mode. The delta engine applies the
  base-case delta first, then iterates the recursive step until no new rows
  are produced (semi-naive evaluation). A depth counter prevents infinite loops;
  the limit is controlled by `pg_trickle.ivm_recursive_max_depth` (default 100).
  A warning is emitted at stream table creation time for very deep hierarchy
  queries so you know the recursion guard is active.

- **IMMEDIATE mode — TopK micro-refresh (IM2)** — `ORDER BY ... LIMIT N`
  queries are now fully supported in IMMEDIATE refresh mode. On each DML
  statement the top-N rows are recomputed and merged into the stream table.
  Ten E2E tests validate INSERT/UPDATE/DELETE propagation, paged TopK,
  aggregate TopK, threshold rejection, and mode switching between IMMEDIATE
  and DIFFERENTIAL. The maximum N is controlled by
  `pg_trickle.ivm_topk_max_limit` (default 1000).

- **Edge case hardening (EC1–EC3)** — New operational guardrails and coverage:
  - `pg_trickle.max_grouping_set_branches` GUC (default 64) caps
    CUBE/ROLLUP branch-count explosion at parse time, with three E2E tests.
  - Post-restart CDC `TRANSITIONING` health checks detect stuck transitions
    after crash or restart and roll them back to TRIGGER mode.
  - `pg_trickle.foreign_table_polling` enables polling-based differential
    maintenance for foreign tables via snapshot comparison (`EXCEPT ALL`), with
    three E2E tests.

- **WAL CDC hardening (W1/W2/W3):**
  - WAL mode E2E coverage now mirrors the trigger-based suite for slot
    creation, DML capture, fallback, cleanup, and keyless-table handling.
  - Automatic fallback is hardened with consecutive-error tracking and
    `wal_level` revalidation on each poll cycle.
  - `pg_trickle.cdc_mode` default is promoted from `'trigger'` to `'auto'`.

- **Documentation sweep and reorganization (DS1/DS2/DS3):**
  - DDL-during-refresh behaviour, replication/standby limitations, and
    PgBouncer constraints are now documented.
  - `CONFIGURATION.md` and `SQL_REFERENCE.md` are reorganized around practical
    operator workflows instead of flat lists.
  - `GETTING_STARTED.md` now clarifies that trigger-based CDC supports source
    tables without a primary key; a PK is only required for WAL auto-transition.

### Changed

- `create_stream_table` default `refresh_mode` changed from `'DIFFERENTIAL'`
  to `'AUTO'`.
- `create_stream_table` default `schedule` changed from `'1m'` to
  `'calculated'`.
- `pg_trickle.cdc_mode` default changed from `'trigger'` to `'auto'` — the
  scheduler now starts WAL-based CDC automatically when `wal_level = logical`;
  it falls back to trigger-based CDC on replicas or when WAL is unavailable.

---

## [0.2.1] — 2026-03-05

### Added

- **Upgrade migration infrastructure** — Complete safety net for
  `ALTER EXTENSION pg_trickle UPDATE` upgrades:
  - `scripts/check_upgrade_completeness.sh` — CI-runnable script that diffs
    the pgrx-generated full install SQL against the hand-authored upgrade
    script to detect missing functions, views, or event triggers.
  - `tests/Dockerfile.e2e-upgrade` — lightweight Docker image for testing
    real version-to-version upgrades (`CREATE EXTENSION VERSION '0.1.3'` →
    `ALTER EXTENSION UPDATE TO '0.2.0'`).
  - 6 new upgrade E2E tests verifying function existence, stream table
    survival, view queryability, event triggers, version consistency, and
    function parity with fresh installs after upgrade.
  - `upgrade-check` CI job runs on every PR (no Docker needed).
  - `upgrade-e2e` CI job runs on push-to-main and daily schedule.
  - `sql/archive/` directory with archived SQL baselines for each version.
  - `just check-upgrade`, `just build-upgrade-image`, `just test-upgrade`
    convenience targets.
  - `docs/UPGRADING.md` user-facing upgrade guide.
  - `sql/pg_trickle--0.2.0--0.2.1.sql` — three schema changes:
    - `has_keyless_source BOOLEAN NOT NULL DEFAULT FALSE` (EC-06): flag set
      when any source table lacks a primary key; changes the row-id apply
      strategy from MERGE to counted DELETE to handle duplicate rows safely.
    - `function_hashes TEXT` (EC-16): stores the last-seen MD5 hash of each
      function body referenced in the defining query; differential refreshes
      detect silent `ALTER FUNCTION` body changes and force a full refresh.
    - `topk_offset INT` (OS2 / EC-14): stores the `OFFSET` value for paged
      TopK queries (`ORDER BY … LIMIT … OFFSET`). Both the column and the full
      OFFSET implementation ship in this release (see
      **ORDER BY + LIMIT + OFFSET** below).

- **GitHub Pages book expansion** — Six new documentation pages across
  three new sections:
  - *Integrations:* dbt-pgtrickle documentation.
  - *Reference:* Contributing guide, Security policy, Release process.
  - *Research:* pg_ivm comparison, Triggers vs Replication analysis.
  - Book grew from 14 to 20 pages across 6 sections.

- **ORDER BY + LIMIT + OFFSET (Paged TopK)** — `OFFSET` is now supported in
  defining queries that use `ORDER BY … LIMIT N OFFSET M`. This lets you
  create a stream table over a paginated result (for example, "the second page
  of the top-100 products by revenue"). Refreshes use the same scoped
  recomputation as regular TopK, so adding OFFSET has no additional cost.
  Eight new E2E tests cover paging, catalog storage, aggregate TopK with
  OFFSET, and rejection of invalid patterns (OFFSET without LIMIT, dynamic
  OFFSET, negative OFFSET).

- **`'calculated'` schedule keyword** — `create_stream_table` and
  `alter_stream_table` now accept `'calculated'` as the schedule value to
  request automatic schedule derivation. Previously you had to pass SQL `NULL`,
  which was confusing; SQL `NULL` now returns a helpful error message suggesting
  `'calculated'` instead. New `pg_trickle.default_schedule_seconds` GUC
  (default 1 s) sets the target refresh interval for isolated CALCULATED stream
  tables (those not driven by an upstream stream table's refresh).

- **Diamond GUC simplification** — The cluster-wide
  `pg_trickle.diamond_consistency` and `pg_trickle.diamond_schedule_policy`
  GUCs have been removed. Per-stream-table `diamond_consistency` and
  `diamond_schedule_policy` parameters on `create_stream_table` /
  `alter_stream_table` are still fully supported and now default to `'atomic'`
  / `'fastest'`. Diamond detection, atomic-group scheduling, and the
  `pgtrickle.diamond_groups()` monitoring function all continue to work.

- **Edge case hardening — correctness + operational safety** — Nine issues
  addressed across the refresh pipeline:
  - **EC-01** — Inner/left/full join delta operators now evaluate deletes
    against the pre-change right-side state (R₀) rather than the current state.
    This prevents stale partner rows from contaminating results when a left-key
    change and a right-row deletion happen in the same transaction.
  - **EC-06** — Warning emitted at `create_stream_table` time when any source
    table lacks a primary key. The `has_keyless_source` catalog flag switches
    the row-apply strategy to a counted DELETE that handles duplicate rows
    safely.
  - **EC-11** — `scheduler_falling_behind` NOTIFY alert when the refresh queue
    exceeds 80 % capacity, giving operators early warning before refreshes start
    queuing up.
  - **EC-13** — `diamond_consistency` now defaults to `'atomic'` for new
    stream tables (was `'none'`). The atomic-group scheduling logic was fixed
    to correctly identify and batch all diamond members.
  - **EC-15** — Warning at `create_stream_table` time when the defining query
    uses `SELECT *`. Star expansion silently picks up new columns after
    `ALTER TABLE ADD COLUMN`, which can break differential maintenance.
  - **EC-18** — Rate-limited log message explaining when and why CDC
    auto-transition is stuck in the trigger phase (e.g. no replication slot
    available), so the situation is diagnosable without reading source code.
  - **EC-19** — Hard error at `create_stream_table` time when WAL CDC is
    requested for a keyless table that has not set `REPLICA IDENTITY FULL`.
  - **EC-25/26** — Guard triggers on stream table storage tables block
    accidental direct DML outside a pg_trickle refresh. A new
    `pg_trickle.internal_refresh` GUC bypass lets the refresh executor through.
  - **EC-34** — Scheduler auto-detects a missing WAL replication slot and falls
    back to trigger-based CDC rather than entering an error loop.

- **DVM parser improvements — three new rewrite passes:** These expand the set
  of queries that can be maintained differentially without any changes to your
  SQL:
  - *Nested window functions in expressions* — queries like
    `SELECT ABS(ROW_NUMBER() OVER (ORDER BY score) - 5) AS dist FROM t` are
    auto-rewritten by lifting the window call into an inner subquery before
    the DVM parser analyzes the query.
  - *NULL-safe ALL subqueries* — `WHERE col > ALL(SELECT …)` now correctly
    excludes NULLs in the subquery result per SQL standard semantics.
  - *Deep AND/OR with SubLinks* — `WHERE (a AND b) OR EXISTS (SELECT …)` at
    arbitrary depth is correctly rewritten to the semi-join/anti-join form the
    DVM delta pipeline expects.

- **IMMEDIATE mode parity — initial infrastructure** — Foundation for full
  IMMEDIATE mode parity with DIFFERENTIAL (completed and hardened with E2E
  tests in v0.2.2):
  - *`WITH RECURSIVE` in IMMEDIATE* — the parser allows recursive CTEs and the
    delta engine applies the base-case delta followed by the recursive step.
  - *TopK in IMMEDIATE* via `apply_topk_micro_refresh()` — recomputes the top-N
    rows on each DML statement. Bounded by the new
    `pg_trickle.ivm_topk_max_limit` GUC (default 1000).
  - *`pg_trickle.max_grouping_set_branches`* GUC (default 64, range 1–65536)
    limits `GROUPING SETS` / `CUBE` / `ROLLUP` expansion to avoid query plan
    explosion.
  - *Foreign table polling* (`pg_trickle.foreign_table_polling = on`) — lets
    stream tables use foreign tables as sources. Changes are detected by
    comparing the foreign table against a snapshot using `EXCEPT ALL`.
  - *Change buffer partitioning* (`pg_trickle.buffer_partitioning`) — enables
    LSN-range RANGE partitioning of CDC change buffer tables. Processed
    partitions can be detached and dropped in O(1) time.
  - *Column pruning* — the DVM scan operator now omits source columns that are
    not referenced in the defining query, reducing I/O and memory usage for
    wide tables.

- **E2E pipeline DAG tests** — 21 new E2E tests across four test files covering
  realistic multi-layer stream table pipelines. Scenarios include multi-cycle
  delta drift over 10 refresh cycles, mixed FULL/DIFFERENTIAL/IMMEDIATE modes
  in a single pipeline, auto-refresh cascade propagation through 3+ DAG layers,
  4-leaf fan-out and 5-layer-deep chains, and IMMEDIATE cascades. Schemas are
  drawn from Nexmark, e-commerce, and IoT domains.

- **CI / developer-experience improvements:**
  - `just test-bench-e2e` and `just test-bench-e2e-fast` convenience targets
    for running benchmark tests inside the E2E Docker container.
  - E2E Docker image cold-build time reduced from ~10 min to ~2–3 min using
    BuildKit cache mounts for the Cargo registry and incremental compile
    artifacts, plus a pre-built `pg_trickle_builder:pg18` base image.
  - E2E test suite timeout raised to 60 minutes.

### Changed

- `create_stream_table` default `schedule` changed from `'1m'` to
  `'calculated'`.
- `pg_trickle.min_schedule_seconds` default lowered from 60 s to 1 s.
- `pg_trickle.diamond_consistency` and `pg_trickle.diamond_schedule_policy`
  cluster-wide GUCs removed; per-table parameters remain and default to
  `'atomic'` / `'fastest'`.

### Fixed

- **Upgrade script completeness** — The `pg_trickle--0.1.3--0.2.0.sql`
  upgrade script was a no-op placeholder, causing `ALTER EXTENSION UPDATE`
  to silently skip 11 new functions. Fixed by adding all missing
  `CREATE OR REPLACE FUNCTION` statements. Validated by the new completeness
  check script.

- **CTE WITH clause in UNION ALL** — The query parser failed to extract source
  tables from queries that combined a `WITH` (CTE) clause with `UNION ALL`
  because it did not strip the `WITH` clause before recursing into the union
  branches. Stream table creation on such queries now works correctly.

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

- **Multi-database auto-discovery** — The background scheduler now
  automatically discovers and services all databases on the server where
  pg_trickle is installed. A single long-lived **launcher worker** connects
  to `postgres`, polls `pg_database` every 10 seconds, and dynamically spawns
  a per-database **scheduler worker** for each database that has the extension.
  Per-database workers exit cleanly if pg_trickle is not installed, and the
  launcher retries each database at a 5-minute back-off. No manual
  configuration is required — removing the need for the `pg_trickle.database`
  GUC, which has been removed.

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

- **`LIMIT ALL` handling** — PostgreSQL 18 represents `LIMIT ALL` internally
  as a null-constant node (`A_Const{isnull=true}`). The TopK detector and
  LIMIT rejection code now correctly treat this as "no limit specified", so
  queries with an explicit `LIMIT ALL` are not incorrectly rejected or tagged
  as TopK queries.

- **False stable-function warnings** — Common arithmetic expressions like
  `depth + 1` or `path || name` sometimes triggered a spurious
  `WARNING: query may produce incorrect incremental results`. This happened
  because the operator volatility check found a more volatile overload of the
  same operator symbol (e.g. `timestamp + interval` is STABLE, even though
  `integer + integer` is IMMUTABLE). Fixed by filtering out temporal and
  pseudo-type operand overloads before computing worst-case volatility.

- **GROUP BY alias resolution in auto-index creation** — When the defining
  query renamed a GROUP BY column (e.g. `SELECT id AS department_id, …`), the
  auto-created composite index referenced the raw source-table name (`"id"`)
  instead of the storage-table alias (`"department_id"`), producing
  `ERROR: column "id" does not exist`. Fixed.

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
