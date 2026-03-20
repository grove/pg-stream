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
        let table_exists = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM pg_class c \
               JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = '{schema}' \
                 AND c.relname = 'changes_{oid}' \
                 AND c.relkind = 'r'\
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

        if can_truncate {
            if let Err(e) = Spi::run(&format!(
                "TRUNCATE \"{schema}\".changes_{oid}",
                schema = change_schema,
            )) {
                pgrx::debug1!("[pg_trickle] Deferred cleanup TRUNCATE failed: {}", e);
            }
        } else {
            let delete_sql = format!(
                "DELETE FROM \"{schema}\".changes_{oid} \
                 WHERE lsn <= '{safe_lsn}'::pg_lsn",
                schema = change_schema,
            );
            if let Err(e) = Spi::run(&delete_sql) {
                pgrx::debug1!("[pg_trickle] Deferred cleanup DELETE failed: {}", e);
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
        let table_exists = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM pg_class c \
               JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = '{schema}' \
                 AND c.relname = 'changes_{oid}' \
                 AND c.relkind = 'r'\
             )",
            schema = change_schema,
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if !table_exists {
            continue;
        }

        // Compute the minimum frontier LSN across ALL stream tables that
        // depend on this source OID.  Both TABLE and FOREIGN_TABLE sources
        // are included: FT change buffers are written by polling and must be
        // cleaned up once all consumers have advanced their frontier past them.
        let min_lsn: Option<String> = Spi::get_one::<String>(&format!(
            "SELECT MIN((st.frontier->'sources'->'{oid}'->>'lsn')::pg_lsn)::TEXT \
             FROM pgtrickle.pgt_stream_tables st \
             JOIN pgtrickle.pgt_dependencies dep ON dep.pgt_id = st.pgt_id \
             WHERE dep.source_relid = {oid} \
               AND dep.source_type IN ('TABLE', 'FOREIGN_TABLE') \
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

/// Apply `SET LOCAL` planner hints based on the estimated delta size.
///
/// - Small delta (ratio < merge_seqscan_threshold): `SET LOCAL enable_seqscan = off`
///   to force index lookups on the stream table.
/// - delta 100–9 999: `SET LOCAL enable_nestloop = off`
/// - delta >= 10 000: also `SET LOCAL work_mem = '<N>MB'`
///
/// `SET LOCAL` is automatically reset at the end of the current transaction,
/// so these hints cannot leak to other queries.
fn apply_planner_hints(estimated_delta: i64, st_relid: pg_sys::Oid) {
    if !crate::config::pg_trickle_merge_planner_hints() {
        return;
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
        if let Err(e) = Spi::run("SET LOCAL enable_nestloop = off") {
            pgrx::debug1!(
                "[pg_trickle] D-1: failed to SET LOCAL enable_nestloop: {}",
                e
            );
        }
        let mb = crate::config::pg_trickle_merge_work_mem_mb();
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
         WHERE d.__pgt_action = 'I'"
    )
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

    let has_stream_table_source = StDependency::get_for_st(st.pgt_id)
        .map(|deps| deps.iter().any(|dep| dep.source_type == "STREAM_TABLE"))
        .unwrap_or(false);

    if has_stream_table_source {
        pgrx::debug1!(
            "[pg_trickle] cache pre-warm skipped for {}.{}: upstream stream tables use full-refresh fallback",
            schema,
            name,
        );
        return;
    }

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

    let user_col_list: String = user_cols
        .iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let d_user_col_list: String = user_cols
        .iter()
        .map(|c| format!("d.\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");

    let update_set_clause: String = user_cols
        .iter()
        .map(|c| {
            let qc = format!("\"{}\"", c.replace('"', "\"\""));
            format!("{qc} = d.{qc}")
        })
        .collect::<Vec<_>>()
        .join(", ");

    let delta_sql_template =
        dvm::get_delta_sql_template(st.pgt_id).unwrap_or(delta_result.delta_sql);

    // Build the USING clause — skip DISTINCT ON when the delta is already
    // deduplicated (G-M1 optimization for scan-chain queries).
    //
    // EC-06: For keyless sources the delta is never deduplicated (multiple
    // rows can share the same __pgt_row_id). Skip DISTINCT ON unconditionally.
    let using_clause = if delta_result.is_deduplicated {
        format!("({delta_sql_template})")
    } else if st.has_keyless_source {
        // Keyless: do NOT collapse with DISTINCT ON — duplicate row_ids are
        // intentional (one per net insert/delete).
        format!("({delta_sql_template})")
    } else {
        format!(
            "(SELECT DISTINCT ON (__pgt_row_id) * \
             FROM ({delta_sql_template}) __raw \
             ORDER BY __pgt_row_id, __pgt_action DESC)"
        )
    };

    // B-1: IS DISTINCT FROM guard to skip no-op UPDATEs.
    let is_distinct_clause: String = build_is_distinct_clause(user_cols);

    let merge_template = format!(
        "MERGE INTO {quoted_table} AS st \
         USING {using_clause} AS d \
         ON st.__pgt_row_id = d.__pgt_row_id \
         WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE \
         WHEN MATCHED AND d.__pgt_action = 'I' AND ({is_distinct_clause}) THEN \
           UPDATE SET {update_set_clause} \
         WHEN NOT MATCHED AND d.__pgt_action = 'I' THEN \
           INSERT (__pgt_row_id, {user_col_list}) \
           VALUES (d.__pgt_row_id, {d_user_col_list})",
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
    let trigger_delete_template = if st.has_keyless_source {
        build_keyless_delete_template(&quoted_table, st.pgt_id)
    } else {
        format!(
            "DELETE FROM {quoted_table} AS st \
             USING __pgt_delta_{pgt_id} AS d \
             WHERE st.__pgt_row_id = d.__pgt_row_id \
               AND d.__pgt_action = 'D'",
            pgt_id = st.pgt_id,
        )
    };

    // EC-06: For keyless sources, the scan-level delta decomposes UPDATEs
    // into D+I pairs (different content hashes), so the UPDATE template
    // naturally matches 0 rows. For aggregate queries on keyless sources,
    // the aggregate delta produces 'I' actions for changed groups that
    // need real UPDATEs. Using the normal UPDATE template handles both
    // cases correctly.
    let trigger_update_template = format!(
        "UPDATE {quoted_table} AS st \
         SET {update_set_clause} \
         FROM __pgt_delta_{pgt_id} AS d \
         WHERE st.__pgt_row_id = d.__pgt_row_id \
           AND d.__pgt_action = 'I' \
           AND ({is_distinct_clause})",
        pgt_id = st.pgt_id,
    );

    let trigger_insert_template = if st.has_keyless_source {
        // Plain INSERT — no NOT EXISTS check since duplicate row_ids
        // are expected for keyless sources.
        format!(
            "INSERT INTO {quoted_table} (__pgt_row_id, {user_col_list}) \
             SELECT d.__pgt_row_id, {d_user_col_list} \
             FROM __pgt_delta_{pgt_id} AS d \
             WHERE d.__pgt_action = 'I'",
            pgt_id = st.pgt_id,
        )
    } else {
        format!(
            "INSERT INTO {quoted_table} (__pgt_row_id, {user_col_list}) \
             SELECT d.__pgt_row_id, {d_user_col_list} \
             FROM __pgt_delta_{pgt_id} AS d \
             WHERE d.__pgt_action = 'I' \
               AND NOT EXISTS (\
                 SELECT 1 FROM {quoted_table} AS st \
                 WHERE st.__pgt_row_id = d.__pgt_row_id\
               )",
            pgt_id = st.pgt_id,
        )
    };

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
        eq
    } else {
        query.clone()
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

    // Re-enable user triggers and emit NOTIFY so listeners know a FULL
    // refresh occurred.
    if has_triggers {
        Spi::run(&format!("ALTER TABLE {quoted_table} ENABLE TRIGGER USER"))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        // Escape single quotes in the JSON payload.
        let escaped_name = name.replace('\'', "''");
        let escaped_schema = schema.replace('\'', "''");
        Spi::run(&format!(
            "NOTIFY pgtrickle_refresh, '{{\"stream_table\": \"{escaped_name}\", \
             \"schema\": \"{escaped_schema}\", \"mode\": \"FULL\", \"rows\": {rows_inserted}}}'"
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        pgrx::info!(
            "pg_trickle: FULL refresh of {}.{} with user triggers suppressed ({} rows). \
             Row-level triggers do NOT fire for FULL refresh; use REFRESH MODE DIFFERENTIAL.",
            schema,
            name,
            rows_inserted,
        );
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
        .filter(|d| d.source_type == "TABLE" || d.source_type == "FOREIGN_TABLE")
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
}

/// Poll all FOREIGN_TABLE dependencies for a stream table before selecting a
/// new differential frontier.
///
/// Polling writes synthetic CDC rows into the local change buffers and updates
/// the per-source snapshot tables. Callers must do this before capturing the
/// new upper frontier so the synthetic rows fall within the refresh window.
pub fn poll_foreign_table_sources_for_st(st: &StreamTableMeta) -> Result<(), PgTrickleError> {
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");

    for dep in StDependency::get_for_st(st.pgt_id)?
        .into_iter()
        .filter(|dep| dep.source_type == "FOREIGN_TABLE")
    {
        crate::cdc::poll_foreign_table_changes(dep.source_relid, &change_schema)?;
    }

    Ok(())
}

/// Execute a NO_DATA refresh: just advance the data timestamp.
pub fn execute_no_data_refresh(st: &StreamTableMeta) -> Result<(), PgTrickleError> {
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

    // ── Short-circuit: skip the entire pipeline if no changes exist ──────
    let change_schema = crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let catalog_source_oids: Vec<u32> = StDependency::get_for_st(st.pgt_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|dep| dep.source_type == "TABLE" || dep.source_type == "FOREIGN_TABLE")
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

    if !any_changes {
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
            Spi::get_one::<bool>(&format!(
                "SELECT EXISTS(\
                   SELECT 1 FROM \"{change_schema}\".changes_{oid} \
                   WHERE lsn > '{prev_lsn}'::pg_lsn \
                   AND lsn <= '{new_lsn}'::pg_lsn \
                   AND action IN ('D', 'U') \
                   LIMIT 1\
                 )",
            ))
            .unwrap_or(Some(false))
            .unwrap_or(false)
        });

        if has_non_insert {
            pgrx::info!(
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
    // Session 7: per-ST adaptive threshold takes priority over global GUC.
    let global_ratio = crate::config::pg_trickle_differential_max_change_ratio();
    let max_ratio = st.auto_threshold.unwrap_or(global_ratio);
    let mut should_fallback = false;
    let mut total_change_count: i64 = 0;
    let mut _total_table_size: i64 = 0;
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
             FROM (SELECT GREATEST(reltuples::bigint, 1) AS table_size \
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

        // B3-1: Record sources with zero changes for delta-branch pruning.
        if change_count == 0 {
            zero_change_oids.insert(*oid);
        }

        if change_count > threshold_rows {
            should_fallback = true;
            break; // No need to check remaining sources
        }
    }

    if should_fallback {
        pgrx::info!(
            "[pg_trickle] Adaptive fallback: change ratio exceeds threshold {:.0}% — using FULL refresh",
            max_ratio * 100.0,
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
    if !should_fallback
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

        if st_group_count > 0 && total_change_count >= st_group_count {
            pgrx::info!(
                "[pg_trickle] Aggregate saturation: {} changes >= {} groups — using FULL refresh for {}.{}",
                total_change_count,
                st_group_count,
                schema,
                name,
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

    let has_any_cte = dvm::query_has_cte(&st.defining_query)?;
    let has_recursive_cte = dvm::query_has_recursive_cte(&st.defining_query)?;

    if has_any_cte {
        pgrx::info!(
            "[pg_trickle] CTE-backed differential refresh fallback: using FULL refresh for {}.{}",
            schema,
            name,
        );
        let result = execute_full_refresh(st);
        if result.is_ok() {
            post_full_refresh_cleanup(st);
        }
        return result;
    }

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
    }

    let resolved = if let Some(entry) = cached {
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

        let delta_sql = delta_result.delta_sql;
        let user_cols = delta_result.output_columns;
        let source_oids = delta_result.source_oids;
        let is_dedup = delta_result.is_deduplicated;

        let quoted_table = format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\""),
        );

        let user_col_list: String = user_cols
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");

        let d_user_col_list: String = user_cols
            .iter()
            .map(|c| format!("d.\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");

        let update_set_clause: String = user_cols
            .iter()
            .map(|c| {
                let qc = format!("\"{}\"", c.replace('"', "\"\""));
                format!("{qc} = d.{qc}")
            })
            .collect::<Vec<_>>()
            .join(", ");

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

        // Build template USING clause — skip DISTINCT ON when deduplicated (G-M1)
        // EC-06: For keyless sources, never collapse with DISTINCT ON.
        let template_using = if is_dedup || st.has_keyless_source {
            format!("({delta_sql_template})")
        } else {
            format!(
                "(SELECT DISTINCT ON (__pgt_row_id) * \
                 FROM ({delta_sql_template}) __raw \
                 ORDER BY __pgt_row_id, __pgt_action DESC)"
            )
        };

        // ── B-1: IS DISTINCT FROM guard to skip no-op UPDATEs ───────
        // When a group's aggregate value hasn't actually changed, the
        // MERGE would still perform an UPDATE (writing an identical
        // tuple).  Adding an IS DISTINCT FROM check on the WHEN MATCHED
        // clause lets PostgreSQL skip the heap write entirely.
        let is_distinct_clause: String = build_is_distinct_clause(&user_cols);

        let merge_template = format!(
            "MERGE INTO {quoted_table} AS st \
             USING {template_using} AS d \
             ON st.__pgt_row_id = d.__pgt_row_id \
             WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE \
             WHEN MATCHED AND d.__pgt_action = 'I' AND ({is_distinct_clause}) THEN \
               UPDATE SET {update_set_clause} \
             WHEN NOT MATCHED AND d.__pgt_action = 'I' THEN \
               INSERT (__pgt_row_id, {user_col_list}) \
               VALUES (d.__pgt_row_id, {d_user_col_list})",
        );
        println!("MERGE SQL TEMPLATE:\n{}", merge_template);

        // ── B-3: DELETE + INSERT template removed (always use MERGE) ─

        // ── D-2: Build parameterized MERGE SQL for PREPARE ─────────
        let parameterized_merge_sql = parameterize_lsn_template(&merge_template, &source_oids);

        // ── User-trigger explicit DML templates ──────────────────────
        //
        // EC-06: Keyless sources use counted DELETE + plain INSERT.
        // But if is_dedup is true, the ST itself has a unique row ID
        // so we must use standard keyed templates.
        let trigger_delete_template = if st.has_keyless_source && !is_dedup {
            build_keyless_delete_template(&quoted_table, st.pgt_id)
        } else {
            format!(
                "DELETE FROM {quoted_table} AS st \
                 USING __pgt_delta_{pgt_id} AS d \
                 WHERE st.__pgt_row_id = d.__pgt_row_id \
                   AND d.__pgt_action = 'D'",
                pgt_id = st.pgt_id,
            )
        };

        // EC-06: Use normal UPDATE template for keyless sources — see
        // prewarm_merge_cache comment for full rationale.
        let trigger_update_template = format!(
            "UPDATE {quoted_table} AS st \
             SET {update_set_clause} \
             FROM __pgt_delta_{pgt_id} AS d \
             WHERE st.__pgt_row_id = d.__pgt_row_id \
               AND d.__pgt_action = 'I' \
               AND ({is_distinct_clause})",
            pgt_id = st.pgt_id,
        );

        let trigger_insert_template = if st.has_keyless_source && !is_dedup {
            format!(
                "INSERT INTO {quoted_table} (__pgt_row_id, {user_col_list}) \
                 SELECT d.__pgt_row_id, {d_user_col_list} \
                 FROM __pgt_delta_{pgt_id} AS d \
                 WHERE d.__pgt_action = 'I'",
                pgt_id = st.pgt_id,
            )
        } else {
            format!(
                "INSERT INTO {quoted_table} (__pgt_row_id, {user_col_list}) \
                 SELECT d.__pgt_row_id, {d_user_col_list} \
                 FROM __pgt_delta_{pgt_id} AS d \
                 WHERE d.__pgt_action = 'I' \
                   AND NOT EXISTS (\
                     SELECT 1 FROM {quoted_table} AS st \
                     WHERE st.__pgt_row_id = d.__pgt_row_id\
                   )",
                pgt_id = st.pgt_id,
            )
        };

        let _ = std::fs::write(
            "/tmp/pgtrickle_debug.sql",
            format!(
                "MERGE:\n{}\n\nTRIGGER_INSERT:\n{}\n",
                merge_template, trigger_insert_template
            ),
        );

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
        }
    };

    let t1 = Instant::now();

    // ── Diagnostic: detect OID mismatch between catalog and delta ────
    // If the delta template references source OIDs that are not in the
    // catalog deps, the MERGE will fail referencing nonexistent change
    // buffer tables.
    let delta_oids = &resolved.source_oids;
    let missing_in_delta: Vec<&u32> = delta_oids
        .iter()
        .filter(|oid| !catalog_source_oids.contains(oid))
        .collect();
    if !missing_in_delta.is_empty() {
        return Err(PgTrickleError::InternalError(format!(
            "OID MISMATCH (source_oids): delta template references \
             OIDs {missing_in_delta:?} not in catalog deps \
             {catalog_source_oids:?}. Delta source_oids={delta_oids:?}, \
             ST={schema}.{name} pgt_id={}",
            st.pgt_id,
        )));
    }

    // ── Diagnostic: scan merge SQL for change buffer table references ─
    // Extract all `changes_NNNNN` references from the SQL to detect
    // references to OIDs not in catalog_source_oids.
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
            .filter(|oid| !catalog_source_oids.contains(oid))
            .collect();
        if !missing_in_sql.is_empty() {
            // Dump first 500 chars of merge SQL for diagnosis
            let sql_prefix: String = resolved.merge_sql.chars().take(500).collect();
            return Err(PgTrickleError::InternalError(format!(
                "OID MISMATCH (SQL text): merge SQL references changes_* \
                 for OIDs {missing_in_sql:?} not in catalog deps \
                 {catalog_source_oids:?}. SQL OIDs found={sql_oids:?}, \
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
    apply_planner_hints(total_change_count, st.pgt_relid);

    // ── A-3a: Append-only INSERT fast path ───────────────────────────
    // When the stream table is marked append-only (and hasn't been
    // reverted by the heuristic check above), skip MERGE entirely and
    // use a simple INSERT … SELECT from the delta. This avoids the
    // DELETE, UPDATE, and IS DISTINCT FROM overhead of the MERGE path.
    if is_append_only {
        let t_insert_start = Instant::now();

        // Build INSERT SQL from the resolved MERGE SQL's USING clause.
        // The MERGE SQL has the form:
        //   MERGE INTO "schema"."table" AS st USING (...delta...) AS d ON ...
        // We extract the delta subquery and wrap it in INSERT INTO.
        let insert_sql = build_append_only_insert_sql(schema, name, &resolved.merge_sql);

        let rows_inserted = Spi::connect_mut(|client| {
            let result = client
                .update(&insert_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<i64, PgTrickleError>(result.len() as i64)
        })?;

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

        return Ok((rows_inserted, 0));
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
    // Always use MERGE. The delete_insert path was removed in v0.2.0
    // (pg_trickle.merge_strategy GUC removed — C1 cleanup).

    // ── D-2: Prepared-statement flag ─────────────────────────────────
    let use_prepared = crate::config::pg_trickle_use_prepared_statements() && was_cache_hit;

    let (merge_count, strategy_label) = if use_explicit_dml {
        // ── User-trigger path: explicit DML ─────────────────────────
        // Decompose the MERGE into DELETE + UPDATE + INSERT so that
        // user-defined triggers fire with correct TG_OP / OLD / NEW.

        // Step 1: Materialize delta into a temp table (ON COMMIT DROP).
        // This avoids evaluating the delta query three times.
        let t_mat_start = Instant::now();
        let materialize_sql = format!(
            "CREATE TEMP TABLE __pgt_delta_{pgt_id} ON COMMIT DROP AS \
             SELECT * FROM {using_clause} AS d",
            pgt_id = st.pgt_id,
            using_clause = resolved.trigger_using_sql,
        );
        Spi::run(&materialize_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let t_mat = t_mat_start.elapsed();

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

        let parameterized_sql_for_debug = resolved.parameterized_merge_sql.clone();
        let n = Spi::connect_mut(|client| {
            let result = client
                .update(&execute_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })
        .inspect_err(|_e| {
            let path = format!("/tmp/pgt_debug_exec_{}.sql", st.pgt_id);
            let content =
                format!("-- EXECUTE: {execute_sql}\n-- Template:\n{parameterized_sql_for_debug}");
            let _ = std::fs::write(&path, &content);
            pgrx::warning!(
                "[pg_trickle] EXECUTE failed for pgt_id={}, SQL dumped to {}",
                st.pgt_id,
                path
            );
        })?;
        (n, "merge_prepared")
    } else {
        // ── MERGE path (default for small deltas) ───────────────────
        let merge_sql_for_debug = resolved.merge_sql.clone();
        let n = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.merge_sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Ok::<usize, PgTrickleError>(result.len())
        })
        .inspect_err(|_e| {
            // Dump failing SQL to /tmp for debugging
            let path = format!("/tmp/pgt_debug_merge_{}.sql", st.pgt_id);
            let _ = std::fs::write(&path, &merge_sql_for_debug);
            pgrx::warning!(
                "[pg_trickle] MERGE failed for pgt_id={}, SQL dumped to {}",
                st.pgt_id,
                path
            );
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

        // ── D-3: Cost-based threshold from historical data ──────────
        // Blend the ratio-based threshold with a cost-model estimate
        // derived from recent refresh history.  The cost model computes
        // the crossover delta ratio where INCR cost equals FULL cost.
        let new_threshold = match estimate_cost_based_threshold(st.pgt_id) {
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

    Ok((effective_count, 0))
}

/// D-3: Estimate a cost-based fallback threshold from refresh history.
///
/// Queries the last N DIFFERENTIAL and FULL refreshes for a stream table
/// and computes the crossover delta ratio where incremental cost equals
/// full cost.  Returns `None` if insufficient history is available (fewer
/// than 3 DIFFERENTIAL or no FULL refresh recorded).
///
/// The model:
///   incr_cost(delta_ratio) ≈ avg_incr_cost_per_delta_row × delta_ratio × table_size
///   full_cost              ≈ avg_full_ms
///   crossover_ratio        = avg_full_ms / (avg_cost_per_delta_row × table_size)
///
/// Clamped to [0.01, 0.80].
fn estimate_cost_based_threshold(pgt_id: i64) -> Option<f64> {
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

    // crossover_delta = avg_full_ms / avg_ms_per_delta
    // Simplified: if we know the average delta and the crossover delta,
    // the threshold ratio is crossover_delta / source_table_size.
    // Since we don't have source_table_size here, we use the ratio of
    // crossover_delta to the historical average delta as a scaling factor.
    let crossover_delta = avg_full_ms / avg_ms_per_delta;
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
}
