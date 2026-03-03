# Plan: Diamond Dependency Consistency (Multi-Path Refresh)

Date: 2026-02-28
Status: IN PROGRESS — Option 1 (Epoch-Based Atomic Groups)
Last Updated: 2026-03-02

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

### Decision: Option 1 — Epoch-Based Atomic Refresh Groups

**Rationale:**

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

5. **Option 2 not needed.** Frontier alignment is fully subsumed: within an
   atomic group B, C, and D are committed together, so misaligned frontiers
   are structurally impossible. Option 2 is not implemented as a user-facing
   mode.

### Configuration

Only two modes are exposed:

```sql
-- Global default
SET pg_trickle.diamond_consistency = 'atomic';  -- or 'none'

-- Per-ST override
SELECT pgtrickle.alter_stream_table(
    'order_avg_by_region',
    diamond_consistency => 'atomic'
);
```

| Mode | Behavior |
|------|----------|
| `'none'` | Current behavior. No cross-path consistency. (Default in v0.x for backward compat.) |
| `'atomic'` | Epoch-based atomic groups. Related STs refresh atomically or not at all. (Recommended.) |

---

## 6. Implementation Plan

Implementation order follows strict dependency: each step builds on the
previous one. Steps 1–3 have no database dependency and can be validated
with `just test-unit`. Steps 4–6 require an integration/E2E environment.

---

### Step 1 — Define data structures in `dag.rs` (no DB) ✅ DONE

**Goal:** Represent diamonds and consistency groups as first-class types.

1. Add `Diamond` struct:
   ```rust
   /// A detected diamond in the ST dependency graph.
   pub struct Diamond {
       /// The fan-in ST that joins two or more upstream paths.
       pub convergence: NodeId,
       /// Base tables / STs that are the shared root(s) of the paths.
       pub shared_sources: Vec<NodeId>,
       /// Intermediate STs on all paths from shared_sources to convergence.
       pub intermediates: Vec<NodeId>,
   }
   ```

2. Add `ConsistencyGroup` struct:
   ```rust
   /// A set of STs that must refresh atomically.
   pub struct ConsistencyGroup {
       /// All members, in topological order, including the convergence ST.
       pub members: Vec<NodeId>,
       /// The fan-in ST(s) that required this group.
       pub convergence_points: Vec<NodeId>,
       /// Monotonically increasing counter; advances on every successful
       /// group refresh.
       pub epoch: u64,
   }
   ```

3. Add an `is_singleton(&self) -> bool` helper on `ConsistencyGroup` that
   returns `true` when `members.len() == 1` — used to short-circuit the
   SAVEPOINT overhead for non-diamond STs.

**Validation:** `just fmt && just lint` must pass with zero warnings.

---

### Step 2 — Implement diamond detection in `dag.rs` (no DB) ✅ DONE

**Goal:** A pure function that detects all diamonds in a given `StDag` and
returns a list of `Diamond` values.

1. Add `pub fn detect_diamonds(dag: &StDag) -> Vec<Diamond>` to `StDag`:

   ```
   detect_diamonds(dag):
     diamonds = []
     for each ST node D where |ST-only upstream deps| >= 2:
       paths = all_paths_to_roots(D, ST-only edges)
       for each pair (P1, P2) in paths:
         shared = ancestors(P1) ∩ ancestors(P2)
         if shared ≠ ∅:
           intermediates = (nodes(P1) ∪ nodes(P2)) \ {D} \ shared
           diamonds.push(Diamond {
             convergence: D,
             shared_sources: shared,
             intermediates: intermediates,
           })
     return merge_overlapping(diamonds)
   ```

2. Add `merge_overlapping(diamonds: Vec<Diamond>) -> Vec<Diamond>`: combine
   any two `Diamond` values whose `intermediates` sets overlap into a single
   `Diamond`. This handles nested and multi-diamond DAGs correctly.

3. **Complexity:** $O(V \cdot E)$ worst case; acceptable because DAGs are
   shallow. Cache the result alongside the DAG (rebuild only when
   `DAG_REBUILD_SIGNAL` advances — see Step 5).

4. Write unit tests (all in `src/dag.rs` under `#[cfg(test)]`):

   | Test name | Scenario |
   |---|---|
   | `test_detect_diamonds_simple` | A→B→D, A→C→D — one diamond detected |
   | `test_detect_diamonds_deep` | A→B→E→D, A→C→D — 3-level path |
   | `test_detect_diamonds_none` | Linear A→B→C — no diamond |
   | `test_detect_diamonds_overlapping` | Two diamonds sharing an intermediate |
   | `test_detect_diamonds_multiple_roots` | B and C have *different* root tables — not a diamond |

**Validation:** `just test-unit` must pass.

---

### Step 3 — Implement consistency group computation in `dag.rs` (no DB) ✅ DONE

**Goal:** Convert the list of `Diamond` values into a list of
`ConsistencyGroup` values that the scheduler can iterate.

