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
use crate::dvm::parser::{Expr, OpTree};
use crate::error::PgTrickleError;

/// Differentiate a Filter node.
pub fn diff_filter(ctx: &mut DiffContext, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
    let OpTree::Filter { predicate, child } = op else {
        return Err(PgTrickleError::InternalError(
            "diff_filter called on non-Filter node".into(),
        ));
    };

    // First, differentiate the child
    let child_result = ctx.diff_node(child)?;

    let cte_name = ctx.next_cte_name("filter");

    // Resolve predicate column references against the child CTE's actual
    // column names.  When the child is a join delta CTE, columns are
    // disambiguated (e.g. `customer__c_custkey` or
    // `join__customer__c_custkey`).  The predicate may contain bare
    // column references like `c_custkey` that must be mapped to the
    // matching disambiguated name.
    let predicate_sql = resolve_predicate_for_child(predicate, &child_result.columns);

    let col_refs: Vec<String> = child_result
        .columns
        .iter()
        .map(|c| quote_ident(c))
        .collect();

    let sql = format!(
        "SELECT __pgt_row_id, __pgt_action, {cols}\n\
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
    // Previously a DISTINCT ON (__pgt_row_id) CTE was added here that
    // collapsed D+I pairs into a single INSERT, which broke aggregate
    // correctness when a row's UPDATE crossed the filter boundary while
    // remaining in the same group.

    Ok(DiffResult {
        cte_name,
        columns: child_result.columns,
        is_deduplicated: child_result.is_deduplicated,
    })
}

// ── Predicate column resolution ──────────────────────────────────────

/// Resolve a predicate expression's column references against the child
/// CTE's actual column names.
///
/// When a filter sits on top of a join delta CTE, the CTE has
/// disambiguated column names like `customer__c_custkey` or
/// `join__orders__o_orderkey`, but the predicate (from the original SQL)
/// may use bare names like `c_custkey` or qualified names like
/// `customer.c_custkey`.  This function maps each column reference to
/// the matching CTE column name so the generated SQL is valid.
///
/// For `Expr::Raw` nodes that contain flattened SQL text with embedded
/// column references, a best-effort string replacement is applied using
/// the column name mapping built from `child_cols`.
fn resolve_predicate_for_child(predicate: &Expr, child_cols: &[String]) -> String {
    match predicate {
        Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name,
        } => {
            // Try direct disambiguated: tbl__col
            let disambiguated = format!("{tbl}__{column_name}");
            if child_cols.contains(&disambiguated) {
                return quote_ident(&disambiguated);
            }
            // Try nested join prefix: *__tbl__col
            for c in child_cols {
                if c.ends_with(&format!("__{tbl}__{column_name}")) {
                    return quote_ident(c);
                }
            }
            // Exact match on just column name
            if child_cols.contains(column_name) {
                return quote_ident(column_name);
            }
            // Fallback: original expression
            predicate.to_sql()
        }
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => {
            // Exact match
            if child_cols.contains(column_name) {
                return quote_ident(column_name);
            }
            // Suffix match: find column ending in __column_name
            let suffix = format!("__{column_name}");
            let matches: Vec<&String> =
                child_cols.iter().filter(|c| c.ends_with(&suffix)).collect();
            if matches.len() == 1 {
                return quote_ident(matches[0]);
            }
            // Fallback: unquoted column name (let PostgreSQL resolve)
            quote_ident(column_name)
        }
        Expr::BinaryOp { op, left, right } => {
            format!(
                "({} {op} {})",
                resolve_predicate_for_child(left, child_cols),
                resolve_predicate_for_child(right, child_cols),
            )
        }
        Expr::FuncCall { func_name, args } => {
            let resolved: Vec<String> = args
                .iter()
                .map(|a| resolve_predicate_for_child(a, child_cols))
                .collect();
            format!("{func_name}({})", resolved.join(", "))
        }
        Expr::Star { .. } | Expr::Literal(_) => predicate.to_sql(),
        Expr::Raw(sql) => {
            // Best-effort: replace known column name patterns in the Raw SQL
            // string.  Build a mapping from original column names (the suffix
            // after the last `__`) to full CTE column names.
            replace_column_refs_in_raw(sql, child_cols)
        }
    }
}

/// Best-effort replacement of column references in a raw SQL string.
///
/// Builds a mapping from base column names to their disambiguated CTE
/// column names, then replaces occurrences in the SQL text.  Only
/// replaces names that appear as word boundaries (not inside other
/// identifiers or string literals).
pub fn replace_column_refs_in_raw(sql: &str, child_cols: &[String]) -> String {
    // Build column name mapping: base_name → disambiguated_name
    // For "customer__c_custkey", base_name = "c_custkey"
    // For "join__customer__c_custkey", base_name = "c_custkey"
    let mut col_map: Vec<(String, String)> = Vec::new();
    for col in child_cols {
        if let Some(pos) = col.rfind("__") {
            let base = &col[pos + 2..];
            // Only add if the base name doesn't exactly match a real column
            // (avoid replacing "id" when "id" IS the actual column)
            if !child_cols.contains(&base.to_string()) {
                col_map.push((base.to_string(), col.clone()));
            }
        }
    }

    // Sort by length descending to replace longer names first (avoid
    // partial matches, e.g., "o_orderkey" before "o_order")
    col_map.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    // Deduplicate: if multiple CTE columns map to the same base name
    // (ambiguous), skip those entries.
    let mut seen_bases = std::collections::HashMap::new();
    for (base, full) in &col_map {
        seen_bases
            .entry(base.clone())
            .or_insert_with(Vec::new)
            .push(full.clone());
    }

    let mut result = sql.to_string();
    for (base, fulls) in &seen_bases {
        if fulls.len() != 1 {
            continue; // Ambiguous — skip
        }
        let full = &fulls[0];
        // Replace occurrences at word boundaries using a simple scan.
        // We look for the base name NOT preceded or followed by alphanumeric/underscore.
        result = replace_word_boundary(&result, base, &quote_ident(full));
    }
    result
}

/// Replace all occurrences of `word` in `text` that appear at word
/// boundaries (not preceded or followed by `[a-zA-Z0-9_]`).
///
/// Also avoids replacements inside single-quoted string literals.
fn replace_word_boundary(text: &str, word: &str, replacement: &str) -> String {
    if word.is_empty() || !text.contains(word) {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let word_chars: Vec<char> = word.chars().collect();
    let word_len = word_chars.len();
    let mut result = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;

    while i < chars.len() {
        // Track single-quoted strings
        if chars[i] == '\'' {
            in_string = !in_string;
            result.push(chars[i]);
            i += 1;
            continue;
        }

        if in_string {
            result.push(chars[i]);
            i += 1;
            continue;
        }

        // Check if `word` matches at position i
        if i + word_len <= chars.len() && &chars[i..i + word_len] == word_chars.as_slice() {
            // Check word boundary: char before must not be alphanumeric/underscore
            let before_ok = if i == 0 {
                true
            } else {
                let c = chars[i - 1];
                !c.is_alphanumeric() && c != '_' && c != '.'
            };
            // Check word boundary: char after must not be alphanumeric/underscore
            let after_ok = if i + word_len >= chars.len() {
                true
            } else {
                let c = chars[i + word_len];
                !c.is_alphanumeric() && c != '_'
            };

            if before_ok && after_ok {
                result.push_str(replacement);
                i += word_len;
                continue;
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    result
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

        assert_sql_contains(&sql, "__pgt_row_id");
        assert_sql_contains(&sql, "__pgt_action");
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
