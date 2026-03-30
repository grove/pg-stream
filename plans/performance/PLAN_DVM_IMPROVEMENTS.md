# PLAN: DVM Engine Improvements — Reducing Delta SQL Intermediate Cardinality

**Date:** 2025-07-22  
**Status:** Planning  
**Scope:** Reduce temporary data volume in generated delta SQL, targeting the
multi-GB temp file spill that blocks TPC-H Q05/Q09 in DIFFERENTIAL mode and
the O(n²) blowup on correlated semi-joins (Q20).

---

## Table of Contents

1. [Motivation](#1-motivation)
2. [Current Architecture](#2-current-architecture)
3. [Bottleneck Analysis](#3-bottleneck-analysis)
4. [Proposals](#4-proposals)
   - [DI-1: Materialize Per-Leaf L₀ Snapshots](#di-1-materialize-per-leaf-l₀-snapshots)
   - [DI-2: Pre-Image Capture from Change Buffer](#di-2-pre-image-capture-from-change-buffer)
   - [DI-3: Group-Key Filtered Aggregate Old Rescan](#di-3-group-key-filtered-aggregate-old-rescan)
   - [DI-4: Shared R₀ CTE Across Join Parts](#di-4-shared-r₀-cte-across-join-parts)
   - [DI-5: Part 3 Correction Consolidation](#di-5-part-3-correction-consolidation)
   - [DI-6: Lazy Semi-Join R_old Materialization](#di-6-lazy-semi-join-r_old-materialization)
   - [DI-7: Scan-Count-Aware Strategy Selector](#di-7-scan-count-aware-strategy-selector)
5. [Dependency Graph](#5-dependency-graph)
6. [Priority & Schedule](#6-priority--schedule)
7. [Background: EC-01 and EC-01B](#7-background-ec-01-and-ec-01b)

---

## 1. Motivation

TPC-H at SF=0.01 demonstrates two classes of delta SQL cardinality problems:

| Query | Tables | Issue | Symptom |
|-------|--------|-------|---------|
| Q05 | 6-way join (supplier→lineitem→orders→customer→nation→region) | Per-leaf EXCEPT ALL computed 3× per join node; cascading CTEs spill to disk | `temp_file_limit (4194304kB)` exceeded |
| Q09 | 6-way join (nation→supplier→partsupp→lineitem→orders→part) | Same pattern as Q05 | `temp_file_limit (4194304kB)` exceeded |
| Q20 | Doubly-nested correlated semi-join | R_old MATERIALIZED for both EXISTS levels; EXCEPT ALL inside inner semi-join | 6824ms DIFF vs 15ms FULL (0.00× speedup) |
| Q07/Q08 | 5–6 way join | DIFF vs IMMEDIATE skipped | `temp_file_limit` exceeded in cross-mode comparison |

Raising `temp_file_limit` above 4 GB may allow these queries to complete but
at extreme I/O cost. The sustainable fix is reducing the intermediate data
volume the delta SQL produces.

### Impact

Solving Q05/Q09 would bring TPC-H DIFFERENTIAL correctness from 20/22 to
22/22, meeting the v0.13.0 exit criterion. Solving Q20 would remove the most
severe performance outlier from the benchmark suite.

---

## 2. Current Architecture

### 2.1 Join Delta Formula

For an inner join `J = L ⋈ R`, the delta is computed as:

```
ΔJ = (ΔL_ins ⋈ R₁) ∪ (ΔL_del ⋈ R₀)     -- Part 1a + 1b (EC-01 split)
   ∪ (L₀ ⋈ ΔR)                            -- Part 2
   - correction                            -- Part 3 (nested joins only)
```

Where:
- **R₁** = post-change snapshot (current table state)  
- **R₀** = pre-change snapshot = R₁ EXCEPT ALL ΔR_ins UNION ALL ΔR_del  
- **L₀** = pre-change snapshot of left child (same formula, recursive)  
- **Part 3 correction** = ΔL ⋈ ΔR (removes double-counted rows)

### 2.2 Pre-Change Snapshot Strategy (EC-01B)

Since v0.12.0, the `use_pre_change_snapshot` function in `join_common.rs`
applies per-leaf CTE reconstruction for **all** join depths—no scan-count
threshold. Each leaf Scan's L₀ is computed as:

```sql
(SELECT * FROM base_table
 EXCEPT ALL SELECT cols FROM delta WHERE action='I'
 UNION ALL SELECT cols FROM delta WHERE action='D')
```

SemiJoin/AntiJoin-containing subtrees fall back to post-change snapshots
(L₁/R₁) with correction terms to avoid the Q21 numwait regression.

### 2.3 CTE Volume for a 6-Table Join (Q05-like)

A 6-table inner join chain `((((A ⋈ B) ⋈ C) ⋈ D) ⋈ E) ⋈ F` where table C
changes produces approximately:

| CTE Category | Count | Notes |
|-------------|-------|-------|
| Delta capture (change buffer reads) | 2 | ins + del from `pgtrickle_changes` |
| Per-leaf L₀ snapshots | 5 | One per non-changed leaf (A, B, D, E, F) |
| Per-node Part 1a (ins ⋈ R₁) | 5 | One per join node on the delta path |
| Per-node Part 1b (del ⋈ R₀) | 5 | One per join node; each references per-leaf L₀ |
| Per-node Part 2 (L₀ ⋈ ΔR) | 5 | Each expands L₀ inline |
| Part 3 corrections | 2–4 | For shallow nested joins |
| UNION ALL assembly | 5 | Combining Part 1+2+3 per node |

**Total: ~22–30 CTEs**, most referencing the same per-leaf L₀ snapshot
expressions inline rather than as named CTEs. The planner may evaluate each
inline reference separately, causing the same EXCEPT ALL to execute multiple
times per join node.

### 2.4 Aggregate Rescan Paths

Two paths in `aggregate.rs`:

- **Algebraic path** (COUNT, SUM, AVG): `old = new - ins + del`. No EXCEPT ALL
  needed. Uses `new_rescan` CTE only (post-change aggregate).
- **Non-algebraic path** (MIN, MAX, BIT_AND, STRING_AGG, etc.): Computes
  `old_rescan` via full EXCEPT ALL on the child's data plus re-aggregation.
  This second `diff_node(child)` call duplicates the entire child CTE tree.

---

## 3. Bottleneck Analysis

Four locations cause intermediate cardinality explosion:

### 3.1 Repeated Per-Leaf L₀ Inline Expansion

Each join node's Part 1b and Part 2 reference L₀ of their respective children.
For a 6-table join, the innermost leaf's L₀ is:

```
L₀_A = A EXCEPT ALL delta_A_ins UNION ALL delta_A_del
```

This expression appears **inline** at every join node that needs the left
child's pre-change state. With 5 join nodes × 2 references (Part 1b + Part 2),
a single leaf's EXCEPT ALL can be evaluated up to 10 times.

**Estimated waste:** 5–10× redundant base-table scans for unchanged leaves.

### 3.2 R₀ Recomputation in Part 1b

Part 1b computes `ΔL_del ⋈ R₀`, where R₀ is the pre-change right child.
For a deep right subtree, R₀ involves cascading EXCEPT ALL operations.
Part 2 also needs L₀ which may share structure with R₀ at the parent level
but is computed independently.

**Current code:** `build_pre_change_snapshot_sql()` in `join_common.rs`
constructs the snapshot SQL string each time it's called, producing a fresh
inline subquery. Two calls for the same subtree produce textually identical
but separately-planned subqueries.

### 3.3 Non-Algebraic Aggregate Old Rescan

When `is_algebraically_invertible` returns false (MIN, MAX, BIT_AND,
STRING_AGG), the aggregate operator calls `ctx.diff_node(child)` a second
time to get delta column names, then wraps the entire child FROM with:

```sql
SELECT aggs FROM (
  SELECT * FROM child_source
  EXCEPT ALL SELECT cols FROM child_delta WHERE action='I'
  UNION ALL SELECT cols FROM child_delta WHERE action='D'
) old_source
GROUP BY group_cols
```

This rescans the full child extent to compute the old aggregate. For a join
child, this means re-executing the entire join on full data.

**Key issue:** The GROUP BY produces one row per group, but the EXCEPT ALL
inside operates on the full pre-GROUP-BY cardinality. If only a few groups
are affected, most of this work is wasted.

### 3.4 Semi-Join MATERIALIZED R_old

Semi-joins (`EXISTS (SELECT ...)`) always build a `MATERIALIZED` R_old CTE:

```sql
r_old AS MATERIALIZED (
  SELECT * FROM right_source
  EXCEPT ALL SELECT cols FROM right_delta WHERE action = 'I'
  UNION ALL SELECT cols FROM right_delta WHERE action = 'D'
)
```

For nested semi-joins (Q20: `EXISTS(... EXISTS(...))`) this materializes at
each level. The inner level's R_old is the full inner subquery result minus
deltas — potentially large.

---

## 4. Proposals

### DI-1: Materialize Per-Leaf L₀ Snapshots

**Problem:** Per-leaf L₀ (pre-change base table snapshot) is computed inline
at every reference site. PostgreSQL evaluates each inline subquery
independently, causing redundant EXCEPT ALL + full table scans.

**Proposal:** Emit each per-leaf L₀ as a **named CTE** (with NOT MATERIALIZED
hint to let the planner fold it when beneficial, or MATERIALIZED when the
reference count exceeds a threshold, e.g. ≥3).

**Implementation:**
1. In `build_pre_change_snapshot_sql()` (`join_common.rs:399`), instead of
   returning inline SQL, register a named CTE via `ctx.add_cte()` and return
   the CTE name.
2. Track reference counts per leaf. If a leaf's L₀ is referenced ≥3 times,
   mark it MATERIALIZED.
3. Unchanged leaves (no delta rows) can skip EXCEPT ALL entirely — their L₀
   equals the current table. Use delta-branch pruning (already implemented
   in B3-1) to detect this.

**Estimated impact:** 20–40% reduction in intermediate data for 4+ table joins.
For Q05/Q09 (6-way), eliminates ~5× redundant full-table scans per leaf.

**Effort:** Medium (2–3 days). Core change is in `build_pre_change_snapshot_sql`
and `diff_inner_join`. Must verify that named CTEs don't break column
disambiguation.

**Risk:** Low. CTE naming is well-established in the diff engine. The main
risk is column aliasing — the returned CTE must carry disambiguated column
names matching what inline SQL currently produces.

**Prerequisite:** None. Can be done independently.

---

### DI-2: Pre-Image Capture from Change Buffer

**Problem:** EXCEPT ALL is fundamentally expensive — it requires sorting or
hashing the full base table to subtract delta inserts. For large tables
this dominates refresh time.

**Proposal:** Capture the **old row values** (pre-image) in the CDC trigger
alongside the new values already captured. The change buffer would store
both old and new tuples, enabling direct computation of L₀ rows without
any EXCEPT ALL against the base table.

**Implementation:**
1. Modify the AFTER trigger in `cdc.rs` to capture `OLD.*` for UPDATE and
   DELETE operations (INSERT captures `NEW.*` only, as today).
2. Extend the change buffer schema to include `old_row` columns (or a JSONB
   `old_values` column).
3. In delta SQL generation, replace:
   ```sql
   SELECT * FROM base_table
   EXCEPT ALL SELECT cols FROM delta WHERE action='I'
   UNION ALL SELECT cols FROM delta WHERE action='D'
   ```
   with:
   ```sql
   SELECT * FROM base_table  -- already correct for inserts
   -- For updates: old values come from change buffer directly
   -- For deletes: old values come from change buffer directly
   ```
   Effectively, L₀ = current table state with targeted row replacements
   from the change buffer, no set-difference needed.

**Estimated impact:** Eliminates EXCEPT ALL entirely. For Q05/Q09, removes
the single largest source of intermediate data. Expected 50–80% reduction
in temp file usage for deep joins.

**Effort:** High (1–2 weeks). Requires CDC trigger changes, change buffer
schema migration, delta SQL generator rewrite for pre-image mode, and
comprehensive testing of UPDATE edge cases (partial column updates, TOAST
columns, nullable columns).

**Risk:** Medium-High.
- CDC trigger overhead increases (capturing OLD.* doubles the per-row cost
  for UPDATEs and DELETEs).
- TOAST columns: PostgreSQL doesn't detoast unchanged columns in OLD, so
  the pre-image may contain compressed/external pointers that can't be
  used directly in equality comparisons.
- Schema migration: existing change buffers need a migration path.
- This is a fundamental CDC architecture change.

**Prerequisite:** None, but conflicts with DI-1 (which optimizes the current
EXCEPT ALL approach). DI-1 should be done first as a cheaper interim fix.

**Target:** v1.x (deferred — aligns with existing ADR decision).

---

### DI-3: Group-Key Filtered Aggregate Old Rescan

**Problem:** Non-algebraic aggregates (MIN, MAX, STRING_AGG, etc.) compute
`old_rescan` by rescanning the **entire** child data through EXCEPT ALL +
GROUP BY. If only 3 groups out of 10,000 are affected by the current delta,
99.97% of the rescan work is wasted.

**Proposal:** Filter the old rescan to only groups that appear in the delta:

```sql
agg_old AS (
  SELECT aggs
  FROM (
    SELECT * FROM child_source
    WHERE (group_col1, group_col2) IN (
      SELECT group_col1, group_col2 FROM delta_cte
    )
    EXCEPT ALL
    SELECT cols FROM child_delta WHERE action='I'
    UNION ALL
    SELECT cols FROM child_delta WHERE action='D'
  ) old
  GROUP BY group_cols
)
```

The `WHERE ... IN (SELECT ... FROM delta_cte)` clause restricts the base table
scan to only rows belonging to affected groups before the EXCEPT ALL.

**Implementation:**
1. In `aggregate.rs`, non-algebraic branch (~line 750), add a WHERE clause
   to the FROM subquery that filters on group keys present in `delta_cte`.
2. Extract group column names from `group_output` and `delta_cte`.
3. For single-group (no GROUP BY) aggregates, skip the optimization (all rows
   are in one group).

**Estimated impact:** Proportional to the ratio of affected groups to total
groups. At 1% change rate with uniform distribution: ~99% reduction in
old_rescan volume. Real-world impact depends on data distribution.

**Effort:** Low (0.5–1 day). Small, localized change in the non-algebraic
branch of `diff_aggregate`.

**Risk:** Low. The group-key filter is a pure optimization — it doesn't change
the EXCEPT ALL semantics, just reduces its input. Edge case: if the group
column has NULLs, the IN clause must use `IS NOT DISTINCT FROM` semantics
(which the existing join conditions already handle).

**Prerequisite:** None. Independent of all other proposals.

---

### DI-4: Shared R₀ CTE Across Join Parts

**Problem:** In `diff_inner_join`, Part 1b computes `ΔL_del ⋈ R₀` and Part 2
computes `L₀ ⋈ ΔR`. Both reference the pre-change snapshot of their
respective sides. When the **right** child is a join subtree (not a simple
Scan), R₀ is a complex inline subquery that gets emitted twice — once for
Part 1b and once for Part 2's usage of R₀ in the parent's perspective.

Actually the duplication is more subtle: at each join node, Part 1b needs
R₀ (pre-change right), and Part 2 needs L₀ (pre-change left). At the
**parent** join node, the current node's result is either L or R, and the
parent may compute its snapshot inline again.

**Proposal:** When `build_pre_change_snapshot_sql()` is called for a subtree
that has already been computed, return the existing CTE name instead of
generating a new inline expression. This requires a cache keyed by OpTree
node identity.

**Implementation:**
1. Add a `snapshot_cache: HashMap<usize, String>` to `DiffContext` (keyed by
   OpTree pointer or node ID).
2. In `build_pre_change_snapshot_sql()`, check the cache before generating SQL.
3. On first computation, register as a named CTE and cache the name.

**Estimated impact:** 10–20% reduction for 4+ table joins. Eliminates ~1
redundant snapshot computation per shared subtree.

**Effort:** Medium (1–2 days). Requires introducing a cache mechanism and
ensuring OpTree identity is stable across calls.

**Risk:** Low-Medium. The main risk is cache invalidation — the snapshot
must correspond to the correct point in the delta computation (pre-change
vs post-change). Since we only cache L₀/R₀ (pre-change), and the pre-change
state is fixed for the entire delta computation, this should be safe.

**Prerequisite:** DI-1 (named CTE for snapshots makes caching natural).

---

### DI-5: Part 3 Correction Consolidation

**Problem:** Part 3 (correction for double-counted rows in nested joins)
generates separate CTEs for each join node in the chain. For a 6-table
join, this can produce 2–4 correction CTEs, each referencing deltas from
both left and right children.

**Proposal:** Consolidate Part 3 corrections for adjacent join nodes in a
linear chain into a single correction CTE that handles all overlapping
deltas at once.

**Implementation:**
1. In `diff_inner_join`, detect when both left and right children are also
   inner joins (linear chain pattern).
2. For linear chains, emit a single correction CTE that joins all deltas
   with appropriate conditions, instead of per-node corrections.
3. Fall back to per-node corrections for non-linear (bushy) join trees.

**Estimated impact:** 5–10% reduction in CTE count for linear join chains.
Modest impact on data volume since Part 3 corrections typically have low
cardinality (they process only the intersection of left and right deltas).

**Effort:** Medium (2–3 days). Requires understanding the correction term
algebra for chains and proving the consolidated version is equivalent.

**Risk:** Medium. Correctness of the consolidated correction is non-trivial
to verify. The existing per-node approach is well-tested. Incorrect
consolidation would cause silent data corruption of type
insert duplication or missed deletes.

**Prerequisite:** None, but should be validated against TPC-H Q05/Q07/Q08/Q09
to confirm the correction volume is actually significant enough to warrant
this complexity.

---

### DI-6: Lazy Semi-Join R_old Materialization

**Problem:** Semi-join differentiation always materializes `R_old`:

```sql
r_old AS MATERIALIZED (
  SELECT * FROM right_source
  EXCEPT ALL SELECT cols FROM right_delta WHERE action='I'
  UNION ALL SELECT cols FROM right_delta WHERE action='D'
)
```

For EXISTS subqueries, the right side often has high cardinality but the
semi-join only needs to prove existence. Materializing the full R_old is
wasteful when only a few rows from the left delta need to be checked.

**Proposal:** Replace unconditional MATERIALIZED with a heuristic:
- If the right side has a delta (changes occurred), keep MATERIALIZED
  (correctness requires the pre-change snapshot).
- If the right side has **no** delta (no changes to the EXISTS subquery
  tables), skip the EXCEPT ALL entirely and use the current table directly.
  The existing delta-branch pruning (B3-1) should already handle this, but
  the materialization hint is still applied.

Additionally, for cases where R_old must be computed, add a semi-join push-down:

```sql
r_old AS (
  SELECT * FROM right_source
  WHERE right_key IN (SELECT left_key FROM left_delta)
  EXCEPT ALL SELECT cols FROM right_delta WHERE action='I'
  UNION ALL SELECT cols FROM right_delta WHERE action='D'
)
```

This restricts R_old to only rows that could potentially match the left delta,
reducing materialization volume.

**Implementation:**
1. In `semi_join.rs`, check if the right child has any changes (via delta
   source tracking). If not, emit R₁ directly without EXCEPT ALL.
2. For changed right children, extract the equi-join key from the semi-join
   condition and add a `WHERE key IN (...)` filter before EXCEPT ALL.
3. Remove the MATERIALIZED hint when the filtered R_old is expected to be
   small (< estimated threshold).

**Estimated impact:** For Q20-type queries, 50–80% reduction in R_old volume.
The semi-join key filter restricts materialization to only matching rows.

**Effort:** Medium (1–2 days). The equi-join key extraction is already
implemented for inner joins (EC-01). Porting to semi-joins is straightforward.

**Risk:** Low-Medium. The key filter is a pure restriction and doesn't change
correctness. Risk is in the semi-join condition parsing — if the condition
isn't a simple equi-join (e.g., `EXISTS (SELECT 1 WHERE correlated_expr)`),
the filter can't be applied. Need a fallback to the current behavior.

**Prerequisite:** None. Independent of other proposals.

---

### DI-7: Scan-Count-Aware Strategy Selector

**Problem:** The current DVM engine applies the same delta strategy regardless
of join tree complexity. A 2-table join and a 10-table join both use per-leaf
EXCEPT ALL reconstruction. The marginal cost of each additional table is
super-linear due to CTE explosion.

**Proposal:** Introduce a configurable complexity threshold that switches delta
strategy based on join tree characteristics:

| Scan Count | Strategy |
|-----------|----------|
| 1–3 | Full per-leaf EXCEPT ALL (current, optimal for small joins) |
| 4–6 | Named CTE L₀ with materialization (DI-1) + group-key filtering (DI-3) |
| 7+ | Automatic fallback to FULL refresh for the affected stream table |

Additionally, expose a per-stream-table GUC or catalog option:
```sql
SELECT pgtrickle.alter_stream_table('my_complex_view',
  refresh_mode => 'auto',       -- default: try DIFFERENTIAL, fall back
  max_differential_joins => 6   -- above this, use FULL refresh
);
```

**Implementation:**
1. Add `join_scan_count()` call at the start of `diff_node()` for join trees.
2. If count exceeds threshold, return a `DiffResult` that signals "use FULL".
3. Add a `max_differential_joins` column to `pgtrickle.pgt_stream_tables`.
4. Default threshold: 6 (covers Q07/Q08 but falls back for pathological cases).

**Estimated impact:** Prevents pathological blowup for very complex views.
Acts as a safety net rather than a performance optimization per se.

**Effort:** Low (1 day). The `join_scan_count` function already exists.

**Risk:** Low. This is a conservative fallback — it reduces the blast radius
of complex delta SQL rather than trying to optimize it.

**Prerequisite:** None, but most valuable **after** DI-1/DI-3/DI-6 raise the
practical threshold for what DIFFERENTIAL mode can handle.

---

## 5. Dependency Graph

```
DI-1 (Named CTE L₀)
 └──→ DI-4 (Shared R₀ cache) ──→ DI-2 (Pre-image capture, v1.x)

DI-3 (Group-key aggregate filter) — independent
DI-5 (Part 3 consolidation) — independent
DI-6 (Lazy semi-join R_old) — independent
DI-7 (Strategy selector) — after DI-1, DI-3, DI-6
```

DI-1, DI-3, DI-5, DI-6 can all be developed in parallel.
DI-4 depends on DI-1. DI-7 should come last (tuning after optimizations land).
DI-2 is a v1.x architectural change; all others are v0.x candidates.

---

## 6. Priority & Schedule

| Priority | Proposal | Impact | Effort | Target |
|----------|----------|--------|--------|--------|
| P0 | DI-1: Named CTE L₀ | High (Q05/Q09) | Medium | v0.13 |
| P0 | DI-3: Group-key aggregate filter | Medium-High | Low | v0.13 |
| P1 | DI-6: Lazy semi-join R_old | High (Q20) | Medium | v0.13 |
| P1 | DI-4: Shared R₀ cache | Medium | Medium | v0.13 |
| P2 | DI-7: Strategy selector | Safety net | Low | v0.13 |
| P2 | DI-5: Part 3 consolidation | Low-Medium | Medium | v0.13 |
| P3 | DI-2: Pre-image capture | Very high | High | v1.x |

**Recommended sequence for v0.13:**
1. DI-1 (unblocks DI-4, biggest single improvement for deep joins)
2. DI-3 (independent, small, high ROI for aggregate queries)
3. DI-6 (semi-join optimization, improves Q20 and similar)
4. DI-4 (builds on DI-1's CTE infrastructure)
5. DI-7 (safety net after other optimizations raise the bar)

**Validation gate:** After DI-1 + DI-3, re-run TPC-H at SF=0.01 with
`temp_file_limit = '4GB'`. If Q05/Q09 pass, the v0.13.0 correctness gate
(22/22) is met. If not, temporarily raise
`temp_file_limit` and measure actual disk usage delta.

---

## 7. Background: EC-01 and EC-01B

### EC-01: Join Key Change Split (v0.10.0) — DONE

Split Part 1 into Part 1a (inserts ⋈ R₁) and Part 1b (deletes ⋈ R₀) so
that updates changing the join key propagate the correct pre-change and
post-change join results.

**Code:** `diff_inner_join()` in `src/dvm/operators/join.rs:109+`

### EC-01B: Per-Leaf CTE Snapshot (v0.12.0) — DONE

Removed the `join_scan_count <= 2` threshold that previously limited
per-leaf snapshot reconstruction to small subtrees. Now uses per-leaf
EXCEPT ALL for **all** join depths, including deep chains (Q07/Q08/Q09).

SemiJoin/AntiJoin subtrees still fall back to post-change snapshots to
avoid the Q21 numwait regression.

**Code:** `use_pre_change_snapshot()` in `src/dvm/operators/join_common.rs:1342+`

### Pre-Image Capture (deferred to v1.x)

Capture OLD row values in the CDC trigger to eliminate EXCEPT ALL entirely.
See DI-2 above and ADR-001/ADR-002 in `plans/adrs/PLAN_ADRS.md`.
