//! Query differentiation framework.
//!
//! Traverses the operator tree bottom-up and generates SQL CTEs (Common
//! Table Expressions) for each node's delta computation.
//!
//! The differentiate() method recursively walks the OpTree, calling operator-
//! specific diff functions that add CTEs to the context. The final result
//! is a `WITH cte1 AS (...), cte2 AS (...), ... SELECT * FROM final_cte`
//! query that computes the delta.

use crate::config::pg_trickle_change_buffer_schema;
use crate::dvm::operators;
use crate::dvm::parser::{CteRegistry, OpTree};
use crate::error::PgTrickleError;
use crate::version::Frontier;
use std::collections::{HashMap, HashSet};

/// Source of delta data for scan operators.
///
/// Determines how the `Scan` operator reads change data:
/// - `ChangeBuffer`: reads from `pgtrickle_changes.changes_<oid>` tables
///   with LSN-range filtering (for DIFFERENTIAL mode).
/// - `TransitionTable`: reads from statement-level trigger transition tables
///   (`__pgt_newtable` / `__pgt_oldtable`), registered as Ephemeral Named
///   Relations (for IMMEDIATE mode).
#[derive(Debug, Clone, Default)]
pub enum DeltaSource {
    /// Deferred mode: read from change buffer tables with LSN range filtering.
    #[default]
    ChangeBuffer,
    /// Immediate mode: read from trigger transition tables.
    /// Contains the transition table name suffixes per source table OID.
    TransitionTable {
        /// Map from source table OID to the transition table names
        /// (old_table_name, new_table_name). A name is None if the
        /// operation doesn't produce that transition table (e.g., INSERT
        /// has no OLD table).
        tables: HashMap<u32, TransitionTableNames>,
    },
}

/// Names of the transition tables for a specific source table in IMMEDIATE mode.
#[derive(Debug, Clone)]
pub struct TransitionTableNames {
    /// Name of the OLD transition table (for DELETE/UPDATE). None for INSERT.
    pub old_name: Option<String>,
    /// Name of the NEW transition table (for INSERT/UPDATE). None for DELETE.
    pub new_name: Option<String>,
}

/// The result of differentiating a single operator node.
/// Contains the CTE name that holds this node's delta output.
#[derive(Debug, Clone)]
pub struct DiffResult {
    /// Name of the CTE containing this node's delta rows.
    pub cte_name: String,
    /// Column names in the delta (excludes __pgt_row_id and __pgt_action).
    pub columns: Vec<String>,
    /// When true, the delta output has at most one row per `__pgt_row_id`.
    /// The MERGE statement can skip the outer DISTINCT ON + ORDER BY.
    pub is_deduplicated: bool,
    /// A-2: When true, the delta CTE includes a `__pgt_key_changed` boolean
    /// column indicating whether any key column (GROUP BY, JOIN ON, WHERE)
    /// was modified. Downstream operators can use this signal to optimize
    /// value-only UPDATEs — e.g., skip the DELETE+INSERT cycle for
    /// invertible aggregates when only aggregate argument columns changed.
    pub has_key_changed: bool,
}

