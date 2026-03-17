//! Inner join differentiation.
//!
//! ΔI(Q ⋈C R) = (ΔQ_I ⋈C R₁) + (ΔQ_D ⋈C R₀) + (Q₀ ⋈C ΔR)
//!
//! Where:
//! - R₁ = current state of R (post-change, i.e. live table)
//! - R₀ = pre-change state of R, reconstructed as
//!   R_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
//! - Q₀ = pre-change state of Q, reconstructed similarly
//! - ΔQ_I, ΔQ_D = insert and delete deltas for the left side
//! - ΔR = delta for the right side
//!
//! ## EC-01 fix: R₀ for DELETE deltas in Part 1
//!
//! The original formula `(ΔQ ⋈ R₁)` reads the right side AFTER all
//! changes. When the old join partner on the right is deleted before
//! the delta runs, the DELETE half finds no match and is silently
//! dropped, leaving a stale row in the stream table.
//!
//! The fix splits Part 1 into:
//! - **Part 1a**: ΔQ_inserts ⋈ R₁ (post-change right — new rows need current partners)
//! - **Part 1b**: ΔQ_deletes ⋈ R₀ (pre-change right — old rows need old partners)
//!
//! R₀ is computed identically to Q₀: `R_current EXCEPT ALL ΔR_I UNION ALL ΔR_D`.
//! The split is applied when `use_r0` is true (simple right children).
//!
//! Using Q₀ (pre-change) in Part 2 instead of Q₁ (post-change) avoids
//! double-counting when both sides change simultaneously: Part 1 already
//! handles (ΔQ ⋈ R₁), and using Q₀ excludes newly-inserted Q rows that
//! would duplicate Part 1's contribution.
//!
//! Q₀ is computed via EXCEPT ALL for:
//! - **Scan children**: one table scan + small delta (cheap)
//! - **Subquery/Aggregate children**: snapshot + delta EXCEPT ALL
//!   (critical for correlated sources like Q15 where revenue0 and
//!   MAX(total_revenue) both depend on lineitem)
//! - **Non-SemiJoin join children (≤ 2 tables)**: full join snapshot +
//!   EXCEPT ALL.  Limited to small subtrees (≤ 2 scan nodes) because
//!   larger join snapshots spill multiple GB of temp files at SF=0.01.
//!   Safe because the Q21 regression only affects SemiJoin interactions.
//!
//! For **SemiJoin-containing join children**, Q₀ via EXCEPT ALL interacts
//! badly with SemiJoin R_old (Q21 numwait regression), so Q₁ is used
//! with a correction term for shallow cases (Part 3).
//!
//! ## Nested join children — correction term (Part 3)
//!
//! For Scan children, Q₀ is computed cheaply via EXCEPT ALL / UNION ALL.
//! For nested join children (multi-table chains), computing Q₀ requires
//! the full join snapshot plus EXCEPT ALL — prohibitively expensive.
//! Instead, Part 2 uses Q₁ (post-change) and a **correction term** (Part 3)
//! that subtracts the double-counted rows:
//!
//! ```text
//! Error = (Q₁ − Q₀) ⋈ ΔR = (ΔQ_I − ΔQ_D) ⋈ ΔR
//! ```
//!
//! Part 3 joins ΔQ with ΔR directly:
//! - For ΔQ_I rows (inserts): emit with FLIPPED dr action (cancels excess)
//! - For ΔQ_D rows (deletes): emit with dr action (adds missing contribution)
//!
//! The correction is only applied when the left child is a **shallow** join
//! (both of its children are simple Scan nodes).  For deeper chains (e.g.
//! Q08 with 8 tables), cascading corrections at every level generate SQL
//! too complex for PostgreSQL's planner, so the correction is skipped.
//!
//! ## Semi-join optimization
//!
//! When the base table is scanned for the "current" side of the join,
//! a semi-join filter limits the scan to rows whose join keys appear in
//! the delta of the other side. For example, Part 2 becomes:
//!
//! ```sql
//! FROM (SELECT * FROM left_table
//!       WHERE left_key IN (SELECT DISTINCT right_key FROM delta_right)
//! ) l
//! JOIN delta_right dr ON ...
//! ```
//!
//! This converts a full sequential scan of the base table into an indexed
//! lookup when the join key has an index, providing 10x+ speedup at low
//! change rates.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::operators::join_common::{
    build_base_table_key_exprs, build_snapshot_sql, is_join_child, is_simple_child,
    join_scan_count, rewrite_join_condition, use_pre_change_snapshot,
};
use crate::dvm::parser::{Expr, OpTree};
use crate::error::PgTrickleError;

