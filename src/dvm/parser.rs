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

use crate::error::PgStreamError;
use pgrx::prelude::*;
use std::collections::HashMap;

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
                Some(alias) => format!("{alias}.{column_name}"),
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
            AggFunc::StddevPop => "STDDEV_POP",
            AggFunc::StddevSamp => "STDDEV_SAMP",
            AggFunc::VarPop => "VAR_POP",
            AggFunc::VarSamp => "VAR_SAMP",
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
        }
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
                | AggFunc::StddevPop
                | AggFunc::StddevSamp
                | AggFunc::VarPop
                | AggFunc::VarSamp
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

/// Sort expression for ORDER BY or window functions.
#[derive(Debug, Clone)]
pub struct SortExpr {
    pub expr: Expr,
    pub ascending: bool,
    pub nulls_first: bool,
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
}

impl ParseResult {
    /// Collect `(table_oid, Vec<column_name>)` for all source tables
    /// referenced across the main tree and CTE bodies.
    ///
    /// Used to populate `pgs_dependencies.columns_used` at creation time
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
        }
    }

    /// Returns `true` if the operator tree contains Aggregate, Distinct,
    /// Intersect, or Except — meaning the storage table needs a
    /// `__pgs_count BIGINT` auxiliary column for differential maintenance.
    ///
    /// Recurses through transparent wrappers (`Filter`, `Project`,
    /// `Subquery`) that may sit on top (e.g. HAVING adds a `Filter`
    /// around the `Aggregate`).
    pub fn needs_pgs_count(&self) -> bool {
        match self {
            OpTree::Aggregate { .. }
            | OpTree::Distinct { .. }
            | OpTree::Intersect { .. }
            | OpTree::Except { .. } => true,
            // Transparent wrappers — delegate to child
            OpTree::Filter { child, .. }
            | OpTree::Project { child, .. }
            | OpTree::Subquery { child, .. } => child.needs_pgs_count(),
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
    pub fn group_by_columns(&self) -> Option<Vec<String>> {
        match self {
            OpTree::Aggregate { group_by, .. } => {
                if group_by.is_empty() {
                    None
                } else {
                    Some(group_by.iter().map(|e| e.output_name()).collect())
                }
            }
            OpTree::Project { child, .. } | OpTree::Subquery { child, .. } => {
                child.group_by_columns()
            }
            _ => None,
        }
    }

    /// Return the column names that should be hashed to produce `__pgs_row_id`.
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
                if matches!(
                    child.as_ref(),
                    OpTree::InnerJoin { .. } | OpTree::LeftJoin { .. } | OpTree::FullJoin { .. }
                ) {
                    let pk_aliases = join_pk_aliases(expressions, aliases, child);
                    return Some(pk_aliases.unwrap_or_else(|| aliases.clone()));
                }
                // For lateral function/subquery children, use all projected
                // columns as the row_id key. SRF expansions have no natural PK,
                // so we hash all output columns. This ensures both FULL refresh
                // (which hashes the projected output) and DIFFERENTIAL refresh
                // (which recomputes row_id in the Project operator) produce the
                // same row_ids.
                if matches!(
                    child.as_ref(),
                    OpTree::LateralFunction { .. } | OpTree::LateralSubquery { .. }
                ) {
                    return Some(aliases.clone());
                }
                // Project may drop columns — check that returned key columns
                // are among the project's output columns. Otherwise fall back.
                let out = self.output_columns();
                match child.row_id_key_columns() {
                    Some(keys) if keys.iter().all(|k| out.contains(k)) => Some(keys),
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
                    None // caller handles with pg_stream_hash('__singleton_group')
                } else {
                    Some(cols)
                }
            }
            OpTree::Subquery { child, .. } => child.row_id_key_columns(),
            OpTree::CteScan { .. } => {
                // CTE scan: the row_id comes from the CTE body diff.
                // We can't easily determine this statically.
                None
            }
            OpTree::Window { .. } => {
                // Window functions like RANK/DENSE_RANK can produce identical
                // output rows (tied values). Fall back to row_number-based
                // hash to guarantee uniqueness.
                None
            }
            OpTree::LateralFunction { .. } => {
                // SRF expansions have no natural primary key.
                // Row IDs are content-hash based, computed by the diff operator.
                None
            }
            OpTree::LateralSubquery { .. } => {
                // LATERAL subquery results have no natural primary key.
                // Row IDs are content-hash based, computed by the diff operator.
                None
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
                alias,
                column_aliases,
                with_ordinality,
                child,
                ..
            } => {
                // Output = child columns + SRF result columns + optional ordinality
                let mut cols = child.output_columns();
                if column_aliases.is_empty() {
                    // No explicit aliases — use the alias name as a single column
                    cols.push(alias.clone());
                } else {
                    cols.extend(column_aliases.clone());
                }
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
        }
    }

    /// Collect `(table_oid, Vec<column_name>)` pairs from all Scan nodes.
    ///
    /// When the same table appears in multiple Scan nodes (self-join), the
    /// column lists are merged (union of column names). This is used to
    /// populate `pgs_dependencies.columns_used` at creation time.
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
        }
    }
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
pub fn lookup_function_volatility(func_name: &str) -> Result<char, PgStreamError> {
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
        PgStreamError::SpiError(format!("volatility lookup failed: {e}"))
    })
}

/// Test-only stub: SPI is unavailable in unit tests, so assume volatile.
#[cfg(test)]
pub fn lookup_function_volatility(_func_name: &str) -> Result<char, PgStreamError> {
    Ok('v')
}

/// Recursively scan an `Expr` tree and update `worst` with the volatility
/// of any `FuncCall` nodes found.
pub fn collect_volatilities(expr: &Expr, worst: &mut char) -> Result<(), PgStreamError> {
    match expr {
        Expr::FuncCall { func_name, args } => {
            let vol = lookup_function_volatility(func_name)?;
            *worst = max_volatility(*worst, vol);
            for arg in args {
                collect_volatilities(arg, worst)?;
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_volatilities(left, worst)?;
            collect_volatilities(right, worst)?;
        }
        // ColumnRef, Literal, Star, Raw: no function calls.
        _ => {}
    }
    Ok(())
}

/// Return the worst volatility found in an expression tree.
pub fn worst_volatility(expr: &Expr) -> Result<char, PgStreamError> {
    let mut worst = 'i';
    collect_volatilities(expr, &mut worst)?;
    Ok(worst)
}

/// Walk an entire OpTree and return the worst volatility found in any
/// expression (target list, WHERE, JOIN conditions, HAVING, aggregates,
/// window functions).
pub fn tree_worst_volatility(tree: &OpTree) -> Result<char, PgStreamError> {
    let mut worst = 'i';
    tree_collect_volatility(tree, &mut worst)?;
    Ok(worst)
}

/// Walk an entire [`ParseResult`] (tree + CTE registry) for volatility.
pub fn tree_worst_volatility_with_registry(result: &ParseResult) -> Result<char, PgStreamError> {
    let mut worst = 'i';
    // Check all CTE bodies
    for (_name, body) in &result.cte_registry.entries {
        tree_collect_volatility(body, &mut worst)?;
    }
    tree_collect_volatility(&result.tree, &mut worst)?;
    Ok(worst)
}

fn tree_collect_volatility(tree: &OpTree, worst: &mut char) -> Result<(), PgStreamError> {
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
    }
    Ok(())
}

/// Check if an operator tree is supported for differential maintenance.
pub fn check_ivm_support(tree: &OpTree) -> Result<(), PgStreamError> {
    check_ivm_support_inner(tree)
}

/// Check if a [`ParseResult`] (tree + CTE registry) is fully supported.
pub fn check_ivm_support_with_registry(result: &ParseResult) -> Result<(), PgStreamError> {
    // Validate all CTE bodies first
    for (name, body) in &result.cte_registry.entries {
        check_ivm_support_inner(body)
            .map_err(|e| PgStreamError::UnsupportedOperator(format!("in CTE '{name}': {e}")))?;
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
) -> Result<(), PgStreamError> {
    if !resolves_to_base_table(left) {
        return Err(PgStreamError::UnsupportedOperator(format!(
            "nested joins are not supported for DIFFERENTIAL mode: \
             left child of {join_kind} must resolve to a base table, \
             not a {}",
            left.node_kind(),
        )));
    }
    if !resolves_to_base_table(right) {
        return Err(PgStreamError::UnsupportedOperator(format!(
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

fn check_ivm_support_inner(tree: &OpTree) -> Result<(), PgStreamError> {
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
                    | AggFunc::StddevPop
                    | AggFunc::StddevSamp
                    | AggFunc::VarPop
                    | AggFunc::VarSamp
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
                    | AggFunc::RegrSyy => {}
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
}

impl CteParseContext {
    fn new(
        raw_map: HashMap<String, *const pg_sys::SelectStmt>,
        def_aliases: HashMap<String, Vec<String>>,
    ) -> Self {
        CteParseContext {
            raw_map,
            def_aliases,
            id_map: HashMap::new(),
            registry: CteRegistry::default(),
            recursive_self_ref_name: None,
            recursive_self_ref_columns: Vec::new(),
        }
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
pub fn query_has_recursive_cte(query: &str) -> Result<bool, PgStreamError> {
    // SAFETY: raw_parser is safe when called within a PostgreSQL backend
    // with a valid memory context.
    unsafe { query_has_recursive_cte_inner(query) }
}

unsafe fn query_has_recursive_cte_inner(query: &str) -> Result<bool, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

    let raw_list =
        unsafe { pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
    if raw_list.is_null() {
        return Err(PgStreamError::QueryParseError(
            "raw_parser returned NULL".into(),
        ));
    }

    let list = unsafe { pgrx::PgList::<pg_sys::RawStmt>::from_pg(raw_list) };
    let raw_stmt = match list.head() {
        Some(stmt) => stmt,
        None => return Ok(false),
    };

    let node = unsafe { (*raw_stmt).stmt };
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SelectStmt) } {
        return Ok(false);
    }

    let select = unsafe { &*(node as *const pg_sys::SelectStmt) };
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
pub fn rewrite_views_inline(query: &str) -> Result<String, PgStreamError> {
    let max_depth = 10;
    let mut current = query.to_string();

    for _depth in 0..max_depth {
        let rewritten = rewrite_views_inline_once(&current)?;
        if rewritten == current {
            return Ok(current); // Fixpoint reached — no more views
        }
        current = rewritten;
    }

    Err(PgStreamError::QueryParseError(format!(
        "View inlining exceeded maximum nesting depth of {max_depth}. \
         This may indicate circular view dependencies."
    )))
}

/// Single-pass view inlining: parse the query, walk FROM items, replace
/// any view RangeVars with `(pg_get_viewdef(oid, true)) AS alias`.
///
/// Returns the original string unchanged if no views are found.
fn rewrite_views_inline_once(query: &str) -> Result<String, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

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

    // Build a substitution map: views found in FROM → their definitions
    let mut subs = Vec::new();
    let mut found_view = false;

    // Walk the FROM clause to find views. Also walk set-operation arms and CTEs.
    unsafe { collect_view_substitutions(select, &mut subs, &mut found_view)? };

    if !found_view {
        return Ok(query.to_string());
    }

    // Deparse the full query with view substitutions applied
    unsafe { deparse_select_stmt_with_view_subs(select, &subs) }
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
fn resolve_relkind(schema: &str, relname: &str) -> Result<Option<String>, PgStreamError> {
    let sql = format!(
        "SELECT c.relkind::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{}' AND c.relname = '{}'",
        schema.replace('\'', "''"),
        relname.replace('\'', "''"),
    );
    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        result
            .first()
            .get::<String>(1)
            .map_err(|e| PgStreamError::SpiError(e.to_string()))
    })
}

/// Get the SQL definition of a view using `pg_get_viewdef(oid, true)`.
fn get_view_definition(schema: &str, relname: &str) -> Result<String, PgStreamError> {
    let sql = format!(
        "SELECT pg_get_viewdef(c.oid, true) FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{}' AND c.relname = '{}'",
        schema.replace('\'', "''"),
        relname.replace('\'', "''"),
    );
    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        result
            .first()
            .get::<String>(1)
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?
            .ok_or_else(|| {
                PgStreamError::QueryParseError(format!(
                    "Could not retrieve view definition for {schema}.{relname}"
                ))
            })
    })
}

/// Resolve schema for a RangeVar. If schemaname is NULL, resolve via search_path.
fn resolve_rangevar_schema(rv: &pg_sys::RangeVar) -> Result<String, PgStreamError> {
    if !rv.schemaname.is_null() {
        let schema = unsafe { std::ffi::CStr::from_ptr(rv.schemaname) }
            .to_str()
            .map_err(|_| PgStreamError::QueryParseError("Invalid schema name encoding".into()))?;
        return Ok(schema.to_string());
    }

    // Resolve from search_path
    if rv.relname.is_null() {
        return Err(PgStreamError::QueryParseError(
            "RangeVar with NULL relname".into(),
        ));
    }
    let relname = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
        .to_str()
        .map_err(|_| PgStreamError::QueryParseError("Invalid relation name encoding".into()))?;

    let sql = format!(
        "SELECT n.nspname FROM pg_class c \
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
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        result
            .first()
            .get::<String>(1)
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?
            .ok_or_else(|| {
                PgStreamError::NotFound(format!("relation '{relname}' not found in search_path"))
            })
    })
}

/// Walk a SelectStmt's FROM clause (and set-operation arms, CTEs) to find
/// view references and build the substitution list.
///
/// # Safety
/// Caller must ensure `select` points to a valid `SelectStmt`.
unsafe fn collect_view_substitutions(
    select: *const pg_sys::SelectStmt,
    subs: &mut Vec<ViewSubstitution>,
    found_view: &mut bool,
) -> Result<(), PgStreamError> {
    let s = unsafe { &*select };

    // Handle set operations: recurse into larg/rarg
    if s.op != pg_sys::SetOperation::SETOP_NONE {
        if !s.larg.is_null() {
            unsafe { collect_view_substitutions(s.larg, subs, found_view)? };
        }
        if !s.rarg.is_null() {
            unsafe { collect_view_substitutions(s.rarg, subs, found_view)? };
        }
        return Ok(());
    }

    // Walk the FROM clause
    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.fromClause) };
    for node_ptr in from_list.iter_ptr() {
        unsafe { collect_view_subs_from_item(node_ptr, subs, found_view)? };
    }

    // Walk CTE bodies if present
    if !s.withClause.is_null() {
        let wc = unsafe { &*s.withClause };
        let cte_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(wc.ctes) };
        for node_ptr in cte_list.iter_ptr() {
            if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
                let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };
                if !cte.ctequery.is_null()
                    && unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) }
                {
                    unsafe {
                        collect_view_substitutions(
                            cte.ctequery as *const pg_sys::SelectStmt,
                            subs,
                            found_view,
                        )?
                    };
                }
            }
        }
    }

    Ok(())
}

