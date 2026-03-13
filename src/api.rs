//! User-facing SQL API functions for pgtrickle.
//!
//! All functions are exposed in the `pg_trickle` schema and provide the primary
//! interface for creating, altering, dropping, and refreshing stream tables.

use pgrx::prelude::*;
use std::time::Instant;

use crate::catalog::{CdcMode, StDependency, StreamTableMeta};
use crate::cdc;
use crate::config;
use crate::dag::{
    DagNode, DiamondConsistency, DiamondSchedulePolicy, NodeId, RefreshMode, StDag, StStatus,
};
use crate::error::PgTrickleError;
use crate::refresh;
use crate::shmem;
use crate::version;
use crate::wal_decoder;

/// Create a new stream table.
///
/// # Arguments
/// - `name`: Schema-qualified name (`'schema.table'`) or unqualified (`'table'`).
/// - `query`: The defining SELECT query.
/// - `schedule`: Desired maximum schedule. `'calculated'` for CALCULATED mode (inherits schedule from downstream dependents).
/// - `refresh_mode`: `'AUTO'` (default — DIFFERENTIAL with FULL fallback),
///   `'FULL'`, `'DIFFERENTIAL'`, or `'IMMEDIATE'`.
/// - `initialize`: Whether to populate the table immediately.
/// - `diamond_consistency`: `'none'` (default) or `'atomic'`.
/// - `diamond_schedule_policy`: `'fastest'` (default) or `'slowest'`.
#[allow(clippy::too_many_arguments)]
#[pg_extern(schema = "pgtrickle")]
fn create_stream_table(
    name: &str,
    query: &str,
    schedule: default!(Option<&str>, "'calculated'"),
    refresh_mode: default!(&str, "'AUTO'"),
    initialize: default!(bool, true),
    diamond_consistency: default!(Option<&str>, "NULL"),
    diamond_schedule_policy: default!(Option<&str>, "NULL"),
    cdc_mode: default!(Option<&str>, "NULL"),
    append_only: default!(bool, false),
) {
    let result = create_stream_table_impl(
        name,
        query,
        schedule,
        refresh_mode,
        initialize,
        diamond_consistency,
        diamond_schedule_policy,
        cdc_mode,
        append_only,
    );
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

/// Run the full query rewrite pipeline: view inlining, nested window
/// expressions, DISTINCT ON, GROUPING SETS, scalar subqueries, SubLinks
/// in OR, and ROWS FROM.
///
/// Returns the rewritten query string. The caller should keep the
/// original query separately if needed for `original_query` tracking.
fn run_query_rewrite_pipeline(query: &str) -> Result<String, PgTrickleError> {
    // View inlining — must run first so view definitions containing
    // DISTINCT ON, GROUPING SETS, etc. get further rewritten.
    let query = crate::dvm::rewrite_views_inline(query)?;
    // Nested window expression lift
    let query = crate::dvm::rewrite_nested_window_exprs(&query)?;
    // DISTINCT ON → ROW_NUMBER() window
    let query = crate::dvm::rewrite_distinct_on(&query)?;
    // GROUPING SETS / CUBE / ROLLUP → UNION ALL of GROUP BY
    let query = crate::dvm::rewrite_grouping_sets(&query)?;
    // Scalar subquery in WHERE → CROSS JOIN
    let query = crate::dvm::rewrite_scalar_subquery_in_where(&query)?;
    // Correlated scalar subquery in SELECT → LEFT JOIN
    let query = crate::dvm::rewrite_correlated_scalar_in_select(&query)?;
    // SubLinks inside OR → UNION branches
    let query = crate::dvm::rewrite_sublinks_in_or(&query)?;
    // ROWS FROM() multi-function rewrite
    let query = crate::dvm::rewrite_rows_from(&query)?;
    Ok(query)
}

/// Validated query metadata returned by [`validate_and_parse_query`].
struct ValidatedQuery {
    /// Output columns from `SELECT ... LIMIT 0`.
    columns: Vec<ColumnDef>,
    /// Full DVM parse result (only for DIFFERENTIAL/IMMEDIATE non-TopK).
    parsed_tree: Option<crate::dvm::ParseResult>,
    /// TopK metadata (if ORDER BY + LIMIT pattern detected).
    topk_info: Option<crate::dvm::TopKInfo>,
    /// The effective defining query (base query for TopK, original otherwise).
    effective_query: String,
    /// Whether the query needs `__pgt_count` (aggregate/distinct).
    needs_pgt_count: bool,
    /// Whether the query needs `__pgt_count_l`/`__pgt_count_r` (INTERSECT/EXCEPT).
    needs_dual_count: bool,
    /// Whether the query needs `__pgt_count` for UNION dedup.
    needs_union_dedup: bool,
    /// Whether any source table lacks a PRIMARY KEY (EC-06).
    has_keyless_source: bool,
    /// Source relation OIDs and types extracted from the query.
    source_relids: Vec<(pg_sys::Oid, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CdcModeRequestSource {
    GlobalGuc,
    ExplicitOverride,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CdcRefreshModeInteraction {
    None,
    IgnoreWalForImmediate,
    RejectWalForImmediate,
}

fn classify_cdc_refresh_mode_interaction(
    refresh_mode: RefreshMode,
    requested_cdc_mode: &str,
    source: CdcModeRequestSource,
) -> CdcRefreshModeInteraction {
    if !refresh_mode.is_immediate() || !requested_cdc_mode.eq_ignore_ascii_case("wal") {
        return CdcRefreshModeInteraction::None;
    }

    match source {
        CdcModeRequestSource::GlobalGuc => CdcRefreshModeInteraction::IgnoreWalForImmediate,
        CdcModeRequestSource::ExplicitOverride => CdcRefreshModeInteraction::RejectWalForImmediate,
    }
}

fn enforce_cdc_refresh_mode_interaction(
    stream_table_name: &str,
    refresh_mode: RefreshMode,
    requested_cdc_mode: &str,
    source: CdcModeRequestSource,
) -> Result<(), PgTrickleError> {
    match classify_cdc_refresh_mode_interaction(refresh_mode, requested_cdc_mode, source) {
        CdcRefreshModeInteraction::None => Ok(()),
        CdcRefreshModeInteraction::IgnoreWalForImmediate => {
            pgrx::info!(
                "pg_trickle: cdc_mode 'wal' has no effect for IMMEDIATE refresh mode on {} — using IVM triggers instead of CDC.",
                stream_table_name,
            );
            Ok(())
        }
        CdcRefreshModeInteraction::RejectWalForImmediate => Err(PgTrickleError::InvalidArgument(
            "refresh_mode = 'IMMEDIATE' is incompatible with cdc_mode = 'wal'. \
             IMMEDIATE uses in-transaction IVM triggers; WAL-based CDC is async. \
             Use cdc_mode = 'trigger' or 'auto', or choose a deferred refresh_mode."
                .to_string(),
        )),
    }
}

fn normalize_requested_cdc_mode(
    requested_cdc_mode: Option<&str>,
) -> Result<Option<String>, PgTrickleError> {
    requested_cdc_mode
        .map(|mode| {
            let normalized = mode.trim().to_lowercase();
            match normalized.as_str() {
                "auto" | "trigger" | "wal" => Ok(normalized),
                other => Err(PgTrickleError::InvalidArgument(format!(
                    "invalid cdc_mode value: '{}' (expected 'auto', 'trigger', or 'wal')",
                    other
                ))),
            }
        })
        .transpose()
}

fn resolve_requested_cdc_mode(
    requested_cdc_mode: Option<&str>,
) -> Result<(Option<String>, String, CdcModeRequestSource), PgTrickleError> {
    let requested_override = normalize_requested_cdc_mode(requested_cdc_mode)?;
    let source = if requested_override.is_some() {
        CdcModeRequestSource::ExplicitOverride
    } else {
        CdcModeRequestSource::GlobalGuc
    };
    let effective = requested_override
        .clone()
        .unwrap_or_else(config::pg_trickle_cdc_mode);
    Ok((requested_override, effective, source))
}

fn resolve_requested_cdc_mode_for_st(
    st: &StreamTableMeta,
    requested_cdc_mode: Option<&str>,
) -> Result<(Option<String>, String, CdcModeRequestSource), PgTrickleError> {
    let requested_override = match normalize_requested_cdc_mode(requested_cdc_mode)? {
        Some(mode) => Some(mode),
        None => st.requested_cdc_mode.clone(),
    };
    let source = if requested_override.is_some() {
        CdcModeRequestSource::ExplicitOverride
    } else {
        CdcModeRequestSource::GlobalGuc
    };
    let effective = requested_override
        .clone()
        .unwrap_or_else(config::pg_trickle_cdc_mode);
    Ok((requested_override, effective, source))
}

fn validate_requested_cdc_mode_requirements(
    requested_cdc_mode: &str,
) -> Result<(), PgTrickleError> {
    if requested_cdc_mode != "wal" {
        return Ok(());
    }

    if !cdc::can_use_logical_replication_for_mode(requested_cdc_mode)? {
        return Err(PgTrickleError::InvalidArgument(
            "cdc_mode = 'wal' requires wal_level = logical and an available replication slot"
                .to_string(),
        ));
    }

    Ok(())
}

/// Validate a rewritten query and parse it for DVM. This runs the LIMIT 0
/// check, TopK detection, unsupported construct rejection, DVM parsing,
/// volatility checks, and source relation extraction.
///
/// When `is_auto` is true and the query cannot be maintained incrementally,
/// `refresh_mode` is downgraded to `RefreshMode::Full` instead of returning
/// an error.
fn validate_and_parse_query(
    query: &str,
    refresh_mode: &mut RefreshMode,
    is_auto: bool,
) -> Result<ValidatedQuery, PgTrickleError> {
    // Validate the defining query by running LIMIT 0
    let columns = validate_defining_query(query)?;

    // TopK detection — must run BEFORE reject_limit_offset
    let topk_info = crate::dvm::detect_topk_pattern(query)?;

    let (effective_query, topk_info) = if let Some(info) = topk_info {
        if let Some(offset) = info.offset_value {
            pgrx::info!(
                "pg_trickle: TopK pattern detected (ORDER BY {} LIMIT {} OFFSET {}). \
                 The stream table will maintain rows {}–{}.",
                info.order_by_sql,
                info.limit_value,
                offset,
                offset + 1,
                offset + info.limit_value,
            );
        } else {
            pgrx::info!(
                "pg_trickle: TopK pattern detected (ORDER BY {} LIMIT {}). \
                 The stream table will maintain the top {} rows.",
                info.order_by_sql,
                info.limit_value,
                info.limit_value,
            );
        }
        let base = info.base_query.clone();
        (base, Some(info))
    } else {
        (query.to_string(), None)
    };
    let q = if topk_info.is_some() {
        &effective_query
    } else {
        query
    };

    // Reject LIMIT / OFFSET (TopK already handled above).
    crate::dvm::reject_limit_offset(q)?;
    crate::dvm::warn_limit_without_order_in_subqueries(q);

    // Reject unsupported constructs (NATURAL JOIN, subquery expressions).
    // AUTO mode: downgrade to FULL instead of erroring.
    if let Err(e) = crate::dvm::reject_unsupported_constructs(q) {
        if is_auto {
            pgrx::info!(
                "Query uses constructs not supported by differential maintenance ({}); \
                 using FULL refresh mode. See docs/DVM_OPERATORS.md for supported operators.",
                e
            );
            *refresh_mode = RefreshMode::Full;
        } else {
            return Err(e);
        }
    }

    // Reject matviews/foreign tables in DIFFERENTIAL/IMMEDIATE.
    // AUTO mode: downgrade to FULL if matviews/foreign tables are present.
    if (*refresh_mode == RefreshMode::Differential || *refresh_mode == RefreshMode::Immediate)
        && let Err(e) = crate::dvm::reject_materialized_views(q)
    {
        if is_auto && *refresh_mode == RefreshMode::Differential {
            pgrx::info!(
                "Query references materialized views or foreign tables ({}); \
                 using FULL refresh mode.",
                e
            );
            *refresh_mode = RefreshMode::Full;
        } else {
            return Err(e);
        }
    }

    // IMMEDIATE mode: TopK limit threshold check
    if let (true, Some(info)) = (refresh_mode.is_immediate(), &topk_info) {
        let topk_limit = info.limit_value;
        let max_limit = crate::config::PGS_IVM_TOPK_MAX_LIMIT.get() as i64;
        if max_limit == 0 || topk_limit > max_limit {
            return Err(PgTrickleError::UnsupportedOperator(format!(
                "ORDER BY + LIMIT {topk_limit} (TopK) exceeds the IMMEDIATE mode threshold \
                 (pg_trickle.ivm_topk_max_limit = {max_limit}). TopK tables in IMMEDIATE mode \
                 recompute the top-K rows on every DML statement. Reduce LIMIT, raise the \
                 threshold, or use 'DIFFERENTIAL' mode for large TopK queries."
            )));
        }
        pgrx::warning!(
            "pg_trickle: TopK (LIMIT {}) in IMMEDIATE mode uses micro-refresh — \
             the top-{} rows are recomputed on every DML statement. \
             This adds latency proportional to the defining query cost.",
            topk_limit,
            topk_limit
        );
    }

    // IMMEDIATE mode: query restriction validation
    if refresh_mode.is_immediate() {
        crate::dvm::validate_immediate_mode_support(q)?;
    }

    // DVM parse for DIFFERENTIAL/IMMEDIATE (non-TopK).
    // AUTO mode: if DVM parsing fails, downgrade to FULL instead of erroring.
    let parsed_tree = if (*refresh_mode == RefreshMode::Differential
        || *refresh_mode == RefreshMode::Immediate)
        && topk_info.is_none()
    {
        match crate::dvm::parse_defining_query_full(q) {
            Ok(tree) => Some(tree),
            Err(e) if is_auto && *refresh_mode == RefreshMode::Differential => {
                pgrx::info!(
                    "Query cannot use differential maintenance ({}); \
                     using FULL refresh mode. See docs/DVM_OPERATORS.md for supported operators.",
                    e
                );
                *refresh_mode = RefreshMode::Full;
                None
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    // Volatility check
    if let Some(ref pr) = parsed_tree {
        let vol = crate::dvm::tree_worst_volatility_with_registry(pr)?;
        match vol {
            'v' => {
                return Err(PgTrickleError::UnsupportedOperator(
                    "Defining query contains volatile expressions (e.g., random(), \
                     clock_timestamp(), or custom volatile operators). Volatile \
                     functions and operators are not supported in DIFFERENTIAL or \
                     IMMEDIATE mode because they produce different values on each \
                     evaluation, breaking delta computation. Use FULL refresh mode \
                     instead, or replace with a deterministic alternative."
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
    }

    let needs_pgt_count = parsed_tree
        .as_ref()
        .is_some_and(|pr| pr.tree.needs_pgt_count());
    let needs_dual_count = parsed_tree
        .as_ref()
        .is_some_and(|pr| pr.tree.needs_dual_count());
    let needs_union_dedup = parsed_tree
        .as_ref()
        .is_some_and(|pr| pr.tree.needs_union_dedup_count());

    // Extract source dependencies
    let source_relids = extract_source_relations(q)?;

    // EC-06: Detect keyless sources
    let has_keyless_source = source_relids.iter().any(|(oid, source_type)| {
        if source_type != "TABLE" && source_type != "FOREIGN_TABLE" {
            return false;
        }
        cdc::resolve_pk_columns(*oid)
            .map(|pk| pk.is_empty())
            .unwrap_or(false)
    });

    Ok(ValidatedQuery {
        columns,
        parsed_tree,
        topk_info,
        effective_query,
        needs_pgt_count,
        needs_dual_count,
        needs_union_dedup,
        has_keyless_source,
        source_relids,
    })
}

/// Set up the storage table: CREATE TABLE, row_id index, DML guard trigger,
/// and optional GROUP BY composite index.
#[allow(clippy::too_many_arguments)]
fn setup_storage_table(
    schema: &str,
    table_name: &str,
    columns: &[ColumnDef],
    needs_pgt_count: bool,
    needs_dual_count: bool,
    has_keyless_source: bool,
    refresh_mode: RefreshMode,
    parsed_tree: Option<&crate::dvm::ParseResult>,
) -> Result<pg_sys::Oid, PgTrickleError> {
    let storage_needs_pgt_count = needs_pgt_count;
    let storage_ddl = build_create_table_sql(
        schema,
        table_name,
        columns,
        storage_needs_pgt_count,
        needs_dual_count,
    );
    Spi::run(&storage_ddl)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to create storage table: {}", e)))?;

    let pgt_relid = get_table_oid(schema, table_name)?;

    // Create index on __pgt_row_id (EC-06: non-unique for keyless sources)
    let index_sql = if has_keyless_source {
        format!(
            "CREATE INDEX ON {}.{} (__pgt_row_id)",
            quote_identifier(schema),
            quote_identifier(table_name),
        )
    } else {
        format!(
            "CREATE UNIQUE INDEX ON {}.{} (__pgt_row_id)",
            quote_identifier(schema),
            quote_identifier(table_name),
        )
    };
    Spi::run(&index_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to create row_id index: {}", e)))?;

    // DML guard trigger
    install_dml_guard_trigger(schema, table_name)?;

    // GROUP BY composite index for aggregate queries in DIFFERENTIAL mode
    if refresh_mode == RefreshMode::Differential
        && let Some(pr) = parsed_tree
        && let Some(group_cols) = pr.tree.group_by_columns()
        && !group_cols.is_empty()
    {
        let quoted_cols: Vec<String> = group_cols
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect();
        let group_index_sql = format!(
            "CREATE INDEX ON {}.{} ({})",
            quote_identifier(schema),
            quote_identifier(table_name),
            quoted_cols.join(", "),
        );
        Spi::run(&group_index_sql).map_err(|e| {
            PgTrickleError::SpiError(format!("Failed to create group-by index: {}", e))
        })?;
    }

    Ok(pgt_relid)
}

/// Insert catalog entry and dependency edges for a new stream table.
#[allow(clippy::too_many_arguments)]
fn insert_catalog_and_deps(
    pgt_relid: pg_sys::Oid,
    schema: &str,
    table_name: &str,
    defining_query: &str,
    original_query: Option<&str>,
    schedule: Option<String>,
    refresh_mode: RefreshMode,
    vq: &ValidatedQuery,
    dc: DiamondConsistency,
    dsp: DiamondSchedulePolicy,
    requested_cdc_mode: Option<&str>,
    is_append_only: bool,
) -> Result<i64, PgTrickleError> {
    let pgt_id = StreamTableMeta::insert(
        pgt_relid,
        table_name,
        schema,
        defining_query,
        original_query,
        schedule,
        refresh_mode,
        vq.parsed_tree.as_ref().map(|pr| pr.functions_used()),
        vq.topk_info.as_ref().map(|i| i.limit_value as i32),
        vq.topk_info.as_ref().map(|i| i.order_by_sql.as_str()),
        vq.topk_info
            .as_ref()
            .and_then(|i| i.offset_value.map(|v| v as i32)),
        dc,
        dsp,
        vq.has_keyless_source,
        requested_cdc_mode,
        is_append_only,
    )?;

    // Build per-source column usage map
    let columns_used_map = vq
        .parsed_tree
        .as_ref()
        .map(|pr| pr.source_columns_used())
        .unwrap_or_default();

    // Insert dependency edges with column snapshots
    for (source_oid, source_type) in &vq.source_relids {
        let cols = columns_used_map.get(&source_oid.to_u32()).cloned();

        let (snapshot, fingerprint) = if source_type == "TABLE" || source_type == "FOREIGN_TABLE" {
            match crate::catalog::build_column_snapshot(*source_oid) {
                Ok((s, f)) => (Some(s), Some(f)),
                Err(e) => {
                    pgrx::debug1!(
                        "pg_trickle: failed to build column snapshot for source {}: {}",
                        source_oid.to_u32(),
                        e,
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        StDependency::insert_with_snapshot(
            pgt_id,
            *source_oid,
            source_type,
            cols,
            snapshot,
            fingerprint,
        )?;
    }

    Ok(pgt_id)
}

/// Set up CDC or IVM trigger infrastructure on source tables.
fn setup_trigger_infrastructure(
    source_relids: &[(pg_sys::Oid, String)],
    refresh_mode: RefreshMode,
    pgt_id: i64,
    pgt_relid: pg_sys::Oid,
    defining_query: &str,
) -> Result<(), PgTrickleError> {
    let change_schema = config::pg_trickle_change_buffer_schema();
    if refresh_mode.is_immediate() {
        let lock_mode = crate::ivm::IvmLockMode::for_query(defining_query);
        for (source_oid, source_type) in source_relids {
            if source_type == "TABLE" {
                crate::ivm::setup_ivm_triggers(*source_oid, pgt_id, pgt_relid, lock_mode)?;
            }
        }
    } else {
        for (source_oid, source_type) in source_relids {
            if source_type == "TABLE" {
                setup_cdc_for_source(*source_oid, pgt_id, &change_schema)?;
            } else if source_type == "FOREIGN_TABLE" {
                cdc::setup_foreign_table_polling(*source_oid, pgt_id, &change_schema)?;
            }
        }
    }
    Ok(())
}

// ── Schema comparison for ALTER QUERY ──────────────────────────────────────

/// Classification of how the output schema changed between old and new query.
#[derive(Debug)]
enum SchemaChange {
    /// Column names, types, and count are identical — fast path.
    Same,
    /// Columns added or removed; surviving columns have compatible types.
    Compatible {
        added: Vec<ColumnDef>,
        removed: Vec<String>,
    },
    /// Column type changed incompatibly — requires full storage rebuild.
    Incompatible { reason: String },
}

/// Compare old vs new output column schemas to classify the change.
fn classify_schema_change(old: &[ColumnDef], new: &[ColumnDef]) -> SchemaChange {
    // Build lookup by name for old columns
    let old_map: std::collections::HashMap<&str, &ColumnDef> =
        old.iter().map(|c| (c.name.as_str(), c)).collect();
    let new_map: std::collections::HashMap<&str, &ColumnDef> =
        new.iter().map(|c| (c.name.as_str(), c)).collect();

    // Check for type incompatibilities on surviving columns
    for new_col in new {
        if let Some(old_col) = old_map.get(new_col.name.as_str())
            && old_col.type_oid != new_col.type_oid
        {
            // Check if PostgreSQL has an implicit cast
            let can_cast = Spi::get_one_with_args::<bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_cast \
                 WHERE castsource = $1 AND casttarget = $2 \
                 AND castcontext = 'i')",
                &[
                    old_col.type_oid.value().into(),
                    new_col.type_oid.value().into(),
                ],
            )
            .unwrap_or(Some(false))
            .unwrap_or(false);

            if !can_cast {
                return SchemaChange::Incompatible {
                    reason: format!(
                        "column '{}' type changed from OID {} to {} (no implicit cast)",
                        new_col.name,
                        old_col.type_oid.value(),
                        new_col.type_oid.value(),
                    ),
                };
            }
        }
    }

    // Identify added and removed columns
    let added: Vec<ColumnDef> = new
        .iter()
        .filter(|c| !old_map.contains_key(c.name.as_str()))
        .cloned()
        .collect();
    let removed: Vec<String> = old
        .iter()
        .filter(|c| !new_map.contains_key(c.name.as_str()))
        .map(|c| c.name.clone())
        .collect();

    if added.is_empty() && removed.is_empty() {
        // Check ordering — if column order changed, treat as Compatible
        // (no DDL needed, but we track the difference)
        let same_order = old.len() == new.len()
            && old
                .iter()
                .zip(new.iter())
                .all(|(o, n)| o.name == n.name && o.type_oid == n.type_oid);
        if same_order {
            SchemaChange::Same
        } else {
            // Types compatible but order changed — treated as Same since
            // column order in storage doesn't affect correctness
            SchemaChange::Same
        }
    } else {
        SchemaChange::Compatible { added, removed }
    }
}

// ── Dependency diffing for ALTER QUERY ────────────────────────────────────

/// Result of diffing old vs new source dependencies.
struct DependencyDiff {
    /// Sources present in new query but not old.
    added: Vec<(pg_sys::Oid, String)>,
    /// Sources present in old query but not new.
    removed: Vec<(pg_sys::Oid, String)>,
    /// Sources present in both old and new queries.
    kept: Vec<(pg_sys::Oid, String)>,
}

/// Compute which source dependencies were added, removed, or kept.
fn diff_dependencies(
    old_deps: &[StDependency],
    new_sources: &[(pg_sys::Oid, String)],
) -> DependencyDiff {
    let old_oids: std::collections::HashSet<u32> =
        old_deps.iter().map(|d| d.source_relid.to_u32()).collect();
    let new_oids: std::collections::HashSet<u32> =
        new_sources.iter().map(|(o, _)| o.to_u32()).collect();

    let added = new_sources
        .iter()
        .filter(|(o, _)| !old_oids.contains(&o.to_u32()))
        .cloned()
        .collect();
    let removed = old_deps
        .iter()
        .filter(|d| !new_oids.contains(&d.source_relid.to_u32()))
        .map(|d| (d.source_relid, d.source_type.clone()))
        .collect();
    let kept = new_sources
        .iter()
        .filter(|(o, _)| old_oids.contains(&o.to_u32()))
        .cloned()
        .collect();

    DependencyDiff {
        added,
        removed,
        kept,
    }
}

// ── Storage table migration for ALTER QUERY ──────────────────────────────

/// Migrate the storage table schema for a Compatible schema change.
/// For Same schema, this is a no-op. For Incompatible, the caller
/// must drop and recreate the storage table.
fn migrate_storage_table_compatible(
    schema: &str,
    table_name: &str,
    added: &[ColumnDef],
    removed: &[String],
) -> Result<(), PgTrickleError> {
    let quoted_table = format!(
        "{}.{}",
        quote_identifier(schema),
        quote_identifier(table_name),
    );

    // Add new columns
    for col in added {
        let type_name = match col.type_oid {
            PgOid::Invalid => "text".to_string(),
            oid => {
                Spi::get_one_with_args::<String>("SELECT $1::regtype::text", &[oid.value().into()])
                    .unwrap_or(Some("text".to_string()))
                    .unwrap_or_else(|| "text".to_string())
            }
        };
        let add_sql = format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            quoted_table,
            quote_identifier(&col.name),
            type_name,
        );
        Spi::run(&add_sql).map_err(|e| {
            PgTrickleError::SpiError(format!("Failed to add column '{}': {}", col.name, e))
        })?;
    }

    // Drop removed columns
    for col_name in removed {
        let drop_sql = format!(
            "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
            quoted_table,
            quote_identifier(col_name),
        );
        Spi::run(&drop_sql).map_err(|e| {
            PgTrickleError::SpiError(format!("Failed to drop column '{}': {}", col_name, e))
        })?;
    }

    Ok(())
}

/// Manage auxiliary count columns (__pgt_count, __pgt_count_l, __pgt_count_r)
/// during ALTER QUERY when the query type changes (e.g., flat → aggregate).
fn migrate_aux_columns(
    schema: &str,
    table_name: &str,
    old_needs_pgt_count: bool,
    old_needs_dual_count: bool,
    new_needs_pgt_count: bool,
    new_needs_dual_count: bool,
    new_needs_union_dedup: bool,
) -> Result<(), PgTrickleError> {
    let quoted_table = format!(
        "{}.{}",
        quote_identifier(schema),
        quote_identifier(table_name),
    );

    let new_storage_needs_pgt_count = new_needs_pgt_count || new_needs_union_dedup;

    // Transition: __pgt_count
    if !old_needs_pgt_count && new_storage_needs_pgt_count && !new_needs_dual_count {
        Spi::run(&format!(
            "ALTER TABLE {} ADD COLUMN IF NOT EXISTS __pgt_count BIGINT NOT NULL DEFAULT 0",
            quoted_table
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    } else if old_needs_pgt_count && !new_storage_needs_pgt_count && !new_needs_dual_count {
        Spi::run(&format!(
            "ALTER TABLE {} DROP COLUMN IF EXISTS __pgt_count",
            quoted_table
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    // Transition: __pgt_count_l / __pgt_count_r
    if !old_needs_dual_count && new_needs_dual_count {
        Spi::run(&format!(
            "ALTER TABLE {} ADD COLUMN IF NOT EXISTS __pgt_count_l BIGINT NOT NULL DEFAULT 0",
            quoted_table
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Spi::run(&format!(
            "ALTER TABLE {} ADD COLUMN IF NOT EXISTS __pgt_count_r BIGINT NOT NULL DEFAULT 0",
            quoted_table
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        // Drop __pgt_count if it was there and no longer needed
        if old_needs_pgt_count {
            Spi::run(&format!(
                "ALTER TABLE {} DROP COLUMN IF EXISTS __pgt_count",
                quoted_table
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    } else if old_needs_dual_count && !new_needs_dual_count {
        Spi::run(&format!(
            "ALTER TABLE {} DROP COLUMN IF EXISTS __pgt_count_l",
            quoted_table
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        Spi::run(&format!(
            "ALTER TABLE {} DROP COLUMN IF EXISTS __pgt_count_r",
            quoted_table
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        // Add __pgt_count if newly needed
        if new_storage_needs_pgt_count {
            Spi::run(&format!(
                "ALTER TABLE {} ADD COLUMN IF NOT EXISTS __pgt_count BIGINT NOT NULL DEFAULT 0",
                quoted_table
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }

    Ok(())
}

// ── Core ALTER QUERY implementation ──────────────────────────────────────

/// Perform an in-place query migration on an existing stream table.
/// Called from `alter_stream_table_impl` when `query` is `Some(...)`.
///
/// Executes Phases 0–5 from the ALTER QUERY design:
///   0. Validate & classify
///   1. Suspend & drain
///   2. Tear down old infrastructure
///   3. Migrate storage table
///   4. Update catalog & set up new infrastructure
///   5. Repopulate
fn alter_stream_table_query(
    st: &StreamTableMeta,
    schema: &str,
    table_name: &str,
    new_query: &str,
) -> Result<(), PgTrickleError> {
    // ── Phase 0: Validate & classify ──

    // Run the full rewrite pipeline on the new query
    let original_new_query = new_query.to_string();
    let rewritten_query = run_query_rewrite_pipeline(new_query)?;

    // Determine the effective refresh mode — use the ST's current mode
    let mut refresh_mode = st.refresh_mode;

    // Validate and parse the new query
    let vq = validate_and_parse_query(&rewritten_query, &mut refresh_mode, false)?;

    // Cycle detection on the new dependency set (ALTER-aware: replaces
    // the existing ST's edges rather than creating a sentinel node)
    check_for_cycles_alter(st.pgt_id, &vq.source_relids)?;

    // Get the current storage table columns (excluding internal __pgt_* columns)
    let old_columns = get_storage_table_columns(schema, table_name)?;

    // Classify schema change
    let schema_change = classify_schema_change(&old_columns, &vq.columns);

    // Diff source dependencies
    let old_deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();
    let dep_diff = diff_dependencies(&old_deps, &vq.source_relids);

    // Detect old auxiliary column state from current ST metadata
    let old_needs_pgt_count = crate::dvm::query_needs_pgt_count(&st.defining_query);
    let old_needs_dual_count = crate::dvm::query_needs_dual_count(&st.defining_query);

    // ── Phase 1: Suspend ──
    StreamTableMeta::update_status(st.pgt_id, StStatus::Suspended)?;

    // Flush pending deferred cleanups for sources being removed
    let removed_oids: Vec<u32> = dep_diff.removed.iter().map(|(o, _)| o.to_u32()).collect();
    if !removed_oids.is_empty() {
        crate::refresh::flush_pending_cleanups_for_oids(&removed_oids);
    }

    // ── Phase 2: Tear down old infrastructure ──

    // Remove CDC/IVM triggers from sources that are no longer needed
    for (source_oid, source_type) in &dep_diff.removed {
        if source_type == "TABLE" {
            if refresh_mode.is_immediate() {
                if let Err(e) = crate::ivm::cleanup_ivm_triggers(*source_oid, st.pgt_id) {
                    pgrx::warning!(
                        "Failed to clean up IVM triggers for removed source {}: {}",
                        source_oid.to_u32(),
                        e
                    );
                }
            } else {
                let old_dep = old_deps.iter().find(|d| d.source_relid == *source_oid);
                let cdc_mode = old_dep.map(|d| d.cdc_mode).unwrap_or(CdcMode::Trigger);
                if let Err(e) = cleanup_cdc_for_source(*source_oid, cdc_mode, Some(st.pgt_id)) {
                    pgrx::warning!(
                        "Failed to clean up CDC for removed source {}: {}",
                        source_oid.to_u32(),
                        e
                    );
                }
            }
        }
    }

    // Invalidate caches
    shmem::bump_cache_generation();

    // Flush MERGE template cache and deallocate prepared statements
    refresh::invalidate_merge_cache(st.pgt_id);

    // ── Phase 3: Migrate storage table ──

    let new_pgt_relid = match &schema_change {
        SchemaChange::Same => {
            // No DDL required
            st.pgt_relid
        }
        SchemaChange::Compatible { added, removed } => {
            migrate_storage_table_compatible(schema, table_name, added, removed)?;
            st.pgt_relid
        }
        SchemaChange::Incompatible { reason } => {
            pgrx::warning!(
                "pg_trickle: ALTER QUERY requires full storage rebuild: {}. \
                 The storage table OID will change.",
                reason
            );

            // Detach pgt_relid before DROP so the sql_drop event trigger
            // does not recognise the table as ST storage and delete the
            // catalog row.
            Spi::run_with_args(
                "UPDATE pgtrickle.pgt_stream_tables \
                 SET pgt_relid = 0 WHERE pgt_id = $1",
                &[st.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

            // Drop existing storage table
            let drop_sql = format!(
                "DROP TABLE IF EXISTS {}.{} CASCADE",
                quote_identifier(schema),
                quote_identifier(table_name),
            );
            Spi::run(&drop_sql).map_err(|e| {
                PgTrickleError::SpiError(format!("Failed to drop storage table: {}", e))
            })?;

            // Recreate with new schema
            let storage_needs_pgt_count = vq.needs_pgt_count || vq.needs_union_dedup;
            setup_storage_table(
                schema,
                table_name,
                &vq.columns,
                storage_needs_pgt_count,
                vq.needs_dual_count,
                vq.has_keyless_source,
                refresh_mode,
                vq.parsed_tree.as_ref(),
            )?
        }
    };

    // For Same/Compatible, also handle auxiliary column transitions
    if !matches!(schema_change, SchemaChange::Incompatible { .. }) {
        migrate_aux_columns(
            schema,
            table_name,
            old_needs_pgt_count,
            old_needs_dual_count,
            vq.needs_pgt_count,
            vq.needs_dual_count,
            vq.needs_union_dedup,
        )?;
    }

    // ── Phase 4: Update catalog & set up new infrastructure ──

    // Compute the effective defining query for storage — TopK stores the base query
    let defining_query = if vq.topk_info.is_some() {
        &vq.effective_query
    } else {
        &rewritten_query
    };

    // Update the pgt_stream_tables catalog row
    let original_query_opt = if original_new_query != *defining_query {
        Some(original_new_query.as_str())
    } else {
        None
    };

    let functions_used = vq.parsed_tree.as_ref().map(|pr| pr.functions_used());
    let topk_limit = vq.topk_info.as_ref().map(|i| i.limit_value as i32);
    let topk_order_by_owned = vq.topk_info.as_ref().map(|i| i.order_by_sql.clone());
    let topk_order_by = topk_order_by_owned.as_deref();
    let topk_offset = vq
        .topk_info
        .as_ref()
        .and_then(|i| i.offset_value.map(|v| v as i32));

    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_stream_tables SET \
         pgt_relid = $1, \
         defining_query = $2, \
         original_query = $3, \
         functions_used = $4, \
         topk_limit = $5, \
         topk_order_by = $6, \
         topk_offset = $7, \
         needs_reinit = false, \
         frontier = NULL, \
         is_populated = false, \
         has_keyless_source = $8, \
         updated_at = now() \
         WHERE pgt_id = $9",
        &[
            new_pgt_relid.into(),
            defining_query.into(),
            original_query_opt.into(),
            functions_used.into(),
            topk_limit.into(),
            topk_order_by.into(),
            topk_offset.into(),
            vq.has_keyless_source.into(),
            st.pgt_id.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // Delete old dependency rows and insert new ones
    StDependency::delete_for_st(st.pgt_id)?;

    let columns_used_map = vq
        .parsed_tree
        .as_ref()
        .map(|pr| pr.source_columns_used())
        .unwrap_or_default();

    for (source_oid, source_type) in &vq.source_relids {
        let cols = columns_used_map.get(&source_oid.to_u32()).cloned();
        let (snapshot, fingerprint) = if source_type == "TABLE" || source_type == "FOREIGN_TABLE" {
            match crate::catalog::build_column_snapshot(*source_oid) {
                Ok((s, f)) => (Some(s), Some(f)),
                Err(e) => {
                    pgrx::debug1!(
                        "pg_trickle: failed to build column snapshot for source {}: {}",
                        source_oid.to_u32(),
                        e,
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        StDependency::insert_with_snapshot(
            st.pgt_id,
            *source_oid,
            source_type,
            cols,
            snapshot,
            fingerprint,
        )?;
    }

    // Set up CDC/IVM triggers for newly added sources
    let change_schema = config::pg_trickle_change_buffer_schema();
    for (source_oid, source_type) in &dep_diff.added {
        if source_type == "TABLE" {
            if refresh_mode.is_immediate() {
                let lock_mode = crate::ivm::IvmLockMode::for_query(defining_query);
                crate::ivm::setup_ivm_triggers(*source_oid, st.pgt_id, new_pgt_relid, lock_mode)?;
            } else {
                setup_cdc_for_source(*source_oid, st.pgt_id, &change_schema)?;
            }
        } else if source_type == "FOREIGN_TABLE" && !refresh_mode.is_immediate() {
            cdc::setup_foreign_table_polling(*source_oid, st.pgt_id, &change_schema)?;
        }
    }

    // Register view soft-dependencies if view inlining was applied
    if original_query_opt.is_some()
        && let Ok(original_sources) = extract_source_relations(&original_new_query)
    {
        for (src_oid, src_type) in &original_sources {
            if src_type == "VIEW" {
                let already_registered = vq.source_relids.iter().any(|(o, _)| o == src_oid);
                if !already_registered {
                    StDependency::insert_with_snapshot(
                        st.pgt_id, *src_oid, src_type, None, None, None,
                    )?;
                }
            }
        }
    }

    // Signal DAG rebuild and cache invalidation
    shmem::signal_dag_rebuild();
    shmem::bump_cache_generation();

    // ── Phase 5: Repopulate ──

    // Execute a full refresh to populate the storage table with new query results
    let source_oids: Vec<pg_sys::Oid> = vq
        .source_relids
        .iter()
        .filter(|(_, t)| t == "TABLE")
        .map(|(o, _)| *o)
        .collect();

    // Re-load ST with updated metadata for the refresh
    let updated_st = StreamTableMeta::get_by_name(schema, table_name)?;
    execute_manual_full_refresh(&updated_st, schema, table_name, &source_oids)?;

    // Re-activate the stream table
    StreamTableMeta::update_status(st.pgt_id, StStatus::Active)?;

    // Pre-warm delta SQL + MERGE template cache for DIFFERENTIAL mode
    if refresh_mode == RefreshMode::Differential {
        let st = StreamTableMeta::get_by_name(schema, table_name)?;
        refresh::prewarm_merge_cache(&st);
    }

    // ERG-F: warn so the client sees the full refresh regardless of log_min_messages.
    pgrx::warning!(
        "pg_trickle: stream table {}.{} ALTER QUERY applied a full refresh \
         (schema change: {}). This may take time on large tables.",
        schema,
        table_name,
        match &schema_change {
            SchemaChange::Same => "same",
            SchemaChange::Compatible { .. } => "compatible",
            SchemaChange::Incompatible { .. } => "incompatible (full rebuild)",
        }
    );

    Ok(())
}

/// Get the user-visible columns of a storage table (excluding __pgt_* internal columns).
fn get_storage_table_columns(
    schema: &str,
    table_name: &str,
) -> Result<Vec<ColumnDef>, PgTrickleError> {
    Spi::connect(|client| {
        let table = client
            .select(
                "SELECT a.attname::text, a.atttypid \
                 FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 AND c.relname = $2 \
                 AND a.attnum > 0 AND NOT a.attisdropped \
                 AND a.attname NOT LIKE '__pgt_%' \
                 ORDER BY a.attnum",
                None,
                &[schema.into(), table_name.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        let mut columns = Vec::new();
        for row in table {
            let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());
            let name = row.get::<String>(1).map_err(map_spi)?.unwrap_or_default();
            let type_oid_raw = row
                .get::<pg_sys::Oid>(2)
                .map_err(map_spi)?
                .unwrap_or(pg_sys::InvalidOid);
            columns.push(ColumnDef {
                name,
                type_oid: PgOid::from(type_oid_raw),
            });
        }
        Ok(columns)
    })
}

#[allow(clippy::too_many_arguments)]
fn create_stream_table_impl(
    name: &str,
    query: &str,
    schedule: Option<&str>,
    refresh_mode_str: &str,
    initialize: bool,
    diamond_consistency: Option<&str>,
    diamond_schedule_policy: Option<&str>,
    requested_cdc_mode: Option<&str>,
    append_only: bool,
) -> Result<(), PgTrickleError> {
    let is_auto = RefreshMode::is_auto_str(refresh_mode_str);
    let mut refresh_mode = RefreshMode::from_str(refresh_mode_str)?;

    // Parse diamond consistency — default to 'none' when not specified
    let dc = match diamond_consistency {
        Some(s) => {
            let val = s.to_lowercase();
            match val.as_str() {
                "none" | "atomic" => DiamondConsistency::from_sql_str(&val),
                other => {
                    return Err(PgTrickleError::InvalidArgument(format!(
                        "invalid diamond_consistency value: '{}' (expected 'none' or 'atomic')",
                        other
                    )));
                }
            }
        }
        None => DiamondConsistency::Atomic,
    };

    // Parse diamond schedule policy — default to 'fastest' when not specified
    let dsp = match diamond_schedule_policy {
        Some(s) => match DiamondSchedulePolicy::from_sql_str(s) {
            Some(p) => p,
            None => {
                return Err(PgTrickleError::InvalidArgument(format!(
                    "invalid diamond_schedule_policy value: '{}' (expected 'fastest' or 'slowest')",
                    s
                )));
            }
        },
        None => DiamondSchedulePolicy::Fastest,
    };

    // Parse schema.name
    let (schema, table_name) = parse_qualified_name(name)?;
    let qualified_name = format!("{schema}.{table_name}");

    // Parse and validate schedule
    let schedule_str = if refresh_mode.is_immediate() {
        None
    } else {
        match schedule {
            Some(s) if s.trim().eq_ignore_ascii_case("calculated") => None,
            Some(s) => {
                let _schedule = parse_schedule(s)?;
                Some(s.trim().to_string())
            }
            None => {
                return Err(PgTrickleError::InvalidArgument(
                    "use 'calculated' instead of NULL to set CALCULATED schedule".to_string(),
                ));
            }
        }
    };

    let (requested_cdc_mode_override, effective_requested_cdc_mode, cdc_mode_source) =
        resolve_requested_cdc_mode(requested_cdc_mode)?;
    enforce_cdc_refresh_mode_interaction(
        &qualified_name,
        refresh_mode,
        &effective_requested_cdc_mode,
        cdc_mode_source,
    )?;
    if !refresh_mode.is_immediate() {
        validate_requested_cdc_mode_requirements(&effective_requested_cdc_mode)?;
    }

    // ── Query rewrite pipeline ─────────────────────────────────────
    let original_query = query.to_string();
    let query = &run_query_rewrite_pipeline(query)?;

    // ── Validate & parse ───────────────────────────────────────────
    let vq = validate_and_parse_query(query, &mut refresh_mode, is_auto)?;
    // Warnings
    warn_source_table_properties(&vq.source_relids);
    warn_select_star(query);

    // Validate append_only flag
    if append_only {
        if refresh_mode == RefreshMode::Full {
            return Err(PgTrickleError::InvalidArgument(
                "append_only is not supported with FULL refresh mode. \
                 Use DIFFERENTIAL or AUTO refresh mode."
                    .to_string(),
            ));
        }
        if refresh_mode.is_immediate() {
            return Err(PgTrickleError::InvalidArgument(
                "append_only is not supported with IMMEDIATE refresh mode. \
                 Use DIFFERENTIAL or AUTO refresh mode."
                    .to_string(),
            ));
        }
        if vq.has_keyless_source {
            return Err(PgTrickleError::InvalidArgument(
                "append_only is not supported for stream tables with keyless sources. \
                 Add a PRIMARY KEY to all source tables first."
                    .to_string(),
            ));
        }
    }

    // Check for duplicate
    if StreamTableMeta::get_by_name(&schema, &table_name).is_ok() {
        return Err(PgTrickleError::AlreadyExists(format!(
            "{}.{}",
            schema, table_name
        )));
    }

    // Cycle detection
    check_for_cycles(&vq.source_relids)?;

    // ── Phase 1: DDL ──

    // Create storage table, indexes, and DML guard trigger
    let storage_needs_pgt_count = vq.needs_pgt_count || vq.needs_union_dedup;
    let pgt_relid = setup_storage_table(
        &schema,
        &table_name,
        &vq.columns,
        storage_needs_pgt_count,
        vq.needs_dual_count,
        vq.has_keyless_source,
        refresh_mode,
        vq.parsed_tree.as_ref(),
    )?;

    // Insert catalog entry + dependency edges
    // For TopK, store the base query (ORDER BY/LIMIT stripped) as defining_query.
    // The ORDER BY, LIMIT, and OFFSET are stored separately as topk_order_by,
    // topk_limit, and topk_offset.
    let catalog_defining_query = if vq.topk_info.is_some() {
        &vq.effective_query
    } else {
        query
    };
    let original_query_opt = if original_query != *catalog_defining_query {
        Some(original_query.as_str())
    } else {
        None
    };
    let pgt_id = insert_catalog_and_deps(
        pgt_relid,
        &schema,
        &table_name,
        catalog_defining_query,
        original_query_opt,
        schedule_str,
        refresh_mode,
        &vq,
        dc,
        dsp,
        requested_cdc_mode_override.as_deref(),
        append_only,
    )?;

    // ── Phase 2: CDC / IVM trigger setup ──
    setup_trigger_infrastructure(&vq.source_relids, refresh_mode, pgt_id, pgt_relid, query)?;

    // ── Phase 2b: Register view soft-dependencies for DDL tracking ──
    if original_query_opt.is_some()
        && let Ok(original_sources) = extract_source_relations(&original_query)
    {
        for (src_oid, src_type) in &original_sources {
            if src_type == "VIEW" {
                let already_registered = vq.source_relids.iter().any(|(o, _)| o == src_oid);
                if !already_registered {
                    StDependency::insert_with_snapshot(
                        pgt_id, *src_oid, src_type, None, None, None,
                    )?;
                }
            }
        }
    }

    // Initialize if requested
    if initialize {
        let t_init = Instant::now();
        initialize_st(
            &schema,
            &table_name,
            query,
            pgt_id,
            &vq.columns,
            vq.needs_pgt_count,
            vq.needs_dual_count,
            vq.needs_union_dedup,
            vq.topk_info.as_ref(),
        )?;
        let init_ms = t_init.elapsed().as_secs_f64() * 1000.0;

        // Record initial full materialization time so the adaptive
        // threshold auto-tuner has a FULL baseline from the very first
        // differential refresh.  Without this, `last_full_ms` stays NULL
        // and the auto-tuner never activates for STs whose change rate
        // stays below the fallback threshold.
        if refresh_mode == RefreshMode::Differential
            && let Err(e) = StreamTableMeta::update_adaptive_threshold(pgt_id, None, Some(init_ms))
        {
            pgrx::debug1!("[pg_trickle] Failed to record initial last_full_ms: {}", e);
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
        "Stream table {}.{} created (pgt_id={}, mode={}, initialized={})",
        schema,
        table_name,
        pgt_id,
        refresh_mode.as_str(),
        initialize
    );

    Ok(())
}

/// Alter properties of an existing stream table.
#[allow(clippy::too_many_arguments)]
#[pg_extern(schema = "pgtrickle")]
fn alter_stream_table(
    name: &str,
    query: default!(Option<&str>, "NULL"),
    schedule: default!(Option<&str>, "NULL"),
    refresh_mode: default!(Option<&str>, "NULL"),
    status: default!(Option<&str>, "NULL"),
    diamond_consistency: default!(Option<&str>, "NULL"),
    diamond_schedule_policy: default!(Option<&str>, "NULL"),
    cdc_mode: default!(Option<&str>, "NULL"),
    append_only: default!(Option<bool>, "NULL"),
) {
    let result = alter_stream_table_impl(
        name,
        query,
        schedule,
        refresh_mode,
        status,
        diamond_consistency,
        diamond_schedule_policy,
        cdc_mode,
        append_only,
    );
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

#[allow(clippy::too_many_arguments)]
fn alter_stream_table_impl(
    name: &str,
    query: Option<&str>,
    schedule: Option<&str>,
    refresh_mode: Option<&str>,
    status: Option<&str>,
    diamond_consistency: Option<&str>,
    diamond_schedule_policy: Option<&str>,
    cdc_mode: Option<&str>,
    append_only: Option<bool>,
) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let mut st = StreamTableMeta::get_by_name(&schema, &table_name)?;
    let qualified_name = format!("{schema}.{table_name}");

    // ── Query migration (must run first, before other parameter changes) ──
    if let Some(new_query) = query {
        alter_stream_table_query(&st, &schema, &table_name, new_query)?;
        st = StreamTableMeta::get_by_name(&schema, &table_name)?;
    }

    let (requested_cdc_mode_override, effective_requested_cdc_mode, cdc_mode_source) =
        resolve_requested_cdc_mode_for_st(&st, cdc_mode)?;
    let target_refresh_mode = match refresh_mode {
        Some(mode_str) => RefreshMode::from_str(mode_str)?,
        None => st.refresh_mode,
    };

    enforce_cdc_refresh_mode_interaction(
        &qualified_name,
        target_refresh_mode,
        &effective_requested_cdc_mode,
        cdc_mode_source,
    )?;
    if !target_refresh_mode.is_immediate() {
        validate_requested_cdc_mode_requirements(&effective_requested_cdc_mode)?;
    }

    if requested_cdc_mode_override != st.requested_cdc_mode {
        StreamTableMeta::update_requested_cdc_mode(
            st.pgt_id,
            requested_cdc_mode_override.as_deref(),
        )?;
        st.requested_cdc_mode = requested_cdc_mode_override.clone();
    }

    if let Some(val) = schedule {
        if val.trim().eq_ignore_ascii_case("calculated") {
            // Switch to CALCULATED mode (NULL schedule in catalog)
            Spi::run_with_args(
                "UPDATE pgtrickle.pgt_stream_tables SET schedule = NULL, updated_at = now() WHERE pgt_id = $1",
                &[st.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        } else {
            let _schedule = parse_schedule(val)?;
            let trimmed = val.trim();
            Spi::run_with_args(
                "UPDATE pgtrickle.pgt_stream_tables SET schedule = $1, updated_at = now() WHERE pgt_id = $2",
                &[trimmed.into(), st.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }

    if let Some(mode_str) = refresh_mode {
        let new_mode = target_refresh_mode;
        let old_mode = st.refresh_mode;

        if new_mode != old_mode {
            // ── Validate mode switch ────────────────────────────────
            // TopK tables: check limit threshold for IMMEDIATE mode.
            if let (true, Some(topk_limit)) = (new_mode.is_immediate(), st.topk_limit) {
                let topk_limit = topk_limit as i64;
                let max_limit = crate::config::PGS_IVM_TOPK_MAX_LIMIT.get() as i64;
                if max_limit == 0 || topk_limit > max_limit {
                    return Err(PgTrickleError::UnsupportedOperator(format!(
                        "Cannot switch TopK stream table (LIMIT {topk_limit}) to IMMEDIATE mode. \
                         Exceeds pg_trickle.ivm_topk_max_limit = {max_limit}. Raise the threshold \
                         or keep using DIFFERENTIAL/FULL mode."
                    )));
                }
            }

            // Validate query restrictions for IMMEDIATE mode.
            if new_mode.is_immediate() {
                crate::dvm::validate_immediate_mode_support(&st.defining_query)?;
            }

            // Get dependencies for trigger migration.
            let deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();
            let change_schema = config::pg_trickle_change_buffer_schema();

            // ── Tear down OLD mode's infrastructure ─────────────────
            match old_mode {
                RefreshMode::Immediate => {
                    // Drop IVM triggers from source tables.
                    for dep in &deps {
                        if dep.source_type == "TABLE"
                            && let Err(e) =
                                crate::ivm::cleanup_ivm_triggers(dep.source_relid, st.pgt_id)
                        {
                            pgrx::warning!(
                                "Failed to clean up IVM triggers for oid {}: {}",
                                dep.source_relid.to_u32(),
                                e
                            );
                        }
                    }
                }
                RefreshMode::Full | RefreshMode::Differential => {
                    // Drop CDC triggers + change buffer tables from source
                    // tables (only if switching TO IMMEDIATE; FULL↔DIFF
                    // keeps CDC infrastructure).
                    if new_mode.is_immediate() {
                        for dep in &deps {
                            if dep.source_type == "TABLE"
                                && let Err(e) = cleanup_cdc_for_source(
                                    dep.source_relid,
                                    dep.cdc_mode,
                                    Some(st.pgt_id),
                                )
                            {
                                pgrx::warning!(
                                    "Failed to clean up CDC for oid {}: {}",
                                    dep.source_relid.to_u32(),
                                    e
                                );
                            }
                        }
                    }
                }
            }

            // ── Set up NEW mode's infrastructure ────────────────────
            match new_mode {
                RefreshMode::Immediate => {
                    // Install IVM triggers on source tables.
                    let lock_mode = crate::ivm::IvmLockMode::for_query(&st.defining_query);
                    for dep in &deps {
                        if dep.source_type == "TABLE" {
                            crate::ivm::setup_ivm_triggers(
                                dep.source_relid,
                                st.pgt_id,
                                st.pgt_relid,
                                lock_mode,
                            )?;
                        }
                    }
                    // Clear schedule for IMMEDIATE mode.
                    Spi::run_with_args(
                        "UPDATE pgtrickle.pgt_stream_tables SET schedule = NULL WHERE pgt_id = $1",
                        &[st.pgt_id.into()],
                    )
                    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
                }
                RefreshMode::Full | RefreshMode::Differential => {
                    // If switching FROM IMMEDIATE, recreate CDC triggers.
                    if old_mode.is_immediate() {
                        for dep in &deps {
                            if dep.source_type == "TABLE" {
                                setup_cdc_for_source(dep.source_relid, st.pgt_id, &change_schema)?;
                            }
                        }
                        // Restore a default schedule if none is set.
                        if schedule.is_none() {
                            Spi::run_with_args(
                                "UPDATE pgtrickle.pgt_stream_tables \
                                 SET schedule = COALESCE(schedule, '1m') \
                                 WHERE pgt_id = $1",
                                &[st.pgt_id.into()],
                            )
                            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
                        }
                    }
                }
            }

            // ── Update catalog ──────────────────────────────────────
            Spi::run_with_args(
                "UPDATE pgtrickle.pgt_stream_tables \
                 SET refresh_mode = $1, updated_at = now() WHERE pgt_id = $2",
                &[mode_str.to_uppercase().into(), st.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

            // ── Full refresh to ensure consistency ──────────────────
            let source_oids: Vec<pg_sys::Oid> = deps
                .iter()
                .filter(|d| d.source_type == "TABLE")
                .map(|d| d.source_relid)
                .collect();
            // Re-load ST with updated mode for the refresh dispatch.
            let updated_st = StreamTableMeta::get_by_name(&schema, &table_name)?;
            execute_manual_full_refresh(&updated_st, &schema, &table_name, &source_oids)?;

            // ERG-F: warn so the client sees the implicit full refresh regardless of log_min_messages.
            pgrx::warning!(
                "pg_trickle: stream table {}.{} refresh mode changed from {} to {}; \
                 a full refresh was applied. This may take time on large tables.",
                schema,
                table_name,
                old_mode.as_str(),
                new_mode.as_str(),
            );
        } else {
            // Same mode — just update catalog (no-op but harmless).
            Spi::run_with_args(
                "UPDATE pgtrickle.pgt_stream_tables \
                 SET refresh_mode = $1, updated_at = now() WHERE pgt_id = $2",
                &[mode_str.to_uppercase().into(), st.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }

    if cdc_mode.is_some() && !target_refresh_mode.is_immediate() {
        let deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();
        let change_schema = config::pg_trickle_change_buffer_schema();
        for dep in &deps {
            if dep.source_type == "TABLE" {
                setup_cdc_for_source(dep.source_relid, st.pgt_id, &change_schema)?;
            }
        }
        pgrx::info!(
            "Stream table {}.{} updated requested cdc_mode to {}",
            schema,
            table_name,
            effective_requested_cdc_mode,
        );
    }

    if let Some(status_str) = status {
        let new_status = StStatus::from_str(&status_str.to_uppercase())?;
        StreamTableMeta::update_status(st.pgt_id, new_status)?;
        if new_status == StStatus::Active {
            // Reset errors when resuming
            Spi::run_with_args(
                "UPDATE pgtrickle.pgt_stream_tables SET consecutive_errors = 0, updated_at = now() WHERE pgt_id = $1",
                &[st.pgt_id.into()],
            )
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }

    if let Some(dc_str) = diamond_consistency {
        let val = dc_str.to_lowercase();
        match val.as_str() {
            "none" | "atomic" => {
                let dc = DiamondConsistency::from_sql_str(&val);
                StreamTableMeta::set_diamond_consistency(st.pgt_id, dc)?;
            }
            other => {
                return Err(PgTrickleError::InvalidArgument(format!(
                    "invalid diamond_consistency value: '{}' (expected 'none' or 'atomic')",
                    other
                )));
            }
        }
    }

    if let Some(dsp_str) = diamond_schedule_policy {
        match DiamondSchedulePolicy::from_sql_str(dsp_str) {
            Some(p) => {
                StreamTableMeta::set_diamond_schedule_policy(st.pgt_id, p)?;
            }
            None => {
                return Err(PgTrickleError::InvalidArgument(format!(
                    "invalid diamond_schedule_policy value: '{}' (expected 'fastest' or 'slowest')",
                    dsp_str
                )));
            }
        }
    }

    if let Some(ao) = append_only {
        let effective_mode = match refresh_mode {
            Some(mode_str) => RefreshMode::from_str(mode_str)?,
            None => st.refresh_mode,
        };
        if ao {
            if effective_mode == RefreshMode::Full {
                return Err(PgTrickleError::InvalidArgument(
                    "append_only is not supported with FULL refresh mode.".to_string(),
                ));
            }
            if effective_mode.is_immediate() {
                return Err(PgTrickleError::InvalidArgument(
                    "append_only is not supported with IMMEDIATE refresh mode.".to_string(),
                ));
            }
            if st.has_keyless_source {
                return Err(PgTrickleError::InvalidArgument(
                    "append_only is not supported for stream tables with keyless sources."
                        .to_string(),
                ));
            }
        }
        StreamTableMeta::update_append_only(st.pgt_id, ao)?;
    }

    shmem::signal_dag_rebuild();
    // G8.1: Notify other backends to flush delta/MERGE template caches.
    shmem::bump_cache_generation();
    Ok(())
}

/// Drop a stream table, removing the storage table and all catalog entries.
#[pg_extern(schema = "pgtrickle")]
fn drop_stream_table(name: &str) {
    let result = drop_stream_table_impl(name);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn drop_stream_table_impl(name: &str) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    // CASCADE: drop all stream tables that depend on this one first.
    // `get_downstream_pgt_ids` finds STs whose defining queries read from
    // this ST's storage table.  We iterate by pgt_id to avoid re-querying
    // after each recursive drop changes the catalog.
    let downstream_ids = StDependency::get_downstream_pgt_ids(st.pgt_relid)?;
    for downstream_id in downstream_ids {
        if let Some(downstream_st) = StreamTableMeta::get_by_id(downstream_id)? {
            let qualified = format!("{}.{}", downstream_st.pgt_schema, downstream_st.pgt_name);
            drop_stream_table_impl(&qualified)?;
        }
    }

    // Get dependencies before deleting catalog entries
    let deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();

    // Flush any deferred change-buffer cleanup entries that reference
    // source OIDs about to be cleaned up.  This prevents
    // `drain_pending_cleanups` on the next refresh from attempting to
    // access change-buffer tables that no longer exist.
    let dep_oids: Vec<u32> = deps
        .iter()
        .filter(|d| d.source_type == "TABLE")
        .map(|d| d.source_relid.to_u32())
        .collect();
    crate::refresh::flush_pending_cleanups_for_oids(&dep_oids);

    // Drop the storage table
    let drop_sql = format!(
        "DROP TABLE IF EXISTS {}.{} CASCADE",
        quote_identifier(&schema),
        quote_identifier(&table_name),
    );
    Spi::run(&drop_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to drop storage table: {}", e)))?;

    // Delete catalog entries (cascade handles pgt_dependencies)
    StreamTableMeta::delete(st.pgt_id)?;

    // Clean up CDC resources (triggers, WAL slots, publications) for
    // sources no longer tracked by any ST. For IMMEDIATE-mode STs, clean
    // up IVM triggers instead.
    for dep in &deps {
        if dep.source_type == "TABLE" {
            if st.refresh_mode.is_immediate() {
                if let Err(e) = crate::ivm::cleanup_ivm_triggers(dep.source_relid, st.pgt_id) {
                    pgrx::warning!(
                        "Failed to clean up IVM triggers for oid {}: {}",
                        dep.source_relid.to_u32(),
                        e
                    );
                }
            } else {
                cleanup_cdc_for_source(dep.source_relid, dep.cdc_mode, None)?;
            }
        }
    }

    // Signal scheduler
    shmem::signal_dag_rebuild();
    // G8.1: Notify other backends to flush delta/MERGE template caches.
    shmem::bump_cache_generation();

    pgrx::info!(
        "Stream table {}.{} dropped (pgt_id={})",
        schema,
        table_name,
        st.pgt_id
    );
    Ok(())
}

/// Resume a suspended stream table, clearing its consecutive error count and
/// re-enabling automated and manual refreshes.
#[pg_extern(schema = "pgtrickle")]
fn resume_stream_table(name: &str) {
    let result = resume_stream_table_impl(name);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn resume_stream_table_impl(name: &str) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    if st.status != StStatus::Suspended {
        return Err(PgTrickleError::InvalidArgument(format!(
            "stream table {}.{} is not suspended (current status: {})",
            schema,
            table_name,
            st.status.as_str(),
        )));
    }

    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET status = 'ACTIVE', consecutive_errors = 0, updated_at = now() \
         WHERE pgt_id = $1",
        &[st.pgt_id.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    crate::monitor::alert_resumed(&schema, &table_name);

    pgrx::info!(
        "Stream table {}.{} resumed (pgt_id={})",
        schema,
        table_name,
        st.pgt_id
    );
    Ok(())
}

/// Manually trigger a synchronous refresh of a stream table.
#[pg_extern(schema = "pgtrickle")]
fn refresh_stream_table(name: &str) {
    let result = refresh_stream_table_impl(name);
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}

fn refresh_stream_table_impl(name: &str) -> Result<(), PgTrickleError> {
    // F16 (G8.2): Block manual refresh on read replicas — writes are not possible.
    let is_replica = Spi::get_one::<bool>("SELECT pg_is_in_recovery()")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or(false);
    if is_replica {
        return Err(PgTrickleError::InvalidArgument(
            "Cannot refresh stream tables on a read replica. \
             The server is in recovery mode (pg_is_in_recovery() = true). \
             Run refresh on the primary server instead."
                .into(),
        ));
    }

    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    // Phase 10: Check if ST is suspended — refuse manual refresh
    if st.status == StStatus::Suspended {
        return Err(PgTrickleError::InvalidArgument(format!(
            "stream table {}.{} is suspended; use pgtrickle.resume_stream_table('{}') first",
            schema,
            table_name,
            if schema == "public" {
                table_name.clone()
            } else {
                format!("{}.{}", schema, table_name)
            },
        )));
    }

    // ── Fast no-op exit for DIFFERENTIAL mode ────────────────────────
    // Before acquiring the advisory lock, check if any source table has
    // pending changes. If not, skip the entire refresh pipeline (lock,
    // frontier computation, DVM, cleanup) — just update the timestamp.
    //
    // G-N3 optimization: source OIDs are fetched once and reused.
    let source_oids = get_source_oids_for_manual_refresh(st.pgt_id)?;

    // Phase 10: Advisory lock to prevent concurrent refresh.
    // Use the transaction-scoped variant so the lock is automatically
    // released when the transaction commits or rolls back — including on
    // error-triggered rollbacks where a subsequent session-level
    // pg_advisory_unlock() SPI call would silently no-op.
    let got_lock =
        Spi::get_one_with_args::<bool>("SELECT pg_try_advisory_xact_lock($1)", &[st.pgt_id.into()])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
            .unwrap_or(false);

    if !got_lock {
        return Err(PgTrickleError::RefreshSkipped(format!(
            "{}.{} — another refresh is already in progress",
            schema, table_name,
        )));
    }

    // Transaction-level advisory lock is released automatically at
    // transaction end (commit or rollback); no explicit unlock needed.
    execute_manual_refresh(&st, &schema, &table_name, &source_oids)
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
) -> Result<(), PgTrickleError> {
    // EC-25/EC-26: Set the internal_refresh flag so DML guard triggers
    // allow the refresh executor to modify the storage table.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // R3: Bypass RLS for the refresh defining query so the stream table
    // always materializes the full result set regardless of who called
    // refresh_stream_table(). This mirrors REFRESH MATERIALIZED VIEW
    // semantics and prevents the "who refreshed it?" correctness hazard.
    Spi::run("SET LOCAL row_security = off")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // TopK tables use the scoped-recomputation refresh path regardless of
    // refresh_mode (they always do ORDER BY … LIMIT N via MERGE).
    if st.topk_limit.is_some() {
        let (rows_inserted, rows_deleted) = refresh::execute_topk_refresh(st)?;
        pgrx::info!(
            "Stream table {}.{} refreshed (TopK MERGE: +{} -{})",
            schema,
            table_name,
            rows_inserted,
            rows_deleted,
        );
        return Ok(());
    }

    match st.refresh_mode {
        RefreshMode::Full => execute_manual_full_refresh(st, schema, table_name, source_oids),
        RefreshMode::Differential => {
            execute_manual_differential_refresh(st, schema, table_name, source_oids)
        }
        RefreshMode::Immediate => {
            // For IMMEDIATE mode, manual refresh does a FULL refresh
            // (re-populate from the defining query), same as pg_ivm's
            // refresh_immv(name, true).
            execute_manual_full_refresh(st, schema, table_name, source_oids)
        }
    }
}

/// Execute a FULL manual refresh: truncate + repopulate from the defining query.
///
/// When user triggers are detected (and the GUC is not `"off"`), they are
/// suppressed during the TRUNCATE + INSERT via `DISABLE TRIGGER USER` /
/// `ENABLE TRIGGER USER`. A `NOTIFY pgtrickle_refresh` is emitted so
/// listeners know a FULL refresh occurred.
fn execute_manual_full_refresh(
    st: &StreamTableMeta,
    schema: &str,
    table_name: &str,
    source_oids: &[pg_sys::Oid],
) -> Result<(), PgTrickleError> {
    // EC-25/EC-26: Ensure the internal_refresh flag is set so DML guard
    // triggers allow the refresh executor to modify the storage table.
    // This is needed when called directly (e.g., from alter_stream_table)
    // without going through execute_manual_refresh.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    let quoted_table = format!(
        "{}.{}",
        quote_identifier(schema),
        quote_identifier(table_name),
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

    let truncate_sql = format!("TRUNCATE {quoted_table}");
    Spi::run(&truncate_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // For aggregate/distinct STs in DIFFERENTIAL mode, inject COUNT(*)
    // into the defining query so __pgt_count is populated for subsequent
    // differential refreshes.
    let effective_query = if st.refresh_mode == RefreshMode::Differential
        && crate::dvm::query_needs_pgt_count(&st.defining_query)
    {
        inject_pgt_count(&st.defining_query)
    } else {
        st.defining_query.clone()
    };

    // Compute row_id using the same hash formula as the delta query so
    // the MERGE ON clause matches during subsequent differential refreshes.
    // For INTERSECT/EXCEPT, compute per-branch multiplicities for dual-count
    // storage. For UNION (dedup), convert to UNION ALL and count.
    // For UNION ALL, decompose into per-branch subqueries with
    // child-prefixed row IDs matching diff_union_all's formula.
    let insert_body = if crate::dvm::query_needs_dual_count(&st.defining_query) {
        let col_names = crate::dvm::get_defining_query_columns(&st.defining_query)?;
        if let Some(set_op_sql) = crate::dvm::try_set_op_refresh_sql(&st.defining_query, &col_names)
        {
            set_op_sql
        } else {
            let row_id_expr = crate::dvm::row_id_expr_for_query(&st.defining_query);
            format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({effective_query}) sub",)
        }
    } else if crate::dvm::query_needs_union_dedup_count(&st.defining_query) {
        let col_names = crate::dvm::get_defining_query_columns(&st.defining_query)?;
        if let Some(union_sql) =
            crate::dvm::try_union_dedup_refresh_sql(&st.defining_query, &col_names)
        {
            union_sql
        } else {
            let row_id_expr = crate::dvm::row_id_expr_for_query(&st.defining_query);
            format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({effective_query}) sub",)
        }
    } else if let Some(ua_sql) = crate::dvm::try_union_all_refresh_sql(&st.defining_query) {
        ua_sql
    } else {
        let row_id_expr = crate::dvm::row_id_expr_for_query(&st.defining_query);
        format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({effective_query}) sub",)
    };

    let insert_sql = format!("INSERT INTO {quoted_table} {insert_body}");
    Spi::run(&insert_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // Re-enable user triggers and emit NOTIFY so listeners know a FULL
    // refresh occurred.
    if has_triggers {
        Spi::run(&format!("ALTER TABLE {quoted_table} ENABLE TRIGGER USER"))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        let escaped_name = table_name.replace('\'', "''");
        let escaped_schema = schema.replace('\'', "''");
        Spi::run(&format!(
            "NOTIFY pgtrickle_refresh, '{{\"stream_table\": \"{escaped_name}\", \
             \"schema\": \"{escaped_schema}\", \"mode\": \"FULL\"}}'"
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        pgrx::info!(
            "pg_trickle: FULL refresh of {}.{} with user triggers suppressed.",
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
    StreamTableMeta::store_frontier_and_complete_refresh(st.pgt_id, &frontier, 0)?;

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
) -> Result<(), PgTrickleError> {
    // If the ST has never been refreshed (frontier is None), fall back to
    // a FULL refresh to establish the baseline frontier.
    //
    // NOTE: we check `st.frontier.is_none()` rather than
    // `prev_frontier.is_empty()` because STs whose sources are other stream
    // tables (not raw tables) produce frontiers with `sources = {}` — those
    // frontiers are non-default but still "empty" in the WAL-source sense.
    // Without this distinction the first manual refresh would ALWAYS be a
    // full refresh for ST-on-ST, bumping data_timestamp on every call.
    if st.frontier.is_none() {
        pgrx::info!(
            "Stream table {}.{}: no previous frontier, performing FULL refresh first",
            schema,
            table_name
        );
        return execute_manual_full_refresh(st, schema, table_name, source_oids);
    }

    let prev_frontier = st.frontier.clone().unwrap_or_default();

    // For STs whose sources are all STREAM_TABLEs (frontier.sources is empty
    // because there are no WAL change buffers to track), use upstream
    // data_timestamp comparison.  Only run a FULL refresh (and bump
    // data_timestamp) if an upstream has newer data.  If no upstream is newer,
    // skip the refresh entirely to avoid spurious data_timestamp drift.
    if prev_frontier.is_empty() {
        let any_upstream_newer = has_upstream_stream_table_changes(st.pgt_id)?;
        if any_upstream_newer {
            return execute_manual_full_refresh(st, schema, table_name, source_oids);
        } else {
            pgrx::info!(
                "Stream table {}.{} refreshed (no-op: upstream unchanged)",
                schema,
                table_name,
            );
            return Ok(());
        }
    }

    refresh::poll_foreign_table_sources_for_st(st)?;

    // Mixed-dependency guard: if ANY upstream is a STREAM_TABLE, the DVM
    // delta SQL will reference change buffers that don't exist for stream
    // tables (they have no CDC triggers). Fall back to FULL refresh,
    // consistent with how the scheduler handles these (scheduler.rs:1077).
    {
        let deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();
        let has_st_source = deps.iter().any(|dep| dep.source_type == "STREAM_TABLE");
        if has_st_source {
            let any_st_upstream_newer = has_upstream_stream_table_changes(st.pgt_id)?;

            // Check TABLE source change buffers for pending rows.
            let change_schema =
                crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
            let any_table_changes = deps
                .iter()
                .filter(|dep| dep.source_type == "TABLE" || dep.source_type == "FOREIGN_TABLE")
                .any(|dep| {
                    Spi::get_one::<bool>(&format!(
                        "SELECT EXISTS(SELECT 1 FROM \"{}\".changes_{} LIMIT 1)",
                        change_schema,
                        dep.source_relid.to_u32(),
                    ))
                    .unwrap_or(Some(false))
                    .unwrap_or(false)
                });

            if any_st_upstream_newer || any_table_changes {
                return execute_manual_full_refresh(st, schema, table_name, source_oids);
            } else {
                pgrx::info!(
                    "Stream table {}.{} refreshed (no-op: upstream unchanged)",
                    schema,
                    table_name,
                );
                return Ok(());
            }
        }
    }

    // Get current WAL positions (reuses source_oids from caller — G-N3)
    let slot_positions = cdc::get_slot_positions(source_oids)?;
    let data_ts = get_data_timestamp_str();
    let new_frontier = version::compute_new_frontier(&slot_positions, &data_ts);

    // Execute the differential refresh via the DVM engine
    let (rows_inserted, rows_deleted) =
        refresh::execute_differential_refresh(st, &prev_frontier, &new_frontier)?;

    // Store the new frontier and mark refresh complete in a single SPI call (S3).
    // Matches scheduler behavior: only update data_timestamp when rows were
    // actually written — a no-op differential must not advance data_timestamp
    // or downstream CALCULATED stream tables would see a false "upstream changed"
    // signal and trigger unnecessary refreshes.
    if rows_inserted > 0 || rows_deleted > 0 {
        StreamTableMeta::store_frontier_and_complete_refresh(
            st.pgt_id,
            &new_frontier,
            rows_inserted,
        )?;
    } else {
        // No rows changed — store frontier to advance past processed WAL range,
        // but preserve data_timestamp to avoid spurious downstream wakeups.
        StreamTableMeta::store_frontier(st.pgt_id, &new_frontier)?;
        StreamTableMeta::update_after_no_data_refresh(st.pgt_id)?;
    }

    pgrx::info!(
        "Stream table {}.{} refreshed (DIFFERENTIAL: +{} -{})",
        schema,
        table_name,
        rows_inserted,
        rows_deleted,
    );
    Ok(())
}

/// Check if any STREAM_TABLE upstream has a `data_timestamp` more recent than
/// the given ST's own `data_timestamp`.
///
/// Mirrors the scheduler's `has_stream_table_source_changes()` — used by the
/// manual refresh path to avoid bumping `data_timestamp` on no-op refreshes
/// for calculated (ST-on-ST) stream tables.
fn has_upstream_stream_table_changes(pgt_id: i64) -> Result<bool, PgTrickleError> {
    let deps = StDependency::get_for_st(pgt_id)?;
    for dep in deps {
        if dep.source_type != "STREAM_TABLE" {
            continue;
        }
        let upstream_newer = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS ( \
               SELECT 1 \
               FROM pgtrickle.pgt_stream_tables upstream \
               JOIN pgtrickle.pgt_stream_tables us ON us.pgt_id = {pgt_id} \
               WHERE upstream.pgt_relid = {relid}::oid \
                 AND upstream.data_timestamp \
                     > COALESCE(us.data_timestamp, '-infinity'::timestamptz) \
             )",
            relid = dep.source_relid.to_u32(),
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .unwrap_or(false);

        if upstream_newer {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Get source table OIDs for a stream table (used by manual refresh path).
fn get_source_oids_for_manual_refresh(pgt_id: i64) -> Result<Vec<pg_sys::Oid>, PgTrickleError> {
    let deps = StDependency::get_for_st(pgt_id)?;
    Ok(deps
        .into_iter()
        .filter(|dep| dep.source_type == "TABLE" || dep.source_type == "FOREIGN_TABLE")
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

/// Return the pg_trickle extension version (from `Cargo.toml`).
///
/// This matches the version reported by `pg_extension.extversion`.
#[pg_extern(schema = "pgtrickle", immutable, parallel_safe)]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Bump the shared-memory DAG rebuild signal.
///
/// Called automatically at the end of `CREATE EXTENSION pg_trickle` so the
/// launcher background worker notices the new install and spawns a scheduler
/// immediately, rather than waiting for the 5-minute skip TTL.
///
/// Also safe to call manually if the launcher needs a nudge.
#[pg_extern(schema = "pgtrickle")]
fn _signal_launcher_rescan() {
    crate::shmem::signal_dag_rebuild();
}

/// Rebuild all CDC triggers (function body + trigger DDL) for every source
/// table tracked by pg_trickle.
///
/// Called automatically during `ALTER EXTENSION pg_trickle UPDATE` (0.3.0 →
/// 0.4.0) to migrate existing row-level CDC triggers to statement-level.
/// Can also be called manually after changing `pg_trickle.cdc_trigger_mode`.
///
/// Returns `'done'` on success. Emits a `WARNING` per table on error and
/// continues processing remaining sources.
#[pg_extern(schema = "pgtrickle")]
fn rebuild_cdc_triggers() -> &'static str {
    let change_schema = config::pg_trickle_change_buffer_schema();

    let source_oids: Vec<pg_sys::Oid> = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT DISTINCT source_relid \
                 FROM pgtrickle.pgt_dependencies \
                 WHERE source_type = 'TABLE'",
                None,
                &[],
            )
            .map_err(|e| crate::error::PgTrickleError::SpiError(e.to_string()))?;
        let mut oids = Vec::new();
        for row in result {
            let oid = row
                .get::<pg_sys::Oid>(1)
                .unwrap_or(None)
                .unwrap_or(pg_sys::InvalidOid);
            if oid != pg_sys::InvalidOid {
                oids.push(oid);
            }
        }
        Ok::<_, crate::error::PgTrickleError>(oids)
    })
    .unwrap_or_default();

    for source_oid in source_oids {
        if let Err(e) = cdc::rebuild_cdc_trigger(source_oid, &change_schema) {
            pgrx::warning!(
                "pg_trickle: rebuild_cdc_triggers: failed for OID {}: {}",
                source_oid.to_u32(),
                e
            );
        }
    }

    "done"
}

/// Parse a Prometheus/GNU-style duration string and return seconds.
///
/// Used by SQL views to compare schedule. Returns NULL for invalid input
/// (cron expressions should not be passed).
#[pg_extern(schema = "pgtrickle", immutable, parallel_safe)]
fn parse_duration_seconds(input: &str) -> Option<i64> {
    parse_duration(input).ok()
}

/// Get the status of all stream tables.
///
/// Returns a summary row per stream table including schedule configuration,
/// data timestamp, and computed staleness interval.
#[pg_extern(schema = "pgtrickle", name = "pgt_status")]
#[allow(clippy::type_complexity)]
fn pgt_status() -> TableIterator<
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
                "SELECT pgt_schema || '.' || pgt_name, status, refresh_mode, \
                 is_populated, consecutive_errors, schedule, data_timestamp, \
                 now() - data_timestamp AS staleness \
                 FROM pgtrickle.pgt_stream_tables ORDER BY pgt_schema, pgt_name",
                None,
                &[],
            )
            .map_err(|e| pgrx::error!("st_list: SPI select failed: {e}"))
            .expect("unreachable after error!()");

        let mut out = Vec::new();
        for row in result {
            let name = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let status = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let mode = row.get::<String>(3).unwrap_or(None).unwrap_or_default();
            let populated = row.get::<bool>(4).unwrap_or(None).unwrap_or(false);
            let errors = row.get::<i32>(5).unwrap_or(None).unwrap_or(0);
            let schedule = row.get::<String>(6).unwrap_or(None);
            let data_ts = row.get::<TimestampWithTimeZone>(7).unwrap_or(None);
            let staleness = row.get::<pgrx::datum::Interval>(8).unwrap_or(None);
            out.push((
                name, status, mode, populated, errors, schedule, data_ts, staleness,
            ));
        }
        out
    });

    TableIterator::new(rows)
}

/// Show detected diamond consistency groups.
///
/// Returns one row per group member, indicating which group it belongs to,
/// whether it is a convergence (fan-in) node, the group's current epoch,
/// and the effective schedule policy.
#[pg_extern(schema = "pgtrickle", name = "diamond_groups")]
#[allow(clippy::type_complexity)]
fn diamond_groups() -> TableIterator<
    'static,
    (
        name!(group_id, i32),
        name!(member_name, String),
        name!(member_schema, String),
        name!(is_convergence, bool),
        name!(epoch, i64),
        name!(schedule_policy, String),
    ),
> {
    let rows: Vec<_> = Spi::connect(|_client| {
        let dag = match StDag::build_from_catalog(config::pg_trickle_default_schedule_seconds()) {
            Ok(d) => d,
            Err(e) => {
                pgrx::warning!("diamond_groups: failed to build DAG: {}", e);
                return Vec::new();
            }
        };

        let groups = dag.compute_consistency_groups();
        let mut out = Vec::new();

        for (idx, group) in groups.iter().enumerate() {
            // Skip singletons — they are not in a diamond.
            if group.is_singleton() {
                continue;
            }

            let group_id = (idx + 1) as i32;
            let convergence_set: std::collections::HashSet<_> =
                group.convergence_points.iter().collect();

            // Compute effective schedule policy for the group.
            let effective_policy = group.convergence_points.iter().fold(
                DiamondSchedulePolicy::Fastest,
                |acc, node| {
                    if let NodeId::StreamTable(id) = node {
                        StreamTableMeta::get_all_active()
                            .ok()
                            .and_then(|sts| sts.into_iter().find(|s| s.pgt_id == *id))
                            .map(|st| acc.stricter(st.diamond_schedule_policy))
                            .unwrap_or(acc)
                    } else {
                        acc
                    }
                },
            );

            for member in &group.members {
                if let NodeId::StreamTable(pgt_id) = member {
                    // Load metadata for name/schema
                    let (name, schema) = match StreamTableMeta::get_all_active() {
                        Ok(sts) => {
                            if let Some(st) = sts.iter().find(|s| s.pgt_id == *pgt_id) {
                                (st.pgt_name.clone(), st.pgt_schema.clone())
                            } else {
                                (format!("pgt_id={}", pgt_id), "unknown".to_string())
                            }
                        }
                        Err(_) => (format!("pgt_id={}", pgt_id), "unknown".to_string()),
                    };

                    let is_convergence = convergence_set.contains(member);
                    out.push((
                        group_id,
                        name,
                        schema,
                        is_convergence,
                        group.epoch as i64,
                        effective_policy.as_str().to_string(),
                    ));
                }
            }
        }
        out
    });

    TableIterator::new(rows)
}

// ── Bootstrap Source Gating (v0.5.0, Phase 3) ─────────────────────────────

/// Mark a source table as "gated" so that all stream tables depending on it
/// are skipped by the scheduler until `ungate_source()` is called.
///
/// This is useful when loading bulk historical data into a source table:
/// gate it first, load the data, then ungate it so the scheduler picks it up
/// in one atomic refresh instead of refreshing repeatedly mid-load.
///
/// `source` is the source table name, optionally schema-qualified.
#[pg_extern(schema = "pgtrickle")]
fn gate_source(source: &str) -> Result<(), PgTrickleError> {
    let source_relid = resolve_source_oid(source)?;
    crate::catalog::upsert_gate(source_relid, Some("gate_source"))?;

    // Signal the scheduler that the gate set has changed.
    let payload = format!("{}", source_relid.to_u32());
    Spi::run(&format!(
        "SELECT pg_notify('pgtrickle_source_gate', {})",
        &payload
    ))
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    pgrx::info!(
        "pg_trickle: source {} (oid={}) is now gated",
        source,
        source_relid.to_u32()
    );
    Ok(())
}

/// Ungate a previously gated source table, allowing the scheduler to resume
/// refreshing stream tables that depend on it.
///
/// `source` is the source table name, optionally schema-qualified.
#[pg_extern(schema = "pgtrickle")]
fn ungate_source(source: &str) -> Result<(), PgTrickleError> {
    let source_relid = resolve_source_oid(source)?;
    crate::catalog::set_ungated(source_relid)?;

    // Signal the scheduler that the gate set has changed.
    let payload = format!("{}", source_relid.to_u32());
    Spi::run(&format!(
        "SELECT pg_notify('pgtrickle_source_gate', {})",
        &payload
    ))
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    pgrx::info!(
        "pg_trickle: source {} (oid={}) is now ungated",
        source,
        source_relid.to_u32()
    );
    Ok(())
}

/// Return the current gate status of all registered source gates.
///
/// Only rows that have ever been gated appear in this view (one row per
/// source_relid in `pgt_source_gates`).
#[pg_extern(schema = "pgtrickle", name = "source_gates")]
#[allow(clippy::type_complexity)]
fn source_gates_fn() -> TableIterator<
    'static,
    (
        name!(source_table, String),
        name!(schema_name, String),
        name!(gated, bool),
        name!(gated_at, Option<TimestampWithTimeZone>),
        name!(ungated_at, Option<TimestampWithTimeZone>),
        name!(gated_by, Option<String>),
    ),
> {
    let rows: Vec<_> = Spi::connect(|client| {
        let table = client
            .select(
                "SELECT c.relname::text, n.nspname::text, \
                        g.gated, g.gated_at, g.ungated_at, g.gated_by \
                 FROM pgtrickle.pgt_source_gates g \
                 JOIN pg_class c ON c.oid = g.source_relid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 ORDER BY n.nspname, c.relname",
                None,
                &[],
            )
            .unwrap_or_else(|e| {
                pgrx::warning!("source_gates: SPI error: {}", e);
                // Return an empty iterator-compatible value via an empty Vec
                panic!("source_gates: SPI error: {}", e)
            });

        let mut out = Vec::new();
        for row in table {
            let relname: String = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let nspname: String = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let gated: bool = row.get::<bool>(3).unwrap_or(None).unwrap_or(false);
            let gated_at = row.get::<TimestampWithTimeZone>(4).unwrap_or(None);
            let ungated_at = row.get::<TimestampWithTimeZone>(5).unwrap_or(None);
            let gated_by = row.get::<String>(6).unwrap_or(None);
            out.push((relname, nspname, gated, gated_at, ungated_at, gated_by));
        }
        out
    });

    TableIterator::new(rows)
}

/// Resolve a (possibly schema-qualified) table name to its OID.
fn resolve_source_oid(source: &str) -> Result<pg_sys::Oid, PgTrickleError> {
    let oid = Spi::get_one_with_args::<pg_sys::Oid>("SELECT $1::regclass::oid", &[source.into()])
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| PgTrickleError::NotFound(format!("relation '{}' does not exist", source)))?;
    Ok(oid)
}

// ── Helper functions ───────────────────────────────────────────────────────

/// EC-25/EC-26: Install a guard trigger that blocks direct DML on a stream
/// table's storage table.
///
/// Creates a PL/pgSQL trigger function and a BEFORE trigger for
/// INSERT/UPDATE/DELETE that raises an exception if the caller is not
/// the pg_trickle refresh executor.  The trigger checks the
/// `pg_trickle.internal_refresh` GUC which is set to `true` only during
/// refresh execution.
///
/// Also installs an event trigger guard for TRUNCATE via a separate trigger.
fn install_dml_guard_trigger(schema: &str, table_name: &str) -> Result<(), PgTrickleError> {
    let qualified = format!(
        "{}.{}",
        quote_identifier(schema),
        quote_identifier(table_name),
    );
    let trigger_func_name = format!(
        "{}._pgt_guard_{}",
        quote_identifier(schema),
        table_name.replace('"', ""),
    );

    // Create the guard trigger function
    //
    // IMPORTANT: For DELETE operations, NEW is NULL in PostgreSQL trigger
    // functions. A BEFORE trigger returning NULL silently cancels the
    // operation. We must return OLD for DELETE and NEW for INSERT/UPDATE
    // to allow the managed refresh executor to proceed.
    let create_func_sql = format!(
        "CREATE OR REPLACE FUNCTION {}() RETURNS trigger \
         LANGUAGE plpgsql AS $$ \
         BEGIN \
           IF current_setting('pg_trickle.internal_refresh', true) IS DISTINCT FROM 'true' THEN \
             RAISE EXCEPTION 'Direct DML on stream table % is not allowed. \
             Stream tables are maintained automatically by pg_trickle.', TG_TABLE_NAME; \
           END IF; \
           IF TG_OP = 'DELETE' THEN RETURN OLD; ELSE RETURN NEW; END IF; \
         END; $$",
        trigger_func_name,
    );
    Spi::run(&create_func_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create DML guard function: {}", e))
    })?;

    // Create the BEFORE INSERT/UPDATE/DELETE trigger
    let create_trigger_sql = format!(
        "CREATE TRIGGER pgt_dml_guard \
         BEFORE INSERT OR UPDATE OR DELETE ON {} \
         FOR EACH ROW EXECUTE FUNCTION {}()",
        qualified, trigger_func_name,
    );
    Spi::run(&create_trigger_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create DML guard trigger: {}", e))
    })?;

    // EC-25: Also guard against TRUNCATE via a statement-level trigger
    let create_truncate_trigger_sql = format!(
        "CREATE TRIGGER pgt_truncate_guard \
         BEFORE TRUNCATE ON {} \
         FOR EACH STATEMENT EXECUTE FUNCTION {}()",
        qualified, trigger_func_name,
    );
    Spi::run(&create_truncate_trigger_sql).map_err(|e| {
        PgTrickleError::SpiError(format!("Failed to create TRUNCATE guard trigger: {}", e))
    })?;

    Ok(())
}

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
    pgt_id: i64,
    change_schema: &str,
) -> Result<(), PgTrickleError> {
    let requested_cdc_mode = StDependency::effective_requested_mode_for_source(source_oid)?
        .unwrap_or_else(|| "trigger".to_string());

    // Check if already tracked
    let already_tracked = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_change_tracking WHERE source_relid = $1)",
        &[source_oid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if requested_cdc_mode == "wal" {
        validate_requested_cdc_mode_requirements(&requested_cdc_mode)?;
    }

    if !already_tracked {
        // Resolve PK columns for trigger pk_hash computation
        let pk_columns = cdc::resolve_pk_columns(source_oid)?;

        // EC-19: If CDC mode is "wal" or "auto" and the source table has no
        // primary key, verify REPLICA IDENTITY FULL. Without it, WAL-based
        // CDC cannot produce correct old-row values for UPDATE/DELETE, leading
        // to silent data corruption.
        if pk_columns.is_empty() && requested_cdc_mode == "wal" {
            let identity = cdc::get_replica_identity_mode(source_oid)?;
            if identity != "full" {
                let table_name = Spi::get_one_with_args::<String>(
                    "SELECT format('%I.%I', n.nspname, c.relname) \
                     FROM pg_class c \
                     JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE c.oid = $1",
                    &[source_oid.into()],
                )
                .unwrap_or(None)
                .unwrap_or_else(|| format!("OID {}", source_oid.to_u32()));

                return Err(PgTrickleError::InvalidArgument(format!(
                    "Source table {} has no PRIMARY KEY and REPLICA IDENTITY is '{}'. \
                     WAL-based CDC (cdc_mode = '{}') requires either a PRIMARY KEY \
                     or REPLICA IDENTITY FULL on keyless tables. \
                     Fix: ALTER TABLE {} REPLICA IDENTITY FULL; \
                     or use cdc_mode = 'trigger'/'auto'.",
                    table_name, identity, requested_cdc_mode, table_name
                )));
            }
        }

        // Resolve all source columns for typed change buffer
        let col_defs = cdc::resolve_source_column_defs(source_oid)?;

        // Create the change buffer table (with typed columns + pk_hash always)
        cdc::create_change_buffer_table(source_oid, change_schema, &col_defs)?;

        // Create the CDC trigger on the source table (typed per-column INSERTs)
        let trigger_name =
            cdc::create_change_trigger(source_oid, change_schema, &pk_columns, &col_defs)?;

        // Insert tracking record
        Spi::run_with_args(
            "INSERT INTO pgtrickle.pgt_change_tracking (source_relid, slot_name, tracked_by_pgt_ids) \
             VALUES ($1, $2, ARRAY[$3])",
            &[
                source_oid.into(),
                trigger_name.as_str().into(),
                pgt_id.into(),
            ],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    } else {
        // Already tracked — add this pgt_id to the tracking array
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_change_tracking \
             SET tracked_by_pgt_ids = array_append(tracked_by_pgt_ids, $1) \
             WHERE source_relid = $2 AND NOT ($1 = ANY(tracked_by_pgt_ids))",
            &[pgt_id.into(), source_oid.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    if requested_cdc_mode == "trigger" {
        wal_decoder::force_source_to_trigger(source_oid, change_schema)?;
    }

    Ok(())
}

/// Clean up CDC tracking for a source that may no longer be needed.
///
/// If no other STs reference this source, drop the CDC trigger and
/// change buffer table.
fn cleanup_cdc_for_source(
    source_oid: pg_sys::Oid,
    cdc_mode: CdcMode,
    excluding_pgt_id: Option<i64>,
) -> Result<(), PgTrickleError> {
    // Check if any other STs still reference this source. During ALTER flows,
    // the current ST's dependency row still exists while cleanup runs, so it
    // must be excluded from the reference check.
    let still_referenced = if let Some(pgt_id) = excluding_pgt_id {
        Spi::get_one_with_args::<bool>(
            "SELECT EXISTS( \
                SELECT 1 FROM pgtrickle.pgt_dependencies \
                WHERE source_relid = $1 AND pgt_id <> $2 \
            )",
            &[source_oid.into(), pgt_id.into()],
        )
        .unwrap_or(Some(false))
        .unwrap_or(false)
    } else {
        Spi::get_one_with_args::<bool>(
            "SELECT EXISTS( \
                SELECT 1 FROM pgtrickle.pgt_dependencies WHERE source_relid = $1 \
            )",
            &[source_oid.into()],
        )
        .unwrap_or(Some(false))
        .unwrap_or(false)
    };

    if !still_referenced {
        let change_schema = config::pg_trickle_change_buffer_schema();

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

        // EC-05: Drop the snapshot table (only exists for foreign table sources).
        let drop_snap_sql = format!(
            "DROP TABLE IF EXISTS {}.snapshot_{} CASCADE",
            quote_identifier(&change_schema),
            source_oid.to_u32(),
        );
        let _ = Spi::run(&drop_snap_sql);

        // Delete tracking record
        let _ = Spi::run_with_args(
            "DELETE FROM pgtrickle.pgt_change_tracking WHERE source_relid = $1",
            &[source_oid.into()],
        );
    }

    Ok(())
}

/// Parse a possibly schema-qualified name into `(schema, table)`.
fn parse_qualified_name(name: &str) -> Result<(String, String), PgTrickleError> {
    let parts: Vec<&str> = name.splitn(2, '.').collect();
    match parts.len() {
        1 => {
            let schema = Spi::get_one::<String>("SELECT current_schema()::text")
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_else(|| "public".to_string());
            Ok((schema, parts[0].to_string()))
        }
        2 => Ok((parts[0].to_string(), parts[1].to_string())),
        _ => Err(PgTrickleError::InvalidArgument(format!(
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
fn validate_defining_query(query: &str) -> Result<Vec<ColumnDef>, PgTrickleError> {
    let check_sql = format!("SELECT * FROM ({query}) sub LIMIT 0");

    // Execute to verify syntax and extract columns
    Spi::connect(|client| {
        let result = client
            .select(&check_sql, None, &[])
            .map_err(|e| PgTrickleError::QueryParseError(format!("{}", e)))?;

        // Extract column info from the result tuple descriptor
        let ncols = result
            .columns()
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
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
            return Err(PgTrickleError::QueryParseError(
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
pub(crate) fn parse_duration(s: &str) -> Result<i64, PgTrickleError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(PgTrickleError::InvalidArgument(
            "schedule cannot be empty".into(),
        ));
    }

    // Bare integer → seconds
    if let Ok(secs) = s.parse::<i64>() {
        return if secs >= 0 {
            Ok(secs)
        } else {
            Err(PgTrickleError::InvalidArgument(format!(
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
                    return Err(PgTrickleError::InvalidArgument(format!(
                        "invalid duration unit '{ch}' in '{s}'. \
                         Use s (seconds), m (minutes), h (hours), d (days), w (weeks). \
                         Example: '5m', '1h30m', '2d'"
                    )));
                }
            };

            if num_buf.is_empty() {
                return Err(PgTrickleError::InvalidArgument(format!(
                    "expected a number before '{ch}' in duration '{s}'"
                )));
            }

            let n: i64 = num_buf.parse().map_err(|_| {
                PgTrickleError::InvalidArgument(format!(
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
            return Err(PgTrickleError::InvalidArgument(format!(
                "trailing digits '{num_buf}' without a unit in duration '{s}'. \
                 Append s, m, h, d, or w. Example: '1h30m'"
            )));
        }
        // Pure digits already handled above; shouldn't reach here
        return Err(PgTrickleError::InvalidArgument(format!(
            "invalid duration '{s}'"
        )));
    }

    if total_secs < 0 {
        return Err(PgTrickleError::InvalidArgument(format!(
            "schedule cannot be negative: '{s}'"
        )));
    }

    Ok(total_secs)
}

/// Validate that schedule meets the minimum.
fn validate_schedule(seconds: i64) -> Result<(), PgTrickleError> {
    let min = config::pg_trickle_min_schedule_seconds() as i64;

    if seconds < min {
        return Err(PgTrickleError::InvalidArgument(format!(
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
pub(crate) fn parse_schedule(s: &str) -> Result<Schedule, PgTrickleError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(PgTrickleError::InvalidArgument(
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
fn validate_cron(expr: &str) -> Result<(), PgTrickleError> {
    use std::str::FromStr;

    croner::Cron::from_str(expr).map_err(|e| {
        PgTrickleError::InvalidArgument(format!("invalid cron expression '{expr}': {e}"))
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
) -> Result<Vec<(pg_sys::Oid, String)>, PgTrickleError> {
    use pgrx::PgList;
    use std::ffi::CString;

    let c_sql = CString::new(query)
        .map_err(|e| PgTrickleError::QueryParseError(format!("Query contains null byte: {}", e)))?;

    // SAFETY: We're calling PostgreSQL C parser functions with valid inputs.
    // raw_parser and parse_analyze_fixedparams are safe when called within
    // a PostgreSQL backend with a valid memory context.
    unsafe {
        // Step 1: Parse the raw SQL into a parse tree
        let raw_list = pg_sys::raw_parser(c_sql.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT);

        let stmts = PgList::<pg_sys::RawStmt>::from_pg(raw_list);
        let raw_stmt = stmts.get_ptr(0).ok_or_else(|| {
            PgTrickleError::QueryParseError("Query produced no parse tree nodes".into())
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
            return Err(PgTrickleError::QueryParseError(
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
            return Err(PgTrickleError::QueryParseError(
                "Defining query references no tables".into(),
            ));
        }

        Ok(relations)
    }
}

/// Context for [`relation_oid_walker`] — collects `(Oid, source_type)` pairs.
struct RelationCollectorCtx {
    relations: *mut Vec<(pg_sys::Oid, String)>,
    seen_oids: *mut std::collections::HashSet<pg_sys::Oid>,
}

/// Recursively collect `RTE_RELATION` OIDs from an analyzed `Query` node.
///
/// Uses PostgreSQL's `query_tree_walker_impl` with the
/// `QTW_EXAMINE_RTES_BEFORE` flag so that the callback visits every
/// `RangeTblEntry` in every (sub-)query. This covers:
///
/// 1. Base tables in FROM clauses (`RTE_RELATION`)
/// 2. Subqueries in FROM (`RTE_SUBQUERY` → walker recurses automatically)
/// 3. CTEs (`Query.cteList` → walker recurses automatically)
/// 4. EXISTS / IN / ANY subqueries in WHERE / HAVING / SELECT
///    (`SubLink.subselect` → `expression_tree_walker` recurses,
///    callback handles the resulting `T_Query` node)
///
/// # Safety
/// Caller must ensure `query_node` points to a valid analyzed `Query`.
unsafe fn collect_relation_oids(
    query_node: *mut pg_sys::Query,
    relations: &mut Vec<(pg_sys::Oid, String)>,
    seen_oids: &mut std::collections::HashSet<pg_sys::Oid>,
) {
    if query_node.is_null() {
        return;
    }

    let mut ctx = RelationCollectorCtx {
        relations: relations as *mut _,
        seen_oids: seen_oids as *mut _,
    };

    // SAFETY: query_node is a valid analyzed Query; the walker callback
    // only reads RTE fields and calls classify_source_relation (SPI).
    // QTW_EXAMINE_RTES_BEFORE = 16: the walker calls our callback for
    // each RangeTblEntry *before* recursing into subqueries / CTEs.
    unsafe {
        pg_sys::query_tree_walker_impl(
            query_node,
            Some(relation_oid_walker),
            &mut ctx as *mut RelationCollectorCtx as *mut std::ffi::c_void,
            pg_sys::QTW_EXAMINE_RTES_BEFORE as i32,
        );
    }
}

/// Walker callback for [`collect_relation_oids`].
///
/// Called by `query_tree_walker_impl` / `expression_tree_walker_impl` for
/// every node in the analyzed query tree.
///
/// - `T_RangeTblEntry` with `RTE_RELATION` → extract OID
/// - `T_Query` (from SubLink subselects) → recurse via `query_tree_walker`
/// - Everything else → recurse via `expression_tree_walker`
///
/// # Safety
/// `node` and `context` must be valid pointers provided by the PG walker.
unsafe extern "C-unwind" fn relation_oid_walker(
    node: *mut pg_sys::Node,
    context: *mut std::ffi::c_void,
) -> bool {
    if node.is_null() {
        return false;
    }

    // RTE_RELATION → record the OID
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeTblEntry) } {
        // SAFETY: node tag verified as T_RangeTblEntry.
        let rte = unsafe { &*(node as *const pg_sys::RangeTblEntry) };
        if rte.rtekind == pg_sys::RTEKind::RTE_RELATION {
            // SAFETY: context is our RelationCollectorCtx.
            let ctx = unsafe { &mut *(context as *mut RelationCollectorCtx) };
            let seen = unsafe { &mut *ctx.seen_oids };
            if seen.insert(rte.relid) {
                let source_type = classify_source_relation(rte.relid);
                let rels = unsafe { &mut *ctx.relations };
                rels.push((rte.relid, source_type));
            }
        }
        return false; // continue walking
    }

    // T_Query → use query_tree_walker to handle rtable + expressions
    // (expression_tree_walker does NOT recurse into Query nodes)
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_Query) } {
        // SAFETY: node tag verified as T_Query.
        return unsafe {
            pg_sys::query_tree_walker_impl(
                node as *mut pg_sys::Query,
                Some(relation_oid_walker),
                context,
                pg_sys::QTW_EXAMINE_RTES_BEFORE as i32,
            )
        };
    }

    // All other node types → recurse into children
    // SAFETY: expression_tree_walker handles all standard node types.
    unsafe { pg_sys::expression_tree_walker_impl(node, Some(relation_oid_walker), context) }
}

/// Emit warnings/info for source table edge cases (F13, F14).
///
/// - **Partitioned tables** (F13): Log an info message confirming that CDC
///   triggers on the parent fire for partition-routed DML (PG 13+).
/// - **Logical replication targets** (F14): Emit a WARNING because changes
///   arriving via logical replication do **not** fire normal triggers, which
///   means CDC will miss those changes.
fn warn_source_table_properties(source_relids: &[(pg_sys::Oid, String)]) {
    for (oid, source_type) in source_relids {
        if source_type != "TABLE" {
            continue;
        }

        // Resolve relkind and qualified name.
        let relkind = Spi::get_one_with_args::<String>(
            "SELECT relkind::text FROM pg_class WHERE oid = $1",
            &[(*oid).into()],
        )
        .unwrap_or(None);

        let relkind = match relkind {
            Some(rk) => rk,
            None => continue,
        };

        let table_name = Spi::get_one_with_args::<String>(
            "SELECT format('%I.%I', n.nspname, c.relname) \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.oid = $1",
            &[(*oid).into()],
        )
        .unwrap_or(None)
        .unwrap_or_else(|| format!("OID {}", oid.to_u32()));

        // F13: Partitioned table info
        if relkind == "p" {
            pgrx::info!(
                "pg_trickle: source table {} is a partitioned table. \
                 CDC triggers on the parent fire for all DML routed to \
                 child partitions (PostgreSQL 13+).",
                table_name,
            );
        }

        // F14: Logical replication target warning
        let is_sub_target = Spi::get_one_with_args::<bool>(
            "SELECT EXISTS(\
                SELECT 1 FROM pg_subscription_rel WHERE srrelid = $1\
             )",
            &[(*oid).into()],
        )
        .unwrap_or(Some(false))
        .unwrap_or(false);

        if is_sub_target {
            pgrx::warning!(
                "pg_trickle: source table {} is a logical replication target. \
                 Changes arriving via replication will NOT fire CDC triggers — \
                 the stream table may become stale. Consider using \
                 cdc_mode = 'wal' or a FULL refresh schedule.",
                table_name,
            );
        }

        // EC-06: Keyless table warning — source tables without a PRIMARY KEY
        // use content-based hashing for change detection, which is slower and
        // cannot distinguish between identical duplicate rows.
        match cdc::resolve_pk_columns(*oid) {
            Ok(pk_cols) if pk_cols.is_empty() => {
                pgrx::warning!(
                    "pg_trickle: source table {} has no PRIMARY KEY. Change detection \
                     will use content-based hashing, which is slower for wide tables \
                     and cannot distinguish identical duplicate rows. Consider adding \
                     a PRIMARY KEY for best performance.",
                    table_name,
                );
            }
            _ => {}
        }
    }
}

/// EC-15: Warn when the defining query contains `SELECT *` at the top level.
///
/// `SELECT *` makes the stream table fragile: if a column is added to or
/// removed from a source table, the stream table's storage schema will be
/// out of sync with the defining query, causing errors or silent data loss
/// on the next refresh.
///
/// This is a best-effort heuristic check using the raw query text. It looks
/// for `SELECT ... * ...` patterns that are not inside a subquery or aggregate
/// (e.g., `count(*)` is allowed).
fn warn_select_star(query: &str) {
    if detect_select_star(query) {
        pgrx::warning!(
            "pg_trickle: defining query uses SELECT *. If source table columns \
             are added or removed, the stream table will require reinitialization. \
             Consider listing columns explicitly for resilience against schema \
             changes."
        );
    }
}

/// Pure detection logic for `SELECT *` patterns in a defining query.
///
/// Returns `true` if the query contains a bare `*` (or `table.*`) in the
/// top-level SELECT list. Ignores `*` inside function calls like `count(*)`.
///
/// This is intentionally conservative — false positives are OK (it's a
/// warning), but false negatives for `SELECT *` should be rare.
fn detect_select_star(query: &str) -> bool {
    // Quick exit: no asterisk at all
    if !query.contains('*') {
        return false;
    }

    let upper = query.to_uppercase();

    // Find the first top-level SELECT ... FROM
    if let Some(select_pos) = upper.find("SELECT") {
        let after_select = &upper[select_pos + 6..];
        // Find FROM (at the same nesting level)
        let mut depth = 0i32;
        let mut from_offset = None;
        for (i, ch) in after_select.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 && after_select[i..].starts_with("FROM") {
                // Check if it's a word boundary (not part of a larger word)
                let before_ok = i == 0 || !after_select.as_bytes()[i - 1].is_ascii_alphanumeric();
                let after_ok = i + 4 >= after_select.len()
                    || !after_select.as_bytes()[i + 4].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    from_offset = Some(i);
                    break;
                }
            }
        }

        if let Some(end) = from_offset {
            let select_list = &after_select[..end];
            // Check for bare `*` or `table.*` at top-level (depth 0)
            let mut depth = 0i32;
            let chars: Vec<char> = select_list.chars().collect();
            for &ch in chars.iter() {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    '*' if depth == 0 => {
                        return true;
                    }
                    _ => {}
                }
            }
        }
    }
    false
}

/// Classify a source relation as TABLE, STREAM_TABLE, or VIEW.
fn classify_source_relation(oid: pg_sys::Oid) -> String {
    // Check if this OID is a stream table
    let is_st = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables WHERE pgt_relid = $1)",
        &[oid.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);

    if is_st {
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
        "m" => "MATVIEW".to_string(),
        "f" => "FOREIGN_TABLE".to_string(),
        _ => "TABLE".to_string(),
    }
}

/// Check for cycles after adding the proposed dependency edges.
///
/// Loads the existing DAG from the catalog, adds the proposed edges,
/// and runs Kahn's algorithm for cycle detection.
fn check_for_cycles(source_relids: &[(pg_sys::Oid, String)]) -> Result<(), PgTrickleError> {
    if source_relids.is_empty() {
        return Ok(());
    }

    // Check if any source is itself a stream table — only then can cycles exist
    let has_st_source = source_relids
        .iter()
        .any(|(_, stype)| stype == "STREAM_TABLE");

    if !has_st_source {
        // No stream table sources → no possible cycle
        return Ok(());
    }

    // Build the DAG from catalog and add proposed edges
    let mut dag = StDag::build_from_catalog(config::pg_trickle_default_schedule_seconds())?;

    // Create a temporary node for the proposed ST (use a sentinel pgt_id)
    let proposed_id = NodeId::StreamTable(i64::MAX);
    dag.add_st_node(DagNode {
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
            // Find the pgt_id for this source OID
            match crate::catalog::StreamTableMeta::get_by_relid(*source_oid) {
                Ok(meta) => NodeId::StreamTable(meta.pgt_id),
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

/// Cycle detection variant for ALTER QUERY.
///
/// Instead of creating a sentinel node (as `check_for_cycles` does for CREATE),
/// this function re-uses the existing ST's node in the DAG and replaces its
/// incoming edges with the proposed new source dependencies. This correctly
/// detects cycles like A → B → A that a sentinel node would miss.
fn check_for_cycles_alter(
    pgt_id: i64,
    source_relids: &[(pg_sys::Oid, String)],
) -> Result<(), PgTrickleError> {
    if source_relids.is_empty() {
        return Ok(());
    }

    let has_st_source = source_relids
        .iter()
        .any(|(_, stype)| stype == "STREAM_TABLE");

    if !has_st_source {
        return Ok(());
    }

    let mut dag = StDag::build_from_catalog(config::pg_trickle_default_schedule_seconds())?;

    let target_node = NodeId::StreamTable(pgt_id);

    // Resolve new source node IDs
    let new_sources: Vec<NodeId> = source_relids
        .iter()
        .map(|(source_oid, source_type)| {
            if source_type == "STREAM_TABLE" {
                match crate::catalog::StreamTableMeta::get_by_relid(*source_oid) {
                    Ok(meta) => NodeId::StreamTable(meta.pgt_id),
                    Err(_) => NodeId::BaseTable(source_oid.to_u32()),
                }
            } else {
                NodeId::BaseTable(source_oid.to_u32())
            }
        })
        .collect();

    // Replace the ST's incoming edges with the proposed new ones
    dag.replace_incoming_edges(target_node, new_sources);

    dag.detect_cycles()
}

/// Build CREATE TABLE DDL for the storage table.
fn build_create_table_sql(
    schema: &str,
    name: &str,
    columns: &[ColumnDef],
    needs_pgt_count: bool,
    needs_dual_count: bool,
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

    // Add __pgt_count auxiliary column for aggregate/distinct STs.
    let aux_cols = if needs_dual_count {
        // INTERSECT/EXCEPT need dual branch counts
        ",\n    __pgt_count_l BIGINT NOT NULL DEFAULT 0,\n    __pgt_count_r BIGINT NOT NULL DEFAULT 0"
    } else if needs_pgt_count {
        ",\n    __pgt_count BIGINT NOT NULL DEFAULT 0"
    } else {
        ""
    };

    format!(
        "CREATE TABLE {}.{} (\n    __pgt_row_id BIGINT,\n{}{}\n)",
        quote_identifier(schema),
        quote_identifier(name),
        col_defs.join(",\n"),
        aux_cols,
    )
}

/// Get the OID of a table by schema and name.
fn get_table_oid(schema: &str, name: &str) -> Result<pg_sys::Oid, PgTrickleError> {
    let oid = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT ($1 || '.' || $2)::regclass::oid",
        &[schema.into(), name.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
    .ok_or_else(|| {
        PgTrickleError::NotFound(format!(
            "table {}.{} not found after creation",
            schema, name
        ))
    })?;
    Ok(oid)
}

/// Initialize a stream table by populating it from its defining query.
#[allow(clippy::too_many_arguments)]
fn initialize_st(
    schema: &str,
    name: &str,
    query: &str,
    pgt_id: i64,
    columns: &[ColumnDef],
    needs_pgt_count: bool,
    needs_dual_count: bool,
    needs_union_dedup: bool,
    topk_info: Option<&crate::dvm::TopKInfo>,
) -> Result<(), PgTrickleError> {
    // EC-25/EC-26: Set the internal_refresh flag so DML guard triggers
    // allow the initialization INSERT into the storage table.
    Spi::run("SET LOCAL pg_trickle.internal_refresh = 'true'")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // For aggregate/distinct STs, inject COUNT(*) AS __pgt_count into the
    // defining query so the auxiliary column is populated correctly.
    let effective_query = if needs_pgt_count {
        inject_pgt_count(query)
    } else {
        query.to_string()
    };

    // Compute row_id using the same hash formula as the delta query so
    // the MERGE ON clause matches during subsequent differential refreshes.
    // For INTERSECT/EXCEPT queries, compute per-branch multiplicities
    // matching the dual-count storage schema.
    // For UNION (without ALL) queries, convert to UNION ALL and count
    // per-unique-row multiplicities for the __pgt_count column.
    // For UNION ALL queries, decompose into per-branch subqueries with
    // child-prefixed row IDs matching diff_union_all's formula.
    let insert_body = if needs_dual_count {
        let col_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        if let Some(set_op_sql) = crate::dvm::try_set_op_refresh_sql(query, &col_names) {
            set_op_sql
        } else {
            // Fallback: should not happen since needs_dual_count implies set-op
            let row_id_expr = crate::dvm::row_id_expr_for_query(query);
            format!(
                "SELECT {row_id_expr} AS __pgt_row_id, sub.*, \
                 1::bigint AS __pgt_count_l, 0::bigint AS __pgt_count_r \
                 FROM ({effective_query}) sub",
            )
        }
    } else if needs_union_dedup {
        let col_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        if let Some(union_sql) = crate::dvm::try_union_dedup_refresh_sql(query, &col_names) {
            union_sql
        } else {
            // Fallback: treat as normal query with __pgt_count = 1
            let row_id_expr = crate::dvm::row_id_expr_for_query(query);
            format!(
                "SELECT {row_id_expr} AS __pgt_row_id, sub.*, \
                 1::bigint AS __pgt_count \
                 FROM ({query}) sub",
            )
        }
    } else if let Some(ua_sql) = crate::dvm::try_union_all_refresh_sql(query) {
        ua_sql
    } else if let Some(info) = topk_info {
        // TopK: use the full query (with ORDER BY + LIMIT) for initial population,
        // so only the top K rows are inserted.
        let row_id_expr = crate::dvm::row_id_expr_for_query(query);
        format!(
            "SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({topk_query}) sub",
            topk_query = info.full_query,
        )
    } else {
        let row_id_expr = crate::dvm::row_id_expr_for_query(query);
        format!("SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({effective_query}) sub",)
    };

    let insert_sql = format!(
        "INSERT INTO {schema}.{table} {insert_body}",
        schema = quote_identifier(schema),
        table = quote_identifier(name),
    );

    Spi::run(&insert_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to initialize ST: {}", e)))?;

    // Seed the initial frontier at creation time so every initialized stream
    // table participates in shared change-buffer bookkeeping immediately.
    // Without this, one branch of a diamond can remain frontier-less after the
    // initial populate and later miss source changes that a sibling consumes.
    //
    // FOREIGN_TABLE sources are included so that the frontier is never empty
    // for FT-only stream tables.  An empty frontier causes
    // `execute_manual_differential_refresh` to treat every manual refresh as a
    // no-op (it assumes empty frontiers belong to ST-on-ST dependencies).
    // Including the FT OID with the current WAL LSN gives differential refresh
    // a valid lower bound from which to compare polled change-buffer rows.
    let source_oids: Vec<pg_sys::Oid> = StDependency::get_for_st(pgt_id)?
        .into_iter()
        .filter(|dep| dep.source_type == "TABLE" || dep.source_type == "FOREIGN_TABLE")
        .map(|dep| dep.source_relid)
        .collect();
    let slot_positions = cdc::get_slot_positions(&source_oids)?;
    let data_ts = get_data_timestamp_str();
    let frontier = version::compute_initial_frontier(&slot_positions, &data_ts);
    StreamTableMeta::store_frontier_and_complete_refresh(pgt_id, &frontier, 0)?;
    Ok(())
}

/// Quote a SQL identifier (escape double quotes).
fn quote_identifier(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Inject `COUNT(*) AS __pgt_count` into an aggregate/distinct defining query
/// so that the full refresh populates the auxiliary count column.
///
/// For aggregate queries, this adds `, COUNT(*) AS __pgt_count` before the
/// first top-level `FROM`.
///
/// For DISTINCT queries, transforms `SELECT DISTINCT cols FROM ...` into
/// `SELECT cols, COUNT(*) AS __pgt_count FROM ... GROUP BY cols`.
pub fn inject_pgt_count(query: &str) -> String {
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
                "{select_part}, COUNT(*) AS __pgt_count {from_part} GROUP BY {col_list}",
            );
        }
        // Fallback if FROM not found after stripping DISTINCT
        return distinct_info.stripped;
    }

    // Non-DISTINCT (aggregate) queries: just inject COUNT(*) before FROM.
    if let Some(pos) = find_top_level_keyword(query, "FROM") {
        format!(
            "{}, COUNT(*) AS __pgt_count {}",
            query[..pos].trim_end(),
            &query[pos..],
        )
    } else {
        // Fallback: can't inject; return as-is (will leave __pgt_count = DEFAULT 0)
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
            // Skip single-line comments: -- until end of line
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // skip the newline
                }
            }
            // Skip block comments: /* ... */
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                let mut block_depth = 1i32;
                while i < bytes.len() && block_depth > 0 {
                    if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                        block_depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                        block_depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
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
    fn test_inject_pgt_count_distinct_basic() {
        let query = "SELECT DISTINCT color, size FROM prop_dist";
        let result = inject_pgt_count(query);
        assert_eq!(
            result,
            "SELECT color, size, COUNT(*) AS __pgt_count FROM prop_dist GROUP BY color, size"
        );
    }

    #[test]
    fn test_inject_pgt_count_distinct_lowercase() {
        let query = "select distinct color, size from prop_dist";
        let result = inject_pgt_count(query);
        // Note: "SELECT" is uppercased because detect_and_strip_distinct
        // reconstructs the prefix with literal "SELECT".
        assert_eq!(
            result,
            "SELECT color, size, COUNT(*) AS __pgt_count from prop_dist GROUP BY color, size"
        );
    }

    #[test]
    fn test_inject_pgt_count_distinct_with_where() {
        let query = "SELECT DISTINCT color FROM items WHERE active = true";
        let result = inject_pgt_count(query);
        assert_eq!(
            result,
            "SELECT color, COUNT(*) AS __pgt_count FROM items WHERE active = true GROUP BY color"
        );
    }

    #[test]
    fn test_inject_pgt_count_aggregate_no_distinct() {
        // Non-DISTINCT aggregate: just adds COUNT(*) before FROM
        let query = "SELECT region, SUM(amount) FROM orders GROUP BY region";
        let result = inject_pgt_count(query);
        assert_eq!(
            result,
            "SELECT region, SUM(amount), COUNT(*) AS __pgt_count FROM orders GROUP BY region"
        );
    }

    #[test]
    fn test_inject_pgt_count_distinct_three_cols() {
        let query = "SELECT DISTINCT a, b, c FROM t1";
        let result = inject_pgt_count(query);
        assert_eq!(
            result,
            "SELECT a, b, c, COUNT(*) AS __pgt_count FROM t1 GROUP BY a, b, c"
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

    // ── EC-15: detect_select_star unit tests ───────────────────────────

    #[test]
    fn test_detect_select_star_bare() {
        assert!(detect_select_star("SELECT * FROM t"));
    }

    #[test]
    fn test_detect_select_star_table_qualified() {
        assert!(detect_select_star("SELECT t.* FROM t"));
    }

    #[test]
    fn test_detect_select_star_explicit_columns_no_star() {
        assert!(!detect_select_star("SELECT id, name FROM t"));
    }

    #[test]
    fn test_detect_select_star_count_star_not_flagged() {
        assert!(!detect_select_star("SELECT count(*) FROM t"));
    }

    #[test]
    fn test_detect_select_star_sum_star_not_flagged() {
        assert!(!detect_select_star("SELECT sum(*) FROM t"));
    }

    #[test]
    fn test_detect_select_star_mixed_agg_and_bare_star() {
        // `count(*), *` has a bare `*` at top level
        assert!(detect_select_star("SELECT count(*), * FROM t"));
    }

    #[test]
    fn test_detect_select_star_no_asterisk_at_all() {
        assert!(!detect_select_star("SELECT id FROM t"));
    }

    #[test]
    fn test_detect_select_star_subquery_star_ignored() {
        // The outer SELECT has explicit columns; the inner SELECT * is inside parens
        assert!(!detect_select_star(
            "SELECT id, (SELECT * FROM b LIMIT 1) FROM t"
        ));
    }

    #[test]
    fn test_detect_select_star_case_insensitive() {
        assert!(detect_select_star("select * from t"));
        assert!(detect_select_star("SELECT * FROM t"));
        assert!(detect_select_star("Select * From t"));
    }

    #[test]
    fn test_detect_select_star_no_from() {
        // No FROM clause — should not crash, just return false
        assert!(!detect_select_star("SELECT 1"));
    }

    #[test]
    fn test_classify_cdc_refresh_mode_interaction_global_wal_immediate() {
        assert_eq!(
            classify_cdc_refresh_mode_interaction(
                RefreshMode::Immediate,
                "wal",
                CdcModeRequestSource::GlobalGuc,
            ),
            CdcRefreshModeInteraction::IgnoreWalForImmediate
        );
    }

    #[test]
    fn test_classify_cdc_refresh_mode_interaction_explicit_wal_immediate() {
        assert_eq!(
            classify_cdc_refresh_mode_interaction(
                RefreshMode::Immediate,
                "wal",
                CdcModeRequestSource::ExplicitOverride,
            ),
            CdcRefreshModeInteraction::RejectWalForImmediate
        );
    }

    #[test]
    fn test_classify_cdc_refresh_mode_interaction_non_immediate_is_none() {
        assert_eq!(
            classify_cdc_refresh_mode_interaction(
                RefreshMode::Differential,
                "wal",
                CdcModeRequestSource::GlobalGuc,
            ),
            CdcRefreshModeInteraction::None
        );
    }

    #[test]
    fn test_classify_cdc_refresh_mode_interaction_immediate_non_wal_is_none() {
        assert_eq!(
            classify_cdc_refresh_mode_interaction(
                RefreshMode::Immediate,
                "auto",
                CdcModeRequestSource::GlobalGuc,
            ),
            CdcRefreshModeInteraction::None
        );
    }
}
