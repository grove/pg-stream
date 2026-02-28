//! Differential View Maintenance (DVM) engine.
//!
//! This module implements query differentiation — transforming a defining
//! query Q into a delta query ΔQ that computes only the changes over a
//! given interval.
//!
//! # Theoretical Basis
//!
//! The differential computation framework in this module is derived from:
//!
//! - **DBSP**: Budiu, M. et al. (2023). "DBSP: Automatic Incremental View
//!   Maintenance for Rich Query Languages." PVLDB, 16(7), 1601–1614.
//!   <https://arxiv.org/abs/2203.16684>
//!   The `Z-set` abstraction (rows with +1/−1 multiplicity) directly maps to
//!   the `__pgt_action` column produced by the delta operators.
//!
//! - **Gupta & Mumick (1995)**: "Maintenance of Materialized Views: Problems,
//!   Techniques, and Applications." IEEE Data Engineering Bulletin, 18(2).
//!   The per-operator differentiation rules in `operators/` follow the
//!   derivation given in section 3 of this survey.
//!
//! - **Koch, C. et al. (2014)**: "DBToaster: Higher-order Delta Processing
//!   for Dynamic, Frequently Fresh Views." VLDB Journal, 23(2), 253–278.
//!   Recursive delta compilation strategy inspiration.
//!
//! - **PostgreSQL `REFRESH MATERIALIZED VIEW CONCURRENTLY`** (since PostgreSQL
//!   9.4, December 2014, commit `96ef3b8`): the snapshot-diff strategy used
//!   for recomputation-diff refreshes mirrors the algorithm in
//!   `src/backend/commands/matview.c`.
//!
//! # Submodules
//! - `parser` — Parse defining query into an operator tree
//! - `diff` — Query differentiation framework
//! - `row_id` — Row ID generation strategies
//! - `operators` — Per-operator differentiation rules
//!
//! # Usage
//! ```ignore
//! use crate::dvm::generate_delta_query;
//!
//! let result = generate_delta_query(
//!     &defining_query,
//!     &prev_frontier,
//!     &new_frontier,
//!     "myschema",
//!     "my_st",
//! )?;
//! let delta_sql = result.delta_sql;
//! let columns = result.output_columns;
//! let oids = result.source_oids;
//! ```

pub mod diff;
pub mod operators;
pub mod parser;
pub mod row_id;

pub use diff::DiffContext;
pub use parser::{
    CteRegistry, ParseResult, check_ivm_support, check_ivm_support_with_registry,
    parse_defining_query, parse_defining_query_full, query_has_recursive_cte, reject_limit_offset,
    reject_materialized_views, reject_unsupported_constructs, rewrite_distinct_on,
    rewrite_grouping_sets, rewrite_multi_partition_windows, rewrite_scalar_subquery_in_where,
    rewrite_sublinks_in_or, rewrite_views_inline, tree_worst_volatility_with_registry,
};

use crate::error::PgTrickleError;
use crate::version::Frontier;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

// ── Delta template cache ─────────────────────────────────────────────

/// Cached delta query template: stores the SQL with LSN placeholder tokens
/// and the metadata (output columns, source OIDs) that remain stable across
/// refreshes for the same defining query.
#[derive(Clone)]
struct CachedDeltaTemplate {
    /// Hash of the defining query string — used to detect changes.
    defining_query_hash: u64,
    /// Delta SQL with `__PGS_PREV_LSN_{oid}__` / `__PGS_NEW_LSN_{oid}__`
    /// placeholder tokens instead of literal LSN values.
    delta_sql_template: String,
    /// User-facing output column names (excludes __pgt_row_id / __pgt_action).
    output_columns: Vec<String>,
    /// Deduplicated source table OIDs.
    source_oids: Vec<u32>,
    /// Whether the delta output is already deduplicated per __pgt_row_id.
    is_deduplicated: bool,
}

thread_local! {
    /// Per-session cache of delta SQL templates, keyed by `pgt_id`.
    ///
    /// The template is invalidated when the defining query hash changes
    /// (e.g. after `ALTER STREAM TABLE`). Stale entries for dropped STs
    /// are harmless — they'll be evicted on the next cache miss.
    ///
    /// Cross-session invalidation (G8.1): flushed when the shared
    /// `CACHE_GENERATION` counter advances.
    static DELTA_TEMPLATE_CACHE: RefCell<HashMap<i64, CachedDeltaTemplate>> =
        RefCell::new(HashMap::new());

    /// Local snapshot of the shared `CACHE_GENERATION` counter.
    /// When the shared value advances past this, the entire cache is flushed.
    static LOCAL_DELTA_CACHE_GEN: Cell<u64> = const { Cell::new(0) };
}

