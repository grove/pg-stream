//! GUC (Grand Unified Configuration) variables for pgtrickle.
//!
//! These are registered in `_PG_init()` and control the extension's behavior.
//! All GUC names are prefixed with `pgtrickle.`.

use pgrx::guc::*;

/// Master enable/disable switch for the extension.
pub static PGS_ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Scheduler wake interval in milliseconds.
pub static PGS_SCHEDULER_INTERVAL_MS: GucSetting<i32> = GucSetting::<i32>::new(1000);

/// Minimum allowed schedule in seconds.
pub static PGS_MIN_SCHEDULE_SECONDS: GucSetting<i32> = GucSetting::<i32>::new(1);

/// Default effective schedule (in seconds) for isolated CALCULATED stream tables
/// that have no downstream dependents.
pub static PGS_DEFAULT_SCHEDULE_SECONDS: GucSetting<i32> = GucSetting::<i32>::new(1);

/// Maximum consecutive errors before auto-suspending a stream table.
pub static PGS_MAX_CONSECUTIVE_ERRORS: GucSetting<i32> = GucSetting::<i32>::new(3);

/// Schema name for change buffer tables.
pub static PGS_CHANGE_BUFFER_SCHEMA: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"pgtrickle_changes"));

/// Maximum number of concurrent refresh workers.
pub static PGS_MAX_CONCURRENT_REFRESHES: GucSetting<i32> = GucSetting::<i32>::new(4);

/// Maximum change-to-table ratio before falling back to FULL refresh.
///
/// When the number of pending change buffer rows exceeds this fraction of
/// the source table's estimated row count, DIFFERENTIAL refresh automatically
/// falls back to FULL refresh to avoid the JSONB/window-function overhead
/// that makes DIFFERENTIAL slower than FULL at high change rates.
///
/// Set to 0.0 to disable adaptive fallback (always use DIFFERENTIAL).
/// Set to 1.0 to always fall back (effectively forcing FULL mode).
pub static PGS_DIFFERENTIAL_MAX_CHANGE_RATIO: GucSetting<f64> = GucSetting::<f64>::new(0.15);

/// PH-E1: Maximum estimated delta result rows before falling back to FULL refresh.
///
/// Before executing the MERGE, the refresh executor runs a capped
/// `SELECT count(*) FROM (delta_query LIMIT N+1)` to estimate the output
/// cardinality. If the count reaches this limit, a NOTICE is emitted and
/// the refresh downgrades to FULL to avoid OOM or excessive temp-file spills.
///
/// Set to 0 to disable the estimation check (default).
/// Recommended range: 50_000–500_000 depending on available memory.
pub static PGS_MAX_DELTA_ESTIMATE_ROWS: GucSetting<i32> = GucSetting::<i32>::new(0);

/// WM-7: Maximum seconds a watermark may remain un-advanced before being
/// considered "stuck". When a watermark group contains a stuck source,
/// downstream stream tables in that group are paused (skipped) and a
/// `pgtrickle_alert` NOTIFY with category `watermark_stuck` is emitted.
///
/// Set to 0 to disable stuck-watermark detection (default).
pub static PGS_WATERMARK_HOLDBACK_TIMEOUT: GucSetting<i32> = GucSetting::<i32>::new(0);

/// PH-E2: Temp blocks written threshold for spill detection.
///
/// After each differential MERGE, the refresh executor queries
/// `pg_stat_statements` for `temp_blks_written`. If the value exceeds
/// this threshold, the refresh is considered a "spill". When
/// `spill_consecutive_limit` consecutive spills are recorded for the
/// same stream table, the scheduler forces a FULL refresh on the next
/// cycle to avoid repeated temp-file overhead.
///
/// Set to 0 to disable spill detection (default).
/// Requires `pg_stat_statements` extension to be installed.
pub static PGS_SPILL_THRESHOLD_BLOCKS: GucSetting<i32> = GucSetting::<i32>::new(0);

/// PH-E2: Number of consecutive spills before auto-switching to FULL refresh.
///
/// When a stream table accumulates this many consecutive differential
/// refreshes where `temp_blks_written > spill_threshold_blocks`, the
/// scheduler marks the ST for reinitialization (FULL refresh) on the
/// next cycle. The counter resets after each non-spilling refresh.
pub static PGS_SPILL_CONSECUTIVE_LIMIT: GucSetting<i32> = GucSetting::<i32>::new(3);

/// Whether to use TRUNCATE instead of DELETE for change buffer cleanup
/// when the entire buffer is consumed by a refresh.
///
/// TRUNCATE is O(1) regardless of row count, versus per-row DELETE which
/// must update indexes. This saves 3–5ms per refresh at 10%+ change rates.
///
/// Set to false if the TRUNCATE AccessExclusiveLock on the change buffer
/// is problematic for concurrent DML on the source table.
pub static PGS_CLEANUP_USE_TRUNCATE: GucSetting<bool> = GucSetting::<bool>::new(true);

/// C4: Consolidated planner aggressiveness switch.
///
/// When enabled (default), the refresh executor estimates the delta size and
/// applies `SET LOCAL` planner hints before MERGE execution:
/// - delta >= 100 rows: `SET LOCAL enable_nestloop = off` (favour hash joins)
/// - delta >= 10 000 rows: additionally `SET LOCAL work_mem = '<N>MB'`
///
/// Replaces the old `merge_planner_hints` and `merge_work_mem_mb` GUCs
/// (both still accepted but emit deprecation warnings).
pub static PGS_PLANNER_AGGRESSIVE: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Deprecated — use `pg_trickle.planner_aggressive` instead.
/// Kept for backward compatibility; emits a deprecation warning when read.
pub static PGS_MERGE_PLANNER_HINTS: GucSetting<bool> = GucSetting::<bool>::new(true);

/// `work_mem` (in MB) applied via `SET LOCAL` when the estimated delta
/// exceeds 10 000 rows and planner hints are enabled.
///
/// A higher value lets PostgreSQL use larger hash tables for the MERGE
/// join, avoiding disk-spilling sort/merge strategies on large deltas.
pub static PGS_MERGE_WORK_MEM_MB: GucSetting<i32> = GucSetting::<i32>::new(64);

/// SCAL-3: Maximum `work_mem` (in MB) allowed during delta MERGE execution.
///
/// When the planner hints would set `work_mem` above this cap (for deep
/// joins or large deltas), the refresh falls back to FULL instead. This
/// prevents OOM on unexpectedly large deltas where hash joins would
/// allocate unbounded memory.
///
/// Set to 0 to disable the cap (default — no limit enforced).
/// Recommended range: 128–1024 depending on available system memory.
pub static PGS_DELTA_WORK_MEM_CAP_MB: GucSetting<i32> = GucSetting::<i32>::new(0);

/// Whether to use SQL PREPARE / EXECUTE for MERGE statements.
///
/// When enabled, the refresh executor issues `PREPARE __pgt_merge_{id}`
/// on the first cache-hit cycle, then uses `EXECUTE` on subsequent cycles.
/// After ~5 executions PostgreSQL switches from a custom plan to a generic
/// plan, saving 1–2ms of parse/plan overhead per refresh.
///
/// Disable if prepared-statement parameter sniffing produces poor plans
/// (e.g., highly skewed LSN distributions).
pub static PGS_USE_PREPARED_STATEMENTS: GucSetting<bool> = GucSetting::<bool>::new(true);

/// User-trigger handling mode for stream table refresh.
///
/// - `"auto"` (default): Detect user-defined row-level triggers on the
///   stream table and automatically use explicit DML (DELETE + UPDATE +
///   INSERT) so triggers fire with correct `TG_OP`, `OLD`, and `NEW`.
/// - `"off"`: Always use MERGE; user triggers will NOT fire correctly.
/// - `"on"`: Deprecated compatibility alias for `"auto"`.
pub static PGS_USER_TRIGGERS: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"auto"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTriggersMode {
    Auto,
    Off,
}

impl UserTriggersMode {
    pub fn as_str(self) -> &'static str {
        match self {
            UserTriggersMode::Auto => "auto",
            UserTriggersMode::Off => "off",
        }
    }
}

fn normalize_user_triggers_mode(value: Option<String>) -> UserTriggersMode {
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("off") => UserTriggersMode::Off,
        _ => UserTriggersMode::Auto,
    }
}

fn threshold_mb_to_bytes(megabytes: i32) -> i64 {
    megabytes as i64 * 1024 * 1024
}

/// CDC mechanism selection.
///
/// - `"auto"` (default): Use triggers for creation, transition to WAL if
///   `wal_level = logical` is available. Falls back to triggers automatically.
/// - `"trigger"`: Always use row-level triggers for CDC.
/// - `"wal"`: Require WAL-based CDC (fail if `wal_level != logical`).
pub static PGS_CDC_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"auto"));

/// Maximum time (seconds) to wait for the WAL decoder to catch up during
/// transition from triggers to WAL-based CDC before falling back to triggers.
pub static PGS_WAL_TRANSITION_TIMEOUT: GucSetting<i32> = GucSetting::<i32>::new(300);

/// Warning threshold (in MB) for retained WAL on pg_trickle replication slots.
///
/// When a WAL-mode source retains more than this amount of WAL, pg_trickle:
/// - emits a `slot_lag_warning` NOTIFY event from the scheduler, and
/// - reports a WARN row in `pgtrickle.health_check()`.
pub static PGS_SLOT_LAG_WARNING_THRESHOLD_MB: GucSetting<i32> = GucSetting::<i32>::new(100);

/// Critical threshold (in MB) for retained WAL on pg_trickle replication slots.
///
/// When a WAL-mode source retains more than this amount of WAL,
/// `pgtrickle.check_cdc_health()` reports a `slot_lag_exceeds_threshold` alert
/// for the source.
pub static PGS_SLOT_LAG_CRITICAL_THRESHOLD_MB: GucSetting<i32> = GucSetting::<i32>::new(1024);

/// When true, schema-altering DDL (column ADD/DROP/RENAME/ALTER TYPE) on
/// source tables used by stream tables is blocked with an ERROR instead of
/// triggering reinitialization.
///
/// Benign DDL (CREATE INDEX, COMMENT ON, ALTER TABLE SET STATISTICS) and
/// constraint-only changes are always allowed regardless of this setting.
///
/// Default is `true` (enabled) as of v0.11.0 — set to `false` to restore
/// the previous permissive behavior (DDL triggers reinitialization instead
/// of blocking).
pub static PGS_BLOCK_SOURCE_DDL: GucSetting<bool> = GucSetting::<bool>::new(true);

/// F46 (G9.3): Buffer growth alert threshold (number of pending change rows).
///
/// When any source table's change buffer exceeds this number of rows,
/// a `BufferGrowthWarning` alert is emitted. Configurable to accommodate
/// both high-throughput workloads (raise) and small tables (lower).
pub static PGS_BUFFER_ALERT_THRESHOLD: GucSetting<i32> = GucSetting::<i32>::new(1_000_000);

/// C-4: Change buffer compaction threshold (pending change row count).
///
/// When a source table's pending change buffer exceeds this many rows,
/// compaction is triggered before the next refresh cycle. Compaction
/// eliminates net-zero INSERT+DELETE pairs and collapses multi-change
/// groups to first+last rows per pk_hash.
///
/// Set to 0 to disable compaction. Typical values: 10_000–1_000_000.
pub static PGS_COMPACT_THRESHOLD: GucSetting<i32> = GucSetting::<i32>::new(100_000);