1. Add `pub fn compute_consistency_groups(dag: &StDag) -> Vec<ConsistencyGroup>`:
   - Call `detect_diamonds(dag)` to get raw diamonds.
   - For each diamond, create a group with `members = intermediates ∪
     {convergence}` sorted in topological order (so the convergence ST is
     always last).
   - Merge groups whose member sets overlap (transitive closure — handles
     nested diamonds and multi-convergence DAGs).
   - For every ST not in any group, emit a singleton `ConsistencyGroup`
     (`members = [st]`) so the scheduler can use a uniform loop.

2. Expose `pub fn consistency_groups(&self) -> &[ConsistencyGroup]` on
   `StDag` (cached after DAG rebuild; invalidated on signal).

3. Write unit tests:

   | Test name | Scenario |
   |---|---|
   | `test_groups_simple_diamond` | B and C in one group, D as convergence |
   | `test_groups_singleton_non_diamond` | Linear ST gets a size-1 group |
   | `test_groups_nested_merge` | Two overlapping diamonds merged into one group |
   | `test_groups_independent_diamonds` | Two independent diamonds → two separate groups |

**Validation:** `just test-unit` must pass.

---

### Step 4 — Add catalog column and GUC in `catalog.rs` / `config.rs`

**Goal:** Persist the per-ST `diamond_consistency` setting and expose the
global GUC. No scheduler logic yet.

1. In `config.rs`: add `pg_trickle.diamond_consistency` GUC of type `enum`
   with values `'none'` (default, backward-compatible) and `'atomic'`.
   Define a `DiamondConsistency` Rust enum mirroring these values.

2. In `catalog.rs`: add a `diamond_consistency TEXT NOT NULL DEFAULT 'none'`
   column to `pgtrickle.pgt_stream_tables` via a migration SQL block. Add
   `get_diamond_consistency(oid) -> DiamondConsistency` and
   `set_diamond_consistency(oid, DiamondConsistency)` helpers.

3. Update `create_stream_table()` in `api.rs` to accept an optional
   `diamond_consistency => 'atomic'` parameter; store it via the catalog
   helper. Default to the GUC value when not supplied.

4. Update `alter_stream_table()` in `api.rs` similarly.

5. Write a catalog integration test:

   | Test name | Scenario |
   |---|---|
   | `test_catalog_diamond_consistency_default` | Newly created ST defaults to GUC value |
   | `test_catalog_diamond_consistency_override` | Per-ST override round-trips correctly |

**Validation:** `just fmt && just lint && just test-integration`.

---

### Step 5 — Wire consistency groups into the scheduler (`scheduler.rs`)

**Goal:** Replace the flat `for node in topological_order` loop with a
group-aware loop that uses SAVEPOINT for atomic groups.

1. After the DAG rebuild step, call `dag.consistency_groups()` to obtain
   the current group list. Store it in the scheduler's tick state.

2. Replace the existing per-ST refresh loop with:

   ```rust
   for group in dag.consistency_groups() {
       if group.is_singleton() {
           // Fast path: no SAVEPOINT overhead for non-diamond STs.
           execute_single_refresh(&group.members[0], &mut ctx)?;
           continue;
       }

       // Check that every member has diamond_consistency = 'atomic'.
       // If any member opts out, fall back to independent refreshes.
       let all_atomic = group.members.iter()
           .all(|m| catalog::get_diamond_consistency(m) == DiamondConsistency::Atomic);

       if !all_atomic {
           for member in &group.members {
               execute_single_refresh(member, &mut ctx)?;
           }
           continue;
       }

       Spi::run("SAVEPOINT pgt_consistency_group")?;
       let mut group_ok = true;

       for member in &group.members {       // already in topo order
           if let Err(e) = execute_single_refresh(member, &mut ctx) {
               log!("diamond group rollback: member {:?} failed: {}", member, e);
               group_ok = false;
               break;
           }
       }

       if group_ok {
           Spi::run("RELEASE SAVEPOINT pgt_consistency_group")?;
           group.advance_epoch();           // see Step 3
       } else {
           Spi::run("ROLLBACK TO SAVEPOINT pgt_consistency_group")?;
           // All members retain their pre-tick state; retry next cycle.
           record_group_skip(&group, &mut ctx);
       }
   }
   ```

3. Update `record_refresh_outcome()` (or equivalent) to log the group epoch
   into `pgt_refresh_history` so operators can trace which tick a group
   successfully committed.

4. Confirm that the `BackgroundWorker` transaction wrapping in the scheduler
   tick is **outside** the group loop — each tick is already one outer
   transaction; SAVEPOINTs nest inside it correctly.

**Validation:** `just fmt && just lint`. Do not run E2E yet (needs Step 6).

---

### Step 6 — Add monitoring SQL function (`api.rs`)

**Goal:** Give operators visibility into detected diamond groups.

1. Add `pgtrickle.diamond_groups()` returning `TABLE(group_id INT,
   member_name TEXT, member_schema TEXT, is_convergence BOOL, epoch BIGINT)`.
   Reads the in-memory `consistency_groups()` list via SPI or a lightweight
   shared-state slot.