/// Returns true if `op` is a join whose **both** children are simple (Scan)
/// nodes.  Previously this gated Part 3 correction term generation — now
/// used only for diagnostics; Part 3 is generated for all join children
/// via `is_join_child()` to fix Q07-type double-counting errors.
fn is_shallow_join(op: &OpTree) -> bool {
    match op {
        OpTree::InnerJoin { left, right, .. } | OpTree::LeftJoin { left, right, .. } => {
            is_simple_child(left) && is_simple_child(right)
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => is_shallow_join(child),
        _ => false,
    }
}

/// Differentiate an InnerJoin node.
pub fn diff_inner_join(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
    let OpTree::InnerJoin {
        condition,
        left,
        right,
    } = op
    else {
        return Err(PgTrickleError::InternalError(
            "diff_inner_join called on non-InnerJoin node".into(),
        ));
    };

    // Differentiate both children
    let left_result = ctx.diff_node(left)?;
    let right_result = ctx.diff_node(right)?;

    // Get the base table references for the current snapshot.
    // For Scan children this is the table name; for nested joins
    // this is a snapshot subquery with disambiguated columns.
    let left_table = build_snapshot_sql(left);
    let right_table = build_snapshot_sql(right);

    let left_cols = &left_result.columns;
    let right_cols = &right_result.columns;

    // Disambiguate output columns using table-alias prefixed names.
    // This prevents collisions when both sides have columns with the same
    // name (e.g., both have "id", "val"). The project diff knows how to
    // resolve qualified ColumnRef(table="l", col="id") → "l__id".
    let left_prefix = left.alias();
    let right_prefix = right.alias();

    let mut output_cols = Vec::new();
    for c in left_cols {
        output_cols.push(format!("{left_prefix}__{c}"));
    }
    for c in right_cols {
        output_cols.push(format!("{right_prefix}__{c}"));
    }

    let left_col_refs: Vec<String> = left_cols
        .iter()
        .map(|c| {
            format!(
                "dl.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{left_prefix}__{c}"))
            )
        })
        .collect();
    let right_col_refs: Vec<String> = right_cols
        .iter()
        .map(|c| {
            format!(
                "r.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{right_prefix}__{c}"))
            )
        })
        .collect();
    let left_col_refs2: Vec<String> = left_cols
        .iter()
        .map(|c| {
            format!(
                "l.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{left_prefix}__{c}"))
            )
        })
        .collect();
    let right_col_refs2: Vec<String> = right_cols
        .iter()
        .map(|c| {
            format!(
                "dr.{} AS {}",
                quote_ident(c),
                quote_ident(&format!("{right_prefix}__{c}"))
            )
        })
        .collect();

    let all_cols_part1 = [left_col_refs.as_slice(), right_col_refs.as_slice()]
        .concat()
        .join(", ");
    let all_cols_part2 = [left_col_refs2.as_slice(), right_col_refs2.as_slice()]
        .concat()
        .join(", ");

    // Part 3 correction term columns: left data from delta_left (dl), right
    // data from delta_right (dr). Reuses left_col_refs (which reference dl.)
    // and right_col_refs2 (which reference dr.).
    let all_cols_correction = [left_col_refs.as_slice(), right_col_refs2.as_slice()]
        .concat()
        .join(", ");

    // Row ID: hash of both child row IDs.
    // For the delta side, we use __pgt_row_id from the delta CTE.
    // For the base table side, we hash its PK/non-nullable columns
    // instead of serializing the entire row with row_to_json().
    //
    // S1 optimization: flatten into a single pg_trickle_hash_multi call with
    // all key columns inline, avoiding nested hash calls.
    // For nested join children, falls back to row_to_json for the snapshot side.
    let right_key_exprs = build_base_table_key_exprs(right, "r");
    let left_key_exprs = build_base_table_key_exprs(left, "l");

    let mut hash1_args = vec!["dl.__pgt_row_id::TEXT".to_string()];
    hash1_args.extend(right_key_exprs);
    let hash_part1 = format!(
        "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
        hash1_args.join(", ")
    );

    let mut hash2_args = left_key_exprs;
    hash2_args.push("dr.__pgt_row_id::TEXT".to_string());
    let hash_part2 = format!(
        "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
        hash2_args.join(", ")
    );

    // Rewrite join condition with aliases for each part.
    // The original condition uses the source table aliases (e.g. o.cust_id = c.id).
    // Part 1 needs: dl (delta left) + r (base right).
    // Part 2 needs: l (pre-change left) + dr (delta right).
    //
    // For nested join children, column names are disambiguated with the
    // original table alias prefix (e.g., o.cust_id → dl."o__cust_id").
    let join_cond_part1 = rewrite_join_condition(condition, left, "dl", right, "r");
    let join_cond_part2 = rewrite_join_condition(condition, left, "l", right, "dr");

    // Part 3 correction condition: dl (delta left) + dr (delta right).
    // Used when Part 2 uses L₁ (post-change) instead of L₀.  The
    // correction cancels the (ΔL ⋈ ΔR) double-counting error introduced
    // by using L₁.  Generated for join children up to 3 scan nodes
    // (one level beyond the L₀ threshold of ≤ 2).  This fixes Q07-type
    // revenue drift in 6-table join chains.  Deeper levels (4+ scans)
    // omit the correction to avoid cascading CTE complexity that causes
    // temp file bloat on PostgreSQL.
    //
    // The correction is computed eagerly here but only emitted when
    // `!use_l0` (see `correction_sql` below).
    let join_cond_correction =
        if !is_simple_child(left) && is_join_child(left) && join_scan_count(left) <= 3 {
            Some(rewrite_join_condition(condition, left, "dl", right, "dr"))
        } else {
            None
        };

    // Extract equi-join key pairs for semi-join optimization.
    // If we can identify (left_key, right_key) pairs from the condition,
    // we filter the base table scan to only matching keys from the delta.
    //
    // Skip the optimization when either child is a nested join — the
    // column names in the condition don't directly match the snapshot
    // or delta CTE columns for complex children.
    let equi_keys = if is_simple_child(left) && is_simple_child(right) {
        extract_equijoin_keys(condition, left_prefix, right_prefix)
    } else {
        vec![]
    };

    // Build semi-join-filtered table references.
    // Part 1: right base table filtered by delta-left join keys
    let right_table_filtered = build_semijoin_subquery(
        &right_table,
        &equi_keys,
        &left_result.cte_name,
        JoinSide::Right,
    );
    // ── Pre-change snapshot for Part 2 ────────────────────────────
    //
    // Standard DBSP: ΔJ = (ΔL ⋈ R₁) + (L₀ ⋈ ΔR)
    //
    // L₀ = the state of the left child BEFORE the current cycle's changes.
    // Reconstructed as: L_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes.
    //
    // For Scan children and Subquery/Aggregate children, L₀ is computed
    // via EXCEPT ALL on the snapshot. For Subquery children (e.g.
    // revenue0 in Q15), this is critical when the inner subquery and
    // outer child share a common source table — using L₁ instead of L₀
    // causes missed DELETE rows and duplicate INSERT rows.
    //
    // For nested join children that contain SemiJoin/AntiJoin operators,
    // computing L₀ via EXCEPT ALL interacts badly with SemiJoin R_old
    // (Q21 numwait regression). These use L₁ + correction term for
    // shallow joins, or plain L₁ for deeper chains.
    //
    // For nested join children WITHOUT SemiJoin/AntiJoin (pure InnerJoin/
    // LeftJoin chains), L₀ via EXCEPT ALL is safe but expensive: the
    // full join snapshot must be materialized as temp files for the set
    // difference.  At SF=0.01, a 3-table join snapshot can still spill
    // several GB of temp files.  We limit L₀ to join subtrees with
    // ≤ 2 scan nodes (simple 2-table joins) to keep temp usage bounded
    // while still improving correctness at the first nesting level.
    //
    // Additionally, joins inside a SemiJoin/AntiJoin ancestor must use
    // L₁ to avoid Q21-type regressions where sub-join EXCEPT ALL
    // interacts with the SemiJoin's R_old computation.
    let use_l0 = use_pre_change_snapshot(left, ctx.inside_semijoin);

    let left_part2_source = if use_l0 {
        // Scan, Subquery/Aggregate child, or non-SemiJoin join child:
        // use L₀ via EXCEPT ALL
        let left_data_cols: String = left_cols
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");

        let left_alias = left.alias();
        let left_pre_change = format!(
            "(SELECT {left_data_cols} FROM {left_table} {la} \
             EXCEPT ALL \
             SELECT {left_data_cols} FROM {delta_left} WHERE __pgt_action = 'I' \
             UNION ALL \
             SELECT {left_data_cols} FROM {delta_left} WHERE __pgt_action = 'D')",
            la = quote_ident(left_alias),
            delta_left = left_result.cte_name,
        );
        // Apply semi-join filter to L₀ if equi-keys are available
        if equi_keys.is_empty() {
            left_pre_change
        } else {
            let filters: Vec<String> = equi_keys
                .iter()
                .map(|(left_key, right_key)| {
                    format!(
                        "{left_key} IN (SELECT DISTINCT {right_key} FROM {})",
                        right_result.cte_name
                    )
                })
                .collect();
            format!(
                "(SELECT * FROM {left_pre_change} __l0 WHERE {filters})",
                filters = filters.join(" AND "),
            )
        }
    } else {
        // SemiJoin-containing nested join child: use post-change L₁ with
        // semi-join filter (L₀ via EXCEPT ALL interacts badly with
        // SemiJoin R_old, causing Q21-type regressions).
        // Shallow join children get a correction term (Part 3) below.
        build_semijoin_subquery(
            &left_table,
            &equi_keys,
            &right_result.cte_name,
            JoinSide::Left,
        )
    };

    // ── EC-01: Pre-change snapshot for Part 1 (right side) ──────────
    //
    // Standard formula: ΔJ = (ΔL ⋈ R₁) + (L₀ ⋈ ΔR)
    //
    // Part 1 joins delta_left with the right side. When a left-side row's
    // join key changes AND the old right partner is simultaneously
    // deleted, the DELETE half of Part 1 finds no match in R₁ (current
    // right) and the stale row is silently retained in the stream table.
    //
    // Fix: split Part 1 into two arms:
    //   Part 1a: ΔL_inserts ⋈ R₁  (inserts need current right partners)
    //   Part 1b: ΔL_deletes ⋈ R₀  (deletes need pre-change right partners)
    //
    // R₀ = R_current EXCEPT ALL ΔR_inserts UNION ALL ΔR_deletes
    //
    // The same child-type heuristics as use_l0 apply: Scan and simple
    // children use R₀; SemiJoin-containing deep chains fall back to R₁.
    let use_r0 = use_pre_change_snapshot(right, ctx.inside_semijoin);

    let right_part1_source = if use_r0 {
        // Scan, Subquery/Aggregate child, or non-SemiJoin join child:
        // use R₀ via EXCEPT ALL for DELETE delta rows
        let right_data_cols: String = right_cols
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");

        let right_alias = right.alias();
        let right_pre_change = format!(
            "(SELECT {right_data_cols} FROM {right_table} {ra} \
             EXCEPT ALL \
             SELECT {right_data_cols} FROM {delta_right} WHERE __pgt_action = 'I' \
             UNION ALL \
             SELECT {right_data_cols} FROM {delta_right} WHERE __pgt_action = 'D')",
            ra = quote_ident(right_alias),
            delta_right = right_result.cte_name,
        );
        // Apply semi-join filter to R₀ if equi-keys are available
        // (filter R₀ by join keys that appear in delta_left)
        if equi_keys.is_empty() {
            right_pre_change
        } else {
            let filters: Vec<String> = equi_keys
                .iter()
                .map(|(left_key, right_key)| {
                    format!(
                        "{right_key} IN (SELECT DISTINCT {left_key} FROM {})",
                        left_result.cte_name
                    )
                })
                .collect();
            format!(
                "(SELECT * FROM {right_pre_change} __r0 WHERE {filters})",
                filters = filters.join(" AND "),
            )
        }
    } else {
        // Nested/complex right child: fall back to R₁ (current right).
        // Part 1 is NOT split in this case — kept as-is for safety.
        right_table_filtered.clone()
    };

    let cte_name = ctx.next_cte_name("join");

    // ── Correction term for nested join children (Part 3) ───────────
    //
    // Standard DBSP: ΔJ = (ΔL ⋈ R₁) + (L₀ ⋈ ΔR)
    //
    // When using L₁ (post-change) instead of L₀ (pre-change) for nested
    // join children in Part 2, the error is:
    //   Error = (L₁ - L₀) ⋈ ΔR = (ΔL_I - ΔL_D) ⋈ ΔR
    //
    // Part 3 corrects this by joining both delta CTEs directly:
    //   - ΔL_I ⋈ ΔR rows are excess in Part 2 → emit with flipped action
    //   - ΔL_D ⋈ ΔR rows are missing from Part 2 → emit with original action
    //
    // This avoids computing L₀ for nested joins (expensive EXCEPT ALL on
    // full snapshot) and doesn't change the SemiJoin operator's inputs,
    // avoiding the Q21 regression that the previous L₀ approach caused.
    //
    // Only applied when Part 2 uses L₁ (!use_l0).  When L₀ is used
    // directly, the correction is unnecessary (no double-counting error).
    let correction_sql = if !use_l0 {
        if let Some(cond) = &join_cond_correction {
            // The correction adds a second FROM-clause reference to
            // delta_left.  PostgreSQL (12+) auto-materializes CTEs with
            // >= 2 references, which can spill huge temp files for join
            // delta CTEs.  Mark it NOT MATERIALIZED so PG inlines it.
            ctx.mark_cte_not_materialized(&left_result.cte_name);

            // Row ID for correction rows: hash of both delta row IDs.
            // For aggregate queries (Q03, Q10), the aggregate recomputes row_ids
            // from GROUP BY columns, so the join-level row_id doesn't need to
            // match Part 2's exactly.
            let hash_correction =
                "pgtrickle.pg_trickle_hash_multi(ARRAY[dl.__pgt_row_id::TEXT, dr.__pgt_row_id::TEXT])"
                    .to_string();

            format!(
                "

UNION ALL

-- Part 3: Correction for nested join L₁ → L₀
-- Cancels excess ΔL_I ⋈ ΔR and adds missing ΔL_D ⋈ ΔR.
-- For dl.action='I': these rows are in L₁ but not L₀ → flip dr action to cancel
-- For dl.action='D': these rows are in L₀ but not L₁ → keep dr action to add
SELECT {hash_correction} AS __pgt_row_id,
       CASE WHEN dl.__pgt_action = 'I'
            THEN CASE WHEN dr.__pgt_action = 'I' THEN 'D' ELSE 'I' END
            ELSE dr.__pgt_action
       END AS __pgt_action,
       {all_cols_correction}
FROM {delta_left} dl
JOIN {delta_right} dr ON {cond}",
                delta_left = left_result.cte_name,
                delta_right = right_result.cte_name,
            )
        } else {
            String::new()
        }
    } else {
        // L₀ is used directly — no correction needed.
        String::new()
    };

    // When use_r0 is true, R₀ via EXCEPT ALL references the right delta CTE
    // multiple times (for the EXCEPT ALL sub-selects). Mark it NOT MATERIALIZED
    // to prevent PostgreSQL from spilling temp files for CTE materialization.
    if use_r0 {
        ctx.mark_cte_not_materialized(&right_result.cte_name);
    }

    let sql = if use_r0 {
        // ── EC-01: Split Part 1 into 1a (inserts ⋈ R₁) + 1b (deletes ⋈ R₀)
        format!(
            "\
-- Part 1a: delta_left INSERTS JOIN current_right R₁ (semi-join filtered)
SELECT {hash_part1} AS __pgt_row_id,
       dl.__pgt_action,
       {all_cols_part1}
FROM {delta_left} dl
JOIN {right_table_filtered} r ON {join_cond_part1}
WHERE dl.__pgt_action = 'I'

UNION ALL

-- Part 1b: delta_left DELETES JOIN pre-change_right R₀ (EC-01 fix)
-- R₀ = R_current EXCEPT ALL ΔR_inserts UNION ALL ΔR_deletes
-- Ensures deleted left rows find their old right partner even when
-- the right partner was simultaneously deleted.
SELECT {hash_part1} AS __pgt_row_id,
       dl.__pgt_action,
       {all_cols_part1}
FROM {delta_left} dl
JOIN {right_part1_source} r ON {join_cond_part1}
WHERE dl.__pgt_action = 'D'

UNION ALL

-- Part 2: pre-change_left JOIN delta_right
-- For Scan children: L₀ = L_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
-- For nested joins: L₁ = current snapshot (semi-join filtered, corrected below)
SELECT {hash_part2} AS __pgt_row_id,
       dr.__pgt_action,
       {all_cols_part2}
FROM {left_part2_source} l
JOIN {delta_right} dr ON {join_cond_part2}{correction_sql}",
            delta_left = left_result.cte_name,
            delta_right = right_result.cte_name,
        )
    } else {
        // Right child is complex — keep Part 1 unsplit (no regression).
        format!(
            "\
-- Part 1: delta_left JOIN current_right (semi-join filtered)
SELECT {hash_part1} AS __pgt_row_id,
       dl.__pgt_action,
       {all_cols_part1}
FROM {delta_left} dl
JOIN {right_table_filtered} r ON {join_cond_part1}

UNION ALL

-- Part 2: pre-change_left JOIN delta_right
-- For Scan children: L₀ = L_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
-- For nested joins: L₁ = current snapshot (semi-join filtered, corrected below)
SELECT {hash_part2} AS __pgt_row_id,
       dr.__pgt_action,
       {all_cols_part2}
FROM {left_part2_source} l
JOIN {delta_right} dr ON {join_cond_part2}{correction_sql}",
            delta_left = left_result.cte_name,
            delta_right = right_result.cte_name,
        )
    };

    ctx.add_cte(cte_name.clone(), sql);

    Ok(DiffResult {
        cte_name,
        columns: output_cols,
        is_deduplicated: false,
    })
}

