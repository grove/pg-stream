# Gap Analysis: pg_trickle vs. Epsio — Core SQL IVM Engine (PostgreSQL Features Only)

> **Date:** 2026-02-28
> **pg_trickle version:** 0.1.2 (PostgreSQL 18 extension, Rust/pgrx)
> **Epsio version:** latest (commercial streaming SQL engine, Rust-based, closed-source)
> **Scope:** Core SQL incremental view maintenance engine, limited to SQL
> features available in PostgreSQL. Excludes connectors, deployment,
> multi-database support (MySQL/MSSQL/Snowflake/BigQuery), operational tooling,
> monitoring integrations, and non-IVM features.

---

## Executive Summary

Both pg_trickle and Epsio implement incremental view maintenance (IVM) for
PostgreSQL. pg_trickle is an in-database PG extension; Epsio is an external
sidecar engine that reads via logical replication and writes results back to
PG tables. Both build dataflow graphs from SQL queries and stream diffs through
stateful operators.

This analysis compares only the **core IVM engine** for SQL constructs that
exist in PostgreSQL's SQL dialect.

**Key findings:**

- **Aggregate functions:** pg_trickle supports 39+ vs Epsio's ~10. Epsio lacks
  statistical, ordered-set, and most JSON aggregates.
- **Window functions:** pg_trickle has full support (ROW_NUMBER, RANK,
  DENSE_RANK, NTILE, LAG, LEAD, all aggregate windows); Epsio supports
  only ROW_NUMBER, LAG, and RANK.
- **Joins:** pg_trickle supports all PostgreSQL join types including FULL OUTER
  and NATURAL; Epsio documents LEFT/RIGHT/INNER only.
- **Set operations:** pg_trickle supports all 6 variants; Epsio supports
  UNION and UNION ALL only.
- **Recursion:** Neither system provides truly incremental recursion. pg_trickle
  supports WITH RECURSIVE (recomputation-diff); Epsio has no documented
  recursive query support.
- **CTEs:** Both support non-recursive CTEs.
- **Incremental efficiency:** Both maintain per-operator state (Epsio uses
  RocksDB; pg_trickle generates delta SQL executed by PG's planner with no
  persistent state).

---

## Summary Table

| SQL Feature | Epsio | pg_trickle | Advantage |
|-------------|-------|-----------|-----------|
| **Aggregate functions** | ~10 | 39+ | **pg_trickle** |
| **Window functions** | 3 (ROW_NUMBER, LAG, RANK) | Full | **pg_trickle** |
| **Inner / left / right joins** | ✅ | ✅ | Tied |
| **FULL OUTER JOIN** | ❌ (not documented) | ✅ | **pg_trickle** |
| **NATURAL JOIN** | ❌ (not documented) | ✅ | **pg_trickle** |
| **Semi-join / anti-join** | ❌ (not documented) | ✅ (dedicated operators) | **pg_trickle** |
| **Correlated subqueries** | ❌ (not documented) | ✅ | **pg_trickle** |
| **LATERAL** | ❌ (not documented) | ✅ | **pg_trickle** |
| **Scalar subqueries** | ❌ (not documented) | ✅ | **pg_trickle** |
| **Subqueries (FROM clause)** | ✅ | ✅ | Tied |
| **Views as sources** | ✅ | ✅ (auto-inlined) | Tied |
| **Set operations** | UNION / UNION ALL only | All 6 | **pg_trickle** |
| **Non-recursive CTEs** | ✅ | ✅ | Tied |
| **Recursive queries (WITH RECURSIVE)** | ❌ (not documented) | ✅ (recomputation-diff) | **pg_trickle** |
| **GROUPING SETS / CUBE / ROLLUP** | ❌ (not documented) | ✅ (auto-rewritten) | **pg_trickle** |
| **DISTINCT / DISTINCT ON** | ✅ / ✅ | ✅ / ✅ (auto-rewritten) | Tied |
| **ORDER BY + LIMIT (TopK)** | ✅ | ❌ | **Epsio** |
| **Persistent operator state** | ✅ (RocksDB) | ❌ (stateless delta SQL per refresh) | **Epsio** |
| **Continuous updates** | ✅ (streaming, ~50ms batches) | ❌ (periodic scheduled refresh) | **Epsio** |
| **Transactional consistency** | ✅ (cross-table, via unified CDC stream) | ✅ (single-transaction refresh) | Tied |
| **SQL dialect** | Parsed by custom engine | Native PostgreSQL parser | **pg_trickle** (PG compat) |

