# Plan: Stratified Evaluation for pg_trickle

Date: 2026-03-24
Status: EXPLORATION
Last Updated: 2026-03-24

---

## 1. Problem Statement

### What We Have Today

pg_trickle uses an implicit two-level stratification:

- **Within SCCs**: Only monotone queries are allowed (enforced by
  `check_monotonicity()`). Non-monotone operators (Aggregate, EXCEPT,
  Window, AntiJoin) in cycle members are rejected outright.
- **Across SCCs**: The condensation DAG processes upstream SCCs before
  downstream ones, ensuring converged inputs. Non-monotone operators
  are allowed in acyclic stream tables.

This works well but leaves three concrete gaps:

**Gap 1 -- Overly Conservative Cycle Rejection.** A cycle containing
`NOT EXISTS` is always rejected, even when the negated relation is
*outside* the cycle (in a lower stratum). The monotonicity check is
applied to the entire defining query, not to the specific edges that
form the cycle.

**Gap 2 -- Wasted Downstream Refreshes.** When an upstream SCC converges
with zero net changes, downstream stream tables still execute a full
refresh cycle before discovering there is nothing to do. The scheduler
lacks explicit stratum boundaries that would allow skipping entire
downstream layers.

**Gap 3 -- Non-Monotone Recursive CTEs Always Recompute.** A recursive
CTE with a non-monotone recursive term (e.g., `EXCEPT` for cycle
elimination in graph traversal) falls back to full recomputation even
when the non-monotone sub-expression only references converged (lower
stratum) data within the CTE itself.

### What Stratification Would Give Us

Explicit stratification would:

1. **Unlock new query patterns in cycles** -- allow EXCEPT, NOT EXISTS,
   and aggregation in cycle members when the non-monotone dependency
   targets a lower stratum (already converged data).
2. **Reduce wasted work** -- skip entire strata of downstream stream
   tables when upstream strata produce no changes.
3. **Enable incremental maintenance for some non-monotone recursive
   CTEs** -- use DRed instead of recomputation when the non-monotone
   operator is stratifiable.
4. **Provide better diagnostics** -- show users exactly which stratum
   each stream table belongs to, and which edges prevent cycle
   formation.

---

## 2. Background: Datalog Stratification Theory

### 2.1. Definitions

A **stratification** of a set of rules (or stream tables) is an assignment
of each rule to a non-negative integer **stratum** such that:

- If rule R positively depends on rule S (monotone edge), then
  `stratum(R) >= stratum(S)`.
- If rule R negatively depends on rule S (non-monotone edge), then
  `stratum(R) > stratum(S)`.

A program is **stratifiable** if such an assignment exists. Equivalently,
the program is stratifiable if and only if no cycle in the dependency
graph passes through a non-monotone edge.

### 2.2. Evaluation Order

Stratified evaluation processes strata in order:

```
for s in 0, 1, 2, ...:
    compute all rules in stratum s to their fixed point
    (using semi-naive evaluation within the stratum)
    freeze results -- lower strata are now immutable constants
```

Rules in stratum `s` may read from strata `< s` as constants (no
iteration needed) and from stratum `s` peers via fixed-point iteration.
Non-monotone reads are only allowed from strata `< s`.

### 2.3. Relationship to pg_trickle's Architecture

| Datalog Concept | pg_trickle Equivalent |
|-----------------|----------------------|
| Rule | Stream table defining query |
| Positive dependency | Source reference to another ST (JOIN, UNION) |
| Negative dependency | Source reference via EXCEPT, NOT EXISTS, aggregate |
| Stratum | Group of STs that can safely be co-iterated |
| Fixed-point within stratum | SCC iteration (`execute_worker_cyclic_scc()`) |
| Frozen lower-stratum result | Upstream ST storage (read as plain table) |

### 2.4. Relevance to DBSP

In the DBSP framework (Budiu et al., 2023), stratification corresponds to
a **layered nested trace** where each layer's feedback loop closes
independently. The outer layer integrates converged inner-layer outputs
as constants. pg_trickle's SCC-based scheduling is already a concrete
implementation of this pattern; making stratification explicit simply
formalizes and extends it.

---

## 3. Current Architecture Analysis

### 3.1. Dependency Storage

The `pgt_dependencies` table stores edges with these relevant fields:

| Column | Type | Relevant Info |
|--------|------|---------------|
| `pgt_id` | BIGINT | Downstream stream table |
| `source_relid` | OID | Upstream source table |
| `source_type` | TEXT | TABLE / STREAM_TABLE / VIEW / etc. |
| `columns_used` | TEXT[] | Which columns are read |

**Missing**: No field records whether the dependency is **monotone or
non-monotone**. All edges are treated identically for DAG construction.

### 3.2. DAG Construction

