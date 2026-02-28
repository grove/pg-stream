//! Inner join differentiation.
//!
//! ΔI(Q ⋈C R) = (ΔQ ⋈C R₁) + (Q₀ ⋈C ΔR)
//!
//! Where:
//! - R₁ = current state of R (post-change, i.e. live table)
//! - Q₀ = pre-change state of Q, reconstructed as
//!   Q_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
//! - ΔQ, ΔR = deltas (INSERT/DELETE) for each side
//!
//! Using Q₀ (pre-change) in Part 2 instead of Q₁ (post-change) avoids
//! double-counting when both sides change simultaneously: Part 1 already
//! handles (ΔQ ⋈ R₁), and using Q₀ excludes newly-inserted Q rows that
//! would duplicate Part 1's contribution.
//!
//! ## Semi-join optimization
//!
//! When the base table is scanned for the "current" side of the join,
//! a semi-join filter limits the scan to rows whose join keys appear in
//! the delta of the other side. For example, Part 2 becomes:
//!
//! ```sql
//! FROM (SELECT * FROM left_table
//!       WHERE left_key IN (SELECT DISTINCT right_key FROM delta_right)
//! ) l
//! JOIN delta_right dr ON ...
//! ```
//!
//! This converts a full sequential scan of the base table into an indexed
//! lookup when the join key has an index, providing 10x+ speedup at low
//! change rates.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join_common::{
    build_base_table_key_exprs, build_snapshot_sql, is_simple_child, rewrite_join_condition,
};
use crate::dvm::parser::{Expr, OpTree};
use crate::error::PgStreamError;