/// Context for delta query generation.
pub struct DiffContext {
    /// Frontier at the start of the change interval.
    pub prev_frontier: Frontier,
    /// Frontier at the end of the change interval.
    pub new_frontier: Frontier,
    /// Counter for generating unique CTE names.
    cte_counter: usize,
    /// Accumulated CTE definitions: `(name, sql, is_recursive, is_materialized)`.
    ctes: Vec<(String, String, bool, bool)>,
    /// CTEs that should emit `AS NOT MATERIALIZED (...)` to prevent
    /// PostgreSQL from auto-materializing them when referenced >= 2 times.
    /// Used when Part 3 correction adds a second reference to a child
    /// join delta CTE — without this, PG materializes the CTE into temp
    /// files, exhausting `temp_file_limit`.
    not_materialized_ctes: HashSet<String>,
    /// Schema for change buffer tables.
    pub change_buffer_schema: String,
    /// The target stream table's schema.qualified name (for aggregate merge).
    pub st_qualified_name: Option<String>,
    /// Registry of parsed CTE bodies (populated by the parser).
    pub cte_registry: CteRegistry,
    /// Cache of already-differentiated CTE deltas, keyed by `cte_id`.
    /// When a CTE is referenced multiple times via [`OpTree::CteScan`],
    /// the first encounter differentiates the body and stores the result
    /// here; subsequent encounters reuse it.
    cte_delta_cache: HashMap<usize, DiffResult>,
    /// When true, emit `__PGS_PREV_LSN_{oid}__` / `__PGS_NEW_LSN_{oid}__`
    /// placeholder tokens instead of literal LSN values. This allows the
    /// generated SQL to be cached and re-used across refreshes by
    /// substituting actual LSN values at execution time.
    pub use_placeholders: bool,
    /// The original defining query text, used by recursive CTE
    /// recomputation to re-execute the query directly instead of
    /// reconstructing SQL from the OpTree.
    pub defining_query: Option<String>,
    /// Columns that the ST storage table has (outer projection columns),
    /// used by recursive CTE recomputation to match the storage schema.
    pub st_user_columns: Option<Vec<String>>,
    /// When true, the top-level scan delta should produce at most one row
    /// per PK (merge D+I pairs for updates into a single I). This allows
    /// the MERGE to skip the outer DISTINCT ON + ORDER BY sort.
    ///
    /// Only set to true when the top-level operator is a scan-chain
    /// (Scan/Filter/Project — no aggregate/join/union above it).
    pub merge_safe_dedup: bool,
    /// When true, the current diff node is inside a SemiJoin or AntiJoin
    /// ancestor.  Inner joins inside a SemiJoin context must use L₁
    /// (post-change snapshot) instead of L₀ via EXCEPT ALL to avoid the
    /// Q21-type numwait regression where EXCEPT ALL at sub-join levels
    /// interacts with the SemiJoin's R_old snapshot computation.
    pub inside_semijoin: bool,
    /// Whether the stream table has a `__pgt_count` auxiliary column.
    /// True when the top-level OpTree contains Aggregate or Distinct.
    /// Used by the aggregate operator to detect intermediate aggregates
    /// (e.g., aggregates inside CTE bodies) whose output columns match
    /// the ST but whose `__pgt_count` is not stored.
    pub st_has_pgt_count: bool,
    /// Source of delta data: change buffer tables (deferred) or transition
    /// tables (immediate). Determines how the Scan operator generates SQL.
    pub delta_source: DeltaSource,
    /// Maps child column names to their corresponding ST column names.
    ///
    /// Populated by `diff_project` when a Project renames columns
    /// (e.g., `r.name AS region`). Downstream operators (like aggregate)
    /// use this to reference the correct ST column names in JOIN
    /// conditions and SELECT lists.
    pub st_column_alias_map: Option<HashMap<String, String>>,
    /// Set to `true` immediately before differentiating the child of a HAVING
    /// `Filter` node.  Consumed by `diff_aggregate` to force a full-rescan CTE
    /// that supplies correct aggregate values for groups that were below the
    /// HAVING threshold (absent from the ST) and are now crossing it upward.
    /// Reset to `false` after the child diff returns.
    pub having_filter: bool,
    /// P2-5: CDC column names per source table, ordered by `attnum`.
    ///
    /// Maps `table_oid` → ordered CDC column names (from
    /// `resolve_referenced_column_defs`). The index in this Vec corresponds
    /// to the bit position in the `changed_cols` bitmask stored by the CDC
    /// trigger. Used by `diff_scan_change_buffer` to build a bitmask filter
    /// that skips UPDATE rows where none of the referenced columns changed.
    pub source_cdc_columns: HashMap<u32, Vec<String>>,
    /// A-2: Key column names per source table.
    ///
    /// Maps `table_oid` → column names that appear in key positions
    /// (GROUP BY, JOIN ON, WHERE). Used by the scan operator to compute
    /// a key-column-only bitmask. UPDATE rows where `changed_cols & key_mask
    /// = 0` are "value-only" changes — the row stays in its group/join
    /// bucket — enabling downstream optimization.
    pub source_key_columns: HashMap<u32, Vec<String>>,
    /// P2-7: Predicate pushed down from a Filter node into the Scan.
    ///
    /// When a Filter sits directly above a Scan and the predicate only
    /// references columns from that Scan, `diff_filter` stores the
    /// predicate here instead of generating a separate filter CTE.
    /// `diff_scan_change_buffer` consumes it by injecting rewritten
    /// `WHERE c."old_col" ...` / `c."new_col" ...` clauses into the
    /// final scan CTE's DELETE/INSERT branches.
    pub scan_pushed_predicate: Option<crate::dvm::parser::Expr>,
    /// ST-ST-4: Maps storage-table OIDs to upstream pgt_ids for ST sources.
    ///
    /// When a source OID is a stream table (not a base table), the scan
    /// operator reads from `changes_pgt_{pgt_id}` instead of `changes_{oid}`
    /// and uses `pgt_`-prefixed LSN placeholder tokens.
    pub st_source_pgt_ids: HashMap<u32, i64>,
    /// DAG-4: Maps upstream pgt_id → temp bypass table name.
    ///
    /// When set (by fused-chain execution), `diff_scan_change_buffer` reads
    /// from the bypass temp table instead of the persistent change buffer.
    pub st_bypass_tables: HashMap<i64, String>,
    /// EC01B-1: Maps Scan alias → delta CTE name.
    ///
    /// Populated by `diff_scan` for each leaf Scan node during diff
    /// traversal. Used by `build_pre_change_snapshot_sql` to construct
    /// per-leaf pre-change snapshots for deep join trees (≥3 scan nodes),
    /// avoiding the expensive full-snapshot EXCEPT ALL that spills temp
    /// files. Each leaf's EXCEPT ALL operates on a single table (cheap),
    /// and the join is reconstructed from pre-change leaves.
    pub scan_delta_ctes: HashMap<String, String>,
}