/// BUF-LIMIT: Hard limit on total change buffer rows per source table.
///
/// When a source table's change buffer exceeds this many rows at refresh
/// time, pg_trickle falls back to FULL refresh and truncates the buffer.
/// This prevents unbounded disk growth when differential refresh fails
/// repeatedly.
///
/// Set to 0 to disable the limit. Default: 1,000,000 rows.
pub static PGS_MAX_BUFFER_ROWS: GucSetting<i32> = GucSetting::<i32>::new(1_000_000);

/// AUTO-IDX: Automatic index creation on stream tables.
///
/// When enabled, `create_stream_table()` automatically creates indexes on
/// GROUP BY keys, DISTINCT columns, and adds INCLUDE clauses to the
/// `__pgt_row_id` index for stream tables with ≤ 8 output columns.
pub static PGS_AUTO_INDEX: GucSetting<bool> = GucSetting::<bool>::new(true);

/// B-1: Aggregate fast-path — use explicit DML instead of MERGE for
/// GROUP BY queries where all aggregates are algebraically invertible
/// (COUNT, SUM, AVG, etc.).  The explicit DML path (DELETE+UPDATE+INSERT)
/// avoids the MERGE hash-join cost, which is the dominant overhead for
/// aggregate stream tables with many groups.
pub static PGS_AGGREGATE_FAST_PATH: GucSetting<bool> = GucSetting::<bool>::new(true);

/// G14-SHC: Enable the cross-backend template cache backed by an UNLOGGED
/// catalog table (`pgtrickle.pgt_template_cache`).  When enabled, delta SQL
/// templates are persisted so that new backends avoid the ~45 ms DVM
/// parse+differentiate cost on their first refresh of each stream table.
pub static PGS_TEMPLATE_CACHE: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Maximum allowed grouping set branches for CUBE/ROLLUP expansion (EC-02).
pub static PGS_MAX_GROUPING_SET_BRANCHES: GucSetting<i32> = GucSetting::<i32>::new(64);

/// G13-SD: Maximum recursion depth for the query parser's tree visitors.
///
/// Prevents stack-overflow crashes on pathological queries with deeply
/// nested subqueries, CTEs, or set operations.  Returns
/// `PgTrickleError::QueryTooComplex` when the limit is exceeded.
pub static PGS_MAX_PARSE_DEPTH: GucSetting<i32> = GucSetting::<i32>::new(64);

/// Number of differential refresh cycles after which algebraic aggregate
/// stream tables are automatically reinitialized (full recompute) to reset
/// accumulated floating-point drift in auxiliary sum/sum2 columns.
///
/// Set to 0 to disable periodic drift reset (default).
/// Typical values: 100–1000, depending on workload precision requirements.
pub static PGS_ALGEBRAIC_DRIFT_RESET_CYCLES: GucSetting<i32> = GucSetting::<i32>::new(0);

/// P3-5: Automatic schedule backoff for falling-behind stream tables.
///
/// When enabled and a stream table's refresh duration exceeds 80% of its
/// schedule interval (the falling-behind threshold), the scheduler doubles
/// the effective interval on each consecutive falling-behind cycle. The
/// backoff factor resets to 1.0 on the first on-time cycle.
///
/// This prevents CPU runaway when a stream table's refresh cost exceeds
/// its schedule budget and an operator is not available to respond manually.
pub static PGS_AUTO_BACKOFF: GucSetting<bool> = GucSetting::<bool>::new(true);

/// P3-4: Delta-to-ST-size ratio below which `SET LOCAL enable_seqscan = off`
/// is applied before MERGE execution.
///
/// For small deltas against large stream tables, PostgreSQL's planner often
/// chooses a sequential scan of the stream table for the MERGE join on
/// `__pgt_row_id`, yielding O(n) full-table I/O when an index lookup would
/// be O(log n). When the delta row count is below this fraction of the
/// stream table's estimated row count, the seqscan is disabled.
///
/// Set to 0.0 to disable this optimization.
pub static PGS_MERGE_SEQSCAN_THRESHOLD: GucSetting<f64> = GucSetting::<f64>::new(0.001);

/// Maximum LIMIT value for TopK stream tables in IMMEDIATE mode.
///
/// TopK queries with `LIMIT > threshold` are rejected in IMMEDIATE mode
/// because inline recomputation of large result sets adds unacceptable
/// latency to the trigger path. Set to 0 to disable TopK in IMMEDIATE mode.
pub static PGS_IVM_TOPK_MAX_LIMIT: GucSetting<i32> = GucSetting::<i32>::new(1000);

/// Maximum recursion depth for `WITH RECURSIVE` CTEs in IMMEDIATE mode.
///
/// The semi-naive delta query generated for an IMMEDIATE-mode recursive
/// CTE includes a `__pgt_depth` counter.  Propagation stops when this
/// counter reaches the configured limit, preventing infinite loops caused
/// by cyclic data or deeply recursive hierarchies that would otherwise
/// exhaust PostgreSQL's `max_stack_depth` inside a trigger body.
///
/// Set to 0 to disable the depth guard (allow unlimited recursion).
/// The default (100) is sufficient for virtually all practical hierarchies.
pub static PGS_IVM_RECURSIVE_MAX_DEPTH: GucSetting<i32> = GucSetting::<i32>::new(100);

/// WAKE-1: Event-driven scheduler wake via LISTEN/NOTIFY.
///
/// When enabled, CDC triggers emit `pg_notify('pgtrickle_wake', '')` after
/// writing to the change buffer. The scheduler LISTENs on the channel and
/// wakes immediately instead of waiting for the full poll interval, reducing
/// median end-to-end latency from ~500 ms to ~15 ms for low-volume workloads.
///
/// Falls back to poll-based wake (using `scheduler_interval_ms`) when no
/// notifications arrive. Disable if the NOTIFY overhead is measurable on
/// extremely high-throughput write paths (> 100K DML/s).
// WAKE-1: PostgreSQL's LISTEN command is not allowed in background workers
// (MyBackendType != B_BACKEND — see async.c:Async_Listen()). The scheduler is
// always a background worker, so event-driven wake via LISTEN/NOTIFY cannot
// function as designed. Default is off until a background-worker-compatible
// wake mechanism is implemented (e.g., shared-memory latch signalling).
pub static PGS_EVENT_DRIVEN_WAKE: GucSetting<bool> = GucSetting::<bool>::new(false);

/// WAKE-1: Coalesce debounce interval in milliseconds.
///
/// After the scheduler receives the first `pgtrickle_wake` notification, it
/// waits this many milliseconds to coalesce rapidly arriving notifications
/// before starting a refresh tick. This avoids per-statement wake overhead
/// during bulk DML batches while preserving low single-statement latency.
pub static PGS_WAKE_DEBOUNCE_MS: GucSetting<i32> = GucSetting::<i32>::new(10);

/// Buffer table partitioning mode (Task 3.3).
///
/// Controls whether change buffer tables use `PARTITION BY RANGE (lsn)`:
/// - `"off"` (default): Unpartitioned heap tables (current behaviour).
/// - `"on"`: Always partition. After each refresh cycle, old partitions
///   are detached and dropped (O(1), no VACUUM needed).
/// - `"auto"`: Enable partitioning for sources whose effective refresh
///   schedule is >= 30 s (below that, DDL overhead exceeds VACUUM savings).
pub static PGS_BUFFER_PARTITIONING: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"off"));

/// Enable polling-based change detection for foreign tables (EC-05).
///
/// When enabled, foreign tables used in DIFFERENTIAL / IMMEDIATE mode
/// defining queries will be supported via a snapshot-comparison approach:
/// before each refresh cycle the scheduler materializes a snapshot of
/// the foreign table into a local shadow table, then computes EXCEPT ALL
/// deltas against the previous snapshot.
pub static PGS_FOREIGN_TABLE_POLLING: GucSetting<bool> = GucSetting::<bool>::new(false);

/// When `true`, materialized views referenced in DIFFERENTIAL/IMMEDIATE
/// defining queries will be supported via a snapshot-comparison approach
/// (same mechanism as foreign table polling).
pub static PGS_MATVIEW_POLLING: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Parallel refresh mode — controls whether the scheduler dispatches
/// refresh work to dynamic background workers.
///
/// - `"on"` (default as of v0.11.0): Enable true parallel refresh via
///   dynamic workers. The feature has been stable since v0.4.0.
/// - `"off"`: Sequential refresh (pre-v0.11.0 default).
/// - `"dry_run"`: Compute execution units and log dispatch decisions,
///   but execute inline (no actual workers spawned).
pub static PGS_PARALLEL_REFRESH_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"on"));

/// Cluster-wide cap on concurrently active pg_trickle dynamic refresh workers.
///
/// This is distinct from `pg_trickle.max_concurrent_refreshes`, which is the
/// per-database dispatch cap. This GUC prevents multiple database coordinators
/// from overcommitting the shared PostgreSQL `max_worker_processes` budget.
pub static PGS_MAX_DYNAMIC_REFRESH_WORKERS: GucSetting<i32> = GucSetting::<i32>::new(4);

/// CDC trigger granularity.
///
/// - `"statement"` (default): Use statement-level AFTER triggers with transition
///   tables (`NEW TABLE AS __pgt_new` / `OLD TABLE AS __pgt_old`). A single
///   trigger invocation per statement processes all affected rows via a bulk
///   `INSERT … SELECT FROM __pgt_new/old`, giving 50–80% less write-side
///   overhead for bulk DML. Zero change for single-row DML.
/// - `"row"`: Legacy per-row AFTER triggers — one trigger invocation and one
///   change-buffer INSERT per affected row. Equivalent to pg_trickle < 0.4.0.
///
/// Changing this GUC takes effect for newly created stream tables. To migrate
/// existing stream tables call `SELECT pgtrickle.rebuild_cdc_triggers()`.
pub static PGS_CDC_TRIGGER_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"statement"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdcTriggerMode {
    Statement,
    Row,
}

impl CdcTriggerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CdcTriggerMode::Statement => "statement",
            CdcTriggerMode::Row => "row",
        }
    }
}

fn normalize_cdc_trigger_mode(value: Option<String>) -> CdcTriggerMode {
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("row") => CdcTriggerMode::Row,
        _ => CdcTriggerMode::Statement,
    }
}

fn normalize_recursive_max_depth(value: i32) -> Option<i32> {
    if value > 0 { Some(value) } else { None }
}

/// CSS1: Cap CDC consumption to the WAL LSN captured at scheduler tick start.
///
/// When enabled (default), each scheduler tick calls `pg_current_wal_lsn()`
/// at its start to obtain a *tick watermark*. Every refresh within that tick
/// is prevented from consuming WAL changes beyond that watermark, ensuring
/// all stream tables in the same tick share the same consistent LSN view.
///
/// Disable only if you need stream tables to always advance to the very
/// latest available LSN regardless of cross-source consistency.
pub static PGS_TICK_WATERMARK_ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

/// CYC-4: Maximum iterations per SCC before declaring non-convergence.
///
/// When stream tables form a cyclic dependency (circular reference),
/// the scheduler iterates to a fixed point. If convergence is not
/// reached within this many iterations, all members of the SCC are
/// marked as ERROR.
pub static PGS_MAX_FIXPOINT_ITERATIONS: GucSetting<i32> = GucSetting::<i32>::new(100);

/// CYC-4: Master switch for circular dependency support.
///
/// When `false` (default), cycle detection rejects any stream table
/// creation that would introduce a cycle in the dependency graph.
/// When `true`, monotone cycles (those containing only safe operators)
/// are allowed and scheduled with fixed-point iteration.
pub static PGS_ALLOW_CIRCULAR: GucSetting<bool> = GucSetting::<bool>::new(false);

