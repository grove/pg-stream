# Plan: Diamond Dependency Consistency (Multi-Path Refresh)

Date: 2026-02-28
Status: PROPOSED
Last Updated: 2026-02-28

---

## 1. Problem Statement

### The Diamond Dependency Pattern

Consider four stream tables with the following dependency graph:

```
       ┌─────┐
       │  A  │  (base table or stream table)
       └──┬──┘
          │
     ┌────┴────┐
     ▼         ▼
  ┌─────┐  ┌─────┐
  │  B  │  │  C  │
  └──┬──┘  └──┬──┘
     │         │
     └────┬────┘
          ▼
       ┌─────┐
       │  D  │
       └─────┘
```

- A is a source (base table or stream table).
- B depends on A: `B = f(A)`
- C depends on A: `C = g(A)`
- D depends on both B and C: `D = h(B, C)`

When A changes, the change must propagate through **two paths** to reach D:
- Path 1: A → B → D
- Path 2: A → C → D

### The Inconsistency Window

With the current scheduler design, stream tables are refreshed sequentially in
topological order. A typical topological ordering might be: `[A, B, C, D]` or
`[A, C, B, D]`. In either case, B and C are refreshed separately, each in its
own SPI transaction within the scheduler tick.

**The problem arises when B's refresh succeeds but C's refresh fails** (or is
skipped due to advisory lock contention, retry backoff, etc.). In this
scenario:

- B has been refreshed to include A's latest changes (B reflects A@v2).
- C is still stale and reflects A's older state (C reflects A@v1).
- When D is refreshed, it joins B@v2 with C@v1 — **two different versions of
  A's data reach D through different paths**.

This produces an inconsistent result in D: it sees a "split view" of A where
some effects of A's changes are visible (via B) and some are not (via C).

### Concrete Example

```sql
-- Base table
CREATE TABLE orders (id INT, amount NUMERIC, region TEXT);

-- Stream table B: orders by region
SELECT pgtrickle.create_stream_table('order_totals_by_region',
  'SELECT region, SUM(amount) AS total FROM orders GROUP BY region');

-- Stream table C: order count by region
SELECT pgtrickle.create_stream_table('order_counts_by_region',
  'SELECT region, COUNT(*) AS cnt FROM orders GROUP BY region');

-- Stream table D: average order by region (joins B and C)
SELECT pgtrickle.create_stream_table('order_avg_by_region',
  'SELECT b.region, b.total / c.cnt AS avg_amount
   FROM order_totals_by_region b
   JOIN order_counts_by_region c ON b.region = c.region');
```

If `order_totals_by_region` (B) is refreshed with new orders but
`order_counts_by_region` (C) is not yet refreshed, D computes an incorrect
average — the numerator includes new orders but the denominator doesn't.

### Severity

This is not a data corruption issue — it's a **transient inconsistency**. On
the next successful refresh cycle where both B and C are refreshed, D will
self-correct. However, depending on refresh schedules and failure patterns,
the inconsistency window can persist for multiple cycles.

The severity depends on the use case:
- **Dashboards / analytics**: Usually acceptable (eventual consistency).
- **Financial reporting**: Potentially unacceptable if users rely on
  within-snapshot consistency across related stream tables.
- **Downstream triggers / alerts**: Could fire spurious alerts based on
  inconsistent intermediate states.

---

## 2. Theoretical Background

### 2.1 DBSP: Timestamps and Logical Time

In the DBSP model (Budiu et al., 2023), all operators in a dataflow circuit
process changes at the same **logical timestamp** $t$. The integration operator
$I$ and delay operator $z^{-1}$ ensure that:

$$Q[t] = Q^{\Delta}(\Delta I[t-1], \Delta S[t])$$

Where all operators in the circuit see a **consistent snapshot** at time $t$.
There is no diamond inconsistency because the dataflow runtime processes
all operators synchronously within a single logical step.

