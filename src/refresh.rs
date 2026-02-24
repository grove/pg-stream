//! Refresh executor — handles full, differential, and reinitialize refreshes.
//!
//! The executor is called by the scheduler for automated refreshes and by
//! `pgstream.refresh_stream_table()` for manual refreshes.
//!
//! ## Delta SQL Caching
//!
//! The differential refresh path caches the delta SQL template and MERGE
//! SQL template per `pgs_id` in thread-local storage. On subsequent
//! refreshes, the cached templates are resolved with actual frontier LSN
//! values — skipping SQL parsing, DVM differentiation, and MERGE SQL
//! string formatting. This eliminates ~45ms of overhead per refresh
//! (29.6ms planning + 15ms generate_delta).

use pgrx::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Instant;

use crate::catalog::{StDependency, StreamTableMeta};
use crate::dag::RefreshMode;
use crate::dvm;
use crate::error::PgStreamError;
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
    /// DELETE + INSERT SQL template (B-3 alternative for large deltas).
    /// Two statements separated by `';'`.
    delete_insert_template: String,
    /// Parameterized MERGE SQL with `$1`, `$2`, … for LSN values (D-2).
    /// Parameter order: for each source OID (in `source_oids` order),
    /// `$2i-1` = prev_lsn, `$2i` = new_lsn.
    parameterized_merge_sql: String,
    /// Source OIDs for LSN placeholder resolution.
    source_oids: Vec<u32>,
    /// Cleanup template with `__PGS_{PREV,NEW}_LSN_{oid}__` tokens.
    cleanup_sql_template: String,

    // ── User-trigger explicit DML templates ──────────────────────────
    // These templates reference `__pgs_delta_{pgs_id}` (a temp table
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
    /// Per-session cache of MERGE SQL templates, keyed by `pgs_id`.
    static MERGE_TEMPLATE_CACHE: RefCell<HashMap<i64, CachedMergeTemplate>> =
        RefCell::new(HashMap::new());
}

// ── D-2: Prepared statement tracking ────────────────────────────────

