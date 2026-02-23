//! Benchmarks for DVM operator differentiation (delta SQL generation).
//!
//! These measure the speed of transforming OpTree nodes into delta SQL CTEs.
//! All operations are pure Rust — no database required.
//!
//! Run with: `cargo bench --bench diff_operators`

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use pg_stream::dvm::diff::DiffContext;
use pg_stream::dvm::parser::{AggExpr, AggFunc, Column, Expr, OpTree, SortExpr, WindowExpr};
use pg_stream::version::Frontier;

// ── Helpers ────────────────────────────────────────────────────────────────

fn make_column(name: &str) -> Column {
    Column {
        name: name.to_string(),
        type_oid: 23,
        is_nullable: true,
    }
}

fn make_scan(alias: &str, oid: u32, cols: &[&str]) -> OpTree {
    OpTree::Scan {
        table_oid: oid,
        table_name: alias.to_string(),
        schema: "public".to_string(),
        columns: cols.iter().map(|c| make_column(c)).collect(),
        pk_columns: Vec::new(),
        alias: alias.to_string(),
    }
}

fn test_ctx() -> DiffContext {
    let mut prev = Frontier::new();
    prev.set_source(
        16384,
        "0/1000".to_string(),
        "2024-01-01T00:00:00Z".to_string(),
    );
    prev.set_source(
        16385,
        "0/1000".to_string(),
        "2024-01-01T00:00:00Z".to_string(),
    );

    let mut new = Frontier::new();
    new.set_source(
        16384,
        "0/2000".to_string(),
        "2024-01-01T01:00:00Z".to_string(),
    );
    new.set_source(
        16385,
        "0/2000".to_string(),
        "2024-01-01T01:00:00Z".to_string(),
    );

    DiffContext::new_standalone(prev, new)
}

// ── Scan operator ──────────────────────────────────────────────────────────

fn bench_diff_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff_scan");

    for ncols in [3, 10, 20] {
        let cols: Vec<String> = (0..ncols).map(|i| format!("col_{i}")).collect();
        let col_refs: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
        let scan = make_scan("orders", 16384, &col_refs);

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{ncols}cols")),
            &scan,
            |b, scan| {
                b.iter(|| {
                    let mut ctx = test_ctx();
                    ctx.diff_node(black_box(scan)).unwrap()
                });
            },
        );
    }
    group.finish();
}

// ── Filter operator ────────────────────────────────────────────────────────

fn bench_diff_filter(c: &mut Criterion) {
    let filter = OpTree::Filter {
        predicate: Expr::BinaryOp {
            op: ">".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("orders".to_string()),
                column_name: "amount".to_string(),
            }),
            right: Box::new(Expr::Literal("100".to_string())),
        },
        child: Box::new(make_scan("orders", 16384, &["id", "customer_id", "amount"])),
    };

    c.bench_function("diff_filter", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&filter)).unwrap()
        });
    });
}

// ── Project operator ───────────────────────────────────────────────────────

fn bench_diff_project(c: &mut Criterion) {
    let project = OpTree::Project {
        expressions: vec![
            Expr::ColumnRef {
                table_alias: Some("orders".to_string()),
                column_name: "id".to_string(),
            },
            Expr::BinaryOp {
                op: "*".to_string(),
                left: Box::new(Expr::ColumnRef {
                    table_alias: Some("orders".to_string()),
                    column_name: "price".to_string(),
                }),
                right: Box::new(Expr::ColumnRef {
                    table_alias: Some("orders".to_string()),
                    column_name: "qty".to_string(),
                }),
            },
        ],
        aliases: vec!["id".to_string(), "total".to_string()],
        child: Box::new(make_scan("orders", 16384, &["id", "price", "qty"])),
    };

    c.bench_function("diff_project", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&project)).unwrap()
        });
    });
}

// ── Aggregate operator ─────────────────────────────────────────────────────

fn bench_diff_aggregate(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff_aggregate");

    // Simple: COUNT(*) GROUP BY region
    let simple = OpTree::Aggregate {
        group_by: vec![Expr::ColumnRef {
            table_alias: None,
            column_name: "region".to_string(),
        }],
        aggregates: vec![AggExpr {
            function: AggFunc::CountStar,
            argument: None,
            alias: "cnt".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        }],
        child: Box::new(make_scan("orders", 16384, &["id", "region", "amount"])),
    };

    // Complex: SUM(amount) + COUNT(*) + AVG(amount) GROUP BY region, status
    let complex = OpTree::Aggregate {
        group_by: vec![
            Expr::ColumnRef {
                table_alias: None,
                column_name: "region".to_string(),
            },
            Expr::ColumnRef {
                table_alias: None,
                column_name: "status".to_string(),
            },
        ],
        aggregates: vec![
            AggExpr {
                function: AggFunc::Sum,
                argument: Some(Expr::ColumnRef {
                    table_alias: None,
                    column_name: "amount".to_string(),
                }),
                alias: "total".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::CountStar,
                argument: None,
                alias: "cnt".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::Avg,
                argument: Some(Expr::ColumnRef {
                    table_alias: None,
                    column_name: "amount".to_string(),
                }),
                alias: "avg_amount".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
        ],
        child: Box::new(make_scan(
            "orders",
            16384,
            &["id", "region", "status", "amount"],
        )),
    };

    group.bench_function("count_star_1group", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&simple)).unwrap()
        });
    });

    group.bench_function("sum_count_avg_2groups", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&complex)).unwrap()
        });
    });

    group.finish();
}

// ── Inner join operator ────────────────────────────────────────────────────