**Key insight:** DBSP avoids this problem by design — the entire circuit
advances atomically from $t$ to $t+1$. pg_trickle, operating as a
PostgreSQL extension with sequential per-ST refreshes, does not have this
property.

### 2.2 Differential Dataflow: Frontiers and Capabilities

McSherry et al.'s Differential Dataflow uses **frontiers** (Antichain of
timestamps) to track progress. A frontier represents a promise: "I will
never produce data at timestamps earlier than my frontier." Operators
advance their output frontier only when all their inputs have advanced.

The critical property:
- An operator at a join point (like D) does **not advance** until **all**
  upstream paths have delivered all data for the current logical time.
- This means D never sees a partial update — it waits until both B and C
  have fully propagated A's changes.

This is the mathematical foundation for correct multi-path incremental
computation.

### 2.3 Materialize / Feldera: Timestamps as Versions

Materialize (now using the Timely/Differential Dataflow engine) extends this
with **multi-dimensional timestamps** where each dimension represents a
different source. A timestamp like $(lsn_A: 42, lsn_B: 17)$ identifies the
exact version of each input. Operators advance their capabilities on each
dimension independently, and downstream operators only see consistent
combinations.

### 2.4 Noria: Dataflow with Partial State

Noria (Gjengset et al., 2018) takes a different approach — it propagates
changes eagerly through a dataflow graph but uses **upqueries** to
reconstruct state when needed. For diamond dependencies, if an operator
receives a partial update, it can issue an upquery to fetch the missing
data. However, this introduces complexity and is specific to Noria's
memory-resident model.

---

## 3. Current pg_trickle Behavior

### 3.1 Topological Ordering is Necessary but Not Sufficient

The scheduler refreshes STs in topological order (see
[scheduler.rs](../../src/scheduler.rs)), guaranteeing that upstream STs are
refreshed before downstream STs. This prevents D from seeing B@v2 while
A is still at v1 — but it does **not** prevent D from seeing B@v2 and C@v1
when both B and C are upstream but only one successfully refreshed.

### 3.2 Frontier Per-Source LSN Tracking

Each stream table maintains a `Frontier` (see [version.rs](../../src/version.rs))
that maps source OIDs to LSN values. The frontier records exactly "up to which
WAL position have I consumed changes from each source." However, the frontier
is per-ST and per-source-OID — it does not encode cross-ST consistency.

### 3.3 The DVS Guarantee

The architecture promises **Delayed View Semantics** — that every stream
table's contents are logically equivalent to evaluating its defining query at
some past time (`data_timestamp`). But this guarantee is per-ST. There is
currently no guarantee that two stream tables (B and C) reflect the **same**
point in time for a shared source (A).

---

## 4. Proposed Solutions

### 4.1 Option 1 — Epoch-Based Atomic Refresh Groups

**Core idea:** Identify sets of stream tables that must be refreshed
atomically (or not at all) to maintain cross-path consistency. Assign each
refresh cycle a monotonically increasing **epoch number**. Within an epoch,
either all members of a consistency group succeed or all are rolled back.

#### 4.1.1 Consistency Group Detection

A **consistency group** is a set of STs that share a common upstream ancestor
and converge at a common downstream ST. Algorithmically:

```
For each ST D in the DAG:
  For each pair of upstream paths P1, P2 reaching D:
    If P1 and P2 share a common ancestor A:
      GroupTogether(intermediates on P1, intermediates on P2)
```

More precisely, find all ST nodes where `|reverse_edges| > 1` (multiple
upstream ST dependencies). For each such fan-in node D, trace all paths
backward to find shared ancestors. The union of all intermediate STs on
those paths forms a consistency group.

For the A→B→D, A→C→D diamond, the consistency group is `{B, C}` (and
implicitly D depends on this group completing).

#### 4.1.2 Implementation Sketch

