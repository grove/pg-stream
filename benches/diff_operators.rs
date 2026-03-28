//! Benchmarks for DVM operator differentiation (delta SQL generation).
//!
//! These measure the speed of transforming OpTree nodes into delta SQL CTEs.
//! All operations are pure Rust — no database required.
//!
//! Run with: `cargo bench --bench diff_operators`

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use pg_trickle::dvm::diff::DiffContext;
use pg_trickle::dvm::parser::{AggExpr, AggFunc, Column, Expr, OpTree, SortExpr, WindowExpr};
use pg_trickle::version::Frontier;
use std::time::Duration;

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
    group.sample_size(200);
    group.measurement_time(Duration::from_secs(10));

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

// ── TPC-H OpTree composite benchmarks ─────────────────────────────────────
//
// These represent the operator shapes of five TPC-H-derived queries.
// No database is required — they exercise pure-Rust delta SQL generation.
//
// Q01: Scan → Filter → Aggregate (6 agg functions, 2 group-by keys)
// Q05: 6-table InnerJoin chain → Filter → Aggregate
// Q08: 7-table InnerJoin chain → Aggregate with CASE (modelled as Project)
// Q18: InnerJoin(orders, lineitem) with SemiJoin inner subquery
// Q21: AntiJoin + SemiJoin correlated subqueries (supplier with waiters)

/// Q01 shape: single-table filter + multi-aggregate.
///
/// `SELECT l_returnflag, l_linestatus,
///         SUM(qty), SUM(ext_price), SUM(disc_price), SUM(charge),
///         AVG(qty), AVG(ext_price), COUNT(*)
///  FROM lineitem WHERE shipdate <= '...' GROUP BY l_returnflag, l_linestatus`
fn bench_diff_tpch_q01(c: &mut Criterion) {
    let lineitem = make_scan(
        "l",
        16384,
        &[
            "l_orderkey",
            "l_partkey",
            "l_suppkey",
            "l_linenumber",
            "l_quantity",
            "l_extendedprice",
            "l_discount",
            "l_tax",
            "l_returnflag",
            "l_linestatus",
            "l_shipdate",
            "l_commitdate",
            "l_receiptdate",
        ],
    );
    let filter = OpTree::Filter {
        predicate: Expr::BinaryOp {
            op: "<=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_shipdate".to_string(),
            }),
            right: Box::new(Expr::Literal("'1998-09-02'".to_string())),
        },
        child: Box::new(lineitem),
    };
    let agg = OpTree::Aggregate {
        group_by: vec![
            Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_returnflag".to_string(),
            },
            Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_linestatus".to_string(),
            },
        ],
        aggregates: vec![
            AggExpr {
                function: AggFunc::Sum,
                argument: Some(Expr::ColumnRef {
                    table_alias: Some("l".to_string()),
                    column_name: "l_quantity".to_string(),
                }),
                alias: "sum_qty".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::Sum,
                argument: Some(Expr::ColumnRef {
                    table_alias: Some("l".to_string()),
                    column_name: "l_extendedprice".to_string(),
                }),
                alias: "sum_base_price".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::Sum,
                argument: Some(Expr::BinaryOp {
                    op: "*".to_string(),
                    left: Box::new(Expr::ColumnRef {
                        table_alias: Some("l".to_string()),
                        column_name: "l_extendedprice".to_string(),
                    }),
                    right: Box::new(Expr::BinaryOp {
                        op: "-".to_string(),
                        left: Box::new(Expr::Literal("1".to_string())),
                        right: Box::new(Expr::ColumnRef {
                            table_alias: Some("l".to_string()),
                            column_name: "l_discount".to_string(),
                        }),
                    }),
                }),
                alias: "sum_disc_price".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::Avg,
                argument: Some(Expr::ColumnRef {
                    table_alias: Some("l".to_string()),
                    column_name: "l_quantity".to_string(),
                }),
                alias: "avg_qty".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::Avg,
                argument: Some(Expr::ColumnRef {
                    table_alias: Some("l".to_string()),
                    column_name: "l_extendedprice".to_string(),
                }),
                alias: "avg_price".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::CountStar,
                argument: None,
                alias: "count_order".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
        ],
        child: Box::new(filter),
    };

    c.bench_function("diff_tpch_q01", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.differentiate(black_box(&agg)).unwrap()
        });
    });
}