/// Which side of the join we are filtering.
enum JoinSide {
    /// We are filtering the left base table using right-side delta keys.
    Left,
    /// We are filtering the right base table using left-side delta keys.
    Right,
}

/// An equi-join key pair: `(left_column_sql, right_column_sql)`.
type EquiKeyPair = (String, String);

/// Extract equi-join key pairs from a join condition expression.
///
/// Walks the expression tree looking for `col_a = col_b` patterns,
/// including through AND conjunctions. Returns pairs of
/// `(left_table_key, right_table_key)` where each key is normalized
/// so the first element belongs to the left child and the second to
/// the right child, based on table alias matching.
///
/// Falls back gracefully: if the condition is too complex (OR, functions,
/// non-equality operators), returns an empty vec and we skip the
/// semi-join optimization.
fn extract_equijoin_keys(
    condition: &Expr,
    left_alias: &str,
    right_alias: &str,
) -> Vec<EquiKeyPair> {
    let mut keys = Vec::new();
    collect_equijoin_keys(condition, left_alias, right_alias, &mut keys);
    keys
}

/// Recursively collect equi-join key pairs from an expression.
///
/// Table qualifiers are stripped in the output because the keys are used
/// inside semi-join subqueries where the original table aliases are not
/// in scope. However, qualifiers are inspected first to correctly orient
/// each `(left_key, right_key)` pair based on which table alias each
/// side of the `=` belongs to.
fn collect_equijoin_keys(
    expr: &Expr,
    left_alias: &str,
    right_alias: &str,
    keys: &mut Vec<EquiKeyPair>,
) {
    match expr {
        Expr::BinaryOp { op, left, right } if op == "=" => {
            // Found an equality — determine which side belongs to which table.
            let left_stripped = left.strip_qualifier().to_sql();
            let right_stripped = right.strip_qualifier().to_sql();

            let left_belongs_to = classify_column_side(left, left_alias, right_alias);
            let right_belongs_to = classify_column_side(right, left_alias, right_alias);

            match (left_belongs_to, right_belongs_to) {
                (ColumnSide::Left, ColumnSide::Right) => {
                    // Normal order: left_of_= is left-table, right_of_= is right-table
                    keys.push((left_stripped, right_stripped));
                }
                (ColumnSide::Right, ColumnSide::Left) => {
                    // Swapped: left_of_= is right-table, right_of_= is left-table
                    keys.push((right_stripped, left_stripped));
                }
                _ => {
                    // Both same side or unknown — skip this key pair
                    // (semi-join optimization won't apply for this key)
                }
            }
        }
        Expr::BinaryOp { op, left, right } if op.eq_ignore_ascii_case("AND") => {
            // AND conjunction — recurse into both sides
            collect_equijoin_keys(left, left_alias, right_alias, keys);
            collect_equijoin_keys(right, left_alias, right_alias, keys);
        }
        _ => {
            // Non-equality / non-AND: skip (don't add anything).
            // The optimization will be skipped if no keys are found.
        }
    }
}

