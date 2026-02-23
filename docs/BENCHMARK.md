# pg_stream — Benchmark Guide

This document explains how the database-level refresh benchmarks work and how to interpret their output.

---

## Overview

The benchmark suite in `tests/e2e_bench_tests.rs` measures **wall-clock refresh time** for **FULL** vs **INCREMENTAL** mode across a matrix of table sizes, change rates, and query complexities. Each benchmark spawns an isolated PostgreSQL 18.x container via Testcontainers, ensuring reproducible and interference-free measurements.

The core question the benchmarks answer:

> **How much faster is an INCREMENTAL refresh compared to a FULL refresh, given a specific workload?**

---

## Prerequisites

Build the E2E test Docker image before running any benchmarks:

```bash
./tests/build_e2e_image.sh
```

Docker must be running on the host.

---

## Running Benchmarks

All benchmark tests are tagged `#[ignore]` so they are skipped during normal CI. The `--nocapture` flag is required to see the printed output tables.

### Quick Spot Checks (~5–10 seconds each)

```bash
# Simple scan, 10K rows, 1% change rate
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_scan_10k_1pct

# Aggregate query, 100K rows, 1% change rate
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_aggregate_100k_1pct

# Join + aggregate, 100K rows, 10% change rate
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_join_agg_100k_10pct
```

### Zero-Change Latency (~5 seconds)

```bash
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_no_data_refresh_latency
```

### Full Matrix (~15–30 minutes)

Runs all 30 combinations and prints a consolidated summary:

```bash
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_full_matrix
```

### Run All Benchmarks in Parallel

```bash
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture
```

Note: each test starts its own container, so parallel execution requires sufficient Docker resources.

---

## Benchmark Dimensions

### Table Sizes

| Size | Rows | Purpose |
|------|------|---------|
| Small | 10,000 | Fast iteration; measures per-row overhead |
| Medium | 100,000 | More realistic; reveals scaling characteristics |

### Change Rates

| Rate | Description |
|------|-------------|
| 1% | Low churn — the sweet spot for incremental refresh |
| 10% | Moderate churn — tests delta query scalability |
| 50% | High churn — stress test; approaches full-refresh cost |

### Query Complexities

| Scenario | Defining Query | Operators Tested |
|----------|---------------|------------------|
| **scan** | `SELECT id, region, category, amount, score FROM src` | Table scan only |
| **filter** | `SELECT id, region, amount FROM src WHERE amount > 5000` | Scan + filter (WHERE) |
| **aggregate** | `SELECT region, SUM(amount), COUNT(*) FROM src GROUP BY region` | Scan + group-by aggregate |
| **join** | `SELECT s.id, s.region, s.amount, d.region_name FROM src s JOIN dim d ON ...` | Scan + inner join |
| **join_agg** | `SELECT d.region_name, SUM(s.amount), COUNT(*) FROM src s JOIN dim d ON ... GROUP BY ...` | Scan + join + aggregate |

### DML Mix per Cycle

Each change cycle applies a realistic mix of operations:

| Operation | Fraction | Example at 10K rows, 10% rate |
|-----------|----------|-------------------------------|
| UPDATE | 70% | 700 rows have `amount` incremented |
| DELETE | 15% | 150 rows removed |
| INSERT | 15% | 150 new rows added |

---

## What Each Benchmark Does

```
1. Start a fresh PostgreSQL 18.x container
2. Install the pg_stream extension
3. Create and populate the source table (10K or 100K rows)
4. Create dimension table if needed (for join scenarios)
5. ANALYZE for stable query plans

── FULL mode ──
6. Create a Stream Table in FULL refresh mode
7. For each of 3 cycles:
   a. Apply random DML (updates + deletes + inserts)
   b. ANALYZE
   c. Time the FULL refresh (TRUNCATE + re-execute entire query)
   d. Record refresh_ms and ST row count
8. Drop the FULL-mode ST

── INCREMENTAL mode ──
9. Reset source table to same starting state
10. Create a Stream Table in INCREMENTAL refresh mode
11. For each of 3 cycles:
    a. Apply random DML (same parameters)
    b. ANALYZE
    c. Time the INCREMENTAL refresh (delta query + MERGE)
    d. Record refresh_ms and ST row count

12. Print results table and summary
```

