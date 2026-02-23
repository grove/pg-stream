//! EXCEPT [ALL] differentiation.
//!
//! A row appears in the result when it exists in the left branch but
//! **not** (or with fewer multiplicity in) the right branch.
//!
//! - **EXCEPT** (set): row present if `count_L > 0 AND count_R = 0`.
//! - **EXCEPT ALL** (bag): row appears `GREATEST(0, count_L - count_R)` times.
//!
//! Delta strategy: same dual-count tracking as INTERSECT, but the
//! effective-count function is `GREATEST(0, count_L - count_R)`.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::scan::build_hash_expr;
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate an Except node.
pub fn diff_except(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::Except {
        left, right, all, ..
    } = op
    else {
        return Err(PgStreamError::InternalError(
            "diff_except called on non-Except node".into(),
        ));
    };

    // Differentiate both children
    let left_result = ctx.diff_node(left)?;
    let right_result = ctx.diff_node(right)?;

    let cols = &left_result.columns;
    let col_refs: Vec<String> = cols.iter().map(|c| quote_ident(c)).collect();
    let col_list = col_refs.join(", ");

    // Hash all columns for the row_id
    let hash_exprs: Vec<String> = cols
        .iter()
        .map(|c| format!("{}::TEXT", quote_ident(c)))
        .collect();
    let row_id_expr = build_hash_expr(&hash_exprs);

    let dt_table = ctx
        .dt_qualified_name
        .clone()
        .unwrap_or_else(|| "/* dt_table */".to_string());

    // CTE 1: Combine deltas from both branches, tagged with branch indicator
    let delta_cte = ctx.next_cte_name("exct_delta");
    let delta_sql = format!(
        "SELECT {row_id_expr} AS __pgs_row_id, {col_list},\n\
         SUM(CASE WHEN __pgs_action = 'I' THEN 1 ELSE -1 END) AS __net_count,\n\
         'L' AS __branch\n\
         FROM {left_cte}\n\
         GROUP BY {col_list}\n\
         \n\
         UNION ALL\n\
         \n\
         SELECT {row_id_expr} AS __pgs_row_id, {col_list},\n\
         SUM(CASE WHEN __pgs_action = 'I' THEN 1 ELSE -1 END) AS __net_count,\n\
         'R' AS __branch\n\
         FROM {right_cte}\n\
         GROUP BY {col_list}",
        left_cte = left_result.cte_name,
        right_cte = right_result.cte_name,
    );
    ctx.add_cte(delta_cte.clone(), delta_sql);

    // CTE 2: Merge with ST storage to compute old and new per-branch counts
    let merge_cte = ctx.next_cte_name("exct_merge");
    let d_cols = cols
        .iter()
        .map(|c| format!("d.{}", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");

    let merge_sql = format!(
        "SELECT d.__pgs_row_id, {d_cols},\n\
         COALESCE(st.__pgs_count_l, 0)\n\
             + SUM(CASE WHEN d.__branch = 'L' THEN d.__net_count ELSE 0 END) AS new_count_l,\n\
         COALESCE(st.__pgs_count_r, 0)\n\
             + SUM(CASE WHEN d.__branch = 'R' THEN d.__net_count ELSE 0 END) AS new_count_r,\n\
         COALESCE(st.__pgs_count_l, 0) AS old_count_l,\n\
         COALESCE(st.__pgs_count_r, 0) AS old_count_r\n\
         FROM {delta_cte} d\n\
         LEFT JOIN {dt_table} st ON st.__pgs_row_id = d.__pgs_row_id\n\
         GROUP BY d.__pgs_row_id, {d_cols}, st.__pgs_count_l, st.__pgs_count_r",
    );
    ctx.add_cte(merge_cte.clone(), merge_sql);

    // CTE 3: Detect boundary crossings using GREATEST(0, count_L - count_R)
    let final_cte = ctx.next_cte_name("exct_final");

    let final_sql = if *all {
        // EXCEPT ALL: effective count = GREATEST(0, count_L - count_R)
        format!(
            "\
-- Row appears: effective count was 0, now positive
SELECT __pgs_row_id, 'I' AS __pgs_action,
       {col_list}, new_count_l AS __pgs_count_l, new_count_r AS __pgs_count_r
FROM {merge_cte}
WHERE GREATEST(0, old_count_l - old_count_r) <= 0
  AND GREATEST(0, new_count_l - new_count_r) > 0

UNION ALL

-- Row vanishes: effective count was positive, now 0
SELECT __pgs_row_id, 'D' AS __pgs_action,
       {col_list}, 0 AS __pgs_count_l, 0 AS __pgs_count_r
FROM {merge_cte}
WHERE GREATEST(0, old_count_l - old_count_r) > 0
  AND GREATEST(0, new_count_l - new_count_r) <= 0

UNION ALL

-- Counts changed but row still present
SELECT __pgs_row_id, 'I' AS __pgs_action,
       {col_list}, new_count_l AS __pgs_count_l, new_count_r AS __pgs_count_r
FROM {merge_cte}
WHERE GREATEST(0, old_count_l - old_count_r) > 0
  AND GREATEST(0, new_count_l - new_count_r) > 0
  AND (new_count_l != old_count_l OR new_count_r != old_count_r)",
        )
    } else {
        // EXCEPT (set): effective count = count_L > 0 AND count_R = 0
        // We model this as GREATEST(0, count_L - count_R) crossing 0
        format!(
            "\
-- Row appears: was absent, now present
SELECT __pgs_row_id, 'I' AS __pgs_action,
       {col_list}, new_count_l AS __pgs_count_l, new_count_r AS __pgs_count_r
FROM {merge_cte}
WHERE GREATEST(0, old_count_l - old_count_r) <= 0
  AND GREATEST(0, new_count_l - new_count_r) > 0

UNION ALL

-- Row vanishes: was present, now absent
SELECT __pgs_row_id, 'D' AS __pgs_action,
       {col_list}, 0 AS __pgs_count_l, 0 AS __pgs_count_r
FROM {merge_cte}
WHERE GREATEST(0, old_count_l - old_count_r) > 0
  AND GREATEST(0, new_count_l - new_count_r) <= 0

UNION ALL

-- Counts changed but row still present (update stored counts)
SELECT __pgs_row_id, 'I' AS __pgs_action,
       {col_list}, new_count_l AS __pgs_count_l, new_count_r AS __pgs_count_r
FROM {merge_cte}
WHERE GREATEST(0, old_count_l - old_count_r) > 0
  AND GREATEST(0, new_count_l - new_count_r) > 0
  AND (new_count_l != old_count_l OR new_count_r != old_count_r)",
        )
    };
    ctx.add_cte(final_cte.clone(), final_sql);

    let mut output_cols = cols.clone();
    output_cols.push("__pgs_count_l".to_string());
    output_cols.push("__pgs_count_r".to_string());

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
    fn test_diff_except_basic() {
        let mut ctx = test_ctx_with_dt("public", "my_dt");
        let left = scan(1, "a", "public", "a", &["name"]);
        let right = scan(2, "b", "public", "b", &["name"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Output columns: user cols + dual counts
        assert!(result.columns.contains(&"name".to_string()));
        assert!(result.columns.contains(&"__pgs_count_l".to_string()));
        assert!(result.columns.contains(&"__pgs_count_r".to_string()));

        // Should contain the 3-CTE pattern
        assert_sql_contains(&sql, "exct_delta");
        assert_sql_contains(&sql, "exct_merge");
        assert_sql_contains(&sql, "exct_final");
    }

    #[test]
    fn test_diff_except_boundary_crossings() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["val"]);
        let right = scan(2, "b", "public", "b", &["val"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // EXCEPT uses GREATEST(0, count_L - count_R)
        assert_sql_contains(&sql, "GREATEST(0, old_count_l - old_count_r)");
        assert_sql_contains(&sql, "GREATEST(0, new_count_l - new_count_r)");
    }

    #[test]
    fn test_diff_except_all_basic() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["x"]);
        let right = scan(2, "b", "public", "b", &["x"]);
        let tree = except(left, right, true);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // EXCEPT ALL uses GREATEST like EXCEPT
        assert_sql_contains(&sql, "GREATEST(0, old_count_l - old_count_r)");
        assert_sql_contains(&sql, "GREATEST(0, new_count_l - new_count_r)");
    }

    #[test]
    fn test_diff_except_output_columns_include_dual_counts() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["id", "name"]);
        let right = scan(2, "b", "public", "b", &["id", "name"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();

        assert_eq!(
            result.columns,
            vec!["id", "name", "__pgs_count_l", "__pgs_count_r"]
        );
    }

    #[test]
    fn test_diff_except_is_not_commutative() {
        // EXCEPT is L - R, so swapping branches should produce different SQL
        let mut ctx1 = test_ctx_with_dt("public", "dt");
        let tree1 = except(
            scan(1, "a", "public", "a", &["x"]),
            scan(2, "b", "public", "b", &["x"]),
            false,
        );
        let r1 = diff_except(&mut ctx1, &tree1).unwrap();
        let sql1 = ctx1.build_with_query(&r1.cte_name);

        let mut ctx2 = test_ctx_with_dt("public", "dt");
        let tree2 = except(
            scan(2, "b", "public", "b", &["x"]),
            scan(1, "a", "public", "a", &["x"]),
            false,
        );
        let r2 = diff_except(&mut ctx2, &tree2).unwrap();
        let sql2 = ctx2.build_with_query(&r2.cte_name);

        // The left-branch CTE should differ (different tables)
        // Both should still have GREATEST-based logic
        assert_sql_contains(&sql1, "GREATEST");
        assert_sql_contains(&sql2, "GREATEST");
        // They should not be identical — different scan ordering
        assert_ne!(sql1, sql2);
    }

    #[test]
    fn test_diff_except_hash_row_id() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["x"]);
        let right = scan(2, "b", "public", "b", &["x"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_sql_contains(&sql, "pgstream.pg_stream_hash");
    }

    #[test]
    fn test_diff_except_not_deduplicated() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["x"]);
        let right = scan(2, "b", "public", "b", &["x"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_except_error_on_non_except_node() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_except(&mut ctx, &tree);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_except_branch_tagging() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["val"]);
        let right = scan(2, "b", "public", "b", &["val"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_sql_contains(&sql, "'L' AS __branch");
        assert_sql_contains(&sql, "'R' AS __branch");
    }

    #[test]
    fn test_diff_except_multi_column() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["id", "name", "status"]);
        let right = scan(2, "b", "public", "b", &["id", "name", "status"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // All user columns appear in output
        assert_eq!(
            result.columns,
            vec!["id", "name", "status", "__pgs_count_l", "__pgs_count_r"]
        );
        // All user columns are quoted in GROUP BY
        assert_sql_contains(&sql, "\"id\", \"name\", \"status\"");
    }

    #[test]
    fn test_diff_except_storage_table_join() {
        let mut ctx = test_ctx_with_dt("myschema", "my_stream");
        let left = scan(1, "a", "public", "a", &["x"]);
        let right = scan(2, "b", "public", "b", &["x"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Merge CTE should join against the storage table
        assert_sql_contains(&sql, "\"myschema\".\"my_stream\"");
        assert_sql_contains(&sql, "st.__pgs_count_l");
        assert_sql_contains(&sql, "st.__pgs_count_r");
        assert_sql_contains(&sql, "LEFT JOIN");
    }

    #[test]
    fn test_diff_except_delete_action_zeros_counts() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["x"]);
        let right = scan(2, "b", "public", "b", &["x"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // DELETE rows should emit 0 for both counts
        assert_sql_contains(&sql, "'D' AS __pgs_action");
        assert_sql_contains(&sql, "0 AS __pgs_count_l, 0 AS __pgs_count_r");
    }

    #[test]
    fn test_diff_except_all_update_on_count_change() {
        // EXCEPT ALL should emit an update (as INSERT) when counts change
        // but the row is still in the effective result
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["x"]);
        let right = scan(2, "b", "public", "b", &["x"]);
        let tree = except(left, right, true);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_sql_contains(
            &sql,
            "new_count_l != old_count_l OR new_count_r != old_count_r",
        );
    }

    #[test]
    fn test_diff_except_net_count_aggregation() {
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "a", "public", "a", &["x"]);
        let right = scan(2, "b", "public", "b", &["x"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Delta CTE aggregates net counts: INSERT=+1, DELETE=-1
        assert_sql_contains(
            &sql,
            "SUM(CASE WHEN __pgs_action = 'I' THEN 1 ELSE -1 END) AS __net_count",
        );
    }

    #[test]
    fn test_diff_except_set_and_all_both_use_greatest() {
        // Both EXCEPT and EXCEPT ALL use GREATEST(0, L-R) boundary detection
        let mut ctx_set = test_ctx_with_dt("public", "dt");
        let tree_set = except(
            scan(1, "a", "public", "a", &["x"]),
            scan(2, "b", "public", "b", &["x"]),
            false,
        );
        let r_set = diff_except(&mut ctx_set, &tree_set).unwrap();
        let sql_set = ctx_set.build_with_query(&r_set.cte_name);

        let mut ctx_all = test_ctx_with_dt("public", "dt");
        let tree_all = except(
            scan(1, "a", "public", "a", &["x"]),
            scan(2, "b", "public", "b", &["x"]),
            true,
        );
        let r_all = diff_except(&mut ctx_all, &tree_all).unwrap();
        let sql_all = ctx_all.build_with_query(&r_all.cte_name);

        // Both should use GREATEST
        assert_sql_contains(&sql_set, "GREATEST(0, old_count_l - old_count_r)");
        assert_sql_contains(&sql_all, "GREATEST(0, old_count_l - old_count_r)");
        assert_sql_contains(&sql_set, "GREATEST(0, new_count_l - new_count_r)");
        assert_sql_contains(&sql_all, "GREATEST(0, new_count_l - new_count_r)");

        // Output columns are identical
        assert_eq!(r_set.columns, r_all.columns);
    }

    #[test]
    fn test_diff_except_left_branch_is_positive() {
        // Verify the left branch contributes positively (L count increases
        // the effective result) and right branch contributes negatively
        let mut ctx = test_ctx_with_dt("public", "dt");
        let left = scan(1, "orders", "public", "orders", &["id"]);
        let right = scan(2, "cancelled", "public", "cancelled", &["id"]);
        let tree = except(left, right, false);
        let result = diff_except(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // L branch feeds the positive side, R branch the negative side
        // in GREATEST(0, count_L - count_R)
        assert_sql_contains(&sql, "new_count_l - new_count_r");
        assert_sql_contains(&sql, "old_count_l - old_count_r");
    }
}