`StDag::build_from_catalog()` queries `pgt_dependencies` and builds a
homogeneous directed graph. Edge type (monotone vs. non-monotone) is not
preserved -- only topology (who depends on whom) is materialized.

### 3.3. Monotonicity Check

`check_monotonicity()` in `src/dvm/parser.rs` walks an entire `OpTree`
and rejects any non-monotone operator found anywhere in the tree. It does
not distinguish between:

- A non-monotone operator on a **cycle edge** (dangerous -- breaks
  convergence)
- A non-monotone operator on a **non-cycle edge** (safe -- upstream is
  already converged)

This conflation is the root cause of Gap 1: overly conservative rejection.

### 3.4. Scheduler Execution

The scheduler processes cyclic SCCs (Step B3) before acyclic consistency
groups (Step C). Within Step C, consistency groups are processed in
topological order. However:

- There is no mechanism to **propagate "no-change" signals** from a
  completed SCC to downstream groups.
- Downstream groups always run `determine_refresh_action()` which checks
  `has_stream_table_source_changes()` via timestamp comparison. This
  query runs even when the upstream SCC produced zero changes.

---

## 4. Design: Three Stratification Features

### Feature S-1: Stratum-Aware Cycle Validation

**Goal**: Allow non-monotone operators in cycle members when the
non-monotone dependency targets a source *outside* the cycle (in a lower
stratum).

#### 4.1.1. Edge Classification

When a stream table's defining query is parsed, classify each source
dependency as monotone or non-monotone:

```
For each source_relid referenced by the defining query:
    Walk the OpTree to find all paths from the root to Scan nodes
    for this source_relid.

    If ANY path passes through a non-monotone operator
    (Except.right, AntiJoin.subquery, Aggregate):
        edge_type = NON_MONOTONE
    Else:
        edge_type = MONOTONE
```

**Example**:

```sql
-- Stream table: active_users
SELECT u.id, u.name FROM users u
WHERE NOT EXISTS (
    SELECT 1 FROM banned_users b WHERE b.user_id = u.id
)
```

Dependencies:
- `users` -- monotone (left side of anti-join)
- `banned_users` -- **non-monotone** (subquery side of anti-join)

#### 4.1.2. Stratum Assignment Algorithm

Given the dependency DAG with typed edges, assign strata:

```
Input:  DAG G = (V, E) where each edge e has type MONOTONE or NON_MONOTONE
Output: stratum: V -> {0, 1, 2, ...} or UNSTRATIFIABLE

1. Build the condensation graph C of G (collapse SCCs into super-nodes).

2. For each SCC in C:
       If the SCC contains a NON_MONOTONE internal edge:
           return UNSTRATIFIABLE  -- non-monotone cycle, cannot stratify

3. Assign strata via topological sort of C:
       For each super-node S in topological order:
           stratum(S) = max over all predecessors P of:
               if edge(P -> S) is NON_MONOTONE:
                   stratum(P) + 1
               else:
                   stratum(P)
       Base stratum (no predecessors): 0
```

This produces the **minimum stratification** -- each SCC gets the lowest
possible stratum that respects non-monotone ordering.

#### 4.1.3. Updated Cycle Validation

Replace the current logic in `validate_cycle_allowed_inner()`:

**Current**: Run `check_monotonicity()` on the entire defining query of
each cycle member. Reject if any non-monotone operator exists anywhere.

**New**: For each cycle member, check only whether the **intra-cycle
edges** are monotone. Non-monotone edges to sources *outside* the cycle
are safe because those sources are in a lower stratum (already converged).

```rust
fn validate_cycle_allowed_with_strata(
    cycle_members: &[i64],         // pgt_ids in the SCC
    member_queries: &[(i64, &str)], // (pgt_id, defining_query)
) -> Result<(), PgTrickleError> {
    for (pgt_id, query) in member_queries {
        let parse_result = parse_defining_query_full(query)?;

        // Classify each source edge
        for (source_oid, source_type) in &parse_result.source_relids {
            let edge_type = classify_edge_monotonicity(
                &parse_result.tree, *source_oid
            );

            // Is this source another cycle member?
            let is_intra_cycle = cycle_members.iter().any(|&member_id| {
                // Check if source_oid is the storage table of a cycle member
                is_storage_table_of(member_id, *source_oid)
            });

            if is_intra_cycle && edge_type == EdgeType::NonMonotone {
                return Err(PgTrickleError::UnsupportedOperator(format!(
                    "non-monotone dependency on cycle member '{}' \
                     (only monotone edges allowed within a cycle)",
                    source_name,
                )));
            }
            // Non-monotone edges to non-cycle sources are OK!
        }
    }
    Ok(())
}
```

#### 4.1.4. What This Unlocks

