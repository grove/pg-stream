//! Filter/WHERE differentiation.
//!
//! ΔI(σP(Q)) = σP(ΔI(Q))
//!
//! Apply the predicate P to the child's delta stream. Rows that don't
//! satisfy P are dropped from both inserts and deletes.
//!
//! UPDATE correctness: The scan already splits UPDATEs into DELETE+INSERT
//! pairs. A row that transitions from not-matching to matching the predicate
//! will have its DELETE (old values) filtered out and its INSERT (new values)
//! kept — net result: INSERT into the ST. The converse is also correct.

use crate::dvm::diff::{DiffContext, DiffResult, quote_ident};
use crate::dvm::parser::OpTree;
use crate::error::PgStreamError;

/// Differentiate a Filter node.
pub fn diff_filter(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgStreamError> {
    let OpTree::Filter { predicate, child } = op else {
        return Err(PgStreamError::InternalError(
            "diff_filter called on non-Filter node".into(),
        ));
    };

    // First, differentiate the child
    let child_result = ctx.diff_node(child)?;

    let cte_name = ctx.next_cte_name("filter");
    let predicate_sql = predicate.to_sql();

    let col_refs: Vec<String> = child_result
        .columns
        .iter()
        .map(|c| quote_ident(c))
        .collect();

    let sql = format!(
        "SELECT __pgs_row_id, __pgs_action, {cols}\n\
         FROM {child_cte}\n\
         WHERE {predicate}",
        cols = col_refs.join(", "),
        child_cte = child_result.cte_name,
        predicate = predicate_sql,
    );

    ctx.add_cte(cte_name.clone(), sql);

    // Do NOT add a dedup CTE here. When the child produces non-deduplicated
    // D+I pairs (e.g. an UPDATE generates both DELETE(old) and INSERT(new)),
    // we must preserve both events so that upstream operators — especially
    // aggregates — can correctly compute net count/sum changes.
    //
    // For scan-chain queries (filter at top, no aggregate above), the
    // MERGE statement's outer DISTINCT ON already handles dedup.
    //
    // Previously a DISTINCT ON (__pgs_row_id) CTE was added here that
    // collapsed D+I pairs into a single INSERT, which broke aggregate
    // correctness when a row's UPDATE crossed the filter boundary while
    // remaining in the same group.

    Ok(DiffResult {
        cte_name,
        columns: child_result.columns,
        is_deduplicated: child_result.is_deduplicated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    #[test]
    fn test_diff_filter_basic() {
        let mut ctx = test_ctx();
        let child = scan(1, "t", "public", "t", &["id", "amount"]);
        let tree = filter(binop(">", colref("amount"), lit("100")), child);
        let result = diff_filter(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_sql_contains(&sql, "amount");
        assert_sql_contains(&sql, "WHERE");
        assert_eq!(result.columns, vec!["id", "amount"]);
        // Filter no longer adds a dedup CTE — is_deduplicated inherits from child
        assert!(!result.is_deduplicated);
    }

    #[test]
    fn test_diff_filter_preserves_row_id_and_action() {
        let mut ctx = test_ctx();
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree = filter(binop(">", colref("id"), lit("0")), child);
        let result = diff_filter(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        assert_sql_contains(&sql, "__pgs_row_id");
        assert_sql_contains(&sql, "__pgs_action");
    }

    #[test]
    fn test_diff_filter_preserves_dedup_flag() {
        let mut ctx = test_ctx();
        ctx.merge_safe_dedup = true;
        let child = scan(1, "t", "public", "t", &["id"]);
        let tree = filter(binop(">", colref("id"), lit("0")), child);
        let result = diff_filter(&mut ctx, &tree).unwrap();

        // should inherit is_deduplicated from child scan (already true)
        assert!(result.is_deduplicated);
    }

    #[test]
    fn test_diff_filter_no_dedup_cte_for_non_dedup_child() {
        let mut ctx = test_ctx();
        // merge_safe_dedup = false (default) → scan produces D+I pairs
        let child = scan(1, "t", "public", "t", &["id", "amount"]);
        let tree = filter(binop(">", colref("amount"), lit("100")), child);
        let result = diff_filter(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Filter should NOT add its own dedup CTE — D+I pairs must be
        // preserved for aggregate operators above. The MERGE handles dedup.
        // (The scan CTE itself may contain DISTINCT ON, so we check the CTE name instead.)
        assert!(!sql.contains("filter_dedup"));
        assert!(!result.is_deduplicated);
        // CTE name should be plain filter, not filter_dedup
        assert!(result.cte_name.contains("filter"));
        assert!(!result.cte_name.contains("dedup"));
    }

    #[test]
    fn test_diff_filter_inherits_dedup_when_child_already_dedup() {
        let mut ctx = test_ctx();
        ctx.merge_safe_dedup = true;
        let child = scan(1, "t", "public", "t", &["id", "amount"]);
        let tree = filter(binop(">", colref("amount"), lit("100")), child);
        let result = diff_filter(&mut ctx, &tree).unwrap();
        let _sql = ctx.build_with_query(&result.cte_name);

        // No dedup CTE should be added — child is already deduplicated
        assert!(!result.cte_name.contains("filter_dedup"));
        assert!(result.is_deduplicated);
        // CTE name should be plain filter, not filter_dedup
        assert!(result.cte_name.contains("filter"));
        assert!(!result.cte_name.contains("dedup"));
    }

    #[test]
    fn test_diff_filter_contains_predicate_and_columns() {
        let mut ctx = test_ctx();
        let child = scan(1, "t", "public", "t", &["id", "name", "status"]);
        let tree = filter(binop("=", colref("status"), lit("'active'")), child);
        let result = diff_filter(&mut ctx, &tree).unwrap();
        let sql = ctx.build_with_query(&result.cte_name);

        // Filter CTE should contain the predicate
        assert_sql_contains(&sql, "status");
        assert_sql_contains(&sql, "WHERE");
        // Filter CTE should contain all columns
        assert_sql_contains(&sql, "\"id\"");
        assert_sql_contains(&sql, "\"name\"");
        assert_eq!(result.columns, vec!["id", "name", "status"]);
    }

    #[test]
    fn test_diff_filter_error_on_non_filter_node() {
        let mut ctx = test_ctx();
        let tree = scan(1, "t", "public", "t", &["id"]);
        let result = diff_filter(&mut ctx, &tree);
        assert!(result.is_err());
    }
}