---

## Detailed Comparison

### 1. Incremental Computation Model

| Aspect | Epsio | pg_trickle (DVM) |
|--------|-------|-----------------|
| **Architecture** | External sidecar engine; reads PG via logical replication, writes results back to PG tables | In-database PG extension; executes delta SQL within the same PG instance |
| **Operator model** | ~15 Collection types forming a physical dataflow graph; each Collection streams diffs to the next | 21 DVM operator types forming an operator tree; each generates delta SQL CTEs |
| **Operator state** | Persistent per-operator state in RocksDB (disk + in-memory cache) | No persistent state; delta SQL reads current table snapshots on each refresh |
| **Execution** | Parse SQL → logical plan → optimize → physical plan → Rust-based streaming Collections | Parse SQL via PG parser → DVM operator tree → generate delta SQL CTEs → PG executor |
| **Processing model** | Continuous streaming with micro-batches (default 50ms batches, adaptive) | Periodic scheduled refresh (duration intervals or cron expressions) |
| **Optimizer** | Custom logical + physical optimizer (filter/projection pushdown, join reordering) | PostgreSQL planner (cost-based, mature) |
| **CDC mechanism** | Logical replication slot (external); dedicated Rust-based CDC forwarder | Row-level AFTER triggers (in-database); change buffers in `pgtrickle_changes` schema |

**Gap for pg_trickle:** No persistent operator state — each refresh re-reads
current table contents. No continuous streaming — updates are periodic rather
than near-real-time. The trigger-based CDC adds overhead to write transactions.

**Gap for Epsio:** Requires a separate compute instance; results must be
network-shipped back to PG. Custom SQL parser means some valid PG SQL may not
be supported. Logical replication requires `REPLICA IDENTITY FULL` on source
tables (significant write amplification for large rows). No access to PG's
cost-based optimizer for delta queries.

### 2. Aggregate Functions

| Function | Epsio | pg_trickle | Incremental Strategy |
|----------|-------|-----------|---------------------|
| COUNT(*) / COUNT(expr) | ✅ | ✅ | Both: algebraic / stateful counter |
| SUM | ✅ | ✅ | Both: algebraic / stateful |
| AVG | ✅ | ✅ | Both: via SUM/COUNT decomposition |
| MIN / MAX | ✅ | ✅ | Epsio: stateful; pg_trickle: rescan on extremum delete |
| ARRAY_AGG | ✅ | ✅ | Both: expensive O(M) / group-rescan |
| JSON_AGG / JSONB_AGG | ✅ | ✅ | Both: group-rescan |
| STRING_AGG | ✅ | ✅ | Both: group-rescan |
| PERCENTILE_CONT | ✅ | ✅ | Both: group-rescan |
| STDDEV / STDDEV_POP / STDDEV_SAMP | ❌ | ✅ | pg_trickle: group-rescan |
| BIT_AND / BIT_OR / BIT_XOR | ❌ | ✅ | pg_trickle: group-rescan |
| BOOL_AND / BOOL_OR / EVERY / SOME | ❌ | ✅ | pg_trickle: group-rescan |
| JSON_OBJECT_AGG / JSONB_OBJECT_AGG | ❌ | ✅ | pg_trickle: group-rescan |
| MODE | ❌ | ✅ | pg_trickle: group-rescan (ordered-set) |
| PERCENTILE_DISC | ❌ | ✅ | pg_trickle: group-rescan (ordered-set) |
| CORR / COVAR_POP / COVAR_SAMP | ❌ | ✅ | pg_trickle: group-rescan |
| REGR_* (11 functions) | ❌ | ✅ | pg_trickle: group-rescan |
| ANY_VALUE (PG 16+) | ❌ | ✅ | pg_trickle: group-rescan |
| JSON_ARRAYAGG / JSON_OBJECTAGG (PG 16+) | ❌ | ✅ | pg_trickle: group-rescan |
| FILTER (WHERE) clause | ❌ (not documented) | ✅ | pg_trickle: supported on all aggregates |
| WITHIN GROUP (ORDER BY) | ❌ | ✅ | pg_trickle: ordered-set aggregates |
| **Total built-in aggregates** | **~10** | **39+** | |