/// Which side of the join a column belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ColumnSide {
    Left,
    Right,
    Unknown,
}

/// Determine which side of the join a column expression belongs to,
/// based on its table alias.
fn classify_column_side(expr: &Expr, left_alias: &str, right_alias: &str) -> ColumnSide {
    if let Expr::ColumnRef {
        table_alias: Some(alias),
        ..
    } = expr
    {
        if alias == left_alias {
            ColumnSide::Left
        } else if alias == right_alias {
            ColumnSide::Right
        } else {
            ColumnSide::Unknown
        }
    } else {
        ColumnSide::Unknown
    }
}

/// Build a semi-join-filtered subquery for a base table.
///
/// Given equi-join key pairs and the delta CTE name, wraps the base table
/// in a subquery that filters to only rows matching the delta's join keys.
///
/// For `JoinSide::Left`, we filter the left table using right-side keys
/// from the right delta CTE:
/// ```sql
/// (SELECT * FROM left_table WHERE left_key IN
///    (SELECT DISTINCT right_key FROM delta_right))
/// ```
///
/// If no equi-join keys were extracted, returns the plain table reference
/// (no optimization applied).
fn build_semijoin_subquery(
    base_table: &str,
    equi_keys: &[EquiKeyPair],
    delta_cte: &str,
    side: JoinSide,
) -> String {
    if equi_keys.is_empty() {
        return base_table.to_string();
    }

    // Build WHERE ... AND ... clauses for each key pair
    let filters: Vec<String> = equi_keys
        .iter()
        .map(|(left_key, right_key)| {
            match side {
                JoinSide::Left => {
                    // Filtering left table: left_key IN (SELECT DISTINCT right_key FROM delta_right)
                    format!("{left_key} IN (SELECT DISTINCT {right_key} FROM {delta_cte})")
                }
                JoinSide::Right => {
                    // Filtering right table: right_key IN (SELECT DISTINCT left_key FROM delta_left)
                    format!("{right_key} IN (SELECT DISTINCT {left_key} FROM {delta_cte})")
                }
            }
        })
        .collect();

    format!(
        "(SELECT * FROM {base_table} WHERE {filters})",
        filters = filters.join(" AND "),
    )
}

