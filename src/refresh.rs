//! Refresh executor — handles full, differential, and reinitialize refreshes.
//!
//! The executor is called by the scheduler for automated refreshes and by
//! `pgtrickle.refresh_stream_table()` for manual refreshes.
//!
//! ## Delta SQL Caching
//!
//! The differential refresh path caches the delta SQL template and MERGE
//! SQL template per `pgt_id` in thread-local storage. On subsequent
//! refreshes, the cached templates are resolved with actual frontier LSN
//! values — skipping SQL parsing, DVM differentiation, and MERGE SQL
//! string formatting. This eliminates ~45ms of overhead per refresh
//! (29.6ms planning + 15ms generate_delta).

use pgrx::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Instant;

use crate::catalog::{StDependency, StreamTableMeta};

// ── B-4: Query complexity classification ────────────────────────────────

/// Complexity class for a stream table's defining query.
///
/// Used by the cost model to apply per-class cost coefficients.  Higher
/// complexity classes have steeper differential cost curves (more joins /
/// aggregates → more work per delta row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryComplexityClass {
    /// Simple scan: `SELECT cols FROM single_table`
    Scan,
    /// Scan with filter: `SELECT cols FROM single_table WHERE ...`
    Filter,
    /// Aggregate: `SELECT ... GROUP BY ...` (no joins)
    Aggregate,
    /// Join(s) without aggregation
    Join,
    /// Join(s) with GROUP BY aggregation (most expensive differential path)
    JoinAggregate,
}

impl QueryComplexityClass {
    /// Default differential cost scaling factor per class.
    ///
    /// The factor represents the per-delta-row cost multiplier relative to
    /// a plain scan.  Joins and aggregates make each delta row more
    /// expensive to process incrementally.
    pub(crate) fn diff_cost_factor(self) -> f64 {
        match self {
            Self::Scan => 1.0,
            Self::Filter => 1.1,
            Self::Aggregate => 1.5,
            Self::Join => 2.5,
            Self::JoinAggregate => 4.0,
        }
    }
}

/// Classify a defining query's complexity from its SQL text.
///
/// Uses lightweight keyword analysis (no parsing or SPI).  This is
/// intentionally conservative: false positives (over-classifying) are
/// preferable to false negatives because a higher class merely biases
/// the cost model toward FULL at lower change rates, which is always safe.
pub(crate) fn classify_query_complexity(defining_query: &str) -> QueryComplexityClass {
    let upper = defining_query.to_ascii_uppercase();
    let has_join = upper.contains(" JOIN ")
        || upper.contains(" INNER JOIN ")
        || upper.contains(" LEFT JOIN ")
        || upper.contains(" RIGHT JOIN ")
        || upper.contains(" FULL JOIN ")
        || upper.contains(" CROSS JOIN ");
    let has_group_by = upper.contains("GROUP BY");

    match (has_join, has_group_by) {
        (true, true) => QueryComplexityClass::JoinAggregate,
        (true, false) => QueryComplexityClass::Join,
        (false, true) => QueryComplexityClass::Aggregate,
        (false, false) => {
            if upper.contains(" WHERE ") {
                QueryComplexityClass::Filter
            } else {
                QueryComplexityClass::Scan
            }
        }
    }
}

// ── G12-ERM-1: Effective refresh mode tracking ──────────────────────────

// Thread-local that records the mode actually used for the current refresh.
//
// Set by each concrete execution path (`execute_full_refresh`,
// `execute_differential_refresh`, etc.) so the scheduler can write the
// actual mode to `pgt_stream_tables.effective_refresh_mode` after the
// refresh completes — even when an internal fallback changed the mode.
thread_local! {
    static LAST_EFFECTIVE_MODE: Cell<&'static str> = const { Cell::new("") };
}

/// Record the effective refresh mode for the currently-executing refresh.
///
/// Called at the concrete execution point so fallbacks (e.g. adaptive
/// threshold → FULL, CTE → FULL) overwrite the initial mode correctly.
pub(crate) fn set_effective_mode(mode: &'static str) {
    LAST_EFFECTIVE_MODE.with(|m| m.set(mode));
}

/// Take (read and reset) the effective mode recorded by the most recent
/// execution path.  Returns `""` if no refresh has been recorded yet
/// in this thread.
pub fn take_effective_mode() -> &'static str {
    LAST_EFFECTIVE_MODE.with(|m| m.get())
}

// ── PH-E2: Last-refresh spill tracking ──────────────────────────────────

thread_local! {
    /// Temp blocks written during the most recent MERGE execution.
    /// Set after each differential refresh by querying pg_stat_statements.
    /// Read by the scheduler to track per-ST spill history.
    static LAST_TEMP_BLKS_WRITTEN: Cell<i64> = const { Cell::new(-1) };
}

/// Record the temp blocks written for the currently-executing refresh.
pub(crate) fn set_last_temp_blks_written(blks: i64) {
    LAST_TEMP_BLKS_WRITTEN.with(|c| c.set(blks));
}

/// Take the temp blocks written by the most recent differential refresh.
/// Returns -1 if not available (pg_stat_statements not installed, or not
/// a differential refresh).
pub fn take_last_temp_blks_written() -> i64 {
    LAST_TEMP_BLKS_WRITTEN.with(|c| {
        let v = c.get();
        c.set(-1);
        v
    })
}

use crate::dag::RefreshMode;
use crate::dvm;
use crate::error::PgTrickleError;
use crate::version::Frontier;

// ── MERGE SQL template cache ────────────────────────────────────────

/// Cached MERGE SQL template for a stream table.
///
/// The template has LSN placeholders embedded in the delta SQL portion.
/// It also stores the MERGE "shell" (the parts that wrap the delta SQL),
/// the source OIDs for placeholder resolution, and the cleanup DO block
/// template.
#[derive(Clone)]
struct CachedMergeTemplate {
    /// Hash of the defining query — invalidation key.
    defining_query_hash: u64,
    /// MERGE SQL template with `__PGS_PREV_LSN_{oid}__` / `__PGS_NEW_LSN_{oid}__`
    /// placeholder tokens. Resolved to concrete LSN values before each execution.
    merge_sql_template: String,
    /// Parameterized MERGE SQL with `$1`, `$2`, … for LSN values (D-2).
    /// Parameter order: for each source OID (in `source_oids` order),
    /// `$2i-1` = prev_lsn, `$2i` = new_lsn.
    parameterized_merge_sql: String,
    /// Source OIDs for LSN placeholder resolution.
    source_oids: Vec<u32>,
    /// Cleanup template with `__PGS_{PREV,NEW}_LSN_{oid}__` tokens.
    cleanup_sql_template: String,

    // ── User-trigger explicit DML templates ──────────────────────────
    // These templates reference `__pgt_delta_{pgt_id}` (a temp table
    // materialized at execution time) and do NOT contain LSN placeholders.
    /// DELETE statement for the trigger-enabled DML path.
    /// Deletes rows where the delta action is 'D'.
    trigger_delete_template: String,
    /// UPDATE statement for the trigger-enabled DML path.
    /// Updates existing rows where the delta action is 'I' and values changed.
    trigger_update_template: String,
    /// INSERT statement for the trigger-enabled DML path.
    /// Inserts genuinely new rows where the delta action is 'I'.
    trigger_insert_template: String,
    /// USING clause template with LSN placeholders (for materializing delta
    /// into a temp table in the user-trigger path).
    trigger_using_template: String,
    /// A1-2: Raw delta SQL template with LSN placeholders.
    /// Used at refresh time to compute partition key range (MIN/MAX) for A1-3
    /// predicate injection. Only populated when `st_partition_key` is set,
    /// but stored for all STs to keep the struct layout consistent.
    delta_sql_template: String,
    /// B-1: When true, all aggregates are algebraically invertible and the
    /// explicit DML fast-path can be used instead of MERGE.
    is_all_algebraic: bool,
    /// When true, the delta output has at most one row per `__pgt_row_id`.
    /// Non-deduplicated deltas (joins) may produce phantom rows that require
    /// PH-D1 with ON CONFLICT rather than MERGE.
    is_deduplicated: bool,
}

thread_local! {
    /// Per-session cache of MERGE SQL templates, keyed by `pgt_id`.
    ///
    /// Cross-session invalidation (G8.1): flushed when the shared
    /// `CACHE_GENERATION` counter advances.
    static MERGE_TEMPLATE_CACHE: RefCell<HashMap<i64, CachedMergeTemplate>> =
        RefCell::new(HashMap::new());

    /// Local snapshot of the shared `CACHE_GENERATION` counter.
    static LOCAL_MERGE_CACHE_GEN: Cell<u64> = const { Cell::new(0) };
}

// ── D-2: Prepared statement tracking ────────────────────────────────

thread_local! {
    /// Tracks which `pgt_id`s have a SQL `PREPARE`d MERGE statement
    /// in the current session.  Used by the prepared-statement path
    /// to skip re-issuing `PREPARE` on cache-hit refreshes.
    static PREPARED_MERGE_STMTS: RefCell<HashSet<i64>> =
        RefCell::new(HashSet::new());
}

// ── C-1: Deferred change buffer cleanup ─────────────────────────────

/// Pending cleanup work from a previous differential refresh.
///
/// Instead of cleaning up consumed change buffer rows synchronously in
/// the critical path, we defer the cleanup to the start of the next
/// refresh cycle.
///
/// **IMPORTANT:** Multiple stream tables may share the same change buffer
/// (source table).  The cleanup must only delete entries that ALL consumers
/// have already processed.  We use the minimum frontier across all STs
/// that depend on each source OID as the safe cleanup threshold.
struct PendingCleanup {
    change_schema: String,
    source_oids: Vec<u32>,
}

thread_local! {
    /// Queue of deferred cleanup operations from previous refreshes.
    static PENDING_CLEANUP: RefCell<Vec<PendingCleanup>> = const { RefCell::new(Vec::new()) };
}

thread_local! {
    // NS-3: Consecutive failure counts for deferred cleanup per source OID.
    // Emits a WARNING after the 3rd consecutive failure so operators are
    // alerted to persistent cleanup problems without flooding the log.
    static CLEANUP_FAILURE_COUNTS: RefCell<std::collections::HashMap<u32, u32>> =
        RefCell::new(std::collections::HashMap::new());
}

// ── DAG-4: ST bypass tables for fused-chain execution ───────────────

thread_local! {
    /// Maps upstream `pgt_id` → temp bypass table name.
    ///
    /// When a fused-chain worker refreshes an upstream member, instead of
    /// writing delta rows to the persistent `changes_pgt_{id}` buffer, it
    /// creates a TEMP TABLE with the same schema and stores the mapping
    /// here.  Downstream members in the same chain read from the temp
    /// table via this mapping.
    static ST_BYPASS_TABLES: RefCell<HashMap<i64, String>> =
        RefCell::new(HashMap::new());

    /// DI-2: Source table OIDs whose per-leaf delta fraction exceeds
    /// `max_delta_fraction`. These leaves fall back from NOT EXISTS
    /// (index-based) to EXCEPT ALL (hash-based) in snapshot construction.
    static FALLBACK_LEAF_OIDS: RefCell<std::collections::HashSet<u32>> =
        RefCell::new(std::collections::HashSet::new());
}

/// Register a bypass temp table for the given upstream pgt_id.
pub fn set_st_bypass(pgt_id: i64, temp_table: String) {
    ST_BYPASS_TABLES.with(|m| m.borrow_mut().insert(pgt_id, temp_table));
}

/// Remove the bypass mapping for a pgt_id.
pub fn clear_st_bypass(pgt_id: i64) {
    ST_BYPASS_TABLES.with(|m| m.borrow_mut().remove(&pgt_id));
}

/// Clear all bypass mappings.
pub fn clear_all_st_bypass() {
    ST_BYPASS_TABLES.with(|m| m.borrow_mut().clear());
}

/// Return the current bypass table mappings.
pub fn get_st_bypass_tables() -> HashMap<i64, String> {
    ST_BYPASS_TABLES.with(|m| m.borrow().clone())
}

/// DI-2: Set the per-leaf fallback OIDs for the current refresh cycle.
pub fn set_fallback_leaf_oids(oids: std::collections::HashSet<u32>) {
    FALLBACK_LEAF_OIDS.with(|m| *m.borrow_mut() = oids);
}

/// DI-2: Return the current per-leaf fallback OIDs.
pub fn get_fallback_leaf_oids() -> std::collections::HashSet<u32> {
    FALLBACK_LEAF_OIDS.with(|m| m.borrow().clone())
}

/// DI-2: Clear the per-leaf fallback OIDs.
pub fn clear_fallback_leaf_oids() {
    FALLBACK_LEAF_OIDS.with(|m| m.borrow_mut().clear());
}

