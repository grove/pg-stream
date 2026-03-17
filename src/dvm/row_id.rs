//! Row ID generation strategies for different operators.
//!
//! Row ID computation depends on the operator that produces the row.
//! See `PLAN.md` Phase 6.2 for the full strategy table.

/// Strategies for computing row IDs at each operator.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::RowIdStrategy;

    #[test]
    fn test_row_id_strategy_debug_includes_variant_and_columns() {
        let strategy = RowIdStrategy::PrimaryKey {
            pk_columns: vec!["tenant_id".to_string(), "order_id".to_string()],
        };

        let debug = format!("{strategy:?}");

        assert!(debug.contains("PrimaryKey"));
        assert!(debug.contains("tenant_id"));
        assert!(debug.contains("order_id"));
    }

    #[test]
    fn test_row_id_strategy_clone_preserves_group_columns() {
        let original = RowIdStrategy::GroupByKey {
            group_columns: vec!["region".to_string(), "year".to_string()],
        };

        assert_eq!(original.clone(), original);
    }

    #[test]
    fn test_row_id_strategy_all_columns_preserves_column_order() {
        let strategy = RowIdStrategy::AllColumns {
            columns: vec![
                "id".to_string(),
                "created_at".to_string(),
                "payload".to_string(),
            ],
        };

        assert_eq!(
            strategy,
            RowIdStrategy::AllColumns {
                columns: vec![
                    "id".to_string(),
                    "created_at".to_string(),
                    "payload".to_string(),
                ],
            }
        );
    }

    #[test]
    fn test_row_id_strategy_unit_variants_compare_by_variant() {
        assert_eq!(
            RowIdStrategy::CombineChildren,
            RowIdStrategy::CombineChildren
        );
        assert_eq!(RowIdStrategy::PassThrough, RowIdStrategy::PassThrough);
    }

    // ── P3: cross-variant inequality, debug coverage, edge cases ──────────

    #[test]
    fn test_row_id_strategy_variants_are_not_equal_to_each_other() {
        assert_ne!(RowIdStrategy::CombineChildren, RowIdStrategy::PassThrough);
        assert_ne!(
            RowIdStrategy::PrimaryKey { pk_columns: vec![] },
            RowIdStrategy::AllColumns { columns: vec![] },
        );
        assert_ne!(
            RowIdStrategy::PrimaryKey {
                pk_columns: vec!["id".to_string()]
            },
            RowIdStrategy::GroupByKey {
                group_columns: vec!["id".to_string()]
            },
        );
    }

    #[test]
    fn test_row_id_strategy_debug_unit_variants() {
        assert!(format!("{:?}", RowIdStrategy::CombineChildren).contains("CombineChildren"));
        assert!(format!("{:?}", RowIdStrategy::PassThrough).contains("PassThrough"));
    }

    #[test]
    fn test_row_id_strategy_empty_columns_accepted() {
        let pk = RowIdStrategy::PrimaryKey { pk_columns: vec![] };
        let all = RowIdStrategy::AllColumns { columns: vec![] };
        let grp = RowIdStrategy::GroupByKey {
            group_columns: vec![],
        };

        assert_eq!(pk, RowIdStrategy::PrimaryKey { pk_columns: vec![] });
        assert_eq!(all, RowIdStrategy::AllColumns { columns: vec![] });
        assert_eq!(
            grp,
            RowIdStrategy::GroupByKey {
                group_columns: vec![]
            }
        );
    }

    #[test]
    fn test_row_id_strategy_primary_key_clone_is_equal() {
        let original = RowIdStrategy::PrimaryKey {
            pk_columns: vec!["id".to_string(), "tenant".to_string()],
        };
        assert_eq!(original.clone(), original);
    }

    #[test]
    fn test_row_id_strategy_all_columns_clone_is_equal() {
        let original = RowIdStrategy::AllColumns {
            columns: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        assert_eq!(original.clone(), original);
    }
}
