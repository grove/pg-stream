# PLAN_TEST_PROPERTY_BASED_INVARIANTS.md — Ranked Expansion Plan for Property-Based Invariant Testing

**Status:** Proposed  
**Date:** 2026-03-16  
**Scope:** Expand property-based and deterministic invariant testing in the existing test suites, with emphasis on stateful E2E behavior where fixed examples are least able to catch drift, interleaving bugs, and sequence-sensitive correctness failures.

---

## 1. Motivation

pg_trickle already has two strong foundations for invariant-style testing:

1. `tests/property_tests.rs` uses `proptest` for pure Rust properties.
2. `tests/e2e_property_tests.rs` uses a deterministic PRNG harness to assert the core DBSP invariant under randomized DML.

That second suite has already proven its value: randomized invariant checks found correctness bugs that hand-written E2E examples did not catch. The next step is not “more randomness everywhere”, but a **ranked expansion** into the suites where correctness is most sequence-sensitive:

- multi-layer DAGs
- cross-source / diamond consistency
- circular fixpoint iteration
- failure isolation and recovery
- scheduler- and topology-sensitive refresh behavior

The goal is to raise assurance where fixed scenario tests are weakest, while preserving the readability and targeted intent of the existing example-driven suites.

---

## 2. Goals

### Primary goals

1. Increase confidence that stream table contents equal the defining query result after arbitrary bounded mutation traces.
2. Add property-style coverage for **metadata invariants**, not just row-set equality.
3. Reuse the existing deterministic E2E property-testing style instead of introducing a second heavy E2E framework.
4. Prioritize suites where bugs are most likely to emerge only after sequences of mutations or refresh interleavings.

### Secondary goals

1. Make failures reproducible via seeds and trace replay.
2. Share harness code instead of copying ad hoc PRNG logic into multiple suites.
3. Support a small fast PR-time mode and a wider nightly/manual mode.

### Non-goals

1. Do not convert every example E2E test into a property-based test.
2. Do not replace focused SQL semantics tests with random generation.
3. Do not attempt unbounded fuzzing in Docker-backed E2E runs.

---

## 3. Design Principles

### 3.1 Deterministic over opaque randomness

For E2E tests, prefer the existing deterministic seeded approach from `tests/e2e_property_tests.rs` over introducing `proptest` directly into Docker-heavy tests. The requirements here are:

- reproducible failures
- bounded case counts
- explicit operation traces
- controllable runtime

For pure Rust logic, continue using `proptest`.

### 3.2 Invariants should include metadata

A property test should assert more than `Contents(ST) = Q(DB)` when the feature under test has richer semantics. Relevant metadata invariants include:

- `frontier` never regresses
- `data_timestamp` does not move on a no-op refresh
- SCC IDs are equal within a cycle and cleared when the cycle disappears
- a failed branch does not corrupt healthy siblings
- no-refresh and no-op cycles preserve output stability
- diamond / atomic snapshot consistency is maintained across related STs

### 3.3 Small bounded state spaces beat huge random workloads

Keep schemas and topologies small:

- 2–3 source tables
- 2–5 stream tables
- 5–20 base rows
- 5–20 mutation cycles

The value comes from many small diverse traces, not from large data volume.

### 3.4 Preserve readable example tests

Existing example-driven suites remain useful for:

- feature discovery
- regression explanation
- debugging
- precise behavioral requirements

Property-based tests should complement them, not replace them.

---

## 4. Ranked Recommendations

### Priority summary

