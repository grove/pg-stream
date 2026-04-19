# PLAN: Worst-Case Optimal Join Algorithms for pg_trickle

> **Date:** 2026-04-19
> **Status:** Research / Proposed
> **Related:** [PLAN_DVM_IMPROVEMENTS.md](../performance/PLAN_DVM_IMPROVEMENTS.md) (DI-11),
> [PLAN_TPCH_DVM_PERF.md](../performance/PLAN_TPCH_DVM_PERF.md),
> [PLAN_TPC_H_BENCHMARKING.md](../performance/PLAN_TPC_H_BENCHMARKING.md),
> [GAP_ANALYSIS_FELDERA.md](../ecosystem/GAP_ANALYSIS_FELDERA.md)
> **Scope:** Investigate how Leapfrog Triejoin (LFTJ) and related worst-case
> optimal join (WCOJ) algorithms can improve multi-way join performance in
> pg_trickle's DVM engine, both for full refresh and differential delta SQL
> generation.

---

## Table of Contents

1. [Motivation](#1-motivation)
2. [Background: Worst-Case Optimal Join Algorithms](#2-background-worst-case-optimal-join-algorithms)
3. [Application to pg_trickle](#3-application-to-pg_trickle)
4. [Related Algorithms](#4-related-algorithms)
5. [Implementation Strategy](#5-implementation-strategy)
6. [Evaluation Plan](#6-evaluation-plan)
7. [Risk Assessment](#7-risk-assessment)
8. [Effort Estimates](#8-effort-estimates)
9. [References](#9-references)

---

## 1. Motivation

### 1.1 The Multi-Way Join Problem

pg_trickle's DVM engine handles multi-way joins through **recursive binary
decomposition**. A 6-way equi-join `A ⋈ B ⋈ C ⋈ D ⋈ E ⋈ F` is parsed
into a left-deep tree of nested binary join operators, each independently
producing a 3-part `UNION ALL` delta CTE. At TPC-H scale, this produces
22–30 CTEs for a single 6-table query, with per-leaf L₀ snapshots that may
be evaluated multiple times.

### 1.2 Observed Performance Bottlenecks

TPC-H benchmarks at SF=1.0 (April 2026) show catastrophic scaling for
multi-way join queries:

| Query | Tables | SF=0.01 (ms) | SF=1.0 (ms) | Blowup | Root Cause |
|-------|--------|-------------|-------------|--------|------------|
| Q05   | 6      | 103         | 28,404      | 276×   | Binary decomposition: intermediate cardinality explosion |
| Q07   | 6      | 124         | 31,113      | 251×   | Deep CTE chain + temp file spill |
| Q08   | 6      | 138         | 39,940      | 289×   | Deep CTE chain + temp file spill |
| Q09   | 6      | 114         | 29,204      | 256×   | Cascading EXCEPT ALL in per-leaf L₀ |
| Q20   | 4 (nested) | 1,775   | 2,647       | 1.5×   | Doubly-nested semi-join: O(Δ × n) |
| Q21   | 4      | 1,889       | —           | >12×   | Anti-join + semi-join on deep chain |

**At SF=1.0, 18 of 22 TPC-H queries have DIFF slower than FULL.**

The binary decomposition strategy has a fundamental asymptotic limitation:
it can produce $O(n^k)$ intermediate tuples for a k-way join even when the
final result has only $O(n)$ rows. This is the exact problem that worst-case
optimal join algorithms were designed to solve.

### 1.3 Why This Matters for pg_trickle

pg_trickle's primary value proposition is that differential refresh is faster
than full refresh. When multi-way joins make DIFF slower than FULL, the
extension provides negative value for exactly the query patterns that
analytics workloads use most frequently (star schemas, snowflake schemas,
multi-fact joins).

---

## 2. Background: Worst-Case Optimal Join Algorithms

### 2.1 The AGM Bound

Atserias, Grohe, and Marx (2008) proved a tight upper bound on the maximum
output size of a full conjunctive query given constraints on input relation
sizes. For a query $Q = R_1(x_1) \bowtie R_2(x_2) \bowtie \cdots \bowtie R_m(x_m)$,
the AGM bound states:

$$|Q| \leq \prod_{i=1}^{m} |R_i|^{w_i}$$

where the $w_i$ form a fractional edge cover of the query hypergraph.

Traditional binary join plans can produce intermediate results up to
$O(n^{\lfloor k/2 \rfloor})$ for a $k$-way join on $n$-row tables, even
when the AGM bound is $O(n)$. For a triangle query $(R(a,b) \bowtie S(b,c)
\bowtie T(a,c))$, binary plans are $O(n^{3/2})$ while WCOJ achieves $O(n^{3/2})$
matching the AGM bound — and for acyclic queries the difference is even
starker.

### 2.2 Leapfrog Triejoin (LFTJ)

**Veldhuizen (2012/2014)**. Leapfrog Triejoin is a worst-case optimal join
algorithm developed at LogicBlox for their commercial Datalog system.

**Core idea:** Relations are stored in sorted trie order over their
attributes. The join is computed variable-by-variable (not relation-by-relation),
using a "leapfrog" intersection procedure that simultaneously advances
iterators from all relations that share the current variable.

**Key operations:**
- **Open/Up/Next** — Navigate the trie level by level (one attribute per level)
- **Leapfrog join** — Given $k$ sorted iterators on the same variable, the
  leapfrog procedure advances each iterator to the next value ≥ the maximum
  of all current positions, cycling through iterators. Terminates when all
  iterators point to the same value (match) or any iterator is exhausted.
- **Backtracking** — After enumerating all matches at the current level,
  backtrack to the parent level and advance to the next value.

**Complexity:** $O(n \cdot \text{AGM}(Q) \cdot \log n)$ — worst-case optimal
up to a log factor.

**Properties relevant to pg_trickle:**
- Works with conventional B-tree indexes (sorted access)
- Extends naturally to $\exists$-queries (semi-joins)
- Handles cyclic queries optimally (triangles, 4-cliques)
- Attribute order is a tuning parameter — different orders give different
  constant factors

### 2.3 NPRR Algorithm

**Ngo, Porat, Ré, Rudra (2012)**. The first constructive worst-case optimal
join algorithm. Uses a recursive partitioning scheme based on heavy/light
hitters. More complex than LFTJ but achieves the same asymptotic bound
without the log factor.

**Relevance to pg_trickle:** Primarily theoretical — LFTJ is simpler to
implement and has better constant factors in practice. NPRR's contribution
is the proof that WCOJ is achievable, which motivated LFTJ's development.

### 2.4 Generic Join

**Ngo, Ré, Rudra (2014)**. A framework that unifies NPRR and LFTJ under
a common abstraction called "Generic Join." Shows that any algorithm
satisfying a simple "gap" property achieves worst-case optimality.

**Key insight for pg_trickle:** The Generic Join framework proves that
the leapfrog intersection primitive is sufficient — we don't need to
implement the full trie data structure if we can express sorted-intersection
in SQL.

---

## 3. Application to pg_trickle

### 3.1 Opportunity Areas

pg_trickle generates SQL that PostgreSQL executes. The engine does not
control the join algorithm directly — PostgreSQL's executor chooses between
nested loop, hash join, and merge join. However, pg_trickle controls the
**structure of the generated SQL**, which determines what plans the
PostgreSQL optimizer can consider.

There are four concrete opportunity areas:

#### Opportunity A: N-ary Delta Join SQL Generation

**Problem:** The current binary decomposition generates $O(k)$ CTEs for a
$k$-way join, each reading from the previous CTE's output. Intermediate
results can be $O(n^{k/2})$.

**WCOJ approach:** Instead of `(ΔA ⋈ B) ⋈ C ⋈ D`, generate a single
multi-way intersection query that processes all relations simultaneously.

**SQL encoding strategy — RECURSIVE CTE intersection:**

```sql
-- Example: 3-way equi-join A(a,b) ⋈ B(b,c) ⋈ C(a,c)
-- Delta on A: find matching (b,c,a) tuples

WITH delta_a AS (
  -- CDC changes to table A
  SELECT op, a, b FROM pgtrickle_changes.changes_A WHERE ...
),
-- Level 1: enumerate 'b' values from delta, intersected with B
level_b AS (
  SELECT DISTINCT da.op, da.a, da.b
  FROM delta_a da
  WHERE EXISTS (SELECT 1 FROM B WHERE B.b = da.b)
),
-- Level 2: for each (a,b), enumerate 'c' from B intersected with C
level_bc AS (
  SELECT lb.op, lb.a, lb.b, B.c
  FROM level_b lb
  JOIN B ON B.b = lb.b
  WHERE EXISTS (SELECT 1 FROM C WHERE C.a = lb.a AND C.c = B.c)
)
SELECT op, a, b, c FROM level_bc;
```

This variable-at-a-time pattern mirrors LFTJ's trie traversal. PostgreSQL
can use index scans for each `EXISTS` check, achieving the "leapfrog"
effect through B-tree seek operations.

**Expected impact:** Eliminates intermediate cardinality blowup for
cyclic and near-cyclic joins (Q05, Q07, Q08, Q09). The generated SQL
produces at most $O(\text{AGM}(Q))$ intermediate rows per level.

#### Opportunity B: Semi-Join / Anti-Join Optimization

**Problem:** Semi-join Part 2 (right-side changes) requires scanning the
full left snapshot for every changed right row. For Q18/Q20/Q21, this is
the dominant cost — 1–5 seconds at SF=0.01.

**WCOJ approach:** Express the semi-join delta as a multi-way intersection:

```sql
-- Delta(L ⋉ R) for ΔR changes
-- Instead of: for each Δr, scan all matching L rows and check R_old/R_new

WITH delta_r AS (...),
-- Step 1: find left rows correlated with any changed right key
affected_left AS (
  SELECT l.*
  FROM left_table l
  WHERE l.join_key IN (SELECT DISTINCT join_key FROM delta_r)
),
-- Step 2: intersect with R_new and R_old simultaneously
status_check AS (
  SELECT al.*,
    EXISTS (SELECT 1 FROM right_current r WHERE r.key = al.key) AS has_new,
    EXISTS (SELECT 1 FROM right_old ro WHERE ro.key = al.key) AS has_old
  FROM affected_left al
)
SELECT CASE WHEN has_new AND NOT has_old THEN '+' ELSE '-' END AS op, ...
FROM status_check
WHERE has_new <> has_old;
```

The key insight is to use the delta keys as a **restriction set** on the
left side first, reducing the scan from $O(|L|)$ to $O(|\Delta R| \cdot
\text{fan-out})$.

**Expected impact:** Q20 from ~2s to <50ms (matches FULL performance).

#### Opportunity C: Pre-Change Snapshot (L₀) Elimination

**Problem:** The L₀ snapshot reconstruction (`build_pre_change_snapshot_sql`)
uses `EXCEPT ALL` or `NOT EXISTS` per leaf, then re-composes the join tree.
For deep joins, this is the dominant cost.

**WCOJ approach:** Instead of materializing L₀ as a complete relation, use
the LFTJ intersection pattern to compute only the **affected portion** of
L₀. The delta keys from changed tables define the subset of L₀ that matters:

```sql
-- Instead of: L₀ = (full left table EXCEPT ALL deltas)
-- Compute: L₀_affected = rows in L₀ that share a join key with any delta

WITH affected_keys AS (
  SELECT DISTINCT join_key FROM delta_any_table
),
l0_affected AS (
  SELECT l.* FROM left_current l
  WHERE l.key IN (SELECT join_key FROM affected_keys)
  AND NOT EXISTS (
    SELECT 1 FROM delta_left dl
    WHERE dl.__pgt_row_id = l.__pgt_row_id AND dl.op = '+'
  )
  UNION ALL
  SELECT * FROM delta_left WHERE op = '-'
)
...
```

This turns L₀ from an $O(n)$ operation to $O(|\Delta| \cdot \text{fan-out})$.

**Expected impact:** Addresses the "threshold collapse" pattern (Q05/Q07/Q08/Q09)
where L₀ reconstruction dominates at scale.

#### Opportunity D: Cyclic Query Optimization

**Problem:** Cyclic join patterns (triangle queries, diamond joins) appear
in graph-like analytics queries. Binary plans are provably sub-optimal for
these.

**WCOJ approach:** LFTJ was originally designed for cyclic queries in
Datalog. The variable-at-a-time processing handles cycles naturally — each
variable is bound by intersecting all relations that mention it, regardless
of whether the query graph is acyclic.

**Example — triangle query:**
```sql
-- Friends of friends who are also friends
SELECT a.user_id, b.friend_id, c.friend_id
FROM friendships a
JOIN friendships b ON a.friend_id = b.user_id
JOIN friendships c ON b.friend_id = c.user_id AND c.friend_id = a.user_id
```

Binary plan: $O(n^{3/2})$. LFTJ: $O(n^{3/2})$ matching AGM bound (which
is tight for triangle queries). For sparser graphs, LFTJ's bound can be
much better.

**Expected impact:** Enables efficient IVM for graph analytics workloads
that are currently impractical.

### 3.2 Non-Applicable Areas

LFTJ / WCOJ are **not helpful** for:

- **Two-table joins** — Binary join is already optimal for two relations.
  PostgreSQL's hash join is hard to beat here.
- **Non-equi joins** — LFTJ requires sorted equality-based intersection.
  Theta joins (range, inequality) need different techniques.
- **Aggregate-dominated queries** — Q01 (single-table aggregate) is slow
  because all groups are affected, not because of join costs.
- **DISTINCT / UNION / EXCEPT** — These operators have their own delta
  rules unrelated to join optimization.

---

## 4. Related Algorithms

### 4.1 Minesweeper (Abo Khamis et al., 2016)

An **instance-optimal** join algorithm that adapts to the structure of the
actual data, not just relation sizes. Uses certificates (witnesses of gaps
in the data) to skip large portions of the search space.

**Relevance to pg_trickle:** Minesweeper could improve on LFTJ for sparse
data by avoiding enumeration of empty regions. However, it requires custom
data structures (gap-tracking B-trees) that cannot be easily expressed in
SQL. **Verdict: Monitor but do not implement.**

### 4.2 Tetris (Abo Khamis et al., 2018)

Extends Minesweeper with geometric reasoning — models the join as a
hyperrectangle covering problem. Achieves instance-optimality within a
polylog factor.

**Relevance to pg_trickle:** Even more data-structure-dependent than
Minesweeper. **Verdict: Theoretical interest only.**

### 4.3 Free Join (Wang et al., 2023)

A unifying framework that shows traditional binary join plans and WCOJ can
be combined into a single "free join" plan that is never worse than either.
The key insight is that binary join plans are special cases of WCOJ where
variables are grouped into blocks.

**Relevance to pg_trickle:** **High.** Free Join provides a principled way
to mix binary and n-ary join strategies. For acyclic queries, it can choose
binary plans (which PostgreSQL executes well). For cyclic subpatterns, it
switches to WCOJ. This hybrid approach is ideal for pg_trickle because:

1. Most real-world queries are acyclic — we should not regress on these
2. Cyclic subpatterns benefit from WCOJ
3. The decision can be made statically at query analysis time

**Implementation approach:** During `OpTree` construction, detect whether
the join graph has cycles. For acyclic join graphs, keep the current binary
decomposition. For cyclic subgraphs, generate LFTJ-style SQL.

### 4.4 YannakakisAlgorithm (Yannakakis, 1981)

The classic algorithm for **acyclic** conjunctive queries. Runs in $O(n + |output|)$
time by:
1. Semi-join reduction: eliminate dangling tuples top-down
2. Join enumeration: enumerate results bottom-up

**Relevance to pg_trickle:** **High.** Most TPC-H queries are acyclic
(star/snowflake schemas). Yannakakis-style semi-join reduction can be
applied as a **pre-processing pass** before generating delta SQL:

```sql
-- Semi-join reduction: remove orders with no matching lineitem changes
WITH relevant_orders AS (
  SELECT o.* FROM orders o
  WHERE o.o_orderkey IN (SELECT l_orderkey FROM delta_lineitem)
)
-- Now join only relevant_orders with other tables
...
```

This is essentially what DI-6 (lazy semi-join R_old) partially implements.
A systematic Yannakakis pass would generalize it to all join positions.

**Implementation approach:** Before generating delta CTEs, run the
Yannakakis semi-join reduction on the query hypergraph using the delta
keys as the "seed set." This pre-filters every relation to only rows that
can participate in the output, dramatically reducing intermediate sizes.

### 4.5 Factorized Representations (Olteanu & Závodný, 2015)

Instead of materializing the full Cartesian product of join results, keep
results in a **factorized** form that avoids redundancy. For a join
$R(a,b) \bowtie S(b,c)$, instead of storing $(a_i, b_j, c_k)$ tuples,
store the factored form $\{b_j : \{a_i\} \times \{c_k\}\}$.

**Relevance to pg_trickle:** **Medium.** Factorized results cannot be
directly stored in PostgreSQL tables (which are flat relations). However,
the factorization idea can be applied to **intermediate CTE design**:

- Current: CTE produces full rows with all columns
- Factorized: CTE produces only join keys + row references, with full rows
  assembled only in the final projection

This reduces intermediate CTE sizes and temp file usage.

**Implementation approach:** For deep joins, split the delta CTE chain into
a "key propagation" phase (small CTEs with only join keys) and a "row
assembly" phase (single final join against base tables). This is a form
of lazy evaluation.

### 4.6 Delta-WCOJ (Kara et al., 2023)

Combines worst-case optimal joins with incremental view maintenance. The
key contribution is a delta rule for WCOJ that processes only changed
tuples, avoiding recomputation of unchanged join results.

**Relevance to pg_trickle:** **Very high.** This is the direct intersection
of WCOJ and IVM — exactly pg_trickle's domain. The delta rule for an n-ary
join under WCOJ is:

$$\Delta(R_1 \bowtie \cdots \bowtie R_k) = \bigcup_{i=1}^{k} (\Delta R_i \bowtie R_1' \bowtie \cdots \bowtie R_{i-1}' \bowtie R_{i+1} \bowtie \cdots \bowtie R_k)$$

where $R_j' = R_j \cup \Delta R_j$ for $j < i$ (already-updated relations).

This generalizes the binary delta rule to k-way joins directly. Combined
with LFTJ's execution strategy, it processes each $\Delta R_i$ term using
sorted intersection against the current state of all other relations.

**Implementation approach:** This is the theoretical foundation for
Opportunity A. The delta SQL generation for multi-way joins should follow
the Delta-WCOJ formula rather than nested binary decomposition.

### 4.7 Summary of Algorithm Applicability

| Algorithm | Applicability | Priority | Expressible in SQL? |
|-----------|--------------|----------|-------------------|
| **Leapfrog Triejoin** | Multi-way equi-join delta SQL | High | Yes (EXISTS chains + B-tree seeks) |
| **Free Join** | Hybrid binary/n-ary join selection | High | Yes (static query analysis) |
| **Yannakakis** | Semi-join reduction pre-pass | High | Yes (CTE pre-filtering) |
| **Delta-WCOJ** | N-ary delta rule for IVM | Very High | Yes (generalizes current delta rule) |
| **Factorized Repr.** | Intermediate CTE size reduction | Medium | Partially (key-only CTEs) |
| NPRR | Theoretical foundation | Low | No (complex partitioning) |
| Minesweeper | Instance-optimal skipping | Low | No (custom data structures) |
| Tetris | Geometric optimization | Low | No (custom data structures) |

---

## 5. Implementation Strategy

### Phase 1: Yannakakis Semi-Join Reduction (2–3 days)

**Goal:** Add a pre-processing pass that restricts every base table to only
rows reachable from the delta keys, before generating delta CTEs.

**What changes:**

1. **Query hypergraph extraction** — In `src/dvm/parser/mod.rs`, extract the
   join graph as a set of `(relation, attributes, join_keys)` triples during
   `OpTree` construction.

2. **Semi-join reduction pass** — New module `src/dvm/operators/semijoin_reduce.rs`.
   Given the delta table(s) and the join hypergraph, compute which keys are
   "live" (reachable from a delta row through equi-join chains). Generate a
   cascade of `WHERE key IN (SELECT ...)` filters.

3. **Integration** — In `diff_inner_join()` (and `diff_outer_join`, etc.),
   wrap base table references with the semi-join reduction CTEs before
   generating the Part 1/Part 2 SQL.

**Expected impact:**
- Q18/Q20/Q21: 5–10× improvement (semi-join Part 2 left-scan reduction)
- Q05/Q07/Q08/Q09: 2–5× improvement (L₀ size reduction)
- Zero risk to correctness — semi-join reduction preserves join semantics

**Validation:** All 22 TPC-H queries pass DIFF ≡ FULL equivalence check.

### Phase 2: Delta-WCOJ for Multi-Way Joins (5–7 days)

**Goal:** For joins with 4+ tables, generate n-ary delta SQL following
the Delta-WCOJ formula instead of nested binary decomposition.

**What changes:**

1. **N-ary join detection** — In `src/dvm/parser/mod.rs`, detect chains of
   inner equi-joins with 4+ tables. Store as `OpTree::MultiWayJoin { tables,
   conditions, ... }` alongside the existing binary `OpTree::Join`.

2. **Variable ordering** — New function `choose_variable_order(tables,
   conditions, delta_table)` in `src/dvm/operators/multiway_join.rs`. For
   each delta source $\Delta R_i$, choose an attribute enumeration order
   that minimizes intermediate sizes. Heuristic: start with attributes from
   the delta table, then high-selectivity join keys.

3. **LFTJ-style SQL generation** — For each delta source, generate a chain
   of CTEs where each level binds one more variable via `EXISTS`
   intersections:

   ```sql
   -- Level 0: seed from delta
   WITH l0 AS (SELECT DISTINCT a, b FROM delta_R),
   -- Level 1: intersect 'b' with table S
   l1 AS (
     SELECT l0.a, l0.b, S.c
     FROM l0 JOIN S ON S.b = l0.b
     WHERE EXISTS (SELECT 1 FROM T WHERE T.a = l0.a AND T.c = S.c)
   ),
   ...
   ```

4. **Part 1 / Part 2 unification** — The n-ary delta rule naturally handles
   both sides. Each delta source $\Delta R_i$ produces one CTE chain; the
   results are `UNION ALL`'d together with appropriate `op` labels.

5. **Fallback** — If the join graph contains non-equi conditions, outer
   joins, or lateral references, fall back to binary decomposition.

**Expected impact:**
- Q05/Q07/Q08/Q09: 10–100× improvement at SF=1.0 (eliminates intermediate
  cardinality explosion)
- Acyclic queries: modest improvement (binary plans are near-optimal for
  these)
- Cyclic queries: optimal asymptotic behavior

**Validation:** TPC-H 22/22 pass. Property tests with randomized schemas.

### Phase 3: Free Join Hybrid Strategy (3–4 days)

**Goal:** Automatically choose between binary decomposition and n-ary LFTJ
based on the query structure.

**What changes:**

1. **Cycle detection on join graph** — In the parser, compute the GHD
   (generalized hypertree decomposition) of the join query. If the width
   is 1 (acyclic), use binary plan. If width > 1 (cyclic), use LFTJ.

2. **Subgraph decomposition** — For queries with both acyclic and cyclic
   subpatterns, decompose into components and apply the appropriate strategy
   to each.

3. **Cost heuristic** — When both strategies apply, estimate the intermediate
   size using AGM bound (WCOJ) vs. binary bound and choose the cheaper one.

**Expected impact:** Never regresses vs. current strategy. Strictly improves
on cyclic subpatterns.

### Phase 4: Factorized Intermediate CTEs (2–3 days)

**Goal:** Reduce CTE sizes by carrying only join keys through intermediate
stages, assembling full rows in the final projection.

**What changes:**

1. **Key-only CTE mode** — New option in `DiffContext` that generates CTEs
   carrying only `(op, pk_columns, join_keys)` instead of all output columns.

2. **Final row assembly** — After the delta CTE chain completes, a single
   final CTE joins back to base tables using PKs to retrieve full rows.

3. **Applicability check** — Only applies when the final projection needs
   columns from 3+ tables (otherwise the current approach is fine).

**Expected impact:**
- 30–50% reduction in temp file usage for 5+ table joins
- Faster CTE materialization (fewer columns to hash/sort)

---

## 6. Evaluation Plan

### 6.1 Benchmarks

| Benchmark | Purpose | Target |
|-----------|---------|--------|
| TPC-H SF=0.01 | Regression check | No query regresses > 10% |
| TPC-H SF=0.1 | Scale behavior | All 22 queries: DIFF ≤ 2× FULL |
| TPC-H SF=1.0 | Scale behavior | Threshold-collapse queries (Q05/Q07/Q08/Q09): DIFF ≤ 5× FULL |
| Triangle query | Cyclic join test | 3-way self-join on 100K edges: DIFF < 100ms |
| Star schema (8 dim) | Wide join test | 8-table star join: DIFF < 2× FULL |

### 6.2 Correctness Validation

1. **DIFF ≡ FULL equivalence** — For every test query, verify that
   differential refresh produces identical results to full refresh.
2. **Property tests** — Randomized schemas with 3–8 tables, random data,
   random deltas. Check DIFF ≡ FULL for 1000+ iterations.
3. **Soak test** — G17-SOAK with WCOJ-eligible queries running for 24h.

### 6.3 Metrics

- **CTE count** per delta SQL query (target: 50% reduction for 6+ table joins)
- **Peak temp file usage** (target: stay within `temp_file_limit` at SF=1.0)
- **Intermediate row count** per CTE (target: bounded by AGM(Q) × |Δ|)

---

## 7. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| PostgreSQL planner chooses bad plan for LFTJ-style SQL | Medium | High | Add `pgtrickle.delta_work_mem` GUC; test with `SET enable_nestloop = off` |
| EXISTS-chain SQL is slower than hash join for low selectivity | Medium | Medium | Cost heuristic: fall back to binary when selectivity > 10% |
| Correctness bug in n-ary delta rule | Low | Critical | Property tests + DIFF≡FULL validation on every commit |
| Variable ordering heuristic picks wrong order | Medium | Medium | Try multiple orders during testing; pick best via EXPLAIN ANALYZE |
| Outer joins cannot use LFTJ | Low | Low | Fall back to binary decomposition (existing code) |
| Increased code complexity | High | Medium | Keep binary path as default; WCOJ as opt-in initially |

---

## 8. Effort Estimates

| Phase | Item | Days | Confidence | Prerequisite |
|-------|------|------|------------|--------------|
| 1 | Yannakakis semi-join reduction pass | 2–3 | High | — |
| 1 | Validation (TPC-H 22/22) | 1 | High | above |
| 2 | N-ary join detection in parser | 1–2 | High | — |
| 2 | Variable ordering heuristic | 1–2 | Medium | above |
| 2 | LFTJ-style SQL generation | 3–4 | Medium | above |
| 2 | Delta-WCOJ integration | 2–3 | Medium | above |
| 2 | Validation (TPC-H + property tests) | 2 | High | above |
| 3 | GHD computation + Free Join selection | 2–3 | Medium | Phase 2 |
| 3 | Cost heuristic integration | 1 | Medium | above |
| 4 | Key-only CTE mode | 1–2 | High | Phase 2 |
| 4 | Final row assembly | 1 | High | above |

**Total: ~17–24 days** (Phases 1–4). Phases 1 and 2 are independent and can
overlap. Phase 3 depends on Phase 2. Phase 4 is independent of Phase 3.

**Recommended implementation order:**
1. Phase 1 (quick win, 3–4 days, zero risk)
2. Phase 2 (core algorithmic change, 7–11 days)
3. Phase 4 (CTE optimization, 2–3 days)
4. Phase 3 (hybrid strategy, 3–4 days, nice-to-have)

---

## 9. References

### Primary — Worst-Case Optimal Joins

1. **Veldhuizen, T.L. (2014).** "Leapfrog Triejoin: A Worst-Case Optimal
   Join Algorithm." *ICDT 2014.* [arXiv:1210.0481](https://arxiv.org/abs/1210.0481)

2. **Ngo, H.Q., Porat, E., Ré, C., & Rudra, A. (2012).** "Worst-Case
   Optimal Join Algorithms." *PODS 2012.*
   [arXiv:1203.1952](https://arxiv.org/abs/1203.1952)

3. **Ngo, H.Q., Ré, C., & Rudra, A. (2014).** "Skew Strikes Back: New
   Developments in the Theory of Join Algorithms." *SIGMOD Record*, 42(4).
   [arXiv:1310.3314](https://arxiv.org/abs/1310.3314)

4. **Atserias, A., Grohe, M., & Marx, D. (2008).** "Size Bounds and Query
   Plans for Relational Joins." *FOCS 2008.*

### Instance-Optimal Joins

5. **Abo Khamis, M., Ngo, H.Q., Ré, C., & Rudra, A. (2016).**
   "Joins via Geometric Resolutions: Worst-Case and Beyond." *PODS 2016.*
   (Minesweeper algorithm)

6. **Abo Khamis, M., Ngo, H.Q., Ré, C., & Rudra, A. (2018).** "What Do
   Shannon-type Inequalities, Submodular Width, and Disjunctive Datalog
   Have to Do with One Another?" *PODS 2018.* (Tetris algorithm)

### Hybrid & Practical Approaches

7. **Wang, Y., Willsey, M., & Suciu, D. (2023).** "Free Join: Unifying
   Worst-Case Optimal and Traditional Joins." *SIGMOD 2023.*

8. **Aberger, C.R., Tu, S., Olukotun, K., & Ré, C. (2016).** "EmptyHeaded:
   A Relational Engine for Graph Processing." *SIGMOD 2016.*
   [arXiv:1503.02368](https://arxiv.org/abs/1503.02368)

9. **Yannakakis, M. (1981).** "Algorithms for Acyclic Database Schemes."
   *VLDB 1981.*

### Factorized Databases

10. **Olteanu, D. & Závodný, J. (2015).** "Size Bounds for Factorised
    Representations of Query Results." *ACM TODS*, 40(1).

### Incremental WCOJ

11. **Kara, A., Ngo, H.Q., Nikolic, M., Olteanu, D., & Zhang, H. (2023).**
    "Maintaining Triangle Queries under Updates." *ACM TODS*, 48(3).
    (Delta-WCOJ foundations)

### IVM Foundations (already cited in pg_trickle)

12. **Budiu, M., Ryzhyk, L., McSherry, F., & Tannen, V. (2023).** "DBSP:
    Automatic Incremental View Maintenance for Rich Query Languages."
    *PVLDB*, 16(7). [arXiv:2203.16684](https://arxiv.org/abs/2203.16684)

13. **Koch, C. et al. (2014).** "DBToaster: Higher-order Delta Processing
    for Dynamic, Frequently Fresh Views." *VLDB Journal*, 23(2).
