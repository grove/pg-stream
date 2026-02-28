//! Shared helpers for join differentiation operators.
//!
//! Provides snapshot SQL generation and condition rewriting that handle
//! both simple (Scan) and nested (join-of-join) children correctly.
//!
//! ## Nested join handling
//!
//! When a join child is itself a join (e.g., `(A ⋈ B) ⋈ C`), two things
//! differ from the simple binary case:
//!
//! 1. **Snapshot**: The "current state" of the left/right child is not
//!    a plain table reference but a subquery: `(SELECT a."id" AS "a__id",
//!    ... FROM a JOIN b ON ...)`.
//!
//! 2. **Condition rewriting**: The join condition references original
//!    table aliases (e.g., `o.prod_id = p.id`). For nested children,
//!    these aliases are *inside* the snapshot subquery and must be
//!    translated to disambiguated column names (e.g., `l."o__prod_id"`).

use crate::dvm::diff::quote_ident;
use crate::dvm::operators::aggregate::agg_to_rescan_sql;
use crate::dvm::parser::{Expr, OpTree};

// ── Snapshot SQL generation ─────────────────────────────────────────────

/// Build a SQL expression for the current snapshot of an operator subtree.
///
/// For `Scan` nodes, returns the quoted `"schema"."table"` reference.
/// For join nodes, returns a parenthesized subquery with disambiguated
/// column names matching the diff engine's output format.
///
/// Used in join delta formulas where one side of the join must reference
/// the current full state of the other side.
pub fn build_snapshot_sql(op: &OpTree) -> String {
    match op {
        OpTree::Scan {
            schema, table_name, ..
        } => {
            format!(
                "\"{}\".\"{}\"",
                schema.replace('"', "\"\""),
                table_name.replace('"', "\"\""),
            )
        }
        OpTree::InnerJoin {
            condition,
            left,
            right,
        } => build_join_snapshot("JOIN", condition, left, right),
        OpTree::LeftJoin {
            condition,
            left,
            right,
        } => build_join_snapshot("LEFT JOIN", condition, left, right),
        OpTree::FullJoin {
            condition,
            left,
            right,
        } => build_join_snapshot("FULL JOIN", condition, left, right),
        OpTree::Filter { predicate, child } => {
            let child_snap = build_snapshot_sql(child);
            if matches!(child.as_ref(), OpTree::Scan { .. }) {
                let alias = child.alias();
                format!(
                    "(SELECT * FROM {} {} WHERE {})",
                    child_snap,
                    quote_ident(alias),
                    predicate.to_sql()
                )
            } else if matches!(child.as_ref(), OpTree::Aggregate { .. }) {
                // Filter on top of Aggregate = HAVING clause.
                // The child_snap is a `(SELECT ... GROUP BY ...)` subquery.
                // Wrap in an outer SELECT to apply the HAVING predicate.
                format!(
                    "(SELECT * FROM {} __having_sub WHERE {})",
                    child_snap,
                    predicate.to_sql()
                )
            } else {
                // For non-Scan children (e.g. Filter over Join), the filter
                // is applied by diff_filter in the diff pipeline. The
                // snapshot represents the unfiltered child state.
                child_snap
            }
        }
        OpTree::Project {
            expressions,
            aliases,
            child,
        } => {
            // A Project renames/transforms columns. The snapshot must preserve
            // these aliases so that downstream join conditions can reference
            // the projected column names (e.g., `__pgs_scalar_1` from a
            // scalar subquery CROSS JOIN rewrite).
            let inner = build_snapshot_sql(child);
            let child_alias = child.alias();
            let selects: Vec<String> = expressions
                .iter()
                .zip(aliases.iter())
                .map(|(expr, alias)| {
                    let expr_sql = expr.to_sql();
                    let alias_ident = quote_ident(alias);
                    if expr_sql == *alias {
                        alias_ident
                    } else {
                        format!("{expr_sql} AS {alias_ident}")
                    }
                })
                .collect();
            format!(
                "(SELECT {} FROM {} {})",
                selects.join(", "),
                inner,
                quote_ident(child_alias),
            )
        }
        OpTree::Subquery {
            column_aliases,
            child,
            ..
        } => {
            if column_aliases.is_empty() {
                build_snapshot_sql(child)
            } else {
                // Subquery with column aliases (e.g., `(...) AS v("c1", "c2")`).
                // Wrap the child snapshot in a SELECT that renames columns
                // positionally to match the aliases.
                let inner = build_snapshot_sql(child);
                let child_alias = child.alias();
                // Use positional references (ordinal) to rename
                let selects: Vec<String> = column_aliases
                    .iter()
                    .enumerate()
                    .map(|(i, alias)| {
                        // Reference by position: column number i+1
                        // We use a subquery wrapper so we can rename by ordinal
                        format!("__sub.col{} AS {}", i + 1, quote_ident(alias))
                    })
                    .collect();
                // Wrap inner snapshot with ordinal column names
                let child_cols = child.output_columns();
                let inner_selects: Vec<String> = child_cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{} AS col{}", quote_ident(c), i + 1))
                    .collect();
                format!(
                    "(SELECT {} FROM (SELECT {} FROM {} {}) __sub)",
                    selects.join(", "),
                    inner_selects.join(", "),
                    inner,
                    quote_ident(child_alias),
                )
            }
        }
        OpTree::Aggregate {
            group_by,
            aggregates,
            child,
        } => {
            let inner = build_snapshot_sql(child);
            let child_alias = child.alias();
            let mut selects = Vec::new();
            for expr in group_by {
                selects.push(expr.to_sql());
            }
            for agg in aggregates {
                selects.push(format!(
                    "{} AS {}",
                    agg_to_rescan_sql(agg),
                    quote_ident(&agg.alias),
                ));
            }
            let gb = if group_by.is_empty() {
                String::new()
            } else {
                let cols: Vec<String> = group_by.iter().map(|e| e.to_sql()).collect();
                format!(" GROUP BY {}", cols.join(", "))
            };
            format!(
                "(SELECT {} FROM {} {}{})",
                selects.join(", "),
                inner,
                quote_ident(child_alias),
                gb,
            )
        }
        OpTree::SemiJoin {
            condition,
            left,
            right,
        } => {
            let left_snap = build_snapshot_sql(left);
            let right_snap = build_snapshot_sql(right);
            // Use safe non-reserved aliases to avoid Expr::to_sql() emitting
            // unquoted reserved words like "join" (from InnerJoin.alias()).
            let left_alias = "__pgs_sl";
            let right_alias = "__pgs_sr";
            let cond = rewrite_join_condition(condition, left, left_alias, right, right_alias);
            format!(
                "(SELECT {la}.* FROM {left_snap} {la} WHERE EXISTS \
                 (SELECT 1 FROM {right_snap} {ra} WHERE {cond}))",
                la = quote_ident(left_alias),
                ra = quote_ident(right_alias),
            )
        }
        OpTree::AntiJoin {
            condition,
            left,
            right,
        } => {
            let left_snap = build_snapshot_sql(left);
            let right_snap = build_snapshot_sql(right);
            let left_alias = "__pgs_al";
            let right_alias = "__pgs_ar";
            let cond = rewrite_join_condition(condition, left, left_alias, right, right_alias);
            format!(
                "(SELECT {la}.* FROM {left_snap} {la} WHERE NOT EXISTS \
                 (SELECT 1 FROM {right_snap} {ra} WHERE {cond}))",
                la = quote_ident(left_alias),
                ra = quote_ident(right_alias),
            )
        }
        _ => {
            // Fallback for unsupported node types.
            format!("/* unsupported snapshot for {} */", op.node_kind())
        }
    }
}