Both modes start from the same data to ensure a fair comparison. The 3-cycle design captures warm-up effects (cycle 1 may be slower due to plan caching).

---

## Reading the Output

### Detail Table

```
╔══════════════════════════════════════════════════════════════════════════════════════╗
║                    pg_stream Refresh Benchmark Results                      ║
╠════════════╤══════════╤════════╤═════════════╤═══════╤════════════╤═════════════════╣
║ Scenario   │ Rows     │ Chg %  │ Mode        │ Cycle │ Refresh ms │ ST Rows         ║
╠════════════╪══════════╪════════╪═════════════╪═══════╪════════════╪═════════════════╣
║ aggregate  │    10000 │     1% │ FULL        │     1 │       22.1 │               5 ║
║ aggregate  │    10000 │     1% │ FULL        │     2 │        4.8 │               5 ║
║ aggregate  │    10000 │     1% │ FULL        │     3 │        5.3 │               5 ║
║ aggregate  │    10000 │     1% │ INCREMENTAL │     1 │        8.4 │               5 ║
║ aggregate  │    10000 │     1% │ INCREMENTAL │     2 │        4.4 │               5 ║
║ aggregate  │    10000 │     1% │ INCREMENTAL │     3 │        4.6 │               5 ║
╚════════════╧══════════╧════════╧═════════════╧═══════╧════════════╧═════════════════╝
```

| Column | Meaning |
|--------|---------|
| **Scenario** | Query complexity level (scan, filter, aggregate, join, join_agg) |
| **Rows** | Number of rows in the base table |
| **Chg %** | Percentage of rows changed per cycle |
| **Mode** | FULL (truncate + recompute) or INCREMENTAL (delta + merge) |
| **Cycle** | Which of the 3 measurement rounds (cycle 1 often includes warm-up) |
| **Refresh ms** | Wall-clock time for the refresh operation |
| **ST Rows** | Row count in the Stream Table after refresh (sanity check) |

