//! Base table scan differentiation.
//!
//! ΔI(Scan(T)) reads from the change buffer table for T.
//!
//! The change buffer table `pgstream_changes.changes_<oid>` contains:
//! - change_id BIGSERIAL — insertion ordering (no PK index)
//! - lsn PG_LSN
//! - action CHAR(1) — 'I', 'U', 'D'
//! - pk_hash BIGINT — pre-computed PK hash (optional)
//! - new_{col} TYPE — NEW row values (INSERT/UPDATE)
//! - old_{col} TYPE — OLD row values (UPDATE/DELETE)
//!
//! For UPDATEs, we split into DELETE (old values) + INSERT (new values).
//! Row IDs are computed as hash of the primary key columns.
//!
//! ## Single-pass design
//!
//! The change buffer is scanned **once** using typed columns rather than
//! JSONB deserialization. Columns are referenced directly as
//! `c."new_{col}"` / `c."old_{col}"` with proper PostgreSQL types,
//! eliminating `jsonb_populate_record` overhead.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate a Scan node.
///
/// Reads from the change buffer in a **single pass** and produces a delta
/// with columns: `__pgs_row_id`, `__pgs_action`, plus all table columns.
///
/// UPDATEs are expanded into (DELETE old, INSERT new) via UNION ALL
/// branches, so the change buffer index on `lsn` is used exactly once.
///
/// Column extraction uses typed columns `c."new_{col}"` / `c."old_{col}"`
/// directly from the change buffer table — no JSONB deserialization.
pub fn diff_scan(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::Scan {
        table_oid,
        columns,
        pk_columns,
        alias,
        ..
    } = op
    else {
        return Err(PgStreamError::InternalError(
            "diff_scan called on non-Scan node".into(),
        ));
    };

    let change_table = format!(
        "{}.changes_{}",
        quote_ident(&ctx.change_buffer_schema),
        table_oid,
    );

    let prev_lsn = ctx.get_prev_lsn(*table_oid);
    let new_lsn = ctx.get_new_lsn(*table_oid);

    let col_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();

    // Determine PK columns and whether pre-computed pk_hash is available.
    let pk_cols = find_pk_columns(pk_columns, columns);
    let has_precomputed_pk_hash = !pk_columns.is_empty();

    let pk_hash_expr = if has_precomputed_pk_hash {
        // Use the pre-computed pk_hash from the trigger (G-J1 optimization)
        "c.pk_hash".to_string()
    } else {
        // Fallback: compute PK hash from typed columns
        let pk_typed_exprs: Vec<String> = pk_cols
            .iter()
            .map(|c| {
                format!(
                    "COALESCE(c.{new}, c.{old})::text",
                    new = quote_ident(&format!("new_{c}")),
                    old = quote_ident(&format!("old_{c}")),
                )
            })
            .collect();
        if pk_typed_exprs.len() == 1 {
            format!("pgstream.pg_stream_hash({})", pk_typed_exprs[0])
        } else {
            format!(
                "pgstream.pg_stream_hash_multi(ARRAY[{}])",
                pk_typed_exprs.join(", "),
            )
        }
    };

    // Build typed column references for the raw CTE
    let mut typed_col_refs = Vec::new();
    for c in columns {
        typed_col_refs.push(format!("c.{}", quote_ident(&format!("new_{}", c.name))));
        typed_col_refs.push(format!("c.{}", quote_ident(&format!("old_{}", c.name))));
    }
    let typed_col_refs_str = typed_col_refs.join(",\n       ");

    // Build output column references: old_* for DELETE, new_* for INSERT.
    // Each is aliased to the original column name for downstream CTEs.
    let old_col_refs: Vec<String> = columns
        .iter()
        .map(|c| {
            format!(
                "c.{} AS {}",
                quote_ident(&format!("old_{}", c.name)),
                quote_ident(&c.name),
            )
        })
        .collect();
    let new_col_refs: Vec<String> = columns
        .iter()
        .map(|c| {
            format!(
                "c.{} AS {}",
                quote_ident(&format!("new_{}", c.name)),
                quote_ident(&c.name),
            )
        })
        .collect();

    // ## Net-effect scan delta (split fast-path approach)
    //
    // When the same PK has multiple changes within one refresh window
    // (e.g., INSERT then UPDATE, or INSERT then DELETE), we compute the
    // NET effect rather than emitting all raw events.
    //
    // CTE 1 (pk_stats): Groups changes by PK hash, counts per PK.
    //
    // CTE 2 (single): Fast path for PKs with exactly one change (~95%
    // of PKs). No window functions needed — action IS first/last action.
    //
    // CTE 3 (multi_raw): Slow path for PKs with multiple changes.
    // Applies FIRST_VALUE/LAST_VALUE window functions to determine the
    // net effect. The window sort operates on a much smaller data set.
    //
    // CTE 4 (scan_raw): UNION ALL of single + multi_raw paths.
    //
    // CTE 5 (scan): Emits D/I events filtered by first_action/last_action:
    // - DELETE only when first_action != 'I' (row existed before the cycle)
    // - INSERT only when last_action != 'D' (row still exists after the cycle)
    //
    // This correctly handles:
    // - Plain INSERT:         → I(new)
    // - Plain DELETE:         → D(old)
    // - Plain UPDATE:         → D(old) + I(new)
    // - INSERT + UPDATE:      → I(final)   (no spurious DELETE)
    // - INSERT + DELETE:      → nothing     (cancels out)
    // - UPDATE + DELETE:      → D(original)
    // - Multiple UPDATEs:     → D(first old) + I(last new)
    // - INSERT + UPDATE + DELETE: → nothing (cancels out)
    //
    // ## Merge-safe dedup mode (G-M1 optimization)
    //
    // When `ctx.merge_safe_dedup` is true (scan-chain queries without
    // aggregate/join/union above), the DELETE branch is further restricted
    // to only emit when the row is TRULY deleted (last_action = 'D').
    // For updates, only the INSERT branch fires. This produces exactly
    // ONE row per PK, allowing the MERGE to skip DISTINCT ON + ORDER BY.

    // ── R1: pk_stats CTE — count changes per PK ──────────────────────
    //
    // Used to split single-change PKs (fast path, no window functions)
    // from multi-change PKs (require FIRST_VALUE/LAST_VALUE).
    let lsn_filter = format!("c.lsn > '{prev_lsn}'::pg_lsn AND c.lsn <= '{new_lsn}'::pg_lsn");
    let pk_stats_cte = ctx.next_cte_name(&format!("pk_stats_{alias}"));
    let pk_stats_sql = format!(
        "\
SELECT {pk_hash_expr} AS __pk_hash, count(*) AS cnt
FROM {change_table} c
WHERE {lsn_filter}
GROUP BY {pk_hash_expr}",
    );
    ctx.add_cte(pk_stats_cte.clone(), pk_stats_sql);

    // ── R2: Single-change fast path (no window functions) ────────────
    //
    // ~95% of PKs typically have exactly one change per refresh cycle.
    // For these, first_action = last_action = action — skip the sort.
    let single_cte = ctx.next_cte_name(&format!("single_{alias}"));
    let single_sql = format!(
        "\
SELECT {pk_hash_expr} AS __pk_hash,
       c.action,
       c.change_id,
       {typed_col_refs_str},
       c.action AS __first_action,
       c.action AS __last_action
FROM {change_table} c
JOIN {pk_stats_cte} p ON p.__pk_hash = {pk_hash_expr} AND p.cnt = 1
WHERE {lsn_filter}",
    );
    ctx.add_cte(single_cte.clone(), single_sql);

    // ── R3: Multi-change path with window functions ──────────────────
    //
    // Only apply FIRST_VALUE/LAST_VALUE to PKs with multiple changes.
    // The window sort now operates on a much smaller data set.
    let multi_cte = ctx.next_cte_name(&format!("multi_raw_{alias}"));
    let multi_sql = format!(
        "\
SELECT {pk_hash_expr} AS __pk_hash,
       c.action,
       c.change_id,
       {typed_col_refs_str},
       FIRST_VALUE(c.action) OVER (
           PARTITION BY {pk_hash_expr} ORDER BY c.change_id
       ) AS __first_action,
       LAST_VALUE(c.action) OVER (
           PARTITION BY {pk_hash_expr} ORDER BY c.change_id
           ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
       ) AS __last_action
FROM {change_table} c
JOIN {pk_stats_cte} p ON p.__pk_hash = {pk_hash_expr} AND p.cnt > 1
WHERE {lsn_filter}",
    );
    ctx.add_cte(multi_cte.clone(), multi_sql);

    // ── R4: Union single + multi paths ───────────────────────────────
    let raw_cte_name = ctx.next_cte_name(&format!("scan_raw_{alias}"));
    let raw_sql = format!(
        "\
SELECT * FROM {single_cte}
UNION ALL
SELECT * FROM {multi_cte}",
    );
    ctx.add_cte(raw_cte_name.clone(), raw_sql);

    // CTE 2: Emit D/I events with net-effect filtering
    let cte_name = ctx.next_cte_name(&format!("scan_{alias}"));
    let is_deduplicated;

    let sql = if ctx.merge_safe_dedup {
        // ── Merge-safe dedup mode ──────────────────────────────────────
        // Emit at most ONE row per PK: DELETE only for true deletes,
        // INSERT for any row that exists after the cycle (incl. updates).
        is_deduplicated = true;
        format!(
            "\
-- DELETE events: row existed before AND was truly deleted (not just updated)
SELECT c.__pk_hash AS __pgs_row_id,
       'D'::TEXT AS __pgs_action,
       {old_col_refs}
FROM (
  SELECT DISTINCT ON (s.__pk_hash)
         s.*
  FROM {raw_cte_name} s
  WHERE s.__first_action != 'I' AND s.__last_action = 'D'
  ORDER BY s.__pk_hash, s.change_id
) c

UNION ALL

-- INSERT events: row exists after (handles inserts + updates)
SELECT c.__pk_hash AS __pgs_row_id,
       'I'::TEXT AS __pgs_action,
       {new_col_refs}
FROM (
  SELECT DISTINCT ON (s.__pk_hash)
         s.*
  FROM {raw_cte_name} s
  WHERE s.__last_action != 'D'
  ORDER BY s.__pk_hash, s.change_id DESC
) c",
            old_col_refs = old_col_refs.join(",\n       "),
            new_col_refs = new_col_refs.join(",\n       "),
        )
    } else {
        // ── Standard mode (D+I pairs for updates) ──────────────────────
        // Required when aggregate/join/union consumes the scan delta.
        is_deduplicated = false;
        format!(
            "\
-- DELETE events: row existed before (first_action != 'I')
-- Uses old_* columns from the earliest non-INSERT change per PK.
SELECT c.__pk_hash AS __pgs_row_id,
       'D'::TEXT AS __pgs_action,
       {old_col_refs}
FROM (
  SELECT DISTINCT ON (s.__pk_hash)
         s.*
  FROM {raw_cte_name} s
  WHERE s.action != 'I' AND s.__first_action != 'I'
  ORDER BY s.__pk_hash, s.change_id
) c

UNION ALL

-- INSERT events: row exists after (last_action != 'D')
-- Uses new_* columns from the latest non-DELETE change per PK.
SELECT c.__pk_hash AS __pgs_row_id,
       'I'::TEXT AS __pgs_action,
       {new_col_refs}
FROM (
  SELECT DISTINCT ON (s.__pk_hash)
         s.*
  FROM {raw_cte_name} s
  WHERE s.action != 'D' AND s.__last_action != 'D'
  ORDER BY s.__pk_hash, s.change_id DESC
) c",
            old_col_refs = old_col_refs.join(",\n       "),
            new_col_refs = new_col_refs.join(",\n       "),
        )
    };

    ctx.add_cte(cte_name.clone(), sql);

    Ok(DiffResult {
        cte_name,
        columns: col_names,
        is_deduplicated,
    })
}