Patterns that are **currently rejected** but would be **allowed**:

```sql
-- Circular: reach_a depends on reach_b (monotone JOIN -- OK in cycle)
-- and also uses NOT EXISTS on 'blocked_edges' (non-monotone, but
-- blocked_edges is a base table = stratum 0, outside the cycle)

SET pg_trickle.allow_circular = true;

-- reach_a: paths through B, excluding blocked edges
SELECT pgtrickle.create_stream_table('reach_a',
    $$WITH RECURSIVE cte AS (
        SELECT dst AS target FROM red_edges WHERE src = 1
        UNION
        SELECT e.dst FROM red_edges e
        INNER JOIN reach_b rb ON e.src = rb.target
        WHERE NOT EXISTS (
            SELECT 1 FROM blocked_edges b
            WHERE b.src = e.src AND b.dst = e.dst
        )
        UNION
        SELECT e.dst FROM red_edges e INNER JOIN cte c ON e.src = c.target
    )
    SELECT DISTINCT target FROM cte$$,
    '1s', 'DIFFERENTIAL', false
);

-- reach_b: paths through A (purely monotone)
SELECT pgtrickle.create_stream_table('reach_b', ...);
```

Today this would be rejected because `check_monotonicity()` sees the
`NOT EXISTS`. With stratum-aware validation, the anti-join on
`blocked_edges` (a base table in stratum 0) is recognized as safe.

---

### Feature S-2: Stratum-Based Scheduler Optimization

**Goal**: Skip downstream strata when upstream strata produce no changes.

#### 4.2.1. Change Propagation Bitmap

After each stratum completes, record whether it produced any changes:

```rust
struct StratumResult {
    stratum: u32,
    had_changes: bool,   // true if any member produced inserts/deletes
    member_ids: Vec<i64>,
}
```

Before processing stratum N, check if any upstream stratum that feeds
into N via a dependency produced changes:

```rust
fn should_skip_stratum(
    stratum: u32,
    stratum_results: &[StratumResult],
    dag: &StDag,
) -> bool {
    // Find all upstream strata for this stratum
    let upstream_strata = get_upstream_strata(stratum, dag);

    // Skip if NO upstream stratum had changes
    upstream_strata.iter().all(|us| {
        stratum_results.iter()
            .find(|r| r.stratum == *us)
            .map(|r| !r.had_changes)
            .unwrap_or(true)
    })
}
```

#### 4.2.2. Interaction with Base Table Changes

A stratum can be skipped only if:
1. No upstream *stratum* produced changes, AND
2. No base table source of any member in this stratum has pending changes
   in its change buffer.

Base table changes are already detected by `has_table_source_changes()`
which checks `EXISTS (SELECT 1 FROM changes_<oid> WHERE ...)`. This
check is per-member, not per-stratum. The optimization applies only to
the **cascade**: if stratum 0 had changes but stratum 1 (which reads
only from stratum 0 stream tables) produced zero net changes, then
stratum 2 can be skipped.

#### 4.2.3. Expected Impact

In a typical multi-layer pipeline:

```
Base tables  -->  [Stratum 0: joins/filters]
                       |
                  [Stratum 1: aggregations]
                       |
                  [Stratum 2: summary views]
```

When base table changes are infrequent, stratum 0 may produce zero
changes on most scheduler ticks. Today, strata 1 and 2 still execute
`determine_refresh_action()` (which runs SQL to check timestamps and
change buffers). With stratum-based skipping, the scheduler avoids
these SQL round-trips entirely.

**Benchmark estimate**: For a 5-stratum pipeline with changes every
10th tick, this saves ~80% of the per-tick overhead for idle strata.
The overhead is dominated by SPI calls for change detection, not refresh
execution itself.

#### 4.2.4. Implementation Sketch

Modify the scheduler main loop (Step C):

```rust
// Current: process consistency groups in topo order
for group in &consistency_groups {
    // ... check schedule, refresh
}

// New: group consistency groups by stratum, process in stratum order
let strata = assign_strata(&dag);
let mut stratum_results: Vec<StratumResult> = Vec::new();

for stratum_level in 0..=max_stratum {
    // Check if this stratum can be skipped
    if should_skip_stratum(stratum_level, &stratum_results, &dag) {
        stratum_results.push(StratumResult {
            stratum: stratum_level,
            had_changes: false,
            member_ids: /* ... */,
        });
        continue;  // Skip entire stratum!
    }

    let mut stratum_had_changes = false;
    for group in groups_in_stratum(stratum_level) {
        let result = process_consistency_group(group);
        if result.had_changes {
            stratum_had_changes = true;
        }
    }

    stratum_results.push(StratumResult {
        stratum: stratum_level,
        had_changes: stratum_had_changes,
        member_ids: /* ... */,
    });
}
```