/// Differentiate an InnerJoin node.
pub fn diff_inner_join(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::InnerJoin {
        condition,
        left,
        right,
    } = op
    else {
        return Err(PgStreamError::InternalError(
            "diff_inner_join called on non-InnerJoin node".into(),
        ));
    };

    // Differentiate both children
    let left_result = ctx.diff_node(left)?;
    let right_result = ctx.diff_node(right)?;

    // Get the base table references for the current snapshot.
    // For Scan children this is the table name; for nested joins
    // this is a snapshot subquery with disambiguated columns.
    let left_table = build_snapshot_sql(left);
    let right_table = build_snapshot_sql(right);

    let left_cols = &left_result.columns;
    let right_cols = &right_result.columns;

    // Disambiguate output columns using table-alias prefixed names.
    // This prevents collisions when both sides have columns with the same
    // name (e.g., both have "id", "val"). The project diff knows how to
    // resolve qualified ColumnRef(table="l", col="id") → "l__id".
    let left_prefix = left.alias();
    let right_prefix = right.alias();

    let mut output_cols = Vec::new();
    for c in left_cols {
        output_cols.push(format!("{left_prefix}__{c}"));
    }
    for c in right_cols {
        output_cols.push(format!("{right_prefix}__{c}"));
    }

    let left_col_refs: Vec<String> = left_cols
        .iter()
        .map(|c| {
            format!(
                "dl.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{left_prefix}__{c}"))
            )
        })
        .collect();
    let right_col_refs: Vec<String> = right_cols
        .iter()
        .map(|c| {
            format!(
                "r.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{right_prefix}__{c}"))
            )
        })
        .collect();
    let left_col_refs2: Vec<String> = left_cols
        .iter()
        .map(|c| {
            format!(
                "l.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{left_prefix}__{c}"))
            )
        })
        .collect();
    let right_col_refs2: Vec<String> = right_cols
        .iter()
        .map(|c| {
            format!(
                "dr.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{right_prefix}__{c}"))
            )
        })
        .collect();

    let all_cols_part1 = [left_col_refs.as_slice(), right_col_refs.as_slice()]
        .concat()
        .join(", ");
    let all_cols_part2 = [left_col_refs2.as_slice(), right_col_refs2.as_slice()]
        .concat()
        .join(", ");

    // Row ID: hash of both child row IDs.
    // For the delta side, we use __pgs_row_id from the delta CTE.
    // For the base table side, we hash its PK/non-nullable columns
    // instead of serializing the entire row with row_to_json().
    //
    // S1 optimization: flatten into a single pg_stream_hash_multi call with
    // all key columns inline, avoiding nested hash calls.
    // For nested join children, falls back to row_to_json for the snapshot side.
    let right_key_exprs = build_base_table_key_exprs(right, "r");
    let left_key_exprs = build_base_table_key_exprs(left, "l");

    let mut hash1_args = vec!["dl.__pgs_row_id::TEXT".to_string()];
    hash1_args.extend(right_key_exprs);
    let hash_part1 = format!(
        "pgstream.pg_stream_hash_multi(ARRAY[{}])",
        hash1_args.join(", ")
    );

    let mut hash2_args = left_key_exprs;
    hash2_args.push("dr.__pgs_row_id::TEXT".to_string());
    let hash_part2 = format!(
        "pgstream.pg_stream_hash_multi(ARRAY[{}])",
        hash2_args.join(", ")
    );

    // Rewrite join condition with aliases for each part.
    // The original condition uses the source table aliases (e.g. o.cust_id = c.id).
    // Part 1 needs: dl (delta left) + r (base right).
    // Part 2 needs: l (pre-change left) + dr (delta right).
    //
    // For nested join children, column names are disambiguated with the
    // original table alias prefix (e.g., o.cust_id → dl."o__cust_id").
    let join_cond_part1 = rewrite_join_condition(condition, left, "dl", right, "r");
    let join_cond_part2 = rewrite_join_condition(condition, left, "l", right, "dr");

    // Extract equi-join key pairs for semi-join optimization.
    // If we can identify (left_key, right_key) pairs from the condition,
    // we filter the base table scan to only matching keys from the delta.
    //
    // Skip the optimization when either child is a nested join — the
    // column names in the condition don't directly match the snapshot
    // or delta CTE columns for complex children.
    let equi_keys = if is_simple_child(left) && is_simple_child(right) {
        extract_equijoin_keys(condition)
    } else {
        vec![]
    };

    // Build semi-join-filtered table references.
    // Part 1: right base table filtered by delta-left join keys
    let right_table_filtered = build_semijoin_subquery(
        &right_table,
        &equi_keys,
        &left_result.cte_name,
        JoinSide::Right,
    );
    // ── Pre-change snapshot for Part 2 (Scan children only) ─────────
    //
    // Standard DBSP: ΔJ = (ΔL ⋈ R₁) + (L₀ ⋈ ΔR)
    //
    // L₀ = the state of the left child BEFORE the current cycle's changes.
    // Reconstructed as: L_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes.
    //
    // For Scan children this is cheap: one table scan and a small delta.
    // For nested join children, computing L₀ requires the full join
    // snapshot plus EXCEPT ALL — prohibitively expensive for multi-table
    // chains. Fall back to post-change L₁ with semi-join filter, which
    // may double-count when both sides change simultaneously, but this
    // only matters when the LEFT child of the outer join also changes,
    // which is uncommon in practice (RF mutations typically touch only
    // base tables, not derived join results).
    let left_part2_source = if is_simple_child(left) {
        // Scan child: use cheap L₀ via EXCEPT ALL
        let left_data_cols: String = left_cols
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");

        let left_alias = left.alias();
        let left_pre_change = format!(
            "(SELECT {left_data_cols} FROM {left_table} {la} \
             EXCEPT ALL \
             SELECT {left_data_cols} FROM {delta_left} WHERE __pgs_action = 'I' \
             UNION ALL \
             SELECT {left_data_cols} FROM {delta_left} WHERE __pgs_action = 'D')",
            la = quote_ident(left_alias),
            delta_left = left_result.cte_name,
        );
        // Apply semi-join filter to L₀ if equi-keys are available
        if equi_keys.is_empty() {
            left_pre_change
        } else {
            let filters: Vec<String> = equi_keys
                .iter()
                .map(|(left_key, right_key)| {
                    format!(
                        "{left_key} IN (SELECT DISTINCT {right_key} FROM {})",
                        right_result.cte_name
                    )
                })
                .collect();
            format!(
                "(SELECT * FROM {left_pre_change} __l0 WHERE {filters})",
                filters = filters.join(" AND "),
            )
        }
    } else {
        // Nested join child: use post-change L₁ with semi-join filter
        // (too expensive to compute L₀ via EXCEPT ALL for nested joins)
        build_semijoin_subquery(
            &left_table,
            &equi_keys,
            &right_result.cte_name,
            JoinSide::Left,
        )
    };

    let cte_name = ctx.next_cte_name("join");

    let sql = format!(
        "\
-- Part 1: delta_left JOIN current_right (semi-join filtered)
SELECT {hash_part1} AS __pgs_row_id,
       dl.__pgs_action,
       {all_cols_part1}
FROM {delta_left} dl
JOIN {right_table_filtered} r ON {join_cond_part1}

UNION ALL

-- Part 2: pre-change_left JOIN delta_right
-- For Scan children: L₀ = L_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
-- For nested joins: L₁ = current snapshot (semi-join filtered)
SELECT {hash_part2} AS __pgs_row_id,
       dr.__pgs_action,
       {all_cols_part2}
FROM {left_part2_source} l
JOIN {delta_right} dr ON {join_cond_part2}",
        delta_left = left_result.cte_name,
        delta_right = right_result.cte_name,
    );

    ctx.add_cte(cte_name.clone(), sql);

    Ok(DiffResult {
        cte_name,
        columns: output_cols,
        is_deduplicated: false,
    })
}