```rust
/// A consistency group: STs that must refresh atomically.
struct ConsistencyGroup {
    /// The STs in this group (must all succeed or all rollback).
    members: Vec<NodeId>,
    /// The fan-in node(s) that require this group's consistency.
    convergence_points: Vec<NodeId>,
    /// Epoch counter for this group.
    epoch: u64,
}

impl StDag {
    /// Detect diamond dependencies and return consistency groups.
    fn detect_consistency_groups(&self) -> Vec<ConsistencyGroup> {
        let mut groups = Vec::new();
        
        // Find all fan-in nodes (STs with multiple upstream ST dependencies)
        for node_id in self.topological_order().unwrap_or_default() {
            let upstream = self.get_upstream(node_id);
            let upstream_sts: Vec<_> = upstream.iter()
                .filter(|n| matches!(n, NodeId::StreamTable(_)))
                .collect();
            
            if upstream_sts.len() > 1 {
                // Check if any pair shares a common ancestor
                // ... trace paths and find shared ancestors ...
                // If found, create a consistency group
            }
        }
        
        groups
    }
}
```

#### 4.1.3 Refresh Execution Change

Currently in `scheduler.rs`, each ST is refreshed independently in Step C.
With consistency groups, the scheduler would:

1. **Identify groups** during DAG rebuild.
2. **Batch group members** — when the first member of a group is due for
   refresh, mark all group members for refresh.
3. **Execute in a SAVEPOINT wrapper** — wrap the group's refreshes in a
   savepoint. If any member fails, `ROLLBACK TO SAVEPOINT` and skip the
   entire group (including the convergence point D).
4. **Only refresh D** after all members of its consistency group have
   succeeded in the same epoch.

```rust
// Pseudocode for group-aware scheduling
for group in consistency_groups {
    let savepoint = Spi::run("SAVEPOINT consistency_group")?;
    let mut all_ok = true;
    
    for st in &group.members {
        if let Err(e) = execute_single_refresh(st) {
            all_ok = false;
            break;
        }
    }
    
    if all_ok {
        Spi::run("RELEASE SAVEPOINT consistency_group")?;
        // Now safe to refresh convergence points
        for cp in &group.convergence_points {
            execute_single_refresh(cp)?;
        }
    } else {
        Spi::run("ROLLBACK TO SAVEPOINT consistency_group")?;
        // Skip the entire group this cycle — D stays at its previous
        // consistent state
    }
}
```

#### 4.1.4 Pros & Cons

| Pros | Cons |
|------|------|
| Strong consistency — D never sees split versions | Reduced availability: one failing ST blocks the entire group |
| Simple mental model for users | Increased refresh latency (batch wait) |
| No schema changes needed | Complexity in group detection algorithm |
| Works within existing transaction model | Groups can become large in deep DAGs |
| Backward compatible (groups of size 1 = current behavior) | May require SAVEPOINT support in the scheduler's SPI context |

---

### 4.2 Option 2 — Frontier Alignment (Shared Epoch Frontier)

**Core idea:** Instead of each ST independently tracking its per-source
frontier, introduce a **shared epoch frontier** for related STs. Before
refreshing D, verify that B and C have both advanced their frontier for
shared source A to at least the same LSN.

#### 4.2.1 Implementation

Add a pre-check before refreshing fan-in nodes:

```rust
fn can_refresh_consistently(
    dag: &StDag,
    node: NodeId,
    frontiers: &HashMap<NodeId, Frontier>,
) -> bool {
    let upstream_sts = dag.get_upstream(node)
        .into_iter()
        .filter(|n| matches!(n, NodeId::StreamTable(_)));
    
    // Find all transitively shared sources
    let shared_sources = find_shared_transitive_sources(dag, upstream_sts);
    
    // For each shared source, check that all upstream STs have the same
    // frontier LSN
    for source_oid in shared_sources {
        let lsns: Vec<&str> = upstream_sts
            .map(|st| frontiers[st].get_lsn(source_oid))
            .collect();
        
        if !all_equal(&lsns) {
            return false; // Frontier misalignment — skip D this cycle
        }
    }
    
    true
}
```

If the frontiers don't align (because B succeeded but C failed), D is
**skipped** until the next cycle when both B and C have caught up.