---

### Feature S-3: Stratified DRed for Recursive CTEs

**Goal**: Use DRed (incremental) instead of recomputation for recursive
CTEs where the non-monotone operator is stratifiable within the CTE.

#### 4.3.1. When This Applies

A recursive CTE with a non-monotone recursive term where:
- The non-monotone sub-expression references only the **base case** or
  **external tables** (not the recursive self-reference).
- The recursive self-reference is used only in monotone positions.

**Example** -- graph reachability with cycle elimination:

```sql
WITH RECURSIVE reach AS (
    SELECT src, dst, ARRAY[src] AS path FROM edges WHERE src = 1
    UNION ALL
    SELECT e.src, e.dst, r.path || e.src
    FROM edges e
    INNER JOIN reach r ON e.src = r.dst
    WHERE e.dst <> ALL(r.path)   -- cycle elimination (non-monotone!)
)
SELECT DISTINCT src, dst FROM reach;
```

The `<> ALL(r.path)` is non-monotone because it negates membership
in the recursively-computed path. However, this negation is
**self-contained** -- it references the CTE's own growing result, not
an external non-monotone source. This pattern is stratifiable because:

- Stratum 0 (within the CTE): the base case and recursive JOIN are
  monotone.
- The `WHERE` filter with `<> ALL` acts as a **termination guard**,
  not a semantic negation -- it prevents infinite loops but does not
  affect the least fixed point for acyclic paths.

#### 4.3.2. Intra-CTE Stratification Analysis

For a recursive CTE `R = B UNION ALL F(R)`:

```
1. Parse the recursive term F(R) into its OpTree.

2. Find all references to the recursive self-ref (RecursiveSelfRef nodes).

3. For each self-ref, check if it appears in a monotone position:
   - Left side of a JOIN: monotone
   - Right side of EXCEPT: non-monotone
   - Inside NOT EXISTS subquery: non-monotone
   - Inside aggregate: non-monotone (but may be stratifiable)

4. If ALL self-references are in monotone positions:
   - The CTE is "positively recursive" even if it contains
     non-monotone operators on external data.
   - DRed can be used: treat external non-monotone ops as constants
     (they don't participate in the recursion).

5. If ANY self-reference is in a non-monotone position:
   - The CTE has "negative recursion" -- DRed may not converge.
   - Fall back to recomputation.
```

#### 4.3.3. Implementation

Modify `diff_recursive_cte()` in `src/dvm/operators/recursive_cte.rs`:

```rust
// Current logic:
if let Some(reason) = recursive_term_is_non_monotone(recursive) {
    return generate_recomputation_delta(...);
}

// New logic:
if let Some(reason) = recursive_term_is_non_monotone(recursive) {
    // Check if the non-monotonicity is stratifiable
    if is_self_ref_in_monotone_position(recursive, alias) {
        // Non-monotone ops reference only external data or base case
        // Safe to use DRed -- treat external non-monotone as constants
        pgrx::info!(
            "Recursive CTE \"{}\": non-monotone term ({}) is stratifiable. \
             Using DRed instead of recomputation.",
            alias, reason
        );
        // Fall through to normal semi-naive/DRed path
    } else {
        // Self-reference in non-monotone position -- unsafe
        return generate_recomputation_delta(...);
    }
}
```

#### 4.3.4. What This Unlocks

Common recursive CTE patterns that would benefit:

| Pattern | Non-Monotone Op | Stratifiable? | Current | After |
|---------|-----------------|---------------|---------|-------|
| Graph traversal with cycle elimination | `<> ALL(path)` | Yes -- self-termination guard | Recompute | DRed |
| Shortest path (keep min distance) | `Aggregate(MIN)` over self-ref | No -- aggregate on recursive result | Recompute | Recompute |
| Anti-join on external table | `NOT EXISTS (base_table)` | Yes -- external reference | Recompute | DRed |
| Hierarchical EXCEPT | `EXCEPT SELECT ... FROM self_ref` | No -- self-ref in EXCEPT right | Recompute | Recompute |

#### 4.3.5. Expected Impact

Graph traversal with cycle elimination is one of the most common recursive
CTE patterns. Moving from recomputation to DRed for this pattern means:
- INSERT-only: semi-naive propagation (fast, incremental)
- Mixed changes: DRed with over-delete/rederive (incremental, not full scan)

For large graphs (100K+ edges), this is the difference between scanning the
entire edge table vs. processing only the changed edges.

---

## 5. Catalog Schema Changes

### 5.1. Edge Type in Dependencies

Add a column to `pgt_dependencies` to persist the monotonicity
classification:

```sql
ALTER TABLE pgtrickle.pgt_dependencies
    ADD COLUMN edge_type TEXT NOT NULL DEFAULT 'MONOTONE'
        CHECK (edge_type IN ('MONOTONE', 'NON_MONOTONE'));
```

Populated during `create_stream_table()` and `alter_stream_table()` by
running `classify_edge_monotonicity()` on the parsed OpTree.

### 5.2. Stratum Assignment in Stream Tables

Add a column to `pgt_stream_tables`:

```sql
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN stratum INT;
```

- Recomputed by `assign_strata_from_dag()` whenever the dependency graph
  changes (create, alter, drop).
- `NULL` when the graph has a single stratum (optimization: skip stratum
  logic for simple pipelines).

### 5.3. Migration SQL

```sql
-- In upgrade script pg_trickle--X.Y.Z--X.Y.W.sql:
ALTER TABLE pgtrickle.pgt_dependencies
    ADD COLUMN IF NOT EXISTS edge_type TEXT NOT NULL DEFAULT 'MONOTONE'
        CHECK (edge_type IN ('MONOTONE', 'NON_MONOTONE'));

ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS stratum INT;

COMMENT ON COLUMN pgtrickle.pgt_dependencies.edge_type IS
    'MONOTONE if adding input rows can only add output rows; '
    'NON_MONOTONE if adding input rows may remove output rows '
    '(EXCEPT right side, NOT EXISTS subquery, aggregate input).';

COMMENT ON COLUMN pgtrickle.pgt_stream_tables.stratum IS
    'Stratum number in the Datalog stratification of the dependency '
    'graph. Lower strata are fully converged before higher strata begin. '
    'NULL when stratification is not applicable (single stratum).';
```

---

## 6. New Functions and API

### 6.1. Edge Classification

```rust
/// Classify whether a dependency edge from source_oid to this query
/// is monotone or non-monotone.
///
/// Walks the OpTree to find all paths from root to Scan nodes for
/// the given source_oid. If any path passes through a non-monotone
/// operator position (Except right child, AntiJoin subquery,
/// Aggregate input), the edge is non-monotone.
pub fn classify_edge_monotonicity(
    tree: &OpTree,
    source_oid: pg_sys::Oid,
) -> EdgeType
```

### 6.2. Stratum Assignment

```rust
/// Assign strata to all stream tables based on the typed dependency DAG.
///
/// Returns the stratum assignment or an error if the graph contains a
/// non-monotone cycle (unstratifiable).
pub fn assign_strata(dag: &StDag) -> Result<HashMap<NodeId, u32>, PgTrickleError>
```

### 6.3. Recursive CTE Self-Ref Position Check

```rust
/// Check whether all recursive self-references in the OpTree appear
/// in monotone positions (safe for DRed even when other operators
/// are non-monotone).
pub fn is_self_ref_in_monotone_position(
    tree: &OpTree,
    cte_alias: &str,
) -> bool
```

### 6.4. Monitoring: Stratum View

```sql
-- Expose stratification info in monitoring
SELECT pgtrickle.pgt_stratum_status()
    RETURNS TABLE (
        stratum         INT,
        member_count    INT,
        members         TEXT[],
        has_cycles      BOOLEAN,
        non_monotone_edges INT
    );
```

---

## 7. Implementation Plan

### Phase 1: Edge Classification Infrastructure (Foundation)

Persist monotonicity metadata per dependency edge. No behavioral changes
yet -- pure infrastructure.

| Step | Description | Files | Est. Lines |
|------|-------------|-------|------------|
| 1.1 | Add `edge_type` column to `pgt_dependencies` DDL | `src/lib.rs` | 10 |
| 1.2 | Add `edge_type` to `StDependency` struct | `src/catalog.rs` | 15 |
| 1.3 | Implement `classify_edge_monotonicity()` | `src/dvm/parser.rs` | 120 |
| 1.4 | Populate `edge_type` on create/alter | `src/api.rs` | 30 |
| 1.5 | Add `stratum` column to `pgt_stream_tables` | `src/lib.rs`, `src/catalog.rs` | 15 |
| 1.6 | Implement `assign_strata()` | `src/dag.rs` | 100 |
| 1.7 | Call `assign_strata()` after DAG mutations | `src/api.rs` | 20 |
| 1.8 | Unit tests for edge classification | `src/dvm/parser.rs` | 100 |
| 1.9 | Unit tests for stratum assignment | `src/dag.rs` | 80 |
| 1.10 | Upgrade migration SQL | `sql/` | 20 |

**Total Phase 1**: ~510 lines

### Phase 2: Stratum-Aware Cycle Validation (Feature S-1)

Replace the whole-tree monotonicity check with per-edge classification
for cycle validation.

