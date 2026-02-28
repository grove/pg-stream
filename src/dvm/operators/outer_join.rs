//! Outer join differentiation.
//!
//! LEFT JOIN = INNER JOIN + anti-join for non-matching left rows.
//!
//! Differentiate the inner join part normally, then handle the anti-join:
//! - Left rows that lose their last match → INSERT with NULL right columns
//! - Left rows that gain their first match → DELETE the NULL-padded row

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join_common::{build_snapshot_sql, rewrite_join_condition};
use crate::dvm::parser::OpTree;
use crate::error::PgTrickleError;

/// Differentiate a LeftJoin node.
pub fn diff_left_join(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
    let OpTree::LeftJoin {
        condition,
        left,
        right,
    } = op
    else {
        return Err(PgTrickleError::InternalError(
            "diff_left_join called on non-LeftJoin node".into(),
        ));
    };

    // For LEFT JOIN, we reuse inner join differentiation for the matching part
    // and add the anti-join handling for non-matching left rows.

    // Differentiate both children
    let left_result = ctx.diff_node(left)?;
    let right_result = ctx.diff_node(right)?;

    // Rewrite join condition aliases for each part of the delta query.
    // For nested join children, column names are disambiguated with the
    // original table alias prefix (e.g., o.cust_id → dl."o__cust_id").
    let join_cond_part1 = rewrite_join_condition(condition, left, "dl", right, "r");
    let join_cond_part2 = rewrite_join_condition(condition, left, "l", right, "dr");
    let join_cond_antijoin = rewrite_join_condition(condition, left, "dl", right, "r");

    let left_cols = &left_result.columns;
    let right_cols = &right_result.columns;

    // Disambiguate output columns with table-alias prefix, matching
    // inner join convention so diff_project can resolve qualified refs.
    let left_prefix = left.alias();
    let right_prefix = right.alias();

    let mut output_cols = Vec::new();
    for c in left_cols {
        output_cols.push(format!("{left_prefix}__{c}"));
    }
    for c in right_cols {
        output_cols.push(format!("{right_prefix}__{c}"));
    }

    let right_table = build_snapshot_sql(right);
    let left_table = build_snapshot_sql(left);

    // Build column references with AS aliases for disambiguation
    let dl_cols: Vec<String> = left_cols
        .iter()
        .map(|c| {
            format!(
                "dl.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{left_prefix}__{c}"))
            )
        })
        .collect();
    let r_cols: Vec<String> = right_cols
        .iter()
        .map(|c| {
            format!(
                "r.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{right_prefix}__{c}"))
            )
        })
        .collect();
    let l_cols: Vec<String> = left_cols
        .iter()
        .map(|c| {
            format!(
                "l.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{left_prefix}__{c}"))
            )
        })
        .collect();
    let dr_cols: Vec<String> = right_cols
        .iter()
        .map(|c| {
            format!(
                "dr.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{right_prefix}__{c}"))
            )
        })
        .collect();
    let null_right_cols: Vec<String> = right_cols
        .iter()
        .map(|c| format!("NULL AS {}", quote_ident(&format!("{right_prefix}__{c}"))))
        .collect();

    let part1_cols = [dl_cols.as_slice(), r_cols.as_slice()].concat().join(", ");
    let part2_cols = [l_cols.as_slice(), dr_cols.as_slice()].concat().join(", ");
    let antijoin_cols = [dl_cols.as_slice(), null_right_cols.as_slice()]
        .concat()
        .join(", ");

    // For Parts 4 & 5: current_left JOIN conditions with different aliases
    // Part 4/5 JOIN: uses l and dr (same as Part 2)
    // Part 5 NOT EXISTS: uses l (current_left) and r (current_right)
    let not_exists_cond = rewrite_join_condition(condition, left, "l", right, "r");

    // R_old condition: uses l (current_left) and __pgt_r_old (pre-change right)
    let r_old_cond = rewrite_join_condition(condition, left, "l", right, "__pgt_r_old");

    // Build R_old snapshot for Parts 4/5: pre-change right state.
    // R_old = R_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
    // Used to check whether a left row had ANY matching right row BEFORE
    // the current cycle's changes, preventing spurious NULL-padded D/I.
    let right_user_cols: Vec<&String> = right_cols.iter().filter(|c| *c != "__pgt_count").collect();
    let right_col_list: String = right_user_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let right_alias = right.alias();

    let r_old_snapshot = format!(
        "(SELECT {right_col_list} FROM {right_table} {ra} \
         EXCEPT ALL \
         SELECT {right_col_list} FROM {delta_right} WHERE __pgt_action = 'I' \
         UNION ALL \
         SELECT {right_col_list} FROM {delta_right} WHERE __pgt_action = 'D')",
        ra = quote_ident(right_alias),
        delta_right = right_result.cte_name,
    );

    // Null-padded columns for Parts 4 & 5 (left from `l`, right all NULL)
    let l_null_padded_cols = [l_cols.as_slice(), null_right_cols.as_slice()]
        .concat()
        .join(", ");

    // ── G-J2: Pre-compute right-delta action flags ──────────────────
    //
    // Parts 4 and 5 each scan all current left rows joined with the right
    // delta. When the right delta is INSERT-only, Part 5 (which handles
    // right DELETEs) scans left_table for nothing. And vice versa.
    //
    // A single bool_or CTE evaluated once tells us which parts to run,
    // allowing PostgreSQL to skip the full left_table scan for whichever
    // part returns no rows.
    let flags_cte = ctx.next_cte_name("lj_right_flags");
    ctx.add_cte(
        flags_cte.clone(),
        format!(
            "SELECT bool_or(__pgt_action = 'I') AS has_ins,\
                    bool_or(__pgt_action = 'D') AS has_del \
             FROM {delta_right}",
            delta_right = right_result.cte_name,
        ),
    );

    let cte_name = ctx.next_cte_name("left_join");

    let sql = format!(
        "\
-- Part 1: delta_left JOIN current_right (matching rows)
SELECT pgtrickle.pg_trickle_hash_multi(ARRAY[dl.__pgt_row_id::TEXT, pgtrickle.pg_trickle_hash(row_to_json(r)::text)::TEXT]) AS __pgt_row_id,
       dl.__pgt_action,
       {part1_cols}
FROM {delta_left} dl
JOIN {right_table} r ON {join_cond_part1}

UNION ALL

-- Part 2: current_left JOIN delta_right
SELECT pgtrickle.pg_trickle_hash_multi(ARRAY[pgtrickle.pg_trickle_hash(row_to_json(l)::text)::TEXT, dr.__pgt_row_id::TEXT]) AS __pgt_row_id,
       dr.__pgt_action,
       {part2_cols}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}

