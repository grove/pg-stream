//! DISTINCT differentiation.
//!
//! Modeled as GROUP BY ALL with COUNT(*). Rows appear/disappear
//! when their multiplicity count crosses the 0 boundary.
//!
//! A row with count 0→N means INSERT; N→0 means DELETE.
//! Changes that don't cross 0 are suppressed.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::scan::build_hash_expr;
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate a Distinct node.
///
/// Strategy: track multiplicity count per unique row. Only emit row when
/// count transitions through 0.
pub fn diff_distinct(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::Distinct { child } = op else {
        return Err(PgStreamError::InternalError(
            "diff_distinct called on non-Distinct node".into(),
        ));
    };

    let child_result = ctx.diff_node(child)?;

    let cols = &child_result.columns;
    let col_refs: Vec<String> = cols.iter().map(|c| quote_ident(c)).collect();
    let col_list = col_refs.join(", ");

    // Hash all columns for the row_id
    let hash_exprs: Vec<String> = cols
        .iter()
        .map(|c| format!("{}::TEXT", quote_ident(c)))
        .collect();
    let row_id_expr = build_hash_expr(&hash_exprs);

    let st_table = ctx
        .st_qualified_name
        .clone()
        .unwrap_or_else(|| "/* st_table */".to_string());

    // CTE 1: Compute per-row net change (insert_count - delete_count)
    let delta_cte = ctx.next_cte_name("dist_delta");
    let delta_sql = format!(
        "SELECT {row_id_expr} AS __pgs_row_id,\n\
         {col_list},\n\
         SUM(CASE WHEN __pgs_action = 'I' THEN 1 ELSE -1 END) AS __net_count\n\
         FROM {child_cte}\n\
         GROUP BY {col_list}",
        child_cte = child_result.cte_name,
    );
    ctx.add_cte(delta_cte.clone(), delta_sql);

    // CTE 2: Merge with ST state to find boundary crossings
    let merge_cte = ctx.next_cte_name("dist_merge");

    // Join on row_id to get old count
    let merge_sql = format!(
        "SELECT d.__pgs_row_id,\n\
         {d_cols},\n\
         d.__net_count,\n\
         COALESCE(st.__pgs_count, 0) AS old_count,\n\
         COALESCE(st.__pgs_count, 0) + d.__net_count AS new_count\n\
         FROM {delta_cte} d\n\
         LEFT JOIN {st_table} st ON st.__pgs_row_id = d.__pgs_row_id",
        d_cols = cols
            .iter()
            .map(|c| format!("d.{}", quote_ident(c)))
            .collect::<Vec<_>>()
            .join(", "),
    );
    ctx.add_cte(merge_cte.clone(), merge_sql);

    // CTE 3: Emit INSERT for 0→N, DELETE for N→0, UPDATE for count changes
    let final_cte = ctx.next_cte_name("dist_final");
    let final_sql = format!(
        "\
-- Row appears: was absent, now present
SELECT __pgs_row_id, 'I' AS __pgs_action,
       {col_list}, new_count AS __pgs_count
FROM {merge_cte}
WHERE old_count <= 0 AND new_count > 0

UNION ALL

-- Row vanishes: was present, now absent
SELECT __pgs_row_id, 'D' AS __pgs_action,
       {col_list}, 0 AS __pgs_count
FROM {merge_cte}
WHERE old_count > 0 AND new_count <= 0

UNION ALL

-- Count changed but row still present: update the stored count
-- (emitted as 'I' so the MERGE applies UPDATE SET __pgs_count = new_count)
SELECT __pgs_row_id, 'I' AS __pgs_action,
       {col_list}, new_count AS __pgs_count
FROM {merge_cte}
WHERE old_count > 0 AND new_count > 0 AND new_count != old_count",
    );
    ctx.add_cte(final_cte.clone(), final_sql);

    let mut output_cols = cols.clone();
    output_cols.push("__pgs_count".to_string());

    Ok(DiffResult {
        cte_name: final_cte,
        columns: output_cols,
        is_deduplicated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    #[test]
    fn test_diff_distinct_basic() {
        let mut ctx = test_ctx_with_st("public", "my_st");
        let child = scan(1, "t", "public", "t", &["id", "name"]);
        let tree = distinct(child);
        let result = diff_distinct(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Output columns should include user cols + __pgs_count
        assert!(result.columns.contains(&"id".to_string()));
        assert!(result.columns.contains(&"name".to_string()));
        assert!(result.columns.contains(&"__pgs_count".to_string()));

        // Should contain the 3-CTE pattern: delta, merge, final
        assert_sql_contains(&sql, "__net_count");
        assert_sql_contains(&sql, "old_count");
        assert_sql_contains(&sql, "new_count");
    }

    #[test]
    fn test_diff_distinct_hash_row_id() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["a", "b"]);
        let tree = distinct(child);
        let result = diff_distinct(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Row ID is hash of all columns
        assert_sql_contains(&sql, "pgstream.pg_stream_hash");
    }

    #[test]
    fn test_diff_distinct_boundary_crossings() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["val"]);
        let tree = distinct(child);
        let result = diff_distinct(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should emit INSERT when 0→N (appear)
        assert_sql_contains(&sql, "old_count <= 0 AND new_count > 0");
        // Should emit DELETE when N→0 (vanish)
        assert_sql_contains(&sql, "old_count > 0 AND new_count <= 0");
    }

    #[test]
    fn test_diff_distinct_not_deduplicated() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["x"]);
        let tree = distinct(child);
        let result = diff_distinct(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_distinct_error_on_non_distinct_node() {
        let mut ctx = test_ctx_with_st("public", "st");
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_distinct(&mut ctx, &tree);
        assert!(result.is_err());
    }
}
