//! Full outer join differentiation.
//!
//! FULL OUTER JOIN = INNER JOIN + left anti-join + right anti-join.
//!
//! The delta is a multi-part UNION ALL:
//!
//! 1. **Part 1a** — delta_left INSERTS JOIN R₁ (matching insert rows)
//! 2. **Part 1b** — delta_left DELETES JOIN R₀ (matching delete rows, EC-01 fix)
//! 3. **Part 2** — current_left JOIN delta_right (matching rows)
//! 4. **Part 3a** — delta_left INSERTS anti-join R₁ (non-matching left → NULL right)
//! 5. **Part 3b** — delta_left DELETES anti-join R₀ (non-matching left → NULL right)
//! 6. **Part 4** — Delete stale NULL-padded left rows when new right matches
//! 7. **Part 5** — Insert NULL-padded left rows when last right match removed
//! 8. **Part 6** — delta_right anti-join left (non-matching right → NULL left)
//! 9. **Part 7** — Symmetric transitions for right side gaining/losing left matches
//!
//! Parts 1-5 mirror the LEFT JOIN operator (outer_join.rs). Parts 6-7 add
//! the symmetric right-side anti-join handling.
//!
//! ## EC-01 fix: R₀ for DELETE deltas in Parts 1 and 3
//!
//! Same as LEFT JOIN: Part 1 and Part 3 together partition delta_left rows
//! into "matched" and "unmatched". When the right partner is simultaneously
//! deleted, using R₁ (post-change) misses the match. Split by action:
//! - INSERTs check R₁ (need current right state)
//! - DELETEs check R₀ (need pre-change right state)

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join::mark_leaf_delta_ctes_not_materialized;
use crate::dvm::operators::join_common::{
    build_pre_change_snapshot_sql, build_snapshot_sql, is_join_child, rewrite_join_condition,
    use_pre_change_snapshot,
};
use crate::dvm::parser::OpTree;
use crate::error::PgTrickleError;

