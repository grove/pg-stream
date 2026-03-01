# Gap Analysis: pg_trickle vs. Feldera — Core SQL IVM Engine (PostgreSQL Features Only)

> **Date:** 2026-02-28
> **pg_trickle version:** 0.1.2 (PostgreSQL 18 extension, Rust/pgrx)
> **Feldera version:** v0.255.0 (standalone incremental computation engine, Rust/Java)
> **Scope:** Core SQL incremental view maintenance engine, limited to SQL
> features available in PostgreSQL. Excludes connectors, deployment, operational
> tooling, APIs, time-series extensions, Feldera-specific SQL constructs
> (ASOF JOIN, PIVOT, QUALIFY, WITHIN DISTINCT, ARG_MIN/ARG_MAX, COUNTIF,
> lambda expressions, DECLARE RECURSIVE VIEW, CROSS/OUTER APPLY), and non-IVM
> features.

---

## Executive Summary

Both pg_trickle and Feldera implement incremental view maintenance (IVM) with
theoretical roots in DBSP (Budiu et al., VLDB 2023). Feldera *is* the
commercial DBSP implementation by its original authors; pg_trickle applies
DBSP's differentiation rules inside PostgreSQL's executor.

This analysis compares only the **core IVM engine** for SQL constructs that
exist in PostgreSQL's SQL dialect: which constructs each system can
incrementally maintain, how efficiently, and with what correctness guarantees.

**Key findings:**

- **Aggregate functions:** pg_trickle supports 39+ vs Feldera's ~20. pg_trickle
  covers all PG statistical, JSON, and ordered-set aggregates that Feldera
  lacks.
- **Window functions:** pg_trickle has full support; Feldera restricts
  ROW_NUMBER/RANK/DENSE_RANK to TopK patterns only.
- **Recursion:** Feldera has truly incremental fixed-point (DBSP native);
  pg_trickle uses recomputation-diff (correct but less efficient).
- **Set operations:** pg_trickle supports all 6 variants; Feldera lacks
  EXCEPT ALL and INTERSECT ALL.
- **Incremental efficiency:** Feldera maintains true Z-set weights and
  in-memory operator state. pg_trickle generates delta SQL executed by PG's
  planner — no persistent operator state.
- Both systems handle joins, subqueries, and CTEs comparably.

---

## Summary Table

| SQL Feature | Feldera | pg_trickle | Advantage |
|-------------|---------|-----------|-----------|
| **Aggregate functions** | ~20 | 39+ | **pg_trickle** |
| **Window functions** | Partial (TopK only for ranking) | Full | **pg_trickle** |
| **Inner / outer joins** | ✅ | ✅ | Tied |
| **Semi-join / anti-join** | ✅ | ✅ (dedicated operators) | Tied |
| **Correlated subqueries** | ✅ | ✅ | Tied |
| **LATERAL** | ✅ | ✅ | Tied |
| **Scalar subqueries** | ✅ | ✅ | Tied |
| **Set operations (UNION, EXCEPT, INTERSECT)** | 4 of 6 | All 6 | **pg_trickle** |
| **Non-recursive CTEs** | ✅ | ✅ | Tied |
| **Recursive queries (WITH RECURSIVE)** | ✅ (incremental fixed-point) | ✅ (recomputation-diff) | **Feldera** (efficiency) |
| **GROUPING SETS / CUBE / ROLLUP** | ✅ | ✅ (auto-rewritten) | Tied |
| **DISTINCT / DISTINCT ON** | ✅ / ❌ | ✅ / ✅ (auto-rewritten) | **pg_trickle** |
| **Z-set model (true weights)** | ✅ (integer weights, abelian groups) | ❌ (binary I/D + auxiliary counters) | **Feldera** |
| **Persistent operator state** | ✅ (in-memory, NVMe spill) | ❌ (stateless delta SQL per refresh) | **Feldera** |
| **Formal correctness proofs** | ✅ (Lean) | ❌ (empirical: property tests, TPC-H) | **Feldera** |
| **SQL dialect** | Calcite-based | Native PostgreSQL | **pg_trickle** (PG compat) |

---

## Detailed Comparison

### 1. Incremental Computation Model