**Gap for pg_trickle:** None for PostgreSQL-native aggregates — pg_trickle
covers all built-in PG aggregate functions.

**Gap for Epsio:** Missing 30+ aggregate functions available in PostgreSQL
(STDDEV, BIT_*, BOOL_*, all JSON object aggregates, statistical/regression
aggregates, ordered-set aggregates except PERCENTILE_CONT). No FILTER clause
support documented. No WITHIN GROUP.

### 3. Window Functions

| Feature | Epsio | pg_trickle |
|---------|-------|-----------|
| ROW_NUMBER | ✅ | ✅ |
| RANK | ✅ | ✅ |
| DENSE_RANK | ❌ | ✅ |
| LAG | ✅ | ✅ |
| LEAD | ❌ | ✅ |
| NTILE | ❌ | ✅ |
| FIRST_VALUE / LAST_VALUE | ❌ | ✅ |
| SUM / AVG / COUNT / MIN / MAX OVER | ❌ (not documented) | ✅ |
| Frame clauses (ROWS/RANGE/GROUPS) | ❌ (not documented) | ✅ (full) |
| PARTITION BY | ✅ | ✅ |
| Named WINDOW clauses | ❌ (not documented) | ✅ |
| Window in recursive queries | N/A (no recursion) | ✅ |

**Gap for pg_trickle:** None for PostgreSQL-native window function features.

**Gap for Epsio:** Only 3 of PostgreSQL's 11+ window functions are supported.
Missing DENSE_RANK, LEAD, NTILE, FIRST_VALUE, LAST_VALUE, NTH_VALUE,
CUME_DIST, PERCENT_RANK. No documented support for aggregate window functions
(SUM OVER, etc.) or frame clauses.

### 4. Joins

| Feature | Epsio | pg_trickle |
|---------|-------|-----------|
| Inner join | ✅ | ✅ |
| LEFT OUTER JOIN | ✅ | ✅ |
| RIGHT OUTER JOIN | ✅ | ✅ |
| FULL OUTER JOIN | ❌ (not documented) | ✅ |
| CROSS JOIN | ❌ (not documented) | ✅ |
| NATURAL JOIN | ❌ (not documented) | ✅ |
| Self-join | ✅ (implied) | ✅ |
| Non-equi join (theta) | ❌ (not documented) | ✅ |
| Multi-condition join | ✅ (ON ... AND) | ✅ |

**Delta rule comparison:** Both use stateful join operators that maintain state
for each side. When a diff arrives on one side, the operator looks up matching
rows from the other side's state. Epsio uses RocksDB-backed join state;
pg_trickle generates SQL that reads current table snapshots (bilinear DBSP
decomposition: Δ(A ⋈ B) = ΔA ⋈ B + A ⋈ ΔB + ΔA ⋈ ΔB).

**Gap for pg_trickle:** None for PostgreSQL-native join types.

**Gap for Epsio:** Documentation only mentions LEFT/RIGHT/INNER joins. No
documented support for FULL OUTER JOIN, CROSS JOIN, NATURAL JOIN, or non-equi
(theta) joins.

### 5. Subqueries

