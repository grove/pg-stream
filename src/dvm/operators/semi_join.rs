//! Semi-join differentiation (EXISTS / IN subquery).
//!
//! Δ(L ⋉ R) = Part1 ∪ Part2
//!
//! Part 1 — left-side changes:
//!   New/deleted left rows that have a match in current right.
//!   ```sql
//!   SELECT ... FROM delta_left dl
//!   WHERE EXISTS (SELECT 1 FROM right_snapshot r WHERE condition)
//!   ```
//!
//! Part 2 — right-side changes:
//!   Left rows that gain or lose their semi-join match due to right changes.
//!   A left row is affected if it matches any changed right row. Its status
//!   flips from non-matching to matching (INSERT) or matching to non-matching
//!   (DELETE) based on whether a match exists in `R_new` vs `R_old`.
//!
//!   We compute `R_old` from the right snapshot by reversing delta_right:
//!   `R_old = R_current EXCEPT delta_right(action='I') UNION delta_right(action='D')`.
//!   For simplicity, we use the frontier-based approach: the right snapshot at
//!   `prev_frontier` is the "old" state, and the right snapshot at `new_frontier`
//!   is the "new" state. Since we always have the live table as "new", we
//!   approximate R_old by anti-joining delta_right inserts and re-adding deletes.
//!
//!   Simplified approach: for each left row that correlates with any delta_right
//!   row, check if it now matches R_current (live table). If yes → 'I', else → 'D'.
//!   To avoid false positives, also check if it matched R_old. Only emit if status changed.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join_common::{
    build_snapshot_sql, extract_equijoin_keys_aliased, rewrite_join_condition,
};
use crate::dvm::parser::OpTree;
use crate::error::PgTrickleError;