/// Hash a string using the default hasher (for cache invalidation).
fn hash_string(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Resolve a delta SQL template by substituting LSN placeholder tokens
/// with actual frontier values.
fn resolve_delta_template(
    template: &str,
    source_oids: &[u32],
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
) -> String {
    let mut sql = template.to_string();
    for &oid in source_oids {
        let prev_placeholder = format!("__PGS_PREV_LSN_{oid}__");
        let new_placeholder = format!("__PGS_NEW_LSN_{oid}__");
        let prev_lsn = prev_frontier.get_lsn(oid);
        let new_lsn = new_frontier.get_lsn(oid);
        sql = sql.replace(&prev_placeholder, &prev_lsn);
        sql = sql.replace(&new_placeholder, &new_lsn);
    }
    sql
}

/// Invalidate cached delta templates for a given ST (e.g. after DDL).
pub fn invalidate_delta_cache(pgt_id: i64) {
    DELTA_TEMPLATE_CACHE.with(|cache| {
        cache.borrow_mut().remove(&pgt_id);
    });
}

/// Retrieve the raw delta SQL template (with placeholder tokens) for a ST.
///
/// Returns `None` if the template has not been generated yet.
/// The returned template contains `__PGS_PREV_LSN_{oid}__` and
/// `__PGS_NEW_LSN_{oid}__` tokens that must be resolved before execution.
pub fn get_delta_sql_template(pgt_id: i64) -> Option<String> {
    DELTA_TEMPLATE_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&pgt_id)
            .map(|entry| entry.delta_sql_template.clone())
    })
}

/// Check whether the cached delta for a ST is deduplicated (at most one
/// row per `__pgt_row_id`), allowing the MERGE to skip DISTINCT ON.
pub fn is_delta_deduplicated(pgt_id: i64) -> bool {
    DELTA_TEMPLATE_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&pgt_id)
            .map(|entry| entry.is_deduplicated)
            .unwrap_or(false)
    })
}

/// Check whether an OpTree is a "scan-chain" — only Scan, Filter, Project,
/// and Subquery nodes (no Aggregate, Join, UnionAll, Distinct, Window,
/// RecursiveCte, or CteScan).
///
/// When the top-level tree is a scan-chain, the scan delta can produce
/// deduplicated output (at most one row per PK) which eliminates the
/// need for DISTINCT ON in the MERGE statement.
///
/// **Filter is NOT part of a scan chain.** In merge-safe dedup mode the
/// scan emits only a single INSERT event for UPDATEs (no paired DELETE).
/// If a filter sits above the scan, an UPDATE that moves a row from
/// *passing* the predicate to *failing* it would only produce an INSERT
/// with new values, which the filter discards — leaving the stale old
/// row in the ST.  Standard D+I mode (merge_safe_dedup=false) emits
/// both DELETE(old) and INSERT(new), so the filter correctly passes the
/// DELETE for old values that matched and discards the INSERT with new
/// values that don't match.
fn is_scan_chain_tree(tree: &parser::OpTree) -> bool {
    match tree {
        parser::OpTree::Scan { .. } => true,
        parser::OpTree::Project { child, .. } => is_scan_chain_tree(child),
        parser::OpTree::Subquery { child, .. } => is_scan_chain_tree(child),
        _ => false,
    }
}

/// Result returned by [`generate_delta_query`], bundling the delta SQL
/// together with metadata extracted from the single parse so callers
/// do not need to re-parse the defining query.
pub struct DeltaQueryResult {
    /// The complete delta SQL (WITH … SELECT …).
    pub delta_sql: String,
    /// User-facing output column names (excludes __pgt_row_id / __pgt_action).
    pub output_columns: Vec<String>,
    /// Deduplicated source table OIDs (from both main tree and CTE registry).
    pub source_oids: Vec<u32>,
    /// When true, the delta has at most one row per `__pgt_row_id`,
    /// so the MERGE can skip the outer DISTINCT ON + ORDER BY.
    pub is_deduplicated: bool,
}

