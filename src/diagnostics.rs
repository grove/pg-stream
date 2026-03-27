//! Phase 2 (DT-1 – DT-4): SQL-callable diagnostic / introspection functions.
//!
//! All four functions are `#[pg_extern(schema = "pgtrickle")]` with no
//! catalog side-effects — they are pure read-only introspection helpers.
//!
//! ## Functions
//! - [`explain_query_rewrite`] (DT-1): walk a query through every DVM rewrite
//!   pass and report which passes fired, plus detected DVM patterns.
//! - [`diagnose_errors`] (DT-2): return the last 5 FAILED refresh events for
//!   a stream table, classified by error type with remediation hints.
//! - [`list_auxiliary_columns`] (DT-3): list all `__pgt_*` internal columns
//!   on a stream table's storage relation and explain their purpose.
//! - [`validate_query`] (DT-4): parse and validate a query through the DVM
//!   pipeline without creating a stream table; return detected constructs,
//!   warnings, and the resolved refresh mode.

use pgrx::prelude::*;

use crate::catalog::StreamTableMeta;
use crate::dvm;
use crate::dvm::parser::{AggExpr, OpTree};
use crate::error::PgTrickleError;

// ── DT-1: explain_query_rewrite ───────────────────────────────────────────

/// DT-1 — Walk a query through the full DVM rewrite pipeline and report
/// each pass.
///
/// Returns one row per rewrite pass. When a pass changes the query,
/// `changed = true` and `sql_after` contains the SQL text after the
/// transformation. When a pass is a no-op, `changed = false` and
/// `sql_after` is `NULL`.
///
/// Two synthetic rows are appended after all rewrite passes:
/// - `pass_name = 'topk_detection'` — `changed = true` when the query
///   matches the `ORDER BY … LIMIT n` TopK pattern; `sql_after` describes
///   the detected limit and ORDER BY clause.
/// - `pass_name = 'dvm_patterns'` — `changed = false`; `sql_after` is a
///   semicolon-separated list of DVM constructs detected in the parse tree
///   (e.g. `group_rescan_aggregates`, `full_join`, `recursive_cte`).
///
/// # SQL usage
/// ```sql
/// SELECT * FROM pgtrickle.explain_query_rewrite(
///   'SELECT customer_id, SUM(amount) FROM orders GROUP BY customer_id'
/// );
/// ```
#[allow(clippy::type_complexity)]
#[pg_extern(schema = "pgtrickle")]
pub fn explain_query_rewrite(
    query: &str,
) -> TableIterator<
    'static,
    (
        name!(pass_name, String),
        name!(changed, bool),
        name!(sql_after, Option<String>),
    ),
> {
    let rows = match explain_query_rewrite_impl(query) {
        Ok(r) => r,
        Err(e) => pgrx::error!("explain_query_rewrite: {}", e),
    };
    TableIterator::new(rows)
}