/// G-7: Enable tiered refresh scheduling (Hot/Warm/Cold/Frozen).
///
/// When enabled, per-ST `refresh_tier` controls the effective schedule
/// multiplier. Hot (1×), Warm (2×), Cold (10×), Frozen (skip entirely).
/// User-set via `ALTER STREAM TABLE ... SET (tier = 'warm')`.
/// Default tier for new STs is Hot (no change in behavior).
///
/// Default changed to `true` in v0.12.0 (PERF-3) — prevents large
/// deployments from wasting CPU refreshing cold STs at full speed.
pub static PGS_TIERED_SCHEDULING: GucSetting<bool> = GucSetting::<bool>::new(true);

/// QF-1: When `true`, the MERGE SQL template is emitted to the PostgreSQL
/// server log at `LOG` level on every refresh cycle.
///
/// Intended for debugging MERGE query generation only. **Do not enable in
/// production** — every refresh will emit potentially large SQL strings to
/// the server log.
pub static PGS_LOG_MERGE_SQL: GucSetting<bool> = GucSetting::<bool>::new(false);

/// FUSE-5: Global default change-count ceiling for the fuse circuit breaker.
///
/// When a stream table's fuse_mode is 'on' or 'auto' and no per-ST
/// `fuse_ceiling` is configured, this global ceiling is used. If the total
/// pending change buffer rows across all sources of an ST exceed this value,
/// the fuse blows and the ST is suspended.
///
/// Set to 0 to disable the global default ceiling (per-ST ceiling only).
pub static PGS_FUSE_DEFAULT_CEILING: GucSetting<i32> = GucSetting::<i32>::new(0);

/// DAG-3: Delta amplification detection threshold.
///
/// When a DIFFERENTIAL refresh produces `output_delta / input_delta` rows
/// exceeding this ratio, pg_trickle emits a WARNING indicating pathological
/// delta amplification (common with many-to-many joins or large fan-out).
/// The warning includes the stream table name, input/output counts, and
/// the computed ratio, helping operators identify and tune problematic hops.
///
/// Set to 0.0 to disable amplification detection.
pub static PGS_DELTA_AMPLIFICATION_THRESHOLD: GucSetting<f64> = GucSetting::<f64>::new(100.0);

/// DIAG-2: Estimated GROUP BY cardinality threshold for algebraic aggregate
/// DIFFERENTIAL mode warning.
///
/// At `create_stream_table` time, if the defining query uses algebraic
/// aggregates (SUM, COUNT, AVG) in DIFFERENTIAL mode and the estimated
/// group cardinality (from `pg_stats.n_distinct`) is below this threshold,
/// a WARNING is emitted suggesting FULL or AUTO mode instead.
///
/// Low-cardinality GROUP BY columns make DIFFERENTIAL aggregates maintain
/// auxiliary columns for very few groups, which may not justify the overhead.
///
/// Set to 0 to disable the cardinality warning.
pub static PGS_AGG_DIFF_CARDINALITY_THRESHOLD: GucSetting<i32> = GucSetting::<i32>::new(1000);

/// C3-1: Per-database dynamic refresh worker quota.
///
/// When > 0, each per-database scheduler limits itself to this many
/// concurrently active dynamic refresh workers drawn from the cluster-wide
/// `max_dynamic_refresh_workers` pool. This prevents a single busy database
/// from starving other databases in multi-tenant clusters.
///
/// **Burst capacity:** when the cluster has spare capacity (active workers
/// < 80% of `max_dynamic_refresh_workers`), a database may temporarily
/// exceed its quota by up to 50% to absorb sudden backlogs. Burst is
/// reclaimed automatically within 1 scheduler cycle once global load rises.
///
/// **Priority dispatch:** within each dispatch tick, IMMEDIATE-trigger
/// closures are dispatched before other units, followed by atomic groups,
/// singletons, and cyclic SCCs — ensuring transactional consistency
/// requirements are always satisfied first.
///
/// Set to 0 (default) to disable per-database quotas — all databases share
/// `max_dynamic_refresh_workers` on a first-come-first-served basis,
/// bounded per coordinator by `max_concurrent_refreshes`.
pub static PGS_PER_DATABASE_WORKER_QUOTA: GucSetting<i32> = GucSetting::<i32>::new(0);

/// VOL-1: Volatile function policy for DIFFERENTIAL/IMMEDIATE mode.
///
/// Controls how volatile functions in defining queries are handled:
/// - `"reject"` (default): Error — volatile functions are rejected.
/// - `"warn"`: Allow creation with a WARNING.
/// - `"allow"`: Allow silently.
pub static PGS_VOLATILE_FUNCTION_POLICY: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"reject"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolatileFunctionPolicy {
    Reject,
    Warn,
    Allow,
}

impl VolatileFunctionPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            VolatileFunctionPolicy::Reject => "reject",
            VolatileFunctionPolicy::Warn => "warn",
            VolatileFunctionPolicy::Allow => "allow",
        }
    }
}

fn normalize_volatile_function_policy(value: Option<String>) -> VolatileFunctionPolicy {
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("warn") => VolatileFunctionPolicy::Warn,
        Some("allow") => VolatileFunctionPolicy::Allow,
        _ => VolatileFunctionPolicy::Reject,
    }
}

/// PH-D2: Merge join strategy override.
///
/// Controls the join strategy hint applied via `SET LOCAL` during MERGE:
/// - `"auto"` (default): delta-size heuristics choose the strategy.
/// - `"hash_join"`: always prefer hash joins (disable nestloop, raise work_mem).
/// - `"nested_loop"`: always prefer nested loops (disable hashjoin + mergejoin).
/// - `"merge_join"`: always prefer merge joins (disable hashjoin + nestloop).
pub static PGS_MERGE_JOIN_STRATEGY: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"auto"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeJoinStrategy {
    /// Delta-size heuristics (existing behaviour).
    Auto,
    /// Force hash joins.
    HashJoin,
    /// Force nested-loop joins.
    NestedLoop,
    /// Force merge joins.
    MergeJoin,
}

impl MergeJoinStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeJoinStrategy::Auto => "auto",
            MergeJoinStrategy::HashJoin => "hash_join",
            MergeJoinStrategy::NestedLoop => "nested_loop",
            MergeJoinStrategy::MergeJoin => "merge_join",
        }
    }
}

fn normalize_merge_join_strategy(value: Option<String>) -> MergeJoinStrategy {
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("hash_join") => MergeJoinStrategy::HashJoin,
        Some("nested_loop") => MergeJoinStrategy::NestedLoop,
        Some("merge_join") => MergeJoinStrategy::MergeJoin,
        _ => MergeJoinStrategy::Auto,
    }
}

/// D-1a: Create new change buffer tables as UNLOGGED.
///
/// When `true`, newly created change buffer tables (`pgtrickle_changes.changes_*`)
/// are created with `CREATE UNLOGGED TABLE` instead of `CREATE TABLE`. This
/// eliminates WAL writes for trigger-inserted CDC rows, reducing WAL
/// amplification by ~30%.
///
/// **Trade-off:** UNLOGGED tables are truncated on crash recovery and are
/// not replicated to standbys. After a crash or standby restart, affected
/// stream tables will automatically receive a FULL refresh on the next
/// scheduler cycle to resynchronize.
///
/// Existing change buffer tables are not retroactively altered. Use
/// `pgtrickle.convert_buffers_to_unlogged()` to convert existing buffers.
///
/// Default `false` — change buffers remain WAL-logged and crash-safe.
pub static PGS_UNLOGGED_BUFFERS: GucSetting<bool> = GucSetting::<bool>::new(false);

/// PH-D1: MERGE strategy override.
///
/// Controls how differential refresh applies deltas to stream tables:
/// - `"auto"` (default): use DELETE+INSERT when `delta_rows / target_rows`
///   is below `merge_strategy_threshold`; MERGE otherwise.
/// - `"merge"`: always use the MERGE statement.
/// - `"delete_insert"`: always use DELETE + INSERT (two separate statements).
pub static PGS_MERGE_STRATEGY: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"auto"));

/// PH-D1: Delta ratio threshold for the `auto` merge strategy.
///
/// When `merge_strategy = 'auto'`, DELETE+INSERT is used instead of MERGE
/// when `delta_rows / target_rows < merge_strategy_threshold`. This avoids
/// the MERGE join cost for sub-1% deltas against large tables.
///
/// Default: 0.01 (1%).
pub static PGS_MERGE_STRATEGY_THRESHOLD: GucSetting<f64> = GucSetting::<f64>::new(0.01);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Heuristic: DELETE+INSERT for small deltas, MERGE otherwise.
    Auto,
    /// Always MERGE.
    Merge,
    /// Always DELETE + INSERT.
    DeleteInsert,
}

impl MergeStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeStrategy::Auto => "auto",
            MergeStrategy::Merge => "merge",
            MergeStrategy::DeleteInsert => "delete_insert",
        }
    }
}

fn normalize_merge_strategy(value: Option<String>) -> MergeStrategy {
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("merge") => MergeStrategy::Merge,
        Some("delete_insert") => MergeStrategy::DeleteInsert,
        _ => MergeStrategy::Auto,
    }
}

/// B-4: Refresh strategy override.
///
/// Controls the FULL vs. DIFFERENTIAL decision for all stream tables:
/// - `"auto"` (default): Use the adaptive cost-based heuristic that
///   considers `differential_max_change_ratio`, per-ST `auto_threshold`,
///   refresh history, and spill detection to pick the optimal strategy.
/// - `"differential"`: Always use DIFFERENTIAL refresh (skip the adaptive
///   threshold check). Useful when operators know their workload has low
///   change rates and want to avoid any overhead from the ratio check.
/// - `"full"`: Always use FULL refresh. Useful for debugging or when
///   differential refresh is known to be slower for a specific workload.
///
/// This GUC is a cluster-wide override. Per-ST `refresh_mode` in the
/// catalog takes precedence: if a stream table is configured as
/// `refresh_mode = 'FULL'`, it will always use FULL regardless of this GUC.
/// This GUC only affects stream tables with `refresh_mode = 'DIFFERENTIAL'`.
pub static PGS_REFRESH_STRATEGY: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"auto"));

/// B-4: Cost-model safety margin for the FULL vs. DIFFERENTIAL decision.
///
/// When `refresh_strategy = 'auto'`, the cost model compares the estimated
/// DIFFERENTIAL cost against `estimated_full_cost × safety_margin`.
/// A value below 1.0 biases toward DIFFERENTIAL (which has lower lock
/// contention), while a value above 1.0 biases toward FULL.
///
/// Default 0.8 — DIFFERENTIAL is chosen unless it's estimated to cost
/// more than 80% of FULL.
pub static PGS_COST_MODEL_SAFETY_MARGIN: GucSetting<f64> = GucSetting::<f64>::new(0.8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStrategy {
    /// Adaptive cost-based heuristic (existing behavior).
    Auto,
    /// Always use DIFFERENTIAL (skip adaptive fallback to FULL).
    Differential,
    /// Always fall back to FULL refresh.
    Full,
}

impl RefreshStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            RefreshStrategy::Auto => "auto",
            RefreshStrategy::Differential => "differential",
            RefreshStrategy::Full => "full",
        }
    }
}

fn normalize_refresh_strategy(value: Option<String>) -> RefreshStrategy {
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("differential") => RefreshStrategy::Differential,
        Some("full") => RefreshStrategy::Full,
        _ => RefreshStrategy::Auto,
    }
}

