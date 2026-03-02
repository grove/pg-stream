//! Projection differentiation.
//!
//! ΔI(πE(Q)) = πE(ΔI(Q))
//!
//! Apply the same projection expressions to the child's delta.
//! Row ID and action columns are passed through unchanged, except
//! for join children where the row ID is recomputed from the projected
//! columns to match the full-refresh hash formula.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::scan::build_hash_expr;
use crate::dvm::parser::{Expr, OpTree, join_pk_expr_indices, unwrap_transparent};
use crate::error::PgTrickleError;

/// Differentiate a Project node.
pub fn diff_project(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
    let OpTree::Project {
        expressions,
        aliases,
        child,
    } = op
    else {
        return Err(PgTrickleError::InternalError(
            "diff_project called on non-Project node".into(),
        ));
    };

    // First, differentiate the child
    let child_result = ctx.diff_node(child)?;

    // The child CTE's column names. For join children, columns are
    // disambiguated as "table__col" (e.g., "l__id", "r__id").
    let child_cols = &child_result.columns;

    let cte_name = ctx.next_cte_name("project");

    // Build projected column expressions.
    // Qualified column references (e.g., l.id) need to be resolved to
    // the child CTE's disambiguated column names (e.g., "l__id") since
    // the child is a single CTE, not multiple tables.
    let proj_cols: Vec<String> = expressions
        .iter()
        .zip(aliases.iter())
        .map(|(expr, alias)| {
            let resolved_sql = resolve_expr_to_child(expr, child_cols);
            let alias_ident = quote_ident(alias);
            if resolved_sql == *alias {
                alias_ident
            } else {
                format!("{resolved_sql} AS {alias_ident}")
            }
        })
        .collect();

    // For join children, recompute __pgt_row_id from the projected columns.
    // The full refresh uses hash of all output columns for join queries,
    // so the delta must produce matching row IDs.
    //
    // For lateral function/subquery children, also recompute __pgt_row_id
    // from all projected columns. SRF expansions have no natural PK, so
    // the delta's row_id must match the full refresh's hash of the
    // projected output columns.
    //
    // We must reference the SOURCE column names (from child CTE), not the
    // aliases, since SQL doesn't allow referencing aliases defined in the
    // same SELECT clause.
    // Look through transparent wrappers (Filter, Subquery) to find the
    // underlying child node. Q15 has Project > Filter > InnerJoin — the
    // Filter must not prevent PK-based row_ids or lateral detection.
    let unwrapped = unwrap_transparent(child);
    let is_join_child = matches!(
        unwrapped,
        OpTree::InnerJoin { .. } | OpTree::LeftJoin { .. }
    );
    let is_lateral_child = matches!(
        unwrapped,
        OpTree::LateralFunction { .. } | OpTree::LateralSubquery { .. }
    );

    let row_id_select = if is_join_child {
        // Use PK-corresponding expressions for hashing — PK-based row_ids
        // are stable across value changes, avoiding hash mismatch when
        // both join sides change simultaneously.
        let pk_indices = join_pk_expr_indices(expressions, unwrapped);
        let hash_exprs: Vec<&Expr> = if pk_indices.is_empty() {
            // Fallback: hash all expressions (no PK info available)
            expressions.iter().collect()
        } else {
            pk_indices.iter().map(|&i| &expressions[i]).collect()
        };
        let hash_cols: Vec<String> = hash_exprs
            .iter()
            .map(|expr| resolve_expr_to_child(expr, child_cols))
            .collect();
        format!("{} AS __pgt_row_id", build_hash_expr(&hash_cols))
    } else if is_lateral_child {
        // Hash all projected columns — matches row_id_key_columns()
        // which returns all aliases for lateral children.
        let hash_cols: Vec<String> = expressions
            .iter()
            .map(|expr| resolve_expr_to_child(expr, child_cols))
            .collect();
        format!("{} AS __pgt_row_id", build_hash_expr(&hash_cols))
    } else {
        "__pgt_row_id".to_string()
    };

    let sql = format!(
        "SELECT {row_id_select}, __pgt_action, {proj_cols}\n\
         FROM {child_cte}",
        proj_cols = proj_cols.join(", "),
        child_cte = child_result.cte_name,
    );

    ctx.add_cte(cte_name.clone(), sql);

    Ok(DiffResult {
        cte_name,
        columns: aliases.clone(),
        is_deduplicated: child_result.is_deduplicated,
    })
}