/// Find primary key columns for a table.
///
/// Uses real PK columns from `pg_constraint` if available (populated during
/// parsing). Falls back to non-nullable columns as a heuristic, then all
/// columns.
fn find_pk_columns(pk_columns: &[String], columns: &[crate::dvm::parser::Column]) -> Vec<String> {
    if !pk_columns.is_empty() {
        return pk_columns.to_vec();
    }
    // Fallback: non-nullable columns heuristic
    let non_nullable: Vec<String> = columns
        .iter()
        .filter(|c| !c.is_nullable)
        .map(|c| c.name.clone())
        .collect();

    if non_nullable.is_empty() {
        columns.iter().map(|c| c.name.clone()).collect()
    } else {
        non_nullable
    }
}

/// Build a hash expression from a list of SQL expressions.
pub fn build_hash_expr(exprs: &[String]) -> String {
    if exprs.len() == 1 {
        format!("pgstream.pg_stream_hash({})", exprs[0])
    } else {
        let array_items: Vec<String> = exprs.iter().map(|e| format!("{e}::TEXT")).collect();
        format!(
            "pgstream.pg_stream_hash_multi(ARRAY[{}])",
            array_items.join(", "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    // ── diff_scan basic ─────────────────────────────────────────────

    #[test]
    fn test_diff_scan_basic_columns() {
        let mut ctx = test_ctx();
        let tree = scan(100, "orders", "public", "o", &["id", "amount", "region"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();

        assert_eq!(result.columns, vec!["id", "amount", "region"]);
        assert!(!result.cte_name.is_empty());
    }

    #[test]
    fn test_diff_scan_generates_change_table_ref() {
        let mut ctx = test_ctx();
        let tree = scan(42, "orders", "public", "o", &["id", "amount"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_sql_contains(&sql, "\"pgstream_changes\".changes_42");
    }

    #[test]
    fn test_diff_scan_lsn_filter() {
        let mut ctx = test_ctx();
        let tree = scan(100, "orders", "public", "o", &["id"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Default frontiers produce "0/0" LSNs
        assert_sql_contains(&sql, "c.lsn > '0/0'::pg_lsn");
        assert_sql_contains(&sql, "c.lsn <= '0/0'::pg_lsn");
    }

    #[test]
    fn test_diff_scan_placeholder_mode() {
        let mut ctx = test_ctx().with_placeholders();
        let tree = scan(55, "items", "public", "i", &["id", "name"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_sql_contains(&sql, "__PGS_PREV_LSN_55__");
        assert_sql_contains(&sql, "__PGS_NEW_LSN_55__");
    }

    #[test]
    fn test_diff_scan_with_pk_columns() {
        let mut ctx = test_ctx();
        let tree = scan_with_pk(100, "orders", "public", "o", &["id", "amount"], &["id"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // With PK columns, should use pre-computed pk_hash
        assert_sql_contains(&sql, "c.pk_hash");
    }

    #[test]
    fn test_diff_scan_without_pk_fallback() {
        let mut ctx = test_ctx();
        // No PK but non-nullable columns → fallback to non-nullable cols for hash
        let tree = scan_not_null(100, "orders", "public", "o", &["id", "amount"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should use computed hash, not pk_hash
        assert_sql_contains(&sql, "pgstream.pg_stream_hash");
        assert_sql_not_contains(&sql, "c.pk_hash");
    }

    #[test]
    fn test_diff_scan_typed_column_refs() {
        let mut ctx = test_ctx();
        let tree = scan(100, "orders", "public", "o", &["id", "amount"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should reference typed columns
        assert_sql_contains(&sql, "c.\"new_id\"");
        assert_sql_contains(&sql, "c.\"old_id\"");
        assert_sql_contains(&sql, "c.\"new_amount\"");
        assert_sql_contains(&sql, "c.\"old_amount\"");
    }

    #[test]
    fn test_diff_scan_delete_and_insert_branches() {
        let mut ctx = test_ctx();
        let tree = scan(100, "orders", "public", "o", &["id", "amount"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Should have DELETE and INSERT event branches
        assert_sql_contains(&sql, "'D'::TEXT AS __pgs_action");
        assert_sql_contains(&sql, "'I'::TEXT AS __pgs_action");
    }

    #[test]
    fn test_diff_scan_merge_safe_dedup() {
        let mut ctx = test_ctx();
        ctx.merge_safe_dedup = true;
        let tree = scan(100, "orders", "public", "o", &["id", "amount"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();

        // Merge-safe dedup → is_deduplicated = true
        assert!(result.is_deduplicated);

        let sql = ctx.build_with_query(&result.cte_name);
        // Should have the "truly deleted" filter
        assert_sql_contains(&sql, "__last_action = 'D'");
    }

    #[test]
    fn test_diff_scan_standard_mode_not_deduplicated() {
        let mut ctx = test_ctx();
        let tree = scan(100, "orders", "public", "o", &["id"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();

        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_scan_error_on_non_scan_node() {
        let mut ctx = test_ctx();
        let tree = OpTree::Distinct {
            child: Box::new(scan(1, "t", "public", "t", &["id"])),
        };
        let result = diff_scan(&mut ctx, &tree);
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_scan_single_column() {
        let mut ctx = test_ctx();
        let tree = scan(1, "t", "public", "t", &["val"]);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        assert_eq!(result.columns, vec!["val"]);
    }

    #[test]
    fn test_diff_scan_many_columns() {
        let mut ctx = test_ctx();
        let cols: Vec<&str> = (0..20)
            .map(|i| Box::leak(format!("c{i}").into_boxed_str()) as &str)
            .collect();
        let tree = scan(1, "wide", "public", "w", &cols);
        let result = diff_scan(&mut ctx, &tree).unwrap();
        assert_eq!(result.columns.len(), 20);
    }

    // ── find_pk_columns tests ───────────────────────────────────────

    #[test]
    fn test_find_pk_columns_explicit() {
        let pk = vec!["id".to_string()];
        let cols = vec![col("id"), col("name")];
        assert_eq!(find_pk_columns(&pk, &cols), vec!["id"]);
    }

    #[test]
    fn test_find_pk_columns_fallback_non_nullable() {
        let pk: Vec<String> = vec![];
        let cols = vec![col_not_null("id"), col("name")];
        assert_eq!(find_pk_columns(&pk, &cols), vec!["id"]);
    }

    #[test]
    fn test_find_pk_columns_fallback_all_nullable() {
        let pk: Vec<String> = vec![];
        let cols = vec![col("a"), col("b")];
        assert_eq!(find_pk_columns(&pk, &cols), vec!["a", "b"]);
    }

    // ── build_hash_expr tests ───────────────────────────────────────

    #[test]
    fn test_build_hash_expr_single() {
        let result = build_hash_expr(&["x".to_string()]);
        assert_eq!(result, "pgstream.pg_stream_hash(x)");
    }

    #[test]
    fn test_build_hash_expr_multiple() {
        let result = build_hash_expr(&["a".to_string(), "b".to_string()]);
        assert!(result.contains("pgstream.pg_stream_hash_multi"));
        assert!(result.contains("a::TEXT"));
        assert!(result.contains("b::TEXT"));
    }
}