/// Differentiate a FullJoin node.
pub fn diff_full_join(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
    let OpTree::FullJoin {
        condition,
        left,
        right,
    } = op
    else {
        return Err(PgTrickleError::InternalError(
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

    // ── EC-01: Pre-change right snapshot for Parts 1b / 3b ─────────
    //
    // Same fix as inner_join / outer_join: when a left DELETE's old right
    // partner is simultaneously deleted, R₁ misses the match.
    let use_r0 = use_pre_change_snapshot(right, ctx.inside_semijoin);

    let r0_snapshot = if use_r0 {
        if is_join_child(right) {
            // EC01B-1: Deep join child — per-leaf CTE-based snapshot
            let pre_change = build_pre_change_snapshot_sql(right, &ctx.scan_delta_ctes);
            mark_leaf_delta_ctes_not_materialized(right, ctx);
            Some(pre_change)
        } else {
            let right_all_cols: String = right_cols
                .iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            let right_alias = right.alias();
            let r0 = format!(
                "(SELECT {right_all_cols} FROM {right_table} {ra} \
                 EXCEPT ALL \
                 SELECT {right_all_cols} FROM {delta_right} WHERE __pgt_action = 'I' \
                 UNION ALL \
                 SELECT {right_all_cols} FROM {delta_right} WHERE __pgt_action = 'D')",
                ra = quote_ident(right_alias),
                delta_right = right_result.cte_name,
            );
            Some(r0)
        }
    } else {
        None
    };

    if use_r0 {
        ctx.mark_cte_not_materialized(&right_result.cte_name);
    }

    // ── Pre-compute delta action flags for both sides ──────────────
    let left_flags_cte = ctx.next_cte_name("fj_left_flags");
    ctx.add_cte(
        left_flags_cte.clone(),
        format!(
            "SELECT bool_or(__pgt_action = 'I') AS has_ins,\
                    bool_or(__pgt_action = 'D') AS has_del \
             FROM {delta_left}",
            delta_left = left_result.cte_name,
        ),
    );

    let right_flags_cte = ctx.next_cte_name("fj_right_flags");
    ctx.add_cte(
        right_flags_cte.clone(),
        format!(
            "SELECT bool_or(__pgt_action = 'I') AS has_ins,\
                    bool_or(__pgt_action = 'D') AS has_del \
             FROM {delta_right}",
            delta_right = right_result.cte_name,
        ),
    );

    let cte_name = ctx.next_cte_name("full_join");

    let sql = if let Some(ref r0) = r0_snapshot {
        // ── EC-01: Split Part 1 and Part 3 by action ────────────────
        format!(
            "\
-- Part 1a: delta_left INSERTS JOIN current_right R₁ (matching insert rows)
SELECT pgtrickle.pg_trickle_hash_multi(ARRAY[dl.__pgt_row_id::TEXT, pgtrickle.pg_trickle_hash(row_to_json(r)::text)::TEXT]) AS __pgt_row_id,
       dl.__pgt_action,
       {part1_cols}
FROM {delta_left} dl
JOIN {right_table} r ON {join_cond_part1}
WHERE dl.__pgt_action = 'I'

UNION ALL

-- Part 1b: delta_left DELETES JOIN pre-change_right R₀ (EC-01 fix)
SELECT pgtrickle.pg_trickle_hash_multi(ARRAY[dl.__pgt_row_id::TEXT, pgtrickle.pg_trickle_hash(row_to_json(r)::text)::TEXT]) AS __pgt_row_id,
       dl.__pgt_action,
       {part1_cols}
FROM {delta_left} dl
JOIN {r0_snapshot} r ON {join_cond_part1}
WHERE dl.__pgt_action = 'D'

UNION ALL

-- Part 2: current_left JOIN delta_right
SELECT pgtrickle.pg_trickle_hash_multi(ARRAY[pgtrickle.pg_trickle_hash(row_to_json(l)::text)::TEXT, dr.__pgt_row_id::TEXT]) AS __pgt_row_id,
       dr.__pgt_action,
       {part2_cols}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}

UNION ALL

-- Part 3a: delta_left INSERTS anti-join R₁ (non-matching inserts → NULL right cols)
SELECT dl.__pgt_row_id,
       dl.__pgt_action,
       {antijoin_left_cols}
FROM {delta_left} dl
WHERE dl.__pgt_action = 'I'
  AND NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {join_cond_antijoin_l}
)

UNION ALL

-- Part 3b: delta_left DELETES anti-join R₀ (non-matching deletes → NULL right cols)
SELECT dl.__pgt_row_id,
       dl.__pgt_action,
       {antijoin_left_cols}
FROM {delta_left} dl
WHERE dl.__pgt_action = 'D'
  AND NOT EXISTS (
    SELECT 1 FROM {r0_snapshot} r WHERE {join_cond_antijoin_l}
)

UNION ALL

-- Part 4: Delete stale NULL-padded left rows when new right matches appear
SELECT 0::BIGINT AS __pgt_row_id,
       'D'::TEXT AS __pgt_action,
       {l_null_right_padded}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgt_action = 'I'
  AND (SELECT has_ins FROM {right_flags_cte})

UNION ALL

-- Part 5: Insert NULL-padded left rows when left row loses all right matches
SELECT 0::BIGINT AS __pgt_row_id,
       'I'::TEXT AS __pgt_action,
       {l_null_right_padded}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgt_action = 'D'
  AND (SELECT has_del FROM {right_flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {not_exists_cond_lr}
)

UNION ALL

-- Part 6: delta_right anti-join left (non-matching right rows → NULL left cols)
SELECT dr.__pgt_row_id,
       dr.__pgt_action,
       {antijoin_right_cols}
FROM {delta_right} dr
WHERE NOT EXISTS (
    SELECT 1 FROM {left_table} l WHERE {join_cond_antijoin_r}
)

UNION ALL

-- Part 7a: Delete stale NULL-padded right rows when new left matches appear
SELECT 0::BIGINT AS __pgt_row_id,
       'D'::TEXT AS __pgt_action,
       {null_left_r_padded}
FROM {right_table} r
JOIN {delta_left} dl ON {join_cond_antijoin_l}
WHERE dl.__pgt_action = 'I'
  AND (SELECT has_ins FROM {left_flags_cte})

UNION ALL

-- Part 7b: Insert NULL-padded right rows when right row loses all left matches
SELECT 0::BIGINT AS __pgt_row_id,
       'I'::TEXT AS __pgt_action,
       {null_left_r_padded}
FROM {right_table} r
JOIN {delta_left} dl ON {join_cond_antijoin_l}
WHERE dl.__pgt_action = 'D'
  AND (SELECT has_del FROM {left_flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {left_table} l WHERE {not_exists_cond_lr}
)",
            delta_left = left_result.cte_name,
            delta_right = right_result.cte_name,
            r0_snapshot = r0,
            left_flags_cte = left_flags_cte,
            right_flags_cte = right_flags_cte,
        )
    } else {
        // Right child is complex — keep Part 1 and Part 3 unsplit.
        format!(
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

-- Part 3: delta_left anti-join right (non-matching left rows → NULL right cols)
SELECT dl.__pgt_row_id,
       dl.__pgt_action,
       {antijoin_left_cols}
FROM {delta_left} dl
WHERE NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {join_cond_antijoin_l}
)

UNION ALL

-- Part 4: Delete stale NULL-padded left rows when new right matches appear
SELECT 0::BIGINT AS __pgt_row_id,
       'D'::TEXT AS __pgt_action,
       {l_null_right_padded}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgt_action = 'I'
  AND (SELECT has_ins FROM {right_flags_cte})

UNION ALL

-- Part 5: Insert NULL-padded left rows when left row loses all right matches
SELECT 0::BIGINT AS __pgt_row_id,
       'I'::TEXT AS __pgt_action,
       {l_null_right_padded}
FROM {left_table} l
JOIN {delta_right} dr ON {join_cond_part2}
WHERE dr.__pgt_action = 'D'
  AND (SELECT has_del FROM {right_flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {right_table} r WHERE {not_exists_cond_lr}
)

UNION ALL

-- Part 6: delta_right anti-join left (non-matching right rows → NULL left cols)
SELECT dr.__pgt_row_id,
       dr.__pgt_action,
       {antijoin_right_cols}
FROM {delta_right} dr
WHERE NOT EXISTS (
    SELECT 1 FROM {left_table} l WHERE {join_cond_antijoin_r}
)

UNION ALL

-- Part 7a: Delete stale NULL-padded right rows when new left matches appear
SELECT 0::BIGINT AS __pgt_row_id,
       'D'::TEXT AS __pgt_action,
       {null_left_r_padded}
FROM {right_table} r
JOIN {delta_left} dl ON {join_cond_antijoin_l}
WHERE dl.__pgt_action = 'I'
  AND (SELECT has_ins FROM {left_flags_cte})

UNION ALL

-- Part 7b: Insert NULL-padded right rows when right row loses all left matches
SELECT 0::BIGINT AS __pgt_row_id,
       'I'::TEXT AS __pgt_action,
       {null_left_r_padded}
FROM {right_table} r
JOIN {delta_left} dl ON {join_cond_antijoin_l}
WHERE dl.__pgt_action = 'D'
  AND (SELECT has_del FROM {left_flags_cte})
  AND NOT EXISTS (
    SELECT 1 FROM {left_table} l WHERE {not_exists_cond_lr}
)",
            delta_left = left_result.cte_name,
            delta_right = right_result.cte_name,
            left_flags_cte = left_flags_cte,
            right_flags_cte = right_flags_cte,
        )
    };

    ctx.add_cte(cte_name.clone(), sql);

    Ok(DiffResult {
        cte_name,
        columns: output_cols,
        is_deduplicated: false,
        has_key_changed: false,
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

        // EC-01: Parts 1 and 3 are split when right child is simple (Scan)
        assert_sql_contains(&sql, "Part 1a");
        assert_sql_contains(&sql, "Part 1b");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "Part 3a");
        assert_sql_contains(&sql, "Part 3b");
        assert_sql_contains(&sql, "Part 4");
        assert_sql_contains(&sql, "Part 5");
        assert_sql_contains(&sql, "Part 6");
        assert_sql_contains(&sql, "Part 7a");
        assert_sql_contains(&sql, "Part 7b");
    }

    #[test]
    fn test_ec01_full_join_r0_uses_except_all() {
        // For Scan right children, Part 1b and Part 3b should use R₀ via
        // EXCEPT ALL to find pre-change right partners.
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id", "name"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = OpTree::FullJoin {
            condition: cond,
            left: Box::new(left),
            right: Box::new(right),
        };
        let result = diff_full_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // R₀ uses EXCEPT ALL pattern
        assert_sql_contains(&sql, "EXCEPT ALL");
        // Part 1b and Part 3b present
        assert_sql_contains(&sql, "Part 1b");
        assert_sql_contains(&sql, "Part 3b");
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

    // ── Multi-table cross-type join chain tests ─────────────────────

    /// `A FULL JOIN (B SEMI JOIN C)` — right child is a semi-join.
    ///
    /// A full outer join where the right sub-tree filters rows via a semi-join.
    /// Verifies that the full-join differentiator handles a non-standard right
    /// child that emits only left-side columns.
    #[test]
    fn test_diff_full_join_with_semi_join_right_child() {
        // B SEMI JOIN C: orders EXISTS IN customers
        let b = scan(2, "orders", "public", "o", &["id", "cust_id"]);
        let c = scan(3, "customers", "public", "c", &["id"]);
        let right = OpTree::SemiJoin {
            condition: eq_cond("o", "cust_id", "c", "id"),
            left: Box::new(b),
            right: Box::new(c),
        };

        // A FULL JOIN (B SEMI JOIN C): regions FULL JOIN above
        let a = scan(1, "regions", "public", "r", &["id", "name"]);
        let tree = OpTree::FullJoin {
            condition: eq_cond("r", "id", "o", "id"),
            left: Box::new(a),
            right: Box::new(right),
        };

        let mut ctx = test_ctx();
        let result = diff_full_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "A FULL JOIN (B SEMI JOIN C) should succeed: {result:?}"
        );

        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        // Full join always emits UNION ALL across many parts
        assert_sql_contains(&sql, "UNION ALL");
        // SQL must reference the semi-join EXISTS check
        assert!(
            sql.contains("EXISTS"),
            "right sub-tree (semi-join) must appear via EXISTS"
        );
    }

    /// `(A FULL JOIN B) INNER JOIN C` — multi-type chain: outer then inner.
    ///
    /// A full join feeding into an inner join. The inner join's left child
    /// produces nullable columns from the full join; the inner join must
    /// not strip the nullable flag.
    #[test]
    fn test_diff_full_join_as_left_child_of_inner_join() {
        use crate::dvm::operators::join::diff_inner_join;

        // A FULL JOIN B
        let a = scan(1, "a", "public", "a", &["id", "val"]);
        let b = scan(2, "b", "public", "b", &["id", "score"]);
        let full = OpTree::FullJoin {
            condition: eq_cond("a", "id", "b", "id"),
            left: Box::new(a),
            right: Box::new(b),
        };

        // (A FULL JOIN B) INNER JOIN C
        let c = scan(3, "c", "public", "c", &["id", "flag"]);
        let tree = inner_join(eq_cond("a", "id", "c", "id"), full, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "(A FULL JOIN B) INNER JOIN C should succeed: {result:?}"
        );

        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert_sql_contains(&sql, "UNION ALL");
    }
}