/// Q05 shape: 6-table inner-join chain → filter → aggregate.
///
/// Joins region → nation → customer → orders → lineitem → supplier,
/// filters by region name and date range, groups by nation name.
fn bench_diff_tpch_q05(c: &mut Criterion) {
    // Build bottom-up: lineitem ⋈ orders ⋈ customer ⋈ nation ⋈ region
    // plus supplier ⋈ nation join for the supplier side.
    let region = make_scan("r", 16391, &["r_regionkey", "r_name"]);
    let nation1 = make_scan("n", 16390, &["n_nationkey", "n_name", "n_regionkey"]);
    let customer = make_scan("c", 16385, &["c_custkey", "c_nationkey"]);
    let orders = make_scan("o", 16386, &["o_orderkey", "o_custkey", "o_orderdate"]);
    let lineitem = make_scan(
        "l",
        16387,
        &["l_orderkey", "l_suppkey", "l_extendedprice", "l_discount"],
    );
    let supplier = make_scan("s", 16388, &["s_suppkey", "s_nationkey"]);

    let j1 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("r".to_string()),
                column_name: "r_regionkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("n".to_string()),
                column_name: "n_regionkey".to_string(),
            }),
        },
        left: Box::new(region),
        right: Box::new(nation1),
    };
    let j2 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("n".to_string()),
                column_name: "n_nationkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "c_nationkey".to_string(),
            }),
        },
        left: Box::new(j1),
        right: Box::new(customer),
    };
    let j3 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "c_custkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_custkey".to_string(),
            }),
        },
        left: Box::new(j2),
        right: Box::new(orders),
    };
    let j4 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
        },
        left: Box::new(j3),
        right: Box::new(lineitem),
    };
    let j5 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_suppkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("s".to_string()),
                column_name: "s_suppkey".to_string(),
            }),
        },
        left: Box::new(j4),
        right: Box::new(supplier),
    };
    let agg = OpTree::Aggregate {
        group_by: vec![Expr::ColumnRef {
            table_alias: Some("n".to_string()),
            column_name: "n_name".to_string(),
        }],
        aggregates: vec![AggExpr {
            function: AggFunc::Sum,
            argument: Some(Expr::BinaryOp {
                op: "*".to_string(),
                left: Box::new(Expr::ColumnRef {
                    table_alias: Some("l".to_string()),
                    column_name: "l_extendedprice".to_string(),
                }),
                right: Box::new(Expr::BinaryOp {
                    op: "-".to_string(),
                    left: Box::new(Expr::Literal("1".to_string())),
                    right: Box::new(Expr::ColumnRef {
                        table_alias: Some("l".to_string()),
                        column_name: "l_discount".to_string(),
                    }),
                }),
            }),
            alias: "revenue".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        }],
        child: Box::new(j5),
    };

    c.bench_function("diff_tpch_q05", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.differentiate(black_box(&agg)).unwrap()
        });
    });
}