fn bench_diff_inner_join(c: &mut Criterion) {
    let join = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "customer_id".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "id".to_string(),
            }),
        },
        left: Box::new(make_scan("o", 16384, &["id", "customer_id", "amount"])),
        right: Box::new(make_scan("c", 16385, &["id", "name", "region"])),
    };

    c.bench_function("diff_inner_join_3x3col", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&join)).unwrap()
        });
    });
}

// ── Left join operator ─────────────────────────────────────────────────────

fn bench_diff_left_join(c: &mut Criterion) {
    let join = OpTree::LeftJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "customer_id".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "id".to_string(),
            }),
        },
        left: Box::new(make_scan("o", 16384, &["id", "customer_id", "amount"])),
        right: Box::new(make_scan("c", 16385, &["id", "name"])),
    };

    c.bench_function("diff_left_join_3x2col", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&join)).unwrap()
        });
    });
}

// ── Distinct operator ──────────────────────────────────────────────────────

fn bench_diff_distinct(c: &mut Criterion) {
    let distinct = OpTree::Distinct {
        child: Box::new(make_scan("orders", 16384, &["id", "customer_id", "region"])),
    };

    c.bench_function("diff_distinct", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&distinct)).unwrap()
        });
    });
}

// ── Union all operator ─────────────────────────────────────────────────────

fn bench_diff_union_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff_union_all");

    for n_children in [2, 5, 10] {
        let children: Vec<OpTree> = (0..n_children)
            .map(|i| make_scan(&format!("t{i}"), 16384 + i, &["id", "name", "value"]))
            .collect();

        let union_all = OpTree::UnionAll { children };

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{n_children}_children")),
            &union_all,
            |b, op| {
                b.iter(|| {
                    let mut ctx = test_ctx();
                    ctx.diff_node(black_box(op)).unwrap()
                });
            },
        );
    }
    group.finish();
}

// ── Window operator ────────────────────────────────────────────────────────

fn bench_diff_window(c: &mut Criterion) {
    let window = OpTree::Window {
        window_exprs: vec![WindowExpr {
            func_name: "row_number".to_string(),
            args: vec![],
            partition_by: vec![Expr::ColumnRef {
                table_alias: Some("orders".to_string()),
                column_name: "region".to_string(),
            }],
            order_by: vec![SortExpr {
                expr: Expr::ColumnRef {
                    table_alias: Some("orders".to_string()),
                    column_name: "amount".to_string(),
                },
                ascending: false,
                nulls_first: false,
            }],
            alias: "rn".to_string(),
            frame_clause: None,
        }],
        partition_by: vec![Expr::ColumnRef {
            table_alias: Some("orders".to_string()),
            column_name: "region".to_string(),
        }],
        pass_through: vec![
            (
                Expr::ColumnRef {
                    table_alias: Some("orders".to_string()),
                    column_name: "id".to_string(),
                },
                "id".to_string(),
            ),
            (
                Expr::ColumnRef {
                    table_alias: Some("orders".to_string()),
                    column_name: "amount".to_string(),
                },
                "amount".to_string(),
            ),
        ],
        child: Box::new(make_scan("orders", 16384, &["id", "region", "amount"])),
    };

    c.bench_function("diff_window_row_number", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.diff_node(black_box(&window)).unwrap()
        });
    });
}

// ── Composite: join + aggregate pipeline ───────────────────────────────────

fn bench_diff_composite(c: &mut Criterion) {
    // SELECT c.region, SUM(o.amount) FROM orders o JOIN customers c ON ... GROUP BY c.region
    let join = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "customer_id".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "id".to_string(),
            }),
        },
        left: Box::new(make_scan("o", 16384, &["id", "customer_id", "amount"])),
        right: Box::new(make_scan("c", 16385, &["id", "name", "region"])),
    };

    let agg = OpTree::Aggregate {
        group_by: vec![Expr::ColumnRef {
            table_alias: Some("c".to_string()),
            column_name: "region".to_string(),
        }],
        aggregates: vec![AggExpr {
            function: AggFunc::Sum,
            argument: Some(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "amount".to_string(),
            }),
            alias: "total".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        }],
        child: Box::new(join),
    };

    c.bench_function("diff_join_aggregate", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.differentiate(black_box(&agg)).unwrap()
        });
    });
}

// ── Full pipeline: differentiate → build_with_query ────────────────────────

fn bench_differentiate_full(c: &mut Criterion) {
    let mut group = c.benchmark_group("differentiate_full");

    // Simple scan
    let scan = make_scan("orders", 16384, &["id", "customer_id", "amount", "status"]);
    group.bench_function("scan_only", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.merge_safe_dedup = true;
            ctx.differentiate(black_box(&scan)).unwrap()
        });
    });

    // Filter + scan
    let filter = OpTree::Filter {
        predicate: Expr::BinaryOp {
            op: ">".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("orders".to_string()),
                column_name: "amount".to_string(),
            }),
            right: Box::new(Expr::Literal("0".to_string())),
        },
        child: Box::new(make_scan("orders", 16384, &["id", "customer_id", "amount"])),
    };
    group.bench_function("filter_scan", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.differentiate(black_box(&filter)).unwrap()
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_diff_scan,
    bench_diff_filter,
    bench_diff_project,
    bench_diff_aggregate,
    bench_diff_inner_join,
    bench_diff_left_join,
    bench_diff_distinct,
    bench_diff_union_all,
    bench_diff_window,
    bench_diff_composite,
    bench_differentiate_full,
);
criterion_main!(benches);