/// Generate the full delta SQL query for a defining query.
///
/// This is the main entry point for the DVM engine. It:
/// 1. Parses the defining query into an OpTree + CTE registry
/// 2. Checks DVM support (including CTE bodies)
/// 3. Generates the delta query via differentiation
///
/// For recursive CTEs, a recomputation diff strategy is used: the
/// defining query is re-executed in full and diffed against the current
/// ST storage to produce precise INSERT/DELETE deltas.
///
/// Returns a [`DeltaQueryResult`] containing the delta SQL, output
/// column names, and source OIDs — all derived from a single parse.
pub fn generate_delta_query(
    defining_query: &str,
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
    pgt_schema: &str,
    pgt_name: &str,
) -> Result<DeltaQueryResult, PgTrickleError> {
    // Step 1: Parse the defining query into an operator tree + CTE registry.
    // This now handles recursive CTEs via OpTree::RecursiveCte, so no
    // early bypass is needed.
    let result = parse_defining_query_full(defining_query)?;

    // Extract source OIDs before moving cte_registry.
    let mut source_oids: Vec<u32> = result.tree.source_oids();
    source_oids.extend(result.cte_registry.source_oids());
    source_oids.sort_unstable();
    source_oids.dedup();

    // Step 2: Check DVM support (validates CTE bodies + main tree)
    check_ivm_support_with_registry(&result)?;

    // Step 3: Generate the delta query.
    // Use differentiate_with_columns() to get the diff result's column list,
    // which includes auxiliary columns (e.g. __pgt_count) for aggregate/distinct.
    let st_user_cols = result.tree.output_columns();
    let is_scan_chain = is_scan_chain_tree(&result.tree);
    let mut ctx = DiffContext::new(prev_frontier.clone(), new_frontier.clone())
        .with_pgt_name(pgt_schema, pgt_name)
        .with_cte_registry(result.cte_registry)
        .with_defining_query(defining_query);
    ctx.st_user_columns = Some(st_user_cols);
    ctx.merge_safe_dedup = is_scan_chain;
    let (delta_sql, output_columns, diff_dedup) = ctx.differentiate_with_columns(&result.tree)?;

    Ok(DeltaQueryResult {
        delta_sql,
        output_columns,
        source_oids,
        is_deduplicated: is_scan_chain || diff_dedup,
    })
}