| Aspect | Feldera (DBSP) | pg_trickle (DVM) |
|--------|---------------|-----------------|
| **Theoretical basis** | DBSP: Z-sets over abelian groups, lifting transform, integration/differentiation operators | DBSP-inspired: per-operator differentiation rules, binary delta model |
| **Z-set weights** | True integer weights in ℤ (bag semantics, composable) | Binary actions (`'I'`/`'D'`), with `__pgt_count` auxiliary for aggregates |
| **Operator state** | Persistent in-memory state per operator; integration operator (I) maintains running sums | No persistent state; "current state" = stream table contents; delta SQL reads snapshots |
| **Execution** | Compile SQL → DBSP dataflow → continuous Rust circuit | Parse SQL → DVM operator tree → generate delta SQL CTEs → PG executor |
| **Processing model** | Continuous micro-batch (each input change triggers incremental output) | Periodic refresh (scheduler triggers delta SQL execution) |
| **Optimizer** | Calcite (rule-based → DBSP circuit) | PostgreSQL planner (cost-based, mature) |

**Gap for pg_trickle:** No true Z-set weight propagation through the operator
tree. The binary I/D model works correctly (verified empirically) but is
theoretically less general than DBSP's full abelian group model. No persistent
operator state — each refresh re-reads current table contents rather than
maintaining incremental running sums.

**Gap for Feldera:** Relies on Calcite's optimizer, which lacks PostgreSQL's
cost-based join ordering, adaptive execution, and decades of optimization
heuristics. SQL dialect is Calcite-based, not PostgreSQL — queries may need
modification.

### 2. Aggregate Functions

| Function | Feldera | pg_trickle | Incremental Strategy |
|----------|---------|-----------|---------------------|
| COUNT(*) / COUNT(expr) | ✅ | ✅ | Both: algebraic / linear |
| SUM | ✅ | ✅ | Both: algebraic / linear |
| AVG | ✅ | ✅ | Both: via SUM/COUNT decomposition |
| MIN / MAX | ✅ | ✅ | Feldera: O(D log M); pg_trickle: rescan on extremum delete |
| STDDEV / STDDEV_POP / STDDEV_SAMP | ✅ | ✅ | Feldera: linear (int/decimal); pg_trickle: group-rescan |
| BIT_AND / BIT_OR / BIT_XOR | ✅ | ✅ | Feldera: non-linear O(N); pg_trickle: group-rescan |
| BOOL_AND / BOOL_OR / EVERY / SOME | ✅ | ✅ | Feldera: O(D log M); pg_trickle: group-rescan |
| ARRAY_AGG | ✅ | ✅ | Both: expensive O(M) |
| STRING_AGG | ❌ | ✅ | pg_trickle: group-rescan |
| JSON_AGG / JSONB_AGG | ❌ | ✅ | pg_trickle: group-rescan |
| JSON_OBJECT_AGG / JSONB_OBJECT_AGG | ❌ | ✅ | pg_trickle: group-rescan |
| MODE | ❌ | ✅ | pg_trickle: group-rescan (ordered-set) |
| PERCENTILE_CONT / PERCENTILE_DISC | ❌ | ✅ | pg_trickle: group-rescan (ordered-set) |
| CORR / COVAR_POP / COVAR_SAMP | ❌ | ✅ | pg_trickle: group-rescan |
| REGR_* (11 functions) | ❌ | ✅ | pg_trickle: group-rescan |
| ANY_VALUE (PG 16+) | ❌ | ✅ | pg_trickle: group-rescan |
| JSON_ARRAYAGG / JSON_OBJECTAGG (PG 16+) | ❌ | ✅ | pg_trickle: group-rescan |
| FILTER (WHERE) clause | ✅ | ✅ | Feldera: makes agg non-linear |
| WITHIN GROUP (ORDER BY) | ❌ | ✅ | pg_trickle: ordered-set aggregates |
| **Total built-in aggregates** | **~20** | **39+** | |

**Efficiency comparison:** Feldera classifies aggregates into linear (O(D)),
efficient (O(D log M)), and non-linear/expensive (O(M) or O(N)). pg_trickle
classifies into algebraic (fully differential, O(D)), semi-algebraic (rescan
on extremum delete), and group-rescan (re-aggregate affected groups). For the
5 shared algebraic aggregates (COUNT, SUM, AVG, MIN, MAX), both systems are
comparably efficient. For the remaining aggregates, Feldera's approach is
generally more efficient because it maintains per-group state in memory, while
pg_trickle re-aggregates affected groups via SQL.

**Gap for pg_trickle:** No significant gap for PostgreSQL-native aggregates —
pg_trickle covers all built-in PG aggregate functions.