| Feature | Epsio | pg_trickle |
|---------|-------|-----------|
| Subqueries in FROM clause | ✅ | ✅ |
| Views as sources | ✅ | ✅ (auto-inlined) |
| Correlated subqueries | ❌ (not documented) | ✅ |
| EXISTS / NOT EXISTS | ❌ (not documented) | ✅ |
| IN / NOT IN (subquery) | ❌ (not documented) | ✅ (semi-join / anti-join operators) |
| Scalar subquery in SELECT | ❌ (not documented) | ✅ |
| Scalar subquery in WHERE | ❌ (not documented) | ✅ (auto-rewritten to CROSS JOIN) |
| LATERAL subquery | ❌ (not documented) | ✅ |
| LATERAL SRF (UNNEST, etc.) | ❌ (not documented) | ✅ |
| ALL (subquery) | ❌ (not documented) | ✅ (anti-join rewrite) |

**Gap for pg_trickle:** None for PostgreSQL-native subquery features.

**Gap for Epsio:** Only FROM-clause subqueries and views documented. No documented
support for correlated subqueries, EXISTS/NOT EXISTS, IN/NOT IN subqueries,
scalar subqueries, or LATERAL. This is a significant gap for analytical
workloads.

### 6. CTEs & Recursion

| Feature | Epsio | pg_trickle |
|---------|-------|-----------|
| Simple CTE (WITH) | ✅ | ✅ |
| Multi-reference CTE | ✅ (implied) | ✅ (shared delta) |
| Chained CTEs | ✅ (implied) | ✅ |
| Recursive queries (WITH RECURSIVE) | ❌ (not documented) | ✅ (recomputation-diff) |
| Operators in recursive body | N/A | All |

**Gap for pg_trickle:** Recursion uses recomputation-diff (re-executes the full
recursive query and diffs the result), which scales as O(|result|) rather than
O(|Δ|). However, it does support the feature.

**Gap for Epsio:** No documented support for WITH RECURSIVE queries. This is
a significant gap for graph traversal, hierarchy queries, and transitive
closure workloads.

### 7. Set Operations

| Operation | Epsio | pg_trickle |
|-----------|-------|-----------|
| UNION ALL | ✅ | ✅ |
| UNION (DISTINCT) | ✅ | ✅ |
| EXCEPT (DISTINCT) | ❌ | ✅ |
| EXCEPT ALL | ❌ | ✅ |
| INTERSECT (DISTINCT) | ❌ | ✅ |
| INTERSECT ALL | ❌ | ✅ |

**Gap for Epsio:** Only UNION and UNION ALL documented. No EXCEPT or INTERSECT
support in any variant. This limits complex analytical queries that rely on
set subtraction or intersection.

### 8. DISTINCT & Grouping

| Feature | Epsio | pg_trickle |
|---------|-------|-----------|
| SELECT DISTINCT | ✅ | ✅ |
| DISTINCT ON (expr, ...) | ✅ | ✅ (auto-rewritten to ROW_NUMBER) |
| GROUP BY | ✅ | ✅ |
| GROUPING SETS | ❌ (not documented) | ✅ (auto-rewritten to UNION ALL) |
| CUBE | ❌ (not documented) | ✅ (auto-rewritten via GROUPING SETS) |
| ROLLUP | ❌ (not documented) | ✅ (auto-rewritten via GROUPING SETS) |
| GROUPING() function | ❌ (not documented) | ✅ |
| HAVING | ❌ (not documented) | ✅ |
| ORDER BY + LIMIT (TopK) | ✅ | ❌ |

**Gap for pg_trickle:** No ORDER BY + LIMIT (TopK) support. Epsio can
incrementally maintain "top N rows" queries, which is a useful pattern for
dashboards and leaderboards.

**Gap for Epsio:** No documented support for GROUPING SETS, CUBE, ROLLUP, or
HAVING. These are critical for multi-dimensional analytical aggregations.

### 9. Incremental Efficiency by Operator