/// Register all GUC variables. Called from `_PG_init()`.
pub fn register_gucs() {
    GucRegistry::define_bool_guc(
        c"pg_trickle.enabled",
        c"Master enable/disable switch for pgtrickle.",
        c"When false, the scheduler will not run and no refreshes will be triggered.",
        &PGS_ENABLED,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.scheduler_interval_ms",
        c"Scheduler wake interval in milliseconds.",
        c"Controls how frequently the background scheduler checks for STs that need refresh.",
        &PGS_SCHEDULER_INTERVAL_MS,
        100,     // min
        600_000, // max (DI-9: raised from 60s to 600s for long-running benchmarks)
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.min_schedule_seconds",
        c"Minimum allowed schedule in seconds.",
        c"Stream tables cannot specify a schedule smaller than this value.",
        &PGS_MIN_SCHEDULE_SECONDS,
        1,      // min
        86_400, // max (1 day)
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.default_schedule_seconds",
        c"Default effective schedule (seconds) for isolated CALCULATED stream tables.",
        c"When a CALCULATED stream table has no downstream dependents, this value \
           is used as its effective refresh interval. Distinct from min_schedule_seconds \
           which is the validation floor for duration-based schedules.",
        &PGS_DEFAULT_SCHEDULE_SECONDS,
        1,      // min
        86_400, // max (1 day)
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.max_consecutive_errors",
        c"Maximum consecutive errors before auto-suspend.",
        c"After this many consecutive refresh failures, the stream table is automatically suspended.",
        &PGS_MAX_CONSECUTIVE_ERRORS,
        1,    // min
        100,  // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_trickle.change_buffer_schema",
        c"Schema name for change buffer tables.",
        c"CDC change data is stored in tables within this schema.",
        &PGS_CHANGE_BUFFER_SCHEMA,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.max_concurrent_refreshes",
        c"Maximum active refresh workers per database coordinator.",
        c"Limits the number of concurrent refresh operations within a single database. \
           In sequential mode (parallel_refresh_mode=off) this has no effect. \
           In parallel mode, the coordinator will not dispatch more than this many \
           concurrent refresh workers for one database.",
        &PGS_MAX_CONCURRENT_REFRESHES,
        1,  // min
        32, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_float_guc(
        c"pg_trickle.differential_max_change_ratio",
        c"Max change ratio before falling back to FULL refresh.",
        c"When pending changes exceed this fraction of the source table size, DIFFERENTIAL refresh falls back to FULL. Set to 0.0 to disable.",
        &PGS_DIFFERENTIAL_MAX_CHANGE_RATIO,
        0.0,  // min
        1.0,  // max
        GucContext::Suset,
        GucFlags::default(),
    );

    // B-4: Refresh strategy override.
    GucRegistry::define_string_guc(
        c"pg_trickle.refresh_strategy",
        c"Refresh strategy override: auto, differential, or full.",
        c"Controls the FULL vs. DIFFERENTIAL decision for all stream tables. \
           'auto' (default) uses the adaptive cost-based heuristic. \
           'differential' always uses DIFFERENTIAL (skips ratio check). \
           'full' always uses FULL refresh. Per-ST refresh_mode takes precedence.",
        &PGS_REFRESH_STRATEGY,
        GucContext::Suset,
        GucFlags::default(),
    );

    // B-4: Cost-model safety margin.
    GucRegistry::define_float_guc(
        c"pg_trickle.cost_model_safety_margin",
        c"Safety margin for the cost-model FULL vs DIFFERENTIAL decision.",
        c"When refresh_strategy = 'auto', DIFFERENTIAL is chosen unless its \
           estimated cost exceeds estimated_full_cost × this margin. Values \
           below 1.0 bias toward DIFFERENTIAL (lower lock contention). \
           Default 0.8.",
        &PGS_COST_MODEL_SAFETY_MARGIN,
        0.1, // min
        2.0, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    // PH-E1: Delta estimated output cardinality threshold.
    GucRegistry::define_int_guc(
        c"pg_trickle.max_delta_estimate_rows",
        c"Max estimated delta output rows before falling back to FULL (0 = disabled).",
        c"Before executing the MERGE, runs a capped COUNT on the delta subquery. \
           If the count reaches this limit, the refresh downgrades to FULL with a NOTICE \
           to prevent OOM or excessive temp-file spills from unexpectedly large deltas. \
           Set to 0 to disable the estimation check. Recommended: 50000–500000.",
        &PGS_MAX_DELTA_ESTIMATE_ROWS,
        0,          // min (0 = disabled)
        10_000_000, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    // WM-7: Watermark holdback timeout — seconds before a watermark is "stuck".
    GucRegistry::define_int_guc(
        c"pg_trickle.watermark_holdback_timeout",
        c"Seconds before an un-advanced watermark is considered stuck (0 = disabled).",
        c"When non-zero, the scheduler periodically checks all watermark sources. \
           If any source in a watermark group has not advanced within this many seconds, \
           downstream stream tables in that group are paused and a pgtrickle_alert \
           notification with category watermark_stuck is emitted. Set to 0 to disable.",
        &PGS_WATERMARK_HOLDBACK_TIMEOUT,
        0,      // min (0 = disabled)
        86_400, // max (24 hours)
        GucContext::Suset,
        GucFlags::default(),
    );

    // PH-E2: Spill detection threshold.
    GucRegistry::define_int_guc(
        c"pg_trickle.spill_threshold_blocks",
        c"Temp blocks written threshold for spill detection (0 = disabled).",
        c"After each differential MERGE, queries pg_stat_statements for temp_blks_written. \
           If the value exceeds this threshold, the refresh is a spill. After \
           spill_consecutive_limit consecutive spills, forces FULL refresh. \
           Requires pg_stat_statements. Set to 0 to disable.",
        &PGS_SPILL_THRESHOLD_BLOCKS,
        0,           // min (0 = disabled)
        100_000_000, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    // PH-E2: Consecutive spill limit before FULL fallback.
    GucRegistry::define_int_guc(
        c"pg_trickle.spill_consecutive_limit",
        c"Consecutive spilling refreshes before auto-switching to FULL (default 3).",
        c"When a stream table has this many consecutive differential refreshes with \
           temp_blks_written exceeding spill_threshold_blocks, the scheduler forces \
           a FULL refresh on the next cycle. Resets after any non-spilling refresh.",
        &PGS_SPILL_CONSECUTIVE_LIMIT,
        1,   // min
        100, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.cleanup_use_truncate",
        c"Use TRUNCATE for change buffer cleanup when all rows are consumed.",
        c"When true and the entire change buffer is consumed by a refresh, uses TRUNCATE (O(1)) instead of per-row DELETE. Disable if the AccessExclusiveLock is problematic.",
        &PGS_CLEANUP_USE_TRUNCATE,
        GucContext::Suset,
        GucFlags::default(),
    );

    // C4: Consolidated planner aggressiveness switch (v0.14.0).
    GucRegistry::define_bool_guc(
        c"pg_trickle.planner_aggressive",
        c"Enable all planner hints for MERGE execution (consolidates merge_planner_hints + merge_work_mem_mb).",
        c"When true (default), disables nested-loop joins and raises work_mem for medium/large \
           delta sizes to stabilise P95 latency. Replaces the deprecated merge_planner_hints \
           and merge_work_mem_mb GUCs.",
        &PGS_PLANNER_AGGRESSIVE,
        GucContext::Suset,
        GucFlags::default(),
    );

    // Deprecated: kept for backward compatibility.
    GucRegistry::define_bool_guc(
        c"pg_trickle.merge_planner_hints",
        c"Deprecated — use pg_trickle.planner_aggressive instead.",
        c"Deprecated in v0.14.0. When explicitly set, emits a deprecation warning. \
           Use pg_trickle.planner_aggressive instead.",
        &PGS_MERGE_PLANNER_HINTS,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.merge_work_mem_mb",
        c"work_mem (MB) for large-delta MERGE execution.",
        c"Applied via SET LOCAL when planner_aggressive is enabled and the delta exceeds 10 000 rows.",
        &PGS_MERGE_WORK_MEM_MB,
        8,    // min
        4096, // max (4 GB)
        GucContext::Suset,
        GucFlags::default(),
    );

    // SCAL-3: Delta working-set memory cap.
    GucRegistry::define_int_guc(
        c"pg_trickle.delta_work_mem_cap_mb",
        c"Max work_mem (MB) allowed during delta MERGE (0 = no cap).",
        c"When the planner hints would set work_mem above this cap, the refresh \
           falls back to FULL instead of executing a potentially OOM-inducing delta \
           MERGE. Set to 0 to disable. Recommended: 128–1024.",
        &PGS_DELTA_WORK_MEM_CAP_MB,
        0,    // min (0 = disabled)
        8192, // max (8 GB)
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.use_prepared_statements",
        c"Use SQL PREPARE/EXECUTE for MERGE during differential refresh.",
        c"When true, the first cache-hit cycle PREPAREs the MERGE statement and subsequent cycles EXECUTE it. Saves 1-2ms of parse/plan overhead. Disable if plan-parameter sniffing causes poor plans.",
        &PGS_USE_PREPARED_STATEMENTS,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_trickle.user_triggers",
          c"User-trigger handling: auto or off.",
          c"'auto' detects row-level user triggers and switches to explicit DML so they fire correctly. \
              'off' always uses MERGE (triggers will NOT fire correctly). \
              'on' is accepted as a deprecated alias for 'auto'.",
        &PGS_USER_TRIGGERS,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_trickle.cdc_mode",
        c"CDC mechanism: auto (default), trigger, or wal.",
        c"'auto' (default) uses triggers initially and transitions to WAL-based CDC \
           if wal_level=logical, falling back to triggers on error. \
           'trigger' always uses row-level triggers for change capture. \
           'wal' requires wal_level=logical (fails otherwise).",
        &PGS_CDC_MODE,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.wal_transition_timeout",
        c"Max seconds for WAL decoder catch-up during CDC transition.",
        c"When transitioning from trigger-based to WAL-based CDC, the WAL decoder must catch up \
           past the trigger's last captured LSN. If it hasn't caught up within this timeout, \
           the system falls back to trigger-based CDC.",
        &PGS_WAL_TRANSITION_TIMEOUT,
        10,    // min: 10 seconds
        3_600, // max: 1 hour
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.slot_lag_warning_threshold_mb",
        c"WAL slot lag warning threshold in MB.",
        c"When a pg_trickle WAL replication slot retains more than this much WAL, \
           the scheduler emits a slot_lag_warning NOTIFY event and pgtrickle.health_check() \
           reports WARN for slot_lag.",
        &PGS_SLOT_LAG_WARNING_THRESHOLD_MB,
        1,
        1_048_576,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.slot_lag_critical_threshold_mb",
        c"WAL slot lag critical threshold in MB.",
        c"When a pg_trickle WAL replication slot retains more than this much WAL, \
           pgtrickle.check_cdc_health() reports slot_lag_exceeds_threshold for the source.",
        &PGS_SLOT_LAG_CRITICAL_THRESHOLD_MB,
        1,
        1_048_576,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.block_source_ddl",
        c"Block column-altering DDL on source tables used by stream tables.",
        c"When true (default), ALTER TABLE that adds, drops, renames, or changes the type \
           of a column on a source table will ERROR instead of triggering reinitialization. \
           Benign DDL (indexes, comments, statistics) and constraint changes are always allowed. \
           Set to false to allow schema changes (the stream table will be reinitialized on the \
           next scheduler tick). Use ALTER STREAM TABLE to update the query before re-enabling.",
        &PGS_BLOCK_SOURCE_DDL,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.buffer_alert_threshold",
        c"Buffer growth alert threshold (pending change row count).",
        c"When a source table's change buffer exceeds this many rows, a BufferGrowthWarning \
           alert is emitted. Raise for high-throughput workloads, lower for small tables.",
        &PGS_BUFFER_ALERT_THRESHOLD,
        1_000,       // min: 1000 rows
        100_000_000, // max: 100M rows
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.compact_threshold",
        c"Change buffer compaction threshold (pending change row count).",
        c"When a source table's pending changes exceed this count, compaction removes \
           net-zero INSERT+DELETE pairs and collapses multi-change groups. Set to 0 to disable.",
        &PGS_COMPACT_THRESHOLD,
        0,           // min: 0 (disabled)
        100_000_000, // max: 100M rows
        GucContext::Suset,
        GucFlags::default(),
    );

    // BUF-LIMIT: Hard limit on change buffer rows per source table.
    GucRegistry::define_int_guc(
        c"pg_trickle.max_buffer_rows",
        c"Hard limit on change buffer rows per source table (0 = unlimited).",
        c"When a source table's change buffer exceeds this many rows at refresh time, \
           pg_trickle falls back to FULL refresh and truncates the buffer. Prevents \
           unbounded disk growth when differential refresh fails repeatedly.",
        &PGS_MAX_BUFFER_ROWS,
        0,           // min: 0 (disabled)
        100_000_000, // max: 100M rows
        GucContext::Suset,
        GucFlags::default(),
    );

    // AUTO-IDX: Automatic index creation on stream tables.
    GucRegistry::define_bool_guc(
        c"pg_trickle.auto_index",
        c"Automatically create indexes on stream tables at creation time.",
        c"When true (default), create_stream_table() auto-creates indexes on GROUP BY keys, \
           DISTINCT columns, and adds INCLUDE clauses to the __pgt_row_id index for small \
           stream tables. Set to false to manage indexes manually.",
        &PGS_AUTO_INDEX,
        GucContext::Suset,
        GucFlags::default(),
    );

    // B-1: Aggregate fast-path.
    GucRegistry::define_bool_guc(
        c"pg_trickle.aggregate_fast_path",
        c"Use explicit DML instead of MERGE for all-algebraic aggregate stream tables.",
        c"When true (default), stream tables whose aggregates are all algebraically invertible \
           (COUNT, SUM, AVG, STDDEV, etc.) use the targeted DELETE+UPDATE+INSERT path instead \
           of MERGE, avoiding the hash-join cost. Set to false to force MERGE for all stream \
           tables.",
        &PGS_AGGREGATE_FAST_PATH,
        GucContext::Suset,
        GucFlags::default(),
    );

    // G14-SHC: Cross-backend template cache.
    GucRegistry::define_bool_guc(
        c"pg_trickle.template_cache",
        c"Enable the cross-backend delta template cache.",
        c"When true (default), delta SQL templates are persisted in an UNLOGGED catalog table \
           so that new backends skip the ~45 ms DVM parse+differentiate step. \
           Set to false to always regenerate templates from scratch.",
        &PGS_TEMPLATE_CACHE,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.max_grouping_set_branches",
        c"Maximum allowed grouping set branches in CUBE/ROLLUP queries.",
        c"Prevents parsing memory exhaustion during combinatorial expansion. \
           Raise if you need more than 64 grouping set branches.",
        &PGS_MAX_GROUPING_SET_BRANCHES,
        1,
        65536,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.max_parse_depth",
        c"Maximum recursion depth for the query parser tree visitors.",
        c"Prevents stack-overflow crashes on pathological queries with deeply \
           nested subqueries, CTEs, or set operations. Returns a QueryTooComplex \
           error when the limit is exceeded. Raise only if legitimate queries \
           exceed the default.",
        &PGS_MAX_PARSE_DEPTH,
        1,
        10000,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.ivm_topk_max_limit",
        c"Maximum LIMIT for TopK stream tables in IMMEDIATE mode.",
        c"TopK queries exceeding this LIMIT are rejected in IMMEDIATE mode. \
           Set to 0 to disable TopK in IMMEDIATE mode entirely.",
        &PGS_IVM_TOPK_MAX_LIMIT,
        0,
        1_000_000,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.ivm_recursive_max_depth",
        c"Maximum recursion depth for WITH RECURSIVE CTEs in IMMEDIATE mode.",
        c"Limits the depth counter injected into semi-naive delta queries to guard \
           against infinite loops from cyclic data or very deep hierarchies inside \
           trigger bodies. Set to 0 to disable the guard (allow unlimited recursion).",
        &PGS_IVM_RECURSIVE_MAX_DEPTH,
        0,
        100_000,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_trickle.buffer_partitioning",
        c"Buffer table partitioning mode: off, on, or auto.",
        c"'off' uses unpartitioned heap tables (default). \
           'on' always uses PARTITION BY RANGE (lsn) for change buffers. \
           'auto' enables partitioning for sources with refresh cycles >= 30s.",
        &PGS_BUFFER_PARTITIONING,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.foreign_table_polling",
        c"Enable polling-based CDC for foreign tables.",
        c"When true, foreign tables in defining queries are supported via \
           snapshot-comparison. A local shadow table stores the previous state; \
           EXCEPT ALL computes the delta on each refresh cycle.",
        &PGS_FOREIGN_TABLE_POLLING,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.matview_polling",
        c"Enable polling-based CDC for materialized views.",
        c"When true, materialized views in defining queries are supported via \
           snapshot-comparison (same mechanism as foreign table polling). \
           A local shadow table stores the previous state; EXCEPT ALL computes \
           the delta on each refresh cycle.",
        &PGS_MATVIEW_POLLING,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_trickle.parallel_refresh_mode",
        c"Parallel refresh mode: on (default), dry_run, or off.",
        c"'on' (default): enable true parallel refresh via dynamic background workers. \
           'dry_run': compute execution units and log dispatch decisions but execute inline. \
           'off': sequential refresh (pre-v0.11.0 default).",
        &PGS_PARALLEL_REFRESH_MODE,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.max_dynamic_refresh_workers",
        c"Cluster-wide cap on pg_trickle dynamic refresh workers.",
        c"Limits the total number of concurrently active pg_trickle refresh workers \
           across all databases. Prevents overcommitting max_worker_processes.",
        &PGS_MAX_DYNAMIC_REFRESH_WORKERS,
        1,  // min
        64, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_trickle.cdc_trigger_mode",
        c"CDC trigger granularity: statement (default) or row.",
        c"'statement' uses statement-level AFTER triggers with transition tables \
           (NEW TABLE / OLD TABLE). A single invocation per DML statement processes \
           all affected rows in one bulk INSERT … SELECT, giving 50–80% less \
           write-side overhead for bulk UPDATE/DELETE. Single-row DML is unaffected. \
           'row' uses legacy per-row triggers (pg_trickle < 0.4.0 behaviour). \
           Changing this setting takes effect for newly installed CDC triggers. \
           Call pgtrickle.rebuild_cdc_triggers() to migrate existing stream tables.",
        &PGS_CDC_TRIGGER_MODE,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.tick_watermark_enabled",
        c"Cap CDC consumption to the WAL LSN at scheduler tick start.",
        c"When on (default), each scheduler tick captures pg_current_wal_lsn() at its \
           start and prevents any refresh from consuming WAL changes beyond that LSN. \
           This bounds cross-source staleness without requiring user configuration. \
           Disable only if you need STs to always advance to the latest available LSN.",
        &PGS_TICK_WATERMARK_ENABLED,
        GucContext::Suset,
        GucFlags::default(),
    );

    // CYC-4: Circular dependency GUCs.
    GucRegistry::define_int_guc(
        c"pg_trickle.max_fixpoint_iterations",
        c"Maximum iterations per SCC before declaring non-convergence.",
        c"When circular stream table dependencies are iterated to a fixed point, \
           this limits the maximum number of iterations. If convergence is not \
           reached within this limit, all members of the SCC are marked ERROR. \
           Only meaningful when pg_trickle.allow_circular = true.",
        &PGS_MAX_FIXPOINT_ITERATIONS,
        1,      // min
        10_000, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.allow_circular",
        c"Allow circular (cyclic) stream table dependencies.",
        c"When false (default), creating a stream table that would introduce a cycle \
           in the dependency graph is rejected. When true, monotone cycles \
           (containing only safe operators like joins, filters, and projections) \
           are allowed and refreshed via fixed-point iteration.",
        &PGS_ALLOW_CIRCULAR,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.algebraic_drift_reset_cycles",
        c"Differential cycles between automatic full recomputes for algebraic aggregates.",
        c"After this many differential refresh cycles, stream tables with algebraic \
           aggregates (AVG, STDDEV, VAR) are automatically reinitialized to reset \
           accumulated floating-point drift in auxiliary columns. 0 disables.",
        &PGS_ALGEBRAIC_DRIFT_RESET_CYCLES,
        0,       // min (disabled)
        100_000, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.auto_backoff",
        c"Automatically back off schedule for falling-behind stream tables (default on).",
        c"When enabled (the default), the scheduler doubles the effective interval \
           when a refresh takes more than 95% of the schedule window, capped at 8x. \
           Emits a WARNING when the factor changes. Resets on the first on-time cycle.",
        &PGS_AUTO_BACKOFF,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_float_guc(
        c"pg_trickle.merge_seqscan_threshold",
        c"Delta-to-ST ratio below which sequential scans are disabled for MERGE.",
        c"When the delta row count is below this fraction of the stream table size, \
           SET LOCAL enable_seqscan = off is applied before MERGE to favor index \
           lookups. Set to 0.0 to disable.",
        &PGS_MERGE_SEQSCAN_THRESHOLD,
        0.0, // min (disabled)
        1.0, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.tiered_scheduling",
        c"Enable tiered refresh scheduling (Hot/Warm/Cold/Frozen).",
        c"When enabled, per-ST refresh_tier controls the effective schedule \
           multiplier. Hot refreshes at configured interval, Warm at 2x, \
           Cold at 10x, Frozen skips entirely. Set per-ST tier via \
           ALTER STREAM TABLE ... SET (tier = 'warm'). Default is on \
           (changed in v0.12.0; set to off to restore pre-v0.12.0 behavior).",
        &PGS_TIERED_SCHEDULING,
        GucContext::Suset,
        GucFlags::default(),
    );

    // WAKE-1: Event-driven scheduler wake GUCs.
    GucRegistry::define_bool_guc(
        c"pg_trickle.event_driven_wake",
        c"Enable event-driven scheduler wake via LISTEN/NOTIFY (default off; not yet functional in background workers).",
        c"Reserved for future use. PostgreSQL LISTEN is not allowed in background \
           worker processes (MyBackendType != B_BACKEND), so enabling this has no \
           effect — the scheduler operates in polling-only mode regardless. \
           CDC triggers still emit pg_notify('pgtrickle_wake') for future use.",
        &PGS_EVENT_DRIVEN_WAKE,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.wake_debounce_ms",
        c"Coalesce debounce interval (ms) after first NOTIFY wake.",
        c"After the first pgtrickle_wake notification, the scheduler waits this \
           many milliseconds to coalesce rapidly arriving notifications before \
           starting a refresh tick. Lower values reduce latency; higher values \
           reduce wake overhead during bulk DML.",
        &PGS_WAKE_DEBOUNCE_MS,
        1,     // min
        5_000, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.log_merge_sql",
        c"Log the generated MERGE SQL template on every refresh cycle.",
        c"When true, the MERGE SQL template built during differential refresh is \
           emitted to the PostgreSQL server log at LOG level. Intended for debugging \
           MERGE query generation only. Do not enable in production.",
        &PGS_LOG_MERGE_SQL,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.fuse_default_ceiling",
        c"Global default change-count ceiling for the fuse circuit breaker.",
        c"When a stream table has fuse_mode='on' or 'auto' and no per-ST fuse_ceiling, \
           this value is used. If pending changes exceed this count, the fuse blows \
           and the ST is suspended. Set to 0 to disable the global default.",
        &PGS_FUSE_DEFAULT_CEILING,
        0,             // min (disabled)
        2_000_000_000, // max (~2B rows)
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_float_guc(
        c"pg_trickle.delta_amplification_threshold",
        c"Delta amplification detection threshold (output/input ratio).",
        c"When a DIFFERENTIAL refresh produces more than this multiple of the input \
           delta rows, a WARNING is emitted so operators can identify pathological \
           join fan-out or many-to-many amplification. Set to 0.0 to disable.",
        &PGS_DELTA_AMPLIFICATION_THRESHOLD,
        0.0,       // min (disabled)
        100_000.0, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    // DIAG-2: Aggregate cardinality warning threshold.
    GucRegistry::define_int_guc(
        c"pg_trickle.agg_diff_cardinality_threshold",
        c"Estimated GROUP BY cardinality threshold for algebraic aggregate warnings.",
        c"At create_stream_table time, if the defining query uses algebraic aggregates \
           (SUM, COUNT, AVG) in DIFFERENTIAL mode and the estimated group cardinality \
           is below this threshold, a WARNING is emitted suggesting FULL or AUTO mode. \
           Set to 0 to disable.",
        &PGS_AGG_DIFF_CARDINALITY_THRESHOLD,
        0,           // min (disabled)
        100_000_000, // max
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.per_database_worker_quota",
        c"Per-database dynamic refresh worker quota for multi-tenant isolation.",
        c"When > 0, limits each database's concurrent refresh workers to this count \
           from the shared cluster budget (max_dynamic_refresh_workers). Prevents one \
           busy database from starving others. Burst to 150% allowed when cluster has \
           spare capacity (active workers < 80% of max_dynamic_refresh_workers). \
           0 (default) disables per-DB quotas (first-come-first-served from pool). \
           Within each tick, IMMEDIATE closures are dispatched before other units.",
        &PGS_PER_DATABASE_WORKER_QUOTA,
        0,  // min: 0 (disabled)
        64, // max: matches max_dynamic_refresh_workers ceiling
        GucContext::Suset,
        GucFlags::default(),
    );

    // VOL-1: Volatile function policy.
    GucRegistry::define_string_guc(
        c"pg_trickle.volatile_function_policy",
        c"Volatile function policy: reject (default), warn, or allow.",
        c"'reject' (default) errors on volatile functions in DIFFERENTIAL/IMMEDIATE queries. \
           'warn' emits a WARNING but allows creation. \
           'allow' permits volatile functions silently. Volatile functions produce different \
           values on each evaluation, which may break delta computation.",
        &PGS_VOLATILE_FUNCTION_POLICY,
        GucContext::Suset,
        GucFlags::default(),
    );

    // PH-D2: Merge join strategy override.
    GucRegistry::define_string_guc(
        c"pg_trickle.merge_join_strategy",
        c"Join strategy hint for MERGE: auto (default), hash_join, nested_loop, merge_join.",
        c"'auto' (default) uses delta-size heuristics to choose between nested-loop and \
           hash-join hints. 'hash_join' always disables nestloop and raises work_mem. \
           'nested_loop' always disables hashjoin and mergejoin. \
           'merge_join' always disables hashjoin and nestloop.",
        &PGS_MERGE_JOIN_STRATEGY,
        GucContext::Suset,
        GucFlags::default(),
    );

    // D-1a: UNLOGGED change buffers.
    GucRegistry::define_bool_guc(
        c"pg_trickle.unlogged_buffers",
        c"Create new change buffer tables as UNLOGGED to reduce WAL amplification.",
        c"When true, new change buffer tables are UNLOGGED (no WAL writes). \
           Reduces CDC WAL amplification by ~30% but buffers are lost on crash. \
           After crash, affected stream tables receive an automatic FULL refresh. \
           Existing buffers are not changed; use pgtrickle.convert_buffers_to_unlogged() \
           to convert them. Default: false (crash-safe, WAL-logged).",
        &PGS_UNLOGGED_BUFFERS,
        GucContext::Suset,
        GucFlags::default(),
    );

    // PH-D1: MERGE strategy override.
    GucRegistry::define_string_guc(
        c"pg_trickle.merge_strategy",
        c"Delta apply strategy: auto (default), merge, delete_insert.",
        c"'auto' (default) uses DELETE+INSERT for sub-1% deltas (delta_rows / target_rows \
           below merge_strategy_threshold) and MERGE otherwise. \
           'merge' always uses the MERGE statement. \
           'delete_insert' always uses separate DELETE + INSERT statements, \
           avoiding the MERGE join cost for small deltas against large tables.",
        &PGS_MERGE_STRATEGY,
        GucContext::Suset,
        GucFlags::default(),
    );

    // PH-D1: Merge strategy threshold.
    GucRegistry::define_float_guc(
        c"pg_trickle.merge_strategy_threshold",
        c"Delta ratio threshold for auto merge_strategy (default: 0.01 = 1%).",
        c"When merge_strategy = 'auto', DELETE+INSERT is used instead of MERGE when \
           delta_rows / target_rows is below this threshold. Higher values cause more \
           refreshes to use DELETE+INSERT. Range: 0.001 to 1.0.",
        &PGS_MERGE_STRATEGY_THRESHOLD,
        0.001, // min
        1.0,   // max
        GucContext::Suset,
        GucFlags::default(),
    );
}

// ── Convenience accessors ──────────────────────────────────────────────────

/// Returns the number of differential cycles before automatic drift reset.
pub fn pg_trickle_algebraic_drift_reset_cycles() -> i32 {
    PGS_ALGEBRAIC_DRIFT_RESET_CYCLES.get()
}

/// Returns whether automatic schedule backoff is enabled for falling-behind STs.
pub fn pg_trickle_auto_backoff() -> bool {
    PGS_AUTO_BACKOFF.get()
}

/// Returns the delta-to-ST ratio threshold for disabling seqscan before MERGE.
pub fn pg_trickle_merge_seqscan_threshold() -> f64 {
    PGS_MERGE_SEQSCAN_THRESHOLD.get()
}

/// Returns the current value of `pg_trickle.enabled`.
pub fn pg_trickle_enabled() -> bool {
    PGS_ENABLED.get()
}

/// Returns the scheduler interval in milliseconds.
pub fn pg_trickle_scheduler_interval_ms() -> i32 {
    PGS_SCHEDULER_INTERVAL_MS.get()
}

/// Returns the minimum schedule in seconds.
pub fn pg_trickle_min_schedule_seconds() -> i32 {
    PGS_MIN_SCHEDULE_SECONDS.get()
}

/// Returns the default effective schedule (in seconds) for isolated CALCULATED
/// stream tables that have no downstream dependents.
pub fn pg_trickle_default_schedule_seconds() -> i32 {
    PGS_DEFAULT_SCHEDULE_SECONDS.get()
}

/// Returns the max consecutive errors before auto-suspend.
pub fn pg_trickle_max_consecutive_errors() -> i32 {
    PGS_MAX_CONSECUTIVE_ERRORS.get()
}

/// Returns the max change ratio for adaptive FULL fallback.
pub fn pg_trickle_differential_max_change_ratio() -> f64 {
    PGS_DIFFERENTIAL_MAX_CHANGE_RATIO.get()
}

/// B-4: Returns the refresh strategy override.
pub fn pg_trickle_refresh_strategy() -> RefreshStrategy {
    normalize_refresh_strategy(
        PGS_REFRESH_STRATEGY
            .get()
            .and_then(|cs| cs.to_str().ok().map(str::to_owned)),
    )
}

/// B-4: Returns the cost-model safety margin (default 0.8).
pub fn pg_trickle_cost_model_safety_margin() -> f64 {
    PGS_COST_MODEL_SAFETY_MARGIN.get()
}

/// PH-E1: Returns the max estimated delta output rows before FULL fallback.
/// Returns 0 when disabled.
pub fn pg_trickle_max_delta_estimate_rows() -> i32 {
    PGS_MAX_DELTA_ESTIMATE_ROWS.get()
}

/// WM-7: Returns the watermark holdback timeout in seconds (0 = disabled).
pub fn pg_trickle_watermark_holdback_timeout() -> i32 {
    PGS_WATERMARK_HOLDBACK_TIMEOUT.get()
}

/// PH-E2: Returns the spill detection threshold in temp blocks written (0 = disabled).
pub fn pg_trickle_spill_threshold_blocks() -> i32 {
    PGS_SPILL_THRESHOLD_BLOCKS.get()
}

/// PH-E2: Returns the consecutive spill limit before FULL fallback (default 3).
pub fn pg_trickle_spill_consecutive_limit() -> i32 {
    PGS_SPILL_CONSECUTIVE_LIMIT.get()
}

/// Returns the change buffer schema name.
pub fn pg_trickle_change_buffer_schema() -> String {
    PGS_CHANGE_BUFFER_SCHEMA
        .get()
        .map(|cs| cs.to_str().unwrap_or("pgtrickle_changes").to_string())
        .unwrap_or_else(|| "pgtrickle_changes".to_string())
}

/// Returns the maximum number of concurrent refresh workers.
pub fn pg_trickle_max_concurrent_refreshes() -> i32 {
    PGS_MAX_CONCURRENT_REFRESHES.get()
}

/// Returns whether TRUNCATE cleanup is enabled.
pub fn pg_trickle_cleanup_use_truncate() -> bool {
    PGS_CLEANUP_USE_TRUNCATE.get()
}

/// Returns whether MERGE planner hints are enabled.
///
/// C4: Returns the value of `planner_aggressive`. The legacy
/// `merge_planner_hints` GUC is ignored at runtime.
pub fn pg_trickle_merge_planner_hints() -> bool {
    PGS_PLANNER_AGGRESSIVE.get()
}

/// Returns the work_mem value (in MB) for large-delta MERGE.
pub fn pg_trickle_merge_work_mem_mb() -> i32 {
    PGS_MERGE_WORK_MEM_MB.get()
}

/// SCAL-3: Returns the delta work_mem cap (MB). 0 = disabled.
pub fn pg_trickle_delta_work_mem_cap_mb() -> i32 {
    PGS_DELTA_WORK_MEM_CAP_MB.get()
}

/// Returns whether prepared statements are enabled for MERGE.
pub fn pg_trickle_use_prepared_statements() -> bool {
    PGS_USE_PREPARED_STATEMENTS.get()
}

/// Returns the canonical user-trigger handling mode.
///
/// `on` is preserved as a deprecated input alias for backward compatibility
/// but is normalized to `auto` at runtime.
pub fn pg_trickle_user_triggers_mode() -> UserTriggersMode {
    normalize_user_triggers_mode(
        PGS_USER_TRIGGERS
            .get()
            .and_then(|cs| cs.to_str().ok().map(str::to_owned)),
    )
}

/// Returns the canonical user-trigger handling mode as a string.
pub fn pg_trickle_user_triggers() -> String {
    pg_trickle_user_triggers_mode().as_str().to_string()
}

/// Returns the CDC mode: `"auto"`, `"trigger"`, or `"wal"`.
pub fn pg_trickle_cdc_mode() -> String {
    PGS_CDC_MODE
        .get()
        .map(|cs| cs.to_str().unwrap_or("auto").to_string())
        .unwrap_or_else(|| "auto".to_string())
}

/// Returns the WAL transition timeout in seconds.
pub fn pg_trickle_wal_transition_timeout() -> i32 {
    PGS_WAL_TRANSITION_TIMEOUT.get()
}

/// Returns the WAL slot lag warning threshold in bytes.
pub fn pg_trickle_slot_lag_warning_threshold_bytes() -> i64 {
    threshold_mb_to_bytes(PGS_SLOT_LAG_WARNING_THRESHOLD_MB.get())
}

/// Returns the WAL slot lag critical threshold in bytes.
pub fn pg_trickle_slot_lag_critical_threshold_bytes() -> i64 {
    threshold_mb_to_bytes(PGS_SLOT_LAG_CRITICAL_THRESHOLD_MB.get())
}

/// Returns whether source DDL blocking is enabled.
pub fn pg_trickle_block_source_ddl() -> bool {
    PGS_BLOCK_SOURCE_DDL.get()
}

/// Returns the buffer alert threshold (row count).
pub fn pg_trickle_buffer_alert_threshold() -> i64 {
    PGS_BUFFER_ALERT_THRESHOLD.get() as i64
}

/// Returns the change buffer compaction threshold (row count).
/// Returns 0 when compaction is disabled.
pub fn pg_trickle_compact_threshold() -> i64 {
    PGS_COMPACT_THRESHOLD.get() as i64
}

/// Returns the max buffer rows limit (row count).
/// Returns 0 when the limit is disabled.
pub fn pg_trickle_max_buffer_rows() -> i64 {
    PGS_MAX_BUFFER_ROWS.get() as i64
}

/// Returns whether automatic index creation is enabled.
pub fn pg_trickle_auto_index() -> bool {
    PGS_AUTO_INDEX.get()
}

/// B-1: Returns whether the aggregate fast-path (explicit DML for
/// all-algebraic aggregate queries) is enabled.
pub fn pg_trickle_aggregate_fast_path() -> bool {
    PGS_AGGREGATE_FAST_PATH.get()
}

/// G14-SHC: Returns whether the cross-backend template cache is enabled.
pub fn pg_trickle_template_cache_enabled() -> bool {
    PGS_TEMPLATE_CACHE.get()
}

/// Returns the buffer partitioning mode: `"off"`, `"on"`, or `"auto"`.
pub fn pg_trickle_buffer_partitioning() -> String {
    PGS_BUFFER_PARTITIONING
        .get()
        .map(|cs| cs.to_str().unwrap_or("off").to_string())
        .unwrap_or_else(|| "off".to_string())
}

/// Returns whether foreign table polling CDC is enabled.
pub fn pg_trickle_foreign_table_polling() -> bool {
    PGS_FOREIGN_TABLE_POLLING.get()
}

/// Returns whether materialized view polling CDC is enabled.
pub fn pg_trickle_matview_polling() -> bool {
    PGS_MATVIEW_POLLING.get()
}

/// Returns whether the tick watermark (CSS1) feature is enabled.
pub fn pg_trickle_tick_watermark_enabled() -> bool {
    PGS_TICK_WATERMARK_ENABLED.get()
}

/// Returns the CDC trigger granularity mode.
pub fn pg_trickle_cdc_trigger_mode() -> CdcTriggerMode {
    normalize_cdc_trigger_mode(
        PGS_CDC_TRIGGER_MODE
            .get()
            .and_then(|cs| cs.to_str().ok().map(str::to_owned)),
    )
}

/// Returns the maximum recursion depth for WITH RECURSIVE in IMMEDIATE mode.
/// Returns `None` when the guard is disabled (value = 0).
pub fn pg_trickle_ivm_recursive_max_depth() -> Option<i32> {
    normalize_recursive_max_depth(PGS_IVM_RECURSIVE_MAX_DEPTH.get())
}

/// Parallel refresh operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelRefreshMode {
    /// Sequential execution — current behavior (default).
    Off,
    /// Compute execution units and log dispatch decisions, but execute inline.
    DryRun,
    /// Enable true parallel refresh via dynamic background workers.
    On,
}

impl ParallelRefreshMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ParallelRefreshMode::Off => "off",
            ParallelRefreshMode::DryRun => "dry_run",
            ParallelRefreshMode::On => "on",
        }
    }
}

impl std::fmt::Display for ParallelRefreshMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

fn normalize_parallel_refresh_mode(value: Option<String>) -> ParallelRefreshMode {
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("dry_run") => ParallelRefreshMode::DryRun,
        Some("off") => ParallelRefreshMode::Off,
        // Default to On for None (unset) and any unrecognised value.
        // On has been the stable parallel path since v0.4.0.
        _ => ParallelRefreshMode::On,
    }
}

/// Returns the current parallel refresh mode.
pub fn pg_trickle_parallel_refresh_mode() -> ParallelRefreshMode {
    normalize_parallel_refresh_mode(
        PGS_PARALLEL_REFRESH_MODE
            .get()
            .and_then(|cs| cs.to_str().ok().map(str::to_owned)),
    )
}

/// Returns the cluster-wide cap on dynamic refresh workers.
pub fn pg_trickle_max_dynamic_refresh_workers() -> i32 {
    PGS_MAX_DYNAMIC_REFRESH_WORKERS.get()
}

/// Returns the maximum fixpoint iterations for SCC convergence (CYC-4).
pub fn pg_trickle_max_fixpoint_iterations() -> i32 {
    PGS_MAX_FIXPOINT_ITERATIONS.get()
}

/// Returns whether circular (cyclic) dependencies are allowed (CYC-4).
pub fn pg_trickle_allow_circular() -> bool {
    PGS_ALLOW_CIRCULAR.get()
}

/// G-7: Returns whether tiered refresh scheduling is enabled.
pub fn pg_trickle_tiered_scheduling() -> bool {
    PGS_TIERED_SCHEDULING.get()
}

/// QF-1: Returns whether MERGE SQL template logging is enabled.
pub fn pg_trickle_log_merge_sql() -> bool {
    PGS_LOG_MERGE_SQL.get()
}

/// FUSE-5: Returns the global default fuse ceiling (0 = disabled).
pub fn pg_trickle_fuse_default_ceiling() -> i64 {
    PGS_FUSE_DEFAULT_CEILING.get() as i64
}

/// C3-1: Returns the per-database worker quota (0 = disabled).
pub fn pg_trickle_per_database_worker_quota() -> i32 {
    PGS_PER_DATABASE_WORKER_QUOTA.get()
}

/// DAG-3: Returns the delta amplification threshold (0.0 = disabled).
pub fn pg_trickle_delta_amplification_threshold() -> f64 {
    PGS_DELTA_AMPLIFICATION_THRESHOLD.get()
}

/// DIAG-2: Returns the algebraic aggregate cardinality warning threshold.
/// Returns 0 when the warning is disabled.
pub fn pg_trickle_agg_diff_cardinality_threshold() -> i32 {
    PGS_AGG_DIFF_CARDINALITY_THRESHOLD.get()
}

/// G13-SD: Returns the maximum recursion depth for query parser visitors.
pub fn pg_trickle_max_parse_depth() -> usize {
    PGS_MAX_PARSE_DEPTH.get() as usize
}

/// WAKE-1: Returns whether event-driven scheduler wake is enabled.
pub fn pg_trickle_event_driven_wake() -> bool {
    PGS_EVENT_DRIVEN_WAKE.get()
}

/// WAKE-1: Returns the debounce interval in milliseconds.
pub fn pg_trickle_wake_debounce_ms() -> i32 {
    PGS_WAKE_DEBOUNCE_MS.get()
}

/// VOL-1: Returns the volatile function handling policy.
pub fn pg_trickle_volatile_function_policy() -> VolatileFunctionPolicy {
    normalize_volatile_function_policy(
        PGS_VOLATILE_FUNCTION_POLICY
            .get()
            .and_then(|cs| cs.to_str().ok().map(str::to_owned)),
    )
}

/// PH-D2: Returns the merge join strategy override.
pub fn pg_trickle_merge_join_strategy() -> MergeJoinStrategy {
    normalize_merge_join_strategy(
        PGS_MERGE_JOIN_STRATEGY
            .get()
            .and_then(|cs| cs.to_str().ok().map(str::to_owned)),
    )
}

/// D-1a: Returns whether new change buffer tables should be created UNLOGGED.
pub fn pg_trickle_unlogged_buffers() -> bool {
    PGS_UNLOGGED_BUFFERS.get()
}

/// PH-D1: Returns the merge strategy override.
pub fn pg_trickle_merge_strategy() -> MergeStrategy {
    normalize_merge_strategy(
        PGS_MERGE_STRATEGY
            .get()
            .and_then(|cs| cs.to_str().ok().map(str::to_owned)),
    )
}

/// PH-D1: Returns the merge strategy threshold for the `auto` heuristic.
pub fn pg_trickle_merge_strategy_threshold() -> f64 {
    PGS_MERGE_STRATEGY_THRESHOLD.get()
}

#[cfg(test)]
mod tests {
    use super::{
        CdcTriggerMode, MergeJoinStrategy, MergeStrategy, ParallelRefreshMode, RefreshStrategy,
        UserTriggersMode, VolatileFunctionPolicy, normalize_cdc_trigger_mode,
        normalize_merge_join_strategy, normalize_merge_strategy, normalize_parallel_refresh_mode,
        normalize_recursive_max_depth, normalize_refresh_strategy, normalize_user_triggers_mode,
        normalize_volatile_function_policy, threshold_mb_to_bytes,
    };

    #[test]
    fn test_normalize_user_triggers_mode_defaults_to_auto() {
        assert_eq!(normalize_user_triggers_mode(None), UserTriggersMode::Auto);
        assert_eq!(
            normalize_user_triggers_mode(Some("auto".to_string())),
            UserTriggersMode::Auto
        );
        assert_eq!(
            normalize_user_triggers_mode(Some("on".to_string())),
            UserTriggersMode::Auto
        );
        assert_eq!(
            normalize_user_triggers_mode(Some("unexpected".to_string())),
            UserTriggersMode::Auto
        );
    }

    #[test]
    fn test_normalize_user_triggers_mode_accepts_off_case_insensitively() {
        assert_eq!(
            normalize_user_triggers_mode(Some("off".to_string())),
            UserTriggersMode::Off
        );
        assert_eq!(
            normalize_user_triggers_mode(Some("OFF".to_string())),
            UserTriggersMode::Off
        );
    }

    #[test]
    fn test_threshold_mb_to_bytes_converts_megabytes() {
        assert_eq!(threshold_mb_to_bytes(0), 0);
        assert_eq!(threshold_mb_to_bytes(100), 104_857_600);
        assert_eq!(threshold_mb_to_bytes(1024), 1_073_741_824);
    }

    #[test]
    fn test_normalize_cdc_trigger_mode_defaults_to_statement() {
        assert_eq!(normalize_cdc_trigger_mode(None), CdcTriggerMode::Statement);
        assert_eq!(
            normalize_cdc_trigger_mode(Some("statement".to_string())),
            CdcTriggerMode::Statement
        );
        assert_eq!(
            normalize_cdc_trigger_mode(Some("unexpected".to_string())),
            CdcTriggerMode::Statement
        );
    }

    #[test]
    fn test_normalize_cdc_trigger_mode_accepts_row_case_insensitively() {
        assert_eq!(
            normalize_cdc_trigger_mode(Some("row".to_string())),
            CdcTriggerMode::Row
        );
        assert_eq!(
            normalize_cdc_trigger_mode(Some("ROW".to_string())),
            CdcTriggerMode::Row
        );
    }

    #[test]
    fn test_normalize_recursive_max_depth_zero_disables_guard() {
        assert_eq!(normalize_recursive_max_depth(0), None);
        assert_eq!(normalize_recursive_max_depth(-5), None);
        assert_eq!(normalize_recursive_max_depth(100), Some(100));
    }

    #[test]
    fn test_parallel_refresh_mode_display_matches_as_str() {
        assert_eq!(ParallelRefreshMode::Off.as_str(), "off");
        assert_eq!(ParallelRefreshMode::DryRun.as_str(), "dry_run");
        assert_eq!(ParallelRefreshMode::On.as_str(), "on");
        assert_eq!(ParallelRefreshMode::DryRun.to_string(), "dry_run");
    }

    #[test]
    fn test_normalize_parallel_refresh_mode_defaults_to_on() {
        assert_eq!(
            normalize_parallel_refresh_mode(None),
            ParallelRefreshMode::On
        );
        assert_eq!(
            normalize_parallel_refresh_mode(Some("unexpected".to_string())),
            ParallelRefreshMode::On
        );
    }

    #[test]
    fn test_normalize_parallel_refresh_mode_accepts_supported_values() {
        assert_eq!(
            normalize_parallel_refresh_mode(Some("dry_run".to_string())),
            ParallelRefreshMode::DryRun
        );
        assert_eq!(
            normalize_parallel_refresh_mode(Some("DRY_RUN".to_string())),
            ParallelRefreshMode::DryRun
        );
        assert_eq!(
            normalize_parallel_refresh_mode(Some("on".to_string())),
            ParallelRefreshMode::On
        );
    }

    // ── P3: as_str coverage for all enum variants; threshold edge cases ─────

    #[test]
    fn test_user_triggers_mode_as_str() {
        assert_eq!(UserTriggersMode::Auto.as_str(), "auto");
        assert_eq!(UserTriggersMode::Off.as_str(), "off");
    }

    #[test]
    fn test_cdc_trigger_mode_as_str() {
        assert_eq!(CdcTriggerMode::Statement.as_str(), "statement");
        assert_eq!(CdcTriggerMode::Row.as_str(), "row");
    }

    #[test]
    fn test_parallel_refresh_mode_as_str_all_variants() {
        assert_eq!(ParallelRefreshMode::Off.as_str(), "off");
        assert_eq!(ParallelRefreshMode::DryRun.as_str(), "dry_run");
        assert_eq!(ParallelRefreshMode::On.as_str(), "on");
    }

    #[test]
    fn test_threshold_mb_to_bytes_negative_input_is_zero_or_negative() {
        // Negative megabytes should yield a non-positive byte count
        assert!(threshold_mb_to_bytes(-1) <= 0);
        assert!(threshold_mb_to_bytes(-100) < 0);
    }

    #[test]
    fn test_normalize_parallel_refresh_mode_case_insensitive_on() {
        assert_eq!(
            normalize_parallel_refresh_mode(Some("ON".to_string())),
            ParallelRefreshMode::On
        );
    }

    #[test]
    fn test_normalize_user_triggers_mode_roundtrip_via_as_str() {
        for (input, expected) in [
            ("off", UserTriggersMode::Off),
            ("OFF", UserTriggersMode::Off),
        ] {
            assert_eq!(
                normalize_user_triggers_mode(Some(input.to_string())),
                expected
            );
        }
        // as_str / normalize should be consistent
        assert_eq!(
            normalize_user_triggers_mode(Some(UserTriggersMode::Off.as_str().to_string())),
            UserTriggersMode::Off
        );
        assert_eq!(
            normalize_user_triggers_mode(Some(UserTriggersMode::Auto.as_str().to_string())),
            UserTriggersMode::Auto
        );
    }

    #[test]
    fn test_normalize_cdc_trigger_mode_roundtrip_via_as_str() {
        assert_eq!(
            normalize_cdc_trigger_mode(Some(CdcTriggerMode::Row.as_str().to_string())),
            CdcTriggerMode::Row
        );
        assert_eq!(
            normalize_cdc_trigger_mode(Some(CdcTriggerMode::Statement.as_str().to_string())),
            CdcTriggerMode::Statement
        );
    }

    #[test]
    fn test_normalize_volatile_function_policy_defaults_to_reject() {
        assert_eq!(
            normalize_volatile_function_policy(None),
            VolatileFunctionPolicy::Reject
        );
        assert_eq!(
            normalize_volatile_function_policy(Some("reject".to_string())),
            VolatileFunctionPolicy::Reject
        );
        assert_eq!(
            normalize_volatile_function_policy(Some("unexpected".to_string())),
            VolatileFunctionPolicy::Reject
        );
    }

    #[test]
    fn test_normalize_volatile_function_policy_accepts_warn_and_allow() {
        assert_eq!(
            normalize_volatile_function_policy(Some("warn".to_string())),
            VolatileFunctionPolicy::Warn
        );
        assert_eq!(
            normalize_volatile_function_policy(Some("WARN".to_string())),
            VolatileFunctionPolicy::Warn
        );
        assert_eq!(
            normalize_volatile_function_policy(Some("allow".to_string())),
            VolatileFunctionPolicy::Allow
        );
        assert_eq!(
            normalize_volatile_function_policy(Some("ALLOW".to_string())),
            VolatileFunctionPolicy::Allow
        );
    }

    #[test]
    fn test_volatile_function_policy_as_str() {
        assert_eq!(VolatileFunctionPolicy::Reject.as_str(), "reject");
        assert_eq!(VolatileFunctionPolicy::Warn.as_str(), "warn");
        assert_eq!(VolatileFunctionPolicy::Allow.as_str(), "allow");
    }

    #[test]
    fn test_normalize_volatile_function_policy_roundtrip_via_as_str() {
        for policy in [
            VolatileFunctionPolicy::Reject,
            VolatileFunctionPolicy::Warn,
            VolatileFunctionPolicy::Allow,
        ] {
            assert_eq!(
                normalize_volatile_function_policy(Some(policy.as_str().to_string())),
                policy
            );
        }
    }

    #[test]
    fn test_normalize_merge_join_strategy_defaults_to_auto() {
        assert_eq!(normalize_merge_join_strategy(None), MergeJoinStrategy::Auto);
        assert_eq!(
            normalize_merge_join_strategy(Some("auto".to_string())),
            MergeJoinStrategy::Auto
        );
        assert_eq!(
            normalize_merge_join_strategy(Some("unexpected".to_string())),
            MergeJoinStrategy::Auto
        );
    }

    #[test]
    fn test_normalize_merge_join_strategy_all_variants() {
        assert_eq!(
            normalize_merge_join_strategy(Some("hash_join".to_string())),
            MergeJoinStrategy::HashJoin
        );
        assert_eq!(
            normalize_merge_join_strategy(Some("HASH_JOIN".to_string())),
            MergeJoinStrategy::HashJoin
        );
        assert_eq!(
            normalize_merge_join_strategy(Some("nested_loop".to_string())),
            MergeJoinStrategy::NestedLoop
        );
        assert_eq!(
            normalize_merge_join_strategy(Some("NESTED_LOOP".to_string())),
            MergeJoinStrategy::NestedLoop
        );
        assert_eq!(
            normalize_merge_join_strategy(Some("merge_join".to_string())),
            MergeJoinStrategy::MergeJoin
        );
        assert_eq!(
            normalize_merge_join_strategy(Some("MERGE_JOIN".to_string())),
            MergeJoinStrategy::MergeJoin
        );
    }

    #[test]
    fn test_merge_join_strategy_as_str() {
        assert_eq!(MergeJoinStrategy::Auto.as_str(), "auto");
        assert_eq!(MergeJoinStrategy::HashJoin.as_str(), "hash_join");
        assert_eq!(MergeJoinStrategy::NestedLoop.as_str(), "nested_loop");
        assert_eq!(MergeJoinStrategy::MergeJoin.as_str(), "merge_join");
    }

    #[test]
    fn test_normalize_merge_join_strategy_roundtrip_via_as_str() {
        for strategy in [
            MergeJoinStrategy::Auto,
            MergeJoinStrategy::HashJoin,
            MergeJoinStrategy::NestedLoop,
            MergeJoinStrategy::MergeJoin,
        ] {
            assert_eq!(
                normalize_merge_join_strategy(Some(strategy.as_str().to_string())),
                strategy
            );
        }
    }

    #[test]
    fn test_normalize_merge_strategy_defaults_to_auto() {
        assert_eq!(normalize_merge_strategy(None), MergeStrategy::Auto);
        assert_eq!(
            normalize_merge_strategy(Some("".to_string())),
            MergeStrategy::Auto
        );
        assert_eq!(
            normalize_merge_strategy(Some("garbage".to_string())),
            MergeStrategy::Auto
        );
    }

    #[test]
    fn test_normalize_merge_strategy_all_variants() {
        assert_eq!(
            normalize_merge_strategy(Some("merge".to_string())),
            MergeStrategy::Merge
        );
        assert_eq!(
            normalize_merge_strategy(Some("delete_insert".to_string())),
            MergeStrategy::DeleteInsert
        );
        assert_eq!(
            normalize_merge_strategy(Some("auto".to_string())),
            MergeStrategy::Auto
        );
        // Case-insensitive
        assert_eq!(
            normalize_merge_strategy(Some("DELETE_INSERT".to_string())),
            MergeStrategy::DeleteInsert
        );
        assert_eq!(
            normalize_merge_strategy(Some("MERGE".to_string())),
            MergeStrategy::Merge
        );
    }

    #[test]
    fn test_normalize_merge_strategy_roundtrip_via_as_str() {
        for strategy in [
            MergeStrategy::Auto,
            MergeStrategy::Merge,
            MergeStrategy::DeleteInsert,
        ] {
            assert_eq!(
                normalize_merge_strategy(Some(strategy.as_str().to_string())),
                strategy
            );
        }
    }

    // ── B-4: RefreshStrategy normalizer tests ───────────────────────

    #[test]
    fn test_normalize_refresh_strategy_defaults_to_auto() {
        assert_eq!(normalize_refresh_strategy(None), RefreshStrategy::Auto);
        assert_eq!(
            normalize_refresh_strategy(Some("auto".to_string())),
            RefreshStrategy::Auto
        );
        assert_eq!(
            normalize_refresh_strategy(Some("unexpected".to_string())),
            RefreshStrategy::Auto
        );
    }

    #[test]
    fn test_normalize_refresh_strategy_all_variants() {
        assert_eq!(
            normalize_refresh_strategy(Some("differential".to_string())),
            RefreshStrategy::Differential
        );
        assert_eq!(
            normalize_refresh_strategy(Some("DIFFERENTIAL".to_string())),
            RefreshStrategy::Differential
        );
        assert_eq!(
            normalize_refresh_strategy(Some("full".to_string())),
            RefreshStrategy::Full
        );
        assert_eq!(
            normalize_refresh_strategy(Some("FULL".to_string())),
            RefreshStrategy::Full
        );
    }

    #[test]
    fn test_refresh_strategy_as_str() {
        assert_eq!(RefreshStrategy::Auto.as_str(), "auto");
        assert_eq!(RefreshStrategy::Differential.as_str(), "differential");
        assert_eq!(RefreshStrategy::Full.as_str(), "full");
    }

    #[test]
    fn test_normalize_refresh_strategy_roundtrip_via_as_str() {
        for strategy in [
            RefreshStrategy::Auto,
            RefreshStrategy::Differential,
            RefreshStrategy::Full,
        ] {
            assert_eq!(
                normalize_refresh_strategy(Some(strategy.as_str().to_string())),
                strategy
            );
        }
    }

    // Note: GUC default value tests (PGS_WATERMARK_HOLDBACK_TIMEOUT,
    // PGS_SPILL_THRESHOLD_BLOCKS, PGS_SPILL_CONSECUTIVE_LIMIT) require a
    // PostgreSQL backend and are covered by E2E tests.  Calling
    // `GucSetting::get()` in multi-threaded unit tests triggers pgrx's
    // "postgres FFI may not be called from multiple threads" guard.
}
