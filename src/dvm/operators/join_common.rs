//! Shared helpers for join differentiation operators.
//!
//! Provides snapshot SQL generation and condition rewriting that handle
//! both simple (Scan) and nested (join-of-join) children correctly.
//!
//! ## Nested join handling
//!
//! When a join child is itself a join (e.g., `(A ⋈ B) ⋈ C`), two things
//! differ from the simple binary case:
//!
//! 1. **Snapshot**: The "current state" of the left/right child is not
//!    a plain table reference but a subquery: `(SELECT a."id" AS "a__id",
//!    ... FROM a JOIN b ON ...)`.
//!
//! 2. **Condition rewriting**: The join condition references original
//!    table aliases (e.g., `o.prod_id = p.id`). For nested children,
//!    these aliases are *inside* the snapshot subquery and must be
//!    translated to disambiguated column names (e.g., `l."o__prod_id"`).

use crate::dvm::diff::quote_ident;
use crate::dvm::parser::{Expr, OpTree};

// ── Snapshot SQL generation ─────────────────────────────────────────────

/// Build a SQL expression for the current snapshot of an operator subtree.
///
/// For `Scan` nodes, returns the quoted `"schema"."table"` reference.
/// For join nodes, returns a parenthesized subquery with disambiguated
/// column names matching the diff engine's output format.
///
/// Used in join delta formulas where one side of the join must reference
/// the current full state of the other side.
pub fn build_snapshot_sql(op: &OpTree) -> String {
    match op {
        OpTree::Scan {
            schema, table_name, ..
        } => {
            format!(
                "\"{}\".\"{}\"",
                schema.replace('"', "\"\""),
                table_name.replace('"', "\"\""),
            )
        }
        OpTree::InnerJoin {
            condition,
            left,
            right,
        } => build_join_snapshot("JOIN", condition, left, right),
        OpTree::LeftJoin {
            condition,
            left,
            right,
        } => build_join_snapshot("LEFT JOIN", condition, left, right),
        OpTree::FullJoin {
            condition,
            left,
            right,
        } => build_join_snapshot("FULL JOIN", condition, left, right),
        OpTree::Filter { predicate, child } => {
            let child_snap = build_snapshot_sql(child);
            if matches!(child.as_ref(), OpTree::Scan { .. }) {
                let alias = child.alias();
                format!(
                    "(SELECT * FROM {} {} WHERE {})",
                    child_snap,
                    quote_ident(alias),
                    predicate.to_sql()
                )
            } else {
                // For non-Scan children (e.g. Filter over Join), the filter
                // is applied by diff_filter in the diff pipeline. The
                // snapshot represents the unfiltered child state.
                child_snap
            }
        }
        OpTree::Project { child, .. } | OpTree::Subquery { child, .. } => build_snapshot_sql(child),
        _ => {
            // Fallback for unsupported node types.
            format!("/* unsupported snapshot for {} */", op.node_kind())
        }
    }
}

/// Build a snapshot subquery for a join node.
///
/// Produces a parenthesized SELECT with disambiguated column names:
/// ```sql
/// (SELECT l."id" AS "l__id", ..., r."id" AS "r__id", ...
///  FROM left_snap l JOIN right_snap r ON condition)
/// ```
fn build_join_snapshot(join_type: &str, condition: &Expr, left: &OpTree, right: &OpTree) -> String {
    let left_snap = build_snapshot_sql(left);
    let right_snap = build_snapshot_sql(right);
    let left_alias = left.alias();
    let right_alias = right.alias();

    let left_cols = left.output_columns();
    let right_cols = right.output_columns();

    let mut select_parts = Vec::new();
    for c in &left_cols {
        select_parts.push(format!(
            "{}.{} AS {}",
            quote_ident(left_alias),
            quote_ident(c),
            quote_ident(&format!("{left_alias}__{c}"))
        ));
    }
    for c in &right_cols {
        select_parts.push(format!(
            "{}.{} AS {}",
            quote_ident(right_alias),
            quote_ident(c),
            quote_ident(&format!("{right_alias}__{c}"))
        ));
    }

    // Rewrite condition for snapshot: use child aliases directly
    let cond_sql = rewrite_join_condition(condition, left, left_alias, right, right_alias);

    format!(
        "(SELECT {} FROM {} {} {} {} {} ON {})",
        select_parts.join(", "),
        left_snap,
        quote_ident(left_alias),
        join_type,
        right_snap,
        quote_ident(right_alias),
        cond_sql
    )
}

