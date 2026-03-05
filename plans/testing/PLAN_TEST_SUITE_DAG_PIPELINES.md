# PLAN: DAG Pipeline Test Suite — Comprehensive Coverage

**Status:** Complete  
**Date:** 2026-03-04 (coverage map updated 2026-03-04)  
**Branch:** `e2e_pipeline_dag_tests`  
**Scope:** Close all remaining gaps in multi-layer DAG pipeline E2E tests — multi-cycle cascades, mixed refresh modes, operational mid-pipeline changes, auto-refresh propagation, wide topologies, error resilience, and concurrent DML during pipeline refresh.

---

## Table of Contents

1. [Motivation](#motivation)
2. [Current Coverage Map](#current-coverage-map)
3. [Gap Analysis](#gap-analysis)
4. [Test Group 1: Multi-Cycle DAG Cascades](#test-group-1-multi-cycle-dag-cascades)
5. [Test Group 2: Mixed Refresh Modes](#test-group-2-mixed-refresh-modes)
6. [Test Group 3: Operational Mid-Pipeline Changes](#test-group-3-operational-mid-pipeline-changes)
7. [Test Group 4: Auto-Refresh Chain Propagation](#test-group-4-auto-refresh-chain-propagation)
8. [Test Group 5: Wide Topologies (Fan-Out & Fan-In)](#test-group-5-wide-topologies-fan-out--fan-in)
9. [Test Group 6: Error Resilience in Pipelines](#test-group-6-error-resilience-in-pipelines)
10. [Test Group 7: Concurrent DML During Pipeline Refresh](#test-group-7-concurrent-dml-during-pipeline-refresh)
11. [Test Group 8: IMMEDIATE Mode Cascades](#test-group-8-immediate-mode-cascades)
12. [Implementation Roadmap](#implementation-roadmap)
13. [Infrastructure & Helpers](#infrastructure--helpers)
14. [File Organization](#file-organization)

---

## Implementation Status

| Phase | Group | File | Tests | Status |
|-------|-------|------|------:|--------|
| 1 | Multi-Cycle DAG Cascades | `e2e_multi_cycle_dag_tests.rs` | 6 | **Done** |
| 2 | Mixed Refresh Modes | `e2e_mixed_mode_dag_tests.rs` | 5 | **Done** |
| 2 | Auto-Refresh Chain Propagation | `e2e_dag_autorefresh_tests.rs` | 5 | **Done** |
| 2 | Wide Topologies | `e2e_dag_topology_tests.rs` | 5 | **Done** |
| 3 | Operational Mid-Pipeline Changes | `e2e_dag_operations_tests.rs` | 7 | **Done** |
| 3 | Error Resilience | `e2e_dag_error_tests.rs` | 4 | **Done** |
| 3 | Concurrent DML | `e2e_dag_concurrent_tests.rs` | 3 | **Done** |
| 3 | IMMEDIATE Mode Cascades | `e2e_dag_immediate_tests.rs` | 4 | **Done** |
| **Total** | | | **39** | **39 done / 0 remaining** |

### Implementation Complete

All 39 tests across 8 groups have been implemented. No remaining work.

**Phase 3 (P2) — Implemented 2026-03-04:**

| Group | File | Tests | Description |
|-------|------|------:|-------------|
| 3 — Operational Mid-Pipeline | `e2e_dag_operations_tests.rs` | 7 | SUSPEND/ALTER/DROP on intermediate nodes, suspend-resume cycle |
| 6 — Error Resilience | `e2e_dag_error_tests.rs` | 4 | Failure isolation, error recovery, consecutive_errors tracking |
| 7 — Concurrent DML | `e2e_dag_concurrent_tests.rs` | 3 | DML between refreshes, concurrent inserts, rollback safety |
| 8 — IMMEDIATE Cascades | `e2e_dag_immediate_tests.rs` | 4 | 2-layer + 3-layer IMMEDIATE, mixed IMMED+DIFF, rollback |

---

## Motivation

The DAG (Directed Acyclic Graph) is the core orchestration structure in
pg_trickle. Stream tables form dependency chains: base table → ST₁ → ST₂ →
ST₃, with optional diamonds (fan-in) and fan-out. The refresh scheduler
traverses these DAGs in topological order (Kahn's algorithm — `src/dag.rs`),
and correctness depends on every layer seeing a consistent upstream state.

### What existing tests cover well

| Existing File | Layers | Modes | Cycles | Topology |
|---------------|:------:|:-----:|:------:|----------|
| `e2e_pipeline_dag_tests.rs` | 3–4 | DIFF only | 1 per test | Linear, diamond, fan-out |
| `e2e_getting_started_tests.rs` | 3 | DIFF only | 1 per test | Linear (recursive CTE) |
| `e2e_cascade_regression_tests.rs` | 2–3 | DIFF only | 1–2 | Linear |
| `e2e_multi_cycle_tests.rs` | **1** | DIFF only | 4–8 | Single ST |
| `e2e_diamond_tests.rs` | 2–3 | DIFF only | 0–1 | Diamond |
| `e2e_ivm_tests.rs` | 2 | IMMED | 1 | Linear |

### Identified gaps

1. **Multi-cycle on multi-layer DAGs** — the biggest gap. `e2e_multi_cycle_tests.rs` only tests single-layer STs. No test runs 4+ mutation-refresh cycles through a 3+ layer pipeline.
2. **Mixed refresh modes** — No test creates a pipeline where one layer is FULL, another DIFFERENTIAL, another IMMEDIATE.
3. **Operational mid-pipeline changes** — No test SUSPENDS, ALTERs, or DROPs an intermediate ST while data is flowing through the chain.
4. **Auto-refresh chain propagation** — `e2e_cascade_regression_tests.rs` has 2-layer auto-refresh tests but no 3+ layer scheduler-driven cascade.
5. **Wide fan-out** — `e2e_pipeline_dag_tests.rs` has some fan-out but no test with 4+ leaves sharing one root.
6. **IMMEDIATE mode multi-layer** — `test_ivm_cascading_immediate_sts` is 2-layer and requires explicit refresh for the second layer.
7. **Error resilience** — No test verifies that an error in one pipeline layer doesn't corrupt sibling or downstream layers.
8. **Concurrent DML** — `e2e_concurrent_tests.rs` tests concurrent refresh on the same ST, but not DML racing with a multi-layer pipeline refresh.

---

## Current Coverage Map

> **Post-implementation state** — all 39 tests from this branch are now merged.
> Legend: ✅ covered · ⚠️ partial · ❌ not covered

### Existing tests by topology and behavior

| Behavior | Linear 2L | Linear 3L | Diamond | Fan-Out | Wide (4+L / 4+ leaves) |
|----------|:---------:|:---------:|:-------:|:-------:|:----------------------:|
| Initial population | ✅ | ✅ | ✅ | ✅ | ✅ ¹ |
| INSERT cascade | ✅ | ✅ | ✅ | ✅ | ✅ |
| UPDATE cascade | ✅ | ✅ | ✅ ² | ✅ | ⚠️ ³ |
| DELETE cascade | ✅ | ✅ | ✅ | ✅ | ✅ |
| Multi-cycle (4+) | ✅ | ✅ | ✅ | ✅ | ⚠️ ³ |
| Mixed modes (FULL/DIFF/IMMED) | ✅ | ✅ | ⚠️ ⁴ | ✅ | ❌ |
| Mid-pipeline SUSPEND | ✅ | ✅ | ❌ | ❌ | ❌ |
| Mid-pipeline ALTER (schedule/mode/query) | ✅ | ✅ | ❌ | ❌ | ❌ |
| Mid-pipeline DROP | ✅ | ✅ | ❌ | ❌ | ❌ |
| Auto-refresh cascade | ✅ | ✅ | ✅ | ❌ | ❌ |
| Error in middle layer | ✅ | ✅ | ❌ | ❌ | ❌ |
| Concurrent DML | ✅ | ✅ | ❌ | ❌ | ❌ |
| IMMEDIATE propagation | ✅ | ✅ | ❌ | ❌ | ❌ |
| Rollback safety | ✅ | ✅ | ❌ | ❌ | ❌ |

¹ `test_fanout_4_leaves` and `test_deep_linear_5_layers` cover wide/deep initial population.  
² `test_mc_dag_diamond_multi_cycle` includes INSERT+DELETE over 5 cycles; UPDATE is implicitly covered via group-elimination-revival pattern.  
³ Wide topologies are covered for INSERT and DELETE isolation (`test_wide_fanout_deletion_isolation`) but not explicit UPDATE or multi-cycle stress.  
⁴ No test creates a diamond where the two intermediate legs use different refresh modes.

### Key architecture paths — all closed

| Code Path | Location | Previously | Now |
|-----------|----------|-----------|-----|
| `has_stream_table_source_changes()` with 3+ topo levels | `src/scheduler.rs` | 2-layer only | ✅ `test_autorefresh_3_layer_cascade`, `test_autorefresh_no_spurious_3_layer` |
| `resolve_calculated_schedule()` with deep chains | `src/dag.rs` | unit tests only | ✅ `test_autorefresh_calculated_schedule`, `test_autorefresh_staggered_schedules` |
| `determine_refresh_action()` FULL→DIFF transition in cascade | `src/refresh.rs` | not at pipeline level | ✅ `test_alter_mode_mid_pipeline_diff_to_full`, `test_mixed_mode_alter_mid_pipeline` |
| Diamond consistency group update after ALTER | `src/dag.rs` | create only | ✅ `test_alter_mode_mid_pipeline_diff_to_full` |
| Topological ordering with mixed TABLE/STREAM_TABLE sources | `src/scheduler.rs` | 2-layer only | ✅ `test_multi_source_diamond` |
| `pg_sys::BeginInternalSubTransaction` atomic diamond group path | `src/scheduler.rs` | untested (was broken via SPI) | ✅ `test_autorefresh_diamond_cascade` (EC-13 fix) |

### Test file inventory

| File | Tests | Topology | Cycles | Notes |
|------|------:|----------|:------:|-------|
| `e2e_pipeline_dag_tests.rs` | 14 | Linear, diamond, fan-out | 1 | Pre-existing: broad single-cycle coverage |
| `e2e_cascade_regression_tests.rs` | 8 | Linear 2–3L | 1–2 | Pre-existing: regression guard |
| `e2e_diamond_tests.rs` | 15 | Diamond | 0–1 | Pre-existing + EC-13 default update |
| `e2e_multi_cycle_tests.rs` | 5 | Single-ST | 4–8 | Pre-existing: single-layer multi-cycle |
| `e2e_multi_cycle_dag_tests.rs` | 6 | Linear 3L + diamond | 4–10 | **New**: multi-cycle at pipeline level |
| `e2e_mixed_mode_dag_tests.rs` | 5 | Linear 2–3L + immediate | 1–3 | **New**: FULL/DIFF/IMMED combinations |
| `e2e_dag_autorefresh_tests.rs` | 5 | Linear 3L + diamond | scheduler | **New**: scheduler-driven cascade |
| `e2e_dag_topology_tests.rs` | 5 | Fan-out, deep (5L), multi-source | 1 | **New**: wide/deep topologies |
| `e2e_dag_operations_tests.rs` | 7 | Linear 2–3L | 1–2 | **New**: SUSPEND/ALTER/DROP mid-pipeline |
| `e2e_dag_error_tests.rs` | 4 | Linear 2–3L | 1–2 | **New**: error isolation and recovery |
| `e2e_dag_concurrent_tests.rs` | 3 | Linear 2–3L | 1–2 | **New**: DML racing pipeline refresh |
| `e2e_dag_immediate_tests.rs` | 4 | Linear 2–3L + mixed | 1–2 | **New**: IMMEDIATE mode cascades |
| **New total** | **39** | | | |

### Remaining gaps (acknowledged, no current plan)

| Gap | Why not covered now | Priority |
|-----|---------------------|----------|
| Wide topology (4+ leaves) + multi-cycle | High container load; single-cycle is adequate | P3 |
| Diamond + mixed modes | Combinatorial; base diamond + mixed linear covers the logic | P3 |
| Diamond + mid-pipeline ALTER/SUSPEND | Uncommon operational path | P3 |
| Auto-refresh on fan-out (≥4 leaves) | No scheduler policy drives fan-out distinctly | P3 |
| IMMEDIATE + diamond | Not a supported combination today | P3 |

---

## Gap Analysis

### Priority Rating

| Priority | Meaning | Gap Groups |
|----------|---------|------------|
| **P0** | Bugs hide here; drift accumulates silently | Group 1 (multi-cycle DAG) |
| **P1** | Missing coverage for shipped features | Groups 2, 4, 5 |
| **P2** | Edge cases in operational workflows | Groups 3, 6, 7, 8 |

---

## Test Group 1: Multi-Cycle DAG Cascades

**Priority:** P0 — Highest  
**Target file:** `tests/e2e_multi_cycle_dag_tests.rs` (new)  
**Rationale:** This is the intersection of two tested dimensions (multi-cycle + multi-layer) that has **zero** coverage. Delta drift accumulates over cycles and the defining characteristic of IVM is that it converges to the correct answer across many updates. A single mutation-then-check does not catch cumulative errors.

### Schema: Reuse the Nexmark-inspired base tables

```
auctions, bidders, bids (base tables)
    → auction_bids (L1: JOIN + agg)
        → bidder_stats (L2: agg on L1)
            → category_metrics (L3: agg on L2)
```

### Tests

| # | Test Name | Description | Cycles | DML Operations |
|---|-----------|-------------|:------:|----------------|
| 1.1 | `test_mc_dag_insert_heavy_10_cycles` | 10 cycles of INSERT-only DML into all 3 base tables; full pipeline refresh each cycle; `assert_st_matches_query` at all 3 layers | 10 | INSERT |
| 1.2 | `test_mc_dag_mixed_dml_5_cycles` | 5 cycles with mixed INSERT/UPDATE/DELETE at the base; pipeline refresh; check all layers | 5 | INSERT+UPDATE+DELETE |
| 1.3 | `test_mc_dag_noop_cycle_no_drift` | Insert data, refresh pipeline, then run 5 no-op refresh cycles (no DML). Assert `data_timestamp` is stable and contents unchanged at all layers. | 5 | none |
| 1.4 | `test_mc_dag_group_elimination_revival` | Delete all data for a group → refresh → group disappears at all layers → insert new data for same group → refresh → group reappears at all layers | 4 | DELETE then INSERT |
| 1.5 | `test_mc_dag_bulk_mutation_stress` | Single cycle with large batch DML (100 INSERTs, 50 UPDATEs, 30 DELETEs) followed by pipeline refresh | 1 | bulk mixed |
| 1.6 | `test_mc_dag_diamond_multi_cycle` | Diamond topology: base → L1a, base → L1b, L1a+L1b → L2. Run 5 cycles of mixed DML; verify L2 converges each cycle. | 5 | INSERT+DELETE |

### Implementation pattern

```rust
#[tokio::test]
async fn test_mc_dag_insert_heavy_10_cycles() {
    let db = E2eDb::new().await.with_extension().await;
    // Setup base tables + pipeline (reusable helpers)
    setup_nexmark_base(&db).await;
    create_nexmark_pipeline(&db).await;

    let queries = nexmark_defining_queries(); // HashMap<&str, &str>

    for cycle in 1..=10 {
        // Insert new data each cycle
        db.execute(&format!(
            "INSERT INTO bidders (name) VALUES ('cycle_{cycle}_bidder')"
        )).await;
        db.execute(&format!(
            "INSERT INTO auctions (title, category, seller_id) \
             VALUES ('item_{cycle}', 'cat_a', 1)"
        )).await;
        db.execute(&format!(
            "INSERT INTO bids (auction_id, bidder_id, amount) \
             VALUES ({cycle}, 1, {cycle} * 10)"
        )).await;

        // Refresh in topological order
        db.refresh_st("auction_bids").await;
        db.refresh_st("bidder_stats").await;
        db.refresh_st("category_metrics").await;

        // DBSP invariant at every layer
        for (name, query) in &queries {
            db.assert_st_matches_query(name, query).await;
        }
    }
}
```

### What this catches

- **Cumulative delta drift** — Errors that only manifest after N cycles of incremental changes
- **Change buffer cleanup** — Each cycle's CDC entries are consumed and don't leak into the next
- **data_timestamp monotonicity** — Upstream timestamps advance correctly through the chain
- **Prepared statement cache** — Delta SQL prepared plans survive across cycles (PG generic plan threshold)

---

## Test Group 2: Mixed Refresh Modes

**Priority:** P1  
**Target file:** `tests/e2e_mixed_mode_dag_tests.rs` (new)  
**Rationale:** Real-world pipelines mix modes: a FULL-mode root (recursive CTE), DIFFERENTIAL middle layers, and possibly an IMMEDIATE leaf. The scheduler code (`determine_refresh_action`, `check_upstream_changes`) has different paths for each mode, but no E2E test exercises them in a single pipeline.

### Key architecture consideration

When a DIFFERENTIAL ST depends on a FULL-mode upstream ST, the upstream
does not have a CDC change buffer (FULL mode does a complete recompute).
The scheduler uses `data_timestamp` comparison for STREAM_TABLE
dependencies (see `has_stream_table_source_changes()` in
`src/scheduler.rs`), which works regardless of upstream refresh mode. But
this path has never been tested with mixed modes.

### Tests

| # | Test Name | Pipeline | Description |
|---|-----------|----------|-------------|
| 2.1 | `test_mixed_full_then_diff_2_layer` | base →[FULL] L1 →[DIFF] L2 | FULL root feeds DIFFERENTIAL leaf. Mutate base, refresh L1 (FULL recompute), refresh L2 (delta against L1). |
| 2.2 | `test_mixed_diff_then_full_2_layer` | base →[DIFF] L1 →[FULL] L2 | DIFFERENTIAL root feeds FULL leaf. Verify L2 does full recompute regardless. |
| 2.3 | `test_mixed_3_layer_full_diff_diff` | base →[FULL] L1 →[DIFF] L2 →[DIFF] L3 | 3-layer mixed. FULL root, two DIFFERENTIAL downstream. |
| 2.4 | `test_mixed_mode_alter_mid_pipeline` | base →[DIFF] L1 →[DIFF] L2, then ALTER L1 to FULL | Alter middle layer from DIFF→FULL, verify chain still converges. |
| 2.5 | `test_mixed_immediate_leaf` | base →[DIFF] L1 →[IMMED] L2 | DIFFERENTIAL root with IMMEDIATE leaf. Insert into base, refresh L1, verify L2 is updated. |

### Implementation considerations

The `create_st()` helper defaults to `"DIFFERENTIAL"`. For FULL mode:
```rust
db.create_st("layer1", query, "1m", "FULL").await;
```

For IMMEDIATE mode (no schedule needed):
```rust
db.execute(
    "SELECT pgtrickle.create_stream_table('imm_layer', $$...$$, NULL, 'IMMEDIATE')"
).await;
```

### What this catches

- `determine_refresh_action()` dispatching to FULL vs DIFFERENTIAL in a cascade context
- `has_stream_table_source_changes()` comparing `data_timestamp` when upstream mode differs
- Reinit behavior when ALTERing mode mid-pipeline
- IMMEDIATE trigger cascade with a DIFFERENTIAL upstream

---

## Test Group 3: Operational Mid-Pipeline Changes

**Priority:** P2  
**Target file:** `tests/e2e_dag_operations_tests.rs` (new)  
**Rationale:** Operators SUSPEND, ALTER (schedule/mode/query), and DROP
stream tables in production. If this happens to a middle node in a DAG,
the downstream STs must behave predictably. The scheduler must skip
SUSPENDED STs and the DAG must be rebuilt after DROP.

### Key architecture paths exercised

- `DAG_REBUILD_SIGNAL` — incremented on ALTER/DROP, scheduler rebuilds its in-memory `StDag`
- `status = 'SUSPENDED'` — scheduler skips SUSPENDED STs in topological walk
- `determine_refresh_action()` returning `NoAction` for SUSPENDED upstream
- DROP cascade — `drop_stream_table()` refuses if dependents exist (unless they're dropped first)

### Tests

| # | Test Name | Operation | Topology | Description |
|---|-----------|-----------|----------|-------------|
| 3.1 | `test_suspend_middle_layer_blocks_downstream` | SUSPEND | A→B→C | Suspend B. Insert into A, refresh A. Verify C's data_timestamp doesn't advance. Resume B, refresh B+C, verify convergence. |
| 3.2 | `test_alter_schedule_mid_pipeline` | ALTER schedule | A→B→C | Alter B's schedule from 1m to 5s. Verify DAG_REBUILD_SIGNAL fires, scheduler picks up new config, chain converges. |
| 3.3 | `test_alter_mode_mid_pipeline_diff_to_full` | ALTER mode | A→B→C | Change B from DIFFERENTIAL to FULL. Verify next refresh of B does full recompute, C still sees correct data. |
| 3.4 | `test_alter_query_mid_pipeline` | ALTER query | A→B→C | Change B's defining query (add a WHERE filter). Verify reinit, C reflects the filtered data after refresh. |
| 3.5 | `test_drop_leaf_keeps_pipeline_intact` | DROP leaf | A→B→C | Drop C. Verify A and B are unaffected, can still be refreshed. |
| 3.6 | `test_drop_middle_layer_blocked_by_dependents` | DROP middle | A→B→C | Attempt to drop B while C depends on it. Verify error. Drop C first, then B succeeds. |
| 3.7 | `test_suspend_resume_cycle_data_consistency` | SUSPEND+RESUME | A→B | Suspend B, mutate A several times (no refresh on B), resume B, single refresh on B. Verify B has cumulative changes. |

### Implementation pattern

```rust
#[tokio::test]
async fn test_suspend_middle_layer_blocks_downstream() {
    let db = E2eDb::new().await.with_extension().await;
    // base → layer_a → layer_b → layer_c
    setup_3_layer_pipeline(&db).await;

    // Suspend the middle layer
    db.alter_st("layer_b", "status => 'SUSPENDED'").await;
    let (status, _, _, _) = db.pgt_status("layer_b").await;
    assert_eq!(status, "SUSPENDED");

    // Mutate base and refresh layer_a
    db.execute("INSERT INTO base_t VALUES (100, 'new')").await;
    db.refresh_st("layer_a").await;

    // Record layer_c's data_timestamp before attempting refresh
    let ts_before: String = db.query_scalar(
        "SELECT data_timestamp::text FROM pgtrickle.pgt_stream_tables \
         WHERE pgt_name = 'layer_c'"
    ).await;

    // Refresh layer_b should be skipped (SUSPENDED), layer_c should see no upstream change
    // (Manual refresh of a SUSPENDED ST should return an error or no-op)

    // Resume and verify convergence
    db.alter_st("layer_b", "status => 'ACTIVE'").await;
    db.refresh_st("layer_b").await;
    db.refresh_st("layer_c").await;

    db.assert_st_matches_query("layer_c", LAYER_C_QUERY).await;
}
```

---

## Test Group 4: Auto-Refresh Chain Propagation

**Priority:** P1  
**Target file:** `tests/e2e_dag_autorefresh_tests.rs` (new)  
**Rationale:** The scheduler processes STs in topological order within each
tick (`src/scheduler.rs`). For ST-on-ST dependencies it uses
`has_stream_table_source_changes()` (data_timestamp comparison). This path
has only been tested with 2-layer cascades
(`e2e_cascade_regression_tests.rs`). Deeper chains exercise the
scheduler's ordering guarantee more rigorously.

### Critical: Must use `E2eDb::new_on_postgres_db()`

The background worker only connects to the `postgres` database. All tests
in this group must use `new_on_postgres_db()` and
`configure_fast_scheduler()`.

### Tests

| # | Test Name | Layers | Description |
|---|-----------|:------:|-------------|
| 4.1 | `test_autorefresh_3_layer_cascade` | 3 | base → L1 → L2 → L3 all with `1s` schedule. Insert into base, wait for L3 to auto-refresh, verify correctness at all layers. |
| 4.2 | `test_autorefresh_diamond_cascade` | 3 | Diamond: base → L1a + L1b → L2, all `1s`. Insert into base, wait for L2. |
| 4.3 | `test_autorefresh_calculated_schedule` | 2 | L1 (schedule `1s`) → L2 (schedule `CALCULATED`). L2 should refresh whenever L1 has pending changes. |
| 4.4 | `test_autorefresh_no_spurious_3_layer` | 3 | Like `test_no_spurious_cascade_after_noop_upstream_refresh` but extended to 3 layers. No DML → all 3 `data_timestamp`s remain stable across 2+ scheduler ticks. |
| 4.5 | `test_autorefresh_staggered_schedules` | 3 | L1=1s, L2=3s, L3=1s. Verify L3 doesn't refresh until L2 has caught up. |

### Implementation pattern

```rust
#[tokio::test]
async fn test_autorefresh_3_layer_cascade() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    configure_fast_scheduler(&db).await;

    db.execute("CREATE TABLE auto3_src (id SERIAL PRIMARY KEY, val INT)")
        .await;
    db.execute("INSERT INTO auto3_src VALUES (1, 10), (2, 20)").await;

    db.create_st("auto3_l1", "SELECT id, val FROM auto3_src", "1s", "DIFFERENTIAL").await;
    db.execute(
        "SELECT pgtrickle.create_stream_table('auto3_l2', \
         $$SELECT id, val * 2 AS doubled FROM auto3_l1$$, '1s', 'DIFFERENTIAL')"
    ).await;
    db.execute(
        "SELECT pgtrickle.create_stream_table('auto3_l3', \
         $$SELECT id, doubled + 1 AS result FROM auto3_l2$$, '1s', 'DIFFERENTIAL')"
    ).await;

    // Wait for initial scheduler stabilization
    db.wait_for_auto_refresh("auto3_l3", Duration::from_secs(30)).await;

    // Mutate and wait for cascade
    db.execute("INSERT INTO auto3_src VALUES (3, 30)").await;

    // Wait for the deepest layer to pick up the change
    let refreshed = db.wait_for_auto_refresh("auto3_l3", Duration::from_secs(60)).await;
    assert!(refreshed, "auto3_l3 should auto-refresh after base mutation");

    // Verify correctness at all layers
    db.assert_st_matches_query("auto3_l1", "SELECT id, val FROM auto3_src").await;
    db.assert_st_matches_query("auto3_l2",
        "SELECT id, val * 2 AS doubled FROM auto3_src").await;
    db.assert_st_matches_query("auto3_l3",
        "SELECT id, val * 2 + 1 AS result FROM auto3_src").await;
}
```

### Timing considerations

Auto-refresh tests are inherently slower (wall-clock polling). Each test
should use generous timeouts (30–60s) and short schedules (1s). The
`configure_fast_scheduler()` helper sets
`pg_trickle.scheduler_interval_ms = 100` and
`pg_trickle.min_schedule_seconds = 1`.

---

## Test Group 5: Wide Topologies (Fan-Out & Fan-In)

**Priority:** P1  
**Target file:** `tests/e2e_dag_topology_tests.rs` (new)  
**Rationale:** Most tests use linear chains or simple diamonds. Real-world
schemas have wide fan-out (one source → many STs) and deep fan-in (many
sources converging through several layers).

### Tests

| # | Test Name | Topology | Description |
|---|-----------|----------|-------------|
| 5.1 | `test_fanout_4_leaves` | base → L1, L2, L3, L4 | One base table, 4 independent leaf STs. Insert into base, verify all 4 independently correct. |
| 5.2 | `test_fanout_then_converge` | base → L1a, L1b, L1c → L2 (JOIN all) | Wide fan-out at L1, fan-in at L2 (3-way join). |
| 5.3 | `test_deep_linear_5_layers` | base → L1 → L2 → L3 → L4 → L5 | 5-layer linear chain. Verify propagation to deepest layer. |
| 5.4 | `test_multi_source_diamond` | base_a, base_b → L1 (JOIN) → L2, L3 | Two base tables join at L1, L1 fans out to L2, L3. Mutate base_a, verify L2 and L3. Then mutate base_b, verify again. |
| 5.5 | `test_wide_fanout_deletion_isolation` | base → L1..L6 | Delete data relevant to only L3's query. Verify L1, L2, L4, L5, L6 are unaffected. |

### Implementation considerations

For the 5-layer chain (test 5.3), the interim STs must add some
transformation at each level so we're testing actual delta propagation
rather than just projection:

```
L1: SELECT id, val FROM base_t                  -- passthrough
L2: SELECT id, val * 2 AS v2 FROM l1            -- arithmetic
L3: SELECT id, SUM(v2) AS total FROM l2 GROUP BY id  -- aggregate
L4: SELECT id, total, RANK() OVER (ORDER BY total DESC) AS rnk FROM l3  -- window
L5: SELECT id, total FROM l4 WHERE rnk <= 10    -- filter (TopK)
```

This exercises scan → project → aggregate → window → filter operators
across the chain, covering the full DVM operator repertoire in one
pipeline.

---

## Test Group 6: Error Resilience in Pipelines

**Priority:** P2  
**Target file:** `tests/e2e_dag_error_tests.rs` (new)  
**Rationale:** If a middle layer's refresh fails (e.g., division by zero in
the defining query after a data change, or an out-of-memory), the layers
above and below should not be corrupted. The error should be recorded in
the catalog (`consecutive_errors`, `status`), and recovery after data
correction should work.

### Tests

| # | Test Name | Description |
|---|-----------|-------------|
| 6.1 | `test_error_in_middle_layer_does_not_corrupt_siblings` | A→B, A→C. Engineer B's query to fail on certain data (e.g., division by zero). Insert triggering data. Refresh A (succeeds), refresh B (fails), refresh C (succeeds). Verify C is correct, B has `consecutive_errors > 0`. |
| 6.2 | `test_error_recovery_after_data_fix` | A→B→C. B fails on bad data. Fix the data at the base. Refresh A, then B (succeeds now), then C. Verify full convergence. |
| 6.3 | `test_suspended_on_max_errors` | A→B. Set `max_consecutive_errors` GUC. Trigger repeated failures. Verify B transitions to SUSPENDED after N failures. |
| 6.4 | `test_error_in_leaf_does_not_affect_upstream` | A→B→C. C's refresh fails. Verify A and B are still correct and continue to refresh normally. |

### Implementation considerations

To engineer a deterministic failure, create a defining query with
division:

```sql
SELECT id, val / nullif(denom, 0) AS ratio FROM source_table
```

Insert a row with `denom = 0` to trigger a division-by-zero error. The
refresh should fail, and `try_execute` can be used to capture the error:

```rust
let result = db.try_execute(
    "SELECT pgtrickle.refresh_stream_table('faulty_st')"
).await;
assert!(result.is_err(), "refresh should fail on division by zero");
```

---

## Test Group 7: Concurrent DML During Pipeline Refresh

**Priority:** P2  
**Target file:** `tests/e2e_dag_concurrent_tests.rs` (new)  
**Rationale:** In production, DML continues while the scheduler refreshes
the pipeline. This tests that CDC triggers correctly capture changes that
arrive between refresh steps.

### Tests

| # | Test Name | Description |
|---|-----------|-------------|
| 7.1 | `test_dml_between_layer_refreshes` | A→B→C. Insert, refresh A, insert more, refresh B, refresh C. The second insert should appear in the change buffer for the **next** cycle, not corrupt the current one. |
| 7.2 | `test_concurrent_insert_during_pipeline_refresh` | Spawn a task that continuously inserts rows. In the main task, run 5 full pipeline refreshes. After both complete, do a final refresh. Verify convergence. |
| 7.3 | `test_rollback_between_refreshes` | A→B. Insert, refresh A. Start a transaction, insert more, ROLLBACK. Refresh B. Verify B reflects only the committed data. |

### Implementation pattern

```rust
#[tokio::test]
async fn test_dml_between_layer_refreshes() {
    let db = E2eDb::new().await.with_extension().await;
    setup_2_layer_pipeline(&db).await;

    // Insert and refresh L1 only
    db.execute("INSERT INTO base_t VALUES (10, 'first')").await;
    db.refresh_st("layer1").await;

    // More DML arrives between L1 and L2 refresh
    db.execute("INSERT INTO base_t VALUES (11, 'between')").await;

    // Refresh L2 — it should see L1's state (which doesn't include row 11)
    db.refresh_st("layer2").await;
    db.assert_st_matches_query("layer2", LAYER2_QUERY).await;

    // Now refresh L1 again to pick up row 11, then L2
    db.refresh_st("layer1").await;
    db.refresh_st("layer2").await;
    db.assert_st_matches_query("layer2", LAYER2_QUERY).await;
}
```

---

## Test Group 8: IMMEDIATE Mode Cascades

**Priority:** P2  
**Target file:** `tests/e2e_dag_immediate_tests.rs` (new)  
**Rationale:** `test_ivm_cascading_immediate_sts` (in `e2e_ivm_tests.rs`)
shows that a 2-layer IMMEDIATE cascade currently requires explicit refresh
for the second layer — the statement-level trigger only fires for the
directly affected ST. This behavior should be documented via tests, and
deeper IMMEDIATE cascades should be verified.

### Key architecture note

IMMEDIATE mode uses statement-level AFTER triggers (`src/ivm.rs`). When
the base table changes, the trigger refreshes ST₁. But ST₁'s internal
update is done via MERGE, which may or may not fire ST₂'s trigger
(depending on whether ST₂'s trigger is on the materialized table that
ST₁ writes to). Current behavior: it does NOT cascade automatically.

### Tests

| # | Test Name | Description |
|---|-----------|-------------|
| 8.1 | `test_immediate_2_layer_explicit_refresh` | Document current behavior: base → IMMED_A → IMMED_B. Insert into base, A is auto-refreshed, B is NOT (needs explicit refresh). |
| 8.2 | `test_immediate_with_differential_downstream` | base → IMMED_A → DIFF_B. Insert into base, A auto-refreshes. Manual refresh of B picks up A's changes. |
| 8.3 | `test_immediate_3_layer_propagation` | base → IMMED_A → IMMED_B → IMMED_C. Test how far the cascade propagates automatically vs requiring explicit refresh. |
| 8.4 | `test_immediate_rollback_no_side_effects` | base → IMMED_A. Insert in a transaction that is rolled back. Verify A is unchanged. |

---

## Implementation Roadmap

### Phase 1: Multi-Cycle DAG (P0) — ✅ COMPLETE

| Step | Task | Status |
|------|------|--------|
| 1.1 | Create `tests/e2e_multi_cycle_dag_tests.rs` with shared setup helpers | ✅ |
| 1.2 | Implement tests 1.1–1.4 (core multi-cycle cascade coverage) | ✅ |
| 1.3 | Implement tests 1.5–1.6 (stress + diamond multi-cycle) | ✅ |
| 1.4 | Run full test suite, fix any discovered issues | ✅ |

### Phase 2: Mixed Modes + Auto-Refresh (P1) — ✅ COMPLETE

| Step | Task | Status |
|------|------|--------|
| 2.1 | Create `tests/e2e_mixed_mode_dag_tests.rs`, implement tests 2.1–2.3 | ✅ |
| 2.2 | Implement tests 2.4–2.5 (alter + IMMEDIATE leaf) | ✅ |
| 2.3 | Create `tests/e2e_dag_autorefresh_tests.rs`, implement tests 4.1–4.2 | ✅ |
| 2.4 | Implement tests 4.3–4.5 (CALCULATED, no-spurious, staggered) | ✅ |
| 2.5 | Create `tests/e2e_dag_topology_tests.rs`, implement tests 5.1–5.5 | ✅ |

### Phase 3: Operations + Error + Concurrent (P2) — ✅ COMPLETE

| Step | Task | Status |
|------|------|--------|
| 3.1 | Create `tests/e2e_dag_operations_tests.rs`, implement tests 3.1–3.4 | ✅ |
| 3.2 | Implement tests 3.5–3.7 (DROP cascade, suspend-resume) | ✅ |
| 3.3 | Create `tests/e2e_dag_error_tests.rs`, implement tests 6.1–6.4 | ✅ |
| 3.4 | Create `tests/e2e_dag_concurrent_tests.rs`, implement tests 7.1–7.3 | ✅ |
| 3.5 | Create `tests/e2e_dag_immediate_tests.rs`, implement tests 8.1–8.4 | ✅ |

### Phase 4: CI Integration + Documentation — ✅ COMPLETE

| Step | Task | Status |
|------|------|--------|
| 4.1 | `justfile` glob `e2e_*` auto-picks up new files — no changes needed | ✅ |
| 4.2 | `cargo check --tests`, `cargo fmt`, `cargo clippy --tests` — all clean | ✅ |

**Total estimated effort:** ~15 hours (all complete)

---

## Infrastructure & Helpers

### Reusable setup helpers

Each test file should define module-local setup helpers for its pipeline
schemas. These keep tests concise and ensure consistent table structures.

```rust
/// Create a 3-layer linear pipeline: base → L1 (agg) → L2 (project) → L3 (filter)
async fn setup_3_layer_pipeline(db: &E2eDb) {
    db.execute("CREATE TABLE base_t (id SERIAL PRIMARY KEY, grp TEXT, val INT)").await;
    db.execute("INSERT INTO base_t (grp, val) VALUES ('a', 10), ('b', 20), ('c', 30)").await;

    db.create_st(
        "layer1", "SELECT grp, SUM(val) AS total FROM base_t GROUP BY grp",
        "1m", "DIFFERENTIAL",
    ).await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('layer2', \
         $$SELECT grp, total * 2 AS doubled FROM layer1$$, NULL, 'DIFFERENTIAL')"
    ).await;

    db.execute(
        "SELECT pgtrickle.create_stream_table('layer3', \
         $$SELECT grp, doubled FROM layer2 WHERE doubled > 30$$, NULL, 'DIFFERENTIAL')"
    ).await;
}
```

### Defining queries map

For `assert_st_matches_query`, we need the raw defining query for each ST
(evaluated against the base tables). The helper should return a map:

```rust
fn layer_queries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("layer1", "SELECT grp, SUM(val) AS total FROM base_t GROUP BY grp"),
        ("layer2", "SELECT grp, SUM(val) * 2 AS doubled FROM base_t GROUP BY grp"),
        ("layer3", "SELECT grp, SUM(val) * 2 AS doubled FROM base_t GROUP BY grp \
                     HAVING SUM(val) * 2 > 30"),
    ]
}
```

**Important:** The validation query for downstream STs must be written in
terms of the **base tables**, not the intermediate STs, so that
`assert_st_matches_query` compares the ST's materialized contents against
the ground truth.

### E2eDb helper API reference

| Method | Usage |
|--------|-------|
| `E2eDb::new()` | New container with isolated database (manual refresh tests) |
| `E2eDb::new_on_postgres_db()` | Uses `postgres` database (required for bgworker/auto-refresh tests) |
| `db.create_st(name, query, schedule, mode)` | Create + initialize ST |
| `db.refresh_st(name)` | Manual refresh |
| `db.drop_st(name)` | Drop ST |
| `db.alter_st(name, args)` | Alter ST (schedule, status, mode, query) |
| `db.assert_st_matches_query(st, query)` | DBSP correctness invariant (set equality via EXCEPT) |
| `db.pgt_status(name)` | Returns `(status, refresh_mode, is_populated, consecutive_errors)` |
| `db.count(table)` | Row count |
| `db.query_scalar::<T>(sql)` | Single scalar result |
| `db.try_execute(sql)` | Returns `Result<(), sqlx::Error>` for error tests |
| `db.wait_for_auto_refresh(name, timeout)` | Poll `data_timestamp` for bgworker tests |
| `db.table_exists(schema, table)` | Check table existence |
| `db.trigger_exists(trigger, table)` | Check trigger existence |

---

## File Organization

### New files

| File | Group | Tests | Priority |
|------|-------|------:|----------|
| `tests/e2e_multi_cycle_dag_tests.rs` | 1 | 6 | P0 |
| `tests/e2e_mixed_mode_dag_tests.rs` | 2 | 5 | P1 |
| `tests/e2e_dag_autorefresh_tests.rs` | 4 | 5 | P1 |
| `tests/e2e_dag_topology_tests.rs` | 5 | 5 | P1 |
| `tests/e2e_dag_operations_tests.rs` | 3 | 7 | P2 |
| `tests/e2e_dag_error_tests.rs` | 6 | 4 | P2 |
| `tests/e2e_dag_concurrent_tests.rs` | 7 | 3 | P2 |
| `tests/e2e_dag_immediate_tests.rs` | 8 | 4 | P2 |
| **Total** | | **39** | |

### Files NOT modified

The existing `e2e_pipeline_dag_tests.rs` (14 tests) remains as-is. It
covers single-mutation scenarios with realistic schemas (Nexmark,
E-commerce, IoT). The new test files focus on the **behavioral gaps**
(multi-cycle, mixed mode, operations, auto-refresh) rather than duplicating
schema coverage.

### Naming convention

All new test files follow the pattern `e2e_dag_<aspect>_tests.rs` to
clearly group them as DAG-focused tests. The `e2e_*` glob in the justfile
will automatically pick them up.

---

## Success Criteria

After implementing all 8 groups:

| Dimension | Before | After |
|-----------|:------:|:-----:|
| Multi-cycle on multi-layer | 0 tests | 6 tests |
| Mixed refresh modes | 0 tests | 5 tests |
| Operational mid-pipeline | 0 tests | 7 tests |
| Auto-refresh 3+ layers | 0 tests | 5 tests |
| Wide topologies | 0 tests | 5 tests |
| Error resilience | 0 tests | 4 tests |
| Concurrent DML | 0 tests | 3 tests |
| IMMEDIATE cascades | 1 test | 5 tests |
| **Total new DAG pipeline tests** | | **39** |

Combined with the existing 14 tests in `e2e_pipeline_dag_tests.rs`, 15 in
`e2e_diamond_tests.rs`, 5 in `e2e_multi_cycle_tests.rs`, and 8 in
`e2e_cascade_regression_tests.rs`, this brings total DAG-related E2E test
coverage to **81 tests**.
