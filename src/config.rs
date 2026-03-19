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

/// Whether to use TRUNCATE instead of DELETE for change buffer cleanup
/// when the entire buffer is consumed by a refresh.
///
/// TRUNCATE is O(1) regardless of row count, versus per-row DELETE which
/// must update indexes. This saves 3–5ms per refresh at 10%+ change rates.
///
/// Set to false if the TRUNCATE AccessExclusiveLock on the change buffer
/// is problematic for concurrent DML on the source table.
pub static PGS_CLEANUP_USE_TRUNCATE: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Whether to inject `SET LOCAL` planner hints before MERGE execution.
///
/// When enabled, the refresh executor estimates the delta size and applies:
/// - delta >= 100 rows: `SET LOCAL enable_nestloop = off` (favour hash joins)
/// - delta >= 10 000 rows: additionally `SET LOCAL work_mem = '<N>MB'`
///
/// This reduces P95 latency spikes caused by PostgreSQL choosing nested-loop
/// plans for medium/large delta sizes.
pub static PGS_MERGE_PLANNER_HINTS: GucSetting<bool> = GucSetting::<bool>::new(true);

/// `work_mem` (in MB) applied via `SET LOCAL` when the estimated delta
/// exceeds 10 000 rows and planner hints are enabled.
///
/// A higher value lets PostgreSQL use larger hash tables for the MERGE
/// join, avoiding disk-spilling sort/merge strategies on large deltas.
pub static PGS_MERGE_WORK_MEM_MB: GucSetting<i32> = GucSetting::<i32>::new(64);

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
/// Useful for production deployments where source schemas are stable and
/// accidental column changes should be prevented.
pub static PGS_BLOCK_SOURCE_DDL: GucSetting<bool> = GucSetting::<bool>::new(false);

/// F46 (G9.3): Buffer growth alert threshold (number of pending change rows).
///
/// When any source table's change buffer exceeds this number of rows,
/// a `BufferGrowthWarning` alert is emitted. Configurable to accommodate
/// both high-throughput workloads (raise) and small tables (lower).
pub static PGS_BUFFER_ALERT_THRESHOLD: GucSetting<i32> = GucSetting::<i32>::new(1_000_000);

/// Maximum allowed grouping set branches for CUBE/ROLLUP expansion (EC-02).
pub static PGS_MAX_GROUPING_SET_BRANCHES: GucSetting<i32> = GucSetting::<i32>::new(64);

/// Number of differential refresh cycles after which algebraic aggregate
/// stream tables are automatically reinitialized (full recompute) to reset
/// accumulated floating-point drift in auxiliary sum/sum2 columns.
///
/// Set to 0 to disable periodic drift reset (default).
/// Typical values: 100–1000, depending on workload precision requirements.
pub static PGS_ALGEBRAIC_DRIFT_RESET_CYCLES: GucSetting<i32> = GucSetting::<i32>::new(0);

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

/// Parallel refresh mode — controls whether the scheduler dispatches
/// refresh work to dynamic background workers.
///
/// - `"off"` (default): Current sequential behavior.
/// - `"dry_run"`: Compute execution units and log dispatch decisions,
///   but execute inline (no actual workers spawned).
/// - `"on"`: Enable true parallel refresh via dynamic workers.
pub static PGS_PARALLEL_REFRESH_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"off"));

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
        100,    // min
        60_000, // max
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

    GucRegistry::define_bool_guc(
        c"pg_trickle.cleanup_use_truncate",
        c"Use TRUNCATE for change buffer cleanup when all rows are consumed.",
        c"When true and the entire change buffer is consumed by a refresh, uses TRUNCATE (O(1)) instead of per-row DELETE. Disable if the AccessExclusiveLock is problematic.",
        &PGS_CLEANUP_USE_TRUNCATE,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_trickle.merge_planner_hints",
        c"Inject SET LOCAL planner hints before MERGE execution.",
        c"When true, disables nested-loop joins and optionally raises work_mem for medium/large delta sizes to stabilise P95 latency.",
        &PGS_MERGE_PLANNER_HINTS,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_trickle.merge_work_mem_mb",
        c"work_mem (MB) for large-delta MERGE execution.",
        c"Applied via SET LOCAL when planner hints are enabled and the delta exceeds 10 000 rows.",
        &PGS_MERGE_WORK_MEM_MB,
        8,    // min
        4096, // max (4 GB)
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
        c"When true, ALTER TABLE that adds, drops, renames, or changes the type of a column \
           on a source table will ERROR instead of triggering reinitialization. \
           Benign DDL (indexes, comments, statistics) and constraint changes are always allowed. \
           Useful for production deployments where source schemas should be stable.",
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

    GucRegistry::define_string_guc(
        c"pg_trickle.parallel_refresh_mode",
        c"Parallel refresh mode: off, dry_run, or on.",
        c"'off' (default): sequential refresh. \
           'dry_run': compute execution units and log dispatch decisions but execute inline. \
           'on': enable true parallel refresh via dynamic background workers.",
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
}

// ── Convenience accessors ──────────────────────────────────────────────────

/// Returns the number of differential cycles before automatic drift reset.
pub fn pg_trickle_algebraic_drift_reset_cycles() -> i32 {
    PGS_ALGEBRAIC_DRIFT_RESET_CYCLES.get()
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
pub fn pg_trickle_merge_planner_hints() -> bool {
    PGS_MERGE_PLANNER_HINTS.get()
}

/// Returns the work_mem value (in MB) for large-delta MERGE.
pub fn pg_trickle_merge_work_mem_mb() -> i32 {
    PGS_MERGE_WORK_MEM_MB.get()
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
        Some("on") => ParallelRefreshMode::On,
        _ => ParallelRefreshMode::Off,
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

#[cfg(test)]
mod tests {
    use super::{
        CdcTriggerMode, ParallelRefreshMode, UserTriggersMode, normalize_cdc_trigger_mode,
        normalize_parallel_refresh_mode, normalize_recursive_max_depth,
        normalize_user_triggers_mode, threshold_mb_to_bytes,
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
    fn test_normalize_parallel_refresh_mode_defaults_to_off() {
        assert_eq!(
            normalize_parallel_refresh_mode(None),
            ParallelRefreshMode::Off
        );
        assert_eq!(
            normalize_parallel_refresh_mode(Some("unexpected".to_string())),
            ParallelRefreshMode::Off
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
}
