//! User-facing SQL API functions for pgtrickle.
//!
//! All functions are exposed in the `pg_trickle` schema and provide the primary
//! interface for creating, altering, dropping, and refreshing stream tables.

use pgrx::prelude::*;
use std::collections::HashSet;
use std::time::Instant;

use crate::catalog::{CdcMode, RefreshRecord, StDependency, StreamTableMeta};
use crate::cdc;
use crate::config;
use crate::dag::{
    DagNode, DiamondConsistency, DiamondSchedulePolicy, NodeId, RefreshMode, StDag, StStatus,
};
use crate::error::PgTrickleError;
use crate::refresh;
use crate::shmem;
use crate::template_cache;
use crate::version;
use crate::wal_decoder;

// ── G13-EH: Enriched error reporting ────────────────────────────────────────

/// Raise a `PgTrickleError` as a PostgreSQL ERROR, adding DETAIL and HINT
/// fields for well-known error types that benefit from additional context.
///
/// For the four targeted variants (`UnsupportedOperator`, `CycleDetected`,
/// `UpstreamSchemaChanged`, `QueryParseError`) this uses `ErrorReport` with
/// `set_detail` / `set_hint` so the extra information appears in the
/// `DETAIL:` and `HINT:` sections of the PostgreSQL error message.
/// All other error types fall back to `pgrx::error!()`.
#[track_caller]
fn raise_error_with_context(e: PgTrickleError) -> ! {
    use pgrx::pg_sys::panic::ErrorReport;
    use pgrx::{PgLogLevel, PgSqlErrorCode};

    match &e {
        PgTrickleError::UnsupportedOperator(op) => {
            ErrorReport::new(
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                format!("unsupported operator for DIFFERENTIAL mode: {}", op),
                "",
            )
            .set_detail(
                "DIFFERENTIAL mode supports SELECT, WHERE, simple JOINs (INNER/LEFT/RIGHT), \
                 GROUP BY, HAVING, ORDER BY, and LIMIT. CTEs (WITH …), FULL OUTER JOINs, \
                 window functions without a writable equivalent, and certain aggregate forms \
                 are not supported."
                    .to_string(),
            )
            .set_hint(
                "Use refresh_mode = 'FULL' or refresh_mode = 'AUTO' to handle queries that \
                 contain unsupported constructs. AUTO will try DIFFERENTIAL first and fall \
                 back to FULL when needed."
                    .to_string(),
            )
            .report(PgLogLevel::ERROR);
            unreachable!()
        }
        PgTrickleError::CycleDetected(path) => {
            let cycle_str = path.join(" -> ");
            ErrorReport::new(
                PgSqlErrorCode::ERRCODE_INVALID_SCHEMA_DEFINITION,
                format!("cycle detected in dependency graph: {}", cycle_str),
                "",
            )
            .set_detail(
                "Stream tables form a directed acyclic graph (DAG). Adding this stream \
                 table would introduce a cycle, which is not allowed."
                    .to_string(),
            )
            .set_hint(
                "Review the defining query and ensure it does not reference any stream \
                 table that transitively depends on this table. Use \
                 pgtrickle.pgt_dependencies to inspect the current dependency graph."
                    .to_string(),
            )
            .report(PgLogLevel::ERROR);
            unreachable!()
        }
        PgTrickleError::UpstreamSchemaChanged(oid) => {
            ErrorReport::new(
                PgSqlErrorCode::ERRCODE_INVALID_TABLE_DEFINITION,
                format!("upstream table schema changed: OID {}", oid),
                "",
            )
            .set_detail(
                "An upstream source table was modified (ALTER TABLE) after the stream table \
                 was created. The change buffer schema may no longer match the source."
                    .to_string(),
            )
            .set_hint(
                "Call pgtrickle.refresh_stream_table('<name>') which will automatically \
                 reinitialize the stream table, or call pgtrickle.alter_stream_table(\
                 '<name>') with the updated query."
                    .to_string(),
            )
            .report(PgLogLevel::ERROR);
            unreachable!()
        }
        PgTrickleError::QueryParseError(msg) => {
            ErrorReport::new(
                PgSqlErrorCode::ERRCODE_SYNTAX_ERROR_OR_ACCESS_RULE_VIOLATION,
                format!("query parse error: {}", msg),
                "",
            )
            .set_detail(
                "The defining query could not be validated or rewritten. This can happen \
                 when the query references objects that do not exist yet, uses types \
                 incompatible with the storage schema, or triggers a validation check."
                    .to_string(),
            )
            .set_hint(
                "Verify the query runs successfully as a standalone SELECT before creating \
                 the stream table. Check that all referenced tables and columns exist."
                    .to_string(),
            )
            .report(PgLogLevel::ERROR);
            unreachable!()
        }
        // All other error types: plain message without detail/hint.
        other => {
            pgrx::error!("{}", other);
        }
    }
}

/// Create a new stream table.
///
/// # Arguments
/// - `name`: Schema-qualified name (`'schema.table'`) or unqualified (`'table'`).
/// - `query`: The defining SELECT query.
/// - `schedule`: Desired maximum schedule. `'calculated'` for CALCULATED mode (inherits schedule from downstream dependents).
/// - `refresh_mode`: `'AUTO'` (default — DIFFERENTIAL with FULL fallback),
///   `'FULL'`, `'DIFFERENTIAL'`, or `'IMMEDIATE'`.
/// - `initialize`: Whether to populate the table immediately.
/// - `diamond_consistency`: `'atomic'` (default) or `'none'`.
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
    pooler_compatibility_mode: default!(bool, false),
    partition_by: default!(Option<&str>, "NULL"),
    max_differential_joins: default!(Option<i32>, "NULL"),
    max_delta_fraction: default!(Option<f64>, "NULL"),
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
        pooler_compatibility_mode,
        partition_by,
        max_differential_joins,
        max_delta_fraction,
    );
    if let Err(e) = result {
        raise_error_with_context(e);
    }
}

/// Create a stream table if it does not already exist.
///
/// If a stream table with the given name already exists, this is a silent no-op
/// (an INFO message is logged). The existing definition is never modified.
///
/// This is useful for migration scripts that should be safe to re-run.
#[allow(clippy::too_many_arguments)]
#[pg_extern(schema = "pgtrickle")]
fn create_stream_table_if_not_exists(
    name: &str,
    query: &str,
    schedule: default!(Option<&str>, "'calculated'"),
    refresh_mode: default!(&str, "'AUTO'"),
    initialize: default!(bool, true),
    diamond_consistency: default!(Option<&str>, "NULL"),
    diamond_schedule_policy: default!(Option<&str>, "NULL"),
    cdc_mode: default!(Option<&str>, "NULL"),
    append_only: default!(bool, false),
    pooler_compatibility_mode: default!(bool, false),
    partition_by: default!(Option<&str>, "NULL"),
    max_differential_joins: default!(Option<i32>, "NULL"),
    max_delta_fraction: default!(Option<f64>, "NULL"),
) {
    let result = create_stream_table_if_not_exists_impl(
        name,
        query,
        schedule,
        refresh_mode,
        initialize,
        diamond_consistency,
        diamond_schedule_policy,
        cdc_mode,
        append_only,
        pooler_compatibility_mode,
        partition_by,
        max_differential_joins,
        max_delta_fraction,
    );
    if let Err(e) = result {
        raise_error_with_context(e);
    }
}

#[allow(clippy::too_many_arguments)]
fn create_stream_table_if_not_exists_impl(
    name: &str,
    query: &str,
    schedule: Option<&str>,
    refresh_mode: &str,
    initialize: bool,
    diamond_consistency: Option<&str>,
    diamond_schedule_policy: Option<&str>,
    cdc_mode: Option<&str>,
    append_only: bool,
    pooler_compatibility_mode: bool,
    partition_by: Option<&str>,
    max_differential_joins: Option<i32>,
    max_delta_fraction: Option<f64>,
) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;

    match StreamTableMeta::get_by_name(&schema, &table_name) {
        Ok(_) => {
            pgrx::info!(
                "Stream table {}.{} already exists — skipping creation.",
                schema,
                table_name,
            );
            Ok(())
        }
        Err(PgTrickleError::NotFound(_)) => create_stream_table_impl(
            name,
            query,
            schedule,
            refresh_mode,
            initialize,
            diamond_consistency,
            diamond_schedule_policy,
            cdc_mode,
            append_only,
            pooler_compatibility_mode,
            partition_by,
            max_differential_joins,
            max_delta_fraction,
        ),
        Err(e) => Err(e),
    }
}

/// G15-BC: Create multiple stream tables in a single transaction.
///
/// Accepts a JSONB array of stream table definitions. Each element must be
/// an object with at least `name` and `query` keys; all other keys match
/// the parameters of [`create_stream_table`] (snake_case).
///
/// Returns a JSONB array of results, one per input definition:
/// ```json
/// [
///   {"name": "my_st", "status": "created", "pgt_id": 42},
///   {"name": "bad_st", "status": "error", "error": "query parse error: …"}
/// ]
/// ```
///
/// On any error, the entire transaction is rolled back (standard PostgreSQL
/// transactional semantics).
#[pg_extern(schema = "pgtrickle")]
fn bulk_create(definitions: pgrx::JsonB) -> pgrx::JsonB {
    let result = bulk_create_impl(definitions.0);
    match result {
        Ok(results_json) => pgrx::JsonB(results_json),
        Err(e) => raise_error_with_context(e),
    }
}

fn bulk_create_impl(definitions: serde_json::Value) -> Result<serde_json::Value, PgTrickleError> {
    let defs = definitions.as_array().ok_or_else(|| {
        PgTrickleError::InvalidArgument(
            "bulk_create() expects a JSONB array of stream table definitions".into(),
        )
    })?;

    if defs.is_empty() {
        return Err(PgTrickleError::InvalidArgument(
            "bulk_create() definitions array is empty".into(),
        ));
    }

    let mut results = Vec::with_capacity(defs.len());

    for (i, def) in defs.iter().enumerate() {
        let obj = def.as_object().ok_or_else(|| {
            PgTrickleError::InvalidArgument(format!(
                "bulk_create() element [{}] is not a JSON object",
                i
            ))
        })?;

        let name = obj.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
            PgTrickleError::InvalidArgument(format!(
                "bulk_create() element [{}] missing required \"name\" string",
                i
            ))
        })?;

        let query = obj.get("query").and_then(|v| v.as_str()).ok_or_else(|| {
            PgTrickleError::InvalidArgument(format!(
                "bulk_create() element [{}] \"{}\" missing required \"query\" string",
                i, name
            ))
        })?;

        let schedule = obj.get("schedule").and_then(|v| v.as_str());
        let refresh_mode = obj
            .get("refresh_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("AUTO");
        let initialize = obj
            .get("initialize")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let diamond_consistency = obj.get("diamond_consistency").and_then(|v| v.as_str());
        let diamond_schedule_policy = obj.get("diamond_schedule_policy").and_then(|v| v.as_str());
        let cdc_mode = obj.get("cdc_mode").and_then(|v| v.as_str());
        let append_only = obj
            .get("append_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let pooler_compatibility_mode = obj
            .get("pooler_compatibility_mode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let partition_by = obj.get("partition_by").and_then(|v| v.as_str());
        let max_differential_joins = obj
            .get("max_differential_joins")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);
        let max_delta_fraction = obj.get("max_delta_fraction").and_then(|v| v.as_f64());

        match create_stream_table_impl(
            name,
            query,
            schedule,
            refresh_mode,
            initialize,
            diamond_consistency,
            diamond_schedule_policy,
            cdc_mode,
            append_only,
            pooler_compatibility_mode,
            partition_by,
            max_differential_joins,
            max_delta_fraction,
        ) {
            Ok(()) => {
                // Look up pgt_id for the result
                let (schema, table_name) =
                    parse_qualified_name(name).unwrap_or_else(|_| ("public".into(), name.into()));
                let pgt_id = StreamTableMeta::get_by_name(&schema, &table_name)
                    .map(|st| st.pgt_id)
                    .unwrap_or(-1);

                results.push(serde_json::json!({
                    "name": name,
                    "status": "created",
                    "pgt_id": pgt_id,
                }));
            }
            Err(e) => {
                // Abort the entire batch on error — the transaction will
                // be rolled back by PostgreSQL. Return immediate error
                // with context about which definition failed.
                return Err(PgTrickleError::InvalidArgument(format!(
                    "bulk_create() failed on element [{}] \"{}\": {}",
                    i, name, e
                )));
            }
        }
    }

    pgrx::info!(
        "pg_trickle: bulk_create created {} stream table(s)",
        results.len()
    );

    Ok(serde_json::Value::Array(results))
}

/// Create or replace a stream table.
///
/// If the stream table does not exist, it is created (identical to
/// [`create_stream_table`]).  If it already exists:
///
/// - **Identical definition** → no-op (INFO logged).
/// - **Query identical, config differs** → delegates to `alter_stream_table_impl`.
/// - **Query differs** → delegates to `alter_stream_table_impl` with query change
///   (ALTER QUERY path), plus any config changes.
///
/// This is the declarative API for idempotent deployments (dbt, migrations,
/// GitOps). Mirrors PostgreSQL's `CREATE OR REPLACE` convention.
#[allow(clippy::too_many_arguments)]
#[pg_extern(schema = "pgtrickle")]
fn create_or_replace_stream_table(
    name: &str,
    query: &str,
    schedule: default!(Option<&str>, "'calculated'"),
    refresh_mode: default!(&str, "'AUTO'"),
    initialize: default!(bool, true),
    diamond_consistency: default!(Option<&str>, "NULL"),
    diamond_schedule_policy: default!(Option<&str>, "NULL"),
    cdc_mode: default!(Option<&str>, "NULL"),
    append_only: default!(bool, false),
    pooler_compatibility_mode: default!(bool, false),
    partition_by: default!(Option<&str>, "NULL"),
    max_differential_joins: default!(Option<i32>, "NULL"),
    max_delta_fraction: default!(Option<f64>, "NULL"),
) {
    let result = create_or_replace_stream_table_impl(
        name,
        query,
        schedule,
        refresh_mode,
        initialize,
        diamond_consistency,
        diamond_schedule_policy,
        cdc_mode,
        append_only,
        pooler_compatibility_mode,
        partition_by,
        max_differential_joins,
        max_delta_fraction,
    );
    if let Err(e) = result {
        raise_error_with_context(e);
    }
}