// ── Condition rewriting ─────────────────────────────────────────────────

/// Rewrite a join condition for use in a delta or snapshot query.
///
/// Replaces original table alias references with the provided new aliases,
/// handling nested joins by disambiguating column names with the original
/// table alias prefix when the source table is inside a nested join child.
///
/// For a simple case (Scan child), `o.cust_id` → `dl."cust_id"`.
/// For a nested case (Join child), `o.cust_id` → `dl."o__cust_id"`.
pub fn rewrite_join_condition(
    condition: &Expr,
    left: &OpTree,
    new_left: &str,
    right: &OpTree,
    new_right: &str,
) -> String {
    rewrite_expr_for_join(condition, left, new_left, right, new_right).to_sql()
}

/// Recursively rewrite an expression for join delta/snapshot usage.
fn rewrite_expr_for_join(
    expr: &Expr,
    left: &OpTree,
    new_left: &str,
    right: &OpTree,
    new_right: &str,
) -> Expr {
    match expr {
        Expr::ColumnRef {
            table_alias: Some(alias),
            column_name,
        } => {
            if has_source_alias(left, alias) {
                if is_simple_source(left, alias) {
                    // Direct table access — just remap the alias
                    Expr::ColumnRef {
                        table_alias: Some(new_left.to_string()),
                        column_name: column_name.clone(),
                    }
                } else {
                    // Table is inside a nested join — column is disambiguated
                    Expr::ColumnRef {
                        table_alias: Some(new_left.to_string()),
                        column_name: format!("{alias}__{column_name}"),
                    }
                }
            } else if has_source_alias(right, alias) {
                if is_simple_source(right, alias) {
                    Expr::ColumnRef {
                        table_alias: Some(new_right.to_string()),
                        column_name: column_name.clone(),
                    }
                } else {
                    Expr::ColumnRef {
                        table_alias: Some(new_right.to_string()),
                        column_name: format!("{alias}__{column_name}"),
                    }
                }
            } else {
                // Alias not found in either child — pass through unchanged
                expr.clone()
            }
        }
        Expr::BinaryOp {
            op,
            left: l,
            right: r,
        } => Expr::BinaryOp {
            op: op.clone(),
            left: Box::new(rewrite_expr_for_join(l, left, new_left, right, new_right)),
            right: Box::new(rewrite_expr_for_join(r, left, new_left, right, new_right)),
        },
        Expr::FuncCall { func_name, args } => Expr::FuncCall {
            func_name: func_name.clone(),
            args: args
                .iter()
                .map(|a| rewrite_expr_for_join(a, left, new_left, right, new_right))
                .collect(),
        },
        _ => expr.clone(),
    }
}

/// Check if an OpTree contains a source table with the given alias.
///
/// Descends into join children, filters, projects, and subqueries to
/// find whether a specific table alias is accessible from this subtree.
pub fn has_source_alias(op: &OpTree, alias: &str) -> bool {
    match op {
        OpTree::Scan { alias: a, .. } => a == alias,
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => {
            has_source_alias(left, alias) || has_source_alias(right, alias)
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => has_source_alias(child, alias),
        _ => false,
    }
}

/// Check if a table alias is directly accessible (no column disambiguation needed).
///
/// Returns `true` if the alias corresponds to a `Scan` that IS the node
/// or is wrapped only by transparent operators (Filter, Project, Subquery).
/// Returns `false` if the alias is inside a nested join, meaning columns
/// are prefixed with the original table alias.
pub fn is_simple_source(op: &OpTree, alias: &str) -> bool {
    match op {
        OpTree::Scan { alias: a, .. } => a == alias,
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => is_simple_source(child, alias),
        // For joins, the alias is inside the join — needs disambiguation
        _ => false,
    }
}

/// Check if a child is a "simple source" (Scan or transparent wrapper over Scan).
///
/// Used to determine if semi-join optimization can be applied — the
/// optimization requires filtering a plain table, not a complex subquery.
pub fn is_simple_child(op: &OpTree) -> bool {
    match op {
        OpTree::Scan { .. } => true,
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => is_simple_child(child),
        _ => false,
    }
}

/// Build per-column `alias."col"::TEXT` expressions for the base-table
/// side of a join, suitable for inclusion in a flat
/// `pg_stream_hash_multi(ARRAY[...])` call.
///
/// For `Scan` nodes this uses the PK (non-nullable) columns; for
/// non-Scan children it falls back to `row_to_json(alias)::text`.
pub fn build_base_table_key_exprs(op: &OpTree, alias: &str) -> Vec<String> {
    match op {
        OpTree::Scan { columns, .. } => {
            let non_nullable: Vec<&str> = columns
                .iter()
                .filter(|c| !c.is_nullable)
                .map(|c| c.name.as_str())
                .collect();

            let key_cols: Vec<&str> = if non_nullable.is_empty() {
                columns.iter().map(|c| c.name.as_str()).collect()
            } else {
                non_nullable
            };

            key_cols
                .iter()
                .map(|c| format!("{alias}.{}::TEXT", quote_ident(c)))
                .collect()
        }
        _ => {
            // Non-Scan child: fall back to row_to_json
            vec![format!("row_to_json({alias})::text")]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    // ── build_snapshot_sql tests ────────────────────────────────

    #[test]
    fn test_snapshot_scan() {
        let node = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let snap = build_snapshot_sql(&node);
        assert_eq!(snap, "\"public\".\"orders\"");
    }

    #[test]
    fn test_snapshot_inner_join() {
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let node = inner_join(cond, left, right);
        let snap = build_snapshot_sql(&node);

        // Should be a subquery with disambiguated column names
        assert!(snap.starts_with('('));
        assert!(snap.contains("\"o__id\""));
        assert!(snap.contains("\"o__cust_id\""));
        assert!(snap.contains("\"c__id\""));
        assert!(snap.contains("\"c__name\""));
        assert!(snap.contains("JOIN"));
    }

    #[test]
    fn test_snapshot_left_join() {
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let node = left_join(cond, left, right);
        let snap = build_snapshot_sql(&node);

        assert!(snap.contains("LEFT JOIN"));
    }

    #[test]
    fn test_snapshot_nested_join() {
        // (orders o JOIN customers c) JOIN products p
        let o = scan(1, "orders", "public", "o", &["id", "cust_id", "prod_id"]);
        let c = scan(2, "customers", "public", "c", &["id", "name"]);
        let inner = inner_join(eq_cond("o", "cust_id", "c", "id"), o, c);
        let p = scan(3, "products", "public", "p", &["id", "price"]);
        let outer = inner_join(eq_cond("o", "prod_id", "p", "id"), inner, p);

        let snap = build_snapshot_sql(&outer);
        // Outer join should reference a subquery for the inner join
        assert!(snap.contains("\"join\""));
        assert!(snap.contains("\"p\""));
    }

    #[test]
    fn test_snapshot_filter_over_scan() {
        let child = scan(1, "t", "public", "t", &["id", "status"]);
        let node = filter(binop("=", qcolref("t", "status"), lit("'active'")), child);
        let snap = build_snapshot_sql(&node);
        assert!(snap.contains("SELECT *"));
        assert!(snap.contains("WHERE"));
    }

    // ── has_source_alias tests ──────────────────────────────────

    #[test]
    fn test_has_source_alias_scan() {
        let node = scan(1, "orders", "public", "o", &["id"]);
        assert!(has_source_alias(&node, "o"));
        assert!(!has_source_alias(&node, "x"));
    }

    #[test]
    fn test_has_source_alias_nested_join() {
        let o = scan(1, "orders", "public", "o", &["id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let node = inner_join(eq_cond("o", "id", "c", "id"), o, c);
        assert!(has_source_alias(&node, "o"));
        assert!(has_source_alias(&node, "c"));
        assert!(!has_source_alias(&node, "x"));
    }

    // ── is_simple_source tests ──────────────────────────────────

    #[test]
    fn test_is_simple_source_scan() {
        let node = scan(1, "t", "public", "t", &["id"]);
        assert!(is_simple_source(&node, "t"));
    }

    #[test]
    fn test_is_simple_source_filter_over_scan() {
        let node = filter(lit("TRUE"), scan(1, "t", "public", "t", &["id"]));
        assert!(is_simple_source(&node, "t"));
    }

    #[test]
    fn test_is_simple_source_nested_join() {
        let o = scan(1, "orders", "public", "o", &["id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let node = inner_join(eq_cond("o", "id", "c", "id"), o, c);
        // "o" is inside a join → not simple
        assert!(!is_simple_source(&node, "o"));
        assert!(!is_simple_source(&node, "c"));
    }

    // ── rewrite_join_condition tests ────────────────────────────

    #[test]
    fn test_rewrite_simple_condition() {
        let o = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let cond = eq_cond("o", "cust_id", "c", "id");

        let rewritten = rewrite_join_condition(&cond, &o, "dl", &c, "r");
        assert!(rewritten.contains("dl."));
        assert!(rewritten.contains("r."));
    }

    #[test]
    fn test_rewrite_nested_condition() {
        // Outer join: (orders ⋈ customers) ⋈ products
        // Condition: o.prod_id = p.id
        let o = scan(1, "orders", "public", "o", &["id", "prod_id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let inner = inner_join(eq_cond("o", "id", "c", "id"), o, c);
        let p = scan(3, "products", "public", "p", &["id"]);

        let cond = eq_cond("o", "prod_id", "p", "id");
        let rewritten = rewrite_join_condition(&cond, &inner, "dl", &p, "r");

        // "o" is inside the inner join → disambiguated to "o__prod_id"
        assert!(
            rewritten.contains("o__prod_id"),
            "expected o__prod_id, got: {rewritten}"
        );
        // "p" is a simple Scan → plain "id"
        assert!(rewritten.contains("r."));
    }

    #[test]
    fn test_rewrite_both_sides_nested() {
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let left = inner_join(eq_cond("a", "id", "b", "id"), a, b);

        let c = scan(3, "c", "public", "c", &["id"]);
        let d = scan(4, "d", "public", "d", &["id"]);
        let right = inner_join(eq_cond("c", "id", "d", "id"), c, d);

        let cond = eq_cond("a", "id", "c", "id");
        let rewritten = rewrite_join_condition(&cond, &left, "dl", &right, "r");
        assert!(
            rewritten.contains("a__id"),
            "expected a__id, got: {rewritten}"
        );
        assert!(
            rewritten.contains("c__id"),
            "expected c__id, got: {rewritten}"
        );
    }

    // ── is_simple_child tests ───────────────────────────────────

    #[test]
    fn test_is_simple_child_scan() {
        assert!(is_simple_child(&scan(1, "t", "public", "t", &["id"])));
    }

    #[test]
    fn test_is_simple_child_filter_over_scan() {
        let node = filter(lit("TRUE"), scan(1, "t", "public", "t", &["id"]));
        assert!(is_simple_child(&node));
    }

    #[test]
    fn test_is_simple_child_join() {
        let o = scan(1, "a", "public", "a", &["id"]);
        let c = scan(2, "b", "public", "b", &["id"]);
        let node = inner_join(eq_cond("a", "id", "b", "id"), o, c);
        assert!(!is_simple_child(&node));
    }

    // ── build_base_table_key_exprs tests ────────────────────────

    #[test]
    fn test_key_exprs_scan_non_nullable() {
        let node = scan_not_null(1, "orders", "public", "o", &["id", "name"]);
        let exprs = build_base_table_key_exprs(&node, "r");
        assert!(exprs.iter().any(|e| e.contains("r.\"id\"::TEXT")));
    }

    #[test]
    fn test_key_exprs_non_scan_fallback() {
        let o = scan(1, "a", "public", "a", &["id"]);
        let c = scan(2, "b", "public", "b", &["id"]);
        let node = inner_join(eq_cond("a", "id", "b", "id"), o, c);
        let exprs = build_base_table_key_exprs(&node, "l");
        assert_eq!(exprs, vec!["row_to_json(l)::text"]);
    }
}
