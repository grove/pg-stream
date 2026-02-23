//! Full outer join differentiation.
//!
//! FULL OUTER JOIN = INNER JOIN + left anti-join + right anti-join.
//!
//! The delta is a 7-part UNION ALL:
//!
//! 1. **Part 1** — delta_left JOIN current_right (matching rows)
//! 2. **Part 2** — current_left JOIN delta_right (matching rows)
//! 3. **Part 3** — delta_left anti-join right (non-matching left → NULL right)
//! 4. **Part 4** — Delete stale NULL-padded left rows when new right matches
//! 5. **Part 5** — Insert NULL-padded left rows when last right match removed
//! 6. **Part 6** — delta_right anti-join left (non-matching right → NULL left)
//! 7. **Part 7** — Symmetric transitions for right side gaining/losing left matches
//!
//! Parts 1-5 are identical to LEFT JOIN (outer_join.rs). Parts 6-7 add
//! the symmetric right-side anti-join handling.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join_common::{build_snapshot_sql, rewrite_join_condition};
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate a FullJoin node.
pub fn diff_full_join(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::FullJoin {
        condition,
        left,
        right,
    } = op
    else {
        return Err(PgStreamError::InternalError(
            "diff_full_join called on non-FullJoin node".into(),
        ));
    };

    // Differentiate both children
    let left_result = ctx.diff_node(left)?;
    let right_result = ctx.diff_node(right)?;

    // Rewrite join conditions for each part
    let join_cond_part1 = rewrite_join_condition(condition, left, "dl", right, "r");
    let join_cond_part2 = rewrite_join_condition(condition, left, "l", right, "dr");
    let join_cond_antijoin_l = rewrite_join_condition(condition, left, "dl", right, "r");
    let join_cond_antijoin_r = rewrite_join_condition(condition, left, "l", right, "dr");
    let not_exists_cond_lr = rewrite_join_condition(condition, left, "l", right, "r");

    let left_cols = &left_result.columns;
    let right_cols = &right_result.columns;

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

    // Column references for each part
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
    let null_left_cols: Vec<String> = left_cols
        .iter()
        .map(|c| format!("NULL AS {}", quote_ident(&format!("{left_prefix}__{c}"))))
        .collect();

    let part1_cols = [dl_cols.as_slice(), r_cols.as_slice()].concat().join(", ");
    let part2_cols = [l_cols.as_slice(), dr_cols.as_slice()].concat().join(", ");
    let antijoin_left_cols = [dl_cols.as_slice(), null_right_cols.as_slice()]
        .concat()
        .join(", ");
    let antijoin_right_cols = [null_left_cols.as_slice(), dr_cols.as_slice()]
        .concat()
        .join(", ");

    // Null-padded columns for left-side transitions (Parts 4 & 5)
    let l_null_right_padded = [l_cols.as_slice(), null_right_cols.as_slice()]
        .concat()
        .join(", ");

    // Null-padded columns for right-side transitions (Part 7)
    let null_left_r_padded = [null_left_cols.as_slice(), r_cols.as_slice()]
        .concat()
        .join(", ");

    // ── Pre-compute delta action flags for both sides ──────────────
    let left_flags_cte = ctx.next_cte_name("fj_left_flags");
    ctx.add_cte(
        left_flags_cte.clone(),
        format!(
            "SELECT bool_or(__pgs_action = 'I') AS has_ins,\
                    bool_or(__pgs_action = 'D') AS has_del \
             FROM {delta_left}",
            delta_left = left_result.cte_name,
        ),
    );

    let right_flags_cte = ctx.next_cte_name("fj_right_flags");
    ctx.add_cte(
        right_flags_cte.clone(),
        format!(
            "SELECT bool_or(__pgs_action = 'I') AS has_ins,\
                    bool_or(__pgs_action = 'D') AS has_del \
             FROM {delta_right}",
            delta_right = right_result.cte_name,
        ),
    );

    let cte_name = ctx.next_cte_name("full_join");

    let sql = format!(
        "\
-- Part 1: delta_left JOIN current_right (matching rows)
SELECT pgstream.pg_stream_hash_multi(ARRAY[dl.__pgs_row_id::TEXT, pgstream.pg_stream_hash(row_to_json(r)::text)::TEXT]) AS __pgs_row_id,
       dl.__pgs_action,
       {part1_cols}
FROM {delta_left} dl
JOIN {right_table} r ON {join_cond_part1}

UNION ALL

-- Part 2: current_left JOIN delta_right
SELECT pgstream.pg_stream_hash_multi(ARRAY[pgstream.pg_stream_hash(row_to_json(l)::text)::TEXT, dr.__pgs_row_id::TEXT]) AS __pgs_row_id,
       dr.__pgs_action,
       {part2_cols}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}

UNION ALL

-- Part 3: delta_left anti-join right (non-matching left rows → NULL right cols)
SELECT dl.__pgs_row_id,
       dl.__pgs_action,
       {antijoin_left_cols}
FROM {delta_left} dl
WHERE NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {join_cond_antijoin_l}
)

UNION ALL