| Step | Description | Files | Est. Lines |
|------|-------------|-------|------------|
| 2.1 | Refactor `validate_cycle_allowed_inner()` to use edge types | `src/api.rs` | 80 |
| 2.2 | Update `check_for_cycles()` to pass edge metadata | `src/api.rs` | 40 |
| 2.3 | Update `check_for_cycles_alter()` similarly | `src/api.rs` | 40 |
| 2.4 | Build typed DAG in `build_from_catalog()` | `src/dag.rs` | 30 |
| 2.5 | E2E tests: non-monotone external dep in cycle (now allowed) | `tests/` | 150 |
| 2.6 | E2E tests: non-monotone intra-cycle dep (still rejected) | `tests/` | 80 |
| 2.7 | Update error messages with stratum info | `src/api.rs` | 30 |
| 2.8 | Documentation updates | `docs/` | 80 |

**Total Phase 2**: ~530 lines

### Phase 3: Stratum-Based Scheduler Optimization (Feature S-2)

Skip downstream strata when upstream strata produce no changes.

| Step | Description | Files | Est. Lines |
|------|-------------|-------|------------|
| 3.1 | Group consistency groups by stratum in scheduler | `src/scheduler.rs` | 60 |
| 3.2 | Track `StratumResult` (had_changes) per stratum | `src/scheduler.rs` | 40 |
| 3.3 | Implement `should_skip_stratum()` logic | `src/scheduler.rs` | 50 |
| 3.4 | Integrate base-table change detection with stratum skip | `src/scheduler.rs` | 30 |
| 3.5 | Add metrics: strata skipped per tick | `src/monitor.rs` | 20 |
| 3.6 | E2E tests: verify downstream skip on zero-change upstream | `tests/` | 150 |
| 3.7 | E2E tests: verify no skip when base table has changes | `tests/` | 80 |
| 3.8 | Documentation updates | `docs/` | 50 |

**Total Phase 3**: ~480 lines

### Phase 4: Stratified DRed for Recursive CTEs (Feature S-3)

Use DRed for recursive CTEs with stratifiable non-monotone terms.

| Step | Description | Files | Est. Lines |
|------|-------------|-------|------------|
| 4.1 | Implement `is_self_ref_in_monotone_position()` | `src/dvm/operators/recursive_cte.rs` | 80 |
| 4.2 | Update `diff_recursive_cte()` strategy selection | `src/dvm/operators/recursive_cte.rs` | 30 |
| 4.3 | Unit tests for self-ref position analysis | `src/dvm/operators/recursive_cte.rs` | 100 |
| 4.4 | E2E tests: cycle-eliminating graph traversal (DRed) | `tests/` | 150 |
| 4.5 | E2E tests: non-stratifiable patterns (still recompute) | `tests/` | 80 |
| 4.6 | Performance comparison: DRed vs. recomputation | `benches/` | 60 |
| 4.7 | Documentation updates | `docs/` | 50 |

**Total Phase 4**: ~550 lines

### Phase 5: Monitoring and Observability

| Step | Description | Files | Est. Lines |
|------|-------------|-------|------------|
| 5.1 | `pgt_stratum_status()` SQL function | `src/api.rs` | 60 |
| 5.2 | Add `stratum` to `pgt_status()` output | `src/api.rs` | 10 |
| 5.3 | Add `edge_type` to dependency inspection functions | `src/api.rs` | 20 |
| 5.4 | Documentation: stratification section in ARCHITECTURE.md | `docs/` | 100 |
| 5.5 | Documentation: CONFIGURATION.md updates | `docs/` | 30 |
| 5.6 | Documentation: FAQ entries | `docs/` | 40 |

**Total Phase 5**: ~260 lines

---

## 8. Execution Order and Dependencies

```
Phase 1 (Foundation)
    |
    +---> Phase 2 (Cycle Validation)
    |         |
    |         +---> Phase 5 (Monitoring)
    |
    +---> Phase 3 (Scheduler Optimization)
    |
    +---> Phase 4 (Stratified DRed) [independent of Phases 2 & 3]
```

**Recommended order**: Phase 1 -> Phase 2 -> Phase 3 -> Phase 4 ->
Phase 5.

Phase 4 is technically independent of Phases 2 and 3 (it operates at the
intra-CTE level, not the inter-table level) and could be done in
parallel.

---

## 9. Testing Strategy

### 9.1. Unit Tests (Phases 1, 4)

