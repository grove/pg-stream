//! User-facing SQL API functions for pgstream.
//!
//! All functions are exposed in the `pg_stream` schema and provide the primary
//! interface for creating, altering, dropping, and refreshing stream tables.

use pgrx::prelude::*;
use std::time::Instant;

use crate::catalog::{CdcMode, StDependency, StreamTableMeta};
use crate::cdc;
use crate::config;
use crate::dag::{DagNode, NodeId, RefreshMode, StDag, StStatus};
use crate::error::PgStreamError;
use crate::refresh;
use crate::shmem;
use crate::version;
use crate::wal_decoder;

/// Create a new stream table.
///
/// # Arguments
/// - `name`: Schema-qualified name (`'schema.table'`) or unqualified (`'table'`).
/// - `query`: The defining SELECT query.
/// - `schedule`: Desired maximum schedule. `NULL` for CALCULATED.
/// - `refresh_mode`: `'FULL'` or `'DIFFERENTIAL'`.
/// - `initialize`: Whether to populate the table immediately.
#[pg_extern(schema = "pgstream")]
fn create_stream_table(
    name: &str,
    query: &str,
    schedule: default!(Option<&str>, "'1m'"),
    refresh_mode: default!(&str, "'DIFFERENTIAL'"),
    initialize: default!(bool, true),
) {
    let result = create_stream_table_impl(name, query, schedule, refresh_mode, initialize);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn create_stream_table_impl(
    name: &str,
    query: &str,
    schedule: Option<&str>,
    refresh_mode_str: &str,
    initialize: bool,
) -> Result<(), PgStreamError> {
    let refresh_mode = RefreshMode::from_str(refresh_mode_str)?;

    // Parse schema.name
    let (schema, table_name) = parse_qualified_name(name)?;

    // Parse and validate schedule — accepts either a Prometheus-style
    // duration string (e.g., '5m', '1h30m') or a cron expression
    // (e.g., '*/5 * * * *', '@hourly').
    let schedule_str = match schedule {
        Some(s) => {
            let _schedule = parse_schedule(s)?;
            Some(s.trim().to_string())
        }
        None => None,
    };

    // ── DISTINCT ON auto-rewrite ───────────────────────────────────
    // DISTINCT ON (e1, e2) is rewritten to a ROW_NUMBER() window function
    // subquery before further parsing. The original query string is replaced
    // so all downstream validation and parsing sees the rewritten form.
    let query = &crate::dvm::rewrite_distinct_on(query)?;

    // Validate the defining query by running LIMIT 0
    let columns = validate_defining_query(query)?;

    // Reject LIMIT / OFFSET in the defining query — stream tables
    // materialize the full result set regardless of refresh mode.
    crate::dvm::reject_limit_offset(query)?;

    // Reject constructs that are unsupported regardless of refresh mode
    // (NATURAL JOIN, subquery expressions like EXISTS/IN).
    // This is a lightweight check that inspects the raw parse tree without
    // doing full DVM tree construction.
    crate::dvm::reject_unsupported_constructs(query)?;

    // For DIFFERENTIAL mode, run the full DVM parser to catch unsupported
    // aggregates, FILTER clauses, etc. that are specifically problematic
    // for incremental view maintenance. FULL mode skips this since it
    // just truncates and reloads.
    let parsed_tree = if refresh_mode == RefreshMode::Differential {
        Some(crate::dvm::parse_defining_query_full(query)?)
    } else {
        None
    };

    // ── Volatility check ────────────────────────────────────────────
    // Volatile functions break delta computation in DIFFERENTIAL mode.
    // Stable functions are allowed with a warning.
    if let Some(ref pr) = parsed_tree {
        let vol = crate::dvm::tree_worst_volatility_with_registry(pr)?;
        match vol {
            'v' => {
                return Err(PgStreamError::UnsupportedOperator(
                    "Defining query contains volatile functions (e.g., random(), \
                     clock_timestamp()). Volatile functions are not supported in \
                     DIFFERENTIAL mode because they produce different values on \
                     each evaluation, breaking delta computation. \
                     Use FULL refresh mode instead, or replace with a \
                     deterministic alternative."
                        .into(),
                ));
            }
            's' => {
                pgrx::warning!(
                    "Defining query contains stable functions (e.g., now(), \
                     current_timestamp). These return the same value within a \
                     single refresh but may shift between refreshes. \
                     Delta computation is correct within each refresh cycle."
                );
            }
            _ => {} // 'i' (immutable) — no action
        }
    } else if refresh_mode == RefreshMode::Full {
        // FULL mode: warn if volatile functions are present.
        // We still validate the query by parsing it (LIMIT 0 already ran),
        // but we don't have a full OpTree. Do a lightweight SPI check on
        // any function names we can extract. This is best-effort — the
        // user already chose FULL mode, so we just warn.
        // (Skip for now — FULL mode re-evaluates everything from scratch,
        // so volatile is expected-but-surprising behavior.)
    }

    // Detect if the query has aggregate/distinct (needs __pgs_count auxiliary column).
    let needs_count = parsed_tree
        .as_ref()
        .is_some_and(|pr| pr.tree.needs_pgs_count());

    // Note: recursive CTEs (WITH RECURSIVE) are allowed in both FULL and
    // DIFFERENTIAL modes. For DIFFERENTIAL, the DVM engine uses a
    // recomputation diff strategy that re-executes the query and diffs
    // the result against the current storage.

    // Check for duplicate
    if StreamTableMeta::get_by_name(&schema, &table_name).is_ok() {
        return Err(PgStreamError::AlreadyExists(format!(
            "{}.{}",
            schema, table_name
        )));
    }

    // Extract source dependencies from the query
    let source_relids = extract_source_relations(query)?;

    // Cycle detection
    check_for_cycles(&source_relids)?;

    // ── Phase 1: DDL and DML (writes) ──

    // Get the change buffer schema for CDC setup
    let change_schema = config::pg_stream_change_buffer_schema();

    // Create the underlying storage table
    let storage_ddl = build_create_table_sql(&schema, &table_name, &columns, needs_count);
    Spi::run(&storage_ddl)
        .map_err(|e| PgStreamError::SpiError(format!("Failed to create storage table: {}", e)))?;

    // Get the OID of the newly created table
    let pgs_relid = get_table_oid(&schema, &table_name)?;

    // Create unique index on __pgs_row_id
    let index_sql = format!(
        "CREATE UNIQUE INDEX ON {}.{} (__pgs_row_id)",
        quote_identifier(&schema),
        quote_identifier(&table_name),
    );
    Spi::run(&index_sql)
        .map_err(|e| PgStreamError::SpiError(format!("Failed to create row_id index: {}", e)))?;

    // U1/U2: Auto-create composite index on GROUP BY columns for aggregate
    // queries. This accelerates the LEFT JOIN in the agg_merge CTE during
    // differential refreshes by allowing index lookups instead of seq scans.
    if refresh_mode == RefreshMode::Differential
        && let Some(ref pr) = parsed_tree
        && let Some(group_cols) = pr.tree.group_by_columns()
        && !group_cols.is_empty()
    {
        let quoted_cols: Vec<String> = group_cols
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect();
        let group_index_sql = format!(
            "CREATE INDEX ON {}.{} ({})",
            quote_identifier(&schema),
            quote_identifier(&table_name),
            quoted_cols.join(", "),
        );
        Spi::run(&group_index_sql).map_err(|e| {
            PgStreamError::SpiError(format!("Failed to create group-by index: {}", e))
        })?;
    }

    // Insert catalog entry
    let pgs_id = StreamTableMeta::insert(
        pgs_relid,
        &table_name,
        &schema,
        query,
        schedule_str,
        refresh_mode,
    )?;

    // Build per-source column usage map from the parsed OpTree so that
    // `detect_schema_change_kind()` can accurately classify DDL events
    // (benign vs column-affecting) instead of conservatively reinitializing.
    let columns_used_map = parsed_tree
        .as_ref()
        .map(|pr| pr.source_columns_used())
        .unwrap_or_default();

    // Insert dependency edges
    for (source_oid, source_type) in &source_relids {
        let cols = columns_used_map.get(&source_oid.to_u32()).cloned();
        StDependency::insert(pgs_id, *source_oid, source_type, cols)?;
    }

    // ── Phase 2: CDC setup (change buffer tables + triggers + tracking) ──
    for (source_oid, source_type) in &source_relids {
        if source_type == "TABLE" {
            setup_cdc_for_source(*source_oid, pgs_id, &change_schema)?;
        }
    }

    // Initialize if requested
    if initialize {
        let t_init = Instant::now();
        initialize_dt(&schema, &table_name, query, pgs_id, &columns, needs_count)?;
        let init_ms = t_init.elapsed().as_secs_f64() * 1000.0;

        // Record initial full materialization time so the adaptive
        // threshold auto-tuner has a FULL baseline from the very first
        // differential refresh.  Without this, `last_full_ms` stays NULL
        // and the auto-tuner never activates for STs whose change rate
        // stays below the fallback threshold.
        if refresh_mode == RefreshMode::Differential
            && let Err(e) = StreamTableMeta::update_adaptive_threshold(pgs_id, None, Some(init_ms))
        {
            pgrx::debug1!("[pg_stream] Failed to record initial last_full_ms: {}", e);
        }
    }

    // Pre-warm delta SQL + MERGE template cache for DIFFERENTIAL mode,
    // so the first refresh avoids the cold-start parsing penalty.
    if refresh_mode == RefreshMode::Differential && initialize {
        let st = StreamTableMeta::get_by_name(&schema, &table_name)?;
        refresh::prewarm_merge_cache(&st);
    }

    // Signal scheduler to rebuild DAG
    shmem::signal_dag_rebuild();

    pgrx::info!(
        "Stream table {}.{} created (pgs_id={}, mode={}, initialized={})",
        schema,
        table_name,
        pgs_id,
        refresh_mode.as_str(),
        initialize
    );

    Ok(())
}

/// Alter properties of an existing stream table.
#[pg_extern(schema = "pgstream")]
fn alter_stream_table(
    name: &str,
    schedule: default!(Option<&str>, "NULL"),
    refresh_mode: default!(Option<&str>, "NULL"),
    status: default!(Option<&str>, "NULL"),
) {
    let result = alter_stream_table_impl(name, schedule, refresh_mode, status);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn alter_stream_table_impl(
    name: &str,
    schedule: Option<&str>,
    refresh_mode: Option<&str>,
    status: Option<&str>,
) -> Result<(), PgStreamError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    if let Some(val) = schedule {
        let _schedule = parse_schedule(val)?;
        let trimmed = val.trim();
        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables SET schedule = $1, updated_at = now() WHERE pgs_id = $2",
            &[trimmed.into(), st.pgs_id.into()],
        )
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
    }

    if let Some(mode_str) = refresh_mode {
        let _mode = RefreshMode::from_str(mode_str)?;

        // Note: recursive CTEs are now allowed in DIFFERENTIAL mode
        // (the DVM engine uses a recomputation diff strategy).

        Spi::run_with_args(
            "UPDATE pgstream.pgs_stream_tables SET refresh_mode = $1, updated_at = now() WHERE pgs_id = $2",
            &[mode_str.to_uppercase().into(), st.pgs_id.into()],
        )
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
    }

    if let Some(status_str) = status {
        let new_status = StStatus::from_str(&status_str.to_uppercase())?;
        StreamTableMeta::update_status(st.pgs_id, new_status)?;
        if new_status == StStatus::Active {
            // Reset errors when resuming
            Spi::run_with_args(
                "UPDATE pgstream.pgs_stream_tables SET consecutive_errors = 0, updated_at = now() WHERE pgs_id = $1",
                &[st.pgs_id.into()],
            )
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        }
    }

    shmem::signal_dag_rebuild();
    Ok(())
}

