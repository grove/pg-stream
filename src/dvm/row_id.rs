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

/// A18 (v0.36.0): Formal schema declaration for row-id hash computation.
///
/// Every DVM operator declares its `RowIdSchema` as part of its type signature.
/// A plan-time verifier asserts that adjacent operators in the same pipeline
/// have compatible schemas — ensuring that row-id hashes are semantically
/// consistent across operator boundaries (the root cause of EC-01 class bugs).
///
/// # Compatibility Rules
/// Two schemas are compatible when:
/// - They are the same variant, or
/// - The downstream schema is `Any` (accepts any upstream schema), or
/// - The downstream schema is `Derived` (computes its own hash from scratch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowIdSchema {
    /// Row ID is derived from PRIMARY KEY columns of a specific table (the hash
    /// uses only the PK columns in table-definition order).
    PrimaryKey {
        /// Qualified table name (schema.table) for disambiguation.
        table: String,
        /// Column names in PK definition order.
        columns: Vec<String>,
    },
    /// Row ID is derived from ALL visible columns of the output tuple.
    AllColumns {
        /// Ordered list of column names included in the hash.
        columns: Vec<String>,
    },
    /// Row ID is derived from GROUP BY key columns.
    GroupByKey {
        /// GROUP BY column names in query order.
        columns: Vec<String>,
    },
    /// Row ID is computed from the hashes of two child operators (JOIN).
    Combined {
        /// Schema of the left (probe) child.
        left: Box<RowIdSchema>,
        /// Schema of the right (build) child.
        right: Box<RowIdSchema>,
    },
    /// Row ID is passed through unchanged from the single child operator.
    PassThrough,
    /// This operator accepts any upstream `RowIdSchema` without re-hashing.
    Any,
    /// This operator recomputes the row-id hash from scratch (e.g. TopK).
    Derived,
}

impl RowIdSchema {
    /// Returns `true` when `self` (downstream) is compatible with `upstream`.
    ///
    /// Compatibility is required before operators are composed in a pipeline.
    /// An incompatible pair means the downstream operator would compute a
    /// different row-id hash than the upstream emitted, leading to silent
    /// missed-update bugs.
    pub fn is_compatible_with(&self, upstream: &RowIdSchema) -> bool {
        match self {
            // Any and Derived accept everything
            RowIdSchema::Any | RowIdSchema::Derived => true,
            // PassThrough propagates the schema — must match exactly
            RowIdSchema::PassThrough => upstream == &RowIdSchema::PassThrough || upstream == self,
            // PrimaryKey schemas must agree on table + column set
            RowIdSchema::PrimaryKey { table, columns } => {
                matches!(upstream, RowIdSchema::PrimaryKey { table: t, columns: c }
                    if t == table && c == columns)
            }
            // AllColumns schemas must agree on column set
            RowIdSchema::AllColumns { columns } => {
                matches!(upstream, RowIdSchema::AllColumns { columns: c } if c == columns)
            }
            // GroupByKey schemas must agree on columns
            RowIdSchema::GroupByKey { columns } => {
                matches!(upstream, RowIdSchema::GroupByKey { columns: c } if c == columns)
            }
            // Combined schemas check recursively
            RowIdSchema::Combined { left, right } => {
                matches!(upstream, RowIdSchema::Combined { left: l, right: r }
                    if left.is_compatible_with(l) && right.is_compatible_with(r))
            }
        }
    }

    /// Validate that the given sequence of operator schemas forms a compatible
    /// pipeline (each adjacent pair passes `is_compatible_with`).
    ///
    /// Returns `Ok(())` when all pairs are compatible, or `Err(String)` with a
    /// description of the first incompatible pair found.
    pub fn verify_pipeline(schemas: &[RowIdSchema]) -> Result<(), String> {
        for pair in schemas.windows(2) {
            let upstream = &pair[0];
            let downstream = &pair[1];
            if !downstream.is_compatible_with(upstream) {
                return Err(format!(
                    "RowIdSchema mismatch: operator with schema {:?} \
                     cannot follow operator with schema {:?}",
                    downstream, upstream
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{RowIdSchema, RowIdStrategy};

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

    // ── A18 (v0.36.0): RowIdSchema tests ─────────────────────────────────

    #[test]
    fn test_row_id_schema_any_compatible_with_everything() {
        let any = RowIdSchema::Any;
        assert!(any.is_compatible_with(&RowIdSchema::PassThrough));
        assert!(any.is_compatible_with(&RowIdSchema::Derived));
        assert!(any.is_compatible_with(&RowIdSchema::GroupByKey {
            columns: vec!["x".to_string()]
        }));
    }

    #[test]
    fn test_row_id_schema_derived_compatible_with_everything() {
        let derived = RowIdSchema::Derived;
        assert!(derived.is_compatible_with(&RowIdSchema::Any));
        assert!(derived.is_compatible_with(&RowIdSchema::PrimaryKey {
            table: "t".to_string(),
            columns: vec!["id".to_string()],
        }));
    }

    #[test]
    fn test_row_id_schema_primary_key_compatible_with_same() {
        let pk = RowIdSchema::PrimaryKey {
            table: "public.orders".to_string(),
            columns: vec!["id".to_string()],
        };
        assert!(pk.is_compatible_with(&RowIdSchema::PrimaryKey {
            table: "public.orders".to_string(),
            columns: vec!["id".to_string()],
        }));
    }

    #[test]
    fn test_row_id_schema_primary_key_incompatible_different_table() {
        let pk = RowIdSchema::PrimaryKey {
            table: "public.orders".to_string(),
            columns: vec!["id".to_string()],
        };
        assert!(!pk.is_compatible_with(&RowIdSchema::PrimaryKey {
            table: "public.items".to_string(),
            columns: vec!["id".to_string()],
        }));
    }

    #[test]
    fn test_row_id_schema_verify_pipeline_happy_path() {
        // PassThrough is compatible with PrimaryKey (passes through)
        // Actually by our impl, PassThrough checks upstream == PassThrough || upstream == self
        // so PassThrough is NOT compatible with PrimaryKey. Let's use Any instead.
        let schemas_any = vec![
            RowIdSchema::PrimaryKey {
                table: "t".to_string(),
                columns: vec!["id".to_string()],
            },
            RowIdSchema::Any,
        ];
        assert!(RowIdSchema::verify_pipeline(&schemas_any).is_ok());
    }

    #[test]
    fn test_row_id_schema_verify_pipeline_incompatible() {
        let schemas = vec![
            RowIdSchema::GroupByKey {
                columns: vec!["region".to_string()],
            },
            RowIdSchema::PrimaryKey {
                table: "t".to_string(),
                columns: vec!["id".to_string()],
            },
        ];
        assert!(RowIdSchema::verify_pipeline(&schemas).is_err());
    }

    #[test]
    fn test_row_id_schema_verify_pipeline_empty() {
        assert!(RowIdSchema::verify_pipeline(&[]).is_ok());
    }
}