2. Add `pgtrickle.explain_st(name TEXT)` output to include a
   `diamond_group_id` column (or extend the existing function if it exists).

**Validation:** `just fmt && just lint`.

---

### Step 7 — E2E tests

Add to `tests/e2e_diamond_tests.rs`:

| Test name | Type | What it proves |
|---|---|---|
| `test_diamond_atomic_all_succeed` | E2E | B and C both succeed → D refreshed, epoch advances |
| `test_diamond_atomic_partial_fail` | E2E | B succeeds, C fails → ROLLBACK TO SAVEPOINT, D unchanged, B rolled back |
| `test_diamond_atomic_convergence` | E2E | After retry, B+C succeed → D correct and consistent |
| `test_diamond_none_mode_unblocked` | E2E | With `diamond_consistency='none'`, C failure does **not** roll back B |
| `test_diamond_linear_unaffected` | E2E | Linear chain A→B→C refreshes independently (no SAVEPOINT overhead) |
| `test_diamond_nested_merge` | E2E | Nested diamond: all intermediates grouped, D only refreshes when all succeed |
| `test_diamond_groups_sql_function` | E2E | `pgtrickle.diamond_groups()` returns correct membership |

**Validation:** `just test-e2e`.

---

### Step 8 — Documentation

1. Add `diamond_consistency` parameter to `create_stream_table()` and
   `alter_stream_table()` in `docs/SQL_REFERENCE.md`.
2. Add `pg_trickle.diamond_consistency` GUC entry to
   `docs/CONFIGURATION.md`.
3. Add a "Diamond Dependency Consistency" section to
   `docs/ARCHITECTURE.md` explaining the problem, the atomic group
   solution, and the interaction with topological ordering.
4. Update `CHANGELOG.md`.

**Validation:** `just fmt && just lint && just test-all`.

---

### Recommended Implementation Order Summary

| Step | File(s) | Depends on | Testable with | Status |
|---|---|---|---|---|
| 1 — Data structures | `dag.rs` | — | `just lint` | ✅ Done |
| 2 — Diamond detection | `dag.rs` | Step 1 | `just test-unit` | ✅ Done |
| 3 — Consistency groups | `dag.rs` | Step 2 | `just test-unit` | ✅ Done |
| 4 — Catalog + GUC | `catalog.rs`, `config.rs`, `api.rs`, `lib.rs` | Step 3 | `just test-integration` | ✅ Done |
| 5 — Scheduler wiring | `scheduler.rs` | Steps 3, 4 | `just lint` | ✅ Done |
| 6 — Monitoring function | `api.rs` | Steps 3, 5 | `just lint` | ✅ Done |
| 7 — E2E tests | `tests/e2e_diamond_tests.rs` | Steps 5, 6 | `just test-e2e` | ✅ Done |
| 8 — Documentation | `docs/`, `CHANGELOG.md` | Steps 1–7 | `just test-all` | ✅ Done |

### What Was Implemented (2026-03-02)

Steps 1–3 completed in `src/dag.rs`:

- **`Diamond` struct** — represents a detected diamond with convergence node,
  shared sources, and intermediate STs.
- **`ConsistencyGroup` struct** — represents an atomic refresh group with
  topologically-ordered members, convergence points, and epoch counter.
  Includes `is_singleton()` and `advance_epoch()` helpers.
- **`StDag::detect_diamonds()`** — walks all fan-in ST nodes, computes
  transitive ancestor sets per upstream branch, finds shared ancestors,
  and merges overlapping diamonds.
- **`StDag::compute_consistency_groups()`** — converts diamonds into
  scheduler-ready groups in topological order, merges overlapping groups
  transitively, and emits singleton groups for non-diamond STs.
- **Private helpers**: `collect_ancestors()` (recursive upstream walk),
  `merge_overlapping_diamonds()` (union-find-style diamond merging).

**12 new unit tests** added and passing:
`test_detect_diamonds_simple`, `test_detect_diamonds_deep`,
`test_detect_diamonds_none_linear`, `test_detect_diamonds_multiple_roots_no_diamond`,
`test_detect_diamonds_overlapping`, `test_groups_simple_diamond`,
`test_groups_singleton_non_diamond`, `test_groups_independent_diamonds`,
`test_groups_nested_merge`, `test_consistency_group_epoch_advance`,
`test_consistency_group_is_singleton`.

All 965 unit tests pass. `just fmt && just lint` clean.

### What Was Implemented (Steps 4–7)

Steps 4–7 completed across multiple files:

**Step 4 — Catalog + GUC:**
- **`DiamondConsistency` enum** in `dag.rs` — `None` / `Atomic` variants
  with `as_str()`, `from_sql_str()`, `Display`.
- **`PGS_DIAMOND_CONSISTENCY` GUC** in `config.rs` — string GUC defaulting
  to `"none"`, registered as `pg_trickle.diamond_consistency`.
- **`diamond_consistency` column** added to `pgtrickle.pgt_stream_tables`
  DDL in `lib.rs` — `TEXT NOT NULL DEFAULT 'none' CHECK (... IN ('none', 'atomic'))`.
- **`StreamTableMeta.diamond_consistency`** field in `catalog.rs` — added to
  struct, all SELECT queries, `from_spi_table`, `from_spi_heap_tuple`,
  `insert()`. New helpers: `get_diamond_consistency()`, `set_diamond_consistency()`.
- **`create_stream_table()`** now accepts optional `diamond_consistency` param
  (defaults to GUC value).
- **`alter_stream_table()`** now accepts optional `diamond_consistency` param.

**Step 5 — Scheduler wiring:**
- Replaced flat `for node in ordered` loop with group-aware loop using
  `dag.compute_consistency_groups()`.
- Singleton groups use fast path via `refresh_single_st()` (no SAVEPOINT).
- Multi-member groups check that all members have `diamond_consistency = 'atomic'`.
  If not, fall back to independent refreshes.
- Atomic groups: `SAVEPOINT pgt_consistency_group` → refresh each member →
  `RELEASE SAVEPOINT` on success, `ROLLBACK TO SAVEPOINT` on any failure.
- Added `refresh_single_st()` helper to avoid code duplication.

**Step 6 — Monitoring function:**
- **`pgtrickle.diamond_groups()`** SQL function returning `TABLE(group_id,
  member_name, member_schema, is_convergence, epoch)`. Builds DAG on-demand,
  computes groups, skips singletons, and resolves ST names from catalog.

**Step 7 — E2E tests:**
- Created `tests/e2e_diamond_tests.rs` with 8 test cases:
  `test_diamond_consistency_default`, `test_diamond_consistency_create_atomic`,
  `test_diamond_consistency_alter`, `test_diamond_groups_sql_function`,
  `test_diamond_linear_unaffected`, `test_diamond_none_mode_no_groups`,
  `test_diamond_atomic_all_succeed`, `test_diamond_consistency_in_catalog_view`.

**4 new unit tests** for `DiamondConsistency` enum:
`test_diamond_consistency_as_str`, `test_diamond_consistency_from_sql_str`,
`test_diamond_consistency_display`, `test_diamond_consistency_roundtrip`.

All 969 unit tests pass. `just fmt && just lint` clean.

### What Was Implemented (Step 8 — Documentation)

**SQL_REFERENCE.md:**
- Added `diamond_consistency` (6th) parameter to `create_stream_table()` signature, parameter table, and description.
- Added `diamond_consistency` parameter to `alter_stream_table()` signature and parameter table.
- Added full `pgtrickle.diamond_groups()` function documentation with return columns, example output, and usage notes.

**CONFIGURATION.md:**
- Added `pg_trickle.diamond_consistency` GUC section with value table, description, SQL examples, and usage notes.
- Added `pg_trickle.diamond_consistency = 'none'` to the Complete postgresql.conf Example.

**ARCHITECTURE.md:**
- Added section 13 "Diamond Dependency Consistency" covering the problem, detection algorithm, consistency groups, scheduler SAVEPOINT wiring, and monitoring.
- Added `diamond_groups` to Monitoring section's function list.
- Added `pg_trickle.diamond_consistency` row to the GUC quick reference table.

**CHANGELOG.md:**
- Added diamond dependency consistency feature under `[Unreleased]` → `### Added`.

### Prioritized Remaining Work

All 8 steps are complete. No remaining work for the initial implementation.

Potential future enhancements (not prioritized):
- Parallel refresh of diamond group members (see §7.1).
- Expose `diamond_group_id` in `pgtrickle.explain_st()` output.
- Cache consistency groups in shared memory for large deployments (1000+ STs).

All 13 implementation steps are now complete (Steps 1–8: core diamond detection
and atomic refresh; Steps 9–13: `diamond_schedule_policy` per-convergence-node
with GUC fallback, scheduler wiring, documentation, unit tests, and E2E tests).

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

The SCC-based refresh logic in [PLAN_CIRCULAR_REFERENCES.md](PLAN_CIRCULAR_REFERENCES.md)
handles cycles via fixed-point iteration. A diamond that is also part of a
cycle would need the SCC logic to account for consistency groups. The
recommended approach: treat the entire SCC as one consistency group (which
the SCC logic already effectively does by iterating all members together).

### 8.4 Manual Refresh

`pgtrickle.refresh_stream_table('D')` already refreshes upstream STs in
topological order. Adding frontier alignment to the manual refresh path
ensures consistency for diamonds even in manual mode.

---

## 9. Prior Art & Related Work

The diamond dependency consistency problem — ensuring that a downstream
operator that joins two or more independently computed intermediate results
sees a coherent snapshot rather than a "split view" — is a well-known
challenge in dataflow and streaming systems. This section surveys how the
problem manifests and is addressed across the landscape.

### 9.1 The Problem in Context

Jamie Brandon's *"An opinionated map of incremental and streaming systems"*
(2021) provides the clearest framing. He distinguishes **internally
consistent** systems (which always return an output that is the correct
result for some present or past input) from **internally inconsistent** ones:

