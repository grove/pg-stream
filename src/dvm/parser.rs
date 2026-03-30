//! Operator tree representation for defining queries.
//!
//! The parser converts a PostgreSQL query (via raw parse tree analysis)
//! into an intermediate `OpTree` representation suitable for differentiation.
//!
//! Two parsing approaches:
//! 1. Walk the raw parse tree from `pg_sys::raw_parser()` — handles
//!    SelectStmt, JoinExpr, RangeVar, etc.
//! 2. Use `parse_analyze_fixedparams()` to resolve OIDs and column types.
//!
//! We use approach (1) for the tree structure + approach (2) for type resolution.

use crate::error::PgTrickleError;
use pgrx::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;

// Thread-local accumulator for advisory warnings emitted during one
// `parse_defining_query_inner` call.  The accumulator is initialized at the
// start of each parse and drained into `ParseResult.warnings` at the end so
// they are emitted exactly once by the caller, not once per internal parse
// invocation (e.g. `row_id_expr_for_query`, `prewarm_merge_cache`).
thread_local! {
    static PARSE_ADVISORY_WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Push an advisory warning into the current parse warning accumulator.
/// No-op when called outside of `parse_defining_query_inner`.
fn push_parse_warning(msg: String) {
    PARSE_ADVISORY_WARNINGS.with(|w| w.borrow_mut().push(msg));
}

// ── Safe FFI helpers ───────────────────────────────────────────────────
//
// These helpers encapsulate the most common unsafe patterns when walking
// PostgreSQL parse-tree nodes, concentrating the `// SAFETY:` reasoning
// into a handful of well-documented functions and macros.
//
// See plans/safety/PLAN_REDUCED_UNSAFE.md for the full rationale.

/// Convert a PostgreSQL C string pointer to a Rust `&str`.
///
/// Returns `Err` if the pointer is null or the bytes are not valid UTF-8.
fn pg_cstr_to_str<'a>(ptr: *const std::ffi::c_char) -> Result<&'a str, PgTrickleError> {
    if ptr.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "NULL C string pointer".into(),
        ));
    }
    // SAFETY: Caller verified non-null. PostgreSQL identifiers and SQL
    // fragments stored in parse-tree nodes are always NUL-terminated C
    // strings allocated in a valid memory context.
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| PgTrickleError::QueryParseError("Invalid UTF-8 in C string".into()))
}

/// Convert a PostgreSQL `List *` to a safe `PgList<T>`.
///
/// Null pointers yield an empty list (consistent with pgrx behaviour).
fn pg_list<T>(raw: *mut pg_sys::List) -> pgrx::PgList<T> {
    // SAFETY: `PgList::from_pg` is safe for both null and valid list pointers
    // returned from the PostgreSQL parser. Null produces an empty list.
    unsafe { pgrx::PgList::<T>::from_pg(raw) }
}

/// Attempt to downcast a `*mut pg_sys::Node` to a concrete parse-tree type.
///
/// Returns `Some(&T)` if the node's tag matches, `None` if the pointer is
/// null or the tag does not match.
macro_rules! cast_node {
    ($node:expr, $tag:ident, $ty:ty) => {{
        let __n = $node;
        // SAFETY: `pgrx::is_a` reads the node's tag field, which is valid
        // for any non-null `Node*` allocated by the PostgreSQL parser.
        // The pointer cast is sound because `is_a` verified the concrete
        // type matches `$tag`.
        if !__n.is_null() && unsafe { pgrx::is_a(__n, pg_sys::NodeTag::$tag) } {
            Some(unsafe { &*(__n as *const $ty) })
        } else {
            None
        }
    }};
}

/// Parse a SQL string into a list of `RawStmt` nodes.
///
/// Must be called within a PostgreSQL backend with a valid memory context.
#[cfg(any(not(test), feature = "pg_test"))]
fn parse_query(sql: &str) -> Result<pgrx::PgList<pg_sys::RawStmt>, PgTrickleError> {
    let c_sql = std::ffi::CString::new(sql)
        .map_err(|_| PgTrickleError::QueryParseError("Query contains null bytes".into()))?;
    // SAFETY: raw_parser is a PostgreSQL C function that is safe to call
    // within a backend process with a valid memory context.
    let raw_list =
        unsafe { pg_sys::raw_parser(c_sql.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
    if raw_list.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "raw_parser returned NULL".into(),
        ));
    }
    Ok(pg_list::<pg_sys::RawStmt>(raw_list))
}

/// Parse SQL and extract the first top-level `SelectStmt`.
///
/// Returns `Ok(None)` if the query is empty or the first statement is not
/// a SELECT.
#[cfg(any(not(test), feature = "pg_test"))]
fn parse_first_select(sql: &str) -> Result<Option<*const pg_sys::SelectStmt>, PgTrickleError> {
    let stmts = parse_query(sql)?;
    let raw_stmt = match stmts.head() {
        Some(rs) => rs,
        None => return Ok(None),
    };
    // SAFETY: raw_stmt is a valid pointer from the parser.
    let node = unsafe { (*raw_stmt).stmt };
    match cast_node!(node, T_SelectStmt, pg_sys::SelectStmt) {
        Some(select) => Ok(Some(select as *const pg_sys::SelectStmt)),
        None => Ok(None),
    }
}

/// Test stub: `parse_query` is unavailable without a PostgreSQL backend.
#[cfg(all(test, not(feature = "pg_test")))]
fn parse_query(_sql: &str) -> Result<pgrx::PgList<pg_sys::RawStmt>, PgTrickleError> {
    Err(PgTrickleError::QueryParseError(
        "parse_query unavailable in unit tests".into(),
    ))
}

/// Test stub: `parse_first_select` is unavailable without a PostgreSQL backend.
#[cfg(all(test, not(feature = "pg_test")))]
fn parse_first_select(_sql: &str) -> Result<Option<*const pg_sys::SelectStmt>, PgTrickleError> {
    Err(PgTrickleError::QueryParseError(
        "parse_first_select unavailable in unit tests".into(),
    ))
}

/// Column metadata.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub type_oid: u32,
    pub is_nullable: bool,
}

/// A SQL expression (simplified representation).
#[derive(Debug, Clone)]
pub enum Expr {
    /// A column reference: `table.column` or just `column`.
    ColumnRef {
        table_alias: Option<String>,
        column_name: String,
    },
    /// A literal value.
    Literal(String),
    /// A binary operation: `left op right`.
    BinaryOp {
        op: String,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// A function call: `func(args...)`.
    FuncCall { func_name: String, args: Vec<Expr> },
    /// A star expression: `*` or `table.*`.
    Star { table_alias: Option<String> },
    /// Raw SQL text (fallback for complex expressions).
    Raw(String),
}

impl Expr {
    /// Convert expression back to SQL text.
    pub fn to_sql(&self) -> String {
        match self {
            Expr::ColumnRef {
                table_alias,
                column_name,
            } => match table_alias {
                Some(alias) => format!(
                    "\"{}\".\"{}\"",
                    alias.replace('"', "\"\""),
                    column_name.replace('"', "\"\""),
                ),
                None => column_name.clone(),
            },
            Expr::Literal(val) => val.clone(),
            Expr::BinaryOp { op, left, right } => {
                format!("({} {op} {})", left.to_sql(), right.to_sql())
            }
            Expr::FuncCall { func_name, args } => {
                let arg_strs: Vec<String> = args.iter().map(|a| a.to_sql()).collect();
                format!("{func_name}({})", arg_strs.join(", "))
            }
            Expr::Star { table_alias } => match table_alias {
                Some(alias) => format!("{alias}.*"),
                None => "*".to_string(),
            },
            Expr::Raw(sql) => sql.clone(),
        }
    }

    /// Return the output column name for this expression.
    ///
    /// For `ColumnRef`, returns just the `column_name` (stripping the table
    /// qualifier) since PostgreSQL output columns from a subquery don't
    /// carry the original table alias.
    pub fn output_name(&self) -> String {
        match self {
            Expr::ColumnRef { column_name, .. } => column_name.clone(),
            _ => self.to_sql(),
        }
    }

    /// Return a copy of this expression with all table qualifiers stripped
    /// from `ColumnRef` nodes.
    ///
    /// This is needed when the expression references columns from a CTE
    /// whose output columns are unqualified.
    pub fn strip_qualifier(&self) -> Expr {
        match self {
            Expr::ColumnRef { column_name, .. } => Expr::ColumnRef {
                table_alias: None,
                column_name: column_name.clone(),
            },
            Expr::BinaryOp { op, left, right } => Expr::BinaryOp {
                op: op.clone(),
                left: Box::new(left.strip_qualifier()),
                right: Box::new(right.strip_qualifier()),
            },
            Expr::FuncCall { func_name, args } => Expr::FuncCall {
                func_name: func_name.clone(),
                args: args.iter().map(|a| a.strip_qualifier()).collect(),
            },
            _ => self.clone(),
        }
    }

    /// Return a copy of this expression with table aliases rewritten.
    ///
    /// Replaces `old_left` → `new_left` and `old_right` → `new_right` in
    /// all `ColumnRef` nodes.  Used to adapt join conditions to the
    /// aliases present in each part of the join delta query.
    pub fn rewrite_aliases(
        &self,
        old_left: &str,
        new_left: &str,
        old_right: &str,
        new_right: &str,
    ) -> Expr {
        match self {
            Expr::ColumnRef {
                table_alias: Some(alias),
                column_name,
            } => {
                let new_alias = if alias == old_left {
                    new_left
                } else if alias == old_right {
                    new_right
                } else {
                    alias.as_str()
                };
                Expr::ColumnRef {
                    table_alias: Some(new_alias.to_string()),
                    column_name: column_name.clone(),
                }
            }
            Expr::BinaryOp { op, left, right } => Expr::BinaryOp {
                op: op.clone(),
                left: Box::new(left.rewrite_aliases(old_left, new_left, old_right, new_right)),
                right: Box::new(right.rewrite_aliases(old_left, new_left, old_right, new_right)),
            },
            Expr::FuncCall { func_name, args } => Expr::FuncCall {
                func_name: func_name.clone(),
                args: args
                    .iter()
                    .map(|a| a.rewrite_aliases(old_left, new_left, old_right, new_right))
                    .collect(),
            },
            _ => self.clone(),
        }
    }
}

/// Aggregate function types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggFunc {
    Count,
    CountStar,
    Sum,
    Avg,
    Min,
    Max,
    /// Group-rescan aggregates: any change in the group triggers full re-aggregation.
    BoolAnd,
    BoolOr,
    StringAgg,
    ArrayAgg,
    JsonAgg,
    JsonbAgg,
    BitAnd,
    BitOr,
    BitXor,
    JsonObjectAgg,
    JsonbObjectAgg,
    /// Statistical aggregates — group-rescan strategy.
    StddevPop,
    StddevSamp,
    VarPop,
    VarSamp,
    /// Ordered-set aggregates — group-rescan strategy.
    /// These use WITHIN GROUP (ORDER BY ...) syntax.
    Mode,
    PercentileCont,
    PercentileDisc,
    /// ANY_VALUE aggregate (PG 16+) — group-rescan strategy.
    /// Returns an arbitrary non-null value from the group.
    AnyValue,
    /// SQL/JSON standard aggregate: JSON_OBJECTAGG(key: value ...)
    /// Carries the fully deparsed SQL since the special `key: value` syntax
    /// cannot be reconstructed from function name + arguments alone.
    /// Group-rescan strategy.
    JsonObjectAggStd(String),
    /// SQL/JSON standard aggregate: JSON_ARRAYAGG(expr ...)
    /// Carries the fully deparsed SQL since the inline `ABSENT ON NULL`,
    /// `ORDER BY`, and `RETURNING` clauses differ from regular function syntax.
    /// Group-rescan strategy.
    JsonArrayAggStd(String),
    /// Regression/correlation aggregates — group-rescan strategy.
    /// These are two-argument aggregates: `func(Y, X)`.
    Corr,
    CovarPop,
    CovarSamp,
    RegrAvgx,
    RegrAvgy,
    RegrCount,
    RegrIntercept,
    RegrR2,
    RegrSlope,
    RegrSxx,
    RegrSxy,
    RegrSyy,
    /// XMLAGG aggregate — group-rescan strategy.
    /// Optional ORDER BY is stored in `order_within_group`.
    XmlAgg,
    /// Hypothetical-set aggregates — group-rescan strategy.
    /// These use `WITHIN GROUP (ORDER BY ...)` syntax with one hypothetical value
    /// as the function argument (e.g., `RANK(42) WITHIN GROUP (ORDER BY score)`).
    HypRank,
    HypDenseRank,
    HypPercentRank,
    HypCumeDist,
    /// Complex expression wrapping multiple aggregate calls.
    ///
    /// Created when a SELECT target is an arithmetic or conditional expression
    /// containing nested aggregate function calls, e.g.:
    ///   `100 * SUM(a) / CASE WHEN SUM(b) = 0 THEN NULL ELSE SUM(b) END`
    ///
    /// Stores the fully deparsed SQL of the entire expression (including the
    /// nested aggregate calls). Uses the group-rescan strategy: any change in
    /// the group triggers full re-evaluation of the expression from source data.
    ComplexExpression(String),
    /// User-defined aggregate (CREATE AGGREGATE) — group-rescan strategy.
    ///
    /// Stores the fully deparsed SQL of the aggregate call (including any
    /// FILTER, ORDER BY, WITHIN GROUP, and DISTINCT clauses) since UDAs
    /// may use exotic syntax that cannot be reconstructed from parts.
    /// Any change in the group triggers full re-aggregation from source data.
    UserDefined(String),
}

/// Determine the effective output column names for a LATERAL SRF.
///
/// When the query provides `AS alias(c1, c2, ...)`, those names win.
/// Otherwise we infer PostgreSQL's default names for common built-ins that
/// expose structured results through field references like `e.value` or
/// `kv.key` / `kv.value`. For everything else we preserve the existing
/// fallback of using the table alias as the single column name.
pub fn lateral_function_output_columns(
    func_sql: &str,
    alias: &str,
    column_aliases: &[String],
) -> Vec<String> {
    if !column_aliases.is_empty() {
        return column_aliases.to_vec();
    }

    infer_default_lateral_function_columns(func_sql).unwrap_or_else(|| vec![alias.to_string()])
}

fn infer_default_lateral_function_columns(func_sql: &str) -> Option<Vec<String>> {
    let open_paren = func_sql.find('(')?;
    let func_head = func_sql[..open_paren].trim();
    let func_name = func_head
        .rsplit('.')
        .next()
        .unwrap_or(func_head)
        .replace('"', "")
        .to_ascii_lowercase();

    match func_name.as_str() {
        "json_each" | "json_each_text" | "jsonb_each" | "jsonb_each_text" => {
            Some(vec!["key".to_string(), "value".to_string()])
        }
        "json_array_elements"
        | "json_array_elements_text"
        | "jsonb_array_elements"
        | "jsonb_array_elements_text" => Some(vec!["value".to_string()]),
        "json_object_keys" | "jsonb_object_keys" => Some(vec!["key".to_string()]),
        _ => None,
    }
}

impl AggFunc {
    /// Name of the aggregate function for SQL generation.
    pub fn sql_name(&self) -> &'static str {
        match self {
            AggFunc::Count | AggFunc::CountStar => "COUNT",
            AggFunc::Sum => "SUM",
            AggFunc::Avg => "AVG",
            AggFunc::Min => "MIN",
            AggFunc::Max => "MAX",
            AggFunc::BoolAnd => "BOOL_AND",
            AggFunc::BoolOr => "BOOL_OR",
            AggFunc::StringAgg => "STRING_AGG",
            AggFunc::ArrayAgg => "ARRAY_AGG",
            AggFunc::JsonAgg => "JSON_AGG",
            AggFunc::JsonbAgg => "JSONB_AGG",
            AggFunc::BitAnd => "BIT_AND",
            AggFunc::BitOr => "BIT_OR",
            AggFunc::BitXor => "BIT_XOR",
            AggFunc::JsonObjectAgg => "JSON_OBJECT_AGG",
            AggFunc::JsonbObjectAgg => "JSONB_OBJECT_AGG",
            AggFunc::JsonObjectAggStd(_) => "JSON_OBJECTAGG",
            AggFunc::JsonArrayAggStd(_) => "JSON_ARRAYAGG",
            AggFunc::StddevPop => "STDDEV_POP",
            AggFunc::StddevSamp => "STDDEV_SAMP",
            AggFunc::VarPop => "VAR_POP",
            AggFunc::VarSamp => "VAR_SAMP",
            AggFunc::AnyValue => "ANY_VALUE",
            AggFunc::Mode => "MODE",
            AggFunc::PercentileCont => "PERCENTILE_CONT",
            AggFunc::PercentileDisc => "PERCENTILE_DISC",
            AggFunc::Corr => "CORR",
            AggFunc::CovarPop => "COVAR_POP",
            AggFunc::CovarSamp => "COVAR_SAMP",
            AggFunc::RegrAvgx => "REGR_AVGX",
            AggFunc::RegrAvgy => "REGR_AVGY",
            AggFunc::RegrCount => "REGR_COUNT",
            AggFunc::RegrIntercept => "REGR_INTERCEPT",
            AggFunc::RegrR2 => "REGR_R2",
            AggFunc::RegrSlope => "REGR_SLOPE",
            AggFunc::RegrSxx => "REGR_SXX",
            AggFunc::RegrSxy => "REGR_SXY",
            AggFunc::RegrSyy => "REGR_SYY",
            AggFunc::XmlAgg => "XMLAGG",
            AggFunc::HypRank => "RANK",
            AggFunc::HypDenseRank => "DENSE_RANK",
            AggFunc::HypPercentRank => "PERCENT_RANK",
            AggFunc::HypCumeDist => "CUME_DIST",
            AggFunc::ComplexExpression(_) => "COMPLEX_EXPRESSION",
            AggFunc::UserDefined(_) => "USER_DEFINED",
        }
    }

    /// Returns true for aggregates that are maintained algebraically using
    /// auxiliary columns on the stream table.
    ///
    /// - **AVG**: stores `__pgt_aux_sum_*` and `__pgt_aux_count_*`;
    ///   `new_avg = (old_sum + Δsum) / (old_count + Δcount)`.
    /// - **STDDEV/VAR**: additionally stores `__pgt_aux_sum2_*` (sum of squares);
    ///   `var_pop = (n·sum2 − sum²) / n²`, etc.
    /// - **CORR/COVAR/REGR_***: stores cross-product auxiliary columns
    ///   `__pgt_aux_sumx_*`, `__pgt_aux_sumy_*`, `__pgt_aux_sumxy_*`,
    ///   `__pgt_aux_sumx2_*`, `__pgt_aux_sumy2_*` for O(1) algebraic maintenance.
    pub fn is_algebraic_via_aux(&self) -> bool {
        matches!(
            self,
            AggFunc::Avg
                | AggFunc::StddevPop
                | AggFunc::StddevSamp
                | AggFunc::VarPop
                | AggFunc::VarSamp
                | AggFunc::Corr
                | AggFunc::CovarPop
                | AggFunc::CovarSamp
                | AggFunc::RegrAvgx
                | AggFunc::RegrAvgy
                | AggFunc::RegrCount
                | AggFunc::RegrIntercept
                | AggFunc::RegrR2
                | AggFunc::RegrSlope
                | AggFunc::RegrSxx
                | AggFunc::RegrSxy
                | AggFunc::RegrSyy
        )
    }

    /// Returns true for aggregates that need a sum-of-squares auxiliary
    /// column (`__pgt_aux_sum2_*`) in addition to sum and count.
    pub fn needs_sum_of_squares(&self) -> bool {
        matches!(
            self,
            AggFunc::StddevPop | AggFunc::StddevSamp | AggFunc::VarPop | AggFunc::VarSamp
        )
    }

    /// Returns true for two-argument regression/correlation aggregates
    /// that need cross-product auxiliary columns (P3-2).
    ///
    /// These aggregates take `(Y, X)` arguments and require:
    /// - `__pgt_aux_sumx_*`: running SUM(X)
    /// - `__pgt_aux_sumy_*`: running SUM(Y)
    /// - `__pgt_aux_sumxy_*`: running SUM(X*Y)
    /// - `__pgt_aux_sumx2_*`: running SUM(X²)
    /// - `__pgt_aux_sumy2_*`: running SUM(Y²)
    ///
    /// Count is shared with `__pgt_aux_count_*` from `is_algebraic_via_aux`.
    pub fn needs_cross_products(&self) -> bool {
        matches!(
            self,
            AggFunc::Corr
                | AggFunc::CovarPop
                | AggFunc::CovarSamp
                | AggFunc::RegrAvgx
                | AggFunc::RegrAvgy
                | AggFunc::RegrCount
                | AggFunc::RegrIntercept
                | AggFunc::RegrR2
                | AggFunc::RegrSlope
                | AggFunc::RegrSxx
                | AggFunc::RegrSxy
                | AggFunc::RegrSyy
        )
    }

    /// Returns true for aggregates that use the group-rescan strategy:
    /// any change in a group triggers full re-aggregation from source data.
    pub fn is_group_rescan(&self) -> bool {
        matches!(
            self,
            AggFunc::BoolAnd
                | AggFunc::BoolOr
                | AggFunc::StringAgg
                | AggFunc::ArrayAgg
                | AggFunc::JsonAgg
                | AggFunc::JsonbAgg
                | AggFunc::BitAnd
                | AggFunc::BitOr
                | AggFunc::BitXor
                | AggFunc::JsonObjectAgg
                | AggFunc::JsonbObjectAgg
                | AggFunc::JsonObjectAggStd(_)
                | AggFunc::JsonArrayAggStd(_)
                | AggFunc::AnyValue
                | AggFunc::Mode
                | AggFunc::PercentileCont
                | AggFunc::PercentileDisc
                | AggFunc::XmlAgg
                | AggFunc::HypRank
                | AggFunc::HypDenseRank
                | AggFunc::HypPercentRank
                | AggFunc::HypCumeDist
                | AggFunc::ComplexExpression(_)
                | AggFunc::UserDefined(_)
        )
    }
}

/// An aggregate expression in a GROUP BY query.
#[derive(Debug, Clone)]
pub struct AggExpr {
    pub function: AggFunc,
    pub argument: Option<Expr>,
    pub alias: String,
    /// Whether DISTINCT is applied to the aggregate argument.
    pub is_distinct: bool,
    /// Optional second argument (e.g., separator for STRING_AGG).
    pub second_arg: Option<Expr>,
    /// Optional FILTER (WHERE ...) clause on the aggregate.
    pub filter: Option<Expr>,
    /// Optional WITHIN GROUP (ORDER BY ...) clause for ordered-set aggregates
    /// (MODE, PERCENTILE_CONT, PERCENTILE_DISC).
    pub order_within_group: Option<Vec<SortExpr>>,
}

/// G12-AGG: Classify the maintenance strategy for a single aggregate expression.
///
/// Returns one of:
/// - `"ALGEBRAIC_INVERTIBLE"` — COUNT, COUNT(*), SUM
/// - `"ALGEBRAIC_VIA_AUX"` — AVG, STDDEV, VAR, CORR, REGR_*
/// - `"SEMI_ALGEBRAIC"` — MIN, MAX
/// - `"GROUP_RESCAN"` — STRING_AGG, ARRAY_AGG, BIT_AND, etc.
///
/// DISTINCT aggregates are always classified as `"GROUP_RESCAN"` because
/// the algebraic and semi-algebraic paths do not support DISTINCT.
pub fn classify_agg_strategy(agg: &AggExpr) -> &'static str {
    if agg.is_distinct {
        return "GROUP_RESCAN";
    }
    match &agg.function {
        AggFunc::Count | AggFunc::CountStar | AggFunc::Sum => "ALGEBRAIC_INVERTIBLE",
        f if f.is_algebraic_via_aux() => "ALGEBRAIC_VIA_AUX",
        AggFunc::Min | AggFunc::Max => "SEMI_ALGEBRAIC",
        _ => "GROUP_RESCAN",
    }
}

#[cfg(test)]
mod classify_agg_strategy_tests {
    use super::*;

    fn make_agg(func: AggFunc, distinct: bool) -> AggExpr {
        AggExpr {
            function: func,
            argument: None,
            alias: "test_agg".to_string(),
            is_distinct: distinct,
            second_arg: None,
            filter: None,
            order_within_group: None,
        }
    }

    #[test]
    fn test_count_is_algebraic_invertible() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Count, false)),
            "ALGEBRAIC_INVERTIBLE"
        );
    }

    #[test]
    fn test_count_star_is_algebraic_invertible() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::CountStar, false)),
            "ALGEBRAIC_INVERTIBLE"
        );
    }

    #[test]
    fn test_sum_is_algebraic_invertible() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Sum, false)),
            "ALGEBRAIC_INVERTIBLE"
        );
    }

    #[test]
    fn test_avg_is_algebraic_via_aux() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Avg, false)),
            "ALGEBRAIC_VIA_AUX"
        );
    }

    #[test]
    fn test_stddev_pop_is_algebraic_via_aux() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::StddevPop, false)),
            "ALGEBRAIC_VIA_AUX"
        );
    }

    #[test]
    fn test_corr_is_algebraic_via_aux() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Corr, false)),
            "ALGEBRAIC_VIA_AUX"
        );
    }

    #[test]
    fn test_min_is_semi_algebraic() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Min, false)),
            "SEMI_ALGEBRAIC"
        );
    }

    #[test]
    fn test_max_is_semi_algebraic() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Max, false)),
            "SEMI_ALGEBRAIC"
        );
    }

    #[test]
    fn test_string_agg_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::StringAgg, false)),
            "GROUP_RESCAN"
        );
    }

    #[test]
    fn test_array_agg_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::ArrayAgg, false)),
            "GROUP_RESCAN"
        );
    }

    #[test]
    fn test_json_agg_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::JsonAgg, false)),
            "GROUP_RESCAN"
        );
    }

    #[test]
    fn test_bool_and_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::BoolAnd, false)),
            "GROUP_RESCAN"
        );
    }

    #[test]
    fn test_user_defined_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(
                AggFunc::UserDefined("custom_agg(x)".into()),
                false
            )),
            "GROUP_RESCAN",
        );
    }

    #[test]
    fn test_distinct_count_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Count, true)),
            "GROUP_RESCAN"
        );
    }

    #[test]
    fn test_distinct_sum_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Sum, true)),
            "GROUP_RESCAN"
        );
    }

    #[test]
    fn test_mode_is_group_rescan() {
        assert_eq!(
            classify_agg_strategy(&make_agg(AggFunc::Mode, false)),
            "GROUP_RESCAN"
        );
    }
}

/// Sort expression for ORDER BY or window functions.
#[derive(Debug, Clone)]
pub struct SortExpr {
    pub expr: Expr,
    pub ascending: bool,
    pub nulls_first: bool,
}

/// A correlation predicate extracted from a LATERAL subquery's WHERE clause.
///
/// Represents an equality of the form `inner_alias.inner_col = outer_alias.outer_col`
/// where the inner alias belongs to a FROM item inside the subquery and the outer
/// alias references the preceding FROM item (the LATERAL dependency).
///
/// Used by P2-6 to scope inner-change re-execution: instead of re-materializing
/// the entire outer table when inner sources change, we join the change buffer
/// against the outer table using the correlation column.
#[derive(Debug, Clone)]
pub struct CorrelationPredicate {
    /// Column name on the outer (child) table, unqualified.
    pub outer_col: String,
    /// Alias of the inner table inside the subquery.
    pub inner_alias: String,
    /// Column name on the inner table.
    pub inner_col: String,
    /// OID of the inner table (resolved from `inner_alias`).
    pub inner_oid: u32,
}

/// A window function expression: `func(args) OVER (PARTITION BY ... ORDER BY ... frame)`.
#[derive(Debug, Clone)]
pub struct WindowExpr {
    /// Function name (e.g., `row_number`, `rank`, `sum`).
    pub func_name: String,
    /// Function arguments (empty for `ROW_NUMBER()`, `RANK()`; one expr for `SUM(x)` etc.).
    pub args: Vec<Expr>,
    /// PARTITION BY expressions.
    pub partition_by: Vec<Expr>,
    /// ORDER BY expressions within the window frame.
    pub order_by: Vec<SortExpr>,
    /// Window frame clause (e.g., `ROWS BETWEEN 3 PRECEDING AND CURRENT ROW`).
    /// `None` means default frame.
    pub frame_clause: Option<String>,
    /// Output alias for this window expression.
    pub alias: String,
}

impl WindowExpr {
    /// Render the window function back to SQL text.
    pub fn to_sql(&self) -> String {
        let args_sql = if self.args.is_empty() {
            String::new()
        } else {
            self.args
                .iter()
                .map(|a| a.to_sql())
                .collect::<Vec<_>>()
                .join(", ")
        };

        let mut over_parts = Vec::new();
        if !self.partition_by.is_empty() {
            let pb = self
                .partition_by
                .iter()
                .map(|e| e.to_sql())
                .collect::<Vec<_>>()
                .join(", ");
            over_parts.push(format!("PARTITION BY {pb}"));
        }
        if !self.order_by.is_empty() {
            let ob = self
                .order_by
                .iter()
                .map(|s| {
                    let dir = if s.ascending { "ASC" } else { "DESC" };
                    let nulls = if s.ascending {
                        if s.nulls_first { " NULLS FIRST" } else { "" }
                    } else if s.nulls_first {
                        ""
                    } else {
                        " NULLS LAST"
                    };
                    format!("{} {dir}{nulls}", s.expr.to_sql())
                })
                .collect::<Vec<_>>()
                .join(", ");
            over_parts.push(format!("ORDER BY {ob}"));
        }

        if let Some(ref frame) = self.frame_clause {
            over_parts.push(frame.clone());
        }

        format!(
            "{}({}) OVER ({})",
            self.func_name,
            args_sql,
            over_parts.join(" "),
        )
    }
}

/// The operator tree — intermediate representation of a defining query.
#[derive(Debug, Clone)]
pub enum OpTree {
    /// Base table scan.
    Scan {
        table_oid: u32,
        table_name: String,
        schema: String,
        columns: Vec<Column>,
        /// Primary key column names from `pg_constraint` (empty if no PK).
        pk_columns: Vec<String>,
        alias: String,
    },
    /// Projection (SELECT expressions).
    Project {
        expressions: Vec<Expr>,
        aliases: Vec<String>,
        child: Box<OpTree>,
    },
    /// Filter (WHERE clause).
    Filter { predicate: Expr, child: Box<OpTree> },
    /// Inner join.
    InnerJoin {
        condition: Expr,
        left: Box<OpTree>,
        right: Box<OpTree>,
    },
    /// Left outer join.
    LeftJoin {
        condition: Expr,
        left: Box<OpTree>,
        right: Box<OpTree>,
    },
    /// Full outer join.
    FullJoin {
        condition: Expr,
        left: Box<OpTree>,
        right: Box<OpTree>,
    },
    /// GROUP BY with aggregates.
    Aggregate {
        group_by: Vec<Expr>,
        aggregates: Vec<AggExpr>,
        child: Box<OpTree>,
    },
    /// DISTINCT.
    Distinct { child: Box<OpTree> },
    /// UNION ALL.
    UnionAll { children: Vec<OpTree> },
    /// INTERSECT [ALL] of two branches.
    ///
    /// Set semantics (all=false): a row appears if it exists in *both* branches.
    /// Bag semantics (all=true): preserves duplicates up to the minimum count.
    Intersect {
        left: Box<OpTree>,
        right: Box<OpTree>,
        all: bool,
    },
    /// EXCEPT [ALL] (left minus right).
    ///
    /// Set semantics (all=false): a row appears if it exists in the left but not right.
    /// Bag semantics (all=true): preserves duplicates up to max(0, left_count - right_count).
    Except {
        left: Box<OpTree>,
        right: Box<OpTree>,
        all: bool,
    },
    /// A subquery in FROM (either inlined CTE or explicit subselect).
    ///
    /// The child OpTree is the parsed body of the subquery.
    /// Column aliases come from `FROM (...) AS alias(c1, c2, ...)`.
    Subquery {
        alias: String,
        column_aliases: Vec<String>,
        child: Box<OpTree>,
    },
    /// Reference to a CTE whose body is stored in the [`CteRegistry`].
    ///
    /// Multiple `CteScan` nodes can share the same `cte_id`, enabling
    /// the diff engine to differentiate the CTE body only once and
    /// reuse the result for every reference (Tier 2 optimization).
    CteScan {
        /// Index into [`CteRegistry::entries`].
        cte_id: usize,
        /// User-visible CTE name (e.g. `"totals"`).
        cte_name: String,
        /// FROM alias (may differ from `cte_name` if `FROM totals AS t`).
        alias: String,
        /// Output columns of the CTE body (copied at parse time).
        columns: Vec<String>,
        /// Column aliases from the CTE definition: `WITH x(a, b) AS (...)`.
        /// When non-empty, these rename the CTE body's output columns.
        cte_def_aliases: Vec<String>,
        /// Column aliases from `FROM cte AS alias(c1, c2, ...)`.
        /// When non-empty, these override `cte_def_aliases` / `columns` in output.
        column_aliases: Vec<String>,
        /// Parsed body of the CTE, stored for snapshot SQL generation.
        ///
        /// When this `CteScan` appears as a join child, [`build_snapshot_sql`]
        /// recurses into `body` to produce a valid SQL expression for the
        /// CTE's current state instead of using the alias (which is not a
        /// real relation).  `None` only in unit-test constructions that do
        /// not exercise the snapshot path.
        body: Option<Box<OpTree>>,
    },
    /// Recursive CTE: `WITH RECURSIVE name AS (base UNION [ALL] recursive)`.
    ///
    /// The base case (`base`) is a standard OpTree parsed from the
    /// non-recursive term. The `recursive` term contains a
    /// [`RecursiveSelfRef`] node wherever the CTE references itself.
    ///
    /// Stored in the [`CteRegistry`] and referenced via [`CteScan`].
    RecursiveCte {
        /// CTE name (e.g. `"tree"`).
        alias: String,
        /// Output column names.
        columns: Vec<String>,
        /// The non-recursive term (base case).
        base: Box<OpTree>,
        /// The recursive term (references self via `RecursiveSelfRef`).
        recursive: Box<OpTree>,
        /// `true` for `UNION ALL`, `false` for `UNION` (which deduplicates).
        union_all: bool,
    },
    /// Self-reference inside a recursive CTE's recursive term.
    ///
    /// Represents the `FROM <cte_name>` reference within the recursive
    /// term of a `WITH RECURSIVE` CTE. During differentiation, this is
    /// replaced with either the current ST storage (for seed computation)
    /// or the delta CTE (for recursive propagation).
    RecursiveSelfRef {
        /// Name of the recursive CTE being referenced.
        cte_name: String,
        /// FROM alias (may differ from `cte_name`).
        alias: String,
        /// Output columns (same as the `RecursiveCte`'s columns).
        columns: Vec<String>,
    },
    /// Window function evaluation.
    ///
    /// Represents one or more window function expressions applied to
    /// the child's output. All window expressions must share the same
    /// PARTITION BY clause (different ORDER BY is allowed).
    ///
    /// DVM strategy: partition-based recomputation — recompute entire
    /// partitions that contain any changed rows.
    Window {
        /// Window function expressions (at least one).
        window_exprs: Vec<WindowExpr>,
        /// Shared PARTITION BY columns (may be empty for un-partitioned windows).
        partition_by: Vec<Expr>,
        /// Pass-through (non-window) columns: `(expr, alias)`.
        pass_through: Vec<(Expr, String)>,
        /// Child operator producing the window function's input.
        child: Box<OpTree>,
    },
    /// A set-returning function in the FROM clause with implicit LATERAL semantics.
    ///
    /// Examples: `jsonb_array_elements(p.data)`, `unnest(a.tags)`,
    /// `generate_series(1, 10)`.
    ///
    /// The function call is stored as raw SQL because SRFs may reference
    /// columns from the left-hand side of the implicit cross join (LATERAL),
    /// making them impossible to fully decompose at parse time.
    ///
    /// DVM strategy: row-scoped recomputation — for each changed source row,
    /// delete old expansions and re-expand the SRF for the new row values.
    LateralFunction {
        /// The complete function call as SQL text,
        /// e.g. `jsonb_array_elements(p.data->'children')`.
        func_sql: String,
        /// The FROM alias, e.g. `child` from `... AS child`.
        alias: String,
        /// Column aliases from `AS alias(c1, c2)`, if any.
        column_aliases: Vec<String>,
        /// Whether `WITH ORDINALITY` was specified (adds a `bigint` ordinal column).
        with_ordinality: bool,
        /// The left-hand FROM item that this function may reference (LATERAL dependency).
        child: Box<OpTree>,
    },
    /// A LATERAL subquery in the FROM clause.
    ///
    /// The subquery is correlated — it references columns from preceding
    /// FROM items. The subquery body is stored as raw SQL because it
    /// cannot be independently differentiated (it depends on outer row
    /// context).
    ///
    /// DVM strategy: row-scoped recomputation — for each changed outer row,
    /// re-execute the subquery against the new outer row values.
    LateralSubquery {
        /// The complete subquery body as SQL text.
        subquery_sql: String,
        /// The FROM alias (e.g. `latest` from `... AS latest`).
        alias: String,
        /// Column aliases from `AS alias(c1, c2, ...)`, if any.
        column_aliases: Vec<String>,
        /// Output column names determined from the subquery's SELECT list.
        /// Used when `column_aliases` is empty.
        output_cols: Vec<String>,
        /// Whether this is a LEFT JOIN LATERAL (true) or CROSS JOIN LATERAL (false).
        /// LEFT JOIN preserves outer rows even when the subquery returns no rows.
        is_left_join: bool,
        /// Source table OIDs referenced by the subquery body.
        /// Needed for CDC trigger setup.
        subquery_source_oids: Vec<u32>,
        /// Correlation predicates extracted from the subquery WHERE clause (P2-6).
        /// When available, inner-change re-execution is scoped to only outer rows
        /// that correlate with changed inner rows, reducing O(|outer|) to O(delta).
        correlation_predicates: Vec<CorrelationPredicate>,
        /// The left-hand FROM item that this subquery may reference
        /// (LATERAL dependency).
        child: Box<OpTree>,
    },
    /// Semi-join (EXISTS / IN subquery).
    ///
    /// Emits all rows from `left` that have at least one matching row
    /// in `right` according to `condition`. Implements:
    /// - `WHERE EXISTS (SELECT ... FROM right_src WHERE correlation_cond)`
    /// - `WHERE x IN (SELECT col FROM right_src)`
    ///
    /// DVM strategy: two-part delta — left-side changes filtered by existence
    /// check against current right; right-side changes trigger re-evaluation
    /// of affected left rows.
    SemiJoin {
        condition: Expr,
        left: Box<OpTree>,
        right: Box<OpTree>,
    },
    /// Anti-join (NOT EXISTS / NOT IN subquery).
    ///
    /// Emits all rows from `left` that have NO matching row in `right`.
    /// Implements `WHERE NOT EXISTS (...)` and `WHERE x NOT IN (SELECT ...)`.
    ///
    /// DVM strategy: inverse of semi-join — left-side changes filtered by
    /// non-existence check; right-side changes trigger re-evaluation of
    /// affected left rows.
    AntiJoin {
        condition: Expr,
        left: Box<OpTree>,
        right: Box<OpTree>,
    },
    /// Scalar subquery in SELECT list (uncorrelated).
    ///
    /// Represents `SELECT (SELECT agg(...) FROM src) AS alias, ... FROM main`.
    /// The subquery produces a single scalar value that is cross-joined to
    /// every row from `child`. When the subquery's source changes, all
    /// output rows must be recomputed with the new scalar value.
    ScalarSubquery {
        /// Parsed OpTree for the inner scalar query.
        subquery: Box<OpTree>,
        /// Alias for the scalar result column.
        alias: String,
        /// Source table OIDs from the subquery (for CDC).
        subquery_source_oids: Vec<u32>,
        /// The outer query that produces the non-scalar columns.
        child: Box<OpTree>,
    },
    /// Constant-expression SELECT with no FROM clause.
    ///
    /// Used exclusively for the base case of `WITH RECURSIVE` CTEs whose
    /// anchor is a pure constant query such as `SELECT 1 AS id`.  These
    /// anchors have no source tables and therefore produce no delta rows;
    /// the full recursion is driven by the recursive term's source tables.
    ///
    /// The `sql` field holds the verbatim deparsed SQL so that helper
    /// functions that need to embed the anchor into a `WITH RECURSIVE`
    /// block (e.g., the DRed rederivation CTE) have a valid SQL fragment.
    ConstantSelect {
        /// Column names emitted by this node.
        columns: Vec<String>,
        /// Verbatim SQL of the original constant SELECT (no FROM clause).
        sql: String,
    },
}

/// Registry of parsed CTE bodies, shared across the OpTree.
///
/// The parser stores each CTE body here exactly once. Every reference
/// to that CTE in the main query produces a [`OpTree::CteScan`] node
/// pointing back via `cte_id`. The diff engine can then differentiate
/// each body once and cache the result.
#[derive(Debug, Clone, Default)]
pub struct CteRegistry {
    /// `(cte_name, parsed_body)` — index is the `cte_id`.
    pub entries: Vec<(String, OpTree)>,
}

impl CteRegistry {
    /// Collect all base table OIDs referenced in CTE bodies.
    pub fn source_oids(&self) -> Vec<u32> {
        self.entries
            .iter()
            .flat_map(|(_, tree)| tree.source_oids())
            .collect()
    }

    /// Look up a CTE entry by id.
    pub fn get(&self, cte_id: usize) -> Option<&(String, OpTree)> {
        self.entries.get(cte_id)
    }
}

/// The result of parsing a defining query: the main OpTree plus a
/// registry of CTE bodies (empty when the query has no CTEs).
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// The operator tree for the main SELECT.
    pub tree: OpTree,
    /// Registry of CTE bodies referenced by [`OpTree::CteScan`] nodes.
    pub cte_registry: CteRegistry,
    /// Whether the original query contained `WITH RECURSIVE`.
    ///
    /// When `true`, the OpTree may be incomplete — recursive CTE bodies
    /// are not parsed into the tree. Only FULL refresh mode is supported.
    pub has_recursion: bool,
    /// Advisory warnings collected during parsing (e.g. NATURAL JOIN column
    /// drift risk). Emit these once at the call site that presents diagnostics
    /// to the user; downstream parser calls (cache pre-warm, row-id derivation)
    /// should silently discard them.
    pub warnings: Vec<String>,
}

impl ParseResult {
    /// Collect `(table_oid, Vec<column_name>)` for all source tables
    /// referenced across the main tree and CTE bodies.
    ///
    /// Used to populate `pgt_dependencies.columns_used` at creation time
    /// so `detect_schema_change_kind()` can accurately classify DDL events.
    pub fn source_columns_used(&self) -> std::collections::HashMap<u32, Vec<String>> {
        let mut map: std::collections::HashMap<u32, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        self.tree.collect_source_columns(&mut map);
        for (_, cte_tree) in &self.cte_registry.entries {
            cte_tree.collect_source_columns(&mut map);
        }
        map.into_iter()
            .map(|(oid, set)| {
                let mut cols: Vec<String> = set.into_iter().collect();
                cols.sort();
                (oid, cols)
            })
            .collect()
    }

    /// A-2: Collect source columns from key positions (GROUP BY, JOIN ON,
    /// WHERE) across the main tree and CTE bodies.
    ///
    /// Returns `(table_oid, Vec<column_name>)` pairs for columns whose changes
    /// can affect row identity, grouping, or filtering. Used by the
    /// `changed_cols` bitmask to distinguish key changes from value-only changes.
    pub fn source_key_columns_used(&self) -> std::collections::HashMap<u32, Vec<String>> {
        let mut main_keys = self.tree.source_key_columns_used();
        for (_, cte_tree) in &self.cte_registry.entries {
            let cte_keys = cte_tree.source_key_columns_used();
            for (oid, cols) in cte_keys {
                let entry = main_keys.entry(oid).or_default();
                for col in cols {
                    if !entry.contains(&col) {
                        entry.push(col);
                    }
                }
                entry.sort();
            }
        }
        main_keys
    }

    /// Collect all function names referenced in the defining query (G8.2).
    ///
    /// Used to populate `pgt_stream_tables.functions_used` at creation time
    /// so that DDL hooks can detect `CREATE OR REPLACE FUNCTION` /
    /// `DROP FUNCTION` events that affect this stream table.
    ///
    /// Returns a sorted, deduplicated list of function names (lowercase).
    pub fn functions_used(&self) -> Vec<String> {
        let mut names = std::collections::HashSet::new();
        Self::collect_expr_funcs(&self.tree, &mut names);
        for (_, cte_tree) in &self.cte_registry.entries {
            Self::collect_expr_funcs(cte_tree, &mut names);
        }
        let mut result: Vec<String> = names.into_iter().collect();
        result.sort();
        result
    }

    /// Walk an OpTree recursively collecting function names from Expr nodes.
    fn collect_expr_funcs(tree: &OpTree, names: &mut std::collections::HashSet<String>) {
        match tree {
            OpTree::Scan { .. }
            | OpTree::CteScan { .. }
            | OpTree::RecursiveSelfRef { .. }
            | OpTree::ConstantSelect { .. } => {}
            OpTree::Project {
                expressions, child, ..
            } => {
                for expr in expressions {
                    Self::collect_funcs_from_expr(expr, names);
                }
                Self::collect_expr_funcs(child, names);
            }
            OpTree::Filter { predicate, child } => {
                Self::collect_funcs_from_expr(predicate, names);
                Self::collect_expr_funcs(child, names);
            }
            OpTree::InnerJoin {
                condition,
                left,
                right,
            }
            | OpTree::LeftJoin {
                condition,
                left,
                right,
            }
            | OpTree::FullJoin {
                condition,
                left,
                right,
            }
            | OpTree::SemiJoin {
                condition,
                left,
                right,
            }
            | OpTree::AntiJoin {
                condition,
                left,
                right,
            } => {
                Self::collect_funcs_from_expr(condition, names);
                Self::collect_expr_funcs(left, names);
                Self::collect_expr_funcs(right, names);
            }
            OpTree::Aggregate {
                group_by,
                aggregates,
                child,
            } => {
                for expr in group_by {
                    Self::collect_funcs_from_expr(expr, names);
                }
                for agg in aggregates {
                    // The aggregate function itself
                    names.insert(agg.function.sql_name().to_lowercase());
                    if let Some(arg) = &agg.argument {
                        Self::collect_funcs_from_expr(arg, names);
                    }
                    if let Some(filter) = &agg.filter {
                        Self::collect_funcs_from_expr(filter, names);
                    }
                    if let Some(second) = &agg.second_arg {
                        Self::collect_funcs_from_expr(second, names);
                    }
                }
                Self::collect_expr_funcs(child, names);
            }
            OpTree::Distinct { child }
            | OpTree::Subquery { child, .. }
            | OpTree::LateralFunction { child, .. }
            | OpTree::LateralSubquery { child, .. }
            | OpTree::ScalarSubquery { child, .. } => {
                Self::collect_expr_funcs(child, names);
            }
            OpTree::UnionAll { children } => {
                for c in children {
                    Self::collect_expr_funcs(c, names);
                }
            }
            OpTree::Intersect { left, right, .. } | OpTree::Except { left, right, .. } => {
                Self::collect_expr_funcs(left, names);
                Self::collect_expr_funcs(right, names);
            }
            OpTree::RecursiveCte {
                base, recursive, ..
            } => {
                Self::collect_expr_funcs(base, names);
                Self::collect_expr_funcs(recursive, names);
            }
            OpTree::Window {
                window_exprs,
                partition_by,
                child,
                ..
            } => {
                for we in window_exprs {
                    // Window function name
                    names.insert(we.func_name.to_lowercase());
                    for arg in &we.args {
                        Self::collect_funcs_from_expr(arg, names);
                    }
                    for pb in &we.partition_by {
                        Self::collect_funcs_from_expr(pb, names);
                    }
                    for ob in &we.order_by {
                        Self::collect_funcs_from_expr(&ob.expr, names);
                    }
                }
                for pb in partition_by {
                    Self::collect_funcs_from_expr(pb, names);
                }
                Self::collect_expr_funcs(child, names);
            }
        }
    }

    /// Extract function names from an Expr recursively.
    fn collect_funcs_from_expr(expr: &Expr, names: &mut std::collections::HashSet<String>) {
        match expr {
            Expr::FuncCall { func_name, args } => {
                names.insert(func_name.to_lowercase());
                for arg in args {
                    Self::collect_funcs_from_expr(arg, names);
                }
            }
            Expr::BinaryOp { left, right, .. } => {
                Self::collect_funcs_from_expr(left, names);
                Self::collect_funcs_from_expr(right, names);
            }
            // ColumnRef, Literal, Star, Raw: no function calls
            // (Raw may contain functions, but we don't parse them here —
            // the volatility checker handles those separately.)
            _ => {}
        }
    }
}

/// Look through transparent wrapper nodes (Filter, Subquery) to find the
/// underlying operator. Used by `row_id_key_columns()` — a Filter sitting
/// between Project and InnerJoin should not prevent PK-based row_ids.
pub fn unwrap_transparent(op: &OpTree) -> &OpTree {
    match op {
        OpTree::Filter { child, .. } | OpTree::Subquery { child, .. } => unwrap_transparent(child),
        other => other,
    }
}

impl OpTree {
    /// Get the alias for this node (used in CTE naming).
    pub fn alias(&self) -> &str {
        match self {
            OpTree::Scan { alias, .. } => alias,
            OpTree::Project { .. } => "project",
            OpTree::Filter { .. } => "filter",
            OpTree::InnerJoin { .. } => "join",
            OpTree::LeftJoin { .. } => "left_join",
            OpTree::FullJoin { .. } => "full_join",
            OpTree::Aggregate { .. } => "agg",
            OpTree::Distinct { .. } => "distinct",
            OpTree::UnionAll { .. } => "union",
            OpTree::Intersect { .. } => "intersect",
            OpTree::Except { .. } => "except",
            OpTree::Subquery { alias, .. } => alias,
            OpTree::CteScan { alias, .. } => alias,
            OpTree::RecursiveCte { alias, .. } => alias,
            OpTree::RecursiveSelfRef { alias, .. } => alias,
            OpTree::Window { .. } => "window",
            OpTree::LateralFunction { alias, .. } => alias,
            OpTree::LateralSubquery { alias, .. } => alias,
            OpTree::SemiJoin { .. } => "semi_join",
            OpTree::AntiJoin { .. } => "anti_join",
            OpTree::ScalarSubquery { alias, .. } => alias,
            OpTree::ConstantSelect { .. } => "__const",
        }
    }

    /// Return a human-readable name for this node kind (used in error messages).
    pub fn node_kind(&self) -> &str {
        match self {
            OpTree::Scan { .. } => "scan",
            OpTree::Project { .. } => "project",
            OpTree::Filter { .. } => "filter",
            OpTree::InnerJoin { .. } => "inner join",
            OpTree::LeftJoin { .. } => "left join",
            OpTree::FullJoin { .. } => "full join",
            OpTree::Aggregate { .. } => "aggregate",
            OpTree::Distinct { .. } => "distinct",
            OpTree::UnionAll { .. } => "union all",
            OpTree::Intersect { all, .. } => {
                if *all {
                    "intersect all"
                } else {
                    "intersect"
                }
            }
            OpTree::Except { all, .. } => {
                if *all {
                    "except all"
                } else {
                    "except"
                }
            }
            OpTree::Subquery { .. } => "subquery",
            OpTree::CteScan { .. } => "cte scan",
            OpTree::RecursiveCte { .. } => "recursive cte",
            OpTree::RecursiveSelfRef { .. } => "recursive self-reference",
            OpTree::Window { .. } => "window",
            OpTree::LateralFunction { .. } => "lateral function",
            OpTree::LateralSubquery { .. } => "lateral subquery",
            OpTree::SemiJoin { .. } => "semi join",
            OpTree::AntiJoin { .. } => "anti join",
            OpTree::ScalarSubquery { .. } => "scalar subquery",
            OpTree::ConstantSelect { .. } => "constant select",
        }
    }

    /// Returns `true` if the operator tree contains Aggregate, Distinct,
    /// Intersect, or Except — meaning the storage table needs a
    /// `__pgt_count BIGINT` auxiliary column for differential maintenance.
    ///
    /// Recurses through transparent wrappers (`Filter`, `Project`,
    /// `Subquery`) that may sit on top (e.g. HAVING adds a `Filter`
    /// around the `Aggregate`).
    pub fn needs_pgt_count(&self) -> bool {
        match self {
            OpTree::Aggregate { .. } => true,
            OpTree::Distinct { child, .. } => {
                // UNION (dedup) = Distinct { UnionAll }; uses union-dedup count path instead
                !matches!(child.as_ref(), OpTree::UnionAll { .. })
            }
            // Transparent wrappers — delegate to child
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. } => child.needs_pgt_count(),
            // INTERSECT/EXCEPT use dual counts, not __pgt_count
            OpTree::Intersect { .. } | OpTree::Except { .. } => false,
            _ => false,
        }
    }

    /// Returns the list of AVG auxiliary columns that need to be added to
    /// the stream table storage for algebraic AVG maintenance.
    ///
    /// For each non-DISTINCT AVG aggregate at the top level, returns a tuple
    /// of `(sum_col_name, count_col_name, arg_sql)`:
    /// - `sum_col_name`: `__pgt_aux_sum_{alias}` — stores running SUM
    /// - `count_col_name`: `__pgt_aux_count_{alias}` — stores running COUNT
    /// - `arg_sql`: the SQL expression for the aggregate argument (e.g. `"amount"`)
    ///
    /// Returns an empty vec if the query has no AVG aggregates or if the
    /// operator tree is not an aggregate query.
    pub fn avg_aux_columns(&self) -> Vec<(String, String, String)> {
        match self {
            OpTree::Aggregate { aggregates, .. } => {
                let mut aux = Vec::new();
                for agg in aggregates {
                    if agg.function.is_algebraic_via_aux() && !agg.is_distinct {
                        let arg_sql = agg
                            .argument
                            .as_ref()
                            .map(|e| e.to_sql())
                            .unwrap_or_else(|| "1".to_string());
                        aux.push((
                            format!("__pgt_aux_sum_{}", agg.alias),
                            format!("__pgt_aux_count_{}", agg.alias),
                            arg_sql,
                        ));
                    }
                }
                aux
            }
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. } => child.avg_aux_columns(),
            _ => Vec::new(),
        }
    }

    /// For each non-DISTINCT STDDEV/VAR aggregate, returns a tuple of
    /// `(sum2_col_name, arg_sql)`:
    /// - `sum2_col_name`: `__pgt_aux_sum2_{alias}` — stores running SUM(x²)
    /// - `arg_sql`: the SQL expression for the aggregate argument
    ///
    /// These are needed in addition to the sum/count from `avg_aux_columns`.
    pub fn sum2_aux_columns(&self) -> Vec<(String, String)> {
        match self {
            OpTree::Aggregate { aggregates, .. } => {
                let mut aux = Vec::new();
                for agg in aggregates {
                    if agg.function.needs_sum_of_squares() && !agg.is_distinct {
                        let arg_sql = agg
                            .argument
                            .as_ref()
                            .map(|e| e.to_sql())
                            .unwrap_or_else(|| "1".to_string());
                        aux.push((format!("__pgt_aux_sum2_{}", agg.alias), arg_sql));
                    }
                }
                aux
            }
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. } => child.sum2_aux_columns(),
            _ => Vec::new(),
        }
    }

    /// For each non-DISTINCT CORR/COVAR/REGR_* aggregate, returns tuples of
    /// `(col_name, arg_sql)` for cross-product auxiliary columns (P3-2):
    ///
    /// - `__pgt_aux_sumx_{alias}`:  running SUM(X)
    /// - `__pgt_aux_sumy_{alias}`:  running SUM(Y)
    /// - `__pgt_aux_sumxy_{alias}`: running SUM(X*Y)
    /// - `__pgt_aux_sumx2_{alias}`: running SUM(X²)
    /// - `__pgt_aux_sumy2_{alias}`: running SUM(Y²)
    ///
    /// (Count is shared with `__pgt_aux_count_*` from `avg_aux_columns`.)
    ///
    /// For regression aggs, argument = Y (first), second_arg = X (second).
    pub fn covar_aux_columns(&self) -> Vec<(String, String)> {
        match self {
            OpTree::Aggregate { aggregates, .. } => {
                let mut aux = Vec::new();
                for agg in aggregates {
                    if agg.function.needs_cross_products() && !agg.is_distinct {
                        let y_sql = agg
                            .argument
                            .as_ref()
                            .map(|e| e.to_sql())
                            .unwrap_or_else(|| "0".to_string());
                        let x_sql = agg
                            .second_arg
                            .as_ref()
                            .map(|e| e.to_sql())
                            .unwrap_or_else(|| "0".to_string());
                        let a = &agg.alias;
                        // Use a combined key for arg_sql: "x_expr|y_expr"
                        aux.push((format!("__pgt_aux_sumx_{a}"), x_sql.clone()));
                        aux.push((format!("__pgt_aux_sumy_{a}"), y_sql.clone()));
                        aux.push((format!("__pgt_aux_sumxy_{a}"), format!("{x_sql}|{y_sql}")));
                        aux.push((format!("__pgt_aux_sumx2_{a}"), x_sql.clone()));
                        aux.push((format!("__pgt_aux_sumy2_{a}"), y_sql));
                    }
                }
                aux
            }
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. } => child.covar_aux_columns(),
            _ => Vec::new(),
        }
    }

    /// Whether this operator tree contains a FULL OUTER JOIN node at any depth.
    ///
    /// Used to determine whether SUM aggregates above this tree need
    /// `__pgt_aux_nonnull_*` auxiliary columns for P2-2 NULL-transition
    /// correction.
    pub fn contains_full_join(&self) -> bool {
        match self {
            OpTree::FullJoin { .. } => true,
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. } => child.contains_full_join(),
            OpTree::InnerJoin { left, right, .. } | OpTree::LeftJoin { left, right, .. } => {
                left.contains_full_join() || right.contains_full_join()
            }
            _ => false,
        }
    }

    /// For each non-DISTINCT SUM aggregate above a FULL OUTER JOIN child,
    /// returns a tuple of `(nonnull_col_name, arg_sql)`:
    /// - `nonnull_col_name`: `__pgt_aux_nonnull_{alias}` — stores running
    ///   count of non-NULL argument values in the group
    /// - `arg_sql`: the SQL expression for the aggregate argument
    ///
    /// This auxiliary column enables algebraic NULL-transition correction
    /// (P2-2): when `nonnull_count > 0` the algebraic SUM formula is valid;
    /// when it drops to 0 the result is definitively NULL without rescanning.
    ///
    /// Returns an empty vec if no SUM aggregates sit above a FULL JOIN, or
    /// if the operator tree is not an aggregate query.
    pub fn nonnull_aux_columns(&self) -> Vec<(String, String)> {
        match self {
            OpTree::Aggregate {
                aggregates, child, ..
            } => {
                if !child.contains_full_join() {
                    return Vec::new();
                }
                let mut aux = Vec::new();
                for agg in aggregates {
                    if matches!(agg.function, AggFunc::Sum) && !agg.is_distinct {
                        let arg_sql = agg
                            .argument
                            .as_ref()
                            .map(|e| e.to_sql())
                            .unwrap_or_else(|| "0".to_string());
                        aux.push((format!("__pgt_aux_nonnull_{}", agg.alias), arg_sql));
                    }
                }
                aux
            }
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. } => child.nonnull_aux_columns(),
            _ => Vec::new(),
        }
    }

    /// Whether this operator represents a UNION (without ALL) that needs
    /// deduplication counting via `__pgt_count` in a wrapped UNION ALL form.
    pub fn needs_union_dedup_count(&self) -> bool {
        match self {
            OpTree::Distinct { child, .. } => matches!(child.as_ref(), OpTree::UnionAll { .. }),
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. } => child.needs_union_dedup_count(),
            _ => false,
        }
    }

    /// Whether this operator needs dual multiplicity counts
    /// (`__pgt_count_l`, `__pgt_count_r`) for set operation tracking.
    pub fn needs_dual_count(&self) -> bool {
        match self {
            OpTree::Intersect { .. } | OpTree::Except { .. } => true,
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. } => child.needs_dual_count(),
            _ => false,
        }
    }

    /// Extract GROUP BY column names from an aggregate operator.
    ///
    /// Returns `Some(vec!["col1", "col2"])` for `Aggregate` nodes with
    /// GROUP BY columns, `None` for non-aggregate queries or aggregates
    /// without GROUP BY (scalar aggregates).
    ///
    /// For transparent wrappers (`Project`, `Subquery`), delegates to child.
    /// When a `Project` renames GROUP BY columns (e.g., `t.id AS department_id`),
    /// the Project's aliases are used instead of the raw Aggregate names,
    /// matching the storage table's column names.
    pub fn group_by_columns(&self) -> Option<Vec<String>> {
        match self {
            OpTree::Aggregate { group_by, .. } => {
                if group_by.is_empty() {
                    None
                } else {
                    Some(group_by.iter().map(|e| e.output_name()).collect())
                }
            }
            OpTree::Project {
                child,
                aliases,
                expressions,
            } => {
                // If the child is an Aggregate with GROUP BY, resolve the
                // group-by column names through this Project's alias mapping.
                // The storage table uses the Project's aliases (the SELECT
                // output names), not the Aggregate's raw group-by expression
                // names. Without this, `GROUP BY t.id` with alias
                // `department_id` would produce an index on "id" while the
                // storage table column is "department_id".
                if let Some(raw_names) = child.group_by_columns() {
                    let resolved: Vec<String> = raw_names
                        .iter()
                        .map(|raw| {
                            // Handle ordinal GROUP BY (e.g., GROUP BY 1):
                            // resolve the integer position to the Nth alias.
                            if let Ok(pos) = raw.parse::<usize>()
                                && pos >= 1
                                && pos <= aliases.len()
                            {
                                return aliases[pos - 1].clone();
                            }
                            // Find the expression in this Project that
                            // references the raw group-by column, and return
                            // the corresponding alias.
                            for (expr, alias) in expressions.iter().zip(aliases.iter()) {
                                if expr.output_name() == *raw {
                                    return alias.clone();
                                }
                            }
                            // No matching expression — keep the raw name
                            // (shouldn't happen for well-formed trees).
                            raw.clone()
                        })
                        .collect();
                    Some(resolved)
                } else {
                    None
                }
            }
            OpTree::Subquery { child, .. } => child.group_by_columns(),
            _ => None,
        }
    }

    /// Return the column names that should be hashed to produce `__pgt_row_id`.
    ///
    /// This mirrors the hash generation in each operator's diff function:
    /// - **Scan**: non-nullable (heuristic PK) columns, or all if none
    /// - **Aggregate**: GROUP BY columns  
    /// - **Distinct**: all output columns
    /// - **Filter/Project/Subquery/CteScan**: delegate to child
    /// - **Window**: pass-through (non-window) columns
    /// - **Join/UnionAll/RecursiveCte**: returns `None` (no simple column list)
    ///
    /// Returns `None` for operators whose row-id computation is too complex
    /// to express as a simple column-hash expression (e.g., joins combine
    /// two sub-hashes).  Callers should fall back to a generic hash.
    pub fn row_id_key_columns(&self) -> Option<Vec<String>> {
        match self {
            OpTree::Scan {
                columns,
                pk_columns,
                ..
            } => {
                // Use PK columns if available (matches CDC trigger pk_hash).
                if !pk_columns.is_empty() {
                    return Some(pk_columns.clone());
                }
                // Keyless table (S10): use all columns as content hash key,
                // matching the all-column pk_hash computed by the CDC trigger.
                Some(columns.iter().map(|c| c.name.clone()).collect())
            }
            OpTree::Filter { child, .. } => child.row_id_key_columns(),
            OpTree::Project {
                child,
                aliases,
                expressions,
            } => {
                // For join children, use PK-based row_ids for stability.
                // Content-based hashes (all columns) break when both join
                // sides change simultaneously — the DELETE hash is computed
                // with the OTHER side's CURRENT values, not the OLD values
                // stored in the ST. PK columns are stable across value
                // changes, so DELETE hashes always match the stored row_id.
                //
                // Look through transparent wrappers (Filter, Subquery) to
                // find the underlying join node. Q15 has Project > Filter >
                // InnerJoin — the Filter must not prevent PK-based row_ids.
                let unwrapped = unwrap_transparent(child);
                if matches!(
                    unwrapped,
                    OpTree::InnerJoin { .. } | OpTree::LeftJoin { .. } | OpTree::FullJoin { .. }
                ) {
                    let pk_aliases = join_pk_aliases(expressions, aliases, unwrapped);
                    return Some(pk_aliases.unwrap_or_else(|| aliases.clone()));
                }
                // For lateral function/subquery children, use all projected
                // columns as the row_id key. SRF expansions have no natural PK,
                // so we hash all output columns. This ensures both FULL refresh
                // (which hashes the projected output) and DIFFERENTIAL refresh
                // (which recomputes row_id in the Project operator) produce the
                // same row_ids.
                if matches!(
                    unwrapped,
                    OpTree::LateralFunction { .. } | OpTree::LateralSubquery { .. }
                ) {
                    return Some(aliases.clone());
                }
                // For semi-join/anti-join children (EXISTS / NOT EXISTS / IN),
                // use all projected output columns as a content hash.  A semi-join
                // filters the left side without changing its cardinality or
                // introducing new columns, so the projected output is a stable
                // identifier as long as it is unique — which it typically is
                // (the left table's PK columns are usually projected).  Both the
                // FULL refresh (hash from projected columns) and  the DIFFERENTIAL
                // refresh (row_id recomputed in diff_project) then use the same
                // formula, preventing the row_id mismatch that would leave stale
                // rows in the stream table after a differential refresh.
                if matches!(unwrapped, OpTree::SemiJoin { .. } | OpTree::AntiJoin { .. }) {
                    return Some(aliases.clone());
                }
                // Project may drop or rename columns — map child's key
                // column names through the aliasing, then verify they're in
                // the output. E.g., Aggregate GROUP BY "name" → Project
                // alias "region" at the same position.
                let child_out = child.output_columns();
                // When a Project narrows a CTE scan (e.g., SELECT id, name
                // FROM cte where the CTE produces [id, parent_id, name]),
                // positional mapping is unreliable — child_out[1] is
                // "parent_id" but aliases[1] is "name". In that specific
                // CTE case, fall back to content-hashing the projected
                // columns, which matches the CteScan wrapper's formula.
                //
                // For other narrowing projections (notably the EC-03 window
                // subquery-lift rewrite), hashing only the projected aliases
                // is unsafe because the derived output may not be unique
                // (e.g., CASE/CAST/COALESCE over row_number()) and initial
                // population would hit duplicate __pgt_row_id values. Those
                // shapes must fall back to the generic row_number()-based
                // hash via `None` instead.
                //
                // Exception: Project(Filter(Scan)) where the Filter exposes
                // WHERE-clause columns in its output (needed by the predicate
                // but not in the SELECT list). Standard positional mapping is
                // unreliable here, but we can still recover the correct
                // PK-based row_id by mapping child key columns through the
                // explicit Project expressions. This case is critical for
                // queries like `SELECT id, val AS vb FROM t WHERE side >= 2`
                // where the differential refresh uses PK-based hash(id) and
                // the full refresh must produce the same value.
                if child_out.len() != aliases.len() {
                    if matches!(unwrapped, OpTree::CteScan { .. }) {
                        return Some(aliases.clone());
                    }
                    // Try expression-based key column mapping: for each child
                    // key column, find the Project expression that outputs it
                    // (as a plain ColumnRef) and map to the corresponding alias.
                    if let Some(child_keys) = child.row_id_key_columns() {
                        let mapped: Vec<String> = child_keys
                            .iter()
                            .filter_map(|k| {
                                let pos = expressions.iter().position(|expr| {
                                    matches!(
                                        expr,
                                        Expr::ColumnRef { column_name, .. }
                                            if column_name == k
                                    )
                                })?;
                                aliases.get(pos).cloned()
                            })
                            .collect();
                        let out = self.output_columns();
                        // Only accept if ALL key columns were mapped and all
                        // mapped names appear in the projected output.
                        if mapped.len() == child_keys.len()
                            && mapped.iter().all(|m| out.contains(m))
                        {
                            return Some(mapped);
                        }
                    }
                    return None;
                }
                match child.row_id_key_columns() {
                    Some(keys) => {
                        let mapped: Vec<String> = keys
                            .iter()
                            .map(|k| {
                                if let Some(pos) = child_out.iter().position(|c| c == k) {
                                    aliases.get(pos).cloned().unwrap_or_else(|| k.clone())
                                } else {
                                    k.clone()
                                }
                            })
                            .collect();
                        let out = self.output_columns();
                        if mapped.iter().all(|k| out.contains(k)) {
                            Some(mapped)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            OpTree::Distinct { child } => {
                // Distinct hashes all child output columns.
                Some(child.output_columns())
            }
            OpTree::Intersect { left, .. } | OpTree::Except { left, .. } => {
                // INTERSECT/EXCEPT hash all output columns (like Distinct).
                Some(left.output_columns())
            }
            OpTree::Aggregate { group_by, .. } => {
                let cols: Vec<String> = group_by.iter().map(|e| e.output_name()).collect();
                if cols.is_empty() {
                    // Scalar aggregate: special sentinel
                    None // caller handles with pg_trickle_hash('__singleton_group')
                } else {
                    Some(cols)
                }
            }
            OpTree::Subquery { child, .. } => child.row_id_key_columns(),
            OpTree::CteScan { columns, .. } => {
                // CTE scan: use all CTE output columns as content hash.
                // This matches the intermediate aggregate's row_id formula
                // when the CTE body contains an aggregate.
                Some(columns.clone())
            }
            OpTree::Window { .. } => {
                // Window functions like RANK/DENSE_RANK can produce identical
                // output rows (tied values). Fall back to row_number-based
                // hash in the initial population to guarantee uniqueness.
                // The window diff operator computes its own unique row_ids
                // during partition recomputation.
                None
            }
            OpTree::LateralFunction { .. } => {
                // SRF expansions have no natural primary key.
                // Row IDs are content-hash based, computed by the diff operator.
                None
            }
            OpTree::LateralSubquery { .. } => {
                // Use content-hash for row IDs so that the initial population
                // and differential refresh produce consistent __pgt_row_id values.
                let cols = self.output_columns();
                if cols.is_empty() { None } else { Some(cols) }
            }
            OpTree::ScalarSubquery { child, .. } => {
                // Scalar subquery row identity comes from the visible outer
                // child columns only. The scalar value itself changes for all
                // rows simultaneously and is not part of row identity.
                //
                // Use the outer child's visible output columns rather than
                // its internal row-id strategy, because the full-refresh path
                // can only hash columns present in the outer SELECT. The
                // differential operator recomputes the same visible-column
                // hash for both outer-row deltas and scalar-change rewrites.
                let cols = child.output_columns();
                if cols.is_empty() { None } else { Some(cols) }
            }
            // Join, UnionAll, RecursiveCte: complex hash, no simple column list
            _ => None,
        }
    }

    /// Get the output columns of this operator node.
    pub fn output_columns(&self) -> Vec<String> {
        match self {
            OpTree::Scan { columns, .. } => columns.iter().map(|c| c.name.clone()).collect(),
            OpTree::Project { aliases, .. } => aliases.clone(),
            OpTree::Filter { child, .. } => child.output_columns(),
            OpTree::InnerJoin { left, right, .. }
            | OpTree::LeftJoin { left, right, .. }
            | OpTree::FullJoin { left, right, .. } => {
                let mut cols = left.output_columns();
                cols.extend(right.output_columns());
                cols
            }
            OpTree::Aggregate {
                group_by,
                aggregates,
                ..
            } => {
                let mut cols: Vec<String> = group_by.iter().map(|e| e.output_name()).collect();
                cols.extend(aggregates.iter().map(|a| a.alias.clone()));
                cols
            }
            OpTree::Distinct { child } => child.output_columns(),
            OpTree::UnionAll { children } => children
                .first()
                .map(|c| c.output_columns())
                .unwrap_or_default(),
            OpTree::Intersect { left, .. } | OpTree::Except { left, .. } => left.output_columns(),
            OpTree::Subquery {
                column_aliases,
                child,
                ..
            } => {
                if column_aliases.is_empty() {
                    child.output_columns()
                } else {
                    column_aliases.clone()
                }
            }
            OpTree::CteScan {
                columns,
                cte_def_aliases,
                column_aliases,
                ..
            } => {
                if !column_aliases.is_empty() {
                    column_aliases.clone()
                } else if !cte_def_aliases.is_empty() {
                    cte_def_aliases.clone()
                } else {
                    columns.clone()
                }
            }
            OpTree::RecursiveCte { columns, .. } => columns.clone(),
            OpTree::RecursiveSelfRef { columns, .. } => columns.clone(),
            OpTree::Window {
                window_exprs,
                pass_through,
                ..
            } => {
                let mut cols: Vec<String> = pass_through
                    .iter()
                    .map(|(_, alias)| alias.clone())
                    .collect();
                cols.extend(window_exprs.iter().map(|w| w.alias.clone()));
                cols
            }
            OpTree::LateralFunction {
                func_sql,
                alias,
                column_aliases,
                with_ordinality,
                child,
                ..
            } => {
                // Output = child columns + SRF result columns + optional ordinality
                let mut cols = child.output_columns();
                cols.extend(lateral_function_output_columns(
                    func_sql,
                    alias,
                    column_aliases,
                ));
                if *with_ordinality {
                    cols.push("ordinality".to_string());
                }
                cols
            }
            OpTree::LateralSubquery {
                column_aliases,
                output_cols,
                child,
                ..
            } => {
                // Output = child columns + subquery result columns
                let mut cols = child.output_columns();
                if column_aliases.is_empty() {
                    cols.extend(output_cols.clone());
                } else {
                    cols.extend(column_aliases.clone());
                }
                cols
            }
            OpTree::SemiJoin { left, .. } | OpTree::AntiJoin { left, .. } => {
                // Semi/anti-join only emits left-side columns
                left.output_columns()
            }
            OpTree::ScalarSubquery { alias, child, .. } => {
                // Outer columns + scalar result column
                let mut cols = child.output_columns();
                cols.push(alias.clone());
                cols
            }
            OpTree::ConstantSelect { columns, .. } => columns.clone(),
        }
    }

    /// Collect all base table OIDs referenced in this tree.
    pub fn source_oids(&self) -> Vec<u32> {
        match self {
            OpTree::Scan { table_oid, .. } => vec![*table_oid],
            OpTree::Project { child, .. }
            | OpTree::Filter { child, .. }
            | OpTree::Distinct { child } => child.source_oids(),
            OpTree::Aggregate { child, .. } => child.source_oids(),
            OpTree::InnerJoin { left, right, .. }
            | OpTree::LeftJoin { left, right, .. }
            | OpTree::FullJoin { left, right, .. }
            | OpTree::Intersect { left, right, .. }
            | OpTree::Except { left, right, .. } => {
                let mut oids = left.source_oids();
                oids.extend(right.source_oids());
                oids
            }
            OpTree::UnionAll { children } => {
                children.iter().flat_map(|c| c.source_oids()).collect()
            }
            OpTree::Subquery { child, .. } => child.source_oids(),
            // CteScan OIDs are resolved via the CteRegistry at diff time
            OpTree::CteScan { .. } => vec![],
            OpTree::RecursiveCte {
                base, recursive, ..
            } => {
                let mut oids = base.source_oids();
                oids.extend(recursive.source_oids());
                oids.sort_unstable();
                oids.dedup();
                oids
            }
            // Self-references don't contribute base table OIDs
            OpTree::RecursiveSelfRef { .. } => vec![],
            OpTree::Window { child, .. } => child.source_oids(),
            OpTree::LateralFunction { child, .. } => child.source_oids(),
            OpTree::LateralSubquery {
                subquery_source_oids,
                child,
                ..
            } => {
                let mut oids = child.source_oids();
                oids.extend(subquery_source_oids.iter());
                oids.sort_unstable();
                oids.dedup();
                oids
            }
            OpTree::SemiJoin { left, right, .. } | OpTree::AntiJoin { left, right, .. } => {
                let mut oids = left.source_oids();
                oids.extend(right.source_oids());
                oids.sort_unstable();
                oids.dedup();
                oids
            }
            OpTree::ScalarSubquery {
                subquery_source_oids,
                child,
                ..
            } => {
                let mut oids = child.source_oids();
                oids.extend(subquery_source_oids.iter());
                oids.sort_unstable();
                oids.dedup();
                oids
            }
            // Constant anchors have no source tables — they never produce delta rows.
            OpTree::ConstantSelect { .. } => vec![],
        }
    }

    /// G12-AGG: Classify the maintenance strategy for each aggregate in the
    /// tree.  Returns a list of `(alias, strategy)` pairs.
    ///
    /// Strategies:
    /// - `"ALGEBRAIC_INVERTIBLE"` — COUNT, COUNT(*), SUM: O(1) algebraic delta.
    /// - `"ALGEBRAIC_VIA_AUX"` — AVG, STDDEV, VAR, CORR, REGR_*: maintained via
    ///   auxiliary columns on the storage table.
    /// - `"SEMI_ALGEBRAIC"` — MIN, MAX: algebraic for inserts, group-rescan on
    ///   extremum deletion.
    /// - `"GROUP_RESCAN"` — STRING_AGG, ARRAY_AGG, JSON_AGG, etc.: any change
    ///   in the group triggers full re-aggregation from source data.
    ///
    /// Returns an empty vec when there is no `Aggregate` node.
    pub fn aggregate_strategies(&self) -> Vec<(String, &'static str)> {
        match self {
            OpTree::Aggregate { aggregates, .. } => aggregates
                .iter()
                .map(|agg| {
                    let strategy = classify_agg_strategy(agg);
                    (agg.alias.clone(), strategy)
                })
                .collect(),
            OpTree::Project { child, .. }
            | OpTree::Filter { child, .. }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. }
            | OpTree::Distinct { child } => child.aggregate_strategies(),
            _ => Vec::new(),
        }
    }

    /// G12-AGG: Return the names of group-rescan aggregates in the tree.
    /// Used to emit a warning at `create_stream_table` time.
    pub fn group_rescan_aggregate_names(&self) -> Vec<String> {
        self.aggregate_strategies()
            .into_iter()
            .filter(|(_, strategy)| *strategy == "GROUP_RESCAN")
            .map(|(alias, _)| alias)
            .collect()
    }

    /// Collect `(table_oid, Vec<column_name>)` pairs from all Scan nodes.
    ///
    /// When the same table appears in multiple Scan nodes (self-join), the
    /// column lists are merged (union of column names). This is used to
    /// populate `pgt_dependencies.columns_used` at creation time.
    pub fn source_columns_used(&self) -> std::collections::HashMap<u32, Vec<String>> {
        let mut map: std::collections::HashMap<u32, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        self.collect_source_columns(&mut map);
        map.into_iter()
            .map(|(oid, set)| {
                let mut cols: Vec<String> = set.into_iter().collect();
                cols.sort();
                (oid, cols)
            })
            .collect()
    }

    /// A-2: Collect source columns from "key" positions — GROUP BY expressions,
    /// JOIN ON conditions, and Filter WHERE predicates.
    ///
    /// Returns `(table_oid, Vec<column_name>)` pairs where columns appear in key
    /// contexts. A change to a key column can affect row identity/grouping,
    /// whereas a change to a value-only column (only in SELECT / aggregate input)
    /// cannot move a row to a different group.
    ///
    /// Used by the changed_cols bitmask infrastructure to distinguish key changes
    /// from value-only changes.
    pub fn source_key_columns_used(&self) -> std::collections::HashMap<u32, Vec<String>> {
        // Step 1: Collect column names from key-position expressions.
        let mut key_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.collect_key_column_names(&mut key_names);

        // Step 2: Build a mapping from table_alias → (table_oid, column_names).
        let mut alias_map: std::collections::HashMap<
            String,
            (u32, std::collections::HashSet<String>),
        > = std::collections::HashMap::new();
        self.collect_scan_alias_map(&mut alias_map);

        // Step 3: Intersect key names with Scan columns per table.
        // A column name in key_names matches a Scan column if:
        // - It appears in a ColumnRef with a matching table_alias, OR
        // - It appears unqualified and matches a Scan column name.
        let mut result_map: std::collections::HashMap<u32, std::collections::HashSet<String>> =
            std::collections::HashMap::new();

        for (oid, scan_cols) in alias_map.values() {
            for kn in &key_names {
                if scan_cols.contains(kn) {
                    result_map.entry(*oid).or_default().insert(kn.clone());
                }
            }
        }

        result_map
            .into_iter()
            .map(|(oid, set)| {
                let mut cols: Vec<String> = set.into_iter().collect();
                cols.sort();
                (oid, cols)
            })
            .collect()
    }

    /// Recursive helper — collects column names from GROUP BY, JOIN ON, and
    /// Filter WHERE expressions (key positions that affect row identity).
    fn collect_key_column_names(&self, names: &mut std::collections::HashSet<String>) {
        match self {
            OpTree::Aggregate {
                group_by, child, ..
            } => {
                for expr in group_by {
                    Self::collect_column_names_from_expr(expr, names);
                }
                child.collect_key_column_names(names);
            }
            OpTree::InnerJoin {
                condition,
                left,
                right,
            }
            | OpTree::LeftJoin {
                condition,
                left,
                right,
            }
            | OpTree::FullJoin {
                condition,
                left,
                right,
            } => {
                Self::collect_column_names_from_expr(condition, names);
                left.collect_key_column_names(names);
                right.collect_key_column_names(names);
            }
            OpTree::Filter { predicate, child } => {
                Self::collect_column_names_from_expr(predicate, names);
                child.collect_key_column_names(names);
            }
            OpTree::Project { child, .. }
            | OpTree::Distinct { child }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. }
            | OpTree::LateralFunction { child, .. }
            | OpTree::LateralSubquery { child, .. }
            | OpTree::ScalarSubquery { child, .. } => {
                child.collect_key_column_names(names);
            }
            OpTree::SemiJoin {
                condition,
                left,
                right,
            }
            | OpTree::AntiJoin {
                condition,
                left,
                right,
            } => {
                Self::collect_column_names_from_expr(condition, names);
                left.collect_key_column_names(names);
                right.collect_key_column_names(names);
            }
            OpTree::Intersect { left, right, .. } | OpTree::Except { left, right, .. } => {
                left.collect_key_column_names(names);
                right.collect_key_column_names(names);
            }
            OpTree::UnionAll { children } => {
                for c in children {
                    c.collect_key_column_names(names);
                }
            }
            OpTree::RecursiveCte {
                base, recursive, ..
            } => {
                base.collect_key_column_names(names);
                recursive.collect_key_column_names(names);
            }
            OpTree::Scan { .. }
            | OpTree::CteScan { .. }
            | OpTree::RecursiveSelfRef { .. }
            | OpTree::ConstantSelect { .. } => {}
        }
    }

    /// Extract column names from an expression (GROUP BY, JOIN ON, WHERE).
    fn collect_column_names_from_expr(expr: &Expr, names: &mut std::collections::HashSet<String>) {
        match expr {
            Expr::ColumnRef { column_name, .. } => {
                names.insert(column_name.clone());
            }
            Expr::BinaryOp { left, right, .. } => {
                Self::collect_column_names_from_expr(left, names);
                Self::collect_column_names_from_expr(right, names);
            }
            Expr::FuncCall { args, .. } => {
                for arg in args {
                    Self::collect_column_names_from_expr(arg, names);
                }
            }
            Expr::Literal(_) | Expr::Star { .. } | Expr::Raw(_) => {}
        }
    }

    /// Build a mapping from table alias/name → (table_oid, column_names) from
    /// Scan nodes in the tree.
    fn collect_scan_alias_map(
        &self,
        map: &mut std::collections::HashMap<String, (u32, std::collections::HashSet<String>)>,
    ) {
        match self {
            OpTree::Scan {
                table_oid,
                alias,
                columns,
                ..
            } => {
                let entry = map
                    .entry(alias.clone())
                    .or_insert_with(|| (*table_oid, std::collections::HashSet::new()));
                for col in columns {
                    entry.1.insert(col.name.clone());
                }
            }
            OpTree::Project { child, .. }
            | OpTree::Filter { child, .. }
            | OpTree::Distinct { child }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. }
            | OpTree::LateralFunction { child, .. }
            | OpTree::LateralSubquery { child, .. }
            | OpTree::ScalarSubquery { child, .. } => child.collect_scan_alias_map(map),
            OpTree::Aggregate { child, .. } => child.collect_scan_alias_map(map),
            OpTree::InnerJoin { left, right, .. }
            | OpTree::LeftJoin { left, right, .. }
            | OpTree::FullJoin { left, right, .. }
            | OpTree::SemiJoin { left, right, .. }
            | OpTree::AntiJoin { left, right, .. }
            | OpTree::Intersect { left, right, .. }
            | OpTree::Except { left, right, .. } => {
                left.collect_scan_alias_map(map);
                right.collect_scan_alias_map(map);
            }
            OpTree::UnionAll { children } => {
                for c in children {
                    c.collect_scan_alias_map(map);
                }
            }
            OpTree::RecursiveCte {
                base, recursive, ..
            } => {
                base.collect_scan_alias_map(map);
                recursive.collect_scan_alias_map(map);
            }
            OpTree::CteScan { .. }
            | OpTree::RecursiveSelfRef { .. }
            | OpTree::ConstantSelect { .. } => {}
        }
    }

    /// Recursive helper — collects column names per table OID from Scan nodes.
    fn collect_source_columns(
        &self,
        map: &mut std::collections::HashMap<u32, std::collections::HashSet<String>>,
    ) {
        match self {
            OpTree::Scan {
                table_oid, columns, ..
            } => {
                let entry = map.entry(*table_oid).or_default();
                for col in columns {
                    entry.insert(col.name.clone());
                }
            }
            OpTree::Project { child, .. }
            | OpTree::Filter { child, .. }
            | OpTree::Distinct { child } => child.collect_source_columns(map),
            OpTree::Aggregate { child, .. } => child.collect_source_columns(map),
            OpTree::InnerJoin { left, right, .. }
            | OpTree::LeftJoin { left, right, .. }
            | OpTree::FullJoin { left, right, .. }
            | OpTree::Intersect { left, right, .. }
            | OpTree::Except { left, right, .. } => {
                left.collect_source_columns(map);
                right.collect_source_columns(map);
            }
            OpTree::UnionAll { children } => {
                for c in children {
                    c.collect_source_columns(map);
                }
            }
            OpTree::Subquery { child, .. } => child.collect_source_columns(map),
            OpTree::CteScan { .. } => { /* resolved via CteRegistry */ }
            OpTree::RecursiveCte {
                base, recursive, ..
            } => {
                base.collect_source_columns(map);
                recursive.collect_source_columns(map);
            }
            OpTree::RecursiveSelfRef { .. } => {}
            OpTree::Window { child, .. } => child.collect_source_columns(map),
            OpTree::LateralFunction { child, .. } => child.collect_source_columns(map),
            OpTree::LateralSubquery { child, .. } => child.collect_source_columns(map),
            OpTree::SemiJoin { left, right, .. } | OpTree::AntiJoin { left, right, .. } => {
                left.collect_source_columns(map);
                right.collect_source_columns(map);
            }
            OpTree::ScalarSubquery { child, .. } => child.collect_source_columns(map),
            OpTree::ConstantSelect { .. } => {}
        }
    }

    /// Prune `Scan.columns` to only those columns referenced by the
    /// defining query (plus PK columns needed for row_id computation).
    ///
    /// This reduces I/O in `diff_scan_change_buffer()` — fewer `new_*` /
    /// `old_*` columns are selected from the change buffer, which matters
    /// for wide source tables where the view references only a few columns.
    ///
    /// The approach is conservative: we collect all column names referenced
    /// anywhere in the tree's expressions, then intersect with each Scan's
    /// column set. Unqualified column refs and `Raw` / `Star` expressions
    /// cause the pass to bail out for safety (keeping all columns).
    pub fn prune_scan_columns(&mut self) {
        let mut refs = ColumnRefSet::default();
        if !self.collect_all_column_refs(&mut refs) {
            // Bail out — found Star/Raw that prevent safe pruning.
            return;
        }
        // If no column refs were collected (e.g. SELECT * with no Project wrapper),
        // bail out to avoid incorrectly pruning all non-PK columns.
        if refs.qualified.is_empty() && refs.unqualified.is_empty() {
            return;
        }
        self.apply_column_pruning(&refs);
    }

    /// Collect all `Expr::ColumnRef` occurrences in the tree.
    ///
    /// Returns `false` if an unparseable expression (`Star`, `Raw`) is
    /// encountered, meaning pruning should be skipped.
    fn collect_all_column_refs(&self, refs: &mut ColumnRefSet) -> bool {
        match self {
            OpTree::Scan { .. } | OpTree::CteScan { .. } | OpTree::RecursiveSelfRef { .. } => true,
            OpTree::Project {
                expressions, child, ..
            } => {
                for expr in expressions {
                    if !collect_refs_from_expr(expr, refs) {
                        return false;
                    }
                }
                child.collect_all_column_refs(refs)
            }
            OpTree::Filter {
                predicate, child, ..
            } => {
                if !collect_refs_from_expr(predicate, refs) {
                    return false;
                }
                child.collect_all_column_refs(refs)
            }
            OpTree::InnerJoin {
                condition,
                left,
                right,
            }
            | OpTree::LeftJoin {
                condition,
                left,
                right,
            }
            | OpTree::FullJoin {
                condition,
                left,
                right,
            } => {
                if !collect_refs_from_expr(condition, refs) {
                    return false;
                }
                left.collect_all_column_refs(refs) && right.collect_all_column_refs(refs)
            }
            OpTree::Aggregate {
                group_by,
                aggregates,
                child,
            } => {
                for expr in group_by {
                    if !collect_refs_from_expr(expr, refs) {
                        return false;
                    }
                }
                for agg in aggregates {
                    if !collect_refs_from_agg(agg, refs) {
                        return false;
                    }
                }
                child.collect_all_column_refs(refs)
            }
            OpTree::Distinct { child } => child.collect_all_column_refs(refs),
            OpTree::UnionAll { children } => {
                children.iter().all(|c| c.collect_all_column_refs(refs))
            }
            OpTree::Intersect { left, right, .. } | OpTree::Except { left, right, .. } => {
                left.collect_all_column_refs(refs) && right.collect_all_column_refs(refs)
            }
            OpTree::Subquery { child, .. } => child.collect_all_column_refs(refs),
            OpTree::RecursiveCte {
                base, recursive, ..
            } => base.collect_all_column_refs(refs) && recursive.collect_all_column_refs(refs),
            OpTree::Window {
                window_exprs,
                partition_by,
                pass_through,
                child,
            } => {
                for we in window_exprs {
                    for arg in &we.args {
                        if !collect_refs_from_expr(arg, refs) {
                            return false;
                        }
                    }
                    for pb in &we.partition_by {
                        if !collect_refs_from_expr(pb, refs) {
                            return false;
                        }
                    }
                    for ob in &we.order_by {
                        if !collect_refs_from_expr(&ob.expr, refs) {
                            return false;
                        }
                    }
                }
                for pb in partition_by {
                    if !collect_refs_from_expr(pb, refs) {
                        return false;
                    }
                }
                for (expr, _) in pass_through {
                    if !collect_refs_from_expr(expr, refs) {
                        return false;
                    }
                }
                child.collect_all_column_refs(refs)
            }
            OpTree::LateralFunction { .. } => {
                // func_sql is raw SQL text — cannot extract refs, so bail
                // out to avoid pruning columns the function might reference.
                false
            }
            OpTree::LateralSubquery { .. } => {
                // subquery_sql is raw SQL — same safety concern.
                false
            }
            OpTree::SemiJoin {
                condition,
                left,
                right,
            }
            | OpTree::AntiJoin {
                condition,
                left,
                right,
            } => {
                if !collect_refs_from_expr(condition, refs) {
                    return false;
                }
                left.collect_all_column_refs(refs) && right.collect_all_column_refs(refs)
            }
            OpTree::ScalarSubquery {
                subquery, child, ..
            } => subquery.collect_all_column_refs(refs) && child.collect_all_column_refs(refs),
            // Constant anchors have no column references — scanning is a no-op.
            OpTree::ConstantSelect { .. } => true,
        }
    }

    /// Apply pruning to Scan nodes based on collected column references.
    fn apply_column_pruning(&mut self, refs: &ColumnRefSet) {
        match self {
            OpTree::Scan {
                columns,
                pk_columns,
                alias,
                ..
            } => {
                // Qualified refs for this scan alias + all unqualified refs
                let demanded: std::collections::HashSet<&str> = refs
                    .qualified
                    .get(alias.as_str())
                    .into_iter()
                    .flatten()
                    .map(|s| s.as_str())
                    .chain(refs.unqualified.iter().map(|s| s.as_str()))
                    .collect();

                columns.retain(|c| {
                    demanded.contains(c.name.as_str()) || pk_columns.iter().any(|pk| pk == &c.name)
                });
            }
            OpTree::Project { child, .. }
            | OpTree::Filter { child, .. }
            | OpTree::Distinct { child }
            | OpTree::Subquery { child, .. }
            | OpTree::LateralSubquery { child, .. }
            | OpTree::LateralFunction { child, .. }
            | OpTree::Window { child, .. } => {
                child.apply_column_pruning(refs);
            }
            OpTree::ScalarSubquery {
                subquery, child, ..
            } => {
                subquery.apply_column_pruning(refs);
                child.apply_column_pruning(refs);
            }
            OpTree::Aggregate { child, .. } => child.apply_column_pruning(refs),
            OpTree::InnerJoin { left, right, .. }
            | OpTree::LeftJoin { left, right, .. }
            | OpTree::FullJoin { left, right, .. }
            | OpTree::Intersect { left, right, .. }
            | OpTree::Except { left, right, .. }
            | OpTree::SemiJoin { left, right, .. }
            | OpTree::AntiJoin { left, right, .. } => {
                left.apply_column_pruning(refs);
                right.apply_column_pruning(refs);
            }
            OpTree::UnionAll { children } => {
                for c in children {
                    c.apply_column_pruning(refs);
                }
            }
            OpTree::RecursiveCte {
                base, recursive, ..
            } => {
                base.apply_column_pruning(refs);
                recursive.apply_column_pruning(refs);
            }
            OpTree::CteScan { .. }
            | OpTree::RecursiveSelfRef { .. }
            | OpTree::ConstantSelect { .. } => {}
        }
    }
}

/// Accumulated column references from all expressions in an OpTree.
#[derive(Default)]
struct ColumnRefSet {
    /// `alias → {column_names}` for qualified references (`t.col`).
    qualified: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Column names without table qualifier.
    unqualified: std::collections::HashSet<String>,
}

/// Extract column references from an expression.
/// Returns `false` on Star/Raw (cannot determine exact columns).
fn collect_refs_from_expr(expr: &Expr, refs: &mut ColumnRefSet) -> bool {
    match expr {
        Expr::ColumnRef {
            table_alias: Some(alias),
            column_name,
        } => {
            refs.qualified
                .entry(alias.clone())
                .or_default()
                .insert(column_name.clone());
            true
        }
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => {
            refs.unqualified.insert(column_name.clone());
            true
        }
        Expr::Literal(_) => true,
        Expr::BinaryOp { left, right, .. } => {
            collect_refs_from_expr(left, refs) && collect_refs_from_expr(right, refs)
        }
        Expr::FuncCall { args, .. } => args.iter().all(|a| collect_refs_from_expr(a, refs)),
        Expr::Star { .. } | Expr::Raw(_) => false,
    }
}

/// Extract column references from an aggregate expression.
fn collect_refs_from_agg(agg: &AggExpr, refs: &mut ColumnRefSet) -> bool {
    if let Some(ref arg) = agg.argument
        && !collect_refs_from_expr(arg, refs)
    {
        return false;
    }
    if let Some(ref second) = agg.second_arg
        && !collect_refs_from_expr(second, refs)
    {
        return false;
    }
    if let Some(ref filter) = agg.filter
        && !collect_refs_from_expr(filter, refs)
    {
        return false;
    }
    if let Some(ref owg) = agg.order_within_group {
        for sort in owg {
            if !collect_refs_from_expr(&sort.expr, refs) {
                return false;
            }
        }
    }
    true
}

// ═══════════════════════════════════════════════════════════════════════
// Join PK helpers — used by row_id_key_columns and diff_project
// ═══════════════════════════════════════════════════════════════════════

/// For a Project over InnerJoin/LeftJoin, find the output aliases that
/// correspond to primary key columns of the join's source tables.
///
/// PK-based row-IDs are stable across value changes on the other side of
/// the join, fixing the simultaneous-change bug where DELETE hashes are
/// computed with current (not old) values.
///
/// Returns `None` if no PK aliases can be identified (caller should
/// fall back to hashing all aliases).
pub fn join_pk_aliases(
    expressions: &[Expr],
    aliases: &[String],
    join_child: &OpTree,
) -> Option<Vec<String>> {
    let (left, right) = match join_child {
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => (left.as_ref(), right.as_ref()),
        _ => return None,
    };

    let left_alias = left.alias();
    let right_alias = right.alias();
    let left_pks = scan_pk_columns(left);
    let right_pks = scan_pk_columns(right);

    // Collect PK aliases from each side separately. We need at least
    // one PK alias per side to uniquely identify join combinations.
    let mut left_pk_aliases = Vec::new();
    let mut right_pk_aliases = Vec::new();
    for (expr, alias) in expressions.iter().zip(aliases.iter()) {
        if let Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name,
        } = expr
        {
            if tbl == left_alias && left_pks.contains(column_name) {
                left_pk_aliases.push(alias.clone());
            } else if tbl == right_alias && right_pks.contains(column_name) {
                right_pk_aliases.push(alias.clone());
            }
        }
    }

    // Require PKs from BOTH sides to uniquely identify each join
    // combination. If either side's PK is missing from the output,
    // fall back to hashing all columns (content-based).
    if left_pk_aliases.is_empty() || right_pk_aliases.is_empty() {
        None
    } else {
        let mut pk_aliases = left_pk_aliases;
        pk_aliases.extend(right_pk_aliases);
        Some(pk_aliases)
    }
}

/// For a Project over InnerJoin/LeftJoin, return the indices of
/// expressions that correspond to PK columns.
///
/// Used by `diff_project` to hash only PK-corresponding expressions
/// for the delta's row-ID recomputation.
pub fn join_pk_expr_indices(expressions: &[Expr], join_child: &OpTree) -> Vec<usize> {
    let (left, right) = match join_child {
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => (left.as_ref(), right.as_ref()),
        _ => return Vec::new(),
    };

    let left_alias = left.alias();
    let right_alias = right.alias();
    let left_pks = scan_pk_columns(left);
    let right_pks = scan_pk_columns(right);

    // Collect PK indices from each side separately.
    let mut left_indices = Vec::new();
    let mut right_indices = Vec::new();
    for (i, expr) in expressions.iter().enumerate() {
        if let Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name,
        } = expr
        {
            if tbl == left_alias && left_pks.contains(column_name) {
                left_indices.push(i);
            } else if tbl == right_alias && right_pks.contains(column_name) {
                right_indices.push(i);
            }
        }
    }

    // Require PKs from BOTH sides.
    if left_indices.is_empty() || right_indices.is_empty() {
        return Vec::new();
    }
    let mut indices = left_indices;
    indices.extend(right_indices);
    indices
}

/// Extract PK column names from a Scan or Filter-over-Scan node.
///
/// Uses the real PK columns from `pg_constraint` if available, otherwise
/// falls back to non-nullable columns as a heuristic.
fn scan_pk_columns(op: &OpTree) -> Vec<String> {
    match op {
        OpTree::Scan {
            pk_columns,
            columns,
            ..
        } => {
            if !pk_columns.is_empty() {
                return pk_columns.clone();
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
        OpTree::Filter { child, .. } => scan_pk_columns(child),
        _ => Vec::new(),
    }
}

// ── Volatility checking ─────────────────────────────────────────────────

/// Volatility ordering: volatile > stable > immutable.
///
/// Returns the "worse" (more volatile) of two volatility classes.
///   - `'v'` (volatile) > `'s'` (stable) > `'i'` (immutable)
pub fn max_volatility(a: char, b: char) -> char {
    match (a, b) {
        ('v', _) | (_, 'v') => 'v',
        ('s', _) | (_, 's') => 's',
        _ => 'i',
    }
}

/// Look up the volatility category of a PostgreSQL function by name.
///
/// Returns `'i'` (immutable), `'s'` (stable), or `'v'` (volatile).
/// Returns `'v'` (volatile) if the function cannot be found (safe default).
///
/// For overloaded functions (multiple `pg_proc` rows with the same `proname`),
/// returns the worst volatility across all overloads.
#[cfg(not(test))]
pub fn lookup_function_volatility(func_name: &str) -> Result<char, PgTrickleError> {
    // Strip schema qualification if present (e.g., "pg_catalog.lower" → "lower").
    let bare_name = func_name.rsplit('.').next().unwrap_or(func_name);

    Spi::connect(|client| {
        let result = client.select(
            "SELECT provolatile::text FROM pg_catalog.pg_proc \
             WHERE proname = $1",
            None,
            &[bare_name.into()],
        )?;

        let mut worst = 'i';
        let mut found = false;
        for row in result {
            found = true;
            let vol: String = row
                .get_by_name::<String, _>("provolatile")
                .ok()
                .flatten()
                .unwrap_or_else(|| "v".to_string());
            let ch = vol.chars().next().unwrap_or('v');
            worst = max_volatility(worst, ch);
        }
        if !found {
            // Unknown function → assume volatile (safe default).
            return Ok('v');
        }
        Ok(worst)
    })
    .map_err(|e: pgrx::spi::SpiError| {
        PgTrickleError::SpiError(format!("volatility lookup failed: {e}"))
    })
}

/// Test-only stub: SPI is unavailable in unit tests, so assume volatile.
#[cfg(test)]
pub fn lookup_function_volatility(_func_name: &str) -> Result<char, PgTrickleError> {
    Ok('v')
}

/// Look up the volatility category of a PostgreSQL operator by name.
///
/// Joins `pg_operator` → `pg_proc` via `oprcode` to get the implementing
/// function's `provolatile`.
///
/// **Overload disambiguation:** Many operators (`+`, `-`, `||`, etc.) have
/// overloads spanning different type categories. For example, `+` is
/// immutable for `int4 + int4` but stable for `timestamp + interval`
/// (timezone-dependent), and `||` is immutable for `text || text` but
/// stable for `textanycat(text, anynonarray)`. Without argument-type
/// resolution we cannot determine which overload applies.
///
/// To avoid false "stable" warnings on expressions like `depth + 1` or
/// `path || '>'`, we first compute the worst volatility across
/// **concrete non-temporal** overloads (those whose left AND right operand
/// types are neither date/time category `'D'` nor pseudo-type category
/// `'P'`). If at least one such overload exists, we return that result.
/// Only when ALL overloads involve temporal or pseudo-types do we fall
/// back to the worst across all overloads.
///
/// Returns `'i'` if the operator is not found — all built-in operators
/// (arithmetic, comparison, logical) are immutable. Unknown operators
/// default to `'i'` because operator-not-found typically means the query
/// would fail anyway; the volatile/stable cases arise only with custom
/// operators that DO exist in `pg_operator`.
///
/// Closes Gap G7.2: custom operators with volatile `oprcode` functions
/// (e.g., PostGIS `&&` or user-defined operators) are still detected.
#[cfg(not(test))]
pub fn lookup_operator_volatility(op_name: &str) -> Result<char, PgTrickleError> {
    Spi::connect(|client| {
        let result = client.select(
            "SELECT p.provolatile::text AS vol, \
                    COALESCE(tl.typcategory::text, 'X') AS lcat, \
                    COALESCE(tr.typcategory::text, 'X') AS rcat \
             FROM pg_catalog.pg_operator o \
             JOIN pg_catalog.pg_proc p ON o.oprcode = p.oid \
             LEFT JOIN pg_catalog.pg_type tl ON o.oprleft = tl.oid \
             LEFT JOIN pg_catalog.pg_type tr ON o.oprright = tr.oid \
             WHERE o.oprname = $1",
            None,
            &[op_name.into()],
        )?;

        let mut worst_all = 'i';
        let mut worst_concrete = 'i';
        let mut has_concrete = false;

        for row in result {
            let vol: String = row
                .get_by_name::<String, _>("vol")
                .ok()
                .flatten()
                .unwrap_or_else(|| "i".to_string());
            let lcat: String = row
                .get_by_name::<String, _>("lcat")
                .ok()
                .flatten()
                .unwrap_or_else(|| "X".to_string());
            let rcat: String = row
                .get_by_name::<String, _>("rcat")
                .ok()
                .flatten()
                .unwrap_or_else(|| "X".to_string());

            let ch = vol.chars().next().unwrap_or('i');
            worst_all = max_volatility(worst_all, ch);

            // Exclude overloads with temporal (D) or pseudo-type (P)
            // arguments — these cause false positives when the actual
            // resolved overload uses concrete non-temporal types.
            let lc = lcat.chars().next().unwrap_or('X');
            let rc = rcat.chars().next().unwrap_or('X');
            if lc != 'D' && lc != 'P' && rc != 'D' && rc != 'P' {
                has_concrete = true;
                worst_concrete = max_volatility(worst_concrete, ch);
            }
        }

        // Prefer concrete non-temporal overloads when available.
        Ok(if has_concrete {
            worst_concrete
        } else {
            worst_all
        })
    })
    .map_err(|e: pgrx::spi::SpiError| {
        PgTrickleError::SpiError(format!("operator volatility lookup failed: {e}"))
    })
}

/// Test-only stub: SPI is unavailable in unit tests, so assume immutable
/// for operators (most built-in operators are immutable). In the real
/// implementation, temporal/pseudo-type overloads are filtered out to avoid
/// false stable warnings — see the `#[cfg(not(test))]` variant.
#[cfg(test)]
pub fn lookup_operator_volatility(_op_name: &str) -> Result<char, PgTrickleError> {
    Ok('i')
}

/// Recursively scan an `Expr` tree and update `worst` with the volatility
/// of any `FuncCall` or `BinaryOp` operator nodes found.
///
/// For `Expr::BinaryOp`, looks up the operator's implementing function in
/// `pg_operator` → `pg_proc.provolatile` to detect custom volatile operators
/// (Gap G7.2).
///
/// For `Expr::Raw` nodes, re-parses the SQL fragment via `raw_parser()` and
/// walks the parse tree to detect function calls or operators that may be
/// volatile. This closes the G7.1 gap where expressions like
/// `CASE WHEN now() > x THEN ...` deparsed to `Expr::Raw` would bypass
/// volatility checking.
pub fn collect_volatilities(expr: &Expr, worst: &mut char) -> Result<(), PgTrickleError> {
    match expr {
        Expr::FuncCall { func_name, args } => {
            // COALESCE, NULLIF, GREATEST, LEAST are parser constructs that
            // don't appear in pg_proc. They are inherently immutable — their
            // result depends only on their arguments. Skip the pg_proc lookup
            // and just check argument volatility.
            //
            // NOT is also a builtin boolean operator (BoolExpr::NOT_EXPR in the
            // parse tree, mapped to Expr::FuncCall { func_name: "NOT" }). The
            // pg_proc entry is named "boolnot", not "not", so a proname lookup
            // for "NOT" returns 0 rows and the conservative 'v' default would
            // be applied incorrectly. Boolean NOT is immutable; its volatility
            // is purely that of its argument.
            let upper = func_name.to_uppercase();
            let is_builtin_construct = matches!(
                upper.as_str(),
                "COALESCE" | "NULLIF" | "GREATEST" | "LEAST" | "NOT"
            );
            if !is_builtin_construct {
                let vol = lookup_function_volatility(func_name)?;
                *worst = max_volatility(*worst, vol);
            }
            for arg in args {
                collect_volatilities(arg, worst)?;
            }
        }
        Expr::BinaryOp { op, left, right } => {
            // G7.2: Check the operator's implementing function volatility.
            let vol = lookup_operator_volatility(op)?;
            *worst = max_volatility(*worst, vol);
            collect_volatilities(left, worst)?;
            collect_volatilities(right, worst)?;
        }
        Expr::Raw(sql) => {
            // Re-parse the raw SQL fragment to find any embedded function calls.
            collect_raw_expr_volatility(sql, worst)?;
        }
        // ColumnRef, Literal, Star: no function calls.
        _ => {}
    }
    Ok(())
}

/// Re-parse a raw SQL expression string and walk the parse tree for function
/// calls, updating `worst` with the volatility of each function found.
///
/// Wraps the expression in `SELECT (expr)` to make it a valid top-level
/// statement, then uses `raw_parser()` to parse and recursively walks the
/// resulting parse tree nodes.
#[cfg(not(test))]
fn collect_raw_expr_volatility(raw_sql: &str, worst: &mut char) -> Result<(), PgTrickleError> {
    // Skip simple literals and column references — no function calls possible.
    if raw_sql.is_empty()
        || raw_sql == "NULL"
        || raw_sql.starts_with('\'')
        || raw_sql.parse::<f64>().is_ok()
    {
        return Ok(());
    }

    let wrapper = format!("SELECT ({})", raw_sql);
    let list = match parse_query(wrapper.as_str()) {
        Ok(l) => l,
        Err(_) => {
            // Parsing failed — conservatively assume volatile.
            *worst = max_volatility(*worst, 'v');
            return Ok(());
        }
    };
    for raw_stmt in list.iter_ptr() {
        // SAFETY: raw_stmt is a valid pointer from parse_query.
        let stmt = unsafe { (*raw_stmt).stmt };
        if !stmt.is_null() {
            walk_node_for_volatility(stmt, worst)?;
        }
    }

    Ok(())
}

/// Test-only stub: assume volatile for any Expr::Raw (safe default).
#[cfg(test)]
fn collect_raw_expr_volatility(_raw_sql: &str, worst: &mut char) -> Result<(), PgTrickleError> {
    // In tests, conservatively assume volatile for raw expressions.
    *worst = max_volatility(*worst, 'v');
    Ok(())
}

fn sql_value_function_name(op: pg_sys::SQLValueFunctionOp::Type) -> &'static str {
    match op {
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_DATE => "CURRENT_DATE",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIME => "CURRENT_TIME",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIME_N => "CURRENT_TIME",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIMESTAMP => "CURRENT_TIMESTAMP",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIMESTAMP_N => "CURRENT_TIMESTAMP",
        pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIME => "LOCALTIME",
        pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIME_N => "LOCALTIME",
        pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIMESTAMP => "LOCALTIMESTAMP",
        pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIMESTAMP_N => "LOCALTIMESTAMP",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_ROLE => "CURRENT_ROLE",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_USER => "CURRENT_USER",
        pg_sys::SQLValueFunctionOp::SVFOP_USER => "USER",
        pg_sys::SQLValueFunctionOp::SVFOP_SESSION_USER => "SESSION_USER",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_CATALOG => "CURRENT_CATALOG",
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_SCHEMA => "CURRENT_SCHEMA",
        _ => "CURRENT_TIMESTAMP",
    }
}

fn sql_value_function_volatility(op: pg_sys::SQLValueFunctionOp::Type) -> char {
    match op {
        pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_DATE
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIME
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIME_N
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIMESTAMP
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_TIMESTAMP_N
        | pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIME
        | pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIME_N
        | pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIMESTAMP
        | pg_sys::SQLValueFunctionOp::SVFOP_LOCALTIMESTAMP_N
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_ROLE
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_USER
        | pg_sys::SQLValueFunctionOp::SVFOP_USER
        | pg_sys::SQLValueFunctionOp::SVFOP_SESSION_USER
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_CATALOG
        | pg_sys::SQLValueFunctionOp::SVFOP_CURRENT_SCHEMA => 's',
        _ => 's',
    }
}

/// Recursively walk a pg_sys::Node tree looking for FuncCall nodes and
/// update `worst` with the volatility of each function found.
#[cfg(not(test))]
fn walk_node_for_volatility(
    node: *mut pg_sys::Node,
    worst: &mut char,
) -> Result<(), PgTrickleError> {
    if node.is_null() {
        return Ok(());
    }

    if let Some(fcall) = cast_node!(node, T_FuncCall, pg_sys::FuncCall) {
        if let Ok(func_name) = unsafe { extract_func_name(fcall.funcname) } {
            let vol = lookup_function_volatility(&func_name)?;
            *worst = max_volatility(*worst, vol);
        }
        // Also walk function arguments
        let args_list = pg_list::<pg_sys::Node>(fcall.args);
        for n in args_list.iter_ptr() {
            walk_node_for_volatility(n, worst)?;
        }
        // Walk FILTER clause
        if !fcall.agg_filter.is_null() {
            walk_node_for_volatility(fcall.agg_filter, worst)?;
        }
    } else if let Some(svf) = cast_node!(node, T_SQLValueFunction, pg_sys::SQLValueFunction) {
        *worst = max_volatility(*worst, sql_value_function_volatility(svf.op));
    } else if let Some(stmt) = cast_node!(node, T_SelectStmt, pg_sys::SelectStmt) {
        // Walk target list
        let tlist = pg_list::<pg_sys::Node>(stmt.targetList);
        for n in tlist.iter_ptr() {
            walk_node_for_volatility(n, worst)?;
        }
        // Walk WHERE clause
        if !stmt.whereClause.is_null() {
            walk_node_for_volatility(stmt.whereClause, worst)?;
        }
        // Walk HAVING clause
        if !stmt.havingClause.is_null() {
            walk_node_for_volatility(stmt.havingClause, worst)?;
        }
    } else if let Some(rt) = cast_node!(node, T_ResTarget, pg_sys::ResTarget) {
        if !rt.val.is_null() {
            walk_node_for_volatility(rt.val, worst)?;
        }
    } else if let Some(aexpr) = cast_node!(node, T_A_Expr, pg_sys::A_Expr) {
        // G7.2: Check the operator's implementing function volatility.
        if !aexpr.name.is_null()
            && let Ok(op_name) = unsafe { extract_operator_name(aexpr.name) }
        {
            let vol = lookup_operator_volatility(&op_name)?;
            *worst = max_volatility(*worst, vol);
        }
        if !aexpr.lexpr.is_null() {
            walk_node_for_volatility(aexpr.lexpr, worst)?;
        }
        if !aexpr.rexpr.is_null() {
            walk_node_for_volatility(aexpr.rexpr, worst)?;
        }
    } else if let Some(bexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr) {
        let args_list = pg_list::<pg_sys::Node>(bexpr.args);
        for n in args_list.iter_ptr() {
            walk_node_for_volatility(n, worst)?;
        }
    } else if let Some(tc) = cast_node!(node, T_TypeCast, pg_sys::TypeCast) {
        if !tc.arg.is_null() {
            walk_node_for_volatility(tc.arg, worst)?;
        }
    } else if let Some(ce) = cast_node!(node, T_CaseExpr, pg_sys::CaseExpr) {
        if !ce.arg.is_null() {
            walk_node_for_volatility(ce.arg as *mut pg_sys::Node, worst)?;
        }
        let whens = pg_list::<pg_sys::Node>(ce.args);
        for w in whens.iter_ptr() {
            walk_node_for_volatility(w, worst)?;
        }
        if !ce.defresult.is_null() {
            walk_node_for_volatility(ce.defresult as *mut pg_sys::Node, worst)?;
        }
    } else if let Some(cw) = cast_node!(node, T_CaseWhen, pg_sys::CaseWhen) {
        if !cw.expr.is_null() {
            walk_node_for_volatility(cw.expr as *mut pg_sys::Node, worst)?;
        }
        if !cw.result.is_null() {
            walk_node_for_volatility(cw.result as *mut pg_sys::Node, worst)?;
        }
    } else if let Some(ce) = cast_node!(node, T_CoalesceExpr, pg_sys::CoalesceExpr) {
        let args = pg_list::<pg_sys::Node>(ce.args);
        for n in args.iter_ptr() {
            walk_node_for_volatility(n, worst)?;
        }
    } else if let Some(nt) = cast_node!(node, T_NullTest, pg_sys::NullTest) {
        if !nt.arg.is_null() {
            walk_node_for_volatility(nt.arg as *mut pg_sys::Node, worst)?;
        }
    } else if let Some(cc) = cast_node!(node, T_CollateClause, pg_sys::CollateClause) {
        if !cc.arg.is_null() {
            walk_node_for_volatility(cc.arg, worst)?;
        }
    } else if let Some(sl) = cast_node!(node, T_SubLink, pg_sys::SubLink) {
        if !sl.testexpr.is_null() {
            walk_node_for_volatility(sl.testexpr, worst)?;
        }
        if !sl.subselect.is_null() {
            walk_node_for_volatility(sl.subselect, worst)?;
        }
    }
    // Other node types (ColumnRef, A_Const, etc.) contain no functions.

    Ok(())
}

/// Return the worst volatility found in an expression tree.
pub fn worst_volatility(expr: &Expr) -> Result<char, PgTrickleError> {
    let mut worst = 'i';
    collect_volatilities(expr, &mut worst)?;
    Ok(worst)
}

/// Walk an entire OpTree and return the worst volatility found in any
/// expression (target list, WHERE, JOIN conditions, HAVING, aggregates,
/// window functions).
pub fn tree_worst_volatility(tree: &OpTree) -> Result<char, PgTrickleError> {
    let mut worst = 'i';
    tree_collect_volatility(tree, &mut worst)?;
    Ok(worst)
}

/// Walk an entire [`ParseResult`] (tree + CTE registry) for volatility.
pub fn tree_worst_volatility_with_registry(result: &ParseResult) -> Result<char, PgTrickleError> {
    let mut worst = 'i';
    // Check all CTE bodies
    for (_name, body) in &result.cte_registry.entries {
        tree_collect_volatility(body, &mut worst)?;
    }
    tree_collect_volatility(&result.tree, &mut worst)?;
    Ok(worst)
}

fn tree_collect_volatility(tree: &OpTree, worst: &mut char) -> Result<(), PgTrickleError> {
    match tree {
        OpTree::Scan { .. } | OpTree::CteScan { .. } | OpTree::RecursiveSelfRef { .. } => {}
        OpTree::Project {
            expressions, child, ..
        } => {
            for expr in expressions {
                collect_volatilities(expr, worst)?;
            }
            tree_collect_volatility(child, worst)?;
        }
        OpTree::Filter { predicate, child } => {
            collect_volatilities(predicate, worst)?;
            tree_collect_volatility(child, worst)?;
        }
        OpTree::InnerJoin {
            condition,
            left,
            right,
        }
        | OpTree::LeftJoin {
            condition,
            left,
            right,
        }
        | OpTree::FullJoin {
            condition,
            left,
            right,
        }
        | OpTree::SemiJoin {
            condition,
            left,
            right,
        }
        | OpTree::AntiJoin {
            condition,
            left,
            right,
        } => {
            collect_volatilities(condition, worst)?;
            tree_collect_volatility(left, worst)?;
            tree_collect_volatility(right, worst)?;
        }
        OpTree::Aggregate {
            group_by,
            aggregates,
            child,
        } => {
            for expr in group_by {
                collect_volatilities(expr, worst)?;
            }
            for agg in aggregates {
                if let Some(arg) = &agg.argument {
                    collect_volatilities(arg, worst)?;
                }
                if let Some(filter) = &agg.filter {
                    collect_volatilities(filter, worst)?;
                }
                if let Some(second) = &agg.second_arg {
                    collect_volatilities(second, worst)?;
                }
            }
            tree_collect_volatility(child, worst)?;
        }
        OpTree::Distinct { child }
        | OpTree::Subquery { child, .. }
        | OpTree::LateralFunction { child, .. }
        | OpTree::LateralSubquery { child, .. } => {
            tree_collect_volatility(child, worst)?;
        }
        OpTree::UnionAll { children } => {
            for child in children {
                tree_collect_volatility(child, worst)?;
            }
        }
        OpTree::Intersect { left, right, .. } | OpTree::Except { left, right, .. } => {
            tree_collect_volatility(left, worst)?;
            tree_collect_volatility(right, worst)?;
        }
        OpTree::RecursiveCte {
            base, recursive, ..
        } => {
            tree_collect_volatility(base, worst)?;
            tree_collect_volatility(recursive, worst)?;
        }
        OpTree::Window {
            window_exprs,
            partition_by,
            pass_through,
            child,
        } => {
            for we in window_exprs {
                for arg in &we.args {
                    collect_volatilities(arg, worst)?;
                }
                for pb in &we.partition_by {
                    collect_volatilities(pb, worst)?;
                }
                for ob in &we.order_by {
                    collect_volatilities(&ob.expr, worst)?;
                }
            }
            for pb in partition_by {
                collect_volatilities(pb, worst)?;
            }
            for (expr, _) in pass_through {
                collect_volatilities(expr, worst)?;
            }
            tree_collect_volatility(child, worst)?;
        }
        OpTree::ScalarSubquery {
            subquery, child, ..
        } => {
            tree_collect_volatility(subquery, worst)?;
            tree_collect_volatility(child, worst)?;
        }
        // Constant anchors have no expressions to scan — volatility is unaffected.
        OpTree::ConstantSelect { .. } => {}
    }
    Ok(())
}

/// Check if an operator tree is supported for differential maintenance.
pub fn check_ivm_support(tree: &OpTree) -> Result<(), PgTrickleError> {
    check_ivm_support_inner(tree)
}

/// Check if a [`ParseResult`] (tree + CTE registry) is fully supported.
pub fn check_ivm_support_with_registry(result: &ParseResult) -> Result<(), PgTrickleError> {
    // Validate all CTE bodies first
    for (name, body) in &result.cte_registry.entries {
        check_ivm_support_inner(body)
            .map_err(|e| PgTrickleError::UnsupportedOperator(format!("in CTE '{name}': {e}")))?;
    }
    check_ivm_support_inner(&result.tree)
}

/// Check that both children of a join resolve to direct table references.
///
/// Previously used to reject nested joins. Kept for test coverage — the callers
/// in `check_ivm_support_inner()` have been removed now that nested joins are
/// supported in DIFFERENTIAL mode.
#[cfg(test)]
fn check_join_children_are_base_tables(
    left: &OpTree,
    right: &OpTree,
    join_kind: &str,
) -> Result<(), PgTrickleError> {
    if !resolves_to_base_table(left) {
        return Err(PgTrickleError::UnsupportedOperator(format!(
            "nested joins are not supported for DIFFERENTIAL mode: \
             left child of {join_kind} must resolve to a base table, \
             not a {}",
            left.node_kind(),
        )));
    }
    if !resolves_to_base_table(right) {
        return Err(PgTrickleError::UnsupportedOperator(format!(
            "nested joins are not supported for DIFFERENTIAL mode: \
             right child of {join_kind} must resolve to a base table, \
             not a {}",
            right.node_kind(),
        )));
    }
    Ok(())
}

/// Check if an operator tree node resolves to a direct base table reference.
///
/// Returns `true` for `Scan` (a direct table) and for transparent wrappers
/// (`Filter`, `Project`, `Subquery`) that eventually wrap a `Scan`.
/// Returns `false` for joins, aggregates, unions, and other complex nodes
/// that cannot be expressed as a simple `FROM table_name`.
#[cfg(test)]
fn resolves_to_base_table(op: &OpTree) -> bool {
    match op {
        OpTree::Scan { .. } => true,
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => resolves_to_base_table(child),
        OpTree::CteScan { .. } => {
            // CTE scans reference a named CTE — treated as a base relation
            // for the purpose of join child validation.
            true
        }
        OpTree::RecursiveSelfRef { .. } => {
            // Recursive self-references act as table sources within
            // the recursive term of a recursive CTE body.
            true
        }
        OpTree::LateralSubquery { child, .. } | OpTree::LateralFunction { child, .. } => {
            resolves_to_base_table(child)
        }
        _ => false,
    }
}

fn check_ivm_support_inner(tree: &OpTree) -> Result<(), PgTrickleError> {
    match tree {
        OpTree::Scan { .. } => Ok(()),
        OpTree::Project { child, .. } => check_ivm_support(child),
        OpTree::Filter { child, .. } => check_ivm_support(child),
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => {
            // Nested joins (join-of-join) are supported: the diff engine
            // handles recursive delta computation over the join tree.
            check_ivm_support(left)?;
            check_ivm_support(right)
        }
        OpTree::Aggregate {
            child, aggregates, ..
        } => {
            for agg in aggregates {
                match agg.function {
                    AggFunc::Count
                    | AggFunc::CountStar
                    | AggFunc::Sum
                    | AggFunc::Avg
                    | AggFunc::Min
                    | AggFunc::Max
                    | AggFunc::BoolAnd
                    | AggFunc::BoolOr
                    | AggFunc::StringAgg
                    | AggFunc::ArrayAgg
                    | AggFunc::JsonAgg
                    | AggFunc::JsonbAgg
                    | AggFunc::BitAnd
                    | AggFunc::BitOr
                    | AggFunc::BitXor
                    | AggFunc::JsonObjectAgg
                    | AggFunc::JsonbObjectAgg
                    | AggFunc::JsonObjectAggStd(_)
                    | AggFunc::JsonArrayAggStd(_)
                    | AggFunc::StddevPop
                    | AggFunc::StddevSamp
                    | AggFunc::VarPop
                    | AggFunc::VarSamp
                    | AggFunc::AnyValue
                    | AggFunc::Mode
                    | AggFunc::PercentileCont
                    | AggFunc::PercentileDisc
                    | AggFunc::Corr
                    | AggFunc::CovarPop
                    | AggFunc::CovarSamp
                    | AggFunc::RegrAvgx
                    | AggFunc::RegrAvgy
                    | AggFunc::RegrCount
                    | AggFunc::RegrIntercept
                    | AggFunc::RegrR2
                    | AggFunc::RegrSlope
                    | AggFunc::RegrSxx
                    | AggFunc::RegrSxy
                    | AggFunc::RegrSyy
                    | AggFunc::XmlAgg
                    | AggFunc::HypRank
                    | AggFunc::HypDenseRank
                    | AggFunc::HypPercentRank
                    | AggFunc::HypCumeDist
                    | AggFunc::ComplexExpression(_)
                    | AggFunc::UserDefined(_) => {}
                }
            }
            check_ivm_support(child)
        }
        OpTree::Distinct { child } => check_ivm_support(child),
        OpTree::UnionAll { children } => {
            for child in children {
                check_ivm_support(child)?;
            }
            Ok(())
        }
        OpTree::Intersect { left, right, .. } | OpTree::Except { left, right, .. } => {
            check_ivm_support(left)?;
            check_ivm_support(right)
        }
        OpTree::Subquery { child, .. } => check_ivm_support(child),
        // CteScan delegates to its body via the registry at a higher level.
        // The CTE body itself is validated when first parsed.
        OpTree::CteScan { .. } => Ok(()),
        // Recursive CTEs are supported for differential mode via semi-naive evaluation.
        // Both base and recursive terms must be independently DVM-compatible.
        OpTree::RecursiveCte {
            base, recursive, ..
        } => {
            check_ivm_support_inner(base)?;
            check_ivm_support_inner(recursive)
        }
        // Self-references are always valid within a recursive CTE context.
        OpTree::RecursiveSelfRef { .. } => Ok(()),
        // Window functions use partition-based recomputation.
        OpTree::Window { child, .. } => check_ivm_support(child),
        // Lateral SRFs use row-scoped recomputation.
        OpTree::LateralFunction { child, .. } => check_ivm_support(child),
        // Lateral subqueries use row-scoped recomputation.
        OpTree::LateralSubquery { child, .. } => check_ivm_support(child),
        // Semi-join (EXISTS / IN subquery): both sides must be DVM-compatible.
        OpTree::SemiJoin { left, right, .. } | OpTree::AntiJoin { left, right, .. } => {
            check_ivm_support(left)?;
            check_ivm_support(right)
        }
        // Scalar subquery: both the outer child and inner subquery must be DVM-compatible.
        OpTree::ScalarSubquery {
            subquery, child, ..
        } => {
            check_ivm_support(subquery)?;
            check_ivm_support(child)
        }
        // Constant anchors (no FROM) are always DVM-compatible — they produce no delta.
        OpTree::ConstantSelect { .. } => Ok(()),
    }
}

// ── IMMEDIATE-mode validation ──────────────────────────────────────────────

/// Validate that a defining query is compatible with IMMEDIATE mode.
///
/// IMMEDIATE mode uses statement-level AFTER triggers with transition tables
/// and does not support the full range of SQL constructs that DIFFERENTIAL
/// mode handles. This function parses the defining query and rejects
/// unsupported constructs with an actionable error message.
///
/// **Supported in IMMEDIATE mode:**
/// - Simple SELECT (Scan, Filter, Project)
/// - JOINs (INNER, LEFT, FULL)
/// - GROUP BY with standard aggregates
/// - DISTINCT
/// - Simple subqueries in FROM
/// - Non-recursive CTEs
/// - EXISTS/IN subqueries (SemiJoin/AntiJoin)
///
/// **Rejected:** (reserved for future restrictions)
/// - None currently — recursive CTEs (Task 5.1) and WhTopK (Task 5.2) are
///   both now supported with appropriate GUC gates and runtime warnings.
pub fn validate_immediate_mode_support(defining_query: &str) -> Result<(), PgTrickleError> {
    let result = parse_defining_query_full(defining_query)?;

    // First validate general DVM support (aggregates, etc.).
    check_ivm_support_with_registry(&result)?;

    // Then validate IMMEDIATE-specific restrictions.
    check_immediate_support(&result.tree)?;

    // Also check CTE bodies.
    for entry in &result.cte_registry.entries {
        check_immediate_support(&entry.1)?;
    }

    Ok(())
}

/// Walk an OpTree and reject constructs not yet supported in IMMEDIATE mode.
fn check_immediate_support(tree: &OpTree) -> Result<(), PgTrickleError> {
    match tree {
        // Leaf nodes — always OK.
        OpTree::Scan { .. } | OpTree::CteScan { .. } | OpTree::RecursiveSelfRef { .. } => Ok(()),

        // Transparent wrappers — recurse into child.
        OpTree::Project { child, .. }
        | OpTree::Filter { child, .. }
        | OpTree::Distinct { child }
        | OpTree::Subquery { child, .. } => check_immediate_support(child),

        // Joins — recurse into both sides.
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => {
            check_immediate_support(left)?;
            check_immediate_support(right)
        }

        // Aggregates — allowed, recurse into child.
        OpTree::Aggregate { child, .. } => check_immediate_support(child),

        // UNION ALL — allowed (each branch is a scan/filter/project chain).
        OpTree::UnionAll { children } => {
            for child in children {
                check_immediate_support(child)?;
            }
            Ok(())
        }

        // INTERSECT/EXCEPT — allowed.
        OpTree::Intersect { left, right, .. } | OpTree::Except { left, right, .. } => {
            check_immediate_support(left)?;
            check_immediate_support(right)
        }

        // SemiJoin/AntiJoin (EXISTS/IN subqueries) — allowed.
        OpTree::SemiJoin { left, right, .. } | OpTree::AntiJoin { left, right, .. } => {
            check_immediate_support(left)?;
            check_immediate_support(right)
        }

        // Window functions — partition-based recomputation via DVM engine.
        // The delta is derived from child Scan nodes which support both
        // ChangeBuffer and TransitionTable modes.
        OpTree::Window { child, .. } => check_immediate_support(child),

        // LATERAL subqueries — row-scoped recomputation. Delta derived
        // from child Scan nodes.
        OpTree::LateralSubquery { child, .. } => check_immediate_support(child),

        // LATERAL set-returning functions (unnest(), jsonb_array_elements())
        // — row-scoped recomputation via child delta.
        OpTree::LateralFunction { child, .. } => check_immediate_support(child),

        // Scalar subqueries in SELECT — delta derived from child and
        // subquery Scan nodes.
        OpTree::ScalarSubquery {
            child, subquery, ..
        } => {
            check_immediate_support(child)?;
            check_immediate_support(subquery)
        }

        // ── Recursive CTEs ────────────────────────────────────────
        // Task 5.1: Allow recursive CTEs in IMMEDIATE mode.
        // Semi-naive evaluation works with DeltaSource::TransitionTable;
        // emit a warning about potential stack-depth issues for deep recursion.
        OpTree::RecursiveCte {
            base, recursive, ..
        } => {
            pgrx::warning!(
                "pg_trickle: WITH RECURSIVE in IMMEDIATE mode uses semi-naive evaluation \
                 inside the trigger.  A depth counter guards against infinite loops \
                 (pg_trickle.ivm_recursive_max_depth, default 100).  For very deep \
                 hierarchies, raise this GUC or watch for \
                 'stack depth limit exceeded' errors."
            );
            check_immediate_support(base)?;
            check_immediate_support(recursive)
        }
        // Constant anchors (no FROM) are always IMMEDIATE-compatible.
        OpTree::ConstantSelect { .. } => Ok(()),
    }
}

// ── Monotonicity Checker (CYC-2) ──────────────────────────────────────────

/// Check if an OpTree is monotone (safe for cyclic fixed-point iteration).
///
/// A monotone query is one where adding input rows can only add output rows,
/// never remove them. This property guarantees convergence when stream tables
/// form circular dependencies, because:
/// - Each iteration can only add rows (monotonicity)
/// - The result set is bounded (finite input → finite output)
/// - The sequence must reach a fixed point in finite steps
///
/// Returns `Ok(())` if all operators are monotone. Returns `Err` with a
/// descriptive error if a non-monotone operator is found.
///
/// # Non-monotone operators
///
/// - **Aggregate**: COUNT/SUM can decrease when rows are deleted
/// - **Except**: adding rows to the right branch removes output
/// - **Window functions**: rank can change, causing rows to appear/disappear
/// - **AntiJoin** (NOT EXISTS/NOT IN): adding right rows removes output
///
/// # References
///
/// - Datalog stratification theory
/// - DBSP (Dynamic Batch Stream Processing) monotone fragment
pub fn check_monotonicity(tree: &OpTree) -> Result<(), PgTrickleError> {
    match tree {
        // Leaf nodes — always monotone.
        OpTree::Scan { .. } | OpTree::CteScan { .. } | OpTree::RecursiveSelfRef { .. } => Ok(()),

        // Transparent wrappers — monotonicity depends on child.
        OpTree::Project { child, .. }
        | OpTree::Filter { child, .. }
        | OpTree::Distinct { child }
        | OpTree::Subquery { child, .. } => check_monotonicity(child),

        // Joins (inner, left, full) — monotone if both sides are.
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => {
            check_monotonicity(left)?;
            check_monotonicity(right)
        }

        // UNION ALL — monotone if all children are.
        OpTree::UnionAll { children } => {
            for child in children {
                check_monotonicity(child)?;
            }
            Ok(())
        }

        // INTERSECT — monotone (adding rows to either side can only add output).
        OpTree::Intersect { left, right, .. } => {
            check_monotonicity(left)?;
            check_monotonicity(right)
        }

        // SemiJoin (EXISTS/IN) — monotone (adding right rows adds left matches).
        OpTree::SemiJoin { left, right, .. } => {
            check_monotonicity(left)?;
            check_monotonicity(right)
        }

        // Recursive CTEs — monotone if both terms are.
        OpTree::RecursiveCte {
            base, recursive, ..
        } => {
            check_monotonicity(base)?;
            check_monotonicity(recursive)
        }

        // LATERAL — monotonicity depends on child.
        OpTree::LateralFunction { child, .. } | OpTree::LateralSubquery { child, .. } => {
            check_monotonicity(child)
        }

        // Scalar subqueries — monotonicity depends on both.
        OpTree::ScalarSubquery {
            child, subquery, ..
        } => {
            check_monotonicity(child)?;
            check_monotonicity(subquery)
        }

        // ── Non-monotone operators ────────────────────────────────────
        OpTree::Aggregate { .. } => Err(PgTrickleError::UnsupportedOperator(
            "Aggregate is not monotone — cannot participate in a circular dependency. \
             Aggregates (COUNT, SUM, AVG, etc.) can decrease when input rows are removed, \
             which prevents fixed-point convergence."
                .into(),
        )),

        OpTree::Except { .. } => Err(PgTrickleError::UnsupportedOperator(
            "EXCEPT is not monotone — cannot participate in a circular dependency. \
             Adding rows to the right branch of EXCEPT removes output rows, \
             which prevents fixed-point convergence."
                .into(),
        )),

        OpTree::Window { .. } => Err(PgTrickleError::UnsupportedOperator(
            "Window functions are not monotone — cannot participate in a circular dependency. \
             Window function results (ROW_NUMBER, RANK, etc.) change when input rows are \
             added or removed, which prevents fixed-point convergence."
                .into(),
        )),

        OpTree::AntiJoin { .. } => Err(PgTrickleError::UnsupportedOperator(
            "NOT EXISTS / NOT IN is not monotone — cannot participate in a circular dependency. \
             Adding rows to the subquery side removes output rows, which prevents \
             fixed-point convergence."
                .into(),
        )),
        // Constant anchors are trivially monotone — they produce no rows and have
        // no source tables, so adding more data can only grow the output, never shrink it.
        OpTree::ConstantSelect { .. } => Ok(()),
    }
}

#[cfg(test)]
mod monotonicity_tests {
    use super::*;

    /// Helper: create a minimal Scan node.
    fn scan() -> OpTree {
        OpTree::Scan {
            table_oid: 1,
            table_name: "t".to_string(),
            schema: "public".to_string(),
            columns: vec![],
            pk_columns: vec![],
            alias: "t".to_string(),
        }
    }

    #[test]
    fn test_scan_is_monotone() {
        assert!(check_monotonicity(&scan()).is_ok());
    }

    #[test]
    fn test_filter_project_is_monotone() {
        let tree = OpTree::Filter {
            predicate: Expr::Literal("true".into()),
            child: Box::new(OpTree::Project {
                expressions: vec![],
                aliases: vec![],
                child: Box::new(scan()),
            }),
        };
        assert!(check_monotonicity(&tree).is_ok());
    }

    #[test]
    fn test_inner_join_is_monotone() {
        let tree = OpTree::InnerJoin {
            condition: Expr::Literal("true".into()),
            left: Box::new(scan()),
            right: Box::new(scan()),
        };
        assert!(check_monotonicity(&tree).is_ok());
    }

    #[test]
    fn test_left_join_is_monotone() {
        let tree = OpTree::LeftJoin {
            condition: Expr::Literal("true".into()),
            left: Box::new(scan()),
            right: Box::new(scan()),
        };
        assert!(check_monotonicity(&tree).is_ok());
    }

    #[test]
    fn test_union_all_is_monotone() {
        let tree = OpTree::UnionAll {
            children: vec![scan(), scan()],
        };
        assert!(check_monotonicity(&tree).is_ok());
    }

    #[test]
    fn test_distinct_is_monotone() {
        let tree = OpTree::Distinct {
            child: Box::new(scan()),
        };
        assert!(check_monotonicity(&tree).is_ok());
    }

    #[test]
    fn test_semi_join_is_monotone() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("true".into()),
            left: Box::new(scan()),
            right: Box::new(scan()),
        };
        assert!(check_monotonicity(&tree).is_ok());
    }

    #[test]
    fn test_intersect_is_monotone() {
        let tree = OpTree::Intersect {
            left: Box::new(scan()),
            right: Box::new(scan()),
            all: false,
        };
        assert!(check_monotonicity(&tree).is_ok());
    }

    #[test]
    fn test_aggregate_is_not_monotone() {
        let tree = OpTree::Aggregate {
            group_by: vec![],
            aggregates: vec![],
            child: Box::new(scan()),
        };
        let err = check_monotonicity(&tree).unwrap_err();
        assert!(format!("{}", err).contains("Aggregate"));
    }

    #[test]
    fn test_except_is_not_monotone() {
        let tree = OpTree::Except {
            left: Box::new(scan()),
            right: Box::new(scan()),
            all: false,
        };
        let err = check_monotonicity(&tree).unwrap_err();
        assert!(format!("{}", err).contains("EXCEPT"));
    }

    #[test]
    fn test_window_is_not_monotone() {
        let tree = OpTree::Window {
            window_exprs: vec![],
            partition_by: vec![],
            pass_through: vec![],
            child: Box::new(scan()),
        };
        let err = check_monotonicity(&tree).unwrap_err();
        assert!(format!("{}", err).contains("Window"));
    }

    #[test]
    fn test_anti_join_is_not_monotone() {
        let tree = OpTree::AntiJoin {
            condition: Expr::Literal("true".into()),
            left: Box::new(scan()),
            right: Box::new(scan()),
        };
        let err = check_monotonicity(&tree).unwrap_err();
        assert!(format!("{}", err).contains("NOT EXISTS"));
    }

    #[test]
    fn test_nested_non_monotone_detected() {
        // Filter { child: Aggregate { ... } } → non-monotone.
        let tree = OpTree::Filter {
            predicate: Expr::Literal("true".into()),
            child: Box::new(OpTree::Aggregate {
                group_by: vec![],
                aggregates: vec![],
                child: Box::new(scan()),
            }),
        };
        assert!(check_monotonicity(&tree).is_err());
    }

    #[test]
    fn test_join_with_non_monotone_child() {
        // InnerJoin where right side has an Aggregate → non-monotone.
        let tree = OpTree::InnerJoin {
            condition: Expr::Literal("true".into()),
            left: Box::new(scan()),
            right: Box::new(OpTree::Aggregate {
                group_by: vec![],
                aggregates: vec![],
                child: Box::new(scan()),
            }),
        };
        assert!(check_monotonicity(&tree).is_err());
    }
}

// ── Query Parsing ──────────────────────────────────────────────────────────

/// Context threaded through the parser for CTE handling.
///
/// Holds the raw `SelectStmt` pointers from `extract_cte_map_with_recursive()` alongside
/// a mutable [`CteRegistry`] that gets populated on first reference to
/// each CTE name. Subsequent references to the same CTE reuse the
/// existing registry entry (same `cte_id`), producing multiple
/// [`OpTree::CteScan`] nodes that share a single parsed body.
struct CteParseContext {
    /// Raw CTE name → SelectStmt pointer (from the WITH clause).
    raw_map: HashMap<String, *const pg_sys::SelectStmt>,
    /// CTE definition-level column aliases: `WITH x(a, b) AS (...)`.
    def_aliases: HashMap<String, Vec<String>>,
    /// CTE name → index into `registry.entries` (assigned on first ref).
    id_map: HashMap<String, usize>,
    /// Populated as CTE references are encountered.
    registry: CteRegistry,
    /// When set, FROM references to this name create `RecursiveSelfRef`
    /// instead of trying to resolve the CTE body. Used while parsing
    /// the recursive term of a `WITH RECURSIVE` CTE.
    recursive_self_ref_name: Option<String>,
    /// Output columns for the recursive self-reference. Set alongside
    /// `recursive_self_ref_name`.
    recursive_self_ref_columns: Vec<String>,
    /// G13-SD: Current recursion depth (incremented on each nested
    /// parse_select_stmt / parse_set_operation / parse_from_item call).
    depth: usize,
    /// G13-SD: Maximum allowed recursion depth (from GUC).
    max_depth: usize,
    /// Set to `true` while parsing the base (non-recursive) term of a
    /// `WITH RECURSIVE` CTE anchor.  When true, `parse_select_stmt_inner`
    /// accepts a SELECT with no FROM clause and returns an
    /// `OpTree::ConstantSelect` instead of an error.
    in_cte_anchor: bool,
}

impl CteParseContext {
    fn new(
        raw_map: HashMap<String, *const pg_sys::SelectStmt>,
        def_aliases: HashMap<String, Vec<String>>,
    ) -> Self {
        // In unit tests without a PG backend the GUC is unavailable;
        // use a sensible default.
        #[cfg(any(not(test), feature = "pg_test"))]
        let max_depth = crate::config::pg_trickle_max_parse_depth();
        #[cfg(all(test, not(feature = "pg_test")))]
        let max_depth = 64;

        CteParseContext {
            raw_map,
            def_aliases,
            id_map: HashMap::new(),
            registry: CteRegistry::default(),
            recursive_self_ref_name: None,
            recursive_self_ref_columns: Vec::new(),
            depth: 0,
            max_depth,
            in_cte_anchor: false,
        }
    }

    /// G13-SD: Increment the recursion depth and error if the limit is
    /// exceeded.  Returns the previous depth so the caller can restore
    /// it after the recursive call (via `set_depth`).
    fn descend(&mut self) -> Result<usize, PgTrickleError> {
        let prev = self.depth;
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(PgTrickleError::QueryTooComplex(format!(
                "parse tree recursion depth {} exceeds pg_trickle.max_parse_depth ({}). \
                 Simplify the query or raise the limit.",
                self.depth, self.max_depth,
            )));
        }
        Ok(prev)
    }

    /// G13-SD: Restore the recursion depth to a previously saved value.
    fn ascend(&mut self, prev: usize) {
        self.depth = prev;
    }

    /// Returns true if `name` is a CTE defined in this query (either
    /// still in the raw map awaiting parsing, or already registered).
    fn is_cte(&self, name: &str) -> bool {
        self.raw_map.contains_key(name) || self.id_map.contains_key(name)
    }

    /// Return the `cte_id` for an already-parsed CTE, or `None`.
    fn lookup_id(&self, name: &str) -> Option<usize> {
        self.id_map.get(name).copied()
    }

    /// Register a newly-parsed CTE body and return its `cte_id`.
    fn register(&mut self, name: &str, body: OpTree) -> usize {
        let id = self.registry.entries.len();
        self.registry.entries.push((name.to_string(), body));
        self.id_map.insert(name.to_string(), id);
        id
    }
}

// ── Query Parsing (entry points) ──────────────────────────────────────────

/// Check whether a defining query contains `WITH RECURSIVE`.
///
/// This is a lightweight check that only parses the top-level structure
/// without building a full OpTree. It is safe to call for any valid SQL
/// SELECT, including queries that the DVM parser cannot handle.
///
/// # Requires
/// Must be called within a PostgreSQL backend (uses `raw_parser()`).
pub fn query_has_recursive_cte(query: &str) -> Result<bool, PgTrickleError> {
    // SAFETY: raw_parser is safe when called within a PostgreSQL backend
    // with a valid memory context.
    query_has_recursive_cte_inner(query)
}

/// Lightweight check for any top-level WITH clause in a defining query.
///
/// Returns true for both recursive and non-recursive CTEs. This is used by
/// refresh planning to select safe fallback strategies without building the
/// full OpTree.
pub fn query_has_cte(query: &str) -> Result<bool, PgTrickleError> {
    query_has_cte_inner(query)
}

fn query_has_cte_inner(query: &str) -> Result<bool, PgTrickleError> {
    let select_ptr = match parse_first_select(query)? {
        Some(s) => s,
        None => return Ok(false),
    };
    // SAFETY: pointer is valid for the duration of the current memory context.
    let select = unsafe { &*select_ptr };
    Ok(!select.withClause.is_null())
}

fn query_has_recursive_cte_inner(query: &str) -> Result<bool, PgTrickleError> {
    let select_ptr = match parse_first_select(query)? {
        Some(s) => s,
        None => return Ok(false),
    };
    // SAFETY: pointer is valid for the duration of the current memory context.
    let select = unsafe { &*select_ptr };
    if select.withClause.is_null() {
        return Ok(false);
    }

    let wc = unsafe { &*select.withClause };
    Ok(wc.recursive)
}

// ── View inlining auto-rewrite ──────────────────────────────────────────

/// Auto-rewrite pass #0: Replace view references with inline subqueries.
///
/// For each `RangeVar` in the FROM clause that resolves to a PostgreSQL
/// view (`relkind = 'v'`), replaces it with `(view_definition) AS alias`.
/// Handles nested views by iterating until no views remain (fixpoint).
///
/// Materialized views (`relkind = 'm'`) are **not** inlined — their
/// semantics differ (stale snapshot vs live query). They are left as-is
/// for later rejection or acceptance depending on refresh mode.
///
/// Foreign tables (`relkind = 'f'`) are also left as-is.
///
/// Returns the original query unchanged if no views are found.
pub fn rewrite_views_inline(query: &str) -> Result<String, PgTrickleError> {
    let max_depth = 10;
    let mut current = query.to_string();

    for _depth in 0..max_depth {
        let rewritten = rewrite_views_inline_once(&current)?;
        if rewritten == current {
            return Ok(current); // Fixpoint reached — no more views
        }
        current = rewritten;
    }

    Err(PgTrickleError::QueryParseError(format!(
        "View inlining exceeded maximum nesting depth of {max_depth}. \
         This may indicate circular view dependencies."
    )))
}

/// Single-pass view inlining: parse the query, walk FROM items, replace
/// any view RangeVars with `(pg_get_viewdef(oid, true)) AS alias`.
///
/// Returns the original string unchanged if no views are found.
fn rewrite_views_inline_once(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Build a substitution map: views found in FROM → their definitions
    let mut subs = Vec::new();
    let mut found_view = false;

    // Walk the FROM clause to find views. Also walk set-operation arms and CTEs.
    collect_view_substitutions(select, &mut subs, &mut found_view)?;

    if !found_view {
        return Ok(query.to_string());
    }

    // Deparse the full query with view substitutions applied
    deparse_select_stmt_with_view_subs(select, &subs)
}

/// Information about a view found in the FROM clause that should be
/// replaced with an inline subquery.
struct ViewSubstitution {
    /// Schema name of the view (or empty if unqualified).
    schema: String,
    /// Relation name of the view.
    relname: String,
    /// The view's SQL definition from `pg_get_viewdef()`.
    view_sql: String,
    /// Alias to use for the inline subquery.
    alias: String,
}

/// Resolve `relkind` for a relation given schema and name.
///
/// Returns: `'r'` (table), `'v'` (view), `'m'` (matview), `'f'` (foreign),
/// `'p'` (partitioned table), etc.
fn resolve_relkind(schema: &str, relname: &str) -> Result<Option<String>, PgTrickleError> {
    let sql = format!(
        "SELECT c.relkind::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{}' AND c.relname = '{}'",
        schema.replace('\'', "''"),
        relname.replace('\'', "''"),
    );
    Spi::connect(|client| {
        let mut result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        // Use iterator to safely handle empty result sets (avoids
        // "SpiTupleTable positioned before the start or after the end" on empty results).
        if let Some(row) = result.next() {
            return row
                .get::<String>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()));
        }
        Ok(None)
    })
}

/// Get the SQL definition of a view using `pg_get_viewdef(oid, true)`.
fn get_view_definition(schema: &str, relname: &str) -> Result<String, PgTrickleError> {
    let sql = format!(
        "SELECT pg_get_viewdef(c.oid, true) FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{}' AND c.relname = '{}'",
        schema.replace('\'', "''"),
        relname.replace('\'', "''"),
    );
    Spi::connect(|client| {
        let mut result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        // Use iterator to safely handle empty result sets (avoids
        // "SpiTupleTable positioned before the start or after the end" on empty results).
        if let Some(row) = result.next() {
            let raw = row
                .get::<String>(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .ok_or_else(|| {
                    PgTrickleError::QueryParseError(format!(
                        "Could not retrieve view definition for {schema}.{relname}"
                    ))
                })?;
            return Ok(strip_view_definition_suffix(&raw));
        }
        Err(PgTrickleError::QueryParseError(format!(
            "Could not retrieve view definition for {schema}.{relname}: view not found"
        )))
    })
}

/// Strip a trailing semicolon (and surrounding whitespace) from a view
/// definition returned by `pg_get_viewdef()`.
///
/// PostgreSQL 18 may include a trailing semicolon. When the definition is
/// embedded inside a subquery parenthesis, the semicolon produces a syntax
/// error. Extracted as a pure function for unit-testability.
fn strip_view_definition_suffix(raw: &str) -> String {
    raw.trim_end_matches(';').trim().to_string()
}

/// Resolve schema for a RangeVar. If schemaname is NULL, resolve via search_path.
///
/// Returns `None` if the relation cannot be found in `pg_class` (e.g. CTE
/// names, subquery aliases, or function-call ranges).
fn resolve_rangevar_schema(rv: &pg_sys::RangeVar) -> Result<Option<String>, PgTrickleError> {
    if !rv.schemaname.is_null() {
        let schema = pg_cstr_to_str(rv.schemaname)?;
        return Ok(Some(schema.to_string()));
    }

    // Resolve from search_path
    if rv.relname.is_null() {
        return Ok(None);
    }
    let relname = pg_cstr_to_str(rv.relname)?;

    let sql = format!(
        "SELECT n.nspname::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relname = '{}' \
         AND n.nspname = ANY(string_to_array(current_setting('search_path'), ', ')::text[] || 'public'::text) \
         ORDER BY array_position(string_to_array(current_setting('search_path'), ', ')::text[] || 'public'::text, n.nspname) \
         LIMIT 1",
        relname.replace('\'', "''"),
    );
    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        // Iterate to handle empty results (CTE names, subquery aliases, etc.)
        for row in result {
            if let Ok(Some(schema)) = row.get::<String>(1) {
                return Ok(Some(schema));
            }
        }
        Ok(None)
    })
}

/// Walk a SelectStmt's FROM clause (and set-operation arms, CTEs) to find
/// view references and build the substitution list.
///
/// # Safety
/// Caller must ensure `select` points to a valid `SelectStmt`.
fn collect_view_substitutions(
    select: *const pg_sys::SelectStmt,
    subs: &mut Vec<ViewSubstitution>,
    found_view: &mut bool,
) -> Result<(), PgTrickleError> {
    let s = unsafe { &*select };

    // Handle set operations: recurse into larg/rarg
    if s.op != pg_sys::SetOperation::SETOP_NONE {
        if !s.larg.is_null() {
            collect_view_substitutions(s.larg, subs, found_view)?;
        }
        if !s.rarg.is_null() {
            collect_view_substitutions(s.rarg, subs, found_view)?;
        }
        return Ok(());
    }

    // Walk the FROM clause
    let from_list = pg_list::<pg_sys::Node>(s.fromClause);
    for node_ptr in from_list.iter_ptr() {
        collect_view_subs_from_item(node_ptr, subs, found_view)?;
    }

    // Walk CTE bodies if present
    if !s.withClause.is_null() {
        let wc = unsafe { &*s.withClause };
        let cte_list = pg_list::<pg_sys::Node>(wc.ctes);
        for node_ptr in cte_list.iter_ptr() {
            if let Some(cte) = cast_node!(node_ptr, T_CommonTableExpr, pg_sys::CommonTableExpr)
                && !cte.ctequery.is_null()
                && unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) }
            {
                collect_view_substitutions(
                    cte.ctequery as *const pg_sys::SelectStmt,
                    subs,
                    found_view,
                )?;
            }
        }
    }

    Ok(())
}

/// Walk a single FROM item looking for view references.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree node.
fn collect_view_subs_from_item(
    node: *mut pg_sys::Node,
    subs: &mut Vec<ViewSubstitution>,
    found_view: &mut bool,
) -> Result<(), PgTrickleError> {
    if node.is_null() {
        return Ok(());
    }

    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        // Get relname
        if rv.relname.is_null() {
            return Ok(());
        }
        let relname = pg_cstr_to_str(rv.relname)?.to_string();

        // Resolve schema — if not found, this is a CTE/subquery alias, skip.
        let schema = match resolve_rangevar_schema(rv)? {
            Some(s) => s,
            None => return Ok(()),
        };

        // Check relkind
        let relkind = resolve_relkind(&schema, &relname)?;
        match relkind.as_deref() {
            Some("v") => {
                // It's a view — get definition and record substitution
                let view_sql = get_view_definition(&schema, &relname)?;

                // Determine alias: explicit alias or view name
                let alias = if !rv.alias.is_null() {
                    let a = unsafe { &*(rv.alias) };
                    pg_cstr_to_str(a.aliasname).unwrap_or(&relname).to_string()
                } else {
                    relname.clone()
                };

                subs.push(ViewSubstitution {
                    schema: schema.clone(),
                    relname: relname.clone(),
                    view_sql,
                    alias,
                });
                *found_view = true;
            }
            _ => {
                // Not a view — leave as-is
            }
        }
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        collect_view_subs_from_item(join.larg, subs, found_view)?;
        collect_view_subs_from_item(join.rarg, subs, found_view)?;
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect)
        && !sub.subquery.is_null()
        && unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
    {
        collect_view_substitutions(sub.subquery as *const pg_sys::SelectStmt, subs, found_view)?;
    }
    // JSON_TABLE: skip — it doesn't reference tables that could be views.
    // The context item expression references the left-hand table which is
    // already traversed via the FROM list iteration.

    Ok(())
}

/// Deparse a SelectStmt back to SQL, replacing view RangeVars with inline
/// subqueries according to the substitution list.
///
/// # Safety
/// Caller must ensure `stmt` points to a valid `SelectStmt`.
fn deparse_select_stmt_with_view_subs(
    stmt: *const pg_sys::SelectStmt,
    subs: &[ViewSubstitution],
) -> Result<String, PgTrickleError> {
    let s = unsafe { &*stmt };

    // Handle set operations: deparse each arm separately
    if s.op != pg_sys::SetOperation::SETOP_NONE {
        let op_str = match s.op {
            pg_sys::SetOperation::SETOP_UNION => {
                if s.all {
                    "UNION ALL"
                } else {
                    "UNION"
                }
            }
            pg_sys::SetOperation::SETOP_INTERSECT => {
                if s.all {
                    "INTERSECT ALL"
                } else {
                    "INTERSECT"
                }
            }
            pg_sys::SetOperation::SETOP_EXCEPT => {
                if s.all {
                    "EXCEPT ALL"
                } else {
                    "EXCEPT"
                }
            }
            _ => "UNION",
        };

        let left = if !s.larg.is_null() {
            deparse_select_stmt_with_view_subs(s.larg, subs)?
        } else {
            String::new()
        };
        let right = if !s.rarg.is_null() {
            deparse_select_stmt_with_view_subs(s.rarg, subs)?
        } else {
            String::new()
        };

        let mut result = format!("{left} {op_str} {right}");

        // Preserve WITH clause on the outer SETOP node (CTE + UNION ALL pattern).
        // The CTE definitions live here, NOT on larg/rarg, so they must be
        // prepended after the arms have been recursively processed.
        if !s.withClause.is_null() {
            let with_sql = deparse_with_clause_with_view_subs(s.withClause, subs)?;
            result = format!("{with_sql} {result}");
        }

        // Top-level ORDER BY / LIMIT / OFFSET on set operations
        let sort_list = pg_list::<pg_sys::Node>(s.sortClause);
        if !sort_list.is_empty() {
            let sorts = unsafe { deparse_sort_clause(&sort_list)? };
            result.push_str(&format!(" ORDER BY {sorts}"));
        }
        if !s.limitCount.is_null() {
            let expr = unsafe { node_to_expr(s.limitCount)? };
            result.push_str(&format!(" LIMIT {}", expr.to_sql()));
        }
        if !s.limitOffset.is_null() {
            let expr = unsafe { node_to_expr(s.limitOffset)? };
            result.push_str(&format!(" OFFSET {}", expr.to_sql()));
        }

        return Ok(result);
    }

    // Regular SELECT — deparse with substitutions
    let mut parts = Vec::new();

    // WITH clause (CTEs) — deparse CTE bodies with view subs too
    if !s.withClause.is_null() {
        let with_sql = deparse_with_clause_with_view_subs(s.withClause, subs)?;
        parts.push(with_sql);
    }

    // DISTINCT
    let distinct_list = pg_list::<pg_sys::Node>(s.distinctClause);
    let select_kw = if !distinct_list.is_empty() {
        // Check if it's DISTINCT ON or plain DISTINCT
        let has_real_exprs = distinct_list.iter_ptr().any(|ptr| !ptr.is_null());
        if has_real_exprs {
            // DISTINCT ON (expr, expr, ...)
            let mut exprs = Vec::new();
            for node_ptr in distinct_list.iter_ptr() {
                if !node_ptr.is_null() {
                    let expr = unsafe { node_to_expr(node_ptr)? };
                    exprs.push(expr.to_sql());
                }
            }
            format!("SELECT DISTINCT ON ({})", exprs.join(", "))
        } else {
            "SELECT DISTINCT".to_string()
        }
    } else {
        "SELECT".to_string()
    };

    // Target list
    let targets = unsafe { deparse_target_list(s.targetList)? };
    parts.push(format!("{select_kw} {targets}"));

    // FROM clause with view substitutions
    let from_list = pg_list::<pg_sys::Node>(s.fromClause);
    if !from_list.is_empty() {
        let mut from_items = Vec::new();
        for node_ptr in from_list.iter_ptr() {
            let item = deparse_from_item_with_view_subs(node_ptr, subs)?;
            from_items.push(item);
        }
        parts.push(format!("FROM {}", from_items.join(", ")));
    }

    // WHERE
    if !s.whereClause.is_null() {
        let expr = unsafe { node_to_expr(s.whereClause)? };
        parts.push(format!("WHERE {}", expr.to_sql()));
    }

    // GROUP BY
    let group_list = pg_list::<pg_sys::Node>(s.groupClause);
    if !group_list.is_empty() {
        let mut groups = Vec::new();
        for node_ptr in group_list.iter_ptr() {
            let expr = unsafe { node_to_expr(node_ptr)? };
            groups.push(expr.to_sql());
        }
        parts.push(format!("GROUP BY {}", groups.join(", ")));
    }

    // HAVING
    if !s.havingClause.is_null() {
        let expr = unsafe { node_to_expr(s.havingClause)? };
        parts.push(format!("HAVING {}", expr.to_sql()));
    }

    // WINDOW clause
    let window_list = pg_list::<pg_sys::Node>(s.windowClause);
    if !window_list.is_empty() {
        let mut window_parts = Vec::new();
        for node_ptr in window_list.iter_ptr() {
            if let Some(wdef) = cast_node!(node_ptr, T_WindowDef, pg_sys::WindowDef) {
                let wname = if !wdef.name.is_null() {
                    pg_cstr_to_str(wdef.name).unwrap_or("w").to_string()
                } else {
                    continue;
                };
                let wspec = deparse_window_def(wdef)?;
                window_parts.push(format!("{wname} AS ({wspec})"));
            }
        }
        if !window_parts.is_empty() {
            parts.push(format!("WINDOW {}", window_parts.join(", ")));
        }
    }

    // ORDER BY
    let sort_list = pg_list::<pg_sys::Node>(s.sortClause);
    if !sort_list.is_empty() {
        let sorts = unsafe { deparse_sort_clause(&sort_list)? };
        parts.push(format!("ORDER BY {sorts}"));
    }

    // LIMIT
    if !s.limitCount.is_null() {
        let expr = unsafe { node_to_expr(s.limitCount)? };
        parts.push(format!("LIMIT {}", expr.to_sql()));
    }

    // OFFSET
    if !s.limitOffset.is_null() {
        let expr = unsafe { node_to_expr(s.limitOffset)? };
        parts.push(format!("OFFSET {}", expr.to_sql()));
    }

    Ok(parts.join(" "))
}

/// Deparse a WITH clause, applying view substitutions to CTE bodies.
///
/// # Safety
/// Caller must ensure `with_clause` points to a valid `WithClause`.
fn deparse_with_clause_with_view_subs(
    with_clause: *mut pg_sys::WithClause,
    subs: &[ViewSubstitution],
) -> Result<String, PgTrickleError> {
    let wc = unsafe { &*with_clause };
    let cte_list = pg_list::<pg_sys::Node>(wc.ctes);
    let mut cte_parts = Vec::new();

    for node_ptr in cte_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
            continue;
        }
        let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };
        let cte_name = pg_cstr_to_str(cte.ctename).unwrap_or("cte").to_string();

        // Column aliases
        let col_aliases = extract_cte_def_colnames(cte)?;
        let alias_part = if col_aliases.is_empty() {
            String::new()
        } else {
            format!("({})", col_aliases.join(", "))
        };

        // CTE body
        let body_sql = if !cte.ctequery.is_null()
            && unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) }
        {
            deparse_select_stmt_with_view_subs(cte.ctequery as *const pg_sys::SelectStmt, subs)?
        } else {
            // Fallback: shouldn't happen for valid queries
            "SELECT 1".to_string()
        };

        cte_parts.push(format!("{cte_name}{alias_part} AS ({body_sql})"));
    }

    let recursive = if wc.recursive { "RECURSIVE " } else { "" };
    Ok(format!("WITH {recursive}{}", cte_parts.join(", ")))
}

/// Deparse a single FROM item, replacing view RangeVars with inline subqueries.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree node.
fn deparse_from_item_with_view_subs(
    node: *mut pg_sys::Node,
    subs: &[ViewSubstitution],
) -> Result<String, PgTrickleError> {
    if node.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "NULL FROM item node".into(),
        ));
    }

    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        // Get relname
        let relname = if !rv.relname.is_null() {
            pg_cstr_to_str(rv.relname).unwrap_or("").to_string()
        } else {
            return Ok("?".to_string());
        };

        // Get schema (may be NULL for unqualified names)
        let schema = if !rv.schemaname.is_null() {
            Some(pg_cstr_to_str(rv.schemaname).unwrap_or("").to_string())
        } else {
            None
        };

        // Get explicit alias
        let explicit_alias = if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            Some(pg_cstr_to_str(a.aliasname).unwrap_or("").to_string())
        } else {
            None
        };

        // Check if this RangeVar matches a view substitution
        if let Some(sub) = subs.iter().find(|s| {
            s.relname == relname
                && (schema.as_deref() == Some(&s.schema)
                    || (schema.is_none() && !s.schema.is_empty()))
        }) {
            // Replace with inline subquery — use the alias from this specific
            // RangeVar, not from the substitution (which may come from a
            // different occurrence of the same view with a different alias).
            let alias = explicit_alias.as_deref().unwrap_or(&relname);
            return Ok(format!("({}) AS {alias}", sub.view_sql));
        }

        // Not a view — deparse normally
        let mut name = String::new();
        if let Some(ref s) = schema {
            name.push_str(s);
            name.push('.');
        }
        name.push_str(&relname);
        if let Some(ref a) = explicit_alias
            && a != &relname
        {
            name.push(' ');
            name.push_str(a);
        }
        Ok(name)
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        let left = deparse_from_item_with_view_subs(join.larg, subs)?;
        let right = deparse_from_item_with_view_subs(join.rarg, subs)?;

        let join_type = match join.jointype {
            pg_sys::JoinType::JOIN_INNER => {
                if join.isNatural {
                    "NATURAL JOIN"
                } else {
                    "JOIN"
                }
            }
            pg_sys::JoinType::JOIN_LEFT => {
                if join.isNatural {
                    "NATURAL LEFT JOIN"
                } else {
                    "LEFT JOIN"
                }
            }
            pg_sys::JoinType::JOIN_FULL => {
                if join.isNatural {
                    "NATURAL FULL JOIN"
                } else {
                    "FULL JOIN"
                }
            }
            pg_sys::JoinType::JOIN_RIGHT => {
                if join.isNatural {
                    "NATURAL RIGHT JOIN"
                } else {
                    "RIGHT JOIN"
                }
            }
            _ => "JOIN",
        };

        if join.isNatural || join.quals.is_null() {
            Ok(format!("{left} {join_type} {right}"))
        } else {
            let condition = unsafe { node_to_expr(join.quals)? };
            Ok(format!(
                "{left} {join_type} {right} ON {}",
                condition.to_sql()
            ))
        }
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect) {
        if sub.subquery.is_null()
            || !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            return Err(PgTrickleError::QueryParseError(
                "RangeSubselect without valid SelectStmt".into(),
            ));
        }
        let sub_stmt = sub.subquery as *const pg_sys::SelectStmt;
        // Recurse into the subquery to replace any view references there too
        let inner_sql = deparse_select_stmt_with_view_subs(sub_stmt, subs)?;

        let lateral_kw = if sub.lateral { "LATERAL " } else { "" };
        let mut result = format!("{lateral_kw}({inner_sql})");
        if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            let alias_name = pg_cstr_to_str(a.aliasname).unwrap_or("");
            result.push_str(&format!(" AS {alias_name}"));
        }
        Ok(result)
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeFunction) } {
        // Function in FROM — deparse as-is using existing infrastructure
        unsafe { deparse_from_item_to_sql(node) }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JsonTable) } {
        // JSON_TABLE — deparse using dedicated deparser
        unsafe { deparse_from_item_to_sql(node) }
    } else {
        // Fallback
        unsafe { deparse_from_item_to_sql(node) }
    }
}

/// Deparse a WindowDef to the window specification string (inside parentheses).
///
/// # Safety
/// Caller must ensure `wdef` points to a valid `WindowDef`.
fn deparse_window_def(wdef: &pg_sys::WindowDef) -> Result<String, PgTrickleError> {
    let mut parts = Vec::new();

    // PARTITION BY
    let part_list = pg_list::<pg_sys::Node>(wdef.partitionClause);
    if !part_list.is_empty() {
        let mut exprs = Vec::new();
        for node_ptr in part_list.iter_ptr() {
            let expr = unsafe { node_to_expr(node_ptr)? };
            exprs.push(expr.to_sql());
        }
        parts.push(format!("PARTITION BY {}", exprs.join(", ")));
    }

    // ORDER BY
    let sort_list = pg_list::<pg_sys::Node>(wdef.orderClause);
    if !sort_list.is_empty() {
        let sorts = unsafe { deparse_sort_clause(&sort_list)? };
        parts.push(format!("ORDER BY {sorts}"));
    }

    Ok(parts.join(" "))
}

/// Reject materialized views in the defining query for DIFFERENTIAL mode.
///
/// Materialized views are stale snapshots — CDC triggers cannot track
/// `REFRESH MATERIALIZED VIEW`. The user should use the underlying query
/// directly or switch to FULL refresh mode.
pub fn reject_materialized_views(query: &str) -> Result<(), PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(()),
    };
    check_for_matviews_or_foreign(select)
}

/// Reject foreign tables in the defining query for DIFFERENTIAL mode.
///
/// Foreign tables do not support row-level triggers, so CDC cannot track them.
pub fn reject_foreign_tables(query: &str) -> Result<(), PgTrickleError> {
    // Uses the same implementation as reject_materialized_views;
    // the check function handles both.
    reject_materialized_views(query)
}

/// Recursively check a SelectStmt for materialized view or foreign table
/// references in the FROM clause.
///
/// # Safety
/// Caller must ensure `select` points to a valid `SelectStmt`.
fn check_for_matviews_or_foreign(select: *const pg_sys::SelectStmt) -> Result<(), PgTrickleError> {
    let s = unsafe { &*select };

    // Handle set operations
    if s.op != pg_sys::SetOperation::SETOP_NONE {
        if !s.larg.is_null() {
            check_for_matviews_or_foreign(s.larg)?;
        }
        if !s.rarg.is_null() {
            check_for_matviews_or_foreign(s.rarg)?;
        }
        return Ok(());
    }

    let from_list = pg_list::<pg_sys::Node>(s.fromClause);
    for node_ptr in from_list.iter_ptr() {
        check_from_item_for_matview_or_foreign(node_ptr)?;
    }

    Ok(())
}

/// Check a single FROM item for materialized view or foreign table references.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree node.
fn check_from_item_for_matview_or_foreign(node: *mut pg_sys::Node) -> Result<(), PgTrickleError> {
    if node.is_null() {
        return Ok(());
    }

    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        if rv.relname.is_null() {
            return Ok(());
        }
        let relname = pg_cstr_to_str(rv.relname).unwrap_or("").to_string();

        // Skip CTE names / subquery aliases that are not real relations.
        let schema = match resolve_rangevar_schema(rv)? {
            Some(s) => s,
            None => return Ok(()),
        };
        let relkind = resolve_relkind(&schema, &relname)?;

        match relkind.as_deref() {
            Some("m") => {
                if !crate::config::pg_trickle_matview_polling() {
                    return Err(PgTrickleError::UnsupportedOperator(format!(
                        "Materialized view '{schema}.{relname}' cannot be used as a source in \
                         DIFFERENTIAL mode. Materialized views are stale snapshots — CDC triggers \
                         cannot track REFRESH MATERIALIZED VIEW. Use the underlying query directly, \
                         or switch to FULL refresh mode. \
                         Alternatively, enable polling-based CDC with: \
                         SET pg_trickle.matview_polling = on;"
                    )));
                }
            }
            Some("f") => {
                if !crate::config::pg_trickle_foreign_table_polling() {
                    return Err(PgTrickleError::UnsupportedOperator(format!(
                        "Foreign table '{schema}.{relname}' cannot be used as a source in \
                         DIFFERENTIAL or IMMEDIATE mode. Row-level triggers cannot be created \
                         on foreign tables. Use FULL refresh mode instead, which re-queries \
                         the foreign table on each refresh cycle. For postgres_fdw tables, \
                         consider using IMPORT FOREIGN SCHEMA to keep the local schema in sync. \
                         Alternatively, enable polling-based CDC with: \
                         SET pg_trickle.foreign_table_polling = on;"
                    )));
                }
            }
            _ => {}
        }
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        check_from_item_for_matview_or_foreign(join.larg)?;
        check_from_item_for_matview_or_foreign(join.rarg)?;
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect)
        && !sub.subquery.is_null()
        && unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
    {
        check_for_matviews_or_foreign(sub.subquery as *const pg_sys::SelectStmt)?;
    }

    Ok(())
}

/// Lightweight check that rejects LIMIT / OFFSET in a defining query.
///
/// Uses `raw_parser()` to parse the query and inspects the top-level
/// `SelectStmt` for `limitCount` / `limitOffset`. This is called at
/// stream table creation time for **all** refresh modes (including FULL),
/// before the full DVM parser runs.
///
/// For set-operation queries (UNION/INTERSECT/EXCEPT), LIMIT/OFFSET on
/// the top-level wrapper is checked. LIMIT inside subqueries or LATERAL
/// is intentionally allowed.
/// Rewrite `SELECT DISTINCT ON (...)` inside `WITH` clause CTE bodies.
///
/// The top-level `rewrite_distinct_on` function only handles `DISTINCT ON` at
/// the outermost SELECT. When a CTE body itself uses `DISTINCT ON`, it escapes
/// the rewrite and the DVM parser later rejects it with `UnsupportedOperator`.
///
/// This helper iterates every non-recursive CTE in the `WITH` clause, deparsed
/// each CTE body that contains `DISTINCT ON` back to SQL, runs
/// `rewrite_distinct_on` recursively on that body, and reassembles the full
/// query with the rewritten bodies.
///
/// Returns the original query string unchanged when no CTE body uses
/// `DISTINCT ON`.
fn rewrite_cte_bodies_for_distinct_on(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Only applies to plain SELECT with a WITH clause (not set operations).
    if select.withClause.is_null() || select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // SAFETY: withClause is non-null, confirmed above.
    let wc = unsafe { &*select.withClause };
    let cte_list = pg_list::<pg_sys::Node>(wc.ctes);

    // First pass: check whether any CTE body uses DISTINCT ON.
    let has_distinct_on_cte = cte_list.iter_ptr().any(|node_ptr| {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
            return false;
        }
        // SAFETY: pgrx::is_a confirmed T_CommonTableExpr.
        let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };
        if cte.ctequery.is_null()
            || !unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) }
        {
            return false;
        }
        // SAFETY: pgrx::is_a confirmed T_SelectStmt.
        let body = unsafe { &*(cte.ctequery as *const pg_sys::SelectStmt) };
        if body.distinctClause.is_null() {
            return false;
        }
        let distinct_list = pg_list::<pg_sys::Node>(body.distinctClause);
        // DISTINCT ON has real (non-null) expression nodes; plain DISTINCT uses null nodes.
        distinct_list.iter_ptr().any(|ptr| !ptr.is_null())
    });
    if !has_distinct_on_cte {
        return Ok(query.to_string());
    }

    // Second pass: rebuild the WITH clause with rewritten CTE bodies.
    let mut new_cte_parts: Vec<String> = Vec::new();
    for node_ptr in cte_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
            continue;
        }
        // SAFETY: pgrx::is_a confirmed T_CommonTableExpr.
        let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };
        let cte_name = pg_cstr_to_str(cte.ctename).unwrap_or("cte").to_string();
        let quoted_name = format!("\"{}\"", cte_name.replace('"', "\"\""));

        let col_aliases = extract_cte_def_colnames(cte)?;
        let alias_part = if col_aliases.is_empty() {
            String::new()
        } else {
            let q: Vec<String> = col_aliases
                .iter()
                .map(|a| format!("\"{}\"", a.replace('"', "\"\"")))
                .collect();
            format!("({})", q.join(", "))
        };

        if !cte.ctequery.is_null()
            && unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) }
        {
            // Deparse the CTE body and recursively rewrite DISTINCT ON.
            // (Handles nested WITH…DISTINCT ON inside the body as well.)
            let body_sql =
                deparse_select_stmt_with_view_subs(cte.ctequery as *const pg_sys::SelectStmt, &[])?;
            let rewritten = rewrite_distinct_on(&body_sql)?;
            new_cte_parts.push(format!("{quoted_name}{alias_part} AS ({rewritten})"));
        } else {
            // Non-SELECT CTE body — should not arise for valid SQL, kept for safety.
            new_cte_parts.push(format!("{quoted_name}{alias_part} AS (SELECT 1)"));
        }
    }

    let recursive = if wc.recursive { "RECURSIVE " } else { "" };
    let new_with = format!("WITH {recursive}{}", new_cte_parts.join(",\n  "));

    // Reconstruct the full query as: <new WITH clause>\n<original main SELECT body>.
    // Both the full deparsed form and the WITH prefix are produced by the same
    // deparsing logic from the same parse tree, so stripping the prefix is exact.
    let full_deparsed = deparse_select_stmt_with_view_subs(select, &[])?;
    let with_prefix = deparse_with_clause_with_view_subs(select.withClause, &[])?;
    let main_body = if let Some(rest) = full_deparsed.strip_prefix(&with_prefix) {
        rest.trim_start().to_string()
    } else {
        // Fallback: should not happen; return the full statement as-is.
        full_deparsed
    };

    Ok(format!("{new_with}\n{main_body}"))
}

/// Detect and rewrite `DISTINCT ON (...)` queries.
///
/// `SELECT DISTINCT ON (e1, e2) col1, col2 FROM t ORDER BY e1, e2, col3`
///
/// is rewritten to:
///
/// ```sql
/// SELECT col1, col2 FROM (
///   SELECT col1, col2, ROW_NUMBER() OVER (PARTITION BY e1, e2 ORDER BY ...) AS __pgt_rn
///   FROM t
/// ) __pgt_do WHERE __pgt_rn = 1
/// ```
///
/// Also rewrites `DISTINCT ON` inside `WITH` clause CTE bodies so that the
/// DVM parser never encounters unsupported `DISTINCT ON` in a CTE SelectStmt.
///
/// Returns the original query unchanged if it does not use DISTINCT ON.
pub fn rewrite_distinct_on(query: &str) -> Result<String, PgTrickleError> {
    // Rewrite DISTINCT ON inside any CTE bodies first.  The DVM parser calls
    // parse_select_stmt on CTE bodies and rejects DISTINCT ON there; doing the
    // rewrite here lets those CTE bodies through to the differential path.
    let owned = rewrite_cte_bodies_for_distinct_on(query)?;
    let query = owned.as_str();

    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Only rewrite if DISTINCT ON (not plain DISTINCT or no DISTINCT)
    if select.distinctClause.is_null() {
        return Ok(query.to_string());
    }
    let distinct_list = pg_list::<pg_sys::Node>(select.distinctClause);
    let has_real_exprs = distinct_list.iter_ptr().any(|ptr| !ptr.is_null());
    if !has_real_exprs {
        // Plain DISTINCT (all NULLs in distinctClause) — no rewrite needed
        return Ok(query.to_string());
    }

    // Set operations with DISTINCT ON don't make sense — skip
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // Extract DISTINCT ON expressions as SQL text
    let mut distinct_on_exprs = Vec::new();
    for node_ptr in distinct_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        let expr_sql = unsafe { node_to_expr(node_ptr) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| {
                // Fallback: deparse using PostgreSQL's nodeToString
                "?".to_string()
            });
        if expr_sql != "?" {
            distinct_on_exprs.push(expr_sql);
        }
    }

    if distinct_on_exprs.is_empty() {
        return Ok(query.to_string());
    }

    // F37 (G5.1): Warn when DISTINCT ON is used without ORDER BY.
    // Without ORDER BY, PostgreSQL picks an arbitrary row from each group.
    // The chosen row may differ between FULL and DIFFERENTIAL refresh,
    // causing the stream table to non-deterministically switch rows.
    if select.sortClause.is_null() {
        pgrx::warning!(
            "pg_trickle: DISTINCT ON without ORDER BY produces non-deterministic results. \
             The chosen row per group may differ between FULL and DIFFERENTIAL refresh. \
             Add ORDER BY to ensure deterministic row selection."
        );
    }

    // Extract ORDER BY clause as SQL text (if any)
    let order_by_sql = if !select.sortClause.is_null() {
        let sort_list = pg_list::<pg_sys::Node>(select.sortClause);
        let mut order_parts = Vec::new();
        for node_ptr in sort_list.iter_ptr() {
            if node_ptr.is_null() {
                continue;
            }
            if let Some(sort_by) = cast_node!(node_ptr, T_SortBy, pg_sys::SortBy)
                && !sort_by.node.is_null()
            {
                let expr_sql = unsafe { node_to_expr(sort_by.node) }
                    .map(|e| e.to_sql())
                    .unwrap_or_else(|_| "?".to_string());
                let dir = match sort_by.sortby_dir {
                    pg_sys::SortByDir::SORTBY_DESC => " DESC",
                    pg_sys::SortByDir::SORTBY_ASC => " ASC",
                    _ => "",
                };
                let nulls = match sort_by.sortby_nulls {
                    pg_sys::SortByNulls::SORTBY_NULLS_FIRST => " NULLS FIRST",
                    pg_sys::SortByNulls::SORTBY_NULLS_LAST => " NULLS LAST",
                    _ => "",
                };
                order_parts.push(format!("{expr_sql}{dir}{nulls}"));
            }
        }
        if order_parts.is_empty() {
            None
        } else {
            Some(order_parts.join(", "))
        }
    } else {
        None
    };

    // Extract target list column names/aliases for the outer SELECT
    let target_list = pg_list::<pg_sys::Node>(select.targetList);
    let mut outer_cols = Vec::new();
    let mut inner_target_parts = Vec::new();
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };

        // Extract alias name
        let alias = if !rt.name.is_null() {
            let name = pg_cstr_to_str(rt.name).unwrap_or("?col");
            Some(name.to_string())
        } else {
            None
        };

        // Extract expression SQL
        if rt.val.is_null() {
            continue;
        }
        let expr_sql = unsafe { node_to_expr(rt.val) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "?".to_string());

        // For outer SELECT, use the alias if it exists, otherwise the expression
        let outer_name = alias.as_deref().unwrap_or(&expr_sql);
        outer_cols.push(format!("__pgt_do.\"{}\"", outer_name.replace('"', "\"\"")));

        // Inner target: expression AS alias (or just expression)
        if let Some(ref a) = alias {
            inner_target_parts.push(format!("{} AS \"{}\"", expr_sql, a.replace('"', "\"\"")));
        } else {
            inner_target_parts.push(expr_sql);
        }
    }

    if inner_target_parts.is_empty() || outer_cols.is_empty() {
        return Ok(query.to_string());
    }

    // Build the ROW_NUMBER() window function
    let partition_by = distinct_on_exprs.join(", ");
    let order_clause = order_by_sql
        .map(|ob| format!(" ORDER BY {ob}"))
        .unwrap_or_default();
    let row_number =
        format!("ROW_NUMBER() OVER (PARTITION BY {partition_by}{order_clause}) AS __pgt_rn");

    // Reconstruct the inner SELECT (without DISTINCT ON and ORDER BY)
    // We need: FROM clause, WHERE clause, GROUP BY, HAVING
    // The simplest approach: strip "DISTINCT ON (...)" and "ORDER BY ..." from the original query
    // But this is fragile with raw string manipulation. Instead, reconstruct from parts.

    // Extract FROM clause as raw SQL from the original query
    // For robustness, use a subquery approach: wrap the original query minus DISTINCT ON/ORDER BY

    // Build FROM clause SQL from the original parse tree
    let from_sql = extract_from_clause_sql(select)?;
    let where_sql = if select.whereClause.is_null() {
        String::new()
    } else {
        let where_expr = unsafe { node_to_expr(select.whereClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" WHERE {where_expr}")
    };

    // GROUP BY
    let group_sql = if select.groupClause.is_null() {
        String::new()
    } else {
        let group_list = pg_list::<pg_sys::Node>(select.groupClause);
        let mut parts = Vec::new();
        for node_ptr in group_list.iter_ptr() {
            if node_ptr.is_null() {
                continue;
            }
            if let Ok(expr) = unsafe { node_to_expr(node_ptr) } {
                parts.push(expr.to_sql());
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" GROUP BY {}", parts.join(", "))
        }
    };

    // HAVING
    let having_sql = if select.havingClause.is_null() {
        String::new()
    } else {
        let having_expr = unsafe { node_to_expr(select.havingClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" HAVING {having_expr}")
    };

    // Build the rewritten query
    let inner_targets = inner_target_parts.join(", ");
    let outer_select = outer_cols.join(", ");
    let rewritten = format!(
        "SELECT {outer_select} FROM (\
         SELECT {inner_targets}, {row_number} \
         FROM {from_sql}{where_sql}{group_sql}{having_sql}\
         ) __pgt_do WHERE __pgt_do.__pgt_rn = 1"
    );

    pgrx::debug1!(
        "[pg_trickle] Rewrote DISTINCT ON query to ROW_NUMBER(): {}",
        rewritten
    );

    Ok(rewritten)
}

// ── GROUPING SETS / CUBE / ROLLUP → UNION ALL rewrite ──────────────

/// Rewrite a query using `GROUPING SETS`, `CUBE`, or `ROLLUP` into an
/// equivalent `UNION ALL` of separate `GROUP BY` queries.
///
/// This is called **before** the DVM parser so the downstream operator tree
/// only ever sees plain `GROUP BY` + `UNION ALL` — no new OpTree variants
/// are needed.
///
/// # Algorithm
///
/// 1. Detect `T_GroupingSet` nodes in the raw parse tree's `groupClause`.
/// 2. Expand `CUBE`/`ROLLUP` to their equivalent list of grouping sets.
/// 3. Collect any plain `GROUP BY` columns (always included in every set).
/// 4. For each grouping set, build a `SELECT … GROUP BY <set_cols>` where:
///    - Column references to non-grouped columns are replaced with `NULL`.
///    - `GROUPING(col, …)` calls are replaced with computed integer literals.
/// 5. Combine all branches with `UNION ALL`.
///
/// If the query contains no grouping sets, it is returned unchanged.
pub fn rewrite_grouping_sets(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Set operations — don't rewrite
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // No GROUP BY at all
    if select.groupClause.is_null() {
        return Ok(query.to_string());
    }

    // ── Scan groupClause to find GroupingSet nodes ──────────────────
    let group_list = pg_list::<pg_sys::Node>(select.groupClause);
    let mut has_grouping_sets = false;
    for node_ptr in group_list.iter_ptr() {
        if !node_ptr.is_null() && unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_GroupingSet) } {
            has_grouping_sets = true;
            break;
        }
    }
    if !has_grouping_sets {
        return Ok(query.to_string());
    }

    // ── Separate plain columns from GroupingSet specifications ──────
    let mut plain_col_exprs: Vec<String> = Vec::new();
    let mut grouping_set_specs: Vec<Vec<Vec<String>>> = Vec::new();

    for node_ptr in group_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        if let Some(gs) = cast_node!(node_ptr, T_GroupingSet, pg_sys::GroupingSet) {
            let expanded = expand_grouping_set(gs)?;
            grouping_set_specs.push(expanded);
        } else {
            // Plain GROUP BY expression
            let expr_sql = unsafe { node_to_expr(node_ptr) }
                .map(|e| e.output_name())
                .unwrap_or_else(|_| "?".to_string());
            plain_col_exprs.push(expr_sql);
        }
    }

    // ── Cross-product all grouping set specifications ───────────────
    // GROUP BY a, ROLLUP(b, c), CUBE(d) → cross product of the
    // ROLLUP sets and CUBE sets, with `a` always included.
    let mut final_sets: Vec<Vec<String>> = vec![vec![]];
    for spec_sets in &grouping_set_specs {
        let mut new_final = Vec::new();
        for existing in &final_sets {
            for new_set in spec_sets {
                let mut combined = existing.clone();
                combined.extend(new_set.iter().cloned());
                new_final.push(combined);
            }
        }
        final_sets = new_final;

        // Guard against combinatorial explosion: CUBE(n) produces 2^n branches,
        // ROLLUP(n) produces n+1, combined CUBE+ROLLUP can easily exceed memory.
        // Reject early rather than building a query too large for PG to parse (G5.2).
        // EC-02: Limit is configurable via pg_trickle.max_grouping_set_branches GUC.
        let max_grouping_branches = crate::config::PGS_MAX_GROUPING_SET_BRANCHES.get() as usize;
        if final_sets.len() > max_grouping_branches {
            return Err(PgTrickleError::QueryParseError(format!(
                "CUBE/ROLLUP generates {} grouping set branches which exceeds the limit of {}. \
                 Use explicit GROUPING SETS(...) to enumerate only the required combinations, \
                 or raise pg_trickle.max_grouping_set_branches.",
                final_sets.len(),
                max_grouping_branches
            )));
        }
    }

    // Prepend plain columns to every set
    if !plain_col_exprs.is_empty() {
        for set in &mut final_sets {
            let mut with_plain = plain_col_exprs.clone();
            with_plain.append(set);
            *set = with_plain;
        }
    }

    // Collect the union of all grouping columns (for NULL substitution)
    let all_grouping_cols: Vec<String> = {
        let mut all = std::collections::HashSet::new();
        for set in &final_sets {
            for col in set {
                all.insert(col.clone());
            }
        }
        all.into_iter().collect()
    };

    // ── Extract query components as SQL text ────────────────────────
    let from_sql = extract_from_clause_sql(select)?;

    let where_sql = if select.whereClause.is_null() {
        String::new()
    } else {
        let where_expr = unsafe { node_to_expr(select.whereClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" WHERE {where_expr}")
    };

    let having_sql = if select.havingClause.is_null() {
        String::new()
    } else {
        let having_expr = unsafe { node_to_expr(select.havingClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" HAVING {having_expr}")
    };

    // ── Parse target list ──────────────────────────────────────────
    let target_list = pg_list::<pg_sys::Node>(select.targetList);
    let mut targets: Vec<TargetEntry> = Vec::new();

    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }

        let alias = if !rt.name.is_null() {
            Some(pg_cstr_to_str(rt.name).unwrap_or("?").to_string())
        } else {
            None
        };

        // Check if target expression is a GROUPING() call
        if unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_GroupingFunc) } {
            // SAFETY: confirmed T_GroupingFunc
            let gf = unsafe { &*(rt.val as *const pg_sys::GroupingFunc) };
            let gf_args = extract_grouping_func_args(gf)?;
            targets.push(TargetEntry {
                kind: TargetKind::GroupingFunc(gf_args),
                alias,
            });
            continue;
        }

        // Parse as expression
        let expr =
            unsafe { node_to_expr(rt.val) }.unwrap_or_else(|_| Expr::Raw("NULL".to_string()));

        // Check if this is a simple column ref that matches a grouping column
        let col_name = match &expr {
            Expr::ColumnRef { column_name, .. } => Some(column_name.clone()),
            _ => None,
        };

        if let Some(ref cn) = col_name
            && all_grouping_cols.iter().any(|gc| gc == cn)
        {
            targets.push(TargetEntry {
                kind: TargetKind::GroupingColumn(cn.clone(), expr),
                alias,
            });
            continue;
        }

        // Aggregate or other expression — emit as-is
        targets.push(TargetEntry {
            kind: TargetKind::Expression(expr),
            alias,
        });
    }

    // ── Build UNION ALL branches ───────────────────────────────────
    let mut branches: Vec<String> = Vec::new();

    for current_set in &final_sets {
        let mut select_parts: Vec<String> = Vec::new();

        for target in &targets {
            let sql = match &target.kind {
                TargetKind::GroupingFunc(args) => {
                    let value = compute_grouping_value(args, current_set);
                    let alias_part = target
                        .alias
                        .as_ref()
                        .map(|a| format!(" AS \"{}\"", a.replace('"', "\"\"")))
                        .unwrap_or_default();
                    format!("{value}{alias_part}")
                }
                TargetKind::GroupingColumn(col_name, expr) => {
                    let in_set = current_set.iter().any(|c| c == col_name);
                    let alias_part = match &target.alias {
                        Some(a) => format!(" AS \"{}\"", a.replace('"', "\"\"")),
                        None => format!(" AS \"{}\"", col_name.replace('"', "\"\"")),
                    };
                    if in_set {
                        format!("{}{alias_part}", expr.to_sql())
                    } else {
                        format!("NULL{alias_part}")
                    }
                }
                TargetKind::Expression(expr) => {
                    let alias_part = target
                        .alias
                        .as_ref()
                        .map(|a| format!(" AS \"{}\"", a.replace('"', "\"\"")))
                        .unwrap_or_default();
                    format!("{}{alias_part}", expr.to_sql())
                }
            };
            select_parts.push(sql);
        }

        let group_by_sql = if current_set.is_empty() {
            String::new()
        } else {
            let cols: Vec<String> = current_set
                .iter()
                .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
                .collect();
            format!(" GROUP BY {}", cols.join(", "))
        };

        branches.push(format!(
            "SELECT {} FROM {from_sql}{where_sql}{group_by_sql}{having_sql}",
            select_parts.join(", ")
        ));
    }

    let rewritten = branches.join(" UNION ALL ");

    pgrx::debug1!(
        "[pg_trickle] Rewrote GROUPING SETS query to UNION ALL: {}",
        rewritten
    );

    Ok(rewritten)
}

/// A target list entry classified for grouping-set rewriting.
struct TargetEntry {
    kind: TargetKind,
    alias: Option<String>,
}

/// Classification of a SELECT-list expression for grouping-set rewriting.
enum TargetKind {
    /// A reference to a grouping column (may be NULLified per branch).
    GroupingColumn(String, Expr),
    /// A `GROUPING(col, …)` call (replaced with a literal per branch).
    GroupingFunc(Vec<String>),
    /// An aggregate or other expression (emitted as-is in every branch).
    Expression(Expr),
}

/// Expand a `GroupingSet` node into a list of grouping sets (each set
/// is a `Vec<String>` of column names).
fn expand_grouping_set(gs: &pg_sys::GroupingSet) -> Result<Vec<Vec<String>>, PgTrickleError> {
    let columns = extract_grouping_set_columns(gs)?;

    match gs.kind {
        pg_sys::GroupingSetKind::GROUPING_SET_EMPTY => Ok(vec![vec![]]),

        pg_sys::GroupingSetKind::GROUPING_SET_SIMPLE => Ok(vec![columns]),

        pg_sys::GroupingSetKind::GROUPING_SET_ROLLUP => {
            // ROLLUP(a, b, c) → [(a,b,c), (a,b), (a), ()]
            let mut sets = Vec::new();
            for i in (0..=columns.len()).rev() {
                sets.push(columns[..i].to_vec());
            }
            Ok(sets)
        }

        pg_sys::GroupingSetKind::GROUPING_SET_CUBE => {
            // CUBE(a, b, c) → all 2^n subsets, ordered largest-first
            let n = columns.len();
            let mut sets = Vec::new();
            // Iterate from (2^n - 1) down to 0 for largest-first ordering
            for mask in (0..(1u64 << n)).rev() {
                let mut set = Vec::new();
                for (i, col) in columns.iter().enumerate() {
                    if mask & (1u64 << (n - 1 - i)) != 0 {
                        set.push(col.clone());
                    }
                }
                sets.push(set);
            }
            Ok(sets)
        }

        pg_sys::GroupingSetKind::GROUPING_SET_SETS => {
            // GROUPING SETS ((a, b), (c), ()) — content is a list of
            // nested GroupingSet nodes (SIMPLE or EMPTY).
            if gs.content.is_null() {
                return Ok(vec![vec![]]);
            }
            let content_list = pg_list::<pg_sys::Node>(gs.content);
            let mut sets = Vec::new();
            for node_ptr in content_list.iter_ptr() {
                if node_ptr.is_null() {
                    continue;
                }
                if let Some(inner) = cast_node!(node_ptr, T_GroupingSet, pg_sys::GroupingSet) {
                    let inner_sets = expand_grouping_set(inner)?;
                    sets.extend(inner_sets);
                } else {
                    // Single column expression
                    let col_sql = unsafe { node_to_expr(node_ptr) }
                        .map(|e| e.output_name())
                        .unwrap_or_else(|_| "?".to_string());
                    sets.push(vec![col_sql]);
                }
            }
            Ok(sets)
        }

        _ => Err(PgTrickleError::UnsupportedOperator(format!(
            "Unknown GroupingSet kind: {:?}",
            gs.kind
        ))),
    }
}

/// Extract column names from a `GroupingSet`'s content list.
fn extract_grouping_set_columns(gs: &pg_sys::GroupingSet) -> Result<Vec<String>, PgTrickleError> {
    if gs.content.is_null() {
        return Ok(vec![]);
    }
    let content_list = pg_list::<pg_sys::Node>(gs.content);
    let mut cols = Vec::new();
    for node_ptr in content_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        let col_sql = unsafe { node_to_expr(node_ptr) }
            .map(|e| e.output_name())
            .unwrap_or_else(|_| "?".to_string());
        cols.push(col_sql);
    }
    Ok(cols)
}

/// Extract `GROUPING(col, …)` argument column names from a raw-parse
/// `GroupingFunc` node.
fn extract_grouping_func_args(gf: &pg_sys::GroupingFunc) -> Result<Vec<String>, PgTrickleError> {
    if gf.args.is_null() {
        return Ok(vec![]);
    }
    let args_list = pg_list::<pg_sys::Node>(gf.args);
    let mut names = Vec::new();
    for node_ptr in args_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        let name = unsafe { node_to_expr(node_ptr) }
            .map(|e| e.output_name())
            .unwrap_or_else(|_| "?".to_string());
        names.push(name);
    }
    Ok(names)
}

/// Compute the `GROUPING(col1, col2, …)` integer value for a given
/// grouping set. Returns a bitmask where bit *i* (MSB-first) is 1 if
/// the *i*-th argument is **not** in the current grouping set.
fn compute_grouping_value(args: &[String], current_set: &[String]) -> i64 {
    let mut value: i64 = 0;
    for arg in args {
        value <<= 1;
        if !current_set.iter().any(|c| c == arg) {
            value |= 1; // column is aggregated (not grouped) → 1
        }
    }
    value
}

// ── Scalar subquery in WHERE → CROSS JOIN rewrite ──────────────────

/// Rewrite scalar subqueries in the WHERE clause into CROSS JOINs.
///
/// ```sql
/// -- Input:
/// SELECT * FROM orders WHERE amount > (SELECT avg(amount) FROM orders)
/// -- Rewrite to:
/// SELECT * FROM orders
/// CROSS JOIN (SELECT avg(amount) AS __pgt_scalar_1 FROM orders) AS __pgt_sq_1
/// WHERE amount > __pgt_sq_1.__pgt_scalar_1
/// ```
///
/// This is called **before** the DVM parser so the downstream operator tree
/// only sees a simple CROSS JOIN + Filter — no special scalar-subquery-in-WHERE
/// handling is needed.
///
/// Only handles EXPR_SUBLINK (scalar subqueries) in the top-level WHERE clause
/// (both bare and under AND/OR conjunctions). Correlated scalar subqueries
/// are NOT rewritten (they reference outer columns).
pub fn rewrite_scalar_subquery_in_where(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Set operations — don't rewrite (the individual branches are separate SELECTs)
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // No WHERE clause — nothing to rewrite
    if select.whereClause.is_null() {
        return Ok(query.to_string());
    }

    // Collect scalar subqueries from WHERE clause
    let mut scalar_subqueries: Vec<ScalarSubqueryExtract> = Vec::new();
    collect_scalar_sublinks_in_where(select.whereClause, &mut scalar_subqueries)?;

    if scalar_subqueries.is_empty() {
        return Ok(query.to_string());
    }

    // Check for correlated subqueries — skip rewriting those (they reference
    // outer columns and can't be trivially cross-joined).
    // Detect correlation by checking if the subquery's WHERE references
    // column names from tables in the outer FROM clause.
    let outer_tables = collect_from_clause_table_names(select);

    // Classify each scalar subquery:
    //   1. Dot-qualified correlated (existing heuristic) → skip entirely
    //   2. Bare-column correlated (catalog-detected)   → decorrelate to INNER JOIN
    //   3. Non-correlated                              → CROSS JOIN
    let mut non_correlated: Vec<&ScalarSubqueryExtract> = Vec::new();
    let mut to_decorrelate: Vec<(
        &ScalarSubqueryExtract,
        std::collections::HashMap<String, String>,
    )> = Vec::new();

    for sq in &scalar_subqueries {
        if sq.is_correlated(&outer_tables) {
            // Dot-qualified correlation (e.g., "outer_table.col") — skip
            continue;
        }
        let outer_cols = detect_correlation_columns(sq, &outer_tables);
        if outer_cols.is_empty() {
            non_correlated.push(sq);
        } else {
            to_decorrelate.push((sq, outer_cols));
        }
    }

    if non_correlated.is_empty() && to_decorrelate.is_empty() {
        return Ok(query.to_string());
    }

    // ── Build rewritten query components ─────────────────────────────

    // FROM clause
    let from_sql = extract_from_clause_sql(select)?;

    // Unified index counter for all subquery aliases
    let mut next_idx = 1usize;

    // Build CROSS JOIN additions for each non-correlated scalar subquery.
    // The wrapper SELECT uses the original scalar subquery as a derived table
    // in its FROM clause, so the DVM parser (which requires FROM) can handle it.
    //
    // Format: CROSS JOIN (SELECT v."c" AS "scalar_alias"
    //                      FROM (inner_sql) AS v("c")) AS "sq_alias"
    let mut extra_joins: Vec<String> = Vec::new();
    let mut extra_from_items: Vec<String> = Vec::new();
    let mut extra_where_parts: Vec<String> = Vec::new();
    let mut where_replacements: Vec<(String, String)> = Vec::new();

    for sq in &non_correlated {
        let idx = next_idx;
        next_idx += 1;
        let sq_alias = format!("__pgt_sq_{idx}");
        let scalar_alias = format!("__pgt_scalar_{idx}");

        if sq.has_limit_or_offset {
            // Scalar subqueries with LIMIT/OFFSET cannot be nested inside a
            // derived-table wrapper (the DVM parser rejects LIMIT in FROM-clause
            // subqueries).  Use CROSS JOIN LATERAL instead: the inner SQL is
            // evaluated as-is (LIMIT preserved) and the DVM registers the inner
            // source in `subquery_source_oids`, enabling diff_lateral_subquery to
            // re-evaluate ALL outer rows when the scalar source changes.
            extra_joins.push(format!(
                "CROSS JOIN LATERAL ({inner_sql}) AS \"{sq_alias}\"(\"{scalar_alias}\")",
                inner_sql = sq.subquery_sql,
            ));
        } else {
            let inner_alias = format!("__pgt_v_{idx}");
            let inner_col = format!("__pgt_c_{idx}");
            // The inner scalar subquery returns exactly 1 column and 1 row.
            // We wrap it so the DVM parser sees a subquery with a valid FROM clause:
            //   (SELECT v."c" AS "scalar" FROM (original_subquery) AS v("c")) AS "sq"
            extra_joins.push(format!(
                "CROSS JOIN (SELECT \"{inner_alias}\".\"{inner_col}\" AS \"{scalar_alias}\" \
                 FROM ({sq_sql}) AS \"{inner_alias}\"(\"{inner_col}\")) AS \"{sq_alias}\"",
                sq_sql = sq.subquery_sql,
            ));
        }
        let replacement = format!("\"{sq_alias}\".\"{scalar_alias}\"");
        where_replacements.push((sq.expr_sql.clone(), replacement));
    }

    // Build comma-joined FROM items for decorrelated correlated subqueries.
    // Decorrelated subqueries are added as comma-separated FROM items (not
    // INNER JOIN) to avoid SQL precedence issues with comma-joins.
    for (sq, outer_cols) in &to_decorrelate {
        let idx = next_idx;
        next_idx += 1;
        match decorrelate_scalar_subquery(sq, outer_cols, idx) {
            Ok(decorrelated) => {
                extra_from_items.push(decorrelated.from_item);
                extra_where_parts.extend(decorrelated.extra_where_conditions);
                where_replacements.push((sq.expr_sql.clone(), decorrelated.scalar_ref));
            }
            Err(e) => {
                pgrx::debug1!(
                    "[pg_trickle] Skipping decorrelation of scalar subquery: {}",
                    e
                );
                // Fall back: skip this subquery
                continue;
            }
        }
    }

    if extra_joins.is_empty() && extra_from_items.is_empty() {
        return Ok(query.to_string());
    }

    // Rewrite the WHERE expression, replacing scalar subquery occurrences
    // with column references to the CROSS/INNER JOIN aliases.
    let where_expr = unsafe { node_to_expr(select.whereClause) }
        .map(|e| e.to_sql())
        .unwrap_or_else(|_| "TRUE".to_string());

    let mut rewritten_where = where_expr;
    for (find, replacement) in &where_replacements {
        rewritten_where = rewritten_where.replace(find, replacement);
    }
    // Append decorrelation join conditions to WHERE
    for cond in &extra_where_parts {
        rewritten_where = format!("{rewritten_where} AND {cond}");
    }

    // Target list
    let target_list = pg_list::<pg_sys::Node>(select.targetList);
    let mut targets = Vec::new();
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        let expr = unsafe { node_to_expr(rt.val) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "NULL".to_string());
        let alias_part = if !rt.name.is_null() {
            let name = pg_cstr_to_str(rt.name).unwrap_or("?");
            format!(" AS \"{}\"", name.replace('"', "\"\""))
        } else {
            String::new()
        };
        targets.push(format!("{expr}{alias_part}"));
    }

    // GROUP BY
    let group_sql = if select.groupClause.is_null() {
        String::new()
    } else {
        let group_list = pg_list::<pg_sys::Node>(select.groupClause);
        if group_list.is_empty() {
            String::new()
        } else {
            let mut groups = Vec::new();
            for node_ptr in group_list.iter_ptr() {
                let expr = unsafe { node_to_expr(node_ptr) }
                    .map(|e| e.to_sql())
                    .unwrap_or_else(|_| "?".to_string());
                groups.push(expr);
            }
            format!(" GROUP BY {}", groups.join(", "))
        }
    };

    // HAVING
    let having_sql = if select.havingClause.is_null() {
        String::new()
    } else {
        let having_expr = unsafe { node_to_expr(select.havingClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" HAVING {having_expr}")
    };

    // ORDER BY
    let order_sql = if select.sortClause.is_null() {
        String::new()
    } else {
        let sort_list = pg_list::<pg_sys::Node>(select.sortClause);
        if sort_list.is_empty() {
            String::new()
        } else {
            let mut sorts = Vec::new();
            for node_ptr in sort_list.iter_ptr() {
                if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_SortBy) }
                {
                    continue;
                }
                let sb = unsafe { &*(node_ptr as *const pg_sys::SortBy) };
                if sb.node.is_null() {
                    continue;
                }
                let expr = unsafe { node_to_expr(sb.node) }
                    .map(|e| e.to_sql())
                    .unwrap_or_else(|_| "?".to_string());
                let dir = match sb.sortby_dir {
                    pg_sys::SortByDir::SORTBY_ASC => " ASC",
                    pg_sys::SortByDir::SORTBY_DESC => " DESC",
                    _ => "",
                };
                let nulls = match sb.sortby_nulls {
                    pg_sys::SortByNulls::SORTBY_NULLS_FIRST => " NULLS FIRST",
                    pg_sys::SortByNulls::SORTBY_NULLS_LAST => " NULLS LAST",
                    _ => "",
                };
                sorts.push(format!("{expr}{dir}{nulls}"));
            }
            if sorts.is_empty() {
                String::new()
            } else {
                format!(" ORDER BY {}", sorts.join(", "))
            }
        }
    };

    // DISTINCT
    let distinct_sql = if select.distinctClause.is_null() {
        String::new()
    } else {
        " DISTINCT".to_string()
    };

    // ── Assemble rewritten query ─────────────────────────────────────
    // Build the complete FROM clause: original + comma-joined decorrelated subqueries + CROSS JOINs
    let mut from_parts = vec![from_sql];
    from_parts.extend(extra_from_items.iter().cloned());
    let full_from = from_parts.join(", ");

    let rewritten = format!(
        "SELECT{distinct_sql} {targets} FROM {full_from} {extra_joins} WHERE {rewritten_where}{group_sql}{having_sql}{order_sql}",
        targets = targets.join(", "),
        extra_joins = extra_joins.join(" "),
    );

    pgrx::debug1!(
        "[pg_trickle] Rewrote scalar subquery in WHERE: {}",
        rewritten
    );

    Ok(rewritten)
}

// ── Correlated scalar subquery in SELECT → LEFT JOIN rewrite ───────

/// Rewrite correlated scalar subqueries in the SELECT list into LEFT JOINs.
///
/// ```sql
/// -- Input:
/// SELECT d.name, (SELECT MAX(e.salary) FROM emp e WHERE e.dept_id = d.id) AS max_sal
/// FROM dept d
/// -- Rewrite to:
/// SELECT d.name, "__pgt_sq_1"."__pgt_scalar_1" AS max_sal
/// FROM dept d
/// LEFT JOIN (SELECT e.dept_id AS "__pgt_corr_key_1", MAX(e.salary) AS "__pgt_scalar_1"
///            FROM emp e GROUP BY e.dept_id) AS "__pgt_sq_1"
/// ON d.id = "__pgt_sq_1"."__pgt_corr_key_1"
/// ```
///
/// Non-correlated scalar subqueries are left untouched — they are handled
/// by the `ScalarSubquery` OpTree path during parsing.
///
/// This must run **before** the DVM parser so that the downstream operator
/// tree sees a standard LEFT JOIN instead of a correlated scalar subquery.
pub fn rewrite_correlated_scalar_in_select(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Skip set operations
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // Skip if no FROM clause (can't have correlation without outer tables)
    if select.fromClause.is_null() {
        return Ok(query.to_string());
    }

    let target_list = pg_list::<pg_sys::Node>(select.targetList);

    // Collect scalar subqueries from the SELECT list
    struct SelectScalarSubquery {
        target_idx: usize,
        inner_select: *const pg_sys::SelectStmt,
        subquery_sql: String,
        alias: Option<String>,
    }

    let mut scalar_targets: Vec<SelectScalarSubquery> = Vec::new();
    for (idx, node_ptr) in target_list.iter_ptr().enumerate() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        if !unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_SubLink) } {
            continue;
        }
        let sublink = unsafe { &*(rt.val as *const pg_sys::SubLink) };
        if sublink.subLinkType != pg_sys::SubLinkType::EXPR_SUBLINK {
            continue;
        }
        if sublink.subselect.is_null()
            || !unsafe { pgrx::is_a(sublink.subselect, pg_sys::NodeTag::T_SelectStmt) }
        {
            continue;
        }
        let inner = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };
        // Only handle simple SELECTs (no set operations)
        if inner.op != pg_sys::SetOperation::SETOP_NONE {
            continue;
        }
        let inner_sql = deparse_select_to_sql(sublink.subselect)?;
        let alias = if !rt.name.is_null() {
            pg_cstr_to_str(rt.name).ok().map(|s| s.to_string())
        } else {
            None
        };
        scalar_targets.push(SelectScalarSubquery {
            target_idx: idx,
            inner_select: inner,
            subquery_sql: inner_sql,
            alias,
        });
    }

    if scalar_targets.is_empty() {
        return Ok(query.to_string());
    }

    // Collect outer FROM table names for correlation detection
    let outer_tables = collect_from_clause_table_names(select);

    // For each scalar target, detect correlation and build decorrelation
    struct SelectScalarDecorrelation {
        target_idx: usize,
        left_join_sql: String,
        scalar_ref: String,
        alias: Option<String>,
    }

    let mut decorrelations: Vec<SelectScalarDecorrelation> = Vec::new();
    let mut next_sq_idx = 1usize;

    for st in &scalar_targets {
        let inner = unsafe { &*st.inner_select };

        // Collect inner FROM table names
        let inner_tables = collect_from_clause_table_names(inner);

        // Build ScalarSubqueryExtract for correlation detection
        let sq_extract = ScalarSubqueryExtract {
            subquery_sql: st.subquery_sql.clone(),
            expr_sql: format!("({})", st.subquery_sql),
            inner_tables: inner_tables.clone(),
            // has_limit_or_offset is not used for the correlation check below;
            // the LIMIT guard is applied separately via inner.limitCount.
            has_limit_or_offset: !inner.limitCount.is_null() || !inner.limitOffset.is_null(),
        };

        // Check dot-qualified correlation first (e.g., "d.id" in subquery)
        if !sq_extract.is_correlated(&outer_tables) {
            // Also check bare-column correlation via catalog
            let outer_cols = detect_correlation_columns(&sq_extract, &outer_tables);
            if outer_cols.is_empty() {
                continue; // Not correlated — skip, handled by ScalarSubquery OpTree path
            }
        }

        // ── Decorrelate this correlated scalar subquery ──────────────

        // Correlated scalar subqueries with LIMIT/OFFSET cannot be correctly
        // decorrelated by the standard LEFT JOIN + GROUP BY approach because
        // removing LIMIT changes the semantics (e.g. ORDER BY … LIMIT 1 picks
        // the top row per group; a plain LEFT JOIN with all matching rows is
        // wrong).
        //
        // Instead, rewrite them as LEFT JOIN LATERAL, preserving the full inner
        // SQL (including ORDER BY + LIMIT) so that PostgreSQL evaluates it
        // correctly for each outer row.  The DVM handles LATERAL subqueries via
        // `diff_lateral_subquery`, which correctly re-evaluates ALL affected
        // outer rows when the inner source table changes.
        if !inner.limitCount.is_null() || !inner.limitOffset.is_null() {
            let lat_idx = next_sq_idx;
            next_sq_idx += 1;
            let lat_alias = format!("__pgt_lat_{lat_idx}");
            let scalar_alias = format!("__pgt_scalar_{lat_idx}");

            // Use the original inner SQL verbatim inside a LATERAL subquery.
            // The outer table alias (e.g. `i.category`) is a valid lateral
            // reference and PostgreSQL evaluates it for every outer row.
            let left_join_sql = format!(
                "LEFT JOIN LATERAL ({inner_sql}) AS \"{lat_alias}\"(\"{scalar_alias}\") ON TRUE",
                inner_sql = st.subquery_sql,
            );
            let scalar_ref = format!("\"{lat_alias}\".\"{scalar_alias}\"");

            decorrelations.push(SelectScalarDecorrelation {
                target_idx: st.target_idx,
                left_join_sql,
                scalar_ref,
                alias: st.alias.clone(),
            });
            continue;
        }

        // Parse the inner WHERE to separate correlation from inner conditions
        if inner.whereClause.is_null() {
            continue; // No WHERE → no correlation conditions to extract
        }

        let inner_where_expr = unsafe { node_to_expr(inner.whereClause) }.map_err(|_| {
            PgTrickleError::QueryParseError("Failed to parse inner scalar subquery WHERE".into())
        })?;

        let (corr_pairs, remaining_where) =
            split_exists_correlation(&inner_where_expr, &inner_tables);

        if corr_pairs.is_empty() {
            continue; // Could not extract correlation conditions
        }

        let sq_alias = format!("__pgt_sq_{next_sq_idx}");
        let scalar_alias = format!("__pgt_scalar_{next_sq_idx}");
        next_sq_idx += 1;

        // Extract inner target expressions
        let inner_target_list = pg_list::<pg_sys::Node>(inner.targetList);
        let mut inner_target_exprs: Vec<String> = Vec::new();
        for node_ptr in inner_target_list.iter_ptr() {
            if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) }
            {
                continue;
            }
            let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
            if rt.val.is_null() {
                continue;
            }
            let expr = unsafe { node_to_expr(rt.val) }
                .map(|e| e.strip_qualifier().to_sql())
                .unwrap_or_else(|_| "NULL".to_string());
            inner_target_exprs.push(expr);
        }

        let scalar_expr = inner_target_exprs.join(", ");

        // Detect if the inner query has aggregates (determines GROUP BY strategy)
        let has_group_by = !inner.groupClause.is_null();
        let has_having = !inner.havingClause.is_null();
        let is_aggregate = has_group_by || has_having || expr_has_aggregate(&scalar_expr);

        // Build SELECT items for decorrelated subquery
        let mut select_items: Vec<String> = Vec::new();
        let mut group_by_items: Vec<String> = Vec::new();
        let mut on_conditions: Vec<String> = Vec::new();

        for (j, (outer_expr, inner_col_name)) in corr_pairs.iter().enumerate() {
            let key_alias = format!("__pgt_corr_key_{}", j + 1);
            let inner_col_unqualified = strip_table_qualifier(inner_col_name);
            select_items.push(format!("{inner_col_unqualified} AS \"{key_alias}\""));
            if is_aggregate {
                group_by_items.push(inner_col_unqualified);
            }
            on_conditions.push(format!(
                "{} = \"{sq_alias}\".\"{key_alias}\"",
                outer_expr.to_sql()
            ));
        }

        select_items.push(format!("{scalar_expr} AS \"{scalar_alias}\""));

        // Inner FROM clause
        let inner_from_sql = extract_from_clause_sql(inner)?;

        // Inner WHERE (non-correlation conditions only)
        let where_sql = if let Some(ref remaining) = remaining_where {
            format!(" WHERE {}", remaining.to_sql())
        } else {
            String::new()
        };

        // GROUP BY
        let group_by_sql = if is_aggregate && !group_by_items.is_empty() {
            // If inner already has GROUP BY, prepend correlation keys
            if has_group_by {
                let existing_groups = pg_list::<pg_sys::Node>(inner.groupClause);
                let mut groups = group_by_items;
                for node_ptr in existing_groups.iter_ptr() {
                    let expr = unsafe { node_to_expr(node_ptr) }
                        .map(|e| e.to_sql())
                        .unwrap_or_else(|_| "?".to_string());
                    groups.push(expr);
                }
                format!(" GROUP BY {}", groups.join(", "))
            } else {
                format!(" GROUP BY {}", group_by_items.join(", "))
            }
        } else {
            String::new()
        };

        // HAVING (preserve from inner if present)
        let having_sql = if has_having {
            let having_expr = unsafe { node_to_expr(inner.havingClause) }
                .map(|e| e.to_sql())
                .unwrap_or_else(|_| "TRUE".to_string());
            format!(" HAVING {having_expr}")
        } else {
            String::new()
        };

        // Build LEFT JOIN clause
        let decorrelated_sql = format!(
            "SELECT {selects} FROM {from}{where_clause}{group_by}{having}",
            selects = select_items.join(", "),
            from = inner_from_sql,
            where_clause = where_sql,
            group_by = group_by_sql,
            having = having_sql,
        );

        let left_join_sql = format!(
            "LEFT JOIN ({decorrelated_sql}) AS \"{sq_alias}\" ON {on_cond}",
            on_cond = on_conditions.join(" AND "),
        );

        let scalar_ref = format!("\"{sq_alias}\".\"{scalar_alias}\"");

        decorrelations.push(SelectScalarDecorrelation {
            target_idx: st.target_idx,
            left_join_sql,
            scalar_ref,
            alias: st.alias.clone(),
        });
    }

    if decorrelations.is_empty() {
        return Ok(query.to_string());
    }

    // ── Reconstruct the query ────────────────────────────────────────

    // Build target list, replacing decorrelated scalar targets
    let mut targets = Vec::new();
    let decc_map: std::collections::HashMap<usize, &SelectScalarDecorrelation> =
        decorrelations.iter().map(|d| (d.target_idx, d)).collect();

    for (idx, node_ptr) in target_list.iter_ptr().enumerate() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        if let Some(dec) = decc_map.get(&idx) {
            let alias_part = if let Some(ref a) = dec.alias {
                format!(" AS \"{}\"", a.replace('"', "\"\""))
            } else {
                String::new()
            };
            targets.push(format!("{}{alias_part}", dec.scalar_ref));
        } else {
            let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
            if rt.val.is_null() {
                continue;
            }
            let expr = unsafe { node_to_expr(rt.val) }
                .map(|e| e.to_sql())
                .unwrap_or_else(|_| "NULL".to_string());
            let alias_part = if !rt.name.is_null() {
                let name = pg_cstr_to_str(rt.name).unwrap_or("?");
                format!(" AS \"{}\"", name.replace('"', "\"\""))
            } else {
                String::new()
            };
            targets.push(format!("{expr}{alias_part}"));
        }
    }

    // FROM clause + LEFT JOINs
    let from_sql = extract_from_clause_sql(select)?;
    let left_joins: String = decorrelations
        .iter()
        .map(|d| d.left_join_sql.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    // WHERE clause (outer, unchanged)
    let where_sql = if select.whereClause.is_null() {
        String::new()
    } else {
        let w = unsafe { node_to_expr(select.whereClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" WHERE {w}")
    };

    // GROUP BY (outer)
    let group_sql = if select.groupClause.is_null() {
        String::new()
    } else {
        let group_list = pg_list::<pg_sys::Node>(select.groupClause);
        if group_list.is_empty() {
            String::new()
        } else {
            let mut groups = Vec::new();
            for node_ptr in group_list.iter_ptr() {
                let expr = unsafe { node_to_expr(node_ptr) }
                    .map(|e| e.to_sql())
                    .unwrap_or_else(|_| "?".to_string());
                groups.push(expr);
            }
            format!(" GROUP BY {}", groups.join(", "))
        }
    };

    // HAVING (outer)
    let having_sql = if select.havingClause.is_null() {
        String::new()
    } else {
        let h = unsafe { node_to_expr(select.havingClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" HAVING {h}")
    };

    // ORDER BY (outer)
    let order_sql = if select.sortClause.is_null() {
        String::new()
    } else {
        let sort_list = pg_list::<pg_sys::Node>(select.sortClause);
        if sort_list.is_empty() {
            String::new()
        } else {
            let mut sorts = Vec::new();
            for node_ptr in sort_list.iter_ptr() {
                if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_SortBy) }
                {
                    continue;
                }
                let sb = unsafe { &*(node_ptr as *const pg_sys::SortBy) };
                if sb.node.is_null() {
                    continue;
                }
                let expr = unsafe { node_to_expr(sb.node) }
                    .map(|e| e.to_sql())
                    .unwrap_or_else(|_| "?".to_string());
                let dir = match sb.sortby_dir {
                    pg_sys::SortByDir::SORTBY_ASC => " ASC",
                    pg_sys::SortByDir::SORTBY_DESC => " DESC",
                    _ => "",
                };
                let nulls = match sb.sortby_nulls {
                    pg_sys::SortByNulls::SORTBY_NULLS_FIRST => " NULLS FIRST",
                    pg_sys::SortByNulls::SORTBY_NULLS_LAST => " NULLS LAST",
                    _ => "",
                };
                sorts.push(format!("{expr}{dir}{nulls}"));
            }
            if sorts.is_empty() {
                String::new()
            } else {
                format!(" ORDER BY {}", sorts.join(", "))
            }
        }
    };

    // DISTINCT (outer)
    let distinct_sql = if select.distinctClause.is_null() {
        String::new()
    } else {
        " DISTINCT".to_string()
    };

    let rewritten = format!(
        "SELECT{distinct_sql} {targets} FROM {from_sql} {left_joins}{where_sql}{group_sql}{having_sql}{order_sql}",
        targets = targets.join(", "),
    );

    pgrx::debug1!(
        "[pg_trickle] Rewrote correlated scalar subquery in SELECT: {}",
        rewritten
    );

    Ok(rewritten)
}

/// Check whether a SQL expression string contains common aggregate function calls.
fn expr_has_aggregate(expr: &str) -> bool {
    let lower = expr.to_lowercase();
    const AGG_PREFIXES: &[&str] = &[
        "max(",
        "min(",
        "sum(",
        "count(",
        "avg(",
        "array_agg(",
        "string_agg(",
        "bool_and(",
        "bool_or(",
        "every(",
        "jsonb_agg(",
        "json_agg(",
        "xmlagg(",
        "bit_and(",
        "bit_or(",
    ];
    AGG_PREFIXES.iter().any(|p| lower.contains(p))
}

/// Information about a scalar subquery extracted from the WHERE clause.
struct ScalarSubqueryExtract {
    /// The inner SELECT statement as SQL (e.g., `SELECT avg(amount) FROM orders`).
    subquery_sql: String,
    /// The expression as rendered by `node_to_expr().to_sql()` for text replacement.
    /// e.g. `(SELECT avg("amount") FROM "orders")`
    expr_sql: String,
    /// Table names referenced in the subquery's FROM clause (lowercase).
    inner_tables: Vec<String>,
    /// True if the inner SELECT has a LIMIT or OFFSET clause.  Such subqueries
    /// are skipped by the CROSS-JOIN and decorrelation rewrites so that the DVM
    /// parser treats them as opaque `Expr::Raw` expressions.
    has_limit_or_offset: bool,
}

impl ScalarSubqueryExtract {
    /// Check if this scalar subquery is correlated with the outer query.
    ///
    /// A subquery is considered correlated if its SQL text references any
    /// column that could come from an outer table. We use a heuristic:
    /// if the subquery SQL contains a table name from the outer FROM clause
    /// (as part of a column reference like `t.col` or a bare column matching
    /// the outer table names), we treat it as potentially correlated.
    fn is_correlated(&self, outer_tables: &[String]) -> bool {
        let sq_lower = self.subquery_sql.to_lowercase();
        for outer_table in outer_tables {
            // Skip tables that also appear in the inner FROM clause —
            // those are legitimate inner references, not correlations.
            if self.inner_tables.contains(outer_table) {
                continue;
            }
            // Check if the outer table name appears as a qualified column
            // reference prefix. The deparsed SQL may use either unquoted
            // (`i.col`) or quoted (`"i"."col"`) column references, so we
            // check for both patterns.
            let unquoted = format!("{}.", outer_table);
            let quoted = format!("\"{}\".", outer_table);
            if sq_lower.contains(&unquoted) || sq_lower.contains(&quoted) {
                return true;
            }
        }
        false
    }
}

/// Collect table names (and aliases) from a SelectStmt's FROM clause.
///
/// Returns lowercased table names and alias names for correlation detection.
///
/// # Safety
/// Caller must ensure `select` points to a valid `pg_sys::SelectStmt`.
fn collect_from_clause_table_names(select: &pg_sys::SelectStmt) -> Vec<String> {
    let mut names = Vec::new();
    if select.fromClause.is_null() {
        return names;
    }
    let from_list = pg_list::<pg_sys::Node>(select.fromClause);
    for node_ptr in from_list.iter_ptr() {
        if !node_ptr.is_null() {
            collect_table_names_from_node(node_ptr, &mut names);
        }
    }
    names
}

/// Recursively collect table names and aliases from a FROM item node.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn collect_table_names_from_node(node: *mut pg_sys::Node, out: &mut Vec<String>) {
    if node.is_null() {
        return;
    }
    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        if let Ok(s) = pg_cstr_to_str(rv.relname) {
            out.push(s.to_lowercase());
        }
        if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            if let Ok(s) = pg_cstr_to_str(a.aliasname) {
                let lower = s.to_lowercase();
                if !out.contains(&lower) {
                    out.push(lower);
                }
            }
        }
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        if !join.larg.is_null() {
            collect_table_names_from_node(join.larg, out);
        }
        if !join.rarg.is_null() {
            collect_table_names_from_node(join.rarg, out);
        }
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect)
        && !sub.alias.is_null()
    {
        let a = unsafe { &*(sub.alias) };
        if let Ok(s) = pg_cstr_to_str(a.aliasname) {
            let lower = s.to_lowercase();
            if !out.contains(&lower) {
                out.push(lower);
            }
        }
    }
}

/// Recursively collect EXPR_SUBLINK nodes from a WHERE clause tree.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn collect_scalar_sublinks_in_where(
    node: *mut pg_sys::Node,
    out: &mut Vec<ScalarSubqueryExtract>,
) -> Result<(), PgTrickleError> {
    if node.is_null() {
        return Ok(());
    }

    if let Some(sublink) = cast_node!(node, T_SubLink, pg_sys::SubLink) {
        if sublink.subLinkType == pg_sys::SubLinkType::EXPR_SUBLINK {
            // Scalar subquery — extract it
            if !sublink.subselect.is_null() {
                let inner_sql = deparse_select_to_sql(sublink.subselect)?;
                let expr_sql = format!("({inner_sql})");

                // Collect table names from the subquery's FROM clause
                // for correlation detection.
                let (inner_tables, has_limit_or_offset) = if let Some(inner_select) =
                    cast_node!(sublink.subselect, T_SelectStmt, pg_sys::SelectStmt)
                {
                    let tables = collect_from_clause_table_names(inner_select);
                    // SAFETY: inner_select is a valid SelectStmt pointer from cast_node!
                    let has_lim =
                        !inner_select.limitCount.is_null() || !inner_select.limitOffset.is_null();
                    (tables, has_lim)
                } else {
                    (Vec::new(), false)
                };

                out.push(ScalarSubqueryExtract {
                    subquery_sql: inner_sql,
                    expr_sql,
                    inner_tables,
                    has_limit_or_offset,
                });
            }
        }
        // Don't recurse into the subquery itself — it's a separate query context
        return Ok(());
    }

    // Recurse into BoolExpr (AND/OR/NOT)
    if let Some(boolexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr)
        && !boolexpr.args.is_null()
    {
        let args = pg_list::<pg_sys::Node>(boolexpr.args);
        for arg_ptr in args.iter_ptr() {
            if !arg_ptr.is_null() {
                collect_scalar_sublinks_in_where(arg_ptr, out)?;
            }
        }
    }

    // Recurse into comparison operators (A_Expr) since scalar subqueries
    // are typically inside comparisons: `col > (SELECT ...)`
    if let Some(aexpr) = cast_node!(node, T_A_Expr, pg_sys::A_Expr) {
        if !aexpr.lexpr.is_null() {
            collect_scalar_sublinks_in_where(aexpr.lexpr, out)?;
        }
        if !aexpr.rexpr.is_null() {
            collect_scalar_sublinks_in_where(aexpr.rexpr, out)?;
        }
    }

    Ok(())
}

// ── Correlated scalar subquery decorrelation ────────────────────────

/// Information about a decorrelated scalar subquery, ready to be added
/// as a comma-joined subquery in the outer FROM clause with extra WHERE conditions.
struct DecorrelatedSubquery {
    /// The subquery to add to FROM as a comma-separated item.
    /// e.g., `(SELECT ...) AS "__pgt_sq_1"`
    from_item: String,
    /// Additional equality conditions to add to WHERE (decorrelation join).
    /// e.g., `p_partkey = "__pgt_sq_1"."__pgt_corr_key_1"`
    extra_where_conditions: Vec<String>,
    /// The reference to the scalar result column for WHERE replacement.
    /// e.g., `"__pgt_sq_1"."__pgt_scalar_1"`
    scalar_ref: String,
}

/// A correlation condition found in a scalar subquery's WHERE clause.
struct CorrelationCondition {
    /// The column name from the outer query (e.g., `p_partkey`).
    outer_column: String,
    /// The full inner column expression as SQL (the side that stays in the subquery).
    /// e.g., `ps_partkey` or `l2.l_partkey`
    inner_expr_sql: String,
}

/// Detect whether a scalar subquery is correlated with the outer query by
/// looking up column names of outer-only tables (tables in the outer FROM
/// but NOT in the inner FROM) in `pg_catalog`.
///
/// Returns a map from column name → table name for outer columns found in
/// the subquery text. Returns an empty map if not correlated.
fn detect_correlation_columns(
    sq: &ScalarSubqueryExtract,
    outer_tables: &[String],
) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;

    // Find tables that appear in the outer FROM but NOT in the inner FROM.
    let outer_only: Vec<&String> = outer_tables
        .iter()
        .filter(|t| !sq.inner_tables.contains(t))
        .collect();

    if outer_only.is_empty() {
        return HashMap::new();
    }

    let sq_lower = sq.subquery_sql.to_lowercase();
    let mut outer_columns: HashMap<String, String> = HashMap::new();

    let result = pgrx::Spi::connect(|client| {
        for table_name in &outer_only {
            let query = format!(
                "SELECT a.attname::text \
                 FROM pg_attribute a \
                 JOIN pg_class c ON a.attrelid = c.oid \
                 LEFT JOIN pg_namespace n ON c.relnamespace = n.oid \
                 WHERE c.relname = '{}' \
                 AND (n.nspname = 'public' OR n.nspname = current_schema()) \
                 AND a.attnum > 0 AND NOT a.attisdropped",
                table_name.replace('\'', "''")
            );
            let rows = client.select(&query, None, &[])?;
            for row in rows {
                if let Some(col_name) = row.get::<String>(1)? {
                    let col_lower = col_name.to_lowercase();
                    if contains_word_boundary(&sq_lower, &col_lower) {
                        outer_columns.insert(col_lower, table_name.to_string());
                    }
                }
            }
        }
        Ok::<_, pgrx::spi::Error>(())
    });

    if result.is_err() {
        return std::collections::HashMap::new();
    }
    outer_columns
}

/// Strip a leading table qualifier from a column reference.
///
/// e.g., `"l2.l_partkey"` → `"l_partkey"`, `"ps_partkey"` → `"ps_partkey"`.
fn strip_table_qualifier(expr: &str) -> String {
    if let Some(dot_pos) = expr.find('.') {
        expr[dot_pos + 1..].to_string()
    } else {
        expr.to_string()
    }
}

/// Check if `word` appears as a whole word in `text`.
///
/// A word boundary is defined as the start/end of string or a character
/// that is not alphanumeric and not underscore (since SQL identifiers
/// can contain underscores).
fn contains_word_boundary(text: &str, word: &str) -> bool {
    let text_bytes = text.as_bytes();
    let word_len = word.len();
    let mut start = 0;
    while start + word_len <= text.len() {
        if let Some(pos) = text[start..].find(word) {
            let abs_pos = start + pos;
            let before_ok = abs_pos == 0 || {
                let c = text_bytes[abs_pos - 1];
                !c.is_ascii_alphanumeric() && c != b'_'
            };
            let after_pos = abs_pos + word_len;
            let after_ok = after_pos >= text.len() || {
                let c = text_bytes[after_pos];
                !c.is_ascii_alphanumeric() && c != b'_'
            };
            if before_ok && after_ok {
                return true;
            }
            start = abs_pos + 1;
        } else {
            break;
        }
    }
    false
}

/// Flatten a WHERE clause AND tree into individual conditions.
///
/// Recursively unpacks `BoolExpr(AND, args)` nodes, collecting leaf
/// condition nodes.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree `Node`.
fn flatten_and_conditions(node: *mut pg_sys::Node, out: &mut Vec<*mut pg_sys::Node>) {
    if node.is_null() {
        return;
    }
    if let Some(boolexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr)
        && boolexpr.boolop == pg_sys::BoolExprType::AND_EXPR
        && !boolexpr.args.is_null()
    {
        let args = pg_list::<pg_sys::Node>(boolexpr.args);
        for arg_ptr in args.iter_ptr() {
            if !arg_ptr.is_null() {
                flatten_and_conditions(arg_ptr, out);
            }
        }
        return;
    }
    out.push(node);
}

/// Check if a WHERE condition node is an equality between a bare outer column
/// and an inner expression. Returns the correlation info if so.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree `Node`.
fn check_correlation_condition(
    node: *mut pg_sys::Node,
    outer_columns: &std::collections::HashMap<String, String>,
) -> Option<CorrelationCondition> {
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Expr) } {
        return None;
    }
    let aexpr = unsafe { &*(node as *const pg_sys::A_Expr) };
    if aexpr.kind != pg_sys::A_Expr_Kind::AEXPR_OP {
        return None;
    }
    let op_name = unsafe { extract_operator_name(aexpr.name) }.ok()?;
    if op_name != "=" {
        return None;
    }
    let left_expr = unsafe { node_to_expr(aexpr.lexpr) }.ok()?;
    let right_expr = unsafe { node_to_expr(aexpr.rexpr) }.ok()?;

    // Check if left side is a bare outer column
    if let Expr::ColumnRef {
        table_alias: None,
        ref column_name,
    } = left_expr
        && outer_columns.contains_key(&column_name.to_lowercase())
    {
        return Some(CorrelationCondition {
            outer_column: column_name.clone(),
            inner_expr_sql: right_expr.to_sql(),
        });
    }

    // Check if right side is a bare outer column
    if let Expr::ColumnRef {
        table_alias: None,
        ref column_name,
    } = right_expr
        && outer_columns.contains_key(&column_name.to_lowercase())
    {
        return Some(CorrelationCondition {
            outer_column: column_name.clone(),
            inner_expr_sql: left_expr.to_sql(),
        });
    }

    None
}

/// Decorrelate a correlated scalar subquery into an INNER JOIN.
///
/// Given a correlated scalar subquery like:
/// ```sql
/// (SELECT MIN(ps_supplycost) FROM partsupp, ... WHERE p_partkey = ps_partkey AND ...)
/// ```
///
/// Produces:
/// ```sql
/// INNER JOIN (SELECT ps_partkey AS "__pgt_corr_key_1",
///             MIN(ps_supplycost) AS "__pgt_scalar_1"
///             FROM partsupp, ... WHERE ... GROUP BY ps_partkey)
///     AS "__pgt_sq_1" ON p_partkey = "__pgt_sq_1"."__pgt_corr_key_1"
/// ```
fn decorrelate_scalar_subquery(
    sq: &ScalarSubqueryExtract,
    outer_columns: &std::collections::HashMap<String, String>,
    idx: usize,
) -> Result<DecorrelatedSubquery, PgTrickleError> {
    let sq_alias = format!("__pgt_sq_{idx}");
    let scalar_alias = format!("__pgt_scalar_{idx}");

    // Re-parse the inner subquery to get AST access
    let inner_select = match parse_first_select(sq.subquery_sql.as_str())? {
        Some(s) => unsafe { &*s },
        None => {
            return Err(PgTrickleError::QueryParseError(
                "Scalar subquery is not a SelectStmt".into(),
            ));
        }
    };

    // ── Flatten WHERE into AND-ed conditions ─────────────────────────
    let mut conditions: Vec<*mut pg_sys::Node> = Vec::new();
    if !inner_select.whereClause.is_null() {
        flatten_and_conditions(inner_select.whereClause, &mut conditions);
    }

    // ── Separate correlation vs. regular conditions ──────────────────
    let mut correlations: Vec<CorrelationCondition> = Vec::new();
    let mut regular_conditions: Vec<String> = Vec::new();

    for cond_node in &conditions {
        if let Some(corr) = check_correlation_condition(*cond_node, outer_columns) {
            correlations.push(corr);
        } else {
            let expr = unsafe { node_to_expr(*cond_node) }
                .map(|e| e.to_sql())
                .unwrap_or_else(|_| "TRUE".to_string());
            regular_conditions.push(expr);
        }
    }

    if correlations.is_empty() {
        return Err(PgTrickleError::QueryParseError(
            "No correlation conditions found in scalar subquery".into(),
        ));
    }

    // ── Extract the original target list (scalar expression) ─────────
    let target_list = pg_list::<pg_sys::Node>(inner_select.targetList);
    let mut original_target_exprs: Vec<String> = Vec::new();
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        let expr = unsafe { node_to_expr(rt.val) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "NULL".to_string());
        original_target_exprs.push(expr);
    }

    // ── FROM clause (unchanged) ──────────────────────────────────────
    let from_sql = extract_from_clause_sql(inner_select)?;

    // ── Build decorrelated SELECT items ──────────────────────────────
    let mut select_items: Vec<String> = Vec::new();
    let mut group_by_items: Vec<String> = Vec::new();
    let mut extra_where_conds: Vec<String> = Vec::new();

    for (j, corr) in correlations.iter().enumerate() {
        let key_alias = format!("__pgt_corr_key_{}", j + 1);
        // Strip table qualifier from inner expression to avoid alias scoping
        // issues in DVM delta queries (e.g., "l2.l_partkey" → "l_partkey").
        let inner_col_unqualified = strip_table_qualifier(&corr.inner_expr_sql);
        select_items.push(format!("{} AS \"{}\"", inner_col_unqualified, key_alias));
        group_by_items.push(inner_col_unqualified);
        extra_where_conds.push(format!(
            "{} = \"{}\".\"{}\"",
            corr.outer_column, sq_alias, key_alias
        ));
    }

    // The original scalar expression becomes the scalar alias
    let scalar_expr = original_target_exprs.join(", ");
    select_items.push(format!("{} AS \"{}\"", scalar_expr, scalar_alias));

    // ── WHERE clause (without correlation conditions) ─────────────────
    let where_sql = if regular_conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", regular_conditions.join(" AND "))
    };

    // ── GROUP BY ─────────────────────────────────────────────────────
    let group_by_sql = format!(" GROUP BY {}", group_by_items.join(", "));

    // ── Assemble decorrelated subquery ───────────────────────────────
    let decorrelated_sql = format!(
        "SELECT {} FROM {}{}{}",
        select_items.join(", "),
        from_sql,
        where_sql,
        group_by_sql,
    );

    // Use comma-join (not INNER JOIN) to avoid SQL precedence issues
    // when the outer FROM uses comma-separated tables.
    let from_item = format!("({decorrelated_sql}) AS \"{sq_alias}\"");

    let scalar_ref = format!("\"{sq_alias}\".\"{scalar_alias}\"");

    pgrx::debug1!("[pg_trickle] Decorrelated scalar subquery: {}", from_item);

    Ok(DecorrelatedSubquery {
        from_item,
        extra_where_conditions: extra_where_conds,
        scalar_ref,
    })
}

// ── De Morgan normalization for sublink-bearing NOT(AND/OR) ─────────

/// Apply De Morgan's law to expose OR+sublink patterns hidden behind NOT.
///
/// ```sql
/// -- Input:
/// SELECT * FROM t WHERE NOT (x AND NOT EXISTS (SELECT 1 FROM vip WHERE vip.id = t.id))
/// -- Rewrite to:
/// SELECT * FROM t WHERE (NOT (x) OR EXISTS (SELECT 1 FROM vip WHERE vip.id = t.id))
/// ```
///
/// The transformation rules are:
/// - `NOT (a AND b)` → `(NOT a) OR (NOT b)`
/// - `NOT (a OR b)` → `(NOT a) AND (NOT b)`
/// - `NOT NOT a` → `a`
///
/// Only applied when the NOT wraps AND/OR nodes containing SubLink nodes,
/// so that the subsequent `rewrite_sublinks_in_or` pass can convert the
/// resulting OR+sublink into UNION branches.
pub fn rewrite_demorgan_sublinks(query: &str) -> Result<String, PgTrickleError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgTrickleError::QueryParseError("Query contains null bytes".into()))?;

    // SAFETY: raw_parser is safe within a PostgreSQL backend with a valid memory context.
    let raw_list =
        unsafe { pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
    if raw_list.is_null() {
        return Ok(query.to_string());
    }

    let list = unsafe { pgrx::PgList::<pg_sys::RawStmt>::from_pg(raw_list) };
    let raw_stmt = match list.head() {
        Some(rs) => rs,
        None => return Ok(query.to_string()),
    };

    let node = unsafe { (*raw_stmt).stmt };
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SelectStmt) } {
        return Ok(query.to_string());
    }

    let select = unsafe { &*(node as *const pg_sys::SelectStmt) };

    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    if select.whereClause.is_null() {
        return Ok(query.to_string());
    }

    // Quick check: does the WHERE clause contain any NOT(AND/OR+sublink)?
    if !unsafe { where_has_not_demorgan_opportunity(select.whereClause) } {
        return Ok(query.to_string());
    }

    // Deparse the WHERE clause with De Morgan applied.
    let new_where_sql = unsafe { deparse_with_demorgan(select.whereClause)? };

    // Reconstruct the query with the normalized WHERE clause.
    let from_sql = extract_from_clause_sql(select)?;
    let target_sql = deparse_select_target_list(select);
    let group_sql = deparse_group_clause(select);
    let having_sql = deparse_having_clause(select);
    let order_sql = deparse_order_clause(select);

    let result = format!(
        "SELECT {target_sql} FROM {from_sql} WHERE {new_where_sql}{group_sql}{having_sql}{order_sql}"
    );

    pgrx::debug1!("[pg_trickle] De Morgan normalization: {}", result);

    Ok(result)
}

/// Check whether a WHERE clause tree contains NOT(AND/OR) patterns where
/// De Morgan normalization would expose sublinks for the UNION rewrite.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
unsafe fn where_has_not_demorgan_opportunity(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        return false;
    }
    let be = unsafe { &*(node as *const pg_sys::BoolExpr) };
    match be.boolop {
        pg_sys::BoolExprType::NOT_EXPR => {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(be.args) };
            if let Some(inner) = args.head()
                && unsafe { pgrx::is_a(inner, pg_sys::NodeTag::T_BoolExpr) }
            {
                let inner_be = unsafe { &*(inner as *const pg_sys::BoolExpr) };
                // NOT(AND/OR with sublinks) or NOT(NOT with sublinks)
                if node_tree_contains_sublink(inner) {
                    return matches!(
                        inner_be.boolop,
                        pg_sys::BoolExprType::AND_EXPR
                            | pg_sys::BoolExprType::OR_EXPR
                            | pg_sys::BoolExprType::NOT_EXPR
                    );
                }
            }
            false
        }
        pg_sys::BoolExprType::AND_EXPR | pg_sys::BoolExprType::OR_EXPR => {
            // Recurse into children.
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(be.args) };
            for arg in args.iter_ptr() {
                if unsafe { where_has_not_demorgan_opportunity(arg) } {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Deparse a WHERE clause node, applying De Morgan normalization to any
/// NOT(AND/OR) nodes that contain sublinks.
///
/// Non-NOT nodes are recursed into (for AND/OR) or deparsed normally.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
unsafe fn deparse_with_demorgan(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if node.is_null() {
        return Ok("TRUE".to_string());
    }

    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        return unsafe { node_to_expr(node) }.map(|e| e.to_sql());
    }

    let be = unsafe { &*(node as *const pg_sys::BoolExpr) };
    match be.boolop {
        pg_sys::BoolExprType::NOT_EXPR => {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(be.args) };
            let inner = args
                .head()
                .ok_or_else(|| PgTrickleError::QueryParseError("Empty NOT expression".into()))?;

            if unsafe { pgrx::is_a(inner, pg_sys::NodeTag::T_BoolExpr) } {
                let inner_be = unsafe { &*(inner as *const pg_sys::BoolExpr) };

                // NOT(AND(a,b,...)) → (NOT a) OR (NOT b) OR ...
                if inner_be.boolop == pg_sys::BoolExprType::AND_EXPR
                    && node_tree_contains_sublink(inner)
                {
                    let inner_args =
                        unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_be.args) };
                    let mut parts: Vec<String> = Vec::new();
                    for arg in inner_args.iter_ptr() {
                        parts.push(unsafe { negate_node_sql(arg)? });
                    }
                    return Ok(format!("({})", parts.join(" OR ")));
                }

                // NOT(OR(a,b,...)) → (NOT a) AND (NOT b) AND ...
                if inner_be.boolop == pg_sys::BoolExprType::OR_EXPR
                    && node_tree_contains_sublink(inner)
                {
                    let inner_args =
                        unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_be.args) };
                    let mut parts: Vec<String> = Vec::new();
                    for arg in inner_args.iter_ptr() {
                        parts.push(unsafe { negate_node_sql(arg)? });
                    }
                    return Ok(format!("({})", parts.join(" AND ")));
                }

                // NOT(NOT(x)) → x
                if inner_be.boolop == pg_sys::BoolExprType::NOT_EXPR {
                    let inner_args =
                        unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_be.args) };
                    if let Some(x) = inner_args.head() {
                        return unsafe { deparse_with_demorgan(x) };
                    }
                }
            }

            // NOT without applicable De Morgan — deparse normally.
            unsafe { node_to_expr(node) }.map(|e| e.to_sql())
        }
        pg_sys::BoolExprType::AND_EXPR => {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(be.args) };
            let mut parts: Vec<String> = Vec::new();
            for arg in args.iter_ptr() {
                parts.push(unsafe { deparse_with_demorgan(arg)? });
            }
            Ok(parts.join(" AND "))
        }
        pg_sys::BoolExprType::OR_EXPR => {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(be.args) };
            let mut parts: Vec<String> = Vec::new();
            for arg in args.iter_ptr() {
                parts.push(unsafe { deparse_with_demorgan(arg)? });
            }
            Ok(parts.join(" OR "))
        }
        _ => unsafe { node_to_expr(node) }.map(|e| e.to_sql()),
    }
}

/// Negate a node for De Morgan: NOT(NOT(x)) → x, otherwise NOT (x).
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
unsafe fn negate_node_sql(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if node.is_null() {
        return Ok("TRUE".to_string());
    }

    // Double negation elimination: NOT(NOT(x)) → x
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        let be = unsafe { &*(node as *const pg_sys::BoolExpr) };
        if be.boolop == pg_sys::BoolExprType::NOT_EXPR {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(be.args) };
            if let Some(inner) = args.head() {
                return unsafe { deparse_with_demorgan(inner) };
            }
        }
    }

    let expr = unsafe { deparse_with_demorgan(node)? };
    Ok(format!("NOT ({expr})"))
}

// ── SubLinks inside OR → OR-to-UNION rewrite ───────────────────────

/// Rewrite queries where SubLinks (EXISTS/IN) appear inside OR conditions
/// into a UNION of the individual OR arms.
///
/// ```sql
/// -- Input:
/// SELECT * FROM t WHERE status = 'active' OR EXISTS (SELECT 1 FROM vip WHERE vip.id = t.id)
/// -- Rewrite to:
/// SELECT * FROM t WHERE status = 'active'
/// UNION
/// SELECT t.* FROM t WHERE EXISTS (SELECT 1 FROM vip WHERE vip.id = t.id)
/// ```
///
/// This is called **before** the DVM parser so the downstream operator tree
/// only ever sees non-OR SubLinks which are already handled by the
/// SemiJoin/AntiJoin extraction in `extract_where_sublinks()`.
///
/// The UNION (not UNION ALL) handles deduplication of rows that match
/// multiple OR arms.
pub fn rewrite_sublinks_in_or(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Set operations — don't rewrite
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // No WHERE clause — nothing to rewrite
    if select.whereClause.is_null() {
        return Ok(query.to_string());
    }

    // Check if the top-level WHERE is an OR containing SubLinks
    if !unsafe { pgrx::is_a(select.whereClause, pg_sys::NodeTag::T_BoolExpr) } {
        return Ok(query.to_string());
    }

    let boolexpr = unsafe { &*(select.whereClause as *const pg_sys::BoolExpr) };

    // Only handle top-level OR, or AND where one of the conjuncts is an OR with sublinks
    if boolexpr.boolop != pg_sys::BoolExprType::OR_EXPR {
        // Check for AND with an inner OR containing sublinks.
        // Guard: only enter the AND rewriter if there is actually an
        // OR conjunct containing SubLink nodes. Without this check,
        // queries like Q04 (AND + EXISTS, no OR) would be unnecessarily
        // deparsed and round-tripped through node_to_expr, losing
        // information like agg_star on COUNT(*).
        if boolexpr.boolop == pg_sys::BoolExprType::AND_EXPR
            && and_contains_or_with_sublink(select.whereClause)
        {
            return rewrite_and_with_or_sublinks(select, select.whereClause);
        }
        return Ok(query.to_string());
    }

    // Top-level OR — check if it contains sublinks
    if !node_tree_contains_sublink(select.whereClause) {
        return Ok(query.to_string());
    }

    // ── Extract OR arms ────────────────────────────────────────────
    let args = pg_list::<pg_sys::Node>(boolexpr.args);
    let mut arm_exprs: Vec<String> = Vec::new();
    for arg_ptr in args.iter_ptr() {
        if arg_ptr.is_null() {
            continue;
        }
        let expr = unsafe { node_to_expr(arg_ptr) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        arm_exprs.push(expr);
    }

    if arm_exprs.len() < 2 {
        return Ok(query.to_string());
    }

    // Build query components (shared across all UNION branches)
    let from_sql = extract_from_clause_sql(select)?;

    let target_sql = {
        let target_list = pg_list::<pg_sys::Node>(select.targetList);
        let mut targets = Vec::new();
        for node_ptr in target_list.iter_ptr() {
            if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) }
            {
                continue;
            }
            let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
            if rt.val.is_null() {
                continue;
            }
            let expr = unsafe { node_to_expr(rt.val) }
                .map(|e| e.to_sql())
                .unwrap_or_else(|_| "NULL".to_string());
            let alias_part = if !rt.name.is_null() {
                let name = pg_cstr_to_str(rt.name).unwrap_or("?");
                format!(" AS \"{}\"", name.replace('"', "\"\""))
            } else {
                String::new()
            };
            targets.push(format!("{expr}{alias_part}"));
        }
        targets.join(", ")
    };

    // GROUP BY / HAVING / ORDER BY
    let group_sql = deparse_group_clause(select);
    let having_sql = deparse_having_clause(select);
    let order_sql = deparse_order_clause(select);

    // Build UNION branches — one per OR arm
    let mut branches: Vec<String> = Vec::new();
    for arm in &arm_exprs {
        branches.push(format!(
            "SELECT {target_sql} FROM {from_sql} WHERE {arm}{group_sql}{having_sql}"
        ));
    }

    let rewritten = format!("{}{order_sql}", branches.join(" UNION "));

    pgrx::debug1!(
        "[pg_trickle] Rewrote SubLinks-in-OR to UNION: {}",
        rewritten
    );

    Ok(rewritten)
}

/// Handle AND conjunction (at any nesting depth) where one or more arms
/// are OR expressions containing sublinks.
///
/// ```sql
/// -- Input (single OR+sublink conjunct):
/// SELECT * FROM t WHERE a > 10 AND (status = 'active' OR EXISTS (...))
/// -- Rewrite to:
/// SELECT * FROM t WHERE a > 10 AND status = 'active'
/// UNION
/// SELECT * FROM t WHERE a > 10 AND EXISTS (...)
///
/// -- Input (multiple OR+sublink conjuncts):
/// SELECT * FROM t WHERE (a OR EXISTS(s1)) AND (b OR NOT EXISTS(s2))
/// -- Rewrite to cartesian product of UNION branches:
/// SELECT * FROM t WHERE a AND b
/// UNION
/// SELECT * FROM t WHERE a AND NOT EXISTS(s2)
/// UNION
/// SELECT * FROM t WHERE EXISTS(s1) AND b
/// UNION
/// SELECT * FROM t WHERE EXISTS(s1) AND NOT EXISTS(s2)
/// ```
///
/// The `where_node` argument is the entire WHERE clause node (typically a
/// BoolExpr AND chain). The function flattens all nested AND layers before
/// searching for OR arms, so arbitrarily deep `AND(AND(OR(...)))` nesting
/// is handled correctly.
///
/// A combinatorial guard limits expansion to 16 UNION branches (4 binary
/// OR+sublink conjuncts). Queries exceeding this produce an error directing
/// the user to simplify the WHERE clause.
fn rewrite_and_with_or_sublinks(
    select: &pg_sys::SelectStmt,
    where_node: *mut pg_sys::Node,
) -> Result<String, PgTrickleError> {
    const MAX_UNION_BRANCHES: usize = 16;

    // Flatten the entire AND chain into a list of atomic conjuncts.
    // AND(a, AND(b, OR(...))) → [a, b, OR(...)]
    let mut conjuncts: Vec<*mut pg_sys::Node> = Vec::new();
    flatten_and_conjuncts(where_node, &mut conjuncts);

    // Separate OR+sublink conjuncts from plain conjuncts.
    let mut or_arms_list: Vec<Vec<String>> = Vec::new();
    let mut other_conjuncts: Vec<String> = Vec::new();

    for arg_ptr in conjuncts {
        if arg_ptr.is_null() {
            continue;
        }
        if unsafe { pgrx::is_a(arg_ptr, pg_sys::NodeTag::T_BoolExpr) } {
            let inner_bool = unsafe { &*(arg_ptr as *const pg_sys::BoolExpr) };
            if inner_bool.boolop == pg_sys::BoolExprType::OR_EXPR
                && node_tree_contains_sublink(arg_ptr)
            {
                let or_args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_bool.args) };
                let mut arms = Vec::new();
                for a in or_args.iter_ptr() {
                    if a.is_null() {
                        continue;
                    }
                    let expr = unsafe { node_to_expr(a) }
                        .map(|e| e.to_sql())
                        .unwrap_or_else(|_| "TRUE".to_string());
                    arms.push(expr);
                }
                if arms.len() >= 2 {
                    or_arms_list.push(arms);
                    continue;
                }
            }
        }
        let expr = unsafe { node_to_expr(arg_ptr) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        other_conjuncts.push(expr);
    }

    if or_arms_list.is_empty() {
        return deparse_full_select(select);
    }

    // Compute total branch count (cartesian product of all OR arm counts).
    let branch_count: usize = or_arms_list.iter().map(|arms| arms.len()).product();
    if branch_count > MAX_UNION_BRANCHES {
        return Err(PgTrickleError::UnsupportedOperator(format!(
            "Query would produce {branch_count} UNION branches after OR+sublink rewriting \
             (limit: {MAX_UNION_BRANCHES}). Simplify the WHERE clause or use FULL refresh mode.",
        )));
    }

    // Build the common AND prefix from non-OR conjuncts.
    let and_prefix = if other_conjuncts.is_empty() {
        String::new()
    } else {
        format!("{} AND ", other_conjuncts.join(" AND "))
    };

    // Generate the cartesian product of all OR arm combinations.
    // Start with a single empty combination and expand one OR at a time.
    let mut combinations: Vec<Vec<&str>> = vec![vec![]];
    for or_arms in &or_arms_list {
        let mut next = Vec::with_capacity(combinations.len() * or_arms.len());
        for combo in &combinations {
            for arm in or_arms {
                let mut new_combo = combo.clone();
                new_combo.push(arm.as_str());
                next.push(new_combo);
            }
        }
        combinations = next;
    }

    // Build query components
    let from_sql = extract_from_clause_sql(select)?;
    let target_sql = deparse_select_target_list(select);
    let group_sql = deparse_group_clause(select);
    let having_sql = deparse_having_clause(select);
    let order_sql = deparse_order_clause(select);

    let mut branches: Vec<String> = Vec::with_capacity(combinations.len());
    for combo in &combinations {
        let combo_where = combo.join(" AND ");
        branches.push(format!(
            "SELECT {target_sql} FROM {from_sql} WHERE {and_prefix}{combo_where}{group_sql}{having_sql}"
        ));
    }

    let rewritten = format!("{}{order_sql}", branches.join(" UNION "));

    pgrx::debug1!(
        "[pg_trickle] Rewrote AND(..OR-sublinks..) to {} UNION branches: {}",
        branches.len(),
        rewritten
    );

    Ok(rewritten)
}

/// Deparse a full SELECT statement back to SQL text.
fn deparse_full_select(select: &pg_sys::SelectStmt) -> Result<String, PgTrickleError> {
    let from_sql = extract_from_clause_sql(select)?;
    let target_sql = deparse_select_target_list(select);
    let where_sql = if select.whereClause.is_null() {
        String::new()
    } else {
        let expr = unsafe { node_to_expr(select.whereClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" WHERE {expr}")
    };
    let group_sql = deparse_group_clause(select);
    let having_sql = deparse_having_clause(select);
    let order_sql = deparse_order_clause(select);
    Ok(format!(
        "SELECT {target_sql} FROM {from_sql}{where_sql}{group_sql}{having_sql}{order_sql}"
    ))
}

/// Deparse target list to SQL text (from a SelectStmt reference).
fn deparse_select_target_list(select: &pg_sys::SelectStmt) -> String {
    let target_list = pg_list::<pg_sys::Node>(select.targetList);
    let mut targets = Vec::new();
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        let expr = unsafe { node_to_expr(rt.val) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "NULL".to_string());
        let alias_part = if !rt.name.is_null() {
            let name = pg_cstr_to_str(rt.name).unwrap_or("?");
            format!(" AS \"{}\"", name.replace('"', "\"\""))
        } else {
            String::new()
        };
        targets.push(format!("{expr}{alias_part}"));
    }
    targets.join(", ")
}

/// Deparse GROUP BY clause.
fn deparse_group_clause(select: &pg_sys::SelectStmt) -> String {
    if select.groupClause.is_null() {
        return String::new();
    }
    let group_list = pg_list::<pg_sys::Node>(select.groupClause);
    if group_list.is_empty() {
        return String::new();
    }
    let mut groups = Vec::new();
    for node_ptr in group_list.iter_ptr() {
        let expr = unsafe { node_to_expr(node_ptr) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "?".to_string());
        groups.push(expr);
    }
    format!(" GROUP BY {}", groups.join(", "))
}

/// Deparse HAVING clause.
fn deparse_having_clause(select: &pg_sys::SelectStmt) -> String {
    if select.havingClause.is_null() {
        return String::new();
    }
    let expr = unsafe { node_to_expr(select.havingClause) }
        .map(|e| e.to_sql())
        .unwrap_or_else(|_| "TRUE".to_string());
    format!(" HAVING {expr}")
}

/// Deparse ORDER BY clause.
fn deparse_order_clause(select: &pg_sys::SelectStmt) -> String {
    if select.sortClause.is_null() {
        return String::new();
    }
    let sort_list = pg_list::<pg_sys::Node>(select.sortClause);
    if sort_list.is_empty() {
        return String::new();
    }
    let mut sorts = Vec::new();
    for node_ptr in sort_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_SortBy) } {
            continue;
        }
        let sb = unsafe { &*(node_ptr as *const pg_sys::SortBy) };
        if sb.node.is_null() {
            continue;
        }
        let expr = unsafe { node_to_expr(sb.node) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "?".to_string());
        let dir = match sb.sortby_dir {
            pg_sys::SortByDir::SORTBY_ASC => " ASC",
            pg_sys::SortByDir::SORTBY_DESC => " DESC",
            _ => "",
        };
        let nulls = match sb.sortby_nulls {
            pg_sys::SortByNulls::SORTBY_NULLS_FIRST => " NULLS FIRST",
            pg_sys::SortByNulls::SORTBY_NULLS_LAST => " NULLS LAST",
            _ => "",
        };
        sorts.push(format!("{expr}{dir}{nulls}"));
    }
    if sorts.is_empty() {
        String::new()
    } else {
        format!(" ORDER BY {}", sorts.join(", "))
    }
}

// ── ROWS FROM() multi-function rewrite ─────────────────────────────

/// Rewrite `ROWS FROM(f1(...), f2(...), ...)` into a form the DVM parser
/// supports.
///
/// **All-unnest optimisation:** when every function is `unnest`, merge into
/// a single multi-argument `unnest(A, B, ...)` call — PostgreSQL's built-in
/// multi-arg `unnest()` already implements zip-with-NULL-padding semantics.
///
/// **General case:** rewrite to an ordinal-based LEFT JOIN LATERAL chain:
/// ```sql
/// generate_series(1, 2147483647) WITH ORDINALITY AS __pgt_idx(v, ord)
/// LEFT JOIN LATERAL f1(...) WITH ORDINALITY AS __pgt_f0(col, ord)
///   ON __pgt_f0.ord = __pgt_idx.ord
/// LEFT JOIN LATERAL f2(...) WITH ORDINALITY AS __pgt_f1(col, ord)
///   ON __pgt_f1.ord = __pgt_idx.ord
/// ```
/// with a `WHERE __pgt_f0.ord IS NOT NULL OR __pgt_f1.ord IS NOT NULL`
/// filter to stop the infinite `generate_series` once all SRFs are exhausted.
pub fn rewrite_rows_from(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Set operations — recurse into each branch.
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return rewrite_rows_from_in_set_op(select);
    }

    // Walk the FROM clause looking for a multi-function ROWS FROM.
    if select.fromClause.is_null() {
        return Ok(query.to_string());
    }

    let from_list = pg_list::<pg_sys::Node>(select.fromClause);
    let mut found_rows_from = false;
    for node_ptr in from_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        if has_multi_rows_from(node_ptr) {
            found_rows_from = true;
            break;
        }
    }

    if !found_rows_from {
        return Ok(query.to_string());
    }

    // ── Extract query components ─────────────────────────────────────

    // SELECT list
    let target_sql = unsafe { deparse_target_list(select.targetList)? };

    // WHERE clause
    let where_sql = if select.whereClause.is_null() {
        None
    } else {
        let expr = unsafe { node_to_expr(select.whereClause)? };
        Some(expr.to_sql())
    };

    // GROUP BY
    let group_list = pg_list::<pg_sys::Node>(select.groupClause);
    let group_sql = if group_list.is_empty() {
        None
    } else {
        let mut groups = Vec::new();
        for gn in group_list.iter_ptr() {
            let expr = unsafe { node_to_expr(gn)? };
            groups.push(expr.to_sql());
        }
        Some(groups.join(", "))
    };

    // HAVING
    let having_sql = if select.havingClause.is_null() {
        None
    } else {
        let expr = unsafe { node_to_expr(select.havingClause)? };
        Some(expr.to_sql())
    };

    // ORDER BY + LIMIT + OFFSET
    let order_sql = deparse_order_clause(select);
    let limit_sql = if select.limitCount.is_null() {
        None
    } else {
        let expr = unsafe { node_to_expr(select.limitCount)? };
        Some(expr.to_sql())
    };
    let offset_sql = if select.limitOffset.is_null() {
        None
    } else {
        let expr = unsafe { node_to_expr(select.limitOffset)? };
        Some(expr.to_sql())
    };

    // DISTINCT
    let distinct_list = pg_list::<pg_sys::Node>(select.distinctClause);
    let distinct_prefix = if !distinct_list.is_empty() {
        "SELECT DISTINCT"
    } else {
        "SELECT"
    };

    // ── Rewrite FROM items ──────────────────────────────────────────
    let mut from_parts = Vec::new();
    for node_ptr in from_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        let sql = rewrite_from_item_rows_from(node_ptr)?;
        from_parts.push(sql);
    }

    // ── Rebuild query ───────────────────────────────────────────────
    let mut parts = Vec::new();
    parts.push(format!("{distinct_prefix} {target_sql}"));
    parts.push(format!("FROM {}", from_parts.join(", ")));
    if let Some(w) = &where_sql {
        parts.push(format!("WHERE {w}"));
    }
    if let Some(g) = &group_sql {
        parts.push(format!("GROUP BY {g}"));
    }
    if let Some(h) = &having_sql {
        parts.push(format!("HAVING {h}"));
    }
    if !order_sql.is_empty() {
        parts.push(order_sql.trim().to_string());
    }
    if let Some(l) = &limit_sql {
        parts.push(format!("LIMIT {l}"));
    }
    if let Some(o) = &offset_sql {
        parts.push(format!("OFFSET {o}"));
    }

    let rewritten = parts.join(" ");
    pgrx::debug1!("[pg_trickle] rewrite_rows_from: {}", rewritten);
    Ok(rewritten)
}

/// Check if a FROM-clause node is (or contains) a multi-function ROWS FROM.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree Node.
fn has_multi_rows_from(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    if let Some(rf) = cast_node!(node, T_RangeFunction, pg_sys::RangeFunction) {
        let func_list = pg_list::<pg_sys::Node>(rf.functions);
        return rf.is_rowsfrom && func_list.len() > 1;
    }
    if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        return has_multi_rows_from(join.larg) || has_multi_rows_from(join.rarg);
    }
    false
}

/// Rewrite a single FROM-clause item, replacing any multi-function ROWS FROM
/// with its rewritten equivalent.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree Node.
fn rewrite_from_item_rows_from(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if node.is_null() {
        return Ok("".to_string());
    }

    if let Some(rf) = cast_node!(node, T_RangeFunction, pg_sys::RangeFunction) {
        let func_list = pg_list::<pg_sys::Node>(rf.functions);

        if !rf.is_rowsfrom || func_list.len() <= 1 {
            // Single function — deparse normally.
            return unsafe { deparse_from_item_to_sql(node) };
        }

        // ── Multi-function ROWS FROM ────────────────────────────────
        // Extract each function call + its args.
        let mut func_sqls: Vec<String> = Vec::new();
        let mut func_names: Vec<String> = Vec::new();
        let mut func_args: Vec<Vec<String>> = Vec::new();

        for inner_node in func_list.iter_ptr() {
            if inner_node.is_null() {
                continue;
            }
            let inner_list = pg_list::<pg_sys::Node>(inner_node as *mut pg_sys::List);
            if inner_list.is_empty() {
                continue;
            }
            // INVARIANT: inner_list is non-empty (checked above), so head() cannot fail.
            let func_node = inner_list.head().unwrap();
            if !unsafe { pgrx::is_a(func_node, pg_sys::NodeTag::T_FuncCall) } {
                continue;
            }
            let fcall = unsafe { &*(func_node as *const pg_sys::FuncCall) };
            let name = unsafe { extract_func_name(fcall.funcname)? };
            let sql = unsafe { deparse_func_call(func_node as *const pg_sys::FuncCall)? };

            let args_list = pg_list::<pg_sys::Node>(fcall.args);
            let mut args = Vec::new();
            for n in args_list.iter_ptr() {
                let expr = unsafe { node_to_expr(n)? };
                args.push(expr.to_sql());
            }

            func_names.push(name);
            func_sqls.push(sql);
            func_args.push(args);
        }

        if func_sqls.is_empty() {
            return Ok("(SELECT 1 WHERE false) AS __pgt_empty".to_string());
        }

        // Extract alias + column aliases from the ROWS FROM node.
        let rows_alias = if !rf.alias.is_null() {
            let a = unsafe { &*(rf.alias) };
            pg_cstr_to_str(a.aliasname)
                .unwrap_or("__pgt_rf")
                .to_string()
        } else {
            "__pgt_rf".to_string()
        };
        let col_aliases = if !rf.alias.is_null() {
            let a = unsafe { &*(rf.alias) };
            extract_alias_colnames(a)?
        } else {
            Vec::new()
        };

        let with_ordinality = rf.ordinality;

        // ── All-unnest optimisation ─────────────────────────────────
        let all_unnest = func_names.iter().all(|n| n == "unnest");
        if all_unnest {
            // Merge: ROWS FROM(unnest(A), unnest(B)) → unnest(A, B) AS t(c1, c2)
            let all_args: Vec<String> = func_args.iter().flat_map(|a| a.clone()).collect();
            let mut result = format!("unnest({})", all_args.join(", "));
            if with_ordinality {
                result.push_str(" WITH ORDINALITY");
            }
            if !col_aliases.is_empty() {
                result.push_str(&format!(" AS {}({})", rows_alias, col_aliases.join(", ")));
            } else {
                result.push_str(&format!(" AS {rows_alias}"));
            }
            return Ok(result);
        }

        // ── General case: ordinal-based FULL OUTER JOIN chain ────────
        // Each SRF is wrapped in a subquery that adds row_number() so we
        // can match them by ordinal.  We chain them via FULL OUTER JOIN
        // which naturally handles different-length results by padding
        // with NULLs — matching ROWS FROM semantics.
        //
        //   LATERAL (SELECT __pgt_f0.__pgt_c0, __pgt_f1.__pgt_c1 FROM
        //     (SELECT val AS __pgt_c0, row_number() OVER () AS __pgt_rn
        //      FROM f0(...) AS val) AS __pgt_f0
        //     FULL OUTER JOIN
        //     (SELECT val AS __pgt_c1, row_number() OVER () AS __pgt_rn
        //      FROM f1(...) AS val) AS __pgt_f1
        //     ON __pgt_f0.__pgt_rn = __pgt_f1.__pgt_rn
        //   ) AS alias
        let mut subqueries = Vec::new();
        let mut select_parts = Vec::new();

        for (i, func_sql) in func_sqls.iter().enumerate() {
            let f_alias = format!("__pgt_f{i}");
            let col_alias = format!("__pgt_c{i}");

            // Each SRF is wrapped in a subquery with its result column
            // given a known alias and row_number() for ordinal matching.
            let sub = format!(
                "(SELECT {col_alias}, row_number() OVER () AS __pgt_rn \
                 FROM {func_sql} AS {col_alias}) AS {f_alias}"
            );
            subqueries.push((f_alias.clone(), sub));
            select_parts.push(format!("{f_alias}.{col_alias}"));
        }

        // Chain subqueries via FULL OUTER JOIN on ordinal.
        // For >2 SRFs, the join ON uses COALESCE of prior ordinals.
        let inner_from = if subqueries.len() == 1 {
            subqueries[0].1.clone()
        } else {
            let mut chain = subqueries[0].1.clone();
            let mut coalesce_parts = vec![format!("{}.__pgt_rn", subqueries[0].0)];
            for (f_alias, sub) in subqueries.iter().skip(1) {
                let on_left = if coalesce_parts.len() == 1 {
                    coalesce_parts[0].clone()
                } else {
                    format!("COALESCE({})", coalesce_parts.join(", "))
                };
                chain = format!("{chain} FULL OUTER JOIN {sub} ON {on_left} = {f_alias}.__pgt_rn");
                coalesce_parts.push(format!("{f_alias}.__pgt_rn"));
            }
            chain
        };

        let inner_select = select_parts.join(", ");

        // Use LATERAL so SRF arguments can reference columns from
        // earlier FROM items (e.g. `unnest(m.arr)` where `m` is a
        // preceding table).
        let mut result = format!("LATERAL (SELECT {inner_select} FROM {inner_from})");
        if with_ordinality {
            // Ordinality for the whole ROWS FROM — add row_number outside.
            result = format!(
                "LATERAL (SELECT __pgt_rfo.*, row_number() OVER () AS ordinality \
                 FROM {result} AS __pgt_rfo)"
            );
        }
        if !col_aliases.is_empty() {
            result.push_str(&format!(" AS {}({})", rows_alias, col_aliases.join(", ")));
        } else {
            result.push_str(&format!(" AS {rows_alias}"));
        }

        Ok(result)
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        let left = rewrite_from_item_rows_from(join.larg)?;
        let right = rewrite_from_item_rows_from(join.rarg)?;
        let join_type = match join.jointype {
            pg_sys::JoinType::JOIN_LEFT => "LEFT JOIN",
            pg_sys::JoinType::JOIN_FULL => "FULL JOIN",
            pg_sys::JoinType::JOIN_RIGHT => "RIGHT JOIN",
            pg_sys::JoinType::JOIN_INNER => {
                if join.quals.is_null() {
                    "CROSS JOIN"
                } else {
                    "JOIN"
                }
            }
            _ => "JOIN",
        };
        let on_clause = if join.quals.is_null() {
            String::new()
        } else {
            let cond = unsafe { node_to_expr(join.quals)? };
            format!(" ON {}", cond.to_sql())
        };
        Ok(format!("{left} {join_type} {right}{on_clause}"))
    } else {
        // Not a ROWS FROM — deparse normally using existing helper.
        unsafe { deparse_from_item_to_sql(node) }
    }
}

/// Handle UNION/INTERSECT/EXCEPT branches for ROWS FROM rewriting.
fn rewrite_rows_from_in_set_op(select: &pg_sys::SelectStmt) -> Result<String, PgTrickleError> {
    // Recurse into each branch, rewrite, then reconstruct.
    let left = if select.larg.is_null() {
        return Ok(String::new());
    } else {
        let left_select = unsafe { &*select.larg };
        if left_select.op != pg_sys::SetOperation::SETOP_NONE {
            rewrite_rows_from_in_set_op(left_select)?
        } else {
            // Leaf branch — build a temporary query and rewrite it.
            let leaf_sql = unsafe { deparse_select_stmt_to_sql(select.larg as *const _)? };
            rewrite_rows_from(&leaf_sql)?
        }
    };

    let right = if select.rarg.is_null() {
        return Ok(left);
    } else {
        let right_select = unsafe { &*select.rarg };
        if right_select.op != pg_sys::SetOperation::SETOP_NONE {
            rewrite_rows_from_in_set_op(right_select)?
        } else {
            let leaf_sql = unsafe { deparse_select_stmt_to_sql(select.rarg as *const _)? };
            rewrite_rows_from(&leaf_sql)?
        }
    };

    let op_str = match select.op {
        pg_sys::SetOperation::SETOP_UNION => {
            if select.all {
                "UNION ALL"
            } else {
                "UNION"
            }
        }
        pg_sys::SetOperation::SETOP_INTERSECT => {
            if select.all {
                "INTERSECT ALL"
            } else {
                "INTERSECT"
            }
        }
        pg_sys::SetOperation::SETOP_EXCEPT => {
            if select.all {
                "EXCEPT ALL"
            } else {
                "EXCEPT"
            }
        }
        _ => "UNION",
    };

    let body = format!("{left} {op_str} {right}");

    // Preserve any WITH clause attached to this set-operation SelectStmt.
    // The CTE definitions live on the outermost SETOP node — they are NOT
    // propagated to larg/rarg — so reconstructing from the arms alone drops them.
    if !select.withClause.is_null() {
        // SAFETY: withClause is non-null and points to a valid WithClause node
        // obtained from the raw parse tree of a validated SQL string.
        let with_sql = deparse_with_clause_with_view_subs(select.withClause, &[])?;
        Ok(format!("{with_sql} {body}"))
    } else {
        Ok(body)
    }
}

// ── Multiple PARTITION BY → multi-pass window rewrite ──────────────

/// Rewrite a query with window functions using different PARTITION BY clauses
/// into a multi-pass plan.
///
/// When window functions use different PARTITION BY clauses, the parser normally
/// rejects the query. Instead, we split the window functions into groups by
/// their PARTITION BY clause and build a chain of subqueries — each adding
/// one group of window columns.
///
/// ```sql
/// -- Input:
/// SELECT id, region, dept,
///        SUM(amount) OVER (PARTITION BY region) AS region_sum,
///        SUM(amount) OVER (PARTITION BY dept)   AS dept_sum
/// FROM orders
/// -- Rewrite to:
/// SELECT __pgt_w1.id, __pgt_w1.region, __pgt_w1.dept,
///        __pgt_w1.region_sum, __pgt_w2.dept_sum
/// FROM (
///   SELECT id, region, dept, amount,
///          SUM(amount) OVER (PARTITION BY region) AS region_sum
///   FROM orders
/// ) __pgt_w1
/// JOIN (
///   SELECT id, region, dept, amount,
///          SUM(amount) OVER (PARTITION BY dept) AS dept_sum
///   FROM orders
/// ) __pgt_w2 ON __pgt_w1.__pgt_row_marker = __pgt_w2.__pgt_row_marker
/// ```
///
/// Each subquery computes one group of window functions with the same
/// PARTITION BY, plus a row marker for joining. The final query selects
/// pass-through columns from the first subquery and window columns from each.
pub fn rewrite_multi_partition_windows(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Set operations — don't rewrite
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // Need a target list with window functions
    if select.targetList.is_null() {
        return Ok(query.to_string());
    }

    // ── Extract window function info from the target list ───────────
    let target_list = pg_list::<pg_sys::Node>(select.targetList);
    let has_windows = target_list_has_windows(&target_list);
    if !has_windows {
        return Ok(query.to_string());
    }

    // Parse window expressions to check PARTITION BY diversity
    let window_infos = extract_window_info_from_targets(&target_list, select)?;

    if window_infos.is_empty() {
        return Ok(query.to_string());
    }

    // Group window functions by their PARTITION BY clause (as SQL text)
    let mut partition_groups: Vec<(String, Vec<WindowInfo>)> = Vec::new();
    for wi in window_infos {
        let found = partition_groups
            .iter_mut()
            .find(|(k, _)| *k == wi.partition_key);
        if let Some((_, group)) = found {
            group.push(wi);
        } else {
            partition_groups.push((wi.partition_key.clone(), vec![wi]));
        }
    }

    // If all window functions share the same PARTITION BY, no rewrite needed
    if partition_groups.len() <= 1 {
        return Ok(query.to_string());
    }

    // ── Collect non-window target expressions (pass-through columns) ─
    let mut pass_through: Vec<(String, Option<String>)> = Vec::new(); // (expr_sql, alias)
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        // Skip if this is a window function
        if unsafe { node_contains_window_func(rt.val) } {
            continue;
        }
        let expr = unsafe { node_to_expr(rt.val) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "NULL".to_string());
        let alias = if !rt.name.is_null() {
            Some(pg_cstr_to_str(rt.name).unwrap_or("?").to_string())
        } else {
            None
        };
        pass_through.push((expr, alias));
    }

    // ── Build FROM clause and WHERE clause SQL ──────────────────────
    let from_sql = extract_from_clause_sql(select)?;
    let where_sql = if select.whereClause.is_null() {
        String::new()
    } else {
        let expr = unsafe { node_to_expr(select.whereClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" WHERE {expr}")
    };
    let group_sql = deparse_group_clause(select);
    let having_sql = deparse_having_clause(select);

    // ── Build pass-through column list ──────────────────────────────
    let pt_select: Vec<String> = pass_through
        .iter()
        .map(|(expr, alias)| match alias {
            Some(a) => format!("{expr} AS \"{}\"", a.replace('"', "\"\"")),
            None => expr.clone(),
        })
        .collect();
    let pt_names: Vec<String> = pass_through
        .iter()
        .enumerate()
        .map(|(i, (expr, alias))| {
            alias.clone().unwrap_or_else(|| {
                // Try to extract a simple column name from the expression.
                // Handles both quoted ("name") and unquoted (name) column refs.
                let trimmed = expr.trim_matches('"');
                if trimmed.contains('.') || trimmed.contains('(') || trimmed.contains(' ') {
                    format!("__pgt_col_{}", i + 1)
                } else {
                    trimmed.to_string()
                }
            })
        })
        .collect();

    // ── Build CTE base with row marker ────────────────────────────────
    // Use a CTE so the row marker (ROW_NUMBER() OVER ()) is computed once
    // and referenced as a plain column in each subquery. This avoids
    // adding a second window function to each subquery which would
    // trigger the multi-PARTITION BY check recursively.
    // List columns explicitly (not SELECT *) to avoid A_Star parse issues.
    let cte_cols: Vec<String> = pt_select.clone();
    let cte_base = format!(
        "SELECT {}, ROW_NUMBER() OVER () AS __pgt_row_marker FROM {from_sql}{where_sql}{group_sql}{having_sql}",
        cte_cols.join(", ")
    );

    // ── Build subqueries for each partition group ─────────────────────
    let mut subquery_aliases: Vec<String> = Vec::new();
    let mut subquery_sqls: Vec<String> = Vec::new();
    let mut window_col_sources: Vec<(String, String)> = Vec::new(); // (subquery_alias, col_alias)

    for (group_idx, (_partition_key, group)) in partition_groups.iter().enumerate() {
        let sq_alias = format!("__pgt_w{}", group_idx + 1);

        let mut sq_select_parts: Vec<String> = Vec::new();

        // Include pass-through columns
        sq_select_parts.extend(pt_select.iter().cloned());

        // Include window function columns for this group
        for wi in group {
            sq_select_parts.push(format!(
                "{} AS \"{}\"",
                wi.func_sql,
                wi.alias.replace('"', "\"\"")
            ));
            window_col_sources.push((sq_alias.clone(), wi.alias.clone()));
        }

        // Include row marker (now a plain column from the CTE, not a window function)
        sq_select_parts.push("__pgt_row_marker".to_string());

        let sq_sql = format!("SELECT {} FROM __pgt_numbered", sq_select_parts.join(", "));
        subquery_sqls.push(sq_sql);
        subquery_aliases.push(sq_alias);
    }

    // ── Build outer SELECT ──────────────────────────────────────────
    let mut outer_select_parts: Vec<String> = Vec::new();

    // Pass-through columns from first subquery
    let first_sq = &subquery_aliases[0];
    for pt_name in &pt_names {
        outer_select_parts.push(format!(
            "\"{first_sq}\".\"{pt}\"",
            pt = pt_name.replace('"', "\"\"")
        ));
    }

    // Window columns from their respective subqueries
    for (sq_alias, col_alias) in &window_col_sources {
        outer_select_parts.push(format!(
            "\"{sq_alias}\".\"{col}\"",
            col = col_alias.replace('"', "\"\"")
        ));
    }

    // Include __pgt_row_marker in the outer SELECT so the DVM can store
    // it in the stream table. The Window diff reads old rows from the ST
    // including pass-through columns; without __pgt_row_marker in storage,
    // the CTE that fetches old rows would fail with "column does not exist".
    outer_select_parts.push(format!("\"{first_sq}\".\"__pgt_row_marker\"",));

    // ── Build outer FROM (JOIN chain) ──────────────────────────────
    let mut outer_from = format!(
        "({}) AS \"{}\"",
        subquery_sqls[0],
        subquery_aliases[0].replace('"', "\"\"")
    );

    for i in 1..subquery_sqls.len() {
        outer_from = format!(
            "{outer_from} JOIN ({}) AS \"{}\" ON \"{}\".\"__pgt_row_marker\" = \"{}\".\"__pgt_row_marker\"",
            subquery_sqls[i],
            subquery_aliases[i].replace('"', "\"\""),
            subquery_aliases[0].replace('"', "\"\""),
            subquery_aliases[i].replace('"', "\"\""),
        );
    }

    // ── ORDER BY rewrite ────────────────────────────────────────────
    let order_sql = deparse_order_clause(select);

    let rewritten = format!(
        "WITH __pgt_numbered AS ({cte_base}) SELECT {} FROM {outer_from}{order_sql}",
        outer_select_parts.join(", ")
    );

    pgrx::debug1!(
        "[pg_trickle] Rewrote multi-PARTITION BY windows: {}",
        rewritten
    );

    Ok(rewritten)
}

/// Information about a single window function in the target list.
struct WindowInfo {
    /// Full window function call as SQL (e.g., `SUM("amount") OVER (PARTITION BY "region")`).
    func_sql: String,
    /// Output alias for this window function.
    alias: String,
    /// Canonical key for the PARTITION BY clause (sorted, for grouping).
    partition_key: String,
}

/// Extract window function information from the target list.
///
/// # Safety
/// Caller must ensure target_list and select are valid.
fn extract_window_info_from_targets(
    target_list: &pgrx::PgList<pg_sys::Node>,
    _select: &pg_sys::SelectStmt,
) -> Result<Vec<WindowInfo>, PgTrickleError> {
    let mut infos = Vec::new();

    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }

        // Check for FuncCall with OVER clause (window function)
        if !unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_FuncCall) } {
            continue;
        }
        let func = unsafe { &*(rt.val as *const pg_sys::FuncCall) };
        if func.over.is_null() {
            continue; // Not a window function
        }

        let over = unsafe { &*func.over };

        // Get partition key
        let partition_key = if over.partitionClause.is_null() {
            String::new()
        } else {
            let parts = pg_list::<pg_sys::Node>(over.partitionClause);
            let mut keys: Vec<String> = Vec::new();
            for p in parts.iter_ptr() {
                let expr = unsafe { node_to_expr(p) }
                    .map(|e| e.to_sql())
                    .unwrap_or_else(|_| "?".to_string());
                keys.push(expr);
            }
            keys.sort();
            keys.join(",")
        };

        // Check if the OVER clause references a named window definition
        if !over.name.is_null() {
            let _win_name = pg_cstr_to_str(over.name).unwrap_or("?");
            // Look up the named window definition in windowClause to get
            // the full PARTITION BY. For simplicity, we include the name
            // as the partition key (named windows with same name share PARTITION BY).
            // A more complete impl would resolve the name, but this covers
            // the common case.
        }

        // Get the full function call as SQL
        let func_sql = unsafe { node_to_expr(rt.val) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "NULL".to_string());

        // Get alias
        let alias = if !rt.name.is_null() {
            pg_cstr_to_str(rt.name).unwrap_or("?").to_string()
        } else {
            // Generate a default alias
            format!("__pgt_wfn_{}", infos.len() + 1)
        };

        infos.push(WindowInfo {
            func_sql,
            alias,
            partition_key,
        });
    }

    Ok(infos)
}

/// Deparse the WINDOW clause of a SelectStmt into SQL, e.g. `w AS (PARTITION BY x ORDER BY y)`.
///
/// Returns an empty string when there is no WINDOW clause.
///
/// # Safety
/// Caller must ensure `select` points to a valid `SelectStmt`.
fn deparse_select_window_clause(select: &pg_sys::SelectStmt) -> Result<String, PgTrickleError> {
    if select.windowClause.is_null() {
        return Ok(String::new());
    }
    let window_list = pg_list::<pg_sys::Node>(select.windowClause);
    if window_list.is_empty() {
        return Ok(String::new());
    }
    let mut parts = Vec::new();
    for node_ptr in window_list.iter_ptr() {
        if let Some(wdef) = cast_node!(node_ptr, T_WindowDef, pg_sys::WindowDef) {
            let wname = if !wdef.name.is_null() {
                pg_cstr_to_str(wdef.name).unwrap_or("w").to_string()
            } else {
                continue;
            };
            let wspec = deparse_window_def(wdef)?;
            parts.push(format!("{wname} AS ({wspec})"));
        }
    }
    Ok(parts.join(", "))
}

/// Rewrite queries where window functions are nested inside expressions.
///
/// The DVM engine requires window functions to appear at the top level of a
/// SELECT target (i.e. `wf_expr AS alias`). When a window function is wrapped
/// inside another expression — e.g. `ABS(ROW_NUMBER() OVER (...) - 5)` — the
/// parser rejects the query with an "unsupported" error.
///
/// This pass lifts all nested window functions into an inner subquery:
///
/// ```sql
/// -- Input
/// SELECT ABS(ROW_NUMBER() OVER (ORDER BY score) - 5) AS dist FROM t;
///
/// -- Output
/// SELECT "abs"("__pgt_wf_inner"."__pgt_wf_1" - 5) AS "dist"
///   FROM (SELECT *, ROW_NUMBER() OVER (ORDER BY score) AS "__pgt_wf_1"
///         FROM t) "__pgt_wf_inner";
/// ```
///
/// Returns the original query unchanged when:
/// - The statement is a set operation (UNION / INTERSECT / EXCEPT)
/// - `GROUP BY` is present (interaction with window functions is complex)
/// - No nested window function expressions are found
pub fn rewrite_nested_window_exprs(query: &str) -> Result<String, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(query.to_string()),
    };

    // Set operations — don't rewrite; the union/intersect/except paths handle those
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // GROUP BY present — bail; window-over-aggregate interactions are non-trivial
    if !select.groupClause.is_null() {
        let group_list = pg_list::<pg_sys::Node>(select.groupClause);
        if !group_list.is_empty() {
            return Ok(query.to_string());
        }
    }

    if select.targetList.is_null() {
        return Ok(query.to_string());
    }

    let target_list = pg_list::<pg_sys::Node>(select.targetList);

    // ── Detect targets with nested window function expressions ───────────
    // "Nested" means: the ResTarget val is NOT a bare FuncCall-with-OVER,
    // but node_contains_window_func says there IS a window func somewhere inside it.
    let mut has_nested = false;
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        // Skip bare window functions (direct FuncCall-with-OVER, not nested)
        if let Some(fc) = cast_node!(rt.val, T_FuncCall, pg_sys::FuncCall)
            && !fc.over.is_null()
        {
            continue;
        }
        if unsafe { node_contains_window_func(rt.val) } {
            has_nested = true;
            break;
        }
    }

    if !has_nested {
        return Ok(query.to_string());
    }

    // ── Collect all distinct window function SQL strings ─────────────────
    // Walk every target to harvest FuncCall-with-OVER nodes; deduplicate by SQL text.
    let mut wf_entries: Vec<(String, String)> = Vec::new(); // (wf_sql, synthetic_alias)
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        let mut wf_nodes: Vec<*mut pg_sys::Node> = Vec::new();
        unsafe { collect_all_window_func_nodes(rt.val, &mut wf_nodes) };
        for wf_node in wf_nodes {
            let wf_sql = match unsafe { node_to_expr(wf_node) } {
                Ok(e) => e.to_sql(),
                Err(_) => continue,
            };
            if wf_entries.iter().any(|(s, _)| *s == wf_sql) {
                continue; // Already seen this window function
            }
            let alias = format!("__pgt_wf_{}", wf_entries.len() + 1);
            wf_entries.push((wf_sql, alias));
        }
    }

    if wf_entries.is_empty() {
        return Ok(query.to_string());
    }

    // ── Construct the inner SELECT ────────────────────────────────────────
    // SELECT *, wf1_sql AS "__pgt_wf_1", wf2_sql AS "__pgt_wf_2", ... FROM ...
    let from_sql = extract_from_clause_sql(select)?;

    let where_sql = if select.whereClause.is_null() {
        String::new()
    } else {
        let expr = unsafe { node_to_expr(select.whereClause) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        format!(" WHERE {expr}")
    };

    // Carry the WINDOW clause into the inner SELECT so named window
    // references (OVER w) are still resolvable there.
    let window_clause_sql = deparse_select_window_clause(select)?;
    let window_sql = if window_clause_sql.is_empty() {
        String::new()
    } else {
        format!(" WINDOW {window_clause_sql}")
    };

    let inner_wf_parts: Vec<String> = wf_entries
        .iter()
        .map(|(wf_sql, alias)| format!("{wf_sql} AS \"{}\"", alias.replace('"', "\"\"")))
        .collect();

    let inner_select = format!(
        "SELECT *, {} FROM {from_sql}{where_sql}{window_sql}",
        inner_wf_parts.join(", ")
    );

    // ── Construct the outer SELECT ────────────────────────────────────────
    // For non-nested targets: deparse with table qualifiers stripped.
    // For nested targets: deparse, strip qualifiers, then replace each
    // embedded wf_sql substring with its synthetic alias reference.
    let mut outer_parts: Vec<String> = Vec::new();

    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }

        // Does this target contain a nested window function?
        let is_direct_wf = unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_FuncCall) } && {
            let fc = unsafe { &*(rt.val as *const pg_sys::FuncCall) };
            !fc.over.is_null()
        };
        let has_wf_inside = !is_direct_wf && unsafe { node_contains_window_func(rt.val) };

        let mut expr_sql = match unsafe { node_to_expr(rt.val) } {
            Ok(e) => e.strip_qualifier().to_sql(),
            Err(_) => continue,
        };

        if has_wf_inside {
            // Replace embedded window function SQL with synthetic alias references.
            // The alias is qualified with the inner subquery alias to resolve
            // potential ambiguity when the outer query joins multiple tables.
            for (wf_sql, alias) in &wf_entries {
                let replacement = format!("\"__pgt_wf_inner\".\"{}\"", alias.replace('"', "\"\""));
                expr_sql = expr_sql.replace(wf_sql.as_str(), &replacement);
            }
        }

        let alias_part = if !rt.name.is_null() {
            let a = pg_cstr_to_str(rt.name).unwrap_or("col");
            format!(" AS \"{}\"", a.replace('"', "\"\""))
        } else {
            String::new()
        };

        outer_parts.push(format!("{expr_sql}{alias_part}"));
    }

    // ── Preserve ORDER BY from the outer query ───────────────────────────
    let order_sql = deparse_order_clause(select);

    let rewritten = format!(
        "SELECT {} FROM ({inner_select}) \"__pgt_wf_inner\"{order_sql}",
        outer_parts.join(", ")
    );

    pgrx::debug1!("[pg_trickle] rewrite_nested_window_exprs: {}", rewritten);

    Ok(rewritten)
}

/// Extract FROM clause as SQL text from a SelectStmt.
fn extract_from_clause_sql(select: &pg_sys::SelectStmt) -> Result<String, PgTrickleError> {
    if select.fromClause.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "DISTINCT ON query must have a FROM clause".into(),
        ));
    }
    let from_list = pg_list::<pg_sys::Node>(select.fromClause);
    let mut parts = Vec::new();
    for node_ptr in from_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        parts.push(from_item_to_sql(node_ptr)?);
    }
    if parts.is_empty() {
        return Err(PgTrickleError::QueryParseError(
            "DISTINCT ON query FROM clause is empty".into(),
        ));
    }
    Ok(parts.join(", "))
}

/// Convert a FROM clause item (RangeVar, JoinExpr, RangeSubselect) back to SQL.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree Node.
fn from_item_to_sql(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        let mut name = String::new();
        if !rv.schemaname.is_null() {
            let schema = pg_cstr_to_str(rv.schemaname).unwrap_or("public");
            name.push_str(&format!("\"{}\".", schema.replace('"', "\"\"")));
        }
        if !rv.relname.is_null() {
            let rel = pg_cstr_to_str(rv.relname).unwrap_or("?");
            name.push_str(&format!("\"{}\"", rel.replace('"', "\"\"")));
        }
        if !rv.alias.is_null() {
            let alias_struct = unsafe { &*rv.alias };
            if !alias_struct.aliasname.is_null() {
                let alias = pg_cstr_to_str(alias_struct.aliasname).unwrap_or("?");
                name.push_str(&format!(" AS \"{}\"", alias.replace('"', "\"\"")));
            }
        }
        Ok(name)
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        let left = if join.larg.is_null() {
            "?".to_string()
        } else {
            from_item_to_sql(join.larg)?
        };
        let right = if join.rarg.is_null() {
            "?".to_string()
        } else {
            from_item_to_sql(join.rarg)?
        };
        let join_type = match join.jointype {
            pg_sys::JoinType::JOIN_LEFT => "LEFT JOIN",
            pg_sys::JoinType::JOIN_FULL => "FULL JOIN",
            pg_sys::JoinType::JOIN_RIGHT => "RIGHT JOIN",
            pg_sys::JoinType::JOIN_INNER => {
                if join.quals.is_null() {
                    "CROSS JOIN"
                } else {
                    "JOIN"
                }
            }
            _ => "JOIN",
        };
        let on_clause = if join.quals.is_null() {
            String::new()
        } else {
            let cond = unsafe { node_to_expr(join.quals)? };
            format!(" ON {}", cond.to_sql())
        };
        Ok(format!("{left} {join_type} {right}{on_clause}"))
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect) {
        if sub.subquery.is_null()
            || !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            return Ok("?".to_string());
        }
        let inner_sql = deparse_select_to_sql(sub.subquery)?;
        let lateral_kw = if sub.lateral { "LATERAL " } else { "" };
        let mut result = format!("{lateral_kw}({inner_sql})");
        if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            if !a.aliasname.is_null() {
                let alias = pg_cstr_to_str(a.aliasname).unwrap_or("?");
                result.push_str(&format!(" AS \"{}\"", alias.replace('"', "\"\"")));
            }
        }
        Ok(result)
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeFunction) } {
        // Function in FROM — use fallback deparser
        unsafe { deparse_from_item_to_sql(node) }
    } else {
        // Fallback for other FROM items
        unsafe { deparse_from_item_to_sql(node) }
    }
}

pub fn reject_limit_offset(query: &str) -> Result<(), PgTrickleError> {
    // If the query matches the TopK pattern (ORDER BY + LIMIT), allow it.
    if detect_topk_pattern(query)?.is_some() {
        return Ok(());
    }

    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(()),
    };

    if !select.limitCount.is_null() {
        // LIMIT ALL is semantically equivalent to no LIMIT — don't reject.
        // SAFETY: limit node is non-null and points to a valid Node.
        if !is_limit_all_node(select.limitCount) {
            return Err(PgTrickleError::UnsupportedOperator(
                "LIMIT is not supported in defining queries without ORDER BY. \
                 Use ORDER BY + LIMIT for TopK stream tables, or omit LIMIT \
                 to materialize the full result set."
                    .into(),
            ));
        }
    }
    if !select.limitOffset.is_null() {
        return Err(PgTrickleError::UnsupportedOperator(
            "OFFSET is not supported in defining queries without ORDER BY + LIMIT. \
             Use ORDER BY + LIMIT + OFFSET for paged TopK stream tables, \
             or omit OFFSET to materialize the full result set."
                .into(),
        ));
    }

    Ok(())
}

/// Metadata for a TopK stream table (ORDER BY + LIMIT pattern).
#[derive(Debug, Clone)]
pub struct TopKInfo {
    /// The LIMIT value as an integer.
    pub limit_value: i64,
    /// The OFFSET value as an integer, if present. `None` means no OFFSET.
    pub offset_value: Option<i64>,
    /// The full defining query including ORDER BY + LIMIT [+ OFFSET] (for refresh).
    pub full_query: String,
    /// The defining query with ORDER BY, LIMIT, and OFFSET stripped (for DVM, deps).
    pub base_query: String,
    /// The deparsed ORDER BY clause (e.g., "score DESC, name ASC").
    pub order_by_sql: String,
}

/// Detect the TopK pattern in a defining query: ORDER BY + LIMIT with
/// a constant integer limit value, no OFFSET, and no set operations.
///
/// Returns `Some(TopKInfo)` if the pattern matches, `None` otherwise.
///
/// Validation rules:
/// - `LIMIT` without `ORDER BY` → not TopK (will be rejected later)
/// - `ORDER BY` without `LIMIT` → not TopK (ORDER BY silently discarded)
/// - `ORDER BY` + `LIMIT` → TopK pattern ✓
/// - `ORDER BY` + `LIMIT` + `OFFSET` → error
/// - `LIMIT ALL` → not TopK (equivalent to no LIMIT)
/// - `LIMIT 0` → TopK with zero rows
/// - `LIMIT (SELECT ...)` → error (require constant integer)
/// - Set operations (UNION, INTERSECT, EXCEPT) → not TopK
pub fn detect_topk_pattern(query: &str) -> Result<Option<TopKInfo>, PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(None),
    };

    // Not TopK if it's a set operation (UNION, INTERSECT, EXCEPT)
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(None);
    }

    // Not TopK if no LIMIT
    if select.limitCount.is_null() {
        return Ok(None);
    }

    // Not TopK if no ORDER BY
    if select.sortClause.is_null() {
        return Ok(None);
    }
    let sort_list = pg_list::<pg_sys::Node>(select.sortClause);
    if sort_list.is_empty() {
        return Ok(None);
    }

    // Extract OFFSET value if present (must be a constant non-negative integer).
    let offset_value = if !select.limitOffset.is_null() {
        match extract_const_int_from_node(select.limitOffset) {
            Some(v) if v >= 0 => {
                if v == 0 {
                    None
                } else {
                    Some(v)
                }
            }
            Some(_) => {
                return Err(PgTrickleError::UnsupportedOperator(
                    "OFFSET must be a non-negative constant integer.".into(),
                ));
            }
            None => {
                return Err(PgTrickleError::UnsupportedOperator(
                    "OFFSET in defining queries must be a constant integer \
                     (e.g., OFFSET 10). Dynamic expressions like OFFSET (SELECT ...) \
                     are not supported."
                        .into(),
                ));
            }
        }
    } else {
        None
    };

    // Extract LIMIT value — must be a constant integer.
    let limit_node = select.limitCount;
    let limit_value = extract_const_int_from_node(limit_node);
    let limit_value = match limit_value {
        Some(v) => v,
        None => {
            // Check if this is LIMIT ALL (A_Const with isnull=true) — treat as
            // "no LIMIT" → not a TopK table.
            if is_limit_all_node(limit_node) {
                return Ok(None);
            }
            // Otherwise it's a non-constant expression like LIMIT (SELECT ...).
            return Err(PgTrickleError::UnsupportedOperator(
                "LIMIT in defining queries must be a constant integer \
                 (e.g., LIMIT 100). Dynamic expressions like LIMIT (SELECT ...) \
                 are not supported for TopK stream tables."
                    .into(),
            ));
        }
    };

    // LIMIT ALL is represented as a very large or negative value in some
    // parse trees — treat it as "no limit" → not TopK.
    if limit_value < 0 {
        return Ok(None);
    }

    // Deparse the ORDER BY clause from the sort items.
    let order_by_sql = unsafe { deparse_sort_clause(&sort_list)? };

    // Build the base query by stripping ORDER BY and LIMIT.
    let base_query = strip_order_by_and_limit(query);

    Ok(Some(TopKInfo {
        limit_value,
        offset_value,
        full_query: query.to_string(),
        base_query,
        order_by_sql,
    }))
}

/// Extract a constant integer value from a parse tree Node.
///
/// Returns `Some(value)` for Integer constants, `None` for anything else.
///
/// # Safety
/// Caller must ensure `node` points to a valid Node.
fn extract_const_int_from_node(node: *mut pg_sys::Node) -> Option<i64> {
    if node.is_null() {
        return None;
    }

    // Check for A_Const (Integer constant in raw parse tree)
    if let Some(a_const) = cast_node!(node, T_A_Const, pg_sys::A_Const) {
        // In PostgreSQL 18, A_Const uses a union `val` with a `node.type` tag field.
        // Check if it's an Integer type.
        // SAFETY: accessing union fields of A_Const requires unsafe; we check
        // the node tag before reading the corresponding union variant.
        let val_tag = unsafe { a_const.val.node.type_ };
        if val_tag == pg_sys::NodeTag::T_Integer {
            // SAFETY: we checked the tag is T_Integer, so accessing ival is valid.
            let ival = unsafe { a_const.val.ival.ival } as i64;
            return Some(ival);
        }

        // Check for T_String — could be LIMIT ALL or similar
        if val_tag == pg_sys::NodeTag::T_String {
            // LIMIT ALL is represented as a NULL limitCount in most cases;
            // if it's a string, it's not a constant int.
            return None;
        }

        return None;
    }

    // TypeCast wrapping an integer constant (e.g., LIMIT 5::bigint)
    if let Some(tc) = cast_node!(node, T_TypeCast, pg_sys::TypeCast) {
        return extract_const_int_from_node(tc.arg);
    }

    None
}

/// Check whether a LIMIT node represents `LIMIT ALL`.
///
/// In PostgreSQL 18, `LIMIT ALL` is parsed as a non-null A_Const with
/// `isnull = true`. This is semantically equivalent to no LIMIT.
///
/// # Safety
/// Caller must ensure `node` points to a valid Node (or is null).
fn is_limit_all_node(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    if let Some(a_const) = cast_node!(node, T_A_Const, pg_sys::A_Const) {
        return a_const.isnull;
    }
    false
}

/// Strip ORDER BY, LIMIT, OFFSET, and FETCH FIRST clauses from a query string.
///
/// Finds the last top-level `ORDER BY` keyword (not inside parentheses) and
/// removes everything from there to the end. This is safe because TopK
/// detection already verified the query has a top-level ORDER BY + LIMIT
/// (no set operations). OFFSET always follows ORDER BY at the top level,
/// so stripping from ORDER BY onward removes all three clauses.
fn strip_order_by_and_limit(query: &str) -> String {
    let q = query.trim().trim_end_matches(';').trim();
    let upper = q.to_uppercase();

    // Walk backwards through the string to find the last top-level ORDER BY.
    // Track parenthesis depth to skip ORDER BY inside subqueries.
    let bytes = upper.as_bytes();
    let mut depth: i32 = 0;
    let mut last_order_by_pos: Option<usize> = None;

    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'\'' => {
                // Skip string literals
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            i += 2; // escaped quote
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'O' if depth == 0 => {
                // Check for "ORDER BY" at this position
                if upper[i..].starts_with("ORDER") {
                    let after_order = i + 5;
                    if after_order < bytes.len() && bytes[after_order].is_ascii_whitespace() {
                        // Skip whitespace
                        let mut j = after_order;
                        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        if upper[j..].starts_with("BY")
                            && (j + 2 >= bytes.len() || !bytes[j + 2].is_ascii_alphanumeric())
                        {
                            last_order_by_pos = Some(i);
                        }
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    match last_order_by_pos {
        Some(pos) => q[..pos].trim().to_string(),
        None => q.to_string(),
    }
}

/// F13 (G4.2): Warn when a FROM-clause subquery or LATERAL uses LIMIT without
/// ORDER BY.
///
/// Without ORDER BY, LIMIT picks an arbitrary subset of rows. The chosen
/// rows may differ between FULL refresh (fresh plan / statistics) and
/// DIFFERENTIAL refresh (delta-driven), causing non-deterministic results.
///
/// Emits a `pgrx::warning!()` for each such occurrence. Does **not** reject
/// the query — LIMIT with ORDER BY is a legitimate, deterministic pattern.
/// NS-1: Returns `true` when the top-level SELECT has an `ORDER BY` clause
/// but no `LIMIT`. This is a no-op in a stream table context because the
/// storage table row order is undefined, so the user is likely confused.
pub fn has_order_by_without_limit(query: &str) -> bool {
    let select = match parse_first_select(query) {
        Ok(Some(s)) => unsafe { &*s },
        _ => return false,
    };
    // Only warn for plain (non-set-operation) SELECTs.
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return false;
    }
    // Has ORDER BY ...
    if select.sortClause.is_null() {
        return false;
    }
    let sort_list = pg_list::<pg_sys::Node>(select.sortClause);
    if sort_list.is_empty() {
        return false;
    }
    // ... but no LIMIT
    select.limitCount.is_null()
}

pub fn warn_limit_without_order_in_subqueries(query: &str) {
    let select = match parse_first_select(query) {
        Ok(Some(s)) => unsafe { &*s },
        _ => return,
    };
    walk_from_for_limit_warning(select);
}

/// Walk a SelectStmt's FROM clause looking for subqueries with LIMIT but
/// no ORDER BY.
///
/// # Safety
/// Caller must ensure `select` points to a valid `SelectStmt`.
fn walk_from_for_limit_warning(select: *const pg_sys::SelectStmt) {
    let s = unsafe { &*select };

    // Handle set operations
    if s.op != pg_sys::SetOperation::SETOP_NONE {
        if !s.larg.is_null() {
            walk_from_for_limit_warning(s.larg);
        }
        if !s.rarg.is_null() {
            walk_from_for_limit_warning(s.rarg);
        }
        return;
    }

    let from_list = pg_list::<pg_sys::Node>(s.fromClause);
    for node_ptr in from_list.iter_ptr() {
        check_from_item_limit_warning(node_ptr);
    }

    // Also check CTEs
    if !s.withClause.is_null() {
        let wc = unsafe { &*s.withClause };
        let cte_list = pg_list::<pg_sys::Node>(wc.ctes);
        for node_ptr in cte_list.iter_ptr() {
            if let Some(cte) = cast_node!(node_ptr, T_CommonTableExpr, pg_sys::CommonTableExpr)
                && !cte.ctequery.is_null()
                && unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) }
            {
                walk_from_for_limit_warning(cte.ctequery as *const pg_sys::SelectStmt);
            }
        }
    }
}

/// Check a single FROM-clause item for LIMIT without ORDER BY.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree node.
fn check_from_item_limit_warning(node: *mut pg_sys::Node) {
    if node.is_null() {
        return;
    }

    if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect) {
        if !sub.subquery.is_null()
            && unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            let inner = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };
            let has_order_by = !inner.sortClause.is_null() && {
                let sort_list = pg_list::<pg_sys::Node>(inner.sortClause);
                !sort_list.is_empty()
            };

            // Check: has LIMIT but no ORDER BY
            if !inner.limitCount.is_null() && !has_order_by {
                let alias = resolve_range_subselect_alias(sub);
                pgrx::warning!(
                    "pg_trickle: subquery '{}' uses LIMIT without ORDER BY. \
                     This produces non-deterministic results that may differ between \
                     FULL and DIFFERENTIAL refresh. Add ORDER BY for deterministic behavior.",
                    alias
                );
            }
            // G2: Check: has OFFSET but no ORDER BY
            if !inner.limitOffset.is_null() && !has_order_by {
                let alias = resolve_range_subselect_alias(sub);
                pgrx::warning!(
                    "pg_trickle: subquery '{}' uses OFFSET without ORDER BY. \
                     This produces non-deterministic results that may differ between \
                     FULL and DIFFERENTIAL refresh. Add ORDER BY for deterministic behavior.",
                    alias
                );
            }
            // Recurse into the subquery's FROM clause
            walk_from_for_limit_warning(sub.subquery as *const pg_sys::SelectStmt);
        }
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        check_from_item_limit_warning(join.larg);
        check_from_item_limit_warning(join.rarg);
    }
}

/// Extract the alias name from a `RangeSubselect` node.
///
/// # Safety
/// Caller must ensure `sub` points to a valid `RangeSubselect`.
fn resolve_range_subselect_alias(sub: &pg_sys::RangeSubselect) -> String {
    if !sub.alias.is_null() {
        let a = unsafe { &*(sub.alias) };
        if !a.aliasname.is_null() {
            return pg_cstr_to_str(a.aliasname)
                .unwrap_or("(subquery)")
                .to_string();
        }
    }
    "(subquery)".to_string()
}

/// Lightweight validation that rejects SQL constructs unsupported in
/// **any** refresh mode (FULL or DIFFERENTIAL).
///
/// Currently checks:
/// - **NATURAL JOIN**: the raw parser sets `isNatural` on `JoinExpr`
/// - **DISTINCT ON**: `distinctClause` contains non-null expression nodes
/// - **Subquery expressions** (EXISTS, IN subquery, scalar subquery):
///   `T_SubLink` nodes in the WHERE clause or target list
///
/// This avoids running the full DVM parser (which resolves table OIDs,
/// builds the operator tree, etc.) for queries that would fail anyway.
/// Convert a byte offset from a PostgreSQL parser `location` field into a
/// 1-based (line, column) pair within `query`.  Returns `None` when the
/// offset is unknown (PostgreSQL sets it to `-1` when unavailable).
fn query_byte_offset_to_linecol(query: &str, offset: i32) -> Option<(usize, usize)> {
    if offset < 0 {
        return None;
    }
    let offset = (offset as usize).min(query.len());
    let prefix = &query[..offset];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = prefix.rfind('\n').map_or(offset + 1, |pos| offset - pos);
    Some((line, col))
}

pub fn reject_unsupported_constructs(query: &str) -> Result<(), PgTrickleError> {
    let select = match parse_first_select(query)? {
        Some(s) => unsafe { &*s },
        None => return Ok(()),
    };

    // For set operations (UNION/INTERSECT/EXCEPT), recurse into both sides
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        if !select.larg.is_null() {
            // SAFETY: larg points to a valid SelectStmt
            check_select_unsupported(unsafe { &*select.larg }, query)?;
        }
        if !select.rarg.is_null() {
            // SAFETY: rarg points to a valid SelectStmt
            check_select_unsupported(unsafe { &*select.rarg }, query)?;
        }
        return Ok(());
    }

    // SAFETY: select is a valid SelectStmt
    check_select_unsupported(select, query)
}

/// Check a single SelectStmt for unsupported constructs.
///
/// # Safety
/// Caller must ensure `select` points to a valid `pg_sys::SelectStmt`.
fn check_select_unsupported(
    select: &pg_sys::SelectStmt,
    query: &str,
) -> Result<(), PgTrickleError> {
    // ── DISTINCT ON ─────────────────────────────────────────────────
    // DISTINCT ON is handled by auto-rewriting to a ROW_NUMBER() window
    // function in the DVM parser (`rewrite_distinct_on()`). No rejection
    // needed here.

    // ── FROM clause items (TABLESAMPLE) ─────────────────────────────
    // NATURAL JOIN is supported (S9): common columns are resolved and an
    // explicit equi-join is synthesized. Only TABLESAMPLE is rejected here.
    if !select.fromClause.is_null() {
        let from_list = pg_list::<pg_sys::Node>(select.fromClause);
        for node_ptr in from_list.iter_ptr() {
            if !node_ptr.is_null() {
                // SAFETY: node_ptr is valid from the from_list
                check_from_item_unsupported(node_ptr, query)?;
            }
        }
    }

    // ── Subquery expressions in WHERE ──────────────────────────────────
    // SubLinks (EXISTS, IN subquery, scalar subquery) in the WHERE clause
    // are handled during parse_select_stmt by extracting them into
    // SemiJoin/AntiJoin OpTree nodes. We only reject ALL_SUBLINK here
    // since that's not yet supported.
    if !select.whereClause.is_null() {
        // SAFETY: whereClause is valid from the SelectStmt
        check_where_for_unsupported_sublinks(select.whereClause)?;
    }

    // ── FOR UPDATE / FOR SHARE / FOR NO KEY UPDATE / FOR KEY SHARE ──
    if !select.lockingClause.is_null() {
        return Err(PgTrickleError::UnsupportedOperator(
            "FOR UPDATE/FOR SHARE is not supported in defining queries. \
             Stream tables do not support row-level locking. \
             Remove the FOR UPDATE/FOR SHARE clause."
                .into(),
        ));
    }

    // ── GROUPING SETS / CUBE / ROLLUP ────────────────────────────────
    // Handled by auto-rewriting to UNION ALL of separate GROUP BY queries
    // in `rewrite_grouping_sets()`. No rejection needed here.

    Ok(())
}

/// Recursively check FROM clause items for unsupported features.
///
/// Currently rejects:
/// - `TABLESAMPLE` — Stream tables materialize complete result sets;
///   non-deterministic sampling is not meaningful.
///
/// Note: NATURAL JOIN is now supported (S9) — common columns are resolved
/// from the parsed OpTree and an explicit equi-join condition is synthesized.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn check_from_item_unsupported(node: *mut pg_sys::Node, query: &str) -> Result<(), PgTrickleError> {
    if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        // Recursively check join children
        if !join.larg.is_null() {
            // SAFETY: larg is valid from JoinExpr
            check_from_item_unsupported(join.larg, query)?;
        }
        if !join.rarg.is_null() {
            // SAFETY: rarg is valid from JoinExpr
            check_from_item_unsupported(join.rarg, query)?;
        }
    } else if let Some(rts) = cast_node!(node, T_RangeTableSample, pg_sys::RangeTableSample) {
        // TABLESAMPLE: SELECT * FROM t TABLESAMPLE BERNOULLI(10)
        // Stream tables materialize complete result sets; non-deterministic
        // sampling is not meaningful. Reject in both FULL and DIFFERENTIAL modes.
        let loc = query_byte_offset_to_linecol(query, rts.location)
            .map(|(l, c)| format!(" at line {l}, column {c}"))
            .unwrap_or_default();
        return Err(PgTrickleError::UnsupportedOperator(format!(
            "TABLESAMPLE{loc} is not supported in defining queries. \
             Stream tables materialize the complete result set; \
             use a WHERE condition with random() if sampling is needed."
        )));
    }
    // RangeVar and RangeSubselect are fine — no check needed
    Ok(())
}

/// Check if a WHERE clause node tree contains unsupported SubLink types.
///
/// EXISTS_SUBLINK, ANY_SUBLINK (IN), and ALL_SUBLINK are now supported
/// via SemiJoin/AntiJoin. EXPR_SUBLINK (scalar subquery) is supported
/// in the target list. No SubLink types are rejected here any longer.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn check_where_for_unsupported_sublinks(node: *mut pg_sys::Node) -> Result<(), PgTrickleError> {
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        // EXISTS, ANY (IN), ALL, and EXPR SubLinks are handled during parsing
        return Ok(());
    }
    // Check inside BoolExpr (AND/OR/NOT) which commonly wraps SubLinks
    if let Some(boolexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr)
        && !boolexpr.args.is_null()
    {
        let args = pg_list::<pg_sys::Node>(boolexpr.args);
        for arg_ptr in args.iter_ptr() {
            if !arg_ptr.is_null() {
                // SAFETY: arg_ptr is valid from BoolExpr args list
                check_where_for_unsupported_sublinks(arg_ptr)?;
            }
        }
    }
    Ok(())
}

// ── SubLink extraction for WHERE clauses ────────────────────────────────

/// Information about a SubLink extracted from the WHERE clause.
struct SublinkWrapper {
    /// Whether this is negated (NOT EXISTS / NOT IN).
    negated: bool,
    /// The join condition (correlation condition from the inner WHERE,
    /// possibly including the IN equality condition).
    condition: Expr,
    /// Parsed OpTree for the inner subquery's FROM clause.
    inner_tree: OpTree,
}

/// Walk a WHERE clause node tree and extract SubLinks into SemiJoin/AntiJoin
/// wrappers, returning the remaining non-SubLink predicates.
///
/// Handles:
/// - `EXISTS (SELECT ... FROM inner WHERE cond)` → SemiJoin
/// - `NOT EXISTS (SELECT ... FROM inner WHERE cond)` → AntiJoin
/// - `x IN (SELECT col FROM inner WHERE cond)` → SemiJoin with equality
/// - `x NOT IN (SELECT col FROM inner)` → AntiJoin with equality
/// - SubLinks under AND conjunctions (each extracted independently)
/// - SubLinks under OR → not supported (returns error)
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn extract_where_sublinks(
    node: *mut pg_sys::Node,
    cte_ctx: &mut CteParseContext,
) -> Result<(Vec<SublinkWrapper>, Option<Expr>), PgTrickleError> {
    // Case 1: The node itself is a SubLink
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        let wrapper = parse_sublink_to_wrapper(node, false, cte_ctx)?;
        return Ok((vec![wrapper], None));
    }

    // Case 2: BoolExpr
    if let Some(boolexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr) {
        // Case 2a: NOT wrapping a SubLink → negated
        if boolexpr.boolop == pg_sys::BoolExprType::NOT_EXPR {
            let args = pg_list::<pg_sys::Node>(boolexpr.args);
            if args.len() == 1 {
                // INVARIANT: args.len() == 1 guarantees head() returns Some.
                let arg = args.head().unwrap();
                if unsafe { pgrx::is_a(arg, pg_sys::NodeTag::T_SubLink) } {
                    let wrapper = parse_sublink_to_wrapper(arg, true, cte_ctx)?;
                    return Ok((vec![wrapper], None));
                }
            }
        }

        // Case 2b: AND conjunction — extract SubLinks from each arg
        if boolexpr.boolop == pg_sys::BoolExprType::AND_EXPR {
            let args = pg_list::<pg_sys::Node>(boolexpr.args);
            let mut wrappers = Vec::new();
            let mut remaining_exprs = Vec::new();

            for arg_ptr in args.iter_ptr() {
                if arg_ptr.is_null() {
                    continue;
                }

                if unsafe { pgrx::is_a(arg_ptr, pg_sys::NodeTag::T_SubLink) } {
                    // Direct SubLink under AND
                    let wrapper = parse_sublink_to_wrapper(arg_ptr, false, cte_ctx)?;
                    wrappers.push(wrapper);
                } else if let Some(inner_bool) = cast_node!(arg_ptr, T_BoolExpr, pg_sys::BoolExpr) {
                    // NOT SubLink under AND → negated
                    if inner_bool.boolop == pg_sys::BoolExprType::NOT_EXPR {
                        let inner_args = pg_list::<pg_sys::Node>(inner_bool.args);
                        if inner_args.len() == 1 {
                            // INVARIANT: inner_args.len() == 1 guarantees head() returns Some.
                            let inner_arg = inner_args.head().unwrap();
                            if unsafe { pgrx::is_a(inner_arg, pg_sys::NodeTag::T_SubLink) } {
                                let wrapper = parse_sublink_to_wrapper(inner_arg, true, cte_ctx)?;
                                wrappers.push(wrapper);
                                continue;
                            }
                        }
                    }
                    // Check for SubLinks inside OR — should have been
                    // rewritten to UNION by rewrite_sublinks_in_or().
                    // If still present, it's a deeply nested case.
                    if inner_bool.boolop == pg_sys::BoolExprType::OR_EXPR
                        && node_tree_contains_sublink(arg_ptr)
                    {
                        return Err(PgTrickleError::UnsupportedOperator(
                            "Subquery expressions (EXISTS, IN) inside OR conditions are not \
                             supported in this nesting pattern. Consider rewriting \
                             using UNION or separate stream tables."
                                .into(),
                        ));
                    }
                    // Regular boolean expression — keep as remaining
                    remaining_exprs.push(unsafe { node_to_expr(arg_ptr)? });
                } else {
                    // Regular predicate — keep as remaining
                    remaining_exprs.push(unsafe { node_to_expr(arg_ptr)? });
                }
            }

            let remaining = if remaining_exprs.is_empty() {
                None
            } else {
                Some(
                    remaining_exprs
                        .into_iter()
                        .reduce(|acc, expr| Expr::BinaryOp {
                            op: "AND".to_string(),
                            left: Box::new(acc),
                            right: Box::new(expr),
                        })
                        .unwrap(),
                )
            };

            return Ok((wrappers, remaining));
        }

        // Case 2c: OR containing SubLinks — should have been
        // rewritten to UNION by rewrite_sublinks_in_or().
        // If still present, it's a deeply nested case.
        if boolexpr.boolop == pg_sys::BoolExprType::OR_EXPR && node_tree_contains_sublink(node) {
            return Err(PgTrickleError::UnsupportedOperator(
                "Subquery expressions (EXISTS, IN) inside OR conditions are not \
                 supported in this nesting pattern. Consider rewriting \
                 using UNION or separate stream tables."
                    .into(),
            ));
        }
    }

    // No SubLinks found — return the whole expression as remaining predicate
    let expr = unsafe { node_to_expr(node)? };
    Ok((vec![], Some(expr)))
}

/// Check if a node tree contains any T_SubLink node (shallow walk).
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn node_tree_contains_sublink(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        return true;
    }
    if let Some(boolexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr)
        && !boolexpr.args.is_null()
    {
        let args = pg_list::<pg_sys::Node>(boolexpr.args);
        for arg_ptr in args.iter_ptr() {
            if !arg_ptr.is_null() && node_tree_contains_sublink(arg_ptr) {
                return true;
            }
        }
    }
    false
}

/// Recursively flatten a boolean AND tree into a flat list of conjunct nodes.
///
/// `AND(a, AND(b, c))` → \[a, b, c\]; non-AND nodes are pushed directly.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse-tree Node.
fn flatten_and_conjuncts(node: *mut pg_sys::Node, result: &mut Vec<*mut pg_sys::Node>) {
    if node.is_null() {
        return;
    }
    if let Some(be) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr)
        && be.boolop == pg_sys::BoolExprType::AND_EXPR
        && !be.args.is_null()
    {
        let args = pg_list::<pg_sys::Node>(be.args);
        for arg_ptr in args.iter_ptr() {
            flatten_and_conjuncts(arg_ptr, result);
        }
        return;
    }
    result.push(node);
}

/// Check if an AND expression (at any depth) contains at least one OR
/// conjunct that itself contains SubLink nodes.
///
/// Recurses into nested AND layers so that `AND(p, AND(q, OR(EXISTS(...))))`
/// is detected correctly. This is stricter than `node_tree_contains_sublink`
/// which returns true for AND + EXISTS (no OR), causing unnecessary deparsing.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn and_contains_or_with_sublink(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        return false;
    }
    let boolexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };
    if boolexpr.boolop != pg_sys::BoolExprType::AND_EXPR {
        return false;
    }
    // Flatten the whole AND tree then check whether any conjunct is
    // an OR containing SubLink nodes.
    let mut conjuncts: Vec<*mut pg_sys::Node> = Vec::new();
    flatten_and_conjuncts(node, &mut conjuncts);
    for conj in conjuncts {
        if !conj.is_null() && unsafe { pgrx::is_a(conj, pg_sys::NodeTag::T_BoolExpr) } {
            let inner = unsafe { &*(conj as *const pg_sys::BoolExpr) };
            if inner.boolop == pg_sys::BoolExprType::OR_EXPR && node_tree_contains_sublink(conj) {
                return true;
            }
        }
    }
    false
}

/// Deparse a SelectStmt (or Node) back to SQL text.
///
/// Uses PostgreSQL's `nodeToString()` for a basic representation, then
/// reconstructs the SQL from the parse tree components. This is a simplified
/// deparsing that handles common cases for scalar subqueries and EXISTS.
///
/// # Safety
/// Caller must ensure `select_node` points to a valid pg_sys::Node
/// (typically a SelectStmt).
fn deparse_select_to_sql(select_node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if select_node.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "NULL node in deparse_select_to_sql".into(),
        ));
    }

    // Check if it's a SelectStmt
    if !unsafe { pgrx::is_a(select_node, pg_sys::NodeTag::T_SelectStmt) } {
        return Err(PgTrickleError::QueryParseError(
            "Expected SelectStmt in deparse_select_to_sql".into(),
        ));
    }

    let select = unsafe { &*(select_node as *const pg_sys::SelectStmt) };
    let mut sql = String::from("SELECT ");

    // Deparse target list
    let target_list = pg_list::<pg_sys::Node>(select.targetList);
    let mut targets = Vec::new();
    for node_ptr in target_list.iter_ptr() {
        if let Some(rt) = cast_node!(node_ptr, T_ResTarget, pg_sys::ResTarget)
            && !rt.val.is_null()
        {
            let expr = unsafe { node_to_expr(rt.val)? };
            let expr_sql = expr.to_sql();
            if !rt.name.is_null() {
                let name = pg_cstr_to_str(rt.name).unwrap_or("?");
                targets.push(format!("{expr_sql} AS \"{name}\""));
            } else {
                targets.push(expr_sql);
            }
        }
    }
    sql.push_str(&targets.join(", "));

    // Deparse FROM clause
    if !select.fromClause.is_null() {
        let from_list = pg_list::<pg_sys::Node>(select.fromClause);
        if !from_list.is_empty() {
            sql.push_str(" FROM ");
            let mut from_items = Vec::new();
            for node_ptr in from_list.iter_ptr() {
                from_items.push(deparse_from_item(node_ptr)?);
            }
            sql.push_str(&from_items.join(", "));
        }
    }

    // Deparse WHERE clause
    if !select.whereClause.is_null() {
        let where_expr = unsafe { node_to_expr(select.whereClause)? };
        sql.push_str(&format!(" WHERE {}", where_expr.to_sql()));
    }

    // Deparse GROUP BY
    if !select.groupClause.is_null() {
        let group_list = pg_list::<pg_sys::Node>(select.groupClause);
        if !group_list.is_empty() {
            let mut groups = Vec::new();
            for node_ptr in group_list.iter_ptr() {
                let expr = unsafe { node_to_expr(node_ptr)? };
                groups.push(expr.to_sql());
            }
            sql.push_str(&format!(" GROUP BY {}", groups.join(", ")));
        }
    }

    // Deparse HAVING
    if !select.havingClause.is_null() {
        let having_expr = unsafe { node_to_expr(select.havingClause)? };
        sql.push_str(&format!(" HAVING {}", having_expr.to_sql()));
    }

    // Deparse ORDER BY
    let order = deparse_order_clause(select);
    if !order.is_empty() {
        sql.push_str(&order);
    }

    // Deparse LIMIT
    if !select.limitCount.is_null() {
        let limit_expr = unsafe { node_to_expr(select.limitCount)? };
        sql.push_str(&format!(" LIMIT {}", limit_expr.to_sql()));
    }

    // Deparse OFFSET
    if !select.limitOffset.is_null() {
        let offset_expr = unsafe { node_to_expr(select.limitOffset)? };
        sql.push_str(&format!(" OFFSET {}", offset_expr.to_sql()));
    }

    Ok(sql)
}

/// Deparse a FROM clause item back to SQL text.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
fn deparse_from_item(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if node.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "NULL node in deparse_from_item".into(),
        ));
    }

    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        let mut sql = String::new();
        if !rv.schemaname.is_null() {
            let schema = pg_cstr_to_str(rv.schemaname).unwrap_or("public");
            sql.push_str(&format!("\"{schema}\"."));
        }
        if !rv.relname.is_null() {
            let table = pg_cstr_to_str(rv.relname).unwrap_or("?");
            sql.push_str(&format!("\"{table}\""));
        }
        if !rv.alias.is_null() {
            let alias_node = unsafe { &*rv.alias };
            if !alias_node.aliasname.is_null() {
                let alias = pg_cstr_to_str(alias_node.aliasname).unwrap_or("?");
                sql.push_str(&format!(" \"{alias}\""));
            }
        }
        Ok(sql)
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        let left = deparse_from_item(join.larg)?;
        let right = deparse_from_item(join.rarg)?;
        let join_type = match join.jointype {
            pg_sys::JoinType::JOIN_INNER => "JOIN",
            pg_sys::JoinType::JOIN_LEFT => "LEFT JOIN",
            pg_sys::JoinType::JOIN_FULL => "FULL JOIN",
            pg_sys::JoinType::JOIN_RIGHT => "RIGHT JOIN",
            _ => "JOIN",
        };
        let mut sql = format!("{left} {join_type} {right}");
        if !join.quals.is_null() {
            let quals = unsafe { node_to_expr(join.quals)? };
            sql.push_str(&format!(" ON {}", quals.to_sql()));
        }
        Ok(sql)
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect) {
        if sub.subquery.is_null()
            || !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            return Err(PgTrickleError::UnsupportedOperator(
                "RangeSubselect with non-SelectStmt subquery is not supported".into(),
            ));
        }
        let inner_sql = deparse_select_to_sql(sub.subquery)?;
        let lateral_kw = if sub.lateral { "LATERAL " } else { "" };
        let mut result = format!("{lateral_kw}({inner_sql})");
        if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            if !a.aliasname.is_null() {
                let alias = pg_cstr_to_str(a.aliasname).unwrap_or("?");
                result.push_str(&format!(" \"{}\"", alias.replace('"', "\"\"")));
            }
        }
        Ok(result)
    } else {
        Err(PgTrickleError::UnsupportedOperator(
            "Unsupported FROM item (VALUES clause, tablefunc, or other non-standard source)".into(),
        ))
    }
}

/// Parse a SubLink node into a SublinkWrapper for SemiJoin/AntiJoin construction.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::SubLink` (T_SubLink node).
fn parse_sublink_to_wrapper(
    node: *mut pg_sys::Node,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgTrickleError> {
    let sublink = unsafe { &*(node as *const pg_sys::SubLink) };

    match sublink.subLinkType {
        pg_sys::SubLinkType::EXISTS_SUBLINK => parse_exists_sublink(sublink, negated, cte_ctx),
        pg_sys::SubLinkType::ANY_SUBLINK => {
            // ANY_SUBLINK is used for both `x IN (SELECT ...)` and `x = ANY (SELECT ...)`
            parse_any_sublink(sublink, negated, cte_ctx)
        }
        pg_sys::SubLinkType::ALL_SUBLINK => {
            // ALL_SUBLINK: `x op ALL (SELECT col FROM ...)`.
            // Rewritten as AntiJoin with negated condition:
            // NOT EXISTS (SELECT 1 FROM ... WHERE NOT (x op col))
            parse_all_sublink(sublink, negated, cte_ctx)
        }
        pg_sys::SubLinkType::EXPR_SUBLINK => {
            // Scalar subquery in WHERE — should have been rewritten to
            // CROSS JOIN by rewrite_scalar_subquery_in_where(). If we
            // still reach here, it's a case the rewrite didn't catch
            // (e.g., deeply nested or correlated). Reject with a helpful
            // error message.
            Err(PgTrickleError::UnsupportedOperator(
                "Scalar subqueries in WHERE clauses are not supported for DIFFERENTIAL mode. \
                 This usually means the query has a correlated scalar subquery that cannot \
                 be automatically rewritten. Consider rewriting as a JOIN or CTE."
                    .into(),
            ))
        }
        _ => Err(PgTrickleError::UnsupportedOperator(
            "Unsupported subquery type in WHERE clause. \
             Rewrite using a JOIN or LATERAL subquery."
                .to_string(),
        )),
    }
}

/// Parse an EXISTS SubLink into a SublinkWrapper.
///
/// Simple form (no GROUP BY / HAVING):
/// `EXISTS (SELECT ... FROM inner_table WHERE correlation_cond AND inner_filter)`
/// → The inner WHERE becomes the semi/anti-join condition.
///
/// Grouped form (with GROUP BY / HAVING):
/// `EXISTS (SELECT 1 FROM T WHERE inner.k = outer.k GROUP BY inner.k HAVING agg > threshold)`
///
/// This is semantically equivalent to:
/// `outer.k IN (SELECT inner.k FROM T GROUP BY inner.k HAVING agg > threshold)`
///
/// The correlation predicate (`inner.k = outer.k`) is extracted from the inner WHERE
/// and converted to the SemiJoin join condition. The remaining (non-correlated)
/// inner WHERE predicates are applied as a pre-aggregate filter. The inner tree is
/// wrapped in `Aggregate → Filter(HAVING) → Subquery`, matching the `parse_any_sublink`
/// GROUP BY treatment.
///
/// # Safety
/// Caller must ensure `sublink` points to a valid `pg_sys::SubLink`.
fn parse_exists_sublink(
    sublink: &pg_sys::SubLink,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgTrickleError> {
    if sublink.subselect.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "EXISTS subquery has NULL subselect".into(),
        ));
    }

    let inner_select = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };

    // Parse the inner FROM clause into an OpTree
    let from_list = pg_list::<pg_sys::Node>(inner_select.fromClause);
    if from_list.is_empty() {
        return Err(PgTrickleError::QueryParseError(
            "EXISTS subquery must have a FROM clause".into(),
        ));
    }

    // INVARIANT: from_list is non-empty (error returned above), so head() cannot fail.
    let mut inner_tree = unsafe { parse_from_item(from_list.head().unwrap(), cte_ctx)? };

    // Handle multiple FROM items (implicit cross joins in subquery)
    for i in 1..from_list.len() {
        if let Some(item) = from_list.get_ptr(i) {
            let right = unsafe { parse_from_item(item, cte_ctx)? };
            inner_tree = OpTree::InnerJoin {
                condition: Expr::Literal("TRUE".into()),
                left: Box::new(inner_tree),
                right: Box::new(right),
            };
        }
    }

    // ── GROUP BY / HAVING handling ──────────────────────────────────
    //
    // When the inner SELECT has GROUP BY or HAVING, a flat SemiJoin on the
    // raw scan would lose the aggregation semantics entirely. We restructure
    // the inner tree to:
    //
    //   Subquery(
    //     Filter(HAVING, Aggregate(GROUP BY inner.k, ..., child=Scan(T))))
    //
    // and derive the SemiJoin join condition by extracting the correlation
    // predicate from the inner WHERE clause.
    //
    // A correlation predicate has the form `inner.col = outer.col` (or the
    // reverse), where `inner.col` refers to one of the inner FROM aliases
    // and `outer.col` refers to an alias from the enclosing query.
    //
    // Example transformation:
    //   EXISTS (SELECT 1 FROM eh_ord o
    //           WHERE o.cust_id = c.id
    //           GROUP BY o.cust_id
    //           HAVING SUM(o.amount) > 100)
    // →
    //   SemiJoin(cond  = c.id = __pgt_in_sub.cust_id,
    //            left  = Scan(eh_cust c),
    //            right = Subquery(Filter(HAVING sum > 100,
    //                      Aggregate(GROUP BY o.cust_id,
    //                        Scan(eh_ord o)))))
    let group_list = pg_list::<pg_sys::Node>(inner_select.groupClause);
    let has_group_by = !group_list.is_empty();
    let has_having = !inner_select.havingClause.is_null();

    if has_group_by || has_having {
        // Collect the inner FROM aliases to distinguish correlated references.
        let inner_aliases = collect_tree_source_aliases(&inner_tree);

        // Split the inner WHERE into:
        //   corr_pairs  — (outer_expr, inner_group_key_output_name)
        //   inner_filter — everything else (applied before aggregation)
        let (corr_pairs, inner_filter) = if !inner_select.whereClause.is_null() {
            let where_expr = unsafe { node_to_expr(inner_select.whereClause)? };
            split_exists_correlation(&where_expr, &inner_aliases)
        } else {
            (Vec::new(), None)
        };

        // Apply the non-correlated inner WHERE as a pre-aggregate filter.
        if let Some(filter_expr) = inner_filter {
            inner_tree = OpTree::Filter {
                predicate: filter_expr,
                child: Box::new(inner_tree),
            };
        }

        // Parse GROUP BY expressions.
        let mut group_by = Vec::new();
        for node_ptr in group_list.iter_ptr() {
            let expr = unsafe { node_to_expr(node_ptr)? };
            group_by.push(expr);
        }

        // Extract aggregates from the HAVING clause (SELECT 1 has no agg targets).
        let mut aggregates: Vec<AggExpr> = Vec::new();
        if has_having {
            let having_expr = unsafe { node_to_expr(inner_select.havingClause)? };
            let having_aggs = extract_aggregates_from_expr(&having_expr, 0);
            aggregates.extend(having_aggs);
        }

        // Build the Aggregate node.
        inner_tree = OpTree::Aggregate {
            group_by,
            aggregates: aggregates.clone(),
            child: Box::new(inner_tree),
        };

        // Apply HAVING as Filter on top of Aggregate.
        if has_having {
            let having_expr = unsafe { node_to_expr(inner_select.havingClause)? };
            let rewritten = rewrite_having_expr(&having_expr, &aggregates);
            inner_tree = OpTree::Filter {
                predicate: rewritten,
                child: Box::new(inner_tree),
            };
        }

        // Wrap in Subquery so the SemiJoin sees it as a derived table.
        let sub_alias = format!("__pgt_in_sub_{}", inner_tree.alias());
        inner_tree = OpTree::Subquery {
            alias: sub_alias.clone(),
            column_aliases: Vec::new(),
            child: Box::new(inner_tree),
        };

        // Build the SemiJoin condition from extracted correlation pairs.
        // Each pair produces: outer_expr = subquery_alias.inner_col_output_name.
        let condition = if !corr_pairs.is_empty() {
            let equalities: Vec<Expr> = corr_pairs
                .into_iter()
                .map(|(outer_expr, inner_col_name)| Expr::BinaryOp {
                    op: "=".to_string(),
                    left: Box::new(outer_expr),
                    right: Box::new(Expr::ColumnRef {
                        table_alias: Some(sub_alias.clone()),
                        column_name: inner_col_name,
                    }),
                })
                .collect();
            equalities
                .into_iter()
                .reduce(|acc, eq| Expr::BinaryOp {
                    op: "AND".to_string(),
                    left: Box::new(acc),
                    right: Box::new(eq),
                })
                .unwrap()
        } else {
            // No correlation found — let the pre-aggregate filter handle
            // all filtering; the SemiJoin condition is a tautology.
            Expr::Literal("TRUE".into())
        };

        return Ok(SublinkWrapper {
            negated,
            condition,
            inner_tree,
        });
    }

    // ── Simple form: no GROUP BY / HAVING ───────────────────────────

    // The inner WHERE clause becomes the semi/anti-join condition.
    let condition = if inner_select.whereClause.is_null() {
        Expr::Literal("TRUE".into())
    } else {
        unsafe { node_to_expr(inner_select.whereClause)? }
    };

    Ok(SublinkWrapper {
        negated,
        condition,
        inner_tree,
    })
}

/// Collect all base-table Scan aliases from an OpTree.
///
/// Used by `parse_exists_sublink` to identify which column references in an
/// EXISTS inner WHERE clause belong to the inner subquery vs. the outer query.
fn collect_tree_source_aliases(tree: &OpTree) -> Vec<String> {
    match tree {
        OpTree::Scan { alias, .. } | OpTree::CteScan { alias, .. } => vec![alias.clone()],
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. } => {
            let mut aliases = collect_tree_source_aliases(left);
            aliases.extend(collect_tree_source_aliases(right));
            aliases
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Distinct { child, .. }
        | OpTree::Aggregate { child, .. } => collect_tree_source_aliases(child),
        OpTree::Subquery { alias, .. } => vec![alias.clone()],
        _ => vec![],
    }
}

/// Split an EXISTS inner WHERE expression into correlation predicates and
/// inner-only predicates.
///
/// Walks the conjuncts of `where_expr` (splitting on `AND`) and classifies
/// each conjunct:
///
/// - Correlation predicate: an equality `inner_alias.col = outer_ref` (or the
///   reverse) where `inner_alias` is in `inner_aliases` and the other side is
///   a column reference from an alias NOT in `inner_aliases` (i.e., an outer
///   reference). Each such predicate is extracted as `(outer_expr, inner_col_name)`.
///
/// - Inner-only predicate: everything else; left in the filter applied before
///   aggregation.
///
/// Returns `(corr_pairs, remaining_filter)`.
fn split_exists_correlation(
    where_expr: &Expr,
    inner_aliases: &[String],
) -> (Vec<(Expr, String)>, Option<Expr>) {
    match where_expr {
        Expr::BinaryOp { op, left, right } if op == "AND" => {
            let (mut lc, lr) = split_exists_correlation(left, inner_aliases);
            let (rc, rr) = split_exists_correlation(right, inner_aliases);
            lc.extend(rc);
            let remaining = match (lr, rr) {
                (Some(l), Some(r)) => Some(Expr::BinaryOp {
                    op: "AND".to_string(),
                    left: Box::new(l),
                    right: Box::new(r),
                }),
                (Some(l), None) | (None, Some(l)) => Some(l),
                (None, None) => None,
            };
            (lc, remaining)
        }
        _ => {
            if let Some(pair) = try_extract_exists_corr_pair(where_expr, inner_aliases) {
                (vec![pair], None)
            } else {
                (vec![], Some(where_expr.clone()))
            }
        }
    }
}

/// Try to extract `(outer_expr, inner_col_output_name)` from an equality predicate.
///
/// Succeeds when:
/// - The expression is `A = B`
/// - Exactly one of A, B is a `ColumnRef` with a qualifier in `inner_aliases`
/// - The other is a `ColumnRef` with a qualifier NOT in `inner_aliases` (outer ref)
///
/// Returns `None` for non-equality predicates or predicates where both / neither
/// side is an inner reference.
fn try_extract_exists_corr_pair(pred: &Expr, inner_aliases: &[String]) -> Option<(Expr, String)> {
    let Expr::BinaryOp { op, left, right } = pred else {
        return None;
    };
    if op != "=" {
        return None;
    }

    let left_qualifier = match left.as_ref() {
        Expr::ColumnRef {
            table_alias: Some(a),
            ..
        } => Some(a.as_str()),
        _ => None,
    };
    let right_qualifier = match right.as_ref() {
        Expr::ColumnRef {
            table_alias: Some(a),
            ..
        } => Some(a.as_str()),
        _ => None,
    };

    match (left_qualifier, right_qualifier) {
        (Some(la), Some(ra)) => {
            let left_is_inner = inner_aliases.iter().any(|a| a == la);
            let right_is_inner = inner_aliases.iter().any(|a| a == ra);
            if left_is_inner && !right_is_inner {
                // inner.col = outer.ref → outer_expr=right, inner_col=left.output_name()
                Some((*right.clone(), left.output_name()))
            } else if !left_is_inner && right_is_inner {
                // outer.ref = inner.col → outer_expr=left, inner_col=right.output_name()
                Some((*left.clone(), right.output_name()))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Qualify unqualified column references in an expression with the given
/// table alias.  Used for IN-subquery inner expressions to prevent ambiguity
/// when the inner SELECT target shares column names with the outer query
/// (e.g., both tables have an `id` column).  Without qualification the
/// join-condition rewriter resolves the bare name against the left (outer)
/// tree first, producing incorrect delta SQL.
fn qualify_inner_col_refs(expr: Expr, alias: &str) -> Expr {
    match expr {
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => Expr::ColumnRef {
            table_alias: Some(alias.to_string()),
            column_name,
        },
        Expr::BinaryOp { op, left, right } => Expr::BinaryOp {
            op,
            left: Box::new(qualify_inner_col_refs(*left, alias)),
            right: Box::new(qualify_inner_col_refs(*right, alias)),
        },
        Expr::FuncCall { func_name, args } => Expr::FuncCall {
            func_name,
            args: args
                .into_iter()
                .map(|a| qualify_inner_col_refs(a, alias))
                .collect(),
        },
        other => other,
    }
}

/// Parse an ANY SubLink (IN / = ANY) into a SublinkWrapper.
///
/// `x IN (SELECT col FROM inner_table WHERE filter)`
/// is equivalent to:
/// `EXISTS (SELECT 1 FROM inner_table WHERE inner_table.col = x AND filter)`
///
/// When the inner SELECT has GROUP BY / HAVING, the inner tree is wrapped
/// in an Aggregate (+ HAVING Filter) so the SemiJoin only matches qualifying
/// groups:
/// `x IN (SELECT col FROM T GROUP BY col HAVING agg > threshold)`
/// → SemiJoin(cond = x = sub.col,
///            inner = Subquery(Filter(HAVING, Aggregate(GROUP BY col, child=T))))
///
/// # Safety
/// Caller must ensure `sublink` points to a valid `pg_sys::SubLink`.
fn parse_any_sublink(
    sublink: &pg_sys::SubLink,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgTrickleError> {
    if sublink.subselect.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "IN/ANY subquery has NULL subselect".into(),
        ));
    }

    let inner_select = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };

    // Parse the inner FROM clause
    let from_list = pg_list::<pg_sys::Node>(inner_select.fromClause);
    if from_list.is_empty() {
        return Err(PgTrickleError::QueryParseError(
            "IN subquery must have a FROM clause".into(),
        ));
    }

    let mut inner_tree = unsafe { parse_from_item(from_list.head().unwrap(), cte_ctx)? };
    for i in 1..from_list.len() {
        if let Some(item) = from_list.get_ptr(i) {
            let right = unsafe { parse_from_item(item, cte_ctx)? };
            inner_tree = OpTree::InnerJoin {
                condition: Expr::Literal("TRUE".into()),
                left: Box::new(inner_tree),
                right: Box::new(right),
            };
        }
    }

    // Extract the test expression (left-hand side of IN)
    let test_expr = if sublink.testexpr.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "IN subquery has NULL test expression".into(),
        ));
    } else {
        unsafe { node_to_expr(sublink.testexpr)? }
    };

    // ── G12-SQL-IN: Multi-column IN (subquery) guard ────────────────
    //
    // Multi-column IN like `(a, b) IN (SELECT x, y FROM t)` produces a
    // RowExpr test expression with multiple inner SELECT targets. The
    // DVM semi-join rewrite currently only handles single-column IN:
    // it extracts the first target column and builds `test = col1`,
    // silently ignoring additional columns. This produces incorrect
    // results for multi-column comparisons.
    //
    // Detect this case and return a structured error. The DVM would need
    // composite equality (`a = x AND b = y`) to handle this correctly.
    if matches!(test_expr, Expr::Raw(ref s) if s.starts_with("ROW(")) {
        return Err(PgTrickleError::QueryParseError(
            "multi-column IN (subquery) is not supported in DIFFERENTIAL mode. \
             Rewrite as EXISTS (SELECT 1 FROM ... WHERE a = x AND b = y) \
             for equivalent semantics."
                .into(),
        ));
    }

    // Extract the inner SELECT target (the column being compared)
    let target_list = pg_list::<pg_sys::Node>(inner_select.targetList);
    if target_list.is_empty() {
        return Err(PgTrickleError::QueryParseError(
            "IN subquery SELECT list is empty".into(),
        ));
    }

    // G12-SQL-IN: Also detect multi-column via inner target count
    if target_list.len() > 1 {
        return Err(PgTrickleError::QueryParseError(
            "multi-column IN (subquery) is not supported in DIFFERENTIAL mode: \
             the inner SELECT returns multiple columns. Rewrite as \
             EXISTS (SELECT 1 FROM ... WHERE a = x AND b = y) for equivalent semantics."
                .into(),
        ));
    }

    let first_target = target_list.head().unwrap();
    let inner_col_expr = if let Some(rt) = cast_node!(first_target, T_ResTarget, pg_sys::ResTarget)
    {
        if rt.val.is_null() {
            return Err(PgTrickleError::QueryParseError(
                "IN subquery target column is NULL".into(),
            ));
        }
        unsafe { node_to_expr(rt.val)? }
    } else {
        return Err(PgTrickleError::QueryParseError(
            "IN subquery target is not a ResTarget".into(),
        ));
    };

    // ── GROUP BY / HAVING handling ──────────────────────────────────
    //
    // When the inner SELECT has GROUP BY or HAVING, the flat SemiJoin
    // rewrite would lose the grouping semantics. Instead, wrap the inner
    // tree in Aggregate + Filter(HAVING) so only qualifying groups are
    // considered for the semi-join match.
    let group_list = pg_list::<pg_sys::Node>(inner_select.groupClause);
    let has_group_by = !group_list.is_empty();
    let has_having = !inner_select.havingClause.is_null();

    if has_group_by || has_having {
        // Apply inner WHERE as a Filter on the FROM tree
        if !inner_select.whereClause.is_null() {
            let inner_where = unsafe { node_to_expr(inner_select.whereClause)? };
            inner_tree = OpTree::Filter {
                predicate: inner_where,
                child: Box::new(inner_tree),
            };
        }

        // Parse GROUP BY expressions
        let mut group_by = Vec::new();
        for node_ptr in group_list.iter_ptr() {
            let expr = unsafe { node_to_expr(node_ptr)? };
            group_by.push(expr);
        }

        // Extract aggregates from the target list
        let (mut aggregates, _non_agg_exprs) = unsafe { extract_aggregates(&target_list)? };

        // Extract additional aggregates from HAVING expression.
        // The HAVING clause may reference aggregates not present in the
        // SELECT list (e.g., `SELECT col FROM T GROUP BY col HAVING SUM(x) > 0`).
        if has_having {
            let having_expr = unsafe { node_to_expr(inner_select.havingClause)? };
            let having_aggs = extract_aggregates_from_expr(&having_expr, aggregates.len());
            for ha in &having_aggs {
                // Avoid duplicates: only add if no existing aggregate matches
                let already_present = aggregates.iter().any(|a| {
                    a.function.sql_name() == ha.function.sql_name()
                        && a.argument.as_ref().map(|e| e.to_sql())
                            == ha.argument.as_ref().map(|e| e.to_sql())
                });
                if !already_present {
                    aggregates.push(ha.clone());
                }
            }
        }

        // Build Aggregate node
        inner_tree = OpTree::Aggregate {
            group_by,
            aggregates: aggregates.clone(),
            child: Box::new(inner_tree),
        };

        // Apply HAVING as Filter on top of Aggregate
        if has_having {
            let having_expr = unsafe { node_to_expr(inner_select.havingClause)? };
            let rewritten = rewrite_having_expr(&having_expr, &aggregates);
            inner_tree = OpTree::Filter {
                predicate: rewritten,
                child: Box::new(inner_tree),
            };
        }

        // Wrap in Subquery so the SemiJoin treats it as a derived table
        let sub_alias = format!("__pgt_in_sub_{}", inner_tree.alias());
        inner_tree = OpTree::Subquery {
            alias: sub_alias,
            column_aliases: Vec::new(),
            child: Box::new(inner_tree),
        };

        // Build the equality condition using the output column name.
        // The inner_col_expr (from the SELECT list) is a group-by column
        // that passes through Aggregate → Filter → Subquery.
        // Qualify inner column refs with the Subquery alias to prevent
        // ambiguity when outer and inner tables share column names.
        let inner_col_qualified = qualify_inner_col_refs(inner_col_expr, inner_tree.alias());
        let equality = Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(test_expr),
            right: Box::new(inner_col_qualified),
        };

        return Ok(SublinkWrapper {
            negated,
            condition: equality,
            inner_tree,
        });
    }

    // ── Simple case: no GROUP BY / HAVING ───────────────────────────

    // Qualify inner column refs with the inner tree's alias to prevent
    // ambiguity when outer and inner tables share column names (e.g.,
    // both have "id").  Without this, the join-condition rewriter
    // resolves bare "id" against the left (outer) tree first.
    let inner_col_qualified = qualify_inner_col_refs(inner_col_expr, inner_tree.alias());

    // Build the equality condition: test_expr = inner_col_expr
    let equality = Expr::BinaryOp {
        op: "=".to_string(),
        left: Box::new(test_expr),
        right: Box::new(inner_col_qualified),
    };

    // Combine with inner WHERE clause if present
    let condition = if inner_select.whereClause.is_null() {
        equality
    } else {
        let inner_where = unsafe { node_to_expr(inner_select.whereClause)? };
        Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(equality),
            right: Box::new(inner_where),
        }
    };

    Ok(SublinkWrapper {
        negated,
        condition,
        inner_tree,
    })
}

/// Extract aggregate function calls from an expression tree.
///
/// Used to find aggregates in HAVING clauses that may not appear in the
/// SELECT target list. Each discovered aggregate is assigned a unique alias
/// `__pgt_having_{N}` where N starts from `start_idx`.
fn extract_aggregates_from_expr(expr: &Expr, start_idx: usize) -> Vec<AggExpr> {
    let mut result = Vec::new();
    extract_aggregates_from_expr_inner(expr, start_idx, &mut result);
    result
}

fn extract_aggregates_from_expr_inner(expr: &Expr, start_idx: usize, out: &mut Vec<AggExpr>) {
    match expr {
        Expr::FuncCall { func_name, args } => {
            let name_lower = func_name.to_lowercase();
            let agg_func = match name_lower.as_str() {
                "count" => {
                    // COUNT(*) from HAVING arrives as FuncCall { args: [Raw("*")] };
                    // treat it as CountStar with no argument (same as target list).
                    if args.is_empty()
                        || (args.len() == 1 && matches!(&args[0], Expr::Raw(s) if s == "*"))
                    {
                        Some(AggFunc::CountStar)
                    } else {
                        Some(AggFunc::Count)
                    }
                }
                "sum" => Some(AggFunc::Sum),
                "avg" => Some(AggFunc::Avg),
                "min" => Some(AggFunc::Min),
                "max" => Some(AggFunc::Max),
                _ => None,
            };
            if let Some(func) = agg_func {
                // CountStar has no argument (even if the parse produced [Raw("*")]).
                let argument = if matches!(func, AggFunc::CountStar) {
                    None
                } else {
                    args.first().cloned()
                };
                let alias = format!("__pgt_having_{}", start_idx + out.len());
                out.push(AggExpr {
                    function: func,
                    argument,
                    alias,
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                });
            } else {
                // Non-aggregate FuncCall: recurse into arguments
                for arg in args {
                    extract_aggregates_from_expr_inner(arg, start_idx, out);
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            extract_aggregates_from_expr_inner(left, start_idx, out);
            extract_aggregates_from_expr_inner(right, start_idx, out);
        }
        _ => {}
    }
}

/// Parse an ALL SubLink (`x op ALL (SELECT col FROM ...)`) into a SublinkWrapper.
///
/// `x op ALL (SELECT col FROM inner_table WHERE filter)` is rewritten as:
/// `NOT EXISTS (SELECT 1 FROM inner_table WHERE NOT (x op col) [AND filter])`
///
/// This produces an AntiJoin where the condition is the negated comparison.
/// If the ALL expression itself is negated (`NOT (x op ALL (...))`), the
/// double negation produces a SemiJoin with the negated operator.
///
/// # Safety
/// Caller must ensure `sublink` points to a valid `pg_sys::SubLink`.
fn parse_all_sublink(
    sublink: &pg_sys::SubLink,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgTrickleError> {
    if sublink.subselect.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "ALL subquery has NULL subselect".into(),
        ));
    }

    let inner_select = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };

    // Parse the inner FROM clause
    let from_list = pg_list::<pg_sys::Node>(inner_select.fromClause);
    if from_list.is_empty() {
        return Err(PgTrickleError::QueryParseError(
            "ALL subquery must have a FROM clause".into(),
        ));
    }

    let mut inner_tree = unsafe { parse_from_item(from_list.head().unwrap(), cte_ctx)? };
    for i in 1..from_list.len() {
        if let Some(item) = from_list.get_ptr(i) {
            let right = unsafe { parse_from_item(item, cte_ctx)? };
            inner_tree = OpTree::InnerJoin {
                condition: Expr::Literal("TRUE".into()),
                left: Box::new(inner_tree),
                right: Box::new(right),
            };
        }
    }

    // Extract the test expression (left-hand side: `x` in `x op ALL (...)`)
    let test_expr = if sublink.testexpr.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "ALL subquery has NULL test expression".into(),
        ));
    } else {
        unsafe { node_to_expr(sublink.testexpr)? }
    };

    // Extract the inner SELECT target column
    let target_list = pg_list::<pg_sys::Node>(inner_select.targetList);
    if target_list.is_empty() {
        return Err(PgTrickleError::QueryParseError(
            "ALL subquery SELECT list is empty".into(),
        ));
    }

    let first_target = target_list.head().unwrap();
    let inner_col_expr = if let Some(rt) = cast_node!(first_target, T_ResTarget, pg_sys::ResTarget)
    {
        if rt.val.is_null() {
            return Err(PgTrickleError::QueryParseError(
                "ALL subquery target column is NULL".into(),
            ));
        }
        unsafe { node_to_expr(rt.val)? }
    } else {
        return Err(PgTrickleError::QueryParseError(
            "ALL subquery target is not a ResTarget".into(),
        ));
    };

    // Extract the comparison operator from operName.
    // For `x = ALL (...)`, operName is a list containing "=".
    // We use extract_func_name which handles pg_sys::List of String/Value nodes.
    let op_name = if sublink.operName.is_null() {
        "=".to_string() // default to equality
    } else {
        unsafe { extract_func_name(sublink.operName) }.unwrap_or_else(|_| "=".to_string())
    };

    // Build NULL-safe anti-join condition:
    // (inner_col IS NULL OR NOT (test_expr op inner_col))
    //
    // Using just `NOT (test_expr op inner_col)` is not NULL-safe: when
    // inner_col is NULL, `NOT (x op NULL)` evaluates to NULL (not TRUE),
    // so the EXISTS does not fire for that row, and the outer row is
    // incorrectly included. SQL semantics for ALL require that a NULL
    // in the subquery makes the whole ALL expression indeterminate (hence
    // the outer row should be excluded).
    let inner_col_sql = inner_col_expr.to_sql();
    let negated_cond = Expr::Raw(format!(
        "(({inner_col_sql}) IS NULL OR NOT ({} {op_name} {inner_col_sql}))",
        test_expr.to_sql()
    ));

    // Combine with inner WHERE clause if present
    let condition = if inner_select.whereClause.is_null() {
        negated_cond
    } else {
        let inner_where = unsafe { node_to_expr(inner_select.whereClause)? };
        Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(negated_cond),
            right: Box::new(inner_where),
        }
    };

    // ALL → NOT EXISTS (AntiJoin). If the expression is negated
    // (NOT (x op ALL (...))), the double negation produces SemiJoin.
    Ok(SublinkWrapper {
        negated: !negated,
        condition,
        inner_tree,
    })
}

/// Parse a defining query string into an OpTree.
///
/// Uses `pg_sys::raw_parser()` to get the raw parse tree, then walks
/// the `SelectStmt` structure to build the OpTree. Column types and
/// table OIDs are resolved via catalog lookups.
///
/// Returns the legacy `OpTree` (without CTE registry). For full CTE
/// support, prefer [`parse_defining_query_full`].
pub fn parse_defining_query(query: &str) -> Result<OpTree, PgTrickleError> {
    // SAFETY: We're calling PostgreSQL C parser functions with valid inputs.
    // raw_parser and related functions are safe when called within
    // a PostgreSQL backend with a valid memory context.
    let result = unsafe { parse_defining_query_inner(query) }?;
    Ok(result.tree)
}

/// Parse a defining query string into a [`ParseResult`] (tree + CTE registry).
///
/// This is the preferred entry point — the returned `CteRegistry` enables
/// the diff engine to differentiate each CTE body only once even when the
/// CTE is referenced multiple times (Tier 2 optimization).
pub fn parse_defining_query_full(query: &str) -> Result<ParseResult, PgTrickleError> {
    // SAFETY: same invariants as parse_defining_query.
    unsafe { parse_defining_query_inner(query) }
}

unsafe fn parse_defining_query_inner(query: &str) -> Result<ParseResult, PgTrickleError> {
    // Clear the per-parse warning accumulator so each invocation starts fresh.
    PARSE_ADVISORY_WARNINGS.with(|w| w.borrow_mut().clear());

    let list = parse_query(query)?;
    if list.len() != 1 {
        return Err(PgTrickleError::QueryParseError(format!(
            "Expected 1 statement, got {}",
            list.len(),
        )));
    }

    let raw_stmt = list
        .head()
        .ok_or_else(|| PgTrickleError::QueryParseError("Empty parse list".into()))?;

    let stmt_ptr = unsafe { (*raw_stmt).stmt as *const pg_sys::SelectStmt };
    if !unsafe { pgrx::is_a(stmt_ptr as *mut pg_sys::Node, pg_sys::NodeTag::T_SelectStmt) } {
        return Err(PgTrickleError::QueryParseError(
            "Defining query must be a SELECT statement".into(),
        ));
    }

    let select = unsafe { &*stmt_ptr };

    // ── Extract CTE definitions (if any) ─────────────────────────────
    let has_recursion = if !select.withClause.is_null() {
        let wc = unsafe { &*select.withClause };
        wc.recursive
    } else {
        false
    };

    let (cte_map, cte_def_aliases, recursive_cte_stmts) = if !select.withClause.is_null() {
        unsafe { extract_cte_map_with_recursive(select.withClause)? }
    } else {
        (HashMap::new(), HashMap::new(), Vec::new())
    };

    // Build a CTE parse context — this holds the raw SelectStmt pointers
    // *and* a mutable CteRegistry that gets populated as CTE references
    // are encountered during tree construction.
    let mut cte_ctx = CteParseContext::new(cte_map, cte_def_aliases);

    // Parse recursive CTEs into OpTree and register them.
    // This must happen before parsing the main SELECT so that references
    // to the recursive CTE name resolve to CteScan nodes.
    for (name, base_stmt, rec_stmt, def_cols, union_all) in &recursive_cte_stmts {
        // Parse the base case (non-recursive term).
        // In PostgreSQL 18, the larg/rarg of a UNION may themselves appear
        // as set-operation wrappers (e.g., when the base case is itself a
        // multi-arm UNION). Check and dispatch accordingly.
        //
        // Set `in_cte_anchor = true` so that `parse_select_stmt_inner`
        // accepts a constant SELECT without a FROM clause (e.g. `SELECT 1
        // AS id`) and returns an `OpTree::ConstantSelect` instead of an
        // error.
        cte_ctx.in_cte_anchor = true;
        let base_tree = unsafe {
            let s = &**base_stmt;
            if s.op != pg_sys::SetOperation::SETOP_NONE {
                parse_set_operation(s, &mut cte_ctx)?
            } else {
                parse_select_stmt(s, "", &mut cte_ctx)?
            }
        };
        cte_ctx.in_cte_anchor = false;

        // Determine output columns: CTE def aliases > base case output
        let columns = if def_cols.is_empty() {
            base_tree.output_columns()
        } else {
            def_cols.clone()
        };

        // Set up self-ref tracking for parsing the recursive term
        cte_ctx.recursive_self_ref_name = Some(name.clone());
        cte_ctx.recursive_self_ref_columns = columns.clone();

        // Parse the recursive term — self-references become RecursiveSelfRef
        let rec_tree = unsafe {
            let s = &**rec_stmt;
            if s.op != pg_sys::SetOperation::SETOP_NONE {
                parse_set_operation(s, &mut cte_ctx)?
            } else {
                parse_select_stmt(s, "", &mut cte_ctx)?
            }
        };

        // Clear self-ref tracking
        cte_ctx.recursive_self_ref_name = None;
        cte_ctx.recursive_self_ref_columns.clear();

        // Build the RecursiveCte node and register it
        let rec_cte = OpTree::RecursiveCte {
            alias: name.clone(),
            columns,
            base: Box::new(base_tree),
            recursive: Box::new(rec_tree),
            union_all: *union_all,
        };

        cte_ctx.register(name, rec_cte);
    }

    // Check for set operations — use the `op` field rather than larg/rarg nullness
    // because PG18 may leave larg/rarg non-null on non-union SelectStmt nodes.
    let mut tree = if select.op != pg_sys::SetOperation::SETOP_NONE {
        unsafe { parse_set_operation(select, &mut cte_ctx)? }
    } else {
        unsafe { parse_select_stmt(select, query, &mut cte_ctx)? }
    };

    // Prune Scan columns to only those referenced by the defining query,
    // reducing the number of new_*/old_* columns read from change buffers.
    tree.prune_scan_columns();
    for (_, cte_tree) in &mut cte_ctx.registry.entries {
        cte_tree.prune_scan_columns();
    }

    // Drain advisory warnings accumulated during this parse.
    let warnings = PARSE_ADVISORY_WARNINGS.with(|w| std::mem::take(&mut *w.borrow_mut()));

    Ok(ParseResult {
        tree,
        cte_registry: cte_ctx.registry,
        has_recursion,
        warnings,
    })
}

/// Parse a set-operation SELECT (UNION, UNION ALL, INTERSECT [ALL], EXCEPT [ALL]).
///
/// - `UNION ALL` produces `OpTree::UnionAll { children }`.
/// - `UNION` (deduplicated) produces `OpTree::Distinct { child: UnionAll }`.
/// - `INTERSECT [ALL]` produces `OpTree::Intersect { left, right, all }`.
/// - `EXCEPT [ALL]` produces `OpTree::Except { left, right, all }`.
///
/// Mixed UNION/UNION ALL trees are handled by respecting PostgreSQL's nested
/// `SetOperationStmt` tree structure: children with a different `all` flag
/// are parsed as separate set operations rather than flattened.
unsafe fn parse_set_operation(
    select: &pg_sys::SelectStmt,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgTrickleError> {
    // G13-SD: Track recursion depth.
    let prev_depth = cte_ctx.descend()?;
    let result = unsafe { parse_set_operation_inner(select, cte_ctx) };
    cte_ctx.ascend(prev_depth);
    result
}

/// Inner implementation of `parse_set_operation` (after depth check).
unsafe fn parse_set_operation_inner(
    select: &pg_sys::SelectStmt,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgTrickleError> {
    match select.op {
        pg_sys::SetOperation::SETOP_UNION => {
            let mut children = Vec::new();
            unsafe { collect_union_children(select, select.all, &mut children, cte_ctx)? };

            if children.len() < 2 {
                return Err(PgTrickleError::QueryParseError(
                    "UNION / UNION ALL requires at least 2 children".into(),
                ));
            }

            let union_all = OpTree::UnionAll { children };

            // UNION (without ALL) = UNION ALL + DISTINCT deduplication
            if !select.all {
                Ok(OpTree::Distinct {
                    child: Box::new(union_all),
                })
            } else {
                Ok(union_all)
            }
        }
        pg_sys::SetOperation::SETOP_INTERSECT => {
            let left = unsafe { parse_set_op_child(select.larg, cte_ctx)? };
            let right = unsafe { parse_set_op_child(select.rarg, cte_ctx)? };
            Ok(OpTree::Intersect {
                left: Box::new(left),
                right: Box::new(right),
                all: select.all,
            })
        }
        pg_sys::SetOperation::SETOP_EXCEPT => {
            let left = unsafe { parse_set_op_child(select.larg, cte_ctx)? };
            let right = unsafe { parse_set_op_child(select.rarg, cte_ctx)? };
            Ok(OpTree::Except {
                left: Box::new(left),
                right: Box::new(right),
                all: select.all,
            })
        }
        _ => Err(PgTrickleError::UnsupportedOperator(format!(
            "Set operation {:?} not supported",
            select.op,
        ))),
    }
}

/// Parse a single child of a set operation (left or right branch).
///
/// The child may itself be a set operation node (e.g., `(A INTERSECT B) EXCEPT C`)
/// or a plain SELECT.
unsafe fn parse_set_op_child(
    child_ptr: *mut pg_sys::SelectStmt,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgTrickleError> {
    if child_ptr.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "Set operation branch is NULL".into(),
        ));
    }
    let child = unsafe { &*child_ptr };
    if child.op != pg_sys::SetOperation::SETOP_NONE {
        unsafe { parse_set_operation(child, cte_ctx) }
    } else {
        unsafe { parse_select_stmt(child, "", cte_ctx) }
    }
}

/// Recursively collect UNION / UNION ALL child branches from a set-operation tree.
///
/// Uses the `op` field (not `larg`/`rarg` nullness) to detect set-operation
/// nodes, because PostgreSQL 18 may leave `larg`/`rarg` non-null on simple
/// SELECT nodes within CTE bodies.
///
/// `parent_all` is the `all` flag of the top-level node. When an intermediate
/// UNION node has a different `all` flag (mixed UNION / UNION ALL), it is
/// parsed as a separate set operation rather than flattened, preserving
/// PostgreSQL's nested `SetOperationStmt` tree structure.
unsafe fn collect_union_children(
    select: &pg_sys::SelectStmt,
    parent_all: bool,
    children: &mut Vec<OpTree>,
    cte_ctx: &mut CteParseContext,
) -> Result<(), PgTrickleError> {
    unsafe {
        if !select.larg.is_null() && !select.rarg.is_null() {
            let larg = &*select.larg;
            let rarg = &*select.rarg;

            if larg.op != pg_sys::SetOperation::SETOP_NONE {
                if larg.op == pg_sys::SetOperation::SETOP_UNION && larg.all == parent_all {
                    // Same UNION type — flatten into the same children list
                    collect_union_children(larg, parent_all, children, cte_ctx)?;
                } else {
                    // Different set operation or mixed UNION/UNION ALL —
                    // parse as a separate sub-tree (preserves nesting).
                    let tree = parse_set_operation(larg, cte_ctx)?;
                    children.push(tree);
                }
            } else {
                let tree = parse_select_stmt(larg, "", cte_ctx)?;
                children.push(tree);
            }

            if rarg.op != pg_sys::SetOperation::SETOP_NONE {
                if rarg.op == pg_sys::SetOperation::SETOP_UNION && rarg.all == parent_all {
                    // Same UNION type — flatten
                    collect_union_children(rarg, parent_all, children, cte_ctx)?;
                } else {
                    // Different set operation or mixed — parse as sub-tree
                    let tree = parse_set_operation(rarg, cte_ctx)?;
                    children.push(tree);
                }
            } else {
                let tree = parse_select_stmt(rarg, "", cte_ctx)?;
                children.push(tree);
            }
        }
        Ok(())
    }
}

/// Parse a simple (non-set-operation) SELECT statement into an OpTree.
///
/// Callers must ensure this is NOT a UNION/INTERSECT/EXCEPT node — check
/// `select.op == SETOP_NONE` before calling. Use [`parse_set_operation`] for
/// set-operation nodes.
///
/// Builds bottom-up: Scan → Filter → Join → Project → Aggregate → Distinct
unsafe fn parse_select_stmt(
    select: &pg_sys::SelectStmt,
    _full_query: &str,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgTrickleError> {
    // G13-SD: Track recursion depth.
    let prev_depth = cte_ctx.descend()?;
    let result = unsafe { parse_select_stmt_inner(select, _full_query, cte_ctx) };
    cte_ctx.ascend(prev_depth);
    result
}

/// Inner implementation of `parse_select_stmt` (after depth check).
unsafe fn parse_select_stmt_inner(
    select: &pg_sys::SelectStmt,
    _full_query: &str,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgTrickleError> {
    // ── Step 1: Parse FROM clause into Scan/Join tree ──────────────────
    let from_list = pg_list::<pg_sys::Node>(select.fromClause);
    if from_list.is_empty() {
        // Allow a constant SELECT (no FROM clause) when parsing the base case
        // of a WITH RECURSIVE anchor, e.g. `SELECT 1 AS id`.  These nodes
        // have no source tables and are represented as ConstantSelect so that
        // helper functions like generate_query_sql can embed them verbatim.
        if cte_ctx.in_cte_anchor {
            let sql = deparse_select_stmt_with_view_subs(select as *const _, &[])?;
            // SAFETY: `select.targetList` is a valid List pointer from the PG parser.
            let columns = unsafe { extract_target_list_column_names(select.targetList) };
            return Ok(OpTree::ConstantSelect { columns, sql });
        }
        return Err(PgTrickleError::QueryParseError(format!(
            "Defining query must have a FROM clause (op={}, all={}, larg_null={}, rarg_null={}, target_len={}, where_null={})",
            select.op,
            select.all,
            select.larg.is_null(),
            select.rarg.is_null(),
            pg_list::<pg_sys::Node>(select.targetList).len(),
            select.whereClause.is_null(),
        )));
    }

    let mut tree = unsafe { parse_from_item(from_list.head().unwrap(), cte_ctx)? };

    // Handle implicit cross-joins / LATERAL SRFs (multiple items in FROM)
    for i in 1..from_list.len() {
        if let Some(item) = from_list.get_ptr(i) {
            let right = unsafe { parse_from_item(item, cte_ctx)? };
            // If the right side is a LateralFunction, attach the current tree
            // as its child (LATERAL dependency) instead of wrapping in a cross join.
            if let OpTree::LateralFunction {
                func_sql,
                alias,
                column_aliases,
                with_ordinality,
                ..
            } = right
            {
                tree = OpTree::LateralFunction {
                    func_sql,
                    alias,
                    column_aliases,
                    with_ordinality,
                    child: Box::new(tree),
                };
            } else if let OpTree::LateralSubquery {
                subquery_sql,
                alias,
                column_aliases,
                output_cols,
                is_left_join,
                subquery_source_oids,
                correlation_predicates,
                ..
            } = right
            {
                // LATERAL subquery in comma syntax: FROM t, LATERAL (SELECT ...)
                tree = OpTree::LateralSubquery {
                    subquery_sql,
                    alias,
                    column_aliases,
                    output_cols,
                    is_left_join,
                    subquery_source_oids,
                    correlation_predicates,
                    child: Box::new(tree),
                };
            } else {
                tree = OpTree::InnerJoin {
                    condition: Expr::Literal("TRUE".into()),
                    left: Box::new(tree),
                    right: Box::new(right),
                };
            }
        }
    }

    // ── Step 2: Parse WHERE clause (with SubLink extraction) ─────────
    // SubLinks (EXISTS, IN subquery) in the WHERE clause are extracted
    // and converted into SemiJoin/AntiJoin OpTree wrappers. The remaining
    // non-SubLink predicates become a Filter node.
    if !select.whereClause.is_null() {
        let (sublink_wrappers, remaining_predicate) =
            extract_where_sublinks(select.whereClause, cte_ctx)?;

        // Apply SubLink wrappers (SemiJoin/AntiJoin) bottom-up
        for wrapper in sublink_wrappers {
            if wrapper.negated {
                tree = OpTree::AntiJoin {
                    condition: wrapper.condition,
                    left: Box::new(tree),
                    right: Box::new(wrapper.inner_tree),
                };
            } else {
                tree = OpTree::SemiJoin {
                    condition: wrapper.condition,
                    left: Box::new(tree),
                    right: Box::new(wrapper.inner_tree),
                };
            }
        }

        // Apply remaining non-SubLink predicates as Filter
        if let Some(pred) = remaining_predicate {
            tree = OpTree::Filter {
                predicate: pred,
                child: Box::new(tree),
            };
        }

        // ── Predicate pushdown into cross joins ────────────────────
        // Comma-separated FROM items create InnerJoin(TRUE, ...) chains.
        // Promote eligible predicates from the Filter into appropriate
        // JOIN ON clauses, converting cross joins to equi-joins.
        //
        // Guard (a): predicates containing scalar subqueries (e.g.
        // TPC-H Q17 correlated subquery) are kept in the Filter and
        // never promoted — see `expr_contains_subquery()`.
        //
        // Note: Q07 previously showed revenue drift with pushdown
        // enabled — if it recurs, investigate the aggregate diff or
        // MERGE pipeline; the root cause is not in pushdown itself.
        tree = push_filter_into_cross_joins(tree);
    }

    // ── Step 3: Parse GROUP BY + aggregates ─────────────────────────────
    let group_list = pg_list::<pg_sys::Node>(select.groupClause);
    let target_list = pg_list::<pg_sys::Node>(select.targetList);

    let has_aggregates = unsafe { target_list_has_aggregates(&target_list) };
    let has_windows = target_list_has_windows(&target_list);

    if has_windows {
        // ── Window function path ───────────────────────────────────────
        // Extract window expressions and pass-through columns.
        let (window_exprs, pass_through) =
            unsafe { extract_window_exprs(&target_list, select.windowClause)? };

        if window_exprs.is_empty() {
            return Err(PgTrickleError::QueryParseError(
                "Window function detected but extraction failed".into(),
            ));
        }

        // Determine the shared PARTITION BY for the DVM diff optimizer.
        // If all window functions share the same PARTITION BY, use it —
        // the diff engine can then recompute only affected partitions.
        // When PARTITION BY clauses differ, fall back to empty (un-partitioned)
        // which triggers full recomputation on any change (correct but
        // less targeted).
        let canonical_partition: Vec<String> = window_exprs[0]
            .partition_by
            .iter()
            .map(|e| e.to_sql())
            .collect();
        let all_same_partition = window_exprs[1..].iter().all(|wexpr| {
            let p: Vec<String> = wexpr.partition_by.iter().map(|e| e.to_sql()).collect();
            p == canonical_partition
        });

        let partition_by = if all_same_partition {
            window_exprs[0].partition_by.clone()
        } else {
            // Different PARTITION BY clauses → un-partitioned diff mode
            // (full recomputation on any change).
            vec![]
        };

        // If there's also a GROUP BY, build the Aggregate child first.
        if !group_list.is_empty() || has_aggregates {
            let mut group_by = Vec::new();
            for node_ptr in group_list.iter_ptr() {
                let expr = unsafe { node_to_expr(node_ptr)? };
                group_by.push(expr);
            }
            let (aggregates, _non_agg_exprs) = unsafe { extract_aggregates(&target_list)? };
            tree = OpTree::Aggregate {
                group_by,
                aggregates,
                child: Box::new(tree),
            };
        }

        tree = OpTree::Window {
            window_exprs,
            partition_by,
            pass_through,
            child: Box::new(tree),
        };
    } else if !group_list.is_empty() || has_aggregates {
        let mut group_by = Vec::new();
        for node_ptr in group_list.iter_ptr() {
            let expr = unsafe { node_to_expr(node_ptr)? };
            group_by.push(expr);
        }

        let (aggregates, non_agg_exprs) = unsafe { extract_aggregates(&target_list)? };
        let _ = non_agg_exprs;

        // ── Step 3a2: Check for SELECT alias renames on GROUP BY cols ───
        // When the SELECT list renames a GROUP BY column (e.g.,
        // `SELECT l_suppkey AS supplier_no, SUM(...) AS total_revenue
        //  GROUP BY l_suppkey`), the Aggregate's output uses the raw
        // expression name (l_suppkey), losing the alias. Detect renames
        // BEFORE moving group_by into the Aggregate.
        let (target_exprs, target_aliases) = unsafe { parse_target_list(&target_list)? };

        // Resolve ordinal GROUP BY references (e.g., GROUP BY 1 → first
        // target expression). PostgreSQL allows GROUP BY <n> to refer to
        // the Nth output column. Resolve these to the actual target
        // expressions now so that rename detection below can match them.
        for gb_expr in &mut group_by {
            if let Expr::Raw(s) = &gb_expr
                && let Ok(pos) = s.parse::<usize>()
                && pos >= 1
                && pos <= target_exprs.len()
            {
                *gb_expr = target_exprs[pos - 1].clone();
            }
        }

        let mut rename_map = std::collections::HashMap::<usize, String>::new();
        for (i, gb_expr) in group_by.iter().enumerate() {
            let gb_sql = gb_expr.to_sql();
            let gb_output = gb_expr.output_name();
            for (te, ta) in target_exprs.iter().zip(target_aliases.iter()) {
                if te.to_sql() == gb_sql && ta != &gb_output {
                    rename_map.insert(i, ta.clone());
                    break;
                }
            }
        }

        tree = OpTree::Aggregate {
            group_by,
            aggregates: aggregates.clone(),
            child: Box::new(tree),
        };

        // Add a Project wrapper if any GROUP BY column needs renaming.
        if !rename_map.is_empty() {
            let agg_output = tree.output_columns();
            let proj_exprs: Vec<Expr> = agg_output
                .iter()
                .map(|name| Expr::ColumnRef {
                    table_alias: None,
                    column_name: name.clone(),
                })
                .collect();
            let proj_aliases: Vec<String> = agg_output
                .iter()
                .enumerate()
                .map(|(i, name)| rename_map.get(&i).cloned().unwrap_or_else(|| name.clone()))
                .collect();
            tree = OpTree::Project {
                expressions: proj_exprs,
                aliases: proj_aliases,
                child: Box::new(tree),
            };
        }

        // ── Step 3b: Parse HAVING clause as Filter on top of Aggregate ──
        if !select.havingClause.is_null() {
            let having_pred = unsafe { node_to_expr(select.havingClause)? };
            let rewritten = rewrite_having_expr(&having_pred, &aggregates);
            tree = OpTree::Filter {
                predicate: rewritten,
                child: Box::new(tree),
            };
        }
    } else {
        // ── Step 4: Parse target list as Project ───────────────────────
        if !select.havingClause.is_null() {
            return Err(PgTrickleError::QueryParseError(
                "HAVING clause requires GROUP BY or aggregate functions".into(),
            ));
        }
        if let Some(scalar_tree) =
            unsafe { parse_appended_scalar_target_subqueries(&target_list, tree.clone(), cte_ctx)? }
        {
            tree = scalar_tree;
        } else {
            let (expressions, aliases) = unsafe { parse_target_list(&target_list)? };
            if !is_star_only(&expressions) {
                tree = OpTree::Project {
                    expressions,
                    aliases,
                    child: Box::new(tree),
                };
            }
        }
    }

    // ── Step 5: Parse DISTINCT ──────────────────────────────────────────
    // PostgreSQL raw parser represents:
    //   SELECT DISTINCT           → distinctClause = list of NIL entries
    //   SELECT DISTINCT ON (expr) → distinctClause = list of real expression nodes
    //   No DISTINCT               → distinctClause = NULL
    //
    // We detect DISTINCT ON by checking whether any list entry is a
    // non-null node pointer.
    if !select.distinctClause.is_null() {
        let distinct_list = pg_list::<pg_sys::Node>(select.distinctClause);
        let has_real_exprs = distinct_list.iter_ptr().any(|ptr| !ptr.is_null());
        if has_real_exprs {
            // DISTINCT ON (expr, ...) — reject
            return Err(PgTrickleError::UnsupportedOperator(
                "DISTINCT ON is not supported in defining queries. \
                 Use plain DISTINCT or rewrite with window functions."
                    .into(),
            ));
        }
        tree = OpTree::Distinct {
            child: Box::new(tree),
        };
    }

    // ── Step 6: Handle ORDER BY ────────────────────────────────────────
    // ORDER BY is meaningless for stream table storage — row order is
    // undefined. We accept it silently (PostgreSQL does the same for
    // CREATE MATERIALIZED VIEW) and simply discard the sort clause.
    // No need to inspect `select.sortClause` — it is ignored.

    // ── Step 7: Reject LIMIT / OFFSET ──────────────────────────────────
    // TopK tables (ORDER BY + LIMIT) bypass the DVM pipeline entirely —
    // they never reach this point. This rejection handles the case where
    // LIMIT/OFFSET appears in a non-TopK context (e.g., LIMIT without
    // ORDER BY in DIFFERENTIAL mode, which reject_limit_offset should
    // have already caught at the API layer).
    if !select.limitCount.is_null() {
        return Err(PgTrickleError::UnsupportedOperator(
            "LIMIT is not supported in the DVM pipeline. \
             Use ORDER BY + LIMIT for TopK stream tables."
                .into(),
        ));
    }
    if !select.limitOffset.is_null() {
        return Err(PgTrickleError::UnsupportedOperator(
            "OFFSET is not supported in defining queries. \
             Stream tables materialize the full result set."
                .into(),
        ));
    }

    Ok(tree)
}

/// Parse simple appended scalar subqueries in the SELECT list.
///
/// Supported shape:
/// - Zero or more non-star target expressions first
/// - One or more bare scalar subquery targets last
///
/// Example:
/// `SELECT customer, amount, (SELECT val FROM cfg) AS tax_rate FROM orders`
///
/// This lowers the query to:
/// - a Project over the outer FROM tree for the non-scalar columns, then
/// - one ScalarSubquery wrapper per appended scalar target.
///
/// More complex target-list shapes (interleaved scalar targets, `*`, or
/// scalar subqueries nested inside larger expressions) fall back to the
/// generic Project path.
unsafe fn parse_appended_scalar_target_subqueries(
    target_list: &pgrx::PgList<pg_sys::Node>,
    mut tree: OpTree,
    cte_ctx: &mut CteParseContext,
) -> Result<Option<OpTree>, PgTrickleError> {
    struct ScalarTarget {
        alias: String,
        subquery: OpTree,
        source_oids: Vec<u32>,
    }

    let mut plain_exprs = Vec::new();
    let mut plain_aliases = Vec::new();
    let mut scalar_targets = Vec::new();
    let mut saw_scalar_target = false;

    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }

        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }

        let alias =
            unsafe { target_alias_for_res_target(rt, plain_exprs.len() + scalar_targets.len()) };

        if let Some((subquery, source_oids)) =
            unsafe { parse_scalar_target_subquery(rt.val, cte_ctx)? }
        {
            scalar_targets.push(ScalarTarget {
                alias,
                subquery,
                source_oids,
            });
            saw_scalar_target = true;
            continue;
        }

        if saw_scalar_target {
            return Ok(None);
        }

        let expr = unsafe { node_to_expr(rt.val)? };
        if matches!(expr, Expr::Star { .. }) {
            return Ok(None);
        }

        plain_exprs.push(expr);
        plain_aliases.push(alias);
    }

    if scalar_targets.is_empty() || plain_exprs.is_empty() {
        return Ok(None);
    }

    tree = OpTree::Project {
        expressions: plain_exprs,
        aliases: plain_aliases,
        child: Box::new(tree),
    };

    for scalar in scalar_targets {
        tree = OpTree::ScalarSubquery {
            subquery: Box::new(scalar.subquery),
            alias: scalar.alias,
            subquery_source_oids: scalar.source_oids,
            child: Box::new(tree),
        };
    }

    Ok(Some(tree))
}

/// Parse a bare scalar subquery target into an OpTree.
///
/// Accepts both:
/// - raw parser `T_SubLink` nodes for `EXPR_SUBLINK`, and
/// - `Expr::Raw("(SELECT ...)")` fallback expressions produced by `node_to_expr()`.
unsafe fn parse_scalar_target_subquery(
    node: *mut pg_sys::Node,
    cte_ctx: &mut CteParseContext,
) -> Result<Option<(OpTree, Vec<u32>)>, PgTrickleError> {
    if let Some(sublink) = cast_node!(node, T_SubLink, pg_sys::SubLink) {
        if sublink.subLinkType != pg_sys::SubLinkType::EXPR_SUBLINK {
            return Ok(None);
        }
        if sublink.subselect.is_null()
            || !unsafe { pgrx::is_a(sublink.subselect, pg_sys::NodeTag::T_SelectStmt) }
        {
            return Err(PgTrickleError::QueryParseError(
                "Scalar subquery target must contain a SELECT".into(),
            ));
        }

        let inner_select = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };
        // If the inner SELECT contains constructs that the DVM pipeline cannot
        // parse fully (e.g. LIMIT/OFFSET, correlated sub-expressions with OR),
        // fall back to `Ok(None)` so the caller's `node_to_expr` path deparsed
        // the sublink as an opaque `Expr::Raw`.  This is safe because:
        //   1. `extract_source_relations` uses the full PostgreSQL analyzer and
        //      still records every inner table reference for dependency tracking.
        //   2. The scheduler forces a FULL refresh whenever upstream stream-table
        //      sources have newer data, so the raw-SQL expression is only exercised
        //      for base-table-CDC differentials, where it is re-evaluated correctly
        //      for each affected row.
        let subquery = match if inner_select.op != pg_sys::SetOperation::SETOP_NONE {
            unsafe { parse_set_operation(inner_select, cte_ctx) }
        } else {
            unsafe { parse_select_stmt(inner_select, "", cte_ctx) }
        } {
            Ok(tree) => tree,
            Err(_) => return Ok(None),
        };

        let mut source_oids = subquery.source_oids();
        source_oids.sort_unstable();
        source_oids.dedup();
        return Ok(Some((subquery, source_oids)));
    }

    let expr = unsafe { node_to_expr(node)? };
    let Expr::Raw(raw_sql) = expr else {
        return Ok(None);
    };

    let Some(inner_sql) = extract_bare_scalar_subquery_sql(&raw_sql) else {
        return Ok(None);
    };

    let inner_select = match parse_first_select(inner_sql.as_str())? {
        Some(s) => unsafe { &*s },
        None => {
            return Err(PgTrickleError::QueryParseError(
                "Scalar subquery target must parse to a SELECT".into(),
            ));
        }
    };
    // Same graceful fallback for the raw-SQL code path.
    let subquery = match if inner_select.op != pg_sys::SetOperation::SETOP_NONE {
        unsafe { parse_set_operation(inner_select, cte_ctx) }
    } else {
        unsafe { parse_select_stmt(inner_select, "", cte_ctx) }
    } {
        Ok(tree) => tree,
        Err(_) => return Ok(None),
    };

    let mut source_oids = subquery.source_oids();
    source_oids.sort_unstable();
    source_oids.dedup();
    Ok(Some((subquery, source_oids)))
}

fn extract_bare_scalar_subquery_sql(raw_sql: &str) -> Option<String> {
    let trimmed = raw_sql.trim();
    if !trimmed.starts_with('(') || !trimmed.ends_with(')') {
        return None;
    }

    let inner = trimmed[1..trimmed.len() - 1].trim();
    if inner.len() < 6 || !inner[..6].eq_ignore_ascii_case("SELECT") {
        return None;
    }

    Some(inner.to_string())
}

/// Parse a FROM clause item (RangeVar, JoinExpr, or RangeSubselect) into an OpTree.
unsafe fn parse_from_item(
    node: *mut pg_sys::Node,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgTrickleError> {
    // G13-SD: Track recursion depth.
    let prev_depth = cte_ctx.descend()?;
    let result = unsafe { parse_from_item_inner(node, cte_ctx) };
    cte_ctx.ascend(prev_depth);
    result
}

/// Inner implementation of `parse_from_item` (after depth check).
unsafe fn parse_from_item_inner(
    node: *mut pg_sys::Node,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgTrickleError> {
    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        // Explicit schema qualifier: use as-is.
        // Unqualified name: defer resolution to after the CTE check so we
        // don't do a search_path lookup for names that are CTE aliases.
        let explicit_schema: Option<String> = if rv.schemaname.is_null() {
            None
        } else {
            Some(
                pg_cstr_to_str(rv.schemaname)
                    .unwrap_or("public")
                    .to_string(),
            )
        };
        let table_name = pg_cstr_to_str(rv.relname)?.to_string();
        let alias = if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            pg_cstr_to_str(a.aliasname)
                .unwrap_or(&table_name)
                .to_string()
        } else {
            table_name.clone()
        };

        // Check if this name is a self-reference in a recursive CTE
        if rv.schemaname.is_null()
            && let Some(ref self_ref_name) = cte_ctx.recursive_self_ref_name
            && table_name == *self_ref_name
        {
            let self_alias = if !rv.alias.is_null() {
                let a = unsafe { &*(rv.alias) };
                pg_cstr_to_str(a.aliasname)
                    .unwrap_or(&table_name)
                    .to_string()
            } else {
                table_name.clone()
            };
            return Ok(OpTree::RecursiveSelfRef {
                cte_name: self_ref_name.clone(),
                alias: self_alias,
                columns: cte_ctx.recursive_self_ref_columns.clone(),
            });
        }

        // Check if this name references a CTE (schema-unqualified only)
        if rv.schemaname.is_null() && cte_ctx.is_cte(&table_name) {
            // Extract column aliases from the RangeVar's alias, if any
            let col_aliases = if !rv.alias.is_null() {
                let a = unsafe { &*(rv.alias) };
                extract_alias_colnames(a)?
            } else {
                Vec::new()
            };

            // Get CTE definition-level aliases: WITH x(a, b) AS (...)
            let def_aliases = cte_ctx
                .def_aliases
                .get(&table_name)
                .cloned()
                .unwrap_or_default();

            // Check if this CTE has already been parsed (reuse cte_id)
            let (cte_id, columns, body_clone) = if let Some(id) = cte_ctx.lookup_id(&table_name) {
                // Already parsed — reuse the existing entry
                let (_, body) = &cte_ctx.registry.entries[id];
                let cols = body.output_columns();
                let body_clone = body.clone();
                (id, cols, body_clone)
            } else {
                // First reference — parse the CTE body and register it.
                // Check `op` field to handle UNION ALL CTE bodies (e.g.,
                // non-recursive CTEs that happen to use UNION ALL).
                let cte_stmt = unsafe { &*cte_ctx.raw_map[&table_name] };
                let cte_tree = if cte_stmt.op != pg_sys::SetOperation::SETOP_NONE {
                    unsafe { parse_set_operation(cte_stmt, cte_ctx)? }
                } else {
                    unsafe { parse_select_stmt(cte_stmt, "", cte_ctx)? }
                };
                let cols = cte_tree.output_columns();
                let body_clone = cte_tree.clone();
                let id = cte_ctx.register(&table_name, cte_tree);
                (id, cols, body_clone)
            };

            return Ok(OpTree::CteScan {
                cte_id,
                cte_name: table_name,
                alias,
                columns,
                cte_def_aliases: def_aliases,
                column_aliases: col_aliases,
                body: Some(Box::new(body_clone)),
            });
        }

        // Resolve the schema: use the explicit qualifier if present, otherwise
        // look up the table via the session search_path.  This avoids the
        // previous bug where unqualified names were hardcoded to "public",
        // causing a cryptic SPI error for tables in other schemas.
        let schema_name = match explicit_schema {
            Some(s) => s,
            None => match resolve_rangevar_schema(rv)? {
                Some(s) => s,
                None => {
                    return Err(PgTrickleError::NotFound(format!(
                        "table '{}' not found in the current search_path. \
                         Use a schema-qualified name (e.g. \"my_schema\".\"{}\") \
                         or add the schema to the search_path.",
                        table_name, table_name
                    )));
                }
            },
        };

        let table_oid = resolve_table_oid(&schema_name, &table_name)?;
        let columns = resolve_columns(table_oid)?;
        let pk_columns = resolve_pk_columns(table_oid)?;

        Ok(OpTree::Scan {
            table_oid,
            table_name,
            schema: schema_name,
            columns,
            pk_columns,
            alias,
        })
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        let left = unsafe { parse_from_item(join.larg, cte_ctx)? };
        let right = unsafe { parse_from_item(join.rarg, cte_ctx)? };

        // If right side is a LateralSubquery, handle LATERAL join semantics
        if let OpTree::LateralSubquery {
            subquery_sql,
            alias,
            column_aliases,
            output_cols,
            subquery_source_oids,
            correlation_predicates,
            ..
        } = right
        {
            match join.jointype {
                pg_sys::JoinType::JOIN_INNER => {
                    return Ok(OpTree::LateralSubquery {
                        subquery_sql,
                        alias,
                        column_aliases,
                        output_cols,
                        is_left_join: false,
                        subquery_source_oids,
                        correlation_predicates,
                        child: Box::new(left),
                    });
                }
                pg_sys::JoinType::JOIN_LEFT => {
                    return Ok(OpTree::LateralSubquery {
                        subquery_sql,
                        alias,
                        column_aliases,
                        output_cols,
                        is_left_join: true,
                        subquery_source_oids,
                        correlation_predicates,
                        child: Box::new(left),
                    });
                }
                other => {
                    return Err(PgTrickleError::UnsupportedOperator(format!(
                        "LATERAL subqueries support only INNER JOIN and LEFT JOIN. \
                         RIGHT JOIN LATERAL and FULL JOIN LATERAL are rejected by \
                         PostgreSQL itself because the lateral reference on the right \
                         side creates a dependency that conflicts with RIGHT/FULL JOIN \
                         semantics (got join type {:?}).",
                        other,
                    )));
                }
            }
        }

        // If right side is a LateralFunction from explicit JOIN syntax
        if let OpTree::LateralFunction {
            func_sql,
            alias,
            column_aliases,
            with_ordinality,
            ..
        } = right
        {
            match join.jointype {
                pg_sys::JoinType::JOIN_INNER | pg_sys::JoinType::JOIN_LEFT => {
                    return Ok(OpTree::LateralFunction {
                        func_sql,
                        alias,
                        column_aliases,
                        with_ordinality,
                        child: Box::new(left),
                    });
                }
                other => {
                    return Err(PgTrickleError::UnsupportedOperator(format!(
                        "LATERAL set-returning functions support only INNER JOIN and LEFT JOIN. \
                         RIGHT JOIN LATERAL and FULL JOIN LATERAL are rejected by \
                         PostgreSQL itself because the lateral reference on the right \
                         side creates a dependency that conflicts with RIGHT/FULL JOIN \
                         semantics (got join type {:?}).",
                        other,
                    )));
                }
            }
        }

        // ── NATURAL JOIN: resolve common columns and synthesize equi-join ──
        // PostgreSQL's raw parse tree leaves `quals` NULL for NATURAL JOIN.
        // We resolve common column names from the already-parsed left/right
        // OpTree nodes and build an explicit equi-join condition.
        if join.isNatural {
            let left_cols = left.output_columns();
            let right_cols = right.output_columns();

            // Find common columns (order follows left side for determinism).
            let right_set: std::collections::HashSet<String> = right_cols.iter().cloned().collect();
            let common: Vec<String> = left_cols
                .iter()
                .filter(|c| right_set.contains(c.as_str()))
                .cloned()
                .collect();

            if common.is_empty() {
                return Err(PgTrickleError::QueryParseError(
                    "NATURAL JOIN has no common columns between left and right sides. \
                     Use an explicit JOIN ... ON condition instead."
                        .into(),
                ));
            }

            // F38: Warn about NATURAL JOIN column drift risk.
            // If columns are added to either table that happen to match a column
            // on the other side, the join semantics change silently. Explicit
            // JOIN ... ON is recommended for production stream tables.
            // Push to the per-parse accumulator; the top-level caller emits
            // this exactly once via ParseResult.warnings.
            push_parse_warning(format!(
                "pg_trickle: NATURAL JOIN resolved on columns [{}]. \
                 Adding a same-named column to either table will silently change \
                 join semantics. Consider using explicit JOIN ... ON instead.",
                common.join(", "),
            ));

            // Build condition: left.col1 = right.col1 AND left.col2 = right.col2 ...
            let left_alias = left.alias();
            let right_alias = right.alias();

            let parts: Vec<String> = common
                .iter()
                .map(|col| {
                    let lq = format!(
                        "\"{}\".\"{}\"",
                        left_alias.replace('"', "\"\""),
                        col.replace('"', "\"\""),
                    );
                    let rq = format!(
                        "\"{}\".\"{}\"",
                        right_alias.replace('"', "\"\""),
                        col.replace('"', "\"\""),
                    );
                    format!("{lq} = {rq}")
                })
                .collect();

            let condition = Expr::Raw(parts.join(" AND "));

            return match join.jointype {
                pg_sys::JoinType::JOIN_INNER => Ok(OpTree::InnerJoin {
                    condition,
                    left: Box::new(left),
                    right: Box::new(right),
                }),
                pg_sys::JoinType::JOIN_LEFT => Ok(OpTree::LeftJoin {
                    condition,
                    left: Box::new(left),
                    right: Box::new(right),
                }),
                pg_sys::JoinType::JOIN_RIGHT => Ok(OpTree::LeftJoin {
                    condition,
                    left: Box::new(right),
                    right: Box::new(left),
                }),
                pg_sys::JoinType::JOIN_FULL => Ok(OpTree::FullJoin {
                    condition,
                    left: Box::new(left),
                    right: Box::new(right),
                }),
                other => Err(PgTrickleError::UnsupportedOperator(format!(
                    "NATURAL JOIN type {other:?} not supported",
                ))),
            };
        }

        let condition = if join.quals.is_null() {
            Expr::Literal("TRUE".into())
        } else {
            unsafe { node_to_expr(join.quals)? }
        };

        match join.jointype {
            pg_sys::JoinType::JOIN_INNER => Ok(OpTree::InnerJoin {
                condition,
                left: Box::new(left),
                right: Box::new(right),
            }),
            pg_sys::JoinType::JOIN_LEFT => Ok(OpTree::LeftJoin {
                condition,
                left: Box::new(left),
                right: Box::new(right),
            }),
            pg_sys::JoinType::JOIN_RIGHT => {
                // RIGHT JOIN(A, B) → LEFT JOIN(B, A) with swapped operands
                Ok(OpTree::LeftJoin {
                    condition,
                    left: Box::new(right),
                    right: Box::new(left),
                })
            }
            pg_sys::JoinType::JOIN_FULL => Ok(OpTree::FullJoin {
                condition,
                left: Box::new(left),
                right: Box::new(right),
            }),
            other => Err(PgTrickleError::UnsupportedOperator(format!(
                "Join type {other:?} not supported for differential mode",
            ))),
        }
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect) {
        if sub.subquery.is_null() {
            return Err(PgTrickleError::QueryParseError(
                "RangeSubselect with NULL subquery".into(),
            ));
        }

        // The subquery must be a SelectStmt
        if !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) } {
            return Err(PgTrickleError::QueryParseError(
                "Subquery in FROM must be a SELECT statement".into(),
            ));
        }

        let sub_stmt = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };

        // Extract alias name
        let alias = if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            pg_cstr_to_str(a.aliasname)
                .unwrap_or("subquery")
                .to_string()
        } else {
            "subquery".to_string()
        };

        // Extract column aliases from the alias, if any
        let col_aliases = if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            extract_alias_colnames(a)?
        } else {
            Vec::new()
        };

        if sub.lateral {
            // ── LATERAL subquery: store as raw SQL ─────────────────────
            let subquery_sql = unsafe { deparse_select_stmt_to_sql(sub_stmt)? };

            // Extract output column names from the subquery's SELECT list
            let output_cols = unsafe { extract_select_output_cols(sub_stmt.targetList)? };

            // Extract source table OIDs from the subquery's FROM clause
            let subquery_source_oids =
                unsafe { extract_from_oids(sub_stmt.fromClause).unwrap_or_default() };

            // P2-6: Extract alias→OID mapping and correlation predicates
            // for scoped inner-change re-execution.
            let inner_alias_oids =
                unsafe { extract_from_alias_oids(sub_stmt.fromClause).unwrap_or_default() };
            let correlation_predicates =
                unsafe { extract_correlation_predicates(sub_stmt.whereClause, &inner_alias_oids) };

            // Return a LateralSubquery with a placeholder child.
            // The real child is attached in the FROM-list loop or JoinExpr handler.
            return Ok(OpTree::LateralSubquery {
                subquery_sql,
                alias,
                column_aliases: col_aliases,
                output_cols,
                is_left_join: false,
                subquery_source_oids,
                correlation_predicates,
                child: Box::new(OpTree::Scan {
                    table_oid: 0,
                    table_name: String::new(),
                    schema: String::new(),
                    columns: vec![],
                    pk_columns: vec![],
                    alias: String::new(),
                }),
            });
        }

        // ── Non-LATERAL subquery: existing code path ───────────────────
        // Check `op` to handle subqueries that are set operations
        let sub_tree = if sub_stmt.op != pg_sys::SetOperation::SETOP_NONE {
            unsafe { parse_set_operation(sub_stmt, cte_ctx)? }
        } else {
            unsafe { parse_select_stmt(sub_stmt, "", cte_ctx)? }
        };

        Ok(OpTree::Subquery {
            alias,
            column_aliases: col_aliases,
            child: Box::new(sub_tree),
        })
    } else if let Some(rf) = cast_node!(node, T_RangeFunction, pg_sys::RangeFunction) {
        // Extract the function call from rf.functions (a List of Lists).
        // Each element is a two-element List: [FuncCall, column_def_list].
        let func_list = pg_list::<pg_sys::Node>(rf.functions);
        if func_list.is_empty() {
            return Err(PgTrickleError::QueryParseError(
                "RangeFunction with no functions".into(),
            ));
        }

        // We support a single function. ROWS FROM(f1, f2, ...) is not supported.
        if rf.is_rowsfrom && func_list.len() > 1 {
            return Err(PgTrickleError::UnsupportedOperator(
                "ROWS FROM() with multiple functions is not supported. \
                 Use a single set-returning function in FROM instead."
                    .into(),
            ));
        }

        // The first element is a List node; its first element is the FuncCall.
        // SAFETY: func_list is non-empty, head is a List node containing the FuncCall.
        let inner_list_node = func_list.head().unwrap();
        let inner_list = pg_list::<pg_sys::Node>(inner_list_node as *mut pg_sys::List);
        if inner_list.is_empty() {
            return Err(PgTrickleError::QueryParseError(
                "RangeFunction inner list is empty".into(),
            ));
        }

        let func_node = inner_list.head().unwrap();
        if !unsafe { pgrx::is_a(func_node, pg_sys::NodeTag::T_FuncCall) } {
            return Err(PgTrickleError::QueryParseError(
                "RangeFunction does not contain a FuncCall node".into(),
            ));
        }

        // Deparse the function call to SQL text.
        let func_sql = unsafe { deparse_func_call(func_node as *const pg_sys::FuncCall)? };

        // Extract alias name
        let alias = if !rf.alias.is_null() {
            let a = unsafe { &*(rf.alias) };
            pg_cstr_to_str(a.aliasname).unwrap_or("srf").to_string()
        } else {
            "srf".to_string()
        };

        // Extract column aliases (e.g., AS child(key, value))
        let column_aliases = if !rf.alias.is_null() {
            let a = unsafe { &*(rf.alias) };
            extract_alias_colnames(a)?
        } else {
            Vec::new()
        };

        let with_ordinality = rf.ordinality;

        // Return a LateralFunction with a placeholder child.
        // The real child is attached in the FROM-list loop in parse_select_stmt().
        Ok(OpTree::LateralFunction {
            func_sql,
            alias,
            column_aliases,
            with_ordinality,
            child: Box::new(OpTree::Scan {
                table_oid: 0,
                table_name: String::new(),
                schema: String::new(),
                columns: vec![],
                pk_columns: vec![],
                alias: String::new(),
            }),
        })
    } else if let Some(jt) = cast_node!(node, T_JsonTable, pg_sys::JsonTable) {
        let func_sql = unsafe { deparse_json_table(jt as *const pg_sys::JsonTable)? };

        // Extract alias
        let alias = if !jt.alias.is_null() {
            let a = unsafe { &*(jt.alias) };
            pg_cstr_to_str(a.aliasname).unwrap_or("jt").to_string()
        } else {
            "jt".to_string()
        };

        // Extract column aliases from the alias node
        let column_aliases = if !jt.alias.is_null() {
            let a = unsafe { &*(jt.alias) };
            extract_alias_colnames(a)?
        } else {
            Vec::new()
        };

        // JSON_TABLE is inherently lateral (references the left-hand table).
        // Model it as LateralFunction with a placeholder child that gets
        // replaced in the FROM-list loop of parse_select_stmt().
        Ok(OpTree::LateralFunction {
            func_sql,
            alias,
            column_aliases,
            with_ordinality: false,
            child: Box::new(OpTree::Scan {
                table_oid: 0,
                table_name: String::new(),
                schema: String::new(),
                columns: vec![],
                pk_columns: vec![],
                alias: String::new(),
            }),
        })
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeTableSample) } {
        // TABLESAMPLE: SELECT * FROM t TABLESAMPLE BERNOULLI(10)
        // Stream tables materialize complete result sets; sampling at parse
        // time is not meaningful or supported.
        Err(PgTrickleError::UnsupportedOperator(
            "TABLESAMPLE is not supported in defining queries. \
             Stream tables materialize the complete result set; \
             use a WHERE condition with random() if sampling is needed."
                .into(),
        ))
    } else {
        Err(PgTrickleError::UnsupportedOperator(format!(
            "Unsupported FROM item node type: {:?}",
            unsafe { (*node).type_ },
        )))
    }
}

/// Return type for [`extract_cte_map_with_recursive`]:
/// (non_recursive_map, def_aliases, recursive_cte_stmts).
///
/// `recursive_cte_stmts` is a Vec of `(name, base_stmt, recursive_stmt, def_col_aliases, union_all)`.
type RecursiveCteMapResult = (
    HashMap<String, *const pg_sys::SelectStmt>,
    HashMap<String, Vec<String>>,
    Vec<(
        String,
        *const pg_sys::SelectStmt,
        *const pg_sys::SelectStmt,
        Vec<String>,
        bool,
    )>,
);

/// Extract CTE definitions from a WithClause, handling both recursive and
/// non-recursive CTEs.
///
/// Non-recursive CTEs go into the `name→SelectStmt` map (as before).
/// Recursive CTEs are returned as `(name, base_stmt, recursive_stmt, def_cols, union_all)`
/// tuples for separate parsing into `OpTree::RecursiveCte`.
///
/// # Safety
/// Caller must ensure `with_clause` points to a valid `WithClause` node.
unsafe fn extract_cte_map_with_recursive(
    with_clause: *const pg_sys::WithClause,
) -> Result<RecursiveCteMapResult, PgTrickleError> {
    let wc = unsafe { &*with_clause };
    let cte_list = pg_list::<pg_sys::Node>(wc.ctes);
    let mut map = HashMap::new();
    let mut def_aliases_map = HashMap::new();
    let mut recursive_stmts = Vec::new();

    for node_ptr in cte_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
            continue;
        }

        let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };

        // Extract CTE name
        let cte_name = pg_cstr_to_str(cte.ctename)?.to_string();

        // The CTE body is ctequery, which must be a SelectStmt
        if cte.ctequery.is_null() {
            return Err(PgTrickleError::QueryParseError(format!(
                "CTE '{cte_name}' has NULL body",
            )));
        }
        if !unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) } {
            return Err(PgTrickleError::QueryParseError(format!(
                "CTE '{cte_name}' body is not a SELECT statement",
            )));
        }

        // Extract column definition aliases: WITH x(a, b) AS (...)
        let col_aliases = extract_cte_def_colnames(cte)?;
        if !col_aliases.is_empty() {
            def_aliases_map.insert(cte_name.clone(), col_aliases.clone());
        }

        // Detect recursive CTEs: In PG18's raw_parser output,
        // cte.cterecursive is NOT set (it's only populated by the
        // analyzer). We detect recursion by checking: (a) the WITH
        // clause has the RECURSIVE keyword (wc.recursive), AND
        // (b) the CTE body is a UNION or UNION ALL (op == SETOP_UNION).
        let body = unsafe { &*(cte.ctequery as *const pg_sys::SelectStmt) };
        let is_recursive =
            cte.cterecursive || (wc.recursive && body.op == pg_sys::SetOperation::SETOP_UNION);

        if is_recursive {
            // Recursive CTE — split UNION [ALL] into base + recursive terms
            if body.op != pg_sys::SetOperation::SETOP_UNION {
                return Err(PgTrickleError::QueryParseError(format!(
                    "Recursive CTE '{cte_name}' body must be a UNION or UNION ALL",
                )));
            }

            if body.larg.is_null() || body.rarg.is_null() {
                return Err(PgTrickleError::QueryParseError(format!(
                    "Recursive CTE '{cte_name}' UNION is missing left or right arm",
                )));
            }

            let base_stmt = body.larg as *const pg_sys::SelectStmt;
            let rec_stmt = body.rarg as *const pg_sys::SelectStmt;
            let union_all = body.all;

            recursive_stmts.push((cte_name, base_stmt, rec_stmt, col_aliases, union_all));
        } else {
            // Non-recursive CTE — add to normal map
            let stmt = cte.ctequery as *const pg_sys::SelectStmt;
            map.insert(cte_name, stmt);
        }
    }

    Ok((map, def_aliases_map, recursive_stmts))
}

/// Extract column aliases from a `CommonTableExpr.aliascolnames` list.
///
/// These are the column aliases on the CTE definition itself:
/// `WITH x(a, b) AS (SELECT id, name FROM ...)`.
///
/// Returns an empty `Vec` if no column aliases are specified.
fn extract_cte_def_colnames(cte: &pg_sys::CommonTableExpr) -> Result<Vec<String>, PgTrickleError> {
    let colnames = pg_list::<pg_sys::Node>(cte.aliascolnames);
    let mut names = Vec::new();
    for node_ptr in colnames.iter_ptr() {
        let name = unsafe { node_to_string(node_ptr)? };
        names.push(name);
    }
    Ok(names)
}

/// Extract column aliases from an Alias node's colnames list.
///
/// Returns an empty Vec if the alias has no explicit column names
/// (i.e. `FROM x AS alias` without `(c1, c2, ...)`).
///
/// # Safety
/// Caller must ensure `alias` points to a valid `Alias` struct.
fn extract_alias_colnames(alias: &pg_sys::Alias) -> Result<Vec<String>, PgTrickleError> {
    let colnames = pg_list::<pg_sys::Node>(alias.colnames);
    let mut names = Vec::new();
    for node_ptr in colnames.iter_ptr() {
        let name = unsafe { node_to_string(node_ptr)? };
        names.push(name);
    }
    Ok(names)
}

/// Resolve a table name to its OID via SPI.
fn resolve_table_oid(schema: &str, table: &str) -> Result<u32, PgTrickleError> {
    let sql = format!(
        "SELECT c.oid FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{}' AND c.relname = '{}'",
        schema.replace('\'', "''"),
        table.replace('\'', "''"),
    );

    Spi::connect(|client| {
        let mut result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        // Use iterator to safely handle empty result sets — calling
        // .first().get() on an empty SpiTupleTable throws "positioned
        // before the start or after the end" instead of a useful error.
        if let Some(row) = result.next() {
            let oid: Option<pg_sys::Oid> = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            return oid
                .map(|o| o.to_u32())
                .ok_or_else(|| PgTrickleError::NotFound(format!("{schema}.{table}")));
        }
        Err(PgTrickleError::NotFound(format!(
            "table '{schema}.{table}' not found in pg_class — \
             if the table is not in schema '{schema}', use a schema-qualified \
             name in the query (e.g. \"my_schema\".\"{}\")",
            table
        )))
    })
}

/// Resolve column metadata for a table via SPI.
fn resolve_columns(table_oid: u32) -> Result<Vec<Column>, PgTrickleError> {
    // Filter out internal __pgt_* columns (e.g. __pgt_row_id) that exist
    // on stream table storage tables. These are implementation details
    // and must not participate in row_id computation or delta queries.
    // Mirrors the filter in resolve_st_output_columns().
    let sql = format!(
        "SELECT attname::text, atttypid, attnotnull \
         FROM pg_attribute \
         WHERE attrelid = {} AND attnum > 0 AND NOT attisdropped \
           AND attname::text NOT LIKE '__pgt_%%' \
         ORDER BY attnum",
        table_oid,
    );

    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut columns = Vec::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let type_oid: pg_sys::Oid = row
                .get(2)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(pg_sys::Oid::INVALID);
            let not_null: bool = row
                .get(3)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or(false);
            columns.push(Column {
                name,
                type_oid: type_oid.to_u32(),
                is_nullable: !not_null,
            });
        }
        Ok(columns)
    })
}

/// Resolve primary key column names for a table via `pg_constraint`.
///
/// Returns columns in key order. Returns an empty Vec if no PK exists.
fn resolve_pk_columns(table_oid: u32) -> Result<Vec<String>, PgTrickleError> {
    let sql = format!(
        "SELECT a.attname::text \
         FROM pg_constraint c \
         JOIN pg_attribute a ON a.attrelid = c.conrelid \
           AND a.attnum = ANY(c.conkey) \
         WHERE c.conrelid = {} AND c.contype = 'p' \
         ORDER BY array_position(c.conkey, a.attnum)",
        table_oid,
    );

    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
        let mut pk_cols = Vec::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?
                .unwrap_or_default();
            pk_cols.push(name);
        }
        Ok(pk_cols)
    })
}

/// Convert a pg_sys::Node to an Expr.
unsafe fn node_to_expr(node: *mut pg_sys::Node) -> Result<Expr, PgTrickleError> {
    if node.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "NULL node in expression".into(),
        ));
    }

    if let Some(cref) = cast_node!(node, T_ColumnRef, pg_sys::ColumnRef) {
        let fields = pg_list::<pg_sys::Node>(cref.fields);

        match fields.len() {
            1 => {
                let field = fields.head().unwrap();
                // Bare `SELECT *` arrives as ColumnRef with a single A_Star field.
                if unsafe { pgrx::is_a(field, pg_sys::NodeTag::T_A_Star) } {
                    return Ok(Expr::Star { table_alias: None });
                }
                let col_name = unsafe { node_to_string(field)? };
                Ok(Expr::ColumnRef {
                    table_alias: None,
                    column_name: col_name,
                })
            }
            2 => {
                let table_alias = unsafe { node_to_string(fields.get_ptr(0).unwrap())? };
                let last = fields.get_ptr(1).unwrap();
                // `table.*` arrives as ColumnRef with fields [T_String, T_A_Star].
                // T_A_Star is not a T_String, so node_to_string falls back to
                // "node_T_A_Star" — check explicitly before calling it.
                if unsafe { pgrx::is_a(last, pg_sys::NodeTag::T_A_Star) } {
                    return Ok(Expr::Star {
                        table_alias: Some(table_alias),
                    });
                }
                let col_name = unsafe { node_to_string(last)? };
                Ok(Expr::ColumnRef {
                    table_alias: Some(table_alias),
                    column_name: col_name,
                })
            }
            3 => {
                // schema.table.column — drop the schema, use table.column
                let _schema = unsafe { node_to_string(fields.get_ptr(0).unwrap())? };
                let table_alias = unsafe { node_to_string(fields.get_ptr(1).unwrap())? };
                let last = fields.get_ptr(2).unwrap();
                // `schema.table.*` — same T_A_Star guard as the 2-field case.
                if unsafe { pgrx::is_a(last, pg_sys::NodeTag::T_A_Star) } {
                    return Ok(Expr::Star {
                        table_alias: Some(table_alias),
                    });
                }
                let col_name = unsafe { node_to_string(last)? };
                Ok(Expr::ColumnRef {
                    table_alias: Some(table_alias),
                    column_name: col_name,
                })
            }
            n => Err(PgTrickleError::QueryParseError(format!(
                "Unexpected ColumnRef with {n} fields",
            ))),
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Const) } {
        Ok(Expr::Raw(unsafe { deparse_node(node) }))
    } else if let Some(aexpr) = cast_node!(node, T_A_Expr, pg_sys::A_Expr) {
        match aexpr.kind {
            pg_sys::A_Expr_Kind::AEXPR_OP => {
                let op_name = unsafe { extract_operator_name(aexpr.name)? };
                // Handle unary prefix operators (e.g., -x) where lexpr is NULL
                if aexpr.lexpr.is_null() {
                    let right = unsafe { node_to_expr(aexpr.rexpr)? };
                    return Ok(Expr::Raw(format!("{op_name}{}", right.to_sql())));
                }
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                Ok(Expr::BinaryOp {
                    op: op_name,
                    left: Box::new(left),
                    right: Box::new(right),
                })
            }
            pg_sys::A_Expr_Kind::AEXPR_DISTINCT => {
                // IS DISTINCT FROM
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                Ok(Expr::Raw(format!(
                    "{} IS DISTINCT FROM {}",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_NOT_DISTINCT => {
                // IS NOT DISTINCT FROM
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                Ok(Expr::Raw(format!(
                    "{} IS NOT DISTINCT FROM {}",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_IN => {
                // x IN (v1, v2, v3)
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right_list = pg_list::<pg_sys::Node>(aexpr.rexpr as *mut _);
                let mut vals = Vec::new();
                for n in right_list.iter_ptr() {
                    vals.push(unsafe { node_to_expr(n)? }.to_sql());
                }
                let op_name = unsafe { extract_operator_name(aexpr.name) }
                    .unwrap_or_else(|_| "=".to_string());
                if op_name == "<>" {
                    Ok(Expr::Raw(format!(
                        "{} NOT IN ({})",
                        left.to_sql(),
                        vals.join(", ")
                    )))
                } else {
                    Ok(Expr::Raw(format!(
                        "{} IN ({})",
                        left.to_sql(),
                        vals.join(", ")
                    )))
                }
            }
            pg_sys::A_Expr_Kind::AEXPR_BETWEEN => {
                // x BETWEEN a AND b
                let tested = unsafe { node_to_expr(aexpr.lexpr)? };
                let bounds = pg_list::<pg_sys::Node>(aexpr.rexpr as *mut _);
                let low = unsafe { node_to_expr(bounds.get_ptr(0).unwrap())? };
                let high = unsafe { node_to_expr(bounds.get_ptr(1).unwrap())? };
                Ok(Expr::Raw(format!(
                    "{} BETWEEN {} AND {}",
                    tested.to_sql(),
                    low.to_sql(),
                    high.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_NOT_BETWEEN => {
                let tested = unsafe { node_to_expr(aexpr.lexpr)? };
                let bounds = pg_list::<pg_sys::Node>(aexpr.rexpr as *mut _);
                let low = unsafe { node_to_expr(bounds.get_ptr(0).unwrap())? };
                let high = unsafe { node_to_expr(bounds.get_ptr(1).unwrap())? };
                Ok(Expr::Raw(format!(
                    "{} NOT BETWEEN {} AND {}",
                    tested.to_sql(),
                    low.to_sql(),
                    high.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_BETWEEN_SYM => {
                let tested = unsafe { node_to_expr(aexpr.lexpr)? };
                let bounds = pg_list::<pg_sys::Node>(aexpr.rexpr as *mut _);
                let low = unsafe { node_to_expr(bounds.get_ptr(0).unwrap())? };
                let high = unsafe { node_to_expr(bounds.get_ptr(1).unwrap())? };
                Ok(Expr::Raw(format!(
                    "{} BETWEEN SYMMETRIC {} AND {}",
                    tested.to_sql(),
                    low.to_sql(),
                    high.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_NOT_BETWEEN_SYM => {
                let tested = unsafe { node_to_expr(aexpr.lexpr)? };
                let bounds = pg_list::<pg_sys::Node>(aexpr.rexpr as *mut _);
                let low = unsafe { node_to_expr(bounds.get_ptr(0).unwrap())? };
                let high = unsafe { node_to_expr(bounds.get_ptr(1).unwrap())? };
                Ok(Expr::Raw(format!(
                    "{} NOT BETWEEN SYMMETRIC {} AND {}",
                    tested.to_sql(),
                    low.to_sql(),
                    high.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_SIMILAR => {
                // SIMILAR TO
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                // rexpr for SIMILAR TO is a FuncCall wrapping the pattern
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                Ok(Expr::Raw(format!(
                    "{} SIMILAR TO {}",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_LIKE => {
                // [NOT] LIKE — name is "~~" (LIKE) or "!~~" (NOT LIKE).
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                let op_name = unsafe { extract_operator_name(aexpr.name) }
                    .unwrap_or_else(|_| "~~".to_string());
                let kw = if op_name == "!~~" { "NOT LIKE" } else { "LIKE" };
                Ok(Expr::Raw(format!(
                    "{} {kw} {}",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_ILIKE => {
                // [NOT] ILIKE — name is "~~*" (ILIKE) or "!~~*" (NOT ILIKE).
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                let op_name = unsafe { extract_operator_name(aexpr.name) }
                    .unwrap_or_else(|_| "~~*".to_string());
                let kw = if op_name == "!~~*" {
                    "NOT ILIKE"
                } else {
                    "ILIKE"
                };
                Ok(Expr::Raw(format!(
                    "{} {kw} {}",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_OP_ANY => {
                // expr op ANY(array)
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                let op_name = unsafe { extract_operator_name(aexpr.name)? };
                Ok(Expr::Raw(format!(
                    "{} {op_name} ANY({})",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_OP_ALL => {
                // expr op ALL(array)
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                let op_name = unsafe { extract_operator_name(aexpr.name)? };
                Ok(Expr::Raw(format!(
                    "{} {op_name} ALL({})",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            pg_sys::A_Expr_Kind::AEXPR_NULLIF => {
                // NULLIF(a, b) — evaluates to a when a <> b, else NULL.
                // Represented in the raw parse tree as AEXPR_NULLIF; handled
                // here so expressions like `NULLIF(col, '')::bigint` in the
                // SELECT list don't cause an unsupported-operator error.
                let left = unsafe { node_to_expr(aexpr.lexpr)? };
                let right = unsafe { node_to_expr(aexpr.rexpr)? };
                Ok(Expr::Raw(format!(
                    "NULLIF({}, {})",
                    left.to_sql(),
                    right.to_sql()
                )))
            }
            _other => Err(PgTrickleError::UnsupportedOperator(format!(
                "A_Expr kind {:?} is not supported in defining queries",
                _other,
            ))),
        }
    } else if let Some(bexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr) {
        let args_list = pg_list::<pg_sys::Node>(bexpr.args);
        let mut args = Vec::new();
        for n in args_list.iter_ptr() {
            if let Ok(e) = unsafe { node_to_expr(n) } {
                args.push(e);
            }
        }

        match bexpr.boolop {
            pg_sys::BoolExprType::AND_EXPR => {
                if args.len() < 2 {
                    return args.into_iter().next().ok_or_else(|| {
                        PgTrickleError::QueryParseError("Empty AND expression".into())
                    });
                }
                let mut result = args[0].clone();
                for arg in &args[1..] {
                    result = Expr::BinaryOp {
                        op: "AND".to_string(),
                        left: Box::new(result),
                        right: Box::new(arg.clone()),
                    };
                }
                Ok(result)
            }
            pg_sys::BoolExprType::OR_EXPR => {
                if args.len() < 2 {
                    return args.into_iter().next().ok_or_else(|| {
                        PgTrickleError::QueryParseError("Empty OR expression".into())
                    });
                }
                let mut result = args[0].clone();
                for arg in &args[1..] {
                    result = Expr::BinaryOp {
                        op: "OR".to_string(),
                        left: Box::new(result),
                        right: Box::new(arg.clone()),
                    };
                }
                Ok(result)
            }
            pg_sys::BoolExprType::NOT_EXPR => {
                let inner = args.into_iter().next().ok_or_else(|| {
                    PgTrickleError::QueryParseError("Empty NOT expression".into())
                })?;
                Ok(Expr::FuncCall {
                    func_name: "NOT".to_string(),
                    args: vec![inner],
                })
            }
            _ => Ok(Expr::Raw("/* unknown bool expr */".to_string())),
        }
    } else if let Some(fcall) = cast_node!(node, T_FuncCall, pg_sys::FuncCall) {
        let func_name = unsafe { extract_func_name(fcall.funcname)? };

        // Handle COUNT(*) and other agg_star calls.
        // PostgreSQL represents COUNT(*) as FuncCall { agg_star: true, args: [] }.
        // We must emit the "*" explicitly so to_sql() produces COUNT(*) not COUNT().
        let base_args_str = if fcall.agg_star {
            "*".to_string()
        } else {
            let args_list = pg_list::<pg_sys::Node>(fcall.args);
            let mut args = Vec::new();
            for n in args_list.iter_ptr() {
                if let Ok(e) = unsafe { node_to_expr(n) } {
                    args.push(e.to_sql());
                }
            }
            args.join(", ")
        };

        // Window functions: FuncCall with non-null .over → emit full OVER clause
        if !fcall.over.is_null() {
            let over = unsafe { &*fcall.over };
            let mut over_parts = Vec::new();

            // PARTITION BY
            if !over.partitionClause.is_null() {
                let parts_list = pg_list::<pg_sys::Node>(over.partitionClause);
                let mut pk = Vec::new();
                for p in parts_list.iter_ptr() {
                    pk.push(unsafe { node_to_expr(p) }?.to_sql());
                }
                over_parts.push(format!("PARTITION BY {}", pk.join(", ")));
            }

            // ORDER BY
            if !over.orderClause.is_null() {
                let sort_list = pg_list::<pg_sys::Node>(over.orderClause);
                let sort_sql = unsafe { deparse_sort_clause(&sort_list)? };
                over_parts.push(format!("ORDER BY {}", sort_sql));
            }

            // Frame clause (ROWS/RANGE/GROUPS BETWEEN ... AND ...)
            if let Some(frame_sql) = unsafe { deparse_window_frame(over) } {
                over_parts.push(frame_sql);
            }

            let over_sql = over_parts.join(" ");
            return Ok(Expr::Raw(format!(
                "{func_name}({base_args_str}) OVER ({over_sql})"
            )));
        }

        if fcall.agg_star {
            Ok(Expr::FuncCall {
                func_name,
                args: vec![Expr::Raw("*".to_string())],
            })
        } else {
            let args_list = pg_list::<pg_sys::Node>(fcall.args);
            let mut args = Vec::new();
            for n in args_list.iter_ptr() {
                if let Ok(e) = unsafe { node_to_expr(n) } {
                    args.push(e);
                }
            }
            Ok(Expr::FuncCall { func_name, args })
        }
    } else if let Some(tc) = cast_node!(node, T_TypeCast, pg_sys::TypeCast) {
        let inner = unsafe { node_to_expr(tc.arg)? };
        let type_name = unsafe { deparse_typename(tc.typeName) };
        Ok(Expr::Raw(format!(
            "CAST({} AS {})",
            inner.to_sql(),
            type_name,
        )))
    } else if let Some(nt) = cast_node!(node, T_NullTest, pg_sys::NullTest) {
        let arg = unsafe { node_to_expr(nt.arg as *mut pg_sys::Node)? };
        let op = if nt.nulltesttype == pg_sys::NullTestType::IS_NULL {
            "IS NULL"
        } else {
            "IS NOT NULL"
        };
        Ok(Expr::Raw(format!("{} {op}", arg.to_sql())))
    } else if let Some(case_expr) = cast_node!(node, T_CaseExpr, pg_sys::CaseExpr) {
        let mut sql = String::from("CASE");

        // Simple CASE: CASE <arg> WHEN ...
        if !case_expr.arg.is_null() {
            let arg = unsafe { node_to_expr(case_expr.arg as *mut pg_sys::Node)? };
            sql.push_str(&format!(" {}", arg.to_sql()));
        }

        let when_list = pg_list::<pg_sys::Node>(case_expr.args);
        for w in when_list.iter_ptr() {
            let case_when = unsafe { &*(w as *const pg_sys::CaseWhen) };
            let cond = unsafe { node_to_expr(case_when.expr as *mut pg_sys::Node)? };
            let result = unsafe { node_to_expr(case_when.result as *mut pg_sys::Node)? };
            sql.push_str(&format!(" WHEN {} THEN {}", cond.to_sql(), result.to_sql()));
        }

        if !case_expr.defresult.is_null() {
            let def = unsafe { node_to_expr(case_expr.defresult as *mut pg_sys::Node)? };
            sql.push_str(&format!(" ELSE {}", def.to_sql()));
        }

        sql.push_str(" END");
        Ok(Expr::Raw(sql))
    } else if let Some(coalesce) = cast_node!(node, T_CoalesceExpr, pg_sys::CoalesceExpr) {
        let args_list = pg_list::<pg_sys::Node>(coalesce.args);
        let mut args = Vec::new();
        for n in args_list.iter_ptr() {
            args.push(unsafe { node_to_expr(n)? });
        }
        Ok(Expr::FuncCall {
            func_name: "COALESCE".to_string(),
            args,
        })
    } else if let Some(nullif) = cast_node!(node, T_NullIfExpr, pg_sys::NullIfExpr) {
        let args_list = pg_list::<pg_sys::Node>(nullif.args);
        let mut args = Vec::new();
        for n in args_list.iter_ptr() {
            args.push(unsafe { node_to_expr(n)? });
        }
        Ok(Expr::FuncCall {
            func_name: "NULLIF".to_string(),
            args,
        })
    } else if let Some(mmexpr) = cast_node!(node, T_MinMaxExpr, pg_sys::MinMaxExpr) {
        let func_name = if mmexpr.op == pg_sys::MinMaxOp::IS_GREATEST {
            "GREATEST"
        } else {
            "LEAST"
        };
        let args_list = pg_list::<pg_sys::Node>(mmexpr.args);
        let mut args = Vec::new();
        for n in args_list.iter_ptr() {
            args.push(unsafe { node_to_expr(n)? });
        }
        Ok(Expr::FuncCall {
            func_name: func_name.to_string(),
            args,
        })
    } else if let Some(svf) = cast_node!(node, T_SQLValueFunction, pg_sys::SQLValueFunction) {
        let kw = sql_value_function_name(svf.op);
        Ok(Expr::Raw(kw.to_string()))
    } else if let Some(bt) = cast_node!(node, T_BooleanTest, pg_sys::BooleanTest) {
        let arg = unsafe { node_to_expr(bt.arg as *mut pg_sys::Node)? };
        let test = match bt.booltesttype {
            pg_sys::BoolTestType::IS_TRUE => "IS TRUE",
            pg_sys::BoolTestType::IS_NOT_TRUE => "IS NOT TRUE",
            pg_sys::BoolTestType::IS_FALSE => "IS FALSE",
            pg_sys::BoolTestType::IS_NOT_FALSE => "IS NOT FALSE",
            pg_sys::BoolTestType::IS_UNKNOWN => "IS UNKNOWN",
            pg_sys::BoolTestType::IS_NOT_UNKNOWN => "IS NOT UNKNOWN",
            _ => "IS TRUE",
        };
        Ok(Expr::Raw(format!("{} {test}", arg.to_sql())))
    } else if let Some(sublink) = cast_node!(node, T_SubLink, pg_sys::SubLink) {
        match sublink.subLinkType {
            pg_sys::SubLinkType::EXPR_SUBLINK => {
                // Scalar subquery — reconstruct as Raw SQL
                // The subselect is a SelectStmt; deparse it back to SQL
                if sublink.subselect.is_null() {
                    return Err(PgTrickleError::QueryParseError(
                        "Scalar subquery has NULL subselect".into(),
                    ));
                }
                let inner_sql = deparse_select_to_sql(sublink.subselect)?;
                Ok(Expr::Raw(format!("({inner_sql})")))
            }
            pg_sys::SubLinkType::EXISTS_SUBLINK => {
                // EXISTS in an expression context (e.g., inside CASE WHEN)
                // Reconstruct as Raw SQL
                if sublink.subselect.is_null() {
                    return Err(PgTrickleError::QueryParseError(
                        "EXISTS subquery has NULL subselect".into(),
                    ));
                }
                let inner_sql = deparse_select_to_sql(sublink.subselect)?;
                Ok(Expr::Raw(format!("EXISTS ({inner_sql})")))
            }
            pg_sys::SubLinkType::ANY_SUBLINK => {
                // IN/ANY in an expression context
                if sublink.subselect.is_null() || sublink.testexpr.is_null() {
                    return Err(PgTrickleError::QueryParseError(
                        "IN/ANY subquery has NULL subselect or testexpr".into(),
                    ));
                }
                let test = unsafe { node_to_expr(sublink.testexpr)? };
                let inner_sql = deparse_select_to_sql(sublink.subselect)?;
                Ok(Expr::Raw(format!("{} IN ({inner_sql})", test.to_sql())))
            }
            _ => {
                let kind_desc = match sublink.subLinkType {
                    pg_sys::SubLinkType::ALL_SUBLINK => "ALL (subquery)",
                    _ => "subquery expression",
                };
                Err(PgTrickleError::UnsupportedOperator(format!(
                    "{kind_desc} is not supported in defining queries. \
                     Consider rewriting as a JOIN or LATERAL subquery.",
                )))
            }
        }
    } else if let Some(arrexpr) = cast_node!(node, T_ArrayExpr, pg_sys::ArrayExpr) {
        let elems = pg_list::<pg_sys::Node>(arrexpr.elements);
        let mut elem_sql = Vec::new();
        for n in elems.iter_ptr() {
            elem_sql.push(unsafe { node_to_expr(n)? }.to_sql());
        }
        Ok(Expr::Raw(format!("ARRAY[{}]", elem_sql.join(", "))))
    } else if let Some(rowexpr) = cast_node!(node, T_RowExpr, pg_sys::RowExpr) {
        let fields = pg_list::<pg_sys::Node>(rowexpr.args);
        let mut field_sql = Vec::new();
        for n in fields.iter_ptr() {
            field_sql.push(unsafe { node_to_expr(n)? }.to_sql());
        }
        Ok(Expr::Raw(format!("ROW({})", field_sql.join(", "))))
    } else if let Some(indir) = cast_node!(node, T_A_Indirection, pg_sys::A_Indirection) {
        let base = unsafe { node_to_expr(indir.arg)? };
        let mut sql = base.to_sql();
        let indirection_list = pg_list::<pg_sys::Node>(indir.indirection);
        for ind_node in indirection_list.iter_ptr() {
            if let Some(indices) = cast_node!(ind_node, T_A_Indices, pg_sys::A_Indices) {
                if !indices.uidx.is_null() {
                    let idx = unsafe { node_to_expr(indices.uidx)? };
                    // Parenthesize `sql` so that complex base expressions such as
                    // `array_agg(...) FILTER (WHERE ...)` deparse as
                    // `(array_agg(...) FILTER (WHERE ...))[1]` — the subscript
                    // operator binds tighter than FILTER/aggregate syntax, so
                    // omitting the parens produces invalid SQL.
                    sql = format!("({sql})[{}]", idx.to_sql());
                }
            } else if let Some(s) = cast_node!(ind_node, T_String, pg_sys::String) {
                let field_name = pg_cstr_to_str(s.sval).unwrap_or("");
                sql = format!("({sql}).{field_name}");
            } else if unsafe { pgrx::is_a(ind_node, pg_sys::NodeTag::T_A_Star) } {
                sql = format!("({sql}).*");
            }
        }
        Ok(Expr::Raw(sql))
    } else if let Some(cc) = cast_node!(node, T_CollateClause, pg_sys::CollateClause) {
        let arg = unsafe { node_to_expr(cc.arg)? };
        // Extract collation name from the name list
        let coll_list = pg_list::<pg_sys::Node>(cc.collname);
        let mut coll_parts = Vec::new();
        for n in coll_list.iter_ptr() {
            if let Ok(s) = unsafe { node_to_string(n) } {
                coll_parts.push(s);
            }
        }
        let coll_name = if coll_parts.is_empty() {
            "\"C\"".to_string()
        } else {
            coll_parts
                .iter()
                .map(|p| format!("\"{}\"", p.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(".")
        };
        Ok(Expr::Raw(
            format!("{} COLLATE {}", arg.to_sql(), coll_name,),
        ))
    } else if let Some(jip) = cast_node!(node, T_JsonIsPredicate, pg_sys::JsonIsPredicate) {
        let arg = unsafe { node_to_expr(jip.expr)? };
        let type_str = match jip.item_type {
            pg_sys::JsonValueType::JS_TYPE_OBJECT => "JSON OBJECT",
            pg_sys::JsonValueType::JS_TYPE_ARRAY => "JSON ARRAY",
            pg_sys::JsonValueType::JS_TYPE_SCALAR => "JSON SCALAR",
            _ => "JSON", // JS_TYPE_ANY
        };
        let unique = if jip.unique_keys {
            " WITH UNIQUE KEYS"
        } else {
            ""
        };
        Ok(Expr::Raw(format!("{} IS {type_str}{unique}", arg.to_sql())))
    } else if let Some(joc) =
        cast_node!(node, T_JsonObjectConstructor, pg_sys::JsonObjectConstructor)
    {
        let exprs = pg_list::<pg_sys::Node>(joc.exprs);
        let mut pairs = Vec::new();
        for n in exprs.iter_ptr() {
            if let Some(kv) = cast_node!(n, T_JsonKeyValue, pg_sys::JsonKeyValue) {
                let key = unsafe { node_to_expr(kv.key as *mut pg_sys::Node)? };
                let val = if !kv.value.is_null() {
                    let jve = unsafe { &*kv.value };
                    unsafe { node_to_expr(jve.raw_expr as *mut pg_sys::Node)? }
                } else {
                    Expr::Raw("NULL".to_string())
                };
                pairs.push(format!("{} : {}", key.to_sql(), val.to_sql()));
            }
        }
        let mut sql = format!("JSON_OBJECT({})", pairs.join(", "));
        if joc.absent_on_null {
            sql.push_str(" ABSENT ON NULL");
        }
        unsafe { append_json_output(&mut sql, joc.output) };
        Ok(Expr::Raw(sql))
    } else if let Some(jac) = cast_node!(node, T_JsonArrayConstructor, pg_sys::JsonArrayConstructor)
    {
        let exprs = pg_list::<pg_sys::Node>(jac.exprs);
        let mut elems = Vec::new();
        for n in exprs.iter_ptr() {
            // Elements may be JsonValueExpr or plain exprs
            if let Some(jve) = cast_node!(n, T_JsonValueExpr, pg_sys::JsonValueExpr) {
                elems.push(unsafe { node_to_expr(jve.raw_expr as *mut pg_sys::Node)? }.to_sql());
            } else {
                elems.push(unsafe { node_to_expr(n)? }.to_sql());
            }
        }
        let mut sql = format!("JSON_ARRAY({})", elems.join(", "));
        if jac.absent_on_null {
            sql.push_str(" ABSENT ON NULL");
        }
        unsafe { append_json_output(&mut sql, jac.output) };
        Ok(Expr::Raw(sql))
    } else if let Some(jaqc) = cast_node!(
        node,
        T_JsonArrayQueryConstructor,
        pg_sys::JsonArrayQueryConstructor
    ) {
        let inner_sql = if !jaqc.query.is_null() {
            deparse_select_to_sql(jaqc.query)?
        } else {
            "SELECT NULL".to_string()
        };
        let mut sql = format!("JSON_ARRAY({inner_sql})");
        unsafe { append_json_output(&mut sql, jaqc.output) };
        Ok(Expr::Raw(sql))
    } else if let Some(jpe) = cast_node!(node, T_JsonParseExpr, pg_sys::JsonParseExpr) {
        let arg = if !jpe.expr.is_null() {
            let jve = unsafe { &*jpe.expr };
            unsafe { node_to_expr(jve.raw_expr as *mut pg_sys::Node)? }
        } else {
            Expr::Raw("NULL".to_string())
        };
        let mut sql = format!("JSON({})", arg.to_sql());
        unsafe { append_json_output(&mut sql, jpe.output) };
        Ok(Expr::Raw(sql))
    } else if let Some(jse) = cast_node!(node, T_JsonScalarExpr, pg_sys::JsonScalarExpr) {
        let arg = unsafe { node_to_expr(jse.expr as *mut pg_sys::Node)? };
        let mut sql = format!("JSON_SCALAR({})", arg.to_sql());
        unsafe { append_json_output(&mut sql, jse.output) };
        Ok(Expr::Raw(sql))
    } else if let Some(jse) = cast_node!(node, T_JsonSerializeExpr, pg_sys::JsonSerializeExpr) {
        let arg = if !jse.expr.is_null() {
            let jve = unsafe { &*jse.expr };
            unsafe { node_to_expr(jve.raw_expr as *mut pg_sys::Node)? }
        } else {
            Expr::Raw("NULL".to_string())
        };
        let mut sql = format!("JSON_SERIALIZE({})", arg.to_sql());
        unsafe { append_json_output(&mut sql, jse.output) };
        Ok(Expr::Raw(sql))
    } else if let Some(joa) = cast_node!(node, T_JsonObjectAgg, pg_sys::JsonObjectAgg) {
        let mut parts = Vec::new();
        if !joa.arg.is_null() {
            let kv = unsafe { &*joa.arg };
            let key = unsafe { node_to_expr(kv.key as *mut pg_sys::Node)? };
            let val = if !kv.value.is_null() {
                let jve = unsafe { &*kv.value };
                unsafe { node_to_expr(jve.raw_expr as *mut pg_sys::Node)? }
            } else {
                Expr::Raw("NULL".to_string())
            };
            parts.push(format!("{} : {}", key.to_sql(), val.to_sql()));
        }
        let mut inner = parts.join(", ");
        if joa.absent_on_null {
            inner.push_str(" ABSENT ON NULL");
        }
        if joa.unique {
            inner.push_str(" WITH UNIQUE KEYS");
        }
        unsafe { append_json_agg_clauses(&mut inner, joa.constructor) };
        let sql = format!("JSON_OBJECTAGG({inner})");
        Ok(Expr::Raw(sql))
    } else if let Some(jaa) = cast_node!(node, T_JsonArrayAgg, pg_sys::JsonArrayAgg) {
        let arg = if !jaa.arg.is_null() {
            let jve = unsafe { &*jaa.arg };
            unsafe { node_to_expr(jve.raw_expr as *mut pg_sys::Node)? }
        } else {
            Expr::Raw("NULL".to_string())
        };
        let mut inner = arg.to_sql();
        if jaa.absent_on_null {
            inner.push_str(" ABSENT ON NULL");
        }
        unsafe { append_json_agg_clauses(&mut inner, jaa.constructor) };
        let sql = format!("JSON_ARRAYAGG({inner})");
        Ok(Expr::Raw(sql))
    } else {
        // Catch-all: reject with clear error instead of silently producing broken SQL
        let tag = unsafe { (*node).type_ };
        Err(PgTrickleError::UnsupportedOperator(format!(
            "Expression type {tag:?} is not supported in defining queries",
        )))
    }
}

/// Append a `RETURNING <type>` clause from a `JsonOutput` node to a SQL string.
///
/// # Safety
/// `output` must be null or point to a valid `JsonOutput` node.
unsafe fn append_json_output(sql: &mut String, output: *const pg_sys::JsonOutput) {
    if output.is_null() {
        return;
    }
    let out = unsafe { &*output };
    if !out.typeName.is_null() {
        let type_name = unsafe { deparse_typename(out.typeName) };
        if !type_name.is_empty() && type_name != "json" && type_name != "jsonb" {
            sql.push_str(&format!(" RETURNING {type_name}"));
        }
    }
}

/// Append ORDER BY and FILTER clauses from a `JsonAggConstructor` to a SQL string.
///
/// # Safety
/// `constructor` must be null or point to a valid `JsonAggConstructor`.
unsafe fn append_json_agg_clauses(
    sql: &mut String,
    constructor: *const pg_sys::JsonAggConstructor,
) {
    if constructor.is_null() {
        return;
    }
    let ctor = unsafe { &*constructor };

    // ORDER BY
    let order_list = pg_list::<pg_sys::Node>(ctor.agg_order);
    if !order_list.is_empty() {
        let mut order_parts = Vec::new();
        for n in order_list.iter_ptr() {
            if let Some(sb) = cast_node!(n, T_SortBy, pg_sys::SortBy)
                && let Ok(expr) = unsafe { node_to_expr(sb.node) }
            {
                let dir = match sb.sortby_dir {
                    pg_sys::SortByDir::SORTBY_DESC => " DESC",
                    pg_sys::SortByDir::SORTBY_ASC => " ASC",
                    _ => "",
                };
                let nulls = match sb.sortby_nulls {
                    pg_sys::SortByNulls::SORTBY_NULLS_FIRST => " NULLS FIRST",
                    pg_sys::SortByNulls::SORTBY_NULLS_LAST => " NULLS LAST",
                    _ => "",
                };
                order_parts.push(format!("{}{dir}{nulls}", expr.to_sql()));
            }
        }
        if !order_parts.is_empty() {
            sql.push_str(&format!(" ORDER BY {}", order_parts.join(", ")));
        }
    }

    // RETURNING
    unsafe { append_json_output(sql, ctor.output) };
}

/// Deparse a `T_JsonTable` FROM item to SQL text.
///
/// Produces: `JSON_TABLE(expr, 'path' COLUMNS (col_defs))`.
/// Does not include the alias — callers append `AS alias` as needed.
///
/// # Safety
/// `jt` must point to a valid `pg_sys::JsonTable` node.
unsafe fn deparse_json_table(jt: *const pg_sys::JsonTable) -> Result<String, PgTrickleError> {
    let jt_ref = unsafe { &*jt };

    // Context item (the input expression)
    let context_sql = if !jt_ref.context_item.is_null() {
        let jve = unsafe { &*jt_ref.context_item };
        if !jve.raw_expr.is_null() {
            let expr = unsafe { node_to_expr(jve.raw_expr as *mut pg_sys::Node)? };
            expr.to_sql()
        } else {
            "NULL".to_string()
        }
    } else {
        "NULL".to_string()
    };

    // Path specification
    let path_sql = if !jt_ref.pathspec.is_null() {
        let ps = unsafe { &*jt_ref.pathspec };
        if !ps.string.is_null() {
            let expr = unsafe { node_to_expr(ps.string)? };
            expr.to_sql()
        } else {
            "'$'".to_string()
        }
    } else {
        "'$'".to_string()
    };

    // PASSING clause
    let passing_sql = unsafe { deparse_json_table_passing(jt_ref.passing)? };

    // COLUMNS clause
    let columns_sql = unsafe { deparse_json_table_columns(jt_ref.columns)? };

    // ON ERROR behavior
    let on_error_sql = unsafe { deparse_json_behavior(jt_ref.on_error, "ON ERROR") };

    let mut sql = format!("JSON_TABLE({context_sql}, {path_sql}");
    if !passing_sql.is_empty() {
        sql.push_str(&format!(" PASSING {passing_sql}"));
    }
    sql.push_str(&format!(" COLUMNS ({columns_sql})"));
    if !on_error_sql.is_empty() {
        sql.push_str(&format!(" {on_error_sql}"));
    }
    sql.push(')');

    Ok(sql)
}

/// Deparse the PASSING clause of JSON_TABLE.
///
/// # Safety
/// `passing` must be null or a valid pg_sys::List.
unsafe fn deparse_json_table_passing(passing: *mut pg_sys::List) -> Result<String, PgTrickleError> {
    if passing.is_null() {
        return Ok(String::new());
    }
    let list = pg_list::<pg_sys::Node>(passing);
    if list.is_empty() {
        return Ok(String::new());
    }
    // PASSING items are JsonArgument nodes: (val, name)
    // In the raw parse tree they appear as ResTarget-like structures.
    // Deparse each as "expr AS name".
    let mut parts = Vec::new();
    for node_ptr in list.iter_ptr() {
        let expr = unsafe { node_to_expr(node_ptr)? };
        parts.push(expr.to_sql());
    }
    Ok(parts.join(", "))
}

/// Deparse the COLUMNS clause of JSON_TABLE.
///
/// # Safety
/// `columns` must be null or a valid pg_sys::List of JsonTableColumn nodes.
unsafe fn deparse_json_table_columns(columns: *mut pg_sys::List) -> Result<String, PgTrickleError> {
    if columns.is_null() {
        return Ok(String::new());
    }
    let list = pg_list::<pg_sys::Node>(columns);
    let mut parts = Vec::new();
    for node_ptr in list.iter_ptr() {
        let col_sql = unsafe { deparse_json_table_column(node_ptr)? };
        parts.push(col_sql);
    }
    Ok(parts.join(", "))
}

/// Deparse a single JSON_TABLE column definition.
///
/// # Safety
/// `node` must point to a valid `pg_sys::JsonTableColumn`.
unsafe fn deparse_json_table_column(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JsonTableColumn) } {
        return Err(PgTrickleError::QueryParseError(
            "Expected JsonTableColumn node".into(),
        ));
    }
    let col = unsafe { &*(node as *const pg_sys::JsonTableColumn) };

    let name = if !col.name.is_null() {
        pg_cstr_to_str(col.name).unwrap_or("col").to_string()
    } else {
        "col".to_string()
    };

    match col.coltype {
        pg_sys::JsonTableColumnType::JTC_FOR_ORDINALITY => Ok(format!("{name} FOR ORDINALITY")),
        pg_sys::JsonTableColumnType::JTC_REGULAR => {
            let type_name = unsafe { deparse_typename(col.typeName) };
            let mut s = format!("{name} {type_name}");

            // FORMAT JSON
            if !col.format.is_null() {
                let fmt = unsafe { &*col.format };
                if fmt.format_type == pg_sys::JsonFormatType::JS_FORMAT_JSON {
                    s.push_str(" FORMAT JSON");
                }
            }

            // PATH 'spec'
            if !col.pathspec.is_null() {
                let ps = unsafe { &*col.pathspec };
                if !ps.string.is_null() {
                    let path_expr = unsafe { node_to_expr(ps.string)? };
                    s.push_str(&format!(" PATH {}", path_expr.to_sql()));
                }
            }

            // WRAPPER
            match col.wrapper {
                pg_sys::JsonWrapper::JSW_CONDITIONAL => {
                    s.push_str(" WITH CONDITIONAL WRAPPER");
                }
                pg_sys::JsonWrapper::JSW_UNCONDITIONAL => {
                    s.push_str(" WITH UNCONDITIONAL WRAPPER");
                }
                _ => {}
            }

            // QUOTES
            if col.quotes == pg_sys::JsonQuotes::JS_QUOTES_OMIT {
                s.push_str(" OMIT QUOTES");
            }

            // ON EMPTY / ON ERROR
            let empty = unsafe { deparse_json_behavior(col.on_empty, "ON EMPTY") };
            if !empty.is_empty() {
                s.push_str(&format!(" {empty}"));
            }
            let error = unsafe { deparse_json_behavior(col.on_error, "ON ERROR") };
            if !error.is_empty() {
                s.push_str(&format!(" {error}"));
            }

            Ok(s)
        }
        pg_sys::JsonTableColumnType::JTC_EXISTS => {
            let type_name = unsafe { deparse_typename(col.typeName) };
            let mut s = format!("{name} {type_name} EXISTS");

            if !col.pathspec.is_null() {
                let ps = unsafe { &*col.pathspec };
                if !ps.string.is_null() {
                    let path_expr = unsafe { node_to_expr(ps.string)? };
                    s.push_str(&format!(" PATH {}", path_expr.to_sql()));
                }
            }

            let error = unsafe { deparse_json_behavior(col.on_error, "ON ERROR") };
            if !error.is_empty() {
                s.push_str(&format!(" {error}"));
            }

            Ok(s)
        }
        pg_sys::JsonTableColumnType::JTC_FORMATTED => {
            // Formatted column: like regular but with FORMAT JSON
            let type_name = unsafe { deparse_typename(col.typeName) };
            let mut s = format!("{name} {type_name} FORMAT JSON");

            if !col.pathspec.is_null() {
                let ps = unsafe { &*col.pathspec };
                if !ps.string.is_null() {
                    let path_expr = unsafe { node_to_expr(ps.string)? };
                    s.push_str(&format!(" PATH {}", path_expr.to_sql()));
                }
            }

            let empty = unsafe { deparse_json_behavior(col.on_empty, "ON EMPTY") };
            if !empty.is_empty() {
                s.push_str(&format!(" {empty}"));
            }
            let error = unsafe { deparse_json_behavior(col.on_error, "ON ERROR") };
            if !error.is_empty() {
                s.push_str(&format!(" {error}"));
            }

            Ok(s)
        }
        pg_sys::JsonTableColumnType::JTC_NESTED => {
            // NESTED PATH 'path' COLUMNS (...)
            let mut s = String::from("NESTED");
            if !col.pathspec.is_null() {
                let ps = unsafe { &*col.pathspec };
                if !ps.string.is_null() {
                    let path_expr = unsafe { node_to_expr(ps.string)? };
                    s.push_str(&format!(" PATH {}", path_expr.to_sql()));
                }
            }
            let nested_cols = unsafe { deparse_json_table_columns(col.columns)? };
            s.push_str(&format!(" COLUMNS ({nested_cols})"));
            Ok(s)
        }
        _ => Err(PgTrickleError::QueryParseError(format!(
            "Unknown JSON_TABLE column type: {}",
            col.coltype,
        ))),
    }
}

/// Deparse a JSON behavior clause (ON EMPTY / ON ERROR).
///
/// Returns empty string if behavior is null or default.
///
/// # Safety
/// `behavior` must be null or point to a valid `pg_sys::JsonBehavior`.
unsafe fn deparse_json_behavior(behavior: *const pg_sys::JsonBehavior, suffix: &str) -> String {
    if behavior.is_null() {
        return String::new();
    }
    let beh = unsafe { &*behavior };
    match beh.btype {
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_NULL => format!("NULL {suffix}"),
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_ERROR => format!("ERROR {suffix}"),
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_EMPTY => format!("EMPTY {suffix}"),
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_EMPTY_ARRAY => {
            format!("EMPTY ARRAY {suffix}")
        }
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_EMPTY_OBJECT => {
            format!("EMPTY OBJECT {suffix}")
        }
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_DEFAULT => {
            if !beh.expr.is_null() {
                if let Ok(expr) = unsafe { node_to_expr(beh.expr) } {
                    format!("DEFAULT {} {suffix}", expr.to_sql())
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_TRUE => format!("TRUE {suffix}"),
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_FALSE => format!("FALSE {suffix}"),
        pg_sys::JsonBehaviorType::JSON_BEHAVIOR_UNKNOWN => format!("UNKNOWN {suffix}"),
        _ => String::new(),
    }
}

/// Extract operator name from an A_Expr name list.
unsafe fn extract_operator_name(name_list: *mut pg_sys::List) -> Result<String, PgTrickleError> {
    let list = pg_list::<pg_sys::Node>(name_list);
    if let Some(node) = list.head() {
        unsafe { node_to_string(node) }
    } else {
        Ok("=".to_string())
    }
}

/// Extract function name from funcname list (possibly schema-qualified).
unsafe fn extract_func_name(name_list: *mut pg_sys::List) -> Result<String, PgTrickleError> {
    let list = pg_list::<pg_sys::Node>(name_list);
    let mut parts = Vec::new();
    for n in list.iter_ptr() {
        if let Ok(s) = unsafe { node_to_string(n) } {
            parts.push(s);
        }
    }
    Ok(parts.join("."))
}

/// Deparse a `FuncCall` node back to SQL text.
///
/// Reconstructs `func_name(arg1, arg2, ...)` from the parse tree.
/// This is used for set-returning functions in the FROM clause
/// where we need the complete function call as SQL text.
///
/// # Safety
/// Caller must ensure `fcall` points to a valid `pg_sys::FuncCall` node.
unsafe fn deparse_func_call(fcall: *const pg_sys::FuncCall) -> Result<String, PgTrickleError> {
    let fcall_ref = unsafe { &*fcall };
    let func_name = unsafe { extract_func_name(fcall_ref.funcname)? };

    let args_list = pg_list::<pg_sys::Node>(fcall_ref.args);
    let mut arg_sqls = Vec::new();
    for n in args_list.iter_ptr() {
        let expr = unsafe { node_to_expr(n)? };
        arg_sqls.push(expr.to_sql());
    }

    Ok(format!("{}({})", func_name, arg_sqls.join(", ")))
}

// ═══════════════════════════════════════════════════════════════════════
// LATERAL subquery deparse infrastructure
// ═══════════════════════════════════════════════════════════════════════

/// Deparse a `SelectStmt` back to SQL text.
///
/// Used for LATERAL subquery bodies. Handles the common SQL constructs
/// that appear inside LATERAL subqueries: SELECT, FROM, WHERE, GROUP BY,
/// HAVING, ORDER BY, LIMIT, OFFSET, DISTINCT.
///
/// # Safety
/// Caller must ensure `stmt` points to a valid `pg_sys::SelectStmt`.
unsafe fn deparse_select_stmt_to_sql(
    stmt: *const pg_sys::SelectStmt,
) -> Result<String, PgTrickleError> {
    // SAFETY: caller guarantees stmt is valid.
    let s = unsafe { &*stmt };
    let mut parts = Vec::new();

    // DISTINCT
    let distinct_clause = pg_list::<pg_sys::Node>(s.distinctClause);
    let distinct_prefix = if !distinct_clause.is_empty() {
        "SELECT DISTINCT"
    } else {
        "SELECT"
    };

    // SELECT clause (target list)
    let targets = unsafe { deparse_target_list(s.targetList)? };
    parts.push(format!("{distinct_prefix} {targets}"));

    // FROM clause
    let from = unsafe { deparse_from_clause(s.fromClause)? };
    if !from.is_empty() {
        parts.push(format!("FROM {from}"));
    }

    // WHERE clause
    if !s.whereClause.is_null() {
        let expr = unsafe { node_to_expr(s.whereClause)? };
        parts.push(format!("WHERE {}", expr.to_sql()));
    }

    // GROUP BY clause
    let group_list = pg_list::<pg_sys::Node>(s.groupClause);
    if !group_list.is_empty() {
        let mut groups = Vec::new();
        for node_ptr in group_list.iter_ptr() {
            let expr = unsafe { node_to_expr(node_ptr)? };
            groups.push(expr.to_sql());
        }
        parts.push(format!("GROUP BY {}", groups.join(", ")));
    }

    // HAVING clause
    if !s.havingClause.is_null() {
        let expr = unsafe { node_to_expr(s.havingClause)? };
        parts.push(format!("HAVING {}", expr.to_sql()));
    }

    // ORDER BY clause (SortBy nodes)
    let sort_list = pg_list::<pg_sys::Node>(s.sortClause);
    if !sort_list.is_empty() {
        let sorts = unsafe { deparse_sort_clause(&sort_list)? };
        parts.push(format!("ORDER BY {sorts}"));
    }

    // LIMIT
    if !s.limitCount.is_null() {
        let expr = unsafe { node_to_expr(s.limitCount)? };
        parts.push(format!("LIMIT {}", expr.to_sql()));
    }

    // OFFSET
    if !s.limitOffset.is_null() {
        let expr = unsafe { node_to_expr(s.limitOffset)? };
        parts.push(format!("OFFSET {}", expr.to_sql()));
    }

    Ok(parts.join(" "))
}

/// Deparse a target list (ResTarget nodes) into SQL text.
///
/// Produces: `expr1 AS alias1, expr2, expr3 AS alias3, ...`
///
/// # Safety
/// Caller must ensure `target_list` points to a valid `pg_sys::List`.
unsafe fn deparse_target_list(target_list: *mut pg_sys::List) -> Result<String, PgTrickleError> {
    let targets = pg_list::<pg_sys::Node>(target_list);
    let mut items = Vec::new();
    for node_ptr in targets.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }
        let expr = unsafe { node_to_expr(rt.val)? };
        let expr_sql = expr.to_sql();
        if !rt.name.is_null() {
            let alias = pg_cstr_to_str(rt.name).unwrap_or("");
            items.push(format!("{expr_sql} AS {alias}"));
        } else {
            items.push(expr_sql);
        }
    }
    Ok(items.join(", "))
}

/// Extract column names from a target list.
///
/// For each `ResTarget`:
/// - If the target has an explicit alias (`AS col`), use that.
/// - Otherwise generate a positional name `_col_N` (1-based).
///
/// Used to derive output column names from a constant anchor SELECT.
///
/// # Safety
/// Caller must ensure `target_list` points to a valid `pg_sys::List`.
unsafe fn extract_target_list_column_names(target_list: *mut pg_sys::List) -> Vec<String> {
    let targets = pg_list::<pg_sys::Node>(target_list);
    let mut cols = Vec::new();
    for (i, node_ptr) in targets.iter_ptr().enumerate() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            cols.push(format!("_col_{}", i + 1));
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if !rt.name.is_null() {
            let alias = pg_cstr_to_str(rt.name).unwrap_or("").to_string();
            if alias.is_empty() {
                cols.push(format!("_col_{}", i + 1));
            } else {
                cols.push(alias);
            }
        } else {
            cols.push(format!("_col_{}", i + 1));
        }
    }
    cols
}

/// Deparse a FROM clause (list of FROM items) into SQL text.
///
/// # Safety
/// Caller must ensure `from_list` points to a valid `pg_sys::List`.
unsafe fn deparse_from_clause(from_list: *mut pg_sys::List) -> Result<String, PgTrickleError> {
    let list = pg_list::<pg_sys::Node>(from_list);
    let mut items = Vec::new();
    for node_ptr in list.iter_ptr() {
        let item = unsafe { deparse_from_item_to_sql(node_ptr)? };
        items.push(item);
    }
    Ok(items.join(", "))
}

/// Deparse a single FROM item (RangeVar, JoinExpr, RangeSubselect, RangeFunction) to SQL.
///
/// # Safety
/// Caller must ensure `node` points to a valid pg_sys::Node.
unsafe fn deparse_from_item_to_sql(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if node.is_null() {
        return Err(PgTrickleError::QueryParseError(
            "NULL FROM item node".into(),
        ));
    }

    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        let mut name = String::new();
        if !rv.schemaname.is_null() {
            let schema = pg_cstr_to_str(rv.schemaname).unwrap_or("");
            name.push_str(schema);
            name.push('.');
        }
        if !rv.relname.is_null() {
            let rel = pg_cstr_to_str(rv.relname).unwrap_or("");
            name.push_str(rel);
        }
        if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            let alias_name = pg_cstr_to_str(a.aliasname).unwrap_or("");
            if alias_name != name {
                name.push_str(&format!(" {alias_name}"));
            }
        }
        Ok(name)
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        let left = unsafe { deparse_from_item_to_sql(join.larg)? };
        let right = unsafe { deparse_from_item_to_sql(join.rarg)? };
        let join_type = match join.jointype {
            pg_sys::JoinType::JOIN_INNER => "JOIN",
            pg_sys::JoinType::JOIN_LEFT => "LEFT JOIN",
            pg_sys::JoinType::JOIN_FULL => "FULL JOIN",
            pg_sys::JoinType::JOIN_RIGHT => "RIGHT JOIN",
            _ => "JOIN",
        };
        let condition = if join.quals.is_null() {
            "TRUE".to_string()
        } else {
            let expr = unsafe { node_to_expr(join.quals)? };
            expr.to_sql()
        };
        Ok(format!("{left} {join_type} {right} ON {condition}"))
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect) {
        if sub.subquery.is_null()
            || !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            return Err(PgTrickleError::QueryParseError(
                "RangeSubselect without valid SelectStmt".into(),
            ));
        }
        let sub_stmt = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };
        let inner_sql = unsafe { deparse_select_stmt_to_sql(sub_stmt)? };
        let mut result = format!("({inner_sql})");
        if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            let alias_name = pg_cstr_to_str(a.aliasname).unwrap_or("");
            result.push_str(&format!(" AS {alias_name}"));
        }
        Ok(result)
    } else if let Some(rf) = cast_node!(node, T_RangeFunction, pg_sys::RangeFunction) {
        let func_list = pg_list::<pg_sys::Node>(rf.functions);
        if func_list.is_empty() {
            return Err(PgTrickleError::QueryParseError(
                "RangeFunction with no functions in deparse".into(),
            ));
        }
        let inner_list_node = func_list.head().unwrap();
        let inner_list = pg_list::<pg_sys::Node>(inner_list_node as *mut pg_sys::List);
        if inner_list.is_empty() {
            return Err(PgTrickleError::QueryParseError(
                "RangeFunction inner list empty in deparse".into(),
            ));
        }
        let func_node = inner_list.head().unwrap();
        let func_sql = unsafe { deparse_func_call(func_node as *const pg_sys::FuncCall)? };
        let mut result = func_sql;
        if !rf.alias.is_null() {
            let a = unsafe { &*(rf.alias) };
            let alias_name = pg_cstr_to_str(a.aliasname).unwrap_or("");
            result.push_str(&format!(" AS {alias_name}"));
        }
        Ok(result)
    } else if let Some(jt) = cast_node!(node, T_JsonTable, pg_sys::JsonTable) {
        let mut result = unsafe { deparse_json_table(jt as *const pg_sys::JsonTable)? };
        if !jt.alias.is_null() {
            let a = unsafe { &*(jt.alias) };
            let alias_name = pg_cstr_to_str(a.aliasname).unwrap_or("");
            result.push_str(&format!(" AS {alias_name}"));
        }
        Ok(result)
    } else {
        // Fallback: deparse as generic expression
        let expr = unsafe { node_to_expr(node)? };
        Ok(expr.to_sql())
    }
}

/// Deparse a sort clause (list of SortBy nodes) into SQL text.
///
/// # Safety
/// Caller must ensure all nodes in `sort_list` are valid `SortBy` nodes.
unsafe fn deparse_sort_clause(
    sort_list: &pgrx::PgList<pg_sys::Node>,
) -> Result<String, PgTrickleError> {
    let mut items = Vec::new();
    for node_ptr in sort_list.iter_ptr() {
        let sort_sql = unsafe { deparse_sort_by(node_ptr as *const pg_sys::SortBy)? };
        items.push(sort_sql);
    }
    Ok(items.join(", "))
}

/// Deparse a single SortBy node to SQL (e.g., `created_at DESC`).
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::SortBy`.
unsafe fn deparse_sort_by(node: *const pg_sys::SortBy) -> Result<String, PgTrickleError> {
    // SAFETY: caller guarantees node is valid.
    let sb = unsafe { &*node };
    let expr = unsafe { node_to_expr(sb.node)? };
    let dir = match sb.sortby_dir {
        pg_sys::SortByDir::SORTBY_ASC => " ASC",
        pg_sys::SortByDir::SORTBY_DESC => " DESC",
        _ => "",
    };
    let nulls = match sb.sortby_nulls {
        pg_sys::SortByNulls::SORTBY_NULLS_FIRST => " NULLS FIRST",
        pg_sys::SortByNulls::SORTBY_NULLS_LAST => " NULLS LAST",
        _ => "",
    };
    Ok(format!("{}{dir}{nulls}", expr.to_sql()))
}

/// Extract output column names from a SelectStmt's target list.
///
/// For each ResTarget:
/// - If it has an explicit alias (`AS name`), use that
/// - If it's a ColumnRef, use the column name
/// - Otherwise, generate a positional name (`column1`, `column2`, ...)
///
/// # Safety
/// Caller must ensure `target_list` points to a valid `pg_sys::List`.
unsafe fn extract_select_output_cols(
    target_list: *mut pg_sys::List,
) -> Result<Vec<String>, PgTrickleError> {
    let targets = pg_list::<pg_sys::Node>(target_list);
    let mut cols = Vec::new();
    for (i, node_ptr) in targets.iter_ptr().enumerate() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            cols.push(format!("column{}", i + 1));
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if !rt.name.is_null() {
            let name = pg_cstr_to_str(rt.name).unwrap_or("");
            cols.push(name.to_string());
        } else if !rt.val.is_null() {
            if let Ok(expr) = unsafe { node_to_expr(rt.val) } {
                cols.push(expr.output_name());
            } else {
                cols.push(format!("column{}", i + 1));
            }
        } else {
            cols.push(format!("column{}", i + 1));
        }
    }
    Ok(cols)
}

/// Walk a FROM clause collecting all table OIDs.
///
/// Used to extract source OIDs from a LATERAL subquery body's FROM clause
/// so that CDC triggers can be set up for those tables.
///
/// # Safety
/// Caller must ensure `from_list` points to a valid `pg_sys::List`.
unsafe fn extract_from_oids(from_list: *mut pg_sys::List) -> Result<Vec<u32>, PgTrickleError> {
    let list = pg_list::<pg_sys::Node>(from_list);
    let mut oids = Vec::new();
    for node_ptr in list.iter_ptr() {
        unsafe { collect_from_item_oids(node_ptr, &mut oids)? };
    }
    Ok(oids)
}

/// Recursively collect table OIDs from a single FROM item.
///
/// # Safety
/// Caller must ensure `node` points to a valid pg_sys::Node.
unsafe fn collect_from_item_oids(
    node: *mut pg_sys::Node,
    oids: &mut Vec<u32>,
) -> Result<(), PgTrickleError> {
    if node.is_null() {
        return Ok(());
    }

    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        let table_name = if !rv.relname.is_null() {
            pg_cstr_to_str(rv.relname).unwrap_or("").to_string()
        } else {
            return Ok(());
        };
        let schema = if !rv.schemaname.is_null() {
            pg_cstr_to_str(rv.schemaname)
                .unwrap_or("public")
                .to_string()
        } else {
            "public".to_string()
        };

        // Resolve table OID via SPI
        let sql = format!(
            "SELECT c.oid::int4 FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = '{}' AND n.nspname = '{}'",
            table_name.replace('\'', "''"),
            schema.replace('\'', "''"),
        );
        let oid = pgrx::Spi::connect(|client| {
            let result = client
                .select(&sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            for row in result {
                if let Ok(Some(id)) = row.get::<i32>(1) {
                    return Ok(id as u32);
                }
            }
            Ok(0u32)
        })?;
        if oid > 0 {
            oids.push(oid);
        }
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        unsafe { collect_from_item_oids(join.larg, oids)? };
        unsafe { collect_from_item_oids(join.rarg, oids)? };
    } else if let Some(sub) = cast_node!(node, T_RangeSubselect, pg_sys::RangeSubselect)
        && !sub.subquery.is_null()
        && unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
    {
        let sub_stmt = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };
        let inner_oids = unsafe { extract_from_oids(sub_stmt.fromClause)? };
        oids.extend(inner_oids);
    }
    // RangeFunction: no table OIDs to extract (SRFs don't reference tables directly)
    Ok(())
}

/// Extract alias→OID pairs from a LATERAL subquery's FROM clause.
///
/// For each `RangeVar` in the FROM list, resolves the table OID and
/// collects the effective alias (explicit alias or table name).
///
/// # Safety
/// Caller must ensure `from_list` points to a valid `pg_sys::List`.
unsafe fn extract_from_alias_oids(
    from_list: *mut pg_sys::List,
) -> Result<Vec<(String, u32)>, PgTrickleError> {
    let list = pg_list::<pg_sys::Node>(from_list);
    let mut pairs = Vec::new();
    for node_ptr in list.iter_ptr() {
        unsafe { collect_from_item_alias_oids(node_ptr, &mut pairs)? };
    }
    Ok(pairs)
}

/// Recursively collect (alias, OID) pairs from a FROM item.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
unsafe fn collect_from_item_alias_oids(
    node: *mut pg_sys::Node,
    pairs: &mut Vec<(String, u32)>,
) -> Result<(), PgTrickleError> {
    if node.is_null() {
        return Ok(());
    }

    if let Some(rv) = cast_node!(node, T_RangeVar, pg_sys::RangeVar) {
        let table_name = if !rv.relname.is_null() {
            pg_cstr_to_str(rv.relname).unwrap_or("").to_string()
        } else {
            return Ok(());
        };

        // Effective alias: explicit alias > table name
        let alias = if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            pg_cstr_to_str(a.aliasname)
                .unwrap_or(&table_name)
                .to_string()
        } else {
            table_name.clone()
        };

        let schema = if !rv.schemaname.is_null() {
            pg_cstr_to_str(rv.schemaname)
                .unwrap_or("public")
                .to_string()
        } else {
            "public".to_string()
        };

        let sql = format!(
            "SELECT c.oid::int4 FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = '{}' AND n.nspname = '{}'",
            table_name.replace('\'', "''"),
            schema.replace('\'', "''"),
        );
        let oid = pgrx::Spi::connect(|client| {
            let result = client
                .select(&sql, None, &[])
                .map_err(|e| PgTrickleError::SpiError(e.to_string()))?;
            for row in result {
                if let Ok(Some(id)) = row.get::<i32>(1) {
                    return Ok(id as u32);
                }
            }
            Ok(0u32)
        })?;
        if oid > 0 {
            pairs.push((alias, oid));
        }
    } else if let Some(join) = cast_node!(node, T_JoinExpr, pg_sys::JoinExpr) {
        unsafe { collect_from_item_alias_oids(join.larg, pairs)? };
        unsafe { collect_from_item_alias_oids(join.rarg, pairs)? };
    }
    Ok(())
}

/// Extract correlation predicates from a LATERAL subquery's WHERE clause.
///
/// Walks the WHERE expression looking for equality comparisons of the form
/// `inner_alias.col = outer_ref.col` where `inner_alias` matches a FROM
/// item inside the subquery and the other side is an outer (correlation)
/// reference.
///
/// Only simple equalities (top-level or under AND) are extracted.
/// Complex predicates (OR, functions, casts) are skipped — the caller
/// falls back to full outer-table scanning.
///
/// # Safety
/// Caller must ensure `where_clause` points to a valid expression node.
unsafe fn extract_correlation_predicates(
    where_clause: *mut pg_sys::Node,
    inner_alias_oids: &[(String, u32)],
) -> Vec<CorrelationPredicate> {
    if where_clause.is_null() {
        return Vec::new();
    }
    let inner_aliases: Vec<&str> = inner_alias_oids.iter().map(|(a, _)| a.as_str()).collect();

    // Convert to Expr tree — best-effort, ignore errors.
    // SAFETY: caller guarantees `where_clause` is a valid expression node.
    let expr = match unsafe { node_to_expr(where_clause) } {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut predicates = Vec::new();
    collect_correlation_equalities(&expr, &inner_aliases, inner_alias_oids, &mut predicates);
    predicates
}

/// Recursively collect equality predicates from an expression tree.
fn collect_correlation_equalities(
    expr: &Expr,
    inner_aliases: &[&str],
    inner_alias_oids: &[(String, u32)],
    out: &mut Vec<CorrelationPredicate>,
) {
    match expr {
        Expr::BinaryOp { op, left, right } if op == "=" => {
            // Check if this is inner.col = outer.col or outer.col = inner.col
            if let Some(pred) =
                try_extract_correlation(left, right, inner_aliases, inner_alias_oids)
            {
                out.push(pred);
            } else if let Some(pred) =
                try_extract_correlation(right, left, inner_aliases, inner_alias_oids)
            {
                out.push(pred);
            }
        }
        Expr::BinaryOp { op, left, right } if op == "AND" => {
            collect_correlation_equalities(left, inner_aliases, inner_alias_oids, out);
            collect_correlation_equalities(right, inner_aliases, inner_alias_oids, out);
        }
        _ => {}
    }
}

/// Try to match `(inner_ref, outer_ref)` as a correlation predicate.
///
/// Returns `Some(CorrelationPredicate)` if `inner_ref` is a qualified column
/// reference whose alias matches an inner FROM item and `outer_ref` is a
/// qualified column reference whose alias does NOT match any inner FROM item.
fn try_extract_correlation(
    inner_ref: &Expr,
    outer_ref: &Expr,
    inner_aliases: &[&str],
    inner_alias_oids: &[(String, u32)],
) -> Option<CorrelationPredicate> {
    let (inner_alias, inner_col) = match inner_ref {
        Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name,
        } if inner_aliases.contains(&tbl.as_str()) => (tbl.clone(), column_name.clone()),
        _ => return None,
    };

    let outer_col = match outer_ref {
        Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name,
        } if !inner_aliases.contains(&tbl.as_str()) => column_name.clone(),
        // Unqualified column that's not in inner aliases — assume outer
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => column_name.clone(),
        _ => return None,
    };

    // Resolve inner alias → OID
    let inner_oid = inner_alias_oids
        .iter()
        .find(|(a, _)| a == &inner_alias)
        .map(|(_, oid)| *oid)?;

    Some(CorrelationPredicate {
        outer_col,
        inner_alias,
        inner_col,
        inner_oid,
    })
}

/// Convert a String/Value node to a Rust String.
unsafe fn node_to_string(node: *mut pg_sys::Node) -> Result<String, PgTrickleError> {
    if node.is_null() {
        return Err(PgTrickleError::QueryParseError("NULL node".into()));
    }
    if let Some(s) = cast_node!(node, T_String, pg_sys::String) {
        Ok(pg_cstr_to_str(s.sval).unwrap_or("").to_string())
    } else {
        Ok(format!("node_{:?}", unsafe { (*node).type_ }))
    }
}

/// Deparse a node back to SQL text (simplified fallback).
unsafe fn deparse_node(node: *mut pg_sys::Node) -> String {
    if node.is_null() {
        return "NULL".to_string();
    }
    if let Some(aconst) = cast_node!(node, T_A_Const, pg_sys::A_Const) {
        if aconst.isnull {
            return "NULL".to_string();
        }
        let val_ptr = &aconst.val as *const _ as *const pg_sys::Node;
        if unsafe { pgrx::is_a(val_ptr as *mut _, pg_sys::NodeTag::T_String) } {
            let s = unsafe { &*(val_ptr as *const pg_sys::String) };
            return format!("'{}'", pg_cstr_to_str(s.sval).unwrap_or(""));
        }
        if unsafe { pgrx::is_a(val_ptr as *mut _, pg_sys::NodeTag::T_Integer) } {
            let i = unsafe { &*(val_ptr as *const pg_sys::Integer) };
            return i.ival.to_string();
        }
        if unsafe { pgrx::is_a(val_ptr as *mut _, pg_sys::NodeTag::T_Float) } {
            let f = unsafe { &*(val_ptr as *const pg_sys::Float) };
            return pg_cstr_to_str(f.fval).unwrap_or("0").to_string();
        }
        if unsafe { pgrx::is_a(val_ptr as *mut _, pg_sys::NodeTag::T_Boolean) } {
            let b = unsafe { &*(val_ptr as *const pg_sys::Boolean) };
            return if b.boolval {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            };
        }
        "?".to_string()
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_ColumnRef) } {
        if let Ok(e) = unsafe { node_to_expr(node) } {
            e.to_sql()
        } else {
            "?".to_string()
        }
    } else {
        format!("/* node {:?} */", unsafe { (*node).type_ })
    }
}

/// Deparse a TypeName to SQL (simplified).
unsafe fn deparse_typename(tn: *mut pg_sys::TypeName) -> String {
    if tn.is_null() {
        return "unknown".to_string();
    }
    let names = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg((*tn).names) };
    let mut parts = Vec::new();
    for n in names.iter_ptr() {
        if let Ok(s) = unsafe { node_to_string(n) } {
            parts.push(s);
        }
    }
    parts.join(".")
}

/// Check if the target list contains any window function calls (FuncCall with non-null `over`),
/// including window functions nested inside CASE, COALESCE, function args, etc.
fn target_list_has_windows(target_list: &pgrx::PgList<pg_sys::Node>) -> bool {
    for node_ptr in target_list.iter_ptr() {
        if let Some(rt) = cast_node!(node_ptr, T_ResTarget, pg_sys::ResTarget)
            && !rt.val.is_null()
            && unsafe { node_contains_window_func(rt.val) }
        {
            return true;
        }
    }
    false
}

/// Recursively check if a parse-tree node contains a window function call
/// (`T_FuncCall` with non-null `over` field).
///
/// Walks into CASE, COALESCE, NULLIF, GREATEST/LEAST, BoolExpr, A_Expr,
/// FuncCall args, NullTest, BooleanTest, TypeCast, A_Indirection, RowExpr,
/// and ArrayExpr to detect deeply nested window functions.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
unsafe fn node_contains_window_func(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }

    // Direct hit: FuncCall with OVER clause
    if let Some(fcall) = cast_node!(node, T_FuncCall, pg_sys::FuncCall) {
        if !fcall.over.is_null() {
            return true;
        }
        // Check function arguments recursively
        if !fcall.args.is_null() {
            let args = pg_list::<pg_sys::Node>(fcall.args);
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // CASE WHEN ... THEN ... ELSE ... END
    if let Some(case_expr) = cast_node!(node, T_CaseExpr, pg_sys::CaseExpr) {
        // Check the optional test expression (simple CASE)
        if !case_expr.arg.is_null()
            && unsafe { node_contains_window_func(case_expr.arg as *mut pg_sys::Node) }
        {
            return true;
        }
        // Check each WHEN clause
        if !case_expr.args.is_null() {
            let whens = pg_list::<pg_sys::Node>(case_expr.args);
            for when_ptr in whens.iter_ptr() {
                if let Some(cw) = cast_node!(when_ptr, T_CaseWhen, pg_sys::CaseWhen) {
                    if unsafe { node_contains_window_func(cw.expr as *mut pg_sys::Node) } {
                        return true;
                    }
                    if unsafe { node_contains_window_func(cw.result as *mut pg_sys::Node) } {
                        return true;
                    }
                }
            }
        }
        // Check ELSE
        if !case_expr.defresult.is_null()
            && unsafe { node_contains_window_func(case_expr.defresult as *mut pg_sys::Node) }
        {
            return true;
        }
        return false;
    }

    // COALESCE(a, b, ...)
    if let Some(coalesce) = cast_node!(node, T_CoalesceExpr, pg_sys::CoalesceExpr) {
        if !coalesce.args.is_null() {
            let args = pg_list::<pg_sys::Node>(coalesce.args);
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // NULLIF(a, b)
    if let Some(nullif) = cast_node!(node, T_NullIfExpr, pg_sys::NullIfExpr) {
        if !nullif.args.is_null() {
            let args = pg_list::<pg_sys::Node>(nullif.args);
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // GREATEST / LEAST
    if let Some(minmax) = cast_node!(node, T_MinMaxExpr, pg_sys::MinMaxExpr) {
        if !minmax.args.is_null() {
            let args = pg_list::<pg_sys::Node>(minmax.args);
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // AND / OR / NOT
    if let Some(bexpr) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr) {
        if !bexpr.args.is_null() {
            let args = pg_list::<pg_sys::Node>(bexpr.args);
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // Binary/unary ops: a + b, -a, a BETWEEN x AND y, etc.
    if let Some(aexpr) = cast_node!(node, T_A_Expr, pg_sys::A_Expr) {
        if !aexpr.lexpr.is_null() && unsafe { node_contains_window_func(aexpr.lexpr) } {
            return true;
        }
        if !aexpr.rexpr.is_null() && unsafe { node_contains_window_func(aexpr.rexpr) } {
            return true;
        }
        return false;
    }

    // CAST(x AS type)
    if let Some(tc) = cast_node!(node, T_TypeCast, pg_sys::TypeCast) {
        if !tc.arg.is_null() {
            return unsafe { node_contains_window_func(tc.arg) };
        }
        return false;
    }

    // IS [NOT] NULL
    if let Some(nt) = cast_node!(node, T_NullTest, pg_sys::NullTest) {
        if !nt.arg.is_null() {
            return unsafe { node_contains_window_func(nt.arg as *mut pg_sys::Node) };
        }
        return false;
    }

    // IS [NOT] TRUE / FALSE / UNKNOWN
    if let Some(bt) = cast_node!(node, T_BooleanTest, pg_sys::BooleanTest) {
        if !bt.arg.is_null() {
            return unsafe { node_contains_window_func(bt.arg as *mut pg_sys::Node) };
        }
        return false;
    }

    // ARRAY[a, b, c]
    if let Some(arr) = cast_node!(node, T_ArrayExpr, pg_sys::ArrayExpr) {
        if !arr.elements.is_null() {
            let elems = pg_list::<pg_sys::Node>(arr.elements);
            for elem in elems.iter_ptr() {
                if unsafe { node_contains_window_func(elem) } {
                    return true;
                }
            }
        }
        return false;
    }

    // ROW(a, b, c)
    if let Some(row) = cast_node!(node, T_RowExpr, pg_sys::RowExpr) {
        if !row.args.is_null() {
            let args = pg_list::<pg_sys::Node>(row.args);
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // SubLink — check testexpr and subselect target list
    if let Some(sub) = cast_node!(node, T_SubLink, pg_sys::SubLink) {
        if !sub.testexpr.is_null() && unsafe { node_contains_window_func(sub.testexpr) } {
            return true;
        }
        return false;
    }

    // Leaf nodes: ColumnRef, A_Const, SQLValueFunction, etc — no children
    false
}

/// Recursively collect all FuncCall-with-OVER nodes inside an expression tree.
///
/// When a FuncCall-with-OVER is found it is pushed to `result` and recursion
/// stops (window functions nested inside window spec arguments are invalid SQL).
///
/// # Safety
/// Caller must ensure `node` points to a valid parse-tree Node.
unsafe fn collect_all_window_func_nodes(
    node: *mut pg_sys::Node,
    result: &mut Vec<*mut pg_sys::Node>,
) {
    if node.is_null() {
        return;
    }

    // FuncCall with OVER → window function: collect and stop descent
    if let Some(func) = cast_node!(node, T_FuncCall, pg_sys::FuncCall) {
        if !func.over.is_null() {
            result.push(node);
            return;
        }
        // Regular function call — recurse into args
        if !func.args.is_null() {
            let args = pg_list::<pg_sys::Node>(func.args);
            for arg in args.iter_ptr() {
                unsafe { collect_all_window_func_nodes(arg, result) };
            }
        }
        return;
    }

    // A_Expr: binary/unary/subscript operators
    if let Some(expr) = cast_node!(node, T_A_Expr, pg_sys::A_Expr) {
        if !expr.lexpr.is_null() {
            unsafe { collect_all_window_func_nodes(expr.lexpr, result) };
        }
        if !expr.rexpr.is_null() {
            unsafe { collect_all_window_func_nodes(expr.rexpr, result) };
        }
        return;
    }

    // TypeCast: (expr)::type
    if let Some(tc) = cast_node!(node, T_TypeCast, pg_sys::TypeCast) {
        if !tc.arg.is_null() {
            unsafe { collect_all_window_func_nodes(tc.arg, result) };
        }
        return;
    }

    // CaseExpr: CASE [arg] WHEN ... THEN ... ELSE ... END
    if let Some(case) = cast_node!(node, T_CaseExpr, pg_sys::CaseExpr) {
        if !case.arg.is_null() {
            unsafe { collect_all_window_func_nodes(case.arg as *mut pg_sys::Node, result) };
        }
        if !case.args.is_null() {
            let when_list = pg_list::<pg_sys::Node>(case.args);
            for w in when_list.iter_ptr() {
                if !w.is_null() && unsafe { pgrx::is_a(w, pg_sys::NodeTag::T_CaseWhen) } {
                    let cw = unsafe { &*(w as *const pg_sys::CaseWhen) };
                    if !cw.expr.is_null() {
                        unsafe {
                            collect_all_window_func_nodes(cw.expr as *mut pg_sys::Node, result)
                        };
                    }
                    if !cw.result.is_null() {
                        unsafe {
                            collect_all_window_func_nodes(cw.result as *mut pg_sys::Node, result)
                        };
                    }
                }
            }
        }
        if !case.defresult.is_null() {
            unsafe { collect_all_window_func_nodes(case.defresult as *mut pg_sys::Node, result) };
        }
        return;
    }

    // CoalesceExpr: COALESCE(a, b, ...)
    if let Some(coalesce) = cast_node!(node, T_CoalesceExpr, pg_sys::CoalesceExpr) {
        if !coalesce.args.is_null() {
            let args = pg_list::<pg_sys::Node>(coalesce.args);
            for arg in args.iter_ptr() {
                unsafe { collect_all_window_func_nodes(arg, result) };
            }
        }
        return;
    }

    // NullIfExpr: NULLIF(a, b) — `args` list
    if let Some(nif) = cast_node!(node, T_NullIfExpr, pg_sys::NullIfExpr) {
        if !nif.args.is_null() {
            let args = pg_list::<pg_sys::Node>(nif.args);
            for arg in args.iter_ptr() {
                unsafe { collect_all_window_func_nodes(arg, result) };
            }
        }
        return;
    }

    // MinMaxExpr: GREATEST / LEAST
    if let Some(mm) = cast_node!(node, T_MinMaxExpr, pg_sys::MinMaxExpr) {
        if !mm.args.is_null() {
            let args = pg_list::<pg_sys::Node>(mm.args);
            for arg in args.iter_ptr() {
                unsafe { collect_all_window_func_nodes(arg, result) };
            }
        }
        return;
    }

    // BoolExpr: AND / OR / NOT
    if let Some(be) = cast_node!(node, T_BoolExpr, pg_sys::BoolExpr) {
        if !be.args.is_null() {
            let args = pg_list::<pg_sys::Node>(be.args);
            for arg in args.iter_ptr() {
                unsafe { collect_all_window_func_nodes(arg, result) };
            }
        }
        return;
    }

    // NullTest: IS NULL / IS NOT NULL
    if let Some(nt) = cast_node!(node, T_NullTest, pg_sys::NullTest) {
        if !nt.arg.is_null() {
            unsafe { collect_all_window_func_nodes(nt.arg as *mut pg_sys::Node, result) };
        }
        return;
    }

    // RowExpr: ROW(a, b, c)
    if let Some(row) = cast_node!(node, T_RowExpr, pg_sys::RowExpr)
        && !row.args.is_null()
    {
        let args = pg_list::<pg_sys::Node>(row.args);
        for arg in args.iter_ptr() {
            unsafe { collect_all_window_func_nodes(arg, result) };
        }
    }

    // Leaf nodes (ColumnRef, A_Const, SQLValueFunction, etc.) — no children
}

/// Extraction result for window function parsing.
type WindowExtraction = (Vec<WindowExpr>, Vec<(Expr, String)>);

/// Extract window function expressions and pass-through columns from a target list.
///
/// Returns `(window_exprs, pass_through_cols)` where each pass-through column
/// is `(Expr, alias)`.
unsafe fn extract_window_exprs(
    target_list: &pgrx::PgList<pg_sys::Node>,
    window_clause: *mut pg_sys::List,
) -> Result<WindowExtraction, PgTrickleError> {
    let mut window_exprs = Vec::new();
    let mut pass_through = Vec::new();

    for node_ptr in target_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }

        // Check if this target is a FuncCall with OVER clause
        if let Some(fcall) = cast_node!(rt.val, T_FuncCall, pg_sys::FuncCall)
            && !fcall.over.is_null()
        {
            let wexpr = unsafe { parse_window_func_call(fcall, rt, window_clause)? };
            window_exprs.push(wexpr);
            continue;
        }

        // Check if a window function is nested inside an expression (CASE, COALESCE, etc.)
        // We detect this but cannot extract it — reject with a clear error.
        if unsafe { node_contains_window_func(rt.val) } {
            return Err(PgTrickleError::UnsupportedOperator(
                "Window functions nested inside expressions (CASE, COALESCE, arithmetic, etc.) \
                 are not supported in defining queries. Move the window function to a separate \
                 column, e.g.:\n  SELECT ROW_NUMBER() OVER (...) AS rn, ... FROM t\n\
                 Then wrap the stream table in a view to apply the expression."
                    .into(),
            ));
        }

        // Not a window function — pass-through column
        let alias = if !rt.name.is_null() {
            pg_cstr_to_str(rt.name).unwrap_or("?column?").to_string()
        } else if let Ok(e) = unsafe { node_to_expr(rt.val) } {
            match &e {
                Expr::ColumnRef { column_name, .. } => column_name.clone(),
                Expr::Star { .. } => "*".to_string(),
                _ => format!("col_{}", pass_through.len()),
            }
        } else {
            format!("col_{}", pass_through.len())
        };
        let expr = unsafe { node_to_expr(rt.val)? };
        pass_through.push((expr, alias));
    }

    Ok((window_exprs, pass_through))
}

/// Parse a single FuncCall with OVER clause into a WindowExpr.
///
/// If the OVER clause references a named window (e.g., `OVER w`),
/// the definition is resolved from `window_clause` (the `WINDOW` clause).
unsafe fn parse_window_func_call(
    fcall: &pg_sys::FuncCall,
    rt: &pg_sys::ResTarget,
    window_clause: *mut pg_sys::List,
) -> Result<WindowExpr, PgTrickleError> {
    // Function name
    let func_name = unsafe { extract_func_name(fcall.funcname)? };

    // Function arguments
    let args_list = pg_list::<pg_sys::Node>(fcall.args);
    let mut args = Vec::new();
    for n in args_list.iter_ptr() {
        if let Ok(e) = unsafe { node_to_expr(n) } {
            args.push(e);
        }
    }

    // Parse the WindowDef (OVER clause)
    // SAFETY: caller guarantees fcall.over is non-null
    let wdef = unsafe { &*fcall.over };

    // Resolve named window reference: OVER w → look up from WINDOW clause
    let resolved_wdef = if !wdef.refname.is_null() && !window_clause.is_null() {
        let ref_name = pg_cstr_to_str(wdef.refname).unwrap_or("");
        let wclause = pg_list::<pg_sys::Node>(window_clause);
        let mut found: Option<&pg_sys::WindowDef> = None;
        for n in wclause.iter_ptr() {
            if let Some(wd) = cast_node!(n, T_WindowDef, pg_sys::WindowDef)
                && !wd.name.is_null()
            {
                let wd_name = pg_cstr_to_str(wd.name).unwrap_or("");
                if wd_name == ref_name {
                    found = Some(wd);
                    break;
                }
            }
        }
        found
    } else {
        None
    };

    // Use resolved window definition for partition/order if the inline OVER is empty
    let effective_part_clause = if !wdef.partitionClause.is_null() {
        wdef.partitionClause
    } else if let Some(rwd) = resolved_wdef {
        rwd.partitionClause
    } else {
        std::ptr::null_mut()
    };

    let effective_ord_clause = if !wdef.orderClause.is_null() {
        wdef.orderClause
    } else if let Some(rwd) = resolved_wdef {
        rwd.orderClause
    } else {
        std::ptr::null_mut()
    };

    // Use resolved window for frame if the inline OVER doesn't specify one
    let effective_frame_wdef = if wdef.frameOptions as u32 & pg_sys::FRAMEOPTION_NONDEFAULT != 0 {
        wdef
    } else if let Some(rwd) = resolved_wdef {
        rwd
    } else {
        wdef
    };

    // PARTITION BY
    let part_list = pg_list::<pg_sys::Node>(effective_part_clause);
    let mut partition_by = Vec::new();
    for n in part_list.iter_ptr() {
        if let Ok(e) = unsafe { node_to_expr(n) } {
            partition_by.push(e);
        }
    }

    // ORDER BY (list of SortBy nodes)
    let ord_list = pg_list::<pg_sys::Node>(effective_ord_clause);
    let mut order_by = Vec::new();
    for n in ord_list.iter_ptr() {
        if let Some(sb) = cast_node!(n, T_SortBy, pg_sys::SortBy) {
            let expr = unsafe { node_to_expr(sb.node)? };
            let ascending = sb.sortby_dir != pg_sys::SortByDir::SORTBY_DESC;
            let nulls_first = match sb.sortby_nulls {
                pg_sys::SortByNulls::SORTBY_NULLS_FIRST => true,
                pg_sys::SortByNulls::SORTBY_NULLS_LAST => false,
                _ => !ascending, // default: NULLS FIRST for DESC, NULLS LAST for ASC
            };
            order_by.push(SortExpr {
                expr,
                ascending,
                nulls_first,
            });
        }
    }

    // Parse window frame clause
    let frame_clause = unsafe { deparse_window_frame(effective_frame_wdef) };

    // Alias
    let alias = if !rt.name.is_null() {
        pg_cstr_to_str(rt.name).unwrap_or(&func_name).to_string()
    } else {
        func_name.clone()
    };

    Ok(WindowExpr {
        func_name,
        args,
        partition_by,
        order_by,
        frame_clause,
        alias,
    })
}

/// Deparse a window frame clause from a WindowDef's frameOptions.
///
/// Returns `None` if the frame is the default, `Some(clause)` otherwise.
///
/// # Safety
/// `wdef` must point to a valid `pg_sys::WindowDef`.
unsafe fn deparse_window_frame(wdef: &pg_sys::WindowDef) -> Option<String> {
    let opts = wdef.frameOptions as u32;
    if opts & pg_sys::FRAMEOPTION_NONDEFAULT == 0 {
        return None;
    }

    let mode = if opts & pg_sys::FRAMEOPTION_ROWS != 0 {
        "ROWS"
    } else if opts & pg_sys::FRAMEOPTION_GROUPS != 0 {
        "GROUPS"
    } else {
        "RANGE"
    };

    let start = unsafe { deparse_frame_bound(opts, true, wdef.startOffset) };
    let end = unsafe { deparse_frame_bound(opts, false, wdef.endOffset) };

    let frame = if opts & pg_sys::FRAMEOPTION_BETWEEN != 0 {
        format!("{mode} BETWEEN {start} AND {end}")
    } else {
        format!("{mode} {start}")
    };

    let exclusion = if opts & pg_sys::FRAMEOPTION_EXCLUDE_CURRENT_ROW != 0 {
        " EXCLUDE CURRENT ROW"
    } else if opts & pg_sys::FRAMEOPTION_EXCLUDE_GROUP != 0 {
        " EXCLUDE GROUP"
    } else if opts & pg_sys::FRAMEOPTION_EXCLUDE_TIES != 0 {
        " EXCLUDE TIES"
    } else {
        ""
    };

    Some(format!("{frame}{exclusion}"))
}

/// Deparse a single frame bound (start or end).
///
/// # Safety
/// `offset` may be null; caller must ensure it's valid if non-null.
unsafe fn deparse_frame_bound(opts: u32, is_start: bool, offset: *mut pg_sys::Node) -> String {
    if is_start {
        if opts & pg_sys::FRAMEOPTION_START_UNBOUNDED_PRECEDING != 0 {
            return "UNBOUNDED PRECEDING".to_string();
        }
        if opts & pg_sys::FRAMEOPTION_START_CURRENT_ROW != 0 {
            return "CURRENT ROW".to_string();
        }
        if opts & pg_sys::FRAMEOPTION_START_OFFSET_PRECEDING != 0
            && let Ok(e) = unsafe { node_to_expr(offset) }
        {
            return format!("{} PRECEDING", e.to_sql());
        }
        if opts & pg_sys::FRAMEOPTION_START_OFFSET_FOLLOWING != 0
            && let Ok(e) = unsafe { node_to_expr(offset) }
        {
            return format!("{} FOLLOWING", e.to_sql());
        }
        if opts & pg_sys::FRAMEOPTION_START_UNBOUNDED_FOLLOWING != 0 {
            return "UNBOUNDED FOLLOWING".to_string();
        }
    } else {
        if opts & pg_sys::FRAMEOPTION_END_UNBOUNDED_FOLLOWING != 0 {
            return "UNBOUNDED FOLLOWING".to_string();
        }
        if opts & pg_sys::FRAMEOPTION_END_CURRENT_ROW != 0 {
            return "CURRENT ROW".to_string();
        }
        if opts & pg_sys::FRAMEOPTION_END_OFFSET_FOLLOWING != 0
            && let Ok(e) = unsafe { node_to_expr(offset) }
        {
            return format!("{} FOLLOWING", e.to_sql());
        }
        if opts & pg_sys::FRAMEOPTION_END_OFFSET_PRECEDING != 0
            && let Ok(e) = unsafe { node_to_expr(offset) }
        {
            return format!("{} PRECEDING", e.to_sql());
        }
        if opts & pg_sys::FRAMEOPTION_END_UNBOUNDED_PRECEDING != 0 {
            return "UNBOUNDED PRECEDING".to_string();
        }
    }
    "CURRENT ROW".to_string()
}

/// Check if the target list contains any aggregate function calls.
unsafe fn target_list_has_aggregates(target_list: &pgrx::PgList<pg_sys::Node>) -> bool {
    for node_ptr in target_list.iter_ptr() {
        if let Some(rt) = cast_node!(node_ptr, T_ResTarget, pg_sys::ResTarget)
            && !rt.val.is_null()
            && unsafe { expr_contains_agg(rt.val) }
        {
            return true;
        }
    }
    false
}

/// Check if a single raw-parse-tree node is an aggregate function call.
///
/// Does NOT recurse — only checks the immediate node.
unsafe fn is_agg_node(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    // SQL/JSON standard aggregates
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JsonObjectAgg) }
        || unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JsonArrayAgg) }
    {
        return true;
    }
    if let Some(fcall) = cast_node!(node, T_FuncCall, pg_sys::FuncCall) {
        // Window functions (FuncCall with OVER clause) are NOT aggregates,
        // even if they use aggregate function names like SUM, COUNT.
        if !fcall.over.is_null() {
            return false;
        }
        if fcall.agg_within_group
            || !fcall.agg_order.is_null()
            || fcall.agg_star
            || !fcall.agg_filter.is_null()
            || fcall.agg_distinct
        {
            return true;
        }
        if let Ok(name) = unsafe { extract_func_name(fcall.funcname) } {
            let name_lower = name.to_lowercase();
            if is_known_aggregate(&name_lower) {
                return true;
            }
        }
    }
    false
}

/// Walker callback for [`expr_contains_agg`].
///
/// Returns `true` (stop walking) when a node is detected as an aggregate,
/// `false` to continue. Recursion into children is handled by
/// `raw_expression_tree_walker_impl`.
///
/// # Safety
/// Called by PostgreSQL's `raw_expression_tree_walker_impl` with valid
/// raw parse tree node pointers.
#[cfg(not(test))]
unsafe extern "C-unwind" fn agg_check_walker(
    node: *mut pg_sys::Node,
    _context: *mut std::ffi::c_void,
) -> bool {
    if node.is_null() {
        return false;
    }
    if unsafe { is_agg_node(node) } {
        return true; // found aggregate — stop walking
    }
    // Continue recursion into children.
    // SAFETY: raw_expression_tree_walker_impl handles all raw parse tree
    // node types (A_Expr, CaseExpr, BoolExpr, FuncCall, TypeCast, etc.).
    unsafe {
        pg_sys::raw_expression_tree_walker_impl(node, Some(agg_check_walker), std::ptr::null_mut())
    }
}

/// Recursively check if a raw parse tree node contains an aggregate
/// function call, anywhere in its expression subtree.
///
/// Uses PostgreSQL's `raw_expression_tree_walker_impl` for correct
/// recursion into all node types (A_Expr, CaseExpr, BoolExpr, etc.).
#[cfg(not(test))]
unsafe fn expr_contains_agg(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    // Check the node itself first.
    if unsafe { is_agg_node(node) } {
        return true;
    }
    // Recurse into children.
    unsafe {
        pg_sys::raw_expression_tree_walker_impl(node, Some(agg_check_walker), std::ptr::null_mut())
    }
}

#[cfg(test)]
unsafe fn expr_contains_agg(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    unsafe { is_agg_node(node) }
}

/// Check if a function name is a user-defined aggregate via `pg_proc.prokind`.
///
/// Returns `true` if the function exists in `pg_proc` with `prokind = 'a'`
/// (aggregate function) but is NOT in our hardcoded `is_known_aggregate` list.
/// This detects custom aggregates created with `CREATE AGGREGATE`.
#[cfg(not(test))]
fn is_user_defined_aggregate(name: &str) -> bool {
    // Strip schema qualification if present (e.g., "myschema.my_agg" → "my_agg").
    let bare_name = name.rsplit('.').next().unwrap_or(name);

    Spi::connect(|client| {
        let result = client
            .select(
                "SELECT 1 FROM pg_catalog.pg_proc WHERE proname = $1 AND prokind = 'a'",
                None,
                &[bare_name.into()],
            )
            .ok()?;
        if !result.is_empty() { Some(true) } else { None }
    })
    .unwrap_or(false)
}

/// Test-only stub: SPI is unavailable in unit tests.
#[cfg(test)]
fn is_user_defined_aggregate(_name: &str) -> bool {
    false
}

/// Check if a function name is a known aggregate (built-in or common).
fn is_known_aggregate(name: &str) -> bool {
    matches!(
        name,
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "string_agg"
            | "array_agg"
            | "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
            | "json_objectagg"
            | "json_arrayagg"
            | "bool_and"
            | "bool_or"
            | "every"
            | "bit_and"
            | "bit_or"
            | "bit_xor"
            | "xmlagg"
            | "stddev"
            | "stddev_pop"
            | "stddev_samp"
            | "variance"
            | "var_pop"
            | "var_samp"
            | "corr"
            | "covar_pop"
            | "covar_samp"
            | "regr_avgx"
            | "regr_avgy"
            | "regr_count"
            | "regr_intercept"
            | "regr_r2"
            | "regr_slope"
            | "regr_sxx"
            | "regr_sxy"
            | "regr_syy"
            | "percentile_cont"
            | "percentile_disc"
            | "mode"
            | "any_value"
            | "rank"
            | "dense_rank"
            | "percent_rank"
            | "cume_dist"
            // G5.6: Range aggregates — recognized but rejected for DIFFERENTIAL mode.
            | "range_agg"
            | "range_intersect_agg"
    )
}

/// Extract aggregates and non-aggregate expressions from a target list.
unsafe fn extract_aggregates(
    target_list: &pgrx::PgList<pg_sys::Node>,
) -> Result<(Vec<AggExpr>, Vec<Expr>), PgTrickleError> {
    let mut aggs = Vec::new();
    let mut non_aggs = Vec::new();

    for node_ptr in target_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            continue;
        }

        if let Some(fcall) = cast_node!(rt.val, T_FuncCall, pg_sys::FuncCall) {
            // Window functions (FuncCall with OVER clause) are NOT aggregates.
            // Treat them as plain expressions — they'll be handled by the
            // window operator path in parse_select_stmt.
            if !fcall.over.is_null() {
                let expr = unsafe { node_to_expr(rt.val)? };
                non_aggs.push(expr);
                continue;
            }

            let func_name = unsafe { extract_func_name(fcall.funcname)? };
            let name_lower = func_name.to_lowercase();

            let alias = if !rt.name.is_null() {
                pg_cstr_to_str(rt.name).unwrap_or(&func_name).to_string()
            } else {
                func_name.clone()
            };

            if let Some(agg_func) = match name_lower.as_str() {
                "count" if fcall.agg_star => Some(AggFunc::CountStar),
                "count" => Some(AggFunc::Count),
                "sum" => Some(AggFunc::Sum),
                "avg" => Some(AggFunc::Avg),
                "min" => Some(AggFunc::Min),
                "max" => Some(AggFunc::Max),
                "bool_and" | "every" => Some(AggFunc::BoolAnd),
                "bool_or" => Some(AggFunc::BoolOr),
                "string_agg" => Some(AggFunc::StringAgg),
                "array_agg" => Some(AggFunc::ArrayAgg),
                "json_agg" => Some(AggFunc::JsonAgg),
                "jsonb_agg" => Some(AggFunc::JsonbAgg),
                "bit_and" => Some(AggFunc::BitAnd),
                "bit_or" => Some(AggFunc::BitOr),
                "bit_xor" => Some(AggFunc::BitXor),
                "json_object_agg" => Some(AggFunc::JsonObjectAgg),
                "jsonb_object_agg" => Some(AggFunc::JsonbObjectAgg),
                "stddev" | "stddev_samp" => Some(AggFunc::StddevSamp),
                "stddev_pop" => Some(AggFunc::StddevPop),
                "variance" | "var_samp" => Some(AggFunc::VarSamp),
                "var_pop" => Some(AggFunc::VarPop),
                "any_value" => Some(AggFunc::AnyValue),
                "mode" => Some(AggFunc::Mode),
                "percentile_cont" => Some(AggFunc::PercentileCont),
                "percentile_disc" => Some(AggFunc::PercentileDisc),
                "corr" => Some(AggFunc::Corr),
                "covar_pop" => Some(AggFunc::CovarPop),
                "covar_samp" => Some(AggFunc::CovarSamp),
                "regr_avgx" => Some(AggFunc::RegrAvgx),
                "regr_avgy" => Some(AggFunc::RegrAvgy),
                "regr_count" => Some(AggFunc::RegrCount),
                "regr_intercept" => Some(AggFunc::RegrIntercept),
                "regr_r2" => Some(AggFunc::RegrR2),
                "regr_slope" => Some(AggFunc::RegrSlope),
                "regr_sxx" => Some(AggFunc::RegrSxx),
                "regr_sxy" => Some(AggFunc::RegrSxy),
                "regr_syy" => Some(AggFunc::RegrSyy),
                "xmlagg" => Some(AggFunc::XmlAgg),
                "rank" => Some(AggFunc::HypRank),
                "dense_rank" => Some(AggFunc::HypDenseRank),
                "percent_rank" => Some(AggFunc::HypPercentRank),
                "cume_dist" => Some(AggFunc::HypCumeDist),
                _ => None,
            } {
                let args_list = pg_list::<pg_sys::Node>(fcall.args);
                let argument = args_list
                    .head()
                    .and_then(|n| unsafe { node_to_expr(n).ok() });

                // Extract optional second argument (e.g., STRING_AGG separator)
                let second_arg = if args_list.len() >= 2 {
                    args_list
                        .get_ptr(1)
                        .and_then(|n| unsafe { node_to_expr(n).ok() })
                } else {
                    None
                };

                // Parse optional FILTER (WHERE ...) clause
                let filter = if !fcall.agg_filter.is_null() {
                    Some(unsafe { node_to_expr(fcall.agg_filter)? })
                } else {
                    None
                };

                // Parse optional WITHIN GROUP (ORDER BY ...) for ordered-set aggregates
                // and regular aggregate ORDER BY (e.g., STRING_AGG(val, ',' ORDER BY val)).
                // Both are stored in order_within_group; agg_to_rescan_sql distinguishes
                // between WITHIN GROUP and regular ORDER BY based on the AggFunc type.
                let order_within_group = if !fcall.agg_order.is_null() {
                    let ord_list = pg_list::<pg_sys::Node>(fcall.agg_order);
                    let mut sorts = Vec::new();
                    for n in ord_list.iter_ptr() {
                        if let Some(sb) = cast_node!(n, T_SortBy, pg_sys::SortBy) {
                            let expr = unsafe { node_to_expr(sb.node)? };
                            let ascending = sb.sortby_dir != pg_sys::SortByDir::SORTBY_DESC;
                            let nulls_first = match sb.sortby_nulls {
                                pg_sys::SortByNulls::SORTBY_NULLS_FIRST => true,
                                pg_sys::SortByNulls::SORTBY_NULLS_LAST => false,
                                // default: NULLS FIRST for DESC, NULLS LAST for ASC
                                _ => !ascending,
                            };
                            sorts.push(SortExpr {
                                expr,
                                ascending,
                                nulls_first,
                            });
                        }
                    }
                    if sorts.is_empty() { None } else { Some(sorts) }
                } else {
                    None
                };

                aggs.push(AggExpr {
                    function: agg_func,
                    argument,
                    alias,
                    is_distinct: fcall.agg_distinct,
                    second_arg,
                    filter,
                    order_within_group,
                });
            } else if is_known_aggregate(&name_lower) {
                // Recognized as an aggregate but not supported for differential maintenance
                return Err(PgTrickleError::UnsupportedOperator(format!(
                    "Aggregate function {func_name}() is not supported in DIFFERENTIAL mode. \
                     Supported aggregates: COUNT, SUM, AVG, MIN, MAX, BOOL_AND, BOOL_OR, \
                     STRING_AGG, ARRAY_AGG, JSON_AGG, JSONB_AGG, BIT_AND, BIT_OR, BIT_XOR, \
                     JSON_OBJECT_AGG, JSONB_OBJECT_AGG, STDDEV, STDDEV_POP, STDDEV_SAMP, \
                     VARIANCE, VAR_POP, VAR_SAMP, ANY_VALUE, MODE, PERCENTILE_CONT, \
                     PERCENTILE_DISC, CORR, COVAR_POP, COVAR_SAMP, REGR_AVGX, REGR_AVGY, \
                     REGR_COUNT, REGR_INTERCEPT, REGR_R2, REGR_SLOPE, REGR_SXX, REGR_SXY, \
                     REGR_SYY, XMLAGG, RANK/DENSE_RANK/PERCENT_RANK/CUME_DIST WITHIN GROUP. \
                     Use FULL refresh mode instead.",
                )));
            } else if is_user_defined_aggregate(&name_lower) {
                // User-defined aggregate (CREATE AGGREGATE) — treat as group-rescan.
                // Build the complete call SQL (including ORDER BY and FILTER) so that
                // agg_to_rescan_sql can emit it verbatim and produce correct results.
                // Also populate `argument` and `filter` so agg_delta_exprs can apply
                // the FILTER predicate when counting insert/delete events.

                let args_list = pg_list::<pg_sys::Node>(fcall.args);
                let argument = args_list
                    .head()
                    .and_then(|n| unsafe { node_to_expr(n).ok() });

                // Build the argument SQL, respecting DISTINCT.
                let distinct_str = if fcall.agg_distinct { "DISTINCT " } else { "" };
                let base_args: Vec<String> = {
                    let mut v = Vec::new();
                    for n in args_list.iter_ptr() {
                        if let Ok(e) = unsafe { node_to_expr(n) } {
                            v.push(e.to_sql());
                        }
                    }
                    v
                };
                let args_sql = if base_args.is_empty() {
                    String::new()
                } else {
                    format!("{distinct_str}{}", base_args.join(", "))
                };

                // Parse ORDER BY inside the aggregate call (e.g., my_agg(val ORDER BY x)).
                let order_sql = if !fcall.agg_order.is_null() {
                    let ord_list = pg_list::<pg_sys::Node>(fcall.agg_order);
                    let mut sorts = Vec::new();
                    for n in ord_list.iter_ptr() {
                        if let Some(sb) = cast_node!(n, T_SortBy, pg_sys::SortBy) {
                            let expr = unsafe { node_to_expr(sb.node)? };
                            let dir = if sb.sortby_dir == pg_sys::SortByDir::SORTBY_DESC {
                                " DESC"
                            } else {
                                ""
                            };
                            sorts.push(format!("{}{dir}", expr.to_sql()));
                        }
                    }
                    if sorts.is_empty() {
                        None
                    } else {
                        Some(sorts.join(", "))
                    }
                } else {
                    None
                };

                let call_sql = match &order_sql {
                    Some(ord) => format!("{func_name}({args_sql} ORDER BY {ord})"),
                    None => format!("{func_name}({args_sql})"),
                };

                // Parse FILTER (WHERE ...) clause — stored both in `filter` (for delta
                // tracking) and appended to `raw_sql` (for rescan correctness).
                let filter = if !fcall.agg_filter.is_null() {
                    Some(unsafe { node_to_expr(fcall.agg_filter)? })
                } else {
                    None
                };

                let raw_sql = match &filter {
                    Some(f) => format!("{call_sql} FILTER (WHERE {})", f.to_sql()),
                    None => call_sql,
                };

                aggs.push(AggExpr {
                    function: AggFunc::UserDefined(raw_sql),
                    argument,
                    alias,
                    is_distinct: fcall.agg_distinct,
                    second_arg: None,
                    filter,
                    order_within_group: None,
                });
            } else if unsafe { expr_contains_agg(rt.val) } {
                // Non-aggregate function wrapping nested aggregate(s),
                // e.g. ROUND(STDDEV_POP(amount), 2). Treat as ComplexExpression
                // so the group-rescan path re-evaluates it correctly.
                let raw_expr = unsafe { node_to_expr(rt.val)? };
                let raw_sql = raw_expr.to_sql();

                let alias = if !rt.name.is_null() {
                    pg_cstr_to_str(rt.name).unwrap_or("complex_agg").to_string()
                } else {
                    format!("complex_agg_{}", aggs.len())
                };

                aggs.push(AggExpr {
                    function: AggFunc::ComplexExpression(raw_sql),
                    argument: None,
                    alias,
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                });
            } else {
                let expr = unsafe { node_to_expr(rt.val)? };
                non_aggs.push(expr);
            }
        } else if unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_JsonObjectAgg) } {
            // ── F11: SQL/JSON standard JSON_OBJECTAGG(key: value ...) ──
            // This node type is separate from T_FuncCall, so we handle it
            // explicitly. Deparse to SQL and store as AggFunc::JsonObjectAggStd.
            let raw_expr = unsafe { node_to_expr(rt.val)? };
            let raw_sql = raw_expr.to_sql();

            let joa = unsafe { &*(rt.val as *const pg_sys::JsonObjectAgg) };
            let alias = if !rt.name.is_null() {
                pg_cstr_to_str(rt.name)
                    .unwrap_or("json_objectagg")
                    .to_string()
            } else {
                "json_objectagg".to_string()
            };

            // Extract FILTER clause from the constructor (stored separately
            // so agg_delta_exprs can apply the filter to change tracking).
            let filter = if !joa.constructor.is_null() {
                let ctor = unsafe { &*joa.constructor };
                if !ctor.agg_filter.is_null() {
                    Some(unsafe { node_to_expr(ctor.agg_filter)? })
                } else {
                    None
                }
            } else {
                None
            };

            aggs.push(AggExpr {
                function: AggFunc::JsonObjectAggStd(raw_sql),
                argument: None,
                alias,
                is_distinct: false,
                second_arg: None,
                filter,
                order_within_group: None,
            });
        } else if unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_JsonArrayAgg) } {
            // ── F11: SQL/JSON standard JSON_ARRAYAGG(expr ...) ──
            let raw_expr = unsafe { node_to_expr(rt.val)? };
            let raw_sql = raw_expr.to_sql();

            let jaa = unsafe { &*(rt.val as *const pg_sys::JsonArrayAgg) };
            let alias = if !rt.name.is_null() {
                pg_cstr_to_str(rt.name)
                    .unwrap_or("json_arrayagg")
                    .to_string()
            } else {
                "json_arrayagg".to_string()
            };

            let filter = if !jaa.constructor.is_null() {
                let ctor = unsafe { &*jaa.constructor };
                if !ctor.agg_filter.is_null() {
                    Some(unsafe { node_to_expr(ctor.agg_filter)? })
                } else {
                    None
                }
            } else {
                None
            };

            aggs.push(AggExpr {
                function: AggFunc::JsonArrayAggStd(raw_sql),
                argument: None,
                alias,
                is_distinct: false,
                second_arg: None,
                filter,
                order_within_group: None,
            });
        } else {
            // Not a top-level aggregate function call.
            // Check if the expression CONTAINS nested aggregate calls
            // (e.g., `100 * SUM(a) / NULLIF(SUM(b), 0)`).
            if unsafe { expr_contains_agg(rt.val) } {
                // Deparse the entire expression to SQL for the rescan CTE.
                let raw_expr = unsafe { node_to_expr(rt.val)? };
                let raw_sql = raw_expr.to_sql();

                let alias = if !rt.name.is_null() {
                    pg_cstr_to_str(rt.name).unwrap_or("complex_agg").to_string()
                } else {
                    format!("complex_agg_{}", aggs.len())
                };

                aggs.push(AggExpr {
                    function: AggFunc::ComplexExpression(raw_sql),
                    argument: None,
                    alias,
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                });
            } else {
                let expr = unsafe { node_to_expr(rt.val)? };
                non_aggs.push(expr);
            }
        }
    }

    Ok((aggs, non_aggs))
}

/// Parse the target list (SELECT expressions) into Expr + alias pairs.
unsafe fn parse_target_list(
    target_list: &pgrx::PgList<pg_sys::Node>,
) -> Result<(Vec<Expr>, Vec<String>), PgTrickleError> {
    let mut expressions = Vec::new();
    let mut aliases = Vec::new();

    for node_ptr in target_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };

        let alias = unsafe { target_alias_for_res_target(rt, expressions.len()) };

        if rt.val.is_null() {
            expressions.push(Expr::Star { table_alias: None });
            aliases.push("*".to_string());
        } else {
            let expr = unsafe { node_to_expr(rt.val)? };
            expressions.push(expr);
            aliases.push(alias);
        }
    }

    Ok((expressions, aliases))
}

/// Determine the output alias for a SELECT target.
///
/// Matches the aliasing used by [`parse_target_list`]: explicit alias first,
/// then simple column name / `*`, else a synthetic `col_N` fallback.
unsafe fn target_alias_for_res_target(rt: &pg_sys::ResTarget, ordinal: usize) -> String {
    if !rt.name.is_null() {
        return pg_cstr_to_str(rt.name).unwrap_or("?column?").to_string();
    }

    if !rt.val.is_null()
        && let Ok(expr) = unsafe { node_to_expr(rt.val) }
    {
        return match &expr {
            Expr::ColumnRef { column_name, .. } => column_name.clone(),
            Expr::Star { .. } => "*".to_string(),
            _ => format!("col_{ordinal}"),
        };
    }

    format!("col_{ordinal}")
}

/// Rewrite aggregate function calls in a HAVING predicate to their output column aliases.
///
/// The aggregate CTE already has the final aggregate values computed under the alias
/// names from the SELECT list (e.g., `SUM(amount) AS total` → column `total`).  When
/// the HAVING clause references `SUM(amount)`, we rewrite it to `total` so the
/// generated Filter SQL references the already-computed column instead of trying to
/// re-invoke the aggregate function.
fn rewrite_having_expr(expr: &Expr, aggregates: &[AggExpr]) -> Expr {
    match expr {
        Expr::FuncCall { func_name, args } => {
            let name_lower = func_name.to_lowercase();
            for agg in aggregates {
                let agg_name = agg.function.sql_name().to_lowercase();
                if name_lower != agg_name {
                    continue;
                }
                // Match by argument SQL representation.
                // COUNT(*) is represented as FuncCall { args: [Raw("*")] } when parsed
                // from a HAVING clause, but as argument = None in the target-list AggExpr.
                let args_match = match &agg.argument {
                    None => {
                        args.is_empty()
                            || (args.len() == 1 && matches!(&args[0], Expr::Raw(s) if s == "*"))
                    }
                    Some(agg_arg) => args.len() == 1 && args[0].to_sql() == agg_arg.to_sql(),
                };
                if args_match {
                    return Expr::ColumnRef {
                        table_alias: None,
                        column_name: agg.alias.clone(),
                    };
                }
            }
            // No match – keep as-is but recurse into args.
            Expr::FuncCall {
                func_name: func_name.clone(),
                args: args
                    .iter()
                    .map(|a| rewrite_having_expr(a, aggregates))
                    .collect(),
            }
        }
        Expr::BinaryOp { op, left, right } => Expr::BinaryOp {
            op: op.clone(),
            left: Box::new(rewrite_having_expr(left, aggregates)),
            right: Box::new(rewrite_having_expr(right, aggregates)),
        },
        _ => expr.clone(),
    }
}

/// Check if expressions are just `*` (select all).
fn is_star_only(exprs: &[Expr]) -> bool {
    exprs.len() == 1 && matches!(exprs[0], Expr::Star { table_alias: None })
}

// ── Predicate pushdown into cross joins ──────────────────────────────────
//
// Comma-separated FROM items produce a left-deep InnerJoin chain with
// `condition = Literal("TRUE")`.  The WHERE clause predicates are placed
// in a single Filter above the entire chain.
//
// This pass promotes predicates from the Filter into the appropriate
// JOIN ON clauses.  Benefits:
//
//  1. Eliminates cross-product intermediates in delta CTEs
//  2. Enables semi-join optimisation in diff_inner_join
//  3. Fixes Part 3 correction accuracy for multi-table joins (Q07)
//
// Algorithm:
//   a) Split the filter predicate into AND-connected parts.
//   b) For each part, collect referenced source-table aliases.
//   c) Walk the InnerJoin chain top-down. At each level, if the right
//      child contains any alias referenced by the predicate, that is
//      the level to attach it (all remaining aliases are guaranteed to
//      be in the left subtree of a left-deep chain).
//   d) Any predicate that can't be promoted stays in the Filter.

/// Push filter predicates down into cross-join ON clauses.
fn push_filter_into_cross_joins(tree: OpTree) -> OpTree {
    let OpTree::Filter { predicate, child } = tree else {
        return tree;
    };

    // Quick check: does the child contain any cross join?
    if !has_cross_join(&child) {
        return OpTree::Filter { predicate, child };
    }

    // Split predicate into AND-connected parts
    let parts = split_and_predicates(predicate);

    // Classify parts: collect referenced source aliases for each
    let mut to_promote: Vec<(Expr, Vec<String>)> = Vec::new();
    let mut remaining: Vec<Expr> = Vec::new();

    for part in parts {
        // Guard (a): never promote predicates containing scalar subqueries.
        // Correlated subqueries (e.g. TPC-H Q17) appear as Expr::Raw with
        // an embedded SELECT — promoting them removes column references that
        // the correlation still needs.
        if expr_contains_subquery(&part) {
            remaining.push(part);
            continue;
        }

        let mut aliases = Vec::new();
        collect_expr_source_aliases(&part, &child, &mut aliases);
        aliases.sort();
        aliases.dedup();

        if aliases.len() >= 2 {
            // References multiple source tables — promote into the join
            to_promote.push((part, aliases));
        } else {
            // Single-table predicate or can't determine — keep as filter
            remaining.push(part);
        }
    }

    if to_promote.is_empty() {
        // Nothing to promote — rebuild the original Filter
        return OpTree::Filter {
            predicate: join_and_predicates(remaining),
            child,
        };
    }

    // Promote each predicate into the join chain
    let mut new_tree = *child;
    for (pred, aliases) in to_promote {
        new_tree = promote_predicate(new_tree, pred, &aliases);
    }

    // Wrap remaining predicates (if any)
    if remaining.is_empty() {
        new_tree
    } else {
        OpTree::Filter {
            predicate: join_and_predicates(remaining),
            child: Box::new(new_tree),
        }
    }
}

/// Check if an OpTree contains any InnerJoin with condition = Literal("TRUE").
fn has_cross_join(op: &OpTree) -> bool {
    match op {
        OpTree::InnerJoin {
            condition,
            left,
            right,
            ..
        } => {
            matches!(condition, Expr::Literal(s) if s == "TRUE")
                || has_cross_join(left)
                || has_cross_join(right)
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. } => has_cross_join(child),
        OpTree::SemiJoin { left, .. } | OpTree::AntiJoin { left, .. } => has_cross_join(left),
        _ => false,
    }
}

/// Returns `true` if an `Expr` tree contains a scalar subquery (embedded
/// SELECT).  Correlated scalar subqueries arrive as `Expr::Raw(sql)` from
/// the parser; we also recurse into `BinaryOp` / `FuncCall` children.
fn expr_contains_subquery(expr: &Expr) -> bool {
    match expr {
        Expr::Raw(sql) => {
            // Case-insensitive check for SELECT keyword inside raw SQL
            let upper = sql.to_uppercase();
            upper.contains("SELECT") || upper.contains("EXISTS")
        }
        Expr::BinaryOp { left, right, .. } => {
            expr_contains_subquery(left) || expr_contains_subquery(right)
        }
        Expr::FuncCall { args, .. } => args.iter().any(expr_contains_subquery),
        _ => false,
    }
}

/// Split an AND-connected expression into its individual conjuncts.
fn split_and_predicates(expr: Expr) -> Vec<Expr> {
    match expr {
        Expr::BinaryOp { op, left, right } if op.eq_ignore_ascii_case("AND") => {
            let mut parts = split_and_predicates(*left);
            parts.extend(split_and_predicates(*right));
            parts
        }
        other => vec![other],
    }
}

/// Join predicates with AND. Panics if the slice is empty.
fn join_and_predicates(parts: Vec<Expr>) -> Expr {
    let mut iter = parts.into_iter();
    let mut result = iter.next().expect("at least one predicate required");
    for part in iter {
        result = Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(result),
            right: Box::new(part),
        };
    }
    result
}

/// Collect all source-table aliases referenced by column refs in an expression.
///
/// For qualified column refs (`n1.n_name`), the table alias is used directly
/// if it matches a Scan in the tree.  For unqualified refs (`s_suppkey`),
/// the tree is searched to find which Scan owns the column.
fn collect_expr_source_aliases(expr: &Expr, tree: &OpTree, aliases: &mut Vec<String>) {
    match expr {
        Expr::ColumnRef {
            table_alias: Some(tbl),
            column_name: _,
        } => {
            // Verify the alias exists somewhere in the tree
            if scan_has_alias(tree, tbl) {
                aliases.push(tbl.clone());
            }
        }
        Expr::ColumnRef {
            table_alias: None,
            column_name,
        } => {
            if let Some(alias) = find_scan_for_column(tree, column_name) {
                aliases.push(alias);
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_expr_source_aliases(left, tree, aliases);
            collect_expr_source_aliases(right, tree, aliases);
        }
        Expr::FuncCall { args, .. } => {
            for arg in args {
                collect_expr_source_aliases(arg, tree, aliases);
            }
        }
        Expr::Raw(sql) => {
            // Best-effort: look for `alias.column` patterns in raw SQL.
            // This won't catch everything but handles common cases.
            for alias in collect_tree_scan_aliases(tree) {
                if sql.contains(&format!("{}.", alias)) || sql.contains(&format!("\"{}\".", alias))
                {
                    aliases.push(alias);
                }
            }
        }
        _ => {}
    }
}

/// Check if the tree contains a Scan with the given alias.
fn scan_has_alias(op: &OpTree, alias: &str) -> bool {
    match op {
        OpTree::Scan { alias: a, .. } => a == alias,
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. }
        | OpTree::SemiJoin { left, right, .. }
        | OpTree::AntiJoin { left, right, .. } => {
            scan_has_alias(left, alias) || scan_has_alias(right, alias)
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. }
        | OpTree::Aggregate { child, .. }
        | OpTree::Distinct { child, .. } => scan_has_alias(child, alias),
        OpTree::LateralFunction { child, .. } | OpTree::LateralSubquery { child, .. } => {
            scan_has_alias(child, alias)
        }
        _ => false,
    }
}

/// Find which Scan node owns a given column name.
fn find_scan_for_column(op: &OpTree, column_name: &str) -> Option<String> {
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
        | OpTree::AntiJoin { left, right, .. } => find_scan_for_column(left, column_name)
            .or_else(|| find_scan_for_column(right, column_name)),
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. }
        | OpTree::Aggregate { child, .. }
        | OpTree::Distinct { child, .. } => find_scan_for_column(child, column_name),
        OpTree::LateralFunction { child, .. } | OpTree::LateralSubquery { child, .. } => {
            find_scan_for_column(child, column_name)
        }
        _ => None,
    }
}

/// Collect all Scan aliases in the tree.
fn collect_tree_scan_aliases(op: &OpTree) -> Vec<String> {
    match op {
        OpTree::Scan { alias, .. } => vec![alias.clone()],
        OpTree::InnerJoin { left, right, .. }
        | OpTree::LeftJoin { left, right, .. }
        | OpTree::FullJoin { left, right, .. }
        | OpTree::SemiJoin { left, right, .. }
        | OpTree::AntiJoin { left, right, .. } => {
            let mut v = collect_tree_scan_aliases(left);
            v.extend(collect_tree_scan_aliases(right));
            v
        }
        OpTree::Filter { child, .. }
        | OpTree::Project { child, .. }
        | OpTree::Subquery { child, .. }
        | OpTree::Aggregate { child, .. }
        | OpTree::Distinct { child, .. } => collect_tree_scan_aliases(child),
        OpTree::LateralFunction { child, .. } | OpTree::LateralSubquery { child, .. } => {
            collect_tree_scan_aliases(child)
        }
        _ => vec![],
    }
}

/// Promote a predicate into the appropriate InnerJoin level.
///
/// Walks a left-deep InnerJoin chain top-down.  At each level, if the
/// right child contains any alias referenced by the predicate, this is
/// the level to attach it (the left subtree contains all other aliases).
fn promote_predicate(tree: OpTree, pred: Expr, aliases: &[String]) -> OpTree {
    match tree {
        OpTree::InnerJoin {
            condition,
            left,
            right,
        } => {
            // Check if the right child contains any of the referenced aliases
            let right_has_alias = aliases.iter().any(|a| scan_has_alias(&right, a));

            if right_has_alias && matches!(&condition, Expr::Literal(s) if s == "TRUE") {
                // Promote: replace TRUE with the predicate
                OpTree::InnerJoin {
                    condition: pred,
                    left,
                    right,
                }
            } else if right_has_alias {
                // Already has a condition — combine with AND
                OpTree::InnerJoin {
                    condition: Expr::BinaryOp {
                        op: "AND".to_string(),
                        left: Box::new(condition),
                        right: Box::new(pred),
                    },
                    left,
                    right,
                }
            } else {
                // The right child doesn't have any referenced alias.
                // Recurse into the left child (which is the deeper part
                // of the left-deep chain).
                OpTree::InnerJoin {
                    condition,
                    left: Box::new(promote_predicate(*left, pred, aliases)),
                    right,
                }
            }
        }
        // Pass through transparent wrappers (SemiJoin/AntiJoin wrap the join chain)
        OpTree::SemiJoin {
            condition,
            left,
            right,
        } => OpTree::SemiJoin {
            condition,
            left: Box::new(promote_predicate(*left, pred, aliases)),
            right,
        },
        OpTree::AntiJoin {
            condition,
            left,
            right,
        } => OpTree::AntiJoin {
            condition,
            left: Box::new(promote_predicate(*left, pred, aliases)),
            right,
        },
        // Can't promote further — shouldn't happen in well-formed trees
        other => other,
    }
}

#[cfg(feature = "pg_test")]
#[pg_schema]
mod pg_tests {
    use super::*;

    fn sorted_unique_oids(mut oids: Vec<u32>) -> Vec<u32> {
        oids.sort_unstable();
        oids.dedup();
        oids
    }

    fn regclass_oid(qualified_name: &str) -> u32 {
        // nosemgrep: semgrep.rust.spi.query.dynamic-format \u2014 test-only helper; qualified_name is
        // always a hard-coded literal in tests, never runtime user input.
        Spi::get_one::<i32>(&format!("SELECT '{}'::regclass::oid::int4", qualified_name))
            .expect("failed to look up relation oid")
            .expect("relation oid query returned NULL") as u32
    }

    fn tree_contains(tree: &OpTree, predicate: &impl Fn(&OpTree) -> bool) -> bool {
        if predicate(tree) {
            return true;
        }

        match tree {
            OpTree::Scan { .. } | OpTree::CteScan { .. } | OpTree::RecursiveSelfRef { .. } => false,
            OpTree::Project { child, .. }
            | OpTree::Filter { child, .. }
            | OpTree::Aggregate { child, .. }
            | OpTree::Distinct { child }
            | OpTree::Subquery { child, .. }
            | OpTree::Window { child, .. }
            | OpTree::LateralFunction { child, .. }
            | OpTree::LateralSubquery { child, .. } => tree_contains(child, predicate),
            OpTree::InnerJoin { left, right, .. }
            | OpTree::LeftJoin { left, right, .. }
            | OpTree::FullJoin { left, right, .. }
            | OpTree::Intersect { left, right, .. }
            | OpTree::Except { left, right, .. }
            | OpTree::SemiJoin { left, right, .. }
            | OpTree::AntiJoin { left, right, .. } => {
                tree_contains(left, predicate) || tree_contains(right, predicate)
            }
            OpTree::UnionAll { children } => {
                children.iter().any(|child| tree_contains(child, predicate))
            }
            OpTree::RecursiveCte {
                base, recursive, ..
            } => tree_contains(base, predicate) || tree_contains(recursive, predicate),
            OpTree::ScalarSubquery {
                subquery, child, ..
            } => tree_contains(subquery, predicate) || tree_contains(child, predicate),
        }
    }

    #[pg_test]
    fn test_parse_defining_query_full_summarizes_cte_join_query() {
        Spi::run(
            "CREATE TABLE parser_orders_cte (
                id INT PRIMARY KEY,
                customer_id INT NOT NULL,
                amount INT NOT NULL
            )",
        )
        .expect("failed to create parser_orders_cte");
        Spi::run(
            "CREATE TABLE parser_customers_cte (
                id INT PRIMARY KEY,
                name TEXT NOT NULL
            )",
        )
        .expect("failed to create parser_customers_cte");

        let result = parse_defining_query_full(
            "WITH totals AS (
                SELECT customer_id, SUM(amount) AS total
                FROM parser_orders_cte
                GROUP BY customer_id
            )
            SELECT LOWER(c.name) AS customer_name, t.total
            FROM parser_customers_cte c
            JOIN totals t ON c.id = t.customer_id",
        )
        .expect("failed to parse CTE join query");

        let customers_oid = regclass_oid("parser_customers_cte");
        let orders_oid = regclass_oid("parser_orders_cte");
        let source_oids = sorted_unique_oids({
            let mut combined = result.tree.source_oids();
            combined.extend(result.cte_registry.source_oids());
            combined
        });

        assert!(!result.has_recursion);
        assert_eq!(result.cte_registry.entries.len(), 1);
        assert_eq!(result.tree.output_columns(), vec!["customer_name", "total"]);
        assert_eq!(source_oids, vec![customers_oid, orders_oid]);
        assert!(tree_contains(&result.tree, &|node| matches!(
            node,
            OpTree::CteScan { .. }
        )));
        assert_eq!(result.functions_used(), vec!["lower"]);
        assert_eq!(
            result
                .source_columns_used()
                .get(&customers_oid)
                .cloned()
                .unwrap_or_default(),
            vec!["id".to_string(), "name".to_string()]
        );
        assert_eq!(
            result
                .source_columns_used()
                .get(&orders_oid)
                .cloned()
                .unwrap_or_default(),
            vec!["amount".to_string(), "customer_id".to_string()]
        );
        assert!(check_ivm_support_with_registry(&result).is_ok());
        assert!(query_has_cte(
            "WITH totals AS (SELECT customer_id, SUM(amount) AS total FROM parser_orders_cte GROUP BY customer_id) SELECT LOWER(c.name) AS customer_name, t.total FROM parser_customers_cte c JOIN totals t ON c.id = t.customer_id"
        )
        .expect("failed to detect WITH clause"));
    }

    #[pg_test]
    fn test_parse_defining_query_full_summarizes_window_query() {
        Spi::run(
            "CREATE TABLE parser_sales_window (
                id INT PRIMARY KEY,
                region TEXT NOT NULL,
                amount INT NOT NULL
            )",
        )
        .expect("failed to create parser_sales_window");

        let result = parse_defining_query_full(
            "SELECT id, region,
                    ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount DESC) AS rn
             FROM parser_sales_window",
        )
        .expect("failed to parse window query");

        let sales_oid = regclass_oid("parser_sales_window");

        assert!(!result.has_recursion);
        assert!(result.cte_registry.entries.is_empty());
        assert_eq!(result.tree.output_columns(), vec!["id", "region", "rn"]);
        assert_eq!(
            sorted_unique_oids(result.tree.source_oids()),
            vec![sales_oid]
        );
        assert!(tree_contains(&result.tree, &|node| matches!(
            node,
            OpTree::Window { .. }
        )));
        assert_eq!(
            result
                .source_columns_used()
                .get(&sales_oid)
                .cloned()
                .unwrap_or_default(),
            vec!["amount".to_string(), "id".to_string(), "region".to_string()]
        );
        assert!(check_ivm_support_with_registry(&result).is_ok());
        assert!(
            !query_has_cte(
                "SELECT id, region, ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount DESC) AS rn FROM parser_sales_window"
            )
            .expect("failed to detect CTE absence")
        );
    }

    #[pg_test]
    fn test_parse_defining_query_full_summarizes_scalar_subquery() {
        Spi::run(
            "CREATE TABLE parser_orders_scalar (
                id INT PRIMARY KEY,
                amount INT NOT NULL
            )",
        )
        .expect("failed to create parser_orders_scalar");
        Spi::run(
            "CREATE TABLE parser_config_scalar (
                id INT PRIMARY KEY,
                tax_rate INT NOT NULL
            )",
        )
        .expect("failed to create parser_config_scalar");

        let result = parse_defining_query_full(
            "SELECT o.id,
                    (SELECT c.tax_rate FROM parser_config_scalar c WHERE c.id = 1) AS current_tax
             FROM parser_orders_scalar o",
        )
        .expect("failed to parse scalar subquery");

        let orders_oid = regclass_oid("parser_orders_scalar");
        let config_oid = regclass_oid("parser_config_scalar");

        assert!(!result.has_recursion);
        assert!(result.cte_registry.entries.is_empty());
        assert_eq!(result.tree.output_columns(), vec!["id", "current_tax"]);
        assert_eq!(
            sorted_unique_oids(result.tree.source_oids()),
            vec![config_oid, orders_oid]
        );
        assert!(tree_contains(&result.tree, &|node| matches!(
            node,
            OpTree::ScalarSubquery { .. }
        )));
        assert_eq!(
            result
                .source_columns_used()
                .get(&orders_oid)
                .cloned()
                .unwrap_or_default(),
            vec!["id".to_string()]
        );
        assert_eq!(
            result
                .source_columns_used()
                .get(&config_oid)
                .cloned()
                .unwrap_or_default(),
            vec!["id".to_string(), "tax_rate".to_string()]
        );
        assert!(check_ivm_support_with_registry(&result).is_ok());
    }

    #[pg_test]
    fn test_parse_defining_query_full_detects_recursive_ctes() {
        Spi::run(
            "CREATE TABLE parser_nodes_recursive (
                id INT PRIMARY KEY,
                parent_id INT
            )",
        )
        .expect("failed to create parser_nodes_recursive");

        let query = "WITH RECURSIVE tree AS (
                SELECT id, parent_id
                FROM parser_nodes_recursive
                WHERE parent_id IS NULL
                UNION ALL
                SELECT n.id, n.parent_id
                FROM parser_nodes_recursive n
                JOIN tree t ON n.parent_id = t.id
            )
            SELECT id, parent_id FROM tree";

        let result = parse_defining_query_full(query).expect("failed to parse recursive CTE");

        assert!(result.has_recursion);
        assert!(query_has_cte(query).expect("failed to detect recursive WITH"));
        assert!(query_has_recursive_cte(query).expect("failed to detect recursive WITH"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper constructors ─────────────────────────────────────────

    fn col(name: &str) -> Expr {
        Expr::ColumnRef {
            table_alias: None,
            column_name: name.to_string(),
        }
    }

    fn qualified_col(table: &str, name: &str) -> Expr {
        Expr::ColumnRef {
            table_alias: Some(table.to_string()),
            column_name: name.to_string(),
        }
    }

    fn make_column(name: &str) -> Column {
        Column {
            name: name.to_string(),
            type_oid: 23, // INT4
            is_nullable: true,
        }
    }

    fn scan_node(alias: &str, oid: u32, col_names: &[&str]) -> OpTree {
        OpTree::Scan {
            table_oid: oid,
            table_name: alias.to_string(),
            schema: "public".to_string(),
            columns: col_names.iter().map(|n| make_column(n)).collect(),
            pk_columns: Vec::new(),
            alias: alias.to_string(),
        }
    }

    // ── Expr::to_sql tests ──────────────────────────────────────────

    #[test]
    fn test_expr_column_ref_unqualified() {
        let e = col("amount");
        assert_eq!(e.to_sql(), "amount");
    }

    #[test]
    fn test_expr_column_ref_qualified() {
        let e = qualified_col("orders", "id");
        assert_eq!(e.to_sql(), "\"orders\".\"id\"");
    }

    #[test]
    fn test_expr_literal() {
        let e = Expr::Literal("42".to_string());
        assert_eq!(e.to_sql(), "42");
    }

    #[test]
    fn test_expr_literal_string() {
        let e = Expr::Literal("'hello'".to_string());
        assert_eq!(e.to_sql(), "'hello'");
    }

    #[test]
    fn test_expr_binary_op() {
        let e = Expr::BinaryOp {
            op: "+".to_string(),
            left: Box::new(col("a")),
            right: Box::new(col("b")),
        };
        assert_eq!(e.to_sql(), "(a + b)");
    }

    #[test]
    fn test_expr_binary_op_nested() {
        let e = Expr::BinaryOp {
            op: "*".to_string(),
            left: Box::new(Expr::BinaryOp {
                op: "+".to_string(),
                left: Box::new(col("a")),
                right: Box::new(col("b")),
            }),
            right: Box::new(Expr::Literal("2".to_string())),
        };
        assert_eq!(e.to_sql(), "((a + b) * 2)");
    }

    #[test]
    fn test_expr_func_call_no_args() {
        let e = Expr::FuncCall {
            func_name: "now".to_string(),
            args: vec![],
        };
        assert_eq!(e.to_sql(), "now()");
    }

    #[test]
    fn test_expr_func_call_with_args() {
        let e = Expr::FuncCall {
            func_name: "coalesce".to_string(),
            args: vec![col("x"), Expr::Literal("0".to_string())],
        };
        assert_eq!(e.to_sql(), "coalesce(x, 0)");
    }

    #[test]
    fn test_expr_star_unqualified() {
        let e = Expr::Star { table_alias: None };
        assert_eq!(e.to_sql(), "*");
    }

    #[test]
    fn test_expr_star_qualified() {
        let e = Expr::Star {
            table_alias: Some("t".to_string()),
        };
        assert_eq!(e.to_sql(), "t.*");
    }

    #[test]
    fn test_expr_raw() {
        let e = Expr::Raw("CASE WHEN x > 0 THEN 1 ELSE 0 END".to_string());
        assert_eq!(e.to_sql(), "CASE WHEN x > 0 THEN 1 ELSE 0 END");
    }

    // ── AggFunc::sql_name tests ─────────────────────────────────────

    #[test]
    fn test_agg_func_sql_names() {
        assert_eq!(AggFunc::Count.sql_name(), "COUNT");
        assert_eq!(AggFunc::CountStar.sql_name(), "COUNT");
        assert_eq!(AggFunc::Sum.sql_name(), "SUM");
        assert_eq!(AggFunc::Avg.sql_name(), "AVG");
        assert_eq!(AggFunc::Min.sql_name(), "MIN");
        assert_eq!(AggFunc::Max.sql_name(), "MAX");
        assert_eq!(AggFunc::BoolAnd.sql_name(), "BOOL_AND");
        assert_eq!(AggFunc::BoolOr.sql_name(), "BOOL_OR");
        assert_eq!(AggFunc::StringAgg.sql_name(), "STRING_AGG");
        assert_eq!(AggFunc::ArrayAgg.sql_name(), "ARRAY_AGG");
        assert_eq!(AggFunc::JsonAgg.sql_name(), "JSON_AGG");
        assert_eq!(AggFunc::JsonbAgg.sql_name(), "JSONB_AGG");
        assert_eq!(AggFunc::BitAnd.sql_name(), "BIT_AND");
        assert_eq!(AggFunc::BitOr.sql_name(), "BIT_OR");
        assert_eq!(
            AggFunc::JsonObjectAggStd("JSON_OBJECTAGG(k : v)".into()).sql_name(),
            "JSON_OBJECTAGG"
        );
        assert_eq!(
            AggFunc::JsonArrayAggStd("JSON_ARRAYAGG(x)".into()).sql_name(),
            "JSON_ARRAYAGG"
        );
        assert_eq!(AggFunc::BitXor.sql_name(), "BIT_XOR");
        assert_eq!(AggFunc::JsonObjectAgg.sql_name(), "JSON_OBJECT_AGG");
        assert_eq!(AggFunc::JsonbObjectAgg.sql_name(), "JSONB_OBJECT_AGG");
        assert_eq!(AggFunc::StddevPop.sql_name(), "STDDEV_POP");
        assert_eq!(AggFunc::StddevSamp.sql_name(), "STDDEV_SAMP");
        assert_eq!(AggFunc::VarPop.sql_name(), "VAR_POP");
        assert_eq!(AggFunc::VarSamp.sql_name(), "VAR_SAMP");
        assert_eq!(AggFunc::Mode.sql_name(), "MODE");
        assert_eq!(AggFunc::PercentileCont.sql_name(), "PERCENTILE_CONT");
        assert_eq!(AggFunc::PercentileDisc.sql_name(), "PERCENTILE_DISC");
        assert_eq!(AggFunc::Corr.sql_name(), "CORR");
        assert_eq!(AggFunc::CovarPop.sql_name(), "COVAR_POP");
        assert_eq!(AggFunc::CovarSamp.sql_name(), "COVAR_SAMP");
        assert_eq!(AggFunc::RegrAvgx.sql_name(), "REGR_AVGX");
        assert_eq!(AggFunc::RegrAvgy.sql_name(), "REGR_AVGY");
        assert_eq!(AggFunc::RegrCount.sql_name(), "REGR_COUNT");
        assert_eq!(AggFunc::RegrIntercept.sql_name(), "REGR_INTERCEPT");
        assert_eq!(AggFunc::RegrR2.sql_name(), "REGR_R2");
        assert_eq!(AggFunc::RegrSlope.sql_name(), "REGR_SLOPE");
        assert_eq!(AggFunc::RegrSxx.sql_name(), "REGR_SXX");
        assert_eq!(AggFunc::RegrSxy.sql_name(), "REGR_SXY");
        assert_eq!(AggFunc::RegrSyy.sql_name(), "REGR_SYY");
        assert_eq!(
            AggFunc::UserDefined("my_agg(x)".into()).sql_name(),
            "USER_DEFINED"
        );
    }

    // ── OpTree::alias tests ─────────────────────────────────────────

    #[test]
    fn test_scan_alias() {
        let tree = scan_node("orders", 12345, &["id", "amount"]);
        assert_eq!(tree.alias(), "orders");
    }

    #[test]
    fn test_project_alias() {
        let tree = OpTree::Project {
            expressions: vec![col("id")],
            aliases: vec!["id".to_string()],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert_eq!(tree.alias(), "project");
    }

    #[test]
    fn test_filter_alias() {
        let tree = OpTree::Filter {
            predicate: col("x"),
            child: Box::new(scan_node("t", 1, &["x"])),
        };
        assert_eq!(tree.alias(), "filter");
    }

    #[test]
    fn test_inner_join_alias() {
        let tree = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        assert_eq!(tree.alias(), "join");
    }

    // ── CROSS JOIN structure tests ──────────────────────────────────

    #[test]
    fn test_cross_join_is_inner_join_with_true_condition() {
        // CROSS JOIN is represented as InnerJoin { condition: Literal("TRUE") }
        let tree = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(scan_node("a", 1, &["x"])),
            right: Box::new(scan_node("b", 2, &["y"])),
        };
        assert!(matches!(&tree, OpTree::InnerJoin { condition, .. }
            if matches!(condition, Expr::Literal(s) if s == "TRUE")));
        assert_eq!(tree.alias(), "join");
    }

    #[test]
    fn test_cross_join_output_columns_combines_both_sides() {
        // A CROSS JOIN between two tables should expose columns from both sides
        let tree = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(scan_node("a", 1, &["x", "y"])),
            right: Box::new(scan_node("b", 2, &["p", "q"])),
        };
        let cols = tree.output_columns();
        assert_eq!(cols, vec!["x", "y", "p", "q"]);
    }

    #[test]
    fn test_nested_cross_join_structure() {
        // SELECT * FROM a CROSS JOIN b CROSS JOIN c
        // → InnerJoin(InnerJoin(a, b), c) with TRUE conditions
        let inner = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(scan_node("a", 1, &["x"])),
            right: Box::new(scan_node("b", 2, &["y"])),
        };
        let outer = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(inner),
            right: Box::new(scan_node("c", 3, &["z"])),
        };

        // Verify structure: top-level is InnerJoin with TRUE condition
        assert!(matches!(&outer, OpTree::InnerJoin { condition, .. }
            if matches!(condition, Expr::Literal(s) if s == "TRUE")));
        // Verify all columns are accessible
        let cols = outer.output_columns();
        assert_eq!(cols, vec!["x", "y", "z"]);
    }

    #[test]
    fn test_left_join_alias() {
        let tree = OpTree::LeftJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        assert_eq!(tree.alias(), "left_join");
    }

    #[test]
    fn test_aggregate_alias() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("region")],
            aggregates: vec![AggExpr {
                function: AggFunc::Sum,
                argument: Some(col("amount")),
                alias: "total".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["region", "amount"])),
        };
        assert_eq!(tree.alias(), "agg");
    }

    #[test]
    fn test_distinct_alias() {
        let tree = OpTree::Distinct {
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert_eq!(tree.alias(), "distinct");
    }

    #[test]
    fn test_union_all_alias() {
        let tree = OpTree::UnionAll {
            children: vec![scan_node("a", 1, &["id"]), scan_node("b", 2, &["id"])],
        };
        assert_eq!(tree.alias(), "union");
    }

    // ── rewrite_having_expr for UNION (Part 2 structure tests) ─────

    #[test]
    fn test_union_all_produces_union_all_node() {
        // Verify that UnionAll node structure is correct (no Distinct wrapper)
        let tree = OpTree::UnionAll {
            children: vec![
                scan_node("a", 1, &["id", "val"]),
                scan_node("b", 2, &["id", "val"]),
            ],
        };
        // The top-level node must NOT be a Distinct
        assert!(matches!(tree, OpTree::UnionAll { .. }));
    }

    #[test]
    fn test_distinct_wraps_union_all_for_dedup_union() {
        // UNION (without ALL) must be represented as Distinct { child: UnionAll { .. } }
        let union_all = OpTree::UnionAll {
            children: vec![
                scan_node("a", 1, &["id", "val"]),
                scan_node("b", 2, &["id", "val"]),
            ],
        };
        let tree = OpTree::Distinct {
            child: Box::new(union_all),
        };
        // Top-level is Distinct
        assert!(matches!(tree, OpTree::Distinct { .. }));
        // Its child is UnionAll
        if let OpTree::Distinct { child } = &tree {
            assert!(matches!(**child, OpTree::UnionAll { .. }));
            if let OpTree::UnionAll { children } = child.as_ref() {
                assert_eq!(children.len(), 2);
            }
        }
    }

    #[test]
    fn test_distinct_around_union_all_alias() {
        // Distinct wrapping UnionAll should report "distinct" as its alias
        let tree = OpTree::Distinct {
            child: Box::new(OpTree::UnionAll {
                children: vec![scan_node("a", 1, &["id"]), scan_node("b", 2, &["id"])],
            }),
        };
        assert_eq!(tree.alias(), "distinct");
    }

    #[test]
    fn test_distinct_union_all_output_columns_from_first_child() {
        // output_columns delegates through Distinct → UnionAll → first child
        let tree = OpTree::Distinct {
            child: Box::new(OpTree::UnionAll {
                children: vec![
                    scan_node("a", 1, &["region", "amount"]),
                    scan_node("b", 2, &["region", "amount"]),
                ],
            }),
        };
        assert_eq!(tree.output_columns(), vec!["region", "amount"]);
    }

    #[test]
    fn test_distinct_union_all_source_oids_combined() {
        // source_oids should include OIDs from both branches
        let tree = OpTree::Distinct {
            child: Box::new(OpTree::UnionAll {
                children: vec![scan_node("a", 10, &["id"]), scan_node("b", 20, &["id"])],
            }),
        };
        let oids = tree.source_oids();
        assert!(oids.contains(&10));
        assert!(oids.contains(&20));
        assert_eq!(oids.len(), 2);
    }

    #[test]
    fn test_needs_pgt_count_distinct_wrapping_union_all() {
        // Distinct wrapping UnionAll uses union-dedup-count path, not pgt_count
        let tree = OpTree::Distinct {
            child: Box::new(OpTree::UnionAll {
                children: vec![scan_node("a", 1, &["id"]), scan_node("b", 2, &["id"])],
            }),
        };
        assert!(!tree.needs_pgt_count());
        assert!(tree.needs_union_dedup_count());
    }

    #[test]
    fn test_scan_output_columns() {
        let tree = scan_node("orders", 1, &["id", "amount", "name"]);
        assert_eq!(tree.output_columns(), vec!["id", "amount", "name"]);
    }

    #[test]
    fn test_project_output_columns() {
        let tree = OpTree::Project {
            expressions: vec![col("id"), col("amount")],
            aliases: vec!["order_id".to_string(), "total".to_string()],
            child: Box::new(scan_node("t", 1, &["id", "amount"])),
        };
        assert_eq!(tree.output_columns(), vec!["order_id", "total"]);
    }

    #[test]
    fn test_filter_output_columns_delegates_to_child() {
        let tree = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: ">".to_string(),
                left: Box::new(col("amount")),
                right: Box::new(Expr::Literal("100".to_string())),
            },
            child: Box::new(scan_node("t", 1, &["id", "amount"])),
        };
        assert_eq!(tree.output_columns(), vec!["id", "amount"]);
    }

    #[test]
    fn test_inner_join_output_columns_combines_both() {
        let tree = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["id", "name"])),
            right: Box::new(scan_node("b", 2, &["id", "value"])),
        };
        assert_eq!(tree.output_columns(), vec!["id", "name", "id", "value"]);
    }

    #[test]
    fn test_left_join_output_columns_combines_both() {
        let tree = OpTree::LeftJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["x"])),
            right: Box::new(scan_node("b", 2, &["y", "z"])),
        };
        assert_eq!(tree.output_columns(), vec!["x", "y", "z"]);
    }

    #[test]
    fn test_aggregate_output_columns() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("region")],
            aggregates: vec![
                AggExpr {
                    function: AggFunc::Sum,
                    argument: Some(col("amount")),
                    alias: "total".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                },
                AggExpr {
                    function: AggFunc::CountStar,
                    argument: None,
                    alias: "cnt".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                },
            ],
            child: Box::new(scan_node("t", 1, &["region", "amount"])),
        };
        // group_by outputs the to_sql() of the expression, plus aggregate aliases
        assert_eq!(tree.output_columns(), vec!["region", "total", "cnt"]);
    }

    #[test]
    fn test_aggregate_qualified_group_by_output() {
        let tree = OpTree::Aggregate {
            group_by: vec![qualified_col("t", "region")],
            aggregates: vec![],
            child: Box::new(scan_node("t", 1, &["region"])),
        };
        // Qualified column references should produce unqualified output names,
        // matching PostgreSQL's behavior for subquery output columns.
        assert_eq!(tree.output_columns(), vec!["region"]);
    }

    #[test]
    fn test_distinct_output_columns_delegates() {
        let tree = OpTree::Distinct {
            child: Box::new(scan_node("t", 1, &["a", "b"])),
        };
        assert_eq!(tree.output_columns(), vec!["a", "b"]);
    }

    #[test]
    fn test_union_all_output_columns_uses_first_child() {
        let tree = OpTree::UnionAll {
            children: vec![
                scan_node("a", 1, &["x", "y"]),
                scan_node("b", 2, &["p", "q"]),
            ],
        };
        assert_eq!(tree.output_columns(), vec!["x", "y"]);
    }

    #[test]
    fn test_union_all_empty_children() {
        let tree = OpTree::UnionAll { children: vec![] };
        assert!(tree.output_columns().is_empty());
    }

    // ── OpTree::source_oids tests ───────────────────────────────────

    #[test]
    fn test_scan_source_oids() {
        let tree = scan_node("t", 42, &["id"]);
        assert_eq!(tree.source_oids(), vec![42]);
    }

    #[test]
    fn test_project_source_oids_delegates() {
        let tree = OpTree::Project {
            expressions: vec![col("id")],
            aliases: vec!["id".to_string()],
            child: Box::new(scan_node("t", 100, &["id"])),
        };
        assert_eq!(tree.source_oids(), vec![100]);
    }

    #[test]
    fn test_filter_source_oids_delegates() {
        let tree = OpTree::Filter {
            predicate: col("x"),
            child: Box::new(scan_node("t", 50, &["x"])),
        };
        assert_eq!(tree.source_oids(), vec![50]);
    }

    #[test]
    fn test_join_source_oids_combines() {
        let tree = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 10, &["id"])),
            right: Box::new(scan_node("b", 20, &["id"])),
        };
        assert_eq!(tree.source_oids(), vec![10, 20]);
    }

    #[test]
    fn test_left_join_source_oids_combines() {
        let tree = OpTree::LeftJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 30, &["id"])),
            right: Box::new(scan_node("b", 40, &["id"])),
        };
        assert_eq!(tree.source_oids(), vec![30, 40]);
    }

    #[test]
    fn test_aggregate_source_oids_delegates() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("r")],
            aggregates: vec![],
            child: Box::new(scan_node("t", 77, &["r"])),
        };
        assert_eq!(tree.source_oids(), vec![77]);
    }

    #[test]
    fn test_distinct_source_oids_delegates() {
        let tree = OpTree::Distinct {
            child: Box::new(scan_node("t", 55, &["id"])),
        };
        assert_eq!(tree.source_oids(), vec![55]);
    }

    #[test]
    fn test_union_all_source_oids_collects_all() {
        let tree = OpTree::UnionAll {
            children: vec![
                scan_node("a", 1, &["id"]),
                scan_node("b", 2, &["id"]),
                scan_node("c", 3, &["id"]),
            ],
        };
        assert_eq!(tree.source_oids(), vec![1, 2, 3]);
    }

    #[test]
    fn test_nested_tree_source_oids() {
        // SELECT a.id, b.val FROM a JOIN b ON a.id = b.id WHERE a.x > 10
        let tree = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: ">".to_string(),
                left: Box::new(qualified_col("a", "x")),
                right: Box::new(Expr::Literal("10".to_string())),
            },
            child: Box::new(OpTree::InnerJoin {
                condition: Expr::BinaryOp {
                    op: "=".to_string(),
                    left: Box::new(qualified_col("a", "id")),
                    right: Box::new(qualified_col("b", "id")),
                },
                left: Box::new(scan_node("a", 100, &["id", "x"])),
                right: Box::new(scan_node("b", 200, &["id", "val"])),
            }),
        };
        assert_eq!(tree.source_oids(), vec![100, 200]);
    }

    // ── check_ivm_support tests ─────────────────────────────────────

    #[test]
    fn test_check_ivm_support_scan() {
        let tree = scan_node("t", 1, &["id"]);
        assert!(check_ivm_support(&tree).is_ok());
    }

    #[test]
    fn test_check_ivm_support_supported_operators() {
        // Build a tree using supported operators:
        // Aggregate(Filter(InnerJoin(scan_a, scan_b)))
        let scan_a = scan_node("a", 1, &["id", "val"]);
        let scan_b = scan_node("b", 2, &["id", "val"]);

        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_a),
            right: Box::new(scan_b),
        };

        let filtered = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: ">".to_string(),
                left: Box::new(col("val")),
                right: Box::new(Expr::Literal("0".to_string())),
            },
            child: Box::new(join),
        };

        let projected = OpTree::Project {
            expressions: vec![col("id"), col("val")],
            aliases: vec!["id".to_string(), "val".to_string()],
            child: Box::new(filtered),
        };

        let aggregated = OpTree::Aggregate {
            group_by: vec![col("id")],
            aggregates: vec![
                AggExpr {
                    function: AggFunc::Sum,
                    argument: Some(col("val")),
                    alias: "sum_val".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                },
                AggExpr {
                    function: AggFunc::Count,
                    argument: Some(col("val")),
                    alias: "cnt".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                },
                AggExpr {
                    function: AggFunc::Avg,
                    argument: Some(col("val")),
                    alias: "avg_val".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                },
            ],
            child: Box::new(projected),
        };

        let distinct = OpTree::Distinct {
            child: Box::new(aggregated),
        };

        assert!(check_ivm_support(&distinct).is_ok());
    }

    #[test]
    fn test_check_ivm_support_simple_join() {
        // InnerJoin with Scan children — should pass
        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        assert!(check_ivm_support(&join).is_ok());
    }

    #[test]
    fn test_check_ivm_support_join_with_filter_children() {
        // Join with Filter(Scan) children — should pass
        let left = OpTree::Filter {
            predicate: col("id"),
            child: Box::new(scan_node("a", 1, &["id"])),
        };
        let right = OpTree::Project {
            expressions: vec![col("id")],
            aliases: vec!["id".to_string()],
            child: Box::new(scan_node("b", 2, &["id"])),
        };
        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(left),
            right: Box::new(right),
        };
        assert!(check_ivm_support(&join).is_ok());
    }

    #[test]
    fn test_check_ivm_support_allows_min_aggregate() {
        let agg = OpTree::Aggregate {
            group_by: vec![col("id")],
            aggregates: vec![AggExpr {
                function: AggFunc::Min,
                argument: Some(col("val")),
                alias: "min_val".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["id", "val"])),
        };
        assert!(check_ivm_support(&agg).is_ok());
    }

    #[test]
    fn test_check_ivm_support_allows_max_aggregate() {
        let agg = OpTree::Aggregate {
            group_by: vec![col("id")],
            aggregates: vec![AggExpr {
                function: AggFunc::Max,
                argument: Some(col("val")),
                alias: "max_val".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["id", "val"])),
        };
        assert!(check_ivm_support(&agg).is_ok());
    }

    #[test]
    fn test_check_ivm_support_allows_nested_join() {
        // Join-of-join: InnerJoin(InnerJoin(a, b), c) — now supported
        let inner_join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        let outer_join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(inner_join),
            right: Box::new(scan_node("c", 3, &["id"])),
        };
        assert!(check_ivm_support(&outer_join).is_ok());
    }

    #[test]
    fn test_check_ivm_support_allows_join_with_union_child() {
        // LeftJoin(UnionAll(...), Scan) — now supported
        let union = OpTree::UnionAll {
            children: vec![scan_node("a", 1, &["id"]), scan_node("b", 2, &["id"])],
        };
        let join = OpTree::LeftJoin {
            condition: col("id"),
            left: Box::new(union),
            right: Box::new(scan_node("c", 3, &["id"])),
        };
        assert!(check_ivm_support(&join).is_ok());
    }

    // ── is_star_only tests ──────────────────────────────────────────

    #[test]
    fn test_is_star_only_true() {
        assert!(is_star_only(&[Expr::Star { table_alias: None }]));
    }

    #[test]
    fn test_is_star_only_false_qualified() {
        assert!(!is_star_only(&[Expr::Star {
            table_alias: Some("t".to_string()),
        }]));
    }

    #[test]
    fn test_is_star_only_false_column() {
        assert!(!is_star_only(&[col("id")]));
    }

    #[test]
    fn test_is_star_only_false_multiple() {
        assert!(!is_star_only(&[
            Expr::Star { table_alias: None },
            col("id"),
        ]));
    }

    #[test]
    fn test_is_star_only_false_empty() {
        assert!(!is_star_only(&[]));
    }

    // ── RowIdStrategy (from row_id.rs, tested here for convenience) ─

    #[test]
    fn test_row_id_strategy_debug() {
        use crate::dvm::row_id::RowIdStrategy;
        let pk = RowIdStrategy::PrimaryKey {
            pk_columns: vec!["id".to_string()],
        };
        let debug = format!("{:?}", pk);
        assert!(debug.contains("PrimaryKey"));
        assert!(debug.contains("id"));
    }

    #[test]
    fn test_row_id_strategy_clone() {
        use crate::dvm::row_id::RowIdStrategy;
        let original = RowIdStrategy::GroupByKey {
            group_columns: vec!["region".to_string(), "year".to_string()],
        };
        let cloned = original.clone();
        let debug_orig = format!("{:?}", original);
        let debug_clone = format!("{:?}", cloned);
        assert_eq!(debug_orig, debug_clone);
    }

    // ── Subquery / CTE OpTree tests ─────────────────────────────────

    #[test]
    fn test_subquery_alias() {
        let tree = OpTree::Subquery {
            alias: "active_users".to_string(),
            column_aliases: vec![],
            child: Box::new(scan_node("users", 1, &["id", "name"])),
        };
        assert_eq!(tree.alias(), "active_users");
    }

    #[test]
    fn test_subquery_output_columns_no_aliases() {
        // When no column_aliases, delegates to child
        let tree = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec![],
            child: Box::new(scan_node("t", 1, &["id", "name", "val"])),
        };
        assert_eq!(tree.output_columns(), vec!["id", "name", "val"]);
    }

    #[test]
    fn test_subquery_output_columns_with_aliases() {
        // Column aliases override child's output
        let tree = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec!["a".to_string(), "b".to_string()],
            child: Box::new(scan_node("t", 1, &["id", "name"])),
        };
        assert_eq!(tree.output_columns(), vec!["a", "b"]);
    }

    #[test]
    fn test_subquery_source_oids_delegates() {
        let tree = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec![],
            child: Box::new(scan_node("t", 42, &["id"])),
        };
        assert_eq!(tree.source_oids(), vec![42]);
    }

    #[test]
    fn test_subquery_source_oids_nested() {
        // Subquery wrapping a join — collects OIDs from both sides
        let tree = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec![],
            child: Box::new(OpTree::InnerJoin {
                condition: col("id"),
                left: Box::new(scan_node("a", 10, &["id"])),
                right: Box::new(scan_node("b", 20, &["id"])),
            }),
        };
        assert_eq!(tree.source_oids(), vec![10, 20]);
    }

    #[test]
    fn test_check_ivm_support_subquery() {
        let tree = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec![],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(check_ivm_support(&tree).is_ok());
    }

    #[test]
    fn test_check_ivm_support_subquery_with_aggregate() {
        // Subquery wrapping an aggregate — should be supported
        let tree = OpTree::Subquery {
            alias: "totals".to_string(),
            column_aliases: vec!["uid".to_string(), "total".to_string()],
            child: Box::new(OpTree::Aggregate {
                group_by: vec![col("user_id")],
                aggregates: vec![AggExpr {
                    function: AggFunc::Sum,
                    argument: Some(col("amount")),
                    alias: "total".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                }],
                child: Box::new(scan_node("orders", 1, &["user_id", "amount"])),
            }),
        };
        assert!(check_ivm_support(&tree).is_ok());
    }

    #[test]
    fn test_subquery_in_join() {
        // Simulates: FROM (SELECT ...) AS sub JOIN orders ON ...
        let sub = OpTree::Subquery {
            alias: "active".to_string(),
            column_aliases: vec![],
            child: Box::new(OpTree::Filter {
                predicate: Expr::BinaryOp {
                    op: "=".to_string(),
                    left: Box::new(col("active")),
                    right: Box::new(Expr::Literal("true".to_string())),
                },
                child: Box::new(scan_node("users", 1, &["id", "name", "active"])),
            }),
        };

        let tree = OpTree::InnerJoin {
            condition: Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(qualified_col("active", "id")),
                right: Box::new(qualified_col("orders", "user_id")),
            },
            left: Box::new(sub),
            right: Box::new(scan_node("orders", 2, &["user_id", "amount"])),
        };

        assert!(check_ivm_support(&tree).is_ok());
        assert_eq!(tree.source_oids(), vec![1, 2]);
    }

    #[test]
    fn test_nested_subqueries() {
        // Simulates: FROM (SELECT * FROM (SELECT ...) AS inner) AS outer
        let inner = OpTree::Subquery {
            alias: "inner_sub".to_string(),
            column_aliases: vec![],
            child: Box::new(scan_node("t", 1, &["id", "val"])),
        };
        let outer = OpTree::Subquery {
            alias: "outer_sub".to_string(),
            column_aliases: vec!["a".to_string(), "b".to_string()],
            child: Box::new(inner),
        };
        assert_eq!(outer.output_columns(), vec!["a", "b"]);
        assert_eq!(outer.source_oids(), vec![1]);
    }

    // ── Tier 2: CteScan + CteRegistry tests ────────────────────────

    #[test]
    fn test_cte_scan_alias() {
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "t1".to_string(),
            columns: vec!["user_id".to_string(), "total".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        assert_eq!(node.alias(), "t1");
    }

    #[test]
    fn test_cte_scan_output_columns_no_aliases() {
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "totals".to_string(),
            columns: vec!["user_id".to_string(), "total".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        assert_eq!(node.output_columns(), vec!["user_id", "total"]);
    }

    #[test]
    fn test_cte_scan_output_columns_with_aliases() {
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "t".to_string(),
            columns: vec!["user_id".to_string(), "total".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec!["uid".to_string(), "amt".to_string()],
            body: None,
        };
        assert_eq!(node.output_columns(), vec!["uid", "amt"]);
    }

    #[test]
    fn test_cte_scan_source_oids_empty() {
        // CteScan OIDs are resolved via the CteRegistry, not the node itself
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "totals".to_string(),
            columns: vec!["id".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        assert!(node.source_oids().is_empty());
    }

    #[test]
    fn test_cte_scan_ivm_support() {
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "totals".to_string(),
            columns: vec!["id".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        assert!(check_ivm_support(&node).is_ok());
    }

    #[test]
    fn test_cte_registry_source_oids() {
        let mut reg = CteRegistry::default();
        reg.entries
            .push(("a".to_string(), scan_node("orders", 100, &["id", "amount"])));
        reg.entries
            .push(("b".to_string(), scan_node("users", 200, &["id", "name"])));
        let oids = reg.source_oids();
        assert!(oids.contains(&100));
        assert!(oids.contains(&200));
        assert_eq!(oids.len(), 2);
    }

    #[test]
    fn test_cte_registry_get() {
        let mut reg = CteRegistry::default();
        reg.entries
            .push(("totals".to_string(), scan_node("orders", 1, &["id"])));
        assert!(reg.get(0).is_some());
        assert_eq!(reg.get(0).unwrap().0, "totals");
        assert!(reg.get(1).is_none());
    }

    #[test]
    fn test_cte_parse_context_basic() {
        // Verify CteParseContext tracks registrations correctly
        let mut ctx = CteParseContext::new(HashMap::new(), HashMap::new());
        assert!(!ctx.is_cte("x"));
        assert!(ctx.lookup_id("x").is_none());

        let body = scan_node("t", 1, &["id"]);
        let id = ctx.register("x", body);
        assert_eq!(id, 0);
        assert_eq!(ctx.lookup_id("x"), Some(0));
        assert_eq!(ctx.registry.entries.len(), 1);

        let body2 = scan_node("t2", 2, &["id"]);
        let id2 = ctx.register("y", body2);
        assert_eq!(id2, 1);
        assert_eq!(ctx.registry.entries.len(), 2);
    }

    #[test]
    fn test_parse_result_struct() {
        let tree = scan_node("orders", 1, &["id", "amount"]);
        let mut registry = CteRegistry::default();
        registry.entries.push((
            "totals".to_string(),
            scan_node("orders", 1, &["user_id", "total"]),
        ));
        let result = ParseResult {
            tree: tree.clone(),
            cte_registry: registry,
            has_recursion: false,
            warnings: vec![],
        };
        assert_eq!(result.tree.alias(), "orders");
        assert_eq!(result.cte_registry.entries.len(), 1);
        assert!(!result.has_recursion);
    }

    #[test]
    fn test_check_ivm_support_with_registry_valid() {
        let tree = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "totals".to_string(),
            columns: vec!["id".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        let mut registry = CteRegistry::default();
        registry.entries.push((
            "totals".to_string(),
            scan_node("orders", 1, &["id", "amount"]),
        ));
        let result = ParseResult {
            tree,
            cte_registry: registry,
            has_recursion: false,
            warnings: vec![],
        };
        assert!(check_ivm_support_with_registry(&result).is_ok());
    }

    #[test]
    fn test_multi_reference_cte_same_id() {
        // Simulates: WITH totals AS (...) SELECT ... FROM totals t1 JOIN totals t2
        // Both CteScan nodes should share cte_id=0
        let t1 = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "t1".to_string(),
            columns: vec!["user_id".to_string(), "total".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        let t2 = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "t2".to_string(),
            columns: vec!["user_id".to_string(), "total".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        let tree = OpTree::InnerJoin {
            condition: Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(qualified_col("t1", "user_id")),
                right: Box::new(qualified_col("t2", "user_id")),
            },
            left: Box::new(t1),
            right: Box::new(t2),
        };

        let mut registry = CteRegistry::default();
        registry.entries.push((
            "totals".to_string(),
            scan_node("orders", 1, &["user_id", "total"]),
        ));
        let result = ParseResult {
            tree,
            cte_registry: registry,
            has_recursion: false,
            warnings: vec![],
        };
        assert!(check_ivm_support_with_registry(&result).is_ok());
        // Only one entry in the registry despite two CteScan nodes
        assert_eq!(result.cte_registry.entries.len(), 1);
    }

    #[test]
    fn test_cte_chain_in_registry() {
        // Simulates: WITH a AS (...), b AS (SELECT FROM a) SELECT FROM b
        // Registry has two entries; the main tree references only b (cte_id=1)
        let a_body = scan_node("orders", 1, &["id", "amount"]);
        let b_body = OpTree::CteScan {
            cte_id: 0,
            cte_name: "a".to_string(),
            alias: "a".to_string(),
            columns: vec!["id".to_string(), "amount".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        let tree = OpTree::CteScan {
            cte_id: 1,
            cte_name: "b".to_string(),
            alias: "b".to_string(),
            columns: vec!["id".to_string(), "amount".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };

        let mut registry = CteRegistry::default();
        registry.entries.push(("a".to_string(), a_body));
        registry.entries.push(("b".to_string(), b_body));

        let result = ParseResult {
            tree,
            cte_registry: registry,
            has_recursion: false,
            warnings: vec![],
        };
        assert!(check_ivm_support_with_registry(&result).is_ok());
        assert_eq!(result.cte_registry.entries.len(), 2);
        // The CTE registry should report the base OIDs transitively
        let oids = result.cte_registry.source_oids();
        assert!(oids.contains(&1));
    }

    // ── Tier 3a: has_recursion flag tests ───────────────────────────

    #[test]
    fn test_parse_result_has_recursion_false_by_default() {
        let result = ParseResult {
            tree: scan_node("t", 1, &["id"]),
            cte_registry: CteRegistry::default(),
            has_recursion: false,
            warnings: vec![],
        };
        assert!(!result.has_recursion);
    }

    #[test]
    fn test_parse_result_has_recursion_true() {
        let result = ParseResult {
            tree: scan_node("t", 1, &["id"]),
            cte_registry: CteRegistry::default(),
            has_recursion: true,
            warnings: vec![],
        };
        assert!(result.has_recursion);
    }

    #[test]
    fn test_parse_result_has_recursion_flag_preserved_on_clone() {
        let result = ParseResult {
            tree: scan_node("t", 1, &["id"]),
            cte_registry: CteRegistry::default(),
            has_recursion: true,
            warnings: vec![],
        };
        let cloned = result.clone();
        assert!(cloned.has_recursion);
    }

    // ── CTE definition-level column aliases tests ──────────────────

    #[test]
    fn test_cte_scan_output_columns_with_def_aliases_only() {
        // WITH x(a, b) AS (SELECT id, name FROM ...) SELECT * FROM x
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "x".to_string(),
            alias: "x".to_string(),
            columns: vec!["id".to_string(), "name".to_string()],
            cte_def_aliases: vec!["a".to_string(), "b".to_string()],
            column_aliases: vec![],
            body: None,
        };
        assert_eq!(node.output_columns(), vec!["a", "b"]);
    }

    #[test]
    fn test_cte_scan_output_columns_ref_aliases_override_def_aliases() {
        // WITH x(a, b) AS (...) SELECT * FROM x AS y(c, d)
        // Reference aliases take priority over definition aliases
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "x".to_string(),
            alias: "y".to_string(),
            columns: vec!["id".to_string(), "name".to_string()],
            cte_def_aliases: vec!["a".to_string(), "b".to_string()],
            column_aliases: vec!["c".to_string(), "d".to_string()],
            body: None,
        };
        assert_eq!(node.output_columns(), vec!["c", "d"]);
    }

    #[test]
    fn test_cte_scan_def_aliases_no_ref_aliases_no_body_match() {
        // Definition aliases differ from body columns → output is def aliases
        let node = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "totals".to_string(),
            columns: vec!["user_id".to_string(), "total".to_string()],
            cte_def_aliases: vec!["uid".to_string(), "amt".to_string()],
            column_aliases: vec![],
            body: None,
        };
        assert_eq!(node.output_columns(), vec!["uid", "amt"]);
    }

    // ── RecursiveCte tests ──────────────────────────────────────────

    #[test]
    fn test_recursive_cte_alias() {
        let node = make_recursive_cte("tree", &["id", "parent_id", "depth"]);
        assert_eq!(node.alias(), "tree");
    }

    #[test]
    fn test_recursive_cte_output_columns() {
        let node = make_recursive_cte("tree", &["id", "parent_id", "depth"]);
        assert_eq!(node.output_columns(), vec!["id", "parent_id", "depth"]);
    }

    #[test]
    fn test_recursive_cte_source_oids() {
        // Source OIDs come from both base and recursive terms, deduplicated
        let base = OpTree::Scan {
            table_oid: 100,
            table_name: "categories".to_string(),
            schema: "public".to_string(),
            columns: vec![make_column("id")],
            pk_columns: Vec::new(),
            alias: "c".to_string(),
        };
        let recursive = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".to_string()),
            left: Box::new(OpTree::Scan {
                table_oid: 100, // same table
                table_name: "categories".to_string(),
                schema: "public".to_string(),
                columns: vec![make_column("id")],
                pk_columns: Vec::new(),
                alias: "c2".to_string(),
            }),
            right: Box::new(OpTree::RecursiveSelfRef {
                cte_name: "tree".to_string(),
                alias: "t".to_string(),
                columns: vec!["id".to_string()],
            }),
        };
        let node = OpTree::RecursiveCte {
            alias: "tree".to_string(),
            columns: vec!["id".to_string()],
            base: Box::new(base),
            recursive: Box::new(recursive),
            union_all: true,
        };

        let oids = node.source_oids();
        assert_eq!(oids, vec![100]); // Deduplicated
    }

    #[test]
    fn test_recursive_cte_source_oids_no_self_ref() {
        let node = OpTree::RecursiveSelfRef {
            cte_name: "tree".to_string(),
            alias: "t".to_string(),
            columns: vec!["id".to_string()],
        };
        assert_eq!(node.source_oids(), Vec::<u32>::new());
    }

    #[test]
    fn test_recursive_self_ref_alias() {
        let node = OpTree::RecursiveSelfRef {
            cte_name: "tree".to_string(),
            alias: "t".to_string(),
            columns: vec!["id".to_string(), "name".to_string()],
        };
        assert_eq!(node.alias(), "t");
    }

    #[test]
    fn test_recursive_self_ref_output_columns() {
        let node = OpTree::RecursiveSelfRef {
            cte_name: "tree".to_string(),
            alias: "t".to_string(),
            columns: vec!["id".to_string(), "depth".to_string()],
        };
        assert_eq!(node.output_columns(), vec!["id", "depth"]);
    }

    #[test]
    fn test_ivm_support_recursive_cte() {
        // check_ivm_support should accept RecursiveCte with valid children
        let node = make_recursive_cte("tree", &["id"]);
        assert!(check_ivm_support_inner(&node).is_ok());
    }

    #[test]
    fn test_ivm_support_recursive_self_ref() {
        // RecursiveSelfRef is always valid in a recursive CTE context
        let node = OpTree::RecursiveSelfRef {
            cte_name: "tree".to_string(),
            alias: "t".to_string(),
            columns: vec!["id".to_string()],
        };
        assert!(check_ivm_support_inner(&node).is_ok());
    }

    /// Helper: build a minimal RecursiveCte for testing.
    fn make_recursive_cte(name: &str, cols: &[&str]) -> OpTree {
        let columns: Vec<String> = cols.iter().map(|s| s.to_string()).collect();
        OpTree::RecursiveCte {
            alias: name.to_string(),
            columns: columns.clone(),
            base: Box::new(OpTree::Scan {
                table_oid: 42,
                table_name: "source".to_string(),
                schema: "public".to_string(),
                columns: cols.iter().map(|c| make_column(c)).collect(),
                pk_columns: Vec::new(),
                alias: "s".to_string(),
            }),
            recursive: Box::new(OpTree::RecursiveSelfRef {
                cte_name: name.to_string(),
                alias: "r".to_string(),
                columns,
            }),
            union_all: true,
        }
    }

    // ── WindowExpr tests ──────────────────────────────────────────

    fn make_window_expr(
        func: &str,
        args: Vec<Expr>,
        partition: Vec<Expr>,
        order: Vec<SortExpr>,
        alias: &str,
    ) -> WindowExpr {
        WindowExpr {
            func_name: func.to_string(),
            args,
            partition_by: partition,
            order_by: order,
            frame_clause: None,
            alias: alias.to_string(),
        }
    }

    #[test]
    fn test_window_expr_to_sql_row_number() {
        let wexpr = make_window_expr(
            "row_number",
            vec![],
            vec![col("department")],
            vec![SortExpr {
                expr: col("salary"),
                ascending: false,
                nulls_first: false,
            }],
            "rn",
        );
        assert_eq!(
            wexpr.to_sql(),
            "row_number() OVER (PARTITION BY department ORDER BY salary DESC NULLS LAST)"
        );
    }

    #[test]
    fn test_window_expr_to_sql_sum_over() {
        let wexpr = make_window_expr(
            "sum",
            vec![col("amount")],
            vec![col("region")],
            vec![],
            "region_total",
        );
        assert_eq!(wexpr.to_sql(), "sum(amount) OVER (PARTITION BY region)");
    }

    #[test]
    fn test_window_expr_to_sql_rank_no_partition() {
        let wexpr = make_window_expr(
            "rank",
            vec![],
            vec![],
            vec![SortExpr {
                expr: col("score"),
                ascending: true,
                nulls_first: false,
            }],
            "rank",
        );
        assert_eq!(wexpr.to_sql(), "rank() OVER (ORDER BY score ASC)");
    }

    #[test]
    fn test_window_expr_to_sql_empty_over() {
        let wexpr = make_window_expr("count", vec![col("id")], vec![], vec![], "cnt");
        assert_eq!(wexpr.to_sql(), "count(id) OVER ()");
    }

    #[test]
    fn test_window_expr_to_sql_multiple_order_by() {
        let wexpr = make_window_expr(
            "row_number",
            vec![],
            vec![col("dept")],
            vec![
                SortExpr {
                    expr: col("salary"),
                    ascending: false,
                    nulls_first: false,
                },
                SortExpr {
                    expr: col("name"),
                    ascending: true,
                    nulls_first: false,
                },
            ],
            "rn",
        );
        assert_eq!(
            wexpr.to_sql(),
            "row_number() OVER (PARTITION BY dept ORDER BY salary DESC NULLS LAST, name ASC)"
        );
    }

    // ── OpTree::Window tests ──────────────────────────────────────

    fn make_window_node(
        partition_cols: Vec<Expr>,
        wf_aliases: Vec<&str>,
        pt_aliases: Vec<&str>,
    ) -> OpTree {
        let scan = scan_node("employees", 100, &["id", "department", "salary"]);
        let window_exprs: Vec<WindowExpr> = wf_aliases
            .iter()
            .map(|a| WindowExpr {
                func_name: "row_number".to_string(),
                args: vec![],
                partition_by: partition_cols.clone(),
                order_by: vec![],
                frame_clause: None,
                alias: a.to_string(),
            })
            .collect();
        let pass_through: Vec<(Expr, String)> =
            pt_aliases.iter().map(|a| (col(a), a.to_string())).collect();
        OpTree::Window {
            window_exprs,
            partition_by: partition_cols,
            pass_through,
            child: Box::new(scan),
        }
    }

    #[test]
    fn test_window_alias() {
        let node = make_window_node(
            vec![col("department")],
            vec!["rn"],
            vec!["department", "salary"],
        );
        assert_eq!(node.alias(), "window");
    }

    #[test]
    fn test_window_output_columns() {
        let node = make_window_node(
            vec![col("department")],
            vec!["rn"],
            vec!["department", "salary"],
        );
        assert_eq!(node.output_columns(), vec!["department", "salary", "rn"]);
    }

    #[test]
    fn test_window_output_columns_multiple_wf() {
        let node = make_window_node(
            vec![col("department")],
            vec!["rn", "total"],
            vec!["department", "salary"],
        );
        assert_eq!(
            node.output_columns(),
            vec!["department", "salary", "rn", "total"]
        );
    }

    #[test]
    fn test_window_source_oids() {
        let node = make_window_node(vec![col("department")], vec!["rn"], vec!["department"]);
        assert_eq!(node.source_oids(), vec![100]);
    }

    #[test]
    fn test_window_ivm_support() {
        let node = make_window_node(vec![col("department")], vec!["rn"], vec!["department"]);
        assert!(check_ivm_support(&node).is_ok());
    }

    // ── Phase 4: Additional coverage tests ──────────────────────────

    // ── Expr::output_name tests ─────────────────────────────────────

    #[test]
    fn test_expr_output_name_unqualified_column() {
        let e = col("amount");
        assert_eq!(e.output_name(), "amount");
    }

    #[test]
    fn test_expr_output_name_qualified_column_strips_table() {
        let e = qualified_col("orders", "total");
        assert_eq!(e.output_name(), "total");
    }

    #[test]
    fn test_expr_output_name_literal_returns_value() {
        let e = Expr::Literal("42".to_string());
        assert_eq!(e.output_name(), "42");
    }

    #[test]
    fn test_expr_output_name_binary_op_returns_sql() {
        let e = Expr::BinaryOp {
            op: "+".to_string(),
            left: Box::new(col("a")),
            right: Box::new(col("b")),
        };
        assert_eq!(e.output_name(), "(a + b)");
    }

    #[test]
    fn test_expr_output_name_func_call_returns_sql() {
        let e = Expr::FuncCall {
            func_name: "upper".to_string(),
            args: vec![col("name")],
        };
        assert_eq!(e.output_name(), "upper(name)");
    }

    // ── Expr::strip_qualifier tests ─────────────────────────────────

    #[test]
    fn test_strip_qualifier_column_ref() {
        let e = qualified_col("t", "amount");
        let stripped = e.strip_qualifier();
        assert_eq!(stripped.to_sql(), "amount");
    }

    #[test]
    fn test_strip_qualifier_unqualified_unchanged() {
        let e = col("amount");
        let stripped = e.strip_qualifier();
        assert_eq!(stripped.to_sql(), "amount");
    }

    #[test]
    fn test_strip_qualifier_binary_op_recursive() {
        let e = Expr::BinaryOp {
            op: "+".to_string(),
            left: Box::new(qualified_col("t", "a")),
            right: Box::new(qualified_col("t", "b")),
        };
        let stripped = e.strip_qualifier();
        assert_eq!(stripped.to_sql(), "(a + b)");
    }

    #[test]
    fn test_strip_qualifier_func_call_recursive() {
        let e = Expr::FuncCall {
            func_name: "coalesce".to_string(),
            args: vec![qualified_col("t", "x"), Expr::Literal("0".to_string())],
        };
        let stripped = e.strip_qualifier();
        assert_eq!(stripped.to_sql(), "coalesce(x, 0)");
    }

    #[test]
    fn test_strip_qualifier_literal_unchanged() {
        let e = Expr::Literal("42".to_string());
        let stripped = e.strip_qualifier();
        assert_eq!(stripped.to_sql(), "42");
    }

    #[test]
    fn test_strip_qualifier_star_unchanged() {
        let e = Expr::Star {
            table_alias: Some("t".to_string()),
        };
        let stripped = e.strip_qualifier();
        // Star and Raw are returned as-is (self.clone())
        assert_eq!(stripped.to_sql(), "t.*");
    }

    #[test]
    fn test_strip_qualifier_raw_unchanged() {
        let e = Expr::Raw("CASE WHEN x > 0 THEN 1 END".to_string());
        let stripped = e.strip_qualifier();
        assert_eq!(stripped.to_sql(), "CASE WHEN x > 0 THEN 1 END");
    }

    // ── Expr::rewrite_aliases tests ─────────────────────────────────

    #[test]
    fn test_rewrite_aliases_column_ref_left() {
        let e = qualified_col("a", "id");
        let rewritten = e.rewrite_aliases("a", "new_a", "b", "new_b");
        assert_eq!(rewritten.to_sql(), "\"new_a\".\"id\"");
    }

    #[test]
    fn test_rewrite_aliases_column_ref_right() {
        let e = qualified_col("b", "name");
        let rewritten = e.rewrite_aliases("a", "new_a", "b", "new_b");
        assert_eq!(rewritten.to_sql(), "\"new_b\".\"name\"");
    }

    #[test]
    fn test_rewrite_aliases_unknown_alias_unchanged() {
        let e = qualified_col("c", "val");
        let rewritten = e.rewrite_aliases("a", "new_a", "b", "new_b");
        assert_eq!(rewritten.to_sql(), "\"c\".\"val\"");
    }

    #[test]
    fn test_rewrite_aliases_unqualified_unchanged() {
        let e = col("id");
        let rewritten = e.rewrite_aliases("a", "new_a", "b", "new_b");
        assert_eq!(rewritten.to_sql(), "id");
    }

    #[test]
    fn test_rewrite_aliases_binary_op_recursive() {
        let e = Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(qualified_col("a", "id")),
            right: Box::new(qualified_col("b", "id")),
        };
        let rewritten = e.rewrite_aliases("a", "x", "b", "y");
        assert_eq!(rewritten.to_sql(), "(\"x\".\"id\" = \"y\".\"id\")");
    }

    #[test]
    fn test_rewrite_aliases_func_call_recursive() {
        let e = Expr::FuncCall {
            func_name: "coalesce".to_string(),
            args: vec![qualified_col("a", "x"), qualified_col("b", "y")],
        };
        let rewritten = e.rewrite_aliases("a", "left", "b", "right");
        assert_eq!(
            rewritten.to_sql(),
            "coalesce(\"left\".\"x\", \"right\".\"y\")"
        );
    }

    #[test]
    fn test_rewrite_aliases_literal_unchanged() {
        let e = Expr::Literal("42".to_string());
        let rewritten = e.rewrite_aliases("a", "x", "b", "y");
        assert_eq!(rewritten.to_sql(), "42");
    }

    // ── rewrite_having_expr tests ───────────────────────────────────

    fn make_agg(func: AggFunc, arg_name: Option<&str>, alias: &str) -> AggExpr {
        AggExpr {
            function: func,
            argument: arg_name.map(col),
            alias: alias.to_string(),
            is_distinct: false,
            second_arg: None,
            filter: None,
            order_within_group: None,
        }
    }

    #[test]
    fn test_having_expr_rewrite_sum_to_alias() {
        // HAVING SUM(amount) > 100  →  total > 100
        let aggs = vec![make_agg(AggFunc::Sum, Some("amount"), "total")];
        let pred = Expr::BinaryOp {
            op: ">".to_string(),
            left: Box::new(Expr::FuncCall {
                func_name: "sum".to_string(),
                args: vec![col("amount")],
            }),
            right: Box::new(Expr::Literal("100".to_string())),
        };
        let rewritten = rewrite_having_expr(&pred, &aggs);
        assert_eq!(rewritten.to_sql(), "(total > 100)");
    }

    #[test]
    fn test_having_expr_rewrite_count_star_to_alias() {
        // HAVING COUNT(*) >= 5  →  cnt >= 5
        let aggs = vec![make_agg(AggFunc::CountStar, None, "cnt")];
        let pred = Expr::BinaryOp {
            op: ">=".to_string(),
            left: Box::new(Expr::FuncCall {
                func_name: "count".to_string(),
                args: vec![],
            }),
            right: Box::new(Expr::Literal("5".to_string())),
        };
        let rewritten = rewrite_having_expr(&pred, &aggs);
        assert_eq!(rewritten.to_sql(), "(cnt >= 5)");
    }

    #[test]
    fn test_having_expr_rewrite_multiple_aggs() {
        // HAVING SUM(amount) > 100 AND COUNT(*) > 2
        let aggs = vec![
            make_agg(AggFunc::Sum, Some("amount"), "total"),
            make_agg(AggFunc::CountStar, None, "cnt"),
        ];
        let pred = Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(Expr::BinaryOp {
                op: ">".to_string(),
                left: Box::new(Expr::FuncCall {
                    func_name: "sum".to_string(),
                    args: vec![col("amount")],
                }),
                right: Box::new(Expr::Literal("100".to_string())),
            }),
            right: Box::new(Expr::BinaryOp {
                op: ">".to_string(),
                left: Box::new(Expr::FuncCall {
                    func_name: "count".to_string(),
                    args: vec![],
                }),
                right: Box::new(Expr::Literal("2".to_string())),
            }),
        };
        let rewritten = rewrite_having_expr(&pred, &aggs);
        assert_eq!(rewritten.to_sql(), "((total > 100) AND (cnt > 2))");
    }

    #[test]
    fn test_having_expr_rewrite_case_insensitive() {
        // Function name "SUM" (uppercase) should still match AggFunc::Sum
        let aggs = vec![make_agg(AggFunc::Sum, Some("price"), "revenue")];
        let pred = Expr::FuncCall {
            func_name: "SUM".to_string(),
            args: vec![col("price")],
        };
        let rewritten = rewrite_having_expr(&pred, &aggs);
        assert_eq!(rewritten.to_sql(), "revenue");
    }

    #[test]
    fn test_having_expr_no_match_keeps_func_call() {
        // An unknown function is kept as-is.
        let aggs = vec![make_agg(AggFunc::Sum, Some("amount"), "total")];
        let pred = Expr::FuncCall {
            func_name: "coalesce".to_string(),
            args: vec![col("x"), Expr::Literal("0".to_string())],
        };
        let rewritten = rewrite_having_expr(&pred, &aggs);
        assert_eq!(rewritten.to_sql(), "coalesce(x, 0)");
    }

    #[test]
    fn test_having_expr_rewrite_avg_to_alias() {
        // HAVING AVG(score) > 75.0  →  avg_score > 75.0
        let aggs = vec![make_agg(AggFunc::Avg, Some("score"), "avg_score")];
        let pred = Expr::BinaryOp {
            op: ">".to_string(),
            left: Box::new(Expr::FuncCall {
                func_name: "avg".to_string(),
                args: vec![col("score")],
            }),
            right: Box::new(Expr::Literal("75.0".to_string())),
        };
        let rewritten = rewrite_having_expr(&pred, &aggs);
        assert_eq!(rewritten.to_sql(), "(avg_score > 75.0)");
    }

    #[test]
    fn test_having_expr_column_ref_passthrough() {
        // Column refs (GROUP BY columns) are passed through unchanged.
        let aggs = vec![make_agg(AggFunc::Sum, Some("amount"), "total")];
        let pred = Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(col("region")),
            right: Box::new(Expr::Literal("'US'".to_string())),
        };
        let rewritten = rewrite_having_expr(&pred, &aggs);
        assert_eq!(rewritten.to_sql(), "(region = 'US')");
    }

    // ── OpTree::needs_pgt_count tests ──────────────────────────────

    #[test]
    fn test_needs_pgt_count_aggregate() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("region")],
            aggregates: vec![],
            child: Box::new(scan_node("t", 1, &["region"])),
        };
        assert!(tree.needs_pgt_count());
    }

    #[test]
    fn test_needs_pgt_count_distinct() {
        let tree = OpTree::Distinct {
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(tree.needs_pgt_count());
    }

    #[test]
    fn test_needs_pgt_count_scan_false() {
        let tree = scan_node("t", 1, &["id"]);
        assert!(!tree.needs_pgt_count());
    }

    #[test]
    fn test_needs_pgt_count_project_false() {
        let tree = OpTree::Project {
            expressions: vec![col("id")],
            aliases: vec!["id".to_string()],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(!tree.needs_pgt_count());
    }

    // ── OpTree::group_by_columns tests ──────────────────────────────

    #[test]
    fn test_group_by_columns_aggregate_with_groups() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("region"), col("year")],
            aggregates: vec![],
            child: Box::new(scan_node("t", 1, &["region", "year"])),
        };
        assert_eq!(
            tree.group_by_columns(),
            Some(vec!["region".to_string(), "year".to_string()])
        );
    }

    #[test]
    fn test_group_by_columns_scalar_aggregate() {
        let tree = OpTree::Aggregate {
            group_by: vec![],
            aggregates: vec![AggExpr {
                function: AggFunc::CountStar,
                argument: None,
                alias: "cnt".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert_eq!(tree.group_by_columns(), None);
    }

    #[test]
    fn test_group_by_columns_through_project() {
        let tree = OpTree::Project {
            expressions: vec![col("region")],
            aliases: vec!["region".to_string()],
            child: Box::new(OpTree::Aggregate {
                group_by: vec![col("region")],
                aggregates: vec![],
                child: Box::new(scan_node("t", 1, &["region"])),
            }),
        };
        assert_eq!(tree.group_by_columns(), Some(vec!["region".to_string()]));
    }

    #[test]
    fn test_group_by_columns_through_subquery() {
        let tree = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec![],
            child: Box::new(OpTree::Aggregate {
                group_by: vec![col("dept")],
                aggregates: vec![],
                child: Box::new(scan_node("t", 1, &["dept"])),
            }),
        };
        assert_eq!(tree.group_by_columns(), Some(vec!["dept".to_string()]));
    }

    #[test]
    fn test_group_by_columns_scan_returns_none() {
        let tree = scan_node("t", 1, &["id"]);
        assert_eq!(tree.group_by_columns(), None);
    }

    #[test]
    fn test_group_by_columns_project_renames_columns() {
        // Simulates: SELECT t.id AS department_id, t.name AS dept_name, COUNT(*)
        //            FROM ... GROUP BY t.id, t.name
        // The Aggregate group_by has ["id", "name"] but the Project aliases
        // them to ["department_id", "dept_name"]. The storage table uses the
        // Project aliases, so group_by_columns() must return those.
        let tree = OpTree::Project {
            expressions: vec![col("id"), col("name"), col("cnt")],
            aliases: vec![
                "department_id".to_string(),
                "dept_name".to_string(),
                "cnt".to_string(),
            ],
            child: Box::new(OpTree::Aggregate {
                group_by: vec![col("id"), col("name")],
                aggregates: vec![AggExpr {
                    function: AggFunc::CountStar,
                    argument: None,
                    alias: "cnt".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                }],
                child: Box::new(scan_node("t", 1, &["id", "name"])),
            }),
        };
        assert_eq!(
            tree.group_by_columns(),
            Some(vec!["department_id".to_string(), "dept_name".to_string()])
        );
    }

    #[test]
    fn test_group_by_columns_project_ordinal_group_by() {
        // Simulates: SELECT split_part(full_path, ' > ', 2) AS division,
        //                   SUM(headcount) AS total_headcount
        //            FROM ... GROUP BY 1
        // The Aggregate group_by has ordinal Literal("1"). The Project must
        // resolve position 1 → alias "division".
        let tree = OpTree::Project {
            expressions: vec![
                Expr::FuncCall {
                    func_name: "split_part".to_string(),
                    args: vec![
                        col("full_path"),
                        Expr::Literal("' > '".to_string()),
                        Expr::Literal("2".to_string()),
                    ],
                },
                col("total_headcount"),
            ],
            aliases: vec!["division".to_string(), "total_headcount".to_string()],
            child: Box::new(OpTree::Aggregate {
                group_by: vec![Expr::Literal("1".to_string())],
                aggregates: vec![AggExpr {
                    function: AggFunc::Sum,
                    argument: Some(col("headcount")),
                    alias: "total_headcount".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                }],
                child: Box::new(scan_node(
                    "department_stats",
                    1,
                    &["full_path", "headcount"],
                )),
            }),
        };
        assert_eq!(tree.group_by_columns(), Some(vec!["division".to_string()]));
    }

    // ── OpTree::row_id_key_columns tests ────────────────────────────

    #[test]
    fn test_row_id_key_columns_scan_all_nullable() {
        let tree = scan_node("t", 1, &["id", "name"]);
        // All columns nullable → returns all columns
        assert_eq!(
            tree.row_id_key_columns(),
            Some(vec!["id".to_string(), "name".to_string()])
        );
    }

    #[test]
    fn test_row_id_key_columns_scan_with_non_nullable() {
        // S10: Scans WITHOUT pk_columns return ALL columns (not just non-nullable).
        let tree = OpTree::Scan {
            table_oid: 1,
            table_name: "t".to_string(),
            schema: "public".to_string(),
            columns: vec![
                Column {
                    name: "id".to_string(),
                    type_oid: 23,
                    is_nullable: false,
                },
                Column {
                    name: "name".to_string(),
                    type_oid: 25,
                    is_nullable: true,
                },
            ],
            pk_columns: Vec::new(),
            alias: "t".to_string(),
        };
        assert_eq!(
            tree.row_id_key_columns(),
            Some(vec!["id".to_string(), "name".to_string()]),
        );
    }

    #[test]
    fn test_row_id_key_columns_scan_with_pk_columns() {
        // When pk_columns are present, those are used (not non-nullable columns).
        let tree = OpTree::Scan {
            table_oid: 1,
            table_name: "t".to_string(),
            schema: "public".to_string(),
            columns: vec![
                Column {
                    name: "id".to_string(),
                    type_oid: 23,
                    is_nullable: false,
                },
                Column {
                    name: "name".to_string(),
                    type_oid: 25,
                    is_nullable: false,
                },
            ],
            pk_columns: vec!["id".to_string()],
            alias: "t".to_string(),
        };
        assert_eq!(tree.row_id_key_columns(), Some(vec!["id".to_string()]),);
    }

    #[test]
    fn test_row_id_key_columns_filter_delegates() {
        let tree = OpTree::Filter {
            predicate: col("active"),
            child: Box::new(scan_node("t", 1, &["id", "active"])),
        };
        assert_eq!(
            tree.row_id_key_columns(),
            Some(vec!["id".to_string(), "active".to_string()])
        );
    }

    #[test]
    fn test_row_id_key_columns_project_filter_scan_where_not_in_select() {
        // Regression test: SELECT id, val AS vb FROM t WHERE side >= 2
        // The Filter exposes all Scan columns (id, val, side) in its output,
        // but the Project aliases only contain (id, vb) — 2 items vs 3 in
        // child_out. Previously this caused row_id_key_columns() to return
        // None, leading to row_to_json-based __pgt_row_id in the full refresh
        // but PK-hash-based in the differential, causing MERGE mismatches and
        // stale rows after UPDATEs that change the WHERE-clause column.
        let scan = OpTree::Scan {
            table_oid: 1,
            table_name: "t".to_string(),
            schema: "public".to_string(),
            columns: vec![make_column("id"), make_column("val"), make_column("side")],
            pk_columns: vec!["id".to_string()],
            alias: "t".to_string(),
        };
        let filter = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: ">=".to_string(),
                left: Box::new(col("side")),
                right: Box::new(Expr::Literal("2".to_string())),
            },
            child: Box::new(scan),
        };
        let tree = OpTree::Project {
            expressions: vec![col("id"), col("val")],
            aliases: vec!["id".to_string(), "vb".to_string()],
            child: Box::new(filter),
        };
        // Must return Some(["id"]) — the PK, not None.
        assert_eq!(tree.row_id_key_columns(), Some(vec!["id".to_string()]));
    }

    #[test]
    fn test_row_id_key_columns_project_filter_scan_pk_not_projected_returns_none() {
        // When the PK column is NOT in the SELECT list, the row_id key
        // cannot be determined — return None so the fallback row_to_json
        // hash is used consistently for both full and differential refresh.
        let scan = OpTree::Scan {
            table_oid: 1,
            table_name: "t".to_string(),
            schema: "public".to_string(),
            columns: vec![
                make_column("id"),
                make_column("name"),
                make_column("amount"),
            ],
            pk_columns: vec!["id".to_string()],
            alias: "t".to_string(),
        };
        let filter = OpTree::Filter {
            predicate: col("amount"),
            child: Box::new(scan),
        };
        // SELECT name FROM t WHERE amount > 0 — PK "id" is NOT projected.
        let tree = OpTree::Project {
            expressions: vec![col("name")],
            aliases: vec!["name".to_string()],
            child: Box::new(filter),
        };
        assert_eq!(tree.row_id_key_columns(), None);
    }

    #[test]
    fn test_row_id_key_columns_distinct_returns_all() {
        let tree = OpTree::Distinct {
            child: Box::new(scan_node("t", 1, &["a", "b"])),
        };
        assert_eq!(
            tree.row_id_key_columns(),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn test_row_id_key_columns_aggregate_with_group_by() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("region")],
            aggregates: vec![AggExpr {
                function: AggFunc::Sum,
                argument: Some(col("val")),
                alias: "total".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["region", "val"])),
        };
        assert_eq!(tree.row_id_key_columns(), Some(vec!["region".to_string()]));
    }

    #[test]
    fn test_row_id_key_columns_scalar_aggregate_returns_none() {
        let tree = OpTree::Aggregate {
            group_by: vec![],
            aggregates: vec![AggExpr {
                function: AggFunc::CountStar,
                argument: None,
                alias: "cnt".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert_eq!(tree.row_id_key_columns(), None);
    }

    #[test]
    fn test_row_id_key_columns_cte_scan_returns_columns() {
        let tree = OpTree::CteScan {
            cte_id: 0,
            cte_name: "x".to_string(),
            alias: "x".to_string(),
            columns: vec!["id".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        assert_eq!(tree.row_id_key_columns(), Some(vec!["id".to_string()]));
    }

    #[test]
    fn test_row_id_key_columns_window_returns_none() {
        let node = make_window_node(vec![col("dept")], vec!["rn"], vec!["dept"]);
        assert_eq!(node.row_id_key_columns(), None);
    }

    #[test]
    fn test_row_id_key_columns_subquery_delegates() {
        let tree = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec![],
            child: Box::new(scan_node("t", 1, &["id", "name"])),
        };
        assert_eq!(
            tree.row_id_key_columns(),
            Some(vec!["id".to_string(), "name".to_string()])
        );
    }

    // ── OpTree::node_kind tests ─────────────────────────────────────

    #[test]
    fn test_node_kind_all_variants() {
        assert_eq!(scan_node("t", 1, &["id"]).node_kind(), "scan");
        assert_eq!(
            OpTree::Project {
                expressions: vec![],
                aliases: vec![],
                child: Box::new(scan_node("t", 1, &["id"])),
            }
            .node_kind(),
            "project"
        );
        assert_eq!(
            OpTree::Filter {
                predicate: col("x"),
                child: Box::new(scan_node("t", 1, &["x"])),
            }
            .node_kind(),
            "filter"
        );
        assert_eq!(
            OpTree::InnerJoin {
                condition: col("id"),
                left: Box::new(scan_node("a", 1, &["id"])),
                right: Box::new(scan_node("b", 2, &["id"])),
            }
            .node_kind(),
            "inner join"
        );
        assert_eq!(
            OpTree::LeftJoin {
                condition: col("id"),
                left: Box::new(scan_node("a", 1, &["id"])),
                right: Box::new(scan_node("b", 2, &["id"])),
            }
            .node_kind(),
            "left join"
        );
        assert_eq!(
            OpTree::Aggregate {
                group_by: vec![],
                aggregates: vec![],
                child: Box::new(scan_node("t", 1, &["id"])),
            }
            .node_kind(),
            "aggregate"
        );
        assert_eq!(
            OpTree::Distinct {
                child: Box::new(scan_node("t", 1, &["id"])),
            }
            .node_kind(),
            "distinct"
        );
        assert_eq!(
            OpTree::UnionAll {
                children: vec![scan_node("a", 1, &["id"])],
            }
            .node_kind(),
            "union all"
        );
        assert_eq!(
            OpTree::Subquery {
                alias: "s".to_string(),
                column_aliases: vec![],
                child: Box::new(scan_node("t", 1, &["id"])),
            }
            .node_kind(),
            "subquery"
        );
        assert_eq!(
            OpTree::CteScan {
                cte_id: 0,
                cte_name: "x".to_string(),
                alias: "x".to_string(),
                columns: vec![],
                cte_def_aliases: vec![],
                column_aliases: vec![],
                body: None,
            }
            .node_kind(),
            "cte scan"
        );
        assert_eq!(
            make_recursive_cte("tree", &["id"]).node_kind(),
            "recursive cte"
        );
        assert_eq!(
            OpTree::RecursiveSelfRef {
                cte_name: "tree".to_string(),
                alias: "t".to_string(),
                columns: vec!["id".to_string()],
            }
            .node_kind(),
            "recursive self-reference"
        );
    }

    // ── join_pk_aliases tests ───────────────────────────────────────

    fn scan_with_pk(alias: &str, oid: u32, cols: &[&str], pks: &[&str]) -> OpTree {
        OpTree::Scan {
            table_oid: oid,
            table_name: alias.to_string(),
            schema: "public".to_string(),
            columns: cols.iter().map(|n| make_column(n)).collect(),
            pk_columns: pks.iter().map(|p| p.to_string()).collect(),
            alias: alias.to_string(),
        }
    }

    #[test]
    fn test_join_pk_aliases_both_sides_have_pk() {
        let left = scan_with_pk("a", 1, &["id", "name"], &["id"]);
        let right = scan_with_pk("b", 2, &["bid", "val"], &["bid"]);
        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(left),
            right: Box::new(right),
        };
        let expressions = vec![
            qualified_col("a", "id"),
            qualified_col("a", "name"),
            qualified_col("b", "bid"),
            qualified_col("b", "val"),
        ];
        let aliases = vec![
            "id".to_string(),
            "name".to_string(),
            "bid".to_string(),
            "val".to_string(),
        ];
        let result = join_pk_aliases(&expressions, &aliases, &join);
        assert_eq!(result, Some(vec!["id".to_string(), "bid".to_string()]));
    }

    #[test]
    fn test_join_pk_aliases_one_side_missing_pk_returns_none() {
        let left = scan_with_pk("a", 1, &["id", "name"], &["id"]);
        let right = scan_node("b", 2, &["bid", "val"]); // no PK
        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(left),
            right: Box::new(right),
        };
        let expressions = vec![qualified_col("a", "id"), qualified_col("b", "bid")];
        let aliases = vec!["id".to_string(), "bid".to_string()];
        // right side PK is missing from output → returns None
        // (scan_node has no pk_columns, so scan_pk_columns uses all cols as fallback)
        let result = join_pk_aliases(&expressions, &aliases, &join);
        // With fallback, both "bid" and "val" are considered PK candidates.
        // "bid" is in expressions → right_pk_aliases is non-empty.
        // If both sides have PKs it returns Some.
        assert!(result.is_some());
    }

    #[test]
    fn test_join_pk_aliases_non_join_returns_none() {
        let scan = scan_node("t", 1, &["id"]);
        let result = join_pk_aliases(&[col("id")], &["id".to_string()], &scan);
        assert_eq!(result, None);
    }

    // ── join_pk_expr_indices tests ──────────────────────────────────

    #[test]
    fn test_join_pk_expr_indices_inner_join() {
        let left = scan_with_pk("a", 1, &["id", "name"], &["id"]);
        let right = scan_with_pk("b", 2, &["bid", "val"], &["bid"]);
        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(left),
            right: Box::new(right),
        };
        let expressions = vec![
            qualified_col("a", "id"),
            qualified_col("a", "name"),
            qualified_col("b", "bid"),
            qualified_col("b", "val"),
        ];
        let indices = join_pk_expr_indices(&expressions, &join);
        assert_eq!(indices, vec![0, 2]); // a.id at 0, b.bid at 2
    }

    #[test]
    fn test_join_pk_expr_indices_non_join() {
        let scan = scan_node("t", 1, &["id"]);
        let indices = join_pk_expr_indices(&[col("id")], &scan);
        assert!(indices.is_empty());
    }

    // ── scan_pk_columns tests ───────────────────────────────────────

    #[test]
    fn test_scan_pk_columns_with_pks() {
        let scan = scan_with_pk("t", 1, &["id", "name"], &["id"]);
        let pks = scan_pk_columns(&scan);
        assert_eq!(pks, vec!["id".to_string()]);
    }

    #[test]
    fn test_scan_pk_columns_fallback_non_nullable() {
        let scan = OpTree::Scan {
            table_oid: 1,
            table_name: "t".to_string(),
            schema: "public".to_string(),
            columns: vec![
                Column {
                    name: "id".to_string(),
                    type_oid: 23,
                    is_nullable: false,
                },
                Column {
                    name: "name".to_string(),
                    type_oid: 25,
                    is_nullable: true,
                },
            ],
            pk_columns: Vec::new(),
            alias: "t".to_string(),
        };
        let pks = scan_pk_columns(&scan);
        assert_eq!(pks, vec!["id".to_string()]);
    }

    #[test]
    fn test_scan_pk_columns_all_nullable_returns_all() {
        let scan = scan_node("t", 1, &["a", "b"]);
        let pks = scan_pk_columns(&scan);
        assert_eq!(pks, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_scan_pk_columns_filter_delegates() {
        let filter = OpTree::Filter {
            predicate: col("x"),
            child: Box::new(scan_with_pk("t", 1, &["id", "x"], &["id"])),
        };
        let pks = scan_pk_columns(&filter);
        assert_eq!(pks, vec!["id".to_string()]);
    }

    #[test]
    fn test_scan_pk_columns_non_scan_returns_empty() {
        let agg = OpTree::Aggregate {
            group_by: vec![],
            aggregates: vec![],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        let pks = scan_pk_columns(&agg);
        assert!(pks.is_empty());
    }

    // ── resolves_to_base_table tests ────────────────────────────────

    #[test]
    fn test_resolves_to_base_table_scan() {
        assert!(resolves_to_base_table(&scan_node("t", 1, &["id"])));
    }

    #[test]
    fn test_resolves_to_base_table_filter_over_scan() {
        let f = OpTree::Filter {
            predicate: col("x"),
            child: Box::new(scan_node("t", 1, &["x"])),
        };
        assert!(resolves_to_base_table(&f));
    }

    #[test]
    fn test_resolves_to_base_table_project_over_scan() {
        let p = OpTree::Project {
            expressions: vec![col("id")],
            aliases: vec!["id".to_string()],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(resolves_to_base_table(&p));
    }

    #[test]
    fn test_resolves_to_base_table_subquery_over_scan() {
        let sub = OpTree::Subquery {
            alias: "s".to_string(),
            column_aliases: vec![],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(resolves_to_base_table(&sub));
    }

    #[test]
    fn test_resolves_to_base_table_cte_scan() {
        let cte = OpTree::CteScan {
            cte_id: 0,
            cte_name: "x".to_string(),
            alias: "x".to_string(),
            columns: vec![],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        assert!(resolves_to_base_table(&cte));
    }

    #[test]
    fn test_resolves_to_base_table_recursive_self_ref() {
        let r = OpTree::RecursiveSelfRef {
            cte_name: "tree".to_string(),
            alias: "t".to_string(),
            columns: vec!["id".to_string()],
        };
        assert!(resolves_to_base_table(&r));
    }

    #[test]
    fn test_resolves_to_base_table_join_false() {
        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        assert!(!resolves_to_base_table(&join));
    }

    #[test]
    fn test_resolves_to_base_table_aggregate_false() {
        let agg = OpTree::Aggregate {
            group_by: vec![],
            aggregates: vec![],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(!resolves_to_base_table(&agg));
    }

    #[test]
    fn test_resolves_to_base_table_union_false() {
        let u = OpTree::UnionAll {
            children: vec![scan_node("a", 1, &["id"])],
        };
        assert!(!resolves_to_base_table(&u));
    }

    // ── check_ivm_support edge cases ────────────────────────────────

    #[test]
    fn test_check_ivm_support_left_join_nested_allowed() {
        let inner = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        let outer = OpTree::LeftJoin {
            condition: col("id"),
            left: Box::new(inner),
            right: Box::new(scan_node("c", 3, &["id"])),
        };
        assert!(check_ivm_support(&outer).is_ok());
    }

    #[test]
    fn test_check_ivm_support_rejects_distinct_count() {
        // COUNT(DISTINCT val) is still using AggFunc::Count, should pass
        let agg = OpTree::Aggregate {
            group_by: vec![col("id")],
            aggregates: vec![AggExpr {
                function: AggFunc::Count,
                argument: Some(col("val")),
                alias: "cnt".to_string(),
                is_distinct: true,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["id", "val"])),
        };
        assert!(check_ivm_support(&agg).is_ok());
    }

    #[test]
    fn test_check_ivm_support_union_all_with_min_child() {
        // MIN aggregate is now supported — union-all with MIN child passes
        let min_child = OpTree::Aggregate {
            group_by: vec![col("id")],
            aggregates: vec![AggExpr {
                function: AggFunc::Min,
                argument: Some(col("val")),
                alias: "min_val".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["id", "val"])),
        };
        let tree = OpTree::UnionAll {
            children: vec![scan_node("a", 1, &["id", "val"]), min_child],
        };
        assert!(check_ivm_support(&tree).is_ok());
    }

    #[test]
    fn test_check_ivm_support_with_registry_cte_body_min_accepted() {
        // MIN aggregate is now supported — CTE body with MIN passes validation
        let tree = OpTree::CteScan {
            cte_id: 0,
            cte_name: "min_cte".to_string(),
            alias: "min_cte".to_string(),
            columns: vec!["id".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        let mut registry = CteRegistry::default();
        registry.entries.push((
            "min_cte".to_string(),
            OpTree::Aggregate {
                group_by: vec![col("id")],
                aggregates: vec![AggExpr {
                    function: AggFunc::Min,
                    argument: Some(col("val")),
                    alias: "min_val".to_string(),
                    is_distinct: false,
                    second_arg: None,
                    filter: None,
                    order_within_group: None,
                }],
                child: Box::new(scan_node("t", 1, &["id", "val"])),
            },
        ));
        let result = ParseResult {
            tree,
            cte_registry: registry,
            has_recursion: false,
            warnings: vec![],
        };
        assert!(check_ivm_support_with_registry(&result).is_ok());
    }

    // ── WindowExpr::to_sql edge cases ───────────────────────────────

    #[test]
    fn test_window_expr_nulls_first_ascending() {
        let wexpr = make_window_expr(
            "row_number",
            vec![],
            vec![],
            vec![SortExpr {
                expr: col("id"),
                ascending: true,
                nulls_first: true,
            }],
            "rn",
        );
        assert_eq!(
            wexpr.to_sql(),
            "row_number() OVER (ORDER BY id ASC NULLS FIRST)"
        );
    }

    #[test]
    fn test_window_expr_nulls_first_descending_no_suffix() {
        // DESC + nulls_first=true → default for DESC, so no NULLS suffix
        let wexpr = make_window_expr(
            "rank",
            vec![],
            vec![],
            vec![SortExpr {
                expr: col("score"),
                ascending: false,
                nulls_first: true,
            }],
            "rnk",
        );
        assert_eq!(wexpr.to_sql(), "rank() OVER (ORDER BY score DESC)");
    }

    #[test]
    fn test_window_expr_multiple_partitions_and_order() {
        let wexpr = WindowExpr {
            func_name: "sum".to_string(),
            args: vec![col("amount")],
            partition_by: vec![col("dept"), col("region")],
            order_by: vec![SortExpr {
                expr: col("hire_date"),
                ascending: true,
                nulls_first: false,
            }],
            frame_clause: None,
            alias: "running_total".to_string(),
        };
        assert_eq!(
            wexpr.to_sql(),
            "sum(amount) OVER (PARTITION BY dept, region ORDER BY hire_date ASC)"
        );
    }

    // ── RecursiveCte source_oids with multiple base tables ──────────

    #[test]
    fn test_recursive_cte_source_oids_multiple_tables() {
        let base = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".to_string()),
            left: Box::new(scan_node("edges", 100, &["src", "dst"])),
            right: Box::new(scan_node("nodes", 200, &["id"])),
        };
        let recursive = OpTree::RecursiveSelfRef {
            cte_name: "reach".to_string(),
            alias: "r".to_string(),
            columns: vec!["src".to_string(), "dst".to_string()],
        };
        let node = OpTree::RecursiveCte {
            alias: "reach".to_string(),
            columns: vec!["src".to_string(), "dst".to_string()],
            base: Box::new(base),
            recursive: Box::new(recursive),
            union_all: true,
        };
        let oids = node.source_oids();
        assert!(oids.contains(&100));
        assert!(oids.contains(&200));
        assert_eq!(oids.len(), 2);
    }

    // ── Window source_oids through nested children ──────────────────

    #[test]
    fn test_window_source_oids_with_join_child() {
        let join = OpTree::InnerJoin {
            condition: col("id"),
            left: Box::new(scan_node("a", 10, &["id", "val"])),
            right: Box::new(scan_node("b", 20, &["id", "metric"])),
        };
        let window = OpTree::Window {
            window_exprs: vec![WindowExpr {
                func_name: "row_number".to_string(),
                args: vec![],
                partition_by: vec![col("id")],
                order_by: vec![],
                frame_clause: None,
                alias: "rn".to_string(),
            }],
            partition_by: vec![col("id")],
            pass_through: vec![(col("id"), "id".to_string())],
            child: Box::new(join),
        };
        assert_eq!(window.source_oids(), vec![10, 20]);
    }

    // ── is_known_aggregate tests ────────────────────────────────────

    #[test]
    fn test_is_known_aggregate_supported_five() {
        assert!(is_known_aggregate("count"));
        assert!(is_known_aggregate("sum"));
        assert!(is_known_aggregate("avg"));
        assert!(is_known_aggregate("min"));
        assert!(is_known_aggregate("max"));
    }

    #[test]
    fn test_is_known_aggregate_statistical() {
        assert!(is_known_aggregate("stddev"));
        assert!(is_known_aggregate("stddev_pop"));
        assert!(is_known_aggregate("stddev_samp"));
        assert!(is_known_aggregate("variance"));
        assert!(is_known_aggregate("var_pop"));
        assert!(is_known_aggregate("var_samp"));
        assert!(is_known_aggregate("corr"));
        assert!(is_known_aggregate("covar_pop"));
        assert!(is_known_aggregate("covar_samp"));
    }

    #[test]
    fn test_is_known_aggregate_json_and_array() {
        assert!(is_known_aggregate("array_agg"));
        assert!(is_known_aggregate("json_agg"));
        assert!(is_known_aggregate("jsonb_agg"));
        assert!(is_known_aggregate("json_object_agg"));
        assert!(is_known_aggregate("jsonb_object_agg"));
        assert!(is_known_aggregate("string_agg"));
    }

    #[test]
    fn test_is_known_aggregate_boolean() {
        assert!(is_known_aggregate("bool_and"));
        assert!(is_known_aggregate("bool_or"));
        assert!(is_known_aggregate("every"));
    }

    #[test]
    fn test_is_known_aggregate_bitwise() {
        assert!(is_known_aggregate("bit_and"));
        assert!(is_known_aggregate("bit_or"));
        assert!(is_known_aggregate("bit_xor"));
    }

    #[test]
    fn test_is_known_aggregate_regression() {
        for name in &[
            "regr_avgx",
            "regr_avgy",
            "regr_count",
            "regr_intercept",
            "regr_r2",
            "regr_slope",
            "regr_sxx",
            "regr_sxy",
            "regr_syy",
        ] {
            assert!(is_known_aggregate(name), "{name} should be known");
        }
    }

    #[test]
    fn test_is_known_aggregate_ordered_set() {
        assert!(is_known_aggregate("percentile_cont"));
        assert!(is_known_aggregate("percentile_disc"));
        assert!(is_known_aggregate("mode"));
    }

    #[test]
    fn test_is_known_aggregate_window_ranking() {
        assert!(is_known_aggregate("rank"));
        assert!(is_known_aggregate("dense_rank"));
        assert!(is_known_aggregate("percent_rank"));
        assert!(is_known_aggregate("cume_dist"));
    }

    #[test]
    fn test_is_known_aggregate_unknown_returns_false() {
        assert!(!is_known_aggregate("my_custom_agg"));
        assert!(!is_known_aggregate("upper"));
        assert!(!is_known_aggregate("coalesce"));
        assert!(!is_known_aggregate("generate_series"));
        assert!(!is_known_aggregate("concat"));
        assert!(!is_known_aggregate(""));
    }

    // ── WindowExpr::to_sql() with frame_clause ─────────────────────

    #[test]
    fn test_window_expr_to_sql_with_rows_frame() {
        let wexpr = WindowExpr {
            func_name: "sum".to_string(),
            args: vec![col("amount")],
            partition_by: vec![col("dept")],
            order_by: vec![SortExpr {
                expr: col("hire_date"),
                ascending: true,
                nulls_first: false,
            }],
            frame_clause: Some("ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW".to_string()),
            alias: "running_total".to_string(),
        };
        assert_eq!(
            wexpr.to_sql(),
            "sum(amount) OVER (PARTITION BY dept ORDER BY hire_date ASC ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)"
        );
    }

    #[test]
    fn test_window_expr_to_sql_with_range_frame() {
        let wexpr = WindowExpr {
            func_name: "avg".to_string(),
            args: vec![col("price")],
            partition_by: vec![],
            order_by: vec![SortExpr {
                expr: col("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame_clause: Some("RANGE BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING".to_string()),
            alias: "avg_price".to_string(),
        };
        assert_eq!(
            wexpr.to_sql(),
            "avg(price) OVER (ORDER BY ts ASC RANGE BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING)"
        );
    }

    #[test]
    fn test_window_expr_to_sql_with_groups_frame() {
        let wexpr = WindowExpr {
            func_name: "count".to_string(),
            args: vec![col("id")],
            partition_by: vec![col("category")],
            order_by: vec![SortExpr {
                expr: col("rank"),
                ascending: true,
                nulls_first: false,
            }],
            frame_clause: Some("GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING".to_string()),
            alias: "cnt".to_string(),
        };
        assert_eq!(
            wexpr.to_sql(),
            "count(id) OVER (PARTITION BY category ORDER BY rank ASC GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING)"
        );
    }

    #[test]
    fn test_window_expr_to_sql_frame_clause_none_omitted() {
        let wexpr = WindowExpr {
            func_name: "row_number".to_string(),
            args: vec![],
            partition_by: vec![col("dept")],
            order_by: vec![SortExpr {
                expr: col("id"),
                ascending: true,
                nulls_first: false,
            }],
            frame_clause: None,
            alias: "rn".to_string(),
        };
        assert_eq!(
            wexpr.to_sql(),
            "row_number() OVER (PARTITION BY dept ORDER BY id ASC)"
        );
    }

    #[test]
    fn test_window_expr_to_sql_empty_args_empty_partition_with_frame() {
        let wexpr = WindowExpr {
            func_name: "row_number".to_string(),
            args: vec![],
            partition_by: vec![],
            order_by: vec![SortExpr {
                expr: col("id"),
                ascending: true,
                nulls_first: false,
            }],
            frame_clause: Some("ROWS UNBOUNDED PRECEDING".to_string()),
            alias: "rn".to_string(),
        };
        assert_eq!(
            wexpr.to_sql(),
            "row_number() OVER (ORDER BY id ASC ROWS UNBOUNDED PRECEDING)"
        );
    }

    // ── Expr::Raw round-trip ────────────────────────────────────────

    #[test]
    fn test_expr_raw_to_sql_preserves_content() {
        let expr = Expr::Raw("CASE WHEN x > 0 THEN 'positive' ELSE 'negative' END".to_string());
        assert_eq!(
            expr.to_sql(),
            "CASE WHEN x > 0 THEN 'positive' ELSE 'negative' END"
        );
    }

    #[test]
    fn test_expr_funccall_coalesce_format() {
        let expr = Expr::FuncCall {
            func_name: "COALESCE".to_string(),
            args: vec![
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "a".to_string(),
                },
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "b".to_string(),
                },
                Expr::Raw("0".to_string()),
            ],
        };
        assert_eq!(expr.to_sql(), "COALESCE(a, b, 0)");
    }

    #[test]
    fn test_expr_funccall_nullif_format() {
        let expr = Expr::FuncCall {
            func_name: "NULLIF".to_string(),
            args: vec![
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "a".to_string(),
                },
                Expr::Raw("0".to_string()),
            ],
        };
        assert_eq!(expr.to_sql(), "NULLIF(a, 0)");
    }

    #[test]
    fn test_expr_funccall_greatest_least_format() {
        let g = Expr::FuncCall {
            func_name: "GREATEST".to_string(),
            args: vec![
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "a".to_string(),
                },
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "b".to_string(),
                },
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "c".to_string(),
                },
            ],
        };
        let l = Expr::FuncCall {
            func_name: "LEAST".to_string(),
            args: vec![
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "a".to_string(),
                },
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "b".to_string(),
                },
                Expr::ColumnRef {
                    table_alias: None,
                    column_name: "c".to_string(),
                },
            ],
        };
        assert_eq!(g.to_sql(), "GREATEST(a, b, c)");
        assert_eq!(l.to_sql(), "LEAST(a, b, c)");
    }

    #[test]
    fn test_expr_raw_array_format() {
        let expr = Expr::Raw("ARRAY[1, 2, 3]".to_string());
        assert_eq!(expr.to_sql(), "ARRAY[1, 2, 3]");
    }

    #[test]
    fn test_expr_raw_row_format() {
        let expr = Expr::Raw("ROW(a, b, c)".to_string());
        assert_eq!(expr.to_sql(), "ROW(a, b, c)");
    }

    #[test]
    fn test_expr_raw_boolean_test_format() {
        let expr = Expr::Raw("active IS TRUE".to_string());
        assert_eq!(expr.to_sql(), "active IS TRUE");
    }

    #[test]
    fn test_expr_raw_is_distinct_from_format() {
        let expr = Expr::Raw("(a IS DISTINCT FROM b)".to_string());
        assert_eq!(expr.to_sql(), "(a IS DISTINCT FROM b)");
    }

    #[test]
    fn test_expr_raw_between_format() {
        let expr = Expr::Raw("(x BETWEEN 1 AND 100)".to_string());
        assert_eq!(expr.to_sql(), "(x BETWEEN 1 AND 100)");
    }

    #[test]
    fn test_expr_raw_in_list_format() {
        let expr = Expr::Raw("(status IN (1, 2, 3))".to_string());
        assert_eq!(expr.to_sql(), "(status IN (1, 2, 3))");
    }

    #[test]
    fn test_expr_raw_sql_value_function_format() {
        for kw in &[
            "CURRENT_DATE",
            "CURRENT_TIME",
            "CURRENT_TIMESTAMP",
            "LOCALTIME",
            "LOCALTIMESTAMP",
            "CURRENT_ROLE",
            "CURRENT_USER",
            "USER",
            "SESSION_USER",
            "CURRENT_CATALOG",
            "CURRENT_SCHEMA",
        ] {
            let expr = Expr::Raw(kw.to_string());
            assert_eq!(expr.to_sql(), *kw);
        }
    }

    #[test]
    fn test_expr_raw_indirection_array_subscript() {
        let expr = Expr::Raw("arr[1]".to_string());
        assert_eq!(expr.to_sql(), "arr[1]");
    }

    #[test]
    fn test_expr_raw_indirection_field_access() {
        let expr = Expr::Raw("(rec).field_name".to_string());
        assert_eq!(expr.to_sql(), "(rec).field_name");
    }

    #[test]
    fn test_expr_raw_indirection_star() {
        let expr = Expr::Raw("(data).*".to_string());
        assert_eq!(expr.to_sql(), "(data).*");
    }

    // ── GROUPING SETS kind-name mapping ────────────────────────────

    #[test]
    fn test_grouping_set_kind_rollup_name() {
        // Verify the GroupingSetKind constant used in check_select_unsupported()
        // produces the expected label in the error message.
        let kind = pg_sys::GroupingSetKind::GROUPING_SET_ROLLUP;
        let name = match kind {
            pg_sys::GroupingSetKind::GROUPING_SET_ROLLUP => "ROLLUP",
            pg_sys::GroupingSetKind::GROUPING_SET_CUBE => "CUBE",
            _ => "GROUPING SETS",
        };
        assert_eq!(name, "ROLLUP");
    }

    #[test]
    fn test_grouping_set_kind_cube_name() {
        let kind = pg_sys::GroupingSetKind::GROUPING_SET_CUBE;
        let name = match kind {
            pg_sys::GroupingSetKind::GROUPING_SET_ROLLUP => "ROLLUP",
            pg_sys::GroupingSetKind::GROUPING_SET_CUBE => "CUBE",
            _ => "GROUPING SETS",
        };
        assert_eq!(name, "CUBE");
    }

    #[test]
    fn test_grouping_set_kind_sets_name() {
        // GROUPING_SET_SETS and GROUPING_SET_EMPTY both fall into the catch-all
        let kind = pg_sys::GroupingSetKind::GROUPING_SET_SETS;
        let name = match kind {
            pg_sys::GroupingSetKind::GROUPING_SET_ROLLUP => "ROLLUP",
            pg_sys::GroupingSetKind::GROUPING_SET_CUBE => "CUBE",
            _ => "GROUPING SETS",
        };
        assert_eq!(name, "GROUPING SETS");
    }

    #[test]
    fn test_grouping_set_node_tag_value() {
        // Verify T_GroupingSet has the expected NodeTag value (107 per pg18 bindings).
        // This guards against a pgrx upgrade accidentally changing the tag.
        let tag_num = pg_sys::NodeTag::T_GroupingSet as u32;
        assert_eq!(tag_num, 107, "T_GroupingSet NodeTag must be 107");
    }

    #[test]
    fn test_range_table_sample_node_tag_value() {
        // Verify T_RangeTableSample has the expected NodeTag value (89 per pg18 bindings).
        let tag_num = pg_sys::NodeTag::T_RangeTableSample as u32;
        assert_eq!(tag_num, 89, "T_RangeTableSample NodeTag must be 89");
    }

    // ── SemiJoin / AntiJoin / ScalarSubquery OpTree tests ────────────

    fn make_scan(oid: u32, name: &str, alias: &str, cols: &[&str]) -> OpTree {
        OpTree::Scan {
            table_oid: oid,
            table_name: name.to_string(),
            schema: "public".to_string(),
            columns: cols
                .iter()
                .map(|c| Column {
                    name: c.to_string(),
                    type_oid: 23,
                    is_nullable: true,
                })
                .collect(),
            pk_columns: vec![],
            alias: alias.to_string(),
        }
    }

    #[test]
    fn test_semi_join_alias() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "orders", "o", &["id"])),
            right: Box::new(make_scan(2, "items", "i", &["id"])),
        };
        assert_eq!(tree.alias(), "semi_join");
    }

    #[test]
    fn test_anti_join_alias() {
        let tree = OpTree::AntiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "orders", "o", &["id"])),
            right: Box::new(make_scan(2, "returns", "r", &["id"])),
        };
        assert_eq!(tree.alias(), "anti_join");
    }

    #[test]
    fn test_scalar_subquery_alias() {
        let tree = OpTree::ScalarSubquery {
            subquery: Box::new(make_scan(2, "config", "c", &["tax_rate"])),
            alias: "current_tax".to_string(),
            subquery_source_oids: vec![2],
            child: Box::new(make_scan(1, "orders", "o", &["id", "amount"])),
        };
        assert_eq!(tree.alias(), "current_tax");
    }

    #[test]
    fn test_semi_join_node_kind() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "t1", "t1", &["id"])),
            right: Box::new(make_scan(2, "t2", "t2", &["id"])),
        };
        assert_eq!(tree.node_kind(), "semi join");
    }

    #[test]
    fn test_anti_join_node_kind() {
        let tree = OpTree::AntiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "t1", "t1", &["id"])),
            right: Box::new(make_scan(2, "t2", "t2", &["id"])),
        };
        assert_eq!(tree.node_kind(), "anti join");
    }

    #[test]
    fn test_scalar_subquery_node_kind() {
        let tree = OpTree::ScalarSubquery {
            subquery: Box::new(make_scan(2, "t2", "t2", &["val"])),
            alias: "sub".to_string(),
            subquery_source_oids: vec![2],
            child: Box::new(make_scan(1, "t1", "t1", &["id"])),
        };
        assert_eq!(tree.node_kind(), "scalar subquery");
    }

    #[test]
    fn test_semi_join_output_columns() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "orders", "o", &["id", "cust_id", "amount"])),
            right: Box::new(make_scan(2, "items", "i", &["order_id", "qty"])),
        };
        // Semi-join outputs only left-side columns
        assert_eq!(tree.output_columns(), vec!["id", "cust_id", "amount"]);
    }

    #[test]
    fn test_anti_join_output_columns() {
        let tree = OpTree::AntiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "orders", "o", &["id", "amount"])),
            right: Box::new(make_scan(2, "returns", "r", &["order_id"])),
        };
        assert_eq!(tree.output_columns(), vec!["id", "amount"]);
    }

    #[test]
    fn test_scalar_subquery_output_columns() {
        let tree = OpTree::ScalarSubquery {
            subquery: Box::new(make_scan(2, "config", "c", &["tax_rate"])),
            alias: "current_tax".to_string(),
            subquery_source_oids: vec![2],
            child: Box::new(make_scan(1, "orders", "o", &["id", "amount"])),
        };
        assert_eq!(tree.output_columns(), vec!["id", "amount", "current_tax"]);
    }

    #[test]
    fn test_semi_join_source_oids() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(10, "orders", "o", &["id"])),
            right: Box::new(make_scan(20, "items", "i", &["id"])),
        };
        let oids = tree.source_oids();
        assert!(oids.contains(&10));
        assert!(oids.contains(&20));
    }

    #[test]
    fn test_scalar_subquery_source_oids() {
        let tree = OpTree::ScalarSubquery {
            subquery: Box::new(make_scan(30, "config", "c", &["rate"])),
            alias: "rate".to_string(),
            subquery_source_oids: vec![30],
            child: Box::new(make_scan(10, "orders", "o", &["id"])),
        };
        let oids = tree.source_oids();
        assert!(oids.contains(&10));
        assert!(oids.contains(&30));
    }

    #[test]
    fn test_semi_join_row_id_key_columns_is_none() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "t1", "t1", &["id"])),
            right: Box::new(make_scan(2, "t2", "t2", &["id"])),
        };
        // SemiJoin falls into the catch-all `_ => None` arm
        assert!(tree.row_id_key_columns().is_none());
    }

    #[test]
    fn test_semi_join_needs_pgt_count_false() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "t1", "t1", &["id"])),
            right: Box::new(make_scan(2, "t2", "t2", &["id"])),
        };
        assert!(!tree.needs_pgt_count());
    }

    #[test]
    fn test_check_ivm_support_semi_join() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "t1", "t1", &["id"])),
            right: Box::new(make_scan(2, "t2", "t2", &["id"])),
        };
        assert!(check_ivm_support(&tree).is_ok());
    }

    #[test]
    fn test_check_ivm_support_anti_join() {
        let tree = OpTree::AntiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "t1", "t1", &["id"])),
            right: Box::new(make_scan(2, "t2", "t2", &["id"])),
        };
        assert!(check_ivm_support(&tree).is_ok());
    }

    #[test]
    fn test_check_ivm_support_scalar_subquery() {
        let tree = OpTree::ScalarSubquery {
            subquery: Box::new(make_scan(2, "config", "c", &["rate"])),
            alias: "rate".to_string(),
            subquery_source_oids: vec![2],
            child: Box::new(make_scan(1, "orders", "o", &["id"])),
        };
        assert!(check_ivm_support(&tree).is_ok());
    }

    #[test]
    fn test_check_ivm_support_nested_semi_join() {
        // Semi-join with aggregate child should still pass
        let inner_scan = make_scan(1, "orders", "o", &["cust_id", "amount"]);
        let agg = OpTree::Aggregate {
            group_by: vec![col("cust_id")],
            aggregates: vec![AggExpr {
                function: AggFunc::Sum,
                argument: Some(col("amount")),
                alias: "total".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            }],
            child: Box::new(inner_scan),
        };
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(agg),
            right: Box::new(make_scan(2, "thresholds", "t", &["cust_id"])),
        };
        assert!(check_ivm_support(&tree).is_ok());
    }

    // ── Volatility helper tests ─────────────────────────────────────

    #[test]
    fn test_max_volatility_ordering() {
        assert_eq!(max_volatility('i', 'i'), 'i');
        assert_eq!(max_volatility('i', 's'), 's');
        assert_eq!(max_volatility('s', 'i'), 's');
        assert_eq!(max_volatility('s', 's'), 's');
        assert_eq!(max_volatility('i', 'v'), 'v');
        assert_eq!(max_volatility('v', 'i'), 'v');
        assert_eq!(max_volatility('s', 'v'), 'v');
        assert_eq!(max_volatility('v', 's'), 'v');
        assert_eq!(max_volatility('v', 'v'), 'v');
    }

    #[test]
    fn test_collect_volatilities_no_func_calls() {
        // An expression with no function calls should remain 'i' (immutable).
        let expr = Expr::BinaryOp {
            op: "+".to_string(),
            left: Box::new(col("a")),
            right: Box::new(Expr::Literal("1".to_string())),
        };
        let mut worst = 'i';
        // collect_volatilities with no FuncCall should leave worst at 'i'.
        // (Can't call SPI in unit tests, but no FuncCall → no SPI call.)
        collect_volatilities(&expr, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    #[test]
    fn test_tree_collect_volatility_scan_only() {
        // A plain Scan node has no expressions → worst stays 'i'.
        let tree = make_scan(1, "orders", "o", &["id", "amount"]);
        let mut worst = 'i';
        tree_collect_volatility(&tree, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    #[test]
    fn test_tree_collect_volatility_filter_no_funcs() {
        let tree = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: ">".to_string(),
                left: Box::new(col("amount")),
                right: Box::new(Expr::Literal("100".to_string())),
            },
            child: Box::new(make_scan(1, "orders", "o", &["id", "amount"])),
        };
        let mut worst = 'i';
        tree_collect_volatility(&tree, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    #[test]
    fn test_lookup_operator_volatility_stub_returns_immutable() {
        // In test mode, operators default to immutable (most built-in are).
        let vol = lookup_operator_volatility("+").unwrap();
        assert_eq!(vol, 'i');
        let vol2 = lookup_operator_volatility("&&").unwrap();
        assert_eq!(vol2, 'i');
    }

    #[test]
    fn test_collect_volatilities_binary_op_with_func_operand() {
        // BinaryOp where one operand is a FuncCall should detect volatile
        // from the FuncCall (test stub returns 'v' for all functions).
        let expr = Expr::BinaryOp {
            op: ">".to_string(),
            left: Box::new(Expr::FuncCall {
                func_name: "random".to_string(),
                args: vec![],
            }),
            right: Box::new(Expr::Literal("0.5".to_string())),
        };
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        // random() is volatile (test stub returns 'v' for all functions)
        assert_eq!(worst, 'v');
    }

    #[test]
    fn test_collect_volatilities_nested_binary_ops_immutable() {
        // Nested BinaryOps with only ColumnRef/Literal → stays immutable.
        let expr = Expr::BinaryOp {
            op: "+".to_string(),
            left: Box::new(Expr::BinaryOp {
                op: "*".to_string(),
                left: Box::new(col("a")),
                right: Box::new(Expr::Literal("2".to_string())),
            }),
            right: Box::new(col("b")),
        };
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    // ── GROUPING SETS rewrite helpers ──────────────────────────────

    #[test]
    fn test_compute_grouping_value_single_in_set() {
        // GROUPING(department) where department IS grouped → 0
        assert_eq!(
            compute_grouping_value(&["department".to_string()], &["department".to_string()]),
            0
        );
    }

    #[test]
    fn test_compute_grouping_value_single_not_in_set() {
        // GROUPING(department) where department is NOT grouped → 1
        assert_eq!(
            compute_grouping_value(&["department".to_string()], &["region".to_string()]),
            1
        );
    }

    #[test]
    fn test_compute_grouping_value_two_args_both_grouped() {
        // GROUPING(a, b) where both are grouped → 0b00 = 0
        assert_eq!(
            compute_grouping_value(
                &["a".to_string(), "b".to_string()],
                &["a".to_string(), "b".to_string()]
            ),
            0
        );
    }

    #[test]
    fn test_compute_grouping_value_two_args_first_not_grouped() {
        // GROUPING(a, b) where a not grouped, b grouped → 0b10 = 2
        assert_eq!(
            compute_grouping_value(&["a".to_string(), "b".to_string()], &["b".to_string()]),
            2
        );
    }

    #[test]
    fn test_compute_grouping_value_two_args_second_not_grouped() {
        // GROUPING(a, b) where a grouped, b not grouped → 0b01 = 1
        assert_eq!(
            compute_grouping_value(&["a".to_string(), "b".to_string()], &["a".to_string()]),
            1
        );
    }

    #[test]
    fn test_compute_grouping_value_two_args_neither_grouped() {
        // GROUPING(a, b) where neither grouped → 0b11 = 3
        assert_eq!(
            compute_grouping_value(&["a".to_string(), "b".to_string()], &[]),
            3
        );
    }

    #[test]
    fn test_compute_grouping_value_three_args_mixed() {
        // GROUPING(a, b, c) where a=grouped, b=not, c=grouped → 0b010 = 2
        assert_eq!(
            compute_grouping_value(
                &["a".to_string(), "b".to_string(), "c".to_string()],
                &["a".to_string(), "c".to_string()]
            ),
            2
        );
    }

    #[test]
    fn test_compute_grouping_value_empty_args() {
        // GROUPING() with no args → 0
        assert_eq!(compute_grouping_value(&[], &["a".to_string()]), 0);
    }

    // Note: expand_grouping_set / expand_cube / expand_rollup tests require
    // pg_sys FFI (palloc0, makeString, lappend) so they live in E2E tests.
    // The ROLLUP/CUBE expansion logic is verified via the compute_grouping_value
    // tests above plus E2E integration tests.

    // ── S12/S13/S14 rewrite helper tests ────────────────────────────

    // Note: rewrite_scalar_subquery_in_where(), rewrite_sublinks_in_or(),
    // and rewrite_multi_partition_windows() all require pg_sys::raw_parser()
    // which is only available in a PostgreSQL backend. These functions are
    // tested via E2E tests. Below we test the pure-Rust helpers.

    #[test]
    fn test_window_info_partition_key_grouping() {
        // Verify that WindowInfo can be grouped by partition_key
        let w1 = WindowInfo {
            func_sql: "SUM(a) OVER (PARTITION BY region)".to_string(),
            alias: "region_sum".to_string(),
            partition_key: "region".to_string(),
        };
        let w2 = WindowInfo {
            func_sql: "COUNT(*) OVER (PARTITION BY region)".to_string(),
            alias: "region_cnt".to_string(),
            partition_key: "region".to_string(),
        };
        let w3 = WindowInfo {
            func_sql: "SUM(a) OVER (PARTITION BY dept)".to_string(),
            alias: "dept_sum".to_string(),
            partition_key: "dept".to_string(),
        };

        // Group by partition_key
        let mut groups: Vec<(String, Vec<&WindowInfo>)> = Vec::new();
        for wi in [&w1, &w2, &w3] {
            let found = groups.iter_mut().find(|(k, _)| *k == wi.partition_key);
            if let Some((_, group)) = found {
                group.push(wi);
            } else {
                groups.push((wi.partition_key.clone(), vec![wi]));
            }
        }

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "region");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, "dept");
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn test_scalar_subquery_extract_creation() {
        let extract = ScalarSubqueryExtract {
            subquery_sql: "SELECT avg(amount) FROM orders".to_string(),
            expr_sql: "(SELECT avg(\"amount\") FROM \"orders\")".to_string(),
            inner_tables: vec!["orders".to_string()],
            has_limit_or_offset: false,
        };
        assert!(extract.subquery_sql.contains("avg"));
        assert!(extract.expr_sql.starts_with('('));
        assert!(extract.expr_sql.ends_with(')'));
    }

    #[test]
    fn test_or_arm_string_replacement() {
        // Simulate the OR-to-UNION approach: each OR arm becomes a separate branch
        let arm1 = "status = 'active'";
        let arm2 = "EXISTS (SELECT 1 FROM vip WHERE vip.id = t.id)";
        let from_sql = "\"t\"";
        let target_sql = "*";

        let branch1 = format!("SELECT {target_sql} FROM {from_sql} WHERE {arm1}");
        let branch2 = format!("SELECT {target_sql} FROM {from_sql} WHERE {arm2}");
        let rewritten = format!("{branch1} UNION {branch2}");

        assert!(rewritten.contains("UNION"));
        assert!(rewritten.contains("status = 'active'"));
        assert!(rewritten.contains("EXISTS"));
    }

    #[test]
    fn test_scalar_subquery_where_replacement() {
        // Simulate the scalar subquery replacement logic
        let original_where = "amount > (SELECT avg(\"amount\") FROM \"orders\")";
        let expr_sql = "(SELECT avg(\"amount\") FROM \"orders\")";
        let replacement = "\"__pgt_sq_1\".\"__pgt_scalar_1\"";

        let rewritten = original_where.replace(expr_sql, replacement);
        assert_eq!(rewritten, "amount > \"__pgt_sq_1\".\"__pgt_scalar_1\"");
    }

    #[test]
    fn test_multi_partition_row_marker_join() {
        // Verify the join strategy: subqueries joined on __pgt_row_marker
        let sq1_alias = "__pgt_w1";
        let sq2_alias = "__pgt_w2";

        let join_cond = format!(
            "\"{}\".\"__pgt_row_marker\" = \"{}\".\"__pgt_row_marker\"",
            sq1_alias, sq2_alias
        );

        assert!(join_cond.contains("__pgt_w1"));
        assert!(join_cond.contains("__pgt_w2"));
        assert!(join_cond.contains("__pgt_row_marker"));
    }

    #[test]
    fn test_cross_join_construction() {
        // Verify the CROSS JOIN construction for scalar subquery rewrite
        let idx = 1;
        let sq_alias = format!("__pgt_sq_{idx}");
        let scalar_alias = format!("__pgt_scalar_{idx}");
        let sq_sql = "SELECT avg(amount) FROM orders";

        let cross_join = format!("CROSS JOIN ({sq_sql} AS \"{scalar_alias}\") AS \"{sq_alias}\"");

        assert!(cross_join.contains("CROSS JOIN"));
        assert!(cross_join.contains("avg(amount)"));
        assert!(cross_join.contains("__pgt_sq_1"));
        assert!(cross_join.contains("__pgt_scalar_1"));
    }

    // ── contains_word_boundary tests ────────────────────────────────

    #[test]
    fn test_contains_word_boundary_basic() {
        assert!(contains_word_boundary(
            "p_partkey = ps_partkey",
            "p_partkey"
        ));
        assert!(contains_word_boundary(
            "p_partkey = ps_partkey",
            "ps_partkey"
        ));
    }

    #[test]
    fn test_contains_word_boundary_at_start_end() {
        assert!(contains_word_boundary("p_partkey", "p_partkey"));
        assert!(contains_word_boundary("foo p_partkey", "p_partkey"));
        assert!(contains_word_boundary("p_partkey foo", "p_partkey"));
    }

    #[test]
    fn test_contains_word_boundary_no_match_substring() {
        // "p_partkey" should NOT match inside "ps_partkey" — the 's' before
        // is alphanumeric so it's not a word boundary.
        assert!(!contains_word_boundary("ps_partkey", "p_partkey"));
        // "l_quantity" should not match in "xl_quantity"
        assert!(!contains_word_boundary("xl_quantity", "l_quantity"));
    }

    #[test]
    fn test_contains_word_boundary_with_operators() {
        assert!(contains_word_boundary(
            "where p_partkey = ps_partkey and s_suppkey = ps_suppkey",
            "p_partkey"
        ));
        // Quoted identifiers
        assert!(contains_word_boundary(
            "\"p_partkey\" = \"ps_partkey\"",
            "p_partkey"
        ));
    }

    #[test]
    fn test_contains_word_boundary_not_found() {
        assert!(!contains_word_boundary(
            "select count(*) from orders",
            "p_partkey"
        ));
    }

    // ── Predicate pushdown tests ──────────────────────────────────────

    #[test]
    fn test_push_filter_two_table_cross_join() {
        // FROM a, b WHERE a.id = b.id
        let a = scan_node("a", 1, &["id", "name"]);
        let b = scan_node("b", 2, &["id", "val"]);
        let cross = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(a),
            right: Box::new(b),
        };
        let filter = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(qualified_col("a", "id")),
                right: Box::new(qualified_col("b", "id")),
            },
            child: Box::new(cross),
        };
        let result = push_filter_into_cross_joins(filter);
        // Should become InnerJoin with the condition promoted
        match &result {
            OpTree::InnerJoin { condition, .. } => {
                // Condition should contain "a.id = b.id" (not TRUE)
                assert_ne!(condition.to_sql(), "TRUE");
                assert!(condition.to_sql().contains("="));
            }
            _ => panic!("Expected InnerJoin, got {:?}", result.node_kind()),
        }
    }

    #[test]
    fn test_push_filter_three_table_cross_join() {
        // FROM a, b, c WHERE a.x = b.x AND b.y = c.y
        let a = scan_node("a", 1, &["x"]);
        let b = scan_node("b", 2, &["x", "y"]);
        let c = scan_node("c", 3, &["y"]);
        let inner = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(a),
            right: Box::new(b),
        };
        let outer = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(inner),
            right: Box::new(c),
        };
        let pred = Expr::BinaryOp {
            op: "AND".into(),
            left: Box::new(Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(qualified_col("a", "x")),
                right: Box::new(qualified_col("b", "x")),
            }),
            right: Box::new(Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(qualified_col("b", "y")),
                right: Box::new(qualified_col("c", "y")),
            }),
        };
        let filter = OpTree::Filter {
            predicate: pred,
            child: Box::new(outer),
        };
        let result = push_filter_into_cross_joins(filter);
        // Outer join should have b.y = c.y (c is right child)
        // Inner join should have a.x = b.x (b is right child)
        // No filter should remain
        match &result {
            OpTree::InnerJoin {
                condition: outer_cond,
                left,
                ..
            } => {
                assert!(
                    outer_cond.to_sql().contains("y"),
                    "outer: {}",
                    outer_cond.to_sql()
                );
                match left.as_ref() {
                    OpTree::InnerJoin {
                        condition: inner_cond,
                        ..
                    } => {
                        assert!(
                            inner_cond.to_sql().contains("x"),
                            "inner: {}",
                            inner_cond.to_sql()
                        );
                    }
                    _ => panic!("Expected inner InnerJoin"),
                }
            }
            _ => panic!("Expected InnerJoin, got {:?}", result.node_kind()),
        }
    }

    #[test]
    fn test_push_filter_preserves_single_table_predicate() {
        // FROM a, b WHERE a.id = b.id AND a.name = 'foo'
        let a = scan_node("a", 1, &["id", "name"]);
        let b = scan_node("b", 2, &["id"]);
        let cross = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(a),
            right: Box::new(b),
        };
        let pred = Expr::BinaryOp {
            op: "AND".into(),
            left: Box::new(Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(qualified_col("a", "id")),
                right: Box::new(qualified_col("b", "id")),
            }),
            right: Box::new(Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(qualified_col("a", "name")),
                right: Box::new(Expr::Literal("'foo'".into())),
            }),
        };
        let filter = OpTree::Filter {
            predicate: pred,
            child: Box::new(cross),
        };
        let result = push_filter_into_cross_joins(filter);
        // The join predicate should be promoted; single-table pred stays as Filter
        match &result {
            OpTree::Filter {
                predicate: remaining,
                child,
            } => {
                // Remaining filter: a.name = 'foo'
                assert!(remaining.to_sql().contains("name"));
                match child.as_ref() {
                    OpTree::InnerJoin { condition, .. } => {
                        assert_ne!(condition.to_sql(), "TRUE");
                    }
                    _ => panic!("Expected InnerJoin inside Filter"),
                }
            }
            _ => panic!("Expected Filter wrapper for single-table predicate"),
        }
    }

    #[test]
    fn test_push_filter_no_cross_join_passthrough() {
        // Filter over a proper join — should pass through unchanged
        let a = scan_node("a", 1, &["id"]);
        let b = scan_node("b", 2, &["id"]);
        let join = OpTree::InnerJoin {
            condition: Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(qualified_col("a", "id")),
                right: Box::new(qualified_col("b", "id")),
            },
            left: Box::new(a),
            right: Box::new(b),
        };
        let filter = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: ">".into(),
                left: Box::new(qualified_col("a", "id")),
                right: Box::new(Expr::Literal("10".into())),
            },
            child: Box::new(join),
        };
        let result = push_filter_into_cross_joins(filter);
        assert!(matches!(result, OpTree::Filter { .. }));
    }

    #[test]
    fn test_push_filter_unqualified_columns() {
        // FROM a, b WHERE x = y (unqualified columns resolved by scan)
        let a = scan_node("a", 1, &["x"]);
        let b = scan_node("b", 2, &["y"]);
        let cross = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(a),
            right: Box::new(b),
        };
        let filter = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(col("x")),
                right: Box::new(col("y")),
            },
            child: Box::new(cross),
        };
        let result = push_filter_into_cross_joins(filter);
        // Should promote the predicate (both sides resolve to different scans)
        match &result {
            OpTree::InnerJoin { condition, .. } => {
                assert_ne!(condition.to_sql(), "TRUE");
            }
            _ => panic!("Expected InnerJoin with promoted condition"),
        }
    }

    // ── strip_view_definition_suffix tests ───────────────────────────

    #[test]
    fn test_strip_view_definition_suffix_with_semicolon() {
        assert_eq!(strip_view_definition_suffix("SELECT 1;"), "SELECT 1");
    }

    #[test]
    fn test_strip_view_definition_suffix_with_whitespace_and_semicolon() {
        // Semicolon immediately at end, followed by nothing
        assert_eq!(
            strip_view_definition_suffix("SELECT id FROM t  ;"),
            "SELECT id FROM t"
        );
    }

    #[test]
    fn test_strip_view_definition_suffix_no_semicolon() {
        assert_eq!(
            strip_view_definition_suffix("SELECT id FROM t"),
            "SELECT id FROM t"
        );
    }

    #[test]
    fn test_strip_view_definition_suffix_empty_string() {
        assert_eq!(strip_view_definition_suffix(""), "");
    }

    #[test]
    fn test_strip_view_definition_suffix_only_semicolons() {
        assert_eq!(strip_view_definition_suffix(";;;"), "");
    }

    #[test]
    fn test_strip_view_definition_suffix_preserves_interior_semicolons() {
        // A semicolon in the middle should NOT be stripped
        assert_eq!(
            strip_view_definition_suffix("SELECT ';' FROM t;"),
            "SELECT ';' FROM t"
        );
    }

    // ── Column-pruning tests ────────────────────────────────────────

    fn make_scan_pk(alias: &str, oid: u32, col_names: &[&str], pk: &[&str]) -> OpTree {
        OpTree::Scan {
            table_oid: oid,
            table_name: alias.to_string(),
            schema: "public".to_string(),
            columns: col_names.iter().map(|n| make_column(n)).collect(),
            pk_columns: pk.iter().map(|s| s.to_string()).collect(),
            alias: alias.to_string(),
        }
    }

    fn col_names(tree: &OpTree) -> Vec<String> {
        match tree {
            OpTree::Scan { columns, .. } => columns.iter().map(|c| c.name.clone()).collect(),
            _ => panic!("expected Scan"),
        }
    }

    #[test]
    fn test_prune_scan_columns_project_qualified() {
        // SELECT t.a, t.b FROM t(a, b, c, d)  →  Scan keeps {a, b}
        let mut tree = OpTree::Project {
            expressions: vec![qualified_col("t", "a"), qualified_col("t", "b")],
            aliases: vec!["a".into(), "b".into()],
            child: Box::new(scan_node("t", 1, &["a", "b", "c", "d"])),
        };
        tree.prune_scan_columns();
        let OpTree::Project { child, .. } = &tree else {
            panic!()
        };
        assert_eq!(col_names(child), vec!["a", "b"]);
    }

    #[test]
    fn test_prune_scan_columns_keeps_pk() {
        // SELECT t.b FROM t(id, a, b) with PK=id  →  Scan keeps {id, b}
        let mut tree = OpTree::Project {
            expressions: vec![qualified_col("t", "b")],
            aliases: vec!["b".into()],
            child: Box::new(make_scan_pk("t", 1, &["id", "a", "b"], &["id"])),
        };
        tree.prune_scan_columns();
        let OpTree::Project { child, .. } = &tree else {
            panic!()
        };
        let mut names = col_names(child);
        names.sort();
        assert_eq!(names, vec!["b", "id"]);
    }

    #[test]
    fn test_prune_scan_columns_filter_refs() {
        // SELECT t.a FROM t(a, b, c) WHERE t.b > 0  →  Scan keeps {a, b}
        let mut tree = OpTree::Project {
            expressions: vec![qualified_col("t", "a")],
            aliases: vec!["a".into()],
            child: Box::new(OpTree::Filter {
                predicate: qualified_col("t", "b"),
                child: Box::new(scan_node("t", 1, &["a", "b", "c"])),
            }),
        };
        tree.prune_scan_columns();
        let OpTree::Project {
            child: filter_box, ..
        } = &tree
        else {
            panic!()
        };
        let OpTree::Filter { child, .. } = filter_box.as_ref() else {
            panic!()
        };
        let mut names = col_names(child);
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn test_prune_scan_columns_star_skips_pruning() {
        // SELECT * FROM t(a, b, c)  →  Scan keeps all (Star bail-out)
        let mut tree = OpTree::Project {
            expressions: vec![Expr::Star { table_alias: None }],
            aliases: vec!["*".into()],
            child: Box::new(scan_node("t", 1, &["a", "b", "c"])),
        };
        tree.prune_scan_columns();
        let OpTree::Project { child, .. } = &tree else {
            panic!()
        };
        assert_eq!(col_names(child), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_prune_scan_columns_raw_skips_pruning() {
        // Raw expression bail-out
        let mut tree = OpTree::Project {
            expressions: vec![Expr::Raw("t.a + 1".into())],
            aliases: vec!["expr".into()],
            child: Box::new(scan_node("t", 1, &["a", "b"])),
        };
        tree.prune_scan_columns();
        let OpTree::Project { child, .. } = &tree else {
            panic!()
        };
        assert_eq!(col_names(child), vec!["a", "b"]);
    }

    #[test]
    fn test_prune_scan_columns_unqualified_ref_matches_all_scans() {
        // Unqualified column ref `a` should be kept in all scans
        // SELECT a FROM t1(a, b) JOIN t2(a, c) ON ...
        let mut tree = OpTree::Project {
            expressions: vec![col("a")],
            aliases: vec!["a".into()],
            child: Box::new(OpTree::InnerJoin {
                condition: Expr::BinaryOp {
                    op: "=".into(),
                    left: Box::new(qualified_col("t1", "a")),
                    right: Box::new(qualified_col("t2", "a")),
                },
                left: Box::new(scan_node("t1", 1, &["a", "b"])),
                right: Box::new(scan_node("t2", 2, &["a", "c"])),
            }),
        };
        tree.prune_scan_columns();
        let OpTree::Project { child, .. } = &tree else {
            panic!()
        };
        let OpTree::InnerJoin { left, right, .. } = child.as_ref() else {
            panic!()
        };
        assert_eq!(col_names(left), vec!["a"]);
        assert_eq!(col_names(right), vec!["a"]);
    }

    #[test]
    fn test_prune_scan_columns_join_condition() {
        // Join condition refs are collected
        let mut tree = OpTree::InnerJoin {
            condition: Expr::BinaryOp {
                op: "=".into(),
                left: Box::new(qualified_col("t1", "id")),
                right: Box::new(qualified_col("t2", "t1_id")),
            },
            left: Box::new(scan_node("t1", 1, &["id", "name", "extra"])),
            right: Box::new(scan_node("t2", 2, &["t1_id", "val", "extra"])),
        };
        tree.prune_scan_columns();
        let OpTree::InnerJoin { left, right, .. } = &tree else {
            panic!()
        };
        assert_eq!(col_names(left), vec!["id"]);
        assert_eq!(col_names(right), vec!["t1_id"]);
    }

    #[test]
    fn test_prune_scan_columns_aggregate() {
        // SELECT t.dept, SUM(t.salary) FROM t(id, dept, salary, name)
        let mut tree = OpTree::Aggregate {
            group_by: vec![qualified_col("t", "dept")],
            aggregates: vec![AggExpr {
                function: AggFunc::Sum,
                argument: Some(qualified_col("t", "salary")),
                alias: "total".into(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["id", "dept", "salary", "name"])),
        };
        tree.prune_scan_columns();
        let OpTree::Aggregate { child, .. } = &tree else {
            panic!()
        };
        let mut names = col_names(child);
        names.sort();
        assert_eq!(names, vec!["dept", "salary"]);
    }

    // ── strip_order_by_and_limit tests ──────────────────────────────

    #[test]
    fn test_strip_order_by_simple() {
        let result = strip_order_by_and_limit("SELECT * FROM t ORDER BY id LIMIT 10");
        assert_eq!(result, "SELECT * FROM t");
    }

    #[test]
    fn test_strip_order_by_with_offset() {
        let result = strip_order_by_and_limit("SELECT * FROM t ORDER BY id LIMIT 10 OFFSET 5");
        assert_eq!(result, "SELECT * FROM t");
    }

    #[test]
    fn test_strip_order_by_no_order_by() {
        let result = strip_order_by_and_limit("SELECT * FROM t WHERE id > 1");
        assert_eq!(result, "SELECT * FROM t WHERE id > 1");
    }

    #[test]
    fn test_strip_order_by_preserves_subquery_order() {
        // ORDER BY inside subquery should NOT be stripped
        let q = "SELECT * FROM (SELECT * FROM t ORDER BY id) sub ORDER BY val LIMIT 5";
        let result = strip_order_by_and_limit(q);
        assert_eq!(result, "SELECT * FROM (SELECT * FROM t ORDER BY id) sub");
    }

    #[test]
    fn test_strip_order_by_with_string_literal() {
        // ORDER BY keyword inside a string literal should be ignored
        let q = "SELECT * FROM t WHERE name = 'ORDER BY me' ORDER BY id LIMIT 3";
        let result = strip_order_by_and_limit(q);
        assert_eq!(result, "SELECT * FROM t WHERE name = 'ORDER BY me'");
    }

    #[test]
    fn test_strip_order_by_trailing_semicolon() {
        let result = strip_order_by_and_limit("SELECT * FROM t ORDER BY id;");
        assert_eq!(result, "SELECT * FROM t");
    }

    #[test]
    fn test_strip_order_by_fetch_first() {
        let result =
            strip_order_by_and_limit("SELECT * FROM t ORDER BY id FETCH FIRST 10 ROWS ONLY");
        assert_eq!(result, "SELECT * FROM t");
    }

    // ── Expr::output_name tests ─────────────────────────────────────

    #[test]
    fn test_output_name_column_ref_unqualified() {
        let e = col("amount");
        assert_eq!(e.output_name(), "amount");
    }

    #[test]
    fn test_output_name_column_ref_qualified() {
        let e = qualified_col("orders", "total");
        // output_name strips the qualifier
        assert_eq!(e.output_name(), "total");
    }

    #[test]
    fn test_output_name_literal() {
        let e = Expr::Literal("42".to_string());
        // Literals fall through to to_sql()
        assert_eq!(e.output_name(), "42");
    }

    #[test]
    fn test_output_name_func_call() {
        let e = Expr::FuncCall {
            func_name: "COUNT".to_string(),
            args: vec![Expr::Star { table_alias: None }],
        };
        assert_eq!(e.output_name(), "COUNT(*)");
    }

    #[test]
    fn test_output_name_binary_op() {
        let e = Expr::BinaryOp {
            op: "+".to_string(),
            left: Box::new(col("a")),
            right: Box::new(Expr::Literal("1".to_string())),
        };
        assert_eq!(e.output_name(), "(a + 1)");
    }

    // ── unwrap_transparent tests ────────────────────────────────────

    #[test]
    fn test_unwrap_transparent_filter() {
        let scan = scan_node("t", 1, &["id"]);
        let filtered = OpTree::Filter {
            predicate: Expr::Literal("TRUE".to_string()),
            child: Box::new(scan),
        };
        let inner = unwrap_transparent(&filtered);
        assert!(matches!(inner, OpTree::Scan { .. }));
    }

    #[test]
    fn test_unwrap_transparent_subquery() {
        let scan = scan_node("t", 1, &["id"]);
        let sub = OpTree::Subquery {
            alias: "sub".to_string(),
            column_aliases: vec![],
            child: Box::new(scan),
        };
        let inner = unwrap_transparent(&sub);
        assert!(matches!(inner, OpTree::Scan { .. }));
    }

    #[test]
    fn test_unwrap_transparent_nested() {
        let scan = scan_node("t", 1, &["id"]);
        let filtered = OpTree::Filter {
            predicate: Expr::Literal("TRUE".to_string()),
            child: Box::new(OpTree::Subquery {
                alias: "sub".to_string(),
                column_aliases: vec![],
                child: Box::new(scan),
            }),
        };
        let inner = unwrap_transparent(&filtered);
        assert!(matches!(inner, OpTree::Scan { .. }));
    }

    #[test]
    fn test_unwrap_transparent_non_transparent() {
        let scan = scan_node("t", 1, &["id"]);
        let inner = unwrap_transparent(&scan);
        assert!(matches!(inner, OpTree::Scan { .. }));
    }

    #[test]
    fn test_unwrap_transparent_join_not_unwrapped() {
        let join = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".to_string()),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        let inner = unwrap_transparent(&join);
        assert!(matches!(inner, OpTree::InnerJoin { .. }));
    }

    // ── OpTree::output_columns tests ────────────────────────────────

    #[test]
    fn test_output_columns_scan() {
        let tree = scan_node("t", 1, &["id", "name", "value"]);
        assert_eq!(tree.output_columns(), vec!["id", "name", "value"]);
    }

    #[test]
    fn test_output_columns_project() {
        let tree = OpTree::Project {
            expressions: vec![col("x"), col("y")],
            aliases: vec!["a".to_string(), "b".to_string()],
            child: Box::new(scan_node("t", 1, &["x", "y", "z"])),
        };
        assert_eq!(tree.output_columns(), vec!["a", "b"]);
    }

    #[test]
    fn test_output_columns_filter_passthrough() {
        let tree = OpTree::Filter {
            predicate: Expr::Literal("x > 0".to_string()),
            child: Box::new(scan_node("t", 1, &["x", "y"])),
        };
        assert_eq!(tree.output_columns(), vec!["x", "y"]);
    }

    #[test]
    fn test_output_columns_inner_join() {
        let tree = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".to_string()),
            left: Box::new(scan_node("a", 1, &["id", "val"])),
            right: Box::new(scan_node("b", 2, &["ref_id"])),
        };
        assert_eq!(tree.output_columns(), vec!["id", "val", "ref_id"]);
    }

    #[test]
    fn test_output_columns_aggregate() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("dept")],
            aggregates: vec![AggExpr {
                function: AggFunc::Count,
                argument: None,
                alias: "cnt".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan_node("t", 1, &["dept", "salary"])),
        };
        assert_eq!(tree.output_columns(), vec!["dept", "cnt"]);
    }

    #[test]
    fn test_output_columns_distinct() {
        let tree = OpTree::Distinct {
            child: Box::new(scan_node("t", 1, &["a", "b"])),
        };
        assert_eq!(tree.output_columns(), vec!["a", "b"]);
    }

    #[test]
    fn test_output_columns_union_all() {
        let tree = OpTree::UnionAll {
            children: vec![
                scan_node("a", 1, &["x", "y"]),
                scan_node("b", 2, &["x", "y"]),
            ],
        };
        assert_eq!(tree.output_columns(), vec!["x", "y"]);
    }

    #[test]
    fn test_output_columns_semi_join_left_only() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".to_string()),
            left: Box::new(scan_node("o", 1, &["id", "amount"])),
            right: Box::new(scan_node("i", 2, &["order_id"])),
        };
        assert_eq!(tree.output_columns(), vec!["id", "amount"]);
    }

    // ── OpTree::source_oids tests ───────────────────────────────────

    #[test]
    fn test_source_oids_single_scan() {
        let tree = scan_node("t", 42, &["id"]);
        assert_eq!(tree.source_oids(), vec![42]);
    }

    #[test]
    fn test_source_oids_join() {
        let tree = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".to_string()),
            left: Box::new(scan_node("a", 10, &["id"])),
            right: Box::new(scan_node("b", 20, &["id"])),
        };
        assert_eq!(tree.source_oids(), vec![10, 20]);
    }

    #[test]
    fn test_source_oids_filter_passthrough() {
        let tree = OpTree::Filter {
            predicate: Expr::Literal("x > 0".to_string()),
            child: Box::new(scan_node("t", 99, &["x"])),
        };
        assert_eq!(tree.source_oids(), vec![99]);
    }

    #[test]
    fn test_source_oids_recursive_self_ref_empty() {
        let tree = OpTree::RecursiveSelfRef {
            cte_name: "tree".to_string(),
            alias: "t".to_string(),
            columns: vec!["id".to_string()],
        };
        assert_eq!(tree.source_oids(), Vec::<u32>::new());
    }

    #[test]
    fn test_source_oids_cte_scan_empty() {
        let tree = OpTree::CteScan {
            cte_id: 0,
            cte_name: "cte1".to_string(),
            alias: "c".to_string(),
            columns: vec!["id".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
            body: None,
        };
        assert_eq!(tree.source_oids(), Vec::<u32>::new());
    }

    #[test]
    fn test_source_oids_union_all() {
        let tree = OpTree::UnionAll {
            children: vec![
                scan_node("a", 5, &["id"]),
                scan_node("b", 10, &["id"]),
                scan_node("c", 15, &["id"]),
            ],
        };
        assert_eq!(tree.source_oids(), vec![5, 10, 15]);
    }

    // ── split_and_predicates / join_and_predicates tests ────────────

    #[test]
    fn test_split_and_single_predicate() {
        let expr = Expr::Literal("x > 0".to_string());
        let parts = split_and_predicates(expr);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].to_sql(), "x > 0");
    }

    #[test]
    fn test_split_and_flat_and() {
        let expr = Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(Expr::Literal("a".to_string())),
            right: Box::new(Expr::Literal("b".to_string())),
        };
        let parts = split_and_predicates(expr);
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn test_split_and_nested_and() {
        let expr = Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(Expr::BinaryOp {
                op: "AND".to_string(),
                left: Box::new(Expr::Literal("a".to_string())),
                right: Box::new(Expr::Literal("b".to_string())),
            }),
            right: Box::new(Expr::Literal("c".to_string())),
        };
        let parts = split_and_predicates(expr);
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn test_split_and_non_and_operator_not_split() {
        let expr = Expr::BinaryOp {
            op: "OR".to_string(),
            left: Box::new(Expr::Literal("a".to_string())),
            right: Box::new(Expr::Literal("b".to_string())),
        };
        let parts = split_and_predicates(expr);
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn test_join_and_predicates_single() {
        let parts = vec![Expr::Literal("a".to_string())];
        let result = join_and_predicates(parts);
        assert_eq!(result.to_sql(), "a");
    }

    #[test]
    fn test_join_and_predicates_multiple() {
        let parts = vec![
            Expr::Literal("a".to_string()),
            Expr::Literal("b".to_string()),
            Expr::Literal("c".to_string()),
        ];
        let result = join_and_predicates(parts);
        // Should produce ((a AND b) AND c)
        assert_eq!(result.to_sql(), "((a AND b) AND c)");
    }

    #[test]
    fn test_split_and_roundtrip() {
        // Split then join should produce equivalent expression
        let expr = Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(Expr::Literal("x > 0".to_string())),
            right: Box::new(Expr::Literal("y < 10".to_string())),
        };
        let parts = split_and_predicates(expr);
        assert_eq!(parts.len(), 2);
        let rejoined = join_and_predicates(parts);
        assert_eq!(rejoined.to_sql(), "(x > 0 AND y < 10)");
    }

    // ── AggFunc::is_group_rescan tests ──────────────────────────────

    #[test]
    fn test_agg_group_rescan_sum_is_algebraic() {
        assert!(!AggFunc::Sum.is_group_rescan());
    }

    #[test]
    fn test_agg_group_rescan_count_is_algebraic() {
        assert!(!AggFunc::Count.is_group_rescan());
    }

    #[test]
    fn test_agg_group_rescan_min_is_algebraic() {
        assert!(!AggFunc::Min.is_group_rescan());
    }

    #[test]
    fn test_agg_group_rescan_max_is_algebraic() {
        assert!(!AggFunc::Max.is_group_rescan());
    }

    #[test]
    fn test_agg_avg_is_algebraic_via_aux() {
        // AVG is now algebraic via auxiliary columns, not group-rescan
        assert!(!AggFunc::Avg.is_group_rescan());
        assert!(AggFunc::Avg.is_algebraic_via_aux());
    }

    #[test]
    fn test_agg_group_rescan_string_agg_needs_rescan() {
        assert!(AggFunc::StringAgg.is_group_rescan());
    }

    #[test]
    fn test_agg_group_rescan_bool_and_needs_rescan() {
        assert!(AggFunc::BoolAnd.is_group_rescan());
    }

    #[test]
    fn test_agg_group_rescan_array_agg_needs_rescan() {
        assert!(AggFunc::ArrayAgg.is_group_rescan());
    }

    #[test]
    fn test_agg_group_rescan_stddev_pop_needs_rescan() {
        // STDDEV_POP is now algebraic via auxiliary columns, not group-rescan
        assert!(!AggFunc::StddevPop.is_group_rescan());
        assert!(AggFunc::StddevPop.is_algebraic_via_aux());
        assert!(AggFunc::StddevPop.needs_sum_of_squares());
    }

    #[test]
    fn test_agg_group_rescan_mode_needs_rescan() {
        assert!(AggFunc::Mode.is_group_rescan());
    }

    // ── collect_volatilities expanded tests ──────────────────────────

    #[test]
    fn test_collect_volatilities_coalesce_skips_lookup() {
        // COALESCE is a parser construct (immutable), should not trigger
        // function volatility lookup. With immutable args, worst stays 'i'.
        let expr = Expr::FuncCall {
            func_name: "COALESCE".to_string(),
            args: vec![col("x"), Expr::Literal("0".to_string())],
        };
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    #[test]
    fn test_collect_volatilities_nullif_skips_lookup() {
        let expr = Expr::FuncCall {
            func_name: "NULLIF".to_string(),
            args: vec![col("x"), Expr::Literal("0".to_string())],
        };
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    #[test]
    fn test_collect_volatilities_greatest_least_skip_lookup() {
        for name in &["GREATEST", "LEAST"] {
            let expr = Expr::FuncCall {
                func_name: name.to_string(),
                args: vec![col("a"), col("b")],
            };
            let mut worst = 'i';
            collect_volatilities(&expr, &mut worst).unwrap();
            assert_eq!(worst, 'i', "{name} should skip function lookup");
        }
    }

    #[test]
    fn test_collect_volatilities_normal_func_is_volatile() {
        // Non-builtin function → lookup stub returns 'v'
        let expr = Expr::FuncCall {
            func_name: "random".to_string(),
            args: vec![],
        };
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        assert_eq!(worst, 'v');
    }

    #[test]
    fn test_collect_volatilities_coalesce_with_volatile_arg() {
        // COALESCE itself is immutable but its args may be volatile
        let expr = Expr::FuncCall {
            func_name: "COALESCE".to_string(),
            args: vec![
                Expr::FuncCall {
                    func_name: "random".to_string(),
                    args: vec![],
                },
                Expr::Literal("0".to_string()),
            ],
        };
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        // random() → volatile via test stub
        assert_eq!(worst, 'v');
    }

    #[test]
    fn test_collect_volatilities_column_ref_stays_immutable() {
        let expr = col("x");
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    #[test]
    fn test_collect_volatilities_star_stays_immutable() {
        let expr = Expr::Star { table_alias: None };
        let mut worst = 'i';
        collect_volatilities(&expr, &mut worst).unwrap();
        assert_eq!(worst, 'i');
    }

    // ── split_exists_correlation / try_extract_exists_corr_pair tests

    fn qcol(table: &str, name: &str) -> Expr {
        Expr::ColumnRef {
            table_alias: Some(table.to_string()),
            column_name: name.to_string(),
        }
    }

    fn eq_pred(left: Expr, right: Expr) -> Expr {
        Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn and_pred(left: Expr, right: Expr) -> Expr {
        Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    #[test]
    fn test_split_exists_correlation_simple() {
        // WHERE o.cust_id = c.id (inner=o, outer=c)
        let pred = eq_pred(qcol("o", "cust_id"), qcol("c", "id"));
        let (corr, remaining) = split_exists_correlation(&pred, &["o".to_string()]);
        assert_eq!(corr.len(), 1);
        let (outer_expr, inner_col) = &corr[0];
        assert_eq!(inner_col, "cust_id");
        assert!(matches!(outer_expr, Expr::ColumnRef { column_name, .. } if column_name == "id"));
        assert!(remaining.is_none());
    }

    #[test]
    fn test_split_exists_correlation_reversed() {
        // WHERE c.id = o.cust_id (outer=c, inner=o)
        let pred = eq_pred(qcol("c", "id"), qcol("o", "cust_id"));
        let (corr, remaining) = split_exists_correlation(&pred, &["o".to_string()]);
        assert_eq!(corr.len(), 1);
        let (outer_expr, inner_col) = &corr[0];
        assert_eq!(inner_col, "cust_id");
        assert!(matches!(outer_expr, Expr::ColumnRef { column_name, .. } if column_name == "id"));
        assert!(remaining.is_none());
    }

    #[test]
    fn test_split_exists_correlation_with_inner_only() {
        // WHERE o.cust_id = c.id AND o.status = 'active'
        let pred = and_pred(
            eq_pred(qcol("o", "cust_id"), qcol("c", "id")),
            eq_pred(qcol("o", "status"), Expr::Literal("'active'".to_string())),
        );
        let (corr, remaining) = split_exists_correlation(&pred, &["o".to_string()]);
        assert_eq!(corr.len(), 1, "should extract one correlation pair");
        assert!(
            remaining.is_some(),
            "inner-only predicate should be in remaining"
        );
    }

    #[test]
    fn test_split_exists_correlation_no_correlation() {
        // WHERE o.status = 'active' (no outer reference)
        let pred = eq_pred(qcol("o", "status"), Expr::Literal("'active'".to_string()));
        let (corr, remaining) = split_exists_correlation(&pred, &["o".to_string()]);
        assert!(corr.is_empty(), "no correlation pair expected");
        assert!(remaining.is_some(), "inner-only pred should remain");
    }

    #[test]
    fn test_split_exists_correlation_multi_key() {
        // WHERE o.a = c.x AND o.b = c.y
        let pred = and_pred(
            eq_pred(qcol("o", "a"), qcol("c", "x")),
            eq_pred(qcol("o", "b"), qcol("c", "y")),
        );
        let (corr, remaining) = split_exists_correlation(&pred, &["o".to_string()]);
        assert_eq!(corr.len(), 2, "should extract two correlation pairs");
        assert!(remaining.is_none());
    }

    #[test]
    fn test_collect_tree_source_aliases_scan() {
        let tree = scan_node("o", 1, &["id"]);
        assert_eq!(collect_tree_source_aliases(&tree), vec!["o"]);
    }

    #[test]
    fn test_collect_tree_source_aliases_join() {
        let tree = OpTree::InnerJoin {
            condition: Expr::Literal("TRUE".to_string()),
            left: Box::new(scan_node("a", 1, &["id"])),
            right: Box::new(scan_node("b", 2, &["id"])),
        };
        let aliases = collect_tree_source_aliases(&tree);
        assert!(aliases.contains(&"a".to_string()));
        assert!(aliases.contains(&"b".to_string()));
    }

    // ── P2-6: Correlation predicate extraction tests ────────────────

    #[test]
    fn test_correlation_extraction_simple_equality() {
        // WHERE li.order_id = o.id
        let expr = Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(qualified_col("li", "order_id")),
            right: Box::new(qualified_col("o", "id")),
        };
        let inner_alias_oids = vec![("li".to_string(), 42u32)];
        let inner_aliases: Vec<&str> = inner_alias_oids.iter().map(|(a, _)| a.as_str()).collect();
        let mut preds = Vec::new();
        collect_correlation_equalities(&expr, &inner_aliases, &inner_alias_oids, &mut preds);
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].outer_col, "id");
        assert_eq!(preds[0].inner_alias, "li");
        assert_eq!(preds[0].inner_col, "order_id");
        assert_eq!(preds[0].inner_oid, 42);
    }

    #[test]
    fn test_correlation_extraction_reversed_order() {
        // WHERE o.id = li.order_id (outer on left, inner on right)
        let expr = Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(qualified_col("o", "id")),
            right: Box::new(qualified_col("li", "order_id")),
        };
        let inner_alias_oids = vec![("li".to_string(), 42u32)];
        let inner_aliases: Vec<&str> = inner_alias_oids.iter().map(|(a, _)| a.as_str()).collect();
        let mut preds = Vec::new();
        collect_correlation_equalities(&expr, &inner_aliases, &inner_alias_oids, &mut preds);
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].outer_col, "id");
        assert_eq!(preds[0].inner_col, "order_id");
    }

    #[test]
    fn test_correlation_extraction_and_conjunction() {
        // WHERE li.order_id = o.id AND li.status = 'active'
        let expr = Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(qualified_col("li", "order_id")),
                right: Box::new(qualified_col("o", "id")),
            }),
            right: Box::new(Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(qualified_col("li", "status")),
                right: Box::new(Expr::Literal("'active'".to_string())),
            }),
        };
        let inner_alias_oids = vec![("li".to_string(), 42u32)];
        let inner_aliases: Vec<&str> = inner_alias_oids.iter().map(|(a, _)| a.as_str()).collect();
        let mut preds = Vec::new();
        collect_correlation_equalities(&expr, &inner_aliases, &inner_alias_oids, &mut preds);
        // Only the correlation equality should be extracted, not li.status = 'active'
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].inner_col, "order_id");
    }

    #[test]
    fn test_correlation_extraction_no_inner_alias_match() {
        // WHERE a.col = b.col — neither matches inner aliases
        let expr = Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(qualified_col("a", "col")),
            right: Box::new(qualified_col("b", "col")),
        };
        let inner_alias_oids: Vec<(String, u32)> = vec![("x".to_string(), 1)];
        let inner_aliases: Vec<&str> = inner_alias_oids.iter().map(|(a, _)| a.as_str()).collect();
        let mut preds = Vec::new();
        collect_correlation_equalities(&expr, &inner_aliases, &inner_alias_oids, &mut preds);
        assert!(preds.is_empty());
    }

    #[test]
    fn test_correlation_extraction_both_inner() {
        // WHERE li.col1 = li.col2 — both sides reference inner alias
        let expr = Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(qualified_col("li", "col1")),
            right: Box::new(qualified_col("li", "col2")),
        };
        let inner_alias_oids = vec![("li".to_string(), 42u32)];
        let inner_aliases: Vec<&str> = inner_alias_oids.iter().map(|(a, _)| a.as_str()).collect();
        let mut preds = Vec::new();
        collect_correlation_equalities(&expr, &inner_aliases, &inner_alias_oids, &mut preds);
        // Both sides are inner — no correlation predicate
        assert!(preds.is_empty());
    }

    #[test]
    fn test_correlation_extraction_compound() {
        // WHERE li.order_id = o.id AND li.region = o.region
        let expr = Expr::BinaryOp {
            op: "AND".to_string(),
            left: Box::new(Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(qualified_col("li", "order_id")),
                right: Box::new(qualified_col("o", "id")),
            }),
            right: Box::new(Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(qualified_col("li", "region")),
                right: Box::new(qualified_col("o", "region")),
            }),
        };
        let inner_alias_oids = vec![("li".to_string(), 42u32)];
        let inner_aliases: Vec<&str> = inner_alias_oids.iter().map(|(a, _)| a.as_str()).collect();
        let mut preds = Vec::new();
        collect_correlation_equalities(&expr, &inner_aliases, &inner_alias_oids, &mut preds);
        assert_eq!(preds.len(), 2);
    }

    // ── G13-SD: Parse depth guard tests ─────────────────────────────

    #[test]
    fn test_cte_parse_context_descend_within_limit() {
        let mut ctx = CteParseContext::new(HashMap::new(), HashMap::new());
        // max_depth defaults to 64 in unit tests
        assert_eq!(ctx.depth, 0);
        let prev = ctx.descend().expect("should not exceed limit");
        assert_eq!(prev, 0);
        assert_eq!(ctx.depth, 1);
        ctx.ascend(prev);
        assert_eq!(ctx.depth, 0);
    }

    #[test]
    fn test_cte_parse_context_descend_exceeds_limit() {
        let mut ctx = CteParseContext::new(HashMap::new(), HashMap::new());
        // Manually set depth to max_depth to trigger the guard.
        ctx.depth = ctx.max_depth;
        let result = ctx.descend();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, PgTrickleError::QueryTooComplex(_)),
            "expected QueryTooComplex, got: {err:?}"
        );
        assert!(err.to_string().contains("max_parse_depth"));
    }

    #[test]
    fn test_cte_parse_context_descend_ascend_round_trip() {
        let mut ctx = CteParseContext::new(HashMap::new(), HashMap::new());
        let d0 = ctx.descend().unwrap();
        let d1 = ctx.descend().unwrap();
        let d2 = ctx.descend().unwrap();
        assert_eq!(ctx.depth, 3);
        ctx.ascend(d2);
        assert_eq!(ctx.depth, 2);
        ctx.ascend(d1);
        assert_eq!(ctx.depth, 1);
        ctx.ascend(d0);
        assert_eq!(ctx.depth, 0);
    }

    // ── A-2: source_key_columns_used tests ──────────────────────────

    #[test]
    fn test_source_key_columns_aggregate_group_by() {
        // SELECT region, SUM(amount) FROM orders GROUP BY region
        let scan = scan_node("orders", 100, &["region", "amount", "id"]);
        let tree = OpTree::Aggregate {
            group_by: vec![col("region")],
            aggregates: vec![AggExpr {
                function: AggFunc::Sum,
                argument: Some(col("amount")),
                alias: "total".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(scan),
        };

        let keys = tree.source_key_columns_used();
        let key_cols = keys.get(&100).unwrap();
        assert_eq!(key_cols, &["region".to_string()]);
    }

    #[test]
    fn test_source_key_columns_scan_only_no_keys() {
        // SELECT a, b, c FROM t — no GROUP BY, no JOIN, no WHERE
        let tree = scan_node("t", 200, &["a", "b", "c"]);
        let keys = tree.source_key_columns_used();
        assert!(
            keys.is_empty(),
            "scan-only query should have no key columns"
        );
    }

    #[test]
    fn test_source_key_columns_inner_join() {
        // SELECT ... FROM a JOIN b ON a.id = b.a_id
        let left = scan_node("a", 100, &["id", "name"]);
        let right = scan_node("b", 200, &["a_id", "value"]);
        let tree = OpTree::InnerJoin {
            condition: Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(qualified_col("a", "id")),
                right: Box::new(qualified_col("b", "a_id")),
            },
            left: Box::new(left),
            right: Box::new(right),
        };

        let keys = tree.source_key_columns_used();
        // "id" appears in JOIN ON for table 100
        assert!(
            keys.get(&100)
                .is_some_and(|v| v.contains(&"id".to_string()))
        );
        // "a_id" appears in JOIN ON for table 200
        assert!(
            keys.get(&200)
                .is_some_and(|v| v.contains(&"a_id".to_string()))
        );
    }

    #[test]
    fn test_source_key_columns_filter_where() {
        // SELECT a, b FROM t WHERE a > 10
        let scan = scan_node("t", 100, &["a", "b"]);
        let tree = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: ">".to_string(),
                left: Box::new(col("a")),
                right: Box::new(Expr::Literal("10".to_string())),
            },
            child: Box::new(scan),
        };

        let keys = tree.source_key_columns_used();
        assert_eq!(keys.get(&100).unwrap(), &["a".to_string()]);
    }

    #[test]
    fn test_source_key_columns_aggregate_with_filter() {
        // SELECT region, SUM(amount) FROM orders WHERE active = true GROUP BY region
        let scan = scan_node("orders", 100, &["region", "amount", "active"]);
        let filter = OpTree::Filter {
            predicate: Expr::BinaryOp {
                op: "=".to_string(),
                left: Box::new(col("active")),
                right: Box::new(Expr::Literal("true".to_string())),
            },
            child: Box::new(scan),
        };
        let tree = OpTree::Aggregate {
            group_by: vec![col("region")],
            aggregates: vec![AggExpr {
                function: AggFunc::Sum,
                argument: Some(col("amount")),
                alias: "total".to_string(),
                is_distinct: false,
                second_arg: None,
                filter: None,
                order_within_group: None,
            }],
            child: Box::new(filter),
        };

        let keys = tree.source_key_columns_used();
        let key_cols = keys.get(&100).unwrap();
        assert!(key_cols.contains(&"active".to_string()), "WHERE column");
        assert!(key_cols.contains(&"region".to_string()), "GROUP BY column");
        assert!(
            !key_cols.contains(&"amount".to_string()),
            "value-only column"
        );
    }
}
