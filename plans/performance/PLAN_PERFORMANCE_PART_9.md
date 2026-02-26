# PLAN_PERFORMANCE_PART_9.md — Strategic Performance Roadmap

**Date:** 2026-02-26
**Status:** Planning
**Baseline:** PERFORMANCE_PART_8.md results (2026-02-22)
**Scope:** Higher-level performance strategy, architectural improvements,
and benchmark infrastructure enhancements.

---

## Table of Contents

1. [Current State Assessment](#1-current-state-assessment)
2. [Benchmark Results (2026-02-26 Criterion Run)](#2-benchmark-results-2026-02-26-criterion-run)
3. [Higher-Level Performance Decisions](#3-higher-level-performance-decisions)
4. [Phase A: Remaining Regressions & Quick Wins](#4-phase-a-remaining-regressions--quick-wins)
5. [Phase B: CDC Architecture — Statement-Level Triggers](#5-phase-b-cdc-architecture--statement-level-triggers)
6. [Phase C: Parallel Refresh Implementation](#6-phase-c-parallel-refresh-implementation)
7. [Phase D: MERGE Alternatives & Planner Control](#7-phase-d-merge-alternatives--planner-control)
8. [Phase E: Memory and I/O Budget Management](#8-phase-e-memory-and-io-budget-management)
9. [Benchmark Infrastructure Improvements](#9-benchmark-infrastructure-improvements)
10. [Implementation Priority & Schedule](#10-implementation-priority--schedule)

---

## 1. Current State Assessment

### 1.1 Where We Are

After 8 rounds of optimization, the core pipeline overhead (Decision +
Gen+Build) is **under 1ms** for all scenarios. Delta SQL generation runs in
**10–55 µs** (Criterion benchmarks). The system is fast at what it controls.

**The bottleneck is now entirely PostgreSQL MERGE execution** — 70–97% of
refresh time. Further Rust-side optimization yields diminishing returns.

### 1.2 Key Metrics (Part 8 Baseline)

| Scenario | 100K/1% INCR ms | Speedup vs FULL | Status |
|----------|:---------------:|:---------------:|--------|
| scan | 4.6 | 65.9x | Excellent |
| filter | 3.4 | 39.5x | Excellent |
| aggregate | 2.5 | 6.8x | Good |
| join | 18.0 | 16.3x | Regressed from Part 6 (12.3ms) |
| join_agg | 14.6 | 1.9x | Regressed from Part 6 (8.5ms) |

### 1.3 Problem Areas

| Issue | Impact | Root Cause |
|-------|--------|-----------|
| Join regression (+46%) | 18ms vs 12.3ms at 100K/1% | Prepared statement revert; 11-CTE join delta requires re-planning every cycle |
| join_agg regression (+72%) | 14.6ms vs 8.5ms at 100K/1% | Same as join + aggregate overhead compounding |
| join_agg 100K/10% = 0.3x FULL | INCR slower than FULL | Adaptive threshold not triggering correctly |
| scan 100K/50% = 0.7x FULL | INCR slower than FULL | MERGE overhead > TRUNCATE+INSERT at high change rates |
| No-data latency 9.73ms | Borderline target (<10ms) | Additional SPI calls vs Part 6 |
| P95 spikes up to 7.6x median | Unpredictable latency | PostgreSQL plan cache invalidation |
| diff_operators bench broken | Cannot run locally | pgrx symbols require PG library linking |

---

## 2. Benchmark Results (2026-02-26 Criterion Run)

### refresh_bench (Pure Rust, no DB)

These benchmarks ran successfully on macOS. Key results:

| Benchmark | Median | Change vs Previous | Notes |
|-----------|--------|-------------------|-------|
| quote_ident/simple | 93.9 ns | +5.6% | Minor regression — likely measurement noise |
| col_list/10 | 1.37 µs | +5.8% | Minor regression |
| col_list/50 | 6.94 µs | -10.9% | Improved |
| prefixed_col_list/5 | 883.8 ns | -5.6% | Improved |
| prefixed_col_list/20 | 4.00 µs | +34.2% | **Significant regression** — investigate |
| expr_to_sql/simple_column | 26.3 ns | No change | Stable |
| expr_to_sql/func_call | 211.4 ns | +12.7% | Moderate regression |
| lsn_gt/0/0_vs_0/1 | 78.9 ns | +21.9% | **Regression** — string comparison overhead |
| frontier_json/serialize/1 | 117.4 ns | +5.3% | Minor |
| frontier_json/serialize/20 | 3.76 µs | -10.2% | Improved |
| dag/build_linear_chain/10 | 6.52 µs | +4.5% | Minor |
| dag/build_linear_chain/100 | 62.5 µs | No change | Stable |
| xxh64/16B | 4.79 ns | +8.2% | Minor |
| xxh64/4096B | 384.1 ns | +11.1% | Moderate regression |
| canonical_period_60s | 666 ps | No change | Trivial |

### diff_operators (Requires pgrx/PG linking)

**Could not run locally.** The `diff_operators` benchmark links against
`pg_stream` library code that references pgrx symbols (`SPI_processed`,
etc.). These symbols are only available inside a PostgreSQL backend process
or when linked against libpq/PG server libraries.

Previous results (from Part 8, likely run inside Docker or with PG installed):

| Benchmark | Time (µs) |
|-----------|-----------|
| diff_scan/3cols | 9.9 |
| diff_scan/20cols | 47.7 |
| diff_filter | 11.3 |
| diff_aggregate/sum_count_avg | 21.7 |
| diff_inner_join | 30.7 |
| diff_left_join | 26.8 |
| diff_union_all/10_children | 105.9 |
| diff_window_row_number | 17.1 |
| diff_join_aggregate | 54.2 |

### Assessment

1. **Rust-side operations are sub-microsecond to low-microsecond** — not a
   bottleneck. Even `diff_join_aggregate` at 54µs is negligible compared to
   the 14.6ms MERGE execution.

2. The `prefixed_col_list/20` regression (+34%) and `lsn_gt` regression
   (+22%) warrant investigation — these are called frequently during delta
   SQL generation and frontier comparison.

3. The high outlier rates (10–19% of measurements) across many benchmarks
   suggest **system noise** — the benchmarks should be run with higher
   sample counts or on a quieter system for reliable comparisons.

---

## 3. Higher-Level Performance Decisions

These are architectural and strategic decisions that fundamentally affect
pg_stream's performance ceiling. Each is more impactful than any individual
code optimization.

### D1: Where Does Time Actually Go? (The 80/20 Rule)

The Part 8 per-phase breakdown reveals:

| Phase | Time at 100K/1% | % of Total |
|-------|:----------------:|:----------:|
| Decision (SPI: change count, threshold) | 0.5–1.5ms | 5–15% |
| Gen+Build (Rust: delta SQL) | 0.05–0.14ms | <1% |
| **MERGE (PG: execute SQL)** | **1–168ms** | **70–97%** |
| Cleanup (SPI: change buffer) | 0.1–7.7ms | 3–15% |

**Decision:** Future optimization effort should focus on (a) making the MERGE
SQL faster or (b) avoiding it entirely, not on making the Rust pipeline faster.

### D2: When to Give Up on Incremental

At high change rates (10–50%), INCREMENTAL refresh can be **slower** than FULL.
The current adaptive threshold mechanism (auto_threshold) should detect this
and switch, but Part 8 identified that it wasn't triggering correctly for
several scenarios.

**Decision:** The adaptive fallback is the single most important performance
feature. Priorities:

1. **Verify the Part 8 fix** (A-3: `last_full_ms` initialization) actually
   resolves the join_agg 100K/10% = 0.3x regression
2. **Lower the default auto_threshold** from 0.15 to 0.10 — this triggers
   FULL fallback earlier when costs are close
3. **Track and expose the threshold** (per SQL_GAPS_7 §F27) so operators can
   debug strategy decisions

### D3: Row-Level vs Statement-Level CDC Triggers

The current row-level AFTER trigger fires once per affected row, inserting
into the change buffer each time. For bulk DML (e.g., `UPDATE src SET x = x+1
WHERE region = 'north'` hitting 20K rows), this means 20K individual trigger
invocations + 20K change buffer INSERTs + 20K index updates + 20K WAL records.

PostgreSQL supports **statement-level triggers with transition tables** (PG 10+):

```sql
CREATE TRIGGER cdc_stmt AFTER INSERT OR UPDATE OR DELETE ON src
REFERENCING NEW TABLE AS new_rows OLD TABLE AS old_rows
FOR EACH STATEMENT EXECUTE FUNCTION pg_stream_cdc_stmt_fn();
```

**Impact:** A single trigger invocation per statement, processing all affected
rows in one batch `INSERT INTO changes_<oid> SELECT ... FROM new_rows`. This
eliminates per-row trigger overhead (PL/pgSQL entry/exit, WAL per trigger,
sequence increment per row).

**Expected improvement:** 50–80% reduction in write-side overhead for bulk DML.
No change for single-row DML (which already fires once).

**Trade-offs:**
- Statement-level triggers can't access per-row `TG_OP` — all rows in a
  transition table share the same operation kind
- Need to handle the case where both `new_rows` and `old_rows` are present
  (UPDATE) vs only one (INSERT/DELETE)
- TRUNCATE has no transition tables — continue using existing statement-level
  TRUNCATE trigger

**Decision:** This is the highest-ROI write-side optimization. Plan in Phase B.

### D4: Parallel Refresh Dispatch

Per REPORT_PARALLELIZATION.md, the scheduler processes STs sequentially even
though independent DAG branches could run in parallel. The GUC
`pg_stream.max_concurrent_refreshes` exists but is not enforced for parallel
dispatch.

**Impact:** With 50 STs averaging 200ms each, sequential processing takes 10s.
With 4 parallel workers and a balanced DAG, this drops to ~2.5s.

**Decision:** Implement level-parallel DAG dispatch (Option A+B from the
parallelization report). This is a high-value feature for multi-ST deployments
and makes the existing GUC operational.

### D5: UNLOGGED Change Buffers

Change buffer writes generate WAL for durability. If the extension can always
recover from a crash by re-running a full refresh (which it already does on
crash detection), the change buffer doesn't need WAL protection.

**Impact:** ~30% reduction in per-row trigger overhead (eliminates WAL writes
for trigger INSERTs). At 5µs/row trigger overhead, saving 1.5µs/row is
significant at scale.

**Trade-offs:**
- Change buffer data lost on crash — requires full refresh to recover
- Already the default recovery mode (crash → reinitialize)
- Cloud PG providers may not support UNLOGGED tables on replicas

**Decision:** Implement behind a GUC (`pg_stream.change_buffer_unlogged`,
default false). Benchmark with the trigger overhead suite first.

### D6: Column Pruning in Change Buffers

The current trigger captures ALL source table columns in the change buffer.
If the stream table's defining query only references 3 of 20 source columns,
the trigger still writes all 20 `new_*` and 20 `old_*` columns.

**Impact:** For wide tables (20+ columns), trigger overhead scales linearly
with column count (estimated from TRIGGERS_OVERHEAD.md: 3x overhead for 20
cols vs 3 cols for UPDATE). Pruning to only referenced columns could reduce
trigger cost by 50–80% for wide tables.

**Trade-offs:**
- Requires knowing which columns the stream table uses at trigger creation time
  (available from `pgs_dependencies.columns_used`)
- Adding a new stream table referencing additional columns would require
  trigger regeneration
- Columns used by a defining query may include join keys, filter predicates,
  and GROUP BY keys — not just SELECT columns

**Decision:** High-value for wide tables. Implement after statement-level
triggers (D3) since both modify the CDC architecture.

### D7: Prepared Statement Strategy

Part 8 Session 5 implemented `PREPARE`/`EXECUTE` for MERGE statements.
PostgreSQL switches from custom → generic plan after ~5 executions. This
saves 1–2ms parse/plan time per refresh on cache hits.

The Part 8 analysis noted that the prepared statement revert (undone in
P1+P2) is the root cause of the join regression. The join delta query has
11 CTEs — planning this from scratch every cycle is expensive.

**Current state:** `pg_stream.use_prepared_statements` GUC exists (default
true) and is implemented. Need to verify it's working correctly and that the
join regression recovers when enabled.

**Decision:** Verify prepared statements are active in the benchmark harness.
If the join regression persists with prepared statements enabled, the issue
is elsewhere.

### D8: PostgreSQL Parallel Query for Large Refreshes

Per REPORT_PARALLELIZATION.md Option E, PostgreSQL's parallel query engine
can parallelize individual delta SQL / MERGE statements. This is free — it
requires zero code changes, only correct GUC settings.

**Decision:** Verify with `EXPLAIN ANALYZE` that parallel plans are being
chosen for 100K-row refreshes. Document recommended PG settings for
pg_stream deployments:

```ini
max_parallel_workers_per_gather = 2  -- or 4 for large tables
min_parallel_table_scan_size = 1MB   -- lower threshold for stream tables
```

---

## 4. Phase A: Remaining Regressions & Quick Wins

### A-1: Verify Adaptive Threshold Fix (Part 8 A-3)

**Status:** Part 8 fixed `last_full_ms` initialization. Need to verify with
a benchmark re-run.

**Test:** Run `bench_join_agg_100k_10pct`. If INCR > FULL for 3+ consecutive
cycles, the adaptive threshold should switch to FULL by cycle 4–5.

**Expected outcome:** join_agg 100K/10% should show speedup > 1.0x (currently
0.3x).

**Effort:** 30 min (benchmark run + verification).

### A-2: Verify Prepared Statements Recover Join Regression

**Test:** Run `bench_join_100k_1pct` with `pg_stream.use_prepared_statements
= true` (default). Compare cycle 1 (cold, custom plan) vs cycles 6+
(generic plan locked in).

**Expected outcome:** Cycles 6+ should show join 100K/1% < 12ms, recovering
to Part 6 levels.

**Effort:** 30 min.

### A-3: Investigate prefixed_col_list/20 Regression (+34%)

The Criterion benchmark shows `prefixed_col_list(prefix, &cols)` with 20
columns regressed from ~3.0µs to ~4.0µs. While not impactful at absolute
scale, this function is called per column per scan in delta SQL generation.

**Action:** Profile `prefixed_col_list` for unnecessary allocations. Consider
pre-allocating the output string with capacity.

**Effort:** 1 hour.

### A-4: Investigate lsn_gt Regression (+22%)

`lsn_gt("0/0", "0/1")` regressed from ~65ns to ~79ns. This is called during
frontier comparison and change buffer queries.

**Action:** Check if the `lsn_gt` implementation uses string parsing. Consider
caching parsed LSN values as `(u32, u32)` pairs.

**Effort:** 1 hour.

---

## 5. Phase B: CDC Architecture — Statement-Level Triggers

This is the highest-ROI write-side optimization identified across all
performance plans.

### B-1: Statement-Level Trigger Implementation

Replace the per-row AFTER trigger with a statement-level trigger using
transition tables:

```sql
CREATE TRIGGER pg_stream_cdc_tr_{oid}
AFTER INSERT OR UPDATE OR DELETE ON {schema}.{table}
REFERENCING NEW TABLE AS __pgs_new OLD TABLE AS __pgs_old
FOR EACH STATEMENT
EXECUTE FUNCTION pgstream_changes.pg_stream_cdc_stmt_fn_{oid}();
```

The trigger function:

```sql
-- For INSERT:
INSERT INTO pgstream_changes.changes_{oid} (lsn, action, pk_hash, new_*)
SELECT pg_current_wal_lsn(), 'I',
       pg_stream.pg_stream_hash(n."pk"::text), n.*
FROM __pgs_new n;

-- For UPDATE:
INSERT INTO pgstream_changes.changes_{oid} (lsn, action, pk_hash, new_*, old_*)
SELECT pg_current_wal_lsn(), 'U',
       pg_stream.pg_stream_hash(n."pk"::text), n.*, o.*
FROM __pgs_new n JOIN __pgs_old o ON n.pk = o.pk;

-- For DELETE:
INSERT INTO pgstream_changes.changes_{oid} (lsn, action, pk_hash, old_*)
SELECT pg_current_wal_lsn(), 'D',
       pg_stream.pg_stream_hash(o."pk"::text), o.*
FROM __pgs_old o;
```

### B-2: Backward Compatibility

- Keep the row-level trigger as a fallback behind a GUC
  (`pg_stream.cdc_trigger_mode = 'statement'|'row'`, default `'statement'`)
- The row-level trigger handles edge cases: `COPY FROM` with
  `-trigger` options, row-level trigger ordering requirements
- Migration: on ALTER EXTENSION UPDATE, replace row-level triggers with
  statement-level triggers for existing STs

### B-3: Benchmark Write-Side Improvement

Use the TRIGGERS_OVERHEAD.md benchmark design to measure:
- Baseline vs row-level vs statement-level trigger overhead
- Column count scaling: narrow (3), medium (8), wide (20) tables
- Bulk vs single-row DML

**Expected outcome:** 50–80% overhead reduction for bulk DML; neutral for
single-row DML.

**Effort:** 12–16 hours (implementation + testing + benchmarking).

---

## 6. Phase C: Parallel Refresh Implementation

Per REPORT_PARALLELIZATION.md, implementing Option A+B.

### C-1: DAG Level Extraction

Modify `StDag::topological_order()` to return `Vec<Vec<NodeId>>` (levels)
instead of `Vec<NodeId>`. Trivial modification to Kahn's algorithm.

**Effort:** 2–4 hours.

### C-2: Dynamic Background Worker Dispatch

For each level, spawn up to `max_concurrent_refreshes` dynamic background
workers. Each worker:
1. Receives `pgs_id` via datum
2. Acquires advisory lock (or `FOR UPDATE SKIP LOCKED` per SQL_GAPS_7 Q7)
3. Executes `refresh_stream_table()`
4. Writes result to `pgs_refresh_history`
5. Exits

The coordinator waits for all workers in a level to complete before
advancing to the next level.

**Effort:** 12–16 hours.

### C-3: Result Communication

Workers write refresh outcomes to `pgs_refresh_history` (already done by
`refresh_stream_table`). The coordinator reads these after worker completion
to update retry state and scheduling decisions.

**Effort:** 3–4 hours.

---

## 7. Phase D: MERGE Alternatives & Planner Control

### D-1: Hash-Based Change Detection for Wide Tables

Per SQL_GAPS_7 §G4.6, the MERGE `WHEN MATCHED` arm uses per-column
`IS DISTINCT FROM` chains. For 100+ columns, this causes plan cache bloat.

**Fix:** For tables with > 50 columns, replace the per-column chain with a
single hash comparison:

```sql
WHEN MATCHED AND pg_stream.pg_stream_hash(
    row_to_json(st.*)::text
) IS DISTINCT FROM pg_stream.pg_stream_hash(
    row_to_json(d.*)::text
) THEN UPDATE SET ...
```

**Effort:** 4–6 hours.

### D-2: Conditional FULL Bypass for Aggregates

When all groups in an aggregate query are affected (common at high change
rates with few groups), FULL refresh is always cheaper. Detect this condition
before executing the delta:

```
if affected_groups_estimate == total_groups:
    use FULL refresh
```

The affected groups can be estimated from the change buffer:
`SELECT COUNT(DISTINCT group_key) FROM changes WHERE ...`

If this equals the number of groups in the stream table, skip INCR.

**Effort:** 3–4 hours.

### D-3: COST-Based Strategy Selection

Currently, the strategy decision uses a simple ratio (`change_count /
table_size > threshold`). A more sophisticated approach would estimate the
actual cost:

```
estimated_incr_cost = delta_rows × (cte_count × scan_cost + merge_cost)
estimated_full_cost = table_size × scan_cost + truncate_cost
```

Use the historical `pgs_refresh_history` data to calibrate the cost model
per stream table.

**Effort:** 6–8 hours.

---

## 8. Phase E: Memory and I/O Budget Management

### E-1: Delta Size Estimation Before Execution

Before executing the MERGE, estimate the delta result set size:

```sql
SELECT COUNT(*) FROM (delta_query LIMIT 10001) t
```

If the estimate exceeds a threshold, abort INCR and fall back to FULL.
This prevents OOM for unexpected large deltas.

**Effort:** 2–3 hours.

### E-2: work_mem Scaling

The current `pg_stream.merge_work_mem_mb` GUC sets a fixed work_mem for
large deltas. A smarter approach: scale work_mem proportional to estimated
delta size:

```
work_mem = max(base_work_mem, delta_rows * avg_row_width / 1024)
```

Capped at `pg_stream.merge_work_mem_max_mb`.

**Effort:** 2 hours.

### E-3: Change Buffer Partitioning for High-Write Tables

For source tables with > 100K writes/minute, the change buffer table
becomes a hotspot. Partition by `lsn` range (monthly/weekly) to allow
fast TRUNCATE of old partitions.

**Effort:** 8–12 hours. Defer until trigger overhead benchmarks confirm
this is a bottleneck.

---

## 9. Benchmark Infrastructure Improvements

### Current Gaps

The existing benchmark infrastructure has several gaps that limit insight
into system performance:

### I-1: diff_operators Benchmark Cannot Run Locally

**Problem:** The `diff_operators` Criterion bench links against `pg_stream`
library code that uses pgrx symbols (`SPI_processed`, `pg_sys::*`). On
macOS without PostgreSQL server libraries installed, the dyld loader fails
with `symbol not found in flat namespace '_SPI_processed'`.

**Fix:** Refactor `diff_operators.rs` to depend only on pure-Rust types
(`OpTree`, `DiffContext`, `Expr`, `Column`, `Frontier`). The diff operators
themselves are pure Rust — the issue is that the benchmark binary links
against the full `pg_stream` crate which includes SPI-using modules.

**Options:**
| Option | Description | Effort |
|--------|-------------|--------|
| **(a) Feature-gate pgrx** | Add a `bench` feature that stubs out pgrx dependencies | 2–3 hours |
| **(b) Extract pure Rust into sub-crate** | Move `dvm/`, `dag.rs`, `version.rs`, `hash.rs` to a `pg_stream_core` crate that doesn't depend on pgrx | 6–8 hours |
| **(c) Run inside Docker** | Run `cargo bench` inside the E2E Docker container where PG libs are available | 1 hour (justfile target) |

**Recommended:** **(c) as quick fix**, **(b) as long-term solution.**
Option (c) gets the benchmarks running immediately. Option (b) is the
correct architectural fix — pure-Rust logic should not require a PostgreSQL
installation to benchmark.

### I-2: No Latency Distribution / Histogram Output

**Problem:** The E2E benchmarks report avg, median, P95, and cycle-1 vs
cycles-2+ breakdowns. But there's no histogram or full distribution output.
P95 spikes (up to 7.6x median) are noted but not diagnosed.

**Fix:** Add per-cycle timing output as a parseable CSV-like format:

```
[BENCH_CYCLE] scenario=join rows=100000 pct=0.01 cycle=1 mode=INCR ms=44.3
[BENCH_CYCLE] scenario=join rows=100000 pct=0.01 cycle=2 mode=INCR ms=7.1
...
```

This enables external analysis (histogram plots, outlier identification,
trend detection) without changing the test code significantly.

**Effort:** 2 hours.

### I-3: No EXPLAIN ANALYZE Capture for Delta Queries

**Problem:** We know MERGE dominates (70–97% of time) but don't know what
PostgreSQL is doing inside the MERGE. Is it using hash joins? Nested loops?
Seq scans? Parallel workers? Are sorts spilling to disk?

**Fix:** Add a benchmark mode that captures `EXPLAIN (ANALYZE, BUFFERS,
FORMAT JSON)` for the delta query. Store the plan in
`/tmp/bench_plans/<scenario>_<size>_<pct>.json` for offline analysis.

**Implementation:** Add a GUC or environment variable
`PGS_BENCH_EXPLAIN=true` that, when set, runs each MERGE as `EXPLAIN
ANALYZE` on the first measured cycle.

**Effort:** 3–4 hours.

### I-4: No Cross-Run Comparison Tool

**Problem:** Each benchmark run produces printed output. Comparing results
across runs (e.g., before/after an optimization) requires manual
side-by-side reading, as manually done in PERFORMANCE_PART_8.md.

**Fix:** Write benchmark results to a JSON file
(`target/bench_results/<timestamp>.json`). Provide a comparison script:

```bash
just bench-compare target/bench_results/2026-02-22.json target/bench_results/2026-02-26.json
```

Output: per-scenario deltas with color-coded improvement/regression.

**Effort:** 4–6 hours.

### I-5: No Concurrent Writer Benchmarks

**Problem:** All benchmarks use a single connection for DML. In production,
multiple concurrent writers hit source tables simultaneously. This stress-
tests trigger overhead differently (BIGSERIAL contention, change buffer
index lock contention, WAL write serialization).

**Fix:** Extend the trigger overhead benchmark (TRIGGERS_OVERHEAD.md) to
sweep writer concurrency: 1, 2, 4, 8 connections performing DML
simultaneously.

**Effort:** 4–6 hours (requires pgbench-style parallel harness or
tokio::spawn tasks with separate sqlx connections).

### I-6: No Table Size Scaling Benchmark (1M+ Rows)

**Problem:** The current matrix tests 10K and 100K rows. Production tables
commonly have 1M–100M rows. At 1M rows / 1% change rate (10K changes), the
delta is larger than anything currently benchmarked, and the MERGE touches
more stream table rows for the `WHEN NOT MATCHED BY SOURCE` arm.

**Fix:** Add a 1M-row tier to the benchmark matrix. Run as an optional
`bench_large_matrix` test (separate from the standard suite due to longer
run time).

**Effort:** 2 hours (increase TABLE_SIZES; may need container memory bump).

### I-7: No Window / Lateral / CTE Operator Benchmarks

**Problem:** The E2E benchmarks test only 5 scenarios (scan, filter,
aggregate, join, join_agg). Window functions, lateral subqueries, CTEs,
set operations, and their combinations are not benchmarked.

**Fix:** Add scenarios:
- `window`: `SELECT id, region, ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount DESC) FROM src`
- `lateral`: `SELECT s.*, l.top_score FROM src s, LATERAL (SELECT MAX(score) as top_score FROM src s2 WHERE s2.region = s.region) l`
- `cte`: `WITH regional AS (SELECT region, SUM(amount) as total FROM src GROUP BY region) SELECT s.*, r.total FROM src s JOIN regional r ON s.region = r.region`
- `union_all`: `SELECT id, amount, 'high' as tier FROM src WHERE amount > 5000 UNION ALL SELECT id, amount, 'low' FROM src WHERE amount <= 5000`

**Effort:** 4–6 hours.

### I-8: Criterion Benchmark Noise Reduction

**Problem:** The Criterion benchmarks show 10–19% outlier rates across many
tests, suggesting system noise (background processes, thermal throttling,
memory pressure). This makes it difficult to detect real regressions.

**Fix:**
- Increase `sample_size` from default 100 to 200 for critical benchmarks
- Add `measurement_time(Duration::from_secs(10))` for fast benchmarks to
  get more iterations
- Document that benchmarks should be run on a quiet system with
  `nice -n -20` (or macOS equivalent)
- Consider adding a `bench-stable` justfile target that sets CPU frequency
  governor (Linux) or reduces measurement noise

**Effort:** 1–2 hours.

---

## 10. Implementation Priority & Schedule

### Session 1: Verify & Fix Regressions (3–4 hours)

| Step | Task | Effort |
|------|------|--------|
| A-1 | Verify adaptive threshold fix via E2E benchmark | 30 min |
| A-2 | Verify prepared statements recover join regression | 30 min |
| A-3 | Fix `prefixed_col_list/20` regression | 1 hour |
| A-4 | Investigate `lsn_gt` regression | 1 hour |

### Session 2: Benchmark Infrastructure (8–12 hours)

| Step | Task | Effort |
|------|------|--------|
| I-1c | Run diff_operators in Docker (quick fix) | 1 hour |
| I-2 | Add per-cycle CSV output to E2E benchmarks | 2 hours |
| I-3 | Add EXPLAIN ANALYZE capture mode | 3–4 hours |
| I-6 | Add 1M-row benchmark tier | 2 hours |
| I-8 | Reduce Criterion noise | 1 hour |

### Session 3: Statement-Level Triggers (12–16 hours)

| Step | Task | Effort |
|------|------|--------|
| B-1 | Implement statement-level trigger generation | 8 hours |
| B-2 | GUC + backward compatibility + migration | 4 hours |
| B-3 | Benchmark write-side improvement | 2 hours |

### Session 4: Parallel Refresh (16–24 hours)

| Step | Task | Effort |
|------|------|--------|
| C-1 | DAG level extraction | 2–4 hours |
| C-2 | Dynamic background worker dispatch | 12–16 hours |
| C-3 | Result communication + error handling | 3–4 hours |

### Session 5: MERGE Optimization (8–12 hours)

| Step | Task | Effort |
|------|------|--------|
| D-1 | Hash-based change detection for wide tables | 4–6 hours |
| D-2 | Conditional FULL bypass for saturated aggregates | 3–4 hours |
| D-3 | Cost-based strategy selection | 6–8 hours |

### Session 6: Advanced Benchmarks (8–12 hours)

| Step | Task | Effort |
|------|------|--------|
| I-4 | Cross-run comparison tool | 4–6 hours |
| I-5 | Concurrent writer benchmarks | 4–6 hours |
| I-7 | Window / lateral / CTE operator benchmarks | 4–6 hours |

### Summary

| Session | Focus | Effort | Value |
|---------|-------|--------|-------|
| 1 | Regression triage | 3–4h | Recover Part 6 perf; verify fixes |
| 2 | Benchmark infrastructure | 8–12h | Better insights for all future work |
| 3 | Statement-level triggers | 12–16h | 50–80% write-side overhead reduction |
| 4 | Parallel refresh | 16–24h | Linear speedup for multi-ST deployments |
| 5 | MERGE optimization | 8–12h | Better strategy selection; wide table support |
| 6 | Advanced benchmarks | 8–12h | Comprehensive coverage; concurrent write testing |
| **Total** | | **55–80h** | |

### Recommended Execution Order

```
Session 1  →  Verify regressions are fixed          [PREREQUISITE for all]
Session 2  →  Benchmark infrastructure              [Enables data-driven decisions]
Session 3  →  Statement-level triggers               [Highest ROI: write-side]
Session 5  →  MERGE optimization                     [Read-side: strategy selection]
Session 4  →  Parallel refresh                       [Multi-ST scaling]
Session 6  →  Advanced benchmarks                    [Long-term observability]
```

Session 3 (statement-level triggers) is placed before Session 4 (parallel
refresh) because it delivers measurable improvement to every deployment,
while parallel refresh only helps multi-ST deployments.

---

## Appendix: Relationship to Other Plans

| Plan | Relationship |
|------|-------------|
| [PERFORMANCE_PART_8.md](PERFORMANCE_PART_8.md) | Direct predecessor. Part 8's regression analysis (A-1 through A-3) feeds Session 1 here. |
| [REPORT_PARALLELIZATION.md](REPORT_PARALLELIZATION.md) | Session 4 (parallel refresh) implements Options A+B from this report. |
| [TRIGGERS_OVERHEAD.md](TRIGGERS_OVERHEAD.md) | Session 3 (statement-level triggers) builds on this benchmark design. |
| [STATUS_PERFORMANCE.md](STATUS_PERFORMANCE.md) | Historical tracking. Update after each session completes. |
| [SQL_GAPS_7.md](../sql/SQL_GAPS_7.md) | §F27 (expose adaptive threshold), §G4.6 (wide table MERGE), §G8.5 (sequential processing). |
| [SQL_GAPS_7_QUESTIONS.md](../sql/SQL_GAPS_7_QUESTIONS.md) | Q7 (PgBouncer: FOR UPDATE SKIP LOCKED) affects parallel refresh locking strategy. |
