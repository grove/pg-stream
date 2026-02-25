//! LATERAL subquery differentiation via row-scoped recomputation.
//!
//! Strategy: When outer source rows change, re-execute the correlated
//! subquery only for affected rows. This is the same strategy as
//! [`LateralFunction`] but for full subqueries instead of SRFs.
//!
//! CTE chain:
//! 1. Child delta (from recursive diff_node on the LATERAL dependency)
//! 2. Old ST rows for changed source rows (emitted as 'D' actions)
//! 3. Re-execute the subquery for inserted/updated source rows (emitted as 'I' actions)
//! 4. Combine deletes + inserts into final delta
//!
//! Row identity: `hash(child_row_columns || '/' || subquery_result)` — content-based.
//!
//! LEFT JOIN LATERAL: uses `LEFT JOIN LATERAL (...) ON true` in the expand
//! CTE so that outer rows without matching inner rows produce NULL-padded rows.

use crate::dvm::diff::{DiffContext, DiffResult, col_list, quote_ident};
use crate::dvm::operators::scan::build_hash_expr;
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate a LateralSubquery node via row-scoped recomputation.
///
/// For each source row that changed (INSERT/UPDATE/DELETE), delete old
/// subquery results from the ST and re-execute the subquery for the
/// new version of the source row.
pub fn diff_lateral_subquery(
    ctx: &mut DiffContext,
    op: &OpTree,
) -> Result<DiffResult, PgStreamError> {
    let OpTree::LateralSubquery {
        subquery_sql,
        alias,
        column_aliases,
        output_cols,
        is_left_join,
        child,
        ..
    } = op
    else {
        return Err(PgStreamError::InternalError(
            "diff_lateral_subquery called on non-LateralSubquery node".into(),
        ));
    };

    // ── Differentiate child to get the source delta ────────────────────
    let child_result = ctx.diff_node(child)?;

    let st_table = ctx
        .st_qualified_name
        .clone()
        .unwrap_or_else(|| "/* st_table */".to_string());

    // Column names from the child (source table columns)
    let child_cols = &child_result.columns;

    // Subquery result column names
    let sub_cols: Vec<String> = if column_aliases.is_empty() {
        output_cols.clone()
    } else {
        column_aliases.clone()
    };

    // All output columns = child columns + subquery columns
    let mut all_output_cols: Vec<String> = child_cols.clone();
    all_output_cols.extend(sub_cols.iter().cloned());

    // ── CTE 1: Find source rows that changed ──────────────────────────
    let changed_sources_cte = ctx.next_cte_name("lat_sq_changed");
    let changed_sources_sql = format!(
        "SELECT DISTINCT \"__pgs_row_id\", \"__pgs_action\", {child_col_list}\n\
         FROM {child_delta}",
        child_col_list = col_list(child_cols),
        child_delta = child_result.cte_name,
    );
    ctx.add_cte(changed_sources_cte.clone(), changed_sources_sql);

    // ── CTE 2: Old ST rows for changed source rows (DELETE actions) ────
    let old_rows_cte = ctx.next_cte_name("lat_sq_old");

    // Build a join condition: for each child column, match st.col = cs.col
    let join_on_child_cols = child_cols
        .iter()
        .map(|c| {
            let qc = quote_ident(c);
            format!("st.{qc} IS NOT DISTINCT FROM cs.{qc}")
        })
        .collect::<Vec<_>>()
        .join(" AND ");

    let all_cols_st = all_output_cols
        .iter()
        .map(|c| format!("st.{}", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");

    let old_rows_sql = format!(
        "SELECT st.\"__pgs_row_id\", {all_cols_st}\n\
         FROM {st_table} st\n\
         WHERE EXISTS (\n\
             SELECT 1 FROM {changed_sources_cte} cs\n\
             WHERE {join_on_child_cols}\n\
         )",
    );
    ctx.add_cte(old_rows_cte.clone(), old_rows_sql);

    // ── CTE 3: Re-execute subquery for inserted/updated source rows ────
    let expand_cte = ctx.next_cte_name("lat_sq_expand");

    // Build column references for the subquery result
    let sub_col_refs: Vec<String> = sub_cols
        .iter()
        .map(|c| format!("{}.{}", quote_ident(alias), quote_ident(c)))
        .collect();
    let sub_col_refs_str = sub_col_refs.join(", ");

    // Use the outer table's original alias for the changed-sources CTE
    // so that the subquery's column references resolve naturally.
    let outer_alias = child.alias().to_string();
    let child_col_refs: Vec<String> = child_cols
        .iter()
        .map(|c| format!("{}.{}", quote_ident(&outer_alias), quote_ident(c)))
        .collect();
    let child_col_refs_str = child_col_refs.join(", ");

    // Build hash expression for the row ID: hash all output columns
    let hash_exprs: Vec<String> = child_cols
        .iter()
        .map(|c| format!("{}.{}::TEXT", quote_ident(&outer_alias), quote_ident(c)))
        .chain(sub_cols.iter().map(|c| {
            if *is_left_join {
                format!(
                    "COALESCE({}.{}::TEXT, '')",
                    quote_ident(alias),
                    quote_ident(c),
                )
            } else {
                format!("{}.{}::TEXT", quote_ident(alias), quote_ident(c))
            }
        }))
        .collect();
    let row_id_expr = build_hash_expr(&hash_exprs);

    // Build the subquery alias clause
    let sub_alias_clause = if column_aliases.is_empty() {
        quote_ident(alias)
    } else {
        let col_alias_list = sub_cols
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ({col_alias_list})", quote_ident(alias))
    };

    // Build the LATERAL clause: use LEFT JOIN LATERAL or comma syntax
    let (lateral_clause, action_filter_prefix) = if *is_left_join {
        (
            format!(
                "FROM {changed_sources_cte} AS {outer_alias_q}\n\
                 LEFT JOIN LATERAL ({subquery_sql}) AS {sub_alias_clause} ON true",
                outer_alias_q = quote_ident(&outer_alias),
            ),
            format!(
                "{outer_alias_q}.\"__pgs_action\" = 'I'",
                outer_alias_q = quote_ident(&outer_alias),
            ),
        )
    } else {
        (
            format!(
                "FROM {changed_sources_cte} AS {outer_alias_q},\n\
                      LATERAL ({subquery_sql}) AS {sub_alias_clause}",
                outer_alias_q = quote_ident(&outer_alias),
            ),
            format!(
                "{outer_alias_q}.\"__pgs_action\" = 'I'",
                outer_alias_q = quote_ident(&outer_alias),
            ),
        )
    };

    let expand_sql = format!(
        "SELECT {row_id_expr} AS \"__pgs_row_id\",\n\
                {child_col_refs_str},\n\
                {sub_col_refs_str}\n\
         {lateral_clause}\n\
         WHERE {action_filter_prefix}",
    );
    ctx.add_cte(expand_cte.clone(), expand_sql);

    // ── CTE 4: Final delta — DELETE old + INSERT new ───────────────────
    let final_cte = ctx.next_cte_name("lat_sq_final");

    let all_cols_name = col_list(&all_output_cols);

    let final_sql = format!(
        "-- Delete old subquery results for changed source rows\n\
         SELECT \"__pgs_row_id\", 'D' AS \"__pgs_action\", {all_cols_name}\n\
         FROM {old_rows_cte}\n\
         UNION ALL\n\
         -- Insert re-executed subquery results for new/updated source rows\n\
         SELECT \"__pgs_row_id\", 'I' AS \"__pgs_action\", {all_cols_name}\n\
         FROM {expand_cte}",
    );
    ctx.add_cte(final_cte.clone(), final_sql);

    Ok(DiffResult {
        cte_name: final_cte,
        columns: all_output_cols,
        is_deduplicated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    /// Build a LateralSubquery node for tests.
    fn lateral_subquery(
        subquery_sql: &str,
        alias: &str,
        col_aliases: Vec<&str>,
        output_cols: Vec<&str>,
        is_left_join: bool,
        subquery_source_oids: Vec<u32>,
        child: OpTree,
    ) -> OpTree {
        OpTree::LateralSubquery {
            subquery_sql: subquery_sql.to_string(),
            alias: alias.to_string(),
            column_aliases: col_aliases.into_iter().map(|c| c.to_string()).collect(),
            output_cols: output_cols.into_iter().map(|c| c.to_string()).collect(),
            is_left_join,
            subquery_source_oids,
            child: Box::new(child),
        }
    }

    #[test]
    fn test_diff_lateral_subquery_basic() {
        let mut ctx = test_ctx_with_st("public", "my_st");
        let child = scan(1, "orders", "public", "o", &["id", "customer"]);
        let tree = lateral_subquery(
            "SELECT amount, created_at FROM line_items li WHERE li.order_id = o.id ORDER BY created_at DESC LIMIT 1",
            "latest",
            vec![],
            vec!["amount", "created_at"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Output should include child columns + subquery columns
        assert!(result.columns.contains(&"id".to_string()));
        assert!(result.columns.contains(&"customer".to_string()));
        assert!(result.columns.contains(&"amount".to_string()));
        assert!(result.columns.contains(&"created_at".to_string()));

        // Should have the CTE chain
        assert_sql_contains(&sql, "lat_sq_changed");
        assert_sql_contains(&sql, "lat_sq_old");
        assert_sql_contains(&sql, "lat_sq_expand");
        assert_sql_contains(&sql, "lat_sq_final");
    }

    #[test]
    fn test_diff_lateral_subquery_left_join() {
        let mut ctx = test_ctx_with_st("public", "my_st");
        let child = scan(1, "departments", "public", "d", &["id", "name"]);
        let tree = lateral_subquery(
            "SELECT SUM(salary) AS total, COUNT(*) AS cnt FROM employees e WHERE e.dept_id = d.id",
            "stats",
            vec!["total", "cnt"],
            vec!["total", "cnt"],
            true,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should use LEFT JOIN LATERAL
        assert_sql_contains(&sql, "LEFT JOIN LATERAL");
        assert_sql_contains(&sql, "ON true");

        // Output should include all columns
        assert!(result.columns.contains(&"id".to_string()));
        assert!(result.columns.contains(&"name".to_string()));
        assert!(result.columns.contains(&"total".to_string()));
        assert!(result.columns.contains(&"cnt".to_string()));
    }

    #[test]
    fn test_diff_lateral_subquery_uses_original_alias() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "orders", "public", "o", &["id", "customer"]);
        let tree = lateral_subquery(
            "SELECT amount FROM line_items li WHERE li.order_id = o.id LIMIT 1",
            "latest",
            vec![],
            vec!["amount"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // The expand CTE should alias the changed-sources row with the original "o" alias
        // so that the subquery's `o.id` reference resolves correctly
        assert_sql_contains(&sql, "AS \"o\"");
    }

    #[test]
    fn test_diff_lateral_subquery_old_rows_join_condition() {
        let mut ctx = test_ctx_with_st("public", "my_st");
        let child = scan(1, "parent", "public", "p", &["id", "data"]);
        let tree = lateral_subquery(
            "SELECT val FROM child_table c WHERE c.parent_id = p.id",
            "sub",
            vec![],
            vec!["val"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Old rows CTE should join on child columns with IS NOT DISTINCT FROM
        assert_sql_contains(&sql, "IS NOT DISTINCT FROM");
    }

    #[test]
    fn test_diff_lateral_subquery_expand_filters_inserts() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "val"]);
        let tree = lateral_subquery(
            "SELECT x FROM other o WHERE o.fk = t.id",
            "sub",
            vec![],
            vec!["x"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // The expand CTE should only process INSERT actions
        assert_sql_contains(&sql, "__pgs_action\" = 'I'");
    }

    #[test]
    fn test_diff_lateral_subquery_hash_includes_all_columns() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "data"]);
        let tree = lateral_subquery(
            "SELECT val FROM sub_t s WHERE s.fk = t.id",
            "sub",
            vec!["val"],
            vec!["val"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Row ID hash should include both child and subquery columns
        assert_sql_contains(&sql, "pg_stream_hash");
    }

    #[test]
    fn test_diff_lateral_subquery_output_columns() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "data"]);
        let tree = lateral_subquery(
            "SELECT val FROM sub_t s WHERE s.fk = t.id",
            "sub",
            vec![],
            vec!["val"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        assert_eq!(result.columns, vec!["id", "data", "val"]);
    }

    #[test]
    fn test_diff_lateral_subquery_not_deduplicated() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree = lateral_subquery(
            "SELECT x FROM sub_t WHERE fk = t.id",
            "sub",
            vec![],
            vec!["x"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_lateral_subquery_error_on_wrong_node() {
        let mut ctx = test_ctx_with_st("public", "st");
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_lateral_subquery(&mut ctx, &tree);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_lateral_subquery_with_column_aliases() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree = lateral_subquery(
            "SELECT SUM(x) AS total FROM sub_t WHERE fk = t.id",
            "agg",
            vec!["total_amount"],
            vec!["total"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        // When column_aliases are provided, they override output_cols
        assert!(result.columns.contains(&"total_amount".to_string()));
        assert!(!result.columns.contains(&"total".to_string()));
    }

    #[test]
    fn test_diff_lateral_subquery_left_join_coalesce_hash() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree = lateral_subquery(
            "SELECT val FROM sub_t WHERE fk = t.id",
            "sub",
            vec!["val"],
            vec!["val"],
            true, // LEFT JOIN
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // LEFT JOIN should use COALESCE for NULL-safe hashing
        assert_sql_contains(&sql, "COALESCE");
    }

    #[test]
    fn test_diff_lateral_subquery_contains_lateral_keyword() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "data"]);
        let tree = lateral_subquery(
            "SELECT val FROM sub_t s WHERE s.fk = t.id",
            "sub",
            vec![],
            vec!["val"],
            false,
            vec![2],
            child,
        );
        let result = diff_lateral_subquery(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Must use LATERAL keyword
        assert_sql_contains(&sql, "LATERAL (SELECT");
    }

    // ── OpTree method tests ─────────────────────────────────────────────

    #[test]
    fn test_lateral_subquery_output_columns_with_aliases() {
        let child = scan(1, "t", "public", "t", &["id", "data"]);
        let tree = lateral_subquery(
            "SELECT val FROM sub_t",
            "sub",
            vec!["result_val"],
            vec!["val"],
            false,
            vec![],
            child,
        );
        assert_eq!(tree.output_columns(), vec!["id", "data", "result_val"]);
    }

    #[test]
    fn test_lateral_subquery_output_columns_defaults_to_output_cols() {
        let child = scan(1, "t", "public", "t", &["id", "tags"]);
        let tree = lateral_subquery(
            "SELECT name FROM items",
            "sub",
            vec![],
            vec!["name"],
            false,
            vec![],
            child,
        );
        assert_eq!(tree.output_columns(), vec!["id", "tags", "name"]);
    }

    #[test]
    fn test_lateral_subquery_source_oids_includes_child_and_subquery() {
        let child = scan(42, "t", "public", "t", &["id"]);
        let tree = lateral_subquery(
            "SELECT val FROM other_table",
            "sub",
            vec![],
            vec!["val"],
            false,
            vec![99],
            child,
        );
        let oids = tree.source_oids();
        assert!(oids.contains(&42));
        assert!(oids.contains(&99));
    }

    #[test]
    fn test_lateral_subquery_alias() {
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree = lateral_subquery(
            "SELECT val FROM items",
            "my_alias",
            vec![],
            vec!["val"],
            false,
            vec![],
            child,
        );
        assert_eq!(tree.alias(), "my_alias");
    }

    #[test]
    fn test_lateral_subquery_node_kind() {
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree = lateral_subquery(
            "SELECT val FROM items",
            "sub",
            vec![],
            vec!["val"],
            false,
            vec![],
            child,
        );
        assert_eq!(tree.node_kind(), "lateral subquery");
    }

    #[test]
    fn test_lateral_subquery_is_left_join_flag() {
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree_inner = lateral_subquery(
            "SELECT val FROM items",
            "sub",
            vec![],
            vec!["val"],
            false,
            vec![],
            child.clone(),
        );
        let tree_left = lateral_subquery(
            "SELECT val FROM items",
            "sub",
            vec![],
            vec!["val"],
            true,
            vec![],
            child,
        );
        assert!(matches!(
            tree_inner,
            OpTree::LateralSubquery {
                is_left_join: false,
                ..
            }
        ));
        assert!(matches!(
            tree_left,
            OpTree::LateralSubquery {
                is_left_join: true,
                ..
            }
        ));
    }
}