/// Drop a stream table, removing the storage table and all catalog entries.
#[pg_extern(schema = "pgstream")]
fn drop_stream_table(name: &str) {
    let result = drop_stream_table_impl(name);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn drop_stream_table_impl(name: &str) -> Result<(), PgStreamError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    // Get dependencies before deleting catalog entries
    let deps = StDependency::get_for_st(st.pgs_id).unwrap_or_default();

    // Drop the storage table
    let drop_sql = format!(
        "DROP TABLE IF EXISTS {}.{} CASCADE",
        quote_identifier(&schema),
        quote_identifier(&table_name),
    );
    Spi::run(&drop_sql)
        .map_err(|e| PgStreamError::SpiError(format!("Failed to drop storage table: {}", e)))?;

    // Delete catalog entries (cascade handles pgs_dependencies)
    StreamTableMeta::delete(st.pgs_id)?;

    // Clean up CDC resources (triggers, WAL slots, publications) for
    // sources no longer tracked by any ST.
    for dep in &deps {
        if dep.source_type == "TABLE" {
            cleanup_cdc_for_source(dep.source_relid, dep.cdc_mode)?;
        }
    }

    // Signal scheduler
    shmem::signal_dag_rebuild();

    pgrx::info!(
        "Stream table {}.{} dropped (pgs_id={})",
        schema,
        table_name,
        st.pgs_id
    );
    Ok(())
}

/// Manually trigger a synchronous refresh of a stream table.
#[pg_extern(schema = "pgstream")]
fn refresh_stream_table(name: &str) {
    let result = refresh_stream_table_impl(name);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn refresh_stream_table_impl(name: &str) -> Result<(), PgStreamError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    // Phase 10: Check if ST is suspended — refuse manual refresh
    if st.status == StStatus::Suspended {
        return Err(PgStreamError::InvalidArgument(format!(
            "stream table {}.{} is suspended; use pgstream.resume_stream_table() first",
            schema, table_name,
        )));
    }

    // ── Fast no-op exit for DIFFERENTIAL mode ────────────────────────
    // Before acquiring the advisory lock, check if any source table has
    // pending changes. If not, skip the entire refresh pipeline (lock,
    // frontier computation, DVM, cleanup) — just update the timestamp.
    //
    // G-N3 optimization: source OIDs are fetched once and reused.
    let source_oids = get_source_oids_for_manual_refresh(st.pgs_id)?;

    // Phase 10: Advisory lock to prevent concurrent refresh
    let got_lock =
        Spi::get_one_with_args::<bool>("SELECT pg_try_advisory_lock($1)", &[st.pgs_id.into()])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?
            .unwrap_or(false);

    if !got_lock {
        return Err(PgStreamError::RefreshSkipped(format!(
            "{}.{} — another refresh is already in progress",
            schema, table_name,
        )));
    }

    // Ensure advisory lock is released even on error
    let result = execute_manual_refresh(&st, &schema, &table_name, &source_oids);

    // Release the lock
    let _ = Spi::get_one_with_args::<bool>("SELECT pg_advisory_unlock($1)", &[st.pgs_id.into()]);

    result
}

/// Inner function for manual refresh, called while advisory lock is held.
///
/// Dispatches to FULL or DIFFERENTIAL depending on the ST's refresh mode.
/// `source_oids` are pre-fetched to avoid redundant SPI calls (G-N3).
fn execute_manual_refresh(
    st: &StreamTableMeta,
    schema: &str,
    table_name: &str,
    source_oids: &[pg_sys::Oid],
) -> Result<(), PgStreamError> {
    match st.refresh_mode {
        RefreshMode::Full => execute_manual_full_refresh(st, schema, table_name, source_oids),
        RefreshMode::Differential => {
            execute_manual_differential_refresh(st, schema, table_name, source_oids)
        }
    }
}