/// Generate the full delta SQL query, using a per-session cache to avoid
/// re-parsing and re-differentiating the defining query on every refresh.
///
/// On the first call for a given `pgt_id`, the defining query is parsed,
/// validated, and differentiated with LSN placeholders. The resulting SQL
/// template and metadata are cached. On subsequent calls, the cached
/// template is resolved with actual frontier LSN values — skipping the
/// parse, DVM-support check, and differentiation entirely.
///
/// Cache entries are keyed by `pgt_id` and invalidated when the
/// `defining_query` hash changes (e.g. after `ALTER STREAM TABLE`).
pub fn generate_delta_query_cached(
    pgt_id: i64,
    defining_query: &str,
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
    pgt_schema: &str,
    pgt_name: &str,
) -> Result<DeltaQueryResult, PgTrickleError> {
    let query_hash = hash_string(defining_query);

    // G8.1: Cross-session cache invalidation — flush if the shared
    // generation counter has advanced past our local snapshot.
    let shared_gen = crate::shmem::current_cache_generation();
    LOCAL_DELTA_CACHE_GEN.with(|local| {
        if local.get() < shared_gen {
            DELTA_TEMPLATE_CACHE.with(|cache| cache.borrow_mut().clear());
            local.set(shared_gen);
        }
    });

    // Check the thread-local cache.
    let cached = DELTA_TEMPLATE_CACHE.with(|cache| {
        let map = cache.borrow();
        map.get(&pgt_id)
            .filter(|entry| entry.defining_query_hash == query_hash)
            .cloned()
    });

    if let Some(entry) = cached {
        // Cache hit — resolve placeholders and return.
        let delta_sql = resolve_delta_template(
            &entry.delta_sql_template,
            &entry.source_oids,
            prev_frontier,
            new_frontier,
        );
        return Ok(DeltaQueryResult {
            delta_sql,
            output_columns: entry.output_columns,
            source_oids: entry.source_oids,
            is_deduplicated: entry.is_deduplicated,
        });
    }

    // Cache miss — parse, differentiate with placeholder mode, and cache.
    let result = parse_defining_query_full(defining_query)?;

    let mut source_oids: Vec<u32> = result.tree.source_oids();
    source_oids.extend(result.cte_registry.source_oids());
    source_oids.sort_unstable();
    source_oids.dedup();

    check_ivm_support_with_registry(&result)?;

    // Generate template with placeholder tokens instead of literal LSNs.
    // Use dummy frontiers — the actual LSN values come from placeholders.
    let is_scan_chain = is_scan_chain_tree(&result.tree);
    let st_user_cols = result.tree.output_columns();
    let mut ctx = DiffContext::new(Frontier::new(), Frontier::new())
        .with_placeholders()
        .with_pgt_name(pgt_schema, pgt_name)
        .with_cte_registry(result.cte_registry)
        .with_defining_query(defining_query);
    ctx.st_user_columns = Some(st_user_cols);
    ctx.merge_safe_dedup = is_scan_chain;
    let (template_sql, output_columns, diff_dedup) =
        ctx.differentiate_with_columns(&result.tree)?;

    // Store in cache.
    let entry = CachedDeltaTemplate {
        defining_query_hash: query_hash,
        delta_sql_template: template_sql.clone(),
        output_columns: output_columns.clone(),
        source_oids: source_oids.clone(),
        is_deduplicated: is_scan_chain || diff_dedup,
    };
    DELTA_TEMPLATE_CACHE.with(|cache| {
        cache.borrow_mut().insert(pgt_id, entry);
    });

    // Resolve placeholders for this invocation.
    let delta_sql =
        resolve_delta_template(&template_sql, &source_oids, prev_frontier, new_frontier);

    Ok(DeltaQueryResult {
        delta_sql,
        output_columns,
        source_oids,
        is_deduplicated: is_scan_chain || diff_dedup,
    })
}

/// Check whether a defining query needs the `__pgt_count` auxiliary column
/// (the top-level operator is Aggregate or Distinct).
///
/// Uses a lightweight parse — no SPI or database access required.
pub fn query_needs_pgt_count(defining_query: &str) -> bool {
    parse_defining_query(defining_query)
        .map(|tree| tree.needs_pgt_count())
        .unwrap_or(false)
}

/// Extract GROUP BY column names from a defining query.
///
/// Returns `Some(["region", "category"])` for aggregate queries with
/// GROUP BY, `None` for non-aggregate or scalar-aggregate queries.
///
/// Uses a lightweight parse — no SPI or database access required.
pub fn extract_group_by_columns(defining_query: &str) -> Option<Vec<String>> {
    parse_defining_query(defining_query)
        .ok()
        .and_then(|tree| tree.group_by_columns())
}

/// Generate a SQL expression for computing `__pgt_row_id` from a subquery
/// aliased as `sub`, matching the hash formula used by the delta query.
///
/// Returns an expression like `pgtrickle.pg_trickle_hash(sub."id"::text)` for scan PK,
/// `pgtrickle.pg_trickle_hash(sub."region"::text)` for aggregate GROUP BY, etc.
///
/// Falls back to `pgtrickle.pg_trickle_hash(row_to_json(sub)::text)` for queries whose
/// row-id computation is too complex (joins, union all).
pub fn row_id_expr_for_query(defining_query: &str) -> String {
    let tree = parse_defining_query(defining_query).ok();
    let key_cols = tree.as_ref().and_then(|t| t.row_id_key_columns());

    match key_cols {
        Some(cols) if cols.len() == 1 => {
            format!(
                "pgtrickle.pg_trickle_hash(sub.{}::text)",
                diff::quote_ident(&cols[0]),
            )
        }
        Some(cols) if cols.len() > 1 => {
            let array_items: Vec<String> = cols
                .iter()
                .map(|c| format!("sub.{}::TEXT", diff::quote_ident(c)))
                .collect();
            format!(
                "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
                array_items.join(", ")
            )
        }
        _ => {
            // Scalar aggregate (no GROUP BY): use singleton sentinel hash
            // matching the differential delta's __singleton_group row_id.
            // Without this, FULL refresh would use row_to_json hashing
            // while DIFF uses '__singleton_group', causing __pgt_row_id
            // mismatch and phantom row insertion.
            if tree.as_ref().is_some_and(is_scalar_aggregate_root) {
                "pgtrickle.pg_trickle_hash('__singleton_group')".to_string()
            } else {
                // Fallback for complex queries (joins, union all, etc.)
                // Include row_number() to disambiguate duplicate-content rows
                // (e.g., recursive CTEs with UNION ALL that reach the same
                // values via different derivation paths).
                "pgtrickle.pg_trickle_hash(row_to_json(sub)::text || '/' || row_number() OVER ()::text)"
                    .to_string()
            }
        }
    }
}