**Gap for Feldera:** Missing 20+ aggregate functions available in PostgreSQL
(STRING_AGG, all JSON aggregates, statistical/regression aggregates, ordered-set
aggregates). No WITHIN GROUP.

### 3. Window Functions

| Feature | Feldera | pg_trickle |
|---------|---------|-----------|
| ROW_NUMBER | ✅ (TopK pattern only) | ✅ (full) |
| RANK / DENSE_RANK | ✅ (TopK pattern only) | ✅ (full) |
| NTILE | ❌ | ✅ |
| FIRST_VALUE / LAST_VALUE | ✅ (UNLIMITED RANGE only) | ✅ |
| LAG / LEAD | ✅ | ✅ |
| SUM / AVG / COUNT / MIN / MAX OVER | ✅ | ✅ |
| Frame clauses (ROWS/RANGE/GROUPS) | ✅ (constant bounds required) | ✅ (full) |
| PARTITION BY recomputation | ✅ | ✅ |
| Named WINDOW clauses | ? | ✅ |
| Arbitrary ranking functions | ❌ (must be in TopK subquery) | ✅ |
| Window in recursive queries | ❌ | ✅ |

**TopK restriction explained:** Feldera's ROW_NUMBER, RANK, and DENSE_RANK only
work when the compiler detects a TopK pattern: a subquery with the ranking
function, filtered by `rn < K` in the outer query. General use (e.g., numbering
all rows, pagination, gap-and-island detection) is not supported.

**Gap for pg_trickle:** None for PostgreSQL-native window function features.

**Gap for Feldera:** Ranking functions (ROW_NUMBER, RANK, DENSE_RANK) are
restricted to TopK patterns. This is a significant limitation for analytical
queries that use ranking for pagination, deduplication, or gap-and-island
analysis. No NTILE. FIRST_VALUE/LAST_VALUE limited to UNLIMITED RANGE. No
window functions in recursive query bodies.

### 4. Joins

| Feature | Feldera | pg_trickle |
|---------|---------|-----------|
| Inner join | ✅ | ✅ |
| LEFT / RIGHT / FULL OUTER | ✅ | ✅ |
| CROSS JOIN | ✅ | ✅ |
| NATURAL JOIN | ✅ | ✅ |
| Self-join | ✅ | ✅ |
| Non-equi join (theta) | ✅ | ✅ |
| Multi-condition outer join | ✅ | ✅ |

**Delta rule comparison:** Both use the bilinear join decomposition from DBSP
(Δ(A ⋈ B) = ΔA ⋈ B + A ⋈ ΔB + ΔA ⋈ ΔB). Feldera maintains join state
in memory; pg_trickle generates SQL that reads current table snapshots.

No significant gap for either system on PostgreSQL-native join types.

### 5. Subqueries

| Feature | Feldera | pg_trickle |
|---------|---------|-----------|
| Correlated subqueries | ✅ | ✅ |
| EXISTS / NOT EXISTS | ✅ | ✅ |
| IN / NOT IN (subquery) | ✅ | ✅ (semi-join / anti-join operators) |
| Scalar subquery in SELECT | ✅ | ✅ |
| Scalar subquery in WHERE | ✅ | ✅ (auto-rewritten to CROSS JOIN) |
| LATERAL subquery | ✅ | ✅ |
| LATERAL SRF (UNNEST, etc.) | ✅ | ✅ |
| ALL (subquery) | ? | ✅ (anti-join rewrite) |
| Subqueries in OR | ? | ✅ (auto-rewritten to UNION) |

No significant gap for either system on subqueries.

### 6. CTEs & Recursion

| Feature | Feldera | pg_trickle |
|---------|---------|-----------|
| Simple CTE (WITH) | ✅ | ✅ |
| Multi-reference CTE | ✅ | ✅ (shared delta) |
| Chained CTEs | ✅ | ✅ |
| Recursive queries (WITH RECURSIVE) | ✅ (incremental fixed-point, O(ΔR) per step) | ✅ (recomputation-diff) |
| Operators in recursive body | Most (no window functions) | All |
| Recursion syntax | Non-standard (DECLARE RECURSIVE VIEW) | Standard SQL (WITH RECURSIVE) |

**This is the largest theoretical gap.** Feldera implements DBSP's native
fixed-point iteration with the z⁻¹ delay operator — when input changes, only
the affected portion of the recursive computation is re-evaluated (e.g., for
transitive closure, only paths involving changed edges are recomputed).
pg_trickle uses recomputation-diff: it re-executes the full recursive query
and anti-joins the result against the current stream table to produce deltas.
This is correct but scales as O(|result|) rather than O(|Δ|).