> "An internally inconsistent system might return outputs that are not the
> correct result for any of the past inputs. Perhaps they … combined part
> of one input with part of another, or finished processed some update down
> one path in the graph but not yet down another."
> — scattered-thoughts.net, 2021

The last clause is precisely the diamond problem. Brandon observes that
*all* high-temporal-locality systems (Flink, Kafka Streams, ksqlDB, Spark
Structured Streaming) permit internal inconsistency under various
scenarios, whereas low-temporal-locality systems built on explicit logical
time (Differential Dataflow, Materialize) are internally consistent by
construction. pg_trickle occupies a middle ground — it is a periodic-refresh
IVM system (not a continuous streaming system), so it uses per-refresh
epoch boundaries rather than continuous logical timestamps to enforce
consistency within diamond groups.

### 9.2 Foundational Theory

1. **Chandy-Lamport Snapshots** (1985) — The seminal algorithm for
   consistent global snapshots in distributed systems via marker messages.
   Inspiration for our epoch-based approach where the "epoch" acts as a
   marker that flows through the dependency graph. When all paths within a
   consistency group complete under the same SAVEPOINT, the system has
   effectively taken a Chandy-Lamport snapshot of the group.

2. **Vector Clocks** (Mattern, 1989; Fidge, 1988) — Logical timestamps for
   causality tracking in distributed systems. Directly applicable to Option
   4.5 (per-path version vectors) in our options analysis. Our chosen Option
   1 trades the fine-grained per-event tracking of vector clocks for a
   simpler epoch-based grouping that is sufficient for periodic refresh.

### 9.3 Streaming Dataflow Systems

3. **DBSP** (Budiu, McSherry, Ryzhyk & Tannen, 2022) — Formalized as
   synchronous circuits where all operators process the same logical
   timestamp in lock-step. The circuit advances one discrete step at a time,
   computing the entire fixed-point before producing output. This eliminates
   the diamond problem *by construction* — there is no window in which one
   path has advanced while another has not. Feldera is the commercial
   implementation. Our epoch-based atomic groups achieve a similar effect
   within PostgreSQL's transactional model: the SAVEPOINT boundary
   corresponds to a DBSP "step", ensuring all group members advance
   atomically.
   *Ref: arXiv:2203.16684; VLDB 2023*

4. **Differential Dataflow** (McSherry et al., 2013) — Frontier-based
   progress tracking with capabilities. An operator only advances its output
   frontier when *all* input frontiers have advanced past a timestamp. This
   provides automatic diamond consistency: a join operator at the confluence
   of two paths will not emit results at time *t* until both paths have
   delivered all updates through time *t*. Direct inspiration for Options
   4.2 and 4.5 in our analysis.

5. **Timely Dataflow** (Murray et al., SOSP 2013) — Pointstamp-based
   progress tracking in a distributed dataflow system. The `can_advance`
   predicate at each operator answers "could I still receive data at or
   before timestamp *t*?" — analogous to our frontier alignment check.
   Timely's progress protocol is the mechanism that enables Differential
   Dataflow's consistency guarantees.

6. **Apache Flink** — Uses **checkpoint barriers** (inspired by
   Chandy-Lamport) that flow through the dataflow graph. At operators with
   multiple inputs, barriers must be **aligned**: the operator buffers data
   from the faster input until the barrier arrives on all inputs, ensuring
   that the checkpoint captures a consistent cut across all paths. This is
   structurally identical to the diamond problem — Flink's barrier alignment
   ensures that a join operator's snapshot does not mix pre-barrier data from
   one path with post-barrier data from another. Flink also offers
   *unaligned checkpoints* (since 1.11) which trade off consistent cuts for
   lower latency by including in-flight data in the snapshot. Our approach
   is analogous to Flink's aligned barriers, but at the granularity of
   per-group refresh rather than per-record processing.
   *Ref: Carbone et al., "Lightweight Asynchronous Snapshots for
   Distributed Dataflows", arXiv:1506.08603*

7. **Noria / Readyset** (Gjengset et al., OSDI 2018) — A
   partially-stateful dataflow system that takes an interesting different
   approach. Noria uses **upqueries** — on-demand backward traversal of the
   dataflow graph to fill in missing state. For multi-path (diamond)
   topologies, upqueries from a join node must reconcile state from
   multiple parents that may be at different points in time. Noria addresses
   this by processing updates through all paths before making results visible
   at downstream operators. The Readyset commercial system (née Noria)
   describes itself as using "partially-stateful streaming dataflow" for
   incrementally maintaining SQL query result sets.
   *Ref: "Noria: Dynamic, Partially-Stateful Data-Flow for
   High-Performance Web Applications", OSDI 2018*

### 9.4 Streaming Databases