/// Execute a FULL manual refresh: truncate + repopulate from the defining query.
///
/// When user triggers are detected (and the GUC is not `"off"`), they are
/// suppressed during the TRUNCATE + INSERT via `DISABLE TRIGGER USER` /
/// `ENABLE TRIGGER USER`. A `NOTIFY pgstream_refresh` is emitted so
/// listeners know a FULL refresh occurred.
fn execute_manual_full_refresh(
    st: &StreamTableMeta,
    schema: &str,
    table_name: &str,
    source_oids: &[pg_sys::Oid],
) -> Result<(), PgStreamError> {
    let quoted_table = format!(
        "{}.{}",
        quote_identifier(schema),
        quote_identifier(table_name),
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

    let truncate_sql = format!("TRUNCATE {quoted_table}");
    Spi::run(&truncate_sql).map_err(|e| PgStreamError::SpiError(e.to_string()))?;

    // For aggregate/distinct STs in DIFFERENTIAL mode, inject COUNT(*)
    // into the defining query so __pgs_count is populated for subsequent
    // differential refreshes.
    let effective_query = if st.refresh_mode == RefreshMode::Differential
        && crate::dvm::query_needs_pgs_count(&st.defining_query)
    {
        inject_pgs_count(&st.defining_query)
    } else {
        st.defining_query.clone()
    };

    // Compute row_id using the same hash formula as the delta query so
    // the MERGE ON clause matches during subsequent differential refreshes.
    // For UNION ALL queries, decompose into per-branch subqueries with
    // child-prefixed row IDs matching diff_union_all's formula.
    let insert_body =
        if let Some(ua_sql) = crate::dvm::try_union_all_refresh_sql(&st.defining_query) {
            ua_sql
        } else {
            let row_id_expr = crate::dvm::row_id_expr_for_query(&st.defining_query);
            format!("SELECT {row_id_expr} AS __pgs_row_id, sub.* FROM ({effective_query}) sub",)
        };

    let insert_sql = format!("INSERT INTO {quoted_table} {insert_body}");
    Spi::run(&insert_sql).map_err(|e| PgStreamError::SpiError(e.to_string()))?;

    // Re-enable user triggers and emit NOTIFY so listeners know a FULL
    // refresh occurred.
    if has_triggers {
        Spi::run(&format!("ALTER TABLE {quoted_table} ENABLE TRIGGER USER"))
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        let escaped_name = table_name.replace('\'', "''");
        let escaped_schema = schema.replace('\'', "''");
        Spi::run(&format!(
            "NOTIFY pgstream_refresh, '{{\"stream_table\": \"{escaped_name}\", \
             \"schema\": \"{escaped_schema}\", \"mode\": \"FULL\"}}'"
        ))
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

        pgrx::info!(
            "pg_stream: FULL refresh of {}.{} with user triggers suppressed.",
            schema,
            table_name,
        );
    }

    // Compute and store frontier so differential can start from here.
    // S3 optimization: single SPI call combines frontier storage,
    // timestamp update, and marking the ST as populated.
    let slot_positions = cdc::get_slot_positions(source_oids)?;
    let data_ts = get_data_timestamp_str();
    let frontier = version::compute_initial_frontier(&slot_positions, &data_ts);
    StreamTableMeta::store_frontier_and_complete_refresh(st.pgs_id, &frontier, 0)?;

    pgrx::info!("Stream table {}.{} refreshed (FULL)", schema, table_name);
    Ok(())
}

/// Execute a DIFFERENTIAL manual refresh using the DVM engine.
///
/// If no previous frontier exists (first refresh), falls back to FULL.
fn execute_manual_differential_refresh(
    st: &StreamTableMeta,
    schema: &str,
    table_name: &str,
    source_oids: &[pg_sys::Oid],
) -> Result<(), PgStreamError> {
    let prev_frontier = st.frontier.clone().unwrap_or_default();

    // If no previous frontier, the ST has never been refreshed or was
    // reinitialized — do a full refresh to establish the baseline.
    if prev_frontier.is_empty() {
        pgrx::info!(
            "Stream table {}.{}: no previous frontier, performing FULL refresh first",
            schema,
            table_name
        );
        return execute_manual_full_refresh(st, schema, table_name, source_oids);
    }

    // Get current WAL positions (reuses source_oids from caller — G-N3)
    let slot_positions = cdc::get_slot_positions(source_oids)?;
    let data_ts = get_data_timestamp_str();
    let new_frontier = version::compute_new_frontier(&slot_positions, &data_ts);

    // Execute the differential refresh via the DVM engine
    let (rows_inserted, rows_deleted) =
        refresh::execute_differential_refresh(st, &prev_frontier, &new_frontier)?;

    // Store the new frontier and mark refresh complete in a single SPI call (S3).
    StreamTableMeta::store_frontier_and_complete_refresh(st.pgs_id, &new_frontier, rows_inserted)?;

    pgrx::info!(
        "Stream table {}.{} refreshed (DIFFERENTIAL: +{} -{})",
        schema,
        table_name,
        rows_inserted,
        rows_deleted,
    );
    Ok(())
}

/// Get source table OIDs for a stream table (used by manual refresh path).
fn get_source_oids_for_manual_refresh(pgs_id: i64) -> Result<Vec<pg_sys::Oid>, PgStreamError> {
    let deps = StDependency::get_for_st(pgs_id)?;
    Ok(deps
        .into_iter()
        .filter(|dep| dep.source_type == "TABLE")
        .map(|dep| dep.source_relid)
        .collect())
}

/// Get the current data timestamp as an ISO-ish string for frontier computation.
fn get_data_timestamp_str() -> String {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{now_secs}Z")
}

/// Parse a Prometheus/GNU-style duration string and return seconds.
///
/// Used by SQL views to compare schedule. Returns NULL for invalid input
/// (cron expressions should not be passed).
#[pg_extern(schema = "pgstream", immutable, parallel_safe)]
fn parse_duration_seconds(input: &str) -> Option<i64> {
    parse_duration(input).ok()
}

/// Get the status of all stream tables.
///
/// Returns a summary row per stream table including schedule configuration,
/// data timestamp, and computed staleness interval.
#[pg_extern(schema = "pgstream", name = "pgs_status")]
#[allow(clippy::type_complexity)]
fn pgs_status() -> TableIterator<
    'static,
    (
        name!(name, String),
        name!(status, String),
        name!(refresh_mode, String),
        name!(is_populated, bool),
        name!(consecutive_errors, i32),
        name!(schedule, Option<String>),
        name!(data_timestamp, Option<TimestampWithTimeZone>),
        name!(staleness, Option<pgrx::datum::Interval>),
    ),
> {
    let rows: Vec<_> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgs_schema || '.' || pgs_name, status, refresh_mode, \
                 is_populated, consecutive_errors, schedule, data_timestamp, \
                 now() - data_timestamp AS staleness \
                 FROM pgstream.pgs_stream_tables ORDER BY pgs_schema, pgs_name",
                None,
                &[],
            )
            .unwrap();

        let mut out = Vec::new();
        for row in result {
            let name = row.get::<String>(1).unwrap().unwrap_or_default();
            let status = row.get::<String>(2).unwrap().unwrap_or_default();
            let mode = row.get::<String>(3).unwrap().unwrap_or_default();
            let populated = row.get::<bool>(4).unwrap().unwrap_or(false);
            let errors = row.get::<i32>(5).unwrap().unwrap_or(0);
            let schedule = row.get::<String>(6).unwrap();
            let data_ts = row.get::<TimestampWithTimeZone>(7).unwrap();
            let staleness = row.get::<pgrx::datum::Interval>(8).unwrap();
            out.push((
                name, status, mode, populated, errors, schedule, data_ts, staleness,
            ));
        }
        out
    });

    TableIterator::new(rows)
}

// ── Helper functions ───────────────────────────────────────────────────────

/// Set up CDC tracking for a base table source.
///
/// Creates a change buffer table and a CDC trigger on the source table
/// that captures INSERT/UPDATE/DELETE changes directly into the buffer.
///
/// PK columns are resolved from `pg_constraint` and used to pre-compute
/// `pk_hash` in the trigger, avoiding expensive JSONB PK extraction during
/// scan delta window-function partitioning.
fn setup_cdc_for_source(
    source_oid: pg_sys::Oid,
    pgs_id: i64,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    // Check if already tracked
    let already_tracked = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgstream.pgs_change_tracking WHERE source_relid = $1)",
        &[source_oid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !already_tracked {
        // Resolve PK columns for trigger pk_hash computation
        let pk_columns = cdc::resolve_pk_columns(source_oid)?;

        // Resolve all source columns for typed change buffer
        let col_defs = cdc::resolve_source_column_defs(source_oid)?;

        // Create the change buffer table (with typed columns + pk_hash if PK exists)
        cdc::create_change_buffer_table(
            source_oid,
            change_schema,
            !pk_columns.is_empty(),
            &col_defs,
        )?;

        // Create the CDC trigger on the source table (typed per-column INSERTs)
        let trigger_name =
            cdc::create_change_trigger(source_oid, change_schema, &pk_columns, &col_defs)?;

        // Insert tracking record
        Spi::run_with_args(
            "INSERT INTO pgstream.pgs_change_tracking (source_relid, slot_name, tracked_by_pgs_ids) \
             VALUES ($1, $2, ARRAY[$3])",
            &[
                source_oid.into(),
                trigger_name.as_str().into(),
                pgs_id.into(),
            ],
        )
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
    } else {
        // Already tracked — add this pgs_id to the tracking array
        Spi::run_with_args(
            "UPDATE pgstream.pgs_change_tracking \
             SET tracked_by_pgs_ids = array_append(tracked_by_pgs_ids, $1) \
             WHERE source_relid = $2 AND NOT ($1 = ANY(tracked_by_pgs_ids))",
            &[pgs_id.into(), source_oid.into()],
        )
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
    }

    Ok(())
}