/// Check whether the root of an OpTree is a scalar aggregate (GROUP BY
/// with no columns). Looks through transparent wrappers (Filter, Project,
/// Subquery) to find the Aggregate node.
fn is_scalar_aggregate_root(tree: &parser::OpTree) -> bool {
    match tree {
        parser::OpTree::Aggregate { group_by, .. } => group_by.is_empty(),
        parser::OpTree::Filter { child, .. }
        | parser::OpTree::Project { child, .. }
        | parser::OpTree::Subquery { child, .. } => is_scalar_aggregate_root(child),
        _ => false,
    }
}

/// For UNION ALL queries, generate a full-refresh SELECT SQL that computes
/// per-branch child-prefixed row IDs matching the delta query's formula.
///
/// Returns `None` if the query is not a top-level UNION ALL or the branches
/// cannot be decomposed (e.g., a branch has no deterministic PK columns).
///
/// The returned SQL is a SELECT producing `__pgt_row_id` plus user columns,
/// ready to be prefixed with `INSERT INTO schema.table`.
pub fn try_union_all_refresh_sql(defining_query: &str) -> Option<String> {
    let branches = split_top_level_union_all(defining_query)?;

    let mut parts = Vec::new();
    for (i, branch_sql) in branches.iter().enumerate() {
        let idx = i + 1;
        // Parse the branch to determine its row-id key columns.
        let tree = parse_defining_query(branch_sql).ok()?;
        let key_cols = tree.row_id_key_columns()?;

        // Build the child hash expression (same formula as the scan diff).
        let child_hash = if key_cols.len() == 1 {
            format!(
                "pgtrickle.pg_trickle_hash(sub.{}::text)",
                diff::quote_ident(&key_cols[0]),
            )
        } else {
            let items: Vec<String> = key_cols
                .iter()
                .map(|c| format!("sub.{}::TEXT", diff::quote_ident(c)))
                .collect();
            format!(
                "pgtrickle.pg_trickle_hash_multi(ARRAY[{}])",
                items.join(", ")
            )
        };

        // Wrap with branch prefix (matching diff_union_all's idx = i + 1).
        let row_id_expr =
            format!("pgtrickle.pg_trickle_hash_multi(ARRAY['{idx}'::TEXT, ({child_hash})::TEXT])",);

        parts.push(format!(
            "SELECT {row_id_expr} AS __pgt_row_id, sub.* FROM ({branch_sql}) sub",
        ));
    }

    Some(parts.join("\nUNION ALL\n"))
}