#### 4.2.2 Frontier Alignment Modes

We could offer different strictness levels via a GUC or per-ST option:

1. **`strict`** — D is only refreshed when all upstream paths reflect the
   exact same LSN for every shared source. Maximum consistency.
2. **`bounded`** — D is refreshed if all upstream paths are within N LSN
   positions of each other. Trades some consistency for availability.
3. **`none`** — Current behavior. D is refreshed whenever its own schedule
   says so, regardless of upstream frontier alignment.

```sql
-- Per-ST configuration
SELECT pgtrickle.create_stream_table(
    'order_avg_by_region',
    'SELECT ...',
    consistency_mode => 'strict'  -- or 'bounded', 'none'
);
```

#### 4.2.3 Pros & Cons

| Pros | Cons |
|------|------|
| No rollback needed — just skip | D may be delayed indefinitely if one path keeps failing |
| Fine-grained per-ST control | Requires frontier comparison across STs |
| Low implementation complexity | Does not prevent the inconsistency — only prevents D from observing it |
| Works with existing frontier infrastructure | "Bounded" mode is hard to reason about |

---

### 4.3 Option 3 — Unified Transaction for Diamond Subgraphs

**Core idea:** Execute the entire diamond (B, C, and D) in a **single
PostgreSQL transaction**. If any step fails, the entire transaction rolls
back — which means all storage tables revert to their pre-refresh state.

#### 4.3.1 Implementation

The scheduler already runs inside `BackgroundWorker::transaction()`. Today
it refreshes each ST in a separate transaction. The change:

```rust
// Current: each ST gets its own implicit transaction boundary
for node_id in &ordered {
    execute_scheduled_refresh(&st, action);
    // Implicit commit after each refresh
}

// Proposed: group diamond-related STs in one transaction
for group in &diamond_groups {
    BackgroundWorker::transaction(AssertUnwindSafe(|| {
        for st in &group.members_in_topo_order {
            execute_single_refresh(st)?;
        }
        // All succeed → commit
        // Any failure → entire transaction rolls back
    }));
}
```

#### 4.3.2 Pros & Cons

| Pros | Cons |
|------|------|
| Atomic — D, B, C are all-or-nothing | Longer transactions hold locks longer |
| Simplest correctness argument | A failure in C rollbacks B's already-successful work |
| No new catalog columns or GUCs needed | Could cause contention with manual refreshes |
| | Large diamond groups could produce very long transactions |
| | Change buffer cleanup becomes more complex (deferred across group) |

---

### 4.4 Option 4 — Version-Stamped Refresh with Deferred Convergence

**Core idea:** Let B and C refresh independently (current behavior), but
stamp each ST's storage with the **version of each transitive source**.
When D is refreshed, construct its delta query so that it only reads from
B and C the rows that correspond to the **same source version**.

#### 4.4.1 Implementation

Add a `__pgt_source_versions JSONB` column to each stream table's storage:

```sql
ALTER TABLE order_totals_by_region
  ADD COLUMN __pgt_source_versions JSONB;
```

Each row in B carries metadata like:
```json
{"source_a_lsn": "0/1A2B3C4"}
```

When D joins B and C, the delta query includes a predicate:
```sql
WHERE b.__pgt_source_versions->>'source_a_lsn'
    = c.__pgt_source_versions->>'source_a_lsn'
```

This ensures D only combines rows from B and C that originate from the same
version of A.

#### 4.4.2 Pros & Cons

| Pros | Cons |
|------|------|
| B and C refresh independently (no blocking) | Significant storage overhead (JSONB per row) |
| D is always consistent (predicate-guaranteed) | Complex query rewriting — every delta must propagate version columns |
| No coordinator / epoch logic | JSONB comparison in joins is expensive |
| Naturally extends to deeper diamond chains | Fundamentally changes the storage schema of every ST |
| | Difficult to retrofit into existing installations |

---

### 4.5 Option 5 — Logical Clock / Lamport Timestamps