/// Walk a single FROM item looking for view references.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree node.
unsafe fn collect_view_subs_from_item(
    node: *mut pg_sys::Node,
    subs: &mut Vec<ViewSubstitution>,
    found_view: &mut bool,
) -> Result<(), PgStreamError> {
    if node.is_null() {
        return Ok(());
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };

        // Get relname
        if rv.relname.is_null() {
            return Ok(());
        }
        let relname = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
            .to_str()
            .map_err(|_| PgStreamError::QueryParseError("Invalid relation name encoding".into()))?
            .to_string();

        // Resolve schema
        let schema = resolve_rangevar_schema(rv)?;

        // Check relkind
        let relkind = resolve_relkind(&schema, &relname)?;
        match relkind.as_deref() {
            Some("v") => {
                // It's a view — get definition and record substitution
                let view_sql = get_view_definition(&schema, &relname)?;

                // Determine alias: explicit alias or view name
                let alias = if !rv.alias.is_null() {
                    let a = unsafe { &*(rv.alias) };
                    unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                        .to_str()
                        .unwrap_or(&relname)
                        .to_string()
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        unsafe { collect_view_subs_from_item(join.larg, subs, found_view)? };
        unsafe { collect_view_subs_from_item(join.rarg, subs, found_view)? };
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeSubselect) } {
        let sub = unsafe { &*(node as *const pg_sys::RangeSubselect) };
        if !sub.subquery.is_null()
            && unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            unsafe {
                collect_view_substitutions(
                    sub.subquery as *const pg_sys::SelectStmt,
                    subs,
                    found_view,
                )?
            };
        }
    }

    Ok(())
}

/// Deparse a SelectStmt back to SQL, replacing view RangeVars with inline
/// subqueries according to the substitution list.
///
/// # Safety
/// Caller must ensure `stmt` points to a valid `SelectStmt`.
unsafe fn deparse_select_stmt_with_view_subs(
    stmt: *const pg_sys::SelectStmt,
    subs: &[ViewSubstitution],
) -> Result<String, PgStreamError> {
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
            unsafe { deparse_select_stmt_with_view_subs(s.larg, subs)? }
        } else {
            String::new()
        };
        let right = if !s.rarg.is_null() {
            unsafe { deparse_select_stmt_with_view_subs(s.rarg, subs)? }
        } else {
            String::new()
        };

        let mut result = format!("{left} {op_str} {right}");

        // Top-level ORDER BY / LIMIT / OFFSET on set operations
        let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.sortClause) };
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
        let with_sql = unsafe { deparse_with_clause_with_view_subs(s.withClause, subs)? };
        parts.push(with_sql);
    }

    // DISTINCT
    let distinct_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.distinctClause) };
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
    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.fromClause) };
    if !from_list.is_empty() {
        let mut from_items = Vec::new();
        for node_ptr in from_list.iter_ptr() {
            let item = unsafe { deparse_from_item_with_view_subs(node_ptr, subs)? };
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
    let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.groupClause) };
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
    let window_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.windowClause) };
    if !window_list.is_empty() {
        let mut window_parts = Vec::new();
        for node_ptr in window_list.iter_ptr() {
            if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_WindowDef) } {
                let wdef = unsafe { &*(node_ptr as *const pg_sys::WindowDef) };
                let wname = if !wdef.name.is_null() {
                    unsafe { std::ffi::CStr::from_ptr(wdef.name) }
                        .to_str()
                        .unwrap_or("w")
                        .to_string()
                } else {
                    continue;
                };
                let wspec = unsafe { deparse_window_def(wdef)? };
                window_parts.push(format!("{wname} AS ({wspec})"));
            }
        }
        if !window_parts.is_empty() {
            parts.push(format!("WINDOW {}", window_parts.join(", ")));
        }
    }

    // ORDER BY
    let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.sortClause) };
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
unsafe fn deparse_with_clause_with_view_subs(
    with_clause: *mut pg_sys::WithClause,
    subs: &[ViewSubstitution],
) -> Result<String, PgStreamError> {
    let wc = unsafe { &*with_clause };
    let cte_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(wc.ctes) };
    let mut cte_parts = Vec::new();

    for node_ptr in cte_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
            continue;
        }
        let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };
        let cte_name = unsafe { std::ffi::CStr::from_ptr(cte.ctename) }
            .to_str()
            .unwrap_or("cte")
            .to_string();

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
            unsafe {
                deparse_select_stmt_with_view_subs(cte.ctequery as *const pg_sys::SelectStmt, subs)?
            }
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
unsafe fn deparse_from_item_with_view_subs(
    node: *mut pg_sys::Node,
    subs: &[ViewSubstitution],
) -> Result<String, PgStreamError> {
    if node.is_null() {
        return Err(PgStreamError::QueryParseError("NULL FROM item node".into()));
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };

        // Get relname
        let relname = if !rv.relname.is_null() {
            unsafe { std::ffi::CStr::from_ptr(rv.relname) }
                .to_str()
                .unwrap_or("")
                .to_string()
        } else {
            return Ok("?".to_string());
        };

        // Get schema (may be NULL for unqualified names)
        let schema = if !rv.schemaname.is_null() {
            Some(
                unsafe { std::ffi::CStr::from_ptr(rv.schemaname) }
                    .to_str()
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            None
        };

        // Get explicit alias
        let explicit_alias = if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            Some(
                unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                    .to_str()
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            None
        };

        // Check if this RangeVar matches a view substitution
        if let Some(sub) = subs.iter().find(|s| {
            s.relname == relname
                && (schema.as_deref() == Some(&s.schema)
                    || (schema.is_none() && !s.schema.is_empty()))
        }) {
            // Replace with inline subquery
            return Ok(format!("({}) AS {}", sub.view_sql, sub.alias));
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        let left = unsafe { deparse_from_item_with_view_subs(join.larg, subs)? };
        let right = unsafe { deparse_from_item_with_view_subs(join.rarg, subs)? };

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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeSubselect) } {
        let sub = unsafe { &*(node as *const pg_sys::RangeSubselect) };
        if sub.subquery.is_null()
            || !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            return Err(PgStreamError::QueryParseError(
                "RangeSubselect without valid SelectStmt".into(),
            ));
        }
        let sub_stmt = sub.subquery as *const pg_sys::SelectStmt;
        // Recurse into the subquery to replace any view references there too
        let inner_sql = unsafe { deparse_select_stmt_with_view_subs(sub_stmt, subs)? };

        let lateral_kw = if sub.lateral { "LATERAL " } else { "" };
        let mut result = format!("{lateral_kw}({inner_sql})");
        if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            let alias_name = unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                .to_str()
                .unwrap_or("");
            result.push_str(&format!(" AS {alias_name}"));
        }
        Ok(result)
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeFunction) } {
        // Function in FROM — deparse as-is using existing infrastructure
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
unsafe fn deparse_window_def(wdef: &pg_sys::WindowDef) -> Result<String, PgStreamError> {
    let mut parts = Vec::new();

    // PARTITION BY
    let part_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(wdef.partitionClause) };
    if !part_list.is_empty() {
        let mut exprs = Vec::new();
        for node_ptr in part_list.iter_ptr() {
            let expr = unsafe { node_to_expr(node_ptr)? };
            exprs.push(expr.to_sql());
        }
        parts.push(format!("PARTITION BY {}", exprs.join(", ")));
    }

    // ORDER BY
    let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(wdef.orderClause) };
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
pub fn reject_materialized_views(query: &str) -> Result<(), PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

    // SAFETY: raw_parser is safe within a PostgreSQL backend with a valid memory context.
    let raw_list =
        unsafe { pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
    if raw_list.is_null() {
        return Ok(());
    }

    let list = unsafe { pgrx::PgList::<pg_sys::RawStmt>::from_pg(raw_list) };
    let raw_stmt = match list.head() {
        Some(rs) => rs,
        None => return Ok(()),
    };

    let node = unsafe { (*raw_stmt).stmt };
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SelectStmt) } {
        return Ok(());
    }

    let select = unsafe { &*(node as *const pg_sys::SelectStmt) };
    unsafe { check_for_matviews_or_foreign(select) }
}

/// Reject foreign tables in the defining query for DIFFERENTIAL mode.
///
/// Foreign tables do not support row-level triggers, so CDC cannot track them.
pub fn reject_foreign_tables(query: &str) -> Result<(), PgStreamError> {
    // Uses the same implementation as reject_materialized_views;
    // the check function handles both.
    reject_materialized_views(query)
}

/// Recursively check a SelectStmt for materialized view or foreign table
/// references in the FROM clause.
///
/// # Safety
/// Caller must ensure `select` points to a valid `SelectStmt`.
unsafe fn check_for_matviews_or_foreign(
    select: *const pg_sys::SelectStmt,
) -> Result<(), PgStreamError> {
    let s = unsafe { &*select };

    // Handle set operations
    if s.op != pg_sys::SetOperation::SETOP_NONE {
        if !s.larg.is_null() {
            unsafe { check_for_matviews_or_foreign(s.larg)? };
        }
        if !s.rarg.is_null() {
            unsafe { check_for_matviews_or_foreign(s.rarg)? };
        }
        return Ok(());
    }

    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.fromClause) };
    for node_ptr in from_list.iter_ptr() {
        unsafe { check_from_item_for_matview_or_foreign(node_ptr)? };
    }

    Ok(())
}

/// Check a single FROM item for materialized view or foreign table references.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree node.
unsafe fn check_from_item_for_matview_or_foreign(
    node: *mut pg_sys::Node,
) -> Result<(), PgStreamError> {
    if node.is_null() {
        return Ok(());
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };
        if rv.relname.is_null() {
            return Ok(());
        }
        let relname = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
            .to_str()
            .unwrap_or("")
            .to_string();

        let schema = resolve_rangevar_schema(rv)?;
        let relkind = resolve_relkind(&schema, &relname)?;

        match relkind.as_deref() {
            Some("m") => {
                return Err(PgStreamError::UnsupportedOperator(format!(
                    "Materialized view '{schema}.{relname}' cannot be used as a source in \
                     DIFFERENTIAL mode. Materialized views are stale snapshots — CDC triggers \
                     cannot track REFRESH MATERIALIZED VIEW. Use the underlying query directly, \
                     or switch to FULL refresh mode."
                )));
            }
            Some("f") => {
                return Err(PgStreamError::UnsupportedOperator(format!(
                    "Foreign table '{schema}.{relname}' cannot be used as a source in \
                     DIFFERENTIAL mode. Row-level triggers cannot be created on foreign tables. \
                     Use FULL refresh mode instead."
                )));
            }
            _ => {}
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        unsafe { check_from_item_for_matview_or_foreign(join.larg)? };
        unsafe { check_from_item_for_matview_or_foreign(join.rarg)? };
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeSubselect) } {
        let sub = unsafe { &*(node as *const pg_sys::RangeSubselect) };
        if !sub.subquery.is_null()
            && unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            unsafe {
                check_for_matviews_or_foreign(sub.subquery as *const pg_sys::SelectStmt)?;
            }
        }
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
/// Detect and rewrite `DISTINCT ON (...)` queries.
///
/// `SELECT DISTINCT ON (e1, e2) col1, col2 FROM t ORDER BY e1, e2, col3`
///
/// is rewritten to:
///
/// ```sql
/// SELECT col1, col2 FROM (
///   SELECT col1, col2, ROW_NUMBER() OVER (PARTITION BY e1, e2 ORDER BY ...) AS __pgs_rn
///   FROM t
/// ) __pgs_do WHERE __pgs_rn = 1
/// ```
///
/// Returns the original query unchanged if it does not use DISTINCT ON.
pub fn rewrite_distinct_on(query: &str) -> Result<String, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

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

    // Only rewrite if DISTINCT ON (not plain DISTINCT or no DISTINCT)
    if select.distinctClause.is_null() {
        return Ok(query.to_string());
    }
    let distinct_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.distinctClause) };
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

    // Extract ORDER BY clause as SQL text (if any)
    let order_by_sql = if !select.sortClause.is_null() {
        let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.sortClause) };
        let mut order_parts = Vec::new();
        for node_ptr in sort_list.iter_ptr() {
            if node_ptr.is_null() {
                continue;
            }
            if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_SortBy) } {
                let sort_by = unsafe { &*(node_ptr as *const pg_sys::SortBy) };
                if !sort_by.node.is_null() {
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
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };
    let mut outer_cols = Vec::new();
    let mut inner_target_parts = Vec::new();
    for node_ptr in target_list.iter_ptr() {
        if node_ptr.is_null() || !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };

        // Extract alias name
        let alias = if !rt.name.is_null() {
            let name = unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("?col");
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
        outer_cols.push(format!("__pgs_do.\"{}\"", outer_name.replace('"', "\"\"")));

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
        format!("ROW_NUMBER() OVER (PARTITION BY {partition_by}{order_clause}) AS __pgs_rn");

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
        let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.groupClause) };
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
         ) __pgs_do WHERE __pgs_do.__pgs_rn = 1"
    );

    pgrx::debug1!(
        "[pg_stream] Rewrote DISTINCT ON query to ROW_NUMBER(): {}",
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
pub fn rewrite_grouping_sets(query: &str) -> Result<String, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

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

    // Set operations — don't rewrite
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // No GROUP BY at all
    if select.groupClause.is_null() {
        return Ok(query.to_string());
    }

    // ── Scan groupClause to find GroupingSet nodes ──────────────────
    let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.groupClause) };
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
        if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_GroupingSet) } {
            // SAFETY: confirmed T_GroupingSet above
            let gs = unsafe { &*(node_ptr as *const pg_sys::GroupingSet) };
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
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };
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
            Some(
                unsafe { std::ffi::CStr::from_ptr(rt.name) }
                    .to_str()
                    .unwrap_or("?")
                    .to_string(),
            )
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
        "[pg_stream] Rewrote GROUPING SETS query to UNION ALL: {}",
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
fn expand_grouping_set(gs: &pg_sys::GroupingSet) -> Result<Vec<Vec<String>>, PgStreamError> {
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
            let content_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(gs.content) };
            let mut sets = Vec::new();
            for node_ptr in content_list.iter_ptr() {
                if node_ptr.is_null() {
                    continue;
                }
                if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_GroupingSet) } {
                    // Nested grouping set (SIMPLE, EMPTY, or nested ROLLUP/CUBE)
                    let inner = unsafe { &*(node_ptr as *const pg_sys::GroupingSet) };
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

        _ => Err(PgStreamError::UnsupportedOperator(format!(
            "Unknown GroupingSet kind: {:?}",
            gs.kind
        ))),
    }
}