**Gap for pg_trickle:** Recursion is not truly incremental — recomputation-diff
re-executes the full recursive query each refresh. For large recursive
structures (transitive closure of million-edge graphs), this is much slower
than Feldera's approach.

**Gap for Feldera:** Non-standard recursion syntax (DECLARE RECURSIVE VIEW
instead of WITH RECURSIVE). Window functions not supported in recursive
bodies.

### 7. Set Operations

| Operation | Feldera | pg_trickle |
|-----------|---------|-----------|
| UNION ALL | ✅ | ✅ |
| UNION (DISTINCT) | ✅ | ✅ |
| EXCEPT (DISTINCT) | ✅ | ✅ |
| EXCEPT ALL | ❌ | ✅ |
| INTERSECT (DISTINCT) | ✅ | ✅ |
| INTERSECT ALL | ❌ | ✅ |

**Gap for Feldera:** No EXCEPT ALL or INTERSECT ALL. These are less common but
needed for correct multiset semantics in some analytical queries.

### 8. DISTINCT & Grouping

| Feature | Feldera | pg_trickle |
|---------|---------|-----------|
| SELECT DISTINCT | ✅ | ✅ |
| DISTINCT ON (expr, ...) | ❌ | ✅ (auto-rewritten to ROW_NUMBER) |
| GROUP BY | ✅ | ✅ |
| GROUPING SETS | ✅ | ✅ (auto-rewritten to UNION ALL) |
| CUBE | ✅ | ✅ (auto-rewritten via GROUPING SETS) |
| ROLLUP | ✅ | ✅ (auto-rewritten via GROUPING SETS) |
| GROUPING() function | ✅ | ✅ |
| GROUP BY DISTINCT | ✅ | ? |
| HAVING | ✅ | ✅ |

**Gap for Feldera:** No DISTINCT ON (PostgreSQL extension, commonly used for
"latest row per group" patterns).

### 9. Incremental Efficiency by Operator

| Operator | Feldera | pg_trickle |
|----------|---------|-----------|
| **Filter (WHERE)** | O(D) — linear, self-incremental | O(D) — delta passthrough |
| **Project (SELECT)** | O(D) — linear, self-incremental | O(D) — delta passthrough |
| **Inner Join** | O(D × state) — maintains join index | O(D × snapshot) — reads current tables via SQL |
| **Outer Join** | O(D × state) — maintains null-padding state | O(D × snapshot) — 8-part delta for FULL OUTER |
| **Aggregate (algebraic)** | O(D) — linear, in-memory counters | O(D) — algebraic rewrite with `__pgt_count` |
| **Aggregate (group-rescan)** | O(M) — re-aggregate modified groups in memory | O(M) — re-aggregate via SQL LEFT JOIN back |
| **DISTINCT** | O(D log N) — maintains count per distinct value | O(D) — handled via GROUP BY + HAVING count |
| **UNION ALL** | O(D) — passthrough | O(D) — passthrough |
| **Window function** | O(D × partition) — recompute affected partitions | O(D × partition) — recompute via SQL |
| **Recursive CTE** | O(Δ) — incremental fixed-point | O(result) — recomputation-diff |

**Key difference:** Feldera maintains per-operator state in memory (or spilled
to NVMe), allowing O(Δ) incremental updates. pg_trickle generates SQL that
reads current table snapshots — the PostgreSQL planner optimizes this, but
there's no persistent operator state between refreshes. Feldera's approach is
theoretically more efficient for joins and recursion; pg_trickle's approach
benefits from PG's mature cost-based optimizer and avoids memory management.

### 10. Correctness & Verification

| Aspect | Feldera | pg_trickle |
|--------|---------|-----------|
| Formal proof | ✅ DBSP theorems proven in Lean | ❌ |
| Property-based testing | ✅ | ✅ (assert: Contents(ST) = Q(DB) after each mutation) |
| TPC-H validation | ? | ✅ (22-query suite, 20/22 create, 15/22 deterministic) |
| Consistency guarantee | Strongly consistent (provable) | Empirically verified (1,300+ tests) |
| Z-set correctness | Machine-checked chain rule, cycle rule, bilinear decomposition | Manual translation of DBSP rules, verified by tests |
| Recursive correctness | Fixed-point convergence proven | Recomputation-diff trivially correct (full recompute) |