**Core idea:** Assign each change event a **Lamport timestamp** (logical
clock) and propagate it through the DAG. Each ST's refresh advances its
logical clock. D only reads from B and C when their logical clocks are
"compatible" (both reflect the same causal set from A).

#### 4.5.1 Implementation

```rust
struct LogicalClock {
    /// Per-source logical timestamps (monotonically increasing).
    clocks: HashMap<u32, u64>,
}

impl LogicalClock {
    /// Advance the clock for a given source.
    fn advance(&mut self, source_oid: u32) {
        let entry = self.clocks.entry(source_oid).or_insert(0);
        *entry += 1;
    }
    
    /// Check if this clock dominates another (all components >=).
    fn dominates(&self, other: &LogicalClock) -> bool {
        other.clocks.iter().all(|(k, v)| {
            self.clocks.get(k).map_or(false, |mine| mine >= v)
        })
    }
}
```

Each ST maintains a `LogicalClock` alongside its `Frontier`. On refresh:
1. B's clock advances to `{A: 42}` after consuming A's changes.
2. C's clock advances to `{A: 42}` after consuming A's changes.
3. D checks: does B's clock and C's clock both have `A >= 42`? If yes,
   refresh. If not, defer.

This is essentially a formalization of Option 4.2 (Frontier Alignment) using
vector clock theory.

#### 4.5.2 Pros & Cons

| Pros | Cons |
|------|------|
| Mathematically clean — vector clock theory is well-understood | Adds complexity to frontier tracking |
| Generalizes to arbitrary DAG shapes | Essentially equivalent to Option 4.2 with more formalism |
| Allows partial-order reasoning (bounded staleness) | Still requires skipping D on misalignment |
| Composable with other distributed systems concepts | May be over-engineered for single-node PostgreSQL |

---

## 5. Recommendation

### Primary: Option 1 (Epoch-Based Atomic Refresh Groups)

**Recommended for implementation**, with the following rationale:

1. **Strongest guarantee.** By grouping related STs and executing them
   atomically, D **never** observes an inconsistent state. This aligns with
   the DVS principle that each ST should be equivalent to evaluating its
   query at a single point in time.

2. **Leverages existing infrastructure.** PostgreSQL's SAVEPOINT mechanism
   provides exactly the rollback semantics needed. The scheduler already
   processes STs in topological order within a transaction.

3. **Backward compatible.** STs not involved in diamond dependencies
   continue to refresh independently (each forms a group of size 1).

4. **Operationally sound.** If one member of the group fails, the group is
   skipped and retried next cycle — matching existing retry/backoff behavior.

### Secondary: Option 2 (Frontier Alignment) as a lightweight fallback

For users who prefer availability over consistency, the **frontier alignment
check** can be added as a separate, non-default mode. This is simpler to
implement and can serve as a Phase 1 deliverable while the full epoch-based
approach is built.

### Configuration

```sql
-- Global default
SET pg_trickle.diamond_consistency = 'atomic';  -- or 'aligned', 'none'

-- Per-ST override
SELECT pgtrickle.alter_stream_table(
    'order_avg_by_region',
    diamond_consistency => 'atomic'
);
```

| Mode | Behavior |
|------|----------|
| `'none'` | Current behavior. No cross-path consistency. (Default in v0.x for backward compat.) |
| `'aligned'` | Frontier alignment check. D is skipped if upstream frontiers diverge. |
| `'atomic'` | Epoch-based atomic groups. Related STs refresh atomically or not at all. |

---

## 6. Implementation Plan

### Phase 1: Diamond Detection in DAG (dag.rs)

Add `detect_diamond_dependencies()` and `compute_consistency_groups()` to
`StDag`. Unit-testable without a database.

**Key algorithm:** For each fan-in node D (where `|upstream_st_deps| >= 2`),
compute the **transitive closure** of upstream paths. If two paths share a
common ancestor that is a base table or ST, the intermediate STs form a
consistency group.

