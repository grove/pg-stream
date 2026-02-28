//! Scalar subquery differentiation.
//!
//! Handles `SELECT (SELECT agg(...) FROM inner_src) AS alias, ... FROM outer_src`.
//!
//! The scalar subquery produces a single value that is effectively cross-joined
//! to every row from the outer child. When the inner source changes, ALL output
//! rows change (the scalar value is different). When the outer source changes,
//! only the changed rows are affected (the scalar value stays the same).
//!
//! ## Delta strategy
//!
//! **Part 1 — outer child changes only** (inner source unchanged):
//!   The scalar value is constant. Pass through delta_outer rows, appending
//!   the current scalar subquery result as the extra column.
//!
//! **Part 2 — inner source changes** (scalar value changes):
//!   Every row in the outer child needs to be re-emitted with the new scalar
//!   value. We emit DELETE for all current outer rows (with old scalar) and
//!   INSERT for all current outer rows (with new scalar).
//!   This is a full recomputation of the outer side when the scalar changes.
//!
//! Optimization: if only the outer source changed and the inner source is
//! stable, Part 2 is skipped entirely. The diff engine already handles this
//! because diff_node on a stable subtree produces zero delta rows.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join_common::build_snapshot_sql;
use crate::dvm::parser::OpTree;
use crate::error::PgTrickleError;