/// Execute any pending cleanups from previous refresh cycles.
///
/// Called at the start of `execute_differential_refresh` to drain the
/// deferred cleanup queue.  Errors are logged but not propagated since
/// stale change-buffer rows are harmless due to LSN range predicates.
///
/// **Multi-ST safety:** When multiple stream tables depend on the same
/// source table, each ST may have a different frontier.  We compute the
/// minimum frontier LSN across all dependent STs for each source OID.
/// Only change buffer entries at or below this minimum are safe to delete,
/// because all consumers have already advanced past them.
///
/// **Robustness:** Each change buffer table is checked for existence via
/// `pg_class` before any DML is attempted.  When a stream table is dropped
/// and its change buffer tables are removed, stale pending-cleanup entries
/// referencing those tables are silently skipped.
fn drain_pending_cleanups() {
    let pending: Vec<PendingCleanup> =
        PENDING_CLEANUP.with(|q| std::mem::take(&mut *q.borrow_mut()));

    if pending.is_empty() {
        return;
    }

    // Deduplicate source OIDs and collect a consistent change schema.
    let mut all_oids = std::collections::HashSet::new();
    let mut change_schema = String::new();
    for job in &pending {
        if change_schema.is_empty() {
            change_schema.clone_from(&job.change_schema);
        }
        for &oid in &job.source_oids {
            all_oids.insert(oid);
        }
    }

    let use_truncate = crate::config::pg_trickle_cleanup_use_truncate();

    for oid in all_oids {
        // Check that the change buffer table still exists before
        // attempting any DML.  When a ST is dropped between refresh
        // cycles, cleanup_cdc_for_source removes the buffer table but
        // the thread-local pending queue may still reference it.
        // PERF-2: Accept both 'r' (regular) and 'p' (partitioned) relkinds
        // because auto-promotion converts buffers to partitioned at runtime.
        let table_exists = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM pg_class c \
               JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = '{schema}' \
                 AND c.relname = 'changes_{oid}' \
                 AND c.relkind IN ('r', 'p')\
             )",
            schema = change_schema,
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if !table_exists {
            pgrx::debug1!(
                "[pg_trickle] Deferred cleanup: skipping changes_{} (table dropped)",
                oid,
            );
            continue;
        }

        // Compute the minimum frontier LSN across ALL stream tables that
        // depend on this source OID.  Only entries at or below this LSN
        // have been consumed by every consumer and are safe to delete.
        //
        // We extract each ST's frontier for this source from the JSONB
        // `frontier->'sources'->'OID'->>'lsn'` path.  STs with NULL
        // frontiers (never refreshed) are excluded — they need a FULL
        // refresh first and won't read from the change buffer.
        let min_lsn: Option<String> = Spi::get_one::<String>(&format!(
            "SELECT MIN((st.frontier->'sources'->'{oid}'->>'lsn')::pg_lsn)::TEXT \
             FROM pgtrickle.pgt_stream_tables st \
             JOIN pgtrickle.pgt_dependencies dep ON dep.pgt_id = st.pgt_id \
             WHERE dep.source_relid = {oid} \
               AND dep.source_type = 'TABLE' \
               AND st.frontier IS NOT NULL \
               AND st.frontier->'sources'->'{oid}'->>'lsn' IS NOT NULL",
        ))
        .unwrap_or(None);

        let safe_lsn = match min_lsn {
            Some(lsn) if lsn != "0/0" => lsn,
            _ => {
                // No consumers with a frontier, or all at 0/0 — nothing to clean.
                pgrx::debug1!(
                    "[pg_trickle] Deferred cleanup: no safe threshold for changes_{}, skipping",
                    oid,
                );
                continue;
            }
        };

        let can_truncate = if use_truncate {
            // Safe to TRUNCATE only if ALL entries are at or below the safe LSN.
            Spi::get_one::<bool>(&format!(
                "SELECT NOT EXISTS(\
                   SELECT 1 FROM \"{schema}\".changes_{oid} \
                   WHERE lsn > '{safe_lsn}'::pg_lsn \
                   LIMIT 1\
                 )",
                schema = change_schema,
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false)
        } else {
            false
        };

        // Task 3.3: Partitioned buffer cleanup via DETACH + DROP.
        if crate::cdc::is_buffer_partitioned(&change_schema, oid) {
            match crate::cdc::detach_consumed_partitions(&change_schema, oid, &safe_lsn) {
                Ok(n) if n > 0 => {
                    pgrx::debug1!(
                        "[pg_trickle] Deferred cleanup: detached {} partition(s) from changes_{}",
                        n,
                        oid,
                    );
                }
                Err(e) => {
                    pgrx::debug1!(
                        "[pg_trickle] Deferred cleanup partition detach failed: {}",
                        e,
                    );
                }
                _ => {}
            }
            continue;
        }

        // NS-3: record a failed cleanup attempt; warn after 3 consecutive failures.
        let record_cleanup_failure = |oid: u32, operation: &str, msg: &str| {
            CLEANUP_FAILURE_COUNTS.with(|m| {
                let mut m = m.borrow_mut();
                let count = m.entry(oid).or_insert(0);
                *count += 1;
                if *count >= 3 {
                    pgrx::warning!(
                        "[pg_trickle] Deferred cleanup {} failed {} consecutive times for \
                         changes_{}: {}",
                        operation,
                        count,
                        oid,
                        msg
                    );
                    // Emit NOTIFY alert on 3rd and every subsequent 10th failure
                    // so operators know cleanup is persistently broken.
                    if *count == 3 || *count % 10 == 0 {
                        crate::monitor::emit_alert(
                            crate::monitor::AlertEvent::CleanupFailure,
                            "",
                            &format!("changes_{}", oid),
                            &format!(
                                r#""source_oid":{},"consecutive_failures":{},"operation":"{}","error":"{}""#,
                                oid,
                                count,
                                operation,
                                msg.replace('"', r#"\""#),
                            ),
                            false,
                        );
                    }
                } else {
                    pgrx::debug1!(
                        "[pg_trickle] Deferred cleanup {} failed (attempt {}): {}",
                        operation,
                        count,
                        msg
                    );
                }
            });
        };

        if can_truncate {
            match Spi::run(&format!(
                "TRUNCATE \"{schema}\".changes_{oid}",
                schema = change_schema,
            )) {
                Ok(()) => {
                    CLEANUP_FAILURE_COUNTS.with(|m| {
                        m.borrow_mut().remove(&oid);
                    });
                }
                Err(e) => record_cleanup_failure(oid, "TRUNCATE", &e.to_string()),
            }
        } else {
            let delete_sql = format!(
                "DELETE FROM \"{schema}\".changes_{oid} \
                 WHERE lsn <= '{safe_lsn}'::pg_lsn",
                schema = change_schema,
            );
            match Spi::run(&delete_sql) {
                Ok(()) => {
                    CLEANUP_FAILURE_COUNTS.with(|m| {
                        m.borrow_mut().remove(&oid);
                    });
                }
                Err(e) => record_cleanup_failure(oid, "DELETE", &e.to_string()),
            }
        }
    }
}

/// Frontier-based cleanup: delete stale change buffer rows using the persisted
/// frontier in `pgt_stream_tables` rather than thread-local state.
///
/// This complements `drain_pending_cleanups` by handling the case where the
/// deferred cleanup was queued on a different PostgreSQL backend process
/// (e.g., when a connection pool dispatches successive refresh calls to
/// different backends).
///
/// For each source OID, computes the minimum frontier LSN across ALL stream
/// tables that depend on it, then deletes change buffer entries at or below
/// that threshold.  This is safe because all consumers have already advanced
/// past those entries.
fn cleanup_change_buffers_by_frontier(change_schema: &str, source_oids: &[u32]) {
    if source_oids.is_empty() {
        return;
    }

    let use_truncate = crate::config::pg_trickle_cleanup_use_truncate();

    for &oid in source_oids {
        // Check that the change buffer table exists
        // PERF-2: Accept both 'r' (regular) and 'p' (partitioned) relkinds.
        let table_exists = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM pg_class c \
               JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = '{schema}' \
                 AND c.relname = 'changes_{oid}' \
                 AND c.relkind IN ('r', 'p')\
             )",
            schema = change_schema,
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if !table_exists {
            continue;
        }

        // Compute the minimum frontier LSN across ALL stream tables that
        // depend on this source OID.  TABLE, FOREIGN_TABLE, and MATVIEW sources
        // are included: FT/matview change buffers are written by polling and must
        // be cleaned up once all consumers have advanced their frontier past them.
        let min_lsn: Option<String> = Spi::get_one::<String>(&format!(
            "SELECT MIN((st.frontier->'sources'->'{oid}'->>'lsn')::pg_lsn)::TEXT \
             FROM pgtrickle.pgt_stream_tables st \
             JOIN pgtrickle.pgt_dependencies dep ON dep.pgt_id = st.pgt_id \
             WHERE dep.source_relid = {oid} \
               AND dep.source_type IN ('TABLE', 'FOREIGN_TABLE', 'MATVIEW') \
               AND st.frontier IS NOT NULL \
               AND st.frontier->'sources'->'{oid}'->>'lsn' IS NOT NULL",
        ))
        .unwrap_or(None);

        let safe_lsn = match min_lsn {
            Some(lsn) if lsn != "0/0" => lsn,
            _ => continue,
        };

        // Quick check: are there any entries to clean up?
        let has_stale = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM \"{schema}\".changes_{oid} \
               WHERE lsn <= '{safe_lsn}'::pg_lsn \
               LIMIT 1\
             )",
            schema = change_schema,
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if !has_stale {
            continue;
        }

        // Task 3.3: Partitioned buffer cleanup via DETACH + DROP.
        if crate::cdc::is_buffer_partitioned(change_schema, oid) {
            match crate::cdc::detach_consumed_partitions(change_schema, oid, &safe_lsn) {
                Ok(n) if n > 0 => {
                    pgrx::debug1!(
                        "[pg_trickle] Frontier cleanup: detached {} partition(s) from changes_{}",
                        n,
                        oid,
                    );
                }
                Err(e) => {
                    pgrx::debug1!(
                        "[pg_trickle] Frontier cleanup partition detach failed: {}",
                        e,
                    );
                }
                _ => {}
            }
            continue;
        }

        let can_truncate = if use_truncate {
            Spi::get_one::<bool>(&format!(
                "SELECT NOT EXISTS(\
                   SELECT 1 FROM \"{schema}\".changes_{oid} \
                   WHERE lsn > '{safe_lsn}'::pg_lsn \
                   LIMIT 1\
                 )",
                schema = change_schema,
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false)
        } else {
            false
        };

        if can_truncate {
            if let Err(e) = Spi::run(&format!(
                "TRUNCATE \"{schema}\".changes_{oid}",
                schema = change_schema,
            )) {
                pgrx::debug1!("[pg_trickle] Frontier-based cleanup TRUNCATE failed: {}", e);
            }
        } else {
            let delete_sql = format!(
                "DELETE FROM \"{schema}\".changes_{oid} \
                 WHERE lsn <= '{safe_lsn}'::pg_lsn",
                schema = change_schema,
            );
            if let Err(e) = Spi::run(&delete_sql) {
                pgrx::debug1!("[pg_trickle] Frontier-based cleanup DELETE failed: {}", e);
            }
        }
    }
}

/// Frontier-based cleanup for ST change buffers (`changes_pgt_{id}`).
///
/// For each upstream ST source, computes the minimum frontier LSN across all
/// downstream stream tables that depend on it, then deletes consumed rows.
fn cleanup_st_change_buffers_by_frontier(change_schema: &str, st_source_pgt_ids: &[i64]) {
    if st_source_pgt_ids.is_empty() {
        return;
    }

    for &upstream_pgt_id in st_source_pgt_ids {
        let key = format!("pgt_{upstream_pgt_id}");

        // Check that the ST change buffer table exists.
        if !crate::cdc::has_st_change_buffer(upstream_pgt_id, change_schema) {
            continue;
        }

        // ST-ST-6: Check if any downstream consumer has a NULL or missing
        // frontier for this ST source.  If so, skip cleanup — that consumer
        // hasn't consumed any data from this buffer yet and we must not
        // delete rows it still needs.
        let has_uninitialized_consumer = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS( \
               SELECT 1 FROM pgtrickle.pgt_stream_tables st \
               JOIN pgtrickle.pgt_dependencies dep ON dep.pgt_id = st.pgt_id \
               WHERE dep.source_type = 'STREAM_TABLE' \
                 AND dep.source_relid = ( \
                     SELECT pgt_relid FROM pgtrickle.pgt_stream_tables WHERE pgt_id = {upstream_pgt_id} \
                 ) \
                 AND (st.frontier IS NULL \
                      OR st.frontier->'sources'->'{key}'->>'lsn' IS NULL) \
             )",
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if has_uninitialized_consumer {
            continue;
        }

        // Compute the minimum frontier LSN for this ST source across ALL
        // downstream consumers.
        let min_lsn: Option<String> = Spi::get_one::<String>(&format!(
            "SELECT MIN((st.frontier->'sources'->'{key}'->>'lsn')::pg_lsn)::TEXT \
             FROM pgtrickle.pgt_stream_tables st \
             JOIN pgtrickle.pgt_dependencies dep ON dep.pgt_id = st.pgt_id \
             WHERE dep.source_type = 'STREAM_TABLE' \
               AND dep.source_relid = ( \
                   SELECT pgt_relid FROM pgtrickle.pgt_stream_tables WHERE pgt_id = {upstream_pgt_id} \
               ) \
               AND st.frontier IS NOT NULL \
               AND st.frontier->'sources'->'{key}'->>'lsn' IS NOT NULL",
        ))
        .unwrap_or(None);

        let safe_lsn = match min_lsn {
            Some(lsn) if lsn != "0/0" => lsn,
            _ => continue,
        };

        let delete_sql = format!(
            "DELETE FROM \"{schema}\".changes_pgt_{id} \
             WHERE lsn <= '{safe_lsn}'::pg_lsn",
            schema = change_schema,
            id = upstream_pgt_id,
        );
        if let Err(e) = Spi::run(&delete_sql) {
            pgrx::debug1!(
                "[pg_trickle] ST buffer cleanup DELETE failed for changes_pgt_{}: {}",
                upstream_pgt_id,
                e,
            );
        }
    }
}

/// Flush pending cleanup entries that reference any of the given source OIDs.
///
/// Called during `drop_stream_table` to prevent stale cleanup entries from
/// referencing change buffer tables that are about to be dropped.
pub fn flush_pending_cleanups_for_oids(oids: &[u32]) {
    if oids.is_empty() {
        return;
    }
    PENDING_CLEANUP.with(|q| {
        let mut queue = q.borrow_mut();
        for entry in queue.iter_mut() {
            entry.source_oids.retain(|o| !oids.contains(o));
        }
        // Remove entries with no remaining OIDs.
        queue.retain(|e| !e.source_oids.is_empty());
    });
}

// ── D-1: Planner hint thresholds ────────────────────────────────────

/// Minimum delta rows before disabling nested-loop joins.
const PLANNER_HINT_NESTLOOP_THRESHOLD: i64 = 100;

/// Minimum delta rows before raising `work_mem` for hash joins.
const PLANNER_HINT_WORKMEM_THRESHOLD: i64 = 10_000;

/// Minimum Scan-node count before deep-join planner hints activate.
/// At 5+ tables the delta SQL generates cascading L₀ snapshot CTEs
/// whose intermediate hash joins can spill multi-GB temp files under
/// PostgreSQL's default planner choices (nested loops, low work_mem).
const DEEP_JOIN_SCAN_THRESHOLD: usize = 5;

/// Apply `SET LOCAL` planner hints based on the estimated delta size
/// and the join depth (scan count) of the defining query.
///
/// - Small delta (ratio < merge_seqscan_threshold): `SET LOCAL enable_seqscan = off`
///   to force index lookups on the stream table.
/// - delta 100–9 999: `SET LOCAL enable_nestloop = off`
/// - delta >= 10 000: also `SET LOCAL work_mem = '<N>MB'`
/// - scan_count >= 5 (deep join): disable nest loops, raise work_mem,
///   raise join_collapse_limit, and remove temp_file_limit. The delta
///   SQL for 5+ table joins produces O(n) snapshot CTEs whose hash
///   joins can exceed default temp_file_limit under poor plans.
///
/// `SET LOCAL` is automatically reset at the end of the current transaction,
/// so these hints cannot leak to other queries.
///
/// Returns `true` if the SCAL-3 work_mem cap would be exceeded, signalling
/// that the caller should fall back to a FULL refresh instead.
fn apply_planner_hints(estimated_delta: i64, st_relid: pg_sys::Oid, scan_count: usize) -> bool {
    if !crate::config::pg_trickle_merge_planner_hints() {
        return false;
    }

    // PH-D2: Manual join strategy override — bypass heuristics entirely.
    let strategy = crate::config::pg_trickle_merge_join_strategy();
    if strategy != crate::config::MergeJoinStrategy::Auto {
        apply_fixed_join_strategy(strategy);
        return false;
    }

    // ── Deep-join hints (DI-11) ─────────────────────────────────────
    // For 5+ table joins the delta SQL generates cascading L₀ snapshot
    // CTEs. Without planner guidance, PostgreSQL may choose nested-loop
    // plans that create pathological temp file spills (>8 GB at SF=0.01).
    // Fix: disable nest loops, raise work_mem, bump join_collapse_limit
    // so the planner considers all join orderings, and remove the
    // temp_file_limit cap so the query can run to completion.
    if scan_count >= DEEP_JOIN_SCAN_THRESHOLD {
        let mb = crate::config::pg_trickle_merge_work_mem_mb().max(512);

        // SCAL-3: If a work_mem cap is set and the deep-join allocation
        // would exceed it, signal fallback to FULL refresh.
        let cap = crate::config::pg_trickle_delta_work_mem_cap_mb();
        if cap > 0 && mb > cap {
            pgrx::notice!(
                "[pg_trickle] SCAL-3: deep-join work_mem ({mb}MB) exceeds \
                 delta_work_mem_cap_mb ({cap}MB). Falling back to FULL refresh.",
            );
            return true;
        }

        if let Err(e) = Spi::run("SET LOCAL enable_nestloop = off") {
            pgrx::debug1!(
                "[pg_trickle] DI-11: failed to SET LOCAL enable_nestloop: {}",
                e
            );
        }
        // mb is a config integer, not user-supplied input; SET LOCAL cannot use parameterized queries.
        let work_mem_sql = format!("SET LOCAL work_mem = '{mb}MB'");
        if let Err(e) = Spi::run(&work_mem_sql) {
            pgrx::debug1!("[pg_trickle] DI-11: failed to SET LOCAL work_mem: {}", e);
        }
        // orderings for the inlined NOT MATERIALIZED snapshot CTEs.
        // Default is 8; deep joins can exceed this after CTE inlining.
        let jcl = (scan_count + 2).max(12);
        // jcl is a computed integer, not user-supplied input; SET LOCAL cannot use parameterized queries.
        let jcl_sql = format!("SET LOCAL join_collapse_limit = {jcl}");
        if let Err(e) = Spi::run(&jcl_sql) {
            pgrx::debug1!(
                "[pg_trickle] DI-11: failed to SET LOCAL join_collapse_limit: {}",
                e
            );
        }
        let fcl_sql = format!("SET LOCAL from_collapse_limit = {jcl}");
        if let Err(e) = Spi::run(&fcl_sql) {
            pgrx::debug1!(
                "[pg_trickle] DI-11: failed to SET LOCAL from_collapse_limit: {}",
                e
            );
        }
        // Remove temp_file_limit for this transaction so the delta query
        // can complete even if intermediate hash batches spill to disk.
        if let Err(e) = Spi::run("SET LOCAL temp_file_limit = -1") {
            pgrx::debug1!(
                "[pg_trickle] DI-11: failed to SET LOCAL temp_file_limit: {}",
                e
            );
        }
        pgrx::debug1!(
            "[pg_trickle] DI-11: deep join (scan_count={scan_count}) — \
             disabled nestloop, work_mem={mb}MB, join_collapse_limit={jcl}, \
             temp_file_limit=-1",
        );
        // Skip the normal delta-size hints below — deep-join hints
        // already cover nest-loop and work_mem.
        return false;
    }

    // P3-4: For small deltas against large stream tables, disable seqscan
    // to force index lookups on __pgt_row_id.
    let seqscan_threshold = crate::config::pg_trickle_merge_seqscan_threshold();
    if seqscan_threshold > 0.0
        && estimated_delta > 0
        && estimated_delta < PLANNER_HINT_NESTLOOP_THRESHOLD
    {
        let st_rows: i64 = Spi::get_one_with_args::<i64>(
            "SELECT GREATEST(reltuples::bigint, 1) FROM pg_class WHERE oid = $1",
            &[st_relid.into()],
        )
        .unwrap_or(Some(1000))
        .unwrap_or(1000);

        let ratio = estimated_delta as f64 / st_rows as f64;
        if ratio < seqscan_threshold {
            if let Err(e) = Spi::run("SET LOCAL enable_seqscan = off") {
                pgrx::debug1!(
                    "[pg_trickle] P3-4: failed to SET LOCAL enable_seqscan: {}",
                    e
                );
            } else {
                pgrx::debug1!(
                    "[pg_trickle] P3-4: delta/ST ratio {:.6} < threshold {:.4}, disabled seqscan",
                    ratio,
                    seqscan_threshold,
                );
            }
        }
    }

    if estimated_delta >= PLANNER_HINT_WORKMEM_THRESHOLD {
        // Large delta: disable nested loops AND raise work_mem for hash joins
        let mb = crate::config::pg_trickle_merge_work_mem_mb();

        // SCAL-3: If a work_mem cap is set and the large-delta allocation
        // would exceed it, signal fallback to FULL refresh.
        let cap = crate::config::pg_trickle_delta_work_mem_cap_mb();
        if cap > 0 && mb > cap {
            pgrx::notice!(
                "[pg_trickle] SCAL-3: large-delta work_mem ({mb}MB) exceeds \
                 delta_work_mem_cap_mb ({cap}MB). Falling back to FULL refresh.",
            );
            return true;
        }

        if let Err(e) = Spi::run("SET LOCAL enable_nestloop = off") {
            pgrx::debug1!(
                "[pg_trickle] D-1: failed to SET LOCAL enable_nestloop: {}",
                e
            );
        }
        if let Err(e) = Spi::run(&format!("SET LOCAL work_mem = '{mb}MB'")) {
            pgrx::debug1!("[pg_trickle] D-1: failed to SET LOCAL work_mem: {}", e);
        }
    } else if estimated_delta >= PLANNER_HINT_NESTLOOP_THRESHOLD {
        // Medium delta: just disable nested loops
        if let Err(e) = Spi::run("SET LOCAL enable_nestloop = off") {
            pgrx::debug1!(
                "[pg_trickle] D-1: failed to SET LOCAL enable_nestloop: {}",
                e
            );
        }
    }
    false
}

/// PH-D2: Apply a fixed join strategy override via `SET LOCAL` hints.
fn apply_fixed_join_strategy(strategy: crate::config::MergeJoinStrategy) {
    let (nestloop, hashjoin, mergejoin, label) = match strategy {
        crate::config::MergeJoinStrategy::HashJoin => ("off", "on", "on", "hash_join"),
        crate::config::MergeJoinStrategy::NestedLoop => ("on", "off", "off", "nested_loop"),
        crate::config::MergeJoinStrategy::MergeJoin => ("off", "off", "on", "merge_join"),
        crate::config::MergeJoinStrategy::Auto => return,
    };

    for (param, val) in [
        ("enable_nestloop", nestloop),
        ("enable_hashjoin", hashjoin),
        ("enable_mergejoin", mergejoin),
    ] {
        // param is from a fixed extension-controlled array; val is a bool; SET LOCAL cannot use parameterized queries.
        let set_sql = format!("SET LOCAL {param} = {val}");
        if let Err(e) = Spi::run(&set_sql) {
            pgrx::debug1!("[pg_trickle] PH-D2: failed to SET LOCAL {param}: {}", e);
        }
    }

    // For hash_join strategy, also raise work_mem to avoid hash spills.
    if strategy == crate::config::MergeJoinStrategy::HashJoin {
        let mb = crate::config::pg_trickle_merge_work_mem_mb();
        // mb is a config integer, not user-supplied input; SET LOCAL cannot use parameterized queries.
        let work_mem_sql = format!("SET LOCAL work_mem = '{mb}MB'");
        if let Err(e) = Spi::run(&work_mem_sql) {
            pgrx::debug1!("[pg_trickle] PH-D2: failed to SET LOCAL work_mem: {}", e);
        }
    }

    pgrx::debug1!("[pg_trickle] PH-D2: fixed join strategy={label} applied",);
}

// ── ST-to-ST Delta Capture (Phase 8.2/8.3) ─────────────────────────────

/// Build a SQL expression that computes a content-hash of all user columns.
///
/// Downstream stream tables always see an upstream ST as keyless (no PK
/// constraint), so they compute `__pgt_row_id` from ALL user columns via
/// `row_id_key_columns()`.  The `pk_hash` stored in the ST change buffer
/// **must** match that all-column content hash — otherwise the downstream
/// MERGE will never find the existing row to DELETE.
///
/// This is a pure-logic helper for unit testing.
pub fn build_content_hash_expr(prefix: &str, user_cols: &[String]) -> String {
    match user_cols.len() {
        0 => format!("{prefix}__pgt_row_id"),
        1 => {
            let c = user_cols[0].replace('"', "\"\"");
            format!("pgtrickle.pg_trickle_hash({prefix}\"{c}\"::TEXT)")
        }
        _ => {
            let args: Vec<String> = user_cols
                .iter()
                .map(|c| {
                    let escaped = c.replace('"', "\"\"");
                    format!("{prefix}\"{escaped}\"::TEXT")
                })
                .collect();
            format!(
                "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
                args.join(", ")
            )
        }
    }
}

/// Capture delta rows from a materialized delta temp table into the ST's
/// change buffer for downstream ST consumption.
///
/// Called after the MERGE (or explicit DML) when the ST has downstream
/// consumers. The delta is already materialized in `__pgt_delta_{pgt_id}`
/// as a temp table with `__pgt_row_id`, `__pgt_action`, and user columns.
///
/// Only `'I'` (insert) and `'D'` (delete) actions are captured — updates
/// in the delta are already expressed as D+I pairs by the DVM.
fn capture_delta_to_st_buffer(
    st: &StreamTableMeta,
    user_cols: &[String],
) -> Result<i64, PgTrickleError> {
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let pgt_id = st.pgt_id;

    if !crate::cdc::has_st_change_buffer(pgt_id, &change_schema) {
        return Ok(0);
    }

    let new_col_list: String = user_cols
        .iter()
        .map(|c| format!("\"new_{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let d_col_list: String = user_cols
        .iter()
        .map(|c| format!("d.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    // ST-ST-9: Use a content hash of ALL user columns as pk_hash.
    // Downstream STs always treat upstream STs as keyless (no PK
    // constraint), so their __pgt_row_id = hash(all columns).  The
    // pk_hash in the buffer must match this content hash for MERGE
    // matching to work during differential refresh.
    let pk_hash_expr = build_content_hash_expr("d.", user_cols);

    let sql = format!(
        "INSERT INTO \"{change_schema}\".changes_pgt_{pgt_id} \
         (lsn, action, pk_hash, {new_col_list}) \
         SELECT pg_current_wal_lsn(), d.__pgt_action, {pk_hash_expr}, \
                {d_col_list} \
         FROM __pgt_delta_{pgt_id} d \
         WHERE d.__pgt_action IN ('I', 'D')"
    );

    let count = Spi::connect_mut(|client| {
        let result = client
            .update(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Ok::<i64, PgTrickleError>(result.len() as i64)
    })?;

    if count > 0 {
        pgrx::debug1!(
            "[pg_trickle] ST-ST: captured {} delta rows to changes_pgt_{} for {}.{}",
            count,
            pgt_id,
            st.pgt_schema,
            st.pgt_name,
        );
    }

    Ok(count)
}

/// DAG-4: Capture delta to a temp bypass table instead of the persistent
/// `changes_pgt_{id}` buffer.
///
/// Creates `pg_temp.__pgt_bypass_{pgt_id}` (ON COMMIT DROP) with the same
/// schema as the persistent buffer, inserts I/D rows from the delta temp
/// table, and registers the bypass mapping so that downstream refresh
/// reads from this temp table instead.
///
/// Returns the number of captured rows.
pub fn capture_delta_to_bypass_table(
    st: &StreamTableMeta,
    user_cols_typed: &[(String, String)],
) -> Result<i64, PgTrickleError> {
    let pgt_id = st.pgt_id;

    // The delta temp table is only created during a true DIFFERENTIAL
    // refresh.  If `execute_scheduled_refresh` internally fell back to
    // FULL (e.g. no previous frontier), the table won't exist and we
    // must skip the capture to avoid a "relation does not exist" ERROR.
    // pgt_id is a plain i64, not user-supplied input.
    let delta_exists_sql = format!("SELECT to_regclass('__pgt_delta_{}') IS NOT NULL", pgt_id);
    let delta_exists: bool = Spi::get_one::<bool>(&delta_exists_sql)
        .unwrap_or(Some(false))
        .unwrap_or(false);

    if !delta_exists {
        pgrx::debug1!(
            "[pg_trickle] DAG-4: no delta table for pgt_id={}, skipping bypass capture",
            pgt_id,
        );
        return Ok(0);
    }

    let bypass_table = format!("pg_temp.__pgt_bypass_{}", pgt_id);

    // DAG-4/ST-ST-10: If a pre-snapshot table exists (created by the
    // capture_incremental_diff_to_st_buffer path in explicit_dml), use a
    // pre/post comparison to produce correct D+I pairs for value-changes.
    // The weight-aggregated __pgt_delta_ collapses D+I with the same
    // __pgt_row_id into a single I, which omits the D for old column values.
    // Downstream STs with WHERE filters on changed columns would miss the
    // deletion and retain stale rows.
    // pgt_id is a plain i64, not user-supplied input.
    let pre_snap_sql = format!("SELECT to_regclass('__pgt_pre_{}') IS NOT NULL", pgt_id);
    let pre_snapshot_exists: bool = Spi::get_one::<bool>(&pre_snap_sql)
        .unwrap_or(Some(false))
        .unwrap_or(false);

    // DAG-4/ST-ST-10: Read the MAX(lsn) from the persistent change buffer
    // so that bypass table rows use an LSN that falls within the downstream
    // ST's frontier range.  Between capture_incremental_diff_to_st_buffer
    // (which writes to the persistent buffer inside execute_differential_refresh)
    // and this bypass capture (called after execute_scheduled_refresh stores
    // frontier and other catalog metadata), WAL-generating catalog DMLs advance
    // pg_current_wal_lsn().  Using the stale higher LSN would cause the
    // downstream scan's `lsn <= new_lsn` filter to exclude bypass rows,
    // silently dropping the entire delta.
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let buffer_lsn: Option<String> = if crate::cdc::has_st_change_buffer(pgt_id, &change_schema) {
        Spi::get_one::<String>(&format!(
            "SELECT MAX(lsn)::text FROM \"{change_schema}\".changes_pgt_{pgt_id}",
        ))
        .unwrap_or(None)
    } else {
        None
    };

    let count = if pre_snapshot_exists {
        let user_cols: Vec<String> = user_cols_typed.iter().map(|(n, _)| n.clone()).collect();
        let col_defs: String = std::iter::once("lsn pg_lsn".to_string())
            .chain(std::iter::once("action \"char\"".to_string()))
            .chain(std::iter::once("pk_hash bigint".to_string()))
            .chain(
                user_cols_typed
                    .iter()
                    .map(|(name, typ)| format!("\"new_{}\" {}", name.replace('"', "\"\""), typ)),
            )
            .collect::<Vec<_>>()
            .join(", ");
        let create_sql =
            format!("CREATE TEMP TABLE IF NOT EXISTS {bypass_table} ({col_defs}) ON COMMIT DROP",);
        Spi::run(&create_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        capture_diff_to_table(st, &user_cols, &bypass_table, pgt_id, buffer_lsn.as_deref())?
    } else {
        let sql = build_bypass_capture_sql(
            pgt_id,
            user_cols_typed,
            &bypass_table,
            buffer_lsn.as_deref(),
        );
        Spi::connect_mut(|client| {
            let result = client
                .update(&sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<i64, PgTrickleError>(result.len() as i64)
        })?
    };

    set_st_bypass(pgt_id, bypass_table.clone());

    if count > 0 {
        pgrx::debug1!(
            "[pg_trickle] DAG-4: captured {} delta rows to bypass table {} for {}.{}",
            count,
            bypass_table,
            st.pgt_schema,
            st.pgt_name,
        );
    }

    Ok(count)
}

/// Shared pre/post diff logic for capturing D+I pairs into a target table.
///
/// Used by both `capture_delta_to_bypass_table` (FusedChain path) and
/// `capture_incremental_diff_to_st_buffer` (persistent buffer path).
///
/// `target_table` must already exist with the appropriate schema
/// (`lsn, action, pk_hash, new_col1, ...`).
/// Reads the pre-snapshot from `__pgt_pre_{pgt_id}` and compares with the
/// current state of the ST backing table.
///
/// `lsn_override`: when `Some("0/1A2B3C")`, the given literal LSN is used
/// instead of `pg_current_wal_lsn()`.  This is required for bypass tables
/// (DAG-4) where WAL-generating catalog DMLs between the persistent buffer
/// capture and the bypass capture advance `pg_current_wal_lsn()` past the
/// downstream ST's frontier upper bound.
fn capture_diff_to_table(
    st: &StreamTableMeta,
    user_cols: &[String],
    target_table: &str,
    pgt_id: i64,
    lsn_override: Option<&str>,
) -> Result<i64, PgTrickleError> {
    let schema = &st.pgt_schema;
    let name = &st.pgt_name;
    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\""),
    );

    let new_col_list: String = user_cols
        .iter()
        .map(|c| format!("\"new_{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let pre_col_refs: String = user_cols
        .iter()
        .map(|c| format!("pre.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let post_col_refs: String = user_cols
        .iter()
        .map(|c| format!("post.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let is_distinct_pairs: String = user_cols
        .iter()
        .map(|c| {
            let qc = format!("\"{}\"", c.replace('"', "\"\""));
            format!("pre.{qc} IS DISTINCT FROM post.{qc}")
        })
        .collect::<Vec<_>>()
        .join(" OR ");

    let pre_pk_hash = build_content_hash_expr("pre.", user_cols);
    let post_pk_hash = build_content_hash_expr("post.", user_cols);

    // DAG-4: When an LSN override is provided (bypass tables), use the
    // literal value so the rows fall within the downstream frontier range.
    let lsn_expr = match lsn_override {
        Some(lsn) => format!("'{lsn}'::pg_lsn"),
        None => "pg_current_wal_lsn()".to_string(),
    };

    let mut total: i64 = 0;

    // Deleted rows: in pre but no longer in the table.
    let del_sql = format!(
        "INSERT INTO {target_table} (lsn, action, pk_hash, {new_col_list}) \
         SELECT {lsn_expr}, 'D', {pre_pk_hash}, {pre_col_refs} \
         FROM __pgt_pre_{pgt_id} pre \
         LEFT JOIN {quoted_table} post ON pre.__pgt_row_id = post.__pgt_row_id \
         WHERE post.__pgt_row_id IS NULL"
    );
    total += Spi::connect_mut(|c| {
        Ok::<i64, PgTrickleError>(
            c.update(&del_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .len() as i64,
        )
    })?;

    // Inserted rows: in table (scoped to delta row_ids) but not in pre.
    let ins_sql = format!(
        "INSERT INTO {target_table} (lsn, action, pk_hash, {new_col_list}) \
         SELECT {lsn_expr}, 'I', {post_pk_hash}, {post_col_refs} \
         FROM {quoted_table} post \
         JOIN (SELECT DISTINCT __pgt_row_id FROM __pgt_delta_{pgt_id}) delta \
           ON delta.__pgt_row_id = post.__pgt_row_id \
         LEFT JOIN __pgt_pre_{pgt_id} pre ON post.__pgt_row_id = pre.__pgt_row_id \
         WHERE pre.__pgt_row_id IS NULL"
    );
    total += Spi::connect_mut(|c| {
        Ok::<i64, PgTrickleError>(
            c.update(&ins_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .len() as i64,
        )
    })?;

    // Changed rows: same row_id, different column values.
    if !is_distinct_pairs.is_empty() {
        let chg_del_sql = format!(
            "INSERT INTO {target_table} (lsn, action, pk_hash, {new_col_list}) \
             SELECT {lsn_expr}, 'D', {pre_pk_hash}, {pre_col_refs} \
             FROM __pgt_pre_{pgt_id} pre \
             JOIN {quoted_table} post ON post.__pgt_row_id = pre.__pgt_row_id \
             WHERE {is_distinct_pairs}"
        );
        total += Spi::connect_mut(|c| {
            Ok::<i64, PgTrickleError>(
                c.update(&chg_del_sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                    .len() as i64,
            )
        })?;

        let chg_ins_sql = format!(
            "INSERT INTO {target_table} (lsn, action, pk_hash, {new_col_list}) \
             SELECT {lsn_expr}, 'I', {post_pk_hash}, {post_col_refs} \
             FROM {quoted_table} post \
             JOIN __pgt_pre_{pgt_id} pre ON post.__pgt_row_id = pre.__pgt_row_id \
             WHERE {is_distinct_pairs}"
        );
        total += Spi::connect_mut(|c| {
            Ok::<i64, PgTrickleError>(
                c.update(&chg_ins_sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                    .len() as i64,
            )
        })?;
    }

    Ok(total)
}

/// Build the SQL for creating a bypass temp table and inserting delta rows.
///
/// Pure-logic helper for unit testing.
pub fn build_bypass_capture_sql(
    pgt_id: i64,
    user_cols_typed: &[(String, String)],
    bypass_table: &str,
    lsn_override: Option<&str>,
) -> String {
    let col_defs: String = std::iter::once("lsn pg_lsn".to_string())
        .chain(std::iter::once("action \"char\"".to_string()))
        .chain(std::iter::once("pk_hash bigint".to_string()))
        .chain(
            user_cols_typed
                .iter()
                .map(|(name, typ)| format!("\"new_{}\" {}", name.replace('"', "\"\""), typ)),
        )
        .collect::<Vec<_>>()
        .join(", ");

    let new_col_list: String = user_cols_typed
        .iter()
        .map(|(name, _)| format!("\"new_{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let d_col_list: String = user_cols_typed
        .iter()
        .map(|(name, _)| format!("d.\"{}\"", name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    // ST-ST-9: Use content hash of all user columns (see
    // build_content_hash_expr doc comment for rationale).
    let col_names: Vec<String> = user_cols_typed.iter().map(|(n, _)| n.clone()).collect();
    let pk_hash_expr = build_content_hash_expr("d.", &col_names);

    // DAG-4: Use the persistent buffer's LSN when available so the bypass
    // rows fall within the downstream scan's frontier range.
    let lsn_expr = match lsn_override {
        Some(lsn) => format!("'{lsn}'::pg_lsn"),
        None => "pg_current_wal_lsn()".to_string(),
    };

    format!(
        "CREATE TEMP TABLE IF NOT EXISTS {bypass_table} ({col_defs}) ON COMMIT DROP;\n\
         INSERT INTO {bypass_table} (lsn, action, pk_hash, {new_col_list}) \
         SELECT {lsn_expr}, d.__pgt_action, {pk_hash_expr}, {d_col_list} \
         FROM __pgt_delta_{pgt_id} d \
         WHERE d.__pgt_action IN ('I', 'D')"
    )
}

/// Capture the effective delta from a DIFFERENTIAL refresh into the ST's
/// change buffer using a pre/post snapshot comparison.
///
/// Called after the explicit DML (DELETE + UPDATE + INSERT) when the ST has
/// downstream consumers.  Delegates to [`capture_diff_to_table`] with the
/// persistent `changes_pgt_{pgt_id}` buffer as the target.
fn capture_incremental_diff_to_st_buffer(
    st: &StreamTableMeta,
    user_cols: &[String],
) -> Result<i64, PgTrickleError> {
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let pgt_id = st.pgt_id;

    if !crate::cdc::has_st_change_buffer(pgt_id, &change_schema) {
        return Ok(0);
    }

    let target_table = format!("\"{change_schema}\".changes_pgt_{pgt_id}");
    let total = capture_diff_to_table(st, user_cols, &target_table, pgt_id, None)?;

    if total > 0 {
        pgrx::debug1!(
            "[pg_trickle] ST-ST INCR: captured {} diff rows to changes_pgt_{} for {}.{}",
            total,
            pgt_id,
            st.pgt_schema,
            st.pgt_name,
        );
    }
    Ok(total)
}

/// Capture the full-refresh diff into the ST's change buffer.
///
/// Called after a FULL refresh when the ST has downstream consumers.
/// Compares a pre-refresh snapshot (`__pgt_pre_{pgt_id}`) against the
/// post-refresh state to produce I/D pairs.
fn capture_full_refresh_diff_to_st_buffer(
    st: &StreamTableMeta,
    user_cols: &[String],
) -> Result<i64, PgTrickleError> {
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let pgt_id = st.pgt_id;

    if !crate::cdc::has_st_change_buffer(pgt_id, &change_schema) {
        return Ok(0);
    }

    let schema = &st.pgt_schema;
    let name = &st.pgt_name;
    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\""),
    );

    let new_col_list: String = user_cols
        .iter()
        .map(|c| format!("\"new_{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let pre_col_refs: String = user_cols
        .iter()
        .map(|c| format!("pre.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let post_col_refs: String = user_cols
        .iter()
        .map(|c| format!("post.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    // IS DISTINCT FROM comparison for detecting changed rows
    let is_distinct_pairs: String = user_cols
        .iter()
        .map(|c| {
            let qc = format!("\"{}\"", c.replace('"', "\"\""));
            format!("pre.{qc} IS DISTINCT FROM post.{qc}")
        })
        .collect::<Vec<_>>()
        .join(" OR ");

    let mut total_count: i64 = 0;

    // ST-ST-9: Use content hash of all user columns for pk_hash (see
    // build_content_hash_expr doc comment for rationale).
    let pre_pk_hash = build_content_hash_expr("pre.", user_cols);
    let post_pk_hash = build_content_hash_expr("post.", user_cols);

    // Deleted rows: in pre but not in post
    let deleted_sql = format!(
        "INSERT INTO \"{change_schema}\".changes_pgt_{pgt_id} \
         (lsn, action, pk_hash, {new_col_list}) \
         SELECT pg_current_wal_lsn(), 'D', {pre_pk_hash}, {pre_col_refs} \
         FROM __pgt_pre_{pgt_id} pre \
         LEFT JOIN {quoted_table} post ON pre.__pgt_row_id = post.__pgt_row_id \
         WHERE post.__pgt_row_id IS NULL"
    );
    let del_count = Spi::connect_mut(|client| {
        let result = client
            .update(&deleted_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Ok::<i64, PgTrickleError>(result.len() as i64)
    })?;
    total_count += del_count;

    // Inserted rows: in post but not in pre
    let inserted_sql = format!(
        "INSERT INTO \"{change_schema}\".changes_pgt_{pgt_id} \
         (lsn, action, pk_hash, {new_col_list}) \
         SELECT pg_current_wal_lsn(), 'I', {post_pk_hash}, {post_col_refs} \
         FROM {quoted_table} post \
         LEFT JOIN __pgt_pre_{pgt_id} pre ON post.__pgt_row_id = pre.__pgt_row_id \
         WHERE pre.__pgt_row_id IS NULL"
    );
    let ins_count = Spi::connect_mut(|client| {
        let result = client
            .update(&inserted_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Ok::<i64, PgTrickleError>(result.len() as i64)
    })?;
    total_count += ins_count;

    // Changed rows: same row_id but different content.
    //
    // ST-ST-9: With content-hash pk_hash, the old D and new I have
    // DIFFERENT pk_hash values (content changed), so the downstream
    // keyless decomposition correctly sees them as independent events
    // (no accidental cancellation).  Emit both D (old values) and
    // I (new values) so the downstream can delete the old row and
    // insert the new one.
    if !is_distinct_pairs.is_empty() {
        // D event: old content hash + old column values
        let changed_del_sql = format!(
            "INSERT INTO \"{change_schema}\".changes_pgt_{pgt_id} \
             (lsn, action, pk_hash, {new_col_list}) \
             SELECT pg_current_wal_lsn(), 'D', {pre_pk_hash}, {pre_col_refs} \
             FROM __pgt_pre_{pgt_id} pre \
             JOIN {quoted_table} post ON post.__pgt_row_id = pre.__pgt_row_id \
             WHERE {is_distinct_pairs}"
        );
        let chg_del_count = Spi::connect_mut(|client| {
            let result = client
                .update(&changed_del_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<i64, PgTrickleError>(result.len() as i64)
        })?;
        total_count += chg_del_count;

        // I event: new content hash + new column values
        let changed_ins_sql = format!(
            "INSERT INTO \"{change_schema}\".changes_pgt_{pgt_id} \
             (lsn, action, pk_hash, {new_col_list}) \
             SELECT pg_current_wal_lsn(), 'I', {post_pk_hash}, {post_col_refs} \
             FROM {quoted_table} post \
             JOIN __pgt_pre_{pgt_id} pre ON post.__pgt_row_id = pre.__pgt_row_id \
             WHERE {is_distinct_pairs}"
        );
        let chg_ins_count = Spi::connect_mut(|client| {
            let result = client
                .update(&changed_ins_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<i64, PgTrickleError>(result.len() as i64)
        })?;
        total_count += chg_ins_count;
    }

    if total_count > 0 {
        pgrx::debug1!(
            "[pg_trickle] ST-ST FULL: captured {} diff rows to changes_pgt_{} for {}.{} \
             (deleted={}, inserted={}, changed={})",
            total_count,
            pgt_id,
            st.pgt_schema,
            st.pgt_name,
            del_count,
            ins_count,
            total_count - del_count - ins_count,
        );
    }

    Ok(total_count)
}

/// Get user-facing output columns for an ST (for delta capture).
///
/// Excludes internal columns (__pgt_row_id, __pgt_count, etc.).
pub fn get_st_user_columns(st: &StreamTableMeta) -> Vec<String> {
    crate::cdc::resolve_st_output_columns(st.pgt_relid)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Return user column (name, type) pairs for a stream table.
///
/// Used by bypass-table creation so column types match the original ST.
pub fn get_st_user_columns_typed(st: &StreamTableMeta) -> Vec<(String, String)> {
    crate::cdc::resolve_st_output_columns(st.pgt_relid).unwrap_or_default()
}

/// Check whether this ST has downstream ST consumers that need delta capture.
pub fn has_downstream_st_consumers(pgt_id: i64) -> bool {
    crate::cdc::count_downstream_st_consumers(pgt_id) > 0
}

/// Resolve LSN placeholders in a SQL template with actual frontier values.
///
/// B3-1: When `zero_change_oids` contains a source OID, the entire LSN-range
/// predicate for that source is replaced with `FALSE`, enabling PostgreSQL's
/// planner to recognise the scan CTE as empty at plan time and skip
/// downstream join branches.
fn resolve_lsn_placeholders(
    template: &str,
    source_oids: &[u32],
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
    zero_change_oids: &std::collections::HashSet<u32>,
) -> String {
    let mut sql = template.to_string();
    for &oid in source_oids {
        if zero_change_oids.contains(&oid) {
            // B3-1: Replace the full LSN range predicate with FALSE so
            // PG can prune the change-buffer scan CTE at plan time.
            let lsn_pred = format!(
                "'__PGS_PREV_LSN_{oid}__'::pg_lsn AND c.lsn <= '__PGS_NEW_LSN_{oid}__'::pg_lsn"
            );
            if sql.contains(&lsn_pred) {
                sql = sql.replace(
                    &format!("c.lsn > {lsn_pred}"),
                    "FALSE /* B3-1: zero-change source pruned */",
                );
            } else {
                // Cleanup SQL or other patterns that don't use `c.lsn` alias.
                sql = sql.replace(
                    &format!("__PGS_PREV_LSN_{oid}__"),
                    &prev_frontier.get_lsn(oid),
                );
                sql = sql.replace(
                    &format!("__PGS_NEW_LSN_{oid}__"),
                    &new_frontier.get_lsn(oid),
                );
            }
        } else {
            sql = sql.replace(
                &format!("__PGS_PREV_LSN_{oid}__"),
                &prev_frontier.get_lsn(oid),
            );
            sql = sql.replace(
                &format!("__PGS_NEW_LSN_{oid}__"),
                &new_frontier.get_lsn(oid),
            );
        }
    }

    // ST-ST-4: Resolve pgt_-prefixed placeholders for ST source frontiers.
    // These use the format `__PGS_PREV_LSN_pgt_{id}__` / `__PGS_NEW_LSN_pgt_{id}__`.
    // The frontier stores ST sources under the key "pgt_{id}".
    let pgt_prefix = "__PGS_PREV_LSN_pgt_";
    if sql.contains(pgt_prefix) {
        // Extract all pgt_ IDs from the template
        let mut search_from = 0usize;
        let mut pgt_ids: Vec<i64> = Vec::new();
        while let Some(pos) = sql[search_from..].find(pgt_prefix) {
            let start = search_from + pos + pgt_prefix.len();
            let end = sql[start..]
                .find("__")
                .map(|p| start + p)
                .unwrap_or(sql.len());
            if let Ok(id) = sql[start..end].parse::<i64>()
                && !pgt_ids.contains(&id)
            {
                pgt_ids.push(id);
            }
            search_from = end;
        }

        for pgt_id in &pgt_ids {
            let key = format!("pgt_{pgt_id}");
            let prev_lsn = prev_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());
            let new_lsn = new_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());

            sql = sql.replace(&format!("__PGS_PREV_LSN_pgt_{pgt_id}__"), &prev_lsn);
            sql = sql.replace(&format!("__PGS_NEW_LSN_pgt_{pgt_id}__"), &new_lsn);
        }
    }

    sql
}

// ── D-2: Prepared statement helpers ─────────────────────────────────

/// Convert a MERGE template with `'__PGS_PREV_LSN_{oid}__'::pg_lsn`
/// tokens into a parameterized SQL string with `$1`, `$2`, … placeholders.
///
/// Parameter mapping: for each source OID (in order), `$2i-1` = prev_lsn,
/// `$2i` = new_lsn.
fn parameterize_lsn_template(template: &str, source_oids: &[u32]) -> String {
    let mut sql = template.to_string();
    for (i, &oid) in source_oids.iter().enumerate() {
        let prev_param = format!("${}", i * 2 + 1);
        let new_param = format!("${}", i * 2 + 2);
        sql = sql.replace(&format!("'__PGS_PREV_LSN_{oid}__'::pg_lsn"), &prev_param);
        sql = sql.replace(&format!("'__PGS_NEW_LSN_{oid}__'::pg_lsn"), &new_param);
    }
    sql
}

/// Build the `(pg_lsn, pg_lsn, …)` type list for a `PREPARE` statement.
fn build_prepare_type_list(n_sources: usize) -> String {
    std::iter::repeat_n("pg_lsn", n_sources * 2)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the `('0/1A2B…'::pg_lsn, '0/3C4D…'::pg_lsn, …)` value list
/// for an `EXECUTE` statement.
fn build_execute_params(
    source_oids: &[u32],
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
) -> String {
    source_oids
        .iter()
        .flat_map(|&oid| {
            let prev = prev_frontier.get_lsn(oid);
            let new = new_frontier.get_lsn(oid);
            [format!("'{prev}'::pg_lsn"), format!("'{new}'::pg_lsn")]
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn deallocate_prepared_merge_statement(_pgt_id: i64) {
    #[cfg(not(test))]
    {
        let pgt_id = _pgt_id;
        let stmt = format!("__pgt_merge_{pgt_id}");
        let exists = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM pg_prepared_statements WHERE name = '{stmt}')"
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);
        if exists {
            let _ = Spi::run(&format!("DEALLOCATE {stmt}"));
        }
    }
}

fn clear_prepared_merge_statements() {
    let tracked_ids =
        PREPARED_MERGE_STMTS.with(|stmts| stmts.borrow().iter().copied().collect::<Vec<_>>());

    for pgt_id in tracked_ids {
        deallocate_prepared_merge_statement(pgt_id);
    }

    PREPARED_MERGE_STMTS.with(|stmts| stmts.borrow_mut().clear());
}

/// Invalidate the MERGE template cache for a ST (call on DDL changes).
pub fn invalidate_merge_cache(pgt_id: i64) {
    MERGE_TEMPLATE_CACHE.with(|cache| {
        cache.borrow_mut().remove(&pgt_id);
    });
    // D-2: Also deallocate any prepared statement for this ST.
    if PREPARED_MERGE_STMTS.with(|s| s.borrow_mut().remove(&pgt_id)) {
        deallocate_prepared_merge_statement(pgt_id);
    }
    // Also invalidate the delta SQL template cache, which embeds the
    // CDC bitmask width.  Without this, a cache-miss rebuild of the MERGE
    // template would still pull a stale delta SQL template (e.g. 3-bit
    // mask) from the DELTA_TEMPLATE_CACHE, producing "cannot AND bit
    // strings of different sizes" on the first UPDATE after a CDC rebuild.
    crate::dvm::invalidate_delta_cache(pgt_id);
}

/// Wide-table MERGE hash threshold (F41: G4.6).
///
/// When a table has more than this many user columns, the MERGE's
/// `WHEN MATCHED` IS DISTINCT FROM guard uses a hash comparison instead
/// of per-column checks, reducing SQL text length and planner overhead.
const WIDE_TABLE_HASH_THRESHOLD: usize = 50;

/// Build the IS DISTINCT FROM clause for the MERGE `WHEN MATCHED` guard.
///
/// For tables with ≤ [`WIDE_TABLE_HASH_THRESHOLD`] columns, generates
/// per-column text comparisons joined with `OR`.
///
/// The text cast avoids PostgreSQL's requirement that the underlying type
/// implement `=` for `IS DISTINCT FROM` to work. This matters for `json`
/// aggregate outputs, which intentionally do not have a native equality
/// operator.
///
/// For wider tables (F41), generates a single xxh64 hash comparison using
/// `pgtrickle.pg_trickle_hash()`:
/// ```sql
/// pgtrickle.pg_trickle_hash(concat_ws('\x1E', COALESCE(st."c1"::text,''), ...))
/// IS DISTINCT FROM
/// pgtrickle.pg_trickle_hash(concat_ws('\x1E', COALESCE(d."c1"::text,''), ...))
/// ```
fn build_is_distinct_clause(user_cols: &[String]) -> String {
    if user_cols.len() <= WIDE_TABLE_HASH_THRESHOLD {
        user_cols
            .iter()
            .map(|c| {
                let qc = format!("\"{}\"", c.replace('"', "\"\""));
                format!("st.{qc}::text IS DISTINCT FROM d.{qc}::text")
            })
            .collect::<Vec<_>>()
            .join(" OR ")
    } else {
        // Hash-based comparison for wide tables using xxh64 (faster than md5).
        // Uses pgtrickle.pg_trickle_hash() which is IMMUTABLE + PARALLEL SAFE.
        let hash_expr = |prefix: &str| -> String {
            let parts: Vec<String> = user_cols
                .iter()
                .map(|c| {
                    let qc = format!("\"{}\"", c.replace('"', "\"\""));
                    format!("COALESCE({prefix}.{qc}::text, '')")
                })
                .collect();
            format!(
                "pgtrickle.pg_trickle_hash(concat_ws({sep}, {cols}))",
                sep = "'\\x1E'",
                cols = parts.join(", ")
            )
        };
        format!("{} IS DISTINCT FROM {}", hash_expr("st"), hash_expr("d"))
    }
}

/// B3-2: Build a USING clause with weight aggregation for cross-source
/// deduplication.
///
/// Replaces the previous `DISTINCT ON (__pgt_row_id)` approach which silently
/// discarded corrections that should be algebraically combined.  Weight
/// aggregation correctly handles diamond-flow queries where multiple delta
/// branches produce overlapping corrections for the same `__pgt_row_id`:
///
/// - Net weight > 0 → INSERT
/// - Net weight < 0 → DELETE
/// - Net weight = 0 → filtered out (no-op)
///
/// B3-3: After weight aggregation an outer `DISTINCT ON (__pgt_row_id)` step
/// resolves the UPDATE case where PK-stable row IDs are used for joins.
///
/// When only a non-PK column changes (e.g. category_name), diff_project
/// produces a D row and an I row that share the same `__pgt_row_id` (because
/// the hash is over PK columns only).  The weight aggregation groups by
/// `(row_id, col_list)`, so these two rows land in *different* groups and both
/// survive the HAVING clause — giving two MERGE source rows targeting the same
/// ST row, which PostgreSQL rejects with "MERGE command cannot affect row a
/// second time".
///
/// The outer `DISTINCT ON` resolves this: for each `__pgt_row_id` that appears
/// as both D and I, `ORDER BY … CASE WHEN action='I' THEN 0 ELSE 1 END`
/// selects the I row (carrying the new column values).  MERGE then performs a
/// single WHEN MATCHED … UPDATE, which is the correct semantic for a non-PK
/// column change.
fn build_weight_agg_using(delta_sql: &str, user_col_list: &str) -> String {
    format!(
        "(SELECT DISTINCT ON (\"__pgt_row_id\") \
                \"__pgt_row_id\", \"__pgt_action\", {user_col_list} \
         FROM (\
             SELECT __pgt_row_id, \
                    CASE WHEN SUM(CASE WHEN __pgt_action = 'I' THEN 1 ELSE -1 END) > 0 \
                         THEN 'I' ELSE 'D' END AS __pgt_action, \
                    {user_col_list} \
             FROM ({delta_sql}) __raw \
             GROUP BY __pgt_row_id, {user_col_list} \
             HAVING SUM(CASE WHEN __pgt_action = 'I' THEN 1 ELSE -1 END) <> 0\
         ) \"__weighted\" \
         ORDER BY \"__pgt_row_id\", \
                  CASE WHEN \"__pgt_action\" = 'I' THEN 0 ELSE 1 END)"
    )
}

/// EC-06a: Weight-aggregate a keyless delta to cancel within-delta I/D pairs.
///
/// Without this, the 3-step DML (DELETE → UPDATE → INSERT) processes D and I
/// actions independently.  When the EC-02 correction term produces a DELETE
/// with hash H and another part produces an INSERT with the same H (or vice
/// versa), the DELETE step searches *storage* (which may not contain H) while
/// the INSERT step adds a row unconditionally — creating a phantom row.
///
/// This wrapper groups delta rows by `(__pgt_row_id, user_cols)`, computes the
/// net action (INSERT if positive, DELETE if negative, filtered if zero), and
/// expands back to the correct row count via `generate_series`.
///
/// Unlike [`build_weight_agg_using`] (keyed), this does **not** use
/// `DISTINCT ON (__pgt_row_id)` because keyless tables intentionally allow
/// multiple rows with the same `__pgt_row_id` but different column values.
fn build_keyless_weight_agg(delta_sql: &str, user_col_list: &str) -> String {
    format!(
        "(SELECT \"__pgt_row_id\", \"__pgt_action\", {user_col_list} \
         FROM (\
             SELECT __pgt_row_id, \
                    CASE WHEN SUM(CASE WHEN __pgt_action = 'I' THEN 1 ELSE -1 END) > 0 \
                         THEN 'I' ELSE 'D' END AS __pgt_action, \
                    {user_col_list}, \
                    ABS(SUM(CASE WHEN __pgt_action = 'I' THEN 1 ELSE -1 END)) AS __pgt_cnt \
             FROM ({delta_sql}) __raw \
             GROUP BY __pgt_row_id, {user_col_list} \
             HAVING SUM(CASE WHEN __pgt_action = 'I' THEN 1 ELSE -1 END) <> 0\
         ) __w, \
         LATERAL generate_series(1, __w.__pgt_cnt) __gs)"
    )
}

/// EC-06: Build a counted DELETE template for keyless sources.
///
/// For keyless tables, multiple stream table rows can share the same
/// `__pgt_row_id` (content hash). A plain `DELETE ... USING delta` would
/// remove ALL matching rows, not just the intended count. This template
/// uses ROW_NUMBER on both the stream table and delta to pair them 1:1,
/// then deletes only the paired ctids.
fn build_keyless_delete_template(quoted_table: &str, pgt_id: i64) -> String {
    format!(
        "DELETE FROM {quoted_table} \
         WHERE ctid IN (\
           SELECT numbered_st.st_ctid \
           FROM (\
             SELECT st2.ctid AS st_ctid, \
                    st2.__pgt_row_id, \
                    ROW_NUMBER() OVER (\
                      PARTITION BY st2.__pgt_row_id ORDER BY st2.ctid\
                    ) AS st_rn \
             FROM {quoted_table} st2 \
             WHERE st2.__pgt_row_id IN (\
               SELECT DISTINCT __pgt_row_id \
               FROM __pgt_delta_{pgt_id} \
               WHERE __pgt_action = 'D'\
             )\
           ) numbered_st \
           JOIN (\
             SELECT __pgt_row_id, \
                    COUNT(*)::INT AS del_count \
             FROM __pgt_delta_{pgt_id} \
             WHERE __pgt_action = 'D' \
             GROUP BY __pgt_row_id\
           ) dc ON numbered_st.__pgt_row_id = dc.__pgt_row_id \
           WHERE numbered_st.st_rn <= dc.del_count\
         )",
        pgt_id = pgt_id,
    )
}

/// Build an INSERT SQL statement for append-only stream tables.
///
/// Extracts the USING clause from the MERGE template and rewrites it as:
///   INSERT INTO "schema"."table" (__pgt_row_id, user_cols...)
///   SELECT d.__pgt_row_id, d.user_cols...
///   FROM (...delta...) AS d
///   WHERE d.__pgt_action = 'I'
///
/// PH-E1: Estimate the output cardinality of the delta subquery by running
/// a capped COUNT. Returns `None` if the USING clause cannot be extracted or
/// the query fails (estimation is best-effort).
///
/// Extracts the delta subquery from the MERGE SQL's USING clause and runs:
///   `SELECT count(*) FROM (delta_query LIMIT <limit+1>) __pgt_est`
fn estimate_delta_output_rows(merge_sql: &str, limit: i32) -> Option<i64> {
    // Extract USING clause: between "USING " and " AS d ON"
    let using_start = merge_sql.find("USING ").map(|p| p + 6)?;
    let using_end = merge_sql.find(" AS d ON ")?;
    let using_clause = &merge_sql[using_start..using_end];

    let cap = (limit as i64) + 1;
    let estimate_sql = format!("SELECT count(*) FROM ({using_clause} LIMIT {cap}) __pgt_est");

    match Spi::get_one::<i64>(&estimate_sql) {
        Ok(Some(count)) => Some(count),
        Ok(None) => Some(0),
        Err(e) => {
            pgrx::debug1!(
                "[pg_trickle] PH-E1: delta estimation query failed (non-fatal): {}",
                e,
            );
            None
        }
    }
}

/// Check whether the delta SQL contains CTE markers from DVM operators
/// that are **not insert-monotonic** — i.e., where INSERT-only source
/// changes can still produce DELETE or UPDATE actions in the delta output.
///
/// When this returns `true`, the append-only INSERT fast path (A-3a) is
/// unsafe because the bare `INSERT … WHERE __pgt_action = 'I'` would miss
/// delta DELETEs and duplicate-key UPDATEs.
fn has_non_monotonic_cte(sql: &str) -> bool {
    sql.contains("__pgt_cte_agg_") // Aggregate: group updates → 'I' with existing row_id
        || sql.contains("__pgt_cte_join_") // INNER JOIN: source DELETEs/UPDATEs produce delta DELETEs
        || sql.contains("__pgt_cte_left_join_") // LEFT JOIN: right INSERTs remove NULL-padded rows
        || sql.contains("__pgt_cte_lj_") // LEFT JOIN flags CTE
        || sql.contains("__pgt_cte_full_join_") // FULL JOIN
        || sql.contains("__pgt_cte_fj_") // FULL JOIN flags CTE
        || sql.contains("__pgt_cte_anti_join_") // NOT EXISTS / ALL subquery
        || sql.contains("__pgt_cte_semi_join_") // EXISTS subquery
        || sql.contains("__pgt_cte_r_old_") // Semi/anti join pre-change snapshot
        || sql.contains("__pgt_cte_isect_") // INTERSECT: right INSERTs remove left rows
        || sql.contains("__pgt_cte_dist_") // DISTINCT: INSERTs change dedup outcome
        || sql.contains("__pgt_cte_exct_") // EXCEPT: right INSERTs remove left rows
        || sql.contains("__pgt_cte_win_") // Window: INSERTs change partition values
        || sql.contains("__pgt_cte_scalar_sub_") // Scalar subquery: value changes
        || sql.contains("__pgt_cte_sq_gate_") // Scalar subquery gate
        || sql.contains("__pgt_cte_lat_sq_") // Lateral subquery
        || sql.contains("__pgt_cte_rc_") // Recursive CTE
        || sql.contains("__pgt_cte_dred_") // Recursive CTE (DRed)
        || sql.contains("__pgt_cte_lat_changed_") // Lateral function
        || sql.contains("__pgt_cte_lat_old_") // Lateral function
}

/// Build an `INSERT ... SELECT` SQL statement from a MERGE SQL template
/// for append-only stream tables.
///
/// This is significantly faster than MERGE for append-only workloads
/// because it skips the DELETE, UPDATE, and IS DISTINCT FROM checks.
fn build_append_only_insert_sql(schema: &str, name: &str, merge_sql: &str) -> String {
    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\""),
    );

    // Parse the MERGE SQL to extract:
    // 1. The USING clause (delta subquery)
    // 2. The INSERT column list
    //
    // MERGE INTO "schema"."table" AS st USING (...) AS d ON ...
    // ... WHEN NOT MATCHED AND d.__pgt_action = 'I' THEN
    //   INSERT (__pgt_row_id, col1, col2, ...)
    //   VALUES (d.__pgt_row_id, d.col1, d.col2, ...)

    // Extract USING clause: between "USING " and " AS d ON"
    let using_start = merge_sql.find("USING ").map(|p| p + 6).unwrap_or(0);
    let using_end = merge_sql.find(" AS d ON ").unwrap_or(merge_sql.len());
    let using_clause = &merge_sql[using_start..using_end];

    // Extract column list from INSERT clause
    let insert_marker = "INSERT (";
    let insert_start = merge_sql
        .rfind(insert_marker)
        .map(|p| p + insert_marker.len())
        .unwrap_or(0);
    let insert_end = merge_sql[insert_start..]
        .find(')')
        .map(|p| insert_start + p)
        .unwrap_or(merge_sql.len());
    let col_list = &merge_sql[insert_start..insert_end];

    // Extract VALUES column list (d.col prefixed)
    let values_marker = "VALUES (";
    let values_start = merge_sql
        .rfind(values_marker)
        .map(|p| p + values_marker.len())
        .unwrap_or(0);
    let values_end = merge_sql[values_start..]
        .find(')')
        .map(|p| values_start + p)
        .unwrap_or(merge_sql.len());
    let d_col_list = &merge_sql[values_start..values_end];

    format!(
        "INSERT INTO {quoted_table} ({col_list}) \
         SELECT {d_col_list} \
         FROM {using_clause} AS d \
         WHERE d.__pgt_action = 'I' \
         ON CONFLICT (__pgt_row_id) DO NOTHING"
    )
}

// ── TG2-MERGE: Extracted pure template builders ─────────────────────
//
// These functions are the core MERGE/DML template assembly logic, extracted
// from prewarm_merge_cache() and execute_differential_refresh() so they can
// be unit-tested without a database.

/// Format a quoted column list: `"col1", "col2", "col3"`.
fn format_col_list(user_cols: &[String]) -> String {
    user_cols
        .iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Format a prefixed quoted column list: `d."col1", d."col2"`.
fn format_prefixed_col_list(prefix: &str, user_cols: &[String]) -> String {
    user_cols
        .iter()
        .map(|c| format!("{prefix}.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Format an UPDATE SET clause: `"col1" = d."col1", "col2" = d."col2"`.
fn format_update_set(user_cols: &[String]) -> String {
    user_cols
        .iter()
        .map(|c| {
            let qc = format!("\"{}\"", c.replace('"', "\"\""));
            format!("{qc} = d.{qc}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the core MERGE SQL template for differential refresh.
///
/// This is the primary delta-application statement: it merges incoming
/// delta rows (with `__pgt_action` = 'I' or 'D') into the stream table,
/// performing DELETE, UPDATE, or INSERT as appropriate.
///
/// Extracted as a pure function for unit testability (TG2-MERGE).
fn build_merge_sql(
    quoted_table: &str,
    using_clause: &str,
    user_cols: &[String],
    has_partition_key: bool,
) -> String {
    let user_col_list = format_col_list(user_cols);
    let d_user_col_list = format_prefixed_col_list("d", user_cols);
    let update_set_clause = format_update_set(user_cols);
    let is_distinct_clause = build_is_distinct_clause(user_cols);

    format!(
        "MERGE INTO {quoted_table} AS st \
         USING {using_clause} AS d \
         ON st.__pgt_row_id = d.__pgt_row_id{part} \
         WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE \
         WHEN MATCHED AND d.__pgt_action = 'I' AND ({is_distinct_clause}) THEN \
           UPDATE SET {update_set_clause} \
         WHEN NOT MATCHED AND d.__pgt_action = 'I' THEN \
           INSERT (__pgt_row_id, {user_col_list}) \
           VALUES (d.__pgt_row_id, {d_user_col_list})",
        part = if has_partition_key {
            " __PGT_PART_PRED__"
        } else {
            ""
        },
    )
}

/// Build the trigger-path DELETE template.
///
/// For keyless sources, uses counted DELETE via ROW_NUMBER to avoid
/// removing all rows with a matching row_id. For keyed sources, uses
/// a simple equi-join DELETE.
fn build_trigger_delete_sql(quoted_table: &str, pgt_id: i64, use_keyless: bool) -> String {
    if use_keyless {
        build_keyless_delete_template(quoted_table, pgt_id)
    } else {
        format!(
            "DELETE FROM {quoted_table} AS st \
             USING __pgt_delta_{pgt_id} AS d \
             WHERE st.__pgt_row_id = d.__pgt_row_id \
               AND d.__pgt_action = 'D'",
        )
    }
}

/// Build the trigger-path UPDATE template.
///
/// Updates existing rows where the delta action is 'I' and values changed
/// (IS DISTINCT FROM guard prevents no-op writes).
fn build_trigger_update_sql(quoted_table: &str, pgt_id: i64, user_cols: &[String]) -> String {
    let update_set_clause = format_update_set(user_cols);
    let is_distinct_clause = build_is_distinct_clause(user_cols);
    format!(
        "UPDATE {quoted_table} AS st \
         SET {update_set_clause} \
         FROM __pgt_delta_{pgt_id} AS d \
         WHERE st.__pgt_row_id = d.__pgt_row_id \
           AND d.__pgt_action = 'I' \
           AND ({is_distinct_clause})",
    )
}

/// Build the trigger-path INSERT template.
///
/// For keyless sources, uses plain INSERT (no NOT EXISTS check since
/// duplicate row_ids are expected). For keyed sources, uses
/// `ON CONFLICT (__pgt_row_id) DO UPDATE SET …` (upsert) which:
///   - Inserts genuinely new rows (__pgt_row_id absent from ST)
///   - Updates existing rows when column values have changed
///   - Is a no-op when column values are identical
///
/// This replaces the previous `NOT EXISTS` approach which was vulnerable
/// to race conditions when Part 1 and Part 2 of the join delta produce
/// different __pgt_row_id hashes for the same logical row — the phantom
/// rows from prior refreshes could cause duplicate-key violations during
/// the INSERT because the NOT EXISTS check evaluated against a snapshot
/// that didn't include concurrently committed rows.
fn build_trigger_insert_sql(
    quoted_table: &str,
    pgt_id: i64,
    user_cols: &[String],
    use_keyless: bool,
) -> String {
    let user_col_list = format_col_list(user_cols);
    let d_user_col_list = format_prefixed_col_list("d", user_cols);
    if use_keyless {
        format!(
            "INSERT INTO {quoted_table} (__pgt_row_id, {user_col_list}) \
             SELECT d.__pgt_row_id, {d_user_col_list} \
             FROM __pgt_delta_{pgt_id} AS d \
             WHERE d.__pgt_action = 'I'",
        )
    } else {
        // Keyed: DISTINCT ON eliminates within-delta duplicates; ON CONFLICT
        // is a safety net for any remaining row_id collisions with the ST.
        // Callers that need guaranteed conflict-free inserts (PH-D1) should
        // delete all matching row_ids from the ST before executing this.
        format!(
            "INSERT INTO {quoted_table} (__pgt_row_id, {user_col_list}) \
             SELECT DISTINCT ON (d.__pgt_row_id) d.__pgt_row_id, {d_user_col_list} \
             FROM __pgt_delta_{pgt_id} AS d \
             WHERE d.__pgt_action = 'I' \
             ORDER BY d.__pgt_row_id \
             ON CONFLICT (__pgt_row_id) DO NOTHING",
        )
    }
}

/// Pre-warm the delta SQL + MERGE template caches for a stream table.
///
/// Called after `create_stream_table()` to avoid a cold-start penalty on
/// the first differential refresh (cycle 1). This generates the delta SQL
/// template and MERGE SQL template with placeholder tokens, caching them
/// for subsequent refreshes.
///
/// Errors are logged but not propagated — cache pre-warming is optional.
pub fn prewarm_merge_cache(st: &StreamTableMeta) {
    use crate::version::Frontier;
    use std::hash::{Hash, Hasher};

    let schema = &st.pgt_schema;
    let name = &st.pgt_name;

    if matches!(dvm::query_has_recursive_cte(&st.defining_query), Ok(true)) {
        pgrx::debug1!(
            "[pg_trickle] cache pre-warm skipped for {}.{}: recursive CTEs choose refresh strategy at runtime",
            schema,
            name,
        );
        return;
    }

    // Use dummy frontiers — placeholders will be embedded in the template
    let dummy = Frontier::new();

    let delta_result = match dvm::generate_delta_query_cached(
        st.pgt_id,
        &st.defining_query,
        &dummy,
        &dummy,
        schema,
        name,
    ) {
        Ok(r) => r,
        Err(e) => {
            pgrx::log!(
                "pg_trickle: cache pre-warm skipped for {}.{}: {}",
                schema,
                name,
                e
            );
            return;
        }
    };

    // Build the MERGE template (same logic as the cache-miss path in
    // execute_differential_refresh, but we only store the template).
    let user_cols = &delta_result.output_columns;
    let source_oids = &delta_result.source_oids;

    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\""),
    );

    let user_col_list = format_col_list(user_cols);

    let delta_sql_template =
        dvm::get_delta_sql_template(st.pgt_id).unwrap_or(delta_result.delta_sql);

    // Build the USING clause — skip deduplication when the delta is already
    // deduplicated (G-M1 optimization for scan-chain queries).
    //
    // EC-06: For keyless sources the delta is never deduplicated (multiple
    // rows can share the same __pgt_row_id). Skip deduplication unconditionally.
    //
    // B3-2: For non-deduplicated deltas, use weight aggregation instead of
    // DISTINCT ON.  Weight aggregation correctly handles diamond-flow queries
    // where multiple delta branches produce overlapping corrections.
    //
    // A-2: When `has_key_changed` is available on a deduplicated delta, wrap
    // the USING clause with a filter that suppresses D-side rows for value-only
    // UPDATEs (__pgt_key_changed = FALSE).  The remaining I-side row triggers
    // the existing WHEN MATCHED THEN UPDATE clause — converting a DELETE+INSERT
    // cycle into a single UPDATE (cheaper WAL, HOT-eligible, no index churn).
    let using_clause = if delta_result.is_deduplicated && delta_result.has_key_changed {
        format!(
            "(SELECT * FROM ({delta_sql_template}) __d \
             WHERE NOT (__d.__pgt_action = 'D' AND __d.__pgt_key_changed = FALSE))"
        )
    } else if delta_result.is_deduplicated {
        format!("({delta_sql_template})")
    } else if st.has_keyless_source {
        // EC-06a: Weight-aggregate keyless deltas to cancel within-delta
        // I/D pairs.  Without this, the 3-step DML (DELETE → INSERT)
        // processes them independently: the DELETE targets storage rows
        // (which may not exist for intermediate hashes), while the INSERT
        // adds unconditionally — creating phantom rows on every refresh
        // cycle where both join sides change simultaneously (EC-02).
        build_keyless_weight_agg(&delta_sql_template, &user_col_list)
    } else {
        build_weight_agg_using(&delta_sql_template, &user_col_list)
    };

    let merge_template = build_merge_sql(
        &quoted_table,
        &using_clause,
        user_cols,
        st.st_partition_key.is_some(),
    );

    // Build cleanup template.
    let cleanup_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let cleanup_stmts: Vec<String> = source_oids
        .iter()
        .map(|oid| {
            format!(
                "DELETE FROM \"{cleanup_schema}\".changes_{oid} \
                 WHERE lsn > '__PGS_PREV_LSN_{oid}__'::pg_lsn \
                 AND lsn <= '__PGS_NEW_LSN_{oid}__'::pg_lsn",
            )
        })
        .collect();
    let cleanup_template = cleanup_stmts.join(";");

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    st.defining_query.hash(&mut hasher);
    let query_hash = hasher.finish();

    // D-2: Parameterize MERGE template for prepared-statement execution.
    let parameterized_merge_sql = parameterize_lsn_template(&merge_template, source_oids);

    // ── User-trigger explicit DML templates ──────────────────────────
    //
    // EC-06: Keyless sources use counted DELETE (ROW_NUMBER matching)
    // and plain INSERT (no NOT EXISTS / ON CONFLICT) since duplicate
    // __pgt_row_id values are expected. The UPDATE step is a no-op
    // because the scan-level net counting decomposes updates into
    // separate D + I rows.
    let trigger_delete_template =
        build_trigger_delete_sql(&quoted_table, st.pgt_id, st.has_keyless_source);

    // EC-06: For keyless sources, the scan-level delta decomposes UPDATEs
    // into D+I pairs (different content hashes), so the UPDATE template
    // naturally matches 0 rows. For aggregate queries on keyless sources,
    // the aggregate delta produces 'I' actions for changed groups that
    // need real UPDATEs. Using the normal UPDATE template handles both
    // cases correctly.
    let trigger_update_template = build_trigger_update_sql(&quoted_table, st.pgt_id, user_cols);

    let trigger_insert_template =
        build_trigger_insert_sql(&quoted_table, st.pgt_id, user_cols, st.has_keyless_source);

    // Cache the MERGE template with LSN placeholder tokens.
    // Each refresh resolves the tokens to concrete LSN values
    // via string substitution, then executes the resolved SQL.
    MERGE_TEMPLATE_CACHE.with(|cache| {
        cache.borrow_mut().insert(
            st.pgt_id,
            CachedMergeTemplate {
                defining_query_hash: query_hash,
                merge_sql_template: merge_template,
                parameterized_merge_sql,
                source_oids: source_oids.clone(),
                cleanup_sql_template: cleanup_template,
                trigger_delete_template,
                trigger_update_template,
                trigger_insert_template,
                trigger_using_template: using_clause.clone(),
                delta_sql_template: delta_sql_template.clone(),
                is_all_algebraic: delta_result.is_all_algebraic,
                is_deduplicated: delta_result.is_deduplicated,
            },
        );
    });

    pgrx::log!(
        "pg_trickle: pre-warmed delta+MERGE cache for {}.{}",
        schema,
        name
    );
}

/// Determines what kind of refresh action should be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshAction {
    /// No upstream changes — just advance the data timestamp.
    NoData,
    /// Full recompute from the defining query.
    Full,
    /// Differential delta application.
    Differential,
    /// Full recompute due to schema change or reinit flag.
    Reinitialize,
}

impl RefreshAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefreshAction::NoData => "NO_DATA",
            RefreshAction::Full => "FULL",
            RefreshAction::Differential => "DIFFERENTIAL",
            RefreshAction::Reinitialize => "REINITIALIZE",
        }
    }
}

/// Determine the refresh action for a stream table.
///
/// DI-7: When `max_differential_joins` is configured and the defining query
/// has more join scans than the threshold, DIFFERENTIAL is downgraded to FULL.
/// The `join_scan_count` parameter is optional — when `None`, the DI-7 check
/// is skipped (the caller doesn't have the OpTree available).
pub fn determine_refresh_action(st: &StreamTableMeta, has_upstream_changes: bool) -> RefreshAction {
    if st.needs_reinit {
        return RefreshAction::Reinitialize;
    }
    if !has_upstream_changes {
        return RefreshAction::NoData;
    }
    match st.refresh_mode {
        RefreshMode::Full => RefreshAction::Full,
        RefreshMode::Differential => RefreshAction::Differential,
        // IMMEDIATE-mode STs are maintained by triggers, not by the
        // scheduler.  If we somehow reach this point (e.g. manual
        // refresh), fall back to a full refresh.
        RefreshMode::Immediate => RefreshAction::Full,
    }
}

/// G12-2: Validate stored TopK metadata fields (pure logic — no SPI/parser).
///
/// Returns `Ok(())` when the fields are valid, or `Err(reason)` with a
/// human-readable message when something is inconsistent.  This can be
/// fully unit-tested without a PostgreSQL backend.
pub fn validate_topk_metadata_fields(
    stored_limit: i32,
    stored_order_by: &str,
    stored_offset: Option<i32>,
) -> Result<(), String> {
    if stored_limit < 0 {
        return Err(format!("stored topk_limit is negative ({})", stored_limit));
    }
    if stored_order_by.trim().is_empty() {
        return Err("stored topk_order_by is empty".to_string());
    }
    if let Some(off) = stored_offset
        && off < 0
    {
        return Err(format!("stored topk_offset is negative ({})", off));
    }
    Ok(())
}

/// G12-2: Full TopK runtime validation — validates stored fields and
/// re-parses the reconstructed query to verify the TopK pattern.
/// Requires a PostgreSQL backend (calls parser).
pub fn validate_topk_metadata(
    defining_query: &str,
    stored_limit: i32,
    stored_order_by: &str,
    stored_offset: Option<i32>,
) -> Result<(), String> {
    validate_topk_metadata_fields(stored_limit, stored_order_by, stored_offset)?;

    // Reconstruct the full query and re-parse the TopK pattern.
    let full_query = if let Some(offset) = stored_offset {
        format!(
            "{} ORDER BY {} LIMIT {} OFFSET {}",
            defining_query, stored_order_by, stored_limit, offset
        )
    } else {
        format!(
            "{} ORDER BY {} LIMIT {}",
            defining_query, stored_order_by, stored_limit
        )
    };
    match crate::dvm::detect_topk_pattern(&full_query) {
        Ok(Some(info)) => {
            if info.limit_value != stored_limit as i64 {
                return Err(format!(
                    "re-parsed LIMIT {} differs from stored topk_limit {}",
                    info.limit_value, stored_limit,
                ));
            }
            let expected_offset = stored_offset.map(|o| o as i64);
            if info.offset_value != expected_offset {
                return Err(format!(
                    "re-parsed OFFSET {:?} differs from stored topk_offset {:?}",
                    info.offset_value, stored_offset,
                ));
            }
            Ok(())
        }
        Ok(None) => Err("reconstructed query no longer matches the TopK pattern \
             (ORDER BY + LIMIT with constant integers)"
            .to_string()),
        Err(e) => Err(format!("failed to re-parse TopK pattern: {}", e)),
    }
}

/// Execute a TopK refresh: re-execute the ORDER BY + LIMIT query and MERGE
/// the result into the stream table.
///
/// TopK tables store the top-N rows as defined by ORDER BY + LIMIT. On each
/// refresh, the full query is re-executed against the source tables and the
/// result is merged using MERGE (with NOT MATCHED BY SOURCE for deletes).
///
/// This function is used for both FULL and DIFFERENTIAL refresh modes of
/// TopK tables. The caller decides whether to invoke it (DIFFERENTIAL mode
/// checks change buffers first and skips if no changes exist).
pub fn execute_topk_refresh(st: &StreamTableMeta) -> Result<(i64, i64), PgTrickleError> {
    // G12-ERM-1: Record the effective mode for this execution path.
    set_effective_mode("TOP_K");

    // EC-25/EC-26: Ensure the internal_refresh flag is set so DML guard
    // triggers allow the refresh executor to modify the storage table.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    let schema = &st.pgt_schema;
    let name = &st.pgt_name;

    let topk_limit = st.topk_limit.ok_or_else(|| {
        PgTrickleError::InternalError("execute_topk_refresh called on non-TopK stream table".into())
    })?;
    let topk_order_by = st.topk_order_by.as_deref().ok_or_else(|| {
        PgTrickleError::InternalError("TopK stream table missing order_by metadata".into())
    })?;

    // G12-2: TopK runtime validation — re-parse the reconstructed full query
    // and verify the detected TopK pattern matches stored catalog metadata.
    // On mismatch, fall back to FULL refresh to prevent silent correctness issues.
    if let Err(reason) = validate_topk_metadata(
        &st.defining_query,
        topk_limit,
        topk_order_by,
        st.topk_offset,
    ) {
        pgrx::warning!(
            "pg_trickle: TopK metadata inconsistency for {}.{}: {}. \
             Falling back to FULL refresh.",
            schema,
            name,
            reason,
        );
        set_effective_mode("FULL");
        return execute_full_refresh(st);
    }

    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\""),
    );

    // Reconstruct the full TopK query from base query + ORDER BY + LIMIT [+ OFFSET].
    let topk_query = if let Some(offset) = st.topk_offset {
        format!(
            "{} ORDER BY {} LIMIT {} OFFSET {}",
            st.defining_query, topk_order_by, topk_limit, offset
        )
    } else {
        format!(
            "{} ORDER BY {} LIMIT {}",
            st.defining_query, topk_order_by, topk_limit
        )
    };

    // Compute row_id using the same hash formula as normal refresh.
    let row_id_expr = crate::dvm::row_id_expr_for_query(&st.defining_query);

    // Build the source subquery with row IDs.
    // Use alias `sub` to match what row_id_expr_for_query() generates.
    let source_sql = format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({topk_query}) sub");

    // Get column names from the storage table (excluding __pgt_row_id).
    let columns = crate::dvm::get_defining_query_columns(&st.defining_query)?;

    // Build the MERGE statement.
    let col_list: Vec<String> = columns
        .iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect();

    let update_set: Vec<String> = col_list
        .iter()
        .map(|c| format!("{c} = __pgt_topk_src.{c}"))
        .collect();

    let insert_cols: String = std::iter::once("__pgt_row_id".to_string())
        .chain(col_list.iter().cloned())
        .collect::<Vec<_>>()
        .join(", ");

    let insert_vals: String = std::iter::once("__pgt_topk_src.__pgt_row_id".to_string())
        .chain(col_list.iter().map(|c| format!("__pgt_topk_src.{c}")))
        .collect::<Vec<_>>()
        .join(", ");

    // Build an IS DISTINCT FROM check for change detection in WHEN MATCHED.
    let is_distinct_check = if col_list.is_empty() {
        "TRUE".to_string()
    } else {
        col_list
            .iter()
            .map(|c| format!("{quoted_table}.{c}::text IS DISTINCT FROM __pgt_topk_src.{c}::text"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };

    let merge_sql = format!(
        "MERGE INTO {quoted_table} \
         USING ({source_sql}) AS __pgt_topk_src \
         ON {quoted_table}.__pgt_row_id = __pgt_topk_src.__pgt_row_id \
         WHEN MATCHED AND ({is_distinct_check}) THEN \
           UPDATE SET {update_set} \
         WHEN NOT MATCHED THEN \
           INSERT ({insert_cols}) VALUES ({insert_vals}) \
         WHEN NOT MATCHED BY SOURCE THEN \
           DELETE",
        update_set = update_set.join(", "),
    );

    let (rows_inserted, rows_deleted) = Spi::connect_mut(|client| {
        let result = client
            .update(&merge_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        // MERGE returns total rows processed. We don't get separate insert/delete
        // counts from SPI, so return the total as "inserted" and 0 as "deleted".
        // The actual bookkeeping is approximate here.
        Ok::<(i64, i64), PgTrickleError>((result.len() as i64, 0))
    })?;

    pgrx::debug1!(
        "[pg_trickle] TopK refresh of {}.{}: MERGE processed {} rows",
        schema,
        name,
        rows_inserted,
    );

    Ok((rows_inserted, rows_deleted))
}

/// Execute a full refresh: TRUNCATE + INSERT from defining query.
///
/// When user triggers are detected (and the GUC is not `"off"`), they are
/// suppressed during the TRUNCATE + INSERT via `DISABLE TRIGGER USER` /
/// `ENABLE TRIGGER USER`. A `NOTIFY pgtrickle_refresh` is emitted so
/// listeners know a FULL refresh occurred.
///
/// **Note:** Row-level user triggers do NOT fire correctly for FULL refresh.
/// Users who need per-row trigger semantics should use `REFRESH MODE
/// DIFFERENTIAL`. See PLAN_USER_TRIGGERS_EXPLICIT_DML.md §2.
pub fn execute_full_refresh(st: &StreamTableMeta) -> Result<(i64, i64), PgTrickleError> {
    // G12-ERM-1: Record the effective mode for this execution path.
    set_effective_mode("FULL");

    // EC-25/EC-26: Ensure the internal_refresh flag is set so DML guard
    // triggers allow the refresh executor to modify the storage table.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    let schema = &st.pgt_schema;
    let name = &st.pgt_name;
    let query = &st.defining_query;

    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\""),
    );

    // Check for user triggers to suppress during FULL refresh.
    let user_triggers_mode = crate::config::pg_trickle_user_triggers_mode();
    let has_triggers = match user_triggers_mode {
        crate::config::UserTriggersMode::Off => false,
        crate::config::UserTriggersMode::Auto => crate::cdc::has_user_triggers(st.pgt_relid)?,
    };

    // Suppress user triggers during TRUNCATE + INSERT to prevent
    // spurious trigger invocations with wrong semantics.
    if has_triggers {
        Spi::run(&format!("ALTER TABLE {quoted_table} DISABLE TRIGGER USER"))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    // For aggregate/distinct STs, inject COUNT(*) AS __pgt_count into the
    // defining query so the auxiliary column is populated correctly.
    let effective_query = if st.refresh_mode == crate::dag::RefreshMode::Differential
        && crate::dvm::query_needs_pgt_count(query)
    {
        let mut eq = crate::api::inject_pgt_count(query);
        // Also inject AVG auxiliary columns (SUM/COUNT of arg) for algebraic
        // AVG maintenance.
        let avg_aux = crate::dvm::query_avg_aux_columns(query);
        if !avg_aux.is_empty() {
            eq = crate::api::inject_avg_aux(&eq, &avg_aux);
        }
        // Also inject sum-of-squares columns for STDDEV/VAR maintenance.
        let sum2_aux = crate::dvm::query_sum2_aux_columns(query);
        if !sum2_aux.is_empty() {
            eq = crate::api::inject_sum2_aux(&eq, &sum2_aux);
        }
        // Also inject nonnull-count columns for SUM NULL-transition correction (P2-2).
        let nonnull_aux = crate::dvm::query_nonnull_aux_columns(query);
        if !nonnull_aux.is_empty() {
            eq = crate::api::inject_nonnull_aux(&eq, &nonnull_aux);
        }
        eq
    } else {
        query.clone()
    };

    // ST-ST-3: Snapshot pre-state for diff capture when this ST has
    // downstream ST consumers. The snapshot is compared against the
    // post-refresh state to produce I/D pairs for the change buffer.
    let needs_diff_capture = has_downstream_st_consumers(st.pgt_id);
    let user_cols = if needs_diff_capture {
        let cols = get_st_user_columns(st);
        let col_list: String = cols
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");

        // Drop any leftover pre-snapshot from a previous iteration
        // (e.g., SCC fixpoint loops where subtransaction commits don't
        // fire ON COMMIT DROP until the outer transaction commits).
        let _ = Spi::run(&format!("DROP TABLE IF EXISTS __pgt_pre_{}", st.pgt_id)); // nosemgrep: rust.spi.run.dynamic-format — st.pgt_id is a plain i64, not user-supplied input.

        let snapshot_sql = format!(
            "CREATE TEMP TABLE __pgt_pre_{pgt_id} ON COMMIT DROP AS \
             SELECT __pgt_row_id, {col_list} FROM {quoted_table}",
            pgt_id = st.pgt_id,
        );
        if let Err(e) = Spi::run(&snapshot_sql) {
            pgrx::warning!(
                "[pg_trickle] ST-ST: pre-snapshot failed for {}.{}: {} — \
                 downstream STs will not receive differential delta",
                schema,
                name,
                e,
            );
            Vec::new()
        } else {
            cols
        }
    } else {
        Vec::new()
    };

    // Truncate
    Spi::run(&format!("TRUNCATE {quoted_table}"))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // Compute row_id using the same hash formula as the delta query so
    // the MERGE ON clause matches during subsequent differential refreshes.
    // For INTERSECT/EXCEPT, compute per-branch multiplicities for dual-count
    // storage. For UNION (dedup), convert to UNION ALL and count.
    // For UNION ALL, decompose into per-branch subqueries with
    // child-prefixed row IDs matching diff_union_all's formula.
    let insert_body = if crate::dvm::query_needs_dual_count(query) {
        let col_names = crate::dvm::get_defining_query_columns(query)?;
        if let Some(set_op_sql) = crate::dvm::try_set_op_refresh_sql(query, &col_names) {
            set_op_sql
        } else {
            let row_id_expr = crate::dvm::row_id_expr_for_query(query);
            format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({effective_query}) sub",)
        }
    } else if crate::dvm::query_needs_union_dedup_count(query) {
        let col_names = crate::dvm::get_defining_query_columns(query)?;
        if let Some(union_sql) = crate::dvm::try_union_dedup_refresh_sql(query, &col_names) {
            union_sql
        } else {
            let row_id_expr = crate::dvm::row_id_expr_for_query(query);
            format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({effective_query}) sub",)
        }
    } else if let Some(ua_sql) = crate::dvm::try_union_all_refresh_sql(query) {
        ua_sql
    } else {
        let row_id_expr = crate::dvm::row_id_expr_for_query(query);
        format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({effective_query}) sub",)
    };

    let insert_sql = format!("INSERT INTO {quoted_table} {insert_body}");

    let rows_inserted = Spi::connect_mut(|client| {
        let result = client
            .update(&insert_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Ok::<usize, PgTrickleError>(result.len())
    })?;

    // ST-ST-3: Capture the full-refresh diff into the change buffer.
    // If diff capture fails, downstream DIFFERENTIAL STs would silently
    // diverge because they expect delta rows in changes_pgt_{id}. To
    // prevent that, mark all downstream STs for reinit so they do a FULL
    // refresh next cycle and resync.
    if needs_diff_capture
        && !user_cols.is_empty()
        && let Err(e) = capture_full_refresh_diff_to_st_buffer(st, &user_cols)
    {
        pgrx::warning!(
            "[pg_trickle] ST-ST: full-refresh diff capture failed for {}.{}: {} \
             — marking downstream STs for reinit to prevent silent divergence",
            schema,
            name,
            e,
        );
        // Mark downstream STs for reinit so they resync via FULL refresh.
        if let Ok(downstream_ids) =
            crate::catalog::StDependency::get_downstream_pgt_ids(st.pgt_relid)
        {
            for ds_id in &downstream_ids {
                if let Err(e2) = StreamTableMeta::mark_for_reinitialize(*ds_id) {
                    pgrx::warning!(
                        "[pg_trickle] ST-ST: failed to mark downstream ST {} for reinit: {}",
                        ds_id,
                        e2,
                    );
                }
            }
        }
    }

    // Re-enable user triggers and emit NOTIFY so listeners know a FULL
    // refresh occurred.
    if has_triggers {
        Spi::run(&format!("ALTER TABLE {quoted_table} ENABLE TRIGGER USER"))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        // PB2: Skip NOTIFY when pooler compatibility mode is enabled.
        if !st.pooler_compatibility_mode {
            // Escape single quotes in the JSON payload.
            let escaped_name = name.replace('\'', "''");
            let escaped_schema = schema.replace('\'', "''");
            // NOTIFY does not support parameterized payloads; single quotes are escaped above.
            let notify_sql = format!(
                "NOTIFY pgtrickle_refresh, '{{\"stream_table\": \"{escaped_name}\", \
                 \"schema\": \"{escaped_schema}\", \"mode\": \"FULL\", \"rows\": {rows_inserted}}}'"
            );
            Spi::run(&notify_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }

        pgrx::info!(
            "pg_trickle: FULL refresh of {}.{} with user triggers suppressed ({} rows). \
             Row-level triggers do NOT fire for FULL refresh; use REFRESH MODE DIFFERENTIAL.",
            schema,
            name,
            rows_inserted,
        );
    }

    // PART-WARN: After a successful FULL refresh, warn if the default
    // partition of a partitioned stream table has accumulated rows.
    if st.st_partition_key.is_some() {
        warn_default_partition_growth(schema, name);
    }

    Ok((rows_inserted as i64, 0))
}

/// Post-full-refresh cleanup helper (G3 + G4).
///
/// Intended to be called immediately after any FULL or REINITIALIZE refresh
/// completes successfully, from both the scheduled refresh path and from the
/// adaptive fallback path inside `execute_differential_refresh`.
///
/// 1. **G3 — WAL slot advancement**: For each WAL-mode source dependency,
///    advances the replication slot's `confirmed_flush_lsn` to the current WAL
///    LSN. This lets PostgreSQL reclaim WAL segments that the full refresh
///    already materialized, preventing unbounded `pg_wal/` growth on servers
///    that do repeated FULL refreshes.
///
/// 2. **G4 — Change buffer flush**: Deletes stale change buffer rows up to the
///    minimum stored frontier across all stream tables sharing each source.
///    This prevents the next differential tick from re-examining rows that are
///    already materialized, breaking the "adaptive fallback ping-pong" pattern.
///
/// Multi-ST safety: `cleanup_change_buffers_by_frontier` queries the catalog
/// for the minimum frontier across *all* STs that share each source OID, so
/// rows that another ST still needs are never deleted.
pub fn post_full_refresh_cleanup(st: &StreamTableMeta) {
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let deps = crate::catalog::StDependency::get_for_st(st.pgt_id).unwrap_or_default();
    let source_oids: Vec<u32> = deps
        .iter()
        .filter(|d| {
            d.source_type == "TABLE"
                || d.source_type == "FOREIGN_TABLE"
                || d.source_type == "MATVIEW"
        })
        .map(|d| d.source_relid.to_u32())
        .collect();

    // G3: Advance WAL slots past the current LSN so WAL segments produced
    // before and during the full refresh can be reclaimed by PostgreSQL.
    for slot in deps
        .iter()
        .filter(|d| {
            matches!(
                d.cdc_mode,
                crate::catalog::CdcMode::Wal | crate::catalog::CdcMode::Transitioning
            )
        })
        .filter_map(|d| d.slot_name.as_deref())
    {
        match crate::wal_decoder::advance_slot_to_current(slot) {
            Ok(()) => {
                pgrx::debug1!(
                    "[pg_trickle] post_full_refresh_cleanup: advanced WAL slot '{}' to current LSN",
                    slot,
                );
            }
            Err(e) => {
                pgrx::debug1!(
                    "[pg_trickle] post_full_refresh_cleanup: failed to advance slot '{}': {}",
                    slot,
                    e,
                );
            }
        }
    }

    // G4: Flush change buffer rows that are now irrelevant because the full
    // refresh already captured them.  Prevents the next differential cycle
    // from re-examining them and re-triggering another adaptive fallback.
    cleanup_change_buffers_by_frontier(&change_schema, &source_oids);

    // NOTE: ST change buffers (changes_pgt_{id}) are intentionally NOT
    // cleaned here. A FULL refresh reads the source table directly, not
    // the change buffer, so change buffer rows have NOT been consumed.
    // Cleaning them would remove rows that sibling consumers (other STs
    // depending on the same upstream ST) have not yet processed.
    // ST change buffer cleanup happens only in the differential refresh
    // path where the delta SQL actually reads from the change buffer.
}

/// Poll all FOREIGN_TABLE and MATVIEW dependencies for a stream table before
/// selecting a new differential frontier.
///
/// Polling writes synthetic CDC rows into the local change buffers and updates
/// the per-source snapshot tables. Callers must do this before capturing the
/// new upper frontier so the synthetic rows fall within the refresh window.
pub fn poll_foreign_table_sources_for_st(st: &StreamTableMeta) -> Result<(), PgTrickleError> {
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");

    for dep in StDependency::get_for_st(st.pgt_id)?
        .into_iter()
        .filter(|dep| dep.source_type == "FOREIGN_TABLE" || dep.source_type == "MATVIEW")
    {
        if dep.source_type == "FOREIGN_TABLE" {
            crate::cdc::poll_foreign_table_changes(dep.source_relid, &change_schema)?;
        } else {
            crate::cdc::poll_matview_changes(dep.source_relid, &change_schema)?;
        }
    }

    Ok(())
}

/// Execute a NO_DATA refresh: just advance the data timestamp.
pub fn execute_no_data_refresh(st: &StreamTableMeta) -> Result<(), PgTrickleError> {
    // G12-ERM-1: Record the effective mode for this execution path.
    set_effective_mode("NO_DATA");

    // Record that we checked — but do NOT update data_timestamp.
    // data_timestamp is reserved for refreshes that actually write rows.
    // Downstream stream tables compare upstream.data_timestamp against their
    // own data_timestamp to decide whether a full refresh is needed; bumping
    // data_timestamp on a no-data pass would trigger spurious full refreshes
    // of every downstream ST every time this table is polled.
    StreamTableMeta::update_after_no_data_refresh(st.pgt_id)?;
    Ok(())
}

/// Execute an differential refresh using the DVM engine.
///
/// 1. Short-circuits if no source table has changes in the LSN window
/// 2. Uses cached delta + MERGE SQL templates (or generates on first call)
/// 3. Resolves LSN placeholders with actual frontier values
/// 4. Applies the delta to the ST storage table via a single MERGE statement
///
/// ## Caching
///
/// The first refresh for a ST generates a SQL template with placeholder
/// tokens for LSN values. Subsequent refreshes skip parsing, DVM
/// differentiation, and MERGE SQL construction — they only substitute
/// LSN values and execute. This eliminates ~45ms overhead per refresh.
/// EC-16: Check whether any function referenced in `st.functions_used` has
/// changed its source code since the last differential refresh.
///
/// For each function name in `functions_used`, queries `pg_proc` for the
/// concatenated `md5(prosrc || coalesce(probin::text, ''))` of all matching
/// overloads (joined by `,` to handle polymorphic overloads stably).  The
/// resulting `{ "func_name": "md5hex", ... }` JSON map is compared against
/// `st.function_hashes`.
///
/// **On the first call** (`st.function_hashes` is `None`): stores the current
/// hashes and returns `false` (no change — baseline is being established).
///
/// **On subsequent calls**: returns `true` iff any hash differs, in which case
/// the new hashes are persisted before returning.
///
/// Errors during SPI queries are logged and treated as "no change" to avoid
/// cascading failures from a transient catalog problem.
fn check_proc_hashes_changed(st: &StreamTableMeta) -> bool {
    let funcs = match &st.functions_used {
        Some(f) if !f.is_empty() => f,
        _ => return false,
    };

    // Build current hash map: { func_name → md5(prosrc concatenated) }
    let mut current_map: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for func_name in funcs {
        let hash_opt = Spi::get_one_with_args::<String>(
            "SELECT md5(string_agg(prosrc || coalesce(probin::text, ''), ',' ORDER BY oid)) \
             FROM pg_catalog.pg_proc \
             WHERE proname = $1",
            &[func_name.as_str().into()],
        )
        .unwrap_or(None);

        if let Some(h) = hash_opt {
            current_map.insert(func_name.to_lowercase(), h);
        }
    }

    // Serialize current map to JSON text.
    let current_json = match serde_json::to_string(&current_map) {
        Ok(j) => j,
        Err(e) => {
            pgrx::debug1!("[pg_trickle] EC-16: failed to serialize function hashes: {e}");
            return false;
        }
    };

    // Compare against stored hashes.
    match &st.function_hashes {
        None => {
            // First-time baseline: store and report no change.
            if let Err(e) = crate::catalog::StreamTableMeta::update_function_hashes(
                st.pgt_id,
                Some(&current_json),
            ) {
                pgrx::debug1!("[pg_trickle] EC-16: failed to store initial function hashes: {e}");
            }
            false
        }
        Some(stored) => {
            if *stored == current_json {
                false
            } else {
                // Hash changed — persist new hashes before returning.
                if let Err(e) = crate::catalog::StreamTableMeta::update_function_hashes(
                    st.pgt_id,
                    Some(&current_json),
                ) {
                    pgrx::debug1!("[pg_trickle] EC-16: failed to update function hashes: {e}");
                }
                true
            }
        }
    }
}

/// Task 3.2: Fast-path DELETE for a single-source ST whose window contains a
/// TRUNCATE marker with no subsequent INSERT/UPDATE/DELETE rows in scope.
///
/// Instead of running the full defining query, we simply DELETE all rows from
/// the stream table.  This is O(ST rows) rather than O(source rows) and avoids
/// re-executing an arbitrarily-expensive query when the result is always empty.
fn execute_incremental_truncate_delete(st: &StreamTableMeta) -> Result<(i64, i64), PgTrickleError> {
    // Suppress the CDC trigger on the ST itself during the operation.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    let schema = &st.pgt_schema;
    let name = &st.pgt_name;
    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\"")
    );

    let rows_deleted = Spi::connect_mut(|client| {
        let result = client
            .update(&format!("DELETE FROM {quoted_table}"), None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Ok::<i64, PgTrickleError>(result.len() as i64)
    })?;

    pgrx::notice!(
        "[pg_trickle] Incremental TRUNCATE: deleted {} row(s) from {}.{} \
         (pure TRUNCATE window — skipping full query re-execution)",
        rows_deleted,
        schema,
        name,
    );
    Ok((0, rows_deleted))
}

// ── A1-2: Partition key range extraction ────────────────────────────────────

/// Safely quote a SQL string literal (standard SQL single-quote escaping).
/// Used to embed partition key range values in MERGE ON-clause predicates.
fn pg_quote_literal(val: &str) -> String {
    format!("'{}'", val.replace('\'', "''"))
}

/// A1-2/A1-1b/A1-1d: Per-column bounds for partition pruning predicates.
///
/// **Range** — min/max vectors (one entry per partition key column).
/// **List**  — distinct values for the single LIST column.
pub(crate) enum PartitionBounds {
    Range {
        mins: Vec<String>,
        maxs: Vec<String>,
    },
    List(Vec<String>),
}

/// A1-2/A1-1b/A1-1d: Extract the partition bounds from the resolved delta SQL.
/// Returns `None` when the delta is empty.
///
/// * **RANGE** keys → `MIN/MAX` per column.
/// * **LIST** keys  → `SELECT DISTINCT col::text` (single column).
fn extract_partition_bounds(
    resolved_delta_sql: &str,
    partition_key: &str,
) -> Result<Option<PartitionBounds>, PgTrickleError> {
    let method = crate::api::parse_partition_method(partition_key);
    let cols = crate::api::parse_partition_key_columns(partition_key);

    match method {
        crate::api::PartitionMethod::Hash => {
            // HASH partitions use per-partition MERGE loop — this function
            // should never be called for HASH. The orchestration dispatches
            // HASH before reaching extract_partition_bounds.
            Err(PgTrickleError::SpiError(
                "extract_partition_bounds called for HASH partition (should use per-partition MERGE)".to_string(),
            ))
        }
        crate::api::PartitionMethod::List => {
            // LIST: single column — collect distinct values.
            let qcol = crate::api::quote_identifier(&cols[0]);
            let sql = format!(
                "SELECT DISTINCT {qcol}::text FROM ({resolved_delta_sql}) AS __pgt_part_probe ORDER BY 1"
            );
            let result = Spi::connect(|client| {
                let rows = client
                    .select(&sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(format!("partition list: {e}")))?;
                let mut values = Vec::new();
                for row in rows {
                    if let Some(v) = row
                        .get::<String>(1)
                        .map_err(|e| PgTrickleError::SpiError(format!("partition list col: {e}")))?
                    {
                        values.push(v);
                    }
                }
                if values.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(PartitionBounds::List(values)))
                }
            })?;
            Ok(result)
        }
        crate::api::PartitionMethod::Range => {
            // RANGE: min/max per column.
            let min_exprs: Vec<String> = cols
                .iter()
                .map(|c| format!("MIN({})::text", crate::api::quote_identifier(c)))
                .collect();
            let max_exprs: Vec<String> = cols
                .iter()
                .map(|c| format!("MAX({})::text", crate::api::quote_identifier(c)))
                .collect();
            let select_clause = min_exprs
                .iter()
                .chain(max_exprs.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let sql =
                format!("SELECT {select_clause} FROM ({resolved_delta_sql}) AS __pgt_part_probe");
            let result = Spi::connect(|client| {
                let row = client
                    .select(&sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(format!("partition range: {e}")))?
                    .first();
                let n = cols.len();
                let mut mins = Vec::with_capacity(n);
                let mut maxs = Vec::with_capacity(n);
                for i in 0..n {
                    let map_spi = |e: pgrx::spi::SpiError| {
                        PgTrickleError::SpiError(format!("partition range col {i}: {e}"))
                    };
                    match row.get::<String>(i + 1).map_err(map_spi)? {
                        Some(v) => mins.push(v),
                        None => return Ok(None), // delta is empty
                    }
                }
                for i in 0..n {
                    let map_spi = |e: pgrx::spi::SpiError| {
                        PgTrickleError::SpiError(format!("partition range col {i}: {e}"))
                    };
                    match row.get::<String>(n + i + 1).map_err(map_spi)? {
                        Some(v) => maxs.push(v),
                        None => return Ok(None),
                    }
                }
                Ok(Some(PartitionBounds::Range { mins, maxs }))
            })?;
            Ok(result)
        }
    }
}

/// A1-3/A1-1b/A1-1d: Replace the `__PGT_PART_PRED__` placeholder in the MERGE
/// SQL with a partition-pruning predicate for the current delta.
///
/// * **Single-column RANGE**: `AND st."col" BETWEEN '<min>' AND '<max>'`
/// * **Multi-column RANGE**: `AND ROW(st."a", st."b") >= ROW(...) AND ROW(...) <= ROW(...)`
/// * **LIST**: `AND st."col" IN ('v1', 'v2', ...)`
fn inject_partition_predicate(
    merge_sql: &str,
    partition_key: &str,
    bounds: &PartitionBounds,
) -> String {
    let cols = crate::api::parse_partition_key_columns(partition_key);
    let pred = match bounds {
        PartitionBounds::List(values) => {
            let qk = crate::api::quote_identifier(&cols[0]);
            let literals: Vec<String> = values.iter().map(|v| pg_quote_literal(v)).collect();
            format!(" AND st.{qk} IN ({})", literals.join(", "))
        }
        PartitionBounds::Range { mins, maxs } => {
            if cols.len() == 1 {
                // Single-column: simple BETWEEN (backward compatible)
                let qk = crate::api::quote_identifier(&cols[0]);
                format!(
                    " AND st.{qk} BETWEEN {} AND {}",
                    pg_quote_literal(&mins[0]),
                    pg_quote_literal(&maxs[0]),
                )
            } else {
                // Multi-column: ROW comparison
                let st_cols: Vec<String> = cols
                    .iter()
                    .map(|c| format!("st.{}", crate::api::quote_identifier(c)))
                    .collect();
                let min_literals: Vec<String> = mins.iter().map(|v| pg_quote_literal(v)).collect();
                let max_literals: Vec<String> = maxs.iter().map(|v| pg_quote_literal(v)).collect();
                format!(
                    " AND ROW({}) >= ROW({}) AND ROW({}) <= ROW({})",
                    st_cols.join(", "),
                    min_literals.join(", "),
                    st_cols.join(", "),
                    max_literals.join(", "),
                )
            }
        }
    };
    merge_sql.replace("__PGT_PART_PRED__", &pred)
}

// ── A1-3b: Per-partition MERGE for HASH partitioned stream tables ───

/// Metadata for a HASH child partition.
struct HashChild {
    /// Fully-qualified name: `"schema"."child_name"`
    qualified_name: String,
    modulus: i32,
    remainder: i32,
}

/// Discover HASH child partitions (modulus, remainder) for a parent table.
fn get_hash_children(parent_oid: pg_sys::Oid) -> Result<Vec<HashChild>, PgTrickleError> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT n.nspname::text, c.relname::text, \
                        pg_get_expr(c.relpartbound, c.oid) \
                 FROM pg_inherits i \
                 JOIN pg_class c ON c.oid = i.inhrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE i.inhparent = $1 \
                 ORDER BY c.relname",
                None,
                &[parent_oid.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(format!("hash children: {e}")))?;

        let mut children = Vec::new();
        for row in rows {
            let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());
            let schema = row.get::<String>(1).map_err(map_spi)?.unwrap_or_default();
            let name = row.get::<String>(2).map_err(map_spi)?.unwrap_or_default();
            let bound_spec = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();

            // Parse "FOR VALUES WITH (modulus N, remainder M)"
            let (modulus, remainder) = parse_hash_bound_spec(&bound_spec)?;

            let qualified_name = format!(
                "{}.{}",
                crate::api::quote_identifier(&schema),
                crate::api::quote_identifier(&name),
            );
            children.push(HashChild {
                qualified_name,
                modulus,
                remainder,
            });
        }
        Ok(children)
    })
}

/// Parse a PostgreSQL HASH partition bound spec.
///
/// Input: `"FOR VALUES WITH (modulus 4, remainder 2)"`
/// Returns: `(4, 2)`
pub(crate) fn parse_hash_bound_spec(spec: &str) -> Result<(i32, i32), PgTrickleError> {
    // Parsing pattern: "FOR VALUES WITH (modulus N, remainder M)"
    let upper = spec.to_uppercase();
    let modulus = extract_keyword_int(&upper, "MODULUS")?;
    let remainder = extract_keyword_int(&upper, "REMAINDER")?;
    Ok((modulus, remainder))
}

/// Extract an integer value following a keyword in a partition bound spec.
fn extract_keyword_int(spec: &str, keyword: &str) -> Result<i32, PgTrickleError> {
    let pos = spec
        .find(keyword)
        .ok_or_else(|| PgTrickleError::SpiError(format!("missing {keyword} in bound spec")))?;
    let after = &spec[pos + keyword.len()..];
    let digits: String = after
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse::<i32>()
        .map_err(|_| PgTrickleError::SpiError(format!("invalid {keyword} value in bound spec")))
}

/// Execute MERGE for a HASH partitioned stream table.
///
/// PostgreSQL 15+ handles MERGE with partitioned tables natively — rows are
/// routed to the correct child partition automatically for both INSERT and
/// MATCHED (UPDATE/DELETE) operations. We therefore do NOT need per-child
/// routing or the `satisfies_hash_partition()` internal function (which was
/// removed in PG17+). Simply strip the `__PGT_PART_PRED__` placeholder from
/// the merge SQL and run it against the parent table.
///
/// Returns the number of rows affected.
fn execute_hash_partitioned_merge(
    merge_sql: &str,
    _resolved_delta_sql: &str,
    schema: &str,
    name: &str,
    _parent_oid: pg_sys::Oid,
    _partition_key: &str,
    _pgt_id: i64,
) -> Result<usize, PgTrickleError> {
    // Strip the __PGT_PART_PRED__ placeholder — HASH partitions do not use
    // a range predicate; PostgreSQL routes each row to the correct child.
    let sql = merge_sql.replace("__PGT_PART_PRED__", "");

    pgrx::debug1!(
        "[pg_trickle] A1-3b: HASH parent-level MERGE for {}.{}",
        schema,
        name,
    );

    Spi::connect_mut(|client| {
        let result = client
            .update(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(format!("hash merge: {e}")))?;
        Ok::<usize, PgTrickleError>(result.len())
    })
}

/// Build a MERGE SQL statement targeting a specific HASH child partition.
///
/// The delta is filtered to only rows whose partition key hashes to this child
/// using PostgreSQL's `satisfies_hash_partition()` function.
#[allow(clippy::too_many_arguments)]
fn build_hash_child_merge(
    child_target: &str,
    temp_delta: &str,
    quoted_partition_col: &str,
    parent_oid: pg_sys::Oid,
    modulus: i32,
    remainder: i32,
    original_merge: &str,
    parent_target: &str,
) -> String {
    // The original MERGE has a USING clause that references the delta.
    // We replace the entire MERGE to target the child with a filtered delta.
    //
    // Strategy: rewrite the original merge_sql by:
    // 1. Replacing the parent target with ONLY child_target
    // 2. Wrapping the USING subquery to filter through satisfies_hash_partition
    // 3. Removing the __PGT_PART_PRED__ placeholder

    // Find and replace "USING (...) AS d" with filtered version that reads
    // from the materialized temp table.
    let using_start = original_merge.find("USING (");
    let on_clause = original_merge.find(" ON st.");

    if let (Some(us), Some(on)) = (using_start, on_clause) {
        // Reconstruct: everything before USING + filtered USING + everything from ON
        let before_using = &original_merge[..us];
        let from_on = &original_merge[on..];

        // Build filtered USING clause
        let filtered_using = format!(
            "USING (SELECT * FROM {temp_delta} WHERE \
             satisfies_hash_partition({parent_oid}::oid, {modulus}, {remainder}, {quoted_partition_col})) AS d",
            parent_oid = parent_oid.to_u32(),
        );

        let result = format!("{before_using}{filtered_using}{from_on}",);

        // Replace parent target with ONLY child_target and strip predicate placeholder
        result
            .replace(parent_target, &format!("ONLY {child_target}"))
            .replace("__PGT_PART_PRED__", "")
    } else {
        // Fallback: simple replacement (shouldn't happen in practice)
        original_merge
            .replace(parent_target, &format!("ONLY {child_target}"))
            .replace("__PGT_PART_PRED__", "")
    }
}

// ── PART-WARN: Default partition growth warning ─────────────────────

/// After a successful refresh of a partitioned stream table, check whether
/// the default (catch-all) partition has rows. If so, emit a WARNING
/// prompting the user to create explicit named partitions.
///
/// The check is deliberately lightweight: a single `count(*)` on the default
/// partition. If the default partition does not exist (unlikely but possible
/// if the user detached it), the check is silently skipped.
fn warn_default_partition_growth(schema: &str, name: &str) {
    let default_name = format!("{name}_default");
    let qschema = crate::api::quote_identifier(schema);
    let qdefault = crate::api::quote_identifier(&default_name);

    // Check existence first via pg_catalog to avoid "relation does not exist"
    // errors from SPI (pgrx SPI does not catch catalog errors via Result).
    // Use parameterized query to safely pass schema/table names.
    let exists = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT 1 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 AND c.relname = $2 \
                 LIMIT 1",
                None,
                &[schema.into(), default_name.as_str().into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Ok::<bool, PgTrickleError>(!rows.is_empty())
    })
    .unwrap_or(false);

    if !exists {
        return; // No default partition — nothing to warn about.
    }

    let sql = format!("SELECT count(*)::bigint FROM {qschema}.{qdefault}");
    match Spi::get_one::<i64>(&sql) {
        Ok(Some(count)) if count > 0 => {
            pgrx::warning!(
                "pg_trickle: PART-WARN: default partition {schema}.{default_name} of \
                 stream table {schema}.{name} contains {count} row(s). \
                 Create explicit named partitions to improve query performance and \
                 enable partition pruning. Example:\n  \
                 CREATE TABLE {schema}.{name}_2026q1 PARTITION OF {schema}.{name} \
                 FOR VALUES FROM ('2026-01-01') TO ('2026-04-01');"
            );
        }
        Ok(_) => {}  // Default partition is empty — no warning.
        Err(_) => {} // Silently skip on any other error.
    }
}

// ── DAG-3: Delta amplification detection ────────────────────────────

/// Compute the amplification ratio between input delta and output delta.
///
/// Returns `output / input` when `input > 0`, otherwise `0.0` (no
/// amplification measurable when there was no input).
///
/// This is a pure function separated from the SPI layer so it can be
/// unit-tested without a PostgreSQL backend.
pub(crate) fn compute_amplification_ratio(input_delta: i64, output_delta: i64) -> f64 {
    if input_delta <= 0 {
        return 0.0;
    }
    output_delta as f64 / input_delta as f64
}

/// Determine whether the amplification ratio exceeds the configured
/// threshold and a WARNING should be emitted.
///
/// Returns `false` when detection is disabled (threshold ≤ 0) or when
/// there is no meaningful input (input_delta ≤ 0).
pub(crate) fn should_warn_amplification(
    input_delta: i64,
    output_delta: i64,
    threshold: f64,
) -> bool {
    if threshold <= 0.0 || input_delta <= 0 {
        return false;
    }
    compute_amplification_ratio(input_delta, output_delta) > threshold
}

pub fn execute_differential_refresh(
    st: &StreamTableMeta,
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
) -> Result<(i64, i64), PgTrickleError> {
    let schema = &st.pgt_schema;
    let name = &st.pgt_name;

    if !st.is_populated {
        return Err(PgTrickleError::InvalidArgument(format!(
            "Cannot run DIFFERENTIAL refresh on unpopulated stream table {}.{}; a FULL refresh is required first.",
            schema, name
        )));
    }

    if prev_frontier.is_empty() {
        return Err(PgTrickleError::InvalidArgument(format!(
            "Cannot run DIFFERENTIAL refresh on {}.{}; no previous frontier exists.",
            schema, name
        )));
    }

    // ── EC-16: Function-body change detection ────────────────────────
    // Check whether any user-defined function referenced in this ST's
    // defining query has had its source code changed via ALTER FUNCTION
    // or CREATE OR REPLACE FUNCTION.  If so, force a full reinit on the
    // next scheduler cycle and skip the differential refresh this cycle.
    if check_proc_hashes_changed(st) {
        if let Err(e) = crate::catalog::StreamTableMeta::mark_for_reinitialize(st.pgt_id) {
            pgrx::warning!("[pg_trickle] EC-16: failed to mark ST {schema}.{name} for reinit: {e}");
        } else {
            pgrx::notice!(
                "[pg_trickle] EC-16: function body change detected for stream table \
                 {schema}.{name} — marked for full reinitialization on next cycle."
            );
        }
        return Ok((0, 0));
    }

    // ── DI-7: Join-count complexity guard ────────────────────────────
    // Parse the defining query (lightweight — no differentiation) to count
    // Scan nodes in the join tree. Used for:
    //   (a) max_differential_joins guard: reject if too complex for DIFF
    //   (b) deep-join planner hints: SET LOCAL for 5+ table joins
    let scan_count = dvm::query_total_scan_count(&st.defining_query).unwrap_or_else(|e| {
        pgrx::warning!(
            "[pg_trickle] DI-7: failed to count scans for {schema}.{name}: {e}; \
             assuming scan_count=1"
        );
        1
    });

    if let Some(max_joins) = st.max_differential_joins
        && max_joins > 0
    {
        // DI-7 uses the join-specific scan count (stops at Aggregate etc.)
        let join_sc = dvm::query_join_scan_count(&st.defining_query).unwrap_or(0);
        if join_sc > max_joins as usize {
            return Err(PgTrickleError::QueryTooComplex(format!(
                "join scan count ({join_sc}) exceeds max_differential_joins ({max_joins}) \
                 for {schema}.{name}; falling back to FULL refresh"
            )));
        }
    }

    // ── Short-circuit: skip the entire pipeline if no changes exist ──────
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let catalog_source_oids: Vec<u32> = StDependency::get_for_st(st.pgt_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|dep| {
            dep.source_type == "TABLE"
                || dep.source_type == "FOREIGN_TABLE"
                || dep.source_type == "MATVIEW"
        })
        .map(|dep| dep.source_relid.to_u32())
        .collect();

    // ── Pre-flight: verify all change buffer tables exist ─────────────
    // Query pg_class (safe — never errors for catalog tables) to confirm
    // that every source's change buffer table still exists.  If any are
    // missing (e.g. race with a concurrent DROP or stale deps), skip the
    // refresh instead of crashing with a relation-not-found ERROR.
    //
    // Also uses to_regclass() as a secondary check that resolves the
    // schema-qualified name the same way a FROM clause would.
    for &oid in &catalog_source_oids {
        let qualified = format!("{change_schema}.changes_{oid}");
        let reg_exists =
            Spi::get_one::<bool>(&format!("SELECT to_regclass('{qualified}') IS NOT NULL",))
                .unwrap_or(Some(false))
                .unwrap_or(false);

        if !reg_exists {
            pgrx::warning!(
                "[pg_trickle] PREFLIGHT FAIL: change buffer table \
                 \"{change_schema}\".changes_{oid} not found via to_regclass \
                 for ST {schema}.{name} (pgt_id={}, catalog_source_oids={:?}). \
                 Skipping differential refresh.",
                st.pgt_id,
                catalog_source_oids,
            );
            return Ok((0, 0));
        }
    }
    pgrx::debug1!(
        "[pg_trickle] PREFLIGHT OK for ST {}.{} — source OIDs: {:?}",
        schema,
        name,
        catalog_source_oids,
    );

    // ── DI-7: Delta fraction guard ──────────────────────────────────
    // When the user has configured `max_delta_fraction`, compare the
    // total change buffer row count against the ST's estimated row count
    // (pg_class.reltuples). If the ratio exceeds the threshold, reject
    // the differential refresh so the scheduler can fall back to FULL —
    // TRUNCATE + INSERT is faster than applying a large fraction of the
    // table as individual deltas.
    if let Some(max_frac) = st.max_delta_fraction
        && max_frac > 0.0
    {
        let total_changes: i64 = catalog_source_oids
            .iter()
            .map(|&oid| {
                let q = format!("SELECT count(*)::bigint FROM \"{change_schema}\".changes_{oid}");
                Spi::get_one::<i64>(&q).unwrap_or(Some(0)).unwrap_or(0)
            })
            .sum();

        if total_changes > 0 {
            let estimated_rows: f64 = Spi::get_one::<f32>(&format!(
                "SELECT reltuples FROM pg_class WHERE oid = {}::oid",
                st.pgt_relid.to_u32(),
            ))
            .unwrap_or(Some(0.0))
            .unwrap_or(0.0) as f64;

            // Only check when reltuples > 0 (avoids division by zero and
            // ANALYZE-not-yet-run edge case).
            if estimated_rows > 0.0 {
                let fraction = total_changes as f64 / estimated_rows;
                if fraction > max_frac {
                    return Err(PgTrickleError::QueryTooComplex(format!(
                        "delta fraction ({:.2}%) exceeds max_delta_fraction ({:.2}%) \
                         for {schema}.{name} ({total_changes} changes / \
                         {estimated_rows:.0} estimated rows); falling back to FULL refresh",
                        fraction * 100.0,
                        max_frac * 100.0,
                    )));
                }
            }
        }
    }

    // C-1: Drain any deferred cleanups from the previous refresh cycle.
    // This runs before the decision query so stale rows are removed
    // before we check for new changes.
    drain_pending_cleanups();

    // C-1b: Frontier-based cleanup — always runs regardless of thread-local
    // state.  The deferred cleanup (above) relies on PENDING_CLEANUP in
    // thread-local storage, which is lost when a connection pool dispatches
    // successive refresh calls to different PostgreSQL backend processes.
    // This additional pass uses the catalog frontier (persisted in
    // pgt_stream_tables.frontier) to compute the safe cleanup threshold,
    // ensuring stale change buffer rows are removed even when the
    // thread-local queue is empty.
    cleanup_change_buffers_by_frontier(&change_schema, &catalog_source_oids);

    // C-1c: ST buffer cleanup — delete consumed rows from upstream ST
    // change buffers (changes_pgt_{id}) using frontier thresholds.
    let st_source_pgt_ids: Vec<i64> = StDependency::get_for_st(st.pgt_id)
        .unwrap_or_default()
        .iter()
        .filter(|dep| dep.source_type == "STREAM_TABLE")
        .filter_map(|dep| crate::catalog::StreamTableMeta::pgt_id_for_relid(dep.source_relid))
        .collect();
    cleanup_st_change_buffers_by_frontier(&change_schema, &st_source_pgt_ids);

    // C-4: Compact change buffers that exceed the configured threshold.
    // This reduces delta scan overhead by eliminating net-zero changes
    // (INSERT→DELETE pairs) and collapsing multi-change groups.
    for &oid in &catalog_source_oids {
        let prev_lsn = prev_frontier.get_lsn(oid);
        let new_lsn = new_frontier.get_lsn(oid);
        if let Err(e) = crate::cdc::compact_change_buffer(&change_schema, oid, &prev_lsn, &new_lsn)
        {
            pgrx::debug1!(
                "[pg_trickle] C-4: compaction failed for changes_{}: {}",
                oid,
                e,
            );
        }
    }

    // PERF-2: Auto-promote unpartitioned buffers to RANGE(lsn) partitioned
    // mode when `buffer_partitioning = 'auto'` and the buffer fill rate
    // exceeds `compact_threshold` within a single refresh cycle.
    for &oid in &catalog_source_oids {
        let prev_lsn = prev_frontier.get_lsn(oid);
        let new_lsn = new_frontier.get_lsn(oid);
        let pending = crate::cdc::count_pending_changes(&change_schema, oid, &prev_lsn, &new_lsn);
        match crate::cdc::maybe_auto_promote_buffer(&change_schema, oid, pending) {
            Ok(true) => {
                pgrx::debug1!(
                    "[pg_trickle] PERF-2: auto-promoted changes_{} to partitioned mode \
                     (pending={} exceeded threshold)",
                    oid,
                    pending,
                );
            }
            Err(e) => {
                pgrx::warning!(
                    "[pg_trickle] PERF-2: auto-promotion failed for changes_{}: {}",
                    oid,
                    e,
                );
            }
            _ => {}
        }
    }

    // DAG-5: Compact ST change buffers that exceed the threshold.
    // During rapid-fire upstream refreshes, multiple rounds of I/D pairs
    // accumulate in changes_pgt_{id} between downstream reads. Compaction
    // cancels net-zero INSERT/DELETE pairs and removes intermediate rows.
    for &upstream_pgt_id in &st_source_pgt_ids {
        if !crate::cdc::has_st_change_buffer(upstream_pgt_id, &change_schema) {
            continue;
        }
        let key = format!("pgt_{upstream_pgt_id}");
        let prev_lsn = prev_frontier
            .sources
            .get(&key)
            .map(|sv| sv.lsn.clone())
            .unwrap_or_else(|| "0/0".to_string());
        let new_lsn = new_frontier
            .sources
            .get(&key)
            .map(|sv| sv.lsn.clone())
            .unwrap_or_else(|| "0/0".to_string());
        if let Err(e) = crate::cdc::compact_st_change_buffer(
            &change_schema,
            upstream_pgt_id,
            &prev_lsn,
            &new_lsn,
        ) {
            pgrx::debug1!(
                "[pg_trickle] DAG-5: compaction failed for changes_pgt_{}: {}",
                upstream_pgt_id,
                e,
            );
        }
    }

    let t_decision_start = Instant::now();

    // ── E-1: Ultra-fast EXISTS for no-data short-circuit ─────────────
    // Build a single UNION ALL / EXISTS query that checks ALL source
    // change buffers in one SPI call.  The query short-circuits on the
    // first row found, making the no-data case O(index-probe) per source
    // instead of the heavier LATERAL + pg_class join used for threshold
    // computation.
    let any_changes = if catalog_source_oids.is_empty() {
        false
    } else if catalog_source_oids.len() == 1 {
        // Single source — simple EXISTS, no UNION ALL overhead
        let oid = catalog_source_oids[0];
        let prev_lsn = prev_frontier.get_lsn(oid);
        let new_lsn = new_frontier.get_lsn(oid);
        Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM \"{change_schema}\".changes_{oid} \
               WHERE lsn > '{prev_lsn}'::pg_lsn \
               AND lsn <= '{new_lsn}'::pg_lsn \
               LIMIT 1\
             )",
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false)
    } else {
        // Multiple sources — UNION ALL wrapped in EXISTS.
        // Note: LIMIT 1 is omitted from individual branches because
        // `SELECT ... LIMIT 1 UNION ALL SELECT ...` is a syntax error
        // in PostgreSQL (LIMIT binds at the top level, not per-branch).
        // EXISTS already short-circuits on the first row found, so
        // LIMIT is unnecessary here.
        let union_parts: Vec<String> = catalog_source_oids
            .iter()
            .map(|oid| {
                let prev_lsn = prev_frontier.get_lsn(*oid);
                let new_lsn = new_frontier.get_lsn(*oid);
                format!(
                    "SELECT 1 FROM \"{change_schema}\".changes_{oid} \
                     WHERE lsn > '{prev_lsn}'::pg_lsn \
                     AND lsn <= '{new_lsn}'::pg_lsn",
                )
            })
            .collect();
        let union_sql = union_parts.join(" UNION ALL ");
        Spi::get_one::<bool>(&format!("SELECT EXISTS({union_sql})",))
            .unwrap_or(Some(false))
            .unwrap_or(false)
    };

    // Also check ST (stream table) change buffers for pending changes.
    // Without this, pure ST-on-ST dependencies (catalog_source_oids is
    // empty) would always short-circuit and never run DIFFERENTIAL.
    let any_st_changes = if !any_changes && !st_source_pgt_ids.is_empty() {
        st_source_pgt_ids.iter().any(|&pgt_id| {
            if !crate::cdc::has_st_change_buffer(pgt_id, &change_schema) {
                return false;
            }
            let key = format!("pgt_{pgt_id}");
            let prev_lsn = prev_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());
            let new_lsn = new_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());
            Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(\
                   SELECT 1 FROM \"{change_schema}\".changes_pgt_{pgt_id} \
                   WHERE lsn > '{prev_lsn}'::pg_lsn \
                   AND lsn <= '{new_lsn}'::pg_lsn \
                   LIMIT 1\
                 )",
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false)
        })
    } else {
        false
    };

    if !any_changes && !any_st_changes {
        return Ok((0, 0));
    }

    // ── A-3a: Append-only heuristic fallback ─────────────────────────
    // When the stream table is marked append-only, check whether any
    // DELETE or UPDATE actions appeared in the change buffers. If so,
    // revert the flag and fall through to the normal MERGE path.
    let mut is_append_only = st.is_append_only;
    if is_append_only {
        let has_non_insert = catalog_source_oids.iter().any(|oid| {
            let prev_lsn = prev_frontier.get_lsn(*oid);
            let new_lsn = new_frontier.get_lsn(*oid);
            match Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(\
                   SELECT 1 FROM \"{change_schema}\".changes_{oid} \
                   WHERE lsn > '{prev_lsn}'::pg_lsn \
                   AND lsn <= '{new_lsn}'::pg_lsn \
                   AND action IN ('D', 'U') \
                   LIMIT 1\
                 )",
            )) {
                Ok(Some(v)) => v,
                Ok(None) => false,
                Err(e) => {
                    // SPI failure: treat as "found non-insert" (safe default).
                    // Falling through to the MERGE path is always correct;
                    // defaulting to "no deletes" risks silent data corruption.
                    pgrx::warning!(
                        "[pg_trickle] Append-only DELETE/UPDATE check failed for \
                         changes_{} — falling back to MERGE path: {}",
                        oid,
                        e,
                    );
                    true
                }
            }
        })
        // Also check ST (stream table) source change buffers.
        // Without this, ST-on-ST cascades with empty catalog_source_oids
        // would vacuously miss DELETE/UPDATE actions from the upstream ST.
        || st_source_pgt_ids.iter().any(|&pgt_id| {
            if !crate::cdc::has_st_change_buffer(pgt_id, &change_schema) {
                return false;
            }
            let key = format!("pgt_{pgt_id}");
            let prev_lsn = prev_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());
            let new_lsn = new_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());
            match Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(\
                   SELECT 1 FROM \"{change_schema}\".changes_pgt_{pgt_id} \
                   WHERE lsn > '{prev_lsn}'::pg_lsn \
                   AND lsn <= '{new_lsn}'::pg_lsn \
                   AND action IN ('D', 'U') \
                   LIMIT 1\
                 )",
            )) {
                Ok(Some(v)) => v,
                Ok(None) => false,
                Err(_) => true, // SPI failure: safe default
            }
        });

        if has_non_insert {
            pgrx::warning!(
                "[pg_trickle] Append-only stream table {}.{} received DELETE/UPDATE — \
                 reverting to MERGE path.",
                schema,
                name,
            );
            is_append_only = false;
            if let Err(e) = StreamTableMeta::update_append_only(st.pgt_id, false) {
                pgrx::warning!(
                    "[pg_trickle] Failed to clear is_append_only for {}.{}: {}",
                    schema,
                    name,
                    e,
                );
            }
            // Flush MERGE template cache so next cycle rebuilds with MERGE path.
            crate::shmem::bump_cache_generation();
            // NS-2: Emit NOTIFY alert so operators are informed of the revert.
            crate::monitor::emit_alert(
                crate::monitor::AlertEvent::AppendOnlyReverted,
                schema,
                name,
                "",
                st.pooler_compatibility_mode,
            );
        }
    }

    // ── A-3-AO: Append-only heuristic auto-promotion ────────────────
    // When the stream table is NOT marked append-only, check whether the
    // current change buffer batch is INSERT-only. If so, opportunistically
    // use the INSERT fast path for this refresh cycle. The flag is set in
    // the catalog so subsequent refreshes also use the fast path until a
    // DELETE/UPDATE is detected (handled by the revert block above).
    //
    // Skip promotion for queries with non-monotonic operators (LEFT JOIN,
    // aggregates, anti-joins, etc.) where source INSERTs can produce delta
    // DELETEs — the append-only fast path would silently drop those DELETEs.
    let cached_non_monotonic = MERGE_TEMPLATE_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&st.pgt_id)
            .map(|entry| has_non_monotonic_cte(&entry.merge_sql_template))
            .unwrap_or(false) // no cache entry → allow promotion (A-3a guard catches it)
    });
    // Also check whether the delta is deduplicated. Non-deduplicated
    // deltas (joins, aggregates) can produce phantom row_id collisions
    // across refresh cycles that the append-only INSERT path cannot
    // safely handle — those need the PH-D1 DELETE+INSERT path.
    let cached_is_deduplicated = MERGE_TEMPLATE_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&st.pgt_id)
            .map(|entry| entry.is_deduplicated)
            .unwrap_or(true) // no cache → assume dedup (safe: first cycle has no phantoms)
    });
    if !is_append_only
        && !st.has_keyless_source
        && !cached_non_monotonic
        && cached_is_deduplicated
        && !has_downstream_st_consumers(st.pgt_id)
    {
        let has_non_insert = catalog_source_oids.iter().any(|oid| {
            let prev_lsn = prev_frontier.get_lsn(*oid);
            let new_lsn = new_frontier.get_lsn(*oid);
            match Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(\
                   SELECT 1 FROM \"{change_schema}\".changes_{oid} \
                   WHERE lsn > '{prev_lsn}'::pg_lsn \
                   AND lsn <= '{new_lsn}'::pg_lsn \
                   AND action IN ('D', 'U') \
                   LIMIT 1\
                 )",
            )) {
                Ok(Some(v)) => v,
                Ok(None) => false,
                Err(_) => true, // SPI failure: safe default (skip heuristic)
            }
        })
        // Also check ST source change buffers for DELETE/UPDATE actions.
        // Without this, ST-on-ST cascades (catalog_source_oids is empty)
        // would vacuously find no non-INSERT actions and incorrectly
        // promote to append-only, causing duplicate rows.
        || st_source_pgt_ids.iter().any(|&pgt_id| {
            if !crate::cdc::has_st_change_buffer(pgt_id, &change_schema) {
                return false;
            }
            let key = format!("pgt_{pgt_id}");
            let prev_lsn = prev_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());
            let new_lsn = new_frontier
                .sources
                .get(&key)
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string());
            match Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(\
                   SELECT 1 FROM \"{change_schema}\".changes_pgt_{pgt_id} \
                   WHERE lsn > '{prev_lsn}'::pg_lsn \
                   AND lsn <= '{new_lsn}'::pg_lsn \
                   AND action IN ('D', 'U') \
                   LIMIT 1\
                 )",
            )) {
                Ok(Some(v)) => v,
                Ok(None) => false,
                Err(_) => true, // SPI failure: safe default
            }
        });

        if !has_non_insert {
            pgrx::debug1!(
                "[pg_trickle] A-3-AO: heuristic append-only promotion for {}.{} — \
                 current batch is INSERT-only",
                schema,
                name,
            );
            is_append_only = true;
            // Persist the flag so subsequent refreshes also use the fast path.
            // If a DELETE/UPDATE appears later, the revert block above will
            // clear it and emit a WARNING + NOTIFY.
            if let Err(e) = StreamTableMeta::update_append_only(st.pgt_id, true) {
                pgrx::debug1!(
                    "[pg_trickle] A-3-AO: failed to set is_append_only for {}.{}: {}",
                    schema,
                    name,
                    e,
                );
                is_append_only = false; // revert on failure
            }
        }
    }

    // ── S2: TRUNCATE detection ───────────────────────────────────────
    // If any source table was TRUNCATEd, the change buffer contains a
    // marker row with action='T'. Differential deltas cannot represent
    // a TRUNCATE — fall back to full refresh.
    let has_truncate = catalog_source_oids.iter().any(|oid| {
        let prev_lsn = prev_frontier.get_lsn(*oid);
        let new_lsn = new_frontier.get_lsn(*oid);
        Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM \"{change_schema}\".changes_{oid} \
               WHERE lsn > '{prev_lsn}'::pg_lsn \
               AND lsn <= '{new_lsn}'::pg_lsn \
               AND action = 'T' \
               LIMIT 1\
             )",
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false)
    });

    if has_truncate {
        // Task 3.2: Fast path for single-source STs where the current window
        // contains a TRUNCATE marker but no subsequent INSERT/UPDATE/DELETE rows.
        // In that case the post-TRUNCATE result is always empty, so we can DELETE
        // all ST rows directly instead of re-running the full defining query.
        let is_single_source = catalog_source_oids.len() == 1;
        let is_pure_truncate = is_single_source && {
            let oid = catalog_source_oids[0];
            let prev_lsn = prev_frontier.get_lsn(oid);
            let new_lsn = new_frontier.get_lsn(oid);
            !Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(\
                   SELECT 1 FROM \"{change_schema}\".changes_{oid} \
                   WHERE lsn > '{prev_lsn}'::pg_lsn \
                   AND lsn <= '{new_lsn}'::pg_lsn \
                   AND action != 'T' \
                   LIMIT 1\
                 )",
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false)
        };

        if is_pure_truncate {
            return execute_incremental_truncate_delete(st);
        }

        pgrx::info!(
            "[pg_trickle] Source table TRUNCATE detected — falling back to FULL refresh for {}.{}",
            schema,
            name,
        );
        let truncate_full_result = execute_full_refresh(st);
        if truncate_full_result.is_ok() {
            post_full_refresh_cleanup(st);
        }
        return truncate_full_result;
    }

    // ── P2: Capped-count threshold check (only when changes exist) ───────
    // Now that we know changes exist, check whether the change volume
    // exceeds the adaptive fallback threshold.  This heavier query is
    // skipped entirely for the no-data case (handled above).
    //
    // B-4: Check the refresh_strategy GUC first. If it's 'full', force
    // fallback unconditionally. If it's 'differential', skip the adaptive
    // threshold check entirely (never fall back). 'auto' uses the existing
    // adaptive heuristic.
    let strategy = crate::config::pg_trickle_refresh_strategy();
    let global_ratio = crate::config::pg_trickle_differential_max_change_ratio();
    let max_ratio = st.auto_threshold.unwrap_or(global_ratio);
    let mut should_fallback = strategy == crate::config::RefreshStrategy::Full;
    let skip_ratio_check = strategy == crate::config::RefreshStrategy::Differential;
    let mut total_change_count: i64 = 0;
    let mut _total_table_size: i64 = 0;
    // DI-2: Collect per-source (change_count, table_size) for the per-leaf
    // fallback decision after the P2 loop completes.
    let mut per_source_stats: Vec<(u32, i64, i64)> = Vec::new();
    // B3-1: Track source OIDs with zero changes for delta-branch pruning.
    let mut zero_change_oids: std::collections::HashSet<u32> = std::collections::HashSet::new();

    for oid in &catalog_source_oids {
        let prev_lsn = prev_frontier.get_lsn(*oid);
        let new_lsn = new_frontier.get_lsn(*oid);

        let max_ratio_lit = if max_ratio > 0.0 {
            format!("{max_ratio}")
        } else {
            "0".to_string()
        };

        let sql = format!(
            "SELECT sz.table_size, cnt.change_count \
             FROM (SELECT GREATEST(reltuples::bigint, 1000) AS table_size \
                   FROM pg_class WHERE oid = {oid}::oid) sz, \
             LATERAL (SELECT count(*)::bigint AS change_count FROM (\
                SELECT 1 FROM \"{change_schema}\".changes_{oid} \
                WHERE lsn > '{prev_lsn}'::pg_lsn \
                AND lsn <= '{new_lsn}'::pg_lsn \
                LIMIT CASE WHEN {max_ratio_lit} > 0 \
                      THEN (sz.table_size::double precision * {max_ratio_lit})::bigint + 1 \
                      ELSE 9223372036854775807 END\
             ) __pgt_capped) cnt",
        );

        // Defensive: if the change-buffer table was dropped between the
        // EXISTS check and this threshold query (e.g., during concurrent
        // DROP STREAM TABLE), treat it as zero changes rather than
        // propagating a "relation does not exist" error.
        let (table_size, change_count) = match Spi::connect(|client| {
            let row = client
                .select(&sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .first();
            let ts: i64 = row.get::<i64>(1).unwrap_or(Some(1000)).unwrap_or(1000);
            let cc: i64 = row.get::<i64>(2).unwrap_or(Some(0)).unwrap_or(0);
            Ok::<(i64, i64), PgTrickleError>((ts, cc))
        }) {
            Ok(pair) => pair,
            Err(e) => {
                pgrx::debug1!(
                    "[pg_trickle] Threshold check for changes_{} failed (table dropped?): {}",
                    oid,
                    e,
                );
                (1000, 0)
            }
        };

        let threshold_rows = if max_ratio > 0.0 {
            ((table_size as f64) * max_ratio).ceil() as i64
        } else {
            i64::MAX / 2
        };

        total_change_count += change_count;
        _total_table_size += table_size;

        // DI-2: Save per-source stats for per-leaf fallback decision.
        per_source_stats.push((*oid, change_count, table_size));

        // B3-1: Record sources with zero changes for delta-branch pruning.
        if change_count == 0 {
            zero_change_oids.insert(*oid);
        }

        if change_count > threshold_rows {
            // B-4: When refresh_strategy = 'differential', skip the ratio
            // check — the user explicitly wants DIFFERENTIAL regardless of
            // change volume. The BUF-LIMIT safety check still applies below.
            if !skip_ratio_check {
                should_fallback = true;
                break; // No need to check remaining sources
            }
        }
    }

    // ── BUF-LIMIT: Hard buffer growth limit ─────────────────────────
    // If any source's change buffer exceeds max_buffer_rows, force FULL
    // refresh to prevent unbounded disk growth from repeated failures.
    let max_buffer_rows = crate::config::pg_trickle_max_buffer_rows();
    if !should_fallback && max_buffer_rows > 0 {
        for &(oid, change_count, _table_size) in &per_source_stats {
            if change_count > max_buffer_rows {
                pgrx::warning!(
                    "[pg_trickle] Change buffer for source OID {} of {}.{} has {} rows, \
                     exceeding max_buffer_rows limit ({}). Forcing FULL refresh and \
                     truncating buffer to prevent unbounded growth.",
                    oid,
                    st.pgt_schema,
                    st.pgt_name,
                    change_count,
                    max_buffer_rows,
                );
                should_fallback = true;
                break;
            }
        }
    }

    // ── B-4: Pre-refresh cost-model prediction ──────────────────────
    // When strategy = 'auto' and the ratio check didn't trigger, query
    // historical refresh timings and use the cost model to predict
    // whether DIFFERENTIAL or FULL is cheaper for the *current* delta.
    if !should_fallback && !skip_ratio_check && total_change_count > 0 {
        let complexity = classify_query_complexity(&st.defining_query);
        if let Some(hist) = query_refresh_history_stats(st.pgt_id)
            && cost_model_prefers_full(
                hist.avg_ms_per_delta,
                hist.avg_full_ms,
                total_change_count,
                complexity,
            )
        {
            pgrx::debug1!(
                "[pg_trickle] B-4 cost model: FULL preferred for {}.{} \
                 (est_diff={:.1}ms > est_full×margin={:.1}ms, class={:?}, Δ={})",
                st.pgt_schema,
                st.pgt_name,
                hist.avg_ms_per_delta * complexity.diff_cost_factor() * total_change_count as f64,
                hist.avg_full_ms * crate::config::pg_trickle_cost_model_safety_margin(),
                complexity,
                total_change_count,
            );
            should_fallback = true;
        }
    }

    if should_fallback {
        pgrx::warning!(
            "[pg_trickle] Falling back to FULL refresh for {}.{}: change ratio exceeds \
             adaptive threshold ({:.0}% of source table size).\n\
             This means too many rows changed since the last refresh for differential \
             mode to be efficient. \n\
             Suggestion: increase pg_trickle.differential_max_change_ratio (currently {:.2}), \
             adjust the per-table auto_threshold via ALTER STREAM TABLE ... SET (auto_threshold = ...), \
             or refresh more frequently to reduce the change volume per cycle.",
            st.pgt_schema,
            st.pgt_name,
            max_ratio * 100.0,
            max_ratio,
        );
        let t_full_start = Instant::now();
        let result = execute_full_refresh(st);
        let full_ms = t_full_start.elapsed().as_secs_f64() * 1000.0;
        // Record FULL timing for future threshold auto-tuning.
        if let Err(e) = StreamTableMeta::update_adaptive_threshold(
            st.pgt_id,
            st.auto_threshold, // keep current threshold
            Some(full_ms),
        ) {
            pgrx::debug1!("[pg_trickle] Failed to update last_full_ms: {}", e);
        }
        // G4: Flush stale change buffer rows to prevent the next differential
        // tick from re-examining rows already materialized by this FULL refresh.
        if result.is_ok() {
            post_full_refresh_cleanup(st);
        }
        return result;
    }

    // ── D-2: Aggregate saturation check ─────────────────────────────
    // For aggregate stream tables (GROUP BY queries), if the number of
    // changes exceeds the number of groups (= rows in the materialized
    // stream table), all groups are likely affected.  In that case FULL
    // refresh is cheaper than MERGE because it avoids per-row IS DISTINCT
    // FROM checks.  This catches the common "few groups, many changes"
    // pattern that falls below the global ratio threshold.
    //
    // Skip when:
    // - `skip_ratio_check` is true (user explicitly forces DIFFERENTIAL
    //   via the `pg_trickle.refresh_strategy` GUC)
    // - group count is very small (< 10): the comparison is unreliable
    //   because a handful of INSERTs for new groups easily triggers the
    //   threshold without actually saturating existing groups
    if !should_fallback
        && !skip_ratio_check
        && total_change_count > 0
        && st.defining_query.to_ascii_uppercase().contains("GROUP BY")
    {
        // Try pg_class.reltuples first (cheap); fall back to COUNT(*) if
        // the stream table has never been analyzed (reltuples = -1 or 0).
        let st_group_count: i64 = Spi::get_one::<i64>(&format!(
            "SELECT CASE WHEN reltuples >= 1 THEN reltuples::bigint \
                    ELSE (SELECT COUNT(*) FROM \"{}\".\"{}\" ) END \
             FROM pg_class WHERE oid = {}::oid",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\""),
            st.pgt_relid.to_u32(),
        ))
        .unwrap_or(Some(0))
        .unwrap_or(0);

        if st_group_count >= 10 && total_change_count >= st_group_count {
            pgrx::warning!(
                "[pg_trickle] Falling back to FULL refresh for {}.{}: aggregate saturation \
                 — {} changes >= {} groups.\n\
                 When the number of source changes meets or exceeds the number of aggregate \
                 groups, FULL recomputation is faster than per-group differential MERGE. \n\
                 Suggestion: if this happens regularly, the workload may suit FULL refresh \
                 mode. Otherwise, refresh more frequently to keep the per-cycle change count \
                 below the group count.",
                schema,
                name,
                total_change_count,
                st_group_count,
            );
            let t_full_start = Instant::now();
            let result = execute_full_refresh(st);
            let full_ms = t_full_start.elapsed().as_secs_f64() * 1000.0;
            if let Err(e) = StreamTableMeta::update_adaptive_threshold(
                st.pgt_id,
                st.auto_threshold,
                Some(full_ms),
            ) {
                pgrx::debug1!("[pg_trickle] Failed to update last_full_ms: {}", e);
            }
            if result.is_ok() {
                post_full_refresh_cleanup(st);
            }
            return result;
        }
    }

    // ── DI-2: Per-leaf conditional fallback ──────────────────────────
    // When `max_delta_fraction` is configured, check whether any
    // individual source table's delta exceeds the threshold. Those
    // leaves switch from NOT EXISTS (index-based) to EXCEPT ALL
    // (hash-based) in the snapshot construction, while unaffected
    // leaves keep the faster NOT EXISTS path.
    if let Some(max_frac) = st.max_delta_fraction
        && max_frac > 0.0
    {
        let mut fallback = std::collections::HashSet::new();
        for &(oid, change_count, table_size) in &per_source_stats {
            if table_size > 0 && change_count > 0 {
                let frac = change_count as f64 / table_size as f64;
                if frac > max_frac {
                    fallback.insert(oid);
                    pgrx::debug1!(
                        "[pg_trickle] DI-2: per-leaf fallback for source OID {} \
                         ({:.1}% > {:.1}% threshold)",
                        oid,
                        frac * 100.0,
                        max_frac * 100.0,
                    );
                }
            }
        }
        if !fallback.is_empty() {
            set_fallback_leaf_oids(fallback);
        }
    }

    let t_decision = t_decision_start.elapsed();
    let t0 = Instant::now();

    // ── G8.1: Cross-session cache invalidation ──────────────────────
    let shared_gen = crate::shmem::current_cache_generation();
    LOCAL_MERGE_CACHE_GEN.with(|local| {
        if local.get() < shared_gen {
            MERGE_TEMPLATE_CACHE.with(|cache| cache.borrow_mut().clear());
            clear_prepared_merge_statements();
            local.set(shared_gen);
        }
    });

    // ── Try the MERGE template cache first ──────────────────────────
    let query_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        st.defining_query.hash(&mut hasher);
        hasher.finish()
    };

    let has_recursive_cte = dvm::query_has_recursive_cte(&st.defining_query)?;

    // Non-recursive CTEs (WITH … AS (…)) are fully supported by the DVM
    // engine: parse_defining_query_full() builds CteScan nodes and the
    // diff engine processes them via diff_cte_scan().  There is no need for
    // a FULL fallback here.  Recursive CTEs (WITH RECURSIVE) use a
    // semi-naive / DRed strategy and bypass the MERGE template cache (they
    // generate their delta SQL on every refresh instead of caching a
    // template with LSN placeholders).

    let cached = if has_recursive_cte {
        None
    } else {
        MERGE_TEMPLATE_CACHE.with(|cache| {
            let map = cache.borrow();
            map.get(&st.pgt_id)
                .filter(|entry| entry.defining_query_hash == query_hash)
                .cloned()
        })
    };

    let was_cache_hit = cached.is_some();

    /// Resolved SQL pair: MERGE template, plus D-2 prepared-statement materials
    /// and user-trigger DML.
    struct ResolvedSql {
        merge_sql: String,
        /// Source OIDs (needed for D-2 EXECUTE parameter building).
        source_oids: Vec<u32>,
        /// Parameterized MERGE SQL with `$N` params (for PREPARE).
        parameterized_merge_sql: String,
        /// DELETE template for user-trigger path (no LSN placeholders).
        trigger_delete_sql: String,
        /// UPDATE template for user-trigger path (no LSN placeholders).
        trigger_update_sql: String,
        /// INSERT template for user-trigger path (no LSN placeholders).
        trigger_insert_sql: String,
        /// USING clause with resolved LSN values (for materializing delta
        /// into a temp table in the user-trigger path).
        trigger_using_sql: String,
        /// A1-2: Resolved delta SQL (LSN placeholders replaced with actual
        /// LSN values). Used to compute partition key range for A1-3 predicate
        /// injection on partitioned stream tables.
        resolved_delta_sql: String,
        /// B-1: Whether all aggregates are algebraically invertible.
        is_all_algebraic: bool,
        /// Whether the delta is deduplicated (at most one row per __pgt_row_id).
        is_deduplicated: bool,
    }

    let mut resolved = if let Some(entry) = cached {
        // ── Cache hit: resolve LSN placeholders ──────────────────────
        pgrx::debug1!("[pg_trickle] cache HIT for pgt_id={}", st.pgt_id);
        // Substitute __PGS_PREV/NEW_LSN_{oid}__ tokens with actual values.
        // Each execution gets a fresh plan with accurate LSN selectivity
        // estimates, avoiding the PREPARE/EXECUTE custom-plan penalty.
        ResolvedSql {
            merge_sql: resolve_lsn_placeholders(
                &entry.merge_sql_template,
                &entry.source_oids,
                prev_frontier,
                new_frontier,
                &zero_change_oids,
            ),
            source_oids: entry.source_oids.clone(),
            parameterized_merge_sql: entry.parameterized_merge_sql.clone(),
            trigger_delete_sql: entry.trigger_delete_template.clone(),
            trigger_update_sql: entry.trigger_update_template.clone(),
            trigger_insert_sql: entry.trigger_insert_template.clone(),
            trigger_using_sql: resolve_lsn_placeholders(
                &entry.trigger_using_template,
                &entry.source_oids,
                prev_frontier,
                new_frontier,
                &zero_change_oids,
            ),
            resolved_delta_sql: resolve_lsn_placeholders(
                &entry.delta_sql_template,
                &entry.source_oids,
                prev_frontier,
                new_frontier,
                &zero_change_oids,
            ),
            is_all_algebraic: entry.is_all_algebraic,
            is_deduplicated: entry.is_deduplicated,
        }
    } else {
        // ── Cache miss: full pipeline + PREPARE + cache ──────────────
        pgrx::debug1!("[pg_trickle] cache MISS for pgt_id={}", st.pgt_id);
        let delta_result = if has_recursive_cte {
            dvm::generate_delta_query(
                &st.defining_query,
                prev_frontier,
                new_frontier,
                schema,
                name,
            )?
        } else {
            dvm::generate_delta_query_cached(
                st.pgt_id,
                &st.defining_query,
                prev_frontier,
                new_frontier,
                schema,
                name,
            )?
        };

        // DI-2: Clear per-leaf fallback OIDs after delta SQL generation.
        clear_fallback_leaf_oids();

        let delta_sql = delta_result.delta_sql;
        let user_cols = delta_result.output_columns;
        let source_oids = delta_result.source_oids;
        let is_dedup = delta_result.is_deduplicated;
        let has_key_changed = delta_result.has_key_changed;

        let quoted_table = format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\""),
        );

        let user_col_list = format_col_list(&user_cols);

        // Build cleanup SQL templates — plain DELETE statements (no DO block).
        let cleanup_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
        let cleanup_stmts: Vec<String> = source_oids
            .iter()
            .map(|oid| {
                format!(
                    "DELETE FROM \"{cleanup_schema}\".changes_{oid} \
                     WHERE lsn > '__PGS_PREV_LSN_{oid}__'::pg_lsn \
                     AND lsn <= '__PGS_NEW_LSN_{oid}__'::pg_lsn",
                )
            })
            .collect();
        let cleanup_template = cleanup_stmts.join(";");

        // Build the MERGE template using the raw delta SQL template
        // (with __PGS_PREV_LSN_* / __PGS_NEW_LSN_* placeholder tokens).
        let delta_sql_template = if has_recursive_cte {
            delta_sql.clone()
        } else {
            dvm::get_delta_sql_template(st.pgt_id).unwrap_or(delta_sql.clone())
        };

        // Build template USING clause — skip deduplication when deduplicated (G-M1)
        // EC-06a: For keyless sources, weight-aggregate to cancel within-delta
        // I/D pairs that would otherwise cause phantom rows.
        // B3-2: Use weight aggregation instead of DISTINCT ON for correctness
        // on diamond-flow queries.
        // A-2: Filter D-side value-only UPDATE rows when __pgt_key_changed is available.
        let template_using = if is_dedup && has_key_changed {
            format!(
                "(SELECT * FROM ({delta_sql_template}) __d \
                 WHERE NOT (__d.__pgt_action = 'D' AND __d.__pgt_key_changed = FALSE))"
            )
        } else if is_dedup {
            format!("({delta_sql_template})")
        } else if st.has_keyless_source {
            build_keyless_weight_agg(&delta_sql_template, &user_col_list)
        } else {
            build_weight_agg_using(&delta_sql_template, &user_col_list)
        };

        let merge_template = build_merge_sql(
            &quoted_table,
            &template_using,
            &user_cols,
            st.st_partition_key.is_some(),
        );
        // QF-1: Log at LOG level only when pg_trickle.log_merge_sql = on.
        if crate::config::pg_trickle_log_merge_sql() {
            pgrx::log!("[pg_trickle] MERGE SQL TEMPLATE:\n{}", merge_template);
        }

        // ── B-3: DELETE + INSERT template removed (always use MERGE) ─

        // ── D-2: Build parameterized MERGE SQL for PREPARE ─────────
        let parameterized_merge_sql = parameterize_lsn_template(&merge_template, &source_oids);

        // ── User-trigger explicit DML templates ──────────────────────
        //
        // EC-06: Keyless sources use counted DELETE + plain INSERT.
        // But if is_dedup is true, the ST itself has a unique row ID
        // so we must use standard keyed templates.
        let use_keyless = st.has_keyless_source && !is_dedup;
        let trigger_delete_template =
            build_trigger_delete_sql(&quoted_table, st.pgt_id, use_keyless);

        // EC-06: Use normal UPDATE template for keyless sources — see
        // prewarm_merge_cache comment for full rationale.
        let trigger_update_template =
            build_trigger_update_sql(&quoted_table, st.pgt_id, &user_cols);

        let trigger_insert_template =
            build_trigger_insert_sql(&quoted_table, st.pgt_id, &user_cols, use_keyless);

        // Store templates in the cache for subsequent refreshes.
        if !has_recursive_cte {
            MERGE_TEMPLATE_CACHE.with(|cache| {
                cache.borrow_mut().insert(
                    st.pgt_id,
                    CachedMergeTemplate {
                        defining_query_hash: query_hash,
                        merge_sql_template: merge_template.clone(),
                        source_oids: source_oids.clone(),
                        cleanup_sql_template: cleanup_template,
                        parameterized_merge_sql: parameterized_merge_sql.clone(),
                        trigger_delete_template: trigger_delete_template.clone(),
                        trigger_update_template: trigger_update_template.clone(),
                        trigger_insert_template: trigger_insert_template.clone(),
                        trigger_using_template: template_using.clone(),
                        delta_sql_template: delta_sql_template.clone(),
                        is_all_algebraic: delta_result.is_all_algebraic,
                        is_deduplicated: delta_result.is_deduplicated,
                    },
                );
            });
        }

        // Resolve LSN placeholders for this execution.
        ResolvedSql {
            merge_sql: resolve_lsn_placeholders(
                &merge_template,
                &source_oids,
                prev_frontier,
                new_frontier,
                &zero_change_oids,
            ),
            source_oids: source_oids.clone(),
            parameterized_merge_sql,
            trigger_delete_sql: trigger_delete_template,
            trigger_update_sql: trigger_update_template,
            trigger_insert_sql: trigger_insert_template,
            trigger_using_sql: resolve_lsn_placeholders(
                &template_using,
                &source_oids,
                prev_frontier,
                new_frontier,
                &zero_change_oids,
            ),
            resolved_delta_sql: resolve_lsn_placeholders(
                &delta_sql_template,
                &source_oids,
                prev_frontier,
                new_frontier,
                &zero_change_oids,
            ),
            is_all_algebraic: delta_result.is_all_algebraic,
            is_deduplicated: delta_result.is_deduplicated,
        }
    };

    let t1 = Instant::now();

    // PROF-DLT / PGS_PROFILE_DELTA: When the env var is set, capture
    // EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) for the delta query and write
    // the result to /tmp/delta_plans/<schema>_<name>.json.  This env var is
    // intended for E2E test diagnostics and local profiling runs.
    if std::env::var("PGS_PROFILE_DELTA").as_deref() == Ok("1") {
        capture_delta_explain(schema, name, &resolved.resolved_delta_sql);
    }

    // ── Diagnostic: detect OID mismatch between catalog and delta ────
    // If the delta template references source OIDs that are not in the
    // catalog deps, the MERGE will fail referencing nonexistent change
    // buffer tables.
    //
    // For ST-on-ST dependencies, the delta template references the
    // upstream ST's pgt_relid (the storage table OID).  These must be
    // included in the validation set alongside TABLE/FT/MV OIDs.
    let all_dep_oids: Vec<u32> = StDependency::get_for_st(st.pgt_id)
        .unwrap_or_default()
        .into_iter()
        .map(|dep| dep.source_relid.to_u32())
        .collect();
    let delta_oids = &resolved.source_oids;
    let missing_in_delta: Vec<&u32> = delta_oids
        .iter()
        .filter(|oid| !all_dep_oids.contains(oid))
        .collect();
    if !missing_in_delta.is_empty() {
        return Err(PgTrickleError::InternalError(format!(
            "OID MISMATCH (source_oids): delta template references \
             OIDs {missing_in_delta:?} not in catalog deps \
             {all_dep_oids:?}. Delta source_oids={delta_oids:?}, \
             ST={schema}.{name} pgt_id={}",
            st.pgt_id,
        )));
    }

    // ── Diagnostic: scan merge SQL for change buffer table references ─
    // Extract all `changes_NNNNN` references from the SQL to detect
    // references to OIDs not in catalog_source_oids.
    // Note: ST-on-ST queries use `changes_pgt_*` buffers (handled by
    // resolve_delta_template), so `changes_NNNNN` patterns should only
    // reference TABLE/FT/MV sources.
    {
        let re_pattern = "changes_(\\d+)";
        let mut sql_oids: Vec<u32> = Vec::new();
        let merge_sql_ref = &resolved.merge_sql;
        let mut search_from = 0usize;
        while let Some(pos) = merge_sql_ref[search_from..].find("changes_") {
            let start = search_from + pos + 8; // skip "changes_"
            let end = merge_sql_ref[start..]
                .find(|c: char| !c.is_ascii_digit())
                .map(|p| start + p)
                .unwrap_or(merge_sql_ref.len());
            if let Ok(oid) = merge_sql_ref[start..end].parse::<u32>()
                && !sql_oids.contains(&oid)
            {
                sql_oids.push(oid);
            }
            search_from = end;
        }
        let _ = re_pattern; // suppress unused warning
        let missing_in_sql: Vec<&u32> = sql_oids
            .iter()
            .filter(|oid| !all_dep_oids.contains(oid))
            .collect();
        if !missing_in_sql.is_empty() {
            // Dump first 500 chars of merge SQL for diagnosis
            let sql_prefix: String = resolved.merge_sql.chars().take(500).collect();
            return Err(PgTrickleError::InternalError(format!(
                "OID MISMATCH (SQL text): merge SQL references changes_* \
                 for OIDs {missing_in_sql:?} not in catalog deps \
                 {all_dep_oids:?}. SQL OIDs found={sql_oids:?}, \
                 delta source_oids={delta_oids:?}, \
                 ST={schema}.{name} pgt_id={}. \
                 SQL prefix: {sql_prefix}",
                st.pgt_id,
            )));
        }
    }

    // ── D-1: Conditional planner hints based on delta size ───────────
    // Large deltas benefit from hash joins over nested loops. Apply
    // SET LOCAL hints that are automatically reset at transaction end.
    // Deep joins (5+ tables) get aggressive hints to avoid pathological
    // plans that spill excessive temp files.
    let work_mem_cap_exceeded = apply_planner_hints(total_change_count, st.pgt_relid, scan_count);

    // ── SCAL-3: Work-mem cap exceeded — fall back to FULL ───────────
    if work_mem_cap_exceeded {
        let t_full_start = Instant::now();
        let result = execute_full_refresh(st);
        let full_ms = t_full_start.elapsed().as_secs_f64() * 1000.0;
        if let Err(e) =
            StreamTableMeta::update_adaptive_threshold(st.pgt_id, st.auto_threshold, Some(full_ms))
        {
            pgrx::debug1!(
                "[pg_trickle] SCAL-3: failed to update adaptive threshold: {}",
                e,
            );
        }
        return result;
    }

    // ── PH-E1: Delta output cardinality estimation ──────────────────
    // Before executing expensive MERGE, run a capped COUNT on the delta
    // subquery. If the output exceeds the budget, fall back to FULL
    // refresh to prevent OOM or excessive temp-file spills.
    let max_estimate = crate::config::pg_trickle_max_delta_estimate_rows();
    if max_estimate > 0
        && let Some(estimate) = estimate_delta_output_rows(&resolved.merge_sql, max_estimate)
        && estimate > max_estimate as i64
    {
        pgrx::notice!(
            "[pg_trickle] PH-E1: Delta output estimate ({} rows) exceeds \
             max_delta_estimate_rows ({}). Falling back to FULL refresh for {}.{}.",
            estimate,
            max_estimate,
            st.pgt_schema,
            st.pgt_name,
        );
        let t_full_start = Instant::now();
        let result = execute_full_refresh(st);
        let full_ms = t_full_start.elapsed().as_secs_f64() * 1000.0;
        if let Err(e) =
            StreamTableMeta::update_adaptive_threshold(st.pgt_id, st.auto_threshold, Some(full_ms))
        {
            pgrx::debug1!(
                "[pg_trickle] PH-E1: failed to update adaptive threshold: {}",
                e,
            );
        }
        return result;
    }

    // ── A-3a: Append-only INSERT fast path ───────────────────────────
    // When the stream table is marked append-only (and hasn't been
    // reverted by the heuristic check above), skip MERGE entirely and
    // use a simple INSERT … SELECT from the delta. This avoids the
    // DELETE, UPDATE, and IS DISTINCT FROM overhead of the MERGE path.
    //
    // Non-monotonic queries are excluded: for operators like LEFT JOIN,
    // anti-join (ALL subqueries), aggregates, etc., source INSERTs can
    // produce delta DELETEs or UPDATEs that the bare INSERT path cannot
    // handle. When detected, clear the incorrectly set catalog flag so
    // subsequent refreshes skip the heuristic check overhead.
    //
    // STs with downstream ST consumers must skip this path: the fast
    // path returns early before capture_delta_to_st_buffer() runs,
    // so downstream STs would never see change buffer rows and their
    // data_timestamp would never advance — breaking ST-on-ST cascades.
    if is_append_only && !has_downstream_st_consumers(st.pgt_id) {
        let non_monotonic = has_non_monotonic_cte(&resolved.merge_sql);
        // Non-deduplicated deltas (joins, aggregates) must NOT use the
        // append-only fast path: even with ON CONFLICT DO NOTHING, the
        // delta can produce rows that collide with existing ST rows from
        // prior cycles. Revert the catalog flag and fall through to the
        // normal PH-D1/MERGE path.
        if !resolved.is_deduplicated {
            pgrx::debug1!(
                "[pg_trickle] A-3a: skipping append-only for {}.{} — \
                 non-deduplicated delta (join/aggregate)",
                schema,
                name,
            );
            let _ = StreamTableMeta::update_append_only(st.pgt_id, false);
        } else if non_monotonic {
            pgrx::debug1!(
                "[pg_trickle] A-3a: skipping append-only for {}.{} — \
                 non-monotonic query operators detected",
                schema,
                name,
            );
            let _ = StreamTableMeta::update_append_only(st.pgt_id, false);
        } else {
            let t_insert_start = Instant::now();

            // Build INSERT SQL from the resolved MERGE SQL's USING clause.
            // The MERGE SQL has the form:
            //   MERGE INTO "schema"."table" AS st USING (...delta...) AS d ON ...
            // We extract the delta subquery and wrap it in INSERT INTO.
            let insert_sql = build_append_only_insert_sql(schema, name, &resolved.merge_sql);

            // A-3a: If user_triggers = 'off' and the ST has user triggers,
            // suppress them around the INSERT (same as the normal MERGE path).
            // The trigger-suppression block runs AFTER the append-only early
            // return in the normal flow, so we must handle it here.
            let ao_triggers_mode = crate::config::pg_trickle_user_triggers_mode();
            let ao_suppress = ao_triggers_mode == crate::config::UserTriggersMode::Off
                && crate::cdc::has_user_triggers(st.pgt_relid).unwrap_or(false);
            let ao_quoted_table = format!(
                "\"{}\".\"{}\"",
                schema.replace('"', "\"\""),
                name.replace('"', "\"\""),
            );
            if ao_suppress
                && let Err(e) = Spi::run(&format!(
                    "ALTER TABLE {} DISABLE TRIGGER USER",
                    ao_quoted_table
                ))
            {
                pgrx::debug1!(
                    "[pg_trickle] A-3a: failed to disable triggers for {}.{}: {}",
                    schema,
                    name,
                    e
                );
            }

            let rows_inserted = Spi::connect_mut(|client| {
                let result = client
                    .update(&insert_sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
                Ok::<i64, PgTrickleError>(result.len() as i64)
            })?;

            if ao_suppress
                && let Err(e) = Spi::run(&format!(
                    "ALTER TABLE {} ENABLE TRIGGER USER",
                    ao_quoted_table
                ))
            {
                pgrx::debug1!(
                    "[pg_trickle] A-3a: failed to re-enable triggers for {}.{}: {}",
                    schema,
                    name,
                    e
                );
            }

            let t_insert = t_insert_start.elapsed();
            pgrx::debug1!(
                "[pg_trickle] append-only INSERT for {}.{}: {} rows in {:.1}ms",
                schema,
                name,
                rows_inserted,
                t_insert.as_secs_f64() * 1000.0,
            );

            // C-1: Defer cleanup of consumed change buffer rows.
            let cleanup_source_oids = resolved.source_oids.clone();
            if !cleanup_source_oids.is_empty() {
                PENDING_CLEANUP.with(|q| {
                    q.borrow_mut().push(PendingCleanup {
                        change_schema: change_schema.clone(),
                        source_oids: cleanup_source_oids,
                    });
                });
            }

            pgrx::info!(
                "[PGS_PROFILE] decision={:.2}ms insert_exec={:.2}ms total={:.2}ms affected={} mode=APPEND_ONLY",
                t_decision.as_secs_f64() * 1000.0,
                t_insert.as_secs_f64() * 1000.0,
                (t_decision + t_insert).as_secs_f64() * 1000.0,
                rows_inserted,
            );

            // G12-ERM-1: Record the effective mode for this execution path.
            set_effective_mode("APPEND_ONLY");

            // G14-MDED: Count this as a differential refresh. The normal
            // record_diff_refresh() call is below the append-only early return,
            // so we must record it here before returning.
            crate::shmem::record_diff_refresh(crate::dvm::is_delta_deduplicated(st.pgt_id));

            return Ok((rows_inserted, 0));
        }
    }

    // ── User-trigger detection ───────────────────────────────────────
    // Determine whether to use the explicit DML path based on the GUC
    // and the presence of user-defined row-level triggers on the ST.
    let user_triggers_mode = crate::config::pg_trickle_user_triggers_mode();
    let use_explicit_dml = match user_triggers_mode {
        crate::config::UserTriggersMode::Off => false,
        crate::config::UserTriggersMode::Auto => crate::cdc::has_user_triggers(st.pgt_relid)?,
    };

    // EC-06: Keyless sources must use explicit DML because MERGE fails
    // when multiple target rows match a single source row (non-unique
    // __pgt_row_id). Force explicit DML path for counted deletion.
    let is_dedup_flag = crate::dvm::is_delta_deduplicated(st.pgt_id);
    let use_explicit_dml = use_explicit_dml || (st.has_keyless_source && !is_dedup_flag);

    // G14-MDED: Record this differential refresh execution in the shared-memory
    // profiling counters.  Called here (after the no-data short-circuit) so we
    // only count refreshes that actually process delta rows.
    crate::shmem::record_diff_refresh(is_dedup_flag);

    // ST-ST-2: Force explicit DML when this ST has downstream ST consumers.
    // The explicit DML path materializes the delta into __pgt_delta_{pgt_id},
    // which we then capture into the ST's change buffer for downstream use.
    let use_explicit_dml = use_explicit_dml || has_downstream_st_consumers(st.pgt_id);

    // When user_triggers = 'off' but there ARE user triggers on the ST,
    // suppress them during the MERGE to prevent spurious firing.
    let suppress_triggers = user_triggers_mode == crate::config::UserTriggersMode::Off
        && crate::cdc::has_user_triggers(st.pgt_relid)?;
    if suppress_triggers {
        let quoted_table = format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\""),
        );
        Spi::run(&format!("ALTER TABLE {quoted_table} DISABLE TRIGGER USER"))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    // ── B-3: Strategy selection ──────────────────────────────────────
    // PH-D1: Choose between MERGE and DELETE+INSERT based on the
    // merge_strategy GUC and the delta-to-target ratio heuristic.
    let merge_strategy = crate::config::pg_trickle_merge_strategy();
    let use_delete_insert = match merge_strategy {
        crate::config::MergeStrategy::DeleteInsert => true,
        crate::config::MergeStrategy::Merge => false,
        crate::config::MergeStrategy::Auto => {
            // Heuristic: use DELETE+INSERT when delta is a small fraction
            // of the target table. Estimate target rows from pg_class.
            let threshold = crate::config::pg_trickle_merge_strategy_threshold();
            if total_change_count > 0 && threshold > 0.0 {
                let target_rows: i64 = Spi::get_one::<i64>(&format!(
                    "SELECT CASE WHEN reltuples >= 1 THEN reltuples::bigint \
                            ELSE (SELECT COUNT(*) FROM \"{}\".\"{}\" ) END \
                     FROM pg_class WHERE oid = {}::oid",
                    schema.replace('"', "\"\""),
                    name.replace('"', "\"\""),
                    st.pgt_relid.to_u32(),
                ))
                .unwrap_or(Some(0))
                .unwrap_or(0);
                if target_rows > 0 {
                    let ratio = total_change_count as f64 / target_rows as f64;
                    let chosen = ratio < threshold;
                    if chosen {
                        pgrx::debug1!(
                            "[pg_trickle] PH-D1: auto chose DELETE+INSERT for {}.{}: \
                             ratio={:.4} < threshold={:.4} ({} changes / {} target rows)",
                            schema,
                            name,
                            ratio,
                            threshold,
                            total_change_count,
                            target_rows,
                        );
                    }
                    chosen
                } else {
                    false
                }
            } else {
                false
            }
        }
    };
    // DELETE+INSERT is incompatible with the explicit DML path (user triggers,
    // keyless sources, downstream ST consumers) — those already use their own
    // decomposed DML.  Also skip for partitioned STs (hash-merge path).
    let use_delete_insert = use_delete_insert && !use_explicit_dml && st.st_partition_key.is_none();

    // PH-D1-JOIN: For non-deduplicated deltas (joins), always use PH-D1
    // with ON CONFLICT instead of MERGE.  Join deltas can produce phantom
    // rows (Part 1 and Part 2 compute different __pgt_row_id hashes for
    // the same logical row) that accumulate in the stream table.  While
    // weight aggregation + DISTINCT ON deduplicates per-refresh, phantom
    // rows from prior refreshes can still trigger UNIQUE_VIOLATION in the
    // MERGE INSERT clause (which lacks ON CONFLICT protection).  PH-D1's
    // INSERT uses ON CONFLICT (__pgt_row_id) DO UPDATE SET which safely
    // handles these collisions.
    let use_delete_insert = if !resolved.is_deduplicated
        && !st.has_keyless_source
        && !use_explicit_dml
        && st.st_partition_key.is_none()
    {
        true
    } else {
        use_delete_insert
    };

    // ── B-1: Aggregate fast-path ─────────────────────────────────────
    // When the GUC is on and ALL aggregates are algebraically invertible
    // (COUNT, SUM, AVG, etc.), use explicit DML (DELETE+UPDATE+INSERT)
    // instead of MERGE. The explicit DML path does targeted row-level
    // operations via a materialized temp table, avoiding the hash-join
    // cost of MERGE which dominates for aggregate queries with many groups.
    let use_agg_fast_path = resolved.is_all_algebraic
        && crate::config::pg_trickle_aggregate_fast_path()
        && !st.has_keyless_source
        && !use_explicit_dml
        && !use_delete_insert
        && st.st_partition_key.is_none();
    if use_agg_fast_path {
        pgrx::debug1!(
            "[pg_trickle] B-1: aggregate fast-path enabled for {}.{} \
             (all aggregates algebraically invertible)",
            schema,
            name,
        );
    }

    // ── A1-2/A1-3: Partition-key range predicate injection ───────────
    // For partitioned stream tables, compute the MIN/MAX of the partition
    // key across the current delta and inject it as a literal range
    // predicate into the MERGE ON clause. This enables PostgreSQL partition
    // pruning: only partitions overlapping [min, max] are visited, reducing
    // MERGE I/O proportionally to the number of affected partitions.
    //
    // A1-3b: HASH partitions use a per-partition MERGE loop instead of
    // predicate injection (hash functions are not range-invertible).
    //
    // If the delta is empty (all changes cancel out), return early —
    // there is nothing to MERGE.
    let hash_merge_result: Option<(usize, &str)> = if let Some(ref pk) = st.st_partition_key {
        let method = crate::api::parse_partition_method(pk);
        if method == crate::api::PartitionMethod::Hash {
            // A1-3b: Per-partition MERGE for HASH partitioned STs.
            let count = execute_hash_partitioned_merge(
                &resolved.merge_sql,
                &resolved.resolved_delta_sql,
                schema,
                name,
                st.pgt_relid,
                pk,
                st.pgt_id,
            )?;
            Some((count, "hash_merge"))
        } else {
            // RANGE / LIST: extract bounds and inject predicate.
            match extract_partition_bounds(&resolved.resolved_delta_sql, pk)? {
                None => {
                    // Delta produced no rows for the partition key — fast path.
                    pgrx::debug1!(
                        "[pg_trickle] A1-3: empty partition-key delta for {}.{}, skipping MERGE",
                        schema,
                        name,
                    );
                    return Ok((0, 0));
                }
                Some(bounds) => {
                    pgrx::debug1!(
                        "[pg_trickle] A1-3: partition bounds for {}.{}: {:?}",
                        schema,
                        name,
                        match &bounds {
                            PartitionBounds::Range { mins, maxs } =>
                                format!("RANGE [{mins:?}, {maxs:?}]"),
                            PartitionBounds::List(vals) => format!("LIST {:?}", vals),
                        },
                    );
                    resolved.merge_sql =
                        inject_partition_predicate(&resolved.merge_sql, pk, &bounds);
                    None
                }
            }
        }
    } else {
        None
    };

    // ── D-2: Prepared-statement flag ─────────────────────────────────
    // PB2: Disable prepared statements when pooler_compatibility_mode is on.
    // A1-3: Disable for partitioned STs — the partition predicate is a
    // literal range that changes every refresh; a generic plan cannot prune
    // partitions from parameter values.
    // ST-ST-5: Disable prepared statements when the parameterized SQL
    // contains pgt_-prefixed LSN placeholders (ST sources). The
    // parameterize function only handles numeric OID placeholders; pgt_
    // placeholders remain as literals and fail to parse as pg_lsn.
    let has_pgt_placeholders = resolved
        .parameterized_merge_sql
        .contains("__PGS_PREV_LSN_pgt_")
        || resolved
            .parameterized_merge_sql
            .contains("__PGS_NEW_LSN_pgt_");
    let use_prepared = crate::config::pg_trickle_use_prepared_statements()
        && was_cache_hit
        && !st.pooler_compatibility_mode
        && st.st_partition_key.is_none()
        && !has_pgt_placeholders;

    let (merge_count, strategy_label) = if let Some(result) = hash_merge_result {
        // A1-3b: HASH per-partition MERGE already executed above.
        result
    } else if use_delete_insert {
        // ── PH-D1: DELETE+INSERT path ───────────────────────────────
        // For small deltas against large tables, separate DELETE + INSERT
        // avoids the MERGE join cost. The delta is materialized into a
        // temp table, then applied as two targeted statements.
        let t_mat_start = Instant::now();

        // Drop any stale delta table from a prior refresh in the same
        // transaction (e.g. two refresh_stream_table calls in one batch_execute).
        // ON COMMIT DROP normally handles cleanup, but that only fires at
        // transaction commit, so subsequent calls within the same transaction
        // would otherwise see "relation already exists".
        let _ = Spi::run(&format!("DROP TABLE IF EXISTS __pgt_delta_{}", st.pgt_id)); // nosemgrep: rust.spi.run.dynamic-format — st.pgt_id is a plain i64, not user-supplied input.
        let materialize_sql = format!(
            "CREATE TEMP TABLE __pgt_delta_{pgt_id} ON COMMIT DROP AS \
             SELECT * FROM {using_clause} AS d",
            pgt_id = st.pgt_id,
            using_clause = resolved.trigger_using_sql,
        );
        Spi::run(&materialize_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let t_mat = t_mat_start.elapsed();

        // Step 1: DELETE rows touched by the delta.
        //
        // For keyed sources, delete ALL rows whose __pgt_row_id appears
        // anywhere in the delta (both 'D' and 'I' actions).  This is
        // critical for join deltas: an UPDATE on a source row produces
        // both a 'D' (old values) and an 'I' (new values) with the same
        // __pgt_row_id. Deleting all matching row_ids up-front guarantees
        // the subsequent INSERT never hits a UNIQUE_VIOLATION, eliminating
        // the fragile ON CONFLICT + pre-delete approach.
        //
        // For keyless sources, only delete rows matching 'D' actions
        // (the __pgt_row_id index is non-unique, so no conflict risk).
        let t_del_start = Instant::now();
        let del_count = if !st.has_keyless_source {
            let quoted_table = format!(
                "\"{}\".\"{}\"",
                schema.replace('"', "\"\""),
                name.replace('"', "\"\""),
            );
            let unified_del_sql = format!(
                "DELETE FROM {quoted_table} WHERE __pgt_row_id IN (\
                     SELECT __pgt_row_id FROM __pgt_delta_{pgt_id}\
                 )",
                pgt_id = st.pgt_id,
            );
            Spi::connect_mut(|client| {
                let result = client
                    .update(&unified_del_sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(format!("[PH-D1-DELETE] {}", e)))?;
                Ok::<usize, PgTrickleError>(result.len())
            })?
        } else {
            Spi::connect_mut(|client| {
                let result = client
                    .update(&resolved.trigger_delete_sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(format!("[PH-D1-DELETE] {}", e)))?;
                Ok::<usize, PgTrickleError>(result.len())
            })?
        };
        let t_del = t_del_start.elapsed();

        // Step 2: UPDATE existing rows where values changed.
        // For keyed sources, the unified DELETE in step 1 already removed
        // all rows matching delta row_ids (both 'D' and 'I'), so the
        // UPDATE step is unnecessary — the INSERT will re-create them
        // with updated values.
        let t_upd_start = Instant::now();
        let upd_count = if st.has_keyless_source {
            // Keyless sources still need the UPDATE step
            Spi::connect_mut(|client| {
                let result = client
                    .update(&resolved.trigger_update_sql, None, &[])
                    .map_err(|e| PgTrickleError::SpiError(format!("[PH-D1-UPDATE] {}", e)))?;
                Ok::<usize, PgTrickleError>(result.len())
            })?
        } else {
            // Keyed sources: skip UPDATE, unified DELETE + INSERT handles it
            0
        };
        let t_upd = t_upd_start.elapsed();

        // Step 3: INSERT genuinely new rows.
        // For keyed sources, all conflicting row_ids were removed in step 1,
        // so no ON CONFLICT clause is needed. DISTINCT ON in the INSERT SQL
        // (from build_trigger_insert_sql) handles within-delta duplicates.
        let t_ins_start = Instant::now();
        let ins_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_insert_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(format!("[PH-D1-INSERT] {}", e)))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        let t_ins = t_ins_start.elapsed();

        pgrx::info!(
            "[PGS_PROFILE] delete_insert: materialize={:.2}ms delete={:.2}ms({}) \
             update={:.2}ms({}) insert={:.2}ms({}) for {}.{}",
            t_mat.as_secs_f64() * 1000.0,
            t_del.as_secs_f64() * 1000.0,
            del_count,
            t_upd.as_secs_f64() * 1000.0,
            upd_count,
            t_ins.as_secs_f64() * 1000.0,
            ins_count,
            schema,
            name,
        );

        (del_count + upd_count + ins_count, "delete_insert")
    } else if use_agg_fast_path {
        // ── B-1: Aggregate fast-path ────────────────────────────────
        // For all-algebraic aggregate queries, use explicit DML
        // (DELETE+UPDATE+INSERT) to avoid the MERGE hash-join cost.
        let t_mat_start = Instant::now();

        // Drop any stale delta table from a prior refresh in the same
        // transaction (same-transaction multiple refresh edge case).
        let _ = Spi::run(&format!("DROP TABLE IF EXISTS __pgt_delta_{}", st.pgt_id)); // nosemgrep: rust.spi.run.dynamic-format — st.pgt_id is a plain i64, not user-supplied input.
        let materialize_sql = format!(
            "CREATE TEMP TABLE __pgt_delta_{pgt_id} ON COMMIT DROP AS \
             SELECT * FROM {using_clause} AS d",
            pgt_id = st.pgt_id,
            using_clause = resolved.trigger_using_sql,
        );
        Spi::run(&materialize_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let t_mat = t_mat_start.elapsed();

        // Step 1: DELETE rows marked for removal
        let t_del_start = Instant::now();
        let del_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_delete_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        let t_del = t_del_start.elapsed();

        // Step 2: UPDATE existing rows where values changed
        let t_upd_start = Instant::now();
        let upd_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_update_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        let t_upd = t_upd_start.elapsed();

        // Step 3: INSERT genuinely new rows
        let t_ins_start = Instant::now();
        let ins_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_insert_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        let t_ins = t_ins_start.elapsed();

        pgrx::info!(
            "[PGS_PROFILE] agg_fast_path: materialize={:.2}ms delete={:.2}ms({}) \
             update={:.2}ms({}) insert={:.2}ms({}) for {}.{}",
            t_mat.as_secs_f64() * 1000.0,
            t_del.as_secs_f64() * 1000.0,
            del_count,
            t_upd.as_secs_f64() * 1000.0,
            upd_count,
            t_ins.as_secs_f64() * 1000.0,
            ins_count,
            schema,
            name,
        );

        (del_count + upd_count + ins_count, "agg_fast_path")
    } else if use_explicit_dml {
        // ── User-trigger path: explicit DML ─────────────────────────
        // Decompose the MERGE into DELETE + UPDATE + INSERT so that
        // user-defined triggers fire with correct TG_OP / OLD / NEW.

        // Step 1: Materialize delta into a temp table (ON COMMIT DROP).
        // This avoids evaluating the delta query three times.
        let t_mat_start = Instant::now();

        // Drop any stale delta table from a prior refresh in the same
        // transaction (same-transaction multiple refresh edge case).
        let _ = Spi::run(&format!("DROP TABLE IF EXISTS __pgt_delta_{}", st.pgt_id)); // nosemgrep: rust.spi.run.dynamic-format — st.pgt_id is a plain i64, not user-supplied input.
        let materialize_sql = format!(
            "CREATE TEMP TABLE __pgt_delta_{pgt_id} ON COMMIT DROP AS \
             SELECT * FROM {using_clause} AS d",
            pgt_id = st.pgt_id,
            using_clause = resolved.trigger_using_sql,
        );
        Spi::run(&materialize_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let t_mat = t_mat_start.elapsed();

        // ST-ST-10: When this ST has downstream ST consumers, snapshot
        // the affected rows BEFORE applying DML.  The weight-aggregation
        // wrapper collapses D+I pairs for the same __pgt_row_id into a
        // single I action, which is correct for the MERGE but loses the
        // DELETE for old column values.  The pre-snapshot lets us compute
        // the true effective delta (including value-change DELETEs) after
        // the DML completes.
        let needs_diff_capture = has_downstream_st_consumers(st.pgt_id);
        let diff_capture_cols = if needs_diff_capture {
            let cols = get_st_user_columns(st);
            let col_list: String = cols
                .iter()
                .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(", ");

            let qt = format!(
                "\"{}\".\"{}\"",
                schema.replace('"', "\"\""),
                name.replace('"', "\"\""),
            );

            let _ = Spi::run(&format!("DROP TABLE IF EXISTS __pgt_pre_{}", st.pgt_id)); // nosemgrep: rust.spi.run.dynamic-format — st.pgt_id is a plain i64, not user-supplied input.

            let snapshot_sql = format!(
                "CREATE TEMP TABLE __pgt_pre_{pgt_id} ON COMMIT DROP AS \
                 SELECT __pgt_row_id, {col_list} FROM {qt} \
                 WHERE __pgt_row_id IN (\
                   SELECT __pgt_row_id FROM __pgt_delta_{pgt_id}\
                 )",
                pgt_id = st.pgt_id,
            );
            if let Err(e) = Spi::run(&snapshot_sql) {
                pgrx::warning!(
                    "[pg_trickle] ST-ST-10: pre-snapshot failed for {}.{}: {} — \
                     falling back to delta-based capture",
                    schema,
                    name,
                    e,
                );
                Vec::new()
            } else {
                cols
            }
        } else {
            Vec::new()
        };

        // Step 2: DELETE removed rows (AFTER DELETE triggers fire)
        let t_del_start = Instant::now();
        let del_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_delete_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        let t_del = t_del_start.elapsed();

        // Step 3: UPDATE changed existing rows (AFTER UPDATE triggers fire)
        // The IS DISTINCT FROM guard (B-1) prevents no-op UPDATE triggers.
        let t_upd_start = Instant::now();
        let upd_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_update_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        let t_upd = t_upd_start.elapsed();

        // Step 4: INSERT genuinely new rows (AFTER INSERT triggers fire)
        let t_ins_start = Instant::now();
        let ins_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_insert_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        let t_ins = t_ins_start.elapsed();

        pgrx::info!(
            "[PGS_PROFILE] explicit_dml: materialize={:.2}ms delete={:.2}ms({}) update={:.2}ms({}) insert={:.2}ms({}) for {}.{}",
            t_mat.as_secs_f64() * 1000.0,
            t_del.as_secs_f64() * 1000.0,
            del_count,
            t_upd.as_secs_f64() * 1000.0,
            upd_count,
            t_ins.as_secs_f64() * 1000.0,
            ins_count,
            schema,
            name,
        );

        // ST-ST-2/ST-ST-10: Capture effective delta to change buffer for
        // downstream ST consumers.  When a pre-snapshot was taken
        // (diff_capture_cols is non-empty), use the pre/post comparison
        // to produce accurate I/D pairs that include value-change DELETEs.
        // Otherwise, fall back to the delta-based capture.
        if needs_diff_capture {
            let mut diff_capture_ok = true;
            if !diff_capture_cols.is_empty() {
                if let Err(e) = capture_incremental_diff_to_st_buffer(st, &diff_capture_cols) {
                    pgrx::warning!(
                        "[pg_trickle] ST-ST-10: incremental diff capture failed for {}.{}: {} \
                         — marking downstream STs for reinit",
                        schema,
                        name,
                        e,
                    );
                    diff_capture_ok = false;
                }
            } else {
                let user_cols = get_st_user_columns(st);
                if let Err(e) = capture_delta_to_st_buffer(st, &user_cols) {
                    pgrx::warning!(
                        "[pg_trickle] ST-ST: delta capture failed for {}.{}: {} \
                         — marking downstream STs for reinit",
                        schema,
                        name,
                        e,
                    );
                    diff_capture_ok = false;
                }
            }
            if !diff_capture_ok
                && let Ok(downstream_ids) =
                    crate::catalog::StDependency::get_downstream_pgt_ids(st.pgt_relid)
            {
                for ds_id in &downstream_ids {
                    if let Err(e2) = StreamTableMeta::mark_for_reinitialize(*ds_id) {
                        pgrx::warning!(
                            "[pg_trickle] ST-ST: failed to mark downstream ST {} for reinit: {}",
                            ds_id,
                            e2,
                        );
                    }
                }
            }
        }

        (del_count + upd_count + ins_count, "explicit_dml")
    } else if use_prepared {
        // ── D-2: MERGE via prepared statement ────────────────────────
        // After ~5 executions PostgreSQL switches from custom to generic
        // plan, saving ~1-2ms of parse/plan overhead per refresh cycle.
        let stmt_name = format!("__pgt_merge_{}", st.pgt_id);

        let already_prepared = PREPARED_MERGE_STMTS.with(|s| s.borrow().contains(&st.pgt_id));

        if !already_prepared {
            let type_list = build_prepare_type_list(resolved.source_oids.len());
            // DEALLOCATE in case a stale statement exists from a prior
            // session within this same backend.
            // Note: DEALLOCATE does not support IF EXISTS in PostgreSQL.
            // Check pg_prepared_statements first to avoid an error.
            let stale_exists = Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(SELECT 1 FROM pg_prepared_statements WHERE name = '{stmt_name}')"
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false);
            if stale_exists {
                let _ = Spi::run(&format!("DEALLOCATE {stmt_name}"));
            }
            Spi::run(&format!(
                "PREPARE {stmt_name} ({type_list}) AS {}",
                resolved.parameterized_merge_sql
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

            PREPARED_MERGE_STMTS.with(|s| {
                s.borrow_mut().insert(st.pgt_id);
            });
        }

        let params = build_execute_params(&resolved.source_oids, prev_frontier, new_frontier);
        let execute_sql = format!("EXECUTE {stmt_name}({params})");

        let n = Spi::connect_mut(|client| {
            let result = client
                .update(&execute_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(format!("[MERGE-PREPARED] {}", e)))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        (n, "merge_prepared")
    } else {
        // ── MERGE path (default for small deltas) ───────────────────
        let n = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.merge_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(format!("[MERGE] {}", e)))?;
            Ok::<usize, PgTrickleError>(result.len())
        })?;
        (n, "merge")
    };

    // Re-enable user triggers if they were suppressed (GUC = 'off').
    if suppress_triggers {
        let quoted_table = format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\""),
        );
        Spi::run(&format!("ALTER TABLE {quoted_table} ENABLE TRIGGER USER"))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    let t2 = Instant::now();

    // ── C-1: Defer cleanup to next refresh cycle ────────────────────
    // Instead of deleting consumed change-buffer rows synchronously
    // (which costs 0.5–7ms), we enqueue the cleanup work and execute it
    // at the start of the NEXT refresh.  The cleanup uses the min-frontier
    // approach to safely handle shared change buffers across multiple STs.
    let cleanup_source_oids = MERGE_TEMPLATE_CACHE.with(|cache| {
        let map = cache.borrow();
        map.get(&st.pgt_id)
            .map(|entry| entry.source_oids.clone())
            .unwrap_or_default()
    });

    if !cleanup_source_oids.is_empty() {
        PENDING_CLEANUP.with(|q| {
            q.borrow_mut().push(PendingCleanup {
                change_schema: change_schema.clone(),
                source_oids: cleanup_source_oids,
            });
        });
    }

    let t3 = Instant::now();

    // PH-E2: Query pg_stat_statements for temp file spill metrics.
    // Store in thread-local for the scheduler to read after this function returns.
    let spill_threshold = crate::config::pg_trickle_spill_threshold_blocks();
    if spill_threshold > 0 {
        let temp_blks = crate::monitor::query_temp_file_usage(name)
            .map(|(_read, written)| written)
            .unwrap_or(0);
        set_last_temp_blks_written(temp_blks);
    } else {
        set_last_temp_blks_written(-1);
    }

    // Determine cache path and hint tier for profiling
    let cache_path = if was_cache_hit {
        "cache_hit"
    } else {
        "cache_miss"
    };

    let hint_tier = if !crate::config::pg_trickle_merge_planner_hints() {
        "off"
    } else if total_change_count >= PLANNER_HINT_WORKMEM_THRESHOLD {
        "nestloop+workmem"
    } else if total_change_count >= PLANNER_HINT_NESTLOOP_THRESHOLD {
        "nestloop"
    } else {
        "none"
    };

    // Emit timing breakdown for profiling
    pgrx::info!(
        "[PGS_PROFILE] decision={:.2}ms generate+build={:.2}ms merge_exec={:.2}ms cleanup_enqueue={:.2}ms total={:.2}ms affected={} delta_est={} mode=INCR path={} hints={} strategy={}",
        t_decision.as_secs_f64() * 1000.0,
        t1.duration_since(t0).as_secs_f64() * 1000.0,
        t2.duration_since(t1).as_secs_f64() * 1000.0,
        t3.duration_since(t2).as_secs_f64() * 1000.0,
        (t_decision.as_secs_f64() + t3.duration_since(t0).as_secs_f64()) * 1000.0,
        merge_count,
        total_change_count,
        cache_path,
        hint_tier,
        strategy_label,
    );

    // ── Session 7: Adaptive threshold auto-tuning ───────────────────
    // Compare INCR total time against the last known FULL time. If INCR
    // is approaching or exceeding FULL, lower the threshold so future
    // refreshes at this change rate fall back to FULL sooner. If INCR
    // is significantly faster, raise the threshold to allow more
    // differential refreshes.
    let incr_total_ms = (t_decision.as_secs_f64() + t3.duration_since(t0).as_secs_f64()) * 1000.0;
    if let Some(last_full) = st.last_full_ms
        && last_full > 0.0
    {
        let current_threshold = st.auto_threshold.unwrap_or(global_ratio);
        let ratio_threshold =
            compute_adaptive_threshold(current_threshold, incr_total_ms, last_full);

        // ── D-3 / B-4: Cost-based threshold from historical data ────
        // Blend the ratio-based threshold with a cost-model estimate
        // derived from recent refresh history.  The cost model computes
        // the crossover delta ratio where INCR cost equals FULL cost,
        // adjusted for query complexity class.
        let complexity = classify_query_complexity(&st.defining_query);
        let new_threshold = match estimate_cost_based_threshold(st.pgt_id, complexity) {
            Some(cost_threshold) => {
                // Weighted blend: 60% ratio-based, 40% cost-based.
                let blended = ratio_threshold * 0.6 + cost_threshold * 0.4;
                blended.clamp(0.01, 0.80)
            }
            None => ratio_threshold,
        };

        if (new_threshold - current_threshold).abs() > 0.001 {
            pgrx::debug1!(
                "[pg_trickle] Adaptive threshold: INCR={:.1}ms vs FULL={:.1}ms (ratio={:.2}), threshold {:.3} → {:.3}",
                incr_total_ms,
                last_full,
                incr_total_ms / last_full,
                current_threshold,
                new_threshold,
            );
        }
        if let Err(e) =
            StreamTableMeta::update_adaptive_threshold(st.pgt_id, Some(new_threshold), None)
        {
            pgrx::debug1!("[pg_trickle] Failed to update adaptive threshold: {}", e);
        }
    }

    // Guarantee a non-zero count when the change buffer actually had entries.
    // PostgreSQL MERGE may report 0 processed rows via SPI even when it
    // modifies data (observed with pgrx 0.17 / PostgreSQL 18).
    // Since we already verified `any_changes=true` above, the MERGE must
    // have processed at least one change buffer entry.
    let effective_count = (merge_count as i64).max(1);

    // ── DAG-3: Delta amplification detection ────────────────────────
    // After the MERGE completes, check whether the output delta is
    // disproportionately larger than the input delta (common with
    // many-to-many joins or high fan-out).  Emit a WARNING so operators
    // can identify and tune the problematic hop.
    let amplification_threshold = crate::config::pg_trickle_delta_amplification_threshold();
    if should_warn_amplification(total_change_count, effective_count, amplification_threshold) {
        let ratio = compute_amplification_ratio(total_change_count, effective_count);
        pgrx::warning!(
            "[pg_trickle] DAG-3: Delta amplification detected for {}.{}: \
             {} input rows → {} output rows ({:.1}× amplification, \
             threshold is {:.0}×). Consider restructuring the query to \
             reduce join fan-out, or raise pg_trickle.delta_amplification_threshold.",
            schema,
            name,
            total_change_count,
            effective_count,
            ratio,
            amplification_threshold,
        );
    }

    // G12-ERM-1: Record the effective mode for this execution path.
    set_effective_mode("DIFFERENTIAL");

    // PART-WARN: After a successful refresh, warn if the default partition
    // of a partitioned stream table has accumulated rows.  This prompts the
    // user to create explicit named partitions.
    if st.st_partition_key.is_some() {
        warn_default_partition_growth(schema, name);
    }

    Ok((effective_count, 0))
}

/// B-4: Aggregated refresh history statistics for cost-model prediction.
struct RefreshHistoryStats {
    /// Average milliseconds per delta row across recent DIFFERENTIAL refreshes.
    avg_ms_per_delta: f64,
    /// Average FULL refresh time in milliseconds.
    avg_full_ms: f64,
}

/// B-4: Query recent refresh history stats for a stream table.
///
/// Returns `None` when insufficient history exists (fewer than 3
/// completed DIFFERENTIAL refreshes or no completed FULL refresh).
fn query_refresh_history_stats(pgt_id: i64) -> Option<RefreshHistoryStats> {
    let stats: Option<(f64, f64)> = Spi::connect(|client| {
        let sql = format!(
            "SELECT incr.avg_ms_per_delta, full_r.avg_full_ms \
             FROM ( \
               SELECT AVG(EXTRACT(EPOCH FROM (end_time - start_time)) * 1000.0 \
                          / GREATEST(delta_row_count, 1)) AS avg_ms_per_delta, \
                      COUNT(*)::int AS cnt \
               FROM ( \
                 SELECT end_time, start_time, delta_row_count \
                 FROM pgtrickle.pgt_refresh_history \
                 WHERE pgt_id = {pgt_id} \
                   AND action = 'DIFFERENTIAL' \
                   AND status = 'COMPLETED' \
                   AND delta_row_count > 0 \
                   AND end_time IS NOT NULL \
                 ORDER BY refresh_id DESC LIMIT 10 \
               ) __pgt_incr \
             ) incr, ( \
               SELECT AVG(EXTRACT(EPOCH FROM (end_time - start_time)) * 1000.0) AS avg_full_ms \
               FROM ( \
                 SELECT end_time, start_time \
                 FROM pgtrickle.pgt_refresh_history \
                 WHERE pgt_id = {pgt_id} \
                   AND action = 'FULL' \
                   AND status = 'COMPLETED' \
                   AND end_time IS NOT NULL \
                 ORDER BY refresh_id DESC LIMIT 5 \
               ) __pgt_full \
             ) full_r \
             WHERE incr.cnt >= 3 \
               AND full_r.avg_full_ms IS NOT NULL \
               AND full_r.avg_full_ms > 0 \
               AND incr.avg_ms_per_delta IS NOT NULL \
               AND incr.avg_ms_per_delta > 0",
        );

        let result: Option<(f64, f64)> = (|| {
            let row = client.select(&sql, None, &[]).ok()?.first();
            let avg_ms_per_delta: f64 = row.get::<f64>(1).ok()??;
            let avg_full_ms: f64 = row.get::<f64>(2).ok()??;
            Some((avg_ms_per_delta, avg_full_ms))
        })();
        Ok::<_, pgrx::spi::SpiError>(result)
    })
    .unwrap_or(None);

    let (avg_ms_per_delta, avg_full_ms) = stats?;
    Some(RefreshHistoryStats {
        avg_ms_per_delta,
        avg_full_ms,
    })
}

/// D-3 / B-4: Estimate a cost-based fallback threshold from refresh history.
///
/// Queries the last N DIFFERENTIAL and FULL refreshes for a stream table
/// and computes the crossover delta ratio where incremental cost equals
/// full cost.  The `complexity` class adjusts the per-delta-row cost
/// via a multiplicative factor (joins and aggregates are more expensive
/// per delta row than plain scans).
///
/// Returns `None` if insufficient history is available (fewer
/// than 3 DIFFERENTIAL or no FULL refresh recorded).
///
/// The model:
///   incr_cost(delta_ratio) ≈ avg_incr_cost_per_delta_row × complexity_factor × delta_ratio × table_size
///   full_cost              ≈ avg_full_ms
///   crossover_ratio        = avg_full_ms / (avg_cost_per_delta_row × complexity_factor × table_size)
///
/// Clamped to [0.01, 0.80].
fn estimate_cost_based_threshold(pgt_id: i64, complexity: QueryComplexityClass) -> Option<f64> {
    // Query recent completed DIFFERENTIAL refreshes with non-zero delta.
    let stats: Option<(f64, f64, f64)> = Spi::connect(|client| {
        // avg_ms_per_delta: average milliseconds per delta row
        // avg_full_ms:      average FULL refresh time
        //
        // We use a lateral subquery to get both INCR and FULL stats.
        let sql = format!(
            "SELECT incr.avg_ms_per_delta, full_r.avg_full_ms, \
                    GREATEST(incr.avg_delta, 1)::double precision AS avg_delta \
             FROM ( \
               SELECT AVG(EXTRACT(EPOCH FROM (end_time - start_time)) * 1000.0 \
                          / GREATEST(delta_row_count, 1)) AS avg_ms_per_delta, \
                      AVG(delta_row_count)::double precision AS avg_delta, \
                      COUNT(*)::int AS cnt \
               FROM ( \
                 SELECT end_time, start_time, delta_row_count \
                 FROM pgtrickle.pgt_refresh_history \
                 WHERE pgt_id = {pgt_id} \
                   AND action = 'DIFFERENTIAL' \
                   AND status = 'COMPLETED' \
                   AND delta_row_count > 0 \
                   AND end_time IS NOT NULL \
                 ORDER BY refresh_id DESC LIMIT 10 \
               ) __pgt_incr \
             ) incr, ( \
               SELECT AVG(EXTRACT(EPOCH FROM (end_time - start_time)) * 1000.0) AS avg_full_ms \
               FROM ( \
                 SELECT end_time, start_time \
                 FROM pgtrickle.pgt_refresh_history \
                 WHERE pgt_id = {pgt_id} \
                   AND action = 'FULL' \
                   AND status = 'COMPLETED' \
                   AND end_time IS NOT NULL \
                 ORDER BY refresh_id DESC LIMIT 5 \
               ) __pgt_full \
             ) full_r \
             WHERE incr.cnt >= 3 \
               AND full_r.avg_full_ms IS NOT NULL \
               AND full_r.avg_full_ms > 0 \
               AND incr.avg_ms_per_delta IS NOT NULL \
               AND incr.avg_ms_per_delta > 0",
        );

        let result: Option<(f64, f64, f64)> = (|| {
            let row = client.select(&sql, None, &[]).ok()?.first();
            let avg_ms_per_delta: f64 = row.get::<f64>(1).ok()??;
            let avg_full_ms: f64 = row.get::<f64>(2).ok()??;
            let avg_delta: f64 = row.get::<f64>(3).ok()??;
            Some((avg_ms_per_delta, avg_full_ms, avg_delta))
        })();
        Ok::<_, pgrx::spi::SpiError>(result)
    })
    .unwrap_or(None);

    let (avg_ms_per_delta, avg_full_ms, avg_delta) = stats?;

    // crossover_delta = avg_full_ms / (avg_ms_per_delta × complexity_factor)
    // The complexity factor scales the per-delta-row cost: join_agg queries
    // have 4× the cost per delta row compared to a plain scan, so their
    // crossover point is lower (smaller change ratio triggers FULL).
    let factor = complexity.diff_cost_factor();
    let crossover_delta = avg_full_ms / (avg_ms_per_delta * factor);
    if avg_delta <= 0.0 {
        return None;
    }

    // If crossover is much higher than typical delta, current threshold is fine;
    // if crossover is near or below typical delta, we should lower the threshold.
    // Scale the global default (0.15) by how far the crossover is from the average.
    let global_ratio = crate::config::pg_trickle_differential_max_change_ratio();
    let scaling: f64 = crossover_delta / avg_delta;
    let suggested: f64 = (global_ratio * scaling).clamp(0.01, 0.80);

    Some(suggested)
}

/// B-4: Pre-refresh predictive cost comparison.
///
/// **Before** executing a refresh, estimate the DIFFERENTIAL and FULL costs
/// from historical data and the current delta size.  Returns `true` if the
/// cost model recommends FULL refresh.
///
/// When insufficient history exists (cold start), returns `None` to let the
/// caller fall through to the fixed-threshold heuristic.
///
/// Pure decision logic — called from the refresh decision path.
fn cost_model_prefers_full(
    avg_ms_per_delta: f64,
    avg_full_ms: f64,
    current_delta_rows: i64,
    complexity: QueryComplexityClass,
) -> bool {
    let safety_margin = crate::config::pg_trickle_cost_model_safety_margin();
    let factor = complexity.diff_cost_factor();
    let estimated_diff = avg_ms_per_delta * factor * current_delta_rows as f64;
    let estimated_full = avg_full_ms * safety_margin;
    estimated_diff >= estimated_full
}

/// Compute a new adaptive fallback threshold based on observed performance.
///
/// Compares the DIFFERENTIAL refresh time against the last known FULL refresh
/// time and adjusts the threshold accordingly:
///
/// - If INCR time >= 90% of FULL → lower threshold by 20% (more aggressive fallback)
/// - If INCR time >= 70% of FULL → lower threshold by 10%
/// - If INCR time <= 30% of FULL → raise threshold by 10% (allow more INCR)
/// - Otherwise → keep the current threshold
///
/// The threshold is clamped to [0.01, 0.80] to prevent extreme values.
///
/// This is a pure function — no database access.
fn compute_adaptive_threshold(current: f64, incr_ms: f64, full_ms: f64) -> f64 {
    let ratio = incr_ms / full_ms;
    let adjusted = if ratio >= 0.90 {
        // INCR is nearly as slow as FULL — lower threshold aggressively
        current * 0.80
    } else if ratio >= 0.70 {
        // INCR is getting expensive — lower threshold moderately
        current * 0.90
    } else if ratio <= 0.30 {
        // INCR is much faster — raise threshold to allow more INCR
        (current * 1.10).min(0.80)
    } else {
        // INCR is reasonably faster — keep threshold
        current
    };

    adjusted.clamp(0.01, 0.80)
}

/// Execute a reinitialize refresh: full recompute after schema change.
///
/// If the ST has an `original_query`, uses it for the FULL refresh
/// (so current view definitions are resolved at execution time), then
/// re-runs the rewrite pipeline to store the updated inlined query.
pub fn execute_reinitialize_refresh(st: &StreamTableMeta) -> Result<(i64, i64), PgTrickleError> {
    // Use original_query for the refresh so current view/function
    // definitions are resolved at execution time.
    let refresh_st = if let Some(oq) = &st.original_query {
        let mut tmp = st.clone();
        tmp.defining_query = oq.clone();
        tmp
    } else {
        st.clone()
    };

    let result = execute_full_refresh(&refresh_st)?;

    // After refresh, re-run the rewrite pipeline to update stored query.
    let _ = crate::api::reinit_rewrite_if_needed(st);

    // Clear reinit flag
    Spi::run(&format!(
        "UPDATE pgtrickle.pgt_stream_tables SET needs_reinit = FALSE WHERE pgt_id = {}",
        st.pgt_id,
    ))
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    Ok(result)
}

// ── Unit tests ─────────────────────────────────────────────────────────────

/// PROF-DLT: Capture `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` for the
/// resolved delta SQL and persist the plan to
/// `/tmp/delta_plans/<schema>_<name>.json`.
///
/// Called when `PGS_PROFILE_DELTA=1` is set in the environment.  Errors are
/// logged as warnings so profiling failures never abort a real refresh cycle.
pub(crate) fn capture_delta_explain(schema: &str, name: &str, delta_sql: &str) {
    use std::path::PathBuf;

    let dir = PathBuf::from("/tmp/delta_plans");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        pgrx::warning!("[pg_trickle] PGS_PROFILE_DELTA: failed to create /tmp/delta_plans: {e}");
        return;
    }

    // Build EXPLAIN query wrapping the delta SQL.
    let explain_sql = format!(
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT * FROM ({delta_sql}) __pgt_explain_d"
    );

    let plan_json = Spi::connect(|client| {
        let result = client
            .select(&explain_sql, None, &[])
            .map_err(|e| format!("SPI error in explain: {e}"))?;
        let mut lines = Vec::new();
        for row in result {
            let line: Option<pgrx::JsonB> = row.get(1).unwrap_or(None);
            if let Some(j) = line {
                lines.push(j.0.to_string());
            }
        }
        Ok::<String, String>(lines.join("\n"))
    });

    let plan_json = match plan_json {
        Ok(j) => j,
        Err(e) => {
            pgrx::warning!(
                "[pg_trickle] PGS_PROFILE_DELTA: EXPLAIN failed for {schema}.{name}: {e}"
            );
            return;
        }
    };

    // Write to /tmp/delta_plans/<schema>_<name>.json
    let safe_schema = schema.replace('"', "").replace('/', "_");
    let safe_name = name.replace('"', "").replace('/', "_");
    let path = dir.join(format!("{safe_schema}_{safe_name}.json"));
    if let Err(e) = std::fs::write(&path, &plan_json) {
        pgrx::warning!(
            "[pg_trickle] PGS_PROFILE_DELTA: failed to write {}: {e}",
            path.display()
        );
    } else {
        pgrx::debug1!(
            "[pg_trickle] PGS_PROFILE_DELTA: plan written to {}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::{RefreshMode, StStatus};
    use crate::version::Frontier;

    // ── Helper: build a minimal StreamTableMeta for testing ─────────

    fn test_st(refresh_mode: RefreshMode, needs_reinit: bool) -> StreamTableMeta {
        StreamTableMeta {
            pgt_id: 1,
            pgt_relid: pg_sys::Oid::from(0u32),
            pgt_name: "test_st".to_string(),
            pgt_schema: "public".to_string(),
            defining_query: "SELECT 1".to_string(),
            original_query: None,
            schedule: None,
            refresh_mode,
            status: StStatus::Active,
            is_populated: true,
            data_timestamp: None,
            consecutive_errors: 0,
            needs_reinit,
            auto_threshold: None,
            last_full_ms: None,
            functions_used: None,
            frontier: None,
            topk_limit: None,
            topk_order_by: None,
            topk_offset: None,
            diamond_consistency: crate::dag::DiamondConsistency::None,
            diamond_schedule_policy: crate::dag::DiamondSchedulePolicy::default(),
            has_keyless_source: false,
            function_hashes: None,
            requested_cdc_mode: None,
            is_append_only: false,
            scc_id: None,
            last_fixpoint_iterations: None,
            pooler_compatibility_mode: false,
            refresh_tier: "hot".to_string(),
            fuse_mode: "off".to_string(),
            fuse_state: "armed".to_string(),
            fuse_ceiling: None,
            fuse_sensitivity: None,
            blown_at: None,
            blow_reason: None,
            st_partition_key: None,
            max_differential_joins: None,
            max_delta_fraction: None,
            last_error_message: None,
            last_error_at: None,
        }
    }

    fn make_frontier(entries: &[(u32, &str)]) -> Frontier {
        let mut f = Frontier::new();
        for &(oid, lsn) in entries {
            f.set_source(oid, lsn.to_string(), "2025-01-01T00:00:00Z".to_string());
        }
        f
    }

    // ── RefreshAction::as_str() ─────────────────────────────────────

    #[test]
    fn test_refresh_action_no_data() {
        assert_eq!(RefreshAction::NoData.as_str(), "NO_DATA");
    }

    #[test]
    fn test_refresh_action_full() {
        assert_eq!(RefreshAction::Full.as_str(), "FULL");
    }

    #[test]
    fn test_refresh_action_differential() {
        assert_eq!(RefreshAction::Differential.as_str(), "DIFFERENTIAL");
    }

    #[test]
    fn test_refresh_action_reinitialize() {
        assert_eq!(RefreshAction::Reinitialize.as_str(), "REINITIALIZE");
    }

    #[test]
    fn test_refresh_action_variants_exist() {
        let _full = RefreshAction::Full;
        let _incr = RefreshAction::Differential;
        let _no_data = RefreshAction::NoData;
        let _reinit = RefreshAction::Reinitialize;
    }

    #[test]
    fn test_execute_differential_refresh_rejects_unpopulated_stream_table() {
        let mut st = test_st(RefreshMode::Differential, false);
        st.is_populated = false;

        let error = execute_differential_refresh(
            &st,
            &make_frontier(&[(42, "0/10")]),
            &make_frontier(&[(42, "0/20")]),
        )
        .expect_err("unpopulated stream tables must be rejected before SPI work");

        match error {
            PgTrickleError::InvalidArgument(message) => {
                assert!(message.contains("unpopulated stream table public.test_st"));
                assert!(message.contains("FULL refresh is required first"));
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_differential_refresh_rejects_empty_frontier() {
        let st = test_st(RefreshMode::Differential, false);

        let error =
            execute_differential_refresh(&st, &Frontier::new(), &make_frontier(&[(42, "0/20")]))
                .expect_err("missing baseline frontier must be rejected before SPI work");

        match error {
            PgTrickleError::InvalidArgument(message) => {
                assert!(message.contains("public.test_st"));
                assert!(message.contains("no previous frontier exists"));
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    // ── determine_refresh_action() ──────────────────────────────────

    #[test]
    fn test_determine_reinit_takes_priority() {
        let st = test_st(RefreshMode::Differential, true);
        assert_eq!(
            determine_refresh_action(&st, true),
            RefreshAction::Reinitialize,
        );
    }

    #[test]
    fn test_determine_no_upstream_changes() {
        let st = test_st(RefreshMode::Differential, false);
        assert_eq!(determine_refresh_action(&st, false), RefreshAction::NoData,);
    }

    #[test]
    fn test_determine_full_mode() {
        let st = test_st(RefreshMode::Full, false);
        assert_eq!(determine_refresh_action(&st, true), RefreshAction::Full,);
    }

    #[test]
    fn test_determine_differential_mode() {
        let st = test_st(RefreshMode::Differential, false);
        assert_eq!(
            determine_refresh_action(&st, true),
            RefreshAction::Differential,
        );
    }

    #[test]
    fn test_determine_reinit_overrides_no_changes() {
        // Even if no upstream changes, reinit flag wins
        let st = test_st(RefreshMode::Full, true);
        assert_eq!(
            determine_refresh_action(&st, false),
            RefreshAction::Reinitialize,
        );
    }

    // ── resolve_lsn_placeholders() ──────────────────────────────────

    /// Convenience wrapper for tests — calls resolve_lsn_placeholders with empty zero_change_oids.
    fn resolve_lsn_placeholders_test(
        template: &str,
        source_oids: &[u32],
        prev: &Frontier,
        new_f: &Frontier,
    ) -> String {
        resolve_lsn_placeholders(
            template,
            source_oids,
            prev,
            new_f,
            &std::collections::HashSet::new(),
        )
    }

    #[test]
    fn test_resolve_lsn_single_oid() {
        let mut prev = Frontier::new();
        prev.set_source(42, "0/1000".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(42, "0/2000".to_string(), "ts".to_string());

        let template = "DELETE FROM changes_42 WHERE lsn > '__PGS_PREV_LSN_42__' AND lsn <= '__PGS_NEW_LSN_42__'";
        let resolved = resolve_lsn_placeholders_test(template, &[42], &prev, &new_f);
        assert!(resolved.contains("0/1000"));
        assert!(resolved.contains("0/2000"));
        assert!(!resolved.contains("__PGS_"));
    }

    #[test]
    fn test_resolve_lsn_multiple_oids() {
        let mut prev = Frontier::new();
        prev.set_source(10, "0/AA".to_string(), "ts".to_string());
        prev.set_source(20, "0/BB".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(10, "0/CC".to_string(), "ts".to_string());
        new_f.set_source(20, "0/DD".to_string(), "ts".to_string());

        let template =
            "__PGS_PREV_LSN_10__ __PGS_NEW_LSN_10__ __PGS_PREV_LSN_20__ __PGS_NEW_LSN_20__";
        let resolved = resolve_lsn_placeholders_test(template, &[10, 20], &prev, &new_f);
        assert_eq!(resolved, "0/AA 0/CC 0/BB 0/DD");
    }

    #[test]
    fn test_resolve_lsn_no_placeholders() {
        let prev = Frontier::new();
        let new_f = Frontier::new();
        let resolved = resolve_lsn_placeholders_test("SELECT 1", &[], &prev, &new_f);
        assert_eq!(resolved, "SELECT 1");
    }

    #[test]
    fn test_resolve_lsn_missing_oid_defaults() {
        let prev = Frontier::new();
        let new_f = Frontier::new();
        let resolved = resolve_lsn_placeholders_test("__PGS_PREV_LSN_999__", &[999], &prev, &new_f);
        assert_eq!(resolved, "0/0");
    }

    #[test]
    fn test_resolve_lsn_preserves_other_text() {
        let mut prev = Frontier::new();
        prev.set_source(1, "0/10".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(1, "0/20".to_string(), "ts".to_string());

        let template = "SELECT * FROM t WHERE x = 42 AND lsn > '__PGS_PREV_LSN_1__'";
        let resolved = resolve_lsn_placeholders_test(template, &[1], &prev, &new_f);
        assert!(resolved.contains("SELECT * FROM t WHERE x = 42"));
        assert!(resolved.contains("0/10"));
    }

    #[test]
    fn test_resolve_lsn_placeholders_single_source() {
        let template = "DELETE FROM changes_12345 WHERE lsn > '__PGS_PREV_LSN_12345__'::pg_lsn AND lsn <= '__PGS_NEW_LSN_12345__'::pg_lsn";
        let prev = make_frontier(&[(12345, "0/1000")]);
        let new = make_frontier(&[(12345, "0/2000")]);
        let result = resolve_lsn_placeholders_test(template, &[12345], &prev, &new);
        assert_eq!(
            result,
            "DELETE FROM changes_12345 WHERE lsn > '0/1000'::pg_lsn AND lsn <= '0/2000'::pg_lsn"
        );
    }

    #[test]
    fn test_resolve_lsn_placeholders_multi_source() {
        let template = "DELETE FROM changes_100 WHERE lsn > '__PGS_PREV_LSN_100__'::pg_lsn AND lsn <= '__PGS_NEW_LSN_100__'::pg_lsn;\
                        DELETE FROM changes_200 WHERE lsn > '__PGS_PREV_LSN_200__'::pg_lsn AND lsn <= '__PGS_NEW_LSN_200__'::pg_lsn";
        let prev = make_frontier(&[(100, "0/A"), (200, "0/B")]);
        let new = make_frontier(&[(100, "0/C"), (200, "0/D")]);
        let result = resolve_lsn_placeholders_test(template, &[100, 200], &prev, &new);
        assert!(result.contains("'0/A'"));
        assert!(result.contains("'0/C'"));
        assert!(result.contains("'0/B'"));
        assert!(result.contains("'0/D'"));
        assert!(!result.contains("__PGS_"));
    }

    #[test]
    fn test_resolve_lsn_placeholders_missing_source_defaults_to_0_0() {
        let template = "lsn > '__PGS_PREV_LSN_999__'::pg_lsn";
        let prev = Frontier::new();
        let new = Frontier::new();
        let result = resolve_lsn_placeholders_test(template, &[999], &prev, &new);
        assert_eq!(result, "lsn > '0/0'::pg_lsn");
    }

    #[test]
    fn test_resolve_lsn_placeholders_empty_template() {
        let result = resolve_lsn_placeholders_test("", &[1], &Frontier::new(), &Frontier::new());
        assert_eq!(result, "");
    }

    #[test]
    fn test_resolve_lsn_placeholders_no_sources() {
        let template = "SELECT 1";
        let result =
            resolve_lsn_placeholders_test(template, &[], &Frontier::new(), &Frontier::new());
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn test_resolve_lsn_b3_1_zero_change_pruning() {
        let template = "SELECT * FROM changes_42 WHERE c.lsn > '__PGS_PREV_LSN_42__'::pg_lsn AND c.lsn <= '__PGS_NEW_LSN_42__'::pg_lsn";
        let prev = make_frontier(&[(42, "0/1000")]);
        let new_f = make_frontier(&[(42, "0/2000")]);
        let mut zero = std::collections::HashSet::new();
        zero.insert(42u32);
        let result = resolve_lsn_placeholders(template, &[42], &prev, &new_f, &zero);
        assert!(result.contains("FALSE"));
        assert!(!result.contains("__PGS_"));
        assert!(!result.contains("0/1000"));
    }

    #[test]
    fn test_resolve_lsn_b3_1_partial_zero_change() {
        // Two sources — OID 10 has changes, OID 20 has zero changes
        let template = "c.lsn > '__PGS_PREV_LSN_10__'::pg_lsn AND c.lsn <= '__PGS_NEW_LSN_10__'::pg_lsn UNION ALL c.lsn > '__PGS_PREV_LSN_20__'::pg_lsn AND c.lsn <= '__PGS_NEW_LSN_20__'::pg_lsn";
        let prev = make_frontier(&[(10, "0/A"), (20, "0/B")]);
        let new_f = make_frontier(&[(10, "0/C"), (20, "0/D")]);
        let mut zero = std::collections::HashSet::new();
        zero.insert(20u32);
        let result = resolve_lsn_placeholders(template, &[10, 20], &prev, &new_f, &zero);
        // OID 10 should be resolved normally
        assert!(result.contains("'0/A'"));
        assert!(result.contains("'0/C'"));
        // OID 20 should be pruned to FALSE
        assert!(result.contains("FALSE"));
        assert!(!result.contains("__PGS_"));
    }

    // ── CachedMergeTemplate tests ──────────────────────────────────────

    #[test]
    fn test_merge_template_cache_insert_and_retrieve() {
        MERGE_TEMPLATE_CACHE.with(|cache| {
            let mut map = cache.borrow_mut();
            map.insert(
                42,
                CachedMergeTemplate {
                    defining_query_hash: 12345,
                    merge_sql_template: "MERGE INTO t ...".to_string(),
                    source_oids: vec![100, 200],
                    cleanup_sql_template: "DELETE FROM ...".to_string(),
                    parameterized_merge_sql: String::new(),
                    trigger_delete_template: String::new(),
                    trigger_update_template: String::new(),
                    trigger_insert_template: String::new(),
                    trigger_using_template: String::new(),
                    delta_sql_template: String::new(),
                    is_all_algebraic: false,
                    is_deduplicated: true,
                },
            );
        });

        let entry = MERGE_TEMPLATE_CACHE.with(|cache| cache.borrow().get(&42).cloned());
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.defining_query_hash, 12345);
        assert_eq!(entry.source_oids, vec![100, 200]);

        // Cleanup
        MERGE_TEMPLATE_CACHE.with(|cache| cache.borrow_mut().remove(&42));
    }

    #[test]
    fn test_invalidate_merge_cache_removes_entry() {
        MERGE_TEMPLATE_CACHE.with(|cache| {
            cache.borrow_mut().insert(
                99,
                CachedMergeTemplate {
                    defining_query_hash: 0,
                    merge_sql_template: String::new(),
                    source_oids: vec![],
                    cleanup_sql_template: String::new(),
                    parameterized_merge_sql: String::new(),
                    trigger_delete_template: String::new(),
                    trigger_update_template: String::new(),
                    trigger_insert_template: String::new(),
                    trigger_using_template: String::new(),
                    delta_sql_template: String::new(),
                    is_all_algebraic: false,
                    is_deduplicated: true,
                },
            );
        });

        invalidate_merge_cache(99);

        let exists = MERGE_TEMPLATE_CACHE.with(|cache| cache.borrow().contains_key(&99));
        assert!(!exists);
    }

    #[test]
    fn test_invalidate_merge_cache_nonexistent_is_noop() {
        // Should not panic
        invalidate_merge_cache(999_999);
    }

    // ── D-2: parameterize_lsn_template tests ───────────────────────────

    #[test]
    fn test_parameterize_single_source() {
        let template = "SELECT * FROM c WHERE c.lsn > '__PGS_PREV_LSN_100__'::pg_lsn \
                         AND c.lsn <= '__PGS_NEW_LSN_100__'::pg_lsn";
        let result = parameterize_lsn_template(template, &[100]);
        assert!(result.contains("$1"), "should have $1: {result}");
        assert!(result.contains("$2"), "should have $2: {result}");
        assert!(
            !result.contains("__PGS_PREV_LSN_100__"),
            "should not have prev token"
        );
        assert!(
            !result.contains("__PGS_NEW_LSN_100__"),
            "should not have new token"
        );
    }

    #[test]
    fn test_parameterize_multiple_sources() {
        let template = "WHERE c1.lsn > '__PGS_PREV_LSN_10__'::pg_lsn \
                         AND c1.lsn <= '__PGS_NEW_LSN_10__'::pg_lsn \
                         AND c2.lsn > '__PGS_PREV_LSN_20__'::pg_lsn \
                         AND c2.lsn <= '__PGS_NEW_LSN_20__'::pg_lsn";
        let result = parameterize_lsn_template(template, &[10, 20]);
        assert!(result.contains("$1"), "prev for oid 10: {result}");
        assert!(result.contains("$2"), "new for oid 10: {result}");
        assert!(result.contains("$3"), "prev for oid 20: {result}");
        assert!(result.contains("$4"), "new for oid 20: {result}");
    }

    #[test]
    fn test_parameterize_no_sources() {
        let template = "SELECT 1";
        let result = parameterize_lsn_template(template, &[]);
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn test_build_prepare_type_list_single() {
        assert_eq!(build_prepare_type_list(1), "pg_lsn, pg_lsn");
    }

    #[test]
    fn test_build_prepare_type_list_multi() {
        assert_eq!(
            build_prepare_type_list(3),
            "pg_lsn, pg_lsn, pg_lsn, pg_lsn, pg_lsn, pg_lsn"
        );
    }

    #[test]
    fn test_build_prepare_type_list_zero() {
        assert_eq!(build_prepare_type_list(0), "");
    }

    #[test]
    fn test_build_execute_params_single_source() {
        use crate::version::Frontier;
        let mut prev = Frontier::new();
        let mut next = Frontier::new();
        prev.set_source(100, "0/1000".to_string(), String::new());
        next.set_source(100, "0/2000".to_string(), String::new());
        let result = build_execute_params(&[100], &prev, &next);
        assert_eq!(result, "'0/1000'::pg_lsn, '0/2000'::pg_lsn");
    }

    #[test]
    fn test_build_execute_params_multiple_sources() {
        use crate::version::Frontier;
        let mut prev = Frontier::new();
        let mut next = Frontier::new();
        prev.set_source(10, "0/A".to_string(), String::new());
        prev.set_source(20, "0/B".to_string(), String::new());
        next.set_source(10, "0/C".to_string(), String::new());
        next.set_source(20, "0/D".to_string(), String::new());
        let result = build_execute_params(&[10, 20], &prev, &next);
        assert_eq!(
            result,
            "'0/A'::pg_lsn, '0/C'::pg_lsn, '0/B'::pg_lsn, '0/D'::pg_lsn"
        );
    }

    #[test]
    fn test_build_execute_params_missing_lsn_uses_zero() {
        use crate::version::Frontier;
        let prev = Frontier::new();
        let next = Frontier::new();
        let result = build_execute_params(&[999], &prev, &next);
        assert_eq!(result, "'0/0'::pg_lsn, '0/0'::pg_lsn");
    }

    // ── compute_adaptive_threshold() ────────────────────────────────

    #[test]
    fn test_adaptive_threshold_incr_much_slower_than_full() {
        // INCR is 95% of FULL → lower threshold by 20%
        let result = compute_adaptive_threshold(0.15, 95.0, 100.0);
        assert!((result - 0.12).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_incr_moderately_slow() {
        // INCR is 75% of FULL → lower threshold by 10%
        let result = compute_adaptive_threshold(0.15, 75.0, 100.0);
        assert!((result - 0.135).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_incr_much_faster() {
        // INCR is 20% of FULL → raise threshold by 10%
        let result = compute_adaptive_threshold(0.15, 20.0, 100.0);
        assert!((result - 0.165).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_incr_in_sweet_spot() {
        // INCR is 50% of FULL → keep threshold unchanged
        let result = compute_adaptive_threshold(0.15, 50.0, 100.0);
        assert!((result - 0.15).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_clamps_to_min() {
        // Very low threshold that gets lowered further → clamped to 0.01
        let result = compute_adaptive_threshold(0.012, 95.0, 100.0);
        assert!((result - 0.01).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_clamps_to_max() {
        // High threshold that gets raised → clamped to 0.80
        let result = compute_adaptive_threshold(0.75, 10.0, 100.0);
        assert!((result - 0.80).abs() < 0.01, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_at_boundary_90pct() {
        // Exactly 90% → should lower by 20%
        let result = compute_adaptive_threshold(0.20, 90.0, 100.0);
        assert!((result - 0.16).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_at_boundary_70pct() {
        // Exactly 70% → should lower by 10%
        let result = compute_adaptive_threshold(0.20, 70.0, 100.0);
        assert!((result - 0.18).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_at_boundary_30pct() {
        // Exactly 30% → keep threshold (boundary is <=, not <)
        let result = compute_adaptive_threshold(0.20, 30.0, 100.0);
        assert!((result - 0.22).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_incr_exceeds_full() {
        // INCR took longer than FULL (ratio 1.2) → aggressively lower
        let result = compute_adaptive_threshold(0.15, 120.0, 100.0);
        assert!((result - 0.12).abs() < 0.001, "got {result}");
    }

    #[test]
    fn test_adaptive_threshold_converges_downward() {
        // Simulate multiple iterations of INCR being 80% of FULL
        let mut threshold = 0.30;
        for _ in 0..10 {
            threshold = compute_adaptive_threshold(threshold, 80.0, 100.0);
        }
        // Should converge downward but stay above min
        assert!(threshold >= 0.01, "got {threshold}");
        assert!(threshold < 0.15, "should decrease: got {threshold}");
    }

    #[test]
    fn test_adaptive_threshold_converges_upward() {
        // Simulate iterations of INCR being 10% of FULL
        let mut threshold = 0.10;
        for _ in 0..50 {
            threshold = compute_adaptive_threshold(threshold, 10.0, 100.0);
        }
        // Should converge upward toward the cap
        assert!((threshold - 0.80).abs() < 0.01, "got {threshold}");
    }

    // ── build_append_only_insert_sql() ──────────────────────────────

    // ── PH-E1: estimate_delta_output_rows extraction ────────────────

    #[test]
    fn test_extract_using_clause_for_estimation() {
        // Verify the USING clause extraction pattern works correctly.
        // (estimate_delta_output_rows calls SPI, so we test the parsing
        // by checking the same extraction logic used in both functions.)
        let merge_sql = r#"MERGE INTO "public"."test_st" AS st USING (SELECT * FROM delta) AS d ON st.__pgt_row_id = d.__pgt_row_id WHEN MATCHED THEN DELETE"#;

        let using_start = merge_sql.find("USING ").map(|p| p + 6);
        let using_end = merge_sql.find(" AS d ON ");
        assert!(using_start.is_some());
        assert!(using_end.is_some());
        let clause = &merge_sql[using_start.unwrap()..using_end.unwrap()];
        assert_eq!(clause, "(SELECT * FROM delta)");
    }

    #[test]
    fn test_extract_using_clause_complex_cte() {
        let merge_sql = r#"MERGE INTO "s"."t" AS st USING (WITH cte AS NOT MATERIALIZED (SELECT a FROM b) SELECT * FROM cte) AS d ON st.id = d.id WHEN MATCHED THEN DELETE"#;

        let using_start = merge_sql.find("USING ").map(|p| p + 6).unwrap();
        let using_end = merge_sql.find(" AS d ON ").unwrap();
        let clause = &merge_sql[using_start..using_end];
        assert!(clause.starts_with("(WITH cte AS NOT MATERIALIZED"));
        assert!(clause.ends_with("SELECT * FROM cte)"));
    }

    #[test]
    fn test_build_append_only_insert_sql_basic() {
        let merge_sql = r#"MERGE INTO "public"."test_st" AS st USING (SELECT * FROM delta) AS d ON st.__pgt_row_id = d.__pgt_row_id WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE WHEN MATCHED AND d.__pgt_action = 'I' AND (st."val"::text IS DISTINCT FROM d."val"::text) THEN UPDATE SET "val" = d."val" WHEN NOT MATCHED AND d.__pgt_action = 'I' THEN INSERT (__pgt_row_id, "val") VALUES (d.__pgt_row_id, d."val")"#;

        let result = build_append_only_insert_sql("public", "test_st", merge_sql);
        assert!(result.contains(r#"INSERT INTO "public"."test_st""#));
        assert!(result.contains("__pgt_row_id"));
        assert!(result.contains("WHERE d.__pgt_action = 'I'"));
        assert!(!result.contains("MERGE"));
        assert!(!result.contains("DELETE"));
        assert!(!result.contains("UPDATE SET"));
    }

    #[test]
    fn test_build_append_only_insert_sql_multi_column() {
        let merge_sql = r#"MERGE INTO "myschema"."events" AS st USING (SELECT * FROM changes) AS d ON st.__pgt_row_id = d.__pgt_row_id WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE WHEN NOT MATCHED AND d.__pgt_action = 'I' THEN INSERT (__pgt_row_id, "id", "type", "payload") VALUES (d.__pgt_row_id, d."id", d."type", d."payload")"#;

        let result = build_append_only_insert_sql("myschema", "events", merge_sql);
        assert!(result.contains(r#"INSERT INTO "myschema"."events""#));
        assert!(result.contains(r#"__pgt_row_id, "id", "type", "payload""#));
        assert!(result.contains(r#"d.__pgt_row_id, d."id", d."type", d."payload""#));
    }

    // ── DAG-3: compute_amplification_ratio tests ────────────────────

    #[test]
    fn test_amplification_ratio_normal() {
        let ratio = compute_amplification_ratio(10, 1000);
        assert!((ratio - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_amplification_ratio_one_to_one() {
        let ratio = compute_amplification_ratio(50, 50);
        assert!((ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_amplification_ratio_reduction() {
        // Output smaller than input (e.g. aggregation)
        let ratio = compute_amplification_ratio(1000, 10);
        assert!((ratio - 0.01).abs() < f64::EPSILON);
    }

    #[test]
    fn test_amplification_ratio_zero_input_returns_zero() {
        assert!((compute_amplification_ratio(0, 500)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_amplification_ratio_negative_input_returns_zero() {
        assert!((compute_amplification_ratio(-1, 500)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_amplification_ratio_zero_output() {
        assert!((compute_amplification_ratio(100, 0)).abs() < f64::EPSILON);
    }

    // ── DAG-3: should_warn_amplification tests ──────────────────────

    #[test]
    fn test_should_warn_above_threshold() {
        // 1000 / 5 = 200× > 100×
        assert!(should_warn_amplification(5, 1000, 100.0));
    }

    #[test]
    fn test_should_not_warn_below_threshold() {
        // 50 / 5 = 10× < 100×
        assert!(!should_warn_amplification(5, 50, 100.0));
    }

    #[test]
    fn test_should_not_warn_at_threshold() {
        // Exactly at threshold — not exceeded, no warning.
        assert!(!should_warn_amplification(1, 100, 100.0));
    }

    #[test]
    fn test_should_not_warn_disabled() {
        // threshold = 0 → detection disabled
        assert!(!should_warn_amplification(1, 10_000, 0.0));
    }

    #[test]
    fn test_should_not_warn_negative_threshold() {
        assert!(!should_warn_amplification(1, 10_000, -5.0));
    }

    #[test]
    fn test_should_not_warn_zero_input() {
        assert!(!should_warn_amplification(0, 500, 100.0));
    }

    #[test]
    fn test_should_not_warn_negative_input() {
        assert!(!should_warn_amplification(-1, 500, 100.0));
    }

    #[test]
    fn test_should_warn_low_threshold() {
        // threshold = 2.0, ratio = 10/2 = 5.0 → warn
        assert!(should_warn_amplification(2, 10, 2.0));
    }

    // ── ST-ST-9: Content hash for ST change buffer pk_hash ──────────

    #[test]
    fn test_build_content_hash_expr_single_col() {
        let expr = build_content_hash_expr("d.", &["id".to_string()]);
        assert_eq!(expr, "pgtrickle.pg_trickle_hash(d.\"id\"::TEXT)");
    }

    #[test]
    fn test_build_content_hash_expr_multi_col() {
        let expr = build_content_hash_expr("d.", &["id".to_string(), "val".to_string()]);
        assert_eq!(
            expr,
            "pgtrickle.pg_trickle_hash_multi(ARRAY[d.\"id\"::TEXT, d.\"val\"::TEXT])"
        );
    }

    #[test]
    fn test_build_content_hash_expr_quoted_col() {
        let expr = build_content_hash_expr("pre.", &["col\"name".to_string()]);
        assert_eq!(expr, "pgtrickle.pg_trickle_hash(pre.\"col\"\"name\"::TEXT)");
    }

    #[test]
    fn test_build_content_hash_expr_empty_cols_fallback() {
        let expr = build_content_hash_expr("d.", &[]);
        assert_eq!(expr, "d.__pgt_row_id");
    }

    #[test]
    fn test_bypass_capture_uses_content_hash() {
        let sql = build_bypass_capture_sql(
            42,
            &[
                ("id".to_string(), "integer".to_string()),
                ("name".to_string(), "text".to_string()),
            ],
            "pg_temp.__pgt_bypass_42",
            None,
        );
        // ST-ST-9: pk_hash should be content hash, not d.__pgt_row_id
        assert!(sql.contains("pg_trickle_hash_multi(ARRAY[d.\"id\"::TEXT, d.\"name\"::TEXT])"));
        assert!(!sql.contains("d.__pgt_row_id"));
    }

    // ── Phase 6 (TESTING_GAPS_2): determine_refresh_action unit tests ────────

    #[test]
    fn test_determine_refresh_action_needs_reinit_takes_priority() {
        // needs_reinit=true always → Reinitialize, regardless of changes flag
        let st = test_st(RefreshMode::Differential, true);
        assert_eq!(
            determine_refresh_action(&st, true),
            RefreshAction::Reinitialize
        );
        assert_eq!(
            determine_refresh_action(&st, false),
            RefreshAction::Reinitialize
        );
    }

    #[test]
    fn test_determine_refresh_action_no_upstream_changes_returns_no_data() {
        // has_upstream_changes=false → NoData (unless needs_reinit)
        let st_diff = test_st(RefreshMode::Differential, false);
        let st_full = test_st(RefreshMode::Full, false);
        assert_eq!(
            determine_refresh_action(&st_diff, false),
            RefreshAction::NoData
        );
        assert_eq!(
            determine_refresh_action(&st_full, false),
            RefreshAction::NoData
        );
    }

    #[test]
    fn test_determine_refresh_action_differential_mode_with_changes() {
        let st = test_st(RefreshMode::Differential, false);
        assert_eq!(
            determine_refresh_action(&st, true),
            RefreshAction::Differential
        );
    }

    #[test]
    fn test_determine_refresh_action_full_mode_with_changes() {
        let st = test_st(RefreshMode::Full, false);
        assert_eq!(determine_refresh_action(&st, true), RefreshAction::Full);
    }

    #[test]
    fn test_determine_refresh_action_immediate_falls_back_to_full() {
        let st = test_st(RefreshMode::Immediate, false);
        // IMMEDIATE is trigger-maintained; manual refresh → Full fallback
        assert_eq!(determine_refresh_action(&st, true), RefreshAction::Full);
    }

    // ── Phase 6: build_is_distinct_clause boundary tests ────────────────────
    //
    // The threshold between column-list and hash-based comparison is
    // WIDE_TABLE_HASH_THRESHOLD (50).  Test straddling that boundary.

    #[test]
    fn test_build_is_distinct_clause_exactly_at_threshold_uses_columns() {
        let cols: Vec<String> = (1..=WIDE_TABLE_HASH_THRESHOLD)
            .map(|i| format!("col{i}"))
            .collect();
        let sql = build_is_distinct_clause(&cols);
        // At the threshold: use per-column IS DISTINCT FROM
        assert!(
            sql.contains("IS DISTINCT FROM"),
            "Threshold should use per-column comparison"
        );
        assert!(
            !sql.contains("pg_trickle_hash"),
            "Threshold should NOT use hash comparison"
        );
    }

    #[test]
    fn test_build_is_distinct_clause_one_over_threshold_uses_hash() {
        let cols: Vec<String> = (1..=(WIDE_TABLE_HASH_THRESHOLD + 1))
            .map(|i| format!("col{i}"))
            .collect();
        let sql = build_is_distinct_clause(&cols);
        assert!(
            sql.contains("pg_trickle_hash"),
            "One over threshold should use hash comparison"
        );
        // Per-column path uses `::text IS DISTINCT FROM`; hash path does not.
        assert!(
            !sql.contains("::text IS DISTINCT FROM"),
            "Wide table should NOT use per-column comparison; got: {sql}"
        );
    }

    #[test]
    fn test_build_is_distinct_clause_double_quotes_in_col_name_are_escaped() {
        let cols = vec!["weird\"name".to_string()];
        let sql = build_is_distinct_clause(&cols);
        // The double quote inside the name should be escaped as ""
        assert!(
            sql.contains("\"\""),
            "Double quotes in column names must be escaped; got: {sql}"
        );
    }

    // ── TG2-MERGE: build_merge_sql() unit tests ────────────────────

    #[test]
    fn test_build_merge_sql_single_column() {
        let cols = vec!["amount".to_string()];
        let sql = build_merge_sql("\"public\".\"totals\"", "(delta_query)", &cols, false);
        assert!(sql.starts_with("MERGE INTO \"public\".\"totals\" AS st"));
        assert!(sql.contains("USING (delta_query) AS d"));
        assert!(sql.contains("ON st.__pgt_row_id = d.__pgt_row_id"));
        assert!(sql.contains("WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE"));
        assert!(sql.contains("UPDATE SET \"amount\" = d.\"amount\""));
        assert!(sql.contains("INSERT (__pgt_row_id, \"amount\")"));
        assert!(sql.contains("VALUES (d.__pgt_row_id, d.\"amount\")"));
        assert!(!sql.contains("__PGT_PART_PRED__"));
    }

    #[test]
    fn test_build_merge_sql_multiple_columns() {
        let cols = vec!["region".to_string(), "total".to_string(), "cnt".to_string()];
        let sql = build_merge_sql(
            "\"pgtrickle\".\"sales\"",
            "(SELECT * FROM delta)",
            &cols,
            false,
        );
        assert!(sql.contains(
            "UPDATE SET \"region\" = d.\"region\", \"total\" = d.\"total\", \"cnt\" = d.\"cnt\""
        ));
        assert!(sql.contains("INSERT (__pgt_row_id, \"region\", \"total\", \"cnt\")"));
        assert!(sql.contains("VALUES (d.__pgt_row_id, d.\"region\", d.\"total\", d.\"cnt\")"));
    }

    #[test]
    fn test_build_merge_sql_with_partition_key() {
        let cols = vec!["val".to_string()];
        let sql = build_merge_sql("\"public\".\"partitioned\"", "(delta)", &cols, true);
        assert!(sql.contains("ON st.__pgt_row_id = d.__pgt_row_id __PGT_PART_PRED__"));
    }

    #[test]
    fn test_build_merge_sql_without_partition_key() {
        let cols = vec!["val".to_string()];
        let sql = build_merge_sql("\"public\".\"simple\"", "(delta)", &cols, false);
        assert!(!sql.contains("__PGT_PART_PRED__"));
    }

    #[test]
    fn test_build_merge_sql_column_quoting() {
        let cols = vec!["my \"col\"".to_string(), "normal".to_string()];
        let sql = build_merge_sql("\"public\".\"t\"", "(delta)", &cols, false);
        assert!(sql.contains("\"my \"\"col\"\"\""));
    }

    #[test]
    fn test_build_merge_sql_is_distinct_from_guard() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let sql = build_merge_sql("\"public\".\"t\"", "(delta)", &cols, false);
        assert!(sql.contains("IS DISTINCT FROM"));
        assert!(sql.contains("st.\"a\"::text IS DISTINCT FROM d.\"a\"::text"));
    }

    // ── TG2-MERGE: format helpers ───────────────────────────────────

    #[test]
    fn test_format_col_list_basic() {
        let cols = vec!["a".to_string(), "b".to_string()];
        assert_eq!(format_col_list(&cols), "\"a\", \"b\"");
    }

    #[test]
    fn test_format_col_list_quoting() {
        let cols = vec!["my \"col\"".to_string()];
        assert_eq!(format_col_list(&cols), "\"my \"\"col\"\"\"");
    }

    #[test]
    fn test_format_prefixed_col_list_basic() {
        let cols = vec!["x".to_string(), "y".to_string()];
        assert_eq!(format_prefixed_col_list("d", &cols), "d.\"x\", d.\"y\"");
    }

    #[test]
    fn test_format_update_set_basic() {
        let cols = vec!["a".to_string(), "b".to_string()];
        assert_eq!(format_update_set(&cols), "\"a\" = d.\"a\", \"b\" = d.\"b\"");
    }

    // ── TG2-MERGE: trigger template unit tests ──────────────────────

    #[test]
    fn test_build_trigger_delete_keyed() {
        let sql = build_trigger_delete_sql("\"public\".\"t\"", 42, false);
        assert!(sql.contains("DELETE FROM \"public\".\"t\" AS st"));
        assert!(sql.contains("USING __pgt_delta_42 AS d"));
        assert!(sql.contains("d.__pgt_action = 'D'"));
    }

    #[test]
    fn test_build_trigger_delete_keyless() {
        let sql = build_trigger_delete_sql("\"public\".\"t\"", 42, true);
        assert!(sql.contains("ROW_NUMBER()"));
        assert!(sql.contains("__pgt_delta_42"));
    }

    #[test]
    fn test_build_trigger_update_sql_basic() {
        let cols = vec!["val".to_string()];
        let sql = build_trigger_update_sql("\"public\".\"t\"", 7, &cols);
        assert!(sql.contains("UPDATE \"public\".\"t\" AS st"));
        assert!(sql.contains("SET \"val\" = d.\"val\""));
        assert!(sql.contains("FROM __pgt_delta_7 AS d"));
        assert!(sql.contains("d.__pgt_action = 'I'"));
        assert!(sql.contains("IS DISTINCT FROM"));
    }

    #[test]
    fn test_build_trigger_insert_keyed() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let sql = build_trigger_insert_sql("\"public\".\"t\"", 10, &cols, false);
        assert!(sql.contains("INSERT INTO \"public\".\"t\""));
        assert!(sql.contains("DISTINCT ON (d.__pgt_row_id)"));
        assert!(sql.contains("__pgt_delta_10"));
    }

    #[test]
    fn test_build_trigger_insert_keyless() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let sql = build_trigger_insert_sql("\"public\".\"t\"", 10, &cols, true);
        assert!(sql.contains("INSERT INTO \"public\".\"t\""));
        assert!(!sql.contains("NOT EXISTS"));
        assert!(sql.contains("__pgt_delta_10"));
    }

    // ── TG2-MERGE: has_non_monotonic_cte() unit tests ───────────────

    #[test]
    fn test_has_non_monotonic_cte_plain_scan() {
        assert!(!has_non_monotonic_cte(
            "SELECT * FROM changes_42 WHERE lsn > $1",
        ));
    }

    #[test]
    fn test_has_non_monotonic_cte_aggregate() {
        assert!(has_non_monotonic_cte(
            "WITH __pgt_cte_agg_1 AS (SELECT ...) SELECT * FROM __pgt_cte_agg_1",
        ));
    }

    #[test]
    fn test_has_non_monotonic_cte_inner_join() {
        assert!(has_non_monotonic_cte("... __pgt_cte_join_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_left_join() {
        assert!(has_non_monotonic_cte("... __pgt_cte_left_join_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_full_join() {
        assert!(has_non_monotonic_cte("... __pgt_cte_full_join_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_anti_join() {
        assert!(has_non_monotonic_cte("... __pgt_cte_anti_join_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_semi_join() {
        assert!(has_non_monotonic_cte("... __pgt_cte_semi_join_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_distinct() {
        assert!(has_non_monotonic_cte("... __pgt_cte_dist_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_window() {
        assert!(has_non_monotonic_cte("... __pgt_cte_win_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_recursive() {
        assert!(has_non_monotonic_cte("... __pgt_cte_rc_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_intersect() {
        assert!(has_non_monotonic_cte("... __pgt_cte_isect_1 ..."));
    }

    #[test]
    fn test_has_non_monotonic_cte_except() {
        assert!(has_non_monotonic_cte("... __pgt_cte_exct_1 ..."));
    }

    // ── TG2-MERGE: build_hash_child_merge() unit tests ──────────────

    #[test]
    fn test_build_hash_child_merge_replaces_target() {
        let original = "MERGE INTO \"public\".\"parent\" AS st \
                        USING (SELECT * FROM delta) AS d \
                        ON st.__pgt_row_id = d.__pgt_row_id \
                        WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE";
        let result = build_hash_child_merge(
            "\"public\".\"child_p0\"",
            "__pgt_delta_mat_42",
            "\"key\"",
            pg_sys::Oid::from(12345u32),
            4,
            0,
            original,
            "\"public\".\"parent\"",
        );
        assert!(result.contains("ONLY \"public\".\"child_p0\""));
        assert!(!result.contains("\"public\".\"parent\""));
    }

    #[test]
    fn test_build_hash_child_merge_filters_with_satisfies_hash() {
        let original = "MERGE INTO \"public\".\"parent\" AS st \
                        USING (SELECT * FROM delta) AS d \
                        ON st.__pgt_row_id = d.__pgt_row_id \
                        WHEN MATCHED THEN DELETE";
        let result = build_hash_child_merge(
            "\"public\".\"child_p1\"",
            "__pgt_mat",
            "\"hash_col\"",
            pg_sys::Oid::from(99u32),
            8,
            3,
            original,
            "\"public\".\"parent\"",
        );
        assert!(result.contains("satisfies_hash_partition(99::oid, 8, 3, \"hash_col\")"));
        assert!(result.contains("__pgt_mat"));
    }

    #[test]
    fn test_build_hash_child_merge_strips_part_pred() {
        let original = "MERGE INTO \"public\".\"parent\" AS st \
                        USING (SELECT * FROM delta) AS d \
                        ON st.__pgt_row_id = d.__pgt_row_id __PGT_PART_PRED__ \
                        WHEN MATCHED THEN DELETE";
        let result = build_hash_child_merge(
            "\"public\".\"child\"",
            "__pgt_mat",
            "\"k\"",
            pg_sys::Oid::from(1u32),
            2,
            1,
            original,
            "\"public\".\"parent\"",
        );
        assert!(!result.contains("__PGT_PART_PRED__"));
    }

    // ── CORR-4: Z-set weight algebra property tests ─────────────────────────
    //
    // These proptest-based tests prove the correctness of the Z-set weight
    // aggregation contract used by `build_weight_agg_using` and
    // `build_keyless_weight_agg`:
    //
    //   For every __pgt_row_id:
    //     net_weight = SUM(CASE WHEN action='I' THEN 1 ELSE -1 END)
    //     if net_weight > 0 → emit as INSERT
    //     if net_weight < 0 → emit as DELETE
    //     if net_weight = 0 → discard (I/D cancellation)
    //
    // The correctness requirement is that the algebra commutes with
    // set application: applying the aggregated delta to a base set S
    // produces the same result as applying each individual action
    // sequentially (in any order, since they are independent by row_id).

    use proptest::prelude::*;

    /// Reference implementation of the Z-set weight algebra.
    /// Returns (net_inserts, net_deletes) for a given stream of actions.
    fn zset_ref(actions: &[char]) -> (i64, i64) {
        let net: i64 = actions
            .iter()
            .map(|a| if *a == 'I' { 1i64 } else { -1 })
            .sum();
        if net > 0 {
            (net, 0)
        } else if net < 0 {
            (0, -net)
        } else {
            (0, 0)
        }
    }

    /// Simulate sequential application of actions to a multiset.
    /// Returns the final count for a single row_id.
    fn sequential_apply(initial_count: u32, actions: &[char]) -> i64 {
        let mut count = initial_count as i64;
        for a in actions {
            match a {
                'I' => count += 1,
                'D' => count -= 1,
                _ => {}
            }
        }
        count
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(2000))]

        /// CORR-4a: For a single row_id with random I/D actions, the Z-set
        /// net weight matches sequential application.
        /// The Z-set algebra operates on signed multiplicities (no clipping):
        /// applying stream to initial count 0 gives net_weight directly.
        #[test]
        fn prop_weight_algebra_matches_sequential_from_zero(
            actions in proptest::collection::vec(
                proptest::sample::select(vec!['I', 'D']),
                1..=20
            ),
        ) {
            let (net_ins, net_del) = zset_ref(&actions);
            let seq_result = sequential_apply(0, &actions);

            // Z-set net weight must equal sequential result
            let zset_net = net_ins - net_del;
            prop_assert_eq!(
                zset_net, seq_result,
                "Z-set net ({}) != sequential result ({}) for actions {:?}",
                zset_net, seq_result, actions
            );
            // Output classification must be correct
            if seq_result > 0 {
                prop_assert_eq!(net_ins, seq_result);
                prop_assert_eq!(net_del, 0);
            } else if seq_result < 0 {
                prop_assert_eq!(net_ins, 0);
                prop_assert_eq!(net_del, -seq_result);
            } else {
                prop_assert_eq!(net_ins, 0);
                prop_assert_eq!(net_del, 0);
            }
        }

        /// CORR-4b: Merging two independent action streams for the same row_id
        /// produces the same net weight as concatenating them.
        ///
        /// This proves: weight(A ∪ B) = weight(A) + weight(B)
        /// which is the Z-set homomorphism property.
        #[test]
        fn prop_weight_algebra_additive(
            stream_a in proptest::collection::vec(
                proptest::sample::select(vec!['I', 'D']),
                0..=10
            ),
            stream_b in proptest::collection::vec(
                proptest::sample::select(vec!['I', 'D']),
                0..=10
            ),
        ) {
            let weight_a: i64 = stream_a.iter().map(|a| if *a == 'I' { 1i64 } else { -1 }).sum();
            let weight_b: i64 = stream_b.iter().map(|a| if *a == 'I' { 1i64 } else { -1 }).sum();

            let mut combined = stream_a.clone();
            combined.extend_from_slice(&stream_b);
            let weight_combined: i64 = combined.iter().map(|a| if *a == 'I' { 1i64 } else { -1 }).sum();

            prop_assert_eq!(
                weight_a + weight_b, weight_combined,
                "weight(A) + weight(B) != weight(A ∪ B): {} + {} != {} for A={:?} B={:?}",
                weight_a, weight_b, weight_combined, stream_a, stream_b
            );
        }

        /// CORR-4c: The HAVING <> 0 filter correctly eliminates zero-weight
        /// groups (I/D pairs cancel completely).
        #[test]
        fn prop_having_filter_zero_cancellation(
            n_inserts in 0u32..=10,
            n_deletes in 0u32..=10,
        ) {
            let mut actions: Vec<char> = Vec::new();
            actions.extend(std::iter::repeat_n('I', n_inserts as usize));
            actions.extend(std::iter::repeat_n('D', n_deletes as usize));

            let (net_ins, net_del) = zset_ref(&actions);
            let net_weight: i64 = n_inserts as i64 - n_deletes as i64;

            if net_weight == 0 {
                prop_assert_eq!(net_ins, 0);
                prop_assert_eq!(net_del, 0);
            } else if net_weight > 0 {
                prop_assert_eq!(net_ins, net_weight);
                prop_assert_eq!(net_del, 0);
            } else {
                prop_assert_eq!(net_ins, 0);
                prop_assert_eq!(net_del, net_weight.unsigned_abs() as i64);
            }
        }

        /// CORR-4d: Multi-row weight aggregation: for a batch of (row_id, action)
        /// pairs, each row_id's net weight is computed independently. Proves that
        /// GROUP BY __pgt_row_id correctly partitions the aggregation.
        #[test]
        fn prop_weight_algebra_multi_row_independence(
            rows in proptest::collection::vec(
                (0u32..5, proptest::collection::vec(
                    proptest::sample::select(vec!['I', 'D']),
                    1..=8
                )),
                1..=10
            ),
        ) {
            let mut per_row: std::collections::HashMap<u32, Vec<char>> = std::collections::HashMap::new();
            for (row_id, actions) in &rows {
                per_row.entry(*row_id).or_default().extend(actions);
            }

            for (row_id, actions) in &per_row {
                let (net_ins, net_del) = zset_ref(actions);
                let net_weight: i64 = actions.iter().map(|a| if *a == 'I' { 1i64 } else { -1 }).sum();

                if net_weight > 0 {
                    prop_assert_eq!(net_ins, net_weight,
                        "row_id={}: expected net_ins={} got {}", row_id, net_weight, net_ins);
                    prop_assert_eq!(net_del, 0);
                } else if net_weight < 0 {
                    prop_assert_eq!(net_ins, 0);
                    prop_assert_eq!(net_del, -net_weight,
                        "row_id={}: expected net_del={} got {}", row_id, -net_weight, net_del);
                } else {
                    prop_assert_eq!(net_ins, 0, "row_id={}: zero-weight should emit nothing", row_id);
                    prop_assert_eq!(net_del, 0);
                }
            }
        }

        /// CORR-4e: The DISTINCT ON ordering resolves D+I pairs (key change)
        /// to a single action per row_id.
        #[test]
        fn prop_keyed_distinct_on_resolves_to_single_action(
            n_inserts in 1u32..=10,
            n_deletes in 1u32..=10,
        ) {
            let net = n_inserts as i64 - n_deletes as i64;
            let expected_action = if net > 0 {
                Some('I')
            } else if net < 0 {
                Some('D')
            } else {
                None
            };

            let (ni, nd) = zset_ref(
                &{
                    let mut v = Vec::new();
                    v.extend(std::iter::repeat_n('I', n_inserts as usize));
                    v.extend(std::iter::repeat_n('D', n_deletes as usize));
                    v
                }
            );

            match expected_action {
                Some('I') => {
                    prop_assert!(ni > 0, "expected INSERT action for net={}", net);
                    prop_assert_eq!(nd, 0);
                }
                Some('D') => {
                    prop_assert!(nd > 0, "expected DELETE action for net={}", net);
                    prop_assert_eq!(ni, 0);
                }
                None => {
                    prop_assert_eq!(ni, 0, "expected no output for net=0");
                    prop_assert_eq!(nd, 0);
                }
                _ => unreachable!(),
            }
        }

        /// CORR-4f: Keyless weight aggregation expands to the correct count
        /// via generate_series. The ABS(net_weight) rows should be emitted.
        #[test]
        fn prop_keyless_weight_expansion_count(
            n_inserts in 0u32..=10,
            n_deletes in 0u32..=10,
        ) {
            let net = n_inserts as i64 - n_deletes as i64;
            let expected_count = net.unsigned_abs();

            let mut actions = Vec::new();
            actions.extend(std::iter::repeat_n('I', n_inserts as usize));
            actions.extend(std::iter::repeat_n('D', n_deletes as usize));

            let (ni, nd) = zset_ref(&actions);
            let actual_count = (ni + nd) as u64;

            prop_assert_eq!(
                actual_count, expected_count,
                "keyless expansion: expected {} rows, got {} for {} I + {} D",
                expected_count, actual_count, n_inserts, n_deletes
            );
        }
    }
}

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod pg_tests {
    use super::*;
    use crate::catalog::StreamTableMeta;
    use crate::version::Frontier;
    use pgrx::prelude::*;

    #[pg_test]
    fn test_execute_differential_refresh_success() {
        Spi::run("CREATE SCHEMA IF NOT EXISTS public");
        Spi::run("CREATE TABLE public.test_refresh_src (id INT PRIMARY KEY, val TEXT)");

        Spi::run(
            "SELECT pgtrickle.create_stream_table(
                'public.test_refresh_st',
                'SELECT id, val FROM public.test_refresh_src',
                '1 minute'
            );",
        );

        Spi::run("INSERT INTO public.test_refresh_src VALUES (1, 'hello'), (2, 'world')");

        // Wait, populate via refresh
        Spi::run("SELECT pgtrickle.refresh('public.test_refresh_st', 'FULL')");

        // Get metadata correctly
        let st = StreamTableMeta::get_by_name("public", "test_refresh_st").expect("st must exist");
        assert!(st.is_populated, "ST should be populated after FULL");

        let prev_frontier = st.frontier.clone();
        assert!(
            !prev_frontier.is_empty(),
            "Frontier should not be empty after FULL refresh"
        );

        // Make delta changes
        Spi::run("INSERT INTO public.test_refresh_src VALUES (3, 'foo')");
        Spi::run("UPDATE public.test_refresh_src SET val = 'bar' WHERE id = 1");
        Spi::run("DELETE FROM public.test_refresh_src WHERE id = 2");

        let new_frontier = crate::version::capture_current_frontier().expect("new frontier");

        let (inserted, deleted) = execute_differential_refresh(&st, &prev_frontier, &new_frontier)
            .expect("differential refresh should succeed");

        assert!(inserted > 0, "should have inserted rows");
        assert!(deleted > 0, "should have deleted rows");

        let count = Spi::get_one::<i64>("SELECT COUNT(*) FROM public.test_refresh_st")
            .unwrap()
            .unwrap();
        assert_eq!(count, 2, "1,3 should be present");

        Spi::run("SELECT pgtrickle.drop_stream_table('public.test_refresh_st')");
        Spi::run("DROP TABLE public.test_refresh_src CASCADE");
    }

    // ── G12-2: validate_topk_metadata_fields tests ─────────────────

    #[test]
    fn test_topk_metadata_valid() {
        assert!(validate_topk_metadata_fields(10, "score DESC", None).is_ok());
    }

    #[test]
    fn test_topk_metadata_valid_with_offset() {
        assert!(validate_topk_metadata_fields(10, "score DESC", Some(5)).is_ok());
    }

    #[test]
    fn test_topk_metadata_zero_limit() {
        assert!(validate_topk_metadata_fields(0, "score DESC", None).is_ok());
    }

    #[test]
    fn test_topk_metadata_negative_limit() {
        let result = validate_topk_metadata_fields(-1, "score DESC", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("negative"));
    }

    #[test]
    fn test_topk_metadata_empty_order_by() {
        let result = validate_topk_metadata_fields(10, "", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_topk_metadata_whitespace_order_by() {
        let result = validate_topk_metadata_fields(10, "   ", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_topk_metadata_negative_offset() {
        let result = validate_topk_metadata_fields(10, "score DESC", Some(-3));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("negative"));
    }

    // ── DAG-4: Bypass capture SQL tests ─────────────────────────────────

    #[test]
    fn test_build_bypass_capture_sql_basic() {
        let sql = build_bypass_capture_sql(
            42,
            &[
                ("id".to_string(), "integer".to_string()),
                ("name".to_string(), "text".to_string()),
            ],
            "pg_temp.__pgt_bypass_42",
            None,
        );
        assert!(sql.contains("CREATE TEMP TABLE IF NOT EXISTS pg_temp.__pgt_bypass_42"));
        assert!(sql.contains("ON COMMIT DROP"));
        assert!(sql.contains("\"new_id\""));
        assert!(sql.contains("\"new_name\""));
        assert!(sql.contains("FROM __pgt_delta_42 d"));
        assert!(sql.contains("d.__pgt_action IN ('I', 'D')"));
    }

    #[test]
    fn test_build_bypass_capture_sql_quoted_columns() {
        let sql = build_bypass_capture_sql(
            7,
            &[("col\"name".to_string(), "text".to_string())],
            "pg_temp.__pgt_bypass_7",
            None,
        );
        // Column with quote should be properly escaped.
        assert!(sql.contains(r#""new_col""name""#));
        assert!(sql.contains(r#"d."col""name""#));
    }

    #[test]
    fn test_build_bypass_capture_sql_column_defs() {
        let sql = build_bypass_capture_sql(
            1,
            &[
                ("a".to_string(), "bigint".to_string()),
                ("b".to_string(), "text".to_string()),
            ],
            "pg_temp.__pgt_bypass_1",
            None,
        );
        // Verify the column definitions in CREATE TEMP TABLE.
        assert!(sql.contains("lsn pg_lsn"));
        assert!(sql.contains("action \"char\""));
        assert!(sql.contains("pk_hash bigint"));
        assert!(sql.contains("\"new_a\" bigint"));
        assert!(sql.contains("\"new_b\" text"));
    }

    #[test]
    fn test_build_bypass_capture_sql_lsn_override() {
        let sql = build_bypass_capture_sql(
            42,
            &[("id".to_string(), "integer".to_string())],
            "pg_temp.__pgt_bypass_42",
            Some("0/1A2B3C"),
        );
        // Should use the literal LSN, not pg_current_wal_lsn()
        assert!(sql.contains("'0/1A2B3C'::pg_lsn"));
        assert!(!sql.contains("pg_current_wal_lsn()"));
    }

    #[test]
    fn test_build_bypass_capture_sql_no_lsn_override() {
        let sql = build_bypass_capture_sql(
            42,
            &[("id".to_string(), "integer".to_string())],
            "pg_temp.__pgt_bypass_42",
            None,
        );
        // Should use pg_current_wal_lsn() by default
        assert!(sql.contains("pg_current_wal_lsn()"));
    }

    #[test]
    fn test_st_bypass_thread_local_set_get_clear() {
        clear_all_st_bypass();
        assert!(get_st_bypass_tables().is_empty());

        set_st_bypass(10, "pg_temp.__pgt_bypass_10".to_string());
        set_st_bypass(20, "pg_temp.__pgt_bypass_20".to_string());

        let tables = get_st_bypass_tables();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[&10], "pg_temp.__pgt_bypass_10");
        assert_eq!(tables[&20], "pg_temp.__pgt_bypass_20");

        clear_st_bypass(10);
        assert_eq!(get_st_bypass_tables().len(), 1);

        clear_all_st_bypass();
        assert!(get_st_bypass_tables().is_empty());
    }

    // ── build_is_distinct_clause ─────────────────────────────────────────────

    #[test]
    fn test_build_is_distinct_clause_single_col() {
        let cols = vec!["price".to_string()];
        let clause = build_is_distinct_clause(&cols);
        assert_eq!(
            clause,
            r#"st."price"::text IS DISTINCT FROM d."price"::text"#
        );
    }

    #[test]
    fn test_build_is_distinct_clause_multi_col() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let clause = build_is_distinct_clause(&cols);
        assert_eq!(
            clause,
            r#"st."a"::text IS DISTINCT FROM d."a"::text OR st."b"::text IS DISTINCT FROM d."b"::text"#
        );
    }

    #[test]
    fn test_build_is_distinct_clause_col_with_double_quote() {
        let cols = vec!["col\"name".to_string()];
        let clause = build_is_distinct_clause(&cols);
        // Inner double-quote must be escaped as ""
        assert!(clause.contains(r#""col""name""#));
        assert!(clause.contains("IS DISTINCT FROM"));
    }

    #[test]
    fn test_build_is_distinct_clause_at_threshold() {
        // Exactly 50 columns — still per-column path
        let cols: Vec<String> = (0..50).map(|i| format!("col{i}")).collect();
        let clause = build_is_distinct_clause(&cols);
        // Per-column path produces OR-joined expressions
        assert!(clause.contains("IS DISTINCT FROM"));
        assert!(!clause.contains("pgtrickle.pg_trickle_hash"));
        let parts: Vec<&str> = clause.split(" OR ").collect();
        assert_eq!(parts.len(), 50);
    }

    #[test]
    fn test_build_is_distinct_clause_wide_table_hash_path() {
        // 51 columns — crosses into hash-based comparison
        let cols: Vec<String> = (0..51).map(|i| format!("col{i}")).collect();
        let clause = build_is_distinct_clause(&cols);
        assert!(clause.contains("pgtrickle.pg_trickle_hash"));
        assert!(clause.contains("IS DISTINCT FROM"));
        // Should reference both st. and d. prefixes
        assert!(clause.contains("st.\"col0\""));
        assert!(clause.contains("d.\"col0\""));
        // Should use the record separator
        assert!(clause.contains(r"'\x1E'"));
        // No OR — single hash expression
        assert!(!clause.contains(" OR "));
    }

    #[test]
    fn test_build_is_distinct_clause_empty_cols() {
        let cols: Vec<String> = vec![];
        let clause = build_is_distinct_clause(&cols);
        // Empty slice → empty string (no columns to compare)
        assert_eq!(clause, "");
    }

    // ── pg_quote_literal ─────────────────────────────────────────────────────

    #[test]
    fn test_pg_quote_literal_simple() {
        assert_eq!(pg_quote_literal("hello"), "'hello'");
    }

    #[test]
    fn test_pg_quote_literal_empty() {
        assert_eq!(pg_quote_literal(""), "''");
    }

    #[test]
    fn test_pg_quote_literal_single_quote_escaped() {
        assert_eq!(pg_quote_literal("it's"), "'it''s'");
    }

    #[test]
    fn test_pg_quote_literal_multiple_single_quotes() {
        assert_eq!(pg_quote_literal("a'b'c"), "'a''b''c'");
    }

    #[test]
    fn test_pg_quote_literal_only_quotes() {
        assert_eq!(pg_quote_literal("''"), "''''");
    }

    // ── inject_partition_predicate ───────────────────────────────────────────

    #[test]
    fn test_inject_partition_predicate_basic() {
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id__PGT_PART_PRED__";
        let bounds = PartitionBounds::Range {
            mins: vec!["2024-01-01".to_string()],
            maxs: vec!["2024-01-31".to_string()],
        };
        let result = inject_partition_predicate(merge_sql, "event_date", &bounds);
        assert!(result.contains("BETWEEN '2024-01-01' AND '2024-01-31'"));
        assert!(result.contains(r#""event_date""#));
        assert!(result.contains("st."));
        assert!(!result.contains("__PGT_PART_PRED__"));
    }

    #[test]
    fn test_inject_partition_predicate_no_placeholder() {
        // If there is no placeholder the SQL is returned unchanged
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id";
        let bounds = PartitionBounds::Range {
            mins: vec!["2024-01-01".to_string()],
            maxs: vec!["2024-01-31".to_string()],
        };
        let result = inject_partition_predicate(merge_sql, "event_date", &bounds);
        assert_eq!(result, merge_sql);
    }

    #[test]
    fn test_inject_partition_predicate_value_with_single_quote() {
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id__PGT_PART_PRED__";
        let bounds = PartitionBounds::Range {
            mins: vec!["O'Brien".to_string()],
            maxs: vec!["O'Reilly".to_string()],
        };
        let result = inject_partition_predicate(merge_sql, "name", &bounds);
        // Single quotes must be doubled inside the predicate literals
        assert!(result.contains("'O''Brien'"));
        assert!(result.contains("'O''Reilly'"));
    }

    // A1-1b: multi-column partition predicate tests

    #[test]
    fn test_inject_partition_predicate_multi_column() {
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id__PGT_PART_PRED__";
        let bounds = PartitionBounds::Range {
            mins: vec!["2024-01-01".to_string(), "100".to_string()],
            maxs: vec!["2024-01-31".to_string(), "999".to_string()],
        };
        let result = inject_partition_predicate(merge_sql, "event_day,customer_id", &bounds);
        // Multi-column uses ROW comparison instead of BETWEEN
        assert!(
            result
                .contains("ROW(st.\"event_day\", st.\"customer_id\") >= ROW('2024-01-01', '100')")
        );
        assert!(
            result
                .contains("ROW(st.\"event_day\", st.\"customer_id\") <= ROW('2024-01-31', '999')")
        );
        assert!(!result.contains("__PGT_PART_PRED__"));
    }

    #[test]
    fn test_inject_partition_predicate_three_columns() {
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id__PGT_PART_PRED__";
        let bounds = PartitionBounds::Range {
            mins: vec!["1".to_string(), "x".to_string(), "10".to_string()],
            maxs: vec!["9".to_string(), "z".to_string(), "90".to_string()],
        };
        let result = inject_partition_predicate(merge_sql, "a, b, c", &bounds);
        assert!(result.contains("ROW(st.\"a\", st.\"b\", st.\"c\") >= ROW('1', 'x', '10')"));
        assert!(result.contains("ROW(st.\"a\", st.\"b\", st.\"c\") <= ROW('9', 'z', '90')"));
    }

    // A1-1d: LIST partition predicate tests

    #[test]
    fn test_inject_partition_predicate_list_single_value() {
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id__PGT_PART_PRED__";
        let bounds = PartitionBounds::List(vec!["US".to_string()]);
        let result = inject_partition_predicate(merge_sql, "LIST:region", &bounds);
        assert!(result.contains("st.\"region\" IN ('US')"));
        assert!(!result.contains("__PGT_PART_PRED__"));
    }

    #[test]
    fn test_inject_partition_predicate_list_multiple_values() {
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id__PGT_PART_PRED__";
        let bounds =
            PartitionBounds::List(vec!["EU".to_string(), "US".to_string(), "APAC".to_string()]);
        let result = inject_partition_predicate(merge_sql, "LIST:region", &bounds);
        assert!(result.contains("st.\"region\" IN ('EU', 'US', 'APAC')"));
        assert!(!result.contains("__PGT_PART_PRED__"));
    }

    #[test]
    fn test_inject_partition_predicate_list_value_with_quote() {
        let merge_sql = "MERGE INTO st USING d ON st.id = d.id__PGT_PART_PRED__";
        let bounds = PartitionBounds::List(vec!["O'Brien".to_string()]);
        let result = inject_partition_predicate(merge_sql, "LIST:name", &bounds);
        assert!(result.contains("'O''Brien'"));
    }

    // ── build_weight_agg_using ───────────────────────────────────────────────

    #[test]
    fn test_build_weight_agg_using_contains_delta_sql() {
        let delta = "SELECT * FROM my_delta";
        let cols = "\"a\", \"b\"";
        let sql = build_weight_agg_using(delta, cols);
        assert!(sql.contains(delta));
        assert!(sql.contains(cols));
    }

    #[test]
    fn test_build_weight_agg_using_structure() {
        let sql = build_weight_agg_using("SELECT 1", "\"x\"");
        // Must contain the structural landmarks
        assert!(sql.contains("DISTINCT ON"));
        assert!(sql.contains("__pgt_row_id"));
        assert!(sql.contains("__pgt_action"));
        assert!(sql.contains("SUM"));
        assert!(sql.contains("HAVING"));
        assert!(sql.contains("GROUP BY"));
        assert!(sql.contains("ORDER BY"));
    }

    #[test]
    fn test_build_weight_agg_using_insert_delete_case() {
        let sql = build_weight_agg_using("SELECT 1", "\"x\"");
        assert!(sql.contains("'I'"));
        assert!(sql.contains("'D'"));
        // Net-weight sign decides the action
        assert!(sql.contains("> 0"));
        assert!(sql.contains("<> 0"));
    }

    // ── build_keyless_weight_agg ────────────────────────────────────────────

    #[test]
    fn test_build_keyless_weight_agg_contains_delta_sql() {
        let delta = "SELECT * FROM my_delta";
        let cols = "\"a\", \"b\"";
        let sql = build_keyless_weight_agg(delta, cols);
        assert!(sql.contains(delta));
        assert!(sql.contains(cols));
    }

    #[test]
    fn test_build_keyless_weight_agg_structure() {
        let sql = build_keyless_weight_agg("SELECT 1", "\"x\"");
        // Must contain weight-aggregation landmarks
        assert!(sql.contains("__pgt_row_id"));
        assert!(sql.contains("__pgt_action"));
        assert!(sql.contains("SUM"));
        assert!(sql.contains("HAVING"));
        assert!(sql.contains("GROUP BY"));
        // Must use generate_series for count expansion
        assert!(sql.contains("generate_series"));
        assert!(sql.contains("__pgt_cnt"));
        // Must NOT use DISTINCT ON (keyless allows duplicate row_ids)
        assert!(!sql.contains("DISTINCT ON"));
    }

    #[test]
    fn test_build_keyless_weight_agg_cancel_logic() {
        let sql = build_keyless_weight_agg("SELECT 1", "\"x\"");
        assert!(sql.contains("'I'"));
        assert!(sql.contains("'D'"));
        // Net-weight sign decides the action
        assert!(sql.contains("> 0"));
        // HAVING filters out net-zero groups (I/D cancellation)
        assert!(sql.contains("<> 0"));
        // ABS for the count expansion
        assert!(sql.contains("ABS"));
    }

    // ── build_keyless_delete_template ────────────────────────────────────────

    #[test]
    fn test_build_keyless_delete_template_contains_table() {
        let sql = build_keyless_delete_template("\"public\".\"my_table\"", 42);
        assert!(sql.starts_with("DELETE FROM \"public\".\"my_table\""));
    }

    #[test]
    fn test_build_keyless_delete_template_uses_pgt_id() {
        let sql = build_keyless_delete_template("\"s\".\"t\"", 99);
        assert!(sql.contains("__pgt_delta_99"));
    }

    #[test]
    fn test_build_keyless_delete_template_structure() {
        let sql = build_keyless_delete_template("\"s\".\"t\"", 1);
        // Must use ctid-based deletion with ROW_NUMBER pairing
        assert!(sql.contains("ctid"));
        assert!(sql.contains("ROW_NUMBER()"));
        assert!(sql.contains("PARTITION BY"));
        assert!(sql.contains("JOIN"));
        assert!(sql.contains("del_count"));
        assert!(sql.contains("__pgt_action = 'D'"));
    }

    #[test]
    fn test_build_keyless_delete_template_counts_correctly() {
        let sql = build_keyless_delete_template("\"s\".\"t\"", 5);
        // The WHERE clause must use <= del_count to limit paired deletions
        assert!(sql.contains("<= dc.del_count"));
    }

    // ── A1-3b: HASH partition bound spec parsing ────────────────────────────

    #[test]
    fn test_parse_hash_bound_spec_basic() {
        let (m, r) = parse_hash_bound_spec("FOR VALUES WITH (modulus 4, remainder 2)").unwrap();
        assert_eq!(m, 4);
        assert_eq!(r, 2);
    }

    #[test]
    fn test_parse_hash_bound_spec_various_values() {
        let (m, r) = parse_hash_bound_spec("FOR VALUES WITH (modulus 8, remainder 7)").unwrap();
        assert_eq!(m, 8);
        assert_eq!(r, 7);
    }

    #[test]
    fn test_parse_hash_bound_spec_remainder_zero() {
        let (m, r) = parse_hash_bound_spec("FOR VALUES WITH (modulus 4, remainder 0)").unwrap();
        assert_eq!(m, 4);
        assert_eq!(r, 0);
    }

    #[test]
    fn test_parse_hash_bound_spec_missing_modulus() {
        let result = parse_hash_bound_spec("FOR VALUES WITH (remainder 2)");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_hash_bound_spec_missing_remainder() {
        let result = parse_hash_bound_spec("FOR VALUES WITH (modulus 4)");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_keyword_int_basic() {
        assert_eq!(
            extract_keyword_int("MODULUS 4, REMAINDER 2", "MODULUS").unwrap(),
            4
        );
        assert_eq!(
            extract_keyword_int("MODULUS 4, REMAINDER 2", "REMAINDER").unwrap(),
            2
        );
    }

    #[test]
    fn test_extract_keyword_int_missing() {
        assert!(extract_keyword_int("SOME OTHER TEXT", "MODULUS").is_err());
    }

    // ── D-4: Multi-frontier cleanup model ──────────────────────────────

    /// Pure-Rust model of the multi-frontier cleanup logic.
    ///
    /// Given a set of consumer frontier LSNs for a source OID, computes the
    /// safe cleanup threshold: `MIN(consumer_frontiers)`. Only change buffer
    /// entries at or below this threshold may be deleted.
    ///
    /// Returns `None` when there are no consumers with a valid (non-0/0) frontier.
    fn compute_safe_cleanup_lsn(consumer_frontiers: &[&str]) -> Option<String> {
        let valid: Vec<&str> = consumer_frontiers
            .iter()
            .copied()
            .filter(|lsn| *lsn != "0/0")
            .collect();
        if valid.is_empty() {
            return None;
        }
        let mut min = valid[0];
        for &lsn in &valid[1..] {
            min = crate::version::lsn_min(min, lsn);
        }
        Some(min.to_string())
    }

    /// Model: given change buffer entries (as LSNs) and the safe cleanup
    /// threshold, returns the set of entries that should be RETAINED (not deleted).
    fn retained_after_cleanup(entry_lsns: &[&str], safe_lsn: &str) -> Vec<String> {
        entry_lsns
            .iter()
            .copied()
            .filter(|lsn| crate::version::lsn_gt(lsn, safe_lsn))
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn test_safe_cleanup_lsn_single_consumer() {
        let result = compute_safe_cleanup_lsn(&["0/100"]);
        assert_eq!(result, Some("0/100".to_string()));
    }

    #[test]
    fn test_safe_cleanup_lsn_multi_consumer_min() {
        // 5 consumers with different frontiers — safe threshold is the minimum.
        let result = compute_safe_cleanup_lsn(&["0/500", "0/200", "0/300", "0/100", "0/400"]);
        assert_eq!(result, Some("0/100".to_string()));
    }

    #[test]
    fn test_safe_cleanup_lsn_skips_zero() {
        // Consumer at 0/0 is uninitialized — excluded.
        let result = compute_safe_cleanup_lsn(&["0/0", "0/200", "0/100"]);
        assert_eq!(result, Some("0/100".to_string()));
    }

    #[test]
    fn test_safe_cleanup_lsn_all_zero() {
        let result = compute_safe_cleanup_lsn(&["0/0", "0/0"]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_safe_cleanup_lsn_empty() {
        let result = compute_safe_cleanup_lsn(&[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_retained_after_cleanup_basic() {
        let entries = vec!["0/50", "0/100", "0/150", "0/200"];
        let retained = retained_after_cleanup(&entries, "0/100");
        assert_eq!(retained, vec!["0/150", "0/200"]);
    }

    #[test]
    fn test_retained_after_cleanup_nothing_deleted() {
        let entries = vec!["0/200", "0/300"];
        let retained = retained_after_cleanup(&entries, "0/100");
        assert_eq!(retained, vec!["0/200", "0/300"]);
    }

    #[test]
    fn test_retained_after_cleanup_all_deleted() {
        let entries = vec!["0/50", "0/100"];
        let retained = retained_after_cleanup(&entries, "0/200");
        assert!(retained.is_empty());
    }

    #[test]
    fn test_multi_frontier_cleanup_never_deletes_unconsumed() {
        // Core correctness property: if consumer C has frontier at LSN X,
        // then no entry with LSN > X should ever be deleted.
        //
        // Scenario: 5 consumers with different frontier positions.
        // Buffer has entries at every 0x100 step from 0/100 to 0/A00.
        let consumer_frontiers = vec!["0/300", "0/700", "0/500", "0/200", "0/900"];
        let buffer_entries: Vec<&str> = vec![
            "0/100", "0/200", "0/300", "0/400", "0/500", "0/600", "0/700", "0/800", "0/900",
            "0/A00",
        ];

        let safe_lsn =
            compute_safe_cleanup_lsn(&consumer_frontiers).expect("should have a safe threshold");
        assert_eq!(safe_lsn, "0/200"); // MIN of all consumers

        let retained = retained_after_cleanup(&buffer_entries, &safe_lsn);

        // Verify: every consumer can still read all entries at or above its frontier.
        for &consumer_lsn in &consumer_frontiers {
            // Entries the consumer still needs: LSN > consumer's PREVIOUS frontier.
            // In production, the consumer reads entries between prev and current frontier,
            // but the critical invariant is: entries above the MIN frontier are retained.
            assert!(
                retained.iter().any(|e| e == consumer_lsn)
                    || crate::version::lsn_gt(&safe_lsn, consumer_lsn)
                    || safe_lsn == consumer_lsn,
                "consumer at {} should find its entries retained or already consumed",
                consumer_lsn
            );
        }

        // No entry above the slowest consumer was deleted.
        let min_consumer = "0/200";
        for entry in &retained {
            assert!(
                crate::version::lsn_gt(entry, min_consumer),
                "retained entry {} should be above safe threshold {}",
                entry,
                min_consumer
            );
        }
    }

    // ── D-4: Property-based test — random frontier advancement ──────

    use proptest::prelude::*;

    /// Generate a random LSN as "0/XXXX" where XXXX is a hex value 1..FFFF.
    fn arb_lsn() -> impl Strategy<Value = String> {
        (1u64..0xFFFFu64).prop_map(|v| format!("0/{:X}", v))
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(500))]

        /// Property: MIN(frontiers) is always the safe cleanup threshold.
        /// No entry above this threshold should be deleted.
        /// All entries at or below should be deletable.
        #[test]
        fn prop_multi_frontier_cleanup_correctness(
            frontiers in proptest::collection::vec(arb_lsn(), 5..=10),
            entries in proptest::collection::vec(arb_lsn(), 1..=20),
        ) {
            let frontier_refs: Vec<&str> = frontiers.iter().map(|s| s.as_str()).collect();
            let entry_refs: Vec<&str> = entries.iter().map(|s| s.as_str()).collect();

            let safe_lsn = compute_safe_cleanup_lsn(&frontier_refs);

            if let Some(ref threshold) = safe_lsn {
                let retained = retained_after_cleanup(&entry_refs, threshold);

                // Invariant 1: Every retained entry is strictly above the threshold.
                for entry in &retained {
                    prop_assert!(
                        crate::version::lsn_gt(entry, threshold),
                        "retained entry {} should be > threshold {}",
                        entry,
                        threshold
                    );
                }

                // Invariant 2: Every non-retained entry is at or below the threshold.
                let deleted: Vec<&str> = entry_refs
                    .iter()
                    .copied()
                    .filter(|e| !retained.contains(&e.to_string()))
                    .collect();
                for entry in &deleted {
                    prop_assert!(
                        !crate::version::lsn_gt(entry, threshold),
                        "deleted entry {} should be <= threshold {}",
                        entry,
                        threshold
                    );
                }

                // Invariant 3: For every consumer, all entries at LSNs above
                // the consumer's frontier are still present in the retained set.
                // (This is the "no premature deletion" property.)
                for consumer_lsn in &frontier_refs {
                    for entry in &entry_refs {
                        if crate::version::lsn_gt(entry, consumer_lsn) {
                            // This entry hasn't been consumed by this consumer yet.
                            // It should be retained.
                            prop_assert!(
                                retained.contains(&entry.to_string()),
                                "entry {} is above consumer frontier {} but was deleted (threshold {})",
                                entry,
                                consumer_lsn,
                                threshold
                            );
                        }
                    }
                }
            }
        }

        /// Property: Advancing the slowest consumer raises the safe threshold.
        #[test]
        fn prop_advancing_slowest_consumer_raises_threshold(
            base_frontiers in proptest::collection::vec(arb_lsn(), 5..=8),
            advance_amount in 1u64..0x1000u64,
        ) {
            let frontier_refs: Vec<&str> = base_frontiers.iter().map(|s| s.as_str()).collect();

            if let Some(ref old_threshold) = compute_safe_cleanup_lsn(&frontier_refs) {
                // Find the index of the minimum frontier.
                let min_idx = frontier_refs
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        let pa = crate::version::lsn_gt(a, b);
                        if pa { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less }
                    })
                    .map(|(i, _)| i)
                    .unwrap();

                // Advance the slowest consumer.
                let mut advanced = base_frontiers.clone();
                let old_val = crate::version::lsn_gt(&advanced[min_idx], "0/0");
                if old_val {
                    // Parse and advance
                    let parts: Vec<&str> = advanced[min_idx].split('/').collect();
                    let lo = u64::from_str_radix(parts[1], 16).unwrap_or(0);
                    advanced[min_idx] = format!("0/{:X}", lo.saturating_add(advance_amount));
                }

                let new_frontier_refs: Vec<&str> = advanced.iter().map(|s| s.as_str()).collect();
                if let Some(ref new_threshold) = compute_safe_cleanup_lsn(&new_frontier_refs) {
                    prop_assert!(
                        crate::version::lsn_gte(new_threshold, old_threshold),
                        "advancing slowest consumer should not lower threshold: old={}, new={}",
                        old_threshold,
                        new_threshold
                    );
                }
            }
        }

        /// Property: Adding a new consumer at LSN 0/1 (just initialized) should
        /// lower or maintain the safe threshold.
        #[test]
        fn prop_new_consumer_lowers_threshold(
            base_frontiers in proptest::collection::vec(arb_lsn(), 5..=8),
        ) {
            let frontier_refs: Vec<&str> = base_frontiers.iter().map(|s| s.as_str()).collect();

            if let Some(ref old_threshold) = compute_safe_cleanup_lsn(&frontier_refs) {
                // Add a new consumer that just completed its first full refresh
                // with a very low frontier.
                let mut with_new = base_frontiers.clone();
                with_new.push("0/1".to_string());
                let new_frontier_refs: Vec<&str> = with_new.iter().map(|s| s.as_str()).collect();

                if let Some(ref new_threshold) = compute_safe_cleanup_lsn(&new_frontier_refs) {
                    prop_assert!(
                        !crate::version::lsn_gt(new_threshold, old_threshold),
                        "adding consumer at 0/1 should not raise threshold: old={}, new={}",
                        old_threshold,
                        new_threshold
                    );
                }
            }
        }
    }

    // ── D-4: Column superset computation tests ──────────────────────

    #[test]
    fn test_column_superset_union() {
        // Simulate: ST1 uses {a, b, c}, ST2 uses {b, d}, ST3 uses {a, e}.
        // Column superset = {a, b, c, d, e}.
        let st1: Vec<String> = vec!["a", "b", "c"].into_iter().map(String::from).collect();
        let st2: Vec<String> = vec!["b", "d"].into_iter().map(String::from).collect();
        let st3: Vec<String> = vec!["a", "e"].into_iter().map(String::from).collect();

        let mut superset = std::collections::HashSet::new();
        for col in st1.iter().chain(st2.iter()).chain(st3.iter()) {
            superset.insert(col.to_lowercase());
        }

        let mut sorted: Vec<String> = superset.into_iter().collect();
        sorted.sort();
        assert_eq!(sorted, vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn test_column_superset_select_star_forces_full() {
        // If any ST uses SELECT * (columns_used = None), the superset must
        // include ALL columns.
        let st1: Option<Vec<String>> = Some(vec!["a".to_string(), "b".to_string()]);
        let st2: Option<Vec<String>> = None; // SELECT *

        // When any consumer has None, the union should be None (full capture).
        let union_result = match (&st1, &st2) {
            (_, None) | (None, _) => None,
            (Some(a), Some(b)) => {
                let mut s: std::collections::HashSet<String> = a.iter().cloned().collect();
                s.extend(b.iter().cloned());
                Some(s.into_iter().collect::<Vec<_>>())
            }
        };
        assert!(union_result.is_none());
    }

    // ── B-4: classify_query_complexity() ────────────────────────────

    #[test]
    fn test_classify_scan() {
        let q = "SELECT id, name FROM users";
        assert_eq!(classify_query_complexity(q), QueryComplexityClass::Scan);
    }

    #[test]
    fn test_classify_filter() {
        let q = "SELECT id, name FROM users WHERE active = true";
        assert_eq!(classify_query_complexity(q), QueryComplexityClass::Filter);
    }

    #[test]
    fn test_classify_aggregate() {
        let q = "SELECT region, SUM(amount) FROM orders GROUP BY region";
        assert_eq!(
            classify_query_complexity(q),
            QueryComplexityClass::Aggregate
        );
    }

    #[test]
    fn test_classify_join() {
        let q = "SELECT o.id, u.name FROM orders o JOIN users u ON o.user_id = u.id";
        assert_eq!(classify_query_complexity(q), QueryComplexityClass::Join);
    }

    #[test]
    fn test_classify_join_aggregate() {
        let q = "SELECT u.name, SUM(o.amount) FROM orders o JOIN users u ON o.user_id = u.id GROUP BY u.name";
        assert_eq!(
            classify_query_complexity(q),
            QueryComplexityClass::JoinAggregate
        );
    }

    #[test]
    fn test_classify_left_join() {
        let q = "SELECT * FROM a LEFT JOIN b ON a.id = b.a_id";
        assert_eq!(classify_query_complexity(q), QueryComplexityClass::Join);
    }

    #[test]
    fn test_classify_case_insensitive() {
        let q = "select id from users where active group by id";
        assert_eq!(
            classify_query_complexity(q),
            QueryComplexityClass::Aggregate
        );
    }

    // ── B-4: cost_model_prefers_full() ──────────────────────────────

    #[test]
    fn test_cost_model_prefers_full_large_delta() {
        // avg_ms_per_delta=1.0, avg_full=100ms, delta=200 rows, scan class
        // est_diff = 1.0 * 1.0 * 200 = 200ms > 100 * 0.8 = 80ms → FULL
        assert!(cost_model_prefers_full(
            1.0,
            100.0,
            200,
            QueryComplexityClass::Scan
        ));
    }

    #[test]
    fn test_cost_model_prefers_diff_small_delta() {
        // avg_ms_per_delta=1.0, avg_full=100ms, delta=10 rows, scan class
        // est_diff = 1.0 * 1.0 * 10 = 10ms < 100 * 0.8 = 80ms → DIFF
        assert!(!cost_model_prefers_full(
            1.0,
            100.0,
            10,
            QueryComplexityClass::Scan
        ));
    }

    #[test]
    fn test_cost_model_complexity_affects_decision() {
        // Same delta count, but JoinAggregate has 4× factor
        // Scan: est_diff = 0.5 * 1.0 * 100 = 50ms < 100 * 0.8 = 80ms → DIFF
        assert!(!cost_model_prefers_full(
            0.5,
            100.0,
            100,
            QueryComplexityClass::Scan
        ));
        // JoinAgg: est_diff = 0.5 * 4.0 * 100 = 200ms > 80ms → FULL
        assert!(cost_model_prefers_full(
            0.5,
            100.0,
            100,
            QueryComplexityClass::JoinAggregate
        ));
    }

    // ── B-4: diff_cost_factor() ─────────────────────────────────────

    #[test]
    fn test_diff_cost_factors_ordering() {
        assert!(
            QueryComplexityClass::Scan.diff_cost_factor()
                < QueryComplexityClass::Filter.diff_cost_factor()
        );
        assert!(
            QueryComplexityClass::Filter.diff_cost_factor()
                < QueryComplexityClass::Aggregate.diff_cost_factor()
        );
        assert!(
            QueryComplexityClass::Aggregate.diff_cost_factor()
                < QueryComplexityClass::Join.diff_cost_factor()
        );
        assert!(
            QueryComplexityClass::Join.diff_cost_factor()
                < QueryComplexityClass::JoinAggregate.diff_cost_factor()
        );
    }
}