/// Build a snapshot subquery for a join node.
///
/// Produces a parenthesized SELECT with disambiguated column names:
/// ```sql
/// (SELECT l."id" AS "l__id", ..., r."id" AS "r__id", ...
///  FROM left_snap l JOIN right_snap r ON condition)
/// ```
fn build_join_snapshot(join_type: &str, condition: &Expr, left: &OpTree, right: &OpTree) -> String {
    let left_snap = build_snapshot_sql(left);
    let right_snap = build_snapshot_sql(right);
    let left_alias = left.alias();
    let right_alias = right.alias();

    // Use snapshot_output_columns instead of output_columns to get the
    // correct disambiguated names that nested join snapshots produce.
    // For Scan children, these are the same as output_columns().
    // For join children, output_columns() returns raw names (c_custkey)
    // but the snapshot subquery aliases them as customer__c_custkey.
    let left_cols = snapshot_output_columns(left);
    let right_cols = snapshot_output_columns(right);

    let mut select_parts = Vec::new();
    for c in &left_cols {
        select_parts.push(format!(
            "{}.{} AS {}",
            quote_ident(left_alias),
            quote_ident(c),
            quote_ident(&format!("{left_alias}__{c}"))
        ));
    }
    for c in &right_cols {
        select_parts.push(format!(
            "{}.{} AS {}",
            quote_ident(right_alias),
            quote_ident(c),
            quote_ident(&format!("{right_alias}__{c}"))
        ));
    }

    // Rewrite condition for snapshot: use child aliases directly
    let cond_sql = rewrite_join_condition(condition, left, left_alias, right, right_alias);

    format!(
        "(SELECT {} FROM {} {} {} {} {} ON {})",
        select_parts.join(", "),
        left_snap,
        quote_ident(left_alias),
        join_type,
        right_snap,
        quote_ident(right_alias),
        cond_sql
    )
}