/// Tracks which config parameters differ between an existing stream table
/// and a `create_or_replace` call.  Fields are `Some` only when changed.
struct ConfigDiff<'a> {
    schedule: Option<&'a str>,
    refresh_mode: Option<&'a str>,
    diamond_consistency: Option<&'a str>,
    diamond_schedule_policy: Option<&'a str>,
    cdc_mode: Option<&'a str>,
    append_only: Option<bool>,
    pooler_compatibility_mode: Option<bool>,
}

impl ConfigDiff<'_> {
    fn is_empty(&self) -> bool {
        self.schedule.is_none()
            && self.refresh_mode.is_none()
            && self.diamond_consistency.is_none()
            && self.diamond_schedule_policy.is_none()
            && self.cdc_mode.is_none()
            && self.append_only.is_none()
            && self.pooler_compatibility_mode.is_none()
    }
}

/// Compare the requested config parameters against the existing catalog row.
/// Returns `Some` only for parameters that differ from the stored values.
#[allow(clippy::too_many_arguments)]
fn compute_config_diff<'a>(
    existing: &StreamTableMeta,
    new_schedule: Option<&'a str>,
    new_refresh_mode: &'a str,
    new_dc: Option<&'a str>,
    new_dsp: Option<&'a str>,
    new_cdc_mode: Option<&'a str>,
    new_append_only: bool,
    new_pooler_compat: bool,
) -> ConfigDiff<'a> {
    // Schedule: compare raw strings.  'calculated' in user input means NULL in catalog.
    let schedule_changed = match new_schedule {
        Some(s) if s.trim().eq_ignore_ascii_case("calculated") => existing.schedule.is_some(),
        Some(s) => existing
            .schedule
            .as_deref()
            .is_none_or(|cur| cur != s.trim()),
        None => existing.schedule.is_some(),
    };

    // Refresh mode: compare enum values.  AUTO is resolved to DIFFERENTIAL by from_str.
    let new_mode = RefreshMode::from_str(new_refresh_mode).unwrap_or(RefreshMode::Differential);
    let mode_changed = existing.refresh_mode != new_mode;

    // Diamond consistency: compare enum values.
    let new_dc_val = match new_dc {
        Some(s) => DiamondConsistency::from_sql_str(&s.to_lowercase()),
        None => DiamondConsistency::Atomic,
    };
    let dc_changed = existing.diamond_consistency != new_dc_val;

    // Diamond schedule policy: compare enum values.
    let new_dsp_val = match new_dsp {
        Some(s) => DiamondSchedulePolicy::from_sql_str(s).unwrap_or(DiamondSchedulePolicy::Fastest),
        None => DiamondSchedulePolicy::Fastest,
    };
    let dsp_changed = existing.diamond_schedule_policy != new_dsp_val;

    // CDC mode: compare Option<String>.
    let new_cdc_normalized = new_cdc_mode.map(|m| m.trim().to_lowercase());
    let cdc_changed = match (&existing.requested_cdc_mode, &new_cdc_normalized) {
        (None, None) => false,
        (Some(a), Some(b)) => a != b,
        _ => true,
    };

    // Append-only: compare bools.
    let ao_changed = existing.is_append_only != new_append_only;

    // PB2: Pooler compatibility mode.
    let pcm_changed = existing.pooler_compatibility_mode != new_pooler_compat;

    ConfigDiff {
        schedule: if schedule_changed {
            new_schedule.or(Some("calculated"))
        } else {
            None
        },
        refresh_mode: if mode_changed {
            Some(new_refresh_mode)
        } else {
            None
        },
        diamond_consistency: if dc_changed { new_dc } else { None },
        diamond_schedule_policy: if dsp_changed { new_dsp } else { None },
        cdc_mode: if cdc_changed { new_cdc_mode } else { None },
        append_only: if ao_changed {
            Some(new_append_only)
        } else {
            None
        },
        pooler_compatibility_mode: if pcm_changed {
            Some(new_pooler_compat)
        } else {
            None
        },
    }
}

/// Collapse all runs of whitespace (spaces, tabs, newlines) into a single
/// space and trim leading/trailing whitespace. Used for semantic query
/// comparison so cosmetic SQL formatting differences are treated as no-ops.
fn normalize_sql_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[allow(clippy::too_many_arguments)]
fn create_or_replace_stream_table_impl(
    name: &str,
    query: &str,
    schedule: Option<&str>,
    refresh_mode_str: &str,
    initialize: bool,
    diamond_consistency: Option<&str>,
    diamond_schedule_policy: Option<&str>,
    cdc_mode: Option<&str>,
    append_only: bool,
    pooler_compatibility_mode: bool,
    partition_by: Option<&str>,
    max_differential_joins: Option<i32>,
    max_delta_fraction: Option<f64>,
) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;

    match StreamTableMeta::get_by_name(&schema, &table_name) {
        Ok(existing) => {
            // Stream table exists — determine what changed.
            let rw = run_query_rewrite_pipeline(query)?;
            let new_query_rewritten = rw.query;

            // TopK detection: if the new query is TopK, compare against the
            // base query (ORDER BY/LIMIT stripped) since that's what is stored
            // in `defining_query`.
            let topk_info = crate::dvm::detect_topk_pattern(&new_query_rewritten)?;
            let effective_new_query = match &topk_info {
                Some(info) => &info.base_query,
                None => &new_query_rewritten,
            };

            // Normalize whitespace before comparison so cosmetic differences
            // (extra spaces, newlines, tabs) are treated as no-ops.
            let query_changed = normalize_sql_whitespace(&existing.defining_query)
                != normalize_sql_whitespace(effective_new_query);

            let config_diff = compute_config_diff(
                &existing,
                schedule,
                refresh_mode_str,
                diamond_consistency,
                diamond_schedule_policy,
                cdc_mode,
                append_only,
                pooler_compatibility_mode,
            );

            if !query_changed && config_diff.is_empty() {
                pgrx::info!(
                    "Stream table {}.{} already exists with identical definition — no changes made.",
                    schema,
                    table_name,
                );
                return Ok(());
            }

            // Delegate to alter_stream_table_impl with the appropriate
            // combination of query + config changes.
            alter_stream_table_impl(
                name,
                if query_changed { Some(query) } else { None },
                config_diff.schedule,
                config_diff.refresh_mode,
                None, // status: keep current
                config_diff.diamond_consistency,
                config_diff.diamond_schedule_policy,
                config_diff.cdc_mode,
                config_diff.append_only,
                config_diff.pooler_compatibility_mode,
                None, // tier: not set via create_or_replace
                None, // fuse: not set via create_or_replace
                None, // fuse_ceiling: not set via create_or_replace
                None, // fuse_sensitivity: not set via create_or_replace
                None, // partition_by: not changed via create_or_replace
                max_differential_joins,
                max_delta_fraction,
            )?;

            pgrx::info!(
                "Stream table {}.{} replaced (query_changed={}, config_changed={}).",
                schema,
                table_name,
                query_changed,
                !config_diff.is_empty(),
            );

            Ok(())
        }
        Err(PgTrickleError::NotFound(_)) => {
            // Does not exist — create from scratch.
            create_stream_table_impl(
                name,
                query,
                schedule,
                refresh_mode_str,
                initialize,
                diamond_consistency,
                diamond_schedule_policy,
                cdc_mode,
                append_only,
                pooler_compatibility_mode,
                partition_by,
                max_differential_joins,
                max_delta_fraction,
            )
        }
        Err(e) => Err(e),
    }
}

/// Metadata about which query rewrites were applied.
struct RewriteResult {
    /// The rewritten query string.
    query: String,
    /// Whether `rewrite_nested_window_exprs` lifted window functions out of
    /// expressions (e.g. `CASE WHEN ROW_NUMBER() OVER (...) ...`). This
    /// rewrite produces a subquery form that DVM parsing accepts but delta
    /// SQL generation cannot handle — so differential mode must fall back
    /// to full refresh (EC-03).
    had_nested_window_rewrite: bool,
}