| Rank | Area | Current Suite(s) | Why it benefits | Priority | Effort |
|------|------|------------------|-----------------|:--------:|:------:|
| 1 | **[DONE]** Multi-cycle DAG correctness | `e2e_property_dag_tests.rs` | Highest risk of cumulative drift across layers and cycles | P0 | 1–2 wk |
| 2 | **[DONE]** Cross-source / diamond consistency | `e2e_property_snapshot_tests.rs` | Simultaneous source changes and mixed-snapshot bugs are sequence-sensitive | P0 | 1–2 wk |
| 3 | **[DONE]** Circular / SCC / fixpoint correctness | `e2e_property_circular_tests.rs` | Scheduler convergence and SCC bookkeeping are fragile under graph variation | P1 | 1–2 wk |
| 4 | **[DONE]** Error isolation and recovery | `e2e_dag_error_tests.rs` | Example tests cover specific stories, not diverse failure/recovery traces | P1 | 4–6 days |
| 5 | Topology / scheduler stress | `e2e_dag_topology_tests.rs`, `e2e_dag_autorefresh_tests.rs` | Needed for multi-source and branch-interaction assurance | P2 | 4–6 days |
| 6 | Pure Rust DAG / scheduler helper logic | `tests/property_tests.rs`, unit tests in `src/dag.rs`, `src/version.rs` | Cheap additional assurance for ordering / monotonic metadata helpers | P2 | 2–4 days |
| 7 | Legacy lifecycle / scenario workflows | `scenario_tests.rs`, `workflow_tests.rs` | Lower ROI because stronger suites already cover the core invariant | P3 | defer |

### Recommended order of execution

1. Build shared E2E property harness.
2. Apply it to multi-cycle DAG tests.
3. Apply it to cross-source / diamond consistency tests.
4. Add bounded circular fixpoint properties.
5. Add failure-isolation and scheduler/topology property traces.

---

## 5. Where More Property-Based Invariant Testing Will Help Most

## 5.1 Rank 1 — Multi-Cycle DAG Suites

**Target files:**

- `tests/e2e_multi_cycle_dag_tests.rs`
- `tests/e2e_pipeline_dag_tests.rs`

### Why this is first

These suites already test DAG correctness, but mostly through fixed scripts. The highest-value missing assurance is that **repeated mixed mutations across a pipeline do not accumulate drift**. This is the exact class of failure that property-style traces catch well.

### Property families to add

1. **Linear 3-layer trace invariant**
   - Topology: `base -> l1 -> l2 -> l3`
   - Random operations per cycle: insert, update, delete, no-op
   - Assert after each cycle that all three STs match their ground-truth queries.

2. **Diamond trace invariant**
   - Topology: `base -> l1a`, `base -> l1b`, `l1a + l1b -> l2`
   - Allow both single-source and same-cycle multi-source mutation batches.
   - Assert branch correctness and apex correctness each cycle.

3. **Refresh interleaving invariant**
   - Manual refresh order variants within a topologically valid set.
   - Assert final convergence is independent of benign refresh ordering.

4. **No-op stability invariant**
   - Repeated no-op refreshes should not change row contents.
   - `data_timestamp` must not spuriously advance on no-op differential refreshes.

### Specific recommendations

Add a new file:

- `tests/e2e_property_dag_tests.rs`

Keep the current fixed-case files intact. The property file should share setup helpers, not replace the example tests.

---

## 5.2 Rank 2 — Cross-Source / Diamond Snapshot Consistency

**Target file:**

- `tests/e2e_snapshot_consistency_tests.rs`

### Why this is second

This suite is already written in the language of invariants, but its “random-ish” multi-round tests are still hand-shaped. Cross-source correctness is especially vulnerable to:

- partial refreshes
- mixed snapshots
- simultaneous changes in multiple sources
- diamond fan-in bugs

The roadmap explicitly identifies simultaneous multi-source delta correctness as an area requiring stronger proof via property-based testing.

### Property families to add

1. **Two-source aggregate consistency**
   - Mutate both sources independently and sometimes in the same cycle.
   - Assert per-ST contents and cross-ST aggregate equalities.

2. **Atomic snapshot invariant**
   - For atomic-consistency groups, assert related STs always reflect one consistent source snapshot after coordinated refresh.

3. **Diamond simultaneous-change invariant**
   - Mutate both upstream branches before refreshing the apex.
   - Assert the apex equals a direct SQL recomputation from base tables.

4. **Skewed source churn invariant**
   - One source changes heavily, another lightly or not at all.
   - Assert unaffected branches remain correct and the fan-in result converges.

### Specific recommendations

Extend `tests/e2e_snapshot_consistency_tests.rs` or create:

- `tests/e2e_property_snapshot_tests.rs`

Prefer a dedicated property file if the helper machinery becomes substantial.

---

## 5.3 Rank 3 — Circular / SCC / Fixpoint Correctness

**Target file:**

- `tests/e2e_circular_tests.rs`

### Why this is third