/// Return the column names as they appear in a snapshot subquery built by
/// [`build_snapshot_sql`].
///
/// For `Scan` nodes, snapshot columns are the same as `output_columns()`.
/// For join nodes, the snapshot subquery disambiguates names with the child
/// alias prefix (e.g., `customer__c_custkey`), so the returned names must
/// match that format. Using `output_columns()` directly for joins would
/// return the raw un-prefixed names, causing "column X does not exist"
/// errors when a higher-level join references the inner snapshot.
fn snapshot_output_columns(op: &OpTree) -> Vec<String> {
    match op {
        OpTree::Scan { .. } => op.output_columns(),
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => {
            let left_prefix = left.alias();
            let right_prefix = right.alias();
            let mut cols = Vec::new();
            for c in snapshot_output_columns(left) {
                cols.push(format!("{left_prefix}__{c}"));
            }
            for c in snapshot_output_columns(right) {
                cols.push(format!("{right_prefix}__{c}"));
            }
            cols
        }
        OpTree::Filter { child, .. } => snapshot_output_columns(child),
        OpTree::Project { aliases, .. } => {
            // Project snapshot uses the project's alias names
            aliases.clone()
        }
        OpTree::Subquery {
            column_aliases,
            child,
            ..
        } => {
            if column_aliases.is_empty() {
                snapshot_output_columns(child)
            } else {
                column_aliases.clone()
            }
        }
        _ => op.output_columns(),
    }
}

// ── Condition rewriting ─────────────────────────────────────────────────

/// Rewrite a join condition for use in a delta or snapshot query.
///
/// Replaces original table alias references with the provided new aliases,
/// handling nested joins by disambiguating column names with the original
/// table alias prefix when the source table is inside a nested join child.
///
/// For a simple case (Scan child), `o.cust_id` → `dl."cust_id"`.
/// For a nested case (Join child), `o.cust_id` → `dl."o__cust_id"`.
pub fn rewrite_join_condition(
    condition: &Expr,
    left: &OpTree,
    new_left: &str,
    right: &OpTree,
    new_right: &str,
) -> String {
    rewrite_expr_for_join(condition, left, new_left, right, new_right).to_sql()
}