| Operator | Epsio | pg_trickle |
|----------|-------|-----------|
| **Filter (WHERE)** | O(D) — stateless, synchronous passthrough | O(D) — delta passthrough |
| **Project (SELECT)** | O(D) — stateless, synchronous map | O(D) — delta passthrough |
| **Inner Join** | O(D × state) — RocksDB-backed join index | O(D × snapshot) — reads current tables via SQL |
| **Outer Join** | O(D × state) — stateful (LEFT/RIGHT only) | O(D × snapshot) — 8-part delta for FULL OUTER |
| **Aggregate (algebraic)** | O(D) — stateful reduce (RocksDB counters) | O(D) — algebraic rewrite with `__pgt_count` |
| **Aggregate (group-rescan)** | O(M) — re-aggregate in RocksDB state | O(M) — re-aggregate via SQL LEFT JOIN back |
| **DISTINCT** | Stateful (implied) | O(D) — handled via GROUP BY + HAVING count |
| **UNION ALL** | O(D) — passthrough | O(D) — passthrough |
| **Window function** | Limited (3 functions; stateful) | O(D × partition) — recompute via SQL |
| **Recursive CTE** | N/A (not supported) | O(result) — recomputation-diff |

**Key difference:** Epsio maintains per-operator state in RocksDB (with
in-memory cache and async I/O), enabling continuous O(Δ) incremental updates
without re-reading source tables. pg_trickle generates SQL that reads current
table snapshots — the PostgreSQL planner optimizes this, but there's no
persistent operator state between refreshes. Epsio's approach avoids snapshot
reads but requires separate compute and storage; pg_trickle's approach benefits
from PG's mature cost-based optimizer and keeps everything in-database.

### 10. Correctness & Verification

| Aspect | Epsio | pg_trickle |
|--------|-------|-----------|
| Formal proof | ❌ | ❌ |
| Transactional consistency | ✅ (unified CDC stream, cross-table consistency) | ✅ (single-transaction refresh) |
| Consistency model | Eventual (typically ~50ms lag) | Snapshot-consistent at refresh time |
| Population correctness | ✅ (atomic snapshot via pg_current_snapshot) | ✅ (full refresh on create) |
| Sink correctness | ✅ (consolidated diffs, single-transaction write) | ✅ (delta applied in single transaction) |
| Property-based testing | Unknown (closed-source) | ✅ (assert: Contents(ST) = Q(DB) after each mutation) |
| TPC-H validation | Unknown | ✅ (22-query suite, 20/22 create, 15/22 deterministic) |
| Consistency guarantee | Eventual consistency (never partial results) | Empirically verified (1,300+ tests) |
| Error recovery | ✅ (auto-repopulate on internal error, retry on DB error) | Via scheduled refresh retry |

**Gap for pg_trickle:** No continuous near-real-time updates — consistency
depends on refresh schedule.

**Gap for Epsio:** Closed-source — verification approach is not publicly
documented. Eventual consistency model means brief periods (~50ms) where view
lags behind database.

---

## Features Unique to Each System (IVM Engine, PostgreSQL Features Only)

### Epsio-only

| # | Feature | Impact |
|---|---------|--------|
| 1 | **Persistent operator state** (RocksDB) | Avoids re-reading source table snapshots on each update |
| 2 | **Continuous streaming updates** (~50ms batches) | Near-real-time view freshness vs periodic refresh |
| 3 | **ORDER BY + LIMIT (TopK)** | Incrementally maintained "top N" queries |
| 4 | **Adaptive micro-batching** | Auto-adjusts batch size for throughput |
| 5 | **Automatic error recovery** | Repopulates view on internal errors, retries on DB errors |

### pg_trickle-only