8. **Materialize** (materialize.com) — The most relevant commercial system.
   Built on Differential Dataflow and Timely Dataflow, Materialize enforces
   **internal consistency** as a core guarantee. Frank McSherry and Natacha
   Crooks's 2020 blog post defines internal consistency formally:

   > "A view is internally consistent at time *t* if it reflects all events
   > that have a timestamp smaller or equal to *t*, and no events that have
   > a timestamp greater than *t*."

   For hierarchies of views (exactly the diamond pattern), Materialize
   ensures that a downstream view's output at time *t* incorporates all
   upstream changes through time *t* across every path. The key mechanisms
   are: (a) logical timestamps assigned at ingestion, (b) frontier tracking
   that prevents a view from advancing until all inputs have caught up, and
   (c) **timestamp closing** — the ability to determine when no more data
   will arrive at a given timestamp, even for idle sources.

   Materialize also distinguishes **causal dependencies** (event A caused
   event B) from **atomic dependencies** (events A and B should appear
   simultaneously, e.g. within a transaction). Our `diamond_consistency =
   'atomic'` mode enforces atomic dependencies within consistency groups.
   *Ref: "Consistency in Streaming Systems", materialize.com/blog/consistency/*

9. **RisingWave** — A cloud-native streaming database that maintains
   materialized views incrementally. RisingWave uses a barrier-based
   checkpoint mechanism similar to Flink's for consistency across its
   dataflow operators. Barriers propagate through the operator graph and
   must align at join points, ensuring multi-path consistency.
   *Ref: docs.risingwave.com*

### 9.5 How pg_trickle Compares

| System | Consistency mechanism | Granularity | Diamond handling |
|--------|----------------------|-------------|-----------------|
| DBSP / Feldera | Synchronous stepping | Per-circuit-step | By construction |
| Differential Dataflow | Frontier tracking | Per-timestamp | By construction |
| Timely Dataflow | Pointstamp progress | Per-timestamp | By construction |
| Flink | Barrier alignment | Per-checkpoint | Barrier buffering |
| Noria / Readyset | Upquery reconciliation | Per-query | On-demand |
| Materialize | Timestamp frontiers | Per-timestamp | By construction |
| RisingWave | Barrier checkpoints | Per-barrier | Barrier alignment |
| **pg_trickle** | **Epoch + SAVEPOINT** | **Per-refresh-group** | **Atomic rollback** |

pg_trickle's approach is pragmatic and fits naturally within PostgreSQL's
transactional model. Rather than implementing a full logical-timestamp
system (which would require fundamental changes to how PostgreSQL processes
queries), we:

- **Detect** diamond topologies statically via DAG analysis
  (`detect_diamonds`, `compute_consistency_groups`)
- **Group** affected stream tables into consistency groups
- **Execute** each group's refresh within a single SAVEPOINT, so either all
  members advance or all roll back
- **Configure** the trade-off via `diamond_consistency` (none/atomic) and
  the planned `diamond_schedule_policy` (fastest/slowest)

This is most similar to Flink's barrier alignment approach, adapted from a
continuous-streaming context to a periodic-refresh context, and implemented
using PostgreSQL savepoints rather than in-memory barrier buffers.

---

## 10. Open Questions (Resolved)

1. **Granularity of groups:** Should the group include D itself, or only the
   intermediate STs (B, C)? Including D makes the atomic guarantee stronger
   (D's old state is preserved on failure) but increases the group size and
   lock duration.
   **Decision:** Include D in the group. ✅ Implemented in Steps 1–3.

2. **Nested diamonds:** If D is itself part of another diamond (D→E, D→F→G,
   with G joining E and F), should groups be merged transitively?
   **Decision:** Yes — compute the transitive closure of shared-ancestor
   relationships and merge overlapping groups. ✅ Implemented in Step 3
   (`merge_overlapping_diamonds`).

3. **Performance impact of group detection:** The algorithm runs during DAG
   rebuild. For typical deployments (10-50 STs), this is negligible. For
   large deployments (1000+ STs), we may need to cache the result.
   **Decision:** Cache consistency groups alongside the DAG; rebuild only
   when `DAG_REBUILD_SIGNAL` advances. ✅ Already the case — groups are
   computed from the cached DAG which is rebuilt only on signal.

4. **Interaction with cron schedules:** If B has a `*/5 * * * *` schedule
   and C has a `*/10 * * * *` schedule, how should the group decide when to
   refresh?
   **Decision:** Per-convergence-node `diamond_schedule_policy` column (Option A),
   with a cluster-wide `pg_trickle.diamond_schedule_policy` GUC as the default.
   The convergence node (the fan-in ST, i.e. D) owns the policy because the
   group exists to serve D's freshness requirements. B and C are intermediate
   nodes and don't set the policy.
   Two values:
   - `'fastest'` **(default)** — group refreshes when **any** member is due.
     Over-delivers on freshness for slower members. Correct default because
     users who opt into `atomic` mode prioritize consistency over efficiency.
   - `'slowest'` — group refreshes when **all** members are due. Conserves
     resources but under-delivers freshness for faster members.
   The policy only applies when `diamond_consistency = 'atomic'`; in `'none'`
   mode each ST uses its own schedule independently.
   SQL API mirrors `diamond_consistency` — set it on the convergence node at
   create or alter time:
   ```sql
   SELECT pgtrickle.create_stream_table('order_avg_by_region', '...',
     diamond_consistency => 'atomic',
     diamond_schedule_policy => 'slowest');
   ```
   The scheduler reads the policy from `ConsistencyGroup.convergence_points`
   (already computed by `compute_consistency_groups`). When multiple
   convergence points exist, the strictest policy (`slowest > fastest`) wins.
   ⬜ Implementation plan in §11 below.

5. **Monitoring:** How do users know their STs are in a diamond group?
   **Decision:** Expose via `pgtrickle.diamond_groups()` SQL function and
   show in `pgtrickle.explain_st()` output. ✅ `diamond_groups()` implemented
   in Step 6. `explain_st()` enhancement deferred to future work.

---

## 11. Implementation Plan: Diamond Schedule Policy (Q4)

A new **per-convergence-node catalog column** and matching GUC default to
control when a consistency group's refresh fires relative to its members'
individual schedules.

### Design: Option A — Per-convergence-node

The convergence node (D — the fan-in ST) owns the policy because the group
exists to serve D's freshness requirements. Intermediate nodes (B, C) do not
set the policy. The GUC provides the cluster-wide default used when the
catalog column is not set explicitly.

```
-- Intermediate nodes: set consistency only
SELECT pgtrickle.create_stream_table('order_totals_by_region', '...',
  diamond_consistency => 'atomic');

