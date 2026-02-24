# PLAN: External Test Suites for pg_stream

**Status:** Proposed  
**Date:** 2026-02-25  
**Branch:** `main`  
**Scope:** Adopt public SQL test suites and benchmarks to validate correctness and performance of stream tables beyond the project's existing test infrastructure.

---

## Table of Contents

1. [Motivation](#motivation)
2. [Suite 1: TPC-H — Analytical Benchmark](#suite-1-tpc-h--analytical-benchmark)
3. [Suite 2: sqllogictest — Broad Correctness](#suite-2-sqllogictest--broad-correctness)
4. [Suite 3: Join Order Benchmark (JOB) — Real-World Data](#suite-3-join-order-benchmark-job--real-world-data)
5. [Suite 4: Feldera / DBSP Benchmarks — Theory Peer Comparison](#suite-4-feldera--dbsp-benchmarks--theory-peer-comparison)
6. [Implementation Roadmap](#implementation-roadmap)
7. [Infrastructure Requirements](#infrastructure-requirements)

---

## Motivation

pg_stream's existing test suite (878 unit tests, 22 E2E test suites) validates
internal correctness. What it does **not** cover:

- **Industry-standard workloads** — are stream tables effective on the
  queries real analysts write?
- **Scale** — does differential refresh maintain its advantage at 1 GB, 10 GB,
  100 GB?
- **Breadth** — are there SQL patterns we haven't thought to test that produce
  incorrect deltas?
- **Peer comparison** — how does pg_stream's approach compare to standalone
  IVM engines like Feldera on the same workload?

Public test suites address all four gaps.

---

## Suite 1: TPC-H — Analytical Benchmark

### Why TPC-H

TPC-H is the industry-standard decision-support benchmark. Its 22 queries
cover joins (up to 8 tables), aggregates, subqueries (scalar, EXISTS, IN),
HAVING, CASE WHEN, DISTINCT — all operators pg_stream supports in
DIFFERENTIAL mode. Its two **refresh functions** (RF1: bulk INSERT, RF2:
bulk DELETE) directly exercise the CDC → delta refresh pipeline.

**URL:** https://www.tpc.org/tpch/  
**Data generator:** `dbgen` (included in TPC-H tools)

### Query Compatibility Analysis

All 22 TPC-H queries are compatible with pg_stream:

| Category | Queries | Modification |
|----------|---------|-------------|
| Works as-is | Q6, Q14, Q17, Q19 | None |
| Remove ORDER BY only | Q1, Q4, Q5, Q7, Q8, Q9, Q11, Q12, Q13, Q15, Q16, Q20, Q22 | Cosmetic — ORDER BY is silently ignored anyway |
| Remove LIMIT (+ ORDER BY) | Q2, Q3, Q10, Q18, Q21 | LIMIT is rejected by parser — must remove |

- **0 queries are blocked** by unsupported SQL features.
- **Q15** additionally requires inlining its `CREATE VIEW` as a CTE or
  subquery (pg_stream requires a single SELECT statement).
- No TPC-H query uses GROUPING SETS, NATURAL JOIN, or recursive CTEs.

### Detailed Query Matrix

| # | Name | Key SQL Features | Blocked? |
|---|------|-----------------|----------|
| Q1 | Pricing Summary | GROUP BY, SUM/AVG/COUNT, WHERE (date) | No |
| Q2 | Minimum Cost Supplier | Correlated scalar subquery (MIN), 8-table join | Remove LIMIT |
| Q3 | Shipping Priority | 3-table join, GROUP BY, SUM | Remove LIMIT |
| Q4 | Order Priority Checking | EXISTS subquery, GROUP BY, COUNT | No |
| Q5 | Local Supplier Volume | 6-table join, GROUP BY, SUM | No |
| Q6 | Forecasting Revenue | Single-table SUM, WHERE filters | No |
| Q7 | Volume Shipping | 6-table join, CASE WHEN, SUM | No |
| Q8 | National Market Share | 8-table join, CASE WHEN, subquery in FROM | No |
| Q9 | Product Type Profit | 6-table join, expressions, LIKE | No |
| Q10 | Returned Item Reporting | 4-table join, GROUP BY, SUM | Remove LIMIT |
| Q11 | Important Stock ID | HAVING with scalar subquery, 3-table join | No |
| Q12 | Shipping Modes | SUM(CASE WHEN), IN, BETWEEN | No |
| Q13 | Customer Distribution | LEFT OUTER JOIN, nested GROUP BY, subquery in FROM | No |
| Q14 | Promotion Effect | Conditional SUM ratio | No |
| Q15 | Top Supplier | View → inline as CTE; MAX subquery | Inline view |
| Q16 | Parts/Supplier | COUNT(DISTINCT), NOT IN subquery, NOT LIKE | No |
| Q17 | Small-Quantity Revenue | Scalar subquery (AVG), 2-table join | No |
| Q18 | Large Volume Customer | IN subquery with HAVING, 3-table join | Remove LIMIT |
| Q19 | Discounted Revenue | Complex OR/AND WHERE, SUM | No |
| Q20 | Potential Promotion | Nested IN subqueries (2 levels) | No |
| Q21 | Suppliers Waiting | EXISTS + NOT EXISTS, multi-join | Remove LIMIT |
| Q22 | Global Sales Opportunity | NOT EXISTS, scalar subquery, SUBSTRING | No |

### SQL Feature Coverage by TPC-H

| SQL Feature | TPC-H Queries Using It | pg_stream Support |
|-------------|------------------------|-------------------|
| INNER JOIN | Q2,Q3,Q5,Q7–Q12,Q15,Q17–Q21 | ✅ Full |
| LEFT OUTER JOIN | Q13 | ✅ Full |
| Multi-table join (3+) | Q2,Q3,Q5,Q7–Q11,Q18,Q20,Q21 | ✅ Full |
| GROUP BY | Q1,Q3–Q5,Q7–Q13,Q16,Q18,Q22 | ✅ Full |
| SUM | Q1,Q3,Q5–Q10,Q12,Q14,Q17–Q19 | ✅ Algebraic |
| AVG | Q1,Q17,Q22 | ✅ Algebraic |
| COUNT / COUNT(DISTINCT) | Q1,Q4,Q13,Q16,Q21 | ✅ Algebraic / ref-counted |
| MIN / MAX | Q2, Q15 | ✅ Semi-algebraic |
| HAVING | Q11,Q16,Q18 | ✅ Full |
| CASE WHEN | Q7,Q8,Q12,Q14,Q22 | ✅ Full |
| EXISTS / NOT EXISTS | Q4,Q21,Q22 | ✅ Semi-join / anti-join |
| IN / NOT IN subquery | Q12,Q16,Q18,Q20 | ✅ Semi-join / anti-join |
| Scalar subquery | Q2,Q11,Q15,Q17,Q22 | ✅ Full |
| Subquery in FROM | Q8,Q13,Q15,Q22 | ✅ Full |
| BETWEEN | Q1,Q3–Q6,Q12,Q15,Q20 | ✅ Full |
| LIKE / NOT LIKE | Q9,Q13,Q16 | ✅ Full |
| LIMIT | Q2,Q3,Q10,Q18,Q21 | ❌ Rejected — must remove |

### Refresh Functions

| Function | Description | CDC Path |
|----------|-------------|----------|
| RF1 | Bulk INSERT into `orders` + `lineitem` | Triggers capture → differential refresh |
| RF2 | DELETE from `orders` + `lineitem` by key | Triggers capture → differential refresh |

Both RF1 and RF2 exercise pg_stream's core pipeline. No special handling
needed — INSERTs and DELETEs are the native change types that CDC captures.

### Proposed Test Plan

**Phase T1-A: Correctness (Scale Factor 1, ~1 GB)**

1. Load TPC-H SF-1 data using `dbgen`.
2. Create 22 stream tables (one per query, with LIMIT/ORDER BY removed).
3. Perform initial FULL refresh for each.
4. Run RF1 (inserts) + RF2 (deletes) — one cycle.
5. Refresh all stream tables in DIFFERENTIAL mode.
6. Compare every stream table's contents against a fresh FULL refresh.
7. **Pass criterion:** zero row differences across all 22 queries.

**Phase T1-B: Performance (Scale Factors 1, 10, 100)**

1. For each scale factor:
   a. Load data, create stream tables, initial FULL refresh.
   b. Run 10 RF1+RF2 cycles with DIFFERENTIAL refresh.
   c. Run 10 RF1+RF2 cycles with FULL refresh.
   d. Record wall-clock time per refresh per query.
2. **Output:** Speedup ratio table (FULL / DIFFERENTIAL) × query × scale factor.
3. Integrate with the existing `e2e_bench_tests.rs` framework.

**Phase T1-C: Sustained Churn**

1. SF-10. Run 100 RF1+RF2 cycles with DIFFERENTIAL refresh.
2. After every 10th cycle, verify correctness against FULL refresh.
3. Record cumulative drift, memory usage, change buffer table sizes.
4. **Pass criterion:** zero cumulative drift; change buffers stay bounded.

### Files

| File | Purpose |
|------|---------|
| `tests/tpch/schema.sql` | TPC-H DDL (8 tables with PKs) |
| `tests/tpch/queries/` | 22 `.sql` files (LIMIT/ORDER BY stripped) |
| `tests/tpch/load.sh` | `dbgen` + `COPY` loader for a given SF |
| `tests/e2e_tpch_tests.rs` | Correctness + benchmark test harness |

---

## Suite 2: sqllogictest — Broad Correctness

### Why sqllogictest

Originally from SQLite, sqllogictest contains **millions** of SQL statements
with expected results. CockroachDB, DuckDB, and DataFusion all maintain forks.
Its value is **breadth** — covering SQL patterns no developer would think to
write manually. For pg_stream, the key test is: does a stream table in
DIFFERENTIAL mode produce the same result as FULL mode for every query?

**URL:** https://www.sqlite.org/sqllogictest/  
**Rust runner:** https://github.com/risinglightdb/sqllogictest-rs

### Approach

We do not need to run the full sqllogictest corpus verbatim. Instead, we use
it as a **query generator**:

1. **Filter** the corpus for queries that use pg_stream-supported features
   (joins, aggregates, subqueries, etc.) and exclude unsupported ones
   (LIMIT, GROUPING SETS, etc.).
2. For each qualifying query Q over tables T₁…Tₙ:
   a. Create base tables, populate with seed data.
   b. Create stream table ST with defining query Q in DIFFERENTIAL mode.
   c. Initial FULL refresh → record contents as `expected_full`.
   d. Apply a small random change set to T₁…Tₙ.
   e. DIFFERENTIAL refresh → record contents as `actual_diff`.
   f. FULL refresh (from scratch) → record contents as `expected_after`.
   g. **Assert:** `actual_diff == expected_after`.
3. **Pass criterion:** zero mismatches across the filtered corpus.

### Scope

Focus on the `test/index/random/` and `test/select/` directories which contain
the highest density of relevant analytical queries. Skip:
- Tests that are SQLite-specific (type affinity, etc.)
- Tests requiring LIMIT/OFFSET
- Tests with non-deterministic functions

### Estimated Query Count

After filtering: ~5,000–15,000 qualifying queries. Each adds ~100 ms of
test time at small scale. Budget: 10–30 minutes for full correctness sweep.

### Files

| File | Purpose |
|------|---------|
| `tests/sqllogictest/filter.py` | Filter corpus → qualifying queries |
| `tests/sqllogictest/runner.rs` | Custom harness: create ST → mutate → compare |
| `tests/e2e_slt_tests.rs` | Top-level test entry point |

---

## Suite 3: Join Order Benchmark (JOB) — Real-World Data

### Why JOB

The Join Order Benchmark uses the **real IMDB dataset** (~3.6 GB) with 113
analytically complex queries, some joining 10+ tables. Its value is testing
delta correctness under **realistic data skew** — foreign-key distributions
that are far from uniform, unlike synthetic TPC-H data.

**Paper:** Leis et al., "How Good Are Query Optimizers, Really?" (PVLDB 2015)  
**URL:** https://github.com/gregrahn/join-order-benchmark

### Query Compatibility

JOB queries use INNER JOINs, WHERE with equality/range/LIKE, and minimal
aggregation. Most require only ORDER BY / LIMIT removal. No GROUPING SETS,
recursive CTEs, or window functions. Estimated compatibility: **110+/113
queries** with trivial modifications.

### Proposed Test Plan

1. Load IMDB data (CSV import).
2. Create stream tables for the 113 queries.
3. Apply targeted mutations (INSERT/UPDATE/DELETE on high-skew tables like
   `cast_info`, `movie_info`).
4. DIFFERENTIAL refresh → compare against FULL refresh.
5. **Focus metric:** correctness under skewed join cardinalities.

### Files

| File | Purpose |
|------|---------|
| `tests/job/schema.sql` | IMDB DDL (21 tables with PKs) |
| `tests/job/queries/` | 113 `.sql` files (adapted) |
| `tests/e2e_job_tests.rs` | Correctness test harness |

---

## Suite 4: Feldera / DBSP Benchmarks — Theory Peer Comparison

### Why Feldera

Since pg_stream is grounded in DBSP theory, comparing against Feldera (the
reference DBSP implementation) on the same workloads provides a meaningful
performance baseline. Feldera maintains benchmarks for Nexmark (streaming
auction events) and TPC-H variants.

**URL:** https://github.com/feldera/feldera/tree/main/benchmark

### Approach

This is **not** an apples-to-apples comparison (Feldera is a standalone
streaming engine; pg_stream runs inside PostgreSQL). The goal is to
understand the **overhead of the PostgreSQL execution model** vs. a dedicated
dataflow runtime, and to identify queries where pg_stream's delta SQL
generation is suboptimal.

### Nexmark Benchmark

Nexmark models an online auction system with 3 event types (Person, Auction,
Bid) and 22 queries. Several Nexmark queries use window functions and
time-based grouping. Relevant subset for pg_stream:

| Query | Features | Compatible? |
|-------|----------|-------------|
| Q0 | Passthrough (no-op) | ✅ |
| Q1 | Projection + arithmetic | ✅ |
| Q2 | Filter | ✅ |
| Q3 | Join + filter | ✅ |
| Q4 | Join + GROUP BY + AVG + MAX | ✅ |
| Q5 | Window: COUNT per time window | ✅ (window function) |
| Q6 | Join + AVG + window | ✅ |
| Q7 | MAX in time window | ✅ |
| Q8 | Join (person-auction within window) | ✅ |
| Q9–Q22 | Various combinations | Most compatible |

### Proposed Test Plan

1. Adapt Nexmark data generator for PostgreSQL tables.
2. Create stream tables for compatible Nexmark queries.
3. Measure:
   a. Throughput (events/sec processed per refresh cycle).
   b. Latency (time from event commit to reflected in stream table).
   c. Compare published Feldera numbers on same hardware class.
4. Publish results in `docs/BENCHMARK.md` as a separate section.

### Files

| File | Purpose |
|------|---------|
| `tests/nexmark/schema.sql` | Nexmark DDL (3 tables) |
| `tests/nexmark/generator.sql` | Data generation via `generate_series` |
| `tests/nexmark/queries/` | Adapted Nexmark queries |
| `tests/e2e_nexmark_tests.rs` | Benchmark harness |

---

## Implementation Roadmap

| Phase | Suite | Effort | Priority | Value |
|-------|-------|--------|----------|-------|
| **T1** | TPC-H correctness (SF-1) | 2–3 days | **P0** | Industry-standard validation; catches delta bugs in joins + aggregates |
| **T2** | TPC-H performance (SF-1/10/100) | 2–3 days | **P1** | Quantified speedup numbers for README and marketing |
| **T3** | sqllogictest (filtered corpus) | 3–5 days | **P1** | Maximum breadth; confident correctness claim |
| **T4** | JOB correctness | 2 days | **P2** | Real-world data skew validation |
| **T5** | Feldera/Nexmark comparison | 3–5 days | **P2** | Competitive positioning; identifies optimization gaps |

**Recommended start:** T1 → T2 → T3. These three phases provide the highest
signal-to-effort ratio and produce publishable benchmark numbers.

---

## Infrastructure Requirements

### Docker

All test suites run inside the existing E2E Testcontainers infrastructure.
TPC-H at SF-100 requires ~100 GB disk and ~16 GB RAM; run on CI with a
`large` runner or locally.

### Data Generation

| Suite | Tool | Data Size |
|-------|------|-----------|
| TPC-H SF-1 | `dbgen` (C tool, compiles in seconds) | ~1 GB |
| TPC-H SF-10 | `dbgen` | ~10 GB |
| TPC-H SF-100 | `dbgen` | ~100 GB |
| sqllogictest | Corpus download (SQLite repo) | ~50 MB |
| JOB / IMDB | CSV download from ftp.fu-berlin.de | ~3.6 GB |
| Nexmark | SQL `generate_series` | Configurable |

### CI Integration

- **T1 (correctness):** Add to CI as a weekly scheduled job (SF-1, ~10 min).
- **T2 (performance):** Nightly job on dedicated hardware; results tracked
  in a benchmark history file.
- **T3 (sqllogictest):** Weekly; flag regressions via exit code.
- **T4/T5:** Manual or monthly; results published to `docs/BENCHMARK.md`.

### Test Tagging

All external-suite tests should be `#[ignore]` (like existing benchmarks)
and gated behind a `just` target:

```bash
just test-tpch          # TPC-H correctness (SF-1)
just bench-tpch         # TPC-H performance (SF-1/10/100)
just test-slt           # sqllogictest filtered corpus
just test-job           # Join Order Benchmark
just bench-nexmark      # Nexmark comparison benchmarks
```