/// Recursively rewrite an expression for join delta/snapshot usage.
fn rewrite_expr_for_join(
    expr: &Expr,
    left: &OpTree,
    new_left: &str,
    right: &OpTree,
    new_right: &str,
) -> Expr {
    match expr {
        Expr::ColumnRef {
            table_alias: Some(alias),
            column_name,
        } => {
            if has_source_alias(left, alias) {
                if is_simple_source(left, alias) {
                    // Direct table access — just remap the alias
                    Expr::ColumnRef {
                        table_alias: Some(new_left.to_string()),
                        column_name: column_name.clone(),
                    }
                } else if let Some(disambiguated) =
                    resolve_disambiguated_column(left, alias, column_name)
                {
                    // Deep disambiguation: trace through nested joins
                    Expr::ColumnRef {
                        table_alias: Some(new_left.to_string()),
                        column_name: disambiguated,
                    }
                } else {
                    // Fallback: single-level disambiguation
                    Expr::ColumnRef {
                        table_alias: Some(new_left.to_string()),
                        column_name: format!("{alias}__{column_name}"),
                    }
                }
            } else if has_source_alias(right, alias) {
                if is_simple_source(right, alias) {
                    Expr::ColumnRef {
                        table_alias: Some(new_right.to_string()),
                        column_name: column_name.clone(),
                    }
                } else if let Some(disambiguated) =
                    resolve_disambiguated_column(right, alias, column_name)
                {
                    Expr::ColumnRef {
                        table_alias: Some(new_right.to_string()),
                        column_name: disambiguated,
                    }
                } else {
                    Expr::ColumnRef {
                        table_alias: Some(new_right.to_string()),
                        column_name: format!("{alias}__{column_name}"),
                    }
                }
            } else {
                // Alias not found in either child — pass through unchanged
                expr.clone()
            }
        }
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => {
            // Unqualified column ref — resolve against left/right children
            // to find the source Scan and disambiguate correctly.
            if let Some(source_alias) = find_column_source(left, column_name) {
                if is_simple_source(left, &source_alias) {
                    Expr::ColumnRef {
                        table_alias: Some(new_left.to_string()),
                        column_name: column_name.clone(),
                    }
                } else if let Some(disambiguated) =
                    resolve_disambiguated_column(left, &source_alias, column_name)
                {
                    Expr::ColumnRef {
                        table_alias: Some(new_left.to_string()),
                        column_name: disambiguated,
                    }
                } else {
                    expr.clone()
                }
            } else if let Some(source_alias) = find_column_source(right, column_name) {
                if is_simple_source(right, &source_alias) {
                    Expr::ColumnRef {
                        table_alias: Some(new_right.to_string()),
                        column_name: column_name.clone(),
                    }
                } else if let Some(disambiguated) =
                    resolve_disambiguated_column(right, &source_alias, column_name)
                {
                    Expr::ColumnRef {
                        table_alias: Some(new_right.to_string()),
                        column_name: disambiguated,
                    }
                } else {
                    expr.clone()
                }
            } else {
                // Column not found in either child — pass through
                expr.clone()
            }
        }
        Expr::BinaryOp {
            op,
            left: l,
            right: r,
        } => Expr::BinaryOp {
            op: op.clone(),
            left: Box::new(rewrite_expr_for_join(l, left, new_left, right, new_right)),
            right: Box::new(rewrite_expr_for_join(r, left, new_left, right, new_right)),
        },
        Expr::FuncCall { func_name, args } => Expr::FuncCall {
            func_name: func_name.clone(),
            args: args
                .iter()
                .map(|a| rewrite_expr_for_join(a, left, new_left, right, new_right))
                .collect(),
        },
        Expr::Star { table_alias } => {
            // Rewrite star expressions: table.* → new_alias.*
            if let Some(alias) = table_alias {
                if has_source_alias(left, alias) {
                    Expr::Star {
                        table_alias: Some(new_left.to_string()),
                    }
                } else if has_source_alias(right, alias) {
                    Expr::Star {
                        table_alias: Some(new_right.to_string()),
                    }
                } else {
                    expr.clone()
                }
            } else {
                expr.clone()
            }
        }
        // Literals and Raw SQL without column references — pass through
        Expr::Literal(_) => expr.clone(),
        Expr::Raw(sql) => {
            // Best-effort: rewrite qualified column references in raw SQL
            // text. For each source alias in left/right children, replace
            // `alias."col"` and `alias.col` patterns with the new alias.
            let mut result = sql.clone();
            let all_aliases = collect_source_aliases(left)
                .into_iter()
                .chain(collect_source_aliases(right));
            for alias in all_aliases {
                let (new_alias, is_simple) = if has_source_alias(left, &alias) {
                    (new_left, is_simple_source(left, &alias))
                } else {
                    (new_right, is_simple_source(right, &alias))
                };

                if is_simple {
                    // Simple: replace alias.col → new_alias.col
                    // Match both alias."col" and alias.col patterns
                    let pattern = format!("{}.", alias);
                    let replacement = format!("{}.", new_alias);
                    result = result.replace(&pattern, &replacement);
                } else {
                    // Nested: alias.col → new_alias."alias__col"
                    // This is harder in raw SQL — we do a conservative
                    // pattern replacement for alias."col" → new_alias."alias__col"
                    // and alias.col → new_alias."alias__col"
                    let dot_prefix = format!("{}.", alias);
                    if result.contains(&dot_prefix) {
                        // Replace qualified references carefully
                        result = rewrite_raw_alias_refs(&result, &alias, new_alias);
                    }
                }
            }
            Expr::Raw(result)
        }
    }
}

