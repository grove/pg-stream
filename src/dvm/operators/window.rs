//! Window function differentiation via partition-based recomputation.
//!
//! Strategy: For each partition that has *any* changed rows (inserts,
//! updates, or deletes in the child delta), recompute the window
//! function for the entire partition. This avoids tracking complex
//! window state incrementally.
//!
//! CTE chain:
//! 1. Child delta (from recursive diff_node)
//! 2. Changed partition keys (DISTINCT partition_by cols from delta)
//! 3. Old ST rows for changed partitions (emitted as 'D' actions)
//! 4. Reconstruct current input for changed partitions from ST + delta
//! 5. Recompute window function on current input (emitted as 'I' actions)
//! 6. Combine deletes + inserts into final delta

use crate::dvm::diff::{DiffContext, DiffResult, col_list, prefixed_col_list, quote_ident};
use crate::dvm::operators::scan::build_hash_expr;
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate a Window node.
pub fn diff_window(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::Window {
        window_exprs,
        partition_by,
        pass_through,
        child,
    } = op
    else {
        return Err(PgStreamError::InternalError(
            "diff_window called on non-Window node".into(),
        ));
    };

    // ── Differentiate child to get the delta ───────────────────────────
    let child_result = ctx.diff_node(child)?;

    let st_table = ctx
        .st_qualified_name
        .clone()
        .unwrap_or_else(|| "/* st_table */".to_string());

    // Column lists
    let pt_aliases: Vec<String> = pass_through.iter().map(|(_, a)| a.clone()).collect();
    let wf_aliases: Vec<String> = window_exprs.iter().map(|w| w.alias.clone()).collect();
    let mut all_output_cols = pt_aliases.clone();
    all_output_cols.extend(wf_aliases.iter().cloned());

    let partition_cols: Vec<String> = partition_by.iter().map(|e| e.to_sql()).collect();

    // ── CTE 1: Find changed partition keys ─────────────────────────────
    let changed_parts_cte = ctx.next_cte_name("win_parts");
    if partition_cols.is_empty() {
        // Un-partitioned: any change means recompute everything.
        // Emit a single dummy row to trigger recomputation.
        let parts_sql = format!(
            "SELECT 1 AS __pgs_dummy\nFROM {child} LIMIT 1",
            child = child_result.cte_name,
        );
        ctx.add_cte(changed_parts_cte.clone(), parts_sql);
    } else {
        let distinct_cols = col_list(&partition_cols);
        let parts_sql = format!(
            "SELECT DISTINCT {distinct_cols}\nFROM {child}",
            child = child_result.cte_name,
        );
        ctx.add_cte(changed_parts_cte.clone(), parts_sql);
    }

    // ── join condition: st partition cols = cp partition cols ───────────
    let partition_join_dt_cp = if partition_cols.is_empty() {
        "TRUE".to_string()
    } else {
        partition_cols
            .iter()
            .map(|c| {
                let qc = quote_ident(c);
                format!("st.{qc} = cp.{qc}")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    // ── CTE 2: Old ST rows for changed partitions (DELETE actions) ─────
    let old_rows_cte = ctx.next_cte_name("win_old");
    let all_cols_dt = all_output_cols
        .iter()
        .map(|c| format!("st.{}", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ");

    let old_rows_sql = format!(
        "SELECT st.\"__pgs_row_id\", {all_cols_dt}\n\
         FROM {st_table} st\n\
         WHERE EXISTS (\n\
         SELECT 1 FROM {changed_parts_cte} cp WHERE {partition_join_dt_cp}\n\
         )",
    );
    ctx.add_cte(old_rows_cte.clone(), old_rows_sql);

    // ── CTE 3: Reconstruct current input for changed partitions ────────
    // Current input = (old ST rows NOT deleted by delta) UNION ALL (delta inserts)
    let current_input_cte = ctx.next_cte_name("win_input");

    let pt_cols_old = prefixed_col_list("o", &pt_aliases);
    let pt_cols_delta = prefixed_col_list("d", &pt_aliases);

    // For the surviving rows, we need partition cols from the old rows too
    let partition_join_delta_cp = if partition_cols.is_empty() {
        "TRUE".to_string()
    } else {
        partition_cols
            .iter()
            .map(|c| {
                let qc = quote_ident(c);
                format!("d.{qc} = cp.{qc}")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    let current_input_sql = format!(
        "-- Surviving old rows (pass-through only, window cols stripped)\n\
         SELECT o.\"__pgs_row_id\", {pt_cols_old}\n\
         FROM {old_rows_cte} o\n\
         WHERE o.\"__pgs_row_id\" NOT IN (\n\
             SELECT \"__pgs_row_id\" FROM {child_delta} WHERE \"__pgs_action\" = 'D'\n\
         )\n\
         UNION ALL\n\
         -- Newly inserted rows\n\
         SELECT d.\"__pgs_row_id\", {pt_cols_delta}\n\
         FROM {child_delta} d\n\
         WHERE d.\"__pgs_action\" = 'I'\n\
         AND EXISTS (\n\
             SELECT 1 FROM {changed_parts_cte} cp WHERE {partition_join_delta_cp}\n\
         )",
        child_delta = child_result.cte_name,
    );
    ctx.add_cte(current_input_cte.clone(), current_input_sql);

    // ── CTE 4: Recompute window functions on current input ─────────────
    let recomputed_cte = ctx.next_cte_name("win_recomp");

    let window_func_selects: Vec<String> = window_exprs
        .iter()
        .map(|w| format!("{} AS {}", w.to_sql(), quote_ident(&w.alias)))
        .collect();

    // Row ID: re-derive from pass-through columns to stay deterministic
    let hash_exprs: Vec<String> = pt_aliases
        .iter()
        .map(|c| format!("ci.{}::TEXT", quote_ident(c)))
        .collect();
    let row_id_expr = if hash_exprs.is_empty() {
        "pgstream.pg_stream_hash('__window_singleton')".to_string()
    } else {
        build_hash_expr(&hash_exprs)
    };

    let pt_cols_ci = prefixed_col_list("ci", &pt_aliases);

    let recomputed_sql = format!(
        "SELECT {row_id_expr} AS \"__pgs_row_id\",\n\
               {pt_cols_ci},\n\
               {wf_selects}\n\
         FROM {current_input_cte} ci",
        wf_selects = window_func_selects.join(",\n       "),
    );
    ctx.add_cte(recomputed_cte.clone(), recomputed_sql);

    // ── CTE 5: Final delta — DELETE old + INSERT recomputed ────────────
    let final_cte = ctx.next_cte_name("win_final");

    let all_cols_name = col_list(&all_output_cols);

    let final_sql = format!(
        "-- Delete old window results for changed partitions\n\
         SELECT \"__pgs_row_id\", 'D' AS \"__pgs_action\", {all_cols_name}\n\
         FROM {old_rows_cte}\n\
         UNION ALL\n\
         -- Insert recomputed window results\n\
         SELECT \"__pgs_row_id\", 'I' AS \"__pgs_action\", {all_cols_name}\n\
         FROM {recomputed_cte}",
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

    #[test]
    fn test_diff_window_basic() {
        let mut ctx = test_ctx_with_st("public", "my_st");
        let child = scan(1, "orders", "public", "o", &["id", "region", "amount"]);
        let wf = window_expr(
            "ROW_NUMBER",
            vec![],
            vec![colref("region")],
            vec![sort_asc(colref("amount"))],
            "rn",
        );
        let tree = window(
            vec![wf],
            vec![colref("region")],
            vec![
                (colref("id"), "id".to_string()),
                (colref("region"), "region".to_string()),
                (colref("amount"), "amount".to_string()),
            ],
            child,
        );
        let result = diff_window(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Output should include pass-through + window alias
        assert!(result.columns.contains(&"id".to_string()));
        assert!(result.columns.contains(&"region".to_string()));
        assert!(result.columns.contains(&"amount".to_string()));
        assert!(result.columns.contains(&"rn".to_string()));

        // Should have the CTE chain: changed parts, old rows, input, recompute, final
        assert_sql_contains(&sql, "DELETE");
        assert_sql_contains(&sql, "INSERT");
    }

    #[test]
    fn test_diff_window_changed_partition_detection() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "grp", "val"]);
        let wf = window_expr(
            "SUM",
            vec![colref("val")],
            vec![colref("grp")],
            vec![],
            "running_sum",
        );
        let tree = window(
            vec![wf],
            vec![colref("grp")],
            vec![
                (colref("id"), "id".to_string()),
                (colref("grp"), "grp".to_string()),
                (colref("val"), "val".to_string()),
            ],
            child,
        );
        let result = diff_window(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should detect changed partitions via DISTINCT partition keys
        assert_sql_contains(&sql, "DISTINCT");
    }

    #[test]
    fn test_diff_window_unpartitioned() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "val"]);
        let wf = window_expr(
            "ROW_NUMBER",
            vec![],
            vec![],
            vec![sort_asc(colref("val"))],
            "rn",
        );
        let tree = window(
            vec![wf],
            vec![], // no partition_by
            vec![
                (colref("id"), "id".to_string()),
                (colref("val"), "val".to_string()),
            ],
            child,
        );
        let result = diff_window(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Un-partitioned: any change → recompute all
        assert_sql_contains(&sql, "LIMIT 1");
    }

    #[test]
    fn test_diff_window_not_deduplicated() {
        let mut ctx = test_ctx_with_st("public", "st");
        let child = scan(1, "t", "public", "t", &["id", "val"]);
        let wf = window_expr(
            "ROW_NUMBER",
            vec![],
            vec![],
            vec![sort_asc(colref("val"))],
            "rn",
        );
        let tree = window(
            vec![wf],
            vec![],
            vec![
                (colref("id"), "id".to_string()),
                (colref("val"), "val".to_string()),
            ],
            child,
        );
        let result = diff_window(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_window_error_on_non_window_node() {
        let mut ctx = test_ctx_with_st("public", "st");
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_window(&mut ctx, &tree);
        assert!(result.is_err());
    }
}