fn explain_query_rewrite_impl(
    query: &str,
) -> Result<Vec<(String, bool, Option<String>)>, PgTrickleError> {
    let mut rows: Vec<(String, bool, Option<String>)> = Vec::new();

    // Macro: apply one rewrite pass, record whether it changed the query.
    macro_rules! apply_pass {
        ($name:expr, $current:expr, $fn:expr) => {{
            let before = $current.clone();
            let after = $fn(&before)?;
            let changed = after != before;
            rows.push((
                $name.to_string(),
                changed,
                if changed { Some(after.clone()) } else { None },
            ));
            after
        }};
    }

    let q = apply_pass!(
        "view_inlining",
        query.to_string(),
        dvm::rewrite_views_inline
    );
    let q = apply_pass!("nested_window_lift", q, dvm::rewrite_nested_window_exprs);
    let q = apply_pass!("distinct_on", q, dvm::rewrite_distinct_on);
    let q = apply_pass!("grouping_sets", q, dvm::rewrite_grouping_sets);
    let q = apply_pass!(
        "scalar_subquery_in_where",
        q,
        dvm::rewrite_scalar_subquery_in_where
    );
    let q = apply_pass!(
        "correlated_scalar_in_select",
        q,
        dvm::rewrite_correlated_scalar_in_select
    );

    // SubLinks-in-OR + De Morgan normalization: up to 3 passes.
    let mut q = q;
    let mut sublinks_changed = false;
    for _ in 0..3 {
        let prev = q.clone();
        let q2 = dvm::rewrite_demorgan_sublinks(&q)?;
        let q2 = dvm::rewrite_sublinks_in_or(&q2)?;
        if q2 == prev {
            break;
        }
        q = q2;
        sublinks_changed = true;
    }
    rows.push((
        "sublinks_in_or_demorgan".to_string(),
        sublinks_changed,
        None,
    ));

    let q = apply_pass!("rows_from", q, dvm::rewrite_rows_from);

    // TopK detection.
    let topk = dvm::detect_topk_pattern(&q)?;
    let (effective_q, has_topk) = if let Some(ref info) = topk {
        rows.push((
            "topk_detection".to_string(),
            true,
            Some(format!(
                "TopK detected: LIMIT {}, ORDER BY: {}",
                info.limit_value, info.order_by_sql
            )),
        ));
        (info.base_query.clone(), true)
    } else {
        rows.push(("topk_detection".to_string(), false, None));
        (q.clone(), false)
    };

    // DVM parse — detect constructs present in the operator tree.
    if !has_topk {
        match dvm::parse_defining_query_full(&effective_q) {
            Ok(result) => {
                let mut patterns: Vec<String> = Vec::new();

                // IVM support verdict.
                match dvm::check_ivm_support_with_registry(&result) {
                    Ok(_) => patterns.push("ivm_support:DIFFERENTIAL".to_string()),
                    Err(e) => patterns.push(format!("ivm_support:FULL_ONLY({})", e)),
                }

                // Collect tree-level constructs.
                let constructs = collect_tree_constructs(&result.tree, &result.cte_registry);
                patterns.extend(constructs);

                // Recursion.
                if result.has_recursion {
                    patterns.push("recursive_cte:true".to_string());
                }

                // Auxiliary column needs.
                if dvm::query_needs_pgt_count(&effective_q) {
                    patterns.push("needs_pgt_count:true".to_string());
                }
                if dvm::query_needs_dual_count(&effective_q) {
                    patterns.push("needs_dual_count:true".to_string());
                }
                if dvm::query_needs_union_dedup_count(&effective_q) {
                    patterns.push("needs_union_dedup_count:true".to_string());
                }

                // Parse warnings.
                for w in &result.warnings {
                    patterns.push(format!("parse_warning:{}", w));
                }

                // Volatility: 'i' = immutable, 's' = stable, 'v' = volatile.
                if let Ok(v) = dvm::tree_worst_volatility_with_registry(&result) {
                    let label = match v {
                        'i' => "immutable",
                        's' => "stable",
                        _ => "volatile",
                    };
                    patterns.push(format!("volatility:{}", label));
                }

                let patterns_str = if patterns.is_empty() {
                    None
                } else {
                    Some(patterns.join("; "))
                };
                rows.push(("dvm_patterns".to_string(), false, patterns_str));
            }
            Err(e) => {
                rows.push((
                    "dvm_patterns".to_string(),
                    false,
                    Some(format!("dvm_parse_error: {}", e)),
                ));
            }
        }
    } else {
        rows.push((
            "dvm_patterns".to_string(),
            false,
            Some("ivm_support:TOPK".to_string()),
        ));
    }

    Ok(rows)
}