| # | Feature | Impact |
|---|---------|--------|
| 1 | **30+ additional aggregate functions** | STDDEV, BIT_*, BOOL_*, JSON object aggs, statistical, regression, ordered-set |
| 2 | **Full window function support** (11+ functions) | DENSE_RANK, LEAD, NTILE, FIRST_VALUE, LAST_VALUE, aggregate windows, frame clauses |
| 3 | **FULL OUTER / CROSS / NATURAL joins** | Complete PostgreSQL join support |
| 4 | **Correlated subqueries** | EXISTS, NOT EXISTS, IN, NOT IN, scalar subqueries |
| 5 | **LATERAL subqueries** | Row-producing functions, LATERAL JOINs |
| 6 | **WITH RECURSIVE** | Recursive query support (recomputation-diff) |
| 7 | **EXCEPT / INTERSECT** (all 4 variants) | Complete set operations |
| 8 | **GROUPING SETS / CUBE / ROLLUP** | Multi-dimensional analytical aggregations |
| 9 | **HAVING clause** | Aggregate filtering |
| 10 | **FILTER (WHERE) on aggregates** | Per-aggregate conditional filtering |
| 11 | **WITHIN GROUP (ORDER BY)** | Ordered-set aggregate support |
| 12 | **DISTINCT ON** (auto-rewritten) | "Latest per group" pattern |
| 13 | **Native PG parser** | Exact PostgreSQL SQL compatibility |
| 14 | **PG cost-based optimizer** for delta SQL | Mature planner optimizes incremental queries |
| 15 | **Full PG type system** in IVM queries | PostGIS, ranges, domains, custom operators |
| 16 | **Views as sources** (auto-inlined) | Transparent view expansion in IVM |
| 17 | **Partitioned table support** | IVM over partitioned sources |
| 18 | **In-database execution** | No external compute instance, no network round-trips |
| 19 | **Auto-rewrite pipeline** (6 transparent rewrites) | DISTINCT ON, GROUPING SETS, view inlining, etc. |

---

## Recommendations for pg_trickle

### Worth considering (learning from Epsio)

| Priority | Feature | Description | Effort | Rationale |
|----------|---------|-------------|--------|-----------|
| **Medium** | ORDER BY + LIMIT (TopK) | Incrementally maintain "top N rows" queries | 16–24h | Common dashboard/leaderboard pattern; Epsio supports this, pg_trickle does not |

### Not worth pursuing

| Feature | Reason |
|---------|--------|
| Persistent operator state | Would require rearchitecting the delta model or adding RocksDB-like external storage. The stateless delta SQL model benefits from PG's planner and MVCC. |
| Continuous streaming | pg_trickle's trigger-based CDC already captures changes; the periodic refresh model with configurable schedules is a deliberate design choice for predictable resource usage. |
| External sidecar architecture | pg_trickle's in-database design is a core differentiator — zero deployment overhead, no network hops, no REPLICA IDENTITY FULL requirement. |

---

## Conclusion

As pure IVM engines operating on PostgreSQL SQL features, Epsio and pg_trickle
target similar use cases but with fundamentally different architectures:

**Epsio** provides continuous near-real-time updates with persistent operator
state (RocksDB-backed). However, its SQL coverage is notably narrow: ~10
aggregate functions, 3 window functions, no FULL OUTER JOIN, no correlated
subqueries, no LATERAL, no WITH RECURSIVE, no EXCEPT/INTERSECT, no GROUPING
SETS, and no HAVING. It requires a separate compute instance, REPLICA IDENTITY
FULL on source tables, and is closed-source.

**pg_trickle** has dramatically broader SQL coverage: 39+ aggregates, full
window functions, all join types, all set operations, correlated subqueries,
LATERAL, WITH RECURSIVE, GROUPING SETS/CUBE/ROLLUP, and the full PG type
system. It runs in-database with zero external dependencies. Its limitations
are the lack of persistent operator state and periodic (rather than continuous)
refresh.

**Overall assessment:** pg_trickle's SQL coverage is far more comprehensive than
Epsio's. For the vast majority of PostgreSQL queries that users would want to
incrementally maintain, pg_trickle already supports the necessary SQL constructs
while Epsio does not. Epsio's advantages are architectural (continuous updates,
persistent state) rather than SQL coverage.

The only SQL-level feature worth considering from Epsio is **ORDER BY + LIMIT
(TopK)** incremental maintenance, which is a useful pattern pg_trickle does not
currently support.