### Summary Table

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        Summary (avg ms per cycle)                       │
├────────────┬──────────┬────────┬─────────────────┬──────────────────────┤
│ Scenario   │ Rows     │ Chg %  │ FULL avg ms     │ INCREMENTAL avg ms   │
├────────────┼──────────┼────────┼─────────────────┼──────────────────────┤
│ aggregate  │    10000 │     1% │       10.7       │        5.8 (  1.8x) │
└────────────┴──────────┴────────┴─────────────────┴──────────────────────┘
```

The **Speedup** value in parentheses is `FULL avg / INCREMENTAL avg` — how many times faster the incremental refresh is compared to a full refresh.

---

## Interpreting the Speedup

### What to Expect

| Change Rate | Table Size | Expected Speedup | Explanation |
|-------------|------------|-------------------|-------------|
| 1% | 10K | 1.5–5x | Small table; overhead is similar, delta is tiny |
| 1% | 100K | 5–50x | Larger table amplifies full-refresh cost |
| 10% | 100K | 2–10x | Moderate delta; still significantly faster |
| 50% | any | 1–2x | Delta is nearly as large as full table |

### Rules of Thumb

| Speedup | Interpretation |
|---------|---------------|
| **> 10x** | Strong win for INCREMENTAL — typical at low change rates on larger tables |
| **5–10x** | Clear advantage for INCREMENTAL |
| **2–5x** | Moderate advantage — INCREMENTAL is the right choice |
| **1–2x** | Marginal gain — either mode is acceptable |
| **~1x** | Break-even — change rate is too high for incremental to help |
| **< 1x** | INCREMENTAL is slower — would indicate overhead exceeds savings (investigate) |

### Key Patterns to Look For

1. **Scaling with table size**: For the same change rate, speedup should increase with table size. FULL must re-process all rows; INCREMENTAL processes only the delta.

2. **Degradation with change rate**: As change rate rises from 1% → 50%, speedup should decrease. At 50%, INCREMENTAL processes half the table which approaches FULL cost.

3. **Query complexity amplifies speedup**: Aggregate and join queries benefit more from INCREMENTAL because they avoid expensive re-computation. A join_agg at 1% changes should show higher speedup than a simple scan at the same parameters.

4. **Cycle 1 warm-up**: The first cycle in each mode may be slower due to PostgreSQL plan cache population. Use cycles 2–3 for the steadiest numbers.

5. **ST Rows consistency**: The ST row count should be similar between FULL and INCREMENTAL for the same scenario (accounting for random DML). Large discrepancies indicate a correctness issue.

---

## Zero-Change Latency

The `bench_no_data_refresh_latency` test measures the overhead of a refresh when **no data has changed** — the NO_DATA code path.

```
┌──────────────────────────────────────────────┐
│ NO_DATA Refresh Latency (10 iterations)      │
├──────────────────────────────────────────────┤
│ Avg:     3.21 ms                             │
│ Max:     5.10 ms                             │
│ Target: < 10 ms                              │
│ Status: ✅ PASS                              │
└──────────────────────────────────────────────┘
```

| Metric | Meaning |
|--------|---------|
| **Avg** | Average wall-clock time across 10 no-op refreshes |
| **Max** | Worst-case single iteration |
| **Target** | The [PLAN.md](../plans/PLAN.md) goal: < 10 ms per no-op refresh |
| **Status** | PASS if avg < 10 ms, SLOW otherwise |

A passing result confirms the scheduler's per-cycle overhead is negligible. Values > 10 ms in containerized environments may be acceptable due to Docker overhead; bare-metal PostgreSQL should comfortably meet the target.

---

## Available Tests

### Individual Tests (10K rows)

| Test Name | Scenario | Change Rate |
|-----------|----------|-------------|
| `bench_scan_10k_1pct` | scan | 1% |
| `bench_scan_10k_10pct` | scan | 10% |
| `bench_scan_10k_50pct` | scan | 50% |
| `bench_filter_10k_1pct` | filter | 1% |
| `bench_aggregate_10k_1pct` | aggregate | 1% |
| `bench_join_10k_1pct` | join | 1% |
| `bench_join_agg_10k_1pct` | join_agg | 1% |

### Individual Tests (100K rows)

| Test Name | Scenario | Change Rate |
|-----------|----------|-------------|
| `bench_scan_100k_1pct` | scan | 1% |
| `bench_scan_100k_10pct` | scan | 10% |
| `bench_scan_100k_50pct` | scan | 50% |
| `bench_aggregate_100k_1pct` | aggregate | 1% |
| `bench_aggregate_100k_10pct` | aggregate | 10% |
| `bench_join_agg_100k_1pct` | join_agg | 1% |
| `bench_join_agg_100k_10pct` | join_agg | 10% |

### Special Tests

| Test Name | Description |
|-----------|-------------|
| `bench_full_matrix` | All 30 combinations (5 queries × 2 sizes × 3 rates) |
| `bench_no_data_refresh_latency` | Zero-change overhead (10 iterations) |

---

## In-Process Micro-Benchmarks (Criterion.rs)

In addition to the E2E database benchmarks, the project includes two **Criterion.rs** benchmark suites that measure pure Rust computation time without database overhead. These are useful for tracking performance regressions in the internal query-building and IVM differentiation logic.

### Benchmark Suites

#### `refresh_bench` — Utility Functions

`benches/refresh_bench.rs` benchmarks the low-level helper functions used during refresh operations:

| Benchmark Group | What It Measures |
|----------------|------------------|
| **quote_ident** | PostgreSQL identifier quoting speed |
| **col_list** | Column list SQL generation |
| **prefixed_col_list** | Prefixed column list generation (e.g., `NEW.col`) |
| **expr_to_sql** | AST expression → SQL string conversion |
| **output_columns** | Output column extraction from parsed queries |
| **source_oids** | Source table OID resolution |
| **lsn_gt** | LSN comparison expression generation |
| **frontier_json** | Frontier state JSON serialization |
| **canonical_period** | Interval parsing and canonicalization |
| **dag_operations** | DAG topological sort and cycle detection |
| **xxh64** | xxHash-64 hashing throughput |

#### `diff_operators` — IVM Operator Differentiation

`benches/diff_operators.rs` benchmarks the delta SQL generation for every IVM operator. Each benchmark creates a realistic operator tree and measures `differentiate()` throughput:

| Benchmark Group | What It Measures |
|----------------|------------------|
| **diff_scan** | Table scan differentiation (3, 10, 20 columns) |
| **diff_filter** | Filter (WHERE) differentiation |
| **diff_project** | Projection (SELECT subset) differentiation |
| **diff_aggregate** | GROUP BY aggregate differentiation (simple + complex) |
| **diff_inner_join** | Inner join differentiation |
| **diff_left_join** | Left outer join differentiation |
| **diff_distinct** | DISTINCT differentiation |
| **diff_union_all** | UNION ALL differentiation (2, 5, 10 children) |
| **diff_window** | Window function differentiation |
| **diff_join_aggregate** | Composite join + aggregate pipeline |
| **differentiate_full** | Full `differentiate()` call for scan-only and filter+scan trees |

### Running Micro-Benchmarks

```bash
# Run all Criterion benchmarks
just bench