/// Clean up CDC tracking for a source that may no longer be needed.
///
/// If no other STs reference this source, drop the CDC trigger and
/// change buffer table.
fn cleanup_cdc_for_source(source_oid: pg_sys::Oid, cdc_mode: CdcMode) -> Result<(), PgStreamError> {
    // Check if any other STs still reference this source
    let still_referenced = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS( \
            SELECT 1 FROM pgstream.pgs_dependencies WHERE source_relid = $1 \
        )",
        &[source_oid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if !still_referenced {
        let change_schema = config::pg_stream_change_buffer_schema();

        // If WAL-based CDC was active (or transitioning), clean up
        // the replication slot and publication first.
        if matches!(cdc_mode, CdcMode::Wal | CdcMode::Transitioning) {
            let slot_name = wal_decoder::slot_name_for_source(source_oid);
            if let Err(e) = wal_decoder::drop_replication_slot(&slot_name) {
                pgrx::warning!(
                    "Failed to drop replication slot {} for oid {}: {}",
                    slot_name,
                    source_oid.to_u32(),
                    e
                );
            }
            if let Err(e) = wal_decoder::drop_publication(source_oid) {
                pgrx::warning!(
                    "Failed to drop publication for oid {}: {}",
                    source_oid.to_u32(),
                    e
                );
            }
        }

        // Drop the CDC trigger and trigger function (may not exist if
        // already in WAL mode, but safe to attempt)
        if let Err(e) = cdc::drop_change_trigger(source_oid, &change_schema) {
            pgrx::warning!(
                "Failed to drop CDC trigger for oid {}: {}",
                source_oid.to_u32(),
                e
            );
        }

        // Drop the change buffer table
        let drop_buf_sql = format!(
            "DROP TABLE IF EXISTS {}.changes_{} CASCADE",
            quote_identifier(&change_schema),
            source_oid.to_u32(),
        );
        let _ = Spi::run(&drop_buf_sql);

        // Delete tracking record
        let _ = Spi::run_with_args(
            "DELETE FROM pgstream.pgs_change_tracking WHERE source_relid = $1",
            &[source_oid.into()],
        );
    }

    Ok(())
}

/// Parse a possibly schema-qualified name into `(schema, table)`.
fn parse_qualified_name(name: &str) -> Result<(String, String), PgStreamError> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    match parts.len() {
        1 => {
            let schema = Spi::get_one::<String>("SELECT current_schema()::text")
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or_else(|| "public".to_string());
            Ok((schema, parts[0].to_string()))
        }
        2 => Ok((parts[0].to_string(), parts[1].to_string())),
        _ => Err(PgStreamError::InvalidArgument(format!(
            "invalid table name: {name}"
        ))),
    }
}

/// Column metadata from a defining query.
#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub type_oid: PgOid,
}

/// Validate a defining query by executing `SELECT ... LIMIT 0`.
fn validate_defining_query(query: &str) -> Result<Vec<ColumnDef>, PgStreamError> {
    let check_sql = format!("SELECT * FROM ({query}) sub LIMIT 0");

    // Execute to verify syntax and extract columns
    Spi::connect(|client| {
        let result = client
            .select(&check_sql, None, &[])
            .map_err(|e| PgStreamError::QueryParseError(format!("{}", e)))?;

        // Extract column info from the result tuple descriptor
        let ncols = result
            .columns()
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        let mut columns = Vec::new();
        for i in 1..=ncols {
            let col_name = result
                .column_name(i)
                .unwrap_or_else(|_| format!("column_{}", i));
            let col_type_oid = result.column_type_oid(i).unwrap_or(PgOid::Invalid);
            columns.push(ColumnDef {
                name: col_name,
                type_oid: col_type_oid,
            });
        }

        if columns.is_empty() {
            return Err(PgStreamError::QueryParseError(
                "Defining query returns no columns".into(),
            ));
        }

        Ok(columns)
    })
}

/// Parsed schedule specification — either a duration-based schedule
/// or a cron expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Schedule {
    /// Duration-based: refresh when data is older than this many seconds.
    Duration(i64),
    /// Cron-based: refresh at the times specified by the cron expression.
    Cron(String),
}

/// Parse a Prometheus/GNU-style duration string into seconds.
///
/// Supported units: `s` (seconds), `m` (minutes), `h` (hours), `d` (days),
/// `w` (weeks). Compound durations like `1h30m` and `2m30s` are supported.
/// A bare integer (e.g., `"60"`) is treated as seconds.
///
/// Examples: `"30s"`, `"5m"`, `"1h"`, `"1h30m"`, `"1d"`, `"2w"`, `"60"`.
pub(crate) fn parse_duration(s: &str) -> Result<i64, PgStreamError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(PgStreamError::InvalidArgument(
            "schedule cannot be empty".into(),
        ));
    }

    // Bare integer → seconds
    if let Ok(secs) = s.parse::<i64>() {
        return if secs >= 0 {
            Ok(secs)
        } else {
            Err(PgStreamError::InvalidArgument(format!(
                "schedule cannot be negative: '{s}'"
            )))
        };
    }

    let mut total_secs: i64 = 0;
    let mut num_buf = String::new();
    let mut found_unit = false;

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            let multiplier = match ch {
                's' => 1i64,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                'w' => 604800,
                _ => {
                    return Err(PgStreamError::InvalidArgument(format!(
                        "invalid duration unit '{ch}' in '{s}'. \
                         Use s (seconds), m (minutes), h (hours), d (days), w (weeks). \
                         Example: '5m', '1h30m', '2d'"
                    )));
                }
            };

            if num_buf.is_empty() {
                return Err(PgStreamError::InvalidArgument(format!(
                    "expected a number before '{ch}' in duration '{s}'"
                )));
            }

            let n: i64 = num_buf.parse().map_err(|_| {
                PgStreamError::InvalidArgument(format!(
                    "invalid number '{num_buf}' in duration '{s}'"
                ))
            })?;

            total_secs += n * multiplier;
            num_buf.clear();
            found_unit = true;
        }
    }

    // Trailing digits without a unit → error (require explicit unit)
    if !num_buf.is_empty() {
        if found_unit {
            return Err(PgStreamError::InvalidArgument(format!(
                "trailing digits '{num_buf}' without a unit in duration '{s}'. \
                 Append s, m, h, d, or w. Example: '1h30m'"
            )));
        }
        // Pure digits already handled above; shouldn't reach here
        return Err(PgStreamError::InvalidArgument(format!(
            "invalid duration '{s}'"
        )));
    }

    if total_secs < 0 {
        return Err(PgStreamError::InvalidArgument(format!(
            "schedule cannot be negative: '{s}'"
        )));
    }

    Ok(total_secs)
}

/// Validate that schedule meets the minimum.
fn validate_schedule(seconds: i64) -> Result<(), PgStreamError> {
    let min = config::pg_stream_min_schedule_seconds() as i64;

    if seconds < min {
        return Err(PgStreamError::InvalidArgument(format!(
            "schedule must be at least {}s, got {}s",
            min, seconds
        )));
    }
    Ok(())
}

/// Parse a schedule string as either a duration or a cron expression.
///
/// **Duration strings** use Prometheus/GNU-style units: `30s`, `5m`, `1h`,
/// `1h30m`, `1d`, `2w`. A bare integer is treated as seconds.
///
/// **Cron expressions** follow standard 5-field (minute-granularity) or
/// 6-field (second-granularity) cron syntax, plus `@hourly`, `@daily`, etc.
/// aliases. Cron patterns are detected by the presence of spaces or a `@`
/// prefix.
///
/// Returns a `Schedule` variant.
pub(crate) fn parse_schedule(s: &str) -> Result<Schedule, PgStreamError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(PgStreamError::InvalidArgument(
            "schedule cannot be empty".into(),
        ));
    }

    // Heuristic: if the string starts with '@' or contains spaces, treat
    // it as a cron expression. Duration strings never contain spaces.
    if s.starts_with('@') || s.contains(' ') {
        validate_cron(s)?;
        Ok(Schedule::Cron(s.to_string()))
    } else {
        let secs = parse_duration(s)?;
        validate_schedule(secs)?;
        Ok(Schedule::Duration(secs))
    }
}

/// Validate a cron expression by parsing it with croner.
fn validate_cron(expr: &str) -> Result<(), PgStreamError> {
    use std::str::FromStr;

    croner::Cron::from_str(expr).map_err(|e| {
        PgStreamError::InvalidArgument(format!("invalid cron expression '{expr}': {e}"))
    })?;

    Ok(())
}

