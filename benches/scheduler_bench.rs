//! TEST-7: Scheduler dispatch benchmark (500+ stream tables).
//!
//! Benchmarks the per-tick dispatch latency of the scheduler's HashMap-based
//! unit lookup (PERF-5). Creates a mock `ExecutionUnitDag` with 500+ stream
//! tables and measures the cost of:
//!   - `unit_by_id()` lookups (the critical path optimized by PERF-5)
//!   - `unit_for_pgt()` lookups (pgt_id → execution unit resolution)
//!   - Full dispatch sweep (iterating all units and looking up each by ID)
//!
//! Run with: `cargo bench --bench scheduler_bench`

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use pg_trickle::dag::{
    DagNode, ExecutionUnitDag, ExecutionUnitId, NodeId, RefreshMode, StDag, StStatus,
};
use std::time::Duration;

/// Number of stream tables in the benchmark DAG.
const NUM_STREAM_TABLES: i64 = 500;

/// Number of base tables (sources) in the benchmark DAG.
const NUM_BASE_TABLES: u32 = 50;

/// Build a mock StDag with `NUM_STREAM_TABLES` stream tables, each depending
/// on one of `NUM_BASE_TABLES` base tables (round-robin assignment).
fn build_mock_st_dag() -> StDag {
    let mut dag = StDag::new();

    for pgt_id in 1..=NUM_STREAM_TABLES {
        let node = DagNode {
            id: NodeId::StreamTable(pgt_id),
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: format!("st_{pgt_id}"),
            status: StStatus::Active,
            schedule_raw: Some("1m".to_string()),
        };
        dag.add_st_node(node);

        // Assign each ST to a base table (round-robin)
        let base_oid = ((pgt_id as u32 - 1) % NUM_BASE_TABLES) + 16384;
        dag.add_edge(NodeId::BaseTable(base_oid), NodeId::StreamTable(pgt_id));
    }

    dag
}

/// Build an ExecutionUnitDag from the mock StDag.
fn build_mock_eu_dag() -> ExecutionUnitDag {
    let st_dag = build_mock_st_dag();
    ExecutionUnitDag::build_from_st_dag(&st_dag, |_pgt_id| Some(RefreshMode::Differential))
}

/// Collect all unit IDs for iteration benchmarks.
fn collect_unit_ids(eu_dag: &ExecutionUnitDag) -> Vec<ExecutionUnitId> {
    eu_dag.units().map(|u| u.id).collect()
}

fn bench_unit_by_id(c: &mut Criterion) {
    let eu_dag = build_mock_eu_dag();
    let unit_ids = collect_unit_ids(&eu_dag);

    let mut group = c.benchmark_group("scheduler_dispatch");
    group.measurement_time(Duration::from_secs(5));

    // Benchmark: single unit_by_id lookup
    group.bench_function(BenchmarkId::new("unit_by_id", NUM_STREAM_TABLES), |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let uid = unit_ids[idx % unit_ids.len()];
            idx += 1;
            black_box(eu_dag.unit_by_id(uid))
        })
    });

    // Benchmark: unit_for_pgt lookup (pgt_id → unit)
    group.bench_function(BenchmarkId::new("unit_for_pgt", NUM_STREAM_TABLES), |b| {
        let mut pgt_id = 1i64;
        b.iter(|| {
            let id = ((pgt_id - 1) % NUM_STREAM_TABLES) + 1;
            pgt_id += 1;
            black_box(eu_dag.unit_for_pgt(id))
        })
    });

    // Benchmark: full dispatch sweep (all units looked up by ID)
    group.bench_function(BenchmarkId::new("full_sweep", NUM_STREAM_TABLES), |b| {
        b.iter(|| {
            for &uid in &unit_ids {
                black_box(eu_dag.unit_by_id(uid));
            }
        })
    });

    group.finish();
}

fn bench_dag_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("dag_construction");
    group.measurement_time(Duration::from_secs(10));

    // Benchmark: building the ExecutionUnitDag from StDag
    group.bench_function(BenchmarkId::new("build_eu_dag", NUM_STREAM_TABLES), |b| {
        let st_dag = build_mock_st_dag();
        b.iter(|| {
            black_box(ExecutionUnitDag::build_from_st_dag(&st_dag, |_| {
                Some(RefreshMode::Differential)
            }))
        })
    });

    group.finish();
}

criterion_group!(benches, bench_unit_by_id, bench_dag_construction);
criterion_main!(benches);