/// Collect all source table aliases from an OpTree.
fn collect_source_aliases(op: &OpTree) -> Vec<String> {
    match op {
        OpTree::Scan { alias, .. } => vec![alias.clone()],
        OpTree::Subquery { alias, .. } => vec![alias.clone()],
        OpTree::CteScan { alias, .. } => vec![alias.clone()],
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. }
        | OpTree::SemiJoin { left, right, .. }
        | OpTree::AntiJoin { left, right, .. } => {
            let mut aliases = collect_source_aliases(left);
            aliases.extend(collect_source_aliases(right));
            aliases
        }
        OpTree::Intersect { left, right, .. } | OpTree::Except { left, right, .. } => {
            let mut aliases = collect_source_aliases(left);
            aliases.extend(collect_source_aliases(right));
            aliases
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Aggregate { child, .. }
        | OpTree::Distinct { child, .. } => collect_source_aliases(child),
        OpTree::UnionAll { children } => children.iter().flat_map(collect_source_aliases).collect(),
        // Window, LateralFunction, LateralSubquery, RecursiveCte,
        // RecursiveSelfRef, etc. — these rarely appear as direct join
        // children, but return empty to be safe.
        _ => vec![],
    }
}

/// Rewrite `alias.col` and `alias."col"` patterns in raw SQL text
/// to `new_alias."alias__col"` for nested join disambiguation.
fn rewrite_raw_alias_refs(sql: &str, old_alias: &str, new_alias: &str) -> String {
    let prefix = format!("{}.", old_alias);
    let mut result = String::with_capacity(sql.len());
    let mut remaining = sql;

    while let Some(pos) = remaining.find(&prefix) {
        // Copy everything before the match
        result.push_str(&remaining[..pos]);
        remaining = &remaining[pos + prefix.len()..];

        // Extract the column name after the dot
        let col_name = if remaining.starts_with('"') {
            // Quoted identifier: alias."col_name"
            if let Some(end) = remaining[1..].find('"') {
                let name = &remaining[1..1 + end];
                remaining = &remaining[2 + end..];
                name.to_string()
            } else {
                // Unterminated quote — pass through as-is
                result.push_str(&prefix);
                continue;
            }
        } else {
            // Unquoted identifier: read until non-identifier char
            let end = remaining
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(remaining.len());
            if end == 0 {
                // Nothing after the dot — pass through
                result.push_str(&prefix);
                continue;
            }
            let name = &remaining[..end];
            remaining = &remaining[end..];
            name.to_string()
        };

        // Emit the disambiguated reference: new_alias."old_alias__col"
        result.push_str(&format!(
            "{}.\"{}__{}\"",
            new_alias,
            old_alias.replace('"', "\"\""),
            col_name.replace('"', "\"\""),
        ));
    }

    // Append the rest
    result.push_str(remaining);
    result
}

/// Check if an OpTree contains a source table with the given alias.
///
/// Descends into join children, filters, projects, and subqueries to
/// find whether a specific table alias is accessible from this subtree.
pub fn has_source_alias(op: &OpTree, alias: &str) -> bool {
    match op {
        OpTree::Scan { alias: a, .. } => a == alias,
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. }
        | OpTree::SemiJoin { left, right, .. }
        | OpTree::AntiJoin { left, right, .. } => {
            has_source_alias(left, alias) || has_source_alias(right, alias)
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Aggregate { child, .. }
        | OpTree::Distinct { child, .. } => has_source_alias(child, alias),
        OpTree::Subquery {
            alias: sub_alias,
            child,
            ..
        } => {
            // A Subquery introduces a named scope (e.g., `(SELECT ...) AS revenue0`).
            // Its own alias is a valid source alias for column references.
            sub_alias == alias || has_source_alias(child, alias)
        }
        _ => false,
    }
}