/// Check whether a cron schedule is due for refresh.
///
/// Returns `true` if `now >= next_occurrence(last_refresh_at, cron_expr)`.
/// If `last_refresh_at` is `None`, always returns `true` (never refreshed).
pub(crate) fn cron_is_due(cron_expr: &str, last_refresh_epoch: Option<i64>) -> bool {
    use std::str::FromStr;

    let cron = match croner::Cron::from_str(cron_expr) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let now = chrono::Utc::now();

    match last_refresh_epoch {
        None => true, // never refreshed → always due
        Some(epoch) => {
            let last = match chrono::DateTime::from_timestamp(epoch, 0) {
                Some(st) => st,
                None => return true,
            };
            // Find the next occurrence after the last refresh
            match cron.find_next_occurrence(&last, false) {
                Ok(next) => now >= next,
                Err(_) => false,
            }
        }
    }
}

/// Extract source relation OIDs from a defining query using PostgreSQL's parser/analyzer.
///
/// Uses `pg_sys::raw_parser()` + `pg_sys::parse_analyze_fixedparams()` to get
/// fully resolved table OIDs from the query's range table entries.
pub(crate) fn extract_source_relations(
    query: &str,
) -> Result<Vec<(pg_sys::Oid, String)>, PgStreamError> {
    use pgrx::PgList;
    use std::ffi::CString;

    let c_sql = CString::new(query)
        .map_err(|e| PgStreamError::QueryParseError(format!("Query contains null byte: {}", e)))?;

    // SAFETY: We're calling PostgreSQL C parser functions with valid inputs.
    // raw_parser and parse_analyze_fixedparams are safe when called within
    // a PostgreSQL backend with a valid memory context.
    unsafe {
        // Step 1: Parse the raw SQL into a parse tree
        let raw_list = pg_sys::raw_parser(c_sql.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT);

        let stmts = PgList::<pg_sys::RawStmt>::from_pg(raw_list);
        let raw_stmt = stmts.get_ptr(0).ok_or_else(|| {
            PgStreamError::QueryParseError("Query produced no parse tree nodes".into())
        })?;

        // Step 2: Analyze — resolves all table names to OIDs
        let query_node = pg_sys::parse_analyze_fixedparams(
            raw_stmt,
            c_sql.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
        );

        if query_node.is_null() {
            return Err(PgStreamError::QueryParseError(
                "Query analysis returned null".into(),
            ));
        }

        // Step 3: Extract relation OIDs from the analyzed query tree.
        //
        // The top-level rtable may NOT contain base tables referenced
        // inside CTEs — those live in the CTE's own sub-Query rtable.
        // Similarly, subqueries in FROM (RTE_SUBQUERY) have their own
        // rtables. We walk the full tree recursively.
        let mut relations = Vec::new();
        let mut seen_oids = std::collections::HashSet::new();

        collect_relation_oids(query_node, &mut relations, &mut seen_oids);

        if relations.is_empty() {
            return Err(PgStreamError::QueryParseError(
                "Defining query references no tables".into(),
            ));
        }

        Ok(relations)
    }
}

/// Recursively collect `RTE_RELATION` OIDs from an analyzed `Query` node.
///
/// Walks:
/// 1. The top-level `rtable` for `RTE_RELATION` entries (base tables)
/// 2. `RTE_SUBQUERY` entries — recurse into their `subquery` Query node
/// 3. `Query.cteList` — each CTE's `ctequery` is an analyzed `Query`
///    node after `parse_analyze_fixedparams()`
///
/// # Safety
/// Caller must ensure `query_node` points to a valid analyzed `Query`.
unsafe fn collect_relation_oids(
    query_node: *mut pg_sys::Query,
    relations: &mut Vec<(pg_sys::Oid, String)>,
    seen_oids: &mut std::collections::HashSet<pg_sys::Oid>,
) {
    use pgrx::PgList;

    if query_node.is_null() {
        return;
    }

    let query = unsafe { &*query_node };

    // Walk the range table
    let rtable = unsafe { PgList::<pg_sys::RangeTblEntry>::from_pg(query.rtable) };
    for rte_ptr in rtable.iter_ptr() {
        // SAFETY: rte_ptr is a valid pointer from a PgList iteration.
        let rte = unsafe { &*rte_ptr };
        match rte.rtekind {
            pg_sys::RTEKind::RTE_RELATION => {
                let oid = rte.relid;
                if seen_oids.insert(oid) {
                    let source_type = classify_source_relation(oid);
                    relations.push((oid, source_type));
                }
            }
            pg_sys::RTEKind::RTE_SUBQUERY => {
                // Subquery in FROM — recurse into its own Query node
                if !rte.subquery.is_null() {
                    unsafe {
                        collect_relation_oids(rte.subquery, relations, seen_oids);
                    }
                }
            }
            _ => {}
        }
    }

    // Walk CTE list — each CommonTableExpr.ctequery is an analyzed Query
    // node (tagged T_Query) after parse_analyze_fixedparams().
    let cte_list = unsafe { PgList::<pg_sys::Node>::from_pg(query.cteList) };
    for node_ptr in cte_list.iter_ptr() {
        if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
            let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };
            if !cte.ctequery.is_null()
                && unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_Query) }
            {
                unsafe {
                    collect_relation_oids(cte.ctequery as *mut pg_sys::Query, relations, seen_oids);
                }
            }
        }
    }
}

/// Classify a source relation as TABLE, STREAM_TABLE, or VIEW.
fn classify_source_relation(oid: pg_sys::Oid) -> String {
    // Check if this OID is a stream table
    let is_dt = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgstream.pgs_stream_tables WHERE pgs_relid = $1)",
        &[oid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if is_dt {
        return "STREAM_TABLE".to_string();
    }

    // Check relkind: 'r' = table, 'v' = view, 'm' = matview
    let relkind = Spi::get_one_with_args::<String>(
        "SELECT relkind::text FROM pg_class WHERE oid = $1",
        &[oid.into()],
    )
    .unwrap_or(None)
    .unwrap_or_else(|| "r".to_string());

    match relkind.as_str() {
        "v" => "VIEW".to_string(),
        _ => "TABLE".to_string(),
    }
}

/// Check for cycles after adding the proposed dependency edges.
///
/// Loads the existing DAG from the catalog, adds the proposed edges,
/// and runs Kahn's algorithm for cycle detection.
fn check_for_cycles(source_relids: &[(pg_sys::Oid, String)]) -> Result<(), PgStreamError> {
    if source_relids.is_empty() {
        return Ok(());
    }

    // Check if any source is itself a stream table — only then can cycles exist
    let has_dt_source = source_relids
        .iter()
        .any(|(_, stype)| stype == "STREAM_TABLE");

    if !has_dt_source {
        // No stream table sources → no possible cycle
        return Ok(());
    }

    // Build the DAG from catalog and add proposed edges
    let mut dag = StDag::build_from_catalog(config::pg_stream_min_schedule_seconds())?;

    // Create a temporary node for the proposed ST (use a sentinel pgs_id)
    let proposed_id = NodeId::StreamTable(i64::MAX);
    dag.add_dt_node(DagNode {
        id: proposed_id,
        schedule: Some(std::time::Duration::from_secs(60)),
        effective_schedule: std::time::Duration::from_secs(60),
        name: "<proposed>".to_string(),
        status: StStatus::Initializing,
        schedule_raw: None,
    });

    // Add proposed edges
    for (source_oid, source_type) in source_relids {
        let source_node = if source_type == "STREAM_TABLE" {
            // Find the pgs_id for this source OID
            match crate::catalog::StreamTableMeta::get_by_relid(*source_oid) {
                Ok(meta) => NodeId::StreamTable(meta.pgs_id),
                Err(_) => NodeId::BaseTable(source_oid.to_u32()),
            }
        } else {
            NodeId::BaseTable(source_oid.to_u32())
        };
        dag.add_edge(source_node, proposed_id);
    }

    // Run cycle detection
    dag.detect_cycles()
}

/// Build CREATE TABLE DDL for the storage table.
fn build_create_table_sql(
    schema: &str,
    name: &str,
    columns: &[ColumnDef],
    needs_pgs_count: bool,
) -> String {
    let col_defs: Vec<String> = columns
        .iter()
        .map(|c| {
            // Use regtype to get the type name from the OID
            let type_name = match c.type_oid {
                PgOid::Invalid => "text".to_string(),
                oid => {
                    // Try to resolve the type name via SPI
                    Spi::get_one_with_args::<String>(
                        "SELECT $1::regtype::text",
                        &[oid.value().into()],
                    )
                    .unwrap_or(Some("text".to_string()))
                    .unwrap_or_else(|| "text".to_string())
                }
            };
            format!("    {} {}", quote_identifier(&c.name), type_name)
        })
        .collect();

    // Add __pgs_count auxiliary column for aggregate/distinct STs.
    let count_col = if needs_pgs_count {
        ",\n    __pgs_count BIGINT NOT NULL DEFAULT 0"
    } else {
        ""
    };

    format!(
        "CREATE TABLE {}.{} (\n    __pgs_row_id BIGINT,\n{}{}\n)",
        quote_identifier(schema),
        quote_identifier(name),
        col_defs.join(",\n"),
        count_col,
    )
}