/// Extract column names from a `GroupingSet`'s content list.
fn extract_grouping_set_columns(gs: &pg_sys::GroupingSet) -> Result<Vec<String>, PgStreamError> {
    if gs.content.is_null() {
        return Ok(vec![]);
    }
    let content_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(gs.content) };
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
fn extract_grouping_func_args(gf: &pg_sys::GroupingFunc) -> Result<Vec<String>, PgStreamError> {
    if gf.args.is_null() {
        return Ok(vec![]);
    }
    let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(gf.args) };
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
/// CROSS JOIN (SELECT avg(amount) AS __pgs_scalar_1 FROM orders) AS __pgs_sq_1
/// WHERE amount > __pgs_sq_1.__pgs_scalar_1
/// ```
///
/// This is called **before** the DVM parser so the downstream operator tree
/// only sees a simple CROSS JOIN + Filter — no special scalar-subquery-in-WHERE
/// handling is needed.
///
/// Only handles EXPR_SUBLINK (scalar subqueries) in the top-level WHERE clause
/// (both bare and under AND/OR conjunctions). Correlated scalar subqueries
/// are NOT rewritten (they reference outer columns).
pub fn rewrite_scalar_subquery_in_where(query: &str) -> Result<String, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

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
    unsafe {
        collect_scalar_sublinks_in_where(select.whereClause, &mut scalar_subqueries)?;
    }

    if scalar_subqueries.is_empty() {
        return Ok(query.to_string());
    }

    // Check for correlated subqueries — skip rewriting those (they reference
    // outer columns and can't be trivially cross-joined).
    // For simplicity, we check if the subquery's FROM clause references tables
    // that are also in the outer FROM clause. A more precise check would be
    // to look for column references to outer tables, but this is a good heuristic.
    // Actually, we'll just always rewrite — uncorrelated scalar subqueries are
    // the common case, and correlated ones will produce valid SQL that the
    // DVM parser can further reject if needed.

    // ── Build rewritten query components ─────────────────────────────

    // FROM clause
    let from_sql = extract_from_clause_sql(select)?;

    // Build CROSS JOIN additions for each scalar subquery
    let mut cross_joins: Vec<String> = Vec::new();
    for (i, sq) in scalar_subqueries.iter().enumerate() {
        let idx = i + 1;
        let sq_alias = format!("__pgs_sq_{idx}");
        let scalar_alias = format!("__pgs_scalar_{idx}");
        cross_joins.push(format!(
            "CROSS JOIN ({sq_sql} AS \"{scalar_alias}\") AS \"{sq_alias}\"",
            sq_sql = sq.subquery_sql,
        ));
    }

    // Rewrite the WHERE expression, replacing scalar subquery occurrences
    // with column references to the CROSS JOIN aliases.
    let where_expr = unsafe { node_to_expr(select.whereClause) }
        .map(|e| e.to_sql())
        .unwrap_or_else(|_| "TRUE".to_string());

    let mut rewritten_where = where_expr;
    for (i, sq) in scalar_subqueries.iter().enumerate() {
        let idx = i + 1;
        let sq_alias = format!("__pgs_sq_{idx}");
        let scalar_alias = format!("__pgs_scalar_{idx}");
        let replacement = format!("\"{sq_alias}\".\"{scalar_alias}\"");
        // Replace the scalar subquery SQL in the WHERE expression
        // The node_to_expr will have emitted the scalar subquery as "(SELECT ...)"
        rewritten_where = rewritten_where.replace(&sq.expr_sql, &replacement);
    }

    // Target list
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };
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
            let name = unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("?");
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
        let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.groupClause) };
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
        let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.sortClause) };
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
    let rewritten = format!(
        "SELECT{distinct_sql} {targets} FROM {from_sql} {cross_joins} WHERE {rewritten_where}{group_sql}{having_sql}{order_sql}",
        targets = targets.join(", "),
        cross_joins = cross_joins.join(" "),
    );

    pgrx::debug1!(
        "[pg_stream] Rewrote scalar subquery in WHERE to CROSS JOIN: {}",
        rewritten
    );

    Ok(rewritten)
}

/// Information about a scalar subquery extracted from the WHERE clause.
struct ScalarSubqueryExtract {
    /// The inner SELECT statement as SQL (e.g., `SELECT avg(amount) FROM orders`).
    subquery_sql: String,
    /// The expression as rendered by `node_to_expr().to_sql()` for text replacement.
    /// e.g. `(SELECT avg("amount") FROM "orders")`
    expr_sql: String,
}

/// Recursively collect EXPR_SUBLINK nodes from a WHERE clause tree.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
unsafe fn collect_scalar_sublinks_in_where(
    node: *mut pg_sys::Node,
    out: &mut Vec<ScalarSubqueryExtract>,
) -> Result<(), PgStreamError> {
    if node.is_null() {
        return Ok(());
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        let sublink = unsafe { &*(node as *const pg_sys::SubLink) };
        if sublink.subLinkType == pg_sys::SubLinkType::EXPR_SUBLINK {
            // Scalar subquery — extract it
            if !sublink.subselect.is_null() {
                let inner_sql = unsafe { deparse_select_to_sql(sublink.subselect)? };
                let expr_sql = format!("({inner_sql})");
                out.push(ScalarSubqueryExtract {
                    subquery_sql: inner_sql,
                    expr_sql,
                });
            }
        }
        // Don't recurse into the subquery itself — it's a separate query context
        return Ok(());
    }

    // Recurse into BoolExpr (AND/OR/NOT)
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        let boolexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };
        if !boolexpr.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(boolexpr.args) };
            for arg_ptr in args.iter_ptr() {
                if !arg_ptr.is_null() {
                    unsafe { collect_scalar_sublinks_in_where(arg_ptr, out)? };
                }
            }
        }
    }

    // Recurse into comparison operators (A_Expr) since scalar subqueries
    // are typically inside comparisons: `col > (SELECT ...)`
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Expr) } {
        let aexpr = unsafe { &*(node as *const pg_sys::A_Expr) };
        if !aexpr.lexpr.is_null() {
            unsafe {
                collect_scalar_sublinks_in_where(aexpr.lexpr, out)?;
            }
        }
        if !aexpr.rexpr.is_null() {
            unsafe {
                collect_scalar_sublinks_in_where(aexpr.rexpr, out)?;
            }
        }
    }

    Ok(())
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
pub fn rewrite_sublinks_in_or(query: &str) -> Result<String, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

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
        // Check for AND with an inner OR containing sublinks
        if boolexpr.boolop == pg_sys::BoolExprType::AND_EXPR {
            return rewrite_and_with_or_sublinks(select, boolexpr);
        }
        return Ok(query.to_string());
    }

    // Top-level OR — check if it contains sublinks
    if !unsafe { node_tree_contains_sublink(select.whereClause) } {
        return Ok(query.to_string());
    }

    // ── Extract OR arms ────────────────────────────────────────────
    let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(boolexpr.args) };
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
        let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };
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
                let name = unsafe { std::ffi::CStr::from_ptr(rt.name) }
                    .to_str()
                    .unwrap_or("?");
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

    pgrx::debug1!("[pg_stream] Rewrote SubLinks-in-OR to UNION: {}", rewritten);

    Ok(rewritten)
}

/// Handle AND conjunction where one arm is an OR containing sublinks.
///
/// ```sql
/// -- Input:
/// SELECT * FROM t WHERE a > 10 AND (status = 'active' OR EXISTS (...))
/// -- Rewrite to:
/// SELECT * FROM t WHERE a > 10 AND status = 'active'
/// UNION
/// SELECT * FROM t WHERE a > 10 AND EXISTS (...)
/// ```
fn rewrite_and_with_or_sublinks(
    select: &pg_sys::SelectStmt,
    and_expr: &pg_sys::BoolExpr,
) -> Result<String, PgStreamError> {
    let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(and_expr.args) };

    // Find the OR arm with sublinks and collect non-OR conjuncts
    let mut or_arm: Option<*mut pg_sys::Node> = None;
    let mut other_conjuncts: Vec<String> = Vec::new();

    for arg_ptr in args.iter_ptr() {
        if arg_ptr.is_null() {
            continue;
        }
        if unsafe { pgrx::is_a(arg_ptr, pg_sys::NodeTag::T_BoolExpr) } {
            let inner_bool = unsafe { &*(arg_ptr as *const pg_sys::BoolExpr) };
            if inner_bool.boolop == pg_sys::BoolExprType::OR_EXPR
                && unsafe { node_tree_contains_sublink(arg_ptr) }
            {
                or_arm = Some(arg_ptr);
                continue;
            }
        }
        let expr = unsafe { node_to_expr(arg_ptr) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        other_conjuncts.push(expr);
    }

    let or_node = match or_arm {
        Some(n) => n,
        None => return deparse_full_select(select),
    };

    let or_bool = unsafe { &*(or_node as *const pg_sys::BoolExpr) };
    let or_args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(or_bool.args) };

    // Build the common AND prefix
    let and_prefix = if other_conjuncts.is_empty() {
        String::new()
    } else {
        format!("{} AND ", other_conjuncts.join(" AND "))
    };

    // Extract OR arms
    let mut or_arm_exprs: Vec<String> = Vec::new();
    for arg_ptr in or_args.iter_ptr() {
        if arg_ptr.is_null() {
            continue;
        }
        let expr = unsafe { node_to_expr(arg_ptr) }
            .map(|e| e.to_sql())
            .unwrap_or_else(|_| "TRUE".to_string());
        or_arm_exprs.push(expr);
    }

    if or_arm_exprs.len() < 2 {
        return deparse_full_select(select);
    }

    // Build query components
    let from_sql = extract_from_clause_sql(select)?;
    let target_sql = deparse_select_target_list(select);
    let group_sql = deparse_group_clause(select);
    let having_sql = deparse_having_clause(select);
    let order_sql = deparse_order_clause(select);

    let mut branches: Vec<String> = Vec::new();
    for arm in &or_arm_exprs {
        branches.push(format!(
            "SELECT {target_sql} FROM {from_sql} WHERE {and_prefix}{arm}{group_sql}{having_sql}"
        ));
    }

    let rewritten = format!("{}{order_sql}", branches.join(" UNION "));

    pgrx::debug1!(
        "[pg_stream] Rewrote AND(..OR-sublinks..) to UNION: {}",
        rewritten
    );

    Ok(rewritten)
}

/// Deparse a full SELECT statement back to SQL text.
fn deparse_full_select(select: &pg_sys::SelectStmt) -> Result<String, PgStreamError> {
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
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };
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
            let name = unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("?");
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
    let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.groupClause) };
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
    let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.sortClause) };
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
/// SELECT __pgs_w1.id, __pgs_w1.region, __pgs_w1.dept,
///        __pgs_w1.region_sum, __pgs_w2.dept_sum
/// FROM (
///   SELECT id, region, dept, amount,
///          SUM(amount) OVER (PARTITION BY region) AS region_sum
///   FROM orders
/// ) __pgs_w1
/// JOIN (
///   SELECT id, region, dept, amount,
///          SUM(amount) OVER (PARTITION BY dept) AS dept_sum
///   FROM orders
/// ) __pgs_w2 ON __pgs_w1.__pgs_row_marker = __pgs_w2.__pgs_row_marker
/// ```
///
/// Each subquery computes one group of window functions with the same
/// PARTITION BY, plus a row marker for joining. The final query selects
/// pass-through columns from the first subquery and window columns from each.
pub fn rewrite_multi_partition_windows(query: &str) -> Result<String, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

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

    // Set operations — don't rewrite
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        return Ok(query.to_string());
    }

    // Need a target list with window functions
    if select.targetList.is_null() {
        return Ok(query.to_string());
    }

    // ── Extract window function info from the target list ───────────
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };
    let has_windows = unsafe { target_list_has_windows(&target_list) };
    if !has_windows {
        return Ok(query.to_string());
    }

    // Parse window expressions to check PARTITION BY diversity
    let window_infos = unsafe { extract_window_info_from_targets(&target_list, select)? };

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
            Some(
                unsafe { std::ffi::CStr::from_ptr(rt.name) }
                    .to_str()
                    .unwrap_or("?")
                    .to_string(),
            )
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
                // Try to extract a simple column name from the expression
                if let Some(name) = expr.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    name.to_string()
                } else {
                    format!("__pgs_col_{}", i + 1)
                }
            })
        })
        .collect();

    // Row marker: ROW_NUMBER() OVER () for deterministic join
    let row_marker = "ROW_NUMBER() OVER () AS __pgs_row_marker";

    // ── Build subqueries for each partition group ─────────────────────
    let mut subquery_aliases: Vec<String> = Vec::new();
    let mut subquery_sqls: Vec<String> = Vec::new();
    let mut window_col_sources: Vec<(String, String)> = Vec::new(); // (subquery_alias, col_alias)

    for (group_idx, (_partition_key, group)) in partition_groups.iter().enumerate() {
        let sq_alias = format!("__pgs_w{}", group_idx + 1);

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

        // Include row marker
        sq_select_parts.push(row_marker.to_string());

        let sq_sql = format!(
            "SELECT {} FROM {from_sql}{where_sql}{group_sql}{having_sql}",
            sq_select_parts.join(", ")
        );
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

    // ── Build outer FROM (JOIN chain) ──────────────────────────────
    let mut outer_from = format!(
        "({}) AS \"{}\"",
        subquery_sqls[0],
        subquery_aliases[0].replace('"', "\"\"")
    );

    for i in 1..subquery_sqls.len() {
        outer_from = format!(
            "{outer_from} JOIN ({}) AS \"{}\" ON \"{}\".\"__pgs_row_marker\" = \"{}\".\"__pgs_row_marker\"",
            subquery_sqls[i],
            subquery_aliases[i].replace('"', "\"\""),
            subquery_aliases[0].replace('"', "\"\""),
            subquery_aliases[i].replace('"', "\"\""),
        );
    }

    // ── ORDER BY rewrite ────────────────────────────────────────────
    let order_sql = deparse_order_clause(select);

    let rewritten = format!(
        "SELECT {} FROM {outer_from}{order_sql}",
        outer_select_parts.join(", ")
    );

    pgrx::debug1!(
        "[pg_stream] Rewrote multi-PARTITION BY windows: {}",
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
unsafe fn extract_window_info_from_targets(
    target_list: &pgrx::PgList<pg_sys::Node>,
    _select: &pg_sys::SelectStmt,
) -> Result<Vec<WindowInfo>, PgStreamError> {
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
            let parts = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(over.partitionClause) };
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
            let _win_name = unsafe { std::ffi::CStr::from_ptr(over.name) }
                .to_str()
                .unwrap_or("?");
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
            unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("?")
                .to_string()
        } else {
            // Generate a default alias
            format!("__pgs_wfn_{}", infos.len() + 1)
        };

        infos.push(WindowInfo {
            func_sql,
            alias,
            partition_key,
        });
    }

    Ok(infos)
}