/// Find which source Scan a bare (unqualified) column name belongs to.
///
/// Searches the OpTree for Scan nodes whose columns include `column_name`.
/// Returns the alias of the first matching Scan, or `None` if no Scan has
/// that column.
fn find_column_source(op: &OpTree, column_name: &str) -> Option<String> {
    match op {
        OpTree::Scan { alias, columns, .. } => {
            if columns.iter().any(|c| c.name == column_name) {
                Some(alias.clone())
            } else {
                None
            }
        }
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. }
        | OpTree::SemiJoin { left, right, .. }
        | OpTree::AntiJoin { left, right, .. } => {
            find_column_source(left, column_name).or_else(|| find_column_source(right, column_name))
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. }
        | OpTree::Aggregate { child, .. }
        | OpTree::Distinct { child, .. } => find_column_source(child, column_name),
        _ => None,
    }
}

/// Resolve a table-qualified column reference to its fully disambiguated
/// name in the context of a specific OpTree.
///
/// For deeply nested joins (e.g., `((supplier ⋈ l1) ⋈ orders) ⋈ nation`),
/// a column like `l1.l_orderkey` is disambiguated through multiple levels:
/// `l1__l_orderkey` → `join__l1__l_orderkey` → `join__join__l1__l_orderkey`.
///
/// This function recursively traces the nesting to produce the correct
/// fully-qualified column name.
fn resolve_disambiguated_column(
    op: &OpTree,
    table_alias: &str,
    column_name: &str,
) -> Option<String> {
    match op {
        OpTree::Scan { alias, .. } if alias == table_alias => {
            // Found the target scan — return alias__column_name
            Some(format!("{alias}__{column_name}"))
        }
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => {
            if has_source_alias(left, table_alias) {
                if is_simple_source(left, table_alias) {
                    // The table IS the left child scan — single level
                    Some(format!("{table_alias}__{column_name}"))
                } else {
                    // Nested — recurse and add the left child's alias prefix
                    let inner = resolve_disambiguated_column(left, table_alias, column_name)?;
                    Some(format!("{}__{inner}", left.alias()))
                }
            } else if has_source_alias(right, table_alias) {
                if is_simple_source(right, table_alias) {
                    Some(format!("{table_alias}__{column_name}"))
                } else {
                    let inner = resolve_disambiguated_column(right, table_alias, column_name)?;
                    Some(format!("{}__{inner}", right.alias()))
                }
            } else {
                None
            }
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => {
            resolve_disambiguated_column(child, table_alias, column_name)
        }
        // SemiJoin/AntiJoin output only left-side columns. The right
        // side is used for the EXISTS check and doesn't contribute to
        // the output. Recurse into the left child only.
        OpTree::SemiJoin { left, .. } | OpTree::AntiJoin { left, .. } => {
            resolve_disambiguated_column(left, table_alias, column_name)
        }
        _ => None,
    }
}

/// Check if a table alias is directly accessible (no column disambiguation needed).
///
/// Returns `true` if the alias corresponds to a `Scan` that IS the node
/// or is wrapped only by transparent operators (Filter, Project, Subquery).
/// Returns `false` if the alias is inside a nested join, meaning columns
/// are prefixed with the original table alias.
pub fn is_simple_source(op: &OpTree, alias: &str) -> bool {
    match op {
        OpTree::Scan { alias: a, .. } => a == alias,
        OpTree::Filter { child, .. } | OpTree::Project { child, .. } => {
            is_simple_source(child, alias)
        }
        OpTree::Subquery {
            alias: sub_alias,
            child,
            ..
        } => {
            // If the subquery's own alias matches, this IS the atomic source.
            // Columns are directly accessible without disambiguation (e.g.,
            // a derived table `(SELECT ...) AS revenue0` — columns are
            // accessed as `revenue0.col`, not `revenue0__col`).
            if sub_alias == alias {
                true
            } else {
                is_simple_source(child, alias)
            }
        }
        // SemiJoin/AntiJoin pass through the left child's columns
        OpTree::SemiJoin { left, .. } | OpTree::AntiJoin { left, .. } => {
            is_simple_source(left, alias)
        }
        // For joins, the alias is inside the join — needs disambiguation
        _ => false,
    }
}

/// Check if a child is a "simple source" (Scan or transparent wrapper over Scan).
///
/// Used to determine if semi-join optimization can be applied — the
/// optimization requires filtering a plain table, not a complex subquery.
pub fn is_simple_child(op: &OpTree) -> bool {
    match op {
        OpTree::Scan { .. } => true,
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => is_simple_child(child),
        _ => false,
    }
}

/// Build per-column `alias."col"::TEXT` expressions for the base-table
/// side of a join, suitable for inclusion in a flat
/// `pg_stream_hash_multi(ARRAY[...])` call.
///
/// For `Scan` nodes this uses the PK (non-nullable) columns; for
/// non-Scan children it falls back to `row_to_json(alias)::text`.
pub fn build_base_table_key_exprs(op: &OpTree, alias: &str) -> Vec<String> {
    match op {
        OpTree::Scan { columns, .. } => {
            let non_nullable: Vec<&str> = columns
                .iter()
                .filter(|c| !c.is_nullable)
                .map(|c| c.name.as_str())
                .collect();

            let key_cols: Vec<&str> = if non_nullable.is_empty() {
                columns.iter().map(|c| c.name.as_str()).collect()
            } else {
                non_nullable
            };

            key_cols
                .iter()
                .map(|c| format!("{alias}.{}::TEXT", quote_ident(c)))
                .collect()
        }
        _ => {
            // Non-Scan child: fall back to row_to_json
            vec![format!("row_to_json({alias})::text")]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    // ── build_snapshot_sql tests ────────────────────────────────

    #[test]
    fn test_snapshot_scan() {
        let node = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let snap = build_snapshot_sql(&node);
        assert_eq!(snap, "\"public\".\"orders\"");
    }

    #[test]
    fn test_snapshot_inner_join() {
        let left = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let right = scan(2, "customers", "public", "c", &["id", "name"]);
        let cond = eq_cond("o", "cust_id", "c", "id");
        let node = inner_join(cond, left, right);
        let snap = build_snapshot_sql(&node);

        // Should be a subquery with disambiguated column names
        assert!(snap.starts_with('('));
        assert!(snap.contains("\"o__id\""));
        assert!(snap.contains("\"o__cust_id\""));
        assert!(snap.contains("\"c__id\""));
        assert!(snap.contains("\"c__name\""));
        assert!(snap.contains("JOIN"));
    }

    #[test]
    fn test_snapshot_left_join() {
        let left = scan(1, "a", "public", "a", &["id"]);
        let right = scan(2, "b", "public", "b", &["id"]);
        let cond = eq_cond("a", "id", "b", "id");
        let node = left_join(cond, left, right);
        let snap = build_snapshot_sql(&node);

        assert!(snap.contains("LEFT JOIN"));
    }

    #[test]
    fn test_snapshot_nested_join() {
        // (orders o JOIN customers c) JOIN products p
        let o = scan(1, "orders", "public", "o", &["id", "cust_id", "prod_id"]);
        let c = scan(2, "customers", "public", "c", &["id", "name"]);
        let inner = inner_join(eq_cond("o", "cust_id", "c", "id"), o, c);
        let p = scan(3, "products", "public", "p", &["id", "price"]);
        let outer = inner_join(eq_cond("o", "prod_id", "p", "id"), inner, p);

        let snap = build_snapshot_sql(&outer);
        // Outer join should reference a subquery for the inner join
        assert!(snap.contains("\"join\""));
        assert!(snap.contains("\"p\""));
    }

    #[test]
    fn test_snapshot_filter_over_scan() {
        let child = scan(1, "t", "public", "t", &["id", "status"]);
        let node = filter(binop("=", qcolref("t", "status"), lit("'active'")), child);
        let snap = build_snapshot_sql(&node);
        assert!(snap.contains("SELECT *"));
        assert!(snap.contains("WHERE"));
    }

    // ── has_source_alias tests ──────────────────────────────────

    #[test]
    fn test_has_source_alias_scan() {
        let node = scan(1, "orders", "public", "o", &["id"]);
        assert!(has_source_alias(&node, "o"));
        assert!(!has_source_alias(&node, "x"));
    }

    #[test]
    fn test_has_source_alias_nested_join() {
        let o = scan(1, "orders", "public", "o", &["id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let node = inner_join(eq_cond("o", "id", "c", "id"), o, c);
        assert!(has_source_alias(&node, "o"));
        assert!(has_source_alias(&node, "c"));
        assert!(!has_source_alias(&node, "x"));
    }

    // ── is_simple_source tests ──────────────────────────────────

    #[test]
    fn test_is_simple_source_scan() {
        let node = scan(1, "t", "public", "t", &["id"]);
        assert!(is_simple_source(&node, "t"));
    }

    #[test]
    fn test_is_simple_source_filter_over_scan() {
        let node = filter(lit("TRUE"), scan(1, "t", "public", "t", &["id"]));
        assert!(is_simple_source(&node, "t"));
    }

    #[test]
    fn test_is_simple_source_nested_join() {
        let o = scan(1, "orders", "public", "o", &["id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let node = inner_join(eq_cond("o", "id", "c", "id"), o, c);
        // "o" is inside a join → not simple
        assert!(!is_simple_source(&node, "o"));
        assert!(!is_simple_source(&node, "c"));
    }

    // ── rewrite_join_condition tests ────────────────────────────

    #[test]
    fn test_rewrite_simple_condition() {
        let o = scan(1, "orders", "public", "o", &["id", "cust_id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let cond = eq_cond("o", "cust_id", "c", "id");

        let rewritten = rewrite_join_condition(&cond, &o, "dl", &c, "r");
        assert!(rewritten.contains("dl."));
        assert!(rewritten.contains("r."));
    }

    #[test]
    fn test_rewrite_nested_condition() {
        // Outer join: (orders ⋈ customers) ⋈ products
        // Condition: o.prod_id = p.id
        let o = scan(1, "orders", "public", "o", &["id", "prod_id"]);
        let c = scan(2, "customers", "public", "c", &["id"]);
        let inner = inner_join(eq_cond("o", "id", "c", "id"), o, c);
        let p = scan(3, "products", "public", "p", &["id"]);

        let cond = eq_cond("o", "prod_id", "p", "id");
        let rewritten = rewrite_join_condition(&cond, &inner, "dl", &p, "r");

        // "o" is inside the inner join → disambiguated to "o__prod_id"
        assert!(
            rewritten.contains("o__prod_id"),
            "expected o__prod_id, got: {rewritten}"
        );
        // "p" is a simple Scan → plain "id"
        assert!(rewritten.contains("r."));
    }

    #[test]
    fn test_rewrite_both_sides_nested() {
        let a = scan(1, "a", "public", "a", &["id"]);
        let b = scan(2, "b", "public", "b", &["id"]);
        let left = inner_join(eq_cond("a", "id", "b", "id"), a, b);

        let c = scan(3, "c", "public", "c", &["id"]);
        let d = scan(4, "d", "public", "d", &["id"]);
        let right = inner_join(eq_cond("c", "id", "d", "id"), c, d);

        let cond = eq_cond("a", "id", "c", "id");
        let rewritten = rewrite_join_condition(&cond, &left, "dl", &right, "r");
        assert!(
            rewritten.contains("a__id"),
            "expected a__id, got: {rewritten}"
        );
        assert!(
            rewritten.contains("c__id"),
            "expected c__id, got: {rewritten}"
        );
    }

    // ── is_simple_child tests ───────────────────────────────────

    #[test]
    fn test_is_simple_child_scan() {
        assert!(is_simple_child(&scan(1, "t", "public", "t", &["id"])));
    }

    #[test]
    fn test_is_simple_child_filter_over_scan() {
        let node = filter(lit("TRUE"), scan(1, "t", "public", "t", &["id"]));
        assert!(is_simple_child(&node));
    }

    #[test]
    fn test_is_simple_child_join() {
        let o = scan(1, "a", "public", "a", &["id"]);
        let c = scan(2, "b", "public", "b", &["id"]);
        let node = inner_join(eq_cond("a", "id", "b", "id"), o, c);
        assert!(!is_simple_child(&node));
    }

    // ── build_base_table_key_exprs tests ────────────────────────

    #[test]
    fn test_key_exprs_scan_non_nullable() {
        let node = scan_not_null(1, "orders", "public", "o", &["id", "name"]);
        let exprs = build_base_table_key_exprs(&node, "r");
        assert!(exprs.iter().any(|e| e.contains("r.\"id\"::TEXT")));
    }

    #[test]
    fn test_key_exprs_non_scan_fallback() {
        let o = scan(1, "a", "public", "a", &["id"]);
        let c = scan(2, "b", "public", "b", &["id"]);
        let node = inner_join(eq_cond("a", "id", "b", "id"), o, c);
        let exprs = build_base_table_key_exprs(&node, "l");
        assert_eq!(exprs, vec!["row_to_json(l)::text"]);
    }
}