```rust
// Edge classification
#[test]
fn test_classify_scan_is_monotone() { }

#[test]
fn test_classify_anti_join_subquery_source_is_non_monotone() { }

#[test]
fn test_classify_anti_join_left_source_is_monotone() { }

#[test]
fn test_classify_except_right_source_is_non_monotone() { }

#[test]
fn test_classify_except_left_source_is_monotone() { }

#[test]
fn test_classify_aggregate_source_is_non_monotone() { }

#[test]
fn test_classify_join_both_sources_are_monotone() { }

#[test]
fn test_classify_nested_non_monotone_detected() { }

// Stratum assignment
#[test]
fn test_assign_strata_linear_chain() {
    // A -> B -> C (all monotone) => all stratum 0
}

#[test]
fn test_assign_strata_non_monotone_edge() {
    // A --(monotone)--> B --(non_monotone)--> C
    // => A: 0, B: 0, C: 1
}

#[test]
fn test_assign_strata_diamond_with_mixed_edges() {
    // A -> B (mono), A -> C (non-mono), B -> D (mono), C -> D (mono)
    // => A: 0, B: 0, C: 1, D: 1
}

#[test]
fn test_assign_strata_non_monotone_cycle_is_unstratifiable() {
    // A --(non_mono)--> B --(mono)--> A => UNSTRATIFIABLE
}

#[test]
fn test_assign_strata_monotone_cycle_ok() {
    // A --(mono)--> B --(mono)--> A => both stratum 0
}

// Self-ref position check
#[test]
fn test_self_ref_in_join_is_monotone() { }

#[test]
fn test_self_ref_in_except_right_is_non_monotone() { }

#[test]
fn test_self_ref_in_anti_join_left_is_monotone() { }

#[test]
fn test_external_anti_join_with_monotone_self_ref_is_stratifiable() { }
```

### 9.2. E2E Tests

```rust
// Phase 2: Cycle validation
#[tokio::test]
async fn test_stratified_cycle_with_external_not_exists_allowed() {
    // Create cycle where NOT EXISTS targets a base table (stratum 0)
    // Verify: cycle is allowed, convergence occurs
}

#[tokio::test]
async fn test_non_stratifiable_cycle_with_intra_not_exists_rejected() {
    // Create cycle where NOT EXISTS targets another cycle member
    // Verify: rejected with clear error message
}

#[tokio::test]
async fn test_stratified_cycle_with_external_except_allowed() {
    // EXCEPT where right side is a base table
}

#[tokio::test]
async fn test_stratum_assignment_visible_in_status() {
    // Check pgt_status() shows correct stratum for each ST
}

// Phase 3: Scheduler optimization
#[tokio::test]
async fn test_downstream_stratum_skipped_when_upstream_unchanged() {
    // Pipeline: base -> ST_A (stratum 0) -> ST_B (stratum 1)
    // Insert into base, refresh. ST_A changes, ST_B refreshes.
    // No more inserts, refresh again. ST_A no changes, ST_B skipped.
    // Verify ST_B's data_timestamp did not advance.
}

#[tokio::test]
async fn test_downstream_not_skipped_when_base_table_has_changes() {
    // ST_B (stratum 1) reads from ST_A (stratum 0) AND base_table
    // ST_A had no changes but base_table has new rows
    // Verify ST_B still refreshes
}

// Phase 4: Stratified DRed
#[tokio::test]
async fn test_recursive_cte_cycle_elimination_uses_dred() {
    // Graph traversal with WHERE dst <> ALL(path)
    // Insert edges, verify incremental update (not recomputation)
}

#[tokio::test]
async fn test_recursive_cte_external_anti_join_uses_dred() {
    // Recursive CTE with NOT EXISTS on external base table
    // Verify DRed used instead of recomputation
}

#[tokio::test]
async fn test_recursive_cte_self_ref_in_except_still_recomputes() {
    // Recursive CTE where self-ref is in EXCEPT right side
    // Verify recomputation fallback is still used
}
```

### 9.3. Property-Based Tests

```rust
#[tokio::test]
async fn test_stratified_cycle_matches_ground_truth() {
    // Generate random graph with blocked edges
    // Create cycle with NOT EXISTS on blocked_edges (external)
    // Verify result matches non-incremental WITH RECURSIVE ground truth
}
```

---

## 10. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| Edge classification is wrong (false monotone) | High | Conservative: classify as NON_MONOTONE when unsure. Extensive unit tests for all operator types. |
| Stratum-based skip misses required refreshes | High | Always check base table change buffers independently of stratum propagation. Disable via GUC. |
| Stratified DRed produces wrong results | High | Compare against recomputation in tests. Add GUC to force recomputation fallback. |
| Performance regression from stratum computation | Low | `assign_strata()` runs only on DAG mutations (create/alter/drop), not per tick. O(V+E) algorithm. |
| Complex interaction with parallel refresh mode | Medium | Phase 3 should integrate with parallel mode: strata define barriers that parallel execution must respect. |
| `edge_type` gets stale after ALTER QUERY | Medium | Recompute edge types on every `alter_stream_table()` call, same as existing dependency refresh logic. |
| Adoption friction: users must understand strata | Low | Strata are fully automatic. Users only see benefit (more patterns accepted, faster skipping). Monitoring surfaces stratum info for debugging. |