thread_local! {
    /// Tracks which `pgs_id`s have a SQL `PREPARE`d MERGE statement
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
/// refresh cycle.  Stale rows are harmless because the delta query uses
/// strict LSN range predicates (`lsn > prev AND lsn <= new`).
struct PendingCleanup {
    change_schema: String,
    source_oids: Vec<u32>,
    prev_frontier: Frontier,
    new_frontier: Frontier,
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
fn drain_pending_cleanups() {
    let pending: Vec<PendingCleanup> =
        PENDING_CLEANUP.with(|q| std::mem::take(&mut *q.borrow_mut()));

    if pending.is_empty() {
        return;
    }

    let use_truncate = crate::config::pg_stream_cleanup_use_truncate();

    for job in pending {
        for &oid in &job.source_oids {
            let prev_lsn = job.prev_frontier.get_lsn(oid);
            let new_lsn = job.new_frontier.get_lsn(oid);

            let can_truncate = if use_truncate {
                Spi::get_one::<bool>(&format!(
                    "SELECT NOT EXISTS(\
                       SELECT 1 FROM \"{schema}\".changes_{oid} \
                       WHERE lsn <= '{prev_lsn}'::pg_lsn \
                          OR lsn > '{new_lsn}'::pg_lsn \
                       LIMIT 1\
                     )",
                    schema = job.change_schema,
                ))
                .unwrap_or(Some(false))
                .unwrap_or(false)
            } else {
                false
            };

            if can_truncate {
                if let Err(e) = Spi::run(&format!(
                    "TRUNCATE \"{schema}\".changes_{oid}",
                    schema = job.change_schema,
                )) {
                    pgrx::debug1!("[pg_stream] Deferred cleanup TRUNCATE failed: {}", e);
                }
            } else {
                let delete_sql = format!(
                    "DELETE FROM \"{schema}\".changes_{oid} \
                     WHERE lsn > '{prev_lsn}'::pg_lsn \
                     AND lsn <= '{new_lsn}'::pg_lsn",
                    schema = job.change_schema,
                );
                if let Err(e) = Spi::run(&delete_sql) {
                    pgrx::debug1!("[pg_stream] Deferred cleanup DELETE failed: {}", e);
                }
            }
        }
    }
}

// ── D-1: Planner hint thresholds ────────────────────────────────────

/// Minimum delta rows before disabling nested-loop joins.
const PLANNER_HINT_NESTLOOP_THRESHOLD: i64 = 100;

/// Minimum delta rows before raising `work_mem` for hash joins.
const PLANNER_HINT_WORKMEM_THRESHOLD: i64 = 10_000;

/// Apply `SET LOCAL` planner hints based on the estimated delta size.
///
/// - delta < 100 rows: no hints (let planner optimise for small data)
/// - delta 100–9 999: `SET LOCAL enable_nestloop = off`
/// - delta >= 10 000: also `SET LOCAL work_mem = '<N>MB'`
///
/// `SET LOCAL` is automatically reset at the end of the current transaction,
/// so these hints cannot leak to other queries.
fn apply_planner_hints(estimated_delta: i64) {
    if !crate::config::pg_stream_merge_planner_hints() {
        return;
    }

    if estimated_delta >= PLANNER_HINT_WORKMEM_THRESHOLD {
        // Large delta: disable nested loops AND raise work_mem for hash joins
        if let Err(e) = Spi::run("SET LOCAL enable_nestloop = off") {
            pgrx::debug1!(
                "[pg_stream] D-1: failed to SET LOCAL enable_nestloop: {}",
                e
            );
        }
        let mb = crate::config::pg_stream_merge_work_mem_mb();
        if let Err(e) = Spi::run(&format!("SET LOCAL work_mem = '{mb}MB'")) {
            pgrx::debug1!("[pg_stream] D-1: failed to SET LOCAL work_mem: {}", e);
        }
    } else if estimated_delta >= PLANNER_HINT_NESTLOOP_THRESHOLD {
        // Medium delta: just disable nested loops
        if let Err(e) = Spi::run("SET LOCAL enable_nestloop = off") {
            pgrx::debug1!(
                "[pg_stream] D-1: failed to SET LOCAL enable_nestloop: {}",
                e
            );
        }
    }
}

/// Resolve LSN placeholders in a SQL template with actual frontier values.
fn resolve_lsn_placeholders(
    template: &str,
    source_oids: &[u32],
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
) -> String {
    let mut sql = template.to_string();
    for &oid in source_oids {
        sql = sql.replace(
            &format!("__PGS_PREV_LSN_{oid}__"),
            &prev_frontier.get_lsn(oid),
        );
        sql = sql.replace(
            &format!("__PGS_NEW_LSN_{oid}__"),
            &new_frontier.get_lsn(oid),
        );
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

/// Invalidate the MERGE template cache for a ST (call on DDL changes).
pub fn invalidate_merge_cache(pgs_id: i64) {
    MERGE_TEMPLATE_CACHE.with(|cache| {
        cache.borrow_mut().remove(&pgs_id);
    });
    // D-2: Also deallocate any prepared statement for this ST.
    if PREPARED_MERGE_STMTS.with(|s| s.borrow_mut().remove(&pgs_id)) {
        // Guard SPI call so unit tests (which run outside PG) don't
        // force the linker to resolve pg_sys symbols at load time.
        #[cfg(not(test))]
        {
            let stmt = format!("__pgs_merge_{pgs_id}");
            // Note: DEALLOCATE does not support IF EXISTS in PostgreSQL.
            // Check pg_prepared_statements first to avoid an error.
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

    let schema = &st.pgs_schema;
    let name = &st.pgs_name;

    // Use dummy frontiers — placeholders will be embedded in the template
    let dummy = Frontier::new();

    let delta_result = match dvm::generate_delta_query_cached(
        st.pgs_id,
        &st.defining_query,
        &dummy,
        &dummy,
        schema,
        name,
    ) {
        Ok(r) => r,
        Err(e) => {
            pgrx::log!(
                "pg_stream: cache pre-warm skipped for {}.{}: {}",
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
        dvm::get_delta_sql_template(st.pgs_id).unwrap_or(delta_result.delta_sql);

    // Build the USING clause — skip DISTINCT ON when the delta is already
    // deduplicated (G-M1 optimization for scan-chain queries).
    let using_clause = if delta_result.is_deduplicated {
        format!("({delta_sql_template})")
    } else {
        format!(
            "(SELECT DISTINCT ON (__pgs_row_id) * \
             FROM ({delta_sql_template}) __raw \
             ORDER BY __pgs_row_id, __pgs_action DESC)"
        )
    };

    // B-1: IS DISTINCT FROM guard to skip no-op UPDATEs.
    let is_distinct_clause: String = user_cols
        .iter()
        .map(|c| {
            let qc = format!("\"{}\"", c.replace('"', "\"\""));
            format!("st.{qc} IS DISTINCT FROM d.{qc}")
        })
        .collect::<Vec<_>>()
        .join(" OR ");

    let merge_template = format!(
        "MERGE INTO {quoted_table} AS st \
         USING {using_clause} AS d \
         ON st.__pgs_row_id = d.__pgs_row_id \
         WHEN MATCHED AND d.__pgs_action = 'D' THEN DELETE \
         WHEN MATCHED AND d.__pgs_action = 'I' AND ({is_distinct_clause}) THEN \
           UPDATE SET {update_set_clause} \
         WHEN NOT MATCHED AND d.__pgs_action = 'I' THEN \
           INSERT (__pgs_row_id, {user_col_list}) \
           VALUES (d.__pgs_row_id, {d_user_col_list})",
    );

    // B-3: Build DELETE + INSERT template.
    //
    // WARNING: This path has known correctness issues with aggregate and
    // DISTINCT queries whose delta LEFT JOINs back to the stream table.
    // The delta is evaluated twice (once for DELETE, once for INSERT),
    // and the DELETE modifies the table before the INSERT's evaluation.
    // Only used when pg_stream.merge_strategy = 'delete_insert' is set
    // explicitly.  The "auto" mode always uses MERGE.
    let delete_insert_template = format!(
        "DELETE FROM {quoted_table} \
         WHERE __pgs_row_id IN (\
           SELECT __pgs_row_id FROM {using_clause} AS d\
         );\
         INSERT INTO {quoted_table} (__pgs_row_id, {user_col_list}) \
         SELECT __pgs_row_id, {user_col_list} \
         FROM {using_clause} AS d \
         WHERE d.__pgs_action = 'I'",
    );

    // Build cleanup template.
    let cleanup_schema = crate::config::pg_stream_change_buffer_schema().replace('"', "\"\"");
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
    let trigger_delete_template = format!(
        "DELETE FROM {quoted_table} AS st \
         USING __pgs_delta_{pgs_id} AS d \
         WHERE st.__pgs_row_id = d.__pgs_row_id \
           AND d.__pgs_action = 'D'",
        pgs_id = st.pgs_id,
    );

    let trigger_update_template = format!(
        "UPDATE {quoted_table} AS st \
         SET {update_set_clause} \
         FROM __pgs_delta_{pgs_id} AS d \
         WHERE st.__pgs_row_id = d.__pgs_row_id \
           AND d.__pgs_action = 'I' \
           AND ({is_distinct_clause})",
        pgs_id = st.pgs_id,
    );

    let trigger_insert_template = format!(
        "INSERT INTO {quoted_table} (__pgs_row_id, {user_col_list}) \
         SELECT d.__pgs_row_id, {d_user_col_list} \
         FROM __pgs_delta_{pgs_id} AS d \
         WHERE d.__pgs_action = 'I' \
           AND NOT EXISTS (\
             SELECT 1 FROM {quoted_table} AS st \
             WHERE st.__pgs_row_id = d.__pgs_row_id\
           )",
        pgs_id = st.pgs_id,
    );

    // Cache the MERGE template with LSN placeholder tokens.
    // Each refresh resolves the tokens to concrete LSN values
    // via string substitution, then executes the resolved SQL.
    MERGE_TEMPLATE_CACHE.with(|cache| {
        cache.borrow_mut().insert(
            st.pgs_id,
            CachedMergeTemplate {
                defining_query_hash: query_hash,
                merge_sql_template: merge_template,
                delete_insert_template,
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
        "pg_stream: pre-warmed delta+MERGE cache for {}.{}",
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
    }
}

/// Execute a full refresh: TRUNCATE + INSERT from defining query.
///
/// When user triggers are detected (and the GUC is not `"off"`), they are
/// suppressed during the TRUNCATE + INSERT via `DISABLE TRIGGER USER` /
/// `ENABLE TRIGGER USER`. A `NOTIFY pgstream_refresh` is emitted so
/// listeners know a FULL refresh occurred.
///
/// **Note:** Row-level user triggers do NOT fire correctly for FULL refresh.
/// Users who need per-row trigger semantics should use `REFRESH MODE
/// DIFFERENTIAL`. See PLAN_USER_TRIGGERS_EXPLICIT_DML.md §2.
pub fn execute_full_refresh(st: &StreamTableMeta) -> Result<(i64, i64), PgStreamError> {
    let schema = &st.pgs_schema;
    let name = &st.pgs_name;
    let query = &st.defining_query;

    let quoted_table = format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        name.replace('"', "\"\""),
    );

    // Check for user triggers to suppress during FULL refresh.
    let user_triggers_mode = crate::config::pg_stream_user_triggers();
    let has_triggers = match user_triggers_mode.as_str() {
        "on" => true,
        "off" => false,
        _ => crate::cdc::has_user_triggers(st.pgs_relid)?,
    };

    // Suppress user triggers during TRUNCATE + INSERT to prevent
    // spurious trigger invocations with wrong semantics.
    if has_triggers {
        Spi::run(&format!("ALTER TABLE {quoted_table} DISABLE TRIGGER USER"))
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
    }

    // For aggregate/distinct STs, inject COUNT(*) AS __pgs_count into the
    // defining query so the auxiliary column is populated correctly.
    let effective_query = if st.refresh_mode == crate::dag::RefreshMode::Differential
        && crate::dvm::query_needs_pgs_count(query)
    {
        crate::api::inject_pgs_count(query)
    } else {
        query.clone()
    };

    // Truncate
    Spi::run(&format!("TRUNCATE {quoted_table}"))
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

    // Compute row_id using the same hash formula as the delta query so
    // the MERGE ON clause matches during subsequent differential refreshes.
    // For UNION ALL queries, decompose into per-branch subqueries with
    // child-prefixed row IDs matching diff_union_all's formula.
    let insert_body = if let Some(ua_sql) = crate::dvm::try_union_all_refresh_sql(query) {
        ua_sql
    } else {
        let row_id_expr = crate::dvm::row_id_expr_for_query(query);
        format!("SELECT {row_id_expr} AS __pgs_row_id, sub.* FROM ({effective_query}) sub",)
    };

    let insert_sql = format!("INSERT INTO {quoted_table} {insert_body}");

    let rows_inserted = Spi::connect_mut(|client| {
        let result = client
            .update(&insert_sql, None, &[])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        Ok::<usize, PgStreamError>(result.len())
    })?;

    // Re-enable user triggers and emit NOTIFY so listeners know a FULL
    // refresh occurred.
    if has_triggers {
        Spi::run(&format!("ALTER TABLE {quoted_table} ENABLE TRIGGER USER"))
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        // Escape single quotes in the JSON payload.
        let escaped_name = name.replace('\'', "''");
        let escaped_schema = schema.replace('\'', "''");
        Spi::run(&format!(
            "NOTIFY pgstream_refresh, '{{\"stream_table\": \"{escaped_name}\", \
             \"schema\": \"{escaped_schema}\", \"mode\": \"FULL\", \"rows\": {rows_inserted}}}'"
        ))
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        pgrx::info!(
            "pg_stream: FULL refresh of {}.{} with user triggers suppressed ({} rows). \
             Row-level triggers do NOT fire for FULL refresh; use REFRESH MODE DIFFERENTIAL.",
            schema,
            name,
            rows_inserted,
        );
    }

    Ok((rows_inserted as i64, 0))
}

/// Execute a NO_DATA refresh: just advance the data timestamp.
pub fn execute_no_data_refresh(st: &StreamTableMeta) -> Result<(), PgStreamError> {
    let now = Spi::get_one::<TimestampWithTimeZone>("SELECT now()")
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?
        .ok_or_else(|| PgStreamError::InternalError("now() returned NULL".into()))?;

    StreamTableMeta::update_after_refresh(st.pgs_id, now, 0)?;
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
pub fn execute_differential_refresh(
    st: &StreamTableMeta,
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
) -> Result<(i64, i64), PgStreamError> {
    let schema = &st.pgs_schema;
    let name = &st.pgs_name;

    // ── Short-circuit: skip the entire pipeline if no changes exist ──────
    let change_schema = crate::config::pg_stream_change_buffer_schema().replace('"', "\"\"");
    let catalog_source_oids: Vec<u32> = StDependency::get_for_st(st.pgs_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|dep| dep.source_type == "TABLE")
        .map(|dep| dep.source_relid.to_u32())
        .collect();

    // C-1: Drain any deferred cleanups from the previous refresh cycle.
    // This runs before the decision query so stale rows are removed
    // before we check for new changes.
    drain_pending_cleanups();

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
        pgrx::info!(
            "[pg_stream] Source table TRUNCATE detected — falling back to FULL refresh for {}.{}",
            schema,
            name,
        );
        return execute_full_refresh(st);
    }

    // ── P2: Capped-count threshold check (only when changes exist) ───────
    // Now that we know changes exist, check whether the change volume
    // exceeds the adaptive fallback threshold.  This heavier query is
    // skipped entirely for the no-data case (handled above).
    //
    // Session 7: per-ST adaptive threshold takes priority over global GUC.
    let global_ratio = crate::config::pg_stream_differential_max_change_ratio();
    let max_ratio = st.auto_threshold.unwrap_or(global_ratio);
    let mut should_fallback = false;
    let mut total_change_count: i64 = 0;
    let mut _total_table_size: i64 = 0;

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
             ) __pgs_capped) cnt",
        );

        let (table_size, change_count) = Spi::connect(|client| {
            let row = client
                .select(&sql, None, &[])
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .first();
            let ts: i64 = row.get::<i64>(1).unwrap_or(Some(1000)).unwrap_or(1000);
            let cc: i64 = row.get::<i64>(2).unwrap_or(Some(0)).unwrap_or(0);
            Ok::<(i64, i64), PgStreamError>((ts, cc))
        })?;

        let threshold_rows = if max_ratio > 0.0 {
            ((table_size as f64) * max_ratio).ceil() as i64
        } else {
            i64::MAX / 2
        };

        total_change_count += change_count;
        _total_table_size += table_size;

        if change_count > threshold_rows {
            should_fallback = true;
            break; // No need to check remaining sources
        }
    }

    if should_fallback {
        pgrx::info!(
            "[pg_stream] Adaptive fallback: change ratio exceeds threshold {:.0}% — using FULL refresh",
            max_ratio * 100.0,
        );
        let t_full_start = Instant::now();
        let result = execute_full_refresh(st);
        let full_ms = t_full_start.elapsed().as_secs_f64() * 1000.0;
        // Record FULL timing for future threshold auto-tuning.
        if let Err(e) = StreamTableMeta::update_adaptive_threshold(
            st.pgs_id,
            st.auto_threshold, // keep current threshold
            Some(full_ms),
        ) {
            pgrx::debug1!("[pg_stream] Failed to update last_full_ms: {}", e);
        }
        return result;
    }

    let t_decision = t_decision_start.elapsed();
    let t0 = Instant::now();

    // ── Try the MERGE template cache first ──────────────────────────
    let query_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        st.defining_query.hash(&mut hasher);
        hasher.finish()
    };

    let cached = MERGE_TEMPLATE_CACHE.with(|cache| {
        let map = cache.borrow();
        map.get(&st.pgs_id)
            .filter(|entry| entry.defining_query_hash == query_hash)
            .cloned()
    });

    let was_cache_hit = cached.is_some();

    /// Resolved SQL pair: MERGE template + DELETE+INSERT template,
    /// plus D-2 prepared-statement materials and user-trigger DML.
    struct ResolvedSql {
        merge_sql: String,
        delete_insert_sql: String,
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
        // Substitute __PGS_PREV/NEW_LSN_{oid}__ tokens with actual values.
        // Each execution gets a fresh plan with accurate LSN selectivity
        // estimates, avoiding the PREPARE/EXECUTE custom-plan penalty.
        ResolvedSql {
            merge_sql: resolve_lsn_placeholders(
                &entry.merge_sql_template,
                &entry.source_oids,
                prev_frontier,
                new_frontier,
            ),
            delete_insert_sql: resolve_lsn_placeholders(
                &entry.delete_insert_template,
                &entry.source_oids,
                prev_frontier,
                new_frontier,
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
            ),
        }
    } else {
        // ── Cache miss: full pipeline + PREPARE + cache ──────────────
        let delta_result = dvm::generate_delta_query_cached(
            st.pgs_id,
            &st.defining_query,
            prev_frontier,
            new_frontier,
            schema,
            name,
        )?;

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
        let cleanup_schema = crate::config::pg_stream_change_buffer_schema().replace('"', "\"\"");
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
        let delta_sql_template =
            dvm::get_delta_sql_template(st.pgs_id).unwrap_or(delta_sql.clone());

        // Build template USING clause — skip DISTINCT ON when deduplicated (G-M1)
        let template_using = if is_dedup {
            format!("({delta_sql_template})")
        } else {
            format!(
                "(SELECT DISTINCT ON (__pgs_row_id) * \
                 FROM ({delta_sql_template}) __raw \
                 ORDER BY __pgs_row_id, __pgs_action DESC)"
            )
        };

        // ── B-1: IS DISTINCT FROM guard to skip no-op UPDATEs ───────
        // When a group's aggregate value hasn't actually changed, the
        // MERGE would still perform an UPDATE (writing an identical
        // tuple).  Adding an IS DISTINCT FROM check on the WHEN MATCHED
        // clause lets PostgreSQL skip the heap write entirely.
        let is_distinct_clause: String = user_cols
            .iter()
            .map(|c| {
                let qc = format!("\"{}\"", c.replace('"', "\"\""));
                format!("st.{qc} IS DISTINCT FROM d.{qc}")
            })
            .collect::<Vec<_>>()
            .join(" OR ");

        let merge_template = format!(
            "MERGE INTO {quoted_table} AS st \
             USING {template_using} AS d \
             ON st.__pgs_row_id = d.__pgs_row_id \
             WHEN MATCHED AND d.__pgs_action = 'D' THEN DELETE \
             WHEN MATCHED AND d.__pgs_action = 'I' AND ({is_distinct_clause}) THEN \
               UPDATE SET {update_set_clause} \
             WHEN NOT MATCHED AND d.__pgs_action = 'I' THEN \
               INSERT (__pgs_row_id, {user_col_list}) \
               VALUES (d.__pgs_row_id, {d_user_col_list})",
        );

        // ── B-3: DELETE + INSERT template (large-delta alternative) ──
        // For large deltas, DELETE matching rows then INSERT updated
        // values is cheaper than MERGE's branching + visibility checks.
        let delete_insert_template = format!(
            "DELETE FROM {quoted_table} \
             WHERE __pgs_row_id IN (\
               SELECT __pgs_row_id FROM {template_using} AS d\
             );\
             INSERT INTO {quoted_table} (__pgs_row_id, {user_col_list}) \
             SELECT __pgs_row_id, {user_col_list} \
             FROM {template_using} AS d \
             WHERE d.__pgs_action = 'I'",
        );

        // ── D-2: Build parameterized MERGE SQL for PREPARE ─────────
        let parameterized_merge_sql = parameterize_lsn_template(&merge_template, &source_oids);

        // ── User-trigger explicit DML templates ──────────────────────
        let trigger_delete_template = format!(
            "DELETE FROM {quoted_table} AS st \
             USING __pgs_delta_{pgs_id} AS d \
             WHERE st.__pgs_row_id = d.__pgs_row_id \
               AND d.__pgs_action = 'D'",
            pgs_id = st.pgs_id,
        );

        let trigger_update_template = format!(
            "UPDATE {quoted_table} AS st \
             SET {update_set_clause} \
             FROM __pgs_delta_{pgs_id} AS d \
             WHERE st.__pgs_row_id = d.__pgs_row_id \
               AND d.__pgs_action = 'I' \
               AND ({is_distinct_clause})",
            pgs_id = st.pgs_id,
        );

        let trigger_insert_template = format!(
            "INSERT INTO {quoted_table} (__pgs_row_id, {user_col_list}) \
             SELECT d.__pgs_row_id, {d_user_col_list} \
             FROM __pgs_delta_{pgs_id} AS d \
             WHERE d.__pgs_action = 'I' \
               AND NOT EXISTS (\
                 SELECT 1 FROM {quoted_table} AS st \
                 WHERE st.__pgs_row_id = d.__pgs_row_id\
               )",
            pgs_id = st.pgs_id,
        );

        // Store templates in the cache for subsequent refreshes.
        MERGE_TEMPLATE_CACHE.with(|cache| {
            cache.borrow_mut().insert(
                st.pgs_id,
                CachedMergeTemplate {
                    defining_query_hash: query_hash,
                    merge_sql_template: merge_template.clone(),
                    delete_insert_template: delete_insert_template.clone(),
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

        // Resolve LSN placeholders for this execution.
        ResolvedSql {
            merge_sql: resolve_lsn_placeholders(
                &merge_template,
                &source_oids,
                prev_frontier,
                new_frontier,
            ),
            delete_insert_sql: resolve_lsn_placeholders(
                &delete_insert_template,
                &source_oids,
                prev_frontier,
                new_frontier,
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
            ),
        }
    };

    let t1 = Instant::now();

    // ── D-1: Conditional planner hints based on delta size ───────────
    // Large deltas benefit from hash joins over nested loops. Apply
    // SET LOCAL hints that are automatically reset at transaction end.
    apply_planner_hints(total_change_count);

    // ── User-trigger detection ───────────────────────────────────────
    // Determine whether to use the explicit DML path based on the GUC
    // and the presence of user-defined row-level triggers on the ST.
    let user_triggers_mode = crate::config::pg_stream_user_triggers();
    let use_explicit_dml = match user_triggers_mode.as_str() {
        "on" => true,
        "off" => false,
        _ => {
            // "auto": detect user triggers
            crate::cdc::has_user_triggers(st.pgs_relid)?
        }
    };

    // When user_triggers = 'off' but there ARE user triggers on the ST,
    // suppress them during the MERGE to prevent spurious firing.
    let suppress_triggers =
        user_triggers_mode.as_str() == "off" && crate::cdc::has_user_triggers(st.pgs_relid)?;
    if suppress_triggers {
        let quoted_table = format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\""),
        );
        Spi::run(&format!("ALTER TABLE {quoted_table} DISABLE TRIGGER USER"))
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
    }

    // ── B-3: Strategy selection ──────────────────────────────────────
    // Choose between MERGE and DELETE+INSERT based on the GUC setting.
    //
    // The "auto" strategy defaults to MERGE for correctness.
    // DELETE+INSERT has a known correctness issue: aggregate and DISTINCT
    // delta queries LEFT JOIN back to the stream table to read auxiliary
    // columns (__pgs_count). Evaluating the delta query twice (once for
    // DELETE, once for INSERT) causes the INSERT to see a modified table
    // and compute wrong results.
    //
    // Users may explicitly set pg_stream.merge_strategy = 'delete_insert'
    // for workloads where the defining query is a simple scan/join with
    // no aggregates or DISTINCT, after validating correctness.
    let strategy = crate::config::pg_stream_merge_strategy();
    let use_delete_insert = strategy.as_str() == "delete_insert";

    // ── D-2: Prepared-statement flag ─────────────────────────────────
    let use_prepared =
        crate::config::pg_stream_use_prepared_statements() && !use_delete_insert && was_cache_hit;

    let (merge_count, strategy_label) = if use_explicit_dml {
        // ── User-trigger path: explicit DML ─────────────────────────
        // Decompose the MERGE into DELETE + UPDATE + INSERT so that
        // user-defined triggers fire with correct TG_OP / OLD / NEW.

        // Step 1: Materialize delta into a temp table (ON COMMIT DROP).
        // This avoids evaluating the delta query three times.
        let t_mat_start = Instant::now();
        let materialize_sql = format!(
            "CREATE TEMP TABLE __pgs_delta_{pgs_id} ON COMMIT DROP AS \
             SELECT * FROM {using_clause} AS d",
            pgs_id = st.pgs_id,
            using_clause = resolved.trigger_using_sql,
        );
        Spi::run(&materialize_sql).map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        let t_mat = t_mat_start.elapsed();

        // Step 2: DELETE removed rows (AFTER DELETE triggers fire)
        let t_del_start = Instant::now();
        let del_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_delete_sql, None, &[])
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
            Ok::<usize, PgStreamError>(result.len())
        })?;
        let t_del = t_del_start.elapsed();

        // Step 3: UPDATE changed existing rows (AFTER UPDATE triggers fire)
        // The IS DISTINCT FROM guard (B-1) prevents no-op UPDATE triggers.
        let t_upd_start = Instant::now();
        let upd_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_update_sql, None, &[])
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
            Ok::<usize, PgStreamError>(result.len())
        })?;
        let t_upd = t_upd_start.elapsed();

        // Step 4: INSERT genuinely new rows (AFTER INSERT triggers fire)
        let t_ins_start = Instant::now();
        let ins_count = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.trigger_insert_sql, None, &[])
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
            Ok::<usize, PgStreamError>(result.len())
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
    } else if use_delete_insert {
        // ── DELETE + INSERT path ─────────────────────────────────────
        let stmts: Vec<&str> = resolved
            .delete_insert_sql
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        let mut total = 0usize;
        for stmt in stmts {
            let n = Spi::connect_mut(|client| {
                let result = client
                    .update(stmt, None, &[])
                    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
                Ok::<usize, PgStreamError>(result.len())
            })?;
            total += n;
        }
        (total, "delete_insert")
    } else if use_prepared {
        // ── D-2: MERGE via prepared statement ────────────────────────
        // After ~5 executions PostgreSQL switches from custom to generic
        // plan, saving ~1-2ms of parse/plan overhead per refresh cycle.
        let stmt_name = format!("__pgs_merge_{}", st.pgs_id);

        let already_prepared = PREPARED_MERGE_STMTS.with(|s| s.borrow().contains(&st.pgs_id));

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
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

            PREPARED_MERGE_STMTS.with(|s| {
                s.borrow_mut().insert(st.pgs_id);
            });
        }

        let params = build_execute_params(&resolved.source_oids, prev_frontier, new_frontier);
        let execute_sql = format!("EXECUTE {stmt_name}({params})");

        let n = Spi::connect_mut(|client| {
            let result = client
                .update(&execute_sql, None, &[])
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
            Ok::<usize, PgStreamError>(result.len())
        })?;
        (n, "merge_prepared")
    } else {
        // ── MERGE path (default for small deltas) ───────────────────
        let n = Spi::connect_mut(|client| {
            let result = client
                .update(&resolved.merge_sql, None, &[])
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
            Ok::<usize, PgStreamError>(result.len())
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
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
    }

    let t2 = Instant::now();

    // ── C-1: Defer cleanup to next refresh cycle ────────────────────
    // Instead of deleting consumed change-buffer rows synchronously
    // (which costs 0.5–7ms), we enqueue the cleanup work and execute it
    // at the start of the NEXT refresh.  The LSN-range predicates in the
    // delta query ensure stale rows are never re-consumed.
    let cleanup_source_oids = MERGE_TEMPLATE_CACHE.with(|cache| {
        let map = cache.borrow();
        map.get(&st.pgs_id)
            .map(|entry| entry.source_oids.clone())
            .unwrap_or_default()
    });

    if !cleanup_source_oids.is_empty() {
        PENDING_CLEANUP.with(|q| {
            q.borrow_mut().push(PendingCleanup {
                change_schema: change_schema.clone(),
                source_oids: cleanup_source_oids,
                prev_frontier: prev_frontier.clone(),
                new_frontier: new_frontier.clone(),
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

    let hint_tier = if !crate::config::pg_stream_merge_planner_hints() {
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
        let new_threshold = compute_adaptive_threshold(current_threshold, incr_total_ms, last_full);
        if (new_threshold - current_threshold).abs() > 0.001 {
            pgrx::debug1!(
                "[pg_stream] Adaptive threshold: INCR={:.1}ms vs FULL={:.1}ms (ratio={:.2}), threshold {:.3} → {:.3}",
                incr_total_ms,
                last_full,
                incr_total_ms / last_full,
                current_threshold,
                new_threshold,
            );
        }
        if let Err(e) =
            StreamTableMeta::update_adaptive_threshold(st.pgs_id, Some(new_threshold), None)
        {
            pgrx::debug1!("[pg_stream] Failed to update adaptive threshold: {}", e);
        }
    }

    Ok((merge_count as i64, 0))
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
pub fn execute_reinitialize_refresh(st: &StreamTableMeta) -> Result<(i64, i64), PgStreamError> {
    // Same as full refresh but also clears the reinit flag
    let result = execute_full_refresh(st)?;

    // Clear reinit flag
    Spi::run(&format!(
        "UPDATE pgstream.pgs_stream_tables SET needs_reinit = FALSE WHERE pgs_id = {}",
        st.pgs_id,
    ))
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

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
            pgs_id: 1,
            pgs_relid: pg_sys::Oid::from(0u32),
            pgs_name: "test_st".to_string(),
            pgs_schema: "public".to_string(),
            defining_query: "SELECT 1".to_string(),
            schedule: None,
            refresh_mode,
            status: StStatus::Active,
            is_populated: true,
            data_timestamp: None,
            consecutive_errors: 0,
            needs_reinit,
            auto_threshold: None,
            last_full_ms: None,
            frontier: None,
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

    #[test]
    fn test_resolve_lsn_single_oid() {
        let mut prev = Frontier::new();
        prev.set_source(42, "0/1000".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(42, "0/2000".to_string(), "ts".to_string());

        let template = "DELETE FROM changes_42 WHERE lsn > '__PGS_PREV_LSN_42__' AND lsn <= '__PGS_NEW_LSN_42__'";
        let resolved = resolve_lsn_placeholders(template, &[42], &prev, &new_f);
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
        let resolved = resolve_lsn_placeholders(template, &[10, 20], &prev, &new_f);
        assert_eq!(resolved, "0/AA 0/CC 0/BB 0/DD");
    }

    #[test]
    fn test_resolve_lsn_no_placeholders() {
        let prev = Frontier::new();
        let new_f = Frontier::new();
        let resolved = resolve_lsn_placeholders("SELECT 1", &[], &prev, &new_f);
        assert_eq!(resolved, "SELECT 1");
    }

    #[test]
    fn test_resolve_lsn_missing_oid_defaults() {
        let prev = Frontier::new();
        let new_f = Frontier::new();
        let resolved = resolve_lsn_placeholders("__PGS_PREV_LSN_999__", &[999], &prev, &new_f);
        assert_eq!(resolved, "0/0");
    }

    #[test]
    fn test_resolve_lsn_preserves_other_text() {
        let mut prev = Frontier::new();
        prev.set_source(1, "0/10".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(1, "0/20".to_string(), "ts".to_string());

        let template = "SELECT * FROM t WHERE x = 42 AND lsn > '__PGS_PREV_LSN_1__'";
        let resolved = resolve_lsn_placeholders(template, &[1], &prev, &new_f);
        assert!(resolved.contains("SELECT * FROM t WHERE x = 42"));
        assert!(resolved.contains("0/10"));
    }

    #[test]
    fn test_resolve_lsn_placeholders_single_source() {
        let template = "DELETE FROM changes_12345 WHERE lsn > '__PGS_PREV_LSN_12345__'::pg_lsn AND lsn <= '__PGS_NEW_LSN_12345__'::pg_lsn";
        let prev = make_frontier(&[(12345, "0/1000")]);
        let new = make_frontier(&[(12345, "0/2000")]);
        let result = resolve_lsn_placeholders(template, &[12345], &prev, &new);
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
        let result = resolve_lsn_placeholders(template, &[100, 200], &prev, &new);
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
        let result = resolve_lsn_placeholders(template, &[999], &prev, &new);
        assert_eq!(result, "lsn > '0/0'::pg_lsn");
    }

    #[test]
    fn test_resolve_lsn_placeholders_empty_template() {
        let result = resolve_lsn_placeholders("", &[1], &Frontier::new(), &Frontier::new());
        assert_eq!(result, "");
    }

    #[test]
    fn test_resolve_lsn_placeholders_no_sources() {
        let template = "SELECT 1";
        let result = resolve_lsn_placeholders(template, &[], &Frontier::new(), &Frontier::new());
        assert_eq!(result, "SELECT 1");
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
                    delete_insert_template: "DELETE FROM t ...; INSERT INTO t ...".to_string(),
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
                    delete_insert_template: String::new(),
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
}