The circular suite covers named scenarios well, but cycle behavior is naturally sensitive to graph shape, mutation order, and scheduler convergence details. Example tests prove the intended stories; they do not prove that small variations are safe.

### Property families to add

1. **Bounded monotone-cycle convergence**
   - Small graph generator for 2-node or 3-node transitive-closure style recursion.
   - Assert convergence to the recursive SQL ground truth.

2. **SCC metadata invariant**
   - If two STs are in the same cycle, they share one `scc_id`.
   - If the cycle is broken, remaining STs have `scc_id IS NULL` unless another SCC still exists.

3. **Allow/disallow cycle invariant**
   - Under `allow_circular = false`, cycle introduction is always rejected.
   - Under `allow_circular = true`, only differential monotone cycles are allowed.

4. **Fixpoint iteration monotonicity**
   - `last_fixpoint_iterations` is positive after convergence.
   - Non-convergent bounded cases go to `ERROR` when max iterations are too low.

### Constraints

Keep this bounded and deterministic. Full property-style graph generation is likely too expensive for E2E. Use tiny topologies and small edge sets only.

### Specific recommendations

Add a new file:

- `tests/e2e_property_circular_tests.rs`

---

## 5.4 Rank 4 — Error Isolation and Recovery

**Target file:**

- `tests/e2e_dag_error_tests.rs`

### Why this matters

This suite already has good scenario tests, but the failure space is broader than the named examples. Bugs here often depend on the order of:

- data corruption / bad input introduction
- failing refreshes
- healthy sibling refreshes
- data repair
- downstream retries

### Property families to add

1. **Sibling isolation invariant**
   - One branch fails, another branch remains query-correct throughout.

2. **Recovery invariant**
   - After the data is repaired and refresh retried, all branches converge back to ground truth.

3. **Repeated failure invariant**
   - Repeated failing traces never corrupt healthy outputs.
   - Catalog error counters and status changes follow expected monotonic rules where applicable.

### Specific recommendations

Extend `tests/e2e_dag_error_tests.rs` with a small deterministic trace runner instead of a full separate file.

---

## 5.5 Rank 5 — Topology / Scheduler Interaction

**Target files:**

- `tests/e2e_dag_topology_tests.rs`
- `tests/e2e_dag_autorefresh_tests.rs`

### Why this is lower than the areas above

These are important, but the highest-risk correctness bugs are more likely in content convergence than in topology enumeration alone. Still, this area would gain assurance from bounded trace variation.

### Property families to add

1. **Fan-out independence invariant**
   - One source mutation must update all dependent leaves correctly and leave unrelated leaves unaffected.

2. **Deep-chain convergence invariant**
   - Small 4–5 layer chains over bounded traces.

3. **Autorefresh no-spurious-work invariant**
   - No data change means no content change, and scheduling metadata should not imply false work.

4. **Simultaneous upstream change invariant**
   - Aligns with the roadmap’s concern about future multi-source delta batching and diamond-flow correctness.

### Specific recommendations

Add only a few bounded property tests here. Avoid trying to cover every topology family exhaustively.

---

## 5.6 Rank 6 — Pure Rust Property Expansion

**Target files:**

- `tests/property_tests.rs`
- pure unit-test modules in `src/dag.rs`, `src/version.rs`, and related helpers

### Why this is still useful

This is cheaper and faster than Docker-backed E2E work, and can catch subtle logic bugs in:

- topological ordering
- cycle detection
- SCC grouping
- frontier comparison / serialization
- period and schedule helper monotonicity

### Property families to add

1. Random small DAG generation with edge-order independence.
2. SCC partition invariants for random bounded graphs.
3. Frontier merge / compare monotonicity.
4. Schedule-resolution monotonicity and bounds.

### Specific recommendations

Use `proptest` here, not the deterministic E2E harness.

---

## 5.7 Rank 7 — Legacy Lifecycle / Scenario Tests

**Target files:**

- `tests/scenario_tests.rs`
- `tests/workflow_tests.rs`

### Recommendation

Do not prioritize these for property-based conversion. They are valuable as readable smoke and workflow tests, but they are not the best place to spend invariant-testing effort because:

- they overlap with stronger E2E correctness suites
- they are not the highest-risk sequence-sensitive surfaces
- converting them would mostly duplicate coverage

