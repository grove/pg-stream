//! GROUP BY aggregate differentiation.
//!
//! Uses auxiliary counters (count, sum) stored alongside user data
//! to maintain aggregate values incrementally.
//!
//! For each group, computes the net change from inserts/deletes in the child
//! delta, then merges with existing ST state to determine if the group:
//! - Appears (new_count > 0, was 0) → INSERT
//! - Vanishes (new_count ≤ 0, was > 0) → DELETE
//! - Changes value → UPDATE (emitted as DELETE + INSERT pair)

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::scan::build_hash_expr;
use crate::dvm::parser::{AggExpr, AggFunc, Expr, OpTree};
use crate::error::PgTrickleError;

/// Resolve a column reference expression against child CTE column names.
///
/// For a qualified `ColumnRef("c", "region")`, checks if `c__region`
/// exists in child_cols (from join disambiguation). Falls back to
/// the unqualified name, then to the original expression.
///
/// Returns an UNQUOTED column name — callers handle quoting.
fn resolve_col_for_child(expr: &Expr, child_cols: &[String]) -> String {
    match expr {
        Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name,
        } => {
            // Direct disambiguated: tbl__col
            let disambiguated = format!("{tbl}__{column_name}");
            if child_cols.contains(&disambiguated) {
                return disambiguated;
            }
            // Nested join prefix: *__tbl__col
            let nested_suffix = format!("__{tbl}__{column_name}");
            for c in child_cols {
                if c.ends_with(&nested_suffix) {
                    return c.clone();
                }
            }
            // Exact match on column name alone
            if child_cols.contains(column_name) {
                return column_name.clone();
            }
            column_name.clone()
        }
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => {
            // Exact match
            if child_cols.contains(column_name) {
                return column_name.clone();
            }
            // Suffix match: find column ending in __column_name
            let suffix = format!("__{column_name}");
            let matches: Vec<&String> =
                child_cols.iter().filter(|c| c.ends_with(&suffix)).collect();
            if matches.len() == 1 {
                return matches[0].clone();
            }
            column_name.clone()
        }
        _ => expr.strip_qualifier().to_sql(),
    }
}

/// Resolve a group-by expression for the child CTE's column names.
fn resolve_group_col(expr: &Expr, child_cols: &[String]) -> String {
    resolve_col_for_child(expr, child_cols)
}

/// Resolve an entire expression tree against child CTE column names.
///
/// Recursively rewrites `ColumnRef` nodes using `resolve_col_for_child`
/// and rebuilds the surrounding expression in SQL syntax. Used for FILTER
/// clauses where the predicate may contain arbitrary expressions.
fn resolve_expr_for_child(expr: &Expr, child_cols: &[String]) -> String {
    match expr {
        Expr::ColumnRef { .. } => resolve_col_for_child(expr, child_cols),
        Expr::BinaryOp { op, left, right } => {
            format!(
                "({} {op} {})",
                resolve_expr_for_child(left, child_cols),
                resolve_expr_for_child(right, child_cols),
            )
        }
        Expr::FuncCall { func_name, args } => {
            let resolved_args: Vec<String> = args
                .iter()
                .map(|a| resolve_expr_for_child(a, child_cols))
                .collect();
            format!("{func_name}({})", resolved_args.join(", "))
        }
        Expr::Raw(sql) => {
            // Best-effort: replace column refs in raw SQL
            crate::dvm::operators::filter::replace_column_refs_in_raw(sql, child_cols)
        }
        _ => expr.to_sql(),
    }
}

// ── Group-rescan helpers ────────────────────────────────────────────

/// Reconstruct the FROM clause SQL from a child OpTree.
///
/// Returns the SQL fragment for `FROM ...` suitable for the rescan CTE.
/// Returns `None` for complex children (CTEs, subqueries, unions) that
/// cannot be reconstructed reliably.
fn child_to_from_sql(child: &OpTree) -> Option<String> {
    match child {
        OpTree::Scan {
            schema,
            table_name,
            alias,
            ..
        } => Some(format!(
            "\"{}\".\"{}\" AS \"{}\"",
            schema.replace('"', "\"\""),
            table_name.replace('"', "\"\""),
            alias.replace('"', "\"\""),
        )),
        OpTree::Filter { predicate, child } => {
            let inner = child_to_from_sql(child)?;
            Some(format!("{inner} WHERE {}", predicate.to_sql()))
        }
        OpTree::InnerJoin {
            condition,
            left,
            right,
        } => {
            let l = child_to_from_sql(left)?;
            let r = child_to_from_sql(right)?;
            Some(format!("{l} INNER JOIN {r} ON {}", condition.to_sql()))
        }
        OpTree::LeftJoin {
            condition,
            left,
            right,
        } => {
            let l = child_to_from_sql(left)?;
            let r = child_to_from_sql(right)?;
            Some(format!("{l} LEFT JOIN {r} ON {}", condition.to_sql()))
        }
        OpTree::FullJoin {
            condition,
            left,
            right,
        } => {
            let l = child_to_from_sql(left)?;
            let r = child_to_from_sql(right)?;
            Some(format!("{l} FULL JOIN {r} ON {}", condition.to_sql()))
        }
        OpTree::Project { child, .. } => child_to_from_sql(child),
        OpTree::Subquery {
            child,
            alias,
            column_aliases,
        } => {
            // Only recurse into Subquery when the child is an Aggregate.
            // An Aggregate child produces a complete subquery expression
            // (SELECT ... GROUP BY ...) that can be aliased and used as FROM.
            //
            // For other child types (Project, Filter over joins, etc.),
            // return None so callers fall back to the defining-query approach.
            // This is important because Project nodes rename columns with
            // aliases (e.g., `extract(year from o_orderdate) AS o_year`)
            // that are lost when child_to_from_sql recurses through them.
            match child.as_ref() {
                OpTree::Aggregate { .. } => {
                    let inner = child_to_from_sql(child)?;
                    if column_aliases.is_empty() {
                        Some(format!("{inner} AS {}", quote_ident(alias)))
                    } else {
                        // Apply positional column aliases using PostgreSQL's
                        // AS alias(col1, col2) syntax to match the delta's
                        // renamed columns from diff_subquery.
                        let col_list: Vec<String> =
                            column_aliases.iter().map(|a| quote_ident(a)).collect();
                        Some(format!(
                            "{inner} AS {}({})",
                            quote_ident(alias),
                            col_list.join(", ")
                        ))
                    }
                }
                _ => None,
            }
        }
        OpTree::Aggregate {
            group_by,
            aggregates,
            child,
            ..
        } => {
            let inner_from = child_to_from_sql(child)?;
            let mut selects = Vec::new();
            for expr in group_by {
                selects.push(expr.to_sql());
            }
            // NOTE: do NOT include COUNT(*) AS __pgt_count here.
            // The intermediate aggregate delta output_cols excludes __pgt_count
            // (it's internal bookkeeping), so the child_to_from_sql must match.
            // Including it here would cause column count mismatches in the
            // EXCEPT ALL between SELECT * FROM this subquery and the delta CTE.
            for agg in aggregates {
                selects.push(format!(
                    "{} AS {}",
                    agg_to_rescan_sql(agg),
                    quote_ident(&agg.alias),
                ));
            }
            let gb = if group_by.is_empty() {
                String::new()
            } else {
                let cols: Vec<String> = group_by.iter().map(|e| e.to_sql()).collect();
                format!(" GROUP BY {}", cols.join(", "))
            };
            Some(format!(
                "(SELECT {} FROM {inner_from}{})",
                selects.join(", "),
                gb,
            ))
        }
        _ => None,
    }
}

/// Reconstruct an aggregate function call as SQL text for the rescan CTE.
///
/// Handles regular aggregates (`BIT_AND(flags)`), aggregates with DISTINCT,
/// FILTER clauses, second arguments (`STRING_AGG(name, ', ')`), and
/// ordered-set aggregates (`MODE() WITHIN GROUP (ORDER BY amount)`).
///
/// SQL/JSON standard aggregates (`JSON_OBJECTAGG`, `JSON_ARRAYAGG`) carry
/// their full deparsed SQL in the `AggFunc` variant and are returned directly.
pub fn agg_to_rescan_sql(agg: &AggExpr) -> String {
    // SQL/JSON standard aggregates: use the stored raw SQL directly since
    // their special syntax (key: value, ABSENT ON NULL, etc.) cannot be
    // reconstructed from function name + arguments.
    match &agg.function {
        AggFunc::JsonObjectAggStd(raw) | AggFunc::JsonArrayAggStd(raw) => {
            let filter = match &agg.filter {
                Some(f) => format!(" FILTER (WHERE {})", f.to_sql()),
                None => String::new(),
            };
            return format!("{raw}{filter}");
        }
        AggFunc::ComplexExpression(raw) => {
            // Complex expression with nested aggregates: the stored raw SQL
            // already contains the full expression (including aggregate calls).
            // The rescan CTE evaluates it against source data directly.
            return raw.clone();
        }
        _ => {}
    }

    let func_name = agg.function.sql_name();
    let distinct_str = if agg.is_distinct { "DISTINCT " } else { "" };

    // Build argument list
    let mut arg_parts = Vec::new();
    if matches!(agg.function, AggFunc::CountStar) {
        arg_parts.push("*".to_string());
    } else if let Some(ref arg) = agg.argument {
        arg_parts.push(format!("{distinct_str}{}", arg.to_sql()));
    }
    if let Some(ref second) = agg.second_arg {
        arg_parts.push(second.to_sql());
    }

    let args_sql = arg_parts.join(", ");

    // Ordered-set aggregates (MODE, PERCENTILE_*) use WITHIN GROUP (ORDER BY ...).
    // Regular aggregates (STRING_AGG, ARRAY_AGG, etc.) use ORDER BY inside parens.
    let is_ordered_set = matches!(
        agg.function,
        AggFunc::Mode | AggFunc::PercentileCont | AggFunc::PercentileDisc
    );

    let order_sql = match &agg.order_within_group {
        Some(sorts) if !sorts.is_empty() => {
            let sort_list: Vec<String> = sorts
                .iter()
                .map(|s| {
                    let dir = if s.ascending { "" } else { " DESC" };
                    let nulls = if s.ascending {
                        if s.nulls_first { " NULLS FIRST" } else { "" }
                    } else if s.nulls_first {
                        ""
                    } else {
                        " NULLS LAST"
                    };
                    format!("{}{dir}{nulls}", s.expr.to_sql())
                })
                .collect();
            Some(sort_list.join(", "))
        }
        _ => None,
    };

    let (base, within_group) = match order_sql {
        Some(ref order) if is_ordered_set => (
            format!("{func_name}({args_sql})"),
            format!(" WITHIN GROUP (ORDER BY {order})"),
        ),
        Some(ref order) => (
            format!("{func_name}({args_sql} ORDER BY {order})"),
            String::new(),
        ),
        None => (format!("{func_name}({args_sql})"), String::new()),
    };

    // FILTER clause
    let filter = match &agg.filter {
        Some(f) => format!(" FILTER (WHERE {})", f.to_sql()),
        None => String::new(),
    };

    format!("{base}{within_group}{filter}")
}

// ── Intermediate aggregate delta (subquery-in-FROM) ─────────────────

/// Returns true if the aggregate's old value can be computed algebraically
/// from new value + delta counts: `old = new - ins + del`.
///
/// Only COUNT, COUNT_STAR, and SUM are algebraically invertible.
/// MIN/MAX/AVG and group-rescan aggregates require a full rescan of old data.
fn is_algebraically_invertible(agg: &AggExpr) -> bool {
    matches!(
        agg.function,
        AggFunc::CountStar | AggFunc::Count | AggFunc::Sum
    )
}

/// Build delta CTEs for an intermediate aggregate (one whose group-by
/// columns do NOT exist in the stream table).
///
/// Instead of LEFT JOINing to the stream table (which doesn't have the
/// intermediate columns), this builds:
///
/// **Algebraic path** (when all aggregates are COUNT/SUM):
/// 1. A "new rescan" CTE: re-aggregates affected groups from current data.
/// 2. A final CTE: emits 'D' with old values computed algebraically
///    (`old = new - ins + del` from the already-computed delta_cte) and
///    'I' with new values from the rescan.
///
/// **EXCEPT ALL path** (when MIN/MAX/group-rescan aggregates are present):
/// 1. A "new rescan" CTE: re-aggregates affected groups from current data.
/// 2. An "old rescan" CTE: re-aggregates affected groups from old data
///    (current data minus child delta inserts, plus child delta deletes).
/// 3. A final CTE: emits 'D' for old rows and 'I' for new rows.
///
/// The algebraic path is preferred because it:
/// - Avoids a second `diff_node(child)` call (no duplicate CTEs)
/// - Eliminates EXCEPT ALL (no column-matching or materialization issues)
/// - Uses the already-computed delta values from the standard aggregate path
///
/// The parent operator receives D/I pairs and processes them as normal
/// delta events.
fn build_intermediate_agg_delta(
    ctx: &mut DiffContext,
    child: &OpTree,
    group_by: &[Expr],
    group_output: &[String],
    aggregates: &[AggExpr],
    delta_cte: &str,
) -> Result<DiffResult, PgTrickleError> {
    let source_from = child_to_from_sql(child);

    // We need the child's source SQL for rescanning. If we can't reconstruct
    // it, fall back to the defining query approach.
    let from_sql = match source_from {
        Some(sql) => sql,
        None => {
            // Fallback: if we have the defining query, wrap it. Otherwise error.
            return Err(PgTrickleError::InternalError(
                "Cannot build intermediate aggregate delta: \
                 child FROM clause cannot be reconstructed"
                    .into(),
            ));
        }
    };

    // Build the rescan SELECT list: group columns + all aggregates
    let mut rescan_selects = Vec::new();
    for (expr, output) in group_by.iter().zip(group_output.iter()) {
        let expr_sql = expr.to_sql();
        let qt_output = quote_ident(output);
        if expr_sql == *output {
            rescan_selects.push(qt_output);
        } else {
            rescan_selects.push(format!("{expr_sql} AS {qt_output}"));
        }
    }
    // COUNT(*) to track the group's row count (__pgt_count)
    rescan_selects.push("COUNT(*) AS __pgt_count".to_string());
    for agg in aggregates {
        rescan_selects.push(format!(
            "{} AS {}",
            agg_to_rescan_sql(agg),
            quote_ident(&agg.alias),
        ));
    }

    // GROUP BY clause
    let group_by_sql = if group_by.is_empty() {
        String::new()
    } else {
        let gb: Vec<String> = group_by.iter().map(|e| e.to_sql()).collect();
        format!("\nGROUP BY {}", gb.join(", "))
    };

    // Group filter: only rescan affected groups
    let group_filter = if group_output.is_empty() {
        String::new()
    } else if group_output.len() == 1 {
        let col = &group_output[0];
        format!("{col} IN (SELECT {} FROM {delta_cte})", quote_ident(col),)
    } else {
        let corr: Vec<String> = group_output
            .iter()
            .map(|c| format!("{c} IS NOT DISTINCT FROM __pgt_d2.{}", quote_ident(c)))
            .collect();
        format!(
            "EXISTS (SELECT 1 FROM {delta_cte} __pgt_d2 WHERE {})",
            corr.join(" AND "),
        )
    };

    // Determine WHERE/AND connector based on existing WHERE in from_sql.
    // Only check for WHERE at the outer level — if from_sql is a subquery
    // (starts with '('), any WHERE inside is internal to the subquery.
    let has_outer_where = from_sql.contains(" WHERE ") && !from_sql.starts_with('(');
    let where_connector = if group_filter.is_empty() {
        String::new()
    } else if has_outer_where {
        format!("\n  AND {group_filter}")
    } else {
        format!("\nWHERE {group_filter}")
    };

    // ── New rescan CTE: aggregate on current (post-change) data ─────
    let new_rescan_cte = ctx.next_cte_name("agg_new");
    let new_rescan_sql = format!(
        "SELECT {selects}\nFROM {from_sql}{where_connector}{group_by}",
        selects = rescan_selects.join(",\n       "),
        group_by = group_by_sql,
    );
    ctx.add_cte(new_rescan_cte.clone(), new_rescan_sql);

    // Build output columns — exclude __pgt_count since this is an intermediate
    // aggregate. __pgt_count is internal bookkeeping for the top-level aggregate's
    // MERGE with the stream table. Including it in intermediate output causes
    // parent operators (e.g., InnerJoin) to propagate it into column references
    // against snapshots that don't have it (leading to "column __pgt_count
    // does not exist" errors).
    let mut output_cols = Vec::new();
    output_cols.extend(group_output.iter().cloned());
    for agg in aggregates {
        output_cols.push(agg.alias.clone());
    }

    // Check if all aggregates are algebraically invertible.
    // If so, we can compute old = new - ins + del using the already-computed
    // delta_cte, avoiding a second diff_node(child) call and EXCEPT ALL.
    let all_algebraic = aggregates.iter().all(is_algebraically_invertible);

    if all_algebraic {
        // ── Algebraic path: old = new − ins + del ───────────────────
        //
        // D events use the delta_cte (which has __ins_count, __del_count,
        // __ins_{alias}, __del_{alias} per group) LEFT JOINed with
        // new_rescan to compute old values algebraically.
        // I events come directly from new_rescan.
        //
        // This avoids a second diff_node(child) call (no duplicate CTEs)
        // and eliminates the EXCEPT ALL (no column-matching issues).

        let final_cte = ctx.next_cte_name("agg_final");

        // Row ID from group-by columns (d prefix for D events, n for I)
        let group_hash_d: Vec<String> = group_output
            .iter()
            .map(|c| format!("d.{}::TEXT", quote_ident(c)))
            .collect();
        let group_hash_n: Vec<String> = group_output
            .iter()
            .map(|c| format!("n.{}::TEXT", quote_ident(c)))
            .collect();
        let row_id_d = if group_hash_d.is_empty() {
            "pgtrickle.pg_trickle_hash('__singleton_group')".to_string()
        } else {
            build_hash_expr(&group_hash_d)
        };
        let row_id_n = if group_hash_n.is_empty() {
            "pgtrickle.pg_trickle_hash('__singleton_group')".to_string()
        } else {
            build_hash_expr(&group_hash_n)
        };

        // D event group columns from delta_cte
        let d_group_refs: Vec<String> = group_output
            .iter()
            .map(|c| format!("d.{}", quote_ident(c)))
            .collect();
        let n_group_refs: Vec<String> = group_output
            .iter()
            .map(|c| format!("n.{}", quote_ident(c)))
            .collect();

        // Algebraic old expressions: old_X = COALESCE(new_X, 0) - ins_X + del_X
        let old_pgt_count = "COALESCE(n.\"__pgt_count\", 0) \
                             - COALESCE(d.\"__ins_count\", 0) \
                             + COALESCE(d.\"__del_count\", 0)";
        let old_agg_exprs: Vec<String> = aggregates
            .iter()
            .map(|agg| {
                let alias = &agg.alias;
                let ins_col = format!("__ins_{alias}");
                let del_col = format!("__del_{alias}");
                format!(
                    "COALESCE(n.{a}, 0) - COALESCE(d.{i}, 0) + COALESCE(d.{d}, 0) AS {a}",
                    a = quote_ident(alias),
                    i = quote_ident(&ins_col),
                    d = quote_ident(&del_col),
                )
            })
            .collect();

        let new_agg_refs: Vec<String> = aggregates
            .iter()
            .map(|a| format!("n.{}", quote_ident(&a.alias)))
            .collect();

        // JOIN condition between delta_cte and new_rescan on group columns
        let join_cond = if group_output.is_empty() {
            "TRUE".to_string()
        } else {
            group_output
                .iter()
                .map(|c| format!("d.{q} IS NOT DISTINCT FROM n.{q}", q = quote_ident(c),))
                .collect::<Vec<_>>()
                .join(" AND ")
        };

        // Format column sections
        let extra_d_groups = if d_group_refs.is_empty() {
            String::new()
        } else {
            format!("{}, ", d_group_refs.join(", "))
        };
        let extra_n_groups = if n_group_refs.is_empty() {
            String::new()
        } else {
            format!("{}, ", n_group_refs.join(", "))
        };
        let extra_old_aggs = if old_agg_exprs.is_empty() {
            String::new()
        } else {
            format!(",\n       {}", old_agg_exprs.join(",\n       "))
        };
        let extra_new_aggs = if new_agg_refs.is_empty() {
            String::new()
        } else {
            format!(",\n       {}", new_agg_refs.join(",\n       "))
        };

        let final_sql = format!(
            "\
-- D events: old state (algebraic: old = new - ins + del)
SELECT {row_id_d} AS __pgt_row_id,
       'D'::TEXT AS __pgt_action,
       {extra_d_groups}{old_pgt_count} AS __pgt_count{extra_old_aggs}
FROM {delta_cte} d
LEFT JOIN {new_rescan_cte} n ON {join_cond}

UNION ALL

-- I events: new state of affected groups
SELECT {row_id_n} AS __pgt_row_id,
       'I'::TEXT AS __pgt_action,
       {extra_n_groups}n.__pgt_count{extra_new_aggs}
FROM {new_rescan_cte} n",
        );

        ctx.add_cte(final_cte.clone(), final_sql);

        Ok(DiffResult {
            cte_name: final_cte,
            columns: output_cols,
            is_deduplicated: false,
        })
    } else {
        // ── EXCEPT ALL path: rescan old data ────────────────────────
        //
        // Old data = current data - delta inserts + delta deletes.
        // We use EXCEPT ALL / UNION ALL which works positionally (by column
        // position, not by name). The source side uses SELECT * from the
        // original FROM clause, while the delta side uses the child delta
        // CTE columns. Positional matching handles name differences.

        // Get the child's delta CTE columns (these are what diff_node
        // produced for the child). We need data columns only (excluding
        // __pgt_action, __pgt_row_id).
        let child_result = ctx.diff_node(child)?;
        let delta_data_cols: Vec<String> = child_result
            .columns
            .iter()
            .map(|c| quote_ident(c))
            .collect();
        let delta_col_list = delta_data_cols.join(", ");

        // Use the same alias for the old_rescan wrapper as the original
        // FROM expression so qualified column refs resolve correctly.
        let old_rescan_alias = if from_sql.starts_with('(') {
            if let Some(as_pos) = from_sql.rfind(" AS ") {
                from_sql[as_pos + 4..].trim_matches('"').to_string()
            } else {
                "__pgt_old".to_string()
            }
        } else {
            "__pgt_old".to_string()
        };

        let old_rescan_cte = ctx.next_cte_name("agg_old");
        let old_rescan_sql = format!(
            "SELECT {selects}\nFROM (\
             SELECT * FROM {from_sql}{where_connector} \
             EXCEPT ALL \
             SELECT {delta_col_list} FROM {child_delta} WHERE __pgt_action = 'I' \
             UNION ALL \
             SELECT {delta_col_list} FROM {child_delta} WHERE __pgt_action = 'D'\
             ) {old_alias}{group_by}",
            selects = rescan_selects.join(",\n       "),
            child_delta = child_result.cte_name,
            old_alias = quote_ident(&old_rescan_alias),
            group_by = group_by_sql,
        );
        ctx.add_cte(old_rescan_cte.clone(), old_rescan_sql);

        // ── Final CTE: emit D/I pairs ───────────────────────────────
        let final_cte = ctx.next_cte_name("agg_final");

        let group_hash_n: Vec<String> = group_output
            .iter()
            .map(|c| format!("n.{}::TEXT", quote_ident(c)))
            .collect();
        let group_hash_o: Vec<String> = group_output
            .iter()
            .map(|c| format!("o.{}::TEXT", quote_ident(c)))
            .collect();
        let row_id_new = if group_hash_n.is_empty() {
            "pgtrickle.pg_trickle_hash('__singleton_group')".to_string()
        } else {
            build_hash_expr(&group_hash_n)
        };
        let row_id_old = if group_hash_o.is_empty() {
            "pgtrickle.pg_trickle_hash('__singleton_group')".to_string()
        } else {
            build_hash_expr(&group_hash_o)
        };

        let new_group_refs = group_output
            .iter()
            .map(|c| format!("n.{}", quote_ident(c)))
            .collect::<Vec<_>>()
            .join(", ");
        let old_group_refs = group_output
            .iter()
            .map(|c| format!("o.{}", quote_ident(c)))
            .collect::<Vec<_>>()
            .join(", ");

        let new_agg_refs: Vec<String> = aggregates
            .iter()
            .map(|a| format!("n.{}", quote_ident(&a.alias)))
            .collect();
        let old_agg_refs: Vec<String> = aggregates
            .iter()
            .map(|a| format!("o.{}", quote_ident(&a.alias)))
            .collect();

        let extra_new_groups = if new_group_refs.is_empty() {
            String::new()
        } else {
            format!("{new_group_refs}, ")
        };
        let extra_old_groups = if old_group_refs.is_empty() {
            String::new()
        } else {
            format!("{old_group_refs}, ")
        };
        let extra_new_aggs = if new_agg_refs.is_empty() {
            String::new()
        } else {
            format!(",\n       {}", new_agg_refs.join(",\n       "))
        };
        let extra_old_aggs = if old_agg_refs.is_empty() {
            String::new()
        } else {
            format!(",\n       {}", old_agg_refs.join(",\n       "))
        };

        let final_sql = format!(
            "\
-- D events: old state of affected groups
SELECT {row_id_old} AS __pgt_row_id,
       'D'::TEXT AS __pgt_action,
       {extra_old_groups}o.__pgt_count{extra_old_aggs}
FROM {old_rescan_cte} o

UNION ALL

-- I events: new state of affected groups
SELECT {row_id_new} AS __pgt_row_id,
       'I'::TEXT AS __pgt_action,
       {extra_new_groups}n.__pgt_count{extra_new_aggs}
FROM {new_rescan_cte} n",
        );

        ctx.add_cte(final_cte.clone(), final_sql);

        Ok(DiffResult {
            cte_name: final_cte,
            columns: output_cols,
            is_deduplicated: false,
        })
    }
}

/// Build a rescan CTE that re-aggregates affected groups from the source
/// table. Used for group-rescan aggregates (BIT_AND, STRING_AGG, etc.)
/// and MIN/MAX (semi-algebraic: needs rescan when extremum is deleted).
///
/// The CTE selects from the original source tables (reconstructed from
/// the child OpTree or wrapped from the defining query), filters to only
/// the groups that had changes (via semi-join to the delta CTE), and
/// re-aggregates those groups.
///
/// Returns `Some(cte_name)` if a rescan CTE was created, `None` otherwise.
fn build_rescan_cte(
    ctx: &mut DiffContext,
    child: &OpTree,
    group_by: &[Expr],
    group_output: &[String],
    aggregates: &[AggExpr],
    delta_cte: &str,
) -> Option<String> {
    // Include group-rescan aggregates AND MIN/MAX (which need rescan
    // when the old extremum is deleted).
    let rescan_aggs: Vec<&AggExpr> = aggregates
        .iter()
        .filter(|a| {
            a.function.is_group_rescan() || matches!(a.function, AggFunc::Min | AggFunc::Max)
        })
        .collect();
    if rescan_aggs.is_empty() {
        return None;
    }

    let rescan_cte = ctx.next_cte_name("agg_rescan");

    // Build SELECT list: group columns + rescan aggregate calls
    let mut selects = Vec::new();
    for (expr, output) in group_by.iter().zip(group_output.iter()) {
        let expr_sql = expr.to_sql();
        let qt_output = quote_ident(output);
        if expr_sql == *output {
            selects.push(qt_output);
        } else {
            selects.push(format!("{expr_sql} AS {qt_output}"));
        }
    }
    for agg in &rescan_aggs {
        selects.push(format!(
            "{} AS {}",
            agg_to_rescan_sql(agg),
            quote_ident(&agg.alias),
        ));
    }

    // Build GROUP BY clause
    let group_by_sql = if group_by.is_empty() {
        String::new()
    } else {
        let gb: Vec<String> = group_by.iter().map(|e| e.to_sql()).collect();
        format!("\nGROUP BY {}", gb.join(", "))
    };

    // Try to reconstruct the FROM clause from the child OpTree
    let source_from = child_to_from_sql(child);

    let rescan_sql = if let Some(from_sql) = source_from {
        // Direct source reconstruction: more efficient since we can push
        // the group filter into the WHERE clause before aggregation.
        let group_filter = if group_output.is_empty() {
            String::new()
        } else if group_output.len() == 1 {
            let col = &group_output[0];
            format!("{col} IN (SELECT {} FROM {delta_cte})", quote_ident(col),)
        } else {
            // Multi-column group key: use EXISTS with IS NOT DISTINCT FROM
            // to correctly handle NULL group-key values.
            let corr: Vec<String> = group_output
                .iter()
                .map(|c| format!("{c} IS NOT DISTINCT FROM __pgt_d2.{}", quote_ident(c),))
                .collect();
            format!(
                "EXISTS (SELECT 1 FROM {delta_cte} __pgt_d2 WHERE {})",
                corr.join(" AND "),
            )
        };

        // If the child is a Filter, the FROM already includes WHERE.
        // We need to use AND instead of WHERE for the group filter.
        // Only check for WHERE at the outer level — if from_sql is a
        // subquery (starts with '('), any WHERE inside is internal.
        let has_outer_where = from_sql.contains(" WHERE ") && !from_sql.starts_with('(');
        if group_filter.is_empty() {
            format!(
                "SELECT {selects}\nFROM {from_sql}{group_by}",
                selects = selects.join(",\n       "),
                group_by = group_by_sql,
            )
        } else if has_outer_where {
            format!(
                "SELECT {selects}\nFROM {from_sql}\n  AND {group_filter}{group_by}",
                selects = selects.join(",\n       "),
                group_by = group_by_sql,
            )
        } else {
            format!(
                "SELECT {selects}\nFROM {from_sql}\nWHERE {group_filter}{group_by}",
                selects = selects.join(",\n       "),
                group_by = group_by_sql,
            )
        }
    } else if let Some(ref defining_query) = ctx.defining_query {
        // Fallback: wrap the defining query as a subquery and filter to
        // affected groups. Less efficient (re-aggregates all groups then
        // filters) but correct for any child OpTree shape.
        let where_clause = if group_output.is_empty() {
            String::new()
        } else if group_output.len() == 1 {
            let col = &group_output[0];
            format!(
                "\nWHERE __pgt_dq.{} IN (SELECT {} FROM {delta_cte})",
                quote_ident(col),
                quote_ident(col),
            )
        } else {
            let corr: Vec<String> = group_output
                .iter()
                .map(|c| {
                    format!(
                        "__pgt_dq.{qc} IS NOT DISTINCT FROM __pgt_d2.{qc}",
                        qc = quote_ident(c),
                    )
                })
                .collect();
            format!(
                "\nWHERE EXISTS (SELECT 1 FROM {delta_cte} __pgt_d2 WHERE {})",
                corr.join(" AND "),
            )
        };

        // Select only the rescan aggregate columns from the defining query
        let dq_selects: Vec<String> = group_output
            .iter()
            .map(|c| format!("__pgt_dq.{}", quote_ident(c)))
            .chain(
                rescan_aggs
                    .iter()
                    .map(|a| format!("__pgt_dq.{}", quote_ident(&a.alias))),
            )
            .collect();

        format!(
            "SELECT {selects}\nFROM ({defining_query}) __pgt_dq{where_clause}",
            selects = dq_selects.join(", "),
        )
    } else {
        // No defining query available — cannot build rescan CTE.
        // This shouldn't happen in practice since defining_query is
        // always set during refresh. Fall back to no rescan.
        return None;
    };

    ctx.add_cte(rescan_cte.clone(), rescan_sql);
    Some(rescan_cte)
}

// ── P5: Direct aggregate bypass helpers ─────────────────────────────

/// Check if a Scan → Aggregate tree qualifies for the P5 direct bypass.
///
/// Requirements:
/// - Child is a direct `OpTree::Scan` (no intervening Filter/Project/Join)
/// - All aggregates are decomposable (SUM, COUNT, CountStar, AVG — not MIN/MAX)
/// - No DISTINCT aggregates
/// - All aggregate arguments are simple `ColumnRef` (or `None` for COUNT(*))
/// - All group-by expressions are simple `ColumnRef`
fn is_direct_agg_eligible(child: &OpTree, group_by: &[Expr], aggregates: &[AggExpr]) -> bool {
    if !matches!(child, OpTree::Scan { .. }) {
        return false;
    }
    for agg in aggregates {
        // P5 only supports decomposable algebraic aggregates without FILTER
        if matches!(agg.function, AggFunc::Min | AggFunc::Max) || agg.function.is_group_rescan() {
            return false;
        }
        if agg.is_distinct {
            return false;
        }
        if agg.filter.is_some() {
            return false;
        }
        if let Some(arg) = &agg.argument
            && !matches!(arg, Expr::ColumnRef { .. })
        {
            return false;
        }
    }
    for expr in group_by {
        if !matches!(expr, Expr::ColumnRef { .. }) {
            return false;
        }
    }
    true
}

/// P5 + P7 — Generate a direct aggregate delta CTE from the change buffer.
///
/// Instead of differentiating the child Scan (which would go through the full
/// scan delta pipeline with window functions), reads directly from the typed
/// change buffer table. Group-by keys and aggregate arguments are referenced
/// as `c."new_{col}"` / `c."old_{col}"` — typed columns that are already
/// available from the P7 typed change buffer.
///
/// For UPDATE rows, the LATERAL VALUES expansion splits each change into
/// an INSERT side (from `new_*` columns) and a DELETE side (from `old_*`
/// columns), correctly handling group-key changes.
///
/// Returns `(delta_cte_name, group_output_names)`.
fn generate_direct_agg_delta(
    ctx: &mut DiffContext,
    scan: &OpTree,
    group_by: &[Expr],
    aggregates: &[AggExpr],
) -> Result<(String, Vec<String>), PgTrickleError> {
    let OpTree::Scan {
        table_oid,
        columns: _,
        ..
    } = scan
    else {
        return Err(PgTrickleError::InternalError(
            "generate_direct_agg_delta called on non-Scan".into(),
        ));
    };

    let change_table = format!(
        "{}.changes_{}",
        quote_ident(&ctx.change_buffer_schema),
        table_oid,
    );
    let prev_lsn = ctx.get_prev_lsn(*table_oid);
    let new_lsn = ctx.get_new_lsn(*table_oid);

    // Collect group-by column names
    let group_output: Vec<String> = group_by.iter().map(|e| e.output_name()).collect();

    // Collect unique aggregate argument column names
    let mut arg_cols: Vec<String> = Vec::new();
    for agg in aggregates {
        if let Some(arg) = &agg.argument {
            let name = arg.output_name();
            if !arg_cols.contains(&name) {
                arg_cols.push(name);
            }
        }
    }

    // ── Build LATERAL VALUES using typed columns ──────────────────────
    let mut val_aliases = vec!["side".to_string()];
    let mut val_i_parts = vec!["'I'".to_string()];
    let mut val_d_parts = vec!["'D'".to_string()];

    for name in &group_output {
        val_aliases.push(quote_ident(&format!("grp_{name}")));
        val_i_parts.push(format!("c.{}", quote_ident(&format!("new_{name}"))));
        val_d_parts.push(format!("c.{}", quote_ident(&format!("old_{name}"))));
    }

    for name in &arg_cols {
        val_aliases.push(quote_ident(&format!("val_{name}")));
        val_i_parts.push(format!("c.{}", quote_ident(&format!("new_{name}"))));
        val_d_parts.push(format!("c.{}", quote_ident(&format!("old_{name}"))));
    }

    // ── Build SELECT expressions ──────────────────────────────────────
    let delta_cte = ctx.next_cte_name("agg_delta");
    let mut select_exprs = Vec::new();

    // Group columns: v."grp_region" AS "region"
    for name in &group_output {
        select_exprs.push(format!(
            "v.{} AS {}",
            quote_ident(&format!("grp_{name}")),
            quote_ident(name),
        ));
    }

    // __ins_count, __del_count (total row counts per group)
    select_exprs
        .push("SUM(CASE WHEN v.side = 'I' THEN 1 ELSE 0 END)::bigint AS __ins_count".to_string());
    select_exprs
        .push("SUM(CASE WHEN v.side = 'D' THEN 1 ELSE 0 END)::bigint AS __del_count".to_string());

    // Per-aggregate delta expressions
    for agg in aggregates {
        let (ins_expr, del_expr) = direct_agg_delta_exprs(agg);
        select_exprs.push(format!(
            "{ins_expr} AS {}",
            quote_ident(&format!("__ins_{}", agg.alias)),
        ));
        select_exprs.push(format!(
            "{del_expr} AS {}",
            quote_ident(&format!("__del_{}", agg.alias)),
        ));
    }

    // GROUP BY
    let group_by_clause = if group_output.is_empty() {
        String::new()
    } else {
        let refs: Vec<String> = group_output
            .iter()
            .map(|name| format!("v.{}", quote_ident(&format!("grp_{name}"))))
            .collect();
        format!("\nGROUP BY {}", refs.join(", "))
    };

    let delta_sql = format!(
        "\
SELECT {selects}
FROM {change_table} c,
LATERAL (VALUES
    ({val_i}),
    ({val_d})
) v({val_aliases})
WHERE c.lsn > '{prev_lsn}'::pg_lsn AND c.lsn <= '{new_lsn}'::pg_lsn
  AND ((v.side = 'I' AND c.action != 'D')
    OR (v.side = 'D' AND c.action != 'I')){group_by}",
        selects = select_exprs.join(",\n       "),
        val_i = val_i_parts.join(", "),
        val_d = val_d_parts.join(", "),
        val_aliases = val_aliases.join(", "),
        group_by = group_by_clause,
    );

    ctx.add_cte(delta_cte.clone(), delta_sql);

    Ok((delta_cte, group_output))
}

/// Generate per-aggregate delta expressions for the P5 direct bypass CTE.
///
/// References VALUES alias columns `v."val_{col}"` and `v.side`.
fn direct_agg_delta_exprs(agg: &AggExpr) -> (String, String) {
    match &agg.function {
        AggFunc::CountStar => (
            "SUM(CASE WHEN v.side = 'I' THEN 1 ELSE 0 END)::bigint".to_string(),
            "SUM(CASE WHEN v.side = 'D' THEN 1 ELSE 0 END)::bigint".to_string(),
        ),
        AggFunc::Count => {
            let col = agg
                .argument
                .as_ref()
                .map(|e| format!("v.{}", quote_ident(&format!("val_{}", e.output_name()))))
                .unwrap_or_else(|| "1".to_string());
            (
                format!(
                    "SUM(CASE WHEN v.side = 'I' AND {col} IS NOT NULL THEN 1 ELSE 0 END)::bigint"
                ),
                format!(
                    "SUM(CASE WHEN v.side = 'D' AND {col} IS NOT NULL THEN 1 ELSE 0 END)::bigint"
                ),
            )
        }
        AggFunc::Sum => {
            let col = agg
                .argument
                .as_ref()
                .map(|e| format!("v.{}", quote_ident(&format!("val_{}", e.output_name()))))
                .unwrap_or_else(|| "0".to_string());
            (
                format!("SUM(CASE WHEN v.side = 'I' THEN {col} ELSE 0 END)"),
                format!("SUM(CASE WHEN v.side = 'D' THEN {col} ELSE 0 END)"),
            )
        }
        AggFunc::Min | AggFunc::Max => {
            // Should never be reached — eligibility check excludes MIN/MAX
            unreachable!("P5 bypass does not support MIN/MAX aggregates")
        }
        _ => {
            // Group-rescan aggregates should also never reach P5 bypass
            unreachable!("P5 bypass does not support group-rescan aggregates")
        }
    }
}

/// Differentiate an Aggregate node.
pub fn diff_aggregate(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
    let OpTree::Aggregate {
        group_by,
        aggregates,
        child,
    } = op
    else {
        return Err(PgTrickleError::InternalError(
            "diff_aggregate called on non-Aggregate node".into(),
        ));
    };

    // ── CTE 1: Choose between P5 direct bypass or standard path ────────
    //
    // P5: For Scan → Aggregate trees where all aggregates are decomposable
    // (SUM/COUNT/AVG), extract only the needed group-by keys and aggregate
    // argument columns directly from the change buffer via JSONB '->>'
    // instead of deserializing ALL columns with jsonb_populate_record.
    let (delta_cte, group_output) = if is_direct_agg_eligible(child, group_by, aggregates) {
        generate_direct_agg_delta(ctx, child, group_by, aggregates)?
    } else {
        // ── Standard path: differentiate child first ───────────────────
        let child_result = ctx.diff_node(child)?;

        // Resolve group-by expressions against child CTE's column names.
        let child_cols = &child_result.columns;
        let group_resolved: Vec<String> = group_by
            .iter()
            .map(|e| resolve_group_col(e, child_cols))
            .collect();
        let group_output: Vec<String> = group_by.iter().map(|e| e.output_name()).collect();

        let group_by_clause = if group_resolved.is_empty() {
            String::new()
        } else {
            let gb_cols: Vec<String> = group_resolved.iter().map(|c| quote_ident(c)).collect();
            format!("\nGROUP BY {}", gb_cols.join(", "))
        };

        let delta_cte = ctx.next_cte_name("agg_delta");
        let mut delta_selects = Vec::new();

        // Group by columns — alias to output name for consistent downstream refs
        for (resolved, output) in group_resolved.iter().zip(group_output.iter()) {
            if resolved == output {
                delta_selects.push(quote_ident(resolved));
            } else {
                delta_selects.push(format!(
                    "{} AS {}",
                    quote_ident(resolved),
                    quote_ident(output)
                ));
            }
        }

        // Always track insert/delete counts
        delta_selects
            .push("SUM(CASE WHEN __pgt_action = 'I' THEN 1 ELSE 0 END) AS __ins_count".to_string());
        delta_selects
            .push("SUM(CASE WHEN __pgt_action = 'D' THEN 1 ELSE 0 END) AS __del_count".to_string());

        // Per-aggregate tracking
        for agg in aggregates {
            let (ins_expr, del_expr) = agg_delta_exprs(agg, child_cols);
            let alias_i = format!("__ins_{}", agg.alias);
            let alias_d = format!("__del_{}", agg.alias);
            delta_selects.push(format!("{ins_expr} AS {}", quote_ident(&alias_i)));
            delta_selects.push(format!("{del_expr} AS {}", quote_ident(&alias_d)));
        }

        let delta_sql = format!(
            "SELECT {selects}\nFROM {child_cte}{group_by}",
            selects = delta_selects.join(",\n       "),
            child_cte = child_result.cte_name,
            group_by = group_by_clause,
        );
        ctx.add_cte(delta_cte.clone(), delta_sql);

        (delta_cte, group_output)
    };

    // ── Detect intermediate aggregate ───────────────────────────────
    //
    // An intermediate aggregate is one whose output columns do NOT exist
    // in the stream table (e.g., an inner GROUP BY in a subquery-in-FROM,
    // or a global aggregate like MAX inside a scalar subquery).
    // For such aggregates, we cannot LEFT JOIN to the stream table because
    // the ST doesn't have the intermediate columns.  Instead, we build an
    // "old snapshot" CTE by re-aggregating the child's old data (current
    // data minus child delta inserts, plus child delta deletes).
    let is_intermediate = if let Some(ref st_cols) = ctx.st_user_columns {
        if !group_output.is_empty() {
            // Grouped aggregate: check if any group column is missing from ST
            group_output.iter().any(|g| !st_cols.contains(g))
        } else if !aggregates.is_empty() {
            // Global aggregate (no GROUP BY): check if aggregate output
            // columns exist in the stream table. If not, this is an
            // intermediate aggregate (e.g., MAX inside a scalar subquery).
            aggregates.iter().any(|a| !st_cols.contains(&a.alias))
        } else {
            false
        }
    } else {
        false
    };

    if is_intermediate {
        return build_intermediate_agg_delta(
            ctx,
            child,
            group_by,
            &group_output,
            aggregates,
            &delta_cte,
        );
    }

    // ── Rescan CTE: re-aggregate affected groups for group-rescan aggs ──
    let rescan_cte = build_rescan_cte(ctx, child, group_by, &group_output, aggregates, &delta_cte);
    let has_rescan = rescan_cte.is_some();

    // ── CTE 2: Merge with existing ST state to classify actions ────────
    let merge_cte = ctx.next_cte_name("agg_merge");

    let st_table = ctx.st_qualified_name.as_deref().unwrap_or("/* st_table */");

    // Row ID from group-by columns (using output names)
    let group_hash_exprs: Vec<String> = group_output
        .iter()
        .map(|c| format!("d.{}::TEXT", quote_ident(c)))
        .collect();
    let row_id_expr = if group_hash_exprs.is_empty() {
        "pgtrickle.pg_trickle_hash('__singleton_group')".to_string()
    } else {
        build_hash_expr(&group_hash_exprs)
    };

    let mut merge_selects = Vec::new();
    merge_selects.push(format!("{row_id_expr} AS __pgt_row_id"));

    // Group columns
    for col in &group_output {
        merge_selects.push(format!("d.{}", quote_ident(col)));
    }

    // New count = old + inserts - deletes
    // COALESCE guards: for global aggregates (no GROUP BY) the agg_delta
    // CTE always produces exactly one row.  When **all** child-delta rows
    // are filtered out the SUM(CASE …) expressions evaluate over an empty
    // set and return NULL, not 0.  Wrapping in COALESCE(…, 0) keeps the
    // arithmetic safe.
    merge_selects.push(
        "COALESCE(st.__pgt_count, 0) + COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0) AS new_count".to_string(),
    );
    merge_selects.push("COALESCE(st.__pgt_count, 0) AS old_count".to_string());

    // Per-aggregate new values + old values for G-S1 change detection
    for agg in aggregates {
        let new_val_expr = agg_merge_expr(agg, has_rescan);
        merge_selects.push(format!(
            "{new_val_expr} AS {}",
            quote_ident(&format!("new_{}", agg.alias)),
        ));
        // Keep old value alongside so the final CTE can skip unchanged U rows.
        // st.{alias} is NULL for brand-new groups (LEFT JOIN miss), which is
        // correct: IS DISTINCT FROM will see old=NULL vs new=<value> → changed.
        merge_selects.push(format!(
            "st.{} AS {}",
            quote_ident(&agg.alias),
            quote_ident(&format!("old_{}", agg.alias)),
        ));
    }

    // ── Scalar-aggregate guard ───────────────────────────────────
    //
    // PostgreSQL scalar aggregates (no GROUP BY) always return exactly
    // one row: `SELECT SUM(x) FROM empty_table` → 1 row (NULL).  The
    // singleton ST row must **never** be deleted, so we omit the 'D'
    // classification for scalar aggregates and emit 'U' instead.
    let is_scalar_agg = group_by.is_empty();

    // Action classification (same COALESCE guards as new_count)
    let action_case = if is_scalar_agg {
        "\
CASE
    WHEN st.__pgt_count IS NULL AND (COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0)) > 0 THEN 'I'
    ELSE 'U'
END AS __pgt_meta_action"
            .to_string()
    } else {
        "\
CASE
    WHEN st.__pgt_count IS NULL AND (COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0)) > 0 THEN 'I'
    WHEN COALESCE(st.__pgt_count, 0) + COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0) <= 0 THEN 'D'
    ELSE 'U'
END AS __pgt_meta_action"
            .to_string()
    };
    merge_selects.push(action_case);

    // Join condition on group-by columns
    let join_cond = if group_output.is_empty() {
        "TRUE".to_string()
    } else {
        group_output
            .iter()
            .map(|c| format!("st.{qc} = d.{qc}", qc = quote_ident(c)))
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    // Optional LEFT JOIN to the rescan CTE for group-rescan aggregates
    let rescan_join = if let Some(ref rc) = rescan_cte {
        let rescan_join_cond = if group_output.is_empty() {
            "TRUE".to_string()
        } else {
            group_output
                .iter()
                .map(|c| format!("r.{qc} = d.{qc}", qc = quote_ident(c)))
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        format!("\nLEFT JOIN {rc} r ON {rescan_join_cond}")
    } else {
        String::new()
    };

    let merge_sql = format!(
        "SELECT {selects}\nFROM {delta_cte} d\nLEFT JOIN {st_table} st ON {join_cond}{rescan_join}",
        selects = merge_selects.join(",\n       "),
    );
    ctx.add_cte(merge_cte.clone(), merge_sql);

    // ── CTE 3: Single-row-per-group emit ────────────────────────────
    //
    // Each group produces exactly ONE delta row. MERGE handles all
    // three cases correctly with a single action per __pgt_row_id:
    //
    //   I row  → action='I', new values  → NOT MATCHED → INSERT
    //   D row  → action='D', old values  → MATCHED     → DELETE
    //   U row  → action='I', new values  → MATCHED     → UPDATE SET
    //
    // Because the output is already 1-row-per-group-key, the downstream
    // DISTINCT ON sort in the MERGE USING clause can be skipped entirely
    // (is_deduplicated = true).
    let final_cte = ctx.next_cte_name("agg_final");

    // Build output column list
    let mut output_cols = Vec::new();
    output_cols.extend(group_output.iter().cloned());
    output_cols.push("__pgt_count".to_string());
    for agg in aggregates {
        output_cols.push(agg.alias.clone());
    }

    let group_col_refs = group_output
        .iter()
        .map(|c| format!("m.{}", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");

    let extra_group = if group_col_refs.is_empty() {
        String::new()
    } else {
        format!("{group_col_refs}, ")
    };

    // Build CASE expressions: D → old values, I/U → new values
    let count_case = "CASE WHEN m.__pgt_meta_action = 'D' THEN m.old_count ELSE m.new_count END";

    let mut agg_cases: Vec<String> = Vec::new();
    for agg in aggregates {
        let new_col = quote_ident(&format!("new_{}", agg.alias));
        let old_col = quote_ident(&format!("old_{}", agg.alias));
        // For scalar aggregates, SUM (and similar nullable aggs) must return
        // NULL — not 0 — when new_count drops to 0, matching PostgreSQL's
        // `SELECT SUM(x) FROM empty_table` → NULL semantics.  COUNT(*) and
        // COUNT(col) correctly yield 0 from the count arithmetic, so they
        // don't need this override.
        let needs_null_on_empty =
            is_scalar_agg && matches!(agg.function, AggFunc::Sum | AggFunc::Min | AggFunc::Max);
        if needs_null_on_empty {
            agg_cases.push(format!(
                "CASE WHEN m.__pgt_meta_action = 'D' THEN m.{old_col} \
                 WHEN m.new_count <= 0 THEN NULL \
                 ELSE m.{new_col} END AS {}",
                quote_ident(&agg.alias),
            ));
        } else {
            agg_cases.push(format!(
                "CASE WHEN m.__pgt_meta_action = 'D' THEN m.{old_col} ELSE m.{new_col} END AS {}",
                quote_ident(&agg.alias),
            ));
        }
    }

    let extra_agg_cases = if agg_cases.is_empty() {
        String::new()
    } else {
        format!(",\n       {}", agg_cases.join(",\n       "))
    };

    // ── G-S1: Build change-detection guard for UPDATE rows ──────────
    //
    // When a source row changes a non-aggregated column, the group count
    // and all aggregate values stay identical — emitting a row would
    // be a no-op MERGE. Skip U rows entirely when nothing changed.
    //
    // Guard: (new_count IS DISTINCT FROM old_count)
    //     OR (new_agg1 IS DISTINCT FROM old_agg1)
    //     OR ...
    //
    // For IS DISTINCT FROM, NULL vs NULL = same, NULL vs value = different.
    // This is correct for brand-new groups (old = NULL, new = <value>).
    let mut change_checks = vec!["m.new_count IS DISTINCT FROM m.old_count".to_string()];
    for agg in aggregates {
        change_checks.push(format!(
            "m.{new} IS DISTINCT FROM m.{old}",
            new = quote_ident(&format!("new_{}", agg.alias)),
            old = quote_ident(&format!("old_{}", agg.alias)),
        ));
    }
    let change_guard = change_checks.join("\n          OR ");

    let final_sql = format!(
        "\
SELECT m.__pgt_row_id,
       CASE WHEN m.__pgt_meta_action = 'D' THEN 'D' ELSE 'I' END AS __pgt_action,
       {extra_group}{count_case} AS __pgt_count{extra_agg_cases}
FROM {merge_cte} m
WHERE m.__pgt_meta_action IN ('I', 'D')
   OR (m.__pgt_meta_action = 'U'
       AND ({change_guard}))",
    );

    ctx.add_cte(final_cte.clone(), final_sql);

    Ok(DiffResult {
        cte_name: final_cte,
        columns: output_cols,
        is_deduplicated: true,
    })
}

/// Generate SUM expressions for INSERT/DELETE tracking in the delta CTE.
///
/// Aggregate arguments are stripped of table qualifiers because the child
/// CTE (e.g. join delta) outputs unqualified column names.
///
/// When a FILTER clause is present, its condition is added as an additional
/// AND guard in all CASE WHEN expressions, so only rows passing the filter
/// contribute to the aggregate delta.
fn agg_delta_exprs(agg: &AggExpr, child_cols: &[String]) -> (String, String) {
    let filter_sql = agg
        .filter
        .as_ref()
        .map(|f| resolve_expr_for_child(f, child_cols));
    let filter_and = filter_sql
        .as_ref()
        .map(|f| format!(" AND {f}"))
        .unwrap_or_default();

    match &agg.function {
        AggFunc::CountStar => (
            format!("SUM(CASE WHEN __pgt_action = 'I'{filter_and} THEN 1 ELSE 0 END)"),
            format!("SUM(CASE WHEN __pgt_action = 'D'{filter_and} THEN 1 ELSE 0 END)"),
        ),
        AggFunc::Count => {
            let col = agg
                .argument
                .as_ref()
                .map(|e| resolve_expr_for_child(e, child_cols))
                .unwrap_or("*".into());
            (
                format!(
                    "SUM(CASE WHEN __pgt_action = 'I' AND {col} IS NOT NULL{filter_and} THEN 1 ELSE 0 END)"
                ),
                format!(
                    "SUM(CASE WHEN __pgt_action = 'D' AND {col} IS NOT NULL{filter_and} THEN 1 ELSE 0 END)"
                ),
            )
        }
        AggFunc::Sum => {
            let col = agg
                .argument
                .as_ref()
                .map(|e| resolve_expr_for_child(e, child_cols))
                .unwrap_or("0".into());
            (
                format!("SUM(CASE WHEN __pgt_action = 'I'{filter_and} THEN {col} ELSE 0 END)"),
                format!("SUM(CASE WHEN __pgt_action = 'D'{filter_and} THEN {col} ELSE 0 END)"),
            )
        }
        AggFunc::Min | AggFunc::Max => {
            let col = agg
                .argument
                .as_ref()
                .map(|e| resolve_expr_for_child(e, child_cols))
                .unwrap_or("NULL".into());
            let func = agg.function.sql_name();
            (
                format!("{func}(CASE WHEN __pgt_action = 'I'{filter_and} THEN {col} END)"),
                format!("{func}(CASE WHEN __pgt_action = 'D'{filter_and} THEN {col} END)"),
            )
        }
        // Group-rescan aggregates: track insertions/deletions as simple counts.
        // Any change to a group triggers a NULL sentinel in the merge, causing
        // the MERGE layer to re-aggregate the entire group.
        _ if agg.function.is_group_rescan() => {
            let col = agg
                .argument
                .as_ref()
                .map(|e| resolve_expr_for_child(e, child_cols))
                .unwrap_or("1".into());
            // We only need to detect "any change happened" — counting suffices.
            (
                format!(
                    "SUM(CASE WHEN __pgt_action = 'I'{filter_and} AND {col} IS NOT NULL THEN 1 ELSE 0 END)"
                ),
                format!(
                    "SUM(CASE WHEN __pgt_action = 'D'{filter_and} AND {col} IS NOT NULL THEN 1 ELSE 0 END)"
                ),
            )
        }
        _ => unreachable!("unexpected AggFunc variant in agg_delta_exprs"),
    }
}

/// Generate the merge expression for computing the new aggregate value.
///
/// When `has_rescan` is true, group-rescan aggregates reference the rescan
/// CTE (`r.{alias}`) instead of producing a NULL sentinel. This correctly
/// re-aggregates the affected group from source data.
fn agg_merge_expr(agg: &AggExpr, has_rescan: bool) -> String {
    let alias = &agg.alias;
    let qt = quote_ident(alias);
    match &agg.function {
        AggFunc::CountStar | AggFunc::Count => {
            format!(
                "COALESCE(st.{qt}, 0) + COALESCE(d.{ins}, 0) - COALESCE(d.{del}, 0)",
                ins = quote_ident(&format!("__ins_{alias}")),
                del = quote_ident(&format!("__del_{alias}")),
            )
        }
        AggFunc::Sum => {
            format!(
                "COALESCE(st.{qt}, 0) + COALESCE(d.{ins}, 0) - COALESCE(d.{del}, 0)",
                ins = quote_ident(&format!("__ins_{alias}")),
                del = quote_ident(&format!("__del_{alias}")),
            )
        }
        AggFunc::Min | AggFunc::Max => {
            // MIN/MAX merge with group-rescan fallback.
            //
            // Case 1: The old extremum was NOT deleted (or there was no old value).
            //   → Use LEAST/GREATEST(old_value, new_inserts) = simple algebraic merge.
            //
            // Case 2: The old extremum WAS deleted.
            //   → The new extremum might be entirely different. When a rescan CTE
            //     is available (has_rescan=true), use the rescanned value from
            //     source data. Otherwise fall back to just the insert extremum
            //     (which may be NULL if there were no inserts).
            //
            // The "was deleted" check: d.__del_{alias} IS NOT NULL AND
            //   d.__del_{alias} = st.{alias} (the deleted extremum equals the stored one).
            let func = if matches!(agg.function, AggFunc::Min) {
                "LEAST"
            } else {
                "GREATEST"
            };
            let ins = quote_ident(&format!("__ins_{alias}"));
            let del = quote_ident(&format!("__del_{alias}"));
            if has_rescan {
                format!(
                    "CASE WHEN d.{del} IS NOT NULL AND d.{del} = st.{qt} \
                     THEN r.{qt} \
                     ELSE {func}(st.{qt}, d.{ins}) END"
                )
            } else {
                format!(
                    "CASE WHEN d.{del} IS NOT NULL AND d.{del} = st.{qt} \
                     THEN d.{ins} \
                     ELSE {func}(st.{qt}, d.{ins}) END"
                )
            }
        }
        // Group-rescan aggregates: use rescan CTE value when available,
        // or fall back to NULL sentinel.
        //
        // With rescan CTE: when any row in the group changed (ins or del > 0),
        // use the re-aggregated value from the rescan CTE (`r.{alias}`).
        //
        // Without rescan CTE (legacy fallback): return NULL sentinel.
        _ if agg.function.is_group_rescan() => {
            let ins = quote_ident(&format!("__ins_{alias}"));
            let del = quote_ident(&format!("__del_{alias}"));
            if has_rescan {
                format!(
                    "CASE WHEN COALESCE(d.{ins}, 0) > 0 OR COALESCE(d.{del}, 0) > 0 \
                     THEN r.{qt} \
                     ELSE st.{qt} END"
                )
            } else {
                format!(
                    "CASE WHEN COALESCE(d.{ins}, 0) > 0 OR COALESCE(d.{del}, 0) > 0 \
                     THEN NULL \
                     ELSE st.{qt} END"
                )
            }
        }
        _ => unreachable!("unexpected AggFunc variant in agg_merge_expr"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    // ── is_direct_agg_eligible tests ────────────────────────────────

    #[test]
    fn test_eligible_scan_count_star() {
        let child = scan(1, "t", "public", "t", &["id", "region"]);
        let group_by = vec![colref("region")];
        let aggs = vec![count_star("cnt")];
        assert!(is_direct_agg_eligible(&child, &group_by, &aggs));
    }

    #[test]
    fn test_eligible_scan_sum_and_count() {
        let child = scan(1, "t", "public", "t", &["id", "region", "amount"]);
        let group_by = vec![colref("region")];
        let aggs = vec![sum_col("amount", "total"), count_star("cnt")];
        assert!(is_direct_agg_eligible(&child, &group_by, &aggs));
    }

    #[test]
    fn test_ineligible_non_scan_child() {
        let child = filter(
            binop(">", colref("id"), lit("0")),
            scan(1, "t", "public", "t", &["id", "region"]),
        );
        let group_by = vec![colref("region")];
        let aggs = vec![count_star("cnt")];
        assert!(!is_direct_agg_eligible(&child, &group_by, &aggs));
    }

    #[test]
    fn test_ineligible_min_max() {
        let child = scan(1, "t", "public", "t", &["id", "region", "amount"]);
        let group_by = vec![colref("region")];
        let aggs = vec![min_col("amount", "min_amt")];
        assert!(!is_direct_agg_eligible(&child, &group_by, &aggs));
    }

    #[test]
    fn test_ineligible_distinct_aggregate() {
        let child = scan(1, "t", "public", "t", &["id", "region"]);
        let group_by = vec![colref("region")];
        let aggs = vec![AggExpr {
            function: AggFunc::Count,
            argument: Some(colref("id")),
            alias: "cnt".to_string(),
            is_distinct: true,
            filter: None,
            second_arg: None,
            order_within_group: None,
        }];
        assert!(!is_direct_agg_eligible(&child, &group_by, &aggs));
    }

    #[test]
    fn test_eligible_no_group_by() {
        let child = scan(1, "t", "public", "t", &["amount"]);
        let group_by = vec![];
        let aggs = vec![sum_col("amount", "total")];
        assert!(is_direct_agg_eligible(&child, &group_by, &aggs));
    }

    // ── diff_aggregate integration tests ────────────────────────────

    #[test]
    fn test_diff_aggregate_count_star_with_group_by() {
        let mut ctx = test_ctx_with_st("public", "my_st");
        let child = scan(1, "orders", "public", "o", &["id", "region", "amount"]);
        let tree = aggregate(vec![colref("region")], vec![count_star("cnt")], child);
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Output: group cols + __pgt_count + aggregate aliases
        assert!(result.columns.contains(&"region".to_string()));
        assert!(result.columns.contains(&"__pgt_count".to_string()));
        assert!(result.columns.contains(&"cnt".to_string()));

        // Should use direct bypass (P5) since it's Scan → Aggregate with COUNT(*)
        assert_sql_contains(&sql, "changes_1");
        assert_sql_contains(&sql, "LATERAL");
    }

    #[test]
    fn test_diff_aggregate_sum_with_group_by() {
        let mut ctx = test_ctx_with_st("public", "my_st");
        let child = scan(1, "orders", "public", "o", &["id", "region", "amount"]);
        let tree = aggregate(
            vec![colref("region")],
            vec![sum_col("amount", "total")],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert!(result.columns.contains(&"total".to_string()));
        // Uses P5 direct bypass
        assert_sql_contains(&sql, "LATERAL");
    }

    #[test]
    fn test_diff_aggregate_no_group_by() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["amount"]);
        let tree = aggregate(
            vec![],
            vec![sum_col("amount", "total"), count_star("cnt")],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // No GROUP BY → singleton group hash
        assert_sql_contains(&sql, "__singleton_group");
        assert!(result.is_deduplicated);
    }

    #[test]
    fn test_diff_aggregate_standard_path_when_child_is_filter() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = filter(
            binop(">", colref("amount"), lit("0")),
            scan(1, "t", "public", "t", &["id", "region", "amount"]),
        );
        let tree = aggregate(vec![colref("region")], vec![count_star("cnt")], child);
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should NOT use P5 direct bypass when child is not a direct Scan
        assert_sql_not_contains(&sql, "LATERAL");
        // Should use standard path with __pgt_action
        assert_sql_contains(&sql, "__pgt_action");
    }

    #[test]
    fn test_diff_aggregate_is_deduplicated() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "region"]);
        let tree = aggregate(vec![colref("region")], vec![count_star("cnt")], child);
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        // Aggregates produce exactly one row per group → deduplicated
        assert!(result.is_deduplicated);
    }

    #[test]
    fn test_diff_aggregate_error_on_non_aggregate_node() {
        let mut ctx = test_ctx_with_st("public", "st");
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_aggregate(&mut ctx, &tree);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_aggregate_change_detection_guard() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["region", "amount"]);
        let tree = aggregate(
            vec![colref("region")],
            vec![sum_col("amount", "total")],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // G-S1: change detection guard should be present
        assert_sql_contains(&sql, "IS DISTINCT FROM");
    }

    // ── agg_delta_exprs tests ───────────────────────────────────────

    #[test]
    fn test_agg_delta_exprs_count_star() {
        let agg = count_star("cnt");
        let (ins, del) = agg_delta_exprs(&agg, &[]);
        assert!(ins.contains("'I'"));
        assert!(del.contains("'D'"));
    }

    #[test]
    fn test_agg_delta_exprs_sum() {
        let agg = sum_col("amount", "total");
        let child_cols = vec!["amount".to_string()];
        let (ins, del) = agg_delta_exprs(&agg, &child_cols);
        assert!(ins.contains("amount"));
        assert!(del.contains("amount"));
    }

    #[test]
    fn test_agg_delta_exprs_count_col() {
        let agg = count_col("name", "name_count");
        let child_cols = vec!["name".to_string()];
        let (ins, del) = agg_delta_exprs(&agg, &child_cols);
        assert!(ins.contains("IS NOT NULL"));
        assert!(del.contains("IS NOT NULL"));
    }

    // ── agg_merge_expr tests ────────────────────────────────────────

    #[test]
    fn test_agg_merge_expr_count_star() {
        let agg = count_star("cnt");
        let result = agg_merge_expr(&agg, false);
        assert!(result.contains("COALESCE(st.\"cnt\", 0)"));
        assert!(result.contains("__ins_cnt"));
        assert!(result.contains("__del_cnt"));
    }

    #[test]
    fn test_agg_merge_expr_sum() {
        let agg = sum_col("amount", "total");
        let result = agg_merge_expr(&agg, false);
        assert!(result.contains("COALESCE(st.\"total\", 0)"));
        assert!(result.contains("COALESCE(d.\"__ins_total\", 0)"));
    }

    #[test]
    fn test_agg_merge_expr_avg() {
        // AVG uses group-rescan: merge expression should use NULL sentinel
        // (no rescan CTE available in this test)
        let agg = avg_col("score", "avg_score");
        let result = agg_merge_expr(&agg, false);
        assert!(
            result.contains("THEN NULL"),
            "AVG without rescan should use NULL sentinel: {result}"
        );
    }

    #[test]
    fn test_agg_merge_expr_avg_with_rescan() {
        // AVG uses group-rescan: with rescan CTE, should reference r.{alias}
        let agg = avg_col("score", "avg_score");
        let result = agg_merge_expr(&agg, true);
        assert!(
            result.contains("r."),
            "AVG with rescan should reference rescan CTE: {result}"
        );
    }

    // ── MIN/MAX merge expression tests ──────────────────────────────

    #[test]
    fn test_agg_merge_expr_min() {
        let agg = AggExpr {
            function: AggFunc::Min,
            argument: Some(colref("val")),
            alias: "min_val".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        };
        let result = agg_merge_expr(&agg, false);
        // Should use LEAST for MIN
        assert!(
            result.contains("LEAST"),
            "MIN merge should use LEAST: {result}"
        );
        // Should check if deleted extremum matches stored value
        assert!(
            result.contains("__del_min_val"),
            "should reference __del_min_val: {result}"
        );
        assert!(
            result.contains("__ins_min_val"),
            "should reference __ins_min_val: {result}"
        );
        // Should have a CASE expression
        assert!(
            result.contains("CASE WHEN"),
            "should use CASE WHEN: {result}"
        );
    }

    #[test]
    fn test_agg_merge_expr_max() {
        let agg = AggExpr {
            function: AggFunc::Max,
            argument: Some(colref("val")),
            alias: "max_val".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        };
        let result = agg_merge_expr(&agg, false);
        // Should use GREATEST for MAX
        assert!(
            result.contains("GREATEST"),
            "MAX merge should use GREATEST: {result}"
        );
        assert!(
            result.contains("__del_max_val"),
            "should reference __del_max_val: {result}"
        );
        assert!(
            result.contains("__ins_max_val"),
            "should reference __ins_max_val: {result}"
        );
    }

    // ── MIN/MAX delta expression tests ──────────────────────────────

    #[test]
    fn test_agg_delta_exprs_min() {
        let agg = AggExpr {
            function: AggFunc::Min,
            argument: Some(colref("val")),
            alias: "min_val".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        };
        let child_cols = vec!["val".to_string()];
        let (ins, del) = agg_delta_exprs(&agg, &child_cols);
        // MIN of inserted values
        assert!(
            ins.contains("MIN") && ins.contains("'I'"),
            "MIN delta ins should use MIN: {ins}"
        );
        // MIN of deleted values
        assert!(
            del.contains("MIN") && del.contains("'D'"),
            "MIN delta del should use MIN: {del}"
        );
    }

    #[test]
    fn test_agg_delta_exprs_max() {
        let agg = AggExpr {
            function: AggFunc::Max,
            argument: Some(colref("val")),
            alias: "max_val".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        };
        let child_cols = vec!["val".to_string()];
        let (ins, del) = agg_delta_exprs(&agg, &child_cols);
        // MAX of inserted values
        assert!(
            ins.contains("MAX") && ins.contains("'I'"),
            "MAX delta ins should use MAX: {ins}"
        );
        // MAX of deleted values
        assert!(
            del.contains("MAX") && del.contains("'D'"),
            "MAX delta del should use MAX: {del}"
        );
    }

    // ── MIN/MAX diff_aggregate integration tests ────────────────────

    #[test]
    fn test_diff_aggregate_min_with_group_by() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![AggExpr {
                function: AggFunc::Min,
                argument: Some(colref("salary")),
                alias: "min_salary".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            }],
            child: Box::new(scan(1, "employees", "public", "e", &["dept", "salary"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "MIN aggregate should diff successfully: {result:?}"
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("LEAST") || sql.contains("MIN"),
            "MIN aggregate diff should reference LEAST or MIN: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_max_with_group_by() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![AggExpr {
                function: AggFunc::Max,
                argument: Some(colref("salary")),
                alias: "max_salary".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            }],
            child: Box::new(scan(1, "employees", "public", "e", &["dept", "salary"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "MAX aggregate should diff successfully: {result:?}"
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("GREATEST") || sql.contains("MAX"),
            "MAX aggregate diff should reference GREATEST or MAX: {sql}",
        );
    }

    // ── B5: FILTER clause tests ──────────────────────────────────────

    #[test]
    fn test_agg_delta_exprs_count_star_with_filter() {
        let agg = with_filter(
            count_star("cnt"),
            binop("=", colref("status"), lit("'active'")),
        );
        let (ins, del) = agg_delta_exprs(&agg, &[]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "Insert expr should check action: {ins}",
        );
        assert!(
            ins.contains("(status = 'active')"),
            "Insert expr should contain filter: {ins}",
        );
        assert!(
            del.contains("__pgt_action = 'D'"),
            "Delete expr should check action: {del}",
        );
        assert!(
            del.contains("(status = 'active')"),
            "Delete expr should contain filter: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_sum_with_filter() {
        let agg = with_filter(
            sum_col("amount", "total"),
            binop(">", colref("amount"), lit("0")),
        );
        let (ins, _del) = agg_delta_exprs(&agg, &["amount".to_string()]);
        assert!(
            ins.contains("(amount > 0)"),
            "Insert expr should contain filter: {ins}",
        );
        assert!(
            ins.contains("THEN amount"),
            "Insert expr should reference amount column: {ins}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_count_no_filter() {
        let agg = count_star("cnt");
        let (ins, del) = agg_delta_exprs(&agg, &[]);
        assert!(
            !ins.contains("AND"),
            "Without filter, no extra AND should appear: {ins}",
        );
        assert!(
            !del.contains("AND"),
            "Without filter, no extra AND should appear: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_min_with_filter() {
        let agg = with_filter(
            min_col("val", "min_val"),
            binop("=", colref("active"), lit("true")),
        );
        let (ins, _del) = agg_delta_exprs(&agg, &["val".to_string(), "active".to_string()]);
        assert!(
            ins.contains("(active = true)"),
            "MIN insert delta should contain filter: {ins}",
        );
        assert!(ins.contains("MIN("), "Should use MIN function: {ins}");
    }

    #[test]
    fn test_diff_aggregate_with_filter() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![with_filter(
                count_star("active_cnt"),
                binop("=", colref("status"), lit("'active'")),
            )],
            child: Box::new(scan(1, "employees", "public", "e", &["dept", "status"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "FILTER aggregate should diff: {result:?}");
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("(status = 'active')"),
            "Generated SQL should contain FILTER condition: {sql}",
        );
    }

    #[test]
    fn test_filter_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "region", "status"]);
        let group_by = vec![colref("region")];
        let aggs = vec![with_filter(
            count_star("cnt"),
            binop("=", colref("status"), lit("'active'")),
        )];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "Filtered aggregates should not be eligible for P5 bypass",
        );
    }

    // ── B4: Group-rescan aggregate tests ─────────────────────────────

    #[test]
    fn test_is_group_rescan() {
        assert!(!AggFunc::Count.is_group_rescan());
        assert!(!AggFunc::Sum.is_group_rescan());
        assert!(AggFunc::Avg.is_group_rescan());
        assert!(!AggFunc::Min.is_group_rescan());
        assert!(!AggFunc::Max.is_group_rescan());
        assert!(AggFunc::BoolAnd.is_group_rescan());
        assert!(AggFunc::BoolOr.is_group_rescan());
        assert!(AggFunc::StringAgg.is_group_rescan());
        assert!(AggFunc::ArrayAgg.is_group_rescan());
        assert!(AggFunc::JsonAgg.is_group_rescan());
        assert!(AggFunc::JsonbAgg.is_group_rescan());
        assert!(AggFunc::BitAnd.is_group_rescan());
        assert!(AggFunc::BitOr.is_group_rescan());
        assert!(AggFunc::BitXor.is_group_rescan());
        assert!(AggFunc::JsonObjectAgg.is_group_rescan());
        assert!(AggFunc::JsonbObjectAgg.is_group_rescan());
        assert!(AggFunc::JsonObjectAggStd("JSON_OBJECTAGG(k : v)".into()).is_group_rescan());
        assert!(AggFunc::JsonArrayAggStd("JSON_ARRAYAGG(x)".into()).is_group_rescan());
        assert!(AggFunc::StddevPop.is_group_rescan());
        assert!(AggFunc::StddevSamp.is_group_rescan());
        assert!(AggFunc::VarPop.is_group_rescan());
        assert!(AggFunc::VarSamp.is_group_rescan());
        assert!(AggFunc::Mode.is_group_rescan());
        assert!(AggFunc::PercentileCont.is_group_rescan());
        assert!(AggFunc::PercentileDisc.is_group_rescan());
    }

    #[test]
    fn test_agg_to_rescan_sql_json_objectagg_std() {
        let agg = AggExpr {
            function: AggFunc::JsonObjectAggStd(
                "JSON_OBJECTAGG(name : value ABSENT ON NULL)".into(),
            ),
            argument: None,
            alias: "obj".to_string(),
            is_distinct: false,
            second_arg: None,
            filter: None,
            order_within_group: None,
        };
        assert_eq!(
            agg_to_rescan_sql(&agg),
            "JSON_OBJECTAGG(name : value ABSENT ON NULL)"
        );
    }

    #[test]
    fn test_agg_to_rescan_sql_json_arrayagg_std() {
        let agg = AggExpr {
            function: AggFunc::JsonArrayAggStd("JSON_ARRAYAGG(x ORDER BY x)".into()),
            argument: None,
            alias: "arr".to_string(),
            is_distinct: false,
            second_arg: None,
            filter: None,
            order_within_group: None,
        };
        assert_eq!(agg_to_rescan_sql(&agg), "JSON_ARRAYAGG(x ORDER BY x)");
    }

    #[test]
    fn test_agg_to_rescan_sql_json_arrayagg_std_with_filter() {
        let agg = AggExpr {
            function: AggFunc::JsonArrayAggStd("JSON_ARRAYAGG(x)".into()),
            argument: None,
            alias: "arr".to_string(),
            is_distinct: false,
            second_arg: None,
            filter: Some(Expr::Raw("x > 0".into())),
            order_within_group: None,
        };
        assert_eq!(
            agg_to_rescan_sql(&agg),
            "JSON_ARRAYAGG(x) FILTER (WHERE x > 0)"
        );
    }

    #[test]
    fn test_agg_delta_exprs_bool_and() {
        let agg = bool_and_col("active", "all_active");
        let (ins, _del) = agg_delta_exprs(&agg, &["active".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "BOOL_AND insert delta should check action: {ins}",
        );
        assert!(
            ins.contains("active IS NOT NULL"),
            "BOOL_AND insert delta should check NOT NULL: {ins}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_bool_or() {
        let agg = bool_or_col("active", "any_active");
        let (_ins, del) = agg_delta_exprs(&agg, &["active".to_string()]);
        assert!(
            del.contains("__pgt_action = 'D'"),
            "BOOL_OR delete delta should check action: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_string_agg() {
        let agg = string_agg_col("name", "', '", "members");
        let (ins, _del) = agg_delta_exprs(&agg, &["name".to_string()]);
        assert!(
            ins.contains("name IS NOT NULL"),
            "STRING_AGG insert delta should check NOT NULL: {ins}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_array_agg() {
        let agg = array_agg_col("val", "vals");
        let (ins, _del) = agg_delta_exprs(&agg, &["val".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "ARRAY_AGG insert delta should check action: {ins}",
        );
    }

    #[test]
    fn test_agg_merge_expr_bool_and_rescan() {
        let agg = bool_and_col("active", "all_active");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "BOOL_AND merge should return NULL sentinel on change: {merge}",
        );
        assert!(
            merge.contains("d.\"__ins_all_active\""),
            "BOOL_AND merge should reference ins counter: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_string_agg_rescan() {
        let agg = string_agg_col("name", "', '", "members");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "STRING_AGG merge should return NULL sentinel: {merge}",
        );
        assert!(
            merge.contains("st.\"members\""),
            "STRING_AGG merge should reference old value: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_array_agg_rescan() {
        let agg = array_agg_col("val", "vals");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "ARRAY_AGG merge should return NULL sentinel: {merge}",
        );
    }

    #[test]
    fn test_rescan_agg_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "region", "name"]);
        let group_by = vec![colref("region")];
        let aggs = vec![string_agg_col("name", "', '", "members")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "Group-rescan aggregates should not be eligible for P5 bypass",
        );
    }

    #[test]
    fn test_diff_aggregate_bool_and() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![bool_and_col("active", "all_active")],
            child: Box::new(scan(1, "employees", "public", "e", &["dept", "active"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "BOOL_AND aggregate should diff: {result:?}",);
        let dr = result.unwrap();
        assert!(
            dr.columns.contains(&"all_active".to_string()),
            "Output should include BOOL_AND alias: {:?}",
            dr.columns,
        );
    }

    #[test]
    fn test_diff_aggregate_string_agg() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![string_agg_col("name", "', '", "members")],
            child: Box::new(scan(1, "employees", "public", "e", &["dept", "name"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "STRING_AGG aggregate should diff: {result:?}",
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "STRING_AGG diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_array_agg() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![array_agg_col("val", "vals")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "val"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "ARRAY_AGG aggregate should diff: {result:?}",
        );
    }

    #[test]
    fn test_diff_aggregate_bool_or() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("region")],
            aggregates: vec![bool_or_col("has_flag", "any_flag")],
            child: Box::new(scan(1, "t", "public", "t", &["region", "has_flag"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "BOOL_OR should diff: {result:?}");
    }

    #[test]
    fn test_diff_aggregate_mixed_algebraic_and_rescan() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![
                count_star("cnt"),
                sum_col("amount", "total"),
                string_agg_col("name", "', '", "members"),
            ],
            child: Box::new(scan(
                1,
                "employees",
                "public",
                "e",
                &["dept", "amount", "name"],
            )),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "Mixed algebraic+rescan should diff: {result:?}",
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        // Algebraic aggregates use addition
        assert!(
            sql.contains("COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0)"),
            "COUNT should use algebraic merge: {sql}",
        );
        // Rescan aggregates use NULL sentinel
        assert!(
            sql.contains("agg_rescan"),
            "STRING_AGG should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_bool_and_with_filter() {
        let agg = with_filter(
            bool_and_col("active", "all_active"),
            binop("=", colref("dept"), lit("'eng'")),
        );
        let (ins, _del) = agg_delta_exprs(&agg, &["active".to_string(), "dept".to_string()]);
        assert!(
            ins.contains("(dept = 'eng')"),
            "Filtered BOOL_AND should include filter: {ins}",
        );
    }

    // ── resolve_expr_for_child tests ─────────────────────────────────

    #[test]
    fn test_resolve_expr_for_child_simple_column() {
        let expr = colref("amount");
        let result = resolve_expr_for_child(&expr, &["amount".to_string()]);
        assert_eq!(result, "amount");
    }

    #[test]
    fn test_resolve_expr_for_child_qualified_column_with_disambiguation() {
        let expr = qcolref("o", "amount");
        let result =
            resolve_expr_for_child(&expr, &["o__amount".to_string(), "c__name".to_string()]);
        assert_eq!(result, "o__amount");
    }

    #[test]
    fn test_resolve_expr_for_child_binary_op() {
        let expr = binop("=", colref("status"), lit("'active'"));
        let result = resolve_expr_for_child(&expr, &["status".to_string()]);
        assert_eq!(result, "(status = 'active')");
    }

    #[test]
    fn test_resolve_expr_for_child_nested_binary_op() {
        let expr = binop(
            "AND",
            binop("=", colref("status"), lit("'active'")),
            binop(">", colref("amount"), lit("0")),
        );
        let result = resolve_expr_for_child(&expr, &["status".to_string(), "amount".to_string()]);
        assert!(
            result.contains("status = 'active'"),
            "Should resolve left: {result}"
        );
        assert!(
            result.contains("amount > 0"),
            "Should resolve right: {result}"
        );
    }

    // ── BIT_AND / BIT_OR / BIT_XOR tests ────────────────────────────

    #[test]
    fn test_agg_delta_exprs_bit_and() {
        let agg = bit_and_col("flags", "all_flags");
        let (ins, del) = agg_delta_exprs(&agg, &["flags".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "BIT_AND insert delta should check action: {ins}",
        );
        assert!(
            ins.contains("flags IS NOT NULL"),
            "BIT_AND insert delta should check NOT NULL: {ins}",
        );
        assert!(
            del.contains("__pgt_action = 'D'"),
            "BIT_AND delete delta should check action: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_bit_or() {
        let agg = bit_or_col("flags", "any_flags");
        let (ins, _del) = agg_delta_exprs(&agg, &["flags".to_string()]);
        assert!(
            ins.contains("flags IS NOT NULL"),
            "BIT_OR insert delta should check NOT NULL: {ins}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_bit_xor() {
        let agg = bit_xor_col("flags", "xor_flags");
        let (ins, del) = agg_delta_exprs(&agg, &["flags".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "BIT_XOR insert delta should check action: {ins}",
        );
        assert!(
            del.contains("__pgt_action = 'D'"),
            "BIT_XOR delete delta should check action: {del}",
        );
    }

    #[test]
    fn test_agg_merge_expr_bit_and_rescan() {
        let agg = bit_and_col("flags", "all_flags");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "BIT_AND merge should return NULL sentinel on change: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_bit_or_rescan() {
        let agg = bit_or_col("flags", "any_flags");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "BIT_OR merge should return NULL sentinel on change: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_bit_xor_rescan() {
        let agg = bit_xor_col("flags", "xor_flags");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "BIT_XOR merge should return NULL sentinel on change: {merge}",
        );
    }

    #[test]
    fn test_diff_aggregate_bit_and() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![bit_and_col("flags", "all_flags")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "flags"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "BIT_AND aggregate should diff: {result:?}",);
        let dr = result.unwrap();
        assert!(
            dr.columns.contains(&"all_flags".to_string()),
            "Output should include BIT_AND alias: {:?}",
            dr.columns,
        );
    }

    #[test]
    fn test_diff_aggregate_bit_or() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![bit_or_col("flags", "any_flags")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "flags"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "BIT_OR aggregate should diff: {result:?}",);
    }

    #[test]
    fn test_diff_aggregate_bit_xor() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![bit_xor_col("flags", "xor_flags")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "flags"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "BIT_XOR aggregate should diff: {result:?}",);
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "BIT_XOR diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_rescan_agg_bit_and_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "dept", "flags"]);
        let group_by = vec![colref("dept")];
        let aggs = vec![bit_and_col("flags", "all_flags")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "BIT_AND should not be eligible for P5 bypass",
        );
    }

    // ── JSON_OBJECT_AGG / JSONB_OBJECT_AGG tests ────────────────────

    #[test]
    fn test_agg_delta_exprs_json_object_agg() {
        let agg = json_object_agg_col("name", "value", "obj");
        let (ins, del) = agg_delta_exprs(&agg, &["name".to_string(), "value".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "JSON_OBJECT_AGG insert delta should check action: {ins}",
        );
        assert!(
            ins.contains("name IS NOT NULL"),
            "JSON_OBJECT_AGG insert delta should check NOT NULL: {ins}",
        );
        assert!(
            del.contains("__pgt_action = 'D'"),
            "JSON_OBJECT_AGG delete delta should check action: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_jsonb_object_agg() {
        let agg = jsonb_object_agg_col("key", "val", "obj");
        let (ins, _del) = agg_delta_exprs(&agg, &["key".to_string(), "val".to_string()]);
        assert!(
            ins.contains("key IS NOT NULL"),
            "JSONB_OBJECT_AGG insert delta should check NOT NULL on key arg: {ins}",
        );
    }

    #[test]
    fn test_agg_merge_expr_json_object_agg_rescan() {
        let agg = json_object_agg_col("name", "value", "obj");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "JSON_OBJECT_AGG merge should return NULL sentinel: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_jsonb_object_agg_rescan() {
        let agg = jsonb_object_agg_col("key", "val", "obj");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "JSONB_OBJECT_AGG merge should return NULL sentinel: {merge}",
        );
    }

    #[test]
    fn test_diff_aggregate_json_object_agg() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![json_object_agg_col("name", "value", "obj")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "name", "value"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "JSON_OBJECT_AGG aggregate should diff: {result:?}",
        );
        let dr = result.unwrap();
        assert!(
            dr.columns.contains(&"obj".to_string()),
            "Output should include JSON_OBJECT_AGG alias: {:?}",
            dr.columns,
        );
    }

    #[test]
    fn test_diff_aggregate_jsonb_object_agg() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![jsonb_object_agg_col("key", "val", "obj")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "key", "val"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "JSONB_OBJECT_AGG aggregate should diff: {result:?}",
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "JSONB_OBJECT_AGG diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_rescan_agg_json_object_agg_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "dept", "name", "value"]);
        let group_by = vec![colref("dept")];
        let aggs = vec![json_object_agg_col("name", "value", "obj")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "JSON_OBJECT_AGG should not be eligible for P5 bypass",
        );
    }

    #[test]
    fn test_diff_aggregate_mixed_with_bitwise() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![count_star("cnt"), bit_or_col("perms", "combined_perms")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "perms"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "Mixed COUNT + BIT_OR should diff: {result:?}",
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        // COUNT uses algebraic merge
        assert!(
            sql.contains("COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0)"),
            "COUNT should use algebraic merge: {sql}",
        );
        // BIT_OR uses rescan sentinel
        assert!(
            sql.contains("agg_rescan"),
            "BIT_OR should generate rescan CTE: {sql}",
        );
    }

    // ── Statistical aggregate tests ─────────────────────────────────────

    #[test]
    fn test_agg_delta_exprs_stddev_pop() {
        let agg = stddev_pop_col("amount", "sd_pop");
        let (ins, del) = agg_delta_exprs(&agg, &["amount".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "STDDEV_POP insert delta should check action: {ins}",
        );
        assert!(
            del.contains("__pgt_action = 'D'"),
            "STDDEV_POP delete delta should check action: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_stddev_samp() {
        let agg = stddev_samp_col("amount", "sd_samp");
        let (ins, del) = agg_delta_exprs(&agg, &["amount".to_string()]);
        assert!(
            ins.contains("amount IS NOT NULL"),
            "STDDEV_SAMP insert delta should check NOT NULL: {ins}",
        );
        assert!(
            del.contains("amount IS NOT NULL"),
            "STDDEV_SAMP delete delta should check NOT NULL: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_var_pop() {
        let agg = var_pop_col("amount", "v_pop");
        let (ins, del) = agg_delta_exprs(&agg, &["amount".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "VAR_POP insert delta should check action: {ins}",
        );
        assert!(
            del.contains("__pgt_action = 'D'"),
            "VAR_POP delete delta should check action: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_var_samp() {
        let agg = var_samp_col("amount", "v_samp");
        let (ins, _del) = agg_delta_exprs(&agg, &["amount".to_string()]);
        assert!(
            ins.contains("amount IS NOT NULL"),
            "VAR_SAMP insert delta should check NOT NULL: {ins}",
        );
    }

    #[test]
    fn test_agg_merge_expr_stddev_pop_rescan() {
        let agg = stddev_pop_col("amount", "sd_pop");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "STDDEV_POP merge should use NULL sentinel: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_stddev_samp_rescan() {
        let agg = stddev_samp_col("amount", "sd_samp");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "STDDEV_SAMP merge should use NULL sentinel: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_var_pop_rescan() {
        let agg = var_pop_col("amount", "v_pop");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "VAR_POP merge should use NULL sentinel: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_var_samp_rescan() {
        let agg = var_samp_col("amount", "v_samp");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "VAR_SAMP merge should use NULL sentinel: {merge}",
        );
    }

    #[test]
    fn test_diff_aggregate_stddev_pop() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![stddev_pop_col("amount", "sd_pop")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "amount"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "STDDEV_POP should diff: {result:?}");
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "STDDEV_POP diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_stddev_samp() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![stddev_samp_col("amount", "sd_samp")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "amount"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "STDDEV_SAMP should diff: {result:?}");
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "STDDEV_SAMP diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_var_pop() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![var_pop_col("amount", "v_pop")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "amount"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "VAR_POP should diff: {result:?}");
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "VAR_POP diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_var_samp() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![var_samp_col("amount", "v_samp")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "amount"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "VAR_SAMP should diff: {result:?}");
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "VAR_SAMP diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_rescan_agg_stddev_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "dept", "amount"]);
        let group_by = vec![colref("dept")];
        let aggs = vec![stddev_samp_col("amount", "sd")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "STDDEV_SAMP should not be eligible for P5 bypass",
        );
    }

    #[test]
    fn test_rescan_agg_variance_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "dept", "amount"]);
        let group_by = vec![colref("dept")];
        let aggs = vec![var_samp_col("amount", "v")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "VAR_SAMP should not be eligible for P5 bypass",
        );
    }

    #[test]
    fn test_diff_aggregate_mixed_with_statistical() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![
                count_star("cnt"),
                sum_col("amount", "total"),
                stddev_pop_col("amount", "sd_pop"),
            ],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "amount"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "Mixed COUNT + SUM + STDDEV_POP should diff: {result:?}",
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        // COUNT uses algebraic merge
        assert!(
            sql.contains("COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0)"),
            "COUNT should use algebraic merge: {sql}",
        );
        // STDDEV_POP uses rescan sentinel
        assert!(
            sql.contains("agg_rescan"),
            "STDDEV_POP should generate rescan CTE: {sql}",
        );
    }

    // ── Ordered-set aggregate tests (MODE, PERCENTILE_CONT, PERCENTILE_DISC) ──

    #[test]
    fn test_is_group_rescan_ordered_set() {
        assert!(AggFunc::Mode.is_group_rescan());
        assert!(AggFunc::PercentileCont.is_group_rescan());
        assert!(AggFunc::PercentileDisc.is_group_rescan());
    }

    #[test]
    fn test_agg_func_sql_names_ordered_set() {
        assert_eq!(AggFunc::Mode.sql_name(), "MODE");
        assert_eq!(AggFunc::PercentileCont.sql_name(), "PERCENTILE_CONT");
        assert_eq!(AggFunc::PercentileDisc.sql_name(), "PERCENTILE_DISC");
    }

    #[test]
    fn test_agg_merge_expr_mode_rescan() {
        let agg = mode_col("category", "most_common");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "MODE merge should return NULL sentinel on change: {merge}",
        );
        assert!(
            merge.contains("d.\"__ins_most_common\""),
            "MODE merge should reference ins counter: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_percentile_cont_rescan() {
        let agg = percentile_cont_col("0.5", "amount", "median_amount");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "PERCENTILE_CONT merge should return NULL sentinel on change: {merge}",
        );
        assert!(
            merge.contains("st.\"median_amount\""),
            "PERCENTILE_CONT merge should reference old value: {merge}",
        );
    }

    #[test]
    fn test_agg_merge_expr_percentile_disc_rescan() {
        let agg = percentile_disc_col("0.75", "score", "p75_score");
        let merge = agg_merge_expr(&agg, false);
        assert!(
            merge.contains("THEN NULL"),
            "PERCENTILE_DISC merge should return NULL sentinel on change: {merge}",
        );
        assert!(
            merge.contains("st.\"p75_score\""),
            "PERCENTILE_DISC merge should reference old value: {merge}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_mode() {
        let agg = mode_col("category", "most_common");
        let (ins, del) = agg_delta_exprs(&agg, &["category".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "MODE insert delta should check action: {ins}",
        );
        assert!(
            del.contains("__pgt_action = 'D'"),
            "MODE delete delta should check action: {del}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_percentile_cont() {
        let agg = percentile_cont_col("0.5", "amount", "median_amount");
        let (ins, _del) = agg_delta_exprs(&agg, &["amount".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "PERCENTILE_CONT insert delta should check action: {ins}",
        );
    }

    #[test]
    fn test_agg_delta_exprs_percentile_disc() {
        let agg = percentile_disc_col("0.75", "score", "p75_score");
        let (ins, _del) = agg_delta_exprs(&agg, &["score".to_string()]);
        assert!(
            ins.contains("__pgt_action = 'I'"),
            "PERCENTILE_DISC insert delta should check action: {ins}",
        );
    }

    #[test]
    fn test_diff_aggregate_mode() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("region")],
            aggregates: vec![mode_col("category", "most_common")],
            child: Box::new(scan(1, "products", "public", "p", &["region", "category"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(result.is_ok(), "MODE aggregate should diff: {result:?}");
        let dr = result.unwrap();
        assert!(
            dr.columns.contains(&"most_common".to_string()),
            "Output should include MODE alias: {:?}",
            dr.columns,
        );
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "MODE diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_percentile_cont() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![percentile_cont_col("0.5", "salary", "median_salary")],
            child: Box::new(scan(1, "employees", "public", "e", &["dept", "salary"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "PERCENTILE_CONT aggregate should diff: {result:?}",
        );
        let dr = result.unwrap();
        assert!(
            dr.columns.contains(&"median_salary".to_string()),
            "Output should include PERCENTILE_CONT alias: {:?}",
            dr.columns,
        );
        let sql = ctx.build_with_query(&dr.cte_name);
        assert!(
            sql.contains("agg_rescan"),
            "PERCENTILE_CONT diff should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_percentile_disc() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![percentile_disc_col("0.75", "score", "p75")],
            child: Box::new(scan(1, "t", "public", "t", &["dept", "score"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "PERCENTILE_DISC aggregate should diff: {result:?}",
        );
        let dr = result.unwrap();
        assert!(
            dr.columns.contains(&"p75".to_string()),
            "Output should include PERCENTILE_DISC alias: {:?}",
            dr.columns,
        );
    }

    #[test]
    fn test_rescan_agg_mode_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "region", "category"]);
        let group_by = vec![colref("region")];
        let aggs = vec![mode_col("category", "most_common")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "MODE should not be eligible for P5 bypass",
        );
    }

    #[test]
    fn test_rescan_agg_percentile_cont_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "dept", "salary"]);
        let group_by = vec![colref("dept")];
        let aggs = vec![percentile_cont_col("0.5", "salary", "median")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "PERCENTILE_CONT should not be eligible for P5 bypass",
        );
    }

    #[test]
    fn test_rescan_agg_percentile_disc_disqualifies_p5_bypass() {
        let child = scan(1, "t", "public", "t", &["id", "dept", "score"]);
        let group_by = vec![colref("dept")];
        let aggs = vec![percentile_disc_col("0.75", "score", "p75")];
        assert!(
            !is_direct_agg_eligible(&child, &group_by, &aggs),
            "PERCENTILE_DISC should not be eligible for P5 bypass",
        );
    }

    #[test]
    fn test_diff_aggregate_mixed_with_ordered_set() {
        let mut ctx = test_ctx_with_st("public", "st");
        let agg = OpTree::Aggregate {
            group_by: vec![colref("dept")],
            aggregates: vec![
                count_star("cnt"),
                sum_col("salary", "total"),
                percentile_cont_col("0.5", "salary", "median_salary"),
            ],
            child: Box::new(scan(1, "employees", "public", "e", &["dept", "salary"])),
        };
        let result = diff_aggregate(&mut ctx, &agg);
        assert!(
            result.is_ok(),
            "Mixed COUNT + SUM + PERCENTILE_CONT should diff: {result:?}",
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        // COUNT uses algebraic merge
        assert!(
            sql.contains("COALESCE(d.__ins_count, 0) - COALESCE(d.__del_count, 0)"),
            "COUNT should use algebraic merge: {sql}",
        );
        // PERCENTILE_CONT uses rescan sentinel
        assert!(
            sql.contains("agg_rescan"),
            "PERCENTILE_CONT should generate rescan CTE: {sql}",
        );
    }

    #[test]
    fn test_diff_aggregate_mode_with_filter() {
        let agg = with_filter(
            mode_col("category", "most_common"),
            binop("=", colref("active"), lit("true")),
        );
        let (ins, _del) = agg_delta_exprs(&agg, &["category".to_string(), "active".to_string()]);
        assert!(
            ins.contains("(active = true)"),
            "Filtered MODE should include filter: {ins}",
        );
    }

    #[test]
    fn test_diff_aggregate_percentile_cont_with_filter() {
        let agg = with_filter(
            percentile_cont_col("0.5", "amount", "median"),
            binop(">", colref("amount"), lit("0")),
        );
        let (ins, _del) = agg_delta_exprs(&agg, &["amount".to_string()]);
        assert!(
            ins.contains("(amount > 0)"),
            "Filtered PERCENTILE_CONT should include filter: {ins}",
        );
    }

    // ── A-1: Verify rescan CTE does not leak into algebraic aggregate SQL ─────

    #[test]
    fn test_no_rescan_cte_for_sum() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["region", "amount"]);
        let tree = aggregate(
            vec![colref("region")],
            vec![sum_col("amount", "total")],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        assert_sql_not_contains(&sql, "agg_rescan");
    }

    #[test]
    fn test_no_rescan_cte_for_count_star() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["region", "val"]);
        let tree = aggregate(vec![colref("region")], vec![count_star("cnt")], child);
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        assert_sql_not_contains(&sql, "agg_rescan");
    }

    #[test]
    fn test_rescan_cte_for_avg() {
        // AVG now uses group-rescan for precision
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["region", "amount"]);
        let tree = aggregate(
            vec![colref("region")],
            vec![avg_col("amount", "avg_amt")],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        assert_sql_contains(&sql, "agg_rescan");
    }

    #[test]
    fn test_rescan_cte_for_sum_count_avg_combined() {
        // AVG triggers rescan even when mixed with algebraic SUM/COUNT
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["region", "amount"]);
        let tree = aggregate(
            vec![colref("region")],
            vec![
                sum_col("amount", "total"),
                count_star("cnt"),
                avg_col("amount", "avg_amt"),
            ],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        assert_sql_contains(&sql, "agg_rescan");
    }

    #[test]
    fn test_rescan_cte_for_min_max() {
        // MIN/MAX now include a rescan CTE so that when the old extremum
        // is deleted, the correct new extremum is rescanned from source.
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["region", "amount"]);
        let tree = aggregate(
            vec![colref("region")],
            vec![min_col("amount", "min_amt"), max_col("amount", "max_amt")],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        assert_sql_contains(&sql, "agg_rescan");
        // Rescan fallback in merge: when extremum deleted, use r.{col}
        assert_sql_contains(&sql, "THEN r.\"min_amt\"");
        assert_sql_contains(&sql, "THEN r.\"max_amt\"");
    }

    #[test]
    fn test_rescan_cte_only_for_group_rescan_aggregates() {
        // Mixed: SUM (algebraic) + BIT_AND (group-rescan)
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["region", "flags", "amount"]);
        let tree = aggregate(
            vec![colref("region")],
            vec![
                sum_col("amount", "total"),
                bit_and_col("flags", "all_flags"),
            ],
            child,
        );
        let result = diff_aggregate(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // Rescan CTE should be present because BIT_AND requires it
        assert_sql_contains(&sql, "agg_rescan");
        // But the algebraic SUM should still use algebraic merge (COALESCE + ins - del)
        assert_sql_contains(
            &sql,
            "COALESCE(d.\"__ins_total\", 0) - COALESCE(d.\"__del_total\", 0)",
        );
    }
}