**Gap for pg_trickle:** No formal proof. The per-operator differentiation rules
are direct translations of DBSP but verified empirically rather than formally.

**Gap for Feldera:** The formal proofs cover the DBSP theory, but the SQL-to-DBSP
compilation (via Calcite) is not formally verified — bugs in the SQL compiler
could violate the theoretical guarantees.

---

## Features Unique to Each System (IVM Engine, PostgreSQL Features Only)

### Feldera-only

| # | Feature | Impact |
|---|---------|--------|
| 1 | **Incremental recursive fixed-point** | O(Δ) recursion vs O(result) recomputation |
| 2 | **True Z-set weights** (integer, abelian groups) | Theoretically general multiset semantics |
| 3 | **Persistent operator state** (in-memory + NVMe spill) | Avoids re-reading snapshots on each refresh |
| 4 | **Formal DBSP proofs** (Lean) | Machine-checked correctness |

### pg_trickle-only

| # | Feature | Impact |
|---|---------|--------|
| 1 | **20+ additional aggregate functions** | STRING_AGG, JSON aggs, statistical, ordered-set, regression |
| 2 | **Full window function support** (no TopK restriction) | Arbitrary ROW_NUMBER, RANK, DENSE_RANK usage |
| 3 | **DISTINCT ON** (auto-rewritten) | "Latest per group" pattern |
| 4 | **EXCEPT ALL / INTERSECT ALL** | Complete multiset set operations |
| 5 | **WITHIN GROUP (ORDER BY)** | Ordered-set aggregate support |
| 6 | **Native PG parser** | Exact PostgreSQL SQL compatibility |
| 7 | **PG cost-based optimizer** for delta SQL | Mature planner optimizes incremental queries |
| 8 | **Full PG type system** in IVM queries | PostGIS, ranges, domains, custom operators |
| 9 | **Views as sources** (auto-inlined) | Transparent view expansion in IVM |
| 10 | **Partitioned table support** | IVM over partitioned sources |
| 11 | **Window functions in recursive queries** | Full SQL in WITH RECURSIVE bodies |
| 12 | **Auto-rewrite pipeline** (6 transparent rewrites) | DISTINCT ON, GROUPING SETS, view inlining, etc. |

---

## Recommendations for pg_trickle

### Worth considering (IVM engine improvements)

| Priority | Feature | Description | Effort | Rationale |
|----------|---------|-------------|--------|-----------|
| **Medium** | Recursive efficiency | Semi-naive evaluation for WITH RECURSIVE (avoid full recomputation) | 40+h | Key theoretical gap vs Feldera; high effort but high payoff for recursive workloads |

### Not worth pursuing

| Feature | Reason |
|---------|--------|
| True Z-set weight propagation | Would require rearchitecting the delta model. Binary I/D + auxiliary counters is correct and verified. |
| Persistent operator state | Would duplicate PG's storage engine. The stateless delta SQL model benefits from PG's planner and MVCC. |
| Formal proofs (Lean) | High effort, low ROI. Property-based tests + TPC-H provide practical confidence. |

---

## Conclusion

As pure IVM engines operating on PostgreSQL SQL features, Feldera and pg_trickle
make fundamentally different trade-offs:

**Feldera** has stronger theoretical foundations (formal DBSP proofs, true Z-set
weights, incremental fixed-point recursion, persistent operator state). Its
IVM engine is more efficient for joins over large state and recursive
computations. Its weaknesses are restricted window functions (TopK only for
ranking), missing set operation variants, and a smaller built-in aggregate
library.

**pg_trickle** has broader SQL coverage (39+ aggregates, full window functions,
all set operations, DISTINCT ON, PG type system) and benefits from PostgreSQL's
mature cost-based optimizer for delta queries. Its main weakness is the
recomputation-diff approach to recursion.

The two systems are comparable for the most common IVM workloads (joins,
filters, algebraic aggregates, subqueries, CTEs). The gap becomes significant
for recursive queries, where Feldera's IVM engine is categorically more
capable rather than merely differently optimized.

For pg_trickle, the highest-impact improvement would be **semi-naive recursive
evaluation** to close the recursion efficiency gap — the only area where
Feldera has a clear advantage when limited to PostgreSQL SQL features. For all
other PostgreSQL SQL constructs, pg_trickle already matches or exceeds
Feldera's coverage.