```
detect_diamonds(dag):
  diamonds = []
  for each ST node D where in-degree(D, ST-only edges) >= 2:
    paths = all_paths_to_roots(D)
    for each pair of paths (P1, P2):
      shared = ancestors(P1) ∩ ancestors(P2)
      if shared ≠ ∅:
        intermediates = (P1 ∪ P2) \ {D} \ shared
        diamonds.append(Diamond {
          convergence: D,
          shared_sources: shared,
          intermediates: intermediates,
        })
  return merge_overlapping(diamonds)
```

**Complexity:** $O(V \cdot E)$ in the worst case, but DAGs are typically
shallow (2-4 levels deep) so this is negligible.

### Phase 2: Frontier Alignment Check (version.rs, scheduler.rs)

Add `check_frontier_alignment()` to the scheduler's per-ST evaluation loop.
Before refreshing a fan-in node:

1. Identify shared transitive sources.
2. Compare downstream ST frontiers for those sources.
3. If misaligned, skip and log a warning.

This is the simplest valuable delivered — low risk, no schema changes.

### Phase 3: Atomic Refresh Groups (scheduler.rs)

Modify the scheduler's main loop to:

1. During DAG rebuild, compute consistency groups.
2. Replace the flat `for node in ordered` loop with a group-aware loop.
3. Wrap each group's refreshes in a SAVEPOINT.
4. On any group member failure, ROLLBACK TO SAVEPOINT and mark all
   group members as skipped.
5. Record the group refresh outcome in `pgt_refresh_history`.

### Phase 4: Configuration & Documentation

1. Add `pg_trickle.diamond_consistency` GUC.
2. Add `diamond_consistency` column to `pgt_stream_tables`.
3. Update `create_stream_table()` and `alter_stream_table()` to accept the
   new parameter.
4. Add monitoring: `pgtrickle.diamond_groups()` function to inspect detected
   groups.
5. Document the feature in SQL_REFERENCE.md and CONFIGURATION.md.

### Phase 5: Testing

| Test | Type | Description |
|------|------|-------------|
| `test_diamond_detection_simple` | Unit | A→B→D, A→C→D detected |
| `test_diamond_detection_deep` | Unit | A→B→E→D, A→C→D (3-level) |
| `test_diamond_detection_no_diamond` | Unit | Linear chain not flagged |
| `test_diamond_detection_multiple` | Unit | Overlapping diamonds |
| `test_frontier_alignment_pass` | Integration | Both STs at same LSN → D refreshes |
| `test_frontier_alignment_fail` | Integration | B ahead, C behind → D skipped |
| `test_atomic_group_all_succeed` | E2E | B and C both succeed → D refreshed |
| `test_atomic_group_partial_fail` | E2E | B succeeds, C fails → B rolled back, D skipped |
| `test_atomic_group_convergence` | E2E | After retry, B+C succeed → D correct |
| `test_no_diamond_unaffected` | E2E | Linear chains refresh as before |

---

## 7. Alternatives Considered but Not Recommended

### 7.1 Parallel Refresh of Diamond Members

Instead of sequential processing within a group, refresh B and C in
parallel (separate background workers). This would reduce latency but:
- PostgreSQL's `max_worker_processes` budget is limited.
- Parallel transactional coordination within one extension is complex.
- The rollback semantics become much harder (two-phase commit within one PG
  instance is unnecessary complexity).

**Verdict:** Defer to a future "parallel refresh" feature if scaling demands
justify it.

### 7.2 Event Sourcing / Change Log with Global Ordering

Maintain a global ordered log of all changes across all sources, and have
each ST consumer track a global sequence number. This is the Kafka/event
sourcing approach.

**Verdict:** Over-engineered for embedded PostgreSQL. We already have WAL
LSN ordering per source; adding a global ordering layer would duplicate what
PostgreSQL already provides and add significant complexity.

### 7.3 Optimistic Execution with Rollback Detection

Let D refresh optimistically. After D refreshes, check if its inputs were
consistent. If not, mark D as "dirty" and re-refresh on the next cycle.