impl DiffContext {
    /// Create a new differentiation context.
    pub fn new(prev_frontier: Frontier, new_frontier: Frontier) -> Self {
        DiffContext {
            prev_frontier,
            new_frontier,
            cte_counter: 0,
            ctes: Vec::new(),
            not_materialized_ctes: HashSet::new(),
            change_buffer_schema: pg_trickle_change_buffer_schema(),
            st_qualified_name: None,
            cte_registry: CteRegistry::default(),
            cte_delta_cache: HashMap::new(),
            use_placeholders: false,
            defining_query: None,
            st_user_columns: None,
            merge_safe_dedup: false,
            inside_semijoin: false,
            st_has_pgt_count: false,
            delta_source: DeltaSource::ChangeBuffer,
            st_column_alias_map: None,
            having_filter: false,
            source_cdc_columns: HashMap::new(),
            source_key_columns: HashMap::new(),
            scan_pushed_predicate: None,
            st_source_pgt_ids: HashMap::new(),
            st_bypass_tables: HashMap::new(),
            scan_delta_ctes: HashMap::new(),
        }
    }

    /// Create a DiffContext without accessing PostgreSQL GUCs.
    ///
    /// Used by unit tests and benchmarks that run outside of PostgreSQL.
    /// The `change_buffer_schema` defaults to `"pgtrickle_changes"`.
    pub fn new_standalone(prev_frontier: Frontier, new_frontier: Frontier) -> Self {
        DiffContext {
            prev_frontier,
            new_frontier,
            cte_counter: 0,
            ctes: Vec::new(),
            not_materialized_ctes: HashSet::new(),
            change_buffer_schema: "pgtrickle_changes".to_string(),
            st_qualified_name: None,
            cte_registry: CteRegistry::default(),
            cte_delta_cache: HashMap::new(),
            use_placeholders: false,
            defining_query: None,
            st_user_columns: None,
            merge_safe_dedup: false,
            inside_semijoin: false,
            st_has_pgt_count: false,
            delta_source: DeltaSource::ChangeBuffer,
            st_column_alias_map: None,
            having_filter: false,
            source_cdc_columns: HashMap::new(),
            source_key_columns: HashMap::new(),
            scan_pushed_predicate: None,
            st_source_pgt_ids: HashMap::new(),
            st_bypass_tables: HashMap::new(),
            scan_delta_ctes: HashMap::new(),
        }
    }

    /// Enable placeholder mode for generating cacheable SQL templates.
    pub fn with_placeholders(mut self) -> Self {
        self.use_placeholders = true;
        self
    }

    /// Set the delta source (change buffer vs transition tables).
    pub fn with_delta_source(mut self, ds: DeltaSource) -> Self {
        self.delta_source = ds;
        self
    }

