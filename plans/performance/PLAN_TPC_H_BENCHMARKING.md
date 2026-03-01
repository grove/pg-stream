# PLAN: TPC-H-Derived Performance Benchmarking

**Date:** 2026-03-01
**Status:** Planning
**Predecessor:** [PLAN_PERFORMANCE_PART_9.md](PLAN_PERFORMANCE_PART_9.md),
[PLAN_TEST_SUITE_TPC_H.md](../testing/PLAN_TEST_SUITE_TPC_H.md)
**Scope:** Leverage the TPC-H-derived correctness suite (22/22 queries
passing) as a performance benchmark, then use the data to guide targeted
optimizations.

> **TPC-H Fair Use Disclaimer:** This workload is *derived from* the TPC-H
> Benchmark specification but does **not** constitute a TPC-H Benchmark
> result. The data is generated with a custom pure-SQL generator (not
> `dbgen`), queries have been modified (LIMIT removed, LIKE rewritten,
> RF3 added), and no QphH metric is computed. "TPC-H" and "TPC Benchmark"
> are trademarks of the Transaction Processing Performance Council
> ([tpc.org](https://www.tpc.org/)). See Appendix D for full compliance
> notes.

---

## Table of Contents

1. [Motivation](#1-motivation)
2. [Current State](#2-current-state)
3. [Phase 1: Instrument TPC-H Harness for Benchmarking](#3-phase-1-instrument-tpc-h-harness-for-benchmarking)
4. [Phase 2: Baseline Measurement & Analysis](#4-phase-2-baseline-measurement--analysis)
5. [Phase 3: Targeted Optimizations](#5-phase-3-targeted-optimizations)
6. [Phase 4: Extend Criterion Benchmarks](#6-phase-4-extend-criterion-benchmarks)
7. [Phase 5: Re-benchmark & Report](#7-phase-5-re-benchmark--report)
8. [Implementation Schedule](#8-implementation-schedule)
9. [Verification](#9-verification)
10. [Appendix A: TPC-H Query Operator Profile](#appendix-a-tpc-h-query-operator-profile)
11. [Appendix B: Known Slow Queries](#appendix-b-known-slow-queries)
12. [Appendix C: Relationship to Other Plans](#appendix-c-relationship-to-other-plans)
13. [Appendix D: TPC-H Fair Use Compliance](#appendix-d-tpc-h-fair-use-compliance)

---

## 1. Motivation

### 1.1 The Gap

The existing benchmark infrastructure ([e2e_bench_tests.rs](../../tests/e2e_bench_tests.rs))
covers 5 simple scenarios (scan, filter, aggregate, join, join_agg) on a
synthetic 2-table schema. These are useful for micro-level regression
detection but do not reflect real analytical workloads:

| Dimension | e2e_bench_tests | TPC-H-Derived Suite |
|-----------|----------------|---------------------|
| **Query complexity** | 1–3 operators (single-table or 2-table join) | 3–8 operators, 8-table joins, subqueries |
| **Schema** | 2 tables (`src`, `dim`) | 8 tables with realistic cardinalities |
| **Mutation pattern** | Random UPDATE/DELETE/INSERT | Structured business operations (RF1 orders+lineitem INSERT, RF2 cascading DELETE, RF3 price UPDATE) |
| **Operator coverage** | Scan, filter, aggregate, inner join | + semi-join, anti-join, scalar subquery, CASE, left join, EXISTS, IN, multi-aggregate |
| **CDC fan-out** | 1 ST per source table | Multiple STs sharing CDC triggers on same sources |

### 1.2 The Opportunity

The TPC-H-derived correctness suite already exercises all 22 queries with
3 mutation cycles. **All 22 pass all 3 phases** (individual correctness,
cross-query consistency, FULL vs DIFFERENTIAL comparison). Phase 3 already
creates both FULL and DIFFERENTIAL STs per query — the infrastructure for
performance comparison exists, it just doesn't capture timing data.

Adding benchmarking instrumentation to the existing harness requires
~4 hours of work and produces a **22-query performance profile** across
realistic multi-operator analytical queries — data that cannot be obtained
from the current 5-scenario micro-benchmarks.

### 1.3 What We'll Learn

1. **Per-query FULL vs DIFFERENTIAL speedup** across all 22 queries
2. **Which operator compositions are bottlenecks** (e.g., semi-join +
   aggregate, deep join chain + scalar subquery)
3. **Where PostgreSQL MERGE execution is slow** via `EXPLAIN ANALYZE` on
   generated delta SQL
4. **Scale factor sensitivity** — how speedup ratios change from SF-0.01
   (6K lineitems) to SF-0.1 (60K) to SF-1 (600K)
5. **CDC fan-out cost** — trigger overhead when multiple STs share source
   tables (Phase 2 cross-query mode)

---

## 2. Current State

### 2.1 TPC-H Harness Capabilities

The harness ([tests/e2e_tpch_tests.rs](../../tests/e2e_tpch_tests.rs))
has three test phases:

| Phase | Purpose | Performance Data Captured |
|-------|---------|--------------------------|
| **Phase 1** — Individual query correctness | 1 ST per query, 3 RF cycles, multiset equality assertion | Wall-clock `Instant::now()` per cycle (printed, not structured) |
| **Phase 2** — Cross-query consistency | All 22 STs coexist, shared CDC triggers | Cycle wall-clock only |
| **Phase 3** — FULL vs DIFFERENTIAL | Two STs per query, multiset comparison | Cycle wall-clock only |

**What's missing for benchmarking:**
- No `[PGS_PROFILE]` extraction (Decision, Gen+Build, MERGE, Cleanup breakdown)
- No warm-up cycles (first cycle is cold, biases averages)
- No statistical aggregation (median, P95, cycle-1 vs steady-state)
- No structured output (no CSV/JSON for cross-run comparison)
- No separate FULL vs DIFFERENTIAL timing in Phase 3
- No `EXPLAIN ANALYZE` capture for delta queries

### 2.2 Known Slow Queries (from PLAN_TEST_SUITE_TPC_H.md)

| Query | SF-0.01 Cycle 2 | Root Cause | Optimization |
|-------|:----------------:|-----------|--------------|
| **Q18** | ~5,192ms | SemiJoin Part 2: full left-snapshot scan of `orders⋈lineitem` | Delta-key pre-filtering |
| **Q20** | ~2,345ms | SemiJoin Part 2: full left-snapshot scan of `partsupp⋈supplier` | Delta-key pre-filtering |
| **Q21** | ~1,889ms | AntiJoin + SemiJoin on 4-table deep chain, still 12× slowdown despite R_old fix | Delta-key pre-filtering + deep chain optimization |
| **Q04** | ~60ms | Resolved (34× faster after R_old materialization) | — |

These were captured at SF-0.01 (~6,000 lineitems). At SF-0.1 or SF-1,
these bottlenecks will dominate even more.

### 2.3 Existing Benchmark Best Results (from PERFORMANCE_PART_8.md, 100K rows, 1%)

| Scenario | FULL ms | INCR ms | Speedup |
|----------|---------|---------|---------|
| scan | 300 | 4.6 | 65.9× |
| filter | 134 | 3.4 | 39.5× |
| aggregate | 17 | 2.5 | 6.8× |
| join | 294 | 18.0 | 16.3× |
| join_agg | 28 | 14.6 | 1.9× |

These are the "ceiling" for simple queries. TPC-H queries will be slower
due to deeper operator trees and more complex delta CTE chains.

---

## 3. Phase 1: Instrument TPC-H Harness for Benchmarking

### 1-1: Add `[PGS_PROFILE]` Extraction

Reuse `extract_last_profile()` and `parse_profile_line()` from
[e2e_bench_tests.rs](../../tests/e2e_bench_tests.rs). These functions
parse Docker container stderr for the `[PGS_PROFILE]` line emitted by
[refresh.rs](../../src/refresh.rs#L1638).

```rust
// After each refresh_st() call in the TPC-H harness:
let profile = extract_last_profile(&container_id).await;
```

**Files:** `tests/e2e_tpch_tests.rs`
**Effort:** 1 hour

### 1-2: Add Benchmark Mode to Phase 3

Phase 3 (`test_tpch_full_vs_differential`) already creates both FULL and
DIFFERENTIAL STs. Modify it to:

1. **Separate timing**: Wrap each `refresh_st()` call in `Instant::now()`
   and capture FULL vs DIFFERENTIAL timings independently
2. **Add warm-up cycles**: Match `WARMUP_CYCLES = 2` from bench tests
3. **Capture `[PGS_PROFILE]`** for DIFFERENTIAL refreshes
4. **Emit structured output**: `[TPCH_BENCH]` lines per cycle

Output format:
```
[TPCH_BENCH] query=q01 tier=2 cycle=1 mode=FULL ms=45.3
[TPCH_BENCH] query=q01 tier=2 cycle=1 mode=DIFF ms=12.7 decision=0.41 gen=0.08 merge=11.3 cleanup=0.91 path=cache_hit
```

**Files:** `tests/e2e_tpch_tests.rs`
**Effort:** 2 hours

### 1-3: Add Summary Table Output

Add a `print_tpch_summary()` function that aggregates per-query results
and prints:

```
┌──────┬──────┬────────────┬────────────┬─────────┬──────────┬──────────┐
│ Query│ Tier │ FULL med ms│ DIFF med ms│ Speedup │ DIFF P95 │ Merge %  │
├──────┼──────┼────────────┼────────────┼─────────┼──────────┼──────────┤
│ Q01  │  2   │      23.4  │       8.1  │   2.9×  │    11.2  │    89%   │
│ Q02  │  1   │     156.7  │      34.2  │   4.6×  │    42.1  │    92%   │
│ ...  │      │            │            │         │          │          │
└──────┴──────┴────────────┴────────────┴─────────┴──────────┴──────────┘
```

Also emit per-phase breakdown for DIFFERENTIAL:

```
┌──────┬──────────┬───────────┬──────────┬──────────┬──────────┐
│ Query│ Decision │ Gen+Build │ Merge    │ Cleanup  │ Path     │
├──────┼──────────┼───────────┼──────────┼──────────┼──────────┤
│ Q01  │     0.41 │      0.08 │    11.30 │     0.91 │cache_hit │
│ ...  │          │           │          │          │          │
└──────┴──────────┴───────────┴──────────┴──────────┴──────────┘
```

**Files:** `tests/e2e_tpch_tests.rs`
**Effort:** 1 hour

### 1-4: Add `justfile` Targets

```make
# Run TPC-H as a performance benchmark (SF-0.01, benchmark mode)
bench-tpch: build-e2e-image
    TPCH_BENCH=1 cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture test_tpch_full_vs_differential

# TPC-H benchmark at larger scale (SF-0.1)
bench-tpch-large: build-e2e-image
    TPCH_BENCH=1 TPCH_SCALE=0.1 TPCH_CYCLES=5 cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture test_tpch_full_vs_differential

# TPC-H benchmark without rebuilding Docker image
bench-tpch-fast:
    TPCH_BENCH=1 cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture test_tpch_full_vs_differential
```

The `TPCH_BENCH=1` env var enables warm-up cycles, profile extraction, and
summary output. When unset, Phase 3 runs in its original correctness-only
mode (faster, less output).

**Files:** `justfile`
**Effort:** 15 min

### 1-5: Optional — EXPLAIN ANALYZE Capture

Add an opt-in `PGT_EXPLAIN=1` mode that, on the first measured cycle of
DIFFERENTIAL, runs the delta query wrapped in `EXPLAIN (ANALYZE, BUFFERS,
FORMAT JSON)` and saves the plan to `/tmp/tpch_plans/q<NN>.json`.

This requires cooperation from refresh.rs — either a dedicated SQL
function or a GUC that triggers EXPLAIN capture. The simpler approach:
after the first measured DIFFERENTIAL cycle, run `EXPLAIN ANALYZE` on
the raw delta SELECT (without the MERGE wrapper) using the same LSN range.

**Files:** `tests/e2e_tpch_tests.rs`, possibly `src/api.rs`
**Effort:** 3–4 hours
**Dependency:** Needs access to the generated delta SQL. May require a new
SQL function `pgtrickle.explain_delta(st_name, format)`.

---

## 4. Phase 2: Baseline Measurement & Analysis

### 2-1: Run Baseline Benchmarks

Execute at two scale factors:

```bash
just bench-tpch              # SF-0.01, ~3 min
just bench-tpch-large        # SF-0.1,  ~8 min
```

Collect per-query data:
- FULL median, DIFF median, speedup
- Per-phase breakdown (Decision, Gen+Build, MERGE, Cleanup)
- P95 for DIFF to detect spikes

### 2-2: Identify Top 5 Slowest Queries

Based on existing correctness-phase timing, expect:

| Rank | Query | Expected DIFF ms (SF-0.01) | Dominant Operator |
|------|-------|:--------------------------:|-------------------|
| 1 | Q18 | ~5,000 | SemiJoin (left-snapshot scan) |
| 2 | Q20 | ~2,300 | SemiJoin (left-snapshot scan) |
| 3 | Q21 | ~1,900 | AntiJoin + SemiJoin (deep chain) |
| 4 | Q08 | ~200–500 (est.) | 8-table InnerJoin (deep delta CTE) |
| 5 | Q07 | ~200–400 (est.) | 6-table InnerJoin + aggregate |

### 2-3: Analyze MERGE Plans

For the top 5, capture `EXPLAIN ANALYZE` output and look for:
- **Nested loop joins** where hash joins would be better (hint: GUC
  `pg_trickle.merge_planner_hints`)
- **Sequential scans** on large tables where index scans are possible
- **Sort spills to disk** in aggregate partitions
- **Missing parallel workers** (check `max_parallel_workers_per_gather`)

### 2-4: Classify Bottleneck Types

Categorize each slow query into an optimization bucket:

| Bucket | Description | Queries |
|--------|-------------|---------|
| **SemiJoin left-snapshot** | Full scan of pre-change left side | Q18, Q20, Q21 |
| **Deep join CTE** | 11+ CTEs requiring re-planning per cycle | Q07, Q08 |
| **Aggregate saturation** | All groups affected → FULL cheaper | Q01 (4 groups) |
| **MERGE overhead** | MERGE itself is slow for wide result sets | TBD |

---

## 5. Phase 3: Targeted Optimizations

Each optimization is designed to be validated via `just bench-tpch` before
and after. The TPC-H-derived correctness suite ensures that performance changes
don't break query results (run `just test-tpch-fast` after each change).

### O-1: SemiJoin Delta-Key Pre-Filtering (Q18, Q20, Q21)

**Problem:** SemiJoin/AntiJoin Part 2 scans the entire pre-change left side
to find which rows are deleted from the output. For Q18, this means scanning
the full `orders ⋈ lineitem` result set (~6K rows at SF-0.01, ~600K at SF-1)
even though only ~15 rows changed.

**Fix:** Pre-filter the left-snapshot scan using equi-join keys extracted
from the right-side delta:

```sql
-- Current (scans entire left side):
SELECT l.* FROM (left_snapshot) l
WHERE NOT EXISTS (SELECT 1 FROM (right_new) r WHERE r.key = l.key)
  AND EXISTS (SELECT 1 FROM (right_old) r WHERE r.key = l.key)

-- Optimized (pre-filtered by delta keys):
SELECT l.* FROM (left_snapshot) l
WHERE l.key IN (SELECT DISTINCT key FROM delta_right)
  AND NOT EXISTS (SELECT 1 FROM (right_new) r WHERE r.key = l.key)
  AND EXISTS (SELECT 1 FROM (right_old) r WHERE r.key = l.key)
```

The `IN (SELECT DISTINCT key FROM delta_right)` clause uses the delta's
equi-join keys to limit the left-side scan to only partitions that could
be affected by the right-side changes.

**Implementation:**
1. Move `extract_equijoin_keys()` from `join.rs` to `join_common.rs` for
   reuse by semi-join/anti-join operators
2. In `semi_join.rs` Part 2: inject `WHERE key IN (delta_keys)` into the
   left-snapshot subquery
3. Same for `anti_join.rs` Part 2

**Expected impact:**
- Q18: ~5,200ms → ~200ms (25×, eliminates full left-scan)
- Q20: ~2,300ms → ~150ms (15×)
- Q21: partial improvement (also limited by deep chain)

**Files:** `src/dvm/operators/semi_join.rs`, `src/dvm/operators/anti_join.rs`,
`src/dvm/operators/join_common.rs`
**Effort:** 8–10 hours
**Validation:** `just test-tpch-fast` (correctness) + `just bench-tpch` (perf)

### O-2: Statement-Level CDC Triggers (all queries)

**Problem:** Per-row AFTER triggers fire once per affected row. RF1 inserts
~15 orders + ~60 lineitems at SF-0.01 — that's 75 individual trigger calls
with INSERT into change buffer + WAL per call. At SF-1, this is ~1,500
orders + ~6,000 lineitems = 7,500 trigger invocations.

**Fix:** Replace row-level triggers with statement-level triggers using
PostgreSQL transition tables:

```sql
CREATE TRIGGER pg_trickle_cdc_tr_{oid}
AFTER INSERT OR UPDATE OR DELETE ON {schema}.{table}
REFERENCING NEW TABLE AS __pgt_new OLD TABLE AS __pgt_old
FOR EACH STATEMENT
EXECUTE FUNCTION pgtrickle_changes.pg_trickle_cdc_stmt_fn_{oid}();
```

This is a single trigger invocation per DML statement, processing all
affected rows in one batch INSERT.

**Expected impact:** 50–80% reduction in write-side overhead for bulk DML.
Most visible in RF1 (batch INSERT) and RF2 (batch DELETE) timings.

**Files:** `src/cdc.rs`, `src/catalog.rs`
**Effort:** 12–16 hours
**Validation:** All three tiers (`just test-unit`, `just test-e2e`,
`just test-tpch-fast`)

### O-3: UNLOGGED Change Buffers

**Problem:** Change buffer tables generate WAL for every trigger INSERT.
Since change buffers are ephemeral (truncated after each refresh) and the
system already recovers from crashes via full refresh, WAL durability is
unnecessary.

**Fix:** Create change buffer tables as `UNLOGGED`:

```sql
CREATE UNLOGGED TABLE pgtrickle_changes.changes_{oid} (...)
```

Add a GUC `pg_trickle.change_buffer_unlogged` (default: `true`).

**Expected impact:** ~30% reduction in per-row trigger overhead.

**Files:** `src/cdc.rs`, `src/catalog.rs`, `src/config.rs`
**Effort:** 4–6 hours
**Validation:** `just test-e2e` + `just test-tpch-fast`

### O-4: Adaptive Threshold Tuning

**Problem:** The current adaptive threshold (0.15) doesn't trigger FULL
fallback early enough for aggregate-heavy queries. At 10% change rate,
join_agg 100K is 0.3× FULL (slower than recomputing from scratch).

**Fix:**
1. Lower default `pg_trickle.differential_max_change_ratio` from 0.15 to
   0.10
2. Add per-operator-class awareness: queries with aggregate-only operators
   should use a lower threshold (0.05) since group-rescan is expensive
   relative to FULL

**Expected impact:** Avoid pathological INCR-slower-than-FULL scenarios.
For Q01 (4 groups, high saturation at any change rate), FULL fallback
should trigger almost always.

**Files:** `src/refresh.rs`, `src/config.rs`
**Effort:** 3–4 hours
**Validation:** `just bench-tpch` to confirm Q01 uses appropriate strategy

### O-5: Aggregate Group Saturation Bypass

**Problem:** For queries like Q01 that group by `(l_returnflag, l_linestatus)`
— only 4 groups total — any mutation likely affects all groups. The delta
computation reads the full stream table for group-rescan, making it
equivalent to (or slower than) FULL refresh.

**Fix:** Before executing the delta, check group saturation:

```sql
SELECT COUNT(DISTINCT group_key) FROM delta_changes
```

If `affected_groups >= total_groups * 0.8`, bypass INCR and use FULL.

**Expected impact:** Eliminates the aggregate overhead for high-saturation
queries. Q01 should always use FULL at any change rate > 0%.

**Files:** `src/refresh.rs`
**Effort:** 3–4 hours
**Validation:** `just bench-tpch` to confirm Q01 speedup

---

## 6. Phase 4: Extend Criterion Benchmarks

### 4-1: Add TPC-H-Derived OpTree Benchmarks

The current `diff_operators` Criterion benchmarks cover single operators.
Add composite `OpTree` structures representing TPC-H queries to benchmark
delta SQL generation for complex operator trees:

| Benchmark | OpTree Structure | Based On |
|-----------|-----------------|----------|
| `diff_tpch_q01` | Scan → Filter → Aggregate(6 funcs) | Q01 |
| `diff_tpch_q05` | 6×Scan → 5×InnerJoin → Filter → Aggregate | Q05 |
| `diff_tpch_q08` | 8×Scan → 7×InnerJoin → Aggregate | Q08 |
| `diff_tpch_q18` | 3×Scan → InnerJoin → SemiJoin → Aggregate | Q18 |
| `diff_tpch_q21` | 4×Scan → InnerJoin → SemiJoin → AntiJoin → Aggregate | Q21 |

These measure pure-Rust delta SQL generation time (no DB required), helping
detect regressions in the DVM engine as complexity increases.

**Files:** `benches/diff_operators.rs`
**Effort:** 4 hours
**Note:** Requires fixing the pgrx symbol linking issue (Part 9 §I-1) or
running inside Docker.

---

## 7. Phase 5: Re-benchmark & Report

### 5-1: After Each Optimization

After each optimization (O-1 through O-5):

1. Run `just test-tpch-fast` to confirm correctness (22/22 still pass)
2. Run `just bench-tpch` at SF-0.01 to capture performance
3. Run `just bench-tpch-large` at SF-0.1 for scale-dependent results
4. Record results in a comparison table

### 5-2: Final Comparison Table

Produce a before/after table for all 22 queries:

```
┌──────┬──────────────────────┬──────────────────────┬─────────┐
│ Query│ Before (DIFF med ms) │ After (DIFF med ms)  │ Improve │
├──────┼──────────────────────┼──────────────────────┼─────────┤
│ Q01  │           12.7       │           8.1 (FULL) │   1.6×  │
│ Q18  │        5,192.0       │         197.0        │  26.4×  │
│ ...  │                      │                      │         │
└──────┴──────────────────────┴──────────────────────┴─────────┘
```

### 5-3: Update STATUS_PERFORMANCE.md

Add a new "TPC-H-Derived Benchmark" section to
[STATUS_PERFORMANCE.md](STATUS_PERFORMANCE.md) with the full 22-query
results at each scale factor.

---

## 8. Implementation Schedule

### Session 1: Instrumentation (4–5 hours)

| Step | Task | Effort | Depends On |
|------|------|--------|------------|
| 1-1 | Add `[PGS_PROFILE]` extraction to TPC-H harness | 1h | — |
| 1-2 | Add benchmark mode to Phase 3 (warm-up, timing) | 2h | 1-1 |
| 1-3 | Add summary table output | 1h | 1-2 |
| 1-4 | Add `justfile` targets | 15min | 1-2 |

### Session 2: Baseline & Analysis (3–4 hours)

| Step | Task | Effort | Depends On |
|------|------|--------|------------|
| 2-1 | Run baseline at SF-0.01 and SF-0.1 | 30min | Session 1 |
| 2-2 | Identify top 5 slowest queries | 30min | 2-1 |
| 2-3 | Capture EXPLAIN ANALYZE for top 5 | 2–3h | 1-5 (optional) |
| 2-4 | Classify bottleneck types | 30min | 2-2 |

### Session 3: Optimizations (25–34 hours)

| Step | Task | Effort | Priority | Expected Impact |
|------|------|--------|----------|-----------------|
| O-1 | SemiJoin delta-key pre-filtering | 8–10h | **P1** | Q18: 26×, Q20: 15× |
| O-2 | Statement-level CDC triggers | 12–16h | **P2** | 50–80% write-side |
| O-3 | UNLOGGED change buffers | 4–6h | **P3** | ~30% write-side |
| O-4 | Adaptive threshold tuning | 3–4h | **P4** | Avoid INCR-slower-than-FULL |
| O-5 | Aggregate group saturation bypass | 3–4h | **P5** | Q01 always optimal |

### Session 4: Criterion & Reporting (4–6 hours)

| Step | Task | Effort | Depends On |
|------|------|--------|------------|
| 4-1 | Add TPC-H OpTree benchmarks | 4h | — |
| 5-1 | Re-benchmark after each optimization | Included in O-* |
| 5-2 | Final comparison table | 1h | All optimizations |
| 5-3 | Update STATUS_PERFORMANCE.md | 1h | 5-2 |

### Summary

| Session | Focus | Effort | Value |
|---------|-------|--------|-------|
| 1 | Instrumentation | 4–5h | Enables data-driven optimization |
| 2 | Baseline & analysis | 3–4h | Identifies highest-ROI targets |
| 3 | Optimizations | 25–34h | Direct performance improvement |
| 4 | Criterion & reporting | 4–6h | Regression detection & documentation |
| **Total** | | **36–49h** | |

### Recommended Execution Order

```
Session 1  →  Instrument harness                    [PREREQUISITE]
Session 2  →  Baseline measurement                  [Data-driven]
O-1        →  SemiJoin pre-filtering                [Highest per-query ROI]
O-3        →  UNLOGGED change buffers               [Low-risk, independent]
O-4        →  Adaptive threshold tuning             [Quick win]
O-5        →  Aggregate saturation bypass           [Quick win]
O-2        →  Statement-level CDC triggers          [Largest architectural change]
Session 4  →  Criterion + reporting                 [Documentation]
```

O-1 is prioritized first because it fixes the three worst individual
queries (Q18, Q20, Q21) that are 25–90× slower than they should be.
O-3/O-4/O-5 are independent quick wins. O-2 is the biggest change and
benefits all queries — placed later to allow thorough testing.

---

## 9. Verification

### After Every Code Change

```bash
just fmt           # Format
just lint          # Clippy, zero warnings
```

### After Instrumentation Changes (Session 1)

```bash
just test-tpch-fast   # Correctness still passes (22/22)
just bench-tpch       # Verify structured output appears
```

### After Each Optimization (Session 3)

```bash
just test-unit            # Pure Rust unit tests
just test-tpch-fast       # TPC-H correctness (22/22)
just test-e2e             # Full E2E regression
just bench-tpch           # Performance measurement
just bench-tpch-large     # Scale factor validation (optional)
```

### Final Validation

```bash
just test-all             # All test tiers pass
just bench-tpch-large     # Full benchmark at SF-0.1
```

---

## Appendix A: TPC-H Query Operator Profile

Each query's operator composition determines its delta complexity:

| Query | Operators | Tables Joined | Subqueries | Delta CTEs (est.) |
|-------|-----------|:-------------:|:----------:|:-----------------:|
| Q01 | Filter → Aggregate(6) | 1 | 0 | 3 |
| Q02 | 8×Join → ScalarSubquery → Filter | 8 | 1 correlated | 20+ |
| Q03 | 3×Join → Filter → Aggregate | 3 | 0 | 7 |
| Q04 | SemiJoin → Filter → Aggregate | 2 | 1 EXISTS | 5 |
| Q05 | 6×Join → Filter → Aggregate | 6 | 0 | 13 |
| Q06 | Filter → Aggregate(1) | 1 | 0 | 3 |
| Q07 | 6×Join → CASE → Aggregate | 6 | 0 | 13 |
| Q08 | 8×Join → CASE → Aggregate | 8 | 0 | 17 |
| Q09 | 6×Join → Filter → Aggregate | 6 | 0 | 13 |
| Q10 | 4×Join → Filter → Aggregate | 4 | 0 | 9 |
| Q11 | 3×Join → Aggregate → ScalarSubquery → Filter | 3 | 1 scalar | 8 |
| Q12 | 2×Join → CASE → Aggregate | 2 | 0 | 5 |
| Q13 | LeftJoin → Aggregate → Aggregate | 2 | 0 | 7 |
| Q14 | 2×Join → CASE → Aggregate(1) | 2 | 0 | 5 |
| Q15 | 2×Join → Aggregate → ScalarSubquery → Filter | 2 | 1 scalar | 8 |
| Q16 | 3×Join → AntiJoin → Distinct → Aggregate | 3 | 1 NOT IN | 9 |
| Q17 | 2×Join → ScalarSubquery → Aggregate(1) | 2 | 1 correlated | 8 |
| Q18 | 3×Join → SemiJoin → Aggregate | 3 | 1 IN | 9 |
| Q19 | 2×Join → CASE(OR) → Aggregate(1) | 2 | 0 | 5 |
| Q20 | 3×Join → SemiJoin → SemiJoin → Filter | 4 | 2 IN/EXISTS | 11 |
| Q21 | 4×Join → SemiJoin → AntiJoin → Aggregate | 4 | 2 EXISTS/NOT EXISTS | 13 |
| Q22 | AntiJoin → ScalarSubquery → CASE → Aggregate | 2 | 2 (NOT EXISTS + scalar) | 8 |

**Delta CTE counts** are estimates based on the DVM differentiation rules.
Actual counts will be validated in Phase 2 via EXPLAIN output.

---

## Appendix B: Known Slow Queries

### Q18: Large Volume Customer (SemiJoin bottleneck)

```sql
-- Defining query pattern (simplified):
SELECT c_name, o_orderkey, SUM(l_quantity)
FROM customer, orders, lineitem
WHERE o_orderkey IN (
    SELECT l_orderkey FROM lineitem
    GROUP BY l_orderkey HAVING SUM(l_quantity) > 300
)
AND c_custkey = o_custkey
AND o_orderkey = l_orderkey
GROUP BY c_name, o_orderkey, ...
```

**Bottleneck:** SemiJoin Part 2 scans the entire left side
(`customer ⋈ orders ⋈ lineitem`) to find rows that should be removed
from the output when the right-side delta changes which orders qualify
for the HAVING filter. At SF-0.01 this is ~6K rows; at SF-1 it's ~600K.

**Fix (O-1):** Pre-filter by `l_orderkey IN (SELECT DISTINCT l_orderkey
FROM delta_lineitem)` — only check orders whose lineitems actually changed.

### Q21: Suppliers Who Kept Orders Waiting (deep chain)

```sql
-- Pattern: 4-table join + SemiJoin(EXISTS) + AntiJoin(NOT EXISTS)
SELECT s_name, COUNT(*) AS numwait
FROM supplier, lineitem l1, orders, nation
WHERE ...
AND EXISTS (SELECT ... FROM lineitem l2 WHERE l2.l_orderkey = l1.l_orderkey AND l2.l_suppkey <> l1.l_suppkey)
AND NOT EXISTS (SELECT ... FROM lineitem l3 WHERE ... AND l3.l_receiptdate > l3.l_commitdate)
GROUP BY s_name
```

**Bottleneck:** The SemiJoin + AntiJoin nested structure creates a deep
operator tree. Each operator's Part 2 snapshot scans the pre-change left
side, and the left side itself is already a 4-table join. R_old
materialization (P6) improved Q21 from 5.4s to 1.9s, but the 12× slowdown
vs cycle 1 persists.

---

## Appendix C: Relationship to Other Plans

| Plan | Relationship |
|------|-------------|
| [PLAN_PERFORMANCE_PART_9.md](PLAN_PERFORMANCE_PART_9.md) | Parent roadmap. O-1 implements Part 9 §P7 (SemiJoin pre-filter). O-2 implements §B (statement-level triggers). O-3 implements §D5 (UNLOGGED buffers). |
| [PERFORMANCE_PART_8.md](PERFORMANCE_PART_8.md) | Contains the micro-benchmark baseline. TPC-H benchmarks complement these with complex-query data. |
| [STATUS_PERFORMANCE.md](STATUS_PERFORMANCE.md) | Will be updated with TPC-H benchmark results after Session 4. |
| [PLAN_TEST_SUITE_TPC_H.md](../testing/PLAN_TEST_SUITE_TPC_H.md) | Test plan for the TPC-H-derived correctness suite. This document extends it for performance. |
| [TRIGGERS_OVERHEAD.md](TRIGGERS_OVERHEAD.md) | Write-side benchmark design. O-2 results should be cross-referenced. |
| [REPORT_PARALLELIZATION.md](REPORT_PARALLELIZATION.md) | Parallel refresh (Part 9 §C). Not in scope here but would benefit from TPC-H Phase 2 cross-query benchmarks. |

---

## Appendix D: TPC-H Fair Use Compliance

### What We Do

| Aspect | Official TPC-H Requirement | Our Implementation |
|--------|---------------------------|--------------------|
| **Data generator** | `dbgen` (TPC reference generator) | Custom pure-SQL `generate_series` (not `dbgen`) |
| **Scale factors** | SF-1, SF-10, SF-100, etc. (power of 10) | SF-0.01, SF-0.1, SF-1 (non-standard sub-unit SFs) |
| **Queries** | Exact templates from TPC-H spec | Modified: LIMIT removed, LIKE rewritten, BETWEEN rewritten |
| **Refresh functions** | RF1 (INSERT) and RF2 (DELETE) only | RF1 + RF2 + **RF3** (UPDATE — our extension, not in TPC-H) |
| **Metric** | QphH@Size (composite throughput) | Per-query wall-clock ms + speedup ratio (no composite metric) |
| **Auditor** | Required for published results | None — internal development benchmarks only |
| **Result publication** | Must follow TPC-H Full Disclosure rules | Not published as TPC-H results |

### Compliance Requirements

1. **Never claim** that results are "TPC-H Benchmark results" or
   "TPC-H compliant". Always use "TPC-H-derived" or "based on TPC-H
   schema and queries".

2. **Include the disclaimer** at the top of any document that references
   TPC-H performance data:

   > This workload is derived from the TPC-H Benchmark specification but
   > does not constitute a TPC-H Benchmark result. TPC-H is a trademark
   > of the Transaction Processing Performance Council (tpc.org).

3. **Do not compute QphH** or any TPC-defined composite metric. Report
   only per-query timings and speedup ratios.

4. **Our modifications** (LIMIT removal, LIKE rewrites, RF3 addition,
   custom data generator) make our workload explicitly non-conforming to
   the TPC-H specification, which is the safe position.

### Why We're Safe

The TPC's Fair Use Policy restricts the use of the "TPC-H" *trademark*
on published benchmark results that don't follow the full specification.
Our usage falls into the permitted category of *derived workloads for
internal development and testing*:

- We use "TPC-H-derived" terminology, not "TPC-H Benchmark"
- We don't publish results as representative of TPC-H performance
- We don't compute or claim any TPC-defined metric
- Our data generator, queries, and refresh functions are all modified
- The purpose is correctness testing and internal performance analysis,
  not competitive benchmarking