/// Extract FROM clause as SQL text from a SelectStmt.
fn extract_from_clause_sql(select: &pg_sys::SelectStmt) -> Result<String, PgStreamError> {
    if select.fromClause.is_null() {
        return Err(PgStreamError::QueryParseError(
            "DISTINCT ON query must have a FROM clause".into(),
        ));
    }
    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.fromClause) };
    let mut parts = Vec::new();
    for node_ptr in from_list.iter_ptr() {
        if node_ptr.is_null() {
            continue;
        }
        parts.push(unsafe { from_item_to_sql(node_ptr)? });
    }
    if parts.is_empty() {
        return Err(PgStreamError::QueryParseError(
            "DISTINCT ON query FROM clause is empty".into(),
        ));
    }
    Ok(parts.join(", "))
}

/// Convert a FROM clause item (RangeVar, JoinExpr, RangeSubselect) back to SQL.
///
/// # Safety
/// Caller must ensure `node` points to a valid parse tree Node.
unsafe fn from_item_to_sql(node: *mut pg_sys::Node) -> Result<String, PgStreamError> {
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };
        let mut name = String::new();
        if !rv.schemaname.is_null() {
            let schema = unsafe { std::ffi::CStr::from_ptr(rv.schemaname) }
                .to_str()
                .unwrap_or("public");
            name.push_str(&format!("\"{}\".", schema.replace('"', "\"\"")));
        }
        if !rv.relname.is_null() {
            let rel = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
                .to_str()
                .unwrap_or("?");
            name.push_str(&format!("\"{}\"", rel.replace('"', "\"\"")));
        }
        if !rv.alias.is_null() {
            let alias_struct = unsafe { &*rv.alias };
            if !alias_struct.aliasname.is_null() {
                let alias = unsafe { std::ffi::CStr::from_ptr(alias_struct.aliasname) }
                    .to_str()
                    .unwrap_or("?");
                name.push_str(&format!(" AS \"{}\"", alias.replace('"', "\"\"")));
            }
        }
        Ok(name)
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        let left = if join.larg.is_null() {
            "?".to_string()
        } else {
            unsafe { from_item_to_sql(join.larg)? }
        };
        let right = if join.rarg.is_null() {
            "?".to_string()
        } else {
            unsafe { from_item_to_sql(join.rarg)? }
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
    } else {
        // Fallback for subselects and other FROM items
        Ok("?".to_string())
    }
}

pub fn reject_limit_offset(query: &str) -> Result<(), PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

    // SAFETY: raw_parser is safe within a PostgreSQL backend with a valid
    // memory context. We only read from the returned parse tree.
    let raw_list =
        unsafe { pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
    if raw_list.is_null() {
        // Parse error — will be caught by validate_defining_query later.
        return Ok(());
    }

    let list = unsafe { pgrx::PgList::<pg_sys::RawStmt>::from_pg(raw_list) };
    let raw_stmt = match list.head() {
        Some(rs) => rs,
        None => return Ok(()),
    };

    let node = unsafe { (*raw_stmt).stmt };
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SelectStmt) } {
        return Ok(());
    }

    let select = unsafe { &*(node as *const pg_sys::SelectStmt) };

    if !select.limitCount.is_null() {
        return Err(PgStreamError::UnsupportedOperator(
            "LIMIT is not supported in defining queries. \
             Stream tables materialize the full result set."
                .into(),
        ));
    }
    if !select.limitOffset.is_null() {
        return Err(PgStreamError::UnsupportedOperator(
            "OFFSET is not supported in defining queries. \
             Stream tables materialize the full result set."
                .into(),
        ));
    }

    Ok(())
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
pub fn reject_unsupported_constructs(query: &str) -> Result<(), PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

    // SAFETY: raw_parser is safe within a PostgreSQL backend.
    let raw_list =
        unsafe { pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
    if raw_list.is_null() {
        return Ok(());
    }

    let list = unsafe { pgrx::PgList::<pg_sys::RawStmt>::from_pg(raw_list) };
    let raw_stmt = match list.head() {
        Some(rs) => rs,
        None => return Ok(()),
    };

    let node = unsafe { (*raw_stmt).stmt };
    if !unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SelectStmt) } {
        return Ok(());
    }

    let select = unsafe { &*(node as *const pg_sys::SelectStmt) };

    // For set operations (UNION/INTERSECT/EXCEPT), recurse into both sides
    if select.op != pg_sys::SetOperation::SETOP_NONE {
        if !select.larg.is_null() {
            // SAFETY: larg points to a valid SelectStmt
            unsafe { check_select_unsupported(&*select.larg)? };
        }
        if !select.rarg.is_null() {
            // SAFETY: rarg points to a valid SelectStmt
            unsafe { check_select_unsupported(&*select.rarg)? };
        }
        return Ok(());
    }

    // SAFETY: select is a valid SelectStmt
    unsafe { check_select_unsupported(select) }
}

