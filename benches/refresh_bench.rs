//! Benchmark scaffolding for pgstream.
//!
//! These benchmarks measure the performance of pure in-process functions.
//! Database-level benchmarks (refresh duration, delta query execution)
//! require a live PostgreSQL instance and are documented in PLAN.md Phase 11.4.
//!
//! Run with: `cargo bench` (requires pgrx pg18 feature, so may need
//! `cargo bench --no-default-features --features pg18` or just unit benchmarks).

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use pg_stream::dag::{DagNode, NodeId, StDag, StStatus};
use pg_stream::dvm::diff::{col_list, prefixed_col_list, quote_ident};
use pg_stream::dvm::parser::{AggExpr, AggFunc, Column, Expr, OpTree};
use pg_stream::version::{Frontier, lsn_gt, select_canonical_period_secs};
use std::time::Duration;

// ── quote_ident benchmark ──────────────────────────────────────────────────

fn bench_quote_ident(c: &mut Criterion) {
    let names = [
        "simple",
        "with spaces",
        "has\"quotes",
        "a_very_long_column_name_that_is_common_in_real_schemas",
    ];

    let mut group = c.benchmark_group("quote_ident");
    for name in &names {
        group.bench_with_input(BenchmarkId::from_parameter(name), name, |b, name| {
            b.iter(|| quote_ident(black_box(name)));
        });
    }
    group.finish();
}

// ── col_list benchmark ─────────────────────────────────────────────────────

fn bench_col_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("col_list");
    for size in [1, 5, 10, 20, 50] {
        let cols: Vec<String> = (0..size).map(|i| format!("column_{i}")).collect();
        group.bench_with_input(BenchmarkId::from_parameter(size), &cols, |b, cols| {
            b.iter(|| col_list(black_box(cols)));
        });
    }
    group.finish();
}

// ── prefixed_col_list benchmark ────────────────────────────────────────────

fn bench_prefixed_col_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("prefixed_col_list");
    for size in [1, 5, 10, 20] {
        let cols: Vec<String> = (0..size).map(|i| format!("col_{i}")).collect();
        group.bench_with_input(BenchmarkId::from_parameter(size), &cols, |b, cols| {
            b.iter(|| prefixed_col_list(black_box("src"), black_box(cols)));
        });
    }
    group.finish();
}

// ── Expr::to_sql benchmark ─────────────────────────────────────────────────

fn bench_expr_to_sql(c: &mut Criterion) {
    let simple_col = Expr::ColumnRef {
        table_alias: None,
        column_name: "amount".to_string(),
    };

    let nested = Expr::BinaryOp {
        op: "+".to_string(),
        left: Box::new(Expr::BinaryOp {
            op: "*".to_string(),
            left: Box::new(Expr::ColumnRef {
                table_alias: Some("t".to_string()),
                column_name: "price".to_string(),
            }),
            right: Box::new(Expr::ColumnRef {
                table_alias: Some("t".to_string()),
                column_name: "qty".to_string(),
            }),
        }),
        right: Box::new(Expr::Literal("100".to_string())),
    };

    let func_call = Expr::FuncCall {
        func_name: "coalesce".to_string(),
        args: vec![
            Expr::ColumnRef {
                table_alias: None,
                column_name: "x".to_string(),
            },
            Expr::Literal("0".to_string()),
        ],
    };

    let mut group = c.benchmark_group("expr_to_sql");
    group.bench_function("simple_column", |b| {
        b.iter(|| black_box(&simple_col).to_sql());
    });
    group.bench_function("nested_binary_op", |b| {
        b.iter(|| black_box(&nested).to_sql());
    });
    group.bench_function("func_call", |b| {
        b.iter(|| black_box(&func_call).to_sql());
    });
    group.finish();
}

// ── OpTree::output_columns benchmark ───────────────────────────────────────

fn make_scan(alias: &str, oid: u32, ncols: usize) -> OpTree {
    OpTree::Scan {
        table_oid: oid,
        table_name: alias.to_string(),
        schema: "public".to_string(),
        columns: (0..ncols)
            .map(|i| Column {
                name: format!("col_{i}"),
                type_oid: 23,
                is_nullable: true,
            })
            .collect(),
        pk_columns: Vec::new(),
        alias: alias.to_string(),
    }
}

fn bench_output_columns(c: &mut Criterion) {
    let scan = make_scan("t", 1, 20);

    let join = OpTree::InnerJoin {
        condition: Expr::ColumnRef {
            table_alias: None,
            column_name: "id".to_string(),
        },
        left: Box::new(make_scan("a", 1, 10)),
        right: Box::new(make_scan("b", 2, 10)),
    };

    let aggregate = OpTree::Aggregate {
        group_by: vec![Expr::ColumnRef {
            table_alias: None,
            column_name: "region".to_string(),
        }],
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
        ],
        child: Box::new(make_scan("t", 1, 5)),
    };

    let mut group = c.benchmark_group("output_columns");
    group.bench_function("scan_20cols", |b| {
        b.iter(|| black_box(&scan).output_columns());
    });
    group.bench_function("join_20cols", |b| {
        b.iter(|| black_box(&join).output_columns());
    });
    group.bench_function("aggregate", |b| {
        b.iter(|| black_box(&aggregate).output_columns());
    });
    group.finish();
}

