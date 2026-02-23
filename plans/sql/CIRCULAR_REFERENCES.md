# PLAN: Circular References in the Stream Table Dependency Graph

## Objective

Enable stream tables to form mutual dependencies (cycles) in the dependency graph, where stream table A references stream table B and vice versa. The system must refresh such cycles deterministically by iterating to a fixed point, analogous to how recursive CTEs work within a single query.

---

## Background

### Current Behavior

The dependency graph is enforced as a strict DAG. When `create_stream_table()` is called, `check_for_cycles()` in `src/api.rs` builds the full graph, adds the proposed edges, and runs Kahn's algorithm. If a cycle is detected, the creation is rejected with `PgStreamError::CycleDetected`.

The scheduler in `src/scheduler.rs` relies on `topological_order()` (Kahn's BFS) to determine refresh order. This algorithm is undefined for graphs with cycles — it simply excludes nodes that are part of cycles.

### Precedent: Recursive CTEs

The extension already solves the same fundamental problem — circular data flow — at the query level. `WITH RECURSIVE` CTEs use:

1. **Semi-naive evaluation**: Propagate only new rows through the recursive term until no new rows are produced (fixed point).
2. **Delete-and-Rederive (DRed)**: Handle mixed INSERT/DELETE changes by over-deleting, then rederiving to restore rows with alternative derivations.

Both strategies converge to a fixed point for **monotone** queries (queries where adding input rows can only add output rows, never remove them). Non-monotone queries (containing negation, EXCEPT, aggregates) may not converge.

### Theoretical Foundation

The theory comes from **Datalog stratification** and **DBSP** (Dynamic Batch Stream Processing):

- **Monotone cycles** always converge: JOINs, UNIONs, projections, filters without negation.
- **Non-monotone cycles** may oscillate: EXCEPT, NOT IN, anti-joins, aggregates (COUNT can decrease when rows are deleted).
- **Stratification** partitions a program into layers where cycles only occur within monotone strata, and non-monotone dependencies only flow between strata (always downward, never in a cycle).

---

## Design

### Core Concept: Strongly Connected Components (SCCs)

Replace the strict DAG requirement with a **condensation graph**: decompose the dependency graph into its strongly connected components (SCCs) using Tarjan's algorithm. Each SCC becomes a single "super-node" in the condensation, which is always a DAG.

```
Before:                    Condensation:
  A ──→ B                    [A, B] ──→ C
  B ──→ A
  B ──→ C

SCC₁ = {A, B}  (cycle)
SCC₂ = {C}     (singleton)
```

The scheduler processes SCCs in topological order of the condensation graph. Singleton SCCs (no cycle) are refreshed exactly as today. Multi-node SCCs are **iterated to fixed point**.

### Fixed-Point Iteration for an SCC

For an SCC containing stream tables `{ST₁, ST₂, ..., STₙ}`:

```
iteration = 0
loop:
    total_changes = 0
    for each STᵢ in SCC (any order):
        (inserted, deleted) = refresh(STᵢ)
        total_changes += inserted + deleted
    iteration += 1
    if total_changes == 0:
        break  // fixed point reached
    if iteration >= max_iterations:
        mark all STᵢ as ERROR
        log warning "SCC did not converge after {max_iterations} iterations"
        break
```

Each refresh produces a delta (rows inserted + deleted). When a full pass over all SCC members produces zero changes, the system has converged.

### Monotonicity Analysis (Static Safety Check)

At `create_stream_table()` time, if the proposed stream table would create a cycle, analyze whether the cycle is **monotone-safe**:

**Safe operators** (monotone — adding input rows can only add output rows):
- `Scan`, `Filter`, `Project`, `InnerJoin`, `LeftJoin`
- `UnionAll`, `Union`, `Distinct`
- `CteScan`, `RecursiveCte`

**Unsafe operators** (non-monotone — adding input rows may remove output rows):
- `Aggregate` (COUNT/SUM can decrease on DELETE)
- `Except` (adding rows to the right branch removes output)
- `Window` (rank can change, causing rows to appear/disappear)

For cycles containing only safe operators, convergence is guaranteed (in at most N iterations where N is the longest simple path in the SCC). For cycles containing unsafe operators, the system should either:

1. **Reject** — refuse to create the cycle (safest default)
2. **Warn and allow** — with explicit user opt-in via a parameter like `ALLOW CIRCULAR`

### Convergence Guarantee

For monotone cycles, the theory guarantees convergence because:
- Each iteration can only add rows (monotonicity)
- The result set is bounded (finite number of possible rows from finite input tables)
- Therefore, the sequence of result sets forms a monotonically increasing chain bounded above, which must reach a fixed point in finite steps

The maximum number of iterations is bounded by the output cardinality, but in practice convergence happens in much fewer iterations (typically 2-5 for transitive closure patterns).

---

## Implementation

### Part 1 — Tarjan's Algorithm for SCC Decomposition

Replace or augment Kahn's algorithm with Tarjan's algorithm to decompose the graph into SCCs.

#### New Method: `DtDag::compute_sccs()`

```rust
/// A strongly connected component of the dependency graph.
pub struct Scc {
    /// Node IDs in this SCC.
    pub nodes: Vec<NodeId>,
    /// True if this SCC has more than one node (i.e., contains a cycle).
    pub is_cyclic: bool,
}

/// Compute SCCs using Tarjan's algorithm (1972).
///
/// Returns SCCs in reverse topological order of the condensation graph
/// (upstream SCCs first).
pub fn compute_sccs(&self) -> Vec<Scc> { ... }
```

Tarjan's algorithm runs in O(V + E) time, same as Kahn's.

#### New Method: `DtDag::condensation_order()`

Replaces `topological_order()` for the scheduler:

```rust
/// Return stream table nodes in refresh order, with SCCs grouped.
///
/// Singleton SCCs (no cycle) are returned as-is. Multi-node SCCs are
/// returned as groups that must be iterated to fixed point.
pub fn condensation_order(&self) -> Result<Vec<Scc>, PgStreamError> {
    let sccs = self.compute_sccs();
    // SCCs are already in reverse topological order from Tarjan's
    Ok(sccs)
}
```

#### Files to Change

| File | Change |
|---|---|
| `src/dag.rs` | Add `Scc` struct, `compute_sccs()`, `condensation_order()` |
| `src/dag.rs` (tests) | Unit tests for SCC decomposition |

#### Unit Tests

```rust
#[test]
fn test_scc_no_cycles() {
    // A → B → C: three singleton SCCs
}

#[test]
fn test_scc_simple_cycle() {
    // A → B → A: one SCC {A, B}
}

#[test]
fn test_scc_mixed() {
    // {A, B} cycle → C singleton → {D, E} cycle
    // condensation order: [A,B], [C], [D,E]
}

#[test]
fn test_scc_self_loop() {
    // A → A: one SCC {A} (is_cyclic = true)
}

#[test]
fn test_condensation_order_is_topological() {
    // verify upstream SCCs appear before downstream SCCs
}
```

#### Estimated Effort

**Medium** — ~120 lines for Tarjan's algorithm + ~80 lines of tests.

---

### Part 2 — Monotonicity Checker

Add a static analysis pass that determines whether an `OpTree` is monotone (safe for cyclic fixed-point iteration).

#### New Function: `check_monotonicity()`

```rust
/// Check if an OpTree is monotone (safe for cyclic dependencies).
///
/// Returns `Ok(())` if all operators are monotone, or `Err` with the
/// first non-monotone operator found.
pub fn check_monotonicity(tree: &OpTree) -> Result<(), PgStreamError> {
    match tree {
        OpTree::Scan { .. } => Ok(()),
        OpTree::Filter { child, .. } => check_monotonicity(child),
        OpTree::Project { child, .. } => check_monotonicity(child),
        OpTree::InnerJoin { left, right, .. } => {
            check_monotonicity(left)?;
            check_monotonicity(right)
        }
        OpTree::LeftJoin { left, right, .. } => {
            check_monotonicity(left)?;
            check_monotonicity(right)
        }
        OpTree::Distinct { child } => check_monotonicity(child),
        OpTree::UnionAll { children } => {
            for c in children { check_monotonicity(c)?; }
            Ok(())
        }
        OpTree::Aggregate { .. } => Err(PgStreamError::UnsupportedOperator(
            "Aggregate is not monotone — cannot be part of a cyclic dependency".into()
        )),
        OpTree::Except { .. } => Err(PgStreamError::UnsupportedOperator(
            "EXCEPT is not monotone — cannot be part of a cyclic dependency".into()
        )),
        OpTree::Window { .. } => Err(PgStreamError::UnsupportedOperator(
            "Window functions are not monotone — cannot be part of a cyclic dependency".into()
        )),
        // Recurse into subqueries, CTEs, etc.
        OpTree::Subquery { child, .. } => check_monotonicity(child),
        OpTree::CteScan { .. } => Ok(()),
        OpTree::RecursiveCte { base, recursive, .. } => {
            check_monotonicity(base)?;
            check_monotonicity(recursive)
        }
        OpTree::RecursiveSelfRef { .. } => Ok(()),
    }
}
```

#### Integration with `create_stream_table()`

When `check_for_cycles()` detects a cycle, instead of immediately returning `CycleDetected`, it should:

1. Parse the defining queries of all stream tables in the proposed cycle
2. Run `check_monotonicity()` on each
3. If all are monotone → allow the cycle (store a flag in the catalog)
4. If any is non-monotone → return `CycleDetected` with an enhanced error message explaining which operator breaks monotonicity

#### Files to Change

| File | Change |
|---|---|
| `src/dvm/parser.rs` | Add `check_monotonicity()` function |
| `src/api.rs` | Update `check_for_cycles()` to allow monotone cycles |
| `src/dvm/parser.rs` (tests) | Unit tests for monotonicity checker |

#### Unit Tests

```rust
#[test]
fn test_scan_filter_project_is_monotone() { }

#[test]
fn test_join_is_monotone() { }

#[test]
fn test_aggregate_is_not_monotone() { }

#[test]
fn test_except_is_not_monotone() { }

#[test]
fn test_window_is_not_monotone() { }

#[test]
fn test_nested_non_monotone_detected() {
    // Filter { child: Aggregate { ... } } → non-monotone
}
```

#### Estimated Effort

**Low** — ~60 lines for the checker + ~40 lines of tests.

---

### Part 3 — Catalog Changes

Track which stream tables participate in cyclic SCCs so the scheduler and refresh logic know to use fixed-point iteration.

#### Schema Change

```sql
ALTER TABLE pgstream.pgs_stream_tables
    ADD COLUMN scc_id INT;
```

- `scc_id IS NULL` — stream table is in a singleton SCC (no cycle)
- `scc_id = N` — stream table is part of cyclic SCC N (all members share the same value)

When a `create_stream_table()` creates a cycle:
1. Assign a new `scc_id` to all members of the cycle
2. Store it in the `pgs_stream_tables` catalog

When `drop_stream_table()` removes a member and breaks the cycle:
1. Recompute SCCs
2. Clear `scc_id` for any members no longer in a cycle

#### New Refresh History Column

```sql
ALTER TABLE pgstream.pgs_refresh_history
    ADD COLUMN fixpoint_iteration INT;
```

Records which iteration of the fixed-point loop produced this refresh. `NULL` for non-cyclic refreshes. Useful for debugging convergence.

#### Files to Change

| File | Change |
|---|---|
| `src/lib.rs` | Add `scc_id` column to `pgs_stream_tables` DDL, `fixpoint_iteration` to `pgs_refresh_history` |
| `src/catalog.rs` | Add `scc_id` to `StreamTableMeta`, `fixpoint_iteration` to `RefreshRecord` |
| `tests/common/mod.rs` | Mirror DDL changes |
| `docs/SQL_REFERENCE.md` | Document new columns |

#### Estimated Effort

**Low** — ~40 lines across DDL + struct changes.

---

### Part 4 — GUC Configuration

Add configuration variables for fixed-point iteration.

```rust
// Maximum iterations before declaring non-convergence
pub static PGS_MAX_FIXPOINT_ITERATIONS: GucSetting<i32> = GucSetting::<i32>::new(100);

// Whether to allow cyclic dependencies (opt-in safety gate)
pub static PGS_ALLOW_CIRCULAR: GucSetting<bool> = GucSetting::<bool>::new(false);
```

| GUC | Default | Description |
|---|---|---|
| `pg_stream.max_fixpoint_iterations` | `100` | Maximum iterations per SCC before declaring non-convergence and marking members as ERROR |
| `pg_stream.allow_circular` | `false` | Master switch: must be `true` to create cyclic dependencies. When `false`, cycle detection rejects as today |

#### Files to Change

| File | Change |
|---|---|
| `src/config.rs` | Add two new GUCs |
| `docs/CONFIGURATION.md` | Document new GUCs |

#### Estimated Effort

**Low** — ~30 lines.

---

### Part 5 — Scheduler: Fixed-Point Iteration

Modify the scheduler's refresh loop to handle multi-node SCCs.

#### Current Flow

```
DAG → topological_order() → for each ST: check schedule + refresh
```

#### New Flow

```
DAG → condensation_order() → for each SCC:
    if singleton:
        check schedule + refresh (unchanged)
    if cyclic:
        if any member needs refresh:
            iterate_to_fixpoint(SCC)
```

#### `iterate_to_fixpoint()` Implementation

```rust
fn iterate_to_fixpoint(
    scc: &Scc,
    dag: &DtDag,
    retry_states: &mut HashMap<i64, RetryState>,
    retry_policy: &RetryPolicy,
    now_ms: u64,
) {
    let max_iter = config::pg_stream_max_fixpoint_iterations();
    
    for iteration in 0..max_iter {
        let mut total_changes: i64 = 0;
        
        for node_id in &scc.nodes {
            let pgs_id = match node_id {
                NodeId::StreamTable(id) => *id,
                _ => continue,
            };
            
            let dt = match load_dt_by_id(pgs_id) {
                Some(dt) => dt,
                None => continue,
            };
            
            if dt.status != DtStatus::Active {
                continue;
            }
            
            let has_changes = check_upstream_changes(&dt);
            let action = refresh::determine_refresh_action(&dt, has_changes);
            
            match action {
                RefreshAction::NoData if iteration > 0 => {
                    // This member has converged — no changes from upstream
                }
                _ => {
                    let result = execute_scheduled_refresh_with_iteration(&dt, action, iteration);
                    match result {
                        RefreshOutcome::Success(inserted, deleted) => {
                            total_changes += inserted + deleted;
                        }
                        RefreshOutcome::RetryableFailure | RefreshOutcome::PermanentFailure => {
                            // Abort the entire SCC iteration — can't converge
                            // if a member fails
                            log!(
                                "pg_stream: SCC fixpoint aborted — {}.{} failed",
                                dt.pgs_schema, dt.pgs_name,
                            );
                            return;
                        }
                    }
                }
            }
        }
        
        if total_changes == 0 {
            log!(
                "pg_stream: SCC converged after {} iteration(s)",
                iteration + 1,
            );
            return;
        }
    }
    
    // Non-convergence: mark all SCC members as ERROR
    warning!(
        "pg_stream: SCC did not converge after {} iterations — marking members as ERROR",
        max_iter,
    );
    for node_id in &scc.nodes {
        if let NodeId::StreamTable(id) = node_id {
            if let Err(e) = StreamTableMeta::set_status(*id, "ERROR") {
                log!("pg_stream: failed to set ERROR status for ST {}: {}", id, e);
            }
        }
    }
}
```

#### Convergence Detection

The key insight is that `execute_differential_refresh()` already returns `(rows_inserted, rows_deleted)`. When both are zero for all members in a pass, the SCC has converged. No new mechanism is needed — the existing return values provide natural convergence detection.

For FULL refresh mode within an SCC, convergence detection requires comparing the new result against the current storage, which the FULL refresh path doesn't currently do (it truncates and re-inserts). Two options:

1. **Prohibit FULL mode in cyclic SCCs** — only DIFFERENTIAL mode is meaningful for fixed-point iteration. FULL mode would re-insert all rows every iteration, never converging.
2. **Use a hash comparison** — compute a hash of the FULL result before and after. This adds overhead but enables convergence detection without differential tracking.

**Recommendation**: Require DIFFERENTIAL mode for all members of a cyclic SCC. This is natural since fixed-point iteration is inherently an incremental process.

#### Schedule Semantics for SCCs

When any member of an SCC exceeds its schedule, the **entire SCC** is refreshed (iterated to fixed point). The effective schedule for the SCC is `MIN(schedule)` across all members. This follows naturally from the semantics: if any member is stale, all members might be stale due to the mutual dependency.

#### Files to Change

| File | Change |
|---|---|
| `src/scheduler.rs` | Replace `topological_order()` with `condensation_order()`, add `iterate_to_fixpoint()` |
| `src/scheduler.rs` (tests) | Unit tests for fixed-point convergence |
| `src/refresh.rs` | Return change counts more prominently; validate DIFFERENTIAL mode for SCC members |

#### Estimated Effort

**High** — ~200 lines for scheduler changes + ~100 lines of tests.

---

### Part 6 — Validation at Creation Time

Modify `check_for_cycles()` in `src/api.rs` to conditionally allow cycles.

#### New Flow

```rust
fn check_for_cycles(source_relids: &[(Oid, String)]) -> Result<(), PgStreamError> {
    // ... existing DAG build ...
    
    match dag.detect_cycles() {
        Ok(()) => Ok(()),  // No cycle — always allowed
        Err(PgStreamError::CycleDetected(nodes)) => {
            // Cycle detected — check if allowed
            if !config::pg_stream_allow_circular() {
                return Err(PgStreamError::CycleDetected(nodes));
            }
            
            // Check monotonicity of all STs in the cycle
            for node_name in &nodes {
                let meta = StreamTableMeta::get_by_name(node_name)?;
                let parse_result = parse_defining_query(&meta.defining_query)?;
                check_monotonicity(&parse_result.tree)?;
            }
            
            // All members are monotone — allow the cycle
            // Verify all members use DIFFERENTIAL mode
            for node_name in &nodes {
                let meta = StreamTableMeta::get_by_name(node_name)?;
                if meta.refresh_mode != RefreshMode::Differential {
                    return Err(PgStreamError::InvalidArgument(format!(
                        "stream table '{}' must use DIFFERENTIAL refresh mode \
                         to participate in a circular dependency",
                        node_name,
                    )));
                }
            }
            
            Ok(())
        }
        Err(e) => Err(e),
    }
}
```

#### Files to Change

| File | Change |
|---|---|
| `src/api.rs` | Update `check_for_cycles()` to conditionally allow monotone cycles |
| `src/api.rs` | Assign `scc_id` to cycle members after successful creation |
| `src/api.rs` | Update `drop_stream_table()` to recompute SCCs and clear `scc_id` |

#### Estimated Effort

**Medium** — ~100 lines.

---

### Part 7 — Monitoring and Observability

Surface cycle and convergence information in the monitoring infrastructure.

#### Update `pg_stat_stream_tables` View

Add columns:
- `scc_id INT` — SCC identifier (NULL if not in a cycle)
- `last_fixpoint_iterations INT` — number of iterations in the last fixed-point run

#### Update `pgs_status()`

Add column:
- `scc_id INT` — which SCC this ST belongs to (NULL for non-cyclic)

#### New Monitoring Function: `pgstream.pgs_scc_status()`

```sql
pgstream.pgs_scc_status() → SETOF record(
    scc_id              INT,
    member_count        INT,
    members             TEXT[],
    last_iterations     INT,
    last_converged_at   TIMESTAMPTZ,
    is_monotone         BOOL
)
```

#### Files to Change

| File | Change |
|---|---|
| `src/lib.rs` | Update `pg_stat_stream_tables` view |
| `src/api.rs` | Update `pgs_status()`, add `pgs_scc_status()` |
| `docs/SQL_REFERENCE.md` | Document new columns and function |
| `src/monitor.rs` | Update Prometheus metrics if applicable |

#### Estimated Effort

**Medium** — ~80 lines.

---

### Part 8 — Documentation and E2E Tests

#### E2E Tests

```sql
-- Test 1: Simple cycle with monotone queries (transitive closure)
CREATE TABLE edges (src INT, dst INT);
INSERT INTO edges VALUES (1,2), (2,3);

SET pg_stream.allow_circular = true;

-- reach_a finds paths through reach_b
SELECT pgstream.create_stream_table('reach_a',
    'SELECT DISTINCT e.src, rb.dst
     FROM edges e
     INNER JOIN reach_b rb ON e.dst = rb.src',
    '1m', 'DIFFERENTIAL');

-- reach_b starts from edges and extends through reach_a
SELECT pgstream.create_stream_table('reach_b',
    'SELECT DISTINCT e.src, e.dst FROM edges e
     UNION ALL
     SELECT ra.src, e.dst
     FROM reach_a ra
     INNER JOIN edges e ON ra.dst = e.src',
    '1m', 'DIFFERENTIAL');

-- After refresh cycle converges: both contain transitive closure
-- reach_a: (1,3), (2,3) — or similar depending on exact split
-- reach_b: (1,2), (2,3), (1,3)

-- Test 2: Non-monotone cycle rejected
-- (aggregate in cycle member)

-- Test 3: Convergence within max_iterations

-- Test 4: Non-convergence hits max_iterations → ERROR status

-- Test 5: Drop member breaks cycle → scc_id cleared

-- Test 6: allow_circular=false rejects cycles (default behavior preserved)
```

#### Documentation

- Add "Circular Dependencies" section to `docs/ARCHITECTURE.md`
- Add `pg_stream.max_fixpoint_iterations` and `pg_stream.allow_circular` to `docs/CONFIGURATION.md`
- Add `pgs_scc_status()` to `docs/SQL_REFERENCE.md`
- Add note to `README.md` Limitations table: `Circular dependencies | ✅ Supported | Monotone queries only; opt-in via pg_stream.allow_circular`

#### Files to Change

| File | Change |
|---|---|
| `tests/e2e_circular_tests.rs` | **New file** — E2E tests for circular dependencies |
| `docs/ARCHITECTURE.md` | Add circular dependency section |
| `docs/CONFIGURATION.md` | Document new GUCs |
| `docs/SQL_REFERENCE.md` | Document `pgs_scc_status()` and new columns |
| `README.md` | Update Limitations table |

#### Estimated Effort

**Medium** — ~150 lines of tests + ~100 lines of documentation.

---

## Execution Order

| Part | Description | Depends On | Complexity |
|------|------------|-----------|------------|
| 1 | Tarjan's SCC algorithm | — | Medium |
| 2 | Monotonicity checker | — | Low |
| 3 | Catalog changes | — | Low |
| 4 | GUC configuration | — | Low |
| 5 | Scheduler fixed-point iteration | Parts 1, 3, 4 | High |
| 6 | Creation-time validation | Parts 1, 2, 3, 4 | Medium |
| 7 | Monitoring and observability | Parts 3, 5 | Medium |
| 8 | Documentation and E2E tests | All above | Medium |

**Recommended order**: Parts 1-4 (independent, can be done in parallel) → Part 5 → Part 6 → Part 7 → Part 8

**Total estimated effort**: ~1000 lines of code + ~350 lines of tests + documentation.

---

## Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| Non-convergence causes resource exhaustion | High | Hard cap via `max_fixpoint_iterations` GUC; ERROR status on non-convergence |
| Users accidentally create cycles | Medium | `allow_circular = false` by default; explicit opt-in required |
| Non-monotone cycle slips past checker | Medium | Conservative checker — reject anything with Aggregate, Except, Window in cycle |
| Performance regression for non-cyclic graphs | Low | Tarjan's is O(V+E) like Kahn's; singleton SCCs follow the existing fast path |
| FULL mode in cycles never converges | Medium | Reject FULL mode for cycle members at creation time |
| SCC recomputation on drop is expensive | Low | Only recompute when dropping a member of a known SCC (`scc_id IS NOT NULL`) |

---

## Alternatives Considered

### Alternative 1: Rewrite as a single `WITH RECURSIVE` query

Instead of supporting inter-table cycles, automatically rewrite mutually dependent stream tables into a single stream table with a `WITH RECURSIVE` CTE. This avoids the complexity of fixed-point iteration in the scheduler.

**Rejected because**: This changes the user's desired topology. Users may want separate stream tables for independent scheduling, permissions, or operational isolation. It also requires complex query rewriting to merge two defining queries into one recursive CTE.

### Alternative 2: Allow cycles but only with FULL refresh

Refresh all members of a cycle using FULL mode, comparing old vs. new results to detect convergence. This avoids the requirement for DIFFERENTIAL mode.

**Rejected because**: FULL refresh of N tables × M iterations is extremely expensive. The entire point of cycles is to propagate incremental changes, which requires DIFFERENTIAL mode.

### Alternative 3: Async fixed-point with separate worker

Spawn a dedicated background worker per cyclic SCC that iterates independently of the main scheduler.

**Deferred**: This is a valid optimization for large systems with many SCCs, but adds significant complexity (worker lifecycle management, inter-worker coordination). The initial implementation should use the existing scheduler with synchronous iteration.

---

## Implementation Status

| Part | Description | Status |
|------|------------|--------|
| 1 | Tarjan's SCC algorithm | ❌ Not started |
| 2 | Monotonicity checker | ❌ Not started |
| 3 | Catalog changes | ❌ Not started |
| 4 | GUC configuration | ❌ Not started |
| 5 | Scheduler fixed-point iteration | ❌ Not started |
| 6 | Creation-time validation | ❌ Not started |
| 7 | Monitoring and observability | ❌ Not started |
| 8 | Documentation and E2E tests | ❌ Not started |