/// Q08 shape: 7-table inner-join chain → aggregate (CASE WHEN modelled as Project).
///
/// part ⋈ lineitem ⋈ supplier ⋈ orders ⋈ customer ⋈ nation1 ⋈ nation2 ⋈ region
fn bench_diff_tpch_q08(c: &mut Criterion) {
    let part = make_scan("p", 16392, &["p_partkey", "p_type"]);
    let lineitem = make_scan(
        "l",
        16387,
        &[
            "l_partkey",
            "l_suppkey",
            "l_orderkey",
            "l_extendedprice",
            "l_discount",
        ],
    );
    let supplier = make_scan("s", 16388, &["s_suppkey", "s_nationkey"]);
    let orders = make_scan("o", 16386, &["o_orderkey", "o_custkey", "o_orderdate"]);
    let customer = make_scan("c", 16385, &["c_custkey", "c_nationkey"]);
    let nation1 = make_scan("n1", 16390, &["n_nationkey", "n_regionkey"]);
    let nation2 = make_scan("n2", 16393, &["n_nationkey", "n_name"]);
    let region = make_scan("r", 16391, &["r_regionkey", "r_name"]);

    let j1 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("p".to_string()),
                column_name: "p_partkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_partkey".to_string(),
            }),
        },
        left: Box::new(part),
        right: Box::new(lineitem),
    };
    let j2 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_suppkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("s".to_string()),
                column_name: "s_suppkey".to_string(),
            }),
        },
        left: Box::new(j1),
        right: Box::new(supplier),
    };
    let j3 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderkey".to_string(),
            }),
        },
        left: Box::new(j2),
        right: Box::new(orders),
    };
    let j4 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_custkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "c_custkey".to_string(),
            }),
        },
        left: Box::new(j3),
        right: Box::new(customer),
    };
    let j5 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "c_nationkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("n1".to_string()),
                column_name: "n_nationkey".to_string(),
            }),
        },
        left: Box::new(j4),
        right: Box::new(nation1),
    };
    let j6 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("s".to_string()),
                column_name: "s_nationkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("n2".to_string()),
                column_name: "n_nationkey".to_string(),
            }),
        },
        left: Box::new(j5),
        right: Box::new(nation2),
    };
    let j7 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("n1".to_string()),
                column_name: "n_regionkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("r".to_string()),
                column_name: "r_regionkey".to_string(),
            }),
        },
        left: Box::new(j6),
        right: Box::new(region),
    };
    // Aggregate with two SUM expressions (modelling CASE WHEN as a literal sum).
    let agg = OpTree::Aggregate {
        group_by: vec![Expr::FuncCall {
            func_name: "extract".to_string(),
            args: vec![Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderdate".to_string(),
            }],
        }],
        aggregates: vec![
            AggExpr {
                function: AggFunc::Sum,
                argument: Some(Expr::ColumnRef {
                    table_alias: Some("l".to_string()),
                    column_name: "l_extendedprice".to_string(),
                }),
                alias: "mkt_share".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
            AggExpr {
                function: AggFunc::Sum,
                argument: Some(Expr::ColumnRef {
                    table_alias: Some("l".to_string()),
                    column_name: "l_discount".to_string(),
                }),
                alias: "volume".to_string(),
                is_distinct: false,
                filter: None,
                second_arg: None,
                order_within_group: None,
            },
        ],
        child: Box::new(j7),
    };

    c.bench_function("diff_tpch_q08", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.differentiate(black_box(&agg)).unwrap()
        });
    });
}

/// Q18 shape: 3-table inner join with SemiJoin (IN subquery) + aggregate.
///
/// `SELECT c, c_custkey, o_orderkey, o_orderdate, o_totalprice, SUM(l_quantity)
///  FROM customer JOIN orders ON ... JOIN lineitem ON ...
///  WHERE o_orderkey IN (SELECT l_orderkey FROM lineitem GROUP BY l_orderkey
///                       HAVING SUM(l_quantity) > 300)
///  GROUP BY c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice`
fn bench_diff_tpch_q18(c: &mut Criterion) {
    let customer = make_scan("c", 16385, &["c_custkey", "c_name"]);
    let orders = make_scan(
        "o",
        16386,
        &["o_orderkey", "o_custkey", "o_orderdate", "o_totalprice"],
    );
    let lineitem_outer = make_scan("l", 16387, &["l_orderkey", "l_quantity"]);
    let lineitem_inner = make_scan("li", 16387, &["l_orderkey", "l_quantity"]);

    // SemiJoin: orders WHERE o_orderkey IN (SELECT l_orderkey FROM lineitem
    //           GROUP BY l_orderkey HAVING SUM(l_quantity) > 300)
    let inner_agg = OpTree::Aggregate {
        group_by: vec![Expr::ColumnRef {
            table_alias: Some("li".to_string()),
            column_name: "l_orderkey".to_string(),
        }],
        aggregates: vec![AggExpr {
            function: AggFunc::Sum,
            argument: Some(Expr::ColumnRef {
                table_alias: Some("li".to_string()),
                column_name: "l_quantity".to_string(),
            }),
            alias: "sum_qty".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        }],
        child: Box::new(lineitem_inner),
    };

    // Join customer and orders first.
    let cust_ord = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "c_custkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_custkey".to_string(),
            }),
        },
        left: Box::new(customer),
        right: Box::new(orders),
    };

    // SemiJoin: keep only orders that appear in the inner aggregate.
    let semijoin = OpTree::SemiJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("li".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
        },
        left: Box::new(cust_ord),
        right: Box::new(inner_agg),
    };

    // Join with lineitem for the GROUP BY aggregate.
    let j_lineitem = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
        },
        left: Box::new(semijoin),
        right: Box::new(lineitem_outer),
    };

    let agg = OpTree::Aggregate {
        group_by: vec![
            Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "c_name".to_string(),
            },
            Expr::ColumnRef {
                table_alias: Some("c".to_string()),
                column_name: "c_custkey".to_string(),
            },
            Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderkey".to_string(),
            },
            Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderdate".to_string(),
            },
            Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_totalprice".to_string(),
            },
        ],
        aggregates: vec![AggExpr {
            function: AggFunc::Sum,
            argument: Some(Expr::ColumnRef {
                table_alias: Some("l".to_string()),
                column_name: "l_quantity".to_string(),
            }),
            alias: "sum_qty".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        }],
        child: Box::new(j_lineitem),
    };

    c.bench_function("diff_tpch_q18", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.differentiate(black_box(&agg)).unwrap()
        });
    });
}

/// Q21 shape: 4-table join with SemiJoin (EXISTS) + AntiJoin (NOT EXISTS).
///
/// `SELECT s_name, COUNT(*) FROM supplier JOIN lineitem l1 ON ... JOIN orders ON ...
///  WHERE EXISTS (SELECT 1 FROM lineitem l2 WHERE l2.l_orderkey = l1.l_orderkey
///                AND l2.l_suppkey <> l1.l_suppkey)
///    AND NOT EXISTS (SELECT 1 FROM lineitem l3 WHERE l3.l_orderkey = l1.l_orderkey
///                    AND l3.l_suppkey <> l1.l_suppkey
///                    AND l3.l_receiptdate > l3.l_commitdate)
///  GROUP BY s_name`
fn bench_diff_tpch_q21(c: &mut Criterion) {
    let supplier = make_scan("s", 16388, &["s_suppkey", "s_nationkey", "s_name"]);
    let lineitem1 = make_scan(
        "l1",
        16387,
        &["l_orderkey", "l_suppkey", "l_receiptdate", "l_commitdate"],
    );
    let orders = make_scan("o", 16386, &["o_orderkey", "o_orderstatus"]);
    let nation = make_scan("n", 16390, &["n_nationkey", "n_name"]);

    // l2 for EXISTS semi-join: other lineitems for same order, different supplier.
    let lineitem2 = make_scan("l2", 16387, &["l_orderkey", "l_suppkey"]);
    // l3 for NOT EXISTS anti-join: other lineitems late.
    let lineitem3 = make_scan(
        "l3",
        16387,
        &["l_orderkey", "l_suppkey", "l_receiptdate", "l_commitdate"],
    );

    // Core join: supplier ⋈ lineitem1 ⋈ orders ⋈ nation.
    let j1 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("s".to_string()),
                column_name: "s_suppkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("l1".to_string()),
                column_name: "l_suppkey".to_string(),
            }),
        },
        left: Box::new(supplier),
        right: Box::new(lineitem1),
    };
    let j2 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("l1".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("o".to_string()),
                column_name: "o_orderkey".to_string(),
            }),
        },
        left: Box::new(j1),
        right: Box::new(orders),
    };
    let j3 = OpTree::InnerJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("s".to_string()),
                column_name: "s_nationkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("n".to_string()),
                column_name: "n_nationkey".to_string(),
            }),
        },
        left: Box::new(j2),
        right: Box::new(nation),
    };

    // EXISTS (SELECT 1 FROM lineitem l2 WHERE l2.l_orderkey = l1.l_orderkey ...)
    let semijoin = OpTree::SemiJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("l1".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("l2".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
        },
        left: Box::new(j3),
        right: Box::new(lineitem2),
    };

    // NOT EXISTS (SELECT 1 FROM lineitem l3 WHERE ...)
    let antijoin = OpTree::AntiJoin {
        condition: Expr::BinaryOp {
            op: "=".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("l1".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("l3".to_string()),
                column_name: "l_orderkey".to_string(),
            }),
        },
        left: Box::new(semijoin),
        right: Box::new(lineitem3),
    };

    let agg = OpTree::Aggregate {
        group_by: vec![Expr::ColumnRef {
            table_alias: Some("s".to_string()),
            column_name: "s_name".to_string(),
        }],
        aggregates: vec![AggExpr {
            function: AggFunc::CountStar,
            argument: None,
            alias: "numwait".to_string(),
            is_distinct: false,
            filter: None,
            second_arg: None,
            order_within_group: None,
        }],
        child: Box::new(antijoin),
    };

    c.bench_function("diff_tpch_q21", |b| {
        b.iter(|| {
            let mut ctx = test_ctx();
            ctx.differentiate(black_box(&agg)).unwrap()
        });
    });
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
    bench_diff_tpch_q01,
    bench_diff_tpch_q05,
    bench_diff_tpch_q08,
    bench_diff_tpch_q18,
    bench_diff_tpch_q21,
);
criterion_main!(benches);