/// Differentiate a SemiJoin node.
pub fn diff_semi_join(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
    let OpTree::SemiJoin {
        condition,
        left,
        right,
    } = op
    else {
        return Err(PgTrickleError::InternalError(
            "diff_semi_join called on non-SemiJoin node".into(),
        ));
    };

    // Differentiate both children.
    // Set inside_semijoin flag so inner joins within this subtree use L₁
    // (post-change snapshot) instead of L₀ via EXCEPT ALL, avoiding the
    // Q21-type numwait regression.
    let saved_inside_semijoin = ctx.inside_semijoin;
    ctx.inside_semijoin = true;
    let left_result = ctx.diff_node(left)?;
    let right_result = ctx.diff_node(right)?;
    ctx.inside_semijoin = saved_inside_semijoin;

    let right_table = build_snapshot_sql(right);

    // Rewrite join condition aliases for each part
    let cond_part1 = rewrite_join_condition(condition, left, "dl", right, "r");
    let cond_part1_old = rewrite_join_condition(condition, left, "dl", right, "r_old");
    let cond_part2_new = rewrite_join_condition(condition, left, "l", right, "r");
    let cond_part2_dr = rewrite_join_condition(condition, left, "l", right, "dr");
    let cond_part2_old = rewrite_join_condition(condition, left, "l", right, "r_old");

    let left_cols = &left_result.columns;
    let _left_prefix = left.alias();

    // Semi-join only outputs left-side columns
    let output_cols: Vec<String> = left_cols.to_vec();

    let dl_col_refs: Vec<String> = left_cols
        .iter()
        .map(|c| format!("dl.{}", quote_ident(c)))
        .collect();

    let l_col_refs: Vec<String> = left_cols
        .iter()
        .map(|c| format!("l.{}", quote_ident(c)))
        .collect();

    // Row ID: passthrough from left side
    let hash_part1 = "dl.__pgt_row_id".to_string();
    // For Part 2: hash left row using pg_trickle_hash since it comes from snapshot
    let hash_part2 = {
        let key_exprs: Vec<String> = left_cols
            .iter()
            .map(|c| format!("l.{}::TEXT", quote_ident(c)))
            .collect();
        format!(
            "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
            key_exprs.join(", ")
        )
    };

    // Build R_old snapshot: the right table state before the current delta.
    // R_old = (R_current EXCEPT rows inserted by delta_right)
    //         UNION (rows deleted by delta_right)
    //
    // Expressed as a subquery:
    //   (SELECT * FROM right_table
    //    EXCEPT ALL
    //    SELECT <right_cols> FROM delta_right WHERE __pgt_action = 'I'
    //    UNION ALL
    //    SELECT <right_cols> FROM delta_right WHERE __pgt_action = 'D')
    let right_cols = &right_result.columns;
    // Filter out internal metadata columns (__pgt_count) from the EXCEPT ALL /
    // UNION ALL column list. These are aggregate bookkeeping columns that:
    // (a) don't exist in the snapshot (build_snapshot_sql doesn't produce them)
    // (b) shouldn't participate in set-difference matching
    let right_user_cols: Vec<&String> = right_cols.iter().filter(|c| *c != "__pgt_count").collect();
    let right_col_list: String = right_user_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let right_alias = right.alias();

    // Materialize R_old as a CTE to avoid re-evaluating the EXCEPT ALL /
    // UNION ALL set operation for every EXISTS check in Part 1 and Part 2.
    // At SF=0.01 with 3 mutation cycles, this reduces Q21 from ~5.4s to
    // sub-second by allowing PostgreSQL to hash-probe the pre-computed
    // snapshot instead of repeatedly scanning and differencing the tables.
    let r_old_cte_name = ctx.next_cte_name("r_old");
    let r_old_sql = format!(
        "SELECT {right_col_list} FROM {right_table} {right_alias} \
         EXCEPT ALL \
         SELECT {right_col_list} FROM {delta_right} WHERE __pgt_action = 'I' \
         UNION ALL \
         SELECT {right_col_list} FROM {delta_right} WHERE __pgt_action = 'D'",
        delta_right = right_result.cte_name,
        right_alias = quote_ident(right_alias),
    );
    ctx.add_materialized_cte(r_old_cte_name.clone(), r_old_sql);

    // ── Delta-key pre-filtering for Part 2 ──────────────────────────
    //
    // Part 2 scans the full left snapshot looking for rows correlated
    // with delta_right. For large left tables (e.g. lineitem in Q18/Q21)
    // this sequential scan dominates refresh time even though only a few
    // rows are actually affected.
    //
    // Extract equi-join keys from the condition and use them to build a
    // semi-join filter that limits the left snapshot to rows whose join
    // keys appear in delta_right. This converts O(|L|) into O(|ΔR|)
    // when the join key is indexed.
    //
    // The keys are rewritten using the same alias logic as the condition
    // rewriting. We filter to only "clean" key pairs where the left side
    // references the pre-filter alias and the right side references the
    // delta alias — this avoids incorrect filters when rewriting fails.
    let equi_keys_raw = extract_equijoin_keys_aliased(condition, left, "__pgt_pre", right, "dr");
    let equi_keys: Vec<_> = equi_keys_raw
        .into_iter()
        .filter(|(lk, rk)| lk.contains("__pgt_pre") && rk.starts_with("dr."))
        .collect();
    let left_snapshot_raw = build_snapshot_sql(left);
    let left_snapshot_filtered = if equi_keys.is_empty() {
        left_snapshot_raw
    } else {
        let filters: Vec<String> = equi_keys
            .iter()
            .map(|(left_key, right_key)| {
                format!(
                    "{left_key} IN (SELECT DISTINCT {right_key} FROM {} dr)",
                    right_result.cte_name
                )
            })
            .collect();
        format!(
            "(SELECT * FROM {left_snapshot_raw} \"__pgt_pre\" WHERE {filters})",
            filters = filters.join(" AND "),
        )
    };

    let cte_name = ctx.next_cte_name("semi_join");

    let sql = format!(
        "\
-- Part 1: delta_left rows that match right (semi-join filter)
-- INSERT: new left row has match in R_current  → emit INSERT
-- DELETE: old left row had match in R_old      → emit DELETE
-- For INSERTs we check the live right table (post-change state).
-- For DELETEs we check R_old (pre-change state) because the matching
-- right rows may also have been deleted in the same mutation cycle
-- (e.g. RF2 deletes both orders AND their lineitems simultaneously).
SELECT {hash_part1} AS __pgt_row_id,
       dl.__pgt_action,
       {dl_cols}
FROM {delta_left} dl
WHERE CASE WHEN dl.__pgt_action = 'D'
           THEN EXISTS (SELECT 1 FROM {r_old_cte} r_old WHERE {cond_part1_old})
           ELSE EXISTS (SELECT 1 FROM {right_table} r WHERE {cond_part1})
      END

UNION ALL

-- Part 2: left rows whose semi-join status changed due to right-side delta
-- Emit 'I' if row now matches R_current but didn't match R_old
-- Emit 'D' if row matched R_old but no longer matches R_current
-- Left snapshot is pre-filtered by delta-right join keys for performance.
SELECT {hash_part2} AS __pgt_row_id,
       CASE WHEN EXISTS (SELECT 1 FROM {right_table} r WHERE {cond_part2_new})
            THEN 'I' ELSE 'D'
       END AS __pgt_action,
       {l_cols}
FROM {left_snapshot} l
WHERE EXISTS (SELECT 1 FROM {delta_right} dr WHERE {cond_part2_dr})
  AND (EXISTS (SELECT 1 FROM {right_table} r WHERE {cond_part2_new})
       <> EXISTS (SELECT 1 FROM {r_old_cte} r_old WHERE {cond_part2_old}))",
        dl_cols = dl_col_refs.join(", "),
        l_cols = l_col_refs.join(", "),
        delta_left = left_result.cte_name,
        delta_right = right_result.cte_name,
        left_snapshot = left_snapshot_filtered,
        right_table = right_table,
        r_old_cte = r_old_cte_name,
        cond_part1_old = cond_part1_old,
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
    fn test_diff_semi_join_basic() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = OpTree::SemiJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_semi_join(&mut ctx, &tree).unwrap();

        // Semi-join outputs only left-side columns
        assert_eq!(result.columns, vec!["id", "cust_id", "amount"]);
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_semi_join_sql_contains_exists() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "amount"]);
        let right = scan(2, "items", "public", "i", &["order_id", "qty"]);
        let cond = eq_cond("o", "id", "i", "order_id");
        let tree = OpTree::SemiJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_semi_join(&mut ctx, &tree).unwrap();

        // Verify the generated SQL has the expected structure
        let sql = ctx.build_with_query(&result.cte_name);
        assert!(sql.contains("EXISTS"), "SQL should contain EXISTS check");
        assert!(sql.contains("Part 1"), "SQL should have Part 1 comment");
        assert!(sql.contains("Part 2"), "SQL should have Part 2 comment");
        assert!(sql.contains("UNION ALL"), "SQL should UNION ALL both parts");
    }

    #[test]
    fn test_diff_semi_join_wrong_node_type() {
        let mut ctx = test_ctx();
        let scan_node = scan(1, "t", "public", "t", &["id"]);
        let result = diff_semi_join(&mut ctx, &scan_node);
        assert!(result.is_err());
    }
}