/// Run the full query rewrite pipeline: view inlining, nested window
/// expressions, DISTINCT ON, GROUPING SETS, scalar subqueries, SubLinks
/// in OR, and ROWS FROM.
///
/// Returns the rewritten query and metadata about which rewrites fired.
/// The caller should keep the original query separately if needed for
/// `original_query` tracking.
fn run_query_rewrite_pipeline(query: &str) -> Result<RewriteResult, PgTrickleError> {
    // View inlining — must run first so view definitions containing
    // DISTINCT ON, GROUPING SETS, etc. get further rewritten.
    let query = crate::dvm::rewrite_views_inline(query)?;
    // Nested window expression lift — track whether the rewrite fired
    let pre_window = query.clone();
    let query = crate::dvm::rewrite_nested_window_exprs(&query)?;
    let had_nested_window_rewrite = query != pre_window;
    // DISTINCT ON → ROW_NUMBER() window
    let query = crate::dvm::rewrite_distinct_on(&query)?;
    // GROUPING SETS / CUBE / ROLLUP → UNION ALL of GROUP BY
    let query = crate::dvm::rewrite_grouping_sets(&query)?;
    // Scalar subquery in WHERE → CROSS JOIN
    let query = crate::dvm::rewrite_scalar_subquery_in_where(&query)?;
    // Correlated scalar subquery in SELECT → LEFT JOIN
    let query = crate::dvm::rewrite_correlated_scalar_in_select(&query)?;
    // SubLinks inside OR → UNION branches (with De Morgan normalization).
    // Multiple passes handle patterns exposed after the first rewrite,
    // e.g. NOT(AND(…)) → OR(…) → UNION in pass 2.  Cap at 3 iterations.
    let mut query = query;
    for _ in 0..3 {
        let prev = query.clone();
        let q = crate::dvm::rewrite_demorgan_sublinks(&query)?;
        let q = crate::dvm::rewrite_sublinks_in_or(&q)?;
        if q == prev {
            break;
        }
        query = q;
    }
    // ROWS FROM() multi-function rewrite
    let query = crate::dvm::rewrite_rows_from(&query)?;
    Ok(RewriteResult {
        query,
        had_nested_window_rewrite,
    })
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
    /// AVG auxiliary columns: `(sum_col_name, count_col_name, arg_sql)` tuples
    /// for algebraic AVG maintenance. Empty if no non-DISTINCT AVG aggregates.
    avg_aux_columns: Vec<(String, String, String)>,
    /// Sum-of-squares auxiliary columns: `(sum2_col_name, arg_sql)` tuples
    /// for algebraic STDDEV/VAR maintenance. Empty if no non-DISTINCT STDDEV/VAR.
    sum2_aux_columns: Vec<(String, String)>,
    /// Cross-product auxiliary columns: `(col_name, arg_sql)` tuples
    /// for algebraic CORR/COVAR/REGR_* maintenance (P3-2).
    /// Columns: sumx, sumy, sumxy, sumx2, sumy2 per aggregate.
    covar_aux_columns: Vec<(String, String)>,
    /// Nonnull-count auxiliary columns: `(nonnull_col_name, arg_sql)` tuples
    /// for non-DISTINCT SUM aggregates above FULL JOIN children (P2-2).
    /// Empty if no such aggregates exist.
    nonnull_aux_columns: Vec<(String, String)>,
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
    had_nested_window_rewrite: bool,
) -> Result<ValidatedQuery, PgTrickleError> {
    // Validate the defining query and extract output columns
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
        // NS-1: Warn when the query has ORDER BY but no LIMIT.
        // ORDER BY without LIMIT has no effect on a stream table because
        // the underlying storage table row order is undefined.
        if crate::dvm::has_order_by_without_limit(query) {
            pgrx::warning!(
                "pg_trickle: ORDER BY without LIMIT has no effect on stream tables — \
                 storage row order is undefined. Use ORDER BY with LIMIT for TopK, \
                 or remove ORDER BY."
            );
        }
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
            pgrx::warning!(
                "[pg_trickle] Falling back to FULL refresh: unsupported construct \
                 in defining query.\n\
                 Construct: {e}\n\
                 See docs/DVM_OPERATORS.md for the full list of supported operators."
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
            pgrx::warning!(
                "[pg_trickle] Falling back to FULL refresh: query references materialized \
                 views or foreign tables ({}).\n\
                 Suggestion: replace materialized view references with regular views or \
                 tables, or convert them to stream tables so changes can be tracked \
                 incrementally.",
                e
            );
            *refresh_mode = RefreshMode::Full;
        } else {
            return Err(e);
        }
    }

    // EC-03: Nested window expressions (e.g. CASE WHEN ROW_NUMBER() OVER (...) ...)
    // were rewritten into subqueries. DVM parsing succeeds on the rewritten form,
    // but delta SQL generation fails at refresh time. Downgrade to FULL in AUTO
    // mode; emit a warning in explicit DIFFERENTIAL mode.
    if had_nested_window_rewrite
        && (*refresh_mode == RefreshMode::Differential || *refresh_mode == RefreshMode::Immediate)
    {
        if is_auto {
            pgrx::warning!(
                "[pg_trickle] Falling back to FULL refresh: query contains window functions \
                 inside expressions (e.g. CASE WHEN ROW_NUMBER() OVER (...) ...).\n\
                 Suggestion: move the window function into a subquery or CTE and apply \
                 the surrounding expression (CASE, arithmetic, etc.) in the outer query."
            );
            *refresh_mode = RefreshMode::Full;
        } else {
            pgrx::warning!(
                "pg_trickle: Query contains window functions inside expressions \
                 (e.g. CASE WHEN ROW_NUMBER() OVER (...) ...). This pattern is not \
                 supported by differential maintenance and will fall back to full \
                 recomputation at refresh time.\n\
                 Suggestion: move the window function into a subquery or CTE and apply \
                 the surrounding expression in the outer query, or use FULL or AUTO \
                 refresh mode."
            );
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
            Ok(tree) => {
                // Emit advisory warnings (e.g. NATURAL JOIN column drift) exactly
                // once here — downstream parse calls (cache pre-warm, row-id
                // derivation) silently discard them via ParseResult.warnings.
                for msg in &tree.warnings {
                    pgrx::warning!("{}", msg);
                }
                Some(tree)
            }
            Err(e) if is_auto && *refresh_mode == RefreshMode::Differential => {
                pgrx::warning!(
                    "[pg_trickle] Falling back to FULL refresh: query cannot use \
                     differential maintenance ({}).\n\
                     Suggestion: simplify the query to use supported SQL constructs, \
                     or break it into multiple stream tables chained together. \
                     Run SELECT * FROM pgtrickle.explain_refresh_mode('<table>') after \
                     creation for diagnostics. \
                     See docs/DVM_OPERATORS.md for the full list of supported operators.",
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

    // Volatility check — VOL-1: controlled by volatile_function_policy GUC
    if let Some(ref pr) = parsed_tree {
        let vol = crate::dvm::tree_worst_volatility_with_registry(pr)?;
        match vol {
            'v' => {
                let policy = crate::config::pg_trickle_volatile_function_policy();
                match policy {
                    crate::config::VolatileFunctionPolicy::Reject => {
                        return Err(PgTrickleError::UnsupportedOperator(
                            "Defining query contains volatile expressions (e.g., random(), \
                             clock_timestamp(), or custom volatile operators). Volatile \
                             functions and operators are not supported in DIFFERENTIAL or \
                             IMMEDIATE mode because they produce different values on each \
                             evaluation, breaking delta computation. Use FULL refresh mode \
                             instead, or replace with a deterministic alternative. \
                             (Override with: SET pg_trickle.volatile_function_policy = 'warn' or 'allow')"
                                .into(),
                        ));
                    }
                    crate::config::VolatileFunctionPolicy::Warn => {
                        pgrx::warning!(
                            "Defining query contains volatile expressions (e.g., random(), \
                             clock_timestamp()). Volatile functions produce different values \
                             on each evaluation, which may break delta computation. \
                             Allowed by pg_trickle.volatile_function_policy = 'warn'."
                        );
                    }
                    crate::config::VolatileFunctionPolicy::Allow => {
                        // Silent — user explicitly opted in.
                    }
                }
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

    // G12-AGG: Warn when DIFFERENTIAL mode is used with group-rescan aggregates.
    // These aggregates (STRING_AGG, ARRAY_AGG, JSON_AGG, etc.) require full
    // re-aggregation of affected groups on every refresh, which is correct but
    // slower than the algebraic path used by COUNT/SUM/AVG.
    if let Some(ref pr) = parsed_tree {
        let rescan_names = pr.tree.group_rescan_aggregate_names();
        if !rescan_names.is_empty() {
            let names_str = rescan_names.join(", ");
            pgrx::warning!(
                "pg_trickle: DIFFERENTIAL mode with group-rescan aggregates [{}]. \
                 These aggregates re-scan affected groups from source data on each \
                 refresh. This is correct but may be slower than algebraic aggregates \
                 (COUNT, SUM, AVG). Use FULL mode if group-rescan performance is \
                 unacceptable, or replace with algebraic alternatives where possible.",
                names_str,
            );
        }
    }

    // DIAG-2: Warn when algebraic aggregates are used with low-cardinality
    // GROUP BY columns — the DIFFERENTIAL overhead may not be justified.
    // (Placeholder — actual check runs after source_relids is computed below.)
    let diag2_info: Option<(Vec<String>, Vec<String>, i32)> = {
        let threshold = crate::config::pg_trickle_agg_diff_cardinality_threshold();
        if threshold > 0 {
            if let Some(ref pr) = parsed_tree {
                let alg_names = pr.tree.algebraic_aggregate_names();
                if !alg_names.is_empty() {
                    pr.tree
                        .group_by_columns()
                        .map(|gc| (alg_names, gc, threshold))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };

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
    let source_relids = normalize_source_relations(extract_source_relations(q)?);

    // DIAG-2: Now that source_relids is available, emit the low-cardinality warning.
    if let Some((alg_names, group_cols, threshold)) = diag2_info {
        let estimated_groups = estimate_group_cardinality(&source_relids, &group_cols);
        if let Some(est) = estimated_groups
            && est > 0
            && est < threshold as i64
        {
            pgrx::warning!(
                "pg_trickle: DIFFERENTIAL mode with algebraic aggregates [{}] \
                 and estimated GROUP BY cardinality {} (below threshold {}). \
                 Consider refresh_mode='full' or 'auto' for low-cardinality \
                 groupings. Adjust pg_trickle.agg_diff_cardinality_threshold \
                 to tune this warning.",
                alg_names.join(", "),
                est,
                threshold,
            );
        }
    }

    // EC-06: Detect keyless sources
    let has_keyless_source = source_relids.iter().any(|(oid, source_type)| {
        if source_type != "TABLE" && source_type != "FOREIGN_TABLE" && source_type != "MATVIEW" {
            return false;
        }
        cdc::resolve_pk_columns(*oid)
            .map(|pk| pk.is_empty())
            .unwrap_or(false)
    });

    let avg_aux_columns = parsed_tree
        .as_ref()
        .map(|pr| pr.tree.avg_aux_columns())
        .unwrap_or_default();

    let sum2_aux_columns = parsed_tree
        .as_ref()
        .map(|pr| pr.tree.sum2_aux_columns())
        .unwrap_or_default();

    let covar_aux_columns = parsed_tree
        .as_ref()
        .map(|pr| pr.tree.covar_aux_columns())
        .unwrap_or_default();

    let nonnull_aux_columns = parsed_tree
        .as_ref()
        .map(|pr| pr.tree.nonnull_aux_columns())
        .unwrap_or_default();

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
        avg_aux_columns,
        sum2_aux_columns,
        covar_aux_columns,
        nonnull_aux_columns,
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
    avg_aux_columns: &[(String, String, String)],
    sum2_aux_columns: &[(String, String)],
    covar_aux_columns: &[(String, String)],
    nonnull_aux_columns: &[(String, String)],
    // A1-1: partition key column name, or None for non-partitioned STs.
    partition_key: Option<&str>,
) -> Result<pg_sys::Oid, PgTrickleError> {
    let storage_needs_pgt_count = needs_pgt_count;
    let storage_ddl = build_create_table_sql(
        schema,
        table_name,
        columns,
        storage_needs_pgt_count,
        needs_dual_count,
        avg_aux_columns,
        sum2_aux_columns,
        covar_aux_columns,
        nonnull_aux_columns,
        partition_key,
    );
    Spi::run(&storage_ddl)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to create storage table: {}", e)))?;

    // A1-1/A1-3b: For partitioned storage tables, create child partitions.
    // RANGE/LIST: create a catch-all default partition so rows are never rejected.
    // HASH: create N child partitions (no default allowed by PostgreSQL).
    if let Some(pk) = partition_key {
        let method = parse_partition_method(pk);
        match method {
            PartitionMethod::Hash => {
                let modulus = parse_hash_modulus(pk).unwrap_or(4);
                for remainder in 0..modulus {
                    let child_name = format!("{table_name}_p{remainder}");
                    let child_sql = format!(
                        "CREATE TABLE {}.{} PARTITION OF {}.{} \
                         FOR VALUES WITH (modulus {modulus}, remainder {remainder})",
                        quote_identifier(schema),
                        quote_identifier(&child_name),
                        quote_identifier(schema),
                        quote_identifier(table_name),
                    );
                    Spi::run(&child_sql).map_err(|e| {
                        PgTrickleError::SpiError(format!(
                            "Failed to create hash partition {child_name}: {e}"
                        ))
                    })?;
                }
            }
            PartitionMethod::Range | PartitionMethod::List => {
                let default_partition_sql = format!(
                    "CREATE TABLE {}.{} PARTITION OF {}.{} DEFAULT",
                    quote_identifier(schema),
                    quote_identifier(&format!("{table_name}_default")),
                    quote_identifier(schema),
                    quote_identifier(table_name),
                );
                Spi::run(&default_partition_sql).map_err(|e| {
                    PgTrickleError::SpiError(format!("Failed to create default partition: {}", e))
                })?;
            }
        }
    }

    let pgt_relid = get_table_oid(schema, table_name)?;

    // Create index on __pgt_row_id (EC-06: non-unique for keyless sources)
    //
    // A-4 / AUTO-IDX-2: When auto_index is enabled and the output schema has
    // <= 8 user columns, create a covering index with INCLUDE clause to enable
    // index-only scans during MERGE. This eliminates the heap fetch for matched
    // rows, giving 20-50% MERGE time reduction for small-delta / large-target
    // scenarios.
    //
    // A1-1: PostgreSQL does not support global UNIQUE indexes on partitioned
    // tables. Force a non-unique index when the storage table is partitioned.
    let auto_index = crate::config::pg_trickle_auto_index();
    const COVERING_INDEX_MAX_COLUMNS: usize = 8;
    let include_clause =
        if auto_index && columns.len() <= COVERING_INDEX_MAX_COLUMNS && !columns.is_empty() {
            let include_cols: Vec<String> = columns
                .iter()
                .map(|c| quote_identifier(&c.name).to_string())
                .collect();
            format!(" INCLUDE ({})", include_cols.join(", "))
        } else {
            String::new()
        };
    let is_partitioned = partition_key.is_some();
    let index_sql = if has_keyless_source || is_partitioned {
        format!(
            "CREATE INDEX ON {}.{} (__pgt_row_id){include_clause}",
            quote_identifier(schema),
            quote_identifier(table_name),
        )
    } else {
        format!(
            "CREATE UNIQUE INDEX ON {}.{} (__pgt_row_id){include_clause}",
            quote_identifier(schema),
            quote_identifier(table_name),
        )
    };
    Spi::run(&index_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to create row_id index: {}", e)))?;

    // DML guard trigger
    install_dml_guard_trigger(schema, table_name)?;

    // AUTO-IDX-1: Auto-create indexes on GROUP BY / DISTINCT columns.
    // Gated behind pg_trickle.auto_index GUC (default true).
    if auto_index {
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

        // DISTINCT composite index: when a DISTINCT query has no GROUP BY,
        // index the output columns to speed up deduplication lookups.
        if let Some(pr) = parsed_tree
            && pr.tree.group_by_columns().is_none()
            && pr.tree.has_distinct()
        {
            let distinct_cols = pr.tree.output_columns();
            if !distinct_cols.is_empty() && distinct_cols.len() <= COVERING_INDEX_MAX_COLUMNS {
                let quoted_cols: Vec<String> = distinct_cols
                    .iter()
                    .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
                    .collect();
                let distinct_index_sql = format!(
                    "CREATE INDEX ON {}.{} ({})",
                    quote_identifier(schema),
                    quote_identifier(table_name),
                    quoted_cols.join(", "),
                );
                Spi::run(&distinct_index_sql).map_err(|e| {
                    PgTrickleError::SpiError(format!("Failed to create distinct index: {}", e))
                })?;
            }
        }
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
    pooler_compatibility_mode: bool,
    partition_by: Option<&str>,
    max_differential_joins: Option<i32>,
    max_delta_fraction: Option<f64>,
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
        pooler_compatibility_mode,
        partition_by,
        max_differential_joins,
        max_delta_fraction,
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

        let (snapshot, fingerprint) =
            if source_type == "TABLE" || source_type == "FOREIGN_TABLE" || source_type == "MATVIEW"
            {
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
            } else if source_type == "MATVIEW" {
                cdc::setup_matview_polling(*source_oid, pgt_id, &change_schema)?;
            } else if source_type == "STREAM_TABLE" {
                // ST-ST-1: Ensure the upstream ST has a change buffer so
                // downstream STs can consume differential deltas.
                let upstream_pgt_id =
                    crate::catalog::StreamTableMeta::pgt_id_for_relid(*source_oid);
                if let Some(up_id) = upstream_pgt_id {
                    cdc::ensure_st_change_buffer(up_id, *source_oid, &change_schema)?;
                }
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

/// Manage auxiliary columns (__pgt_count, __pgt_count_l/r, __pgt_aux_sum_*,
/// __pgt_aux_count_*, __pgt_aux_sum2_*, __pgt_aux_sumx_*, __pgt_aux_nonnull_*)
/// during ALTER QUERY when the query type or aggregate composition changes.
#[allow(clippy::too_many_arguments)]
fn migrate_aux_columns(
    schema: &str,
    table_name: &str,
    old_needs_pgt_count: bool,
    old_needs_dual_count: bool,
    new_needs_pgt_count: bool,
    new_needs_dual_count: bool,
    new_needs_union_dedup: bool,
    old_avg_aux: &[(String, String, String)],
    new_avg_aux: &[(String, String, String)],
    old_sum2_aux: &[(String, String)],
    new_sum2_aux: &[(String, String)],
    old_covar_aux: &[(String, String)],
    new_covar_aux: &[(String, String)],
    old_nonnull_aux: &[(String, String)],
    new_nonnull_aux: &[(String, String)],
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

    // Transition: AVG auxiliary columns (__pgt_aux_sum_*, __pgt_aux_count_*)
    let old_avg_names: std::collections::HashSet<(&str, &str)> = old_avg_aux
        .iter()
        .map(|(s, c, _)| (s.as_str(), c.as_str()))
        .collect();
    let new_avg_names: std::collections::HashSet<(&str, &str)> = new_avg_aux
        .iter()
        .map(|(s, c, _)| (s.as_str(), c.as_str()))
        .collect();
    // Add new AVG aux columns
    for (sum_col, count_col, _) in new_avg_aux {
        if !old_avg_names.contains(&(sum_col.as_str(), count_col.as_str())) {
            Spi::run(&format!(
                "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} NUMERIC NOT NULL DEFAULT 0",
                quoted_table,
                quote_identifier(sum_col),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Spi::run(&format!(
                "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} BIGINT NOT NULL DEFAULT 0",
                quoted_table,
                quote_identifier(count_col),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }
    // Drop removed AVG aux columns
    for (sum_col, count_col, _) in old_avg_aux {
        if !new_avg_names.contains(&(sum_col.as_str(), count_col.as_str())) {
            Spi::run(&format!(
                "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
                quoted_table,
                quote_identifier(sum_col),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            Spi::run(&format!(
                "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
                quoted_table,
                quote_identifier(count_col),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }

    // Transition: sum-of-squares auxiliary columns (__pgt_aux_sum2_*)
    let old_sum2_names: std::collections::HashSet<&str> =
        old_sum2_aux.iter().map(|(n, _)| n.as_str()).collect();
    let new_sum2_names: std::collections::HashSet<&str> =
        new_sum2_aux.iter().map(|(n, _)| n.as_str()).collect();
    // Add new sum2 aux columns
    for (col_name, _) in new_sum2_aux {
        if !old_sum2_names.contains(col_name.as_str()) {
            Spi::run(&format!(
                "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} NUMERIC NOT NULL DEFAULT 0",
                quoted_table,
                quote_identifier(col_name),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }
    // Drop removed sum2 aux columns
    for (col_name, _) in old_sum2_aux {
        if !new_sum2_names.contains(col_name.as_str()) {
            Spi::run(&format!(
                "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
                quoted_table,
                quote_identifier(col_name),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }

    // Transition: cross-product auxiliary columns (__pgt_aux_sum{x,y,xy,x2,y2}_*)
    // for CORR/COVAR/REGR_* algebraic maintenance (P3-2).
    let old_covar_names: std::collections::HashSet<&str> =
        old_covar_aux.iter().map(|(n, _)| n.as_str()).collect();
    let new_covar_names: std::collections::HashSet<&str> =
        new_covar_aux.iter().map(|(n, _)| n.as_str()).collect();
    // Add new covar aux columns
    for (col_name, _) in new_covar_aux {
        if !old_covar_names.contains(col_name.as_str()) {
            Spi::run(&format!(
                "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} NUMERIC NOT NULL DEFAULT 0",
                quoted_table,
                quote_identifier(col_name),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }
    // Drop removed covar aux columns
    for (col_name, _) in old_covar_aux {
        if !new_covar_names.contains(col_name.as_str()) {
            Spi::run(&format!(
                "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
                quoted_table,
                quote_identifier(col_name),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }

    // Transition: nonnull-count auxiliary columns (__pgt_aux_nonnull_*)
    // for SUM NULL-transition correction (P2-2).
    let old_nonnull_names: std::collections::HashSet<&str> =
        old_nonnull_aux.iter().map(|(n, _)| n.as_str()).collect();
    let new_nonnull_names: std::collections::HashSet<&str> =
        new_nonnull_aux.iter().map(|(n, _)| n.as_str()).collect();
    // Add new nonnull aux columns
    for (col_name, _) in new_nonnull_aux {
        if !old_nonnull_names.contains(col_name.as_str()) {
            Spi::run(&format!(
                "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} BIGINT NOT NULL DEFAULT 0",
                quoted_table,
                quote_identifier(col_name),
            ))
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }
    }
    // Drop removed nonnull aux columns
    for (col_name, _) in old_nonnull_aux {
        if !new_nonnull_names.contains(col_name.as_str()) {
            Spi::run(&format!(
                "ALTER TABLE {} DROP COLUMN IF EXISTS {}",
                quoted_table,
                quote_identifier(col_name),
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
    let rw = run_query_rewrite_pipeline(new_query)?;
    let rewritten_query = rw.query;

    // Determine the effective refresh mode — use the ST's current mode
    let mut refresh_mode = st.refresh_mode;

    // Validate and parse the new query
    let vq = validate_and_parse_query(
        &rewritten_query,
        &mut refresh_mode,
        false,
        rw.had_nested_window_rewrite,
    )?;

    // Cycle detection on the new dependency set (ALTER-aware: replaces
    // the existing ST's edges rather than creating a sentinel node).
    // Pass the proposed query so monotonicity of the altered ST's new
    // query is checked when it participates in a cycle.
    check_for_cycles_alter(st.pgt_id, &vq.source_relids, &rewritten_query)?;

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
    let old_avg_aux = crate::dvm::query_avg_aux_columns(&st.defining_query);
    let old_sum2_aux = crate::dvm::query_sum2_aux_columns(&st.defining_query);
    let old_covar_aux = crate::dvm::query_covar_aux_columns(&st.defining_query);
    let old_nonnull_aux = crate::dvm::query_nonnull_aux_columns(&st.defining_query);

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
    template_cache::invalidate(st.pgt_id);
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
                &vq.avg_aux_columns,
                &vq.sum2_aux_columns,
                &vq.covar_aux_columns,
                &vq.nonnull_aux_columns,
                st.st_partition_key.as_deref(), // A1-1c: preserve partition key on query change
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
            &old_avg_aux,
            &vq.avg_aux_columns,
            &old_sum2_aux,
            &vq.sum2_aux_columns,
            &old_covar_aux,
            &vq.covar_aux_columns,
            &old_nonnull_aux,
            &vq.nonnull_aux_columns,
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
        let (snapshot, fingerprint) =
            if source_type == "TABLE" || source_type == "FOREIGN_TABLE" || source_type == "MATVIEW"
            {
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
        } else if source_type == "MATVIEW" && !refresh_mode.is_immediate() {
            cdc::setup_matview_polling(*source_oid, st.pgt_id, &change_schema)?;
        }
    }

    // Sync CDC trigger functions and change buffer columns for kept sources.
    // When ALTER QUERY adds references to new source columns (e.g. a query
    // changing from `SELECT id, val` to `SELECT id, val, status`), the change
    // buffer for the unchanged source still lacks `new_status`/`old_status`.
    // Rebuilding the trigger function re-reads the updated catalog dependency
    // and calls sync_change_buffer_columns to add the missing columns.
    if !refresh_mode.is_immediate() {
        for (source_oid, source_type) in &dep_diff.kept {
            if source_type == "TABLE" {
                let cdc_mode = old_deps
                    .iter()
                    .find(|d| d.source_relid == *source_oid)
                    .map(|d| d.cdc_mode)
                    .unwrap_or(CdcMode::Trigger);
                if matches!(cdc_mode, CdcMode::Trigger)
                    && let Err(e) = cdc::rebuild_cdc_trigger_function(*source_oid, &change_schema)
                {
                    pgrx::warning!(
                        "pg_trickle: failed to sync CDC trigger for kept source {}: {}",
                        source_oid.to_u32(),
                        e
                    );
                }
            }
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
    shmem::signal_dag_invalidation(st.pgt_id);
    template_cache::invalidate(st.pgt_id);
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

    // CYC-6: Recompute SCC assignments — the query change may have created
    // or broken a cycle.
    if config::pg_trickle_allow_circular()
        && let Err(e) = assign_scc_ids_from_dag()
    {
        pgrx::warning!("Failed to recompute SCCs after ALTER QUERY: {}", e);
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

/// A1-1c: Change the partition key on an existing stream table.
///
/// This is a destructive operation that:
/// 1. Validates the new partition key against the ST's output columns.
/// 2. Drops the old storage table (detaching pgt_relid first).
/// 3. Recreates it with the new partition scheme (or unpartitioned).
/// 4. Updates the catalog.
/// 5. Runs a full refresh to repopulate.
fn alter_stream_table_partition_key(
    st: &StreamTableMeta,
    schema: &str,
    table_name: &str,
    new_partition_key: Option<&str>,
) -> Result<(), PgTrickleError> {
    // Get current storage columns for validation.
    let columns = get_storage_table_columns(schema, table_name)?;

    // Validate new partition key against current columns.
    if let Some(pk) = new_partition_key {
        validate_partition_key(pk, &columns)?;
    }

    pgrx::warning!(
        "pg_trickle: ALTER partition_by on {schema}.{table_name} requires full storage rebuild. \
         The storage table will be recreated and a full refresh applied."
    );

    // Detach pgt_relid so the sql_drop event trigger does not delete the
    // catalog row when we drop the old table.
    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET pgt_relid = 0 WHERE pgt_id = $1",
        &[st.pgt_id.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // Drop the old storage table (CASCADE drops child partitions too).
    let drop_sql = format!(
        "DROP TABLE IF EXISTS {}.{} CASCADE",
        quote_identifier(schema),
        quote_identifier(table_name),
    );
    Spi::run(&drop_sql)
        .map_err(|e| PgTrickleError::SpiError(format!("Failed to drop storage table: {e}")))?;

    // Recompute auxiliary column needs from the defining query.
    let needs_pgt_count = crate::dvm::query_needs_pgt_count(&st.defining_query);
    let needs_dual_count = crate::dvm::query_needs_dual_count(&st.defining_query);
    let avg_aux = crate::dvm::query_avg_aux_columns(&st.defining_query);
    let sum2_aux = crate::dvm::query_sum2_aux_columns(&st.defining_query);
    let covar_aux = crate::dvm::query_covar_aux_columns(&st.defining_query);
    let nonnull_aux = crate::dvm::query_nonnull_aux_columns(&st.defining_query);

    // Recreate the storage table with the new partition scheme.
    let new_pgt_relid = setup_storage_table(
        schema,
        table_name,
        &columns,
        needs_pgt_count,
        needs_dual_count,
        st.has_keyless_source,
        st.refresh_mode,
        None, // parsed_tree not needed for storage creation
        &avg_aux,
        &sum2_aux,
        &covar_aux,
        &nonnull_aux,
        new_partition_key,
    )?;

    // Update catalog: new relid + new partition key.
    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET pgt_relid = $1, st_partition_key = $2, \
             is_populated = false, frontier = NULL, updated_at = now() \
         WHERE pgt_id = $3",
        &[
            new_pgt_relid.into(),
            new_partition_key.into(),
            st.pgt_id.into(),
        ],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // Invalidate caches.
    template_cache::invalidate(st.pgt_id);
    shmem::bump_cache_generation();
    refresh::invalidate_merge_cache(st.pgt_id);

    // Full refresh to repopulate.
    let updated_st = StreamTableMeta::get_by_name(schema, table_name)?;
    let deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();
    let source_oids: Vec<pg_sys::Oid> = deps
        .iter()
        .filter(|d| d.source_type == "TABLE")
        .map(|d| d.source_relid)
        .collect();
    execute_manual_full_refresh(&updated_st, schema, table_name, &source_oids)?;

    pgrx::info!(
        "pg_trickle: partition key for {schema}.{table_name} changed to {}; full refresh applied.",
        new_partition_key.unwrap_or("(none)"),
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
    pooler_compatibility_mode: bool,
    partition_by: Option<&str>,
    max_differential_joins: Option<i32>,
    max_delta_fraction: Option<f64>,
) -> Result<(), PgTrickleError> {
    let is_auto = RefreshMode::is_auto_str(refresh_mode_str);
    let mut refresh_mode = RefreshMode::from_str(refresh_mode_str)?;

    // Parse diamond consistency — default to 'atomic' when not specified
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

    // G15-PV: diamond_schedule_policy='slowest' only makes sense when
    // diamond_consistency='atomic'.  Without atomic reads the 'slowest'
    // policy delays refreshes at convergence nodes without providing any
    // consistency guarantee.
    if dsp == DiamondSchedulePolicy::Slowest && dc == DiamondConsistency::None {
        return Err(PgTrickleError::InvalidArgument(
            "diamond_schedule_policy = 'slowest' requires diamond_consistency = 'atomic'. \
             The 'slowest' policy is only meaningful when atomic cross-branch reads are \
             enabled. Set diamond_consistency = 'atomic' or use diamond_schedule_policy = 'fastest'."
                .to_string(),
        ));
    }

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
    let rw = run_query_rewrite_pipeline(query)?;
    let query = &rw.query;

    // ── Validate & parse ───────────────────────────────────────────
    let vq = validate_and_parse_query(
        query,
        &mut refresh_mode,
        is_auto,
        rw.had_nested_window_rewrite,
    )?;
    // Warnings
    warn_source_table_properties(&vq.source_relids);
    warn_select_star(query);

    // Summary warning when AUTO mode resulted in FULL refresh
    if is_auto && refresh_mode == RefreshMode::Full {
        pgrx::warning!(
            "[pg_trickle] Stream table '{}' will use FULL refresh instead of DIFFERENTIAL. \
             Each refresh will recompute the entire result set from scratch, which is slower \
             than incremental maintenance. See the warnings above for the specific reason \
             and how to fix it. \
             Use SELECT * FROM pgtrickle.explain_refresh_mode('{}') to check the effective \
             mode after the first refresh.",
            name,
            name,
        );
    }

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

    // A1-1: Validate partition_by if provided.
    if let Some(pk) = partition_by {
        validate_partition_key(pk, &vq.columns)?;
        // Partitioned stream tables with IMMEDIATE refresh are not supported —
        // IMMEDIATE triggers fire at DML time and the partition-key range is
        // not known until the delta is accumulated.
        if refresh_mode.is_immediate() {
            return Err(PgTrickleError::InvalidArgument(
                "partition_by is not supported with IMMEDIATE refresh mode. \
                 Use DIFFERENTIAL or AUTO refresh mode."
                    .to_string(),
            ));
        }
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
        &vq.avg_aux_columns,
        &vq.sum2_aux_columns,
        &vq.covar_aux_columns,
        &vq.nonnull_aux_columns,
        partition_by,
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
    // Capture before schedule_str is moved into insert_catalog_and_deps.
    let is_calculated = schedule_str.is_none();
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
        pooler_compatibility_mode,
        partition_by,
        max_differential_joins,
        max_delta_fraction,
    )?;

    // ── Phase 2: CDC / IVM trigger setup ──
    setup_trigger_infrastructure(&vq.source_relids, refresh_mode, pgt_id, pgt_relid, query)?;

    // ── NS-5: Diamond consistency NOTICE ──
    // When the user explicitly opted out of atomic reads (diamond_consistency='none'),
    // check if this new ST is a diamond convergence point and advise.
    if dc == DiamondConsistency::None
        && let Ok(dag) = StDag::build_from_catalog(config::pg_trickle_default_schedule_seconds())
    {
        let diamonds = dag.detect_diamonds();
        if diamonds
            .iter()
            .any(|d| d.convergence == NodeId::StreamTable(pgt_id))
        {
            pgrx::notice!(
                "pg_trickle: Diamond dependency detected for \"{}\".\"{}\" and \
                 diamond_consistency is 'none' — cross-branch reads may be inconsistent. \
                 Consider diamond_consistency='atomic' for consistent results.",
                schema,
                table_name
            );
        }
    }

    // ── NS-7: CALCULATED schedule with no downstream NOTICE ──
    // CALCULATED stream tables inherit their schedule from downstream dependents.
    // If none exist yet, their schedule falls back to the default GUC and the user
    // may not realise rows won't be refreshed on their intended cadence.
    if is_calculated && !refresh_mode.is_immediate() {
        let has_downstream = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(\
               SELECT 1 FROM pgtrickle.pgt_dependencies d \
               WHERE d.source_relid = {relid} AND d.pgt_id != {pid}\
             )",
            relid = pgt_relid.to_u32(),
            pid = pgt_id,
        ))
        .unwrap_or(Some(false))
        .unwrap_or(false);
        if !has_downstream {
            let fallback_secs = config::pg_trickle_default_schedule_seconds();
            pgrx::notice!(
                "pg_trickle: Stream table \"{}\".\"{}\" uses CALCULATED schedule but has no \
                 downstream dependents yet — it will fall back to the default schedule \
                 (pg_trickle.default_schedule_seconds = {}s). Add a downstream stream table \
                 that references this one to activate the intended schedule.",
                schema,
                table_name,
                fallback_secs
            );
        }
    }

    // ── Phase 2a: CYC-6 — Assign SCC IDs when circular dependencies exist ──
    if config::pg_trickle_allow_circular() {
        assign_scc_ids_from_dag()?;
    }

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
            &vq.avg_aux_columns,
            &vq.sum2_aux_columns,
            &vq.covar_aux_columns,
            &vq.nonnull_aux_columns,
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

        // G12-ERM-1: Record the effective refresh mode for the initial
        // population so monitoring and tests can observe the mode from
        // the very first cycle without waiting for a scheduler refresh.
        let initial_eff_mode = if vq.topk_info.is_some() {
            "TOP_K"
        } else {
            refresh_mode.as_str()
        };
        if let Err(e) = StreamTableMeta::update_effective_refresh_mode(pgt_id, initial_eff_mode) {
            pgrx::debug1!(
                "[pg_trickle] Failed to set initial effective_refresh_mode for {}.{}: {}",
                schema,
                table_name,
                e
            );
        }
    }

    // Pre-warm delta SQL + MERGE template cache for DIFFERENTIAL mode,
    // so the first refresh avoids the cold-start parsing penalty.
    if refresh_mode == RefreshMode::Differential && initialize {
        let st = StreamTableMeta::get_by_name(&schema, &table_name)?;
        refresh::prewarm_merge_cache(&st);
    }

    // Signal scheduler to rebuild DAG
    shmem::signal_dag_invalidation(pgt_id);

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
    pooler_compatibility_mode: default!(Option<bool>, "NULL"),
    tier: default!(Option<&str>, "NULL"),
    fuse: default!(Option<&str>, "NULL"),
    fuse_ceiling: default!(Option<i64>, "NULL"),
    fuse_sensitivity: default!(Option<i32>, "NULL"),
    partition_by: default!(Option<&str>, "NULL"),
    max_differential_joins: default!(Option<i32>, "NULL"),
    max_delta_fraction: default!(Option<f64>, "NULL"),
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
        pooler_compatibility_mode,
        tier,
        fuse,
        fuse_ceiling,
        fuse_sensitivity,
        partition_by,
        max_differential_joins,
        max_delta_fraction,
    );
    if let Err(e) = result {
        raise_error_with_context(e);
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
    pooler_compatibility_mode: Option<bool>,
    tier: Option<&str>,
    fuse: Option<&str>,
    fuse_ceiling_arg: Option<i64>,
    fuse_sensitivity_arg: Option<i32>,
    partition_by: Option<&str>,
    max_differential_joins: Option<i32>,
    max_delta_fraction: Option<f64>,
) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let mut st = StreamTableMeta::get_by_name(&schema, &table_name)?;
    let qualified_name = format!("{schema}.{table_name}");

    // ── Query migration (must run first, before other parameter changes) ──
    if let Some(new_query) = query {
        alter_stream_table_query(&st, &schema, &table_name, new_query)?;
        st = StreamTableMeta::get_by_name(&schema, &table_name)?;
    }

    // ── A1-1c: Partition key migration ──────────────────────────────────
    // partition_by => '' (empty string) removes partitioning.
    // partition_by => 'col' or 'LIST:col' adds/changes partitioning.
    // This requires storage table recreation + full refresh.
    if let Some(new_pk_raw) = partition_by {
        let new_pk = if new_pk_raw.trim().is_empty() {
            None
        } else {
            Some(new_pk_raw)
        };

        // Only act when the partition key is actually changing.
        let old_pk = st.st_partition_key.as_deref();
        if new_pk != old_pk {
            alter_stream_table_partition_key(&st, &schema, &table_name, new_pk)?;
            st = StreamTableMeta::get_by_name(&schema, &table_name)?;
        }
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
                // G15-PV: Validate combined diamond params before persisting.
                let effective_dsp = diamond_schedule_policy
                    .and_then(DiamondSchedulePolicy::from_sql_str)
                    .unwrap_or(st.diamond_schedule_policy);
                if effective_dsp == DiamondSchedulePolicy::Slowest && dc == DiamondConsistency::None
                {
                    return Err(PgTrickleError::InvalidArgument(
                        "diamond_schedule_policy = 'slowest' requires diamond_consistency = 'atomic'. \
                         The 'slowest' policy is only meaningful when atomic cross-branch reads are \
                         enabled. Set diamond_consistency = 'atomic' or use diamond_schedule_policy = 'fastest'."
                            .to_string(),
                    ));
                }
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
                // G15-PV: Validate combined diamond params.  Only check when
                // dc is not also being changed (handled in the dc block above).
                if diamond_consistency.is_none() {
                    let effective_dc = st.diamond_consistency;
                    if p == DiamondSchedulePolicy::Slowest
                        && effective_dc == DiamondConsistency::None
                    {
                        return Err(PgTrickleError::InvalidArgument(
                            "diamond_schedule_policy = 'slowest' requires diamond_consistency = 'atomic'. \
                             The 'slowest' policy is only meaningful when atomic cross-branch reads are \
                             enabled. Set diamond_consistency = 'atomic' or use diamond_schedule_policy = 'fastest'."
                                .to_string(),
                        ));
                    }
                }
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

    // PB2: Update pooler compatibility mode if explicitly set.
    if let Some(pcm) = pooler_compatibility_mode {
        StreamTableMeta::update_pooler_compatibility_mode(st.pgt_id, pcm)?;
        if pcm {
            // Deallocate any existing prepared MERGE statement for this ST,
            // since it will no longer be used.
            crate::refresh::invalidate_merge_cache(st.pgt_id);
        }
    }

    // G-7: Update refresh tier if explicitly set.
    if let Some(tier_str) = tier {
        use crate::scheduler::RefreshTier;
        if !RefreshTier::is_valid_str(tier_str) {
            return Err(PgTrickleError::InvalidArgument(format!(
                "invalid tier value: '{}' (expected 'hot', 'warm', 'cold', or 'frozen')",
                tier_str
            )));
        }
        let normalized = tier_str.to_lowercase();

        // C-1b: Emit NOTICE when demoting from Hot to Cold or Frozen so
        // operators are aware their configured interval will be multiplied.
        let old_tier = RefreshTier::from_sql_str(&st.refresh_tier);
        let new_tier = RefreshTier::from_sql_str(&normalized);
        if old_tier == RefreshTier::Hot
            && matches!(new_tier, RefreshTier::Cold | RefreshTier::Frozen)
        {
            let msg = match new_tier {
                RefreshTier::Cold => format!(
                    "stream table {}.{} demoted from hot to cold — effective refresh interval is now 10× the configured schedule",
                    st.pgt_schema, st.pgt_name
                ),
                RefreshTier::Frozen => format!(
                    "stream table {}.{} demoted from hot to frozen — refresh is suspended until the tier is changed back",
                    st.pgt_schema, st.pgt_name
                ),
                _ => unreachable!(),
            };
            pgrx::notice!("{}", msg);
        }

        StreamTableMeta::update_refresh_tier(st.pgt_id, &normalized)?;
    }

    // FUSE-2: Update fuse configuration if any fuse parameter is set.
    if fuse.is_some() || fuse_ceiling_arg.is_some() || fuse_sensitivity_arg.is_some() {
        let fuse_mode = match fuse {
            Some(mode_str) => {
                let normalized = mode_str.to_lowercase();
                match normalized.as_str() {
                    "off" | "on" | "auto" => normalized,
                    _ => {
                        return Err(PgTrickleError::InvalidArgument(format!(
                            "invalid fuse value: '{}' (expected 'off', 'on', or 'auto')",
                            mode_str
                        )));
                    }
                }
            }
            None => st.fuse_mode.clone(),
        };
        let ceiling = fuse_ceiling_arg.or(st.fuse_ceiling);
        let sensitivity = fuse_sensitivity_arg.or(st.fuse_sensitivity);

        if let Some(c) = ceiling
            && c <= 0
        {
            return Err(PgTrickleError::InvalidArgument(
                "fuse_ceiling must be a positive integer".into(),
            ));
        }
        if let Some(s) = sensitivity
            && s <= 0
        {
            return Err(PgTrickleError::InvalidArgument(
                "fuse_sensitivity must be a positive integer".into(),
            ));
        }

        StreamTableMeta::update_fuse_config(st.pgt_id, &fuse_mode, ceiling, sensitivity)?;
    }

    // DI-7: Update max_differential_joins if explicitly set.
    if let Some(mdj) = max_differential_joins {
        if mdj < 0 {
            return Err(PgTrickleError::InvalidArgument(
                "max_differential_joins must be a non-negative integer (0 disables the limit)"
                    .into(),
            ));
        }
        let val: Option<i32> = if mdj == 0 { None } else { Some(mdj) };
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET max_differential_joins = $1, updated_at = now() WHERE pgt_id = $2",
            &[val.into(), st.pgt_id.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    // DI-7: Update max_delta_fraction if explicitly set.
    if let Some(mdf) = max_delta_fraction {
        if mdf < 0.0 {
            return Err(PgTrickleError::InvalidArgument(
                "max_delta_fraction must be a non-negative number (0 disables the limit)".into(),
            ));
        }
        let val: Option<f64> = if mdf == 0.0 { None } else { Some(mdf) };
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables \
             SET max_delta_fraction = $1, updated_at = now() WHERE pgt_id = $2",
            &[val.into(), st.pgt_id.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    shmem::signal_dag_invalidation(st.pgt_id);
    // G14-SHC: Remove from catalog-backed template cache.
    template_cache::invalidate(st.pgt_id);
    // G8.1: Notify other backends to flush delta/MERGE template caches.
    shmem::bump_cache_generation();

    // ERR-1c: Clear error state when a pipeline-regenerating alter succeeds.
    // This lets ALTER STREAM TABLE with a fixed query reset an ERROR table.
    if st.status == StStatus::Error {
        let _ = StreamTableMeta::clear_error_state(st.pgt_id);
        let _ = StreamTableMeta::update_status(st.pgt_id, StStatus::Active);
        Spi::run_with_args(
            "UPDATE pgtrickle.pgt_stream_tables SET consecutive_errors = 0, updated_at = now() WHERE pgt_id = $1",
            &[st.pgt_id.into()],
        )
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
    }

    Ok(())
}

/// Drop a stream table, removing the storage table and all catalog entries.
///
/// When `cascade` is `true` (the default) any downstream stream tables that
/// depend on this one are automatically dropped first.  When `cascade` is
/// `false` the function raises an error if any dependents exist, matching the
/// behaviour of PostgreSQL's own `DROP TABLE … CASCADE | RESTRICT`.
#[pg_extern(schema = "pgtrickle")]
fn drop_stream_table(name: &str, cascade: default!(bool, true)) {
    let result = drop_stream_table_impl(name, cascade);
    if let Err(e) = result {
        raise_error_with_context(e);
    }
}

fn drop_stream_table_impl(name: &str, cascade: bool) -> Result<(), PgTrickleError> {
    let mut visited_pgt_ids = HashSet::new();
    drop_stream_table_impl_inner(name, cascade, &mut visited_pgt_ids)
}

fn drop_stream_table_impl_inner(
    name: &str,
    cascade: bool,
    visited_pgt_ids: &mut HashSet<i64>,
) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    if !visited_pgt_ids.insert(st.pgt_id) {
        return Ok(());
    }

    // CASCADE: drop all stream tables that depend on this one first.
    // `get_downstream_pgt_ids` finds STs whose defining queries read from
    // this ST's storage table.  We iterate by pgt_id to avoid re-querying
    // after each recursive drop changes the catalog.
    let downstream_ids = StDependency::get_downstream_pgt_ids(st.pgt_relid)?;
    if !downstream_ids.is_empty() && !cascade {
        let names: Vec<String> = downstream_ids
            .iter()
            .filter_map(|id| StreamTableMeta::get_by_id(*id).ok().flatten())
            .map(|s| format!("{}.{}", s.pgt_schema, s.pgt_name))
            .collect();
        return Err(PgTrickleError::InvalidArgument(format!(
            "stream table {}.{} has dependent stream tables: {}. Use cascade => true to drop them automatically.",
            schema,
            table_name,
            names.join(", ")
        )));
    }
    for downstream_id in downstream_ids {
        if let Some(downstream_st) = StreamTableMeta::get_by_id(downstream_id)? {
            let qualified = format!("{}.{}", downstream_st.pgt_schema, downstream_st.pgt_name);
            drop_stream_table_impl_inner(&qualified, cascade, visited_pgt_ids)?;
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

    // Remove this ST's pgt_id from the tracked_by_pgt_ids arrays in
    // pgt_change_tracking so consumer counts stay accurate after drop.
    for dep in &deps {
        if dep.source_type == "TABLE" {
            let _ = Spi::run_with_args(
                "UPDATE pgtrickle.pgt_change_tracking \
                 SET tracked_by_pgt_ids = array_remove(tracked_by_pgt_ids, $1) \
                 WHERE source_relid = $2",
                &[st.pgt_id.into(), dep.source_relid.into()],
            );
        }
    }

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
        } else if dep.source_type == "STREAM_TABLE" {
            // ST-ST-1: If this was the last downstream consumer of an
            // upstream ST's change buffer, drop the buffer.
            let upstream_pgt_id =
                crate::catalog::StreamTableMeta::pgt_id_for_relid(dep.source_relid);
            if let Some(up_id) = upstream_pgt_id {
                let consumers = cdc::count_downstream_st_consumers(up_id);
                if consumers == 0 {
                    let change_schema = config::pg_trickle_change_buffer_schema();
                    if let Err(e) = cdc::drop_st_change_buffer_table(up_id, &change_schema) {
                        pgrx::warning!(
                            "Failed to drop ST change buffer for upstream pgt_id {}: {}",
                            up_id,
                            e
                        );
                    }
                }
            }
        }
    }

    // ST-ST-1: Drop this ST's own change buffer (if it had downstream consumers).
    {
        let change_schema = config::pg_trickle_change_buffer_schema();
        if cdc::has_st_change_buffer(st.pgt_id, &change_schema)
            && let Err(e) = cdc::drop_st_change_buffer_table(st.pgt_id, &change_schema)
        {
            pgrx::warning!(
                "Failed to drop own ST change buffer for pgt_id {}: {}",
                st.pgt_id,
                e
            );
        }
    }

    // CYC-6: Recompute SCC assignments when a cycle member is dropped.
    // The dropped ST's catalog entry is already gone, so rebuild the DAG
    // from the remaining STs and reassign scc_id values. Former cycle
    // members that are no longer in a cycle will have their scc_id cleared.
    if st.scc_id.is_some()
        && let Err(e) = assign_scc_ids_from_dag()
    {
        pgrx::warning!("Failed to recompute SCCs after drop: {}", e);
    }

    // Signal scheduler
    shmem::signal_dag_invalidation(st.pgt_id);
    // G14-SHC: Remove from catalog-backed template cache.
    template_cache::invalidate(st.pgt_id);
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
        raise_error_with_context(e);
    }
}

fn resume_stream_table_impl(name: &str) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    if st.status != StStatus::Suspended && st.status != StStatus::Error {
        return Err(PgTrickleError::InvalidArgument(format!(
            "stream table {}.{} is not suspended or in error state (current status: {})",
            schema,
            table_name,
            st.status.as_str(),
        )));
    }

    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET status = 'ACTIVE', consecutive_errors = 0, \
         last_error_message = NULL, last_error_at = NULL, updated_at = now() \
         WHERE pgt_id = $1",
        &[st.pgt_id.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    crate::monitor::alert_resumed(&schema, &table_name, st.pooler_compatibility_mode);

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
        // RefreshSkipped is a transient, non-fatal condition: another refresh
        // is already in progress on this ST. Log it at DEBUG level and return
        // successfully so that concurrent callers (e.g. the test's two racing
        // connections) do not see a client-visible error.
        if let PgTrickleError::RefreshSkipped(_) = &e {
            pgrx::debug1!("{}", e);
        } else {
            raise_error_with_context(e);
        }
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

    // Phase 10: Check if ST is suspended or in error — refuse manual refresh
    if st.status == StStatus::Suspended || st.status == StStatus::Error {
        return Err(PgTrickleError::InvalidArgument(format!(
            "stream table {}.{} is {} ; use pgtrickle.resume_stream_table('{}') first",
            schema,
            table_name,
            if st.status == StStatus::Suspended {
                "suspended"
            } else {
                "in error state"
            },
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

    // Phase 10: Advisory lock to prevent concurrent manual refresh.
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

    // PB1 row-lock: acquire FOR UPDATE SKIP LOCKED on the catalog row so
    // the background scheduler's check_skip_needed() sees this manual
    // refresh as "in progress" and skips the ST for this tick.  Without
    // this, the advisory lock (above) and the scheduler's FOR UPDATE are
    // invisible to each other, allowing both to proceed simultaneously and
    // deadlock when the scheduler's TRUNCATE and the manual refresh's
    // catalog UPDATE race for conflicting locks.
    //
    // If the scheduler already holds the row lock (currently refreshing
    // this ST), we get zero rows back — report "already in progress".
    let row_available = Spi::get_one_with_args::<i64>(
        "SELECT pgt_id FROM pgtrickle.pgt_stream_tables WHERE pgt_id = $1 FOR UPDATE SKIP LOCKED",
        &[st.pgt_id.into()],
    )
    .unwrap_or(None)
    .is_some();

    if !row_available {
        return Err(PgTrickleError::RefreshSkipped(format!(
            "{}.{} — another refresh is already in progress",
            schema, table_name,
        )));
    }

    // Reload the ST metadata now that we hold both the advisory lock and
    // the row lock.  Between the initial get_by_name() and acquiring these
    // locks, the background scheduler may have refreshed this ST and
    // advanced its frontier.  Using the stale frontier would cause the
    // differential refresh to re-process already-consumed change buffer
    // rows, producing incorrect aggregate deltas.
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    // Transaction-level advisory lock is released automatically at
    // transaction end (commit or rollback); no explicit unlock needed.
    execute_manual_refresh(&st, &schema, &table_name, &source_oids)
}

/// Inner function for manual refresh, called while advisory lock is held.
///
/// Dispatches to FULL or DIFFERENTIAL depending on the ST's refresh mode.
/// `source_oids` are pre-fetched to avoid redundant SPI calls (G-N3).
///
/// ERG-D: Records the refresh in `pgt_refresh_history` with
/// `initiated_by = 'MANUAL'`.
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
    Spi::run("SET LOCAL row_security = off") // nosemgrep: sql.row-security.disabled — intentional R3 bypass, mirrors REFRESH MATERIALIZED VIEW semantics.
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // ERG-D: Determine the action label for history recording.
    let action = if st.topk_limit.is_some() {
        "FULL"
    } else {
        match st.refresh_mode {
            RefreshMode::Full | RefreshMode::Immediate => "FULL",
            RefreshMode::Differential => {
                if st.frontier.is_none() {
                    "FULL"
                } else {
                    "DIFFERENTIAL"
                }
            }
        }
    };

    // ERG-D: Record refresh start in pgt_refresh_history.
    let now = Spi::get_one::<TimestampWithTimeZone>("SELECT now()")
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
        .ok_or_else(|| PgTrickleError::InternalError("now() returned NULL".into()))?;

    let refresh_id = RefreshRecord::insert(
        st.pgt_id,
        now,
        action,
        "RUNNING",
        0,
        0,
        None,
        Some("MANUAL"),
        None, // no freshness_deadline for manual refreshes
        0,
        None,
        false,
        None, // no tick_watermark_lsn for manual refreshes
    )?;

    // TopK tables use the scoped-recomputation refresh path regardless of
    // refresh_mode (they always do ORDER BY … LIMIT N via MERGE).
    let result = if st.topk_limit.is_some() {
        refresh::execute_topk_refresh(st).map(|(ins, del)| {
            pgrx::info!(
                "Stream table {}.{} refreshed (TopK MERGE: +{} -{})",
                schema,
                table_name,
                ins,
                del,
            );
            (ins, del)
        })
    } else if st.needs_reinit {
        // When needs_reinit is set (e.g. by DDL hooks for ATTACH/DETACH
        // PARTITION, or EC-16 function body change detection), force a
        // FULL refresh regardless of the ST's refresh_mode.  This mirrors
        // the scheduler's RefreshAction::Reinitialize path.
        pgrx::info!(
            "Stream table {}.{}: needs_reinit is set, performing FULL reinitialization",
            schema,
            table_name,
        );

        // If the ST has an original_query (pre-rewrite, e.g. referencing
        // views by name), use it for the FULL refresh so the current view
        // definitions are resolved at execution time.  Then re-run the
        // rewrite pipeline to update the stored defining_query for future
        // differential refreshes.
        let refresh_st = if let Some(oq) = &st.original_query {
            let mut tmp = st.clone();
            tmp.defining_query = oq.clone();
            tmp
        } else {
            st.clone()
        };

        execute_manual_full_refresh(&refresh_st, schema, table_name, source_oids)?;

        // After the FULL refresh has committed the correct data, re-run
        // the rewrite pipeline to store the updated inlined query.
        let updated = reinit_rewrite_if_needed(st)?;

        // Clear the reinit flag after successful refresh.
        Spi::run(&format!(
            "UPDATE pgtrickle.pgt_stream_tables SET needs_reinit = FALSE WHERE pgt_id = {}",
            updated.pgt_id,
        ))
        .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        Ok((0i64, 0i64))
    } else {
        match st.refresh_mode {
            RefreshMode::Full => execute_manual_full_refresh(st, schema, table_name, source_oids)
                .map(|_| (0i64, 0i64)),
            RefreshMode::Differential => {
                execute_manual_differential_refresh(st, schema, table_name, source_oids)
                    .map(|_| (0i64, 0i64))
            }
            RefreshMode::Immediate => {
                // For IMMEDIATE mode, manual refresh does a FULL refresh
                // (re-populate from the defining query), same as pg_ivm's
                // refresh_immv(name, true).
                execute_manual_full_refresh(st, schema, table_name, source_oids)
                    .map(|_| (0i64, 0i64))
            }
        }
    };

    // ERG-D: Complete the refresh history record.
    match &result {
        Ok((rows_inserted, rows_deleted)) => {
            let _ = RefreshRecord::complete(
                refresh_id,
                "COMPLETED",
                *rows_inserted,
                *rows_deleted,
                None,
                0,
                None,
                false,
            );
            // G12-ERM-1: Persist the effective refresh mode for manual
            // refreshes, mirroring the scheduler path.  Without this,
            // effective_refresh_mode stays NULL after every manual refresh,
            // which breaks diagnostics and test assertions.
            let eff_mode = crate::refresh::take_effective_mode();
            if !eff_mode.is_empty() {
                let _ = StreamTableMeta::update_effective_refresh_mode(st.pgt_id, eff_mode);
            }
        }
        Err(e) => {
            let _ = RefreshRecord::complete(
                refresh_id,
                "FAILED",
                0,
                0,
                Some(&e.to_string()),
                0,
                None,
                false,
            );
        }
    }

    result.map(|_| ())
}

/// Execute a FULL manual refresh: truncate + repopulate from the defining query.
///
/// Re-run the query rewrite pipeline when `needs_reinit` is set and
/// the ST has an `original_query` (indicating the defining query was
/// rewritten, e.g. by view inlining). Updates the catalog's
/// `defining_query` so the full refresh uses the current view/function
/// definitions.
pub fn reinit_rewrite_if_needed(st: &StreamTableMeta) -> Result<StreamTableMeta, PgTrickleError> {
    let original = match &st.original_query {
        Some(oq) => oq.clone(),
        None => return Ok(st.clone()),
    };

    let rw = run_query_rewrite_pipeline(&original)?;
    let new_defining = rw.query;
    if new_defining == st.defining_query {
        return Ok(st.clone());
    }

    pgrx::info!(
        "Stream table {}.{}: re-inlined view/function definitions for reinit",
        st.pgt_schema,
        st.pgt_name,
    );

    // Update the catalog with the new defining query.
    Spi::run_with_args(
        "UPDATE pgtrickle.pgt_stream_tables \
         SET defining_query = $1, updated_at = now() \
         WHERE pgt_id = $2",
        &[new_defining.clone().into(), st.pgt_id.into()],
    )
    .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    let mut updated = st.clone();
    updated.defining_query = new_defining;
    Ok(updated)
}

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

    // ── Snapshot ST change buffer LSNs BEFORE TRUNCATE+INSERT ──────────
    //
    // Under READ COMMITTED, each SPI statement sees the latest committed
    // data at statement start time.  If the frontier's ST-source LSNs are
    // computed AFTER the INSERT, a concurrent upstream refresh might commit
    // new change buffer rows between the INSERT and the MAX(lsn) query.
    // The frontier would then advance past changes the INSERT never saw,
    // causing subsequent manual refreshes to believe the data is current
    // when it is actually stale.
    //
    // Capturing the LSNs before the INSERT ensures the frontier reflects
    // at most the data that was visible to the INSERT.
    let change_schema_for_snapshot =
        crate::config::pg_trickle_change_buffer_schema().replace('"', "\"\"");
    let st_source_lsn_snapshot: Vec<(i64, String)> =
        crate::catalog::StDependency::get_for_st(st.pgt_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|dep| dep.source_type == "STREAM_TABLE")
            .filter_map(|dep| {
                let upstream_pgt_id = StreamTableMeta::pgt_id_for_relid(dep.source_relid)?;
                if !crate::cdc::has_st_change_buffer(upstream_pgt_id, &change_schema_for_snapshot) {
                    return None;
                }
                let lsn = Spi::get_one::<String>(&format!(
                    "SELECT COALESCE(MAX(lsn)::text, pg_current_wal_lsn()::text) \
             FROM \"{schema}\".changes_pgt_{id}",
                    schema = change_schema_for_snapshot,
                    id = upstream_pgt_id,
                ))
                .unwrap_or(None)
                .unwrap_or_else(|| "0/0".to_string());
                Some((upstream_pgt_id, lsn))
            })
            .collect();

    let truncate_sql = format!("TRUNCATE {quoted_table}");
    Spi::run(&truncate_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

    // For aggregate/distinct STs in DIFFERENTIAL mode, inject COUNT(*)
    // into the defining query so __pgt_count is populated for subsequent
    // differential refreshes.
    let effective_query = if st.refresh_mode == RefreshMode::Differential
        && crate::dvm::query_needs_pgt_count(&st.defining_query)
    {
        let mut eq = inject_pgt_count(&st.defining_query);
        // Also inject AVG auxiliary columns for algebraic AVG maintenance.
        let avg_aux = crate::dvm::query_avg_aux_columns(&st.defining_query);
        if !avg_aux.is_empty() {
            eq = inject_avg_aux(&eq, &avg_aux);
        }
        // Also inject sum-of-squares columns for STDDEV/VAR maintenance.
        let sum2_aux = crate::dvm::query_sum2_aux_columns(&st.defining_query);
        if !sum2_aux.is_empty() {
            eq = inject_sum2_aux(&eq, &sum2_aux);
        }
        // Also inject cross-product columns for CORR/COVAR/REGR maintenance (P3-2).
        let covar_aux = crate::dvm::query_covar_aux_columns(&st.defining_query);
        if !covar_aux.is_empty() {
            eq = inject_covar_aux(&eq, &covar_aux);
        }
        // Also inject nonnull-count columns for SUM NULL-transition correction (P2-2).
        let nonnull_aux = crate::dvm::query_nonnull_aux_columns(&st.defining_query);
        if !nonnull_aux.is_empty() {
            eq = inject_nonnull_aux(&eq, &nonnull_aux);
        }
        eq
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
        Spi::run(&format!("ALTER TABLE {quoted_table} ENABLE TRIGGER USER")) // nosemgrep: rust.spi.run.dynamic-format — ALTER TABLE DDL cannot be parameterized; quoted_table is a PostgreSQL-quoted identifier.
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;

        // PB2: Skip NOTIFY when pooler compatibility mode is enabled.
        if !st.pooler_compatibility_mode {
            let escaped_name = table_name.replace('\'', "''");
            let escaped_schema = schema.replace('\'', "''");
            // NOTIFY does not support parameterized payloads; single quotes are escaped above.
            let notify_sql = format!(
                "NOTIFY pgtrickle_refresh, '{{\"stream_table\": \"{escaped_name}\", \
                 \"schema\": \"{escaped_schema}\", \"mode\": \"FULL\"}}'"
            );
            Spi::run(&notify_sql).map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        }

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
    let mut frontier = version::compute_initial_frontier(&slot_positions, &data_ts);

    // Include ST (stream table) sources in the frontier so that the
    // scheduler's `prev_frontier.is_empty()` check doesn't trigger a
    // spurious FULL fallback on the first differential refresh.
    //
    // Uses the pre-INSERT LSN snapshot captured above to avoid the TOCTOU
    // race where concurrent upstream refreshes commit new change buffer
    // rows between the INSERT and this point, advancing the frontier past
    // data the INSERT never saw.
    for (upstream_pgt_id, lsn) in &st_source_lsn_snapshot {
        frontier.set_st_source(*upstream_pgt_id, lsn.clone(), data_ts.clone());
    }

    // Detect no-op FULL refresh: check if any upstream ST source has a
    // data_timestamp newer than ours.  If not, the TRUNCATE+INSERT above
    // reproduced identical data, so we must NOT bump data_timestamp —
    // otherwise downstream CALCULATED stream tables see a false "upstream
    // changed" signal and their own data_timestamp drifts on every no-op
    // cycle.
    //
    // This uses catalog data_timestamps (stable, not affected by change
    // buffer cleanup) rather than frontier LSN comparisons (which become
    // unreliable when buffer rows are consumed and MAX(lsn) falls back to
    // pg_current_wal_lsn()).
    let has_upstream_st_change = Spi::get_one::<bool>(&format!(
        "SELECT EXISTS( \
           SELECT 1 \
           FROM pgtrickle.pgt_dependencies dep \
           JOIN pgtrickle.pgt_stream_tables upstream \
                ON upstream.pgt_relid = dep.source_relid \
           WHERE dep.pgt_id = {pgt_id} \
             AND dep.source_type = 'STREAM_TABLE' \
             AND upstream.data_timestamp > COALESCE( \
                   (SELECT data_timestamp \
                    FROM pgtrickle.pgt_stream_tables \
                    WHERE pgt_id = {pgt_id}), \
                   '-infinity'::timestamptz) \
         )",
        pgt_id = st.pgt_id,
    ))
    .unwrap_or(Some(false))
    .unwrap_or(false);

    // Also check WAL-based sources: if any slot position advanced
    // beyond the previous frontier, data changed.
    let prev_frontier = st.frontier.clone().unwrap_or_default();
    let has_wal_change = slot_positions
        .iter()
        .any(|(oid, lsn)| prev_frontier.get_lsn(*oid) != *lsn);

    if has_upstream_st_change || has_wal_change || prev_frontier.is_empty() {
        StreamTableMeta::store_frontier_and_complete_refresh(st.pgt_id, &frontier, 0)?;
        pgrx::info!("Stream table {}.{} refreshed (FULL)", schema, table_name);
    } else {
        // No upstream changes — store frontier but preserve data_timestamp.
        StreamTableMeta::store_frontier(st.pgt_id, &frontier)?;
        StreamTableMeta::update_after_no_data_refresh(st.pgt_id)?;
        pgrx::info!(
            "Stream table {}.{} refreshed (FULL, no-op — data_timestamp preserved)",
            schema,
            table_name
        );
    }

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
    if st.frontier.is_none() {
        pgrx::info!(
            "Stream table {}.{}: no previous frontier, performing FULL refresh first",
            schema,
            table_name
        );
        return execute_manual_full_refresh(st, schema, table_name, source_oids);
    }

    let prev_frontier = st.frontier.clone().unwrap_or_default();

    // If the frontier exists but tracks zero sources, the ST was populated
    // via FULL but never differentially refreshed. Fall back to FULL to
    // establish proper source tracking.
    if prev_frontier.is_empty() {
        return execute_manual_full_refresh(st, schema, table_name, source_oids);
    }

    refresh::poll_foreign_table_sources_for_st(st)?;

    // ST-source guard: if ANY upstream dependency is a STREAM_TABLE, always
    // fall back to a FULL refresh.  The manual FULL refresh path
    // (`execute_manual_full_refresh`) does not populate ST change buffers
    // (`changes_pgt_`), so a downstream DIFFERENTIAL refresh would see an
    // empty change buffer and silently skip real changes.
    // The background scheduler handles this correctly (via
    // `capture_full_refresh_diff_to_st_buffer`), but the manual path
    // does not, so we must force FULL here.
    {
        let deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();
        let has_st_source = deps.iter().any(|dep| dep.source_type == "STREAM_TABLE");
        if has_st_source {
            return execute_manual_full_refresh(st, schema, table_name, source_oids);
        }
    }

    // Get current WAL positions for non-ST sources (reuses source_oids — G-N3)
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
// ── Sub-modules ─────────────────────────────────────────────────────────────
mod diagnostics;
mod helpers;

// Re-export public items from sub-modules so external callers are unaffected.
pub use helpers::*;

/// Resolve a (possibly schema-qualified) table name to its OID.
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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

    // ── compute_config_diff tests ──────────────────────────────────────

    fn make_test_st() -> StreamTableMeta {
        StreamTableMeta {
            pgt_id: 1,
            pgt_relid: pg_sys::Oid::from(12345u32),
            pgt_name: "test_st".to_string(),
            pgt_schema: "public".to_string(),
            defining_query: "SELECT 1".to_string(),
            original_query: None,
            schedule: Some("1m".to_string()),
            refresh_mode: RefreshMode::Differential,
            status: StStatus::Active,
            is_populated: true,
            data_timestamp: None,
            consecutive_errors: 0,
            needs_reinit: false,
            auto_threshold: None,
            last_full_ms: None,
            functions_used: None,
            frontier: None,
            topk_limit: None,
            topk_order_by: None,
            topk_offset: None,
            diamond_consistency: DiamondConsistency::Atomic,
            diamond_schedule_policy: DiamondSchedulePolicy::Fastest,
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

    // ── normalize_sql_whitespace tests ─────────────────────────────────

    #[test]
    fn test_normalize_whitespace_collapses_spaces() {
        assert_eq!(
            normalize_sql_whitespace("SELECT  id,  val  FROM  t"),
            "SELECT id, val FROM t"
        );
    }

    #[test]
    fn test_normalize_whitespace_tabs_and_newlines() {
        assert_eq!(
            normalize_sql_whitespace("SELECT\tid\n\tFROM\tt"),
            "SELECT id FROM t"
        );
    }

    #[test]
    fn test_normalize_whitespace_trims() {
        assert_eq!(
            normalize_sql_whitespace("  SELECT id FROM t  "),
            "SELECT id FROM t"
        );
    }

    #[test]
    fn test_normalize_whitespace_identical() {
        assert_eq!(
            normalize_sql_whitespace("SELECT id FROM t"),
            normalize_sql_whitespace("SELECT id FROM t")
        );
    }

    #[test]
    fn test_config_diff_all_identical() {
        let st = make_test_st();
        let diff = compute_config_diff(
            &st,
            Some("1m"),
            "DIFFERENTIAL",
            None,
            None,
            None,
            false,
            false,
        );
        assert!(diff.is_empty());
    }

    #[test]
    fn test_config_diff_schedule_changed() {
        let st = make_test_st();
        let diff = compute_config_diff(
            &st,
            Some("5m"),
            "DIFFERENTIAL",
            None,
            None,
            None,
            false,
            false,
        );
        assert!(!diff.is_empty());
        assert_eq!(diff.schedule, Some("5m"));
        assert!(diff.refresh_mode.is_none());
    }

    #[test]
    fn test_config_diff_schedule_to_calculated() {
        let st = make_test_st();
        let diff = compute_config_diff(
            &st,
            Some("calculated"),
            "DIFFERENTIAL",
            None,
            None,
            None,
            false,
            false,
        );
        assert!(!diff.is_empty());
        assert_eq!(diff.schedule, Some("calculated"));
    }

    #[test]
    fn test_config_diff_calculated_already_calculated() {
        let mut st = make_test_st();
        st.schedule = None; // NULL in catalog means CALCULATED
        let diff = compute_config_diff(
            &st,
            Some("calculated"),
            "DIFFERENTIAL",
            None,
            None,
            None,
            false,
            false,
        );
        assert!(diff.is_empty());
    }

    #[test]
    fn test_config_diff_mode_changed() {
        let st = make_test_st();
        let diff = compute_config_diff(&st, Some("1m"), "FULL", None, None, None, false, false);
        assert!(!diff.is_empty());
        assert_eq!(diff.refresh_mode, Some("FULL"));
    }

    #[test]
    fn test_config_diff_auto_vs_differential() {
        // AUTO resolves to DIFFERENTIAL — should be same as existing DIFFERENTIAL
        let st = make_test_st();
        let diff = compute_config_diff(&st, Some("1m"), "AUTO", None, None, None, false, false);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_config_diff_diamond_consistency_changed() {
        let st = make_test_st(); // existing: Atomic
        let diff = compute_config_diff(
            &st,
            Some("1m"),
            "DIFFERENTIAL",
            Some("none"),
            None,
            None,
            false,
            false,
        );
        assert!(!diff.is_empty());
        assert_eq!(diff.diamond_consistency, Some("none"));
    }

    #[test]
    fn test_config_diff_append_only_changed() {
        let st = make_test_st(); // existing: false
        let diff = compute_config_diff(
            &st,
            Some("1m"),
            "DIFFERENTIAL",
            None,
            None,
            None,
            true,
            false,
        );
        assert!(!diff.is_empty());
        assert_eq!(diff.append_only, Some(true));
    }

    #[test]
    fn test_config_diff_cdc_mode_changed() {
        let st = make_test_st(); // existing: None
        let diff = compute_config_diff(
            &st,
            Some("1m"),
            "DIFFERENTIAL",
            None,
            None,
            Some("wal"),
            false,
            false,
        );
        assert!(!diff.is_empty());
        assert_eq!(diff.cdc_mode, Some("wal"));
    }

    #[test]
    fn test_config_diff_cdc_mode_both_none() {
        let st = make_test_st(); // existing: None
        let diff = compute_config_diff(
            &st,
            Some("1m"),
            "DIFFERENTIAL",
            None,
            None,
            None,
            false,
            false,
        );
        assert!(diff.is_empty());
    }

    // ── P2 property / fuzz tests ──────────────────────────────────────────

    proptest! {
        #[test]
        fn prop_detect_select_star_no_panic(input in ".*") {
            let _ = detect_select_star(&input);
        }

        #[test]
        fn prop_detect_select_star_false_without_star(input in "[^*]*") {
            prop_assert!(!detect_select_star(&input));
        }

        #[test]
        fn prop_split_top_level_commas_no_panic(input in ".*") {
            let _ = split_top_level_commas(&input);
        }

        #[test]
        fn prop_split_top_level_commas_nonempty_for_nonempty_input(input in ".+") {
            let parts = split_top_level_commas(&input);
            // Whitespace-only inputs trim to empty and produce no columns — that is correct
            // behaviour. Only assert non-empty output when the input contains non-whitespace.
            if input.chars().any(|c| c != ',' && !c.is_whitespace()) {
                prop_assert!(!parts.is_empty());
            }
        }

        #[test]
        fn prop_find_top_level_keyword_no_panic(
            sql in ".*",
            kw in "[A-Za-z]{1,15}"
        ) {
            let _ = find_top_level_keyword(&sql, &kw);
        }

        #[test]
        fn prop_find_top_level_keyword_pos_in_bounds(
            sql in ".*",
            kw in "[A-Za-z]{1,15}"
        ) {
            if let Some(pos) = find_top_level_keyword(&sql, &kw) {
                prop_assert!(pos < sql.len());
            }
        }

        #[test]
        fn prop_cron_is_due_no_panic(
            cron_expr in ".*",
            epoch in proptest::option::of(proptest::num::i64::ANY)
        ) {
            let _ = cron_is_due(&cron_expr, epoch);
        }
    }

    // ── A1-1b: parse_partition_key_columns tests ────────────────────────────

    #[test]
    fn test_parse_partition_key_single_column() {
        let cols = parse_partition_key_columns("event_day");
        assert_eq!(cols, vec!["event_day"]);
    }

    #[test]
    fn test_parse_partition_key_two_columns() {
        let cols = parse_partition_key_columns("event_day, customer_id");
        assert_eq!(cols, vec!["event_day", "customer_id"]);
    }

    #[test]
    fn test_parse_partition_key_whitespace_handling() {
        let cols = parse_partition_key_columns("  a , b , c  ");
        assert_eq!(cols, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_partition_key_empty_string() {
        let cols = parse_partition_key_columns("");
        assert!(cols.is_empty());
    }

    #[test]
    fn test_parse_partition_key_trailing_comma() {
        let cols = parse_partition_key_columns("a,b,");
        assert_eq!(cols, vec!["a", "b"]);
    }

    #[test]
    fn test_validate_partition_key_multi_column_valid() {
        let columns = vec![
            ColumnDef {
                name: "region".to_string(),
                type_oid: PgOid::Invalid,
            },
            ColumnDef {
                name: "sale_date".to_string(),
                type_oid: PgOid::Invalid,
            },
            ColumnDef {
                name: "amount".to_string(),
                type_oid: PgOid::Invalid,
            },
        ];
        assert!(validate_partition_key("sale_date,region", &columns).is_ok());
    }

    #[test]
    fn test_validate_partition_key_multi_column_one_missing() {
        let columns = vec![
            ColumnDef {
                name: "region".to_string(),
                type_oid: PgOid::Invalid,
            },
            ColumnDef {
                name: "sale_date".to_string(),
                type_oid: PgOid::Invalid,
            },
        ];
        let err = validate_partition_key("sale_date,nonexistent", &columns);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("nonexistent"),
            "Error should mention the missing column: {msg}"
        );
    }

    // ── A1-1d: LIST partitioning tests ──────────────────────────────────────

    #[test]
    fn test_parse_partition_method_range_default() {
        assert_eq!(parse_partition_method("sale_date"), PartitionMethod::Range);
    }

    #[test]
    fn test_parse_partition_method_range_multi() {
        assert_eq!(parse_partition_method("a,b"), PartitionMethod::Range,);
    }

    #[test]
    fn test_parse_partition_method_list_upper() {
        assert_eq!(parse_partition_method("LIST:region"), PartitionMethod::List,);
    }

    #[test]
    fn test_parse_partition_method_list_lower() {
        assert_eq!(parse_partition_method("list:region"), PartitionMethod::List,);
    }

    #[test]
    fn test_parse_partition_method_list_mixed_case() {
        assert_eq!(parse_partition_method("List:region"), PartitionMethod::List,);
    }

    #[test]
    fn test_strip_partition_mode_prefix_none() {
        assert_eq!(strip_partition_mode_prefix("sale_date"), "sale_date");
    }

    #[test]
    fn test_strip_partition_mode_prefix_list() {
        assert_eq!(strip_partition_mode_prefix("LIST:region"), "region");
    }

    #[test]
    fn test_strip_partition_mode_prefix_list_lower() {
        assert_eq!(strip_partition_mode_prefix("list:region"), "region");
    }

    #[test]
    fn test_strip_partition_mode_prefix_mixed_case() {
        assert_eq!(strip_partition_mode_prefix("LiSt:region"), "region");
    }

    #[test]
    fn test_parse_partition_key_columns_with_list_prefix() {
        let cols = parse_partition_key_columns("LIST:region");
        assert_eq!(cols, vec!["region"]);
    }

    #[test]
    fn test_validate_partition_key_list_single_column_ok() {
        let columns = vec![
            ColumnDef {
                name: "region".to_string(),
                type_oid: PgOid::Invalid,
            },
            ColumnDef {
                name: "amount".to_string(),
                type_oid: PgOid::Invalid,
            },
        ];
        assert!(validate_partition_key("LIST:region", &columns).is_ok());
    }

    #[test]
    fn test_validate_partition_key_list_multi_column_rejected() {
        let columns = vec![
            ColumnDef {
                name: "region".to_string(),
                type_oid: PgOid::Invalid,
            },
            ColumnDef {
                name: "category".to_string(),
                type_oid: PgOid::Invalid,
            },
        ];
        let err = validate_partition_key("LIST:region,category", &columns);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("single column"),
            "Error should mention single column: {msg}"
        );
    }

    #[test]
    fn test_validate_partition_key_list_missing_column() {
        let columns = vec![ColumnDef {
            name: "amount".to_string(),
            type_oid: PgOid::Invalid,
        }];
        let err = validate_partition_key("LIST:region", &columns);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("region"),
            "Error should mention the missing column: {msg}"
        );
    }

    // ── A1-3b: HASH partitioning tests ──────────────────────────────────────

    #[test]
    fn test_parse_partition_method_hash_upper() {
        assert_eq!(
            parse_partition_method("HASH:customer_id"),
            PartitionMethod::Hash,
        );
    }

    #[test]
    fn test_parse_partition_method_hash_lower() {
        assert_eq!(
            parse_partition_method("hash:customer_id"),
            PartitionMethod::Hash,
        );
    }

    #[test]
    fn test_parse_partition_method_hash_mixed_case() {
        assert_eq!(
            parse_partition_method("Hash:customer_id"),
            PartitionMethod::Hash,
        );
    }

    #[test]
    fn test_parse_partition_method_hash_with_modulus() {
        assert_eq!(parse_partition_method("HASH:id:8"), PartitionMethod::Hash,);
    }

    #[test]
    fn test_parse_hash_modulus_default() {
        assert_eq!(parse_hash_modulus("HASH:id"), Some(4));
    }

    #[test]
    fn test_parse_hash_modulus_explicit() {
        assert_eq!(parse_hash_modulus("HASH:id:8"), Some(8));
    }

    #[test]
    fn test_parse_hash_modulus_min_value() {
        assert_eq!(parse_hash_modulus("HASH:id:2"), Some(2));
    }

    #[test]
    fn test_parse_hash_modulus_large() {
        assert_eq!(parse_hash_modulus("HASH:id:256"), Some(256));
    }

    #[test]
    fn test_parse_hash_modulus_non_hash_returns_none() {
        assert_eq!(parse_hash_modulus("sale_date"), None);
    }

    #[test]
    fn test_parse_hash_modulus_list_returns_none() {
        assert_eq!(parse_hash_modulus("LIST:region"), None);
    }

    #[test]
    fn test_strip_partition_mode_prefix_hash() {
        assert_eq!(
            strip_partition_mode_prefix("HASH:customer_id"),
            "customer_id"
        );
    }

    #[test]
    fn test_strip_partition_mode_prefix_hash_lower() {
        assert_eq!(
            strip_partition_mode_prefix("hash:customer_id"),
            "customer_id"
        );
    }

    #[test]
    fn test_strip_partition_mode_prefix_hash_with_modulus() {
        assert_eq!(strip_partition_mode_prefix("HASH:id:8"), "id");
    }

    #[test]
    fn test_strip_partition_mode_prefix_hash_with_modulus_256() {
        assert_eq!(
            strip_partition_mode_prefix("HASH:customer_id:256"),
            "customer_id"
        );
    }

    #[test]
    fn test_parse_partition_key_columns_with_hash_prefix() {
        let cols = parse_partition_key_columns("HASH:customer_id");
        assert_eq!(cols, vec!["customer_id"]);
    }

    #[test]
    fn test_parse_partition_key_columns_with_hash_modulus() {
        let cols = parse_partition_key_columns("HASH:customer_id:8");
        assert_eq!(cols, vec!["customer_id"]);
    }

    #[test]
    fn test_validate_partition_key_hash_single_column_ok() {
        let columns = vec![
            ColumnDef {
                name: "customer_id".to_string(),
                type_oid: PgOid::Invalid,
            },
            ColumnDef {
                name: "amount".to_string(),
                type_oid: PgOid::Invalid,
            },
        ];
        assert!(validate_partition_key("HASH:customer_id", &columns).is_ok());
    }

    #[test]
    fn test_validate_partition_key_hash_with_modulus_ok() {
        let columns = vec![ColumnDef {
            name: "id".to_string(),
            type_oid: PgOid::Invalid,
        }];
        assert!(validate_partition_key("HASH:id:8", &columns).is_ok());
    }

    #[test]
    fn test_validate_partition_key_hash_multi_column_rejected() {
        let columns = vec![
            ColumnDef {
                name: "a".to_string(),
                type_oid: PgOid::Invalid,
            },
            ColumnDef {
                name: "b".to_string(),
                type_oid: PgOid::Invalid,
            },
        ];
        let err = validate_partition_key("HASH:a,b", &columns);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("single column"),
            "Error should mention single column: {msg}"
        );
    }

    #[test]
    fn test_validate_partition_key_hash_missing_column() {
        let columns = vec![ColumnDef {
            name: "amount".to_string(),
            type_oid: PgOid::Invalid,
        }];
        let err = validate_partition_key("HASH:customer_id", &columns);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("customer_id"),
            "Error should mention the missing column: {msg}"
        );
    }

    #[test]
    fn test_validate_partition_key_hash_modulus_too_low() {
        let columns = vec![ColumnDef {
            name: "id".to_string(),
            type_oid: PgOid::Invalid,
        }];
        let err = validate_partition_key("HASH:id:1", &columns);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("modulus"),
            "Error should mention modulus: {msg}"
        );
    }

    #[test]
    fn test_validate_partition_key_hash_modulus_too_high() {
        let columns = vec![ColumnDef {
            name: "id".to_string(),
            type_oid: PgOid::Invalid,
        }];
        let err = validate_partition_key("HASH:id:257", &columns);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("modulus"),
            "Error should mention modulus: {msg}"
        );
    }

    // ── G15-BC: bulk_create_impl JSONB validation tests ─────────────

    #[test]
    fn test_bulk_create_impl_rejects_non_array() {
        let input = serde_json::json!({"name": "t1", "query": "SELECT 1"});
        let err = bulk_create_impl(input).unwrap_err();
        assert!(err.to_string().contains("JSONB array"));
    }

    #[test]
    fn test_bulk_create_impl_rejects_empty_array() {
        let input = serde_json::json!([]);
        let err = bulk_create_impl(input).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn test_bulk_create_impl_rejects_non_object_element() {
        let input = serde_json::json!(["not an object"]);
        let err = bulk_create_impl(input).unwrap_err();
        assert!(err.to_string().contains("not a JSON object"));
    }

    #[test]
    fn test_bulk_create_impl_rejects_missing_name() {
        let input = serde_json::json!([{"query": "SELECT 1"}]);
        let err = bulk_create_impl(input).unwrap_err();
        assert!(err.to_string().contains("missing required \"name\""));
    }

    #[test]
    fn test_bulk_create_impl_rejects_missing_query() {
        let input = serde_json::json!([{"name": "t1"}]);
        let err = bulk_create_impl(input).unwrap_err();
        assert!(err.to_string().contains("missing required \"query\""));
    }
}