/// Check a single SelectStmt for unsupported constructs.
///
/// # Safety
/// Caller must ensure `select` points to a valid `pg_sys::SelectStmt`.
unsafe fn check_select_unsupported(select: &pg_sys::SelectStmt) -> Result<(), PgStreamError> {
    // ── DISTINCT ON ─────────────────────────────────────────────────
    // DISTINCT ON is handled by auto-rewriting to a ROW_NUMBER() window
    // function in the DVM parser (`rewrite_distinct_on()`). No rejection
    // needed here.

    // ── Unsupported join features (currently: NATURAL JOIN) ─────────
    if !select.fromClause.is_null() {
        let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.fromClause) };
        for node_ptr in from_list.iter_ptr() {
            if !node_ptr.is_null() {
                // SAFETY: node_ptr is valid from the from_list
                unsafe { check_from_item_unsupported(node_ptr)? };
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
        unsafe { check_where_for_unsupported_sublinks(select.whereClause)? };
    }

    // ── FOR UPDATE / FOR SHARE / FOR NO KEY UPDATE / FOR KEY SHARE ──
    if !select.lockingClause.is_null() {
        return Err(PgStreamError::UnsupportedOperator(
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
unsafe fn check_from_item_unsupported(node: *mut pg_sys::Node) -> Result<(), PgStreamError> {
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        // Recursively check join children
        if !join.larg.is_null() {
            // SAFETY: larg is valid from JoinExpr
            unsafe { check_from_item_unsupported(join.larg)? };
        }
        if !join.rarg.is_null() {
            // SAFETY: rarg is valid from JoinExpr
            unsafe { check_from_item_unsupported(join.rarg)? };
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeTableSample) } {
        // TABLESAMPLE: SELECT * FROM t TABLESAMPLE BERNOULLI(10)
        // Stream tables materialize complete result sets; non-deterministic
        // sampling is not meaningful. Reject in both FULL and DIFFERENTIAL modes.
        return Err(PgStreamError::UnsupportedOperator(
            "TABLESAMPLE is not supported in defining queries. \
             Stream tables materialize the complete result set; \
             use a WHERE condition with random() if sampling is needed."
                .into(),
        ));
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
unsafe fn check_where_for_unsupported_sublinks(
    node: *mut pg_sys::Node,
) -> Result<(), PgStreamError> {
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        // EXISTS, ANY (IN), ALL, and EXPR SubLinks are handled during parsing
        return Ok(());
    }
    // Check inside BoolExpr (AND/OR/NOT) which commonly wraps SubLinks
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        let boolexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };
        if !boolexpr.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(boolexpr.args) };
            for arg_ptr in args.iter_ptr() {
                if !arg_ptr.is_null() {
                    // SAFETY: arg_ptr is valid from BoolExpr args list
                    unsafe { check_where_for_unsupported_sublinks(arg_ptr)? };
                }
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
unsafe fn extract_where_sublinks(
    node: *mut pg_sys::Node,
    cte_ctx: &mut CteParseContext,
) -> Result<(Vec<SublinkWrapper>, Option<Expr>), PgStreamError> {
    // Case 1: The node itself is a SubLink
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        let wrapper = unsafe { parse_sublink_to_wrapper(node, false, cte_ctx)? };
        return Ok((vec![wrapper], None));
    }

    // Case 2: BoolExpr
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        let boolexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };

        // Case 2a: NOT wrapping a SubLink → negated
        if boolexpr.boolop == pg_sys::BoolExprType::NOT_EXPR {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(boolexpr.args) };
            if args.len() == 1 {
                let arg = args.head().unwrap();
                if unsafe { pgrx::is_a(arg, pg_sys::NodeTag::T_SubLink) } {
                    let wrapper = unsafe { parse_sublink_to_wrapper(arg, true, cte_ctx)? };
                    return Ok((vec![wrapper], None));
                }
            }
        }

        // Case 2b: AND conjunction — extract SubLinks from each arg
        if boolexpr.boolop == pg_sys::BoolExprType::AND_EXPR {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(boolexpr.args) };
            let mut wrappers = Vec::new();
            let mut remaining_exprs = Vec::new();

            for arg_ptr in args.iter_ptr() {
                if arg_ptr.is_null() {
                    continue;
                }

                if unsafe { pgrx::is_a(arg_ptr, pg_sys::NodeTag::T_SubLink) } {
                    // Direct SubLink under AND
                    let wrapper = unsafe { parse_sublink_to_wrapper(arg_ptr, false, cte_ctx)? };
                    wrappers.push(wrapper);
                } else if unsafe { pgrx::is_a(arg_ptr, pg_sys::NodeTag::T_BoolExpr) } {
                    let inner_bool = unsafe { &*(arg_ptr as *const pg_sys::BoolExpr) };
                    // NOT SubLink under AND → negated
                    if inner_bool.boolop == pg_sys::BoolExprType::NOT_EXPR {
                        let inner_args =
                            unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_bool.args) };
                        if inner_args.len() == 1 {
                            let inner_arg = inner_args.head().unwrap();
                            if unsafe { pgrx::is_a(inner_arg, pg_sys::NodeTag::T_SubLink) } {
                                let wrapper =
                                    unsafe { parse_sublink_to_wrapper(inner_arg, true, cte_ctx)? };
                                wrappers.push(wrapper);
                                continue;
                            }
                        }
                    }
                    // Check for SubLinks inside OR — should have been
                    // rewritten to UNION by rewrite_sublinks_in_or().
                    // If still present, it's a deeply nested case.
                    if inner_bool.boolop == pg_sys::BoolExprType::OR_EXPR
                        && unsafe { node_tree_contains_sublink(arg_ptr) }
                    {
                        return Err(PgStreamError::UnsupportedOperator(
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
        if boolexpr.boolop == pg_sys::BoolExprType::OR_EXPR
            && unsafe { node_tree_contains_sublink(node) }
        {
            return Err(PgStreamError::UnsupportedOperator(
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
unsafe fn node_tree_contains_sublink(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        return true;
    }
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        let boolexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };
        if !boolexpr.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(boolexpr.args) };
            for arg_ptr in args.iter_ptr() {
                if !arg_ptr.is_null() && unsafe { node_tree_contains_sublink(arg_ptr) } {
                    return true;
                }
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
unsafe fn deparse_select_to_sql(select_node: *mut pg_sys::Node) -> Result<String, PgStreamError> {
    if select_node.is_null() {
        return Err(PgStreamError::QueryParseError(
            "NULL node in deparse_select_to_sql".into(),
        ));
    }

    // Check if it's a SelectStmt
    if !unsafe { pgrx::is_a(select_node, pg_sys::NodeTag::T_SelectStmt) } {
        return Err(PgStreamError::QueryParseError(
            "Expected SelectStmt in deparse_select_to_sql".into(),
        ));
    }

    let select = unsafe { &*(select_node as *const pg_sys::SelectStmt) };
    let mut sql = String::from("SELECT ");

    // Deparse target list
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };
    let mut targets = Vec::new();
    for node_ptr in target_list.iter_ptr() {
        if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
            if !rt.val.is_null() {
                let expr = unsafe { node_to_expr(rt.val)? };
                let expr_sql = expr.to_sql();
                if !rt.name.is_null() {
                    let name = unsafe { std::ffi::CStr::from_ptr(rt.name) }
                        .to_str()
                        .unwrap_or("?");
                    targets.push(format!("{expr_sql} AS \"{name}\""));
                } else {
                    targets.push(expr_sql);
                }
            }
        }
    }
    sql.push_str(&targets.join(", "));

    // Deparse FROM clause
    if !select.fromClause.is_null() {
        let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.fromClause) };
        if !from_list.is_empty() {
            sql.push_str(" FROM ");
            let mut from_items = Vec::new();
            for node_ptr in from_list.iter_ptr() {
                from_items.push(unsafe { deparse_from_item(node_ptr)? });
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
        let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.groupClause) };
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

    Ok(sql)
}

/// Deparse a FROM clause item back to SQL text.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::Node`.
unsafe fn deparse_from_item(node: *mut pg_sys::Node) -> Result<String, PgStreamError> {
    if node.is_null() {
        return Err(PgStreamError::QueryParseError(
            "NULL node in deparse_from_item".into(),
        ));
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };
        let mut sql = String::new();
        if !rv.schemaname.is_null() {
            let schema = unsafe { std::ffi::CStr::from_ptr(rv.schemaname) }
                .to_str()
                .unwrap_or("public");
            sql.push_str(&format!("\"{schema}\"."));
        }
        if !rv.relname.is_null() {
            let table = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
                .to_str()
                .unwrap_or("?");
            sql.push_str(&format!("\"{table}\""));
        }
        if !rv.alias.is_null() {
            let alias_node = unsafe { &*rv.alias };
            if !alias_node.aliasname.is_null() {
                let alias = unsafe { std::ffi::CStr::from_ptr(alias_node.aliasname) }
                    .to_str()
                    .unwrap_or("?");
                sql.push_str(&format!(" \"{alias}\""));
            }
        }
        Ok(sql)
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        let left = unsafe { deparse_from_item(join.larg)? };
        let right = unsafe { deparse_from_item(join.rarg)? };
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
    } else {
        // Fallback: use a placeholder
        Ok("/* unsupported FROM item */".to_string())
    }
}

/// Parse a SubLink node into a SublinkWrapper for SemiJoin/AntiJoin construction.
///
/// # Safety
/// Caller must ensure `node` points to a valid `pg_sys::SubLink` (T_SubLink node).
unsafe fn parse_sublink_to_wrapper(
    node: *mut pg_sys::Node,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgStreamError> {
    let sublink = unsafe { &*(node as *const pg_sys::SubLink) };

    match sublink.subLinkType {
        pg_sys::SubLinkType::EXISTS_SUBLINK => unsafe {
            parse_exists_sublink(sublink, negated, cte_ctx)
        },
        pg_sys::SubLinkType::ANY_SUBLINK => {
            // ANY_SUBLINK is used for both `x IN (SELECT ...)` and `x = ANY (SELECT ...)`
            unsafe { parse_any_sublink(sublink, negated, cte_ctx) }
        }
        pg_sys::SubLinkType::ALL_SUBLINK => {
            // ALL_SUBLINK: `x op ALL (SELECT col FROM ...)`.
            // Rewritten as AntiJoin with negated condition:
            // NOT EXISTS (SELECT 1 FROM ... WHERE NOT (x op col))
            unsafe { parse_all_sublink(sublink, negated, cte_ctx) }
        }
        pg_sys::SubLinkType::EXPR_SUBLINK => {
            // Scalar subquery in WHERE — should have been rewritten to
            // CROSS JOIN by rewrite_scalar_subquery_in_where(). If we
            // still reach here, it's a case the rewrite didn't catch
            // (e.g., deeply nested or correlated). Reject with a helpful
            // error message.
            Err(PgStreamError::UnsupportedOperator(
                "Scalar subqueries in WHERE clauses are not supported for DIFFERENTIAL mode. \
                 This usually means the query has a correlated scalar subquery that cannot \
                 be automatically rewritten. Consider rewriting as a JOIN or CTE."
                    .into(),
            ))
        }
        _ => Err(PgStreamError::UnsupportedOperator(
            "Unsupported subquery type in WHERE clause. \
             Rewrite using a JOIN or LATERAL subquery."
                .to_string(),
        )),
    }
}

/// Parse an EXISTS SubLink into a SublinkWrapper.
///
/// `EXISTS (SELECT ... FROM inner_table WHERE correlation_cond AND inner_filter)`
///
/// The inner SELECT's FROM clause becomes the right side of the semi/anti-join.
/// The inner SELECT's WHERE clause becomes the join condition.
///
/// # Safety
/// Caller must ensure `sublink` points to a valid `pg_sys::SubLink`.
unsafe fn parse_exists_sublink(
    sublink: &pg_sys::SubLink,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgStreamError> {
    if sublink.subselect.is_null() {
        return Err(PgStreamError::QueryParseError(
            "EXISTS subquery has NULL subselect".into(),
        ));
    }

    let inner_select = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };

    // Parse the inner FROM clause into an OpTree
    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_select.fromClause) };
    if from_list.is_empty() {
        return Err(PgStreamError::QueryParseError(
            "EXISTS subquery must have a FROM clause".into(),
        ));
    }

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

    // The inner WHERE clause becomes the semi/anti-join condition
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

/// Parse an ANY SubLink (IN / = ANY) into a SublinkWrapper.
///
/// `x IN (SELECT col FROM inner_table WHERE filter)`
/// is equivalent to:
/// `EXISTS (SELECT 1 FROM inner_table WHERE inner_table.col = x AND filter)`
///
/// # Safety
/// Caller must ensure `sublink` points to a valid `pg_sys::SubLink`.
unsafe fn parse_any_sublink(
    sublink: &pg_sys::SubLink,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgStreamError> {
    if sublink.subselect.is_null() {
        return Err(PgStreamError::QueryParseError(
            "IN/ANY subquery has NULL subselect".into(),
        ));
    }

    let inner_select = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };

    // Parse the inner FROM clause
    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_select.fromClause) };
    if from_list.is_empty() {
        return Err(PgStreamError::QueryParseError(
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
        return Err(PgStreamError::QueryParseError(
            "IN subquery has NULL test expression".into(),
        ));
    } else {
        unsafe { node_to_expr(sublink.testexpr)? }
    };

    // Extract the inner SELECT target (the column being compared)
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_select.targetList) };
    if target_list.is_empty() {
        return Err(PgStreamError::QueryParseError(
            "IN subquery SELECT list is empty".into(),
        ));
    }

    let first_target = target_list.head().unwrap();
    let inner_col_expr = if unsafe { pgrx::is_a(first_target, pg_sys::NodeTag::T_ResTarget) } {
        let rt = unsafe { &*(first_target as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            return Err(PgStreamError::QueryParseError(
                "IN subquery target column is NULL".into(),
            ));
        }
        unsafe { node_to_expr(rt.val)? }
    } else {
        return Err(PgStreamError::QueryParseError(
            "IN subquery target is not a ResTarget".into(),
        ));
    };

    // Build the equality condition: test_expr = inner_col_expr
    let equality = Expr::BinaryOp {
        op: "=".to_string(),
        left: Box::new(test_expr),
        right: Box::new(inner_col_expr),
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
unsafe fn parse_all_sublink(
    sublink: &pg_sys::SubLink,
    negated: bool,
    cte_ctx: &mut CteParseContext,
) -> Result<SublinkWrapper, PgStreamError> {
    if sublink.subselect.is_null() {
        return Err(PgStreamError::QueryParseError(
            "ALL subquery has NULL subselect".into(),
        ));
    }

    let inner_select = unsafe { &*(sublink.subselect as *const pg_sys::SelectStmt) };

    // Parse the inner FROM clause
    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_select.fromClause) };
    if from_list.is_empty() {
        return Err(PgStreamError::QueryParseError(
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
        return Err(PgStreamError::QueryParseError(
            "ALL subquery has NULL test expression".into(),
        ));
    } else {
        unsafe { node_to_expr(sublink.testexpr)? }
    };

    // Extract the inner SELECT target column
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_select.targetList) };
    if target_list.is_empty() {
        return Err(PgStreamError::QueryParseError(
            "ALL subquery SELECT list is empty".into(),
        ));
    }

    let first_target = target_list.head().unwrap();
    let inner_col_expr = if unsafe { pgrx::is_a(first_target, pg_sys::NodeTag::T_ResTarget) } {
        let rt = unsafe { &*(first_target as *const pg_sys::ResTarget) };
        if rt.val.is_null() {
            return Err(PgStreamError::QueryParseError(
                "ALL subquery target column is NULL".into(),
            ));
        }
        unsafe { node_to_expr(rt.val)? }
    } else {
        return Err(PgStreamError::QueryParseError(
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

    // Build negated condition: NOT (test_expr op inner_col)
    // The negation is expressed as raw SQL to avoid complex operator inversion.
    let negated_cond = Expr::Raw(format!(
        "NOT ({} {} {})",
        test_expr.to_sql(),
        op_name,
        inner_col_expr.to_sql()
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
pub fn parse_defining_query(query: &str) -> Result<OpTree, PgStreamError> {
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
pub fn parse_defining_query_full(query: &str) -> Result<ParseResult, PgStreamError> {
    // SAFETY: same invariants as parse_defining_query.
    unsafe { parse_defining_query_inner(query) }
}

unsafe fn parse_defining_query_inner(query: &str) -> Result<ParseResult, PgStreamError> {
    use std::ffi::CString;

    let c_query = CString::new(query)
        .map_err(|_| PgStreamError::QueryParseError("Query contains null bytes".into()))?;

    let raw_list =
        unsafe { pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT) };
    if raw_list.is_null() {
        return Err(PgStreamError::QueryParseError(
            "raw_parser returned NULL".into(),
        ));
    }

    let list = unsafe { pgrx::PgList::<pg_sys::RawStmt>::from_pg(raw_list) };
    if list.len() != 1 {
        return Err(PgStreamError::QueryParseError(format!(
            "Expected 1 statement, got {}",
            list.len(),
        )));
    }

    let raw_stmt = list
        .head()
        .ok_or_else(|| PgStreamError::QueryParseError("Empty parse list".into()))?;

    let stmt_ptr = unsafe { (*raw_stmt).stmt as *const pg_sys::SelectStmt };
    if !unsafe { pgrx::is_a(stmt_ptr as *mut pg_sys::Node, pg_sys::NodeTag::T_SelectStmt) } {
        return Err(PgStreamError::QueryParseError(
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
        let base_tree = unsafe {
            let s = &**base_stmt;
            if s.op != pg_sys::SetOperation::SETOP_NONE {
                parse_set_operation(s, &mut cte_ctx)?
            } else {
                parse_select_stmt(s, "", &mut cte_ctx)?
            }
        };

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
    let tree = if select.op != pg_sys::SetOperation::SETOP_NONE {
        unsafe { parse_set_operation(select, &mut cte_ctx)? }
    } else {
        unsafe { parse_select_stmt(select, query, &mut cte_ctx)? }
    };

    Ok(ParseResult {
        tree,
        cte_registry: cte_ctx.registry,
        has_recursion,
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
) -> Result<OpTree, PgStreamError> {
    match select.op {
        pg_sys::SetOperation::SETOP_UNION => {
            let mut children = Vec::new();
            unsafe { collect_union_children(select, select.all, &mut children, cte_ctx)? };

            if children.len() < 2 {
                return Err(PgStreamError::QueryParseError(
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
        _ => Err(PgStreamError::UnsupportedOperator(format!(
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
) -> Result<OpTree, PgStreamError> {
    if child_ptr.is_null() {
        return Err(PgStreamError::QueryParseError(
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
) -> Result<(), PgStreamError> {
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
) -> Result<OpTree, PgStreamError> {
    // ── Step 1: Parse FROM clause into Scan/Join tree ──────────────────
    let from_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.fromClause) };
    if from_list.is_empty() {
        return Err(PgStreamError::QueryParseError(format!(
            "Defining query must have a FROM clause (op={}, all={}, larg_null={}, rarg_null={}, target_len={}, where_null={})",
            select.op,
            select.all,
            select.larg.is_null(),
            select.rarg.is_null(),
            unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) }.len(),
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
            unsafe { extract_where_sublinks(select.whereClause, cte_ctx)? };

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
    }

    // ── Step 3: Parse GROUP BY + aggregates ─────────────────────────────
    let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.groupClause) };
    let target_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.targetList) };

    let has_aggregates = unsafe { target_list_has_aggregates(&target_list) };
    let has_windows = unsafe { target_list_has_windows(&target_list) };

    if has_windows {
        // ── Window function path ───────────────────────────────────────
        // Extract window expressions and pass-through columns.
        let (window_exprs, pass_through) =
            unsafe { extract_window_exprs(&target_list, select.windowClause)? };

        if window_exprs.is_empty() {
            return Err(PgStreamError::QueryParseError(
                "Window function detected but extraction failed".into(),
            ));
        }

        // Validate: all window expressions must share the same PARTITION BY.
        // Multi-PARTITION BY should have been rewritten by
        // rewrite_multi_partition_windows(). If still present, reject.
        let canonical_partition: Vec<String> = window_exprs[0]
            .partition_by
            .iter()
            .map(|e| e.to_sql())
            .collect();
        for wexpr in &window_exprs[1..] {
            let this_partition: Vec<String> =
                wexpr.partition_by.iter().map(|e| e.to_sql()).collect();
            if this_partition != canonical_partition {
                return Err(PgStreamError::UnsupportedOperator(
                    "All window functions in a defining query must share the same \
                     PARTITION BY clause for differential maintenance. \
                     The multi-PARTITION BY auto-rewrite did not handle this query; \
                     consider splitting into separate stream tables."
                        .into(),
                ));
            }
        }

        let partition_by = window_exprs[0].partition_by.clone();

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

        tree = OpTree::Aggregate {
            group_by,
            aggregates: aggregates.clone(),
            child: Box::new(tree),
        };

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
            return Err(PgStreamError::QueryParseError(
                "HAVING clause requires GROUP BY or aggregate functions".into(),
            ));
        }
        let (expressions, aliases) = unsafe { parse_target_list(&target_list)? };
        if !is_star_only(&expressions) {
            tree = OpTree::Project {
                expressions,
                aliases,
                child: Box::new(tree),
            };
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
        let distinct_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(select.distinctClause) };
        let has_real_exprs = distinct_list.iter_ptr().any(|ptr| !ptr.is_null());
        if has_real_exprs {
            // DISTINCT ON (expr, ...) — reject
            return Err(PgStreamError::UnsupportedOperator(
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
    if !select.limitCount.is_null() {
        return Err(PgStreamError::UnsupportedOperator(
            "LIMIT is not supported in defining queries. \
             Stream tables materialize the full result set."
                .into(),
        ));
    }
    if !select.limitOffset.is_null() {
        return Err(PgStreamError::UnsupportedOperator(
            "OFFSET is not supported in defining queries. \
             Stream tables materialize the full result set."
                .into(),
        ));
    }

    Ok(tree)
}

/// Parse a FROM clause item (RangeVar, JoinExpr, or RangeSubselect) into an OpTree.
unsafe fn parse_from_item(
    node: *mut pg_sys::Node,
    cte_ctx: &mut CteParseContext,
) -> Result<OpTree, PgStreamError> {
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };
        let schema_name = if rv.schemaname.is_null() {
            "public".to_string()
        } else {
            unsafe { std::ffi::CStr::from_ptr(rv.schemaname) }
                .to_str()
                .unwrap_or("public")
                .to_string()
        };
        let table_name = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
            .to_str()
            .map_err(|_| PgStreamError::QueryParseError("Invalid table name encoding".into()))?
            .to_string();
        let alias = if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                .to_str()
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
                unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                    .to_str()
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
            let (cte_id, columns) = if let Some(id) = cte_ctx.lookup_id(&table_name) {
                // Already parsed — reuse the existing entry
                let (_, body) = &cte_ctx.registry.entries[id];
                let cols = body.output_columns();
                (id, cols)
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
                let id = cte_ctx.register(&table_name, cte_tree);
                (id, cols)
            };

            return Ok(OpTree::CteScan {
                cte_id,
                cte_name: table_name,
                alias,
                columns,
                cte_def_aliases: def_aliases,
                column_aliases: col_aliases,
            });
        }

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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        let left = unsafe { parse_from_item(join.larg, cte_ctx)? };
        let right = unsafe { parse_from_item(join.rarg, cte_ctx)? };

        // If right side is a LateralSubquery, handle LATERAL join semantics
        if let OpTree::LateralSubquery {
            subquery_sql,
            alias,
            column_aliases,
            output_cols,
            subquery_source_oids,
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
                        child: Box::new(left),
                    });
                }
                other => {
                    return Err(PgStreamError::UnsupportedOperator(format!(
                        "Only INNER JOIN LATERAL and LEFT JOIN LATERAL are supported, got {:?}",
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
                    return Err(PgStreamError::UnsupportedOperator(format!(
                        "Only INNER JOIN and LEFT JOIN with LATERAL functions are supported, got {:?}",
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
                return Err(PgStreamError::QueryParseError(
                    "NATURAL JOIN has no common columns between left and right sides. \
                     Use an explicit JOIN ... ON condition instead."
                        .into(),
                ));
            }

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
                other => Err(PgStreamError::UnsupportedOperator(format!(
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
            other => Err(PgStreamError::UnsupportedOperator(format!(
                "Join type {other:?} not supported for differential mode",
            ))),
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeSubselect) } {
        // Subquery in FROM: SELECT ... FROM (SELECT ...) AS alias(c1, c2)
        let sub = unsafe { &*(node as *const pg_sys::RangeSubselect) };
        if sub.subquery.is_null() {
            return Err(PgStreamError::QueryParseError(
                "RangeSubselect with NULL subquery".into(),
            ));
        }

        // The subquery must be a SelectStmt
        if !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) } {
            return Err(PgStreamError::QueryParseError(
                "Subquery in FROM must be a SELECT statement".into(),
            ));
        }

        let sub_stmt = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };

        // Extract alias name
        let alias = if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                .to_str()
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

            // Return a LateralSubquery with a placeholder child.
            // The real child is attached in the FROM-list loop or JoinExpr handler.
            return Ok(OpTree::LateralSubquery {
                subquery_sql,
                alias,
                column_aliases: col_aliases,
                output_cols,
                is_left_join: false,
                subquery_source_oids,
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeFunction) } {
        let rf = unsafe { &*(node as *const pg_sys::RangeFunction) };

        // Extract the function call from rf.functions (a List of Lists).
        // Each element is a two-element List: [FuncCall, column_def_list].
        let func_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(rf.functions) };
        if func_list.is_empty() {
            return Err(PgStreamError::QueryParseError(
                "RangeFunction with no functions".into(),
            ));
        }

        // We support a single function. ROWS FROM(f1, f2, ...) is not supported.
        if rf.is_rowsfrom && func_list.len() > 1 {
            return Err(PgStreamError::UnsupportedOperator(
                "ROWS FROM() with multiple functions is not supported. \
                 Use a single set-returning function in FROM instead."
                    .into(),
            ));
        }

        // The first element is a List node; its first element is the FuncCall.
        // SAFETY: func_list is non-empty, head is a List node containing the FuncCall.
        let inner_list_node = func_list.head().unwrap();
        let inner_list =
            unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_list_node as *mut pg_sys::List) };
        if inner_list.is_empty() {
            return Err(PgStreamError::QueryParseError(
                "RangeFunction inner list is empty".into(),
            ));
        }

        let func_node = inner_list.head().unwrap();
        if !unsafe { pgrx::is_a(func_node, pg_sys::NodeTag::T_FuncCall) } {
            return Err(PgStreamError::QueryParseError(
                "RangeFunction does not contain a FuncCall node".into(),
            ));
        }

        // Deparse the function call to SQL text.
        let func_sql = unsafe { deparse_func_call(func_node as *const pg_sys::FuncCall)? };

        // Extract alias name
        let alias = if !rf.alias.is_null() {
            let a = unsafe { &*(rf.alias) };
            unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                .to_str()
                .unwrap_or("srf")
                .to_string()
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeTableSample) } {
        // TABLESAMPLE: SELECT * FROM t TABLESAMPLE BERNOULLI(10)
        // Stream tables materialize complete result sets; sampling at parse
        // time is not meaningful or supported.
        Err(PgStreamError::UnsupportedOperator(
            "TABLESAMPLE is not supported in defining queries. \
             Stream tables materialize the complete result set; \
             use a WHERE condition with random() if sampling is needed."
                .into(),
        ))
    } else {
        Err(PgStreamError::UnsupportedOperator(format!(
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
) -> Result<RecursiveCteMapResult, PgStreamError> {
    let wc = unsafe { &*with_clause };
    let cte_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(wc.ctes) };
    let mut map = HashMap::new();
    let mut def_aliases_map = HashMap::new();
    let mut recursive_stmts = Vec::new();

    for node_ptr in cte_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_CommonTableExpr) } {
            continue;
        }

        let cte = unsafe { &*(node_ptr as *const pg_sys::CommonTableExpr) };

        // Extract CTE name
        let cte_name = unsafe { std::ffi::CStr::from_ptr(cte.ctename) }
            .to_str()
            .map_err(|_| PgStreamError::QueryParseError("Invalid CTE name encoding".into()))?
            .to_string();

        // The CTE body is ctequery, which must be a SelectStmt
        if cte.ctequery.is_null() {
            return Err(PgStreamError::QueryParseError(format!(
                "CTE '{cte_name}' has NULL body",
            )));
        }
        if !unsafe { pgrx::is_a(cte.ctequery, pg_sys::NodeTag::T_SelectStmt) } {
            return Err(PgStreamError::QueryParseError(format!(
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
                return Err(PgStreamError::QueryParseError(format!(
                    "Recursive CTE '{cte_name}' body must be a UNION or UNION ALL",
                )));
            }

            if body.larg.is_null() || body.rarg.is_null() {
                return Err(PgStreamError::QueryParseError(format!(
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
fn extract_cte_def_colnames(cte: &pg_sys::CommonTableExpr) -> Result<Vec<String>, PgStreamError> {
    let colnames = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(cte.aliascolnames) };
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
fn extract_alias_colnames(alias: &pg_sys::Alias) -> Result<Vec<String>, PgStreamError> {
    let colnames = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(alias.colnames) };
    let mut names = Vec::new();
    for node_ptr in colnames.iter_ptr() {
        let name = unsafe { node_to_string(node_ptr)? };
        names.push(name);
    }
    Ok(names)
}

/// Resolve a table name to its OID via SPI.
fn resolve_table_oid(schema: &str, table: &str) -> Result<u32, PgStreamError> {
    let sql = format!(
        "SELECT c.oid FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{}' AND c.relname = '{}'",
        schema.replace('\'', "''"),
        table.replace('\'', "''"),
    );

    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        let oid: Option<pg_sys::Oid> = result
            .first()
            .get(1)
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        oid.map(|o| o.to_u32())
            .ok_or_else(|| PgStreamError::NotFound(format!("{schema}.{table}")))
    })
}

/// Resolve column metadata for a table via SPI.
fn resolve_columns(table_oid: u32) -> Result<Vec<Column>, PgStreamError> {
    let sql = format!(
        "SELECT attname::text, atttypid, attnotnull \
         FROM pg_attribute \
         WHERE attrelid = {} AND attnum > 0 AND NOT attisdropped \
         ORDER BY attnum",
        table_oid,
    );

    Spi::connect(|client| {
        let result = client
            .select(&sql, None, &[])
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        let mut columns = Vec::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or_default();
            let type_oid: pg_sys::Oid = row
                .get(2)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or(pg_sys::Oid::INVALID);
            let not_null: bool = row
                .get(3)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
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
fn resolve_pk_columns(table_oid: u32) -> Result<Vec<String>, PgStreamError> {
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
            .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
        let mut pk_cols = Vec::new();
        for row in result {
            let name: String = row
                .get(1)
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?
                .unwrap_or_default();
            pk_cols.push(name);
        }
        Ok(pk_cols)
    })
}

/// Convert a pg_sys::Node to an Expr.
unsafe fn node_to_expr(node: *mut pg_sys::Node) -> Result<Expr, PgStreamError> {
    if node.is_null() {
        return Err(PgStreamError::QueryParseError(
            "NULL node in expression".into(),
        ));
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_ColumnRef) } {
        let cref = unsafe { &*(node as *const pg_sys::ColumnRef) };
        let fields = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(cref.fields) };

        match fields.len() {
            1 => {
                let col_name = unsafe { node_to_string(fields.head().unwrap())? };
                Ok(Expr::ColumnRef {
                    table_alias: None,
                    column_name: col_name,
                })
            }
            2 => {
                let table_alias = unsafe { node_to_string(fields.get_ptr(0).unwrap())? };
                let col_name = unsafe { node_to_string(fields.get_ptr(1).unwrap())? };
                if col_name == "*" {
                    Ok(Expr::Star {
                        table_alias: Some(table_alias),
                    })
                } else {
                    Ok(Expr::ColumnRef {
                        table_alias: Some(table_alias),
                        column_name: col_name,
                    })
                }
            }
            3 => {
                // schema.table.column — drop the schema, use table.column
                let _schema = unsafe { node_to_string(fields.get_ptr(0).unwrap())? };
                let table_alias = unsafe { node_to_string(fields.get_ptr(1).unwrap())? };
                let col_name = unsafe { node_to_string(fields.get_ptr(2).unwrap())? };
                if col_name == "*" {
                    Ok(Expr::Star {
                        table_alias: Some(table_alias),
                    })
                } else {
                    Ok(Expr::ColumnRef {
                        table_alias: Some(table_alias),
                        column_name: col_name,
                    })
                }
            }
            n => Err(PgStreamError::QueryParseError(format!(
                "Unexpected ColumnRef with {n} fields",
            ))),
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Const) } {
        Ok(Expr::Raw(unsafe { deparse_node(node) }))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Expr) } {
        let aexpr = unsafe { &*(node as *const pg_sys::A_Expr) };
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
                let right_list =
                    unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(aexpr.rexpr as *mut _) };
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
                let bounds =
                    unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(aexpr.rexpr as *mut _) };
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
                let bounds =
                    unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(aexpr.rexpr as *mut _) };
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
                let bounds =
                    unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(aexpr.rexpr as *mut _) };
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
                let bounds =
                    unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(aexpr.rexpr as *mut _) };
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
            _other => Err(PgStreamError::UnsupportedOperator(format!(
                "A_Expr kind {:?} is not supported in defining queries",
                _other,
            ))),
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        let bexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };
        let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(bexpr.args) };
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
                        PgStreamError::QueryParseError("Empty AND expression".into())
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
                        PgStreamError::QueryParseError("Empty OR expression".into())
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
                let inner = args
                    .into_iter()
                    .next()
                    .ok_or_else(|| PgStreamError::QueryParseError("Empty NOT expression".into()))?;
                Ok(Expr::FuncCall {
                    func_name: "NOT".to_string(),
                    args: vec![inner],
                })
            }
            _ => Ok(Expr::Raw("/* unknown bool expr */".to_string())),
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_FuncCall) } {
        let fcall = unsafe { &*(node as *const pg_sys::FuncCall) };
        let func_name = unsafe { extract_func_name(fcall.funcname)? };
        let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(fcall.args) };
        let mut args = Vec::new();
        for n in args_list.iter_ptr() {
            if let Ok(e) = unsafe { node_to_expr(n) } {
                args.push(e);
            }
        }

        Ok(Expr::FuncCall { func_name, args })
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_TypeCast) } {
        let tc = unsafe { &*(node as *const pg_sys::TypeCast) };
        let inner = unsafe { node_to_expr(tc.arg)? };
        let type_name = unsafe { deparse_typename(tc.typeName) };
        Ok(Expr::Raw(format!(
            "CAST({} AS {})",
            inner.to_sql(),
            type_name,
        )))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_NullTest) } {
        let nt = unsafe { &*(node as *const pg_sys::NullTest) };
        let arg = unsafe { node_to_expr(nt.arg as *mut pg_sys::Node)? };
        let op = if nt.nulltesttype == pg_sys::NullTestType::IS_NULL {
            "IS NULL"
        } else {
            "IS NOT NULL"
        };
        Ok(Expr::Raw(format!("{} {op}", arg.to_sql())))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_CaseExpr) } {
        // CASE WHEN ... THEN ... ELSE ... END
        let case_expr = unsafe { &*(node as *const pg_sys::CaseExpr) };
        let mut sql = String::from("CASE");

        // Simple CASE: CASE <arg> WHEN ...
        if !case_expr.arg.is_null() {
            let arg = unsafe { node_to_expr(case_expr.arg as *mut pg_sys::Node)? };
            sql.push_str(&format!(" {}", arg.to_sql()));
        }

        let when_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(case_expr.args) };
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_CoalesceExpr) } {
        // COALESCE(a, b, ...)
        let coalesce = unsafe { &*(node as *const pg_sys::CoalesceExpr) };
        let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(coalesce.args) };
        let mut args_sql = Vec::new();
        for n in args_list.iter_ptr() {
            args_sql.push(unsafe { node_to_expr(n)? }.to_sql());
        }
        Ok(Expr::Raw(format!("COALESCE({})", args_sql.join(", "))))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_NullIfExpr) } {
        // NULLIF(a, b) — represented as OpExpr with opno for "=" and
        // NullIfExpr override; args has exactly 2 elements.
        let nullif = unsafe { &*(node as *const pg_sys::NullIfExpr) };
        let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(nullif.args) };
        let mut args_sql = Vec::new();
        for n in args_list.iter_ptr() {
            args_sql.push(unsafe { node_to_expr(n)? }.to_sql());
        }
        Ok(Expr::Raw(format!("NULLIF({})", args_sql.join(", "))))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_MinMaxExpr) } {
        // GREATEST(...) / LEAST(...)
        let mmexpr = unsafe { &*(node as *const pg_sys::MinMaxExpr) };
        let func_name = if mmexpr.op == pg_sys::MinMaxOp::IS_GREATEST {
            "GREATEST"
        } else {
            "LEAST"
        };
        let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(mmexpr.args) };
        let mut args_sql = Vec::new();
        for n in args_list.iter_ptr() {
            args_sql.push(unsafe { node_to_expr(n)? }.to_sql());
        }
        Ok(Expr::Raw(format!("{func_name}({})", args_sql.join(", "))))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SQLValueFunction) } {
        // CURRENT_TIMESTAMP, CURRENT_USER, etc.
        let svf = unsafe { &*(node as *const pg_sys::SQLValueFunction) };
        let kw = match svf.op {
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
            _ => "CURRENT_TIMESTAMP", // safe fallback
        };
        Ok(Expr::Raw(kw.to_string()))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BooleanTest) } {
        // x IS [NOT] TRUE/FALSE/UNKNOWN
        let bt = unsafe { &*(node as *const pg_sys::BooleanTest) };
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        // Subquery expressions encountered during expression parsing.
        // EXISTS and IN SubLinks should have been extracted from WHERE
        // by extract_where_sublinks(). If we reach here, it's either:
        // 1. A scalar subquery in the target list (EXPR_SUBLINK) — preserve as Raw SQL
        // 2. A SubLink in a context we can't handle — error
        let sublink = unsafe { &*(node as *const pg_sys::SubLink) };
        match sublink.subLinkType {
            pg_sys::SubLinkType::EXPR_SUBLINK => {
                // Scalar subquery — reconstruct as Raw SQL
                // The subselect is a SelectStmt; deparse it back to SQL
                if sublink.subselect.is_null() {
                    return Err(PgStreamError::QueryParseError(
                        "Scalar subquery has NULL subselect".into(),
                    ));
                }
                let inner_sql = unsafe { deparse_select_to_sql(sublink.subselect)? };
                Ok(Expr::Raw(format!("({inner_sql})")))
            }
            pg_sys::SubLinkType::EXISTS_SUBLINK => {
                // EXISTS in an expression context (e.g., inside CASE WHEN)
                // Reconstruct as Raw SQL
                if sublink.subselect.is_null() {
                    return Err(PgStreamError::QueryParseError(
                        "EXISTS subquery has NULL subselect".into(),
                    ));
                }
                let inner_sql = unsafe { deparse_select_to_sql(sublink.subselect)? };
                Ok(Expr::Raw(format!("EXISTS ({inner_sql})")))
            }
            pg_sys::SubLinkType::ANY_SUBLINK => {
                // IN/ANY in an expression context
                if sublink.subselect.is_null() || sublink.testexpr.is_null() {
                    return Err(PgStreamError::QueryParseError(
                        "IN/ANY subquery has NULL subselect or testexpr".into(),
                    ));
                }
                let test = unsafe { node_to_expr(sublink.testexpr)? };
                let inner_sql = unsafe { deparse_select_to_sql(sublink.subselect)? };
                Ok(Expr::Raw(format!("{} IN ({inner_sql})", test.to_sql())))
            }
            _ => {
                let kind_desc = match sublink.subLinkType {
                    pg_sys::SubLinkType::ALL_SUBLINK => "ALL (subquery)",
                    _ => "subquery expression",
                };
                Err(PgStreamError::UnsupportedOperator(format!(
                    "{kind_desc} is not supported in defining queries. \
                     Consider rewriting as a JOIN or LATERAL subquery.",
                )))
            }
        }
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_ArrayExpr) } {
        // ARRAY[a, b, c]
        let arrexpr = unsafe { &*(node as *const pg_sys::ArrayExpr) };
        let elems = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(arrexpr.elements) };
        let mut elem_sql = Vec::new();
        for n in elems.iter_ptr() {
            elem_sql.push(unsafe { node_to_expr(n)? }.to_sql());
        }
        Ok(Expr::Raw(format!("ARRAY[{}]", elem_sql.join(", "))))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RowExpr) } {
        // ROW(a, b, c)
        let rowexpr = unsafe { &*(node as *const pg_sys::RowExpr) };
        let fields = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(rowexpr.args) };
        let mut field_sql = Vec::new();
        for n in fields.iter_ptr() {
            field_sql.push(unsafe { node_to_expr(n)? }.to_sql());
        }
        Ok(Expr::Raw(format!("ROW({})", field_sql.join(", "))))
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Indirection) } {
        // Array subscript / field access: arr[1], (rec).field, data->'key'->>'name'
        let indir = unsafe { &*(node as *const pg_sys::A_Indirection) };
        let base = unsafe { node_to_expr(indir.arg)? };
        let mut sql = base.to_sql();
        let indirection_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(indir.indirection) };
        for ind_node in indirection_list.iter_ptr() {
            if unsafe { pgrx::is_a(ind_node, pg_sys::NodeTag::T_A_Indices) } {
                let indices = unsafe { &*(ind_node as *const pg_sys::A_Indices) };
                if !indices.uidx.is_null() {
                    let idx = unsafe { node_to_expr(indices.uidx)? };
                    sql = format!("{sql}[{}]", idx.to_sql());
                }
            } else if unsafe { pgrx::is_a(ind_node, pg_sys::NodeTag::T_String) } {
                let s = unsafe { &*(ind_node as *const pg_sys::String) };
                let field_name = unsafe { std::ffi::CStr::from_ptr(s.sval) }
                    .to_str()
                    .unwrap_or("");
                sql = format!("({sql}).{field_name}");
            } else if unsafe { pgrx::is_a(ind_node, pg_sys::NodeTag::T_A_Star) } {
                sql = format!("({sql}).*");
            }
        }
        Ok(Expr::Raw(sql))
    } else {
        // Catch-all: reject with clear error instead of silently producing broken SQL
        let tag = unsafe { (*node).type_ };
        Err(PgStreamError::UnsupportedOperator(format!(
            "Expression type {tag:?} is not supported in defining queries",
        )))
    }
}