/// Differentiate a ScalarSubquery node.
pub fn diff_scalar_subquery(
    ctx: &mut DiffContext,
    op: &OpTree,
) -> Result<DiffResult, PgTrickleError> {
    let OpTree::ScalarSubquery {
        subquery,
        alias,
        child,
        ..
    } = op
    else {
        return Err(PgTrickleError::InternalError(
            "diff_scalar_subquery called on non-ScalarSubquery node".into(),
        ));
    };

    // Differentiate both the outer child and the inner subquery
    let child_result = ctx.diff_node(child)?;
    let subquery_result = ctx.diff_node(subquery)?;

    let child_cols = &child_result.columns;
    let child_table = build_snapshot_sql(child);

    // Output columns: child columns + scalar alias
    let mut output_cols = child_cols.clone();
    output_cols.push(alias.clone());

    let dc_col_refs: Vec<String> = child_cols
        .iter()
        .map(|c| format!("dc.{}", quote_ident(c)))
        .collect();

    let cs_col_refs: Vec<String> = child_cols
        .iter()
        .map(|c| format!("cs.{}", quote_ident(c)))
        .collect();

    // Build the scalar subquery SQL that computes the current value
    // The subquery is an aggregate, so we reconstruct it from the subquery's snapshot
    let subquery_snapshot = build_snapshot_sql(subquery);
    let subquery_alias = subquery.alias();
    let subquery_cols = &subquery_result.columns;
    let scalar_col = if subquery_cols.is_empty() {
        "NULL".to_string()
    } else {
        // The scalar subquery should produce exactly one column
        subquery_cols[0].clone()
    };

    let scalar_sql = format!(
        "(SELECT {sq_alias}.{scalar_col} FROM {subquery_snapshot} {sq_alias} LIMIT 1)",
        scalar_col = quote_ident(&scalar_col),
        sq_alias = quote_ident(subquery_alias),
    );

    let cte_name = ctx.next_cte_name("scalar_sub");

    // Part 1: outer child delta with current scalar value appended
    // Part 2: if inner subquery changed, all outer rows with new vs old scalar
    //
    // For Part 2, we need both old and new scalar values.
    // Old scalar = computed from R_old (before changes).
    // New scalar = computed from R_current (after changes).
    // We emit a DELETE for each outer row (old scalar) and INSERT (new scalar).
    //
    // Build R_old for the scalar subquery
    let sq_col_list: String = subquery_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");

    let scalar_old_sql = if subquery_cols.is_empty() {
        "NULL".to_string()
    } else {
        let r_old_snapshot = format!(
            "(SELECT {sq_col_list} FROM {subquery_snapshot} {sq_alias} \
             EXCEPT ALL \
             SELECT {sq_col_list} FROM {delta_sq} WHERE __pgt_action = 'I' \
             UNION ALL \
             SELECT {sq_col_list} FROM {delta_sq} WHERE __pgt_action = 'D')",
            sq_alias = quote_ident(subquery_alias),
            delta_sq = subquery_result.cte_name,
        );
        format!(
            "(SELECT {scalar_col} FROM {r_old_snapshot} sq_old LIMIT 1)",
            scalar_col = quote_ident(&scalar_col),
        )
    };

    // Hash for child rows
    let hash_child = {
        let key_exprs: Vec<String> = child_cols
            .iter()
            .map(|c| format!("cs.{}::TEXT", quote_ident(c)))
            .collect();
        format!(
            "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
            key_exprs.join(", ")
        )
    };

    let sql = format!(
        "\
-- Part 1: outer child delta rows with current scalar value
SELECT dc.__pgt_row_id,
       dc.__pgt_action,
       {dc_cols},
       {scalar_sql} AS {alias_ident}
FROM {delta_child} dc

UNION ALL

-- Part 2: all outer rows re-emitted when scalar subquery value changes (DELETE old)
SELECT {hash_child} AS __pgt_row_id,
       'D' AS __pgt_action,
       {cs_cols},
       {scalar_old_sql} AS {alias_ident}
FROM {child_snapshot} cs
WHERE EXISTS (SELECT 1 FROM {delta_subquery} WHERE 1=1)
  AND {scalar_sql} IS DISTINCT FROM {scalar_old_sql}

UNION ALL

-- Part 2b: all outer rows re-emitted when scalar subquery value changes (INSERT new)
SELECT {hash_child} AS __pgt_row_id,
       'I' AS __pgt_action,
       {cs_cols},
       {scalar_sql} AS {alias_ident}
FROM {child_snapshot} cs
WHERE EXISTS (SELECT 1 FROM {delta_subquery} WHERE 1=1)
  AND {scalar_sql} IS DISTINCT FROM {scalar_old_sql}",
        dc_cols = dc_col_refs.join(", "),
        cs_cols = cs_col_refs.join(", "),
        alias_ident = quote_ident(alias),
        delta_child = child_result.cte_name,
        delta_subquery = subquery_result.cte_name,
        child_snapshot = child_table,
    );

    ctx.add_cte(cte_name.clone(), sql);

    Ok(DiffResult {
        cte_name,
        columns: output_cols,
        is_deduplicated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    #[test]
    fn test_diff_scalar_subquery_basic() {
        let mut ctx = test_ctx();
        let outer = scan(1, "orders", "public", "o", &["id", "amount"]);
        let inner = scan(2, "config", "public", "c", &["tax_rate"]);
        let tree = OpTree::ScalarSubquery {
            subquery: Box::new(inner),
            alias: "current_tax".to_string(),
            subquery_source_oids: vec![2],
            child: Box::new(outer),
        };
        let result = diff_scalar_subquery(&mut ctx, &tree).unwrap();

        // Output should include child columns + scalar alias
        assert_eq!(result.columns, vec!["id", "amount", "current_tax"]);
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_scalar_subquery_sql_structure() {
        let mut ctx = test_ctx();
        let outer = scan(1, "orders", "public", "o", &["id"]);
        let inner = scan(2, "stats", "public", "s", &["avg_val"]);
        let tree = OpTree::ScalarSubquery {
            subquery: Box::new(inner),
            alias: "global_avg".to_string(),
            subquery_source_oids: vec![2],
            child: Box::new(outer),
        };
        let result = diff_scalar_subquery(&mut ctx, &tree).unwrap();

        let sql = ctx.build_with_query(&result.cte_name);
        assert!(sql.contains("Part 1"), "SQL should have Part 1");
        assert!(sql.contains("Part 2"), "SQL should have Part 2");
        assert!(
            sql.contains("IS DISTINCT FROM"),
            "Part 2 should check for scalar value change"
        );
    }

    #[test]
    fn test_diff_scalar_subquery_wrong_node_type() {
        let mut ctx = test_ctx();
        let scan_node = scan(1, "t", "public", "t", &["id"]);
        let result = diff_scalar_subquery(&mut ctx, &scan_node);
        assert!(result.is_err());
    }
}