-- Part 4: Delete stale NULL-padded left rows when new right matches appear
SELECT 0::BIGINT AS __pgs_row_id,
       'D'::TEXT AS __pgs_action,
       {l_null_right_padded}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgs_action = 'I'
  AND (SELECT has_ins FROM {right_flags_cte})

UNION ALL

-- Part 5: Insert NULL-padded left rows when left row loses all right matches
SELECT 0::BIGINT AS __pgs_row_id,
       'I'::TEXT AS __pgs_action,
       {l_null_right_padded}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgs_action = 'D'
  AND (SELECT has_del FROM {right_flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {not_exists_cond_lr}
)

UNION ALL

-- Part 6: delta_right anti-join left (non-matching right rows → NULL left cols)
SELECT dr.__pgs_row_id,
       dr.__pgs_action,
       {antijoin_right_cols}
FROM {delta_right} dr
WHERE NOT EXISTS (
    SELECT 1 FROM {left_table} l WHERE {join_cond_antijoin_r}
)

UNION ALL

-- Part 7a: Delete stale NULL-padded right rows when new left matches appear
SELECT 0::BIGINT AS __pgs_row_id,
       'D'::TEXT AS __pgs_action,
       {null_left_r_padded}
FROM {right_table} r
JOIN {delta_left} dl ON {join_cond_antijoin_l}
WHERE dl.__pgs_action = 'I'
  AND (SELECT has_ins FROM {left_flags_cte})

UNION ALL

-- Part 7b: Insert NULL-padded right rows when right row loses all left matches
SELECT 0::BIGINT AS __pgs_row_id,
       'I'::TEXT AS __pgs_action,
       {null_left_r_padded}
FROM {right_table} r
JOIN {delta_left} dl ON {join_cond_antijoin_l}
WHERE dl.__pgs_action = 'D'
  AND (SELECT has_del FROM {left_flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {left_table} l WHERE {not_exists_cond_lr}
)",
        delta_left = left_result.cte_name,
        delta_right = right_result.cte_name,
        left_flags_cte = left_flags_cte,
        right_flags_cte = right_flags_cte,
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
    fn test_diff_full_join_basic() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = OpTree::FullJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_full_join(&mut ctx, &tree).unwrap();

        // Output columns should be disambiguated
        assert!(result.columns.contains(&"o__id".to_string()));
        assert!(result.columns.contains(&"o__cust_id".to_string()));
        assert!(result.columns.contains(&"c__id".to_string()));
        assert!(result.columns.contains(&"c__name".to_string()));
    }

    #[test]
    fn test_diff_full_join_has_all_parts() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = OpTree::FullJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_full_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should have all 8 parts (1-7b)
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "Part 3");
        assert_sql_contains(&sql, "Part 4");
        assert_sql_contains(&sql, "Part 5");
        assert_sql_contains(&sql, "Part 6");
        assert_sql_contains(&sql, "Part 7a");
        assert_sql_contains(&sql, "Part 7b");
    }

    #[test]
    fn test_diff_full_join_null_padding_both_sides() {
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id", "val"]);
        let right = scan(2, "b", "public", "b", &["id", "name"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = OpTree::FullJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_full_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should have NULL padding for both sides
        assert_sql_contains(&sql, "NULL AS");
    }

    #[test]
    fn test_diff_full_join_delta_flags() {
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = OpTree::FullJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_full_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should have pre-computed delta flags for both sides
        assert_sql_contains(&sql, "has_ins");
        assert_sql_contains(&sql, "has_del");
    }

    #[test]
    fn test_diff_full_join_not_deduplicated() {
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = OpTree::FullJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_full_join(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_full_join_error_on_non_full_join_node() {
        let mut ctx = test_ctx();
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_full_join(&mut ctx, &tree);
        assert!(result.is_err());
    }

    // ── Nested join tests ───────────────────────────────────────────

    #[test]
    fn test_diff_full_join_nested_left_child() {
        // (a ⋈ b) FULL JOIN c — left child is a nested inner join
        let a = scan(1, "a", "public", "a", &["id", "bid"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "bid", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = OpTree::FullJoin {
            condition: eq_cond("a", "id", "c", "id"),
            left: Box::new(inner),
            right: Box::new(c),
        };

        let mut ctx = test_ctx();
        let result = diff_full_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "full join with nested left child should diff: {result:?}"
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert_sql_contains(&sql, "UNION ALL");
    }

    #[test]
    fn test_diff_full_join_nested_both_children() {
        // (a ⋈ b) FULL JOIN (c ⋈ d) — both children are nested joins
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let left = inner_join(eq_cond("a", "id", "b", "id"), a, b);

        let c = scan(3, "c", "public", "c", &["id"]);
        let d = scan(4, "d", "public", "d", &["id"]);
        let right = inner_join(eq_cond("c", "id", "d", "id"), c, d);

        let tree = OpTree::FullJoin {
            condition: eq_cond("a", "id", "c", "id"),
            left: Box::new(left),
            right: Box::new(right),
        };

        let mut ctx = test_ctx();
        let result = diff_full_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "full join with nested children on both sides should diff: {result:?}"
        );
    }
}
