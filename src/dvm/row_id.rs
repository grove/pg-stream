//! Row ID generation strategies for different operators.
//!
//! Row ID computation depends on the operator that produces the row.
//! See `PLAN.md` Phase 6.2 for the full strategy table.

/// Strategies for computing row IDs at each operator.
#[derive(Debug, Clone)]
pub enum RowIdStrategy {
    /// Use the primary key columns of the source table.
    PrimaryKey { pk_columns: Vec<String> },
    /// Hash all columns (fallback when no PK is available).
    AllColumns { columns: Vec<String> },
    /// Combine two child row IDs (for joins).
    CombineChildren,
    /// Hash the group-by columns (for aggregates).
    GroupByKey { group_columns: Vec<String> },
    /// Pass through the child's row ID (for project/filter).
    PassThrough,
}