/// Split a SQL query on top-level `UNION ALL` boundaries.
///
/// Returns `None` if the query has no top-level UNION ALL.
/// Respects parentheses, single-quoted strings, and double-quoted identifiers.
fn split_top_level_union_all(query: &str) -> Option<Vec<String>> {
    let bytes = query.as_bytes();
    let len = bytes.len();
    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut last_split = 0;

    let mut i = 0;
    while i < len {
        let ch = bytes[i];

        if in_single_quote {
            if ch == b'\'' {
                if i + 1 < len && bytes[i + 1] == b'\'' {
                    i += 2; // escaped quote
                    continue;
                }
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            if ch == b'"' {
                if i + 1 < len && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_double_quote = false;
            }
            i += 1;
            continue;
        }

        match ch {
            b'\'' => in_single_quote = true,
            b'"' => in_double_quote = true,
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ if depth == 0 => {
                // Look for keyword UNION at a word boundary.
                if query[i..].len() >= 5
                    && query[i..i + 5].eq_ignore_ascii_case("UNION")
                    && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
                {
                    // Skip whitespace after "UNION"
                    let mut j = i + 5;
                    while j < len && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    // Check for "ALL" keyword
                    if j + 3 <= len
                        && query[j..j + 3].eq_ignore_ascii_case("ALL")
                        && (j + 3 >= len
                            || !(bytes[j + 3].is_ascii_alphanumeric() || bytes[j + 3] == b'_'))
                    {
                        parts.push(query[last_split..i].trim().to_string());
                        last_split = j + 3;
                        i = j + 3;
                        continue;
                    }
                }
            }
            _ => {}
        }

        i += 1;
    }

    parts.push(query[last_split..].trim().to_string());

    if parts.len() >= 2 { Some(parts) } else { None }
}

/// Get output column names from a defining query by running it with LIMIT 0.
///
/// This works for all query types including recursive CTEs, since PostgreSQL
/// handles the full query execution (we just inspect the result metadata).
pub fn get_defining_query_columns(defining_query: &str) -> Result<Vec<String>, PgTrickleError> {
    use pgrx::Spi;

    let probe_sql = format!("SELECT * FROM ({defining_query}) __pgt_probe LIMIT 0");

    Spi::connect(|client| {
        let result = client
            .select(&probe_sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(format!("Column probe failed: {e}")))?;

        let ncols = result
            .columns()
            .map_err(|e| PgTrickleError::SpiError(format!("Failed to get column count: {e}")))?;

        if ncols == 0 {
            return Err(PgTrickleError::QueryParseError(
                "Defining query produces no columns".into(),
            ));
        }

        let mut columns = Vec::with_capacity(ncols);
        for i in 1..=ncols {
            let name = result
                .column_name(i)
                .unwrap_or_else(|_| format!("column_{i}"));
            columns.push(name);
        }

        Ok(columns)
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    // ── split_top_level_union_all (existing) ────────────────────────

    #[test]
    fn test_split_union_all_simple() {
        let parts =
            split_top_level_union_all("SELECT id FROM t1 UNION ALL SELECT id FROM t2").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "SELECT id FROM t1");
        assert_eq!(parts[1], "SELECT id FROM t2");
    }

    #[test]
    fn test_split_union_all_three_branches() {
        let parts = split_top_level_union_all(
            "SELECT a FROM t1 UNION ALL SELECT a FROM t2 UNION ALL SELECT a FROM t3",
        )
        .unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2], "SELECT a FROM t3");
    }

    #[test]
    fn test_split_union_all_in_subquery_not_split() {
        // UNION ALL inside parens should NOT be split at the top level.
        let result = split_top_level_union_all("SELECT * FROM (SELECT 1 UNION ALL SELECT 2) sub");
        assert!(result.is_none());
    }

    #[test]
    fn test_split_union_all_case_insensitive() {
        let parts =
            split_top_level_union_all("SELECT id FROM t1 union all SELECT id FROM t2").unwrap();
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn test_split_union_all_with_extra_whitespace() {
        let parts =
            split_top_level_union_all("SELECT id FROM t1  UNION  ALL  SELECT id FROM t2").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "SELECT id FROM t1");
        assert_eq!(parts[1], "SELECT id FROM t2");
    }

    #[test]
    fn test_split_no_union_all() {
        assert!(split_top_level_union_all("SELECT id FROM t1").is_none());
    }

    #[test]
    fn test_split_union_without_all_not_split() {
        // Plain UNION (without ALL) should not be split.
        assert!(split_top_level_union_all("SELECT id FROM t1 UNION SELECT id FROM t2").is_none());
    }

    #[test]
    fn test_split_union_all_preserves_quoted_strings() {
        let parts =
            split_top_level_union_all("SELECT 'UNION ALL' FROM t1 UNION ALL SELECT id FROM t2")
                .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "SELECT 'UNION ALL' FROM t1");
    }

    // ── hash_string() ───────────────────────────────────────────────

    #[test]
    fn test_hash_string_deterministic() {
        let h1 = hash_string("SELECT id FROM orders");
        let h2 = hash_string("SELECT id FROM orders");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_string_different_inputs_differ() {
        let h1 = hash_string("SELECT id FROM orders");
        let h2 = hash_string("SELECT id FROM items");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_string_empty() {
        // Should not panic; produces a valid u64
        let _ = hash_string("");
    }

    // ── resolve_delta_template() ────────────────────────────────────

    #[test]
    fn test_resolve_delta_template_single_oid() {
        let mut prev = Frontier::new();
        prev.set_source(42, "0/1000".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(42, "0/2000".to_string(), "ts".to_string());

        let template = "SELECT * FROM changes WHERE lsn > '__PGS_PREV_LSN_42__' AND lsn <= '__PGS_NEW_LSN_42__'";
        let resolved = resolve_delta_template(template, &[42], &prev, &new_f);
        assert!(resolved.contains("0/1000"));
        assert!(resolved.contains("0/2000"));
        assert!(!resolved.contains("__PGS_PREV_LSN_42__"));
        assert!(!resolved.contains("__PGS_NEW_LSN_42__"));
    }

    #[test]
    fn test_resolve_delta_template_multiple_oids() {
        let mut prev = Frontier::new();
        prev.set_source(10, "0/AA".to_string(), "ts".to_string());
        prev.set_source(20, "0/BB".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(10, "0/CC".to_string(), "ts".to_string());
        new_f.set_source(20, "0/DD".to_string(), "ts".to_string());

        let template =
            "__PGS_PREV_LSN_10__ __PGS_NEW_LSN_10__ __PGS_PREV_LSN_20__ __PGS_NEW_LSN_20__";
        let resolved = resolve_delta_template(template, &[10, 20], &prev, &new_f);
        assert_eq!(resolved, "0/AA 0/CC 0/BB 0/DD");
    }

    #[test]
    fn test_resolve_delta_template_no_placeholders() {
        let prev = Frontier::new();
        let new_f = Frontier::new();
        let resolved = resolve_delta_template("SELECT 1", &[], &prev, &new_f);
        assert_eq!(resolved, "SELECT 1");
    }

    #[test]
    fn test_resolve_delta_template_missing_oid_defaults() {
        // OID 999 not in frontier — get_lsn returns "0/0"
        let prev = Frontier::new();
        let new_f = Frontier::new();
        let resolved = resolve_delta_template("__PGS_PREV_LSN_999__", &[999], &prev, &new_f);
        assert_eq!(resolved, "0/0");
    }

    // ── is_scan_chain_tree() ────────────────────────────────────────

    #[test]
    fn test_is_scan_chain_bare_scan() {
        let s = scan(1, "t", "public", "t", &["id"]);
        assert!(is_scan_chain_tree(&s));
    }

    #[test]
    fn test_is_scan_chain_project_over_scan() {
        let s = scan(1, "t", "public", "t", &["id", "name"]);
        let p = project(vec![colref("id")], vec!["id"], s);
        assert!(is_scan_chain_tree(&p));
    }

    #[test]
    fn test_is_scan_chain_subquery_over_scan() {
        let s = scan(1, "t", "public", "t", &["id"]);
        let sq = subquery("sub", vec!["id"], s);
        assert!(is_scan_chain_tree(&sq));
    }

    #[test]
    fn test_is_scan_chain_filter_is_false() {
        // Filter is NOT part of a scan chain (see doc comment)
        let s = scan(1, "t", "public", "t", &["id", "val"]);
        let f = filter(binop(">", colref("val"), lit("10")), s);
        assert!(!is_scan_chain_tree(&f));
    }

    #[test]
    fn test_is_scan_chain_aggregate_is_false() {
        let s = scan(1, "t", "public", "t", &["id", "amount"]);
        let agg = aggregate(vec![colref("id")], vec![sum_col("amount", "total")], s);
        assert!(!is_scan_chain_tree(&agg));
    }

    #[test]
    fn test_is_scan_chain_join_is_false() {
        let l = scan(1, "t1", "public", "t1", &["id"]);
        let r = scan(2, "t2", "public", "t2", &["id"]);
        let j = inner_join(eq_cond("t1", "id", "t2", "id"), l, r);
        assert!(!is_scan_chain_tree(&j));
    }

    #[test]
    fn test_is_scan_chain_distinct_is_false() {
        let s = scan(1, "t", "public", "t", &["id"]);
        let d = distinct(s);
        assert!(!is_scan_chain_tree(&d));
    }

    // ── Cache ops: invalidate / get / is_deduplicated ───────────────

    #[test]
    fn test_cache_empty_returns_none() {
        let pgt_id = -9999;
        invalidate_delta_cache(pgt_id); // ensure clean
        assert!(get_delta_sql_template(pgt_id).is_none());
        assert!(!is_delta_deduplicated(pgt_id));
    }

    #[test]
    fn test_cache_insert_and_retrieve() {
        let pgt_id = -9998;
        let entry = CachedDeltaTemplate {
            defining_query_hash: 12345,
            delta_sql_template: "WITH cte AS (SELECT 1) SELECT * FROM cte".to_string(),
            output_columns: vec!["id".to_string()],
            source_oids: vec![42],
            is_deduplicated: true,
        };
        DELTA_TEMPLATE_CACHE.with(|cache| {
            cache.borrow_mut().insert(pgt_id, entry);
        });

        let tmpl = get_delta_sql_template(pgt_id).unwrap();
        assert!(tmpl.contains("SELECT 1"));
        assert!(is_delta_deduplicated(pgt_id));

        // Cleanup
        invalidate_delta_cache(pgt_id);
        assert!(get_delta_sql_template(pgt_id).is_none());
    }

    #[test]
    fn test_cache_invalidate_removes_entry() {
        let pgt_id = -9997;
        let entry = CachedDeltaTemplate {
            defining_query_hash: 0,
            delta_sql_template: "SELECT 1".to_string(),
            output_columns: vec![],
            source_oids: vec![],
            is_deduplicated: false,
        };
        DELTA_TEMPLATE_CACHE.with(|cache| {
            cache.borrow_mut().insert(pgt_id, entry);
        });
        assert!(get_delta_sql_template(pgt_id).is_some());

        invalidate_delta_cache(pgt_id);
        assert!(get_delta_sql_template(pgt_id).is_none());
        assert!(!is_delta_deduplicated(pgt_id));
    }

    // ── OpTree::needs_pgt_count() (unit, no PG parse) ──────────────

    #[test]
    fn test_needs_pgt_count_aggregate() {
        let s = scan(1, "t", "public", "t", &["id", "amount"]);
        let agg = aggregate(vec![colref("id")], vec![sum_col("amount", "total")], s);
        assert!(agg.needs_pgt_count());
    }

    #[test]
    fn test_needs_pgt_count_distinct() {
        let s = scan(1, "t", "public", "t", &["id"]);
        let d = distinct(s);
        assert!(d.needs_pgt_count());
    }

    #[test]
    fn test_needs_pgt_count_scan_false() {
        let s = scan(1, "t", "public", "t", &["id"]);
        assert!(!s.needs_pgt_count());
    }

    // ── is_scalar_aggregate_root() ─────────────────────────────────

    #[test]
    fn test_scalar_aggregate_root_bare() {
        let s = scan(1, "t", "public", "t", &["id", "amount"]);
        let agg = aggregate(vec![], vec![sum_col("amount", "total")], s);
        assert!(is_scalar_aggregate_root(&agg));
    }

    #[test]
    fn test_scalar_aggregate_root_with_filter() {
        let s = scan(1, "t", "public", "t", &["id", "amount"]);
        let f = filter(binop(">", colref("amount"), lit("0")), s);
        let agg = aggregate(vec![], vec![sum_col("amount", "total")], f);
        // Aggregate is root, Filter is child — scalar agg root should be true
        assert!(is_scalar_aggregate_root(&agg));
    }

    #[test]
    fn test_scalar_aggregate_root_through_project() {
        let s = scan(1, "t", "public", "t", &["id", "amount"]);
        let agg = aggregate(vec![], vec![sum_col("amount", "total")], s);
        let p = project(vec![colref("total")], vec!["revenue"], agg);
        // Project wraps the Aggregate — should see through
        assert!(is_scalar_aggregate_root(&p));
    }

    #[test]
    fn test_not_scalar_aggregate_with_group_by() {
        let s = scan(1, "t", "public", "t", &["id", "amount"]);
        let agg = aggregate(vec![colref("id")], vec![sum_col("amount", "total")], s);
        assert!(!is_scalar_aggregate_root(&agg));
    }

    #[test]
    fn test_not_scalar_aggregate_scan() {
        let s = scan(1, "t", "public", "t", &["id"]);
        assert!(!is_scalar_aggregate_root(&s));
    }
}