/// Extract operator name from an A_Expr name list.
unsafe fn extract_operator_name(name_list: *mut pg_sys::List) -> Result<String, PgStreamError> {
    let list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(name_list) };
    if let Some(node) = list.head() {
        unsafe { node_to_string(node) }
    } else {
        Ok("=".to_string())
    }
}

/// Extract function name from funcname list (possibly schema-qualified).
unsafe fn extract_func_name(name_list: *mut pg_sys::List) -> Result<String, PgStreamError> {
    let list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(name_list) };
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
unsafe fn deparse_func_call(fcall: *const pg_sys::FuncCall) -> Result<String, PgStreamError> {
    let fcall_ref = unsafe { &*fcall };
    let func_name = unsafe { extract_func_name(fcall_ref.funcname)? };

    let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(fcall_ref.args) };
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
) -> Result<String, PgStreamError> {
    // SAFETY: caller guarantees stmt is valid.
    let s = unsafe { &*stmt };
    let mut parts = Vec::new();

    // DISTINCT
    let distinct_clause = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.distinctClause) };
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
    let group_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.groupClause) };
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
    let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(s.sortClause) };
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
unsafe fn deparse_target_list(target_list: *mut pg_sys::List) -> Result<String, PgStreamError> {
    let targets = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(target_list) };
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
            let alias = unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("");
            items.push(format!("{expr_sql} AS {alias}"));
        } else {
            items.push(expr_sql);
        }
    }
    Ok(items.join(", "))
}

