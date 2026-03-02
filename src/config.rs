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
pub static PGS_MIN_SCHEDULE_SECONDS: GucSetting<i32> = GucSetting::<i32>::new(60);

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
/// - `"on"`: Always use explicit DML, even if no user triggers exist.
/// - `"off"`: Always use MERGE; user triggers will NOT fire correctly.
pub static PGS_USER_TRIGGERS: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"auto"));

/// CDC mechanism selection.
///
/// - `"trigger"` (default): Always use row-level triggers for CDC.
/// - `"auto"`: Use triggers for creation, transition to WAL if available.
/// - `"wal"`: Require WAL-based CDC (fail if `wal_level != logical`).
pub static PGS_CDC_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"trigger"));

/// Maximum time (seconds) to wait for the WAL decoder to catch up during
/// transition from triggers to WAL-based CDC before falling back to triggers.
pub static PGS_WAL_TRANSITION_TIMEOUT: GucSetting<i32> = GucSetting::<i32>::new(300);

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

/// Default diamond consistency mode for newly created stream tables.
///
/// - `"none"` (default): No atomic grouping — each ST refreshes independently.
/// - `"atomic"`: All members of a diamond's consistency group are wrapped in a
///   single SAVEPOINT; if any member fails the entire group rolls back.
pub static PGS_DIAMOND_CONSISTENCY: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"none"));

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
        c"Reserved for future use — parallel refresh is not yet implemented.",
        c"This setting is reserved for v0.3.0 parallel refresh. \
           It is accepted and stored but has no effect on behaviour in v0.2.0. \
           The scheduler processes stream tables sequentially.",
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
        c"User-trigger handling: auto, on, or off.",
        c"'auto' detects row-level user triggers and switches to explicit DML so they fire correctly. \
           'on' forces explicit DML even without triggers. \
           'off' always uses MERGE (triggers will NOT fire correctly).",
        &PGS_USER_TRIGGERS,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"pg_trickle.cdc_mode",
        c"CDC mechanism: trigger, auto, or wal.",
        c"'trigger' always uses row-level triggers for change capture. \
           'auto' uses triggers initially and transitions to WAL-based CDC if wal_level=logical. \
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
           the system falls back to trigger-based CDC. \
           NOTE: WAL-based CDC is pre-production in v0.2.0 and not recommended for production use.",
        &PGS_WAL_TRANSITION_TIMEOUT,
        10,    // min: 10 seconds
        3_600, // max: 1 hour
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

    GucRegistry::define_string_guc(
        c"pg_trickle.diamond_consistency",
        c"Default diamond consistency mode: none or atomic.",
        c"'none' (default) refreshes each ST independently. \
           'atomic' wraps diamond consistency groups in a SAVEPOINT so all members \
           succeed or fail together.",
        &PGS_DIAMOND_CONSISTENCY,
        GucContext::Suset,
        GucFlags::default(),
    );
}

// ── Convenience accessors ──────────────────────────────────────────────────

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

/// Returns the user-trigger handling mode: `"auto"`, `"on"`, or `"off"`.
pub fn pg_trickle_user_triggers() -> String {
    PGS_USER_TRIGGERS
        .get()
        .map(|cs| cs.to_str().unwrap_or("auto").to_string())
        .unwrap_or_else(|| "auto".to_string())
}

/// Returns the CDC mode: `"trigger"`, `"auto"`, or `"wal"`.
pub fn pg_trickle_cdc_mode() -> String {
    PGS_CDC_MODE
        .get()
        .map(|cs| cs.to_str().unwrap_or("trigger").to_string())
        .unwrap_or_else(|| "trigger".to_string())
}

/// Returns the WAL transition timeout in seconds.
pub fn pg_trickle_wal_transition_timeout() -> i32 {
    PGS_WAL_TRANSITION_TIMEOUT.get()
}

/// Returns whether source DDL blocking is enabled.
pub fn pg_trickle_block_source_ddl() -> bool {
    PGS_BLOCK_SOURCE_DDL.get()
}

/// Returns the buffer alert threshold (row count).
pub fn pg_trickle_buffer_alert_threshold() -> i64 {
    PGS_BUFFER_ALERT_THRESHOLD.get() as i64
}

/// Returns the default diamond consistency mode: `"none"` or `"atomic"`.
pub fn pg_trickle_diamond_consistency() -> String {
    PGS_DIAMOND_CONSISTENCY
        .get()
        .map(|cs| cs.to_str().unwrap_or("none").to_string())
        .unwrap_or_else(|| "none".to_string())
}