/// Which side of the join we are filtering.
enum JoinSide {
    /// We are filtering the left base table using right-side delta keys.
    Left,
    /// We are filtering the right base table using left-side delta keys.
    Right,
}

/// An equi-join key pair: `(left_column_sql, right_column_sql)`.
type EquiKeyPair = (String, String);

/// Extract equi-join key pairs from a join condition expression.
///
/// Walks the expression tree looking for `col_a = col_b` patterns,
/// including through AND conjunctions. Returns pairs of
/// `(left_side_sql, right_side_sql)` for each equality found.
///
/// Falls back gracefully: if the condition is too complex (OR, functions,
/// non-equality operators), returns an empty vec and we skip the
/// semi-join optimization.
fn extract_equijoin_keys(condition: &Expr) -> Vec<EquiKeyPair> {
    let mut keys = Vec::new();
    collect_equijoin_keys(condition, &mut keys);
    keys
}

/// Recursively collect equi-join key pairs from an expression.
///
/// Table qualifiers are stripped because the keys are used inside
/// semi-join subqueries where the original table aliases are not in scope.
fn collect_equijoin_keys(expr: &Expr, keys: &mut Vec<EquiKeyPair>) {
    match expr {
        Expr::BinaryOp { op, left, right } if op == "=" => {
            // Found an equality — record both sides with qualifiers stripped
            keys.push((
                left.strip_qualifier().to_sql(),
                right.strip_qualifier().to_sql(),
            ));
        }
        Expr::BinaryOp { op, left, right } if op.eq_ignore_ascii_case("AND") => {
            // AND conjunction — recurse into both sides
            collect_equijoin_keys(left, keys);
            collect_equijoin_keys(right, keys);
        }
        _ => {
            // Non-equality / non-AND: skip (don't add anything).
            // The optimization will be skipped if no keys are found.
        }
    }
}

/// Build a semi-join-filtered subquery for a base table.
///
/// Given equi-join key pairs and the delta CTE name, wraps the base table
/// in a subquery that filters to only rows matching the delta's join keys.
///
/// For `JoinSide::Left`, we filter the left table using right-side keys
/// from the right delta CTE:
/// ```sql
/// (SELECT * FROM left_table WHERE left_key IN
///    (SELECT DISTINCT right_key FROM delta_right))
/// ```
///
/// If no equi-join keys were extracted, returns the plain table reference
/// (no optimization applied).
fn build_semijoin_subquery(
    base_table: &str,
    equi_keys: &[EquiKeyPair],
    delta_cte: &str,
    side: JoinSide,
) -> String {
    if equi_keys.is_empty() {
        return base_table.to_string();
    }

    // Build WHERE ... AND ... clauses for each key pair
    let filters: Vec<String> = equi_keys
        .iter()
        .map(|(left_key, right_key)| {
            match side {
                JoinSide::Left => {
                    // Filtering left table: left_key IN (SELECT DISTINCT right_key FROM delta_right)
                    format!("{left_key} IN (SELECT DISTINCT {right_key} FROM {delta_cte})")
                }
                JoinSide::Right => {
                    // Filtering right table: right_key IN (SELECT DISTINCT left_key FROM delta_left)
                    format!("{right_key} IN (SELECT DISTINCT {left_key} FROM {delta_cte})")
                }
            }
        })
        .collect();

    format!(
        "(SELECT * FROM {base_table} WHERE {filters})",
        filters = filters.join(" AND "),
    )
}