/// Resolve an expression's column references to match the child CTE's
/// column names.
///
/// When the child is a join CTE, columns are disambiguated as
/// `"table__col"`. A qualified reference like `ColumnRef("l", "id")`
/// becomes `"l__id"` if that name exists in `child_cols`. Unqualified
/// references and non-ColumnRef expressions pass through unchanged.
fn resolve_expr_to_child(expr: &Expr, child_cols: &[String]) -> String {
    #[allow(clippy::match_same_arms)]
    match expr {
        Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name,
        } => {
            // Check if the child has a disambiguated column "tbl__col"
            let disambiguated = format!("{tbl}__{column_name}");
            if child_cols.contains(&disambiguated) {
                quote_ident(&disambiguated)
            } else {
                // Try nested join prefix: *__tbl__col
                let nested_suffix = format!("__{tbl}__{column_name}");
                let nested_match = child_cols.iter().find(|c| c.ends_with(&nested_suffix));
                if let Some(found) = nested_match {
                    quote_ident(found)
                } else if child_cols.contains(column_name) {
                    // No disambiguation needed — column name is unique
                    quote_ident(column_name)
                } else if child_cols.contains(tbl) && !child_cols.contains(column_name) {
                    // The "table" is actually a column in the child CTE — this
                    // happens with LATERAL SRF aliases (e.g., `e.value` where
                    // `e` is a single-column SRF output stored as column "e"
                    // in the child CTE). Resolve to the column name directly;
                    // for SRFs like jsonb_array_elements, `"e"` already holds
                    // the unwrapped value.
                    quote_ident(tbl)
                } else {
                    // Fallback: use original qualified form
                    expr.to_sql()
                }
            }
        }
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => {
            if child_cols.contains(column_name) {
                quote_ident(column_name)
            } else {
                // Suffix match: find column ending in __column_name
                let suffix = format!("__{column_name}");
                let matches: Vec<&String> =
                    child_cols.iter().filter(|c| c.ends_with(&suffix)).collect();
                if matches.len() == 1 {
                    quote_ident(matches[0])
                } else {
                    expr.to_sql()
                }
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = resolve_expr_to_child(left, child_cols);
            let r = resolve_expr_to_child(right, child_cols);
            format!("({l} {op} {r})")
        }
        Expr::FuncCall { func_name, args } => {
            let resolved_args: Vec<String> = args
                .iter()
                .map(|a| resolve_expr_to_child(a, child_cols))
                .collect();
            format!("{}({})", func_name, resolved_args.join(", "))
        }
        Expr::Raw(sql) => {
            // Best-effort: replace column refs in raw SQL
            crate::dvm::operators::filter::replace_column_refs_in_raw(sql, child_cols)
        }
        _ => expr.to_sql(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    #[test]
    fn test_diff_project_basic_columns() {
        let mut ctx = test_ctx();
        let child = scan(1, "t", "public", "t", &["id", "name", "amount"]);
        let tree = project(
            vec![colref("id"), colref("name")],
            vec!["id", "name"],
            child,
        );
        let result = diff_project(&mut ctx, &tree).unwrap();
        assert_eq!(result.columns, vec!["id", "name"]);
    }

    #[test]
    fn test_diff_project_passthrough_row_id() {
        let mut ctx = test_ctx();
        let child = scan(1, "t", "public", "t", &["id", "val"]);
        let tree = project(vec![colref("val")], vec!["val"], child);
        let result = diff_project(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Non-join child: __pgt_row_id passed through directly
        assert_sql_contains(&sql, "__pgt_row_id");
    }

    #[test]
    fn test_diff_project_alias_rename() {
        let mut ctx = test_ctx();
        let child = scan(1, "t", "public", "t", &["id", "amount"]);
        let tree = project(vec![colref("amount")], vec!["total"], child);
        let result = diff_project(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_eq!(result.columns, vec!["total"]);
        assert_sql_contains(&sql, "\"total\"");
    }

    #[test]
    fn test_diff_project_preserves_dedup_flag() {
        let mut ctx = test_ctx();
        ctx.merge_safe_dedup = true;
        let child = scan(1, "t", "public", "t", &["id", "val"]);
        let tree = project(vec![colref("val")], vec!["val"], child);
        let result = diff_project(&mut ctx, &tree).unwrap();
        assert!(result.is_deduplicated);
    }

    #[test]
    fn test_diff_project_error_on_non_project_node() {
        let mut ctx = test_ctx();
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_project(&mut ctx, &tree);
        assert!(result.is_err());
    }

    // ── resolve_expr_to_child tests ─────────────────────────────────

    #[test]
    fn test_resolve_expr_unqualified_column() {
        let child_cols = vec!["id".to_string(), "name".to_string()];
        let result = resolve_expr_to_child(&colref("id"), &child_cols);
        assert_eq!(result, "\"id\"");
    }

    #[test]
    fn test_resolve_expr_qualified_column_disambiguated() {
        let child_cols = vec!["l__id".to_string(), "r__id".to_string()];
        let result = resolve_expr_to_child(&qcolref("l", "id"), &child_cols);
        assert_eq!(result, "\"l__id\"");
    }

    #[test]
    fn test_resolve_expr_qualified_column_not_disambiguated() {
        let child_cols = vec!["id".to_string(), "name".to_string()];
        let result = resolve_expr_to_child(&qcolref("t", "id"), &child_cols);
        assert_eq!(result, "\"id\"");
    }

    #[test]
    fn test_resolve_expr_binary_op() {
        let child_cols = vec!["a".to_string(), "b".to_string()];
        let expr = binop("+", colref("a"), colref("b"));
        let result = resolve_expr_to_child(&expr, &child_cols);
        assert!(result.contains("\"a\""));
        assert!(result.contains("+"));
        assert!(result.contains("\"b\""));
    }

    #[test]
    fn test_resolve_expr_func_call() {
        let child_cols = vec!["x".to_string()];
        let expr = Expr::FuncCall {
            func_name: "upper".to_string(),
            args: vec![colref("x")],
        };
        let result = resolve_expr_to_child(&expr, &child_cols);
        assert_eq!(result, "upper(\"x\")");
    }
}