Keep them example-driven.

---

## 6. Shared Harness Plan

## 6.1 New helper module

Add a reusable helper module for deterministic E2E property traces:

- `tests/e2e/property_support.rs` or
- `tests/common/property_support.rs`

Preferred: keep it near the E2E harness if it depends on `E2eDb`.

### Proposed helper types

```rust
struct SeededRng { ... }

enum MutationOp {
    Insert,
    Update,
    Delete,
    NoOp,
}

struct TraceStep {
    ops: Vec<...>,
    refresh_mode: RefreshStep,
}

struct TraceConfig {
    initial_rows: usize,
    cycles: usize,
    seed: u64,
}
```

### Proposed helper functions

```rust
async fn assert_st_query_invariant(...)
async fn assert_cross_st_invariant(...)
async fn assert_frontier_non_regression(...)
async fn assert_scc_consistency(...)
async fn run_trace(...)
```

### Extraction target

Lift common logic out of `tests/e2e_property_tests.rs` rather than re-implementing:

- deterministic PRNG
- tracked-ID helpers
- invariant assertion helpers

---

## 6.2 Configurability

Support environment-variable tuning for case counts and cycles.

### Proposed env vars

| Variable | Default | Purpose |
|----------|---------|---------|
| `PGS_PROP_CASES` | `8` | number of seeds / traces per test |
| `PGS_PROP_CYCLES` | `8` | mutation cycles per trace |
| `PGS_PROP_INITIAL_ROWS` | `12` | starting row count |
| `PGS_PROP_SEED` | unset | force one reproducible seed for debugging |

### CI policy

PR runs:

- low case count
- bounded cycles
- deterministic default seeds

Nightly/manual:

- larger case counts
- expanded seeds

---

## 7. Concrete Implementation Roadmap

## Phase 1 — Shared Harness Extraction (**COMPLETED**)

**Priority:** P0  
**Estimated effort:** 2–3 days

### Tasks

1. Extract deterministic RNG and tracked-ID helpers from `tests/e2e_property_tests.rs`.
2. Extract reusable invariant assertions for:
   - single ST vs query
   - set-op visibility handling
   - cross-ST aggregate checks
3. Add env-driven case-count and cycle-count configuration.
4. Add seed-printing and replay hooks to failure messages.

### Acceptance criteria

1. No copy-pasted PRNG implementations in new property files.
2. A new E2E property test can be written with less than ~50 lines of harness code.

---

## Phase 2 — Multi-Cycle DAG Property Suite (**COMPLETED**)

**Priority:** P0  
**Estimated effort:** 4–6 days

### Deliverable

- `tests/e2e_property_dag_tests.rs`

### Initial test set

1. `test_prop_linear_3_layer_mixed_trace`
2. `test_prop_diamond_mixed_trace`
3. `test_prop_noop_cycles_do_not_drift`
4. `test_prop_refresh_order_variation_converges`

### Acceptance criteria

1. All layer outputs match ground truth after every cycle.
2. No-op cycles preserve contents and do not create timestamp drift.
3. Failures print the seed and the exact trace step.

---

## Phase 3 — Snapshot / Diamond Property Suite (**COMPLETED**)

**Priority:** P0  
**Estimated effort:** 4–6 days

### Deliverable

- `tests/e2e_property_snapshot_tests.rs` or extensions to `e2e_snapshot_consistency_tests.rs`

### Initial test set

1. `test_prop_two_source_atomic_consistency`
2. `test_prop_diamond_same_cycle_multi_source_changes`
3. `test_prop_cross_source_skewed_churn_no_drift`

### Acceptance criteria

1. Per-ST correctness holds after each cycle.
2. Cross-ST invariants hold after each cycle.
3. Same-cycle mutations across multiple sources do not produce mixed snapshots in atomic configurations.

---

## Phase 4 — Circular / SCC Property Suite (**COMPLETED**)

**Priority:** P1  
**Estimated effort:** 4–6 days

### Deliverable

- `tests/e2e_property_circular_tests.rs`

### Initial test set

1. `test_prop_monotone_cycle_small_graph_converges`
2. `test_prop_cycle_members_share_scc_id`
3. `test_prop_break_cycle_clears_scc_ids`
4. `test_prop_disallowed_cycle_rejected`