---

## 11. Configuration

### 11.1. New GUCs

| GUC | Type | Default | Description |
|-----|------|---------|-------------|
| `pg_trickle.enable_stratum_skip` | bool | `true` | Enable stratum-based downstream skip optimization. Set `false` to revert to current per-ST change detection. |
| `pg_trickle.stratified_cycle_validation` | bool | `true` | Use edge-level monotonicity for cycle validation instead of whole-tree check. Set `false` for conservative (current) behavior. |
| `pg_trickle.stratified_dred` | bool | `true` | Allow DRed for recursive CTEs with stratifiable non-monotone terms. Set `false` to always recompute. |

All three default to `true` because the behavior is strictly better
(more permissive, more efficient) and the safety analysis is sound. The
GUCs exist as escape hatches for diagnosing unexpected behavior.

---

## 12. Alternatives Considered

### Alternative 1: Well-Founded Semantics

Instead of stratification (which rejects unstratifiable programs), use
well-founded semantics which assigns a three-valued truth (true, false,
undefined) to every atom. This handles arbitrary negation cycles.

**Rejected**: Well-founded semantics requires maintaining three-valued
state per row, which triples storage and complicates delta computation.
It also produces "undefined" results that are confusing for SQL users
expecting definite answers. Stratification covers the practical use
cases without this complexity.

### Alternative 2: Magic Sets Optimization

Transform recursive queries using magic sets to push predicates through
recursion, reducing the search space.

**Deferred**: Orthogonal to stratification. Could be combined in the
future but adds significant query rewriting complexity. Semi-naive
evaluation (already implemented) captures most of the benefit.

### Alternative 3: No Explicit Strata -- Rely on SCC Order

Keep the current implicit stratification via SCC topological order.
Add only the scheduler skip optimization without formal stratum
assignment.

**Partially adopted**: The scheduler skip (Phase 3) can be implemented
without formal strata by tracking "had_changes" per SCC and propagating
through the condensation DAG. However, formal strata are needed for
Phase 2 (cycle validation) and Phase 4 (stratified DRed), so the
infrastructure investment is worthwhile.

---

## 13. Open Questions

1. **Should SemiJoin (EXISTS/IN) be classified as monotone or
   non-monotone?** Currently treated as monotone in
   `check_monotonicity()`. This is correct: adding right-side rows
   can only add left-side matches. But the intuition is subtle --
   document clearly.

2. **How should FULL OUTER JOIN be classified?** Adding rows to either
   side can add output rows (monotone) but can also change existing
   NULL-padded rows into matched rows (update, not insert). Currently
   treated as monotone. Validate this is correct for cycle semantics.

3. **Should stratum assignment consider LATERAL subqueries?** LATERAL
   is currently treated as opaque by `recursive_term_is_non_monotone()`.
   If the LATERAL body is monotone, the edge should be classified as
   monotone. Requires deeper analysis.

4. **What is the interaction between `enable_stratum_skip` and
   `parallel_refresh_mode`?** In parallel mode, stratum boundaries
   become synchronization barriers. Need to ensure the parallel
   executor respects stratum ordering.

5. **Should we expose a `pgtrickle.explain_strata()` function?** A
   diagnostic function that takes a set of stream table names and
   returns their stratum assignment, edge types, and any
   unstratifiable cycles. Useful for query design before committing.

---

## 14. References

- Abiteboul, S., Hull, R., Vianu, V. (1995). Foundations of Databases,
  Chapter 15: Stratified Datalog.
- Apt, K., Blair, H., Walker, A. (1988). Towards a Theory of Declarative
  Knowledge. In Foundations of Deductive Databases and Logic Programming.
- Budiu, M. et al. (2023). DBSP: Automatic Incremental View Maintenance.
  VLDB 2023.
- Gupta, A., Mumick, I.S., Subrahmanian, V.S. (1993). Maintaining Views
  Incrementally (DRed algorithm).
- Ceri, S., Gottlob, G., Tanca, L. (1989). What you always wanted to
  know about Datalog (and never dared to ask). IEEE TKDE 1(1).
- Van Gelder, A., Ross, K.A., Schlipf, J.S. (1991). The well-founded
  semantics for general logic programs. JACM 38(3).

---

## 15. Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 1 | Edge classification infrastructure | Not started |
| Phase 2 | Stratum-aware cycle validation (S-1) | Not started |
| Phase 3 | Stratum-based scheduler optimization (S-2) | Not started |
| Phase 4 | Stratified DRed for recursive CTEs (S-3) | Not started |
| Phase 5 | Monitoring and observability | Not started |
