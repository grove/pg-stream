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
use std::collections::HashMap;

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
}

impl DiffContext {
    /// Create a new differentiation context.
    pub fn new(prev_frontier: Frontier, new_frontier: Frontier) -> Self {
        DiffContext {
            prev_frontier,
            new_frontier,
            cte_counter: 0,
            ctes: Vec::new(),
            change_buffer_schema: pg_trickle_change_buffer_schema(),
            st_qualified_name: None,
            cte_registry: CteRegistry::default(),
            cte_delta_cache: HashMap::new(),
            use_placeholders: false,
            defining_query: None,
            st_user_columns: None,
            merge_safe_dedup: false,
            inside_semijoin: false,
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
            change_buffer_schema: "pgtrickle_changes".to_string(),
            st_qualified_name: None,
            cte_registry: CteRegistry::default(),
            cte_delta_cache: HashMap::new(),
            use_placeholders: false,
            defining_query: None,
            st_user_columns: None,
            merge_safe_dedup: false,
            inside_semijoin: false,
        }
    }

    /// Enable placeholder mode for generating cacheable SQL templates.
    pub fn with_placeholders(mut self) -> Self {
        self.use_placeholders = true;
        self
    }

    /// Get the previous LSN for a source table. In placeholder mode,
    /// returns a substitution token; otherwise returns the literal value.
    pub fn get_prev_lsn(&self, source_oid: u32) -> String {
        if self.use_placeholders {
            format!("__PGS_PREV_LSN_{source_oid}__")
        } else {
            self.prev_frontier.get_lsn(source_oid)
        }
    }

    /// Get the new (upper) LSN for a source table. In placeholder mode,
    /// returns a substitution token; otherwise returns the literal value.
    pub fn get_new_lsn(&self, source_oid: u32) -> String {
        if self.use_placeholders {
            format!("__PGS_NEW_LSN_{source_oid}__")
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
    ) -> Result<(String, Vec<String>, bool), PgTrickleError> {
        let result = self.diff_node(op)?;
        let sql = self.build_with_query(&result.cte_name);
        Ok((sql, result.columns, result.is_deduplicated))
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
    cols.iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Helper: build a comma-separated list of prefixed column references.
pub fn prefixed_col_list(prefix: &str, cols: &[String]) -> String {
    cols.iter()
        .map(|c| format!("{prefix}.{}", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(", ")
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
        assert!(result.cte_name.contains("filter"));
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