UNION ALL

-- Part 3: delta_left anti-join right (non-matching left rows get NULL right cols)
SELECT dl.__pgt_row_id,
       dl.__pgt_action,
       {antijoin_cols}
FROM {delta_left} dl
WHERE NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {join_cond_antijoin}
)

UNION ALL

-- Part 4: Delete stale NULL-padded rows when a left row gains its FIRST right match.
-- When a right INSERT creates a new match for a left row that previously had NO
-- matching right rows (was NULL-padded), the NULL-padded ST row must be removed.
-- We check R_old (pre-change right) to verify the left row truly had no matches
-- before. Without this check, left rows that ALREADY had matches would get
-- spurious D(NULL-padded) rows that corrupt intermediate aggregate old-state
-- reconstruction via EXCEPT ALL/UNION ALL.
SELECT 0::BIGINT AS __pgt_row_id,
       'D'::TEXT AS __pgt_action,
       {l_null_padded_cols}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgt_action = 'I'
  AND (SELECT has_ins FROM {flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {r_old_snapshot} __pgt_r_old WHERE {r_old_cond}
  )

UNION ALL

-- Part 5: Insert NULL-padded rows when a left row loses ALL right matches.
-- When a right DELETE removes the last match for a left row, the left row
-- reverts to NULL-padded. Check current right (post-changes) to verify no
-- remaining matches exist, AND check R_old to confirm the left row previously
-- HAD matches (otherwise it was already NULL-padded — no change needed).
SELECT 0::BIGINT AS __pgt_row_id,
       'I'::TEXT AS __pgt_action,
       {l_null_padded_cols}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgt_action = 'D'
  AND (SELECT has_del FROM {flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {not_exists_cond}
  )
  AND EXISTS (
    SELECT 1 FROM {r_old_snapshot} __pgt_r_old WHERE {r_old_cond}
  )",
        delta_left = left_result.cte_name,
        delta_right = right_result.cte_name,
        flags_cte = flags_cte,
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
    fn test_diff_left_join_basic() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = left_join(cond, left, right);
        let result = diff_left_join(&mut ctx, &tree).unwrap();

        // Output columns should be disambiguated
        assert!(result.columns.contains(&"o__id".to_string()));
        assert!(result.columns.contains(&"c__name".to_string()));
    }

    #[test]
    fn test_diff_left_join_has_five_parts() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = left_join(cond, left, right);
        let result = diff_left_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should have 5 parts
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "Part 3");
        assert_sql_contains(&sql, "Part 4");
        assert_sql_contains(&sql, "Part 5");
    }

    #[test]
    fn test_diff_left_join_null_padding() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = left_join(cond, left, right);
        let result = diff_left_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Anti-join part should pad right columns with NULL
        assert_sql_contains(&sql, "NULL AS");
    }

    #[test]
    fn test_diff_left_join_right_delta_flags() {
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = left_join(cond, left, right);
        let result = diff_left_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // G-J2 optimization: pre-computed right-delta action flags
        assert_sql_contains(&sql, "has_ins");
        assert_sql_contains(&sql, "has_del");
    }

    #[test]
    fn test_diff_left_join_not_deduplicated() {
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = left_join(cond, left, right);
        let result = diff_left_join(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_left_join_error_on_non_left_join_node() {
        let mut ctx = test_ctx();
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_left_join(&mut ctx, &tree);
        assert!(result.is_err());
    }

    // ── Nested join tests ───────────────────────────────────────────

    #[test]
    fn test_diff_left_join_nested_three_tables() {
        // (a ⋈ b) LEFT JOIN c — left child is a nested inner join
        let a = scan(1, "a", "public", "a", &["id", "bid"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "bid", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = left_join(eq_cond("a", "id", "c", "id"), inner, c);

        let mut ctx = test_ctx();
        let result = diff_left_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "nested 3-table left join should diff: {result:?}"
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert_sql_contains(&sql, "UNION ALL");
    }

    #[test]
    fn test_diff_left_join_nested_right_child() {
        // a LEFT JOIN (b ⋈ c) — right child is a nested inner join
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id", "cid"]);
        let c = scan(3, "c", "public", "c", &["id"]);
        let inner = inner_join(eq_cond("b", "cid", "c", "id"), b, c);
        let tree = left_join(eq_cond("a", "id", "b", "id"), a, inner);

        let mut ctx = test_ctx();
        let result = diff_left_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "left join with nested right child should diff: {result:?}"
        );
    }

    // ── NATURAL LEFT JOIN diff tests ────────────────────────────────

    #[test]
    fn test_diff_left_join_with_natural_condition() {
        // Simulate NATURAL LEFT JOIN: tables share "id" column
        let left = scan(1, "orders", "public", "o", &["id", "customer_id"]);
        let right = scan(2, "items", "public", "i", &["id", "order_id"]);
        let cond = natural_join_cond(&left, &right);
        let tree = left_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_left_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // Left join diff should have multiple parts with UNION ALL
        assert_sql_contains(&sql, "UNION ALL");
        // Disambiguated columns from both sides
        assert!(result.columns.contains(&"o__id".to_string()));
        assert!(result.columns.contains(&"i__id".to_string()));
    }

    #[test]
    fn test_diff_left_join_natural_multiple_common_cols() {
        // Two tables sharing "id" and "region"
        let left = scan(1, "a", "public", "a", &["id", "region", "val"]);
        let right = scan(2, "b", "public", "b", &["id", "region", "score"]);
        let cond = natural_join_cond(&left, &right);
        let tree = left_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_left_join(&mut ctx, &tree).unwrap();
        assert!(result.columns.contains(&"a__id".to_string()));
        assert!(result.columns.contains(&"a__region".to_string()));
        assert!(result.columns.contains(&"b__region".to_string()));
        assert!(result.columns.contains(&"b__score".to_string()));
    }
}