SELECT pgtrickle.create_stream_table('order_counts_by_region', '...',
  diamond_consistency => 'atomic');

-- Convergence node: also sets schedule policy
SELECT pgtrickle.create_stream_table('order_avg_by_region', '...',
  diamond_consistency => 'atomic',
  diamond_schedule_policy => 'slowest');
```

When multiple convergence points exist (nested diamonds), the strictest policy
(`slowest > fastest`) wins.

| Policy | Trigger condition | Trade-off |
|---|---|---|
| `'fastest'` **(default)** | `any(member, is_due)` | Higher freshness, more refreshes |
| `'slowest'` | `all(member, is_due)` | Lower resource cost, staler data |

Only takes effect when `diamond_consistency = 'atomic'`. In `'none'` mode,
each ST uses its own schedule independently — the policy is irrelevant.

### Step 9 — Add enum, GUC fallback, and catalog column

**Files:** `src/dag.rs`, `src/config.rs`, `src/lib.rs`, `src/catalog.rs`, `src/api.rs`

1. Add `DiamondSchedulePolicy` enum in `dag.rs`:
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
   pub enum DiamondSchedulePolicy {
       #[default]
       Fastest,
       Slowest,
   }

   impl DiamondSchedulePolicy {
       pub fn from_str(s: &str) -> Option<Self> {
           match s.trim().to_lowercase().as_str() {
               "fastest" => Some(Self::Fastest),
               "slowest" => Some(Self::Slowest),
               _ => None,
           }
       }
       pub fn as_str(self) -> &'static str {
           match self {
               Self::Fastest => "fastest",
               Self::Slowest => "slowest",
           }
       }
       /// Strictest of two policies (slowest > fastest).
       pub fn stricter(self, other: Self) -> Self {
           if self == Self::Slowest || other == Self::Slowest {
               Self::Slowest
           } else {
               Self::Fastest
           }
       }
   }
   ```

2. In `src/config.rs` add GUC fallback `PGS_DIAMOND_SCHEDULE_POLICY`
   (default `"fastest"`) and register in `_PG_init()` as
   `pg_trickle.diamond_schedule_policy`. Add accessor:
   ```rust
   pub fn pg_trickle_diamond_schedule_policy() -> DiamondSchedulePolicy {
       PGS_DIAMOND_SCHEDULE_POLICY
           .get()
           .and_then(|s| DiamondSchedulePolicy::from_str(s.to_str().ok()?))
           .unwrap_or_default()
   }
   ```

3. In `src/lib.rs` DDL, add to `pgtrickle.pgt_stream_tables`:
   ```sql
   diamond_schedule_policy TEXT NOT NULL DEFAULT 'fastest'
       CHECK (diamond_schedule_policy IN ('fastest', 'slowest')),
   ```

4. In `src/catalog.rs`, add `diamond_schedule_policy: DiamondSchedulePolicy`
   to `StreamTableRecord`, update all SELECT/INSERT/UPDATE queries, and add:
   ```rust
   pub fn set_diamond_schedule_policy(
       pgt_id: i64,
       policy: DiamondSchedulePolicy,
   ) -> Result<(), PgTrickleError>
   ```

5. In `src/api.rs`, add `diamond_schedule_policy: Option<String>` parameter
   to `create_stream_table` and `alter_stream_table`, validated and persisted
   via `catalog::set_diamond_schedule_policy`. Only the convergence node
   needs it; setting it on B or C is allowed but has no effect.

**Validation:** `just fmt && just lint`.

### Step 10 — Wire group schedule evaluation into the scheduler

**Files:** `src/scheduler.rs`