// ── OpTree::source_oids benchmark ──────────────────────────────────────────

fn bench_source_oids(c: &mut Criterion) {
    // Deep tree: filter → join → scan+scan
    let deep = OpTree::Filter {
        predicate: Expr::Literal("true".to_string()),
        child: Box::new(OpTree::InnerJoin {
            condition: Expr::Literal("true".to_string()),
            left: Box::new(OpTree::Filter {
                predicate: Expr::Literal("true".to_string()),
                child: Box::new(make_scan("a", 1, 5)),
            }),
            right: Box::new(OpTree::Distinct {
                child: Box::new(make_scan("b", 2, 5)),
            }),
        }),
    };

    // Wide tree: union all with 10 children
    let wide = OpTree::UnionAll {
        children: (1..=10)
            .map(|i| make_scan(&format!("t{i}"), i, 3))
            .collect(),
    };

    let mut group = c.benchmark_group("source_oids");
    group.bench_function("deep_tree", |b| {
        b.iter(|| black_box(&deep).source_oids());
    });
    group.bench_function("wide_union_10", |b| {
        b.iter(|| black_box(&wide).source_oids());
    });
    group.finish();
}

// ── LSN comparison benchmark ───────────────────────────────────────────────

fn bench_lsn_comparison(c: &mut Criterion) {
    let pairs = [
        ("0/0", "0/1"),
        ("0/FFFFFFFF", "1/0"),
        ("FF/DEADBEEF", "FF/DEADBEF0"),
        ("0/0", "0/0"),
    ];

    let mut group = c.benchmark_group("lsn_gt");
    for (a, b) in &pairs {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{a}_vs_{b}")),
            &(a, b),
            |bench, (a, b)| {
                bench.iter(|| lsn_gt(black_box(a), black_box(b)));
            },
        );
    }
    group.finish();
}

// ── Frontier JSON serialization benchmark ──────────────────────────────────

fn bench_frontier_json(c: &mut Criterion) {
    let mut group = c.benchmark_group("frontier_json");

    for n_sources in [1, 5, 10, 20] {
        let mut f = Frontier::new();
        for i in 0..n_sources {
            f.set_source(
                i as u32 + 1000,
                format!("0/{:X}", i * 1000),
                "2024-01-01T00:00:00Z".to_string(),
            );
        }
        f.set_data_timestamp("2024-06-15T12:00:00Z".to_string());

        let json = f.to_json().unwrap();

        group.bench_with_input(
            BenchmarkId::new("serialize", n_sources),
            &f,
            |b, frontier| {
                b.iter(|| black_box(frontier).to_json().unwrap());
            },
        );

        group.bench_with_input(
            BenchmarkId::new("deserialize", n_sources),
            &json,
            |b, json_str| {
                b.iter(|| Frontier::from_json(black_box(json_str)).unwrap());
            },
        );
    }
    group.finish();
}

// ── Canonical period selection benchmark ───────────────────────────────────

fn bench_canonical_period(c: &mut Criterion) {
    c.bench_function("canonical_period_60s", |b| {
        b.iter(|| select_canonical_period_secs(black_box(60)));
    });
    c.bench_function("canonical_period_3600s", |b| {
        b.iter(|| select_canonical_period_secs(black_box(3600)));
    });
    c.bench_function("canonical_period_86400s", |b| {
        b.iter(|| select_canonical_period_secs(black_box(86400)));
    });
}

// ── DAG operations benchmark ───────────────────────────────────────────────

fn bench_dag_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("dag");

    for n_nodes in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("build_linear_chain", n_nodes),
            &n_nodes,
            |b, &n| {
                b.iter(|| {
                    let mut dag = StDag::new();
                    for id in 1..=n as i64 {
                        dag.add_st_node(DagNode {
                            id: NodeId::StreamTable(id),
                            schedule: Some(Duration::from_secs(60)),
                            effective_schedule: Duration::from_secs(60),
                            name: format!("st_{id}"),
                            status: StStatus::Active,
                            schedule_raw: None,
                        });
                    }
                    for id in 1..n as i64 {
                        dag.add_edge(NodeId::StreamTable(id), NodeId::StreamTable(id + 1));
                    }
                    dag.detect_cycles().unwrap();
                    dag.topological_order().unwrap()
                });
            },
        );
    }
    group.finish();
}

// ── XXH64 hash benchmark ──────────────────────────────────────────────────

fn bench_xxh64(c: &mut Criterion) {
    use xxhash_rust::xxh64;
    let seed = 0x517cc1b727220a95u64;

    let mut group = c.benchmark_group("xxh64");
    for size in [16, 64, 256, 1024, 4096] {
        let data = vec![b'x'; size];
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{size}B")),
            &data,
            |b, data| {
                b.iter(|| xxh64::xxh64(black_box(data), seed));
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_quote_ident,
    bench_col_list,
    bench_prefixed_col_list,
    bench_expr_to_sql,
    bench_output_columns,
    bench_source_oids,
    bench_lsn_comparison,
    bench_frontier_json,
    bench_canonical_period,
    bench_dag_operations,
    bench_xxh64,
);
criterion_main!(benches);