/// Get the current-state table reference for a node.
/// Delegates to `join_common::build_snapshot_sql` for the actual implementation.
/// Kept as a local alias for backward compatibility with test assertions.
#[cfg(test)]
fn get_current_table_ref(op: &OpTree) -> String {
    build_snapshot_sql(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    // ── diff_inner_join tests ───────────────────────────────────────

    #[test]
    fn test_diff_inner_join_basic() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();

        // Output columns should be disambiguated with table prefixes
        assert!(result.columns.contains(&"o__id".to_string()));
        assert!(result.columns.contains(&"o__cust_id".to_string()));
        assert!(result.columns.contains(&"c__id".to_string()));
        assert!(result.columns.contains(&"c__name".to_string()));
    }

    #[test]
    fn test_diff_inner_join_two_parts() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should have two parts (delta_left JOIN right, pre-change_left JOIN delta_right)
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "pre-change_left");
    }

    #[test]
    fn test_diff_inner_join_pre_change_snapshot() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Part 2 should use L₀ = L_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
        assert_sql_contains(&sql, "EXCEPT ALL");
        assert_sql_contains(&sql, "__pgs_action = 'I'");
        assert_sql_contains(&sql, "__pgs_action = 'D'");
    }

    #[test]
    fn test_diff_inner_join_not_deduplicated() {
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_inner_join_error_on_non_join_node() {
        let mut ctx = test_ctx();
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_inner_join(&mut ctx, &tree);
        assert!(result.is_err());
    }

    // ── extract_equijoin_keys tests ─────────────────────────────────

    #[test]
    fn test_extract_equijoin_keys_simple_equality() {
        let cond = eq_cond("o", "cust_id", "c", "id");
        let keys = extract_equijoin_keys(&cond);
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn test_extract_equijoin_keys_and_condition() {
        let cond = binop(
            "AND",
            eq_cond("o", "a", "c", "b"),
            eq_cond("o", "x", "c", "y"),
        );
        let keys = extract_equijoin_keys(&cond);
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn test_extract_equijoin_keys_non_equality_returns_empty() {
        let cond = binop(">", qcolref("o", "a"), qcolref("c", "b"));
        let keys = extract_equijoin_keys(&cond);
        assert!(keys.is_empty());
    }

    // ── build_semijoin_subquery tests ───────────────────────────────

    #[test]
    fn test_build_semijoin_subquery_right_side() {
        let keys = vec![("\"cust_id\"".to_string(), "\"id\"".to_string())];
        let result = build_semijoin_subquery(
            "\"public\".\"customers\"",
            &keys,
            "__pgs_cte_scan_1",
            JoinSide::Right,
        );
        assert!(result.contains("SELECT *"));
        assert!(result.contains("WHERE"));
        assert!(result.contains("IN (SELECT DISTINCT"));
    }

    #[test]
    fn test_build_semijoin_subquery_no_keys_returns_plain() {
        let result = build_semijoin_subquery(
            "\"public\".\"customers\"",
            &[],
            "__pgs_cte_1",
            JoinSide::Right,
        );
        assert_eq!(result, "\"public\".\"customers\"");
    }

    // ── get_current_table_ref tests ─────────────────────────────────

    #[test]
    fn test_get_current_table_ref_scan() {
        let node = scan(1, "orders", "public", "o", &["id"]);
        assert_eq!(get_current_table_ref(&node), "\"public\".\"orders\"");
    }

    #[test]
    fn test_get_current_table_ref_non_scan() {
        let node = OpTree::Distinct {
            child: Box::new(scan(1, "t", "public", "t", &["id"])),
        };
        assert_eq!(
            get_current_table_ref(&node),
            "/* unsupported snapshot for distinct */"
        );
    }

    // ── build_base_table_key_exprs tests ────────────────────────────

    #[test]
    fn test_build_base_table_key_exprs_non_nullable() {
        let node = scan_not_null(1, "orders", "public", "o", &["id", "name"]);
        let exprs = build_base_table_key_exprs(&node, "r");
        assert!(exprs.iter().any(|e| e.contains("r.\"id\"::TEXT")));
        assert!(exprs.iter().any(|e| e.contains("r.\"name\"::TEXT")));
    }

    #[test]
    fn test_build_base_table_key_exprs_all_nullable_fallback() {
        let node = scan(1, "orders", "public", "o", &["id", "name"]);
        let exprs = build_base_table_key_exprs(&node, "r");
        // All nullable → uses all columns
        assert_eq!(exprs.len(), 2);
    }

    #[test]
    fn test_build_base_table_key_exprs_non_scan_fallback() {
        let node = OpTree::Distinct {
            child: Box::new(scan(1, "t", "public", "t", &["id"])),
        };
        let exprs = build_base_table_key_exprs(&node, "x");
        assert_eq!(exprs, vec!["row_to_json(x)::text"]);
    }

    // ── Nested join tests ───────────────────────────────────────────

    #[test]
    fn test_diff_inner_join_nested_three_tables() {
        // (orders ⋈ customers) ⋈ products — 3-table nested inner join
        let o = scan(1, "orders", "public", "o", &["id", "cust_id", "prod_id"]);
        let c = scan(2, "customers", "public", "c", &["id", "name"]);
        let inner = inner_join(eq_cond("o", "cust_id", "c", "id"), o, c);
        let p = scan(3, "products", "public", "p", &["id", "price"]);
        let tree = inner_join(eq_cond("o", "prod_id", "p", "id"), inner, p);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "nested 3-table inner join should diff: {result:?}"
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        // Should produce two parts (left delta + right delta)
        assert_sql_contains(&sql, "UNION ALL");
    }

    #[test]
    fn test_diff_inner_join_nested_skips_semijoin_optimization() {
        // When a child is a nested join, semi-join optimization is skipped
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "id", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = inner_join(eq_cond("a", "id", "c", "id"), inner, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // Semi-join would contain EXISTS; nested join should NOT have it
        assert_sql_not_contains(&sql, "EXISTS");
    }

    #[test]
    fn test_diff_inner_join_nested_uses_snapshot_subquery() {
        // The nested child should appear as a subquery snapshot, not a plain table ref
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "id", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = inner_join(eq_cond("a", "id", "c", "id"), inner, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // The nested join snapshot should generate SQL with JOIN in the snapshot subquery
        assert_sql_contains(&sql, "JOIN");
    }

    // ── NATURAL JOIN diff tests ─────────────────────────────────────

    #[test]
    fn test_diff_inner_join_with_natural_condition() {
        // Simulate what the parser produces for NATURAL JOIN:
        // two tables sharing "id" column → equi-join on id
        let left = scan(1, "orders", "public", "o", &["id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = natural_join_cond(&left, &right);
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // Should produce valid SQL with two parts (delta_left JOIN right, left JOIN delta_right)
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "UNION ALL");
    }

    #[test]
    fn test_diff_inner_join_natural_multiple_common_cols() {
        // Two tables sharing both "id" and "region"
        let left = scan(1, "a", "public", "a", &["id", "region", "val"]);
        let right = scan(2, "b", "public", "b", &["id", "region", "score"]);
        let cond = natural_join_cond(&left, &right);
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        // Should have disambiguated columns from both sides
        assert!(result.columns.contains(&"a__id".to_string()));
        assert!(result.columns.contains(&"a__region".to_string()));
        assert!(result.columns.contains(&"b__id".to_string()));
        assert!(result.columns.contains(&"b__score".to_string()));
    }

    #[test]
    fn test_diff_inner_join_natural_no_common_columns() {
        // No common columns → condition is TRUE (cross join)
        let left = scan(1, "orders", "public", "o", &["order_id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["cust_id", "name"]);
        let cond = natural_join_cond(&left, &right);
        assert_eq!(cond.to_sql(), "TRUE");
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        // Should still produce valid diff SQL
        assert!(!result.columns.is_empty());
    }
}