# Run only refresh utility benchmarks
cargo bench --bench refresh_bench --features pg18

# Run only IVM diff operator benchmarks
just bench-diff
# or equivalently:
cargo bench --bench diff_operators --features pg18

# Output in Bencher-compatible format (for CI integration)
just bench-bencher
```

### Output and Reports

Criterion produces statistical analysis for each benchmark including:

- **Mean** and **standard deviation** of execution time
- **Throughput** (iterations/sec)
- **Comparison with previous run** — reports improvements/regressions with confidence intervals

HTML reports are generated in `target/criterion/` with interactive charts showing distributions and regression history. Open `target/criterion/report/index.html` to browse all results.

Sample output:

```
diff_scan/3_columns   time:   [11.834 µs 12.074 µs 12.329 µs]
diff_scan/10_columns  time:   [16.203 µs 16.525 µs 16.869 µs]
diff_aggregate/simple time:   [21.447 µs 21.862 µs 22.301 µs]
diff_inner_join       time:   [25.919 µs 26.421 µs 26.952 µs]
```

---

## Continuous Benchmarking with Bencher

[Bencher](https://bencher.dev) provides continuous benchmark tracking in CI, detecting performance regressions on pull requests before they merge.

### How It Works

The `.github/workflows/benchmarks.yml` workflow:

1. **On `main` pushes** — runs both Criterion suites and uploads results to Bencher as the baseline. This establishes the expected performance for each benchmark.

2. **On pull requests** — runs the same benchmarks and compares against the `main` baseline using a **Student's t-test** with a 99% upper confidence boundary. If any benchmark regresses beyond the threshold, the PR check fails.

### Setup

To enable Bencher for your fork or deployment:

1. **Create a Bencher account** at [bencher.dev](https://bencher.dev) and create a project.

2. **Add the API token** as a GitHub Actions secret:
   - Go to **Settings → Secrets and variables → Actions**
   - Add `BENCHER_API_TOKEN` with your Bencher API token

3. **Update the project slug** in `.github/workflows/benchmarks.yml` if your Bencher project name differs from `pg-stream`.

The workflow gracefully degrades — if `BENCHER_API_TOKEN` is not set, benchmarks still run and upload artifacts but skip Bencher tracking.

### Local Bencher-Format Output

To see what Bencher would receive from CI:

```bash
just bench-bencher
```

This runs both suites with `--output-format bencher`, producing JSON output compatible with `bencher run`.

### Dashboard

Once configured, the Bencher dashboard shows:

- **Historical trends** for every benchmark across commits
- **Statistical thresholds** with configurable alerting
- **PR annotations** highlighting which benchmarks regressed and by how much

---

## Troubleshooting

| Issue | Resolution |
|-------|-----------|
| `docker: command not found` | Install Docker Desktop and ensure it is running |
| Container startup timeout | Increase Docker memory allocation (≥ 4 GB recommended) |
| `image not found` | Run `./tests/build_e2e_image.sh` to build the test image |
| Highly variable timings | Close other workloads; use `--test-threads=1` to avoid container contention |
| SLOW status on latency test | Expected in Docker; bare-metal should pass < 10 ms |
