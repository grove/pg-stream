//! Anti-join differentiation (NOT EXISTS / NOT IN subquery).
//!
//! Δ(L ▷ R) = Part1 ∪ Part2
//!
//! Part 1 — left-side changes:
//!   New/deleted left rows that have NO match in current right.
//!   ```sql
//!   SELECT ... FROM delta_left dl
//!   WHERE NOT EXISTS (SELECT 1 FROM right_snapshot r WHERE condition)
//!   ```
//!
//! Part 2 — right-side changes:
//!   Left rows whose anti-join status flips due to right changes.
//!   A left row's status changes when it goes from having no match in R
//!   to having a match (DELETE from anti-join output), or vice versa (INSERT).
//!
//!   For each left row correlated with any delta_right row:
//!   - If NOT EXISTS in R_current AND EXISTS in R_old → INSERT (regained)
//!   - If EXISTS in R_current AND NOT EXISTS in R_old → DELETE (lost)

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join_common::{build_snapshot_sql, rewrite_join_condition};
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate an AntiJoin node.
pub fn diff_anti_join(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::AntiJoin {
        condition,
        left,
        right,
    } = op
    else {
        return Err(PgStreamError::InternalError(
            "diff_anti_join called on non-AntiJoin node".into(),
        ));
    };

    // Differentiate both children
    let left_result = ctx.diff_node(left)?;
    let right_result = ctx.diff_node(right)?;

    let right_table = build_snapshot_sql(right);

    // Rewrite join condition aliases for each part
    let cond_part1 = rewrite_join_condition(condition, left, "dl", right, "r");
    let cond_part2_new = rewrite_join_condition(condition, left, "l", right, "r");
    let cond_part2_dr = rewrite_join_condition(condition, left, "l", right, "dr");
    let cond_part2_old = rewrite_join_condition(condition, left, "l", right, "r_old");

    let left_cols = &left_result.columns;

    // Anti-join only outputs left-side columns
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
    let hash_part1 = "dl.__pgs_row_id".to_string();
    // For Part 2: hash left row using pg_stream_hash
    let hash_part2 = {
        let key_exprs: Vec<String> = left_cols
            .iter()
            .map(|c| format!("l.{}::TEXT", quote_ident(c)))
            .collect();
        format!(
            "pgstream.pg_stream_hash_multi(ARRAY[{}])",
            key_exprs.join(", ")
        )
    };

    // Build R_old snapshot (same approach as semi_join)
    let right_cols = &right_result.columns;
    let right_col_list: String = right_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let right_alias = right.alias();

    let r_old_snapshot = format!(
        "(SELECT {right_col_list} FROM {right_table} {right_alias} \
         EXCEPT ALL \
         SELECT {right_col_list} FROM {delta_right} WHERE __pgs_action = 'I' \
         UNION ALL \
         SELECT {right_col_list} FROM {delta_right} WHERE __pgs_action = 'D') ",
        delta_right = right_result.cte_name,
        right_alias = quote_ident(right_alias),
    );

    let cte_name = ctx.next_cte_name("anti_join");

    let sql = format!(
        "\
-- Part 1: delta_left rows that have NO match in current right (anti-join filter)
SELECT {hash_part1} AS __pgs_row_id,
       dl.__pgs_action,
       {dl_cols}
FROM {delta_left} dl
WHERE NOT EXISTS (SELECT 1 FROM {right_table} r WHERE {cond_part1})

UNION ALL

-- Part 2: left rows whose anti-join status changed due to right-side delta
-- Emit 'I' if row now has no match in R_current but had a match in R_old
-- Emit 'D' if row had no match in R_old but now has a match in R_current
SELECT {hash_part2} AS __pgs_row_id,
       CASE WHEN NOT EXISTS (SELECT 1 FROM {right_table} r WHERE {cond_part2_new})
            THEN 'I' ELSE 'D'
       END AS __pgs_action,
       {l_cols}
FROM {left_snapshot} l
WHERE EXISTS (SELECT 1 FROM {delta_right} dr WHERE {cond_part2_dr})
  AND (EXISTS (SELECT 1 FROM {right_table} r WHERE {cond_part2_new})
       <> EXISTS (SELECT 1 FROM {r_old_snapshot} r_old WHERE {cond_part2_old}))",
        dl_cols = dl_col_refs.join(", "),
        l_cols = l_col_refs.join(", "),
        delta_left = left_result.cte_name,
        delta_right = right_result.cte_name,
        left_snapshot = build_snapshot_sql(left),
        right_table = right_table,
        r_old_snapshot = r_old_snapshot,
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
    fn test_diff_anti_join_basic() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id", "amount"]);
        let right = scan(2, "returns", "public", "r", &["order_id", "reason"]);
        let cond = eq_cond("o", "id", "r", "order_id");
        let tree = OpTree::AntiJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_anti_join(&mut ctx, &tree).unwrap();

        // Anti-join outputs only left-side columns
        assert_eq!(result.columns, vec!["id", "cust_id", "amount"]);
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_anti_join_sql_contains_not_exists() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "amount"]);
        let right = scan(2, "returns", "public", "ret", &["order_id"]);
        let cond = eq_cond("o", "id", "ret", "order_id");
        let tree = OpTree::AntiJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_anti_join(&mut ctx, &tree).unwrap();

        let sql = ctx.build_with_query(&result.cte_name);
        assert!(
            sql.contains("NOT EXISTS"),
            "SQL should contain NOT EXISTS check"
        );
        assert!(sql.contains("Part 1"), "SQL should have Part 1 comment");
        assert!(sql.contains("Part 2"), "SQL should have Part 2 comment");
        assert!(sql.contains("UNION ALL"), "SQL should UNION ALL both parts");
    }

    #[test]
    fn test_diff_anti_join_wrong_node_type() {
        let mut ctx = test_ctx();
        let scan_node = scan(1, "t", "public", "t", &["id"]);
        let result = diff_anti_join(&mut ctx, &scan_node);
        assert!(result.is_err());
    }
}