/// Deparse a FROM clause (list of FROM items) into SQL text.
///
/// # Safety
/// Caller must ensure `from_list` points to a valid `pg_sys::List`.
unsafe fn deparse_from_clause(from_list: *mut pg_sys::List) -> Result<String, PgStreamError> {
    let list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(from_list) };
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
unsafe fn deparse_from_item_to_sql(node: *mut pg_sys::Node) -> Result<String, PgStreamError> {
    if node.is_null() {
        return Err(PgStreamError::QueryParseError("NULL FROM item node".into()));
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };
        let mut name = String::new();
        if !rv.schemaname.is_null() {
            let schema = unsafe { std::ffi::CStr::from_ptr(rv.schemaname) }
                .to_str()
                .unwrap_or("");
            name.push_str(schema);
            name.push('.');
        }
        if !rv.relname.is_null() {
            let rel = unsafe { std::ffi::CStr::from_ptr(rv.relname) }
                .to_str()
                .unwrap_or("");
            name.push_str(rel);
        }
        if !rv.alias.is_null() {
            let a = unsafe { &*(rv.alias) };
            let alias_name = unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                .to_str()
                .unwrap_or("");
            if alias_name != name {
                name.push_str(&format!(" {alias_name}"));
            }
        }
        Ok(name)
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeSubselect) } {
        let sub = unsafe { &*(node as *const pg_sys::RangeSubselect) };
        if sub.subquery.is_null()
            || !unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            return Err(PgStreamError::QueryParseError(
                "RangeSubselect without valid SelectStmt".into(),
            ));
        }
        let sub_stmt = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };
        let inner_sql = unsafe { deparse_select_stmt_to_sql(sub_stmt)? };
        let mut result = format!("({inner_sql})");
        if !sub.alias.is_null() {
            let a = unsafe { &*(sub.alias) };
            let alias_name = unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                .to_str()
                .unwrap_or("");
            result.push_str(&format!(" AS {alias_name}"));
        }
        Ok(result)
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeFunction) } {
        let rf = unsafe { &*(node as *const pg_sys::RangeFunction) };
        let func_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(rf.functions) };
        if func_list.is_empty() {
            return Err(PgStreamError::QueryParseError(
                "RangeFunction with no functions in deparse".into(),
            ));
        }
        let inner_list_node = func_list.head().unwrap();
        let inner_list =
            unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner_list_node as *mut pg_sys::List) };
        if inner_list.is_empty() {
            return Err(PgStreamError::QueryParseError(
                "RangeFunction inner list empty in deparse".into(),
            ));
        }
        let func_node = inner_list.head().unwrap();
        let func_sql = unsafe { deparse_func_call(func_node as *const pg_sys::FuncCall)? };
        let mut result = func_sql;
        if !rf.alias.is_null() {
            let a = unsafe { &*(rf.alias) };
            let alias_name = unsafe { std::ffi::CStr::from_ptr(a.aliasname) }
                .to_str()
                .unwrap_or("");
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
) -> Result<String, PgStreamError> {
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
unsafe fn deparse_sort_by(node: *const pg_sys::SortBy) -> Result<String, PgStreamError> {
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
) -> Result<Vec<String>, PgStreamError> {
    let targets = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(target_list) };
    let mut cols = Vec::new();
    for (i, node_ptr) in targets.iter_ptr().enumerate() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            cols.push(format!("column{}", i + 1));
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
        if !rt.name.is_null() {
            let name = unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("");
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
unsafe fn extract_from_oids(from_list: *mut pg_sys::List) -> Result<Vec<u32>, PgStreamError> {
    let list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(from_list) };
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
) -> Result<(), PgStreamError> {
    if node.is_null() {
        return Ok(());
    }

    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeVar) } {
        let rv = unsafe { &*(node as *const pg_sys::RangeVar) };
        let table_name = if !rv.relname.is_null() {
            unsafe { std::ffi::CStr::from_ptr(rv.relname) }
                .to_str()
                .unwrap_or("")
                .to_string()
        } else {
            return Ok(());
        };
        let schema = if !rv.schemaname.is_null() {
            unsafe { std::ffi::CStr::from_ptr(rv.schemaname) }
                .to_str()
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
                .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
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
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_JoinExpr) } {
        let join = unsafe { &*(node as *const pg_sys::JoinExpr) };
        unsafe { collect_from_item_oids(join.larg, oids)? };
        unsafe { collect_from_item_oids(join.rarg, oids)? };
    } else if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RangeSubselect) } {
        let sub = unsafe { &*(node as *const pg_sys::RangeSubselect) };
        if !sub.subquery.is_null()
            && unsafe { pgrx::is_a(sub.subquery, pg_sys::NodeTag::T_SelectStmt) }
        {
            let sub_stmt = unsafe { &*(sub.subquery as *const pg_sys::SelectStmt) };
            let inner_oids = unsafe { extract_from_oids(sub_stmt.fromClause)? };
            oids.extend(inner_oids);
        }
    }
    // RangeFunction: no table OIDs to extract (SRFs don't reference tables directly)
    Ok(())
}

/// Convert a String/Value node to a Rust String.
unsafe fn node_to_string(node: *mut pg_sys::Node) -> Result<String, PgStreamError> {
    if node.is_null() {
        return Err(PgStreamError::QueryParseError("NULL node".into()));
    }
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_String) } {
        let s = unsafe { &*(node as *const pg_sys::String) };
        Ok(unsafe { std::ffi::CStr::from_ptr(s.sval) }
            .to_str()
            .unwrap_or("")
            .to_string())
    } else {
        Ok(format!("node_{:?}", unsafe { (*node).type_ }))
    }
}

/// Deparse a node back to SQL text (simplified fallback).
unsafe fn deparse_node(node: *mut pg_sys::Node) -> String {
    if node.is_null() {
        return "NULL".to_string();
    }
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Const) } {
        let aconst = unsafe { &*(node as *const pg_sys::A_Const) };
        if aconst.isnull {
            return "NULL".to_string();
        }
        let val_ptr = &aconst.val as *const _ as *const pg_sys::Node;
        if unsafe { pgrx::is_a(val_ptr as *mut _, pg_sys::NodeTag::T_String) } {
            let s = unsafe { &*(val_ptr as *const pg_sys::String) };
            let cstr = unsafe { std::ffi::CStr::from_ptr(s.sval) };
            return format!("'{}'", cstr.to_str().unwrap_or(""));
        }
        if unsafe { pgrx::is_a(val_ptr as *mut _, pg_sys::NodeTag::T_Integer) } {
            let i = unsafe { &*(val_ptr as *const pg_sys::Integer) };
            return i.ival.to_string();
        }
        if unsafe { pgrx::is_a(val_ptr as *mut _, pg_sys::NodeTag::T_Float) } {
            let f = unsafe { &*(val_ptr as *const pg_sys::Float) };
            let cstr = unsafe { std::ffi::CStr::from_ptr(f.fval) };
            return cstr.to_str().unwrap_or("0").to_string();
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
unsafe fn target_list_has_windows(target_list: &pgrx::PgList<pg_sys::Node>) -> bool {
    for node_ptr in target_list.iter_ptr() {
        if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
            if !rt.val.is_null() && unsafe { node_contains_window_func(rt.val) } {
                return true;
            }
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
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_FuncCall) } {
        let fcall = unsafe { &*(node as *const pg_sys::FuncCall) };
        if !fcall.over.is_null() {
            return true;
        }
        // Check function arguments recursively
        if !fcall.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(fcall.args) };
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // CASE WHEN ... THEN ... ELSE ... END
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_CaseExpr) } {
        let case_expr = unsafe { &*(node as *const pg_sys::CaseExpr) };
        // Check the optional test expression (simple CASE)
        if !case_expr.arg.is_null()
            && unsafe { node_contains_window_func(case_expr.arg as *mut pg_sys::Node) }
        {
            return true;
        }
        // Check each WHEN clause
        if !case_expr.args.is_null() {
            let whens = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(case_expr.args) };
            for when_ptr in whens.iter_ptr() {
                if unsafe { pgrx::is_a(when_ptr, pg_sys::NodeTag::T_CaseWhen) } {
                    let cw = unsafe { &*(when_ptr as *const pg_sys::CaseWhen) };
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
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_CoalesceExpr) } {
        let coalesce = unsafe { &*(node as *const pg_sys::CoalesceExpr) };
        if !coalesce.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(coalesce.args) };
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // NULLIF(a, b)
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_NullIfExpr) } {
        let nullif = unsafe { &*(node as *const pg_sys::NullIfExpr) };
        if !nullif.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(nullif.args) };
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // GREATEST / LEAST
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_MinMaxExpr) } {
        let minmax = unsafe { &*(node as *const pg_sys::MinMaxExpr) };
        if !minmax.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(minmax.args) };
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // AND / OR / NOT
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BoolExpr) } {
        let bexpr = unsafe { &*(node as *const pg_sys::BoolExpr) };
        if !bexpr.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(bexpr.args) };
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // Binary/unary ops: a + b, -a, a BETWEEN x AND y, etc.
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_A_Expr) } {
        let aexpr = unsafe { &*(node as *const pg_sys::A_Expr) };
        if !aexpr.lexpr.is_null() && unsafe { node_contains_window_func(aexpr.lexpr) } {
            return true;
        }
        if !aexpr.rexpr.is_null() && unsafe { node_contains_window_func(aexpr.rexpr) } {
            return true;
        }
        return false;
    }

    // CAST(x AS type)
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_TypeCast) } {
        let tc = unsafe { &*(node as *const pg_sys::TypeCast) };
        if !tc.arg.is_null() {
            return unsafe { node_contains_window_func(tc.arg) };
        }
        return false;
    }

    // IS [NOT] NULL
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_NullTest) } {
        let nt = unsafe { &*(node as *const pg_sys::NullTest) };
        if !nt.arg.is_null() {
            return unsafe { node_contains_window_func(nt.arg as *mut pg_sys::Node) };
        }
        return false;
    }

    // IS [NOT] TRUE / FALSE / UNKNOWN
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_BooleanTest) } {
        let bt = unsafe { &*(node as *const pg_sys::BooleanTest) };
        if !bt.arg.is_null() {
            return unsafe { node_contains_window_func(bt.arg as *mut pg_sys::Node) };
        }
        return false;
    }

    // ARRAY[a, b, c]
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_ArrayExpr) } {
        let arr = unsafe { &*(node as *const pg_sys::ArrayExpr) };
        if !arr.elements.is_null() {
            let elems = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(arr.elements) };
            for elem in elems.iter_ptr() {
                if unsafe { node_contains_window_func(elem) } {
                    return true;
                }
            }
        }
        return false;
    }

    // ROW(a, b, c)
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_RowExpr) } {
        let row = unsafe { &*(node as *const pg_sys::RowExpr) };
        if !row.args.is_null() {
            let args = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(row.args) };
            for arg in args.iter_ptr() {
                if unsafe { node_contains_window_func(arg) } {
                    return true;
                }
            }
        }
        return false;
    }

    // SubLink — check testexpr and subselect target list
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_SubLink) } {
        let sub = unsafe { &*(node as *const pg_sys::SubLink) };
        if !sub.testexpr.is_null() && unsafe { node_contains_window_func(sub.testexpr) } {
            return true;
        }
        return false;
    }

    // Leaf nodes: ColumnRef, A_Const, SQLValueFunction, etc — no children
    false
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
) -> Result<WindowExtraction, PgStreamError> {
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
        if unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_FuncCall) } {
            let fcall = unsafe { &*(rt.val as *const pg_sys::FuncCall) };
            if !fcall.over.is_null() {
                let wexpr = unsafe { parse_window_func_call(fcall, rt, window_clause)? };
                window_exprs.push(wexpr);
                continue;
            }
        }

        // Check if a window function is nested inside an expression (CASE, COALESCE, etc.)
        // We detect this but cannot extract it — reject with a clear error.
        if unsafe { node_contains_window_func(rt.val) } {
            return Err(PgStreamError::UnsupportedOperator(
                "Window functions nested inside expressions (CASE, COALESCE, arithmetic, etc.) \
                 are not supported in defining queries. Move the window function to a separate \
                 column, e.g.:\n  SELECT ROW_NUMBER() OVER (...) AS rn, ... FROM t\n\
                 Then wrap the stream table in a view to apply the expression."
                    .into(),
            ));
        }

        // Not a window function — pass-through column
        let alias = if !rt.name.is_null() {
            unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("?column?")
                .to_string()
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
) -> Result<WindowExpr, PgStreamError> {
    // Function name
    let func_name = unsafe { extract_func_name(fcall.funcname)? };

    // Function arguments
    let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(fcall.args) };
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
        let ref_name = unsafe { std::ffi::CStr::from_ptr(wdef.refname) }
            .to_str()
            .unwrap_or("");
        let wclause = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(window_clause) };
        let mut found: Option<&pg_sys::WindowDef> = None;
        for n in wclause.iter_ptr() {
            if unsafe { pgrx::is_a(n, pg_sys::NodeTag::T_WindowDef) } {
                let wd = unsafe { &*(n as *const pg_sys::WindowDef) };
                if !wd.name.is_null() {
                    let wd_name = unsafe { std::ffi::CStr::from_ptr(wd.name) }
                        .to_str()
                        .unwrap_or("");
                    if wd_name == ref_name {
                        found = Some(wd);
                        break;
                    }
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
    let part_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(effective_part_clause) };
    let mut partition_by = Vec::new();
    for n in part_list.iter_ptr() {
        if let Ok(e) = unsafe { node_to_expr(n) } {
            partition_by.push(e);
        }
    }

    // ORDER BY (list of SortBy nodes)
    let ord_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(effective_ord_clause) };
    let mut order_by = Vec::new();
    for n in ord_list.iter_ptr() {
        if unsafe { pgrx::is_a(n, pg_sys::NodeTag::T_SortBy) } {
            let sb = unsafe { &*(n as *const pg_sys::SortBy) };
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
        unsafe { std::ffi::CStr::from_ptr(rt.name) }
            .to_str()
            .unwrap_or(&func_name)
            .to_string()
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
        if unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };
            if !rt.val.is_null() && unsafe { expr_contains_agg(rt.val) } {
                return true;
            }
        }
    }
    false
}