    /// Get the previous LSN for a source table. In placeholder mode,
    /// returns a substitution token; otherwise returns the literal value.
    ///
    /// ST-ST-4: For ST sources, uses `pgt_{pgt_id}` in the token name
    /// instead of the raw OID, matching the `changes_pgt_{id}` buffer name.
    pub fn get_prev_lsn(&self, source_oid: u32) -> String {
        if self.use_placeholders {
            if let Some(&pgt_id) = self.st_source_pgt_ids.get(&source_oid) {
                format!("__PGS_PREV_LSN_pgt_{pgt_id}__")
            } else {
                format!("__PGS_PREV_LSN_{source_oid}__")
            }
        } else if let Some(&pgt_id) = self.st_source_pgt_ids.get(&source_oid) {
            // ST sources use pgt_{id} as the frontier key
            self.prev_frontier
                .sources
                .get(&format!("pgt_{pgt_id}"))
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string())
        } else {
            self.prev_frontier.get_lsn(source_oid)
        }
    }

    /// Get the new (upper) LSN for a source table. In placeholder mode,
    /// returns a substitution token; otherwise returns the literal value.
    pub fn get_new_lsn(&self, source_oid: u32) -> String {
        if self.use_placeholders {
            if let Some(&pgt_id) = self.st_source_pgt_ids.get(&source_oid) {
                format!("__PGS_NEW_LSN_pgt_{pgt_id}__")
            } else {
                format!("__PGS_NEW_LSN_{source_oid}__")
            }
        } else if let Some(&pgt_id) = self.st_source_pgt_ids.get(&source_oid) {
            self.new_frontier
                .sources
                .get(&format!("pgt_{pgt_id}"))
                .map(|sv| sv.lsn.clone())
                .unwrap_or_else(|| "0/0".to_string())
        } else {
            self.new_frontier.get_lsn(source_oid)
        }
    }

    /// Set the stream table name for aggregate merge queries.
    pub fn with_pgt_name(mut self, schema: &str, name: &str) -> Self {
        self.st_qualified_name = Some(format!(
            "\"{}\".\"{}\"",
            schema.replace('"', "\"\""),
            name.replace('"', "\"\""),
        ));
        self
    }

    /// Set the CTE registry (populated by the parser).
    pub fn with_cte_registry(mut self, registry: CteRegistry) -> Self {
        self.cte_registry = registry;
        self
    }

    /// Set the original defining query text for recursive CTE recomputation.
    pub fn with_defining_query(mut self, query: &str) -> Self {
        self.defining_query = Some(query.to_string());
        self
    }

    /// Look up a cached CTE delta result by `cte_id`.
    pub fn get_cte_delta(&self, cte_id: usize) -> Option<&DiffResult> {
        self.cte_delta_cache.get(&cte_id)
    }

    /// Cache a CTE delta result.
    pub fn set_cte_delta(&mut self, cte_id: usize, result: DiffResult) {
        self.cte_delta_cache.insert(cte_id, result);
    }

    /// Generate the complete delta query for an operator tree.
    ///
    /// Returns the final SQL `WITH ... SELECT ...` query string.
    /// The output has columns: `__pgt_row_id`, `__pgt_action`, plus user columns.
    pub fn differentiate(&mut self, op: &OpTree) -> Result<String, PgTrickleError> {
        let result = self.diff_node(op)?;
        Ok(self.build_with_query(&result.cte_name))
    }

    /// Differentiate and also return the final diff columns (includes
    /// auxiliary columns like `__pgt_count` for aggregate/distinct)
    /// and the `is_deduplicated` flag from the operator tree.
    pub fn differentiate_with_columns(
        &mut self,
        op: &OpTree,
    ) -> Result<(String, Vec<String>, bool, bool), PgTrickleError> {
        let result = self.diff_node(op)?;
        let sql = self.build_with_query(&result.cte_name);
        Ok((
            sql,
            result.columns,
            result.is_deduplicated,
            result.has_key_changed,
        ))
    }

    /// Recursively differentiate an operator tree node.
    pub fn diff_node(&mut self, op: &OpTree) -> Result<DiffResult, PgTrickleError> {
        match op {
            OpTree::Scan { .. } => operators::scan::diff_scan(self, op),
            OpTree::Filter { .. } => operators::filter::diff_filter(self, op),
            OpTree::Project { .. } => operators::project::diff_project(self, op),
            OpTree::InnerJoin { .. } => operators::join::diff_inner_join(self, op),
            OpTree::LeftJoin { .. } => operators::outer_join::diff_left_join(self, op),
            OpTree::FullJoin { .. } => operators::full_join::diff_full_join(self, op),
            OpTree::Aggregate { .. } => operators::aggregate::diff_aggregate(self, op),
            OpTree::Distinct { .. } => operators::distinct::diff_distinct(self, op),
            OpTree::UnionAll { .. } => operators::union_all::diff_union_all(self, op),
            OpTree::Intersect { .. } => operators::intersect::diff_intersect(self, op),
            OpTree::Except { .. } => operators::except::diff_except(self, op),
            OpTree::Subquery { .. } => operators::subquery::diff_subquery(self, op),
            OpTree::CteScan { .. } => operators::cte_scan::diff_cte_scan(self, op),
            OpTree::RecursiveCte { .. } => operators::recursive_cte::diff_recursive_cte(self, op),
            OpTree::RecursiveSelfRef { .. } => Err(PgTrickleError::InternalError(
                "RecursiveSelfRef encountered outside RecursiveCte diff context; \
                 this node should only appear inside a RecursiveCte's recursive term"
                    .into(),
            )),
            OpTree::Window { .. } => operators::window::diff_window(self, op),
            OpTree::LateralFunction { .. } => {
                operators::lateral_function::diff_lateral_function(self, op)
            }
            OpTree::LateralSubquery { .. } => {
                operators::lateral_subquery::diff_lateral_subquery(self, op)
            }
            OpTree::SemiJoin { .. } => operators::semi_join::diff_semi_join(self, op),
            OpTree::AntiJoin { .. } => operators::anti_join::diff_anti_join(self, op),
            OpTree::ScalarSubquery { .. } => {
                operators::scalar_subquery::diff_scalar_subquery(self, op)
            }
            OpTree::ConstantSelect { columns, .. } => {
                // A constant anchor has no source tables and never contributes
                // delta rows. Return an empty DiffResult so the recursive CTE
                // diff engine can split the base / recursive arms correctly.
                let empty_cte = self.next_cte_name("const_empty");
                let col_list = columns
                    .iter()
                    .map(|c| format!("NULL::text AS {}", quote_ident(c)))
                    .collect::<Vec<_>>()
                    .join(", ");
                // A WHERE FALSE CTE always produces zero rows — correct for a delta.
                self.add_cte(
                    empty_cte.clone(),
                    format!("SELECT 'I'::text AS __pgt_action, {col_list} WHERE FALSE"),
                );
                Ok(DiffResult {
                    cte_name: empty_cte,
                    columns: columns.clone(),
                    is_deduplicated: false,
                    has_key_changed: false,
                })
            }
        }
    }

    /// Generate a unique CTE name with a descriptive prefix.
    pub fn next_cte_name(&mut self, prefix: &str) -> String {
        self.cte_counter += 1;
        format!("__pgt_cte_{}_{}", prefix, self.cte_counter)
    }

    /// Add a CTE definition.
    pub fn add_cte(&mut self, name: String, sql: String) {
        self.ctes.push((name, sql, false, false));
    }

    /// Add a recursive CTE definition (requires `WITH RECURSIVE`).
    pub fn add_recursive_cte(&mut self, name: String, sql: String) {
        self.ctes.push((name, sql, true, false));
    }

    /// Add a `MATERIALIZED` CTE definition.
    ///
    /// Forces PostgreSQL (12+) to evaluate the CTE once and cache the
    /// result, preventing re-execution for each reference.  Used when
    /// the CTE body is expensive (e.g. EXCEPT ALL / UNION ALL set
    /// operation for R_old snapshots in semi-join / anti-join deltas).
    pub fn add_materialized_cte(&mut self, name: String, sql: String) {
        self.ctes.push((name, sql, false, true));
    }

    /// Retroactively mark an already-added CTE as `NOT MATERIALIZED`.
    ///
    /// PostgreSQL (12+) auto-materializes CTEs referenced >= 2 times.
    /// When Part 3 correction adds a second reference to a child join
    /// delta CTE, the auto-materialization can spill huge temp files.
    /// Marking the CTE as NOT MATERIALIZED forces PG to inline it as
    /// a subquery for each reference, avoiding the temp file issue.
    pub fn mark_cte_not_materialized(&mut self, name: &str) {
        self.not_materialized_ctes.insert(name.to_string());
    }

    /// Look up the SQL body of a CTE by name (test helper).
    #[cfg(test)]
    pub fn cte_sql(&self, name: &str) -> Option<&str> {
        self.ctes
            .iter()
            .find(|(n, _, _, _)| n == name)
            .map(|(_, sql, _, _)| sql.as_str())
    }

    /// Build the final WITH query from accumulated CTEs.
    pub(crate) fn build_with_query(&self, final_cte: &str) -> String {
        if self.ctes.is_empty() {
            return format!("SELECT * FROM {final_cte}");
        }

        let has_recursive = self.ctes.iter().any(|(_, _, is_rec, _)| *is_rec);
        let with_keyword = if has_recursive {
            "WITH RECURSIVE"
        } else {
            "WITH"
        };

        let cte_defs: Vec<String> = self
            .ctes
            .iter()
            .map(|(name, sql, _, is_mat)| {
                if *is_mat {
                    format!("{name} AS MATERIALIZED (\n{sql}\n)")
                } else if self.not_materialized_ctes.contains(name.as_str()) {
                    format!("{name} AS NOT MATERIALIZED (\n{sql}\n)")
                } else {
                    format!("{name} AS (\n{sql}\n)")
                }
            })
            .collect();

        format!(
            "{with_keyword} {}\nSELECT * FROM {final_cte}",
            cte_defs.join(",\n"),
        )
    }
}