/// Recursively walk an `OpTree` and return a list of construct descriptors.
///
/// Returns strings of the form `"construct_name:detail"` for noteworthy
/// patterns (group-rescan aggregates, FULL JOINs, DISTINCT, EXCEPT/INTERSECT,
/// lateral subqueries, window functions, etc.).
fn collect_tree_constructs(tree: &OpTree, cte_registry: &dvm::CteRegistry) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_tree_constructs_inner(tree, cte_registry, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_tree_constructs_inner(
    tree: &OpTree,
    cte_registry: &dvm::CteRegistry,
    out: &mut Vec<String>,
) {
    match tree {
        OpTree::Aggregate {
            aggregates, child, ..
        } => {
            for agg in aggregates {
                let strategy = dvm::classify_agg_strategy(agg);
                let label = agg_label(agg);
                out.push(format!("aggregate:{}({})", label, strategy));
            }
            collect_tree_constructs_inner(child, cte_registry, out);
        }
        OpTree::FullJoin { left, right, .. } => {
            out.push("join:FULL_OUTER".to_string());
            collect_tree_constructs_inner(left, cte_registry, out);
            collect_tree_constructs_inner(right, cte_registry, out);
        }
        OpTree::InnerJoin { left, right, .. } => {
            out.push("join:INNER".to_string());
            collect_tree_constructs_inner(left, cte_registry, out);
            collect_tree_constructs_inner(right, cte_registry, out);
        }
        OpTree::LeftJoin { left, right, .. } => {
            out.push("join:LEFT_OUTER".to_string());
            collect_tree_constructs_inner(left, cte_registry, out);
            collect_tree_constructs_inner(right, cte_registry, out);
        }
        OpTree::SemiJoin { left, right, .. } => {
            out.push("join:SEMI".to_string());
            collect_tree_constructs_inner(left, cte_registry, out);
            collect_tree_constructs_inner(right, cte_registry, out);
        }
        OpTree::AntiJoin { left, right, .. } => {
            out.push("join:ANTI".to_string());
            collect_tree_constructs_inner(left, cte_registry, out);
            collect_tree_constructs_inner(right, cte_registry, out);
        }
        OpTree::Distinct { child } => {
            out.push("set_op:DISTINCT".to_string());
            collect_tree_constructs_inner(child, cte_registry, out);
        }
        OpTree::UnionAll { children } => {
            out.push("set_op:UNION_ALL".to_string());
            for c in children {
                collect_tree_constructs_inner(c, cte_registry, out);
            }
        }
        OpTree::Intersect {
            left, right, all, ..
        } => {
            let label = if *all { "INTERSECT_ALL" } else { "INTERSECT" };
            out.push(format!("set_op:{}", label));
            collect_tree_constructs_inner(left, cte_registry, out);
            collect_tree_constructs_inner(right, cte_registry, out);
        }
        OpTree::Except {
            left, right, all, ..
        } => {
            let label = if *all { "EXCEPT_ALL" } else { "EXCEPT" };
            out.push(format!("set_op:{}", label));
            collect_tree_constructs_inner(left, cte_registry, out);
            collect_tree_constructs_inner(right, cte_registry, out);
        }
        OpTree::Window { child, .. } => {
            out.push("window_function:true".to_string());
            collect_tree_constructs_inner(child, cte_registry, out);
        }
        OpTree::LateralFunction { child, .. } => {
            out.push("lateral:FUNCTION".to_string());
            collect_tree_constructs_inner(child, cte_registry, out);
        }
        OpTree::LateralSubquery { child, .. } => {
            out.push("lateral:SUBQUERY".to_string());
            collect_tree_constructs_inner(child, cte_registry, out);
        }
        OpTree::ScalarSubquery { child, .. } => {
            out.push("scalar_subquery:true".to_string());
            collect_tree_constructs_inner(child, cte_registry, out);
        }
        OpTree::RecursiveCte {
            base, recursive, ..
        } => {
            out.push("recursive_cte:true".to_string());
            collect_tree_constructs_inner(base, cte_registry, out);
            collect_tree_constructs_inner(recursive, cte_registry, out);
        }
        OpTree::CteScan { cte_id, body, .. } => {
            if let Some(body) = body {
                collect_tree_constructs_inner(body, cte_registry, out);
            } else if let Some((_, body)) = cte_registry.get(*cte_id) {
                collect_tree_constructs_inner(body, cte_registry, out);
            }
        }
        OpTree::Project { child, .. }
        | OpTree::Filter { child, .. }
        | OpTree::Subquery { child, .. } => {
            collect_tree_constructs_inner(child, cte_registry, out);
        }
        OpTree::Scan { .. } | OpTree::RecursiveSelfRef { .. } => {}
    }
}

/// Return a display label for an aggregate function (name + DISTINCT marker).
fn agg_label(agg: &AggExpr) -> String {
    use crate::dvm::parser::AggFunc;
    let name = match &agg.function {
        AggFunc::Count => "COUNT",
        AggFunc::CountStar => "COUNT(*)",
        AggFunc::Sum => "SUM",
        AggFunc::Avg => "AVG",
        AggFunc::Min => "MIN",
        AggFunc::Max => "MAX",
        AggFunc::BoolAnd => "BOOL_AND",
        AggFunc::BoolOr => "BOOL_OR",
        AggFunc::StringAgg => "STRING_AGG",
        AggFunc::ArrayAgg => "ARRAY_AGG",
        AggFunc::JsonAgg => "JSON_AGG",
        AggFunc::JsonbAgg => "JSONB_AGG",
        AggFunc::BitAnd => "BIT_AND",
        AggFunc::BitOr => "BIT_OR",
        AggFunc::BitXor => "BIT_XOR",
        AggFunc::StddevPop | AggFunc::StddevSamp => "STDDEV",
        AggFunc::VarPop | AggFunc::VarSamp => "VAR",
        AggFunc::Corr => "CORR",
        AggFunc::CovarPop | AggFunc::CovarSamp => "COVAR",
        AggFunc::XmlAgg => "XMLAGG",
        AggFunc::Mode => "MODE",
        AggFunc::PercentileCont => "PERCENTILE_CONT",
        AggFunc::PercentileDisc => "PERCENTILE_DISC",
        AggFunc::AnyValue => "ANY_VALUE",
        AggFunc::UserDefined(name) => name.as_str(),
        _ => "AGG",
    };
    if agg.is_distinct {
        format!("{} DISTINCT", name)
    } else {
        name.to_string()
    }
}

// ── DT-2: diagnose_errors ─────────────────────────────────────────────────

/// DT-2 — Return the last 5 FAILED refresh events for a stream table,
/// with each error classified by type and supplied with a remediation hint.
///
/// `name` accepts the same format as `create_stream_table`: either a
/// schema-qualified name (`'myschema.my_st'`) or an unqualified name
/// (`'my_st'`, resolved in `public`).
///
/// # Columns
/// - `event_time` — when the refresh started
/// - `error_type` — one of: `user`, `schema`, `correctness`,
///   `performance`, `infrastructure`
/// - `error_message` — the raw error text from `pgt_refresh_history`
/// - `remediation` — a suggested next step
///
/// # SQL usage
/// ```sql
/// SELECT * FROM pgtrickle.diagnose_errors('my_stream_table');
/// ```
#[allow(clippy::type_complexity)]
#[pg_extern(schema = "pgtrickle")]
pub fn diagnose_errors(
    name: &str,
) -> TableIterator<
    'static,
    (
        name!(event_time, Option<TimestampWithTimeZone>),
        name!(error_type, String),
        name!(error_message, String),
        name!(remediation, String),
    ),
> {
    let rows = match diagnose_errors_impl(name) {
        Ok(r) => r,
        Err(e) => pgrx::error!("diagnose_errors: {}", e),
    };
    TableIterator::new(rows)
}

#[allow(clippy::type_complexity)]
fn diagnose_errors_impl(
    name: &str,
) -> Result<Vec<(Option<TimestampWithTimeZone>, String, String, String)>, PgTrickleError> {
    let (schema, table_name) = crate::api::parse_qualified_name_pub(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT h.start_time, h.error_message \
                 FROM pgtrickle.pgt_refresh_history h \
                 WHERE h.pgt_id = $1 AND h.status = 'FAILED' \
                 ORDER BY h.start_time DESC \
                 LIMIT 5",
                None,
                &[st.pgt_id.into()],
            )
            .map_err(|e| {
                PgTrickleError::SpiError(format!("diagnose_errors history query failed: {e}"))
            })?;

        let mut out = Vec::new();
        for row in result {
            let event_time = row.get::<TimestampWithTimeZone>(1).unwrap_or(None);
            let error_msg = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let (error_type, remediation) = classify_error(&error_msg);
            out.push((event_time, error_type, error_msg, remediation));
        }
        Ok(out)
    })?;

    Ok(rows)
}