/// Get the OID of a table by schema and name.
fn get_table_oid(schema: &str, name: &str) -> Result<pg_sys::Oid, PgStreamError> {
    let oid = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT ($1 || '.' || $2)::regclass::oid",
        &[schema.into(), name.into()],
    )
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?
    .ok_or_else(|| {
        PgStreamError::NotFound(format!(
            "table {}.{} not found after creation",
            schema, name
        ))
    })?;
    Ok(oid)
}

/// Initialize a stream table by populating it from its defining query.
fn initialize_dt(
    schema: &str,
    name: &str,
    query: &str,
    pgs_id: i64,
    _columns: &[ColumnDef],
    needs_pgs_count: bool,
) -> Result<(), PgStreamError> {
    // For aggregate/distinct STs, inject COUNT(*) AS __pgs_count into the
    // defining query so the auxiliary column is populated correctly.
    let effective_query = if needs_pgs_count {
        inject_pgs_count(query)
    } else {
        query.to_string()
    };

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

    let insert_sql = format!(
        "INSERT INTO {schema}.{table} {insert_body}",
        schema = quote_identifier(schema),
        table = quote_identifier(name),
    );

    Spi::run(&insert_sql)
        .map_err(|e| PgStreamError::SpiError(format!("Failed to initialize ST: {}", e)))?;

    // Update catalog
    let now = Spi::get_one::<TimestampWithTimeZone>("SELECT now()")
        .map_err(|e| PgStreamError::SpiError(e.to_string()))?
        .ok_or_else(|| PgStreamError::InternalError("now() returned NULL".into()))?;

    StreamTableMeta::update_after_refresh(pgs_id, now, 0)?;
    Ok(())
}