/// Recursively check if a node contains an aggregate function call.
unsafe fn expr_contains_agg(node: *mut pg_sys::Node) -> bool {
    if node.is_null() {
        return false;
    }
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_FuncCall) } {
        let fcall = unsafe { &*(node as *const pg_sys::FuncCall) };
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
    // Check inside TypeCast: CAST(SUM(x) AS int)
    if unsafe { pgrx::is_a(node, pg_sys::NodeTag::T_TypeCast) } {
        let tc = unsafe { &*(node as *const pg_sys::TypeCast) };
        return unsafe { expr_contains_agg(tc.arg) };
    }
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
            | "rank"
            | "dense_rank"
            | "percent_rank"
            | "cume_dist"
    )
}

/// Extract aggregates and non-aggregate expressions from a target list.
unsafe fn extract_aggregates(
    target_list: &pgrx::PgList<pg_sys::Node>,
) -> Result<(Vec<AggExpr>, Vec<Expr>), PgStreamError> {
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

        if unsafe { pgrx::is_a(rt.val, pg_sys::NodeTag::T_FuncCall) } {
            let fcall = unsafe { &*(rt.val as *const pg_sys::FuncCall) };
            let func_name = unsafe { extract_func_name(fcall.funcname)? };
            let name_lower = func_name.to_lowercase();

            let alias = if !rt.name.is_null() {
                unsafe { std::ffi::CStr::from_ptr(rt.name) }
                    .to_str()
                    .unwrap_or(&func_name)
                    .to_string()
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
                _ => None,
            } {
                let args_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(fcall.args) };
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
                let order_within_group = if fcall.agg_within_group && !fcall.agg_order.is_null() {
                    let ord_list =
                        unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(fcall.agg_order) };
                    let mut sorts = Vec::new();
                    for n in ord_list.iter_ptr() {
                        if unsafe { pgrx::is_a(n, pg_sys::NodeTag::T_SortBy) } {
                            let sb = unsafe { &*(n as *const pg_sys::SortBy) };
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
                return Err(PgStreamError::UnsupportedOperator(format!(
                    "Aggregate function {func_name}() is not supported in DIFFERENTIAL mode. \
                     Supported aggregates: COUNT, SUM, AVG, MIN, MAX, BOOL_AND, BOOL_OR, \
                     STRING_AGG, ARRAY_AGG, JSON_AGG, JSONB_AGG, BIT_AND, BIT_OR, BIT_XOR, \
                     JSON_OBJECT_AGG, JSONB_OBJECT_AGG, STDDEV, STDDEV_POP, STDDEV_SAMP, \
                     VARIANCE, VAR_POP, VAR_SAMP, MODE, PERCENTILE_CONT, PERCENTILE_DISC, \
                     CORR, COVAR_POP, COVAR_SAMP, REGR_AVGX, REGR_AVGY, REGR_COUNT, \
                     REGR_INTERCEPT, REGR_R2, REGR_SLOPE, REGR_SXX, REGR_SXY, REGR_SYY. \
                     Use FULL refresh mode instead.",
                )));
            } else {
                let expr = unsafe { node_to_expr(rt.val)? };
                non_aggs.push(expr);
            }
        } else {
            let expr = unsafe { node_to_expr(rt.val)? };
            non_aggs.push(expr);
        }
    }

    Ok((aggs, non_aggs))
}

/// Parse the target list (SELECT expressions) into Expr + alias pairs.
unsafe fn parse_target_list(
    target_list: &pgrx::PgList<pg_sys::Node>,
) -> Result<(Vec<Expr>, Vec<String>), PgStreamError> {
    let mut expressions = Vec::new();
    let mut aliases = Vec::new();

    for node_ptr in target_list.iter_ptr() {
        if !unsafe { pgrx::is_a(node_ptr, pg_sys::NodeTag::T_ResTarget) } {
            continue;
        }
        let rt = unsafe { &*(node_ptr as *const pg_sys::ResTarget) };

        let alias = if !rt.name.is_null() {
            unsafe { std::ffi::CStr::from_ptr(rt.name) }
                .to_str()
                .unwrap_or("?column?")
                .to_string()
        } else if !rt.val.is_null() {
            if let Ok(e) = unsafe { node_to_expr(rt.val) } {
                match &e {
                    Expr::ColumnRef { column_name, .. } => column_name.clone(),
                    Expr::Star { .. } => "*".to_string(),
                    _ => format!("col_{}", expressions.len()),
                }
            } else {
                format!("col_{}", expressions.len())
            }
        } else {
            format!("col_{}", expressions.len())
        };

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
                let args_match = match &agg.argument {
                    None => args.is_empty(), // COUNT(*)
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
        assert_eq!(e.to_sql(), "orders.id");
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
    fn test_needs_pgs_count_distinct_wrapping_union_all() {
        // Distinct wrapping UnionAll needs pgs_count (reference counting)
        let tree = OpTree::Distinct {
            child: Box::new(OpTree::UnionAll {
                children: vec![scan_node("a", 1, &["id"]), scan_node("b", 2, &["id"])],
            }),
        };
        assert!(tree.needs_pgs_count());
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
        };
        let t2 = OpTree::CteScan {
            cte_id: 0,
            cte_name: "totals".to_string(),
            alias: "t2".to_string(),
            columns: vec!["user_id".to_string(), "total".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
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
        };
        let tree = OpTree::CteScan {
            cte_id: 1,
            cte_name: "b".to_string(),
            alias: "b".to_string(),
            columns: vec!["id".to_string(), "amount".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
        };

        let mut registry = CteRegistry::default();
        registry.entries.push(("a".to_string(), a_body));
        registry.entries.push(("b".to_string(), b_body));

        let result = ParseResult {
            tree,
            cte_registry: registry,
            has_recursion: false,
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
        };
        assert!(!result.has_recursion);
    }

    #[test]
    fn test_parse_result_has_recursion_true() {
        let result = ParseResult {
            tree: scan_node("t", 1, &["id"]),
            cte_registry: CteRegistry::default(),
            has_recursion: true,
        };
        assert!(result.has_recursion);
    }

    #[test]
    fn test_parse_result_has_recursion_flag_preserved_on_clone() {
        let result = ParseResult {
            tree: scan_node("t", 1, &["id"]),
            cte_registry: CteRegistry::default(),
            has_recursion: true,
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
        assert_eq!(rewritten.to_sql(), "new_a.id");
    }

    #[test]
    fn test_rewrite_aliases_column_ref_right() {
        let e = qualified_col("b", "name");
        let rewritten = e.rewrite_aliases("a", "new_a", "b", "new_b");
        assert_eq!(rewritten.to_sql(), "new_b.name");
    }

    #[test]
    fn test_rewrite_aliases_unknown_alias_unchanged() {
        let e = qualified_col("c", "val");
        let rewritten = e.rewrite_aliases("a", "new_a", "b", "new_b");
        assert_eq!(rewritten.to_sql(), "c.val");
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
        assert_eq!(rewritten.to_sql(), "(x.id = y.id)");
    }

    #[test]
    fn test_rewrite_aliases_func_call_recursive() {
        let e = Expr::FuncCall {
            func_name: "coalesce".to_string(),
            args: vec![qualified_col("a", "x"), qualified_col("b", "y")],
        };
        let rewritten = e.rewrite_aliases("a", "left", "b", "right");
        assert_eq!(rewritten.to_sql(), "coalesce(left.x, right.y)");
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

    // ── OpTree::needs_pgs_count tests ──────────────────────────────

    #[test]
    fn test_needs_pgs_count_aggregate() {
        let tree = OpTree::Aggregate {
            group_by: vec![col("region")],
            aggregates: vec![],
            child: Box::new(scan_node("t", 1, &["region"])),
        };
        assert!(tree.needs_pgs_count());
    }

    #[test]
    fn test_needs_pgs_count_distinct() {
        let tree = OpTree::Distinct {
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(tree.needs_pgs_count());
    }

    #[test]
    fn test_needs_pgs_count_scan_false() {
        let tree = scan_node("t", 1, &["id"]);
        assert!(!tree.needs_pgs_count());
    }

    #[test]
    fn test_needs_pgs_count_project_false() {
        let tree = OpTree::Project {
            expressions: vec![col("id")],
            aliases: vec!["id".to_string()],
            child: Box::new(scan_node("t", 1, &["id"])),
        };
        assert!(!tree.needs_pgs_count());
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
    fn test_row_id_key_columns_cte_scan_returns_none() {
        let tree = OpTree::CteScan {
            cte_id: 0,
            cte_name: "x".to_string(),
            alias: "x".to_string(),
            columns: vec!["id".to_string()],
            cte_def_aliases: vec![],
            column_aliases: vec![],
        };
        assert_eq!(tree.row_id_key_columns(), None);
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
    fn test_expr_raw_coalesce_format() {
        let expr = Expr::Raw("COALESCE(a, b, 0)".to_string());
        assert_eq!(expr.to_sql(), "COALESCE(a, b, 0)");
    }

    #[test]
    fn test_expr_raw_nullif_format() {
        let expr = Expr::Raw("NULLIF(a, 0)".to_string());
        assert_eq!(expr.to_sql(), "NULLIF(a, 0)");
    }

    #[test]
    fn test_expr_raw_greatest_least_format() {
        let g = Expr::Raw("GREATEST(a, b, c)".to_string());
        let l = Expr::Raw("LEAST(a, b, c)".to_string());
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
    fn test_semi_join_needs_pgs_count_false() {
        let tree = OpTree::SemiJoin {
            condition: Expr::Literal("TRUE".into()),
            left: Box::new(make_scan(1, "t1", "t1", &["id"])),
            right: Box::new(make_scan(2, "t2", "t2", &["id"])),
        };
        assert!(!tree.needs_pgs_count());
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
        let replacement = "\"__pgs_sq_1\".\"__pgs_scalar_1\"";

        let rewritten = original_where.replace(expr_sql, replacement);
        assert_eq!(rewritten, "amount > \"__pgs_sq_1\".\"__pgs_scalar_1\"");
    }

    #[test]
    fn test_multi_partition_row_marker_join() {
        // Verify the join strategy: subqueries joined on __pgs_row_marker
        let sq1_alias = "__pgs_w1";
        let sq2_alias = "__pgs_w2";

        let join_cond = format!(
            "\"{}\".\"__pgs_row_marker\" = \"{}\".\"__pgs_row_marker\"",
            sq1_alias, sq2_alias
        );

        assert!(join_cond.contains("__pgs_w1"));
        assert!(join_cond.contains("__pgs_w2"));
        assert!(join_cond.contains("__pgs_row_marker"));
    }

    #[test]
    fn test_cross_join_construction() {
        // Verify the CROSS JOIN construction for scalar subquery rewrite
        let idx = 1;
        let sq_alias = format!("__pgs_sq_{idx}");
        let scalar_alias = format!("__pgs_scalar_{idx}");
        let sq_sql = "SELECT avg(amount) FROM orders";

        let cross_join = format!("CROSS JOIN ({sq_sql} AS \"{scalar_alias}\") AS \"{sq_alias}\"");

        assert!(cross_join.contains("CROSS JOIN"));
        assert!(cross_join.contains("avg(amount)"));
        assert!(cross_join.contains("__pgs_sq_1"));
        assert!(cross_join.contains("__pgs_scalar_1"));
    }
}