/// Helper: quote a SQL identifier.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Helper: build a comma-separated list of quoted column references.
pub fn col_list(cols: &[String]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(cols.len() * 16);
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}", quote_ident(c));
    }
    out
}

/// Helper: build a comma-separated list of prefixed column references.
pub fn prefixed_col_list(prefix: &str, cols: &[String]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(cols.len() * (prefix.len() + 18));
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{prefix}.{}", quote_ident(c));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dvm::operators::test_helpers::*;

    // ── quote_ident tests ───────────────────────────────────────────

    #[test]
    fn test_quote_ident_simple() {
        assert_eq!(quote_ident("name"), "\"name\"");
    }

    #[test]
    fn test_quote_ident_with_embedded_quotes() {
        assert_eq!(quote_ident("col\"name"), "\"col\"\"name\"");
    }

    #[test]
    fn test_quote_ident_empty() {
        assert_eq!(quote_ident(""), "\"\"");
    }

    #[test]
    fn test_quote_ident_with_spaces() {
        assert_eq!(quote_ident("my column"), "\"my column\"");
    }

    #[test]
    fn test_quote_ident_already_quoted_content() {
        // If name contains double-double quotes, they get doubled again
        assert_eq!(quote_ident("a\"\"b"), "\"a\"\"\"\"b\"");
    }

    // ── col_list tests ──────────────────────────────────────────────

    #[test]
    fn test_col_list_single() {
        let cols = vec!["id".to_string()];
        assert_eq!(col_list(&cols), "\"id\"");
    }

    #[test]
    fn test_col_list_multiple() {
        let cols = vec!["id".to_string(), "name".to_string(), "amount".to_string()];
        assert_eq!(col_list(&cols), "\"id\", \"name\", \"amount\"");
    }

    #[test]
    fn test_col_list_empty() {
        let cols: Vec<String> = vec![];
        assert_eq!(col_list(&cols), "");
    }

    #[test]
    fn test_col_list_with_special_chars() {
        let cols = vec!["col\"1".to_string(), "col 2".to_string()];
        assert_eq!(col_list(&cols), "\"col\"\"1\", \"col 2\"");
    }

    // ── prefixed_col_list tests ─────────────────────────────────────

    #[test]
    fn test_prefixed_col_list_single() {
        let cols = vec!["id".to_string()];
        assert_eq!(prefixed_col_list("t", &cols), "t.\"id\"");
    }

    #[test]
    fn test_prefixed_col_list_multiple() {
        let cols = vec!["x".to_string(), "y".to_string()];
        assert_eq!(prefixed_col_list("src", &cols), "src.\"x\", src.\"y\"");
    }

    #[test]
    fn test_prefixed_col_list_empty() {
        let cols: Vec<String> = vec![];
        assert_eq!(prefixed_col_list("t", &cols), "");
    }

    // ── DiffContext::new_standalone() defaults ──────────────────────

    #[test]
    fn test_diff_context_defaults() {
        let ctx = DiffContext::new_standalone(Frontier::new(), Frontier::new());
        assert_eq!(ctx.change_buffer_schema, "pgtrickle_changes");
        assert!(ctx.st_qualified_name.is_none());
        assert!(!ctx.use_placeholders);
        assert!(!ctx.merge_safe_dedup);
        assert!(ctx.defining_query.is_none());
        assert!(ctx.st_user_columns.is_none());
    }

    #[test]
    fn test_diff_context_preserves_frontiers() {
        let mut prev = Frontier::new();
        prev.set_source(100, "0/AABB".to_string(), "2024-01-01".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(100, "0/CCDD".to_string(), "2024-01-02".to_string());

        let ctx = DiffContext::new_standalone(prev, new_f);
        assert_eq!(ctx.prev_frontier.get_lsn(100), "0/AABB");
        assert_eq!(ctx.new_frontier.get_lsn(100), "0/CCDD");
    }

    // ── with_placeholders() ─────────────────────────────────────────

    #[test]
    fn test_with_placeholders_enables_flag() {
        let ctx = DiffContext::new_standalone(Frontier::new(), Frontier::new()).with_placeholders();
        assert!(ctx.use_placeholders);
    }

    #[test]
    fn test_get_lsn_placeholder_vs_literal() {
        let mut prev = Frontier::new();
        prev.set_source(42, "0/1234".to_string(), "ts".to_string());
        let mut new_f = Frontier::new();
        new_f.set_source(42, "0/5678".to_string(), "ts".to_string());

        // With placeholders
        let ctx = DiffContext::new_standalone(prev.clone(), new_f.clone()).with_placeholders();
        assert_eq!(ctx.get_prev_lsn(42), "__PGS_PREV_LSN_42__");
        assert_eq!(ctx.get_new_lsn(42), "__PGS_NEW_LSN_42__");

        // Without placeholders — literal LSN values
        let ctx2 = DiffContext::new_standalone(prev, new_f);
        assert_eq!(ctx2.get_prev_lsn(42), "0/1234");
        assert_eq!(ctx2.get_new_lsn(42), "0/5678");
    }

    // ── next_cte_name() uniqueness ──────────────────────────────────

    #[test]
    fn test_next_cte_name_sequential() {
        let mut ctx = test_ctx();
        let n1 = ctx.next_cte_name("scan");
        let n2 = ctx.next_cte_name("scan");
        let n3 = ctx.next_cte_name("filter");
        assert_eq!(n1, "__pgt_cte_scan_1");
        assert_eq!(n2, "__pgt_cte_scan_2");
        assert_eq!(n3, "__pgt_cte_filter_3");
    }

    #[test]
    fn test_next_cte_name_all_unique() {
        let mut ctx = test_ctx();
        let mut names = std::collections::HashSet::new();
        for _ in 0..100 {
            let name = ctx.next_cte_name("x");
            assert!(names.insert(name), "Duplicate CTE name generated");
        }
    }

    // ── add_cte() + build_with_query() ──────────────────────────────

    #[test]
    fn test_build_with_query_no_ctes() {
        let ctx = test_ctx();
        let sql = ctx.build_with_query("final");
        assert_eq!(sql, "SELECT * FROM final");
    }

    #[test]
    fn test_build_with_query_single_cte() {
        let mut ctx = test_ctx();
        ctx.add_cte(
            "__pgt_cte_scan_1".to_string(),
            "SELECT id FROM t".to_string(),
        );
        let sql = ctx.build_with_query("__pgt_cte_scan_1");
        assert!(sql.starts_with("WITH "));
        assert!(sql.contains("__pgt_cte_scan_1 AS (\nSELECT id FROM t\n)"));
        assert!(sql.ends_with("SELECT * FROM __pgt_cte_scan_1"));
    }

    #[test]
    fn test_build_with_query_multiple_ctes() {
        let mut ctx = test_ctx();
        ctx.add_cte("cte_a".to_string(), "SELECT 1".to_string());
        ctx.add_cte("cte_b".to_string(), "SELECT * FROM cte_a".to_string());
        let sql = ctx.build_with_query("cte_b");
        assert!(sql.contains("cte_a AS ("));
        assert!(sql.contains("cte_b AS ("));
        assert!(sql.contains("),\n"));
        assert!(sql.ends_with("SELECT * FROM cte_b"));
    }

    // ── add_recursive_cte() ─────────────────────────────────────────

    #[test]
    fn test_recursive_cte_uses_with_recursive() {
        let mut ctx = test_ctx();
        ctx.add_recursive_cte(
            "rec_cte".to_string(),
            "SELECT 1 UNION ALL SELECT n+1 FROM rec_cte WHERE n < 10".to_string(),
        );
        let sql = ctx.build_with_query("rec_cte");
        assert!(
            sql.starts_with("WITH RECURSIVE"),
            "Expected WITH RECURSIVE, got: {sql}",
        );
    }

    #[test]
    fn test_mix_recursive_and_non_recursive_ctes() {
        let mut ctx = test_ctx();
        ctx.add_cte("plain".to_string(), "SELECT 1".to_string());
        ctx.add_recursive_cte(
            "rec".to_string(),
            "SELECT 1 UNION ALL SELECT n+1 FROM rec".to_string(),
        );
        let sql = ctx.build_with_query("rec");
        assert!(sql.starts_with("WITH RECURSIVE"));
        assert!(sql.contains("plain AS ("));
        assert!(sql.contains("rec AS ("));
    }

    // ── with_pgt_name() ──────────────────────────────────────────────

    #[test]
    fn test_with_pgt_name_sets_qualified_name() {
        let ctx = DiffContext::new_standalone(Frontier::new(), Frontier::new())
            .with_pgt_name("myschema", "my_st");
        assert_eq!(
            ctx.st_qualified_name.as_deref(),
            Some("\"myschema\".\"my_st\""),
        );
    }

    #[test]
    fn test_with_pgt_name_escapes_quotes() {
        let ctx = DiffContext::new_standalone(Frontier::new(), Frontier::new())
            .with_pgt_name("sch\"ema", "ta\"ble");
        assert_eq!(
            ctx.st_qualified_name.as_deref(),
            Some("\"sch\"\"ema\".\"ta\"\"ble\""),
        );
    }

    // ── CTE delta cache ─────────────────────────────────────────────

    #[test]
    fn test_cte_delta_cache_set_and_get() {
        let mut ctx = test_ctx();
        assert!(ctx.get_cte_delta(0).is_none());

        let result = DiffResult {
            cte_name: "cte_1".to_string(),
            columns: vec!["id".to_string()],
            is_deduplicated: true,
            has_key_changed: false,
        };
        ctx.set_cte_delta(0, result);
        let cached = ctx.get_cte_delta(0).unwrap();
        assert_eq!(cached.cte_name, "cte_1");
        assert!(cached.is_deduplicated);
    }

    // ── diff_node() dispatch ────────────────────────────────────────

    #[test]
    fn test_diff_node_scan_produces_result() {
        let mut ctx = test_ctx();
        let s = scan_with_pk(1, "orders", "public", "orders", &["id", "amount"], &["id"]);
        let result = ctx.diff_node(&s).unwrap();
        assert!(result.cte_name.contains("scan"));
        assert!(result.columns.contains(&"id".to_string()));
        assert!(result.columns.contains(&"amount".to_string()));
    }

    #[test]
    fn test_diff_node_filter_dispatches() {
        let mut ctx = test_ctx();
        let s = scan_with_pk(1, "t", "public", "t", &["id", "val"], &["id"]);
        let pred = binop(">", colref("val"), lit("10"));
        let f = filter(pred, s);
        let result = ctx.diff_node(&f).unwrap();
        // P2-7: predicate is pushed into the scan CTE, so the result
        // comes from the scan operator rather than a separate filter CTE.
        assert!(
            result.cte_name.contains("scan") || result.cte_name.contains("filter"),
            "expected scan (pushdown) or filter CTE, got: {}",
            result.cte_name,
        );
    }

    #[test]
    fn test_diff_node_recursive_self_ref_errors() {
        let mut ctx = test_ctx();
        let self_ref = OpTree::RecursiveSelfRef {
            cte_name: "rec".to_string(),
            alias: "rec".to_string(),
            columns: vec!["x".to_string()],
        };
        let err = ctx.diff_node(&self_ref).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("RecursiveSelfRef"),
            "Error should mention RecursiveSelfRef: {msg}",
        );
    }

    // ── differentiate() end-to-end ──────────────────────────────────

    #[test]
    fn test_differentiate_simple_scan() {
        let mut ctx = test_ctx();
        let s = scan_with_pk(1, "items", "public", "items", &["id", "name"], &["id"]);
        let sql = ctx.differentiate(&s).unwrap();
        assert!(sql.contains("WITH"), "Expected WITH clause: {sql}");
        assert!(
            sql.contains("SELECT * FROM"),
            "Expected SELECT * FROM: {sql}"
        );
    }

    #[test]
    fn test_differentiate_filter_over_scan() {
        let mut ctx = test_ctx();
        let s = scan_with_pk(1, "t", "public", "t", &["id", "status"], &["id"]);
        let pred = binop("=", colref("status"), lit("'active'"));
        let f = filter(pred, s);
        let sql = ctx.differentiate(&f).unwrap();
        assert!(sql.contains("WITH"));
        assert!(
            sql.contains("status") && sql.contains("active"),
            "Filter predicate should appear: {sql}",
        );
    }

    #[test]
    fn test_differentiate_project_over_scan() {
        let mut ctx = test_ctx();
        let s = scan_with_pk(1, "t", "public", "t", &["id", "x", "y"], &["id"]);
        let p = project(
            vec![colref("id"), binop("+", colref("x"), colref("y"))],
            vec!["id", "total"],
            s,
        );
        let sql = ctx.differentiate(&p).unwrap();
        assert!(sql.contains("WITH"));
        assert!(sql.contains("SELECT * FROM"));
    }

    // ── with_defining_query() ───────────────────────────────────────

    #[test]
    fn test_with_defining_query_stores_text() {
        let ctx = DiffContext::new_standalone(Frontier::new(), Frontier::new())
            .with_defining_query("SELECT 1 FROM t");
        assert_eq!(ctx.defining_query.as_deref(), Some("SELECT 1 FROM t"));
    }

    // ── with_cte_registry() ─────────────────────────────────────────

    #[test]
    fn test_with_cte_registry() {
        let reg = CteRegistry::default();
        let ctx =
            DiffContext::new_standalone(Frontier::new(), Frontier::new()).with_cte_registry(reg);
        assert!(ctx.cte_registry.get(0).is_none());
    }
}

/// Public test helpers for property tests and integration tests.
///
/// These expose internal aggregate merge/delta functions so external
/// tests can verify invariants without needing a PostgreSQL backend.
pub mod test_helpers {
    use crate::dvm::operators::aggregate::{agg_delta_exprs, agg_merge_expr};
    use crate::dvm::parser::AggExpr;

    /// Wrapper around `agg_merge_expr` for external property tests.
    pub fn agg_merge_expr_for_test(agg: &AggExpr, has_rescan: bool) -> String {
        agg_merge_expr(agg, has_rescan)
    }

    /// Wrapper around `agg_delta_exprs` for external property tests.
    pub fn agg_delta_exprs_for_test(agg: &AggExpr, child_cols: &[String]) -> (String, String) {
        agg_delta_exprs(agg, child_cols)
    }
}