/// Quote a SQL identifier (escape double quotes).
fn quote_identifier(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Inject `COUNT(*) AS __pgs_count` into an aggregate/distinct defining query
/// so that the full refresh populates the auxiliary count column.
///
/// For aggregate queries, this adds `, COUNT(*) AS __pgs_count` before the
/// first top-level `FROM`.
///
/// For DISTINCT queries, transforms `SELECT DISTINCT cols FROM ...` into
/// `SELECT cols, COUNT(*) AS __pgs_count FROM ... GROUP BY cols`.
pub fn inject_pgs_count(query: &str) -> String {
    // Detect SELECT DISTINCT — needs special handling because we must
    // replace DISTINCT with GROUP BY (can't mix DISTINCT with aggregates).
    if let Some(distinct_info) = detect_and_strip_distinct(query) {
        // distinct_info.stripped is the query with DISTINCT removed,
        // e.g., "SELECT color, size FROM prop_dist"
        // distinct_info.columns are the SELECT-list columns before FROM.
        if let Some(from_pos) = find_top_level_keyword(&distinct_info.stripped, "FROM") {
            let select_part = distinct_info.stripped[..from_pos].trim_end();
            let from_part = &distinct_info.stripped[from_pos..];
            let col_list = distinct_info.columns.join(", ");
            return format!(
                "{select_part}, COUNT(*) AS __pgs_count {from_part} GROUP BY {col_list}",
            );
        }
        // Fallback if FROM not found after stripping DISTINCT
        return distinct_info.stripped;
    }

    // Non-DISTINCT (aggregate) queries: just inject COUNT(*) before FROM.
    if let Some(pos) = find_top_level_keyword(query, "FROM") {
        format!(
            "{}, COUNT(*) AS __pgs_count {}",
            query[..pos].trim_end(),
            &query[pos..],
        )
    } else {
        // Fallback: can't inject; return as-is (will leave __pgs_count = DEFAULT 0)
        query.to_string()
    }
}

/// Result of stripping DISTINCT from a query.
struct DistinctStripped {
    /// The query with DISTINCT removed.
    stripped: String,
    /// The column expressions from the SELECT list (between SELECT and FROM).
    columns: Vec<String>,
}

/// Detect if a query starts with `SELECT DISTINCT` (at the top level) and
/// return the query with DISTINCT removed plus the extracted column list.
///
/// Returns `None` if the query does not have a top-level DISTINCT.
fn detect_and_strip_distinct(query: &str) -> Option<DistinctStripped> {
    // Find top-level SELECT
    let select_pos = find_top_level_keyword(query, "SELECT")?;
    let after_select = &query[select_pos + 6..]; // len("SELECT") == 6

    // Check if DISTINCT follows (skipping whitespace)
    let trimmed = after_select.trim_start();
    if !trimmed.to_ascii_uppercase().starts_with("DISTINCT") {
        return None;
    }

    // Make sure DISTINCT is followed by a word boundary (not DISTINCT_ON or similar)
    let after_distinct = &trimmed[8..]; // len("DISTINCT") == 8
    if !after_distinct.is_empty() {
        let next_byte = after_distinct.as_bytes()[0];
        if next_byte.is_ascii_alphanumeric() || next_byte == b'_' {
            return None; // e.g., DISTINCTLY or DISTINCT_SOMETHING
        }
    }

    // Build the stripped query: everything before SELECT + "SELECT" + after DISTINCT
    let prefix = &query[..select_pos];
    let stripped = format!("{prefix}SELECT{after_distinct}");

    // Extract column list between SELECT and FROM in the stripped query
    let from_pos = find_top_level_keyword(&stripped, "FROM")?;
    let select_kw_end = find_top_level_keyword(&stripped, "SELECT")? + 6;
    let col_text = stripped[select_kw_end..from_pos].trim();

    // Split the column list on top-level commas
    let columns = split_top_level_commas(col_text);

    Some(DistinctStripped { stripped, columns })
}

/// Split a string on top-level commas (not inside parentheses or string literals).
/// Returns trimmed column expressions.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut start = 0;
    let bytes = s.as_bytes();

    for i in 0..bytes.len() {
        if in_string {
            if bytes[i] == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    // skip escaped quote
                    continue;
                }
                in_string = false;
            }
            continue;
        }
        match bytes[i] {
            b'\'' => in_string = true,
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                let col = s[start..i].trim().to_string();
                if !col.is_empty() {
                    result.push(col);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    // Last segment
    let col = s[start..].trim().to_string();
    if !col.is_empty() {
        result.push(col);
    }
    result
}

/// Find the byte offset of the first top-level occurrence of a SQL keyword
/// (not inside parentheses or string literals).
fn find_top_level_keyword(sql: &str, keyword: &str) -> Option<usize> {
    let kw_len = keyword.len();
    let bytes = sql.as_bytes();
    let kw_upper = keyword.to_ascii_uppercase();
    let kw_bytes = kw_upper.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\'' {
                // Check for escaped quote ''
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                } else {
                    in_string = false;
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }
        match bytes[i] {
            b'\'' => {
                in_string = true;
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            _ if depth == 0 && i + kw_len <= bytes.len() => {
                // Check if this position matches the keyword (case-insensitive)
                let candidate = &bytes[i..i + kw_len];
                if candidate
                    .iter()
                    .zip(kw_bytes.iter())
                    .all(|(a, b)| a.to_ascii_uppercase() == *b)
                {
                    // Verify word boundaries
                    let before_ok =
                        i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
                    let after_ok = i + kw_len >= bytes.len()
                        || !bytes[i + kw_len].is_ascii_alphanumeric() && bytes[i + kw_len] != b'_';
                    if before_ok && after_ok {
                        return Some(i);
                    }
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_pgs_count_distinct_basic() {
        let query = "SELECT DISTINCT color, size FROM prop_dist";
        let result = inject_pgs_count(query);
        assert_eq!(
            result,
            "SELECT color, size, COUNT(*) AS __pgs_count FROM prop_dist GROUP BY color, size"
        );
    }

    #[test]
    fn test_inject_pgs_count_distinct_lowercase() {
        let query = "select distinct color, size from prop_dist";
        let result = inject_pgs_count(query);
        // Note: "SELECT" is uppercased because detect_and_strip_distinct
        // reconstructs the prefix with literal "SELECT".
        assert_eq!(
            result,
            "SELECT color, size, COUNT(*) AS __pgs_count from prop_dist GROUP BY color, size"
        );
    }

    #[test]
    fn test_inject_pgs_count_distinct_with_where() {
        let query = "SELECT DISTINCT color FROM items WHERE active = true";
        let result = inject_pgs_count(query);
        assert_eq!(
            result,
            "SELECT color, COUNT(*) AS __pgs_count FROM items WHERE active = true GROUP BY color"
        );
    }

    #[test]
    fn test_inject_pgs_count_aggregate_no_distinct() {
        // Non-DISTINCT aggregate: just adds COUNT(*) before FROM
        let query = "SELECT region, SUM(amount) FROM orders GROUP BY region";
        let result = inject_pgs_count(query);
        assert_eq!(
            result,
            "SELECT region, SUM(amount), COUNT(*) AS __pgs_count FROM orders GROUP BY region"
        );
    }

    #[test]
    fn test_inject_pgs_count_distinct_three_cols() {
        let query = "SELECT DISTINCT a, b, c FROM t1";
        let result = inject_pgs_count(query);
        assert_eq!(
            result,
            "SELECT a, b, c, COUNT(*) AS __pgs_count FROM t1 GROUP BY a, b, c"
        );
    }

    #[test]
    fn test_detect_and_strip_distinct_none_for_non_distinct() {
        let query = "SELECT color, size FROM prop_dist";
        assert!(detect_and_strip_distinct(query).is_none());
    }

    #[test]
    fn test_detect_and_strip_distinct_basic() {
        let query = "SELECT DISTINCT color, size FROM prop_dist";
        let info = detect_and_strip_distinct(query).unwrap();
        assert_eq!(info.columns, vec!["color", "size"]);
        assert!(info.stripped.contains("SELECT"));
        assert!(!info.stripped.to_uppercase().contains("DISTINCT"));
    }

    #[test]
    fn test_split_top_level_commas() {
        let cols = split_top_level_commas("a, b, c");
        assert_eq!(cols, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_split_top_level_commas_with_parens() {
        let cols = split_top_level_commas("a, COALESCE(b, 0), c");
        assert_eq!(cols, vec!["a", "COALESCE(b, 0)", "c"]);
    }

    // ── quote_identifier tests ─────────────────────────────────────────

    #[test]
    fn test_quote_identifier_simple() {
        assert_eq!(quote_identifier("my_table"), "\"my_table\"");
    }

    #[test]
    fn test_quote_identifier_with_double_quotes() {
        // Embedded double quotes must be escaped by doubling
        assert_eq!(quote_identifier(r#"weird"name"#), r#""weird""name""#);
    }

    #[test]
    fn test_quote_identifier_reserved_word() {
        assert_eq!(quote_identifier("select"), "\"select\"");
    }

    // ── get_data_timestamp_str tests ───────────────────────────────────

    #[test]
    fn test_get_data_timestamp_str_suffix() {
        let ts = get_data_timestamp_str();
        assert!(ts.ends_with('Z'), "expected trailing Z, got: {}", ts);
    }

    #[test]
    fn test_get_data_timestamp_str_reasonable_epoch() {
        let ts = get_data_timestamp_str();
        let numeric = ts.trim_end_matches('Z');
        let secs: u64 = numeric.parse().expect("should be numeric epoch seconds");
        // Epoch should be > 2024-01-01 (1704067200) and < 2040-01-01 (2208988800)
        assert!(secs > 1_704_067_200, "epoch too old: {}", secs);
        assert!(secs < 2_208_988_800, "epoch too far in future: {}", secs);
    }

    // ── find_top_level_keyword tests ───────────────────────────────────

    #[test]
    fn test_find_top_level_keyword_basic() {
        let sql = "SELECT a, b FROM t1 WHERE x > 0";
        assert_eq!(find_top_level_keyword(sql, "FROM"), Some(12));
    }

    #[test]
    fn test_find_top_level_keyword_inside_parens_ignored() {
        // FROM inside parentheses should NOT be found
        let sql = "SELECT (SELECT x FROM y) AS sub FROM t1";
        let pos = find_top_level_keyword(sql, "FROM").unwrap();
        // Should find the outer FROM, not the inner one
        assert!(pos > 24, "found inner FROM at {}, expected outer", pos);
    }

    #[test]
    fn test_find_top_level_keyword_inside_string_ignored() {
        let sql = "SELECT 'FROM' AS label FROM t1";
        let pos = find_top_level_keyword(sql, "FROM").unwrap();
        // Should skip the 'FROM' string literal and find the real FROM
        assert!(pos > 20, "found FROM inside string at {}", pos);
    }

    #[test]
    fn test_find_top_level_keyword_case_insensitive() {
        let sql = "select a from t1";
        assert!(find_top_level_keyword(sql, "FROM").is_some());
    }

    #[test]
    fn test_find_top_level_keyword_not_found() {
        let sql = "SELECT a, b FROM t1";
        assert_eq!(find_top_level_keyword(sql, "WHERE"), None);
    }

    #[test]
    fn test_find_top_level_keyword_word_boundary() {
        // "FROMAGE" contains "FROM" but should not match due to word boundary
        let sql = "SELECT FROMAGE FROM t1";
        let pos = find_top_level_keyword(sql, "FROM").unwrap();
        // Should match the standalone FROM, not FROMAGE
        assert!(pos > 7, "matched FROM inside FROMAGE at {}", pos);
    }

    // ── split_top_level_commas additional edge cases ───────────────────

    #[test]
    fn test_split_top_level_commas_nested_function() {
        let cols = split_top_level_commas("SUM(CASE WHEN x > 0 THEN 1 ELSE 0 END), COUNT(*)");
        assert_eq!(
            cols,
            vec!["SUM(CASE WHEN x > 0 THEN 1 ELSE 0 END)", "COUNT(*)"]
        );
    }

    #[test]
    fn test_split_top_level_commas_quoted_string_with_comma() {
        let cols = split_top_level_commas("'hello, world', b");
        assert_eq!(cols, vec!["'hello, world'", "b"]);
    }

    #[test]
    fn test_split_top_level_commas_empty_input() {
        let cols = split_top_level_commas("");
        assert!(cols.is_empty());
    }

    #[test]
    fn test_split_top_level_commas_single_column() {
        let cols = split_top_level_commas("  a  ");
        assert_eq!(cols, vec!["a"]);
    }

    // ── parse_duration tests ───────────────────────────────────────────

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("5m").unwrap(), 300);
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("2h").unwrap(), 7200);
    }

    #[test]
    fn test_parse_duration_days() {
        assert_eq!(parse_duration("1d").unwrap(), 86400);
    }

    #[test]
    fn test_parse_duration_weeks() {
        assert_eq!(parse_duration("1w").unwrap(), 604800);
    }

    #[test]
    fn test_parse_duration_compound() {
        assert_eq!(parse_duration("1h30m").unwrap(), 5400);
        assert_eq!(parse_duration("2m30s").unwrap(), 150);
        assert_eq!(parse_duration("1d12h").unwrap(), 129600);
    }

    #[test]
    fn test_parse_duration_bare_integer() {
        assert_eq!(parse_duration("60").unwrap(), 60);
        assert_eq!(parse_duration("0").unwrap(), 0);
    }

    #[test]
    fn test_parse_duration_zero() {
        assert_eq!(parse_duration("0s").unwrap(), 0);
        assert_eq!(parse_duration("0m").unwrap(), 0);
    }

    #[test]
    fn test_parse_duration_whitespace_trimmed() {
        assert_eq!(parse_duration("  5m  ").unwrap(), 300);
    }

    #[test]
    fn test_parse_duration_empty_fails() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("  ").is_err());
    }

    #[test]
    fn test_parse_duration_invalid_unit_fails() {
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn test_parse_duration_no_number_before_unit_fails() {
        assert!(parse_duration("m").is_err());
    }

    #[test]
    fn test_parse_duration_trailing_digits_fails() {
        assert!(parse_duration("1h30").is_err());
    }

    #[test]
    fn test_parse_duration_negative_bare_fails() {
        assert!(parse_duration("-60").is_err());
    }

    // ── parse_schedule tests ────────────────────────────────────────────
    // Note: parse_schedule for *duration* strings calls validate_schedule,
    // which accesses a GUC. Duration-based scheduling is already tested via
    // the parse_duration tests above. Here we only test cron parsing and the
    // cron detection heuristic.

    #[test]
    fn test_parse_schedule_cron_every_5_min() {
        let schedule = parse_schedule("*/5 * * * *").unwrap();
        assert_eq!(schedule, Schedule::Cron("*/5 * * * *".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_hourly_alias() {
        let schedule = parse_schedule("@hourly").unwrap();
        assert_eq!(schedule, Schedule::Cron("@hourly".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_daily_alias() {
        let schedule = parse_schedule("@daily").unwrap();
        assert_eq!(schedule, Schedule::Cron("@daily".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_weekly_alias() {
        let schedule = parse_schedule("@weekly").unwrap();
        assert_eq!(schedule, Schedule::Cron("@weekly".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_specific_time() {
        let schedule = parse_schedule("0 6 * * 1-5").unwrap();
        assert_eq!(schedule, Schedule::Cron("0 6 * * 1-5".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_six_field() {
        // 6-field cron (with seconds)
        let schedule = parse_schedule("0 */5 * * * *").unwrap();
        assert_eq!(schedule, Schedule::Cron("0 */5 * * * *".to_string()));
    }

    #[test]
    fn test_parse_schedule_invalid_cron() {
        // Invalid cron expression (only 4 fields)
        assert!(parse_schedule("* * * *").is_err());
    }

    #[test]
    fn test_parse_schedule_invalid_duration() {
        assert!(parse_schedule("abc").is_err());
    }

    #[test]
    fn test_parse_schedule_empty_fails() {
        assert!(parse_schedule("").is_err());
    }

    #[test]
    fn test_parse_schedule_whitespace_trimmed() {
        let schedule = parse_schedule("  @hourly  ").unwrap();
        assert_eq!(schedule, Schedule::Cron("@hourly".to_string()));
    }

    // ── validate_cron tests ─────────────────────────────────────────────

    #[test]
    fn test_validate_cron_standard_expressions() {
        assert!(validate_cron("* * * * *").is_ok());
        assert!(validate_cron("0 0 * * *").is_ok());
        assert!(validate_cron("*/15 * * * *").is_ok());
        assert!(validate_cron("0 9-17 * * 1-5").is_ok());
    }

    #[test]
    fn test_validate_cron_aliases() {
        assert!(validate_cron("@yearly").is_ok());
        assert!(validate_cron("@monthly").is_ok());
        assert!(validate_cron("@weekly").is_ok());
        assert!(validate_cron("@daily").is_ok());
        assert!(validate_cron("@hourly").is_ok());
    }

    #[test]
    fn test_validate_cron_invalid() {
        assert!(validate_cron("not a cron").is_err());
        assert!(validate_cron("99 99 99 99 99").is_err());
    }

    // ── cron_is_due tests ───────────────────────────────────────────────

    #[test]
    fn test_cron_is_due_no_previous_refresh() {
        // If never refreshed, should always be due
        assert!(cron_is_due("* * * * *", None));
    }

    #[test]
    fn test_cron_is_due_recently_refreshed() {
        // If refreshed just now, should not be due (unless cron fires every second)
        let now_epoch = chrono::Utc::now().timestamp();
        // Cron = hourly → next occurrence is ~60 min from now
        assert!(!cron_is_due("@hourly", Some(now_epoch)));
    }

    #[test]
    fn test_cron_is_due_long_ago_refresh() {
        // If refreshed long ago, should be due
        let old_epoch = chrono::Utc::now().timestamp() - 86400; // 24h ago
        // Cron = hourly → should definitely be due
        assert!(cron_is_due("@hourly", Some(old_epoch)));
    }

    #[test]
    fn test_cron_is_due_invalid_expr_returns_false() {
        // Invalid cron should gracefully return false
        assert!(!cron_is_due("invalid cron", None));
    }

    // ── Additional parse_duration edge-case tests ────────────────────────

    #[test]
    fn test_parse_duration_large_compound() {
        // 1 week + 2 days + 3 hours + 4 minutes + 5 seconds
        assert_eq!(
            parse_duration("1w2d3h4m5s").unwrap(),
            604800 + 2 * 86400 + 3 * 3600 + 4 * 60 + 5
        );
    }

    #[test]
    fn test_parse_duration_repeated_units() {
        // Repeated same-unit segments accumulate
        assert_eq!(parse_duration("1m1m1m").unwrap(), 180);
    }

    #[test]
    fn test_parse_duration_only_hours_and_seconds() {
        assert_eq!(parse_duration("2h15s").unwrap(), 7215);
    }

    #[test]
    fn test_parse_duration_large_numbers() {
        assert_eq!(parse_duration("999s").unwrap(), 999);
        assert_eq!(parse_duration("100m").unwrap(), 6000);
    }

    #[test]
    fn test_parse_duration_single_digit_units() {
        assert_eq!(parse_duration("1s").unwrap(), 1);
        assert_eq!(parse_duration("1m").unwrap(), 60);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("1d").unwrap(), 86400);
        assert_eq!(parse_duration("1w").unwrap(), 604800);
    }

    #[test]
    fn test_parse_duration_mixed_invalid_char_fails() {
        assert!(parse_duration("5m+3s").is_err());
        assert!(parse_duration("5m 3s").is_err()); // space treated as cron by parse_schedule
        assert!(parse_duration("5M").is_err()); // uppercase
    }

    // ── Additional validate_cron tests ───────────────────────────────────

    #[test]
    fn test_validate_cron_ranges_and_lists() {
        // Day-of-week range
        assert!(validate_cron("0 9 * * 1-5").is_ok());
        // Comma-separated list
        assert!(validate_cron("0 0 1,15 * *").is_ok());
        // Step + range
        assert!(validate_cron("0 */6 * * *").is_ok());
    }

    #[test]
    fn test_validate_cron_out_of_range() {
        // Minute 60 is invalid (range 0-59)
        assert!(validate_cron("60 * * * *").is_err());
        // Hour 25 is invalid
        assert!(validate_cron("0 25 * * *").is_err());
    }

    #[test]
    fn test_validate_cron_too_few_fields() {
        assert!(validate_cron("* *").is_err());
        assert!(validate_cron("*").is_err());
    }

    #[test]
    fn test_validate_cron_aliases_exhaustive() {
        assert!(validate_cron("@annually").is_ok());
    }

    // ── Additional parse_schedule tests ──────────────────────────────────

    #[test]
    fn test_parse_schedule_cron_with_ranges_and_steps() {
        let schedule = parse_schedule("*/10 9-17 * * 1-5").unwrap();
        assert_eq!(schedule, Schedule::Cron("*/10 9-17 * * 1-5".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_monthly_alias() {
        let schedule = parse_schedule("@monthly").unwrap();
        assert_eq!(schedule, Schedule::Cron("@monthly".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_yearly_alias() {
        let schedule = parse_schedule("@yearly").unwrap();
        assert_eq!(schedule, Schedule::Cron("@yearly".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_annually_alias() {
        let schedule = parse_schedule("@annually").unwrap();
        assert_eq!(schedule, Schedule::Cron("@annually".to_string()));
    }

    #[test]
    fn test_parse_schedule_cron_comma_list() {
        let schedule = parse_schedule("0 0 1,15 * *").unwrap();
        assert_eq!(schedule, Schedule::Cron("0 0 1,15 * *".to_string()));
    }

    #[test]
    fn test_parse_schedule_invalid_cron_bad_field_count() {
        // 3 fields — too few for cron
        assert!(parse_schedule("* * *").is_err());
    }

    #[test]
    fn test_parse_schedule_invalid_cron_bad_range() {
        // Minute 99 is out of range
        assert!(parse_schedule("99 * * * *").is_err());
    }

    // ── Additional cron_is_due tests ────────────────────────────────────

    #[test]
    fn test_cron_is_due_every_minute_recently_refreshed() {
        let now_epoch = chrono::Utc::now().timestamp();
        // Every-minute cron: next fire is ~60s away, so if refreshed
        // just now it should NOT be due (within the minute)
        // But if refreshed 2 minutes ago, should be due
        let old_epoch = now_epoch - 120;
        assert!(cron_is_due("* * * * *", Some(old_epoch)));
    }

    #[test]
    fn test_cron_is_due_yearly_recently_refreshed() {
        let now_epoch = chrono::Utc::now().timestamp();
        // Yearly cron: next fire is ~1 year away
        assert!(!cron_is_due("@yearly", Some(now_epoch)));
    }

    #[test]
    fn test_cron_is_due_with_alias() {
        let old_epoch = chrono::Utc::now().timestamp() - 7200; // 2 hours ago
        assert!(cron_is_due("@hourly", Some(old_epoch)));
    }
}