1. Add a helper that resolves the effective policy for a group by reading
   the catalog value from each convergence node and taking the strictest:
   ```rust
   fn group_schedule_policy(group: &ConsistencyGroup) -> DiamondSchedulePolicy {
       let guc_default = config::pg_trickle_diamond_schedule_policy();
       group.convergence_points.iter().fold(guc_default, |acc, node| {
           if let NodeId::StreamTable(id) = node {
               load_st_by_id(*id)
                   .map(|st| acc.stricter(st.diamond_schedule_policy))
                   .unwrap_or(acc)
           } else {
               acc
           }
       })
   }
   ```

2. Add `is_group_due` helper:
   ```rust
   fn is_group_due(
       group: &ConsistencyGroup,
       policy: DiamondSchedulePolicy,
       now_ms: i64,
   ) -> bool {
       let member_due: Vec<bool> = group.members.iter().filter_map(|m| {
           if let NodeId::StreamTable(id) = m {
               load_st_by_id(*id).map(|st| check_schedule(&st, now_ms) || st.needs_reinit)
           } else {
               None
           }
       }).collect();
       match policy {
           DiamondSchedulePolicy::Fastest => member_due.iter().any(|&d| d),
           DiamondSchedulePolicy::Slowest => member_due.iter().all(|&d| d),
       }
   }
   ```

3. In the group-aware refresh loop, replace per-member schedule checks with:
   ```rust
   let policy = group_schedule_policy(group);
   if !is_group_due(group, policy, now_ms) {
       continue;
   }
   // proceed with SAVEPOINT refresh of all members
   ```
   Singleton groups continue using the existing per-ST `check_schedule()`
   directly — no policy read needed.

4. When the group **is** due, refresh **all** members regardless of their
   individual schedule status — the atomic guarantee requires all-or-nothing.

**Validation:** `just fmt && just lint`.

### Step 11 — Documentation

**Files:** `docs/CONFIGURATION.md`, `docs/ARCHITECTURE.md`, `docs/SQL_REFERENCE.md`, `CHANGELOG.md`

1. **CONFIGURATION.md** — Add `### pg_trickle.diamond_schedule_policy`
   section: values, default, interaction with `diamond_consistency`, note
   that per-convergence-node value overrides GUC.

2. **ARCHITECTURE.md** — Update §13 "Diamond Dependency Consistency" to
   describe the schedule policy, convergence-node ownership, and strictest-
   wins rule for nested diamonds.

3. **SQL_REFERENCE.md** — Add `diamond_schedule_policy` parameter to
   `create_stream_table` and `alter_stream_table` tables; update
   `diamond_groups()` output to show the effective policy per group.

4. **CHANGELOG.md** — Add under `[Unreleased]`.

**Validation:** `just fmt && just lint`.

### Step 12 — Unit tests

**Files:** `src/dag.rs` (`#[cfg(test)]`), `src/scheduler.rs` (`#[cfg(test)]`)

| Test name | What it proves |
|---|---|
| `test_diamond_schedule_policy_from_str` | Parses `"fastest"`, `"slowest"`, rejects invalid |
| `test_diamond_schedule_policy_default` | Default is `Fastest` |
| `test_diamond_schedule_policy_stricter` | `slowest.stricter(fastest)` → `slowest` |
| `test_is_group_due_fastest_any_triggers` | With `Fastest`, group fires when any member is due |
| `test_is_group_due_slowest_all_required` | With `Slowest`, group fires only when all are due |
| `test_is_group_due_singleton_bypass` | Singleton groups bypass policy check |
| `test_group_policy_convergence_node_overrides_guc` | Convergence node `slowest` overrides GUC `fastest` |
| `test_group_policy_nested_strictest_wins` | Two convergence nodes: one `fastest`, one `slowest` → `slowest` |

**Validation:** `just test-unit`.

### Step 13 — E2E tests

**Files:** `tests/e2e_diamond_tests.rs`

| Test name | What it proves |
|---|---|
| `test_diamond_schedule_policy_fastest_default` | Default GUC `fastest` fires when any member due |
| `test_diamond_schedule_policy_slowest_per_node` | Convergence node `slowest` waits until all due |
| `test_diamond_schedule_policy_overrides_guc` | Per-node value overrides cluster GUC |
| `test_diamond_schedule_policy_none_mode_ignored` | Policy has no effect when `diamond_consistency = 'none'` |
| `test_diamond_schedule_policy_nested_strictest_wins` | Nested diamond uses strictest convergence policy |

**Validation:** `just test-e2e`.

### Implementation Order Summary

| Step | File(s) | Depends on | Testable with | Status |
|---|---|---|---|---|
| 9 — Enum + GUC + catalog + API | `dag.rs`, `config.rs`, `lib.rs`, `catalog.rs`, `api.rs` | Steps 1–8 | `just lint` | ✅ Done |
| 10 — Scheduler wiring | `scheduler.rs` | Step 9 | `just lint` | ✅ Done |
| 11 — Documentation | `docs/`, `CHANGELOG.md` | Step 10 | `just lint` | ✅ Done |
| 12 — Unit tests | `dag.rs`, `scheduler.rs` | Step 10 | `just test-unit` | ✅ Done |
| 13 — E2E tests | `tests/e2e_diamond_tests.rs` | Step 10 | `just test-e2e` | ✅ Done |