**Verdict:** This means D temporarily holds incorrect data, which violates
the DVS guarantee and could trigger downstream cascading inconsistencies.

---

## 8. Impact on Existing Features

### 8.1 Cascading Stream Tables

Cascading ST-on-ST dependencies already exist and work via topological
ordering. The diamond consistency feature extends this by adding fan-in
awareness. Non-diamond cascades (linear chains) are unaffected.

### 8.2 Immediate IVM (Transactional Mode)

The planned immediate IVM mode ([PLAN_TRANSACTIONAL_IVM.md](sql/PLAN_TRANSACTIONAL_IVM.md))
inherently avoids this problem because changes propagate within a single
transaction. Diamond consistency is a deferred-mode-only concern.

### 8.3 Circular References (SCCs)

The SCC-based refresh logic in [CIRCULAR_REFERENCES.md](sql/CIRCULAR_REFERENCES.md)
handles cycles via fixed-point iteration. A diamond that is also part of a
cycle would need the SCC logic to account for consistency groups. The
recommended approach: treat the entire SCC as one consistency group (which
the SCC logic already effectively does by iterating all members together).

### 8.4 Manual Refresh

`pgtrickle.refresh_stream_table('D')` already refreshes upstream STs in
topological order. Adding frontier alignment to the manual refresh path
ensures consistency for diamonds even in manual mode.

---

## 9. Prior Art References

1. **DBSP** (Budiu et al., 2023) — Synchronous circuit execution ensures
   all operators process the same logical timestamp. No diamond problem by
   construction.

2. **Differential Dataflow** (McSherry et al., 2013) — Frontier-based
   progress tracking with capabilities. An operator only advances when all
   inputs have advanced. Direct inspiration for Option 4.2 and 4.5.

3. **Timely Dataflow** (Murray et al., 2013) — Pointstamp-based progress
   tracking in a distributed dataflow system. The "can_advance" predicate
   is analogous to our frontier alignment check.

4. **Chandy-Lamport Snapshots** (1985) — Consistent global snapshots in
   distributed systems via marker messages. Inspiration for the epoch-based
   approach where the "epoch" acts as a marker.

5. **Vector Clocks** (Mattern, 1989; Fidge, 1988) — Logical timestamps for
   causality tracking in distributed systems. Directly applicable to Option
   4.5.

6. **Materialize** (materialize.com) — Commercial implementation using
   Differential Dataflow timestamps for consistent multi-path incremental
   computation. Their "read policies" determine at which timestamp queries
   see results — analogous to our consistency mode options.

---

## 10. Open Questions

1. **Granularity of groups:** Should the group include D itself, or only the
   intermediate STs (B, C)? Including D makes the atomic guarantee stronger
   (D's old state is preserved on failure) but increases the group size and
   lock duration. **Proposed answer:** Include D in the group.

2. **Nested diamonds:** If D is itself part of another diamond (D→E, D→F→G,
   with G joining E and F), should groups be merged transitively? **Proposed
   answer:** Yes — compute the transitive closure of shared-ancestor
   relationships and merge overlapping groups.

3. **Performance impact of group detection:** The algorithm runs during DAG
   rebuild. For typical deployments (10-50 STs), this is negligible. For
   large deployments (1000+ STs), we may need to cache the result.
   **Proposed answer:** Cache consistency groups alongside the DAG; rebuild
   only when `DAG_REBUILD_SIGNAL` advances.

4. **Interaction with cron schedules:** If B has a `*/5 * * * *` schedule
   and C has a `*/10 * * * *` schedule, the consistency group forces both to
   refresh at the slower rate (every 10 minutes). Is this acceptable?
   **Proposed answer:** Yes, with documentation. Users who need different
   rates should use `diamond_consistency = 'none'` and accept eventual
   consistency.

5. **Monitoring:** How do users know their STs are in a diamond group?
   **Proposed answer:** Expose via `pgtrickle.diamond_groups()` SQL function
   and show in `pgtrickle.explain_st()` output.