/// Classify a raw error message string into a category with a remediation hint.
///
/// Categories:
/// - `"user"` — bad query, unsupported construct, type mismatch, invalid arg
/// - `"schema"` — upstream schema changed or table dropped
/// - `"correctness"` — phantom rows, EXCEPT ALL overflow, row-count mismatch
/// - `"performance"` — lock timeouts, serialization failures, I/O spills
/// - `"infrastructure"` — permission errors, SPI failures, slot errors
fn classify_error(msg: &str) -> (String, String) {
    let m = msg.to_lowercase();

    if m.contains("unsupported operator")
        || m.contains("query parse error")
        || m.contains("type mismatch")
        || m.contains("invalid argument")
        || m.contains("syntax error")
        || m.contains("unsupported construct")
    {
        return (
            "user".to_string(),
            "Check that the defining query uses only supported constructs. \
             Run pgtrickle.validate_query(query) for a detailed analysis, \
             or set refresh_mode = 'AUTO' to fall back to FULL refresh."
                .to_string(),
        );
    }

    if m.contains("schema changed")
        || m.contains("upstream table dropped")
        || m.contains("upstream table schema")
    {
        return (
            "schema".to_string(),
            "An upstream source table was altered or dropped. \
             Call pgtrickle.alter_stream_table('<name>') with the updated query \
             to reinitialize the stream table. Use pgtrickle.pgt_dependencies \
             to review all affected stream tables."
                .to_string(),
        );
    }

    if m.contains("phantom")
        || m.contains("except all")
        || m.contains("row count mismatch")
        || m.contains("multiplicity")
        || m.contains("correctness")
    {
        return (
            "correctness".to_string(),
            "A differential refresh produced an incorrect result. \
             This may indicate an EC-01 edge case with wide join trees. \
             Set refresh_mode = 'FULL' as a safe workaround and report \
             the query pattern to the pg_trickle issue tracker."
                .to_string(),
        );
    }

    if m.contains("lock timeout")
        || m.contains("deadlock")
        || m.contains("serialization failure")
        || m.contains("could not serialize")
        || m.contains("spill")
        || m.contains("temp file")
        || m.contains("out of memory")
    {
        return (
            "performance".to_string(),
            "The refresh encountered resource contention or excessive resource use. \
             Consider increasing pg_trickle.lock_timeout, reducing the schedule \
             frequency, or enabling buffer_partitioning to reduce change buffer size. \
             For EXCEPT_ALL spills, switch to FULL refresh mode."
                .to_string(),
        );
    }

    if m.contains("permission denied")
        || m.contains("spi permission")
        || m.contains("replication slot")
        || m.contains("wal")
        || m.contains("spi error")
        || m.contains("connection")
    {
        return (
            "infrastructure".to_string(),
            "A system-level failure occurred. Check that the background worker role \
             has SELECT on all source tables and INSERT/UPDATE/DELETE on the stream \
             table. For replication slot errors, verify pg_trickle.slot_name \
             and that max_replication_slots is not exhausted."
                .to_string(),
        );
    }

    // Default: infrastructure
    (
        "infrastructure".to_string(),
        "An unexpected error occurred during refresh. Check the PostgreSQL server log \
         for additional context. If this recurs, consider filing a bug report with \
         pgtrickle.explain_query_rewrite(query) and pgtrickle.validate_query(query) output."
            .to_string(),
    )
}

// ── DT-3: list_auxiliary_columns ─────────────────────────────────────────

/// DT-3 — List all `__pgt_*` internal columns on a stream table's storage
/// relation, with an explanation of each column's role.
///
/// These hidden columns are normally invisible in `SELECT *` output via
/// the `__pgt_*` column-exclusion filter. This function surfaces them for
/// debugging and operator visibility.
///
/// # Columns
/// - `column_name` — the `__pgt_*` column name (e.g. `__pgt_row_id`)
/// - `data_type` — PostgreSQL type name (e.g. `bigint`, `text`)
/// - `purpose` — human-readable description of what the column tracks
///
/// # SQL usage
/// ```sql
/// SELECT * FROM pgtrickle.list_auxiliary_columns('my_stream_table');
/// ```
#[allow(clippy::type_complexity)]
#[pg_extern(schema = "pgtrickle")]
pub fn list_auxiliary_columns(
    name: &str,
) -> TableIterator<
    'static,
    (
        name!(column_name, String),
        name!(data_type, String),
        name!(purpose, String),
    ),
> {
    let rows = match list_auxiliary_columns_impl(name) {
        Ok(r) => r,
        Err(e) => pgrx::error!("list_auxiliary_columns: {}", e),
    };
    TableIterator::new(rows)
}

fn list_auxiliary_columns_impl(
    name: &str,
) -> Result<Vec<(String, String, String)>, PgTrickleError> {
    let (schema, table_name) = crate::api::parse_qualified_name_pub(name)?;
    let st = StreamTableMeta::get_by_name(&schema, &table_name)?;

    let relid_i64: i64 = st.pgt_relid.to_u32() as i64;

    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT a.attname::text, \
                        pg_catalog.format_type(a.atttypid, a.atttypmod) \
                 FROM pg_catalog.pg_attribute a \
                 WHERE a.attrelid = $1::oid \
                   AND a.attname LIKE '__pgt_%' \
                   AND a.attnum > 0 \
                   AND NOT a.attisdropped \
                 ORDER BY a.attnum",
                None,
                &[relid_i64.into()],
            )
            .map_err(|e| {
                PgTrickleError::SpiError(format!(
                    "list_auxiliary_columns attribute query failed: {e}"
                ))
            })?;

        let mut out = Vec::new();
        for row in result {
            let col_name = row.get::<String>(1).unwrap_or(None).unwrap_or_default();
            let data_type = row.get::<String>(2).unwrap_or(None).unwrap_or_default();
            let purpose = describe_aux_column(&col_name);
            out.push((col_name, data_type, purpose));
        }
        Ok(out)
    })?;

    Ok(rows)
}

/// Map a `__pgt_*` column name to a human-readable purpose string.
fn describe_aux_column(col: &str) -> String {
    if col == "__pgt_row_id" {
        return "Row identity tracking: a stable hash key uniquely identifying each \
                logical row. Used as the MERGE join key to apply delta changes \
                (INSERT / UPDATE / DELETE) to the storage table."
            .to_string();
    }
    if col == "__pgt_count" {
        return "Multiplicity counter: tracks how many times each logical row appears \
                in the result set. Required for queries with DISTINCT, aggregation, \
                UNION-dedup, or set-operations where a row can appear more than once."
            .to_string();
    }
    if col == "__pgt_count_l" {
        return "Left-side multiplicity counter: used together with __pgt_count_r for \
                INTERSECT / EXCEPT [ALL] to track left-branch occurrence counts \
                independently from right-branch counts."
            .to_string();
    }
    if col == "__pgt_count_r" {
        return "Right-side multiplicity counter: used together with __pgt_count_l for \
                INTERSECT / EXCEPT [ALL] to track right-branch occurrence counts \
                independently from left-branch counts."
            .to_string();
    }
    if col.starts_with("__pgt_aux_sum_") {
        let arg = col.trim_start_matches("__pgt_aux_sum_");
        return format!(
            "AVG auxiliary — running SUM of '{}': accumulates the sum component \
             so that algebraic AVG can be maintained as SUM / COUNT \
             without a full re-scan on each delta.",
            arg
        );
    }
    if col.starts_with("__pgt_aux_count_") {
        let arg = col.trim_start_matches("__pgt_aux_count_");
        return format!(
            "AVG auxiliary — running COUNT of non-NULL '{}': accumulates the \
             count component paired with __pgt_aux_sum_{} for algebraic AVG \
             maintenance.",
            arg, arg
        );
    }
    if col.starts_with("__pgt_aux_sum2_") {
        let arg = col.trim_start_matches("__pgt_aux_sum2_");
        return format!(
            "STDDEV/VAR auxiliary — running sum-of-squares of '{}': used with \
             the SUM and COUNT auxiliaries to maintain STDDEV_POP, STDDEV_SAMP, \
             VAR_POP, and VAR_SAMP algebraically.",
            arg
        );
    }
    if col.starts_with("__pgt_aux_sumx_") {
        let arg = col.trim_start_matches("__pgt_aux_sumx_");
        return format!(
            "CORR/COVAR/REGR auxiliary — running sum of x-argument '{}': \
             part of the five-column auxiliary set for algebraic regression \
             and correlation aggregate maintenance.",
            arg
        );
    }
    if col.starts_with("__pgt_aux_sumy_") {
        let arg = col.trim_start_matches("__pgt_aux_sumy_");
        return format!(
            "CORR/COVAR/REGR auxiliary — running sum of y-argument '{}': \
             part of the five-column auxiliary set for algebraic regression \
             and correlation aggregate maintenance.",
            arg
        );
    }
    if col.starts_with("__pgt_aux_sumxy_") {
        let arg = col.trim_start_matches("__pgt_aux_sumxy_");
        return format!(
            "CORR/COVAR/REGR auxiliary — running sum of x*y products for '{}': \
             part of the five-column auxiliary set for algebraic co-variance \
             and correlation maintenance.",
            arg
        );
    }
    if col.starts_with("__pgt_aux_sumx2_") {
        let arg = col.trim_start_matches("__pgt_aux_sumx2_");
        return format!(
            "CORR/COVAR/REGR auxiliary — running sum of x² for '{}': \
             part of the five-column auxiliary set used to compute regression \
             slope, intercept, and R².",
            arg
        );
    }
    if col.starts_with("__pgt_aux_sumy2_") {
        let arg = col.trim_start_matches("__pgt_aux_sumy2_");
        return format!(
            "CORR/COVAR/REGR auxiliary — running sum of y² for '{}': \
             part of the five-column auxiliary set used to compute regression \
             slope, intercept, and R².",
            arg
        );
    }
    if col.starts_with("__pgt_aux_nonnull_") {
        let arg = col.trim_start_matches("__pgt_aux_nonnull_");
        return format!(
            "SUM-above-FULL-JOIN auxiliary — running count of non-NULL '{}' values: \
             required when a SUM aggregate sits above a FULL OUTER JOIN child. \
             Prevents incorrect NULL propagation when one side of the join \
             produces no rows for a key.",
            arg
        );
    }
    // Unknown __pgt_* column — shouldn't happen in practice.
    format!(
        "Internal pg_trickle column '{}'. See the pg_trickle documentation \
         for the full list of auxiliary column roles.",
        col
    )
}

// ── DT-4: validate_query ──────────────────────────────────────────────────

/// DT-4 — Parse and validate a query through the DVM pipeline without
/// creating a stream table; return the resolved refresh mode, detected
/// SQL constructs, and any warnings.
///
/// # Columns
/// - `check_name` — name of the check or construct
/// - `result` — the resolved value or detected construct description
/// - `severity` — `'INFO'`, `'WARNING'`, or `'ERROR'`
///
/// The first row always has `check_name = 'resolved_refresh_mode'` with
/// the mode that would be assigned under `refresh_mode = 'AUTO'`
/// (`'DIFFERENTIAL'`, `'FULL'`, or `'TOPK'`).
///
/// # SQL usage
/// ```sql
/// SELECT * FROM pgtrickle.validate_query(
///   'SELECT customer_id, COUNT(*) FROM orders GROUP BY customer_id'
/// );
/// ```
#[allow(clippy::type_complexity)]
#[pg_extern(schema = "pgtrickle")]
pub fn validate_query(
    query: &str,
) -> TableIterator<
    'static,
    (
        name!(check_name, String),
        name!(result, String),
        name!(severity, String),
    ),
> {
    let rows = match validate_query_impl(query) {
        Ok(r) => r,
        Err(e) => pgrx::error!("validate_query: {}", e),
    };
    TableIterator::new(rows)
}

fn validate_query_impl(query: &str) -> Result<Vec<(String, String, String)>, PgTrickleError> {
    let mut rows: Vec<(String, String, String)> = Vec::new();

    // Run the full rewrite pipeline (mirrors run_query_rewrite_pipeline in api.rs).
    let q = match dvm::rewrite_views_inline(query) {
        Ok(q) => q,
        Err(e) => {
            rows.push((
                "rewrite_error".to_string(),
                format!("{}", e),
                "ERROR".to_string(),
            ));
            return Ok(rows);
        }
    };

    let q = apply_rewrite_safe(&q, dvm::rewrite_nested_window_exprs, &mut rows);
    let q = apply_rewrite_safe(&q, dvm::rewrite_distinct_on, &mut rows);
    let q = apply_rewrite_safe(&q, dvm::rewrite_grouping_sets, &mut rows);
    let q = apply_rewrite_safe(&q, dvm::rewrite_scalar_subquery_in_where, &mut rows);
    let q = apply_rewrite_safe(&q, dvm::rewrite_correlated_scalar_in_select, &mut rows);

    let mut q = q;
    for _ in 0..3 {
        let prev = q.clone();
        let q2 = dvm::rewrite_demorgan_sublinks(&q)?;
        let q2 = dvm::rewrite_sublinks_in_or(&q2)?;
        if q2 == prev {
            break;
        }
        q = q2;
    }
    let q = apply_rewrite_safe(&q, dvm::rewrite_rows_from, &mut rows);

    // TopK detection.
    let topk = dvm::detect_topk_pattern(&q)?;
    let (effective_q, is_topk) = if let Some(ref info) = topk {
        rows.push((
            "topk_pattern".to_string(),
            format!(
                "LIMIT {}, ORDER BY: {}",
                info.limit_value, info.order_by_sql
            ),
            "INFO".to_string(),
        ));
        (info.base_query.clone(), true)
    } else {
        (q.clone(), false)
    };

    // Determine what refresh mode AUTO would assign.
    let resolved_mode = if is_topk {
        "TOPK".to_string()
    } else {
        resolve_auto_refresh_mode(&effective_q, &mut rows)
    };

    // Insert the resolved-mode row at position 0.
    rows.insert(
        0,
        (
            "resolved_refresh_mode".to_string(),
            resolved_mode,
            "INFO".to_string(),
        ),
    );

    // Full DVM parse for further construct analysis (skip on FULL-only).
    if !is_topk {
        match dvm::parse_defining_query_full(&effective_q) {
            Ok(result) => {
                // Emit parse warnings.
                for w in &result.warnings {
                    rows.push((
                        "parse_warning".to_string(),
                        w.clone(),
                        "WARNING".to_string(),
                    ));
                }

                // Recurse: report constructs.
                let constructs = collect_tree_constructs(&result.tree, &result.cte_registry);
                for c in constructs {
                    let (check, detail) = split_construct_label(&c);
                    let severity = construct_severity(&check, &detail);
                    rows.push((check, detail, severity));
                }

                // Recursion.
                if result.has_recursion {
                    rows.push((
                        "recursive_cte".to_string(),
                        "true".to_string(),
                        "WARNING".to_string(),
                    ));
                }

                // Volatility.
                if let Ok(v) = dvm::tree_worst_volatility_with_registry(&result) {
                    let (label, sev) = match v {
                        'i' => ("immutable", "INFO"),
                        's' => ("stable", "INFO"),
                        _ => ("volatile", "WARNING"),
                    };
                    rows.push(("volatility".to_string(), label.to_string(), sev.to_string()));
                }

                // Aux-column requirements.
                if dvm::query_needs_pgt_count(&effective_q) {
                    rows.push((
                        "needs_pgt_count".to_string(),
                        "true — multiplicity counter column required (DISTINCT/aggregation)"
                            .to_string(),
                        "INFO".to_string(),
                    ));
                }
                if dvm::query_needs_dual_count(&effective_q) {
                    rows.push((
                        "needs_dual_count".to_string(),
                        "true — left/right multiplicity counters required (INTERSECT/EXCEPT)"
                            .to_string(),
                        "INFO".to_string(),
                    ));
                }
            }
            Err(e) => {
                rows.push((
                    "dvm_parse_error".to_string(),
                    format!("{}", e),
                    "ERROR".to_string(),
                ));
            }
        }
    }

    Ok(rows)
}

/// Run a rewrite pass; on error push an ERROR row and return the input unchanged.
fn apply_rewrite_safe(
    q: &str,
    f: impl Fn(&str) -> Result<String, PgTrickleError>,
    rows: &mut Vec<(String, String, String)>,
) -> String {
    match f(q) {
        Ok(out) => out,
        Err(e) => {
            rows.push((
                "rewrite_error".to_string(),
                format!("{}", e),
                "ERROR".to_string(),
            ));
            q.to_string()
        }
    }
}

/// Run the checks that AUTO mode uses to decide whether DIFFERENTIAL is feasible.
/// Appends relevant WARNING rows and returns `"DIFFERENTIAL"` or `"FULL"`.
fn resolve_auto_refresh_mode(q: &str, rows: &mut Vec<(String, String, String)>) -> String {
    // 1. Non-linear / unsupported constructs.
    if let Err(e) = dvm::reject_unsupported_constructs(q) {
        rows.push((
            "unsupported_construct".to_string(),
            format!("{}", e),
            "WARNING".to_string(),
        ));
        return "FULL".to_string();
    }

    // 2. Materialized views / foreign tables.
    if let Err(e) = dvm::reject_materialized_views(q) {
        rows.push((
            "matview_or_foreign_table".to_string(),
            format!("{}", e),
            "WARNING".to_string(),
        ));
        return "FULL".to_string();
    }

    // 3. DVM parse + IVM support.
    match dvm::parse_defining_query_full(q) {
        Ok(result) => {
            if let Err(e) = dvm::check_ivm_support_with_registry(&result) {
                rows.push((
                    "ivm_support_check".to_string(),
                    format!("DIFFERENTIAL not supported: {}", e),
                    "WARNING".to_string(),
                ));
                return "FULL".to_string();
            }
            "DIFFERENTIAL".to_string()
        }
        Err(e) => {
            rows.push((
                "ivm_support_check".to_string(),
                format!("parse failed: {}", e),
                "WARNING".to_string(),
            ));
            "FULL".to_string()
        }
    }
}

/// Split a `"key:value"` construct label into `(key, value)`.
fn split_construct_label(label: &str) -> (String, String) {
    if let Some(pos) = label.find(':') {
        (label[..pos].to_string(), label[pos + 1..].to_string())
    } else {
        (label.to_string(), String::new())
    }
}

/// Return a severity string for a detected construct.
fn construct_severity(check: &str, detail: &str) -> String {
    match check {
        "join" if detail == "FULL_OUTER" => "WARNING",
        "set_op" if detail.starts_with("EXCEPT") => "WARNING",
        "window_function" => "INFO",
        "scalar_subquery" => "INFO",
        "lateral" => "INFO",
        "aggregate" if detail.contains("GROUP_RESCAN") => "WARNING",
        _ => "INFO",
    }
    .to_string()
}

// ── Shared helper re-exported for use by diagnostics from api.rs ──────────

// `parse_qualified_name` is defined in api.rs (not pub). We expose a thin
// wrapper here that diagnostics.rs can import via crate::api.
//
// NOTE: this wrapper is declared in api.rs as `pub(crate)` — see below.