/// Get the current-state table reference for a node.
/// Delegates to `join_common::build_snapshot_sql` for the actual implementation.
/// Kept as a local alias for backward compatibility with test assertions.
#[cfg(test)]
fn get_current_table_ref(op: &OpTree) -> String {
    build_snapshot_sql(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;
    use proptest::prelude::*;

    // ── diff_inner_join tests ───────────────────────────────────────

    #[test]
    fn test_diff_inner_join_basic() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();

        // Output columns should be disambiguated with table prefixes
        assert!(result.columns.contains(&"o__id".to_string()));
        assert!(result.columns.contains(&"o__cust_id".to_string()));
        assert!(result.columns.contains(&"c__id".to_string()));
        assert!(result.columns.contains(&"c__name".to_string()));
    }

    #[test]
    fn test_diff_inner_join_two_parts() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // EC-01: Part 1 is now split into 1a (inserts ⋈ R₁) + 1b (deletes ⋈ R₀)
        assert_sql_contains(&sql, "Part 1a");
        assert_sql_contains(&sql, "Part 1b");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "pre-change_left");
        assert_sql_contains(&sql, "pre-change_right R");
    }

    #[test]
    fn test_diff_inner_join_pre_change_snapshot() {
        let mut ctx = test_ctx();
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Part 2 should use L₀ = L_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes
        // Part 1b should use R₀ = R_current EXCEPT ALL ΔR_inserts UNION ALL ΔR_deletes
        assert_sql_contains(&sql, "EXCEPT ALL");
        assert_sql_contains(&sql, "__pgt_action = 'I'");
        assert_sql_contains(&sql, "__pgt_action = 'D'");
        // Both L₀ and R₀ should be present (at least two EXCEPT ALL occurrences)
        let except_count = sql.matches("EXCEPT ALL").count();
        assert!(
            except_count >= 2,
            "expected ≥2 EXCEPT ALL (L₀ + R₀), got {except_count}\n{sql}"
        );
    }

    #[test]
    fn test_diff_inner_join_not_deduplicated() {
        let mut ctx = test_ctx();
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let tree = inner_join(cond, left, right);
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_inner_join_error_on_non_join_node() {
        let mut ctx = test_ctx();
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_inner_join(&mut ctx, &tree);
        assert!(result.is_err());
    }

    // ── extract_equijoin_keys tests ─────────────────────────────────

    #[test]
    fn test_extract_equijoin_keys_simple_equality() {
        // Condition: o.cust_id = c.id → left alias "o", right alias "c"
        let cond = eq_cond("o", "cust_id", "c", "id");
        let keys = extract_equijoin_keys(&cond, "o", "c");
        assert_eq!(keys.len(), 1);
        // Normalized: (left_table_key, right_table_key)
        assert!(keys[0].0.contains("cust_id"));
        assert!(keys[0].1.contains("id"));
    }

    #[test]
    fn test_extract_equijoin_keys_swapped_order() {
        // Condition: c.id = o.cust_id (right-table col on LEFT of =)
        let cond = eq_cond("c", "id", "o", "cust_id");
        let keys = extract_equijoin_keys(&cond, "o", "c");
        assert_eq!(keys.len(), 1);
        // Should be normalized: (left_table_key, right_table_key)
        assert!(keys[0].0.contains("cust_id"));
        assert!(keys[0].1.contains("id"));
    }

    #[test]
    fn test_extract_equijoin_keys_and_condition() {
        let cond = binop(
            "AND",
            eq_cond("o", "a", "c", "b"),
            eq_cond("o", "x", "c", "y"),
        );
        let keys = extract_equijoin_keys(&cond, "o", "c");
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn test_extract_equijoin_keys_non_equality_returns_empty() {
        let cond = binop(">", qcolref("o", "a"), qcolref("c", "b"));
        let keys = extract_equijoin_keys(&cond, "o", "c");
        assert!(keys.is_empty());
    }

    // ── build_semijoin_subquery tests ───────────────────────────────

    #[test]
    fn test_build_semijoin_subquery_right_side() {
        let keys = vec![("\"cust_id\"".to_string(), "\"id\"".to_string())];
        let result = build_semijoin_subquery(
            "\"public\".\"customers\"",
            &keys,
            "__pgt_cte_scan_1",
            JoinSide::Right,
        );
        assert!(result.contains("SELECT *"));
        assert!(result.contains("WHERE"));
        assert!(result.contains("IN (SELECT DISTINCT"));
    }

    #[test]
    fn test_build_semijoin_subquery_no_keys_returns_plain() {
        let result = build_semijoin_subquery(
            "\"public\".\"customers\"",
            &[],
            "__pgt_cte_1",
            JoinSide::Right,
        );
        assert_eq!(result, "\"public\".\"customers\"");
    }

    // ── get_current_table_ref tests ─────────────────────────────────

    #[test]
    fn test_get_current_table_ref_scan() {
        let node = scan(1, "orders", "public", "o", &["id"]);
        assert_eq!(get_current_table_ref(&node), "\"public\".\"orders\"");
    }

    #[test]
    fn test_get_current_table_ref_non_scan() {
        let node = OpTree::Distinct {
            child: Box::new(scan(1, "t", "public", "t", &["id"])),
        };
        assert_eq!(
            get_current_table_ref(&node),
            "/* unsupported snapshot for distinct */"
        );
    }

    // ── build_base_table_key_exprs tests ────────────────────────────

    #[test]
    fn test_build_base_table_key_exprs_non_nullable() {
        let node = scan_not_null(1, "orders", "public", "o", &["id", "name"]);
        let exprs = build_base_table_key_exprs(&node, "r");
        assert!(exprs.iter().any(|e| e.contains("r.\"id\"::TEXT")));
        assert!(exprs.iter().any(|e| e.contains("r.\"name\"::TEXT")));
    }

    #[test]
    fn test_build_base_table_key_exprs_all_nullable_fallback() {
        let node = scan(1, "orders", "public", "o", &["id", "name"]);
        let exprs = build_base_table_key_exprs(&node, "r");
        // All nullable → uses all columns
        assert_eq!(exprs.len(), 2);
    }

    #[test]
    fn test_build_base_table_key_exprs_non_scan_fallback() {
        let node = OpTree::Distinct {
            child: Box::new(scan(1, "t", "public", "t", &["id"])),
        };
        let exprs = build_base_table_key_exprs(&node, "x");
        assert_eq!(exprs, vec!["row_to_json(x)::text"]);
    }

    // ── Nested join tests ───────────────────────────────────────────

    #[test]
    fn test_diff_inner_join_nested_three_tables() {
        // (orders ⋈ customers) ⋈ products — 3-table nested inner join.
        // The left child (orders ⋈ customers) is a non-SemiJoin join chain,
        // so Part 2 uses L₀ via EXCEPT ALL (no correction term needed).
        let o = scan(1, "orders", "public", "o", &["id", "cust_id", "prod_id"]);
        let c = scan(2, "customers", "public", "c", &["id", "name"]);
        let inner = inner_join(eq_cond("o", "cust_id", "c", "id"), o, c);
        let p = scan(3, "products", "public", "p", &["id", "price"]);
        let tree = inner_join(eq_cond("o", "prod_id", "p", "id"), inner, p);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "nested 3-table inner join should diff: {result:?}"
        );
        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        // Should produce Part 1 + Part 2 (with L₀ via EXCEPT ALL)
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
        // L₀ uses EXCEPT ALL for the pre-change snapshot
        assert_sql_contains(&sql, "EXCEPT ALL");
        // No correction term — L₀ is exact for non-SemiJoin join children
        assert_sql_not_contains(&sql, "Part 3");
    }

    #[test]
    fn test_diff_inner_join_deep_chain_no_correction() {
        // ((a ⋈ b) ⋈ c) ⋈ d — 4-table chain.
        // Inner level (a⋈b, 2 scans) uses L₀ via EXCEPT ALL.
        // Outer level (left has 3 scans) uses L₁ + Part 3 correction.
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner1 = inner_join(eq_cond("a", "id", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let inner2 = inner_join(eq_cond("a", "id", "c", "id"), inner1, c);
        let d = scan(4, "d", "public", "d", &["id"]);
        let tree = inner_join(eq_cond("a", "id", "d", "id"), inner2, d);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // One correction term at the outermost level (3-scan left > ≤ 2 threshold)
        let correction_count = sql.matches("Correction for nested join").count();
        assert_eq!(
            correction_count, 1,
            "expected 1 correction term (outermost level), got {correction_count}\n{sql}"
        );
    }

    #[test]
    fn test_diff_inner_join_nested_uses_l0_via_except_all() {
        // Non-SemiJoin nested join children use L₀ via EXCEPT ALL,
        // eliminating the need for a correction term.
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "id", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = inner_join(eq_cond("a", "id", "c", "id"), inner, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // L₀ via EXCEPT ALL should be present in the outer join's Part 2
        assert_sql_contains(&sql, "EXCEPT ALL");
        // No correction term — L₀ is exact
        assert_sql_not_contains(&sql, "Correction for nested join");
        assert_sql_not_contains(&sql, "Part 3");
        // Should still have valid structure: Part 1 + Part 2
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
    }

    #[test]
    fn test_diff_inner_join_scan_no_correction() {
        // For Scan left child, no Part 3 correction should be present
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Part 3 correction should NOT appear in the join CTE.
        // Note: we check for the specific Part 3 comment marker, excluding
        // UNION ALL counts which are inflated by scan delta CTEs.
        assert_sql_not_contains(&sql, "Correction for nested join");
    }

    #[test]
    fn test_diff_inner_join_nested_skips_semijoin_optimization() {
        // When a child is a nested join, semi-join optimization is skipped
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "id", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = inner_join(eq_cond("a", "id", "c", "id"), inner, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // Semi-join would contain EXISTS; nested join should NOT have it
        assert_sql_not_contains(&sql, "EXISTS");
    }

    #[test]
    fn test_diff_inner_join_nested_uses_snapshot_subquery() {
        // The nested child should appear as a subquery snapshot, not a plain table ref
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "id", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = inner_join(eq_cond("a", "id", "c", "id"), inner, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // The nested join snapshot should generate SQL with JOIN in the snapshot subquery
        assert_sql_contains(&sql, "JOIN");
    }

    // ── NATURAL JOIN diff tests ─────────────────────────────────────

    #[test]
    fn test_diff_inner_join_with_natural_condition() {
        // Simulate what the parser produces for NATURAL JOIN:
        // two tables sharing "id" column → equi-join on id
        let left = scan(1, "orders", "public", "o", &["id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = natural_join_cond(&left, &right);
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        // Should produce valid SQL with two parts (delta_left JOIN right, left JOIN delta_right)
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "UNION ALL");
    }

    #[test]
    fn test_diff_inner_join_natural_multiple_common_cols() {
        // Two tables sharing both "id" and "region"
        let left = scan(1, "a", "public", "a", &["id", "region", "val"]);
        let right = scan(2, "b", "public", "b", &["id", "region", "score"]);
        let cond = natural_join_cond(&left, &right);
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        // Should have disambiguated columns from both sides
        assert!(result.columns.contains(&"a__id".to_string()));
        assert!(result.columns.contains(&"a__region".to_string()));
        assert!(result.columns.contains(&"b__id".to_string()));
        assert!(result.columns.contains(&"b__score".to_string()));
    }

    #[test]
    fn test_diff_inner_join_natural_no_common_columns() {
        // No common columns → condition is TRUE (cross join)
        let left = scan(1, "orders", "public", "o", &["order_id", "amount"]);
        let right = scan(2, "customers", "public", "c", &["cust_id", "name"]);
        let cond = natural_join_cond(&left, &right);
        assert_eq!(cond.to_sql(), "TRUE");
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        // Should still produce valid diff SQL
        assert!(!result.columns.is_empty());
    }

    #[test]
    fn test_diff_inner_join_inside_semijoin_uses_l1() {
        // When inside_semijoin is true (i.e. this inner join is a child of
        // a SemiJoin/AntiJoin), non-SemiJoin join children should fall back
        // to L₁ (post-change snapshot) to avoid the Q21-type regression.
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let inner = inner_join(eq_cond("a", "id", "b", "id"), a, b);
        let c = scan(3, "c", "public", "c", &["id"]);
        let tree = inner_join(eq_cond("a", "id", "c", "id"), inner, c);

        let mut ctx = test_ctx();
        // Simulate being inside a SemiJoin ancestor
        ctx.inside_semijoin = true;
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // inside_semijoin → L₁ used for the left join child, so the
        // EXCEPT ALL approach is NOT used for the outer join's Part 2
        // left snapshot.  Instead, the correction term (Part 3) appears
        // for the shallow nested join child.
        assert_sql_contains(&sql, "Part 1");
        assert_sql_contains(&sql, "Part 2");
        assert_sql_contains(&sql, "Part 3");
        // L₁ with correction, not L₀ via EXCEPT ALL at the outer level
        assert_sql_contains(&sql, "Correction for nested join");
    }

    #[test]
    fn test_join_scan_count() {
        let a = scan(1, "a", "public", "a", &["id"]);
        assert_eq!(join_scan_count(&a), 1);

        let b = scan(2, "b", "public", "b", &["id"]);
        let j2 = inner_join(eq_cond("a", "id", "b", "id"), a.clone(), b.clone());
        assert_eq!(join_scan_count(&j2), 2);

        let c = scan(3, "c", "public", "c", &["id"]);
        let j3 = inner_join(eq_cond("a", "id", "c", "id"), j2.clone(), c.clone());
        assert_eq!(join_scan_count(&j3), 3);

        let d = scan(4, "d", "public", "d", &["id"]);
        let j4 = inner_join(eq_cond("a", "id", "d", "id"), j3, d);
        assert_eq!(join_scan_count(&j4), 4);
    }

    #[test]
    fn test_deep_join_uses_l1_with_correction() {
        // With threshold=2, a 3-table chain (left has 2 scans) should
        // use L₀ (EXCEPT ALL). A 4-table chain (left has 3 scans) should
        // fall back to L₁ WITH Part 3 correction (the correction cancels
        // ΔL ⋈ ΔR double-counting without expensive snapshot materialization).
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let c = scan(3, "c", "public", "c", &["id"]);

        // left = a ⋈ b (2 scans) — L₀ via EXCEPT ALL
        let j_ab = inner_join(eq_cond("a", "id", "b", "id"), a.clone(), b);
        let tree_3 = inner_join(eq_cond("a", "id", "c", "id"), j_ab, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree_3).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);
        assert_sql_contains(&sql, "EXCEPT ALL");

        // left = a ⋈ b ⋈ c (3 scans) — L₁ WITH Part 3 correction
        let a2 = scan(1, "a", "public", "a", &["id"]);
        let b2 = scan(2, "b", "public", "b", &["id"]);
        let c2 = scan(3, "c", "public", "c", &["id"]);
        let d2 = scan(4, "d", "public", "d", &["id"]);
        let j_ab2 = inner_join(eq_cond("a", "id", "b", "id"), a2, b2);
        let j_abc = inner_join(eq_cond("a", "id", "c", "id"), j_ab2, c2);
        let tree_4 = inner_join(eq_cond("a", "id", "d", "id"), j_abc, d2);

        let mut ctx2 = test_ctx();
        let result2 = diff_inner_join(&mut ctx2, &tree_4).unwrap();
        let sql2 = ctx2.build_with_query(&result2.cte_name);
        // 3-scan left child → L₁ with Part 3 correction
        assert_sql_contains(&sql2, "Part 3");
        assert_sql_contains(&sql2, "Correction for nested join");
    }

    // ── EC-01: R₀ via EXCEPT ALL tests ──────────────────────────────

    #[test]
    fn test_ec01_simple_join_splits_part1() {
        // Two simple Scan children → Part 1 split into 1a (inserts ⋈ R₁)
        // and 1b (deletes ⋈ R₀ via EXCEPT ALL).
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Part 1a: inserts → R₁ (current right)
        assert_sql_contains(&sql, "Part 1a");
        assert_sql_contains(&sql, "INSERTS JOIN current_right R");

        // Part 1b: deletes → R₀ (pre-change right via EXCEPT ALL)
        assert_sql_contains(&sql, "Part 1b");
        assert_sql_contains(&sql, "DELETES JOIN pre-change_right R");
        assert_sql_contains(&sql, "EC-01 fix");

        // Part 1a filters by action = 'I', Part 1b filters by action = 'D'
        assert_sql_contains(&sql, "__pgt_action = 'I'");
        assert_sql_contains(&sql, "__pgt_action = 'D'");
    }

    #[test]
    fn test_ec01_r0_uses_except_all() {
        // Verify R₀ is built via EXCEPT ALL from the right snapshot.
        let left = scan(1, "a", "public", "a", &["id", "key"]);
        let right = scan(2, "b", "public", "b", &["id", "val"]);
        let cond = eq_cond("a", "key", "b", "id");
        let tree = inner_join(cond, left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should have at least 2 EXCEPT ALL occurrences: one for L₀, one for R₀.
        let except_count = sql.matches("EXCEPT ALL").count();
        assert!(
            except_count >= 2,
            "expected ≥2 EXCEPT ALL (L₀ + R₀), got {except_count}"
        );

        // R₀ references the right-side table ("b")
        assert_sql_contains(&sql, "\"public\".\"b\"");
    }

    #[test]
    fn test_ec01_nested_right_child_no_split() {
        // When right child is a nested join with >2 scan nodes,
        // use_r0 is false → Part 1 stays unsplit for the OUTER join.
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let c = scan(3, "c", "public", "c", &["id"]);
        let right_inner = inner_join(eq_cond("b", "id", "c", "id"), b, c);
        let d = scan(4, "d", "public", "d", &["id"]);
        let right_deep = inner_join(eq_cond("b", "id", "d", "id"), right_inner, d);
        // right has 3 scans → use_r0 = false (exceeds ≤2 threshold)
        let tree = inner_join(eq_cond("a", "id", "b", "id"), a, right_deep);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // The outermost join CTE name is result.cte_name. Extract just
        // that CTE's body to check the outer level's Part 1 is unsplit.
        let outer_cte_marker = format!("{} AS", result.cte_name);
        let outer_sql = sql.split(&outer_cte_marker).last().unwrap_or("");
        // Outer join should have unsplit Part 1 (not 1a/1b)
        assert_sql_not_contains(outer_sql, "Part 1a");
        assert_sql_not_contains(outer_sql, "Part 1b");
        assert_sql_contains(outer_sql, "Part 1:");
    }

    #[test]
    fn test_ec01_nested_right_child_2_scans_uses_r0() {
        // When right child is a nested join with ≤2 scan nodes and no
        // SemiJoin, use_r0 is true → Part 1 is split.
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let c = scan(3, "c", "public", "c", &["id"]);
        let right_join = inner_join(eq_cond("b", "id", "c", "id"), b, c);
        let tree = inner_join(eq_cond("a", "id", "b", "id"), a, right_join);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Right has 2 scans → use_r0 = true → Part 1 split
        assert_sql_contains(&sql, "Part 1a");
        assert_sql_contains(&sql, "Part 1b");
    }

    #[test]
    fn test_ec01_three_union_all_arms() {
        // Simple 2-table join: Part 1a + Part 1b + Part 2 = 3 arms
        let left = scan(1, "l", "public", "l", &["id"]);
        let right = scan(2, "r", "public", "r", &["id"]);
        let tree = inner_join(eq_cond("l", "id", "r", "id"), left, right);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // The join CTE should contain Part 1a, 1b, and Part 2 markers.
        let outer_cte_marker = format!("{} AS", result.cte_name);
        let outer_sql = sql.split(&outer_cte_marker).last().unwrap_or("");
        let union_count = outer_sql.matches("UNION ALL").count();
        assert!(
            union_count >= 2,
            "expected ≥2 UNION ALL (1a+1b+Part2), got {union_count}"
        );
    }

    // ── Multi-table join chain tests ────────────────────────────────

    /// `(A LEFT JOIN B) INNER JOIN C` — left child is a LEFT join.
    ///
    /// This is one of the most common 3-table patterns. The inner join wraps a
    /// left join, so the left child sub-tree has nullable right columns.
    /// The inner join differentiator must propagate the nullable columns from
    /// the left join without stripping them.
    #[test]
    fn test_diff_inner_join_left_join_as_left_child() {
        // A LEFT JOIN B: orders LEFT JOIN customers on o.cust_id = c.id
        let a = scan(1, "orders", "public", "o", &["id", "cust_id", "amount"]);
        let b = scan(2, "customers", "public", "c", &["id", "name"]);
        let left_subtree = left_join(eq_cond("o", "cust_id", "c", "id"), a, b);

        // (A LEFT JOIN B) INNER JOIN C: above LEFT JOIN inner-joined to regions
        let c = scan(3, "regions", "public", "r", &["id", "country"]);
        // Join on o.amount == r.id (contrived key for testing)
        let tree = inner_join(eq_cond("o", "id", "r", "id"), left_subtree, c);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "(A LEFT JOIN B) INNER JOIN C should succeed: {result:?}"
        );

        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);

        // Output must include columns from all three tables (A, B, C)
        assert!(
            dr.columns
                .iter()
                .any(|c| c.contains("o__id") || c.contains("id")),
            "output should contain columns from A; got {:?}",
            dr.columns
        );
        // SQL itself must have parts
        assert_sql_contains(&sql, "UNION ALL");

        // Verify it uses the pre-change snapshot (EXCEPT ALL) for correctness
        assert_sql_contains(&sql, "EXCEPT ALL");
    }

    /// `A INNER JOIN (B INNER JOIN C)` — right child is a nested inner join.
    ///
    /// Three-table inner join chain where all joins feed into the outermost
    /// inner join. Verifies that column disambiguation works at all levels.
    #[test]
    fn test_diff_inner_join_with_nested_right_inner_join() {
        // B INNER JOIN C: lineitems INNER JOIN parts
        let b = scan(
            2,
            "lineitems",
            "public",
            "l",
            &["order_id", "part_id", "qty"],
        );
        let c = scan(3, "parts", "public", "p", &["id", "type"]);
        let inner_bc = inner_join(eq_cond("l", "part_id", "p", "id"), b, c);

        // A INNER JOIN (B INNER JOIN C): orders INNER JOIN above
        let a = scan(1, "orders", "public", "o", &["id", "region"]);
        let tree = inner_join(eq_cond("o", "id", "l", "order_id"), a, inner_bc);

        let mut ctx = test_ctx();
        let result = diff_inner_join(&mut ctx, &tree);
        assert!(
            result.is_ok(),
            "A INNER JOIN (B INNER JOIN C) should succeed: {result:?}"
        );

        let dr = result.unwrap();
        let sql = ctx.build_with_query(&dr.cte_name);
        assert_sql_contains(&sql, "UNION ALL");
        // Output must reference all three scans
        assert!(
            sql.to_lowercase().contains("orders"),
            "SQL must reference orders"
        );
        assert!(
            sql.to_lowercase().contains("lineitems"),
            "SQL must reference lineitems"
        );
        assert!(
            sql.to_lowercase().contains("parts"),
            "SQL must reference parts"
        );
    }

    // ── P2-4 property tests ───────────────────────────────────────────────
    //
    // Verify structural invariants of diff_inner_join() output SQL for
    // arbitrarily generated column name sets using proptest.

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(200))]

        /// For any non-empty left/right column sets the output always contains
        /// UNION ALL (the two-part delta decomposition).
        #[test]
        fn prop_diff_inner_join_sql_always_has_union_all(
            left_cols in proptest::collection::vec("[a-z]{2,6}", 1..4usize),
            right_cols in proptest::collection::vec("[a-z]{2,6}", 1..4usize),
        ) {
            // Prefix ensures left/right column names never collide.
            let lcols: Vec<String> = left_cols.iter().enumerate()
                .map(|(i, c)| format!("lc{i}_{c}")).collect();
            let rcols: Vec<String> = right_cols.iter().enumerate()
                .map(|(i, c)| format!("rc{i}_{c}")).collect();
            let lrefs: Vec<&str> = lcols.iter().map(|s| s.as_str()).collect();
            let rrefs: Vec<&str> = rcols.iter().map(|s| s.as_str()).collect();

            let left  = scan(1, "left_t",  "public", "l", &lrefs);
            let right = scan(2, "right_t", "public", "r", &rrefs);
            let tree  = inner_join(eq_cond("l", &lcols[0], "r", &rcols[0]), left, right);

            let mut ctx = test_ctx();
            let result  = diff_inner_join(&mut ctx, &tree);
            prop_assert!(result.is_ok(), "diff_inner_join must not fail: {result:?}");

            let sql = ctx.build_with_query(&result.unwrap().cte_name);
            prop_assert!(sql.contains("UNION ALL"),
                "inner join delta must contain UNION ALL");
        }

        /// The output column count equals left.len() + right.len() for a
        /// 2-table inner join (both sides contribute all columns).
        #[test]
        fn prop_diff_inner_join_output_col_count(
            left_cols in proptest::collection::vec("[a-z]{2,6}", 1..4usize),
            right_cols in proptest::collection::vec("[a-z]{2,6}", 1..4usize),
        ) {
            let lcols: Vec<String> = left_cols.iter().enumerate()
                .map(|(i, c)| format!("lc{i}_{c}")).collect();
            let rcols: Vec<String> = right_cols.iter().enumerate()
                .map(|(i, c)| format!("rc{i}_{c}")).collect();
            let lrefs: Vec<&str> = lcols.iter().map(|s| s.as_str()).collect();
            let rrefs: Vec<&str> = rcols.iter().map(|s| s.as_str()).collect();

            let left  = scan(1, "left_t",  "public", "l", &lrefs);
            let right = scan(2, "right_t", "public", "r", &rrefs);
            let tree  = inner_join(eq_cond("l", &lcols[0], "r", &rcols[0]), left, right);

            let mut ctx = test_ctx();
            let result  = diff_inner_join(&mut ctx, &tree).unwrap();

            prop_assert_eq!(
                result.columns.len(),
                lcols.len() + rcols.len(),
                "inner join must output all left + right columns"
            );
        }
    }
}