### Acceptance criteria

1. Tiny monotone cycles converge to recursive SQL ground truth.
2. SCC IDs are stable and correct across create/alter/drop traces.
3. The suite remains bounded enough for PR CI.

---

## Phase 5 — Error Isolation / Recovery Property Traces (**COMPLETED**)

**Priority:** P1  
**Estimated effort:** 2–4 days

### Deliverable

- extensions to `tests/e2e_dag_error_tests.rs`

### Initial test set

1. `test_prop_sibling_failure_does_not_corrupt_healthy_branch`
2. `test_prop_failure_then_repair_reconverges`
3. `test_prop_repeated_failure_sequences_preserve_invariants`

### Acceptance criteria

1. Healthy branches remain query-correct throughout failing traces.
2. Repair + retry reconverges to full correctness.

---

## Phase 6 — Topology / Scheduler Property Traces (Completed)

**Priority:** P2  
**Estimated effort:** 2–4 days

### Deliverable

- small additions to `tests/e2e_dag_topology_tests.rs` and `tests/e2e_dag_autorefresh_tests.rs`

### Initial test set

1. `test_prop_fanout_leaf_independence`
2. `test_prop_deep_chain_small_trace`
3. `test_prop_autorefresh_no_spurious_changes`

### Acceptance criteria

1. Properties target scheduler/topology interactions specifically.
2. No attempt is made to exhaustively randomize every topology.

---

## Phase 7 — Pure Rust Property Expansion (Completed)

**Priority:** P2  
**Estimated effort:** 2–3 days

### Deliverable

- additions to `tests/property_tests.rs`

### Initial test set

1. random bounded DAG generation + topological-order validity
2. SCC partitioning invariants
3. frontier monotonicity / merge behavior
4. helper monotonicity for scheduling utilities

### Acceptance criteria

1. Fast runtime under `just test-unit`.
2. No PostgreSQL dependency.

---

## 8. Recommended File Organization

### New files

1. `tests/e2e_property_dag_tests.rs`
2. `tests/e2e_property_snapshot_tests.rs`
3. `tests/e2e_property_circular_tests.rs`
4. `tests/e2e/property_support.rs` or equivalent helper module

### Existing files to extend

1. `tests/e2e_dag_error_tests.rs`
2. `tests/e2e_dag_topology_tests.rs`
3. `tests/e2e_dag_autorefresh_tests.rs`
4. `tests/property_tests.rs`

### Files to leave mostly example-driven

1. `tests/scenario_tests.rs`
2. `tests/workflow_tests.rs`
3. most single-feature SQL semantics suites unless a known gap emerges

---

## 9. Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| E2E property tests become too slow | PR friction | keep bounded default case counts; expand only in nightly/manual runs |
| Failures are hard to reproduce | low debugging value | print seed, cycle, trace step, and SQL context |
| Too much overlap with existing example tests | maintenance cost | keep property tests focused on sequence-sensitive surfaces |
| Helpers become over-abstracted | brittle test code | extract only shared RNG, trace, and invariant helpers |
| Circular properties become flaky | CI instability | keep graph sizes tiny and schedules controlled |

---

## 10. Exit Criteria

This plan is complete when all of the following hold:

1. Multi-layer DAG suites have deterministic property-trace coverage.
2. Cross-source / diamond consistency has deterministic multi-source invariant traces.
3. Circular dependency behavior has bounded property-style SCC and convergence tests.
4. Failure-isolation and recovery behavior is covered beyond one-off examples.
5. Shared helper code exists for deterministic E2E property traces.
6. PR CI uses bounded defaults; nightly/manual CI runs broader seeds.
7. The new suites have already demonstrated value by either:
   - catching at least one previously unknown bug, or
   - preventing at least one regression during feature work in DAG, cycle, or snapshot code.

---

## 11. Final Recommendation

If only the top part of this plan is funded, the best return comes from:

1. shared E2E property harness extraction
2. multi-cycle DAG property tests
3. cross-source / diamond property tests

Those three items cover the most failure-prone stateful behavior in pg_trickle and align with the repository’s own experience: randomized invariant testing finds bugs that fixed examples miss.