# PLAN: Enable DIFFERENTIAL Refresh for ST-on-ST Dependencies

**Status:** Proposed  
**Priority:** Critical — blocking world-class performance for multi-layer DAGs  
**Estimated scope:** Small-to-medium (most infrastructure already exists)

---

## Problem

Stream tables that depend on other stream tables (ST-on-ST) are **always
refreshed with FULL mode**, even when configured as `REFRESH MODE
DIFFERENTIAL`. This contradicts the project's primary goal:

> *Differential refresh mode must be used wherever possible — full refresh is a
> fallback of last resort.*

Every ST-on-ST refresh does a complete `TRUNCATE + INSERT … SELECT *` of
the entire defining query output. For large intermediate STs, this can be
orders of magnitude slower than a true differential MERGE that only touches
changed rows.

### Where the force-FULL happens

The scheduler forces `RefreshAction::Full` whenever
`has_stream_table_source_changes` returns true:

```rust
// scheduler.rs — refresh_single_st (sequential path)
let action = if has_changes && has_stream_table_changes {
    RefreshAction::Full    // ← forces FULL even for DIFFERENTIAL STs
} else {
    refresh::determine_refresh_action(&st, has_changes)
};
```

The same pattern was recently replicated into all parallel worker paths
(`execute_worker_singleton`, `execute_worker_atomic_group`,
`execute_worker_immediate_closure`, `execute_worker_fused_chain`) as a
correctness fix for a bug where those paths lacked any ST-on-ST change
handling at all.

### Impact

| Topology | Affected STs | Behaviour today | Expected behaviour |
|----------|-------------|----------------|--------------------|
| Linear chain (ST → ST → ST) | All downstream STs | FULL on every cycle | DIFFERENTIAL via change buffers |
| Diamond (4 L1 STs → join ST) | Convergence ST | FULL on every cycle | DIFFERENTIAL via multi-source join delta |
| Fan-out + convergence | Convergence STs | FULL on every cycle | DIFFERENTIAL |
| Mixed (TABLE + ST sources) | Any ST with ≥1 ST source | FULL even if only TABLE changed | DIFFERENTIAL for TABLE changes, FULL only when needed |

---

## Key Finding: The Infrastructure Already Exists

An exhaustive code audit reveals that **nearly all the infrastructure for
ST-on-ST differential refresh is already implemented and wired up**. The
only thing preventing it from working is the scheduler's premature
force-FULL override.

### What is already implemented

| Component | Status | Location |
|-----------|--------|----------|
| ST change buffer tables (`changes_pgt_{id}`) | ✅ Created | `cdc::ensure_st_change_buffer()` — called during `create_stream_table` |
| Delta capture during DIFFERENTIAL refresh | ✅ Working | `refresh::capture_delta_to_st_buffer()` — writes from `__pgt_delta_{id}` → `changes_pgt_{id}` |
| Delta capture during FULL refresh | ✅ Working | `refresh::capture_full_refresh_diff_to_st_buffer()` — pre/post snapshot diff |
| DVM scan operator reads ST change buffers | ✅ Working | `dvm/operators/scan.rs:197-225` — selects `changes_pgt_{id}` vs `changes_{oid}` via `st_source_pgt_ids` map |
| DVM LSN placeholders for ST sources | ✅ Working | `diff.rs:245-280` — generates `__PGS_PREV_LSN_pgt_{id}__` tokens |
| LSN placeholder resolution | ✅ Working | `refresh.rs:1020-1050` — resolves `pgt_` prefixed placeholders from frontier |
| Frontier tracking for ST sources | ✅ Working | `version::Frontier::set_st_source()` / `get_st_lsn()` with `"pgt_{id}"` keys |
| Frontier stored by scheduler | ✅ Working | `execute_scheduled_refresh()` — `augment_frontier` injects ST source LSNs |
| ST change detection for short-circuit | ✅ Working | `refresh.rs:2556-2580` — `any_st_changes` check using frontier LSN range |
| ST buffer cleanup | ✅ Working | `refresh::cleanup_st_change_buffers_by_frontier()` — min-frontier based |
| ST buffer compaction | ✅ Working | `cdc::compact_st_change_buffer()` — DAG-5 |
| DAG-4 bypass tables for fused chains | ✅ Working | `scan.rs:209-211` reads from `pg_temp.__pgt_bypass_{id}` |
| Forced explicit DML for downstream capture | ✅ Working | `refresh.rs:3342` — `use_explicit_dml \|\| has_downstream_st_consumers()` |
| `old_*` column handling for ST buffers | ✅ Working | `scan.rs:290-300` — aliases `new_*` as `old_*` since ST buffers lack old values |

### What is blocking

| Blocker | Location | Fix |
|---------|----------|-----|
| Scheduler forces FULL for ST changes | `scheduler.rs` — 5 call sites | Remove force-FULL override, let `determine_refresh_action` decide |
| `resolve_delta_template` in `dvm/mod.rs` doesn't resolve `pgt_` placeholders | `dvm/mod.rs:124-140` | Add `pgt_` placeholder resolution (or remove this function — `refresh.rs` already handles it) |
| Manual refresh guard forces FULL for ST deps | `api.rs:execute_manual_differential_refresh` | Remove or gate the mixed-dependency guard |

---

## Implementation Plan

### Phase 1: Remove Force-FULL Overrides

**Scope:** `src/scheduler.rs`, `src/api.rs`  
**Risk:** Low — all downstream infrastructure is proven to work  
**Testing:** Existing E2E tests + new targeted tests

#### Step 1.1: Scheduler — sequential path

In `refresh_single_st()`, replace:

```rust
let action = if has_changes && has_stream_table_changes {
    RefreshAction::Full
} else { ... };
```

With:

```rust
let action = {
    let mut base_action = refresh::determine_refresh_action(&st, has_changes);
    // ... existing drift counter logic ...
    base_action
};
```

The `determine_refresh_action` function already returns `Differential` for
DIFFERENTIAL-mode STs. The DVM will handle ST sources via change buffers.

#### Step 1.2: Scheduler — parallel worker paths

Apply the same change to all four worker functions:
- `execute_worker_singleton()`
- `execute_worker_atomic_group()`
- `execute_worker_immediate_closure()`
- `execute_worker_fused_chain()`

Replace the force-FULL override with `determine_refresh_action`.

#### Step 1.3: Manual refresh guard

In `execute_manual_differential_refresh()`, remove or relax the
mixed-dependency guard that forces FULL when any upstream is STREAM_TABLE.
The guard was added when ST change buffers did not exist; they now do.

#### Step 1.4: Update `resolve_delta_template` in `dvm/mod.rs`

The cache-hit path in `generate_delta_query_cached()` calls
`resolve_delta_template` which only handles OID-based placeholders. Add
`pgt_` placeholder resolution matching what `resolve_lsn_placeholders`
(in refresh.rs) already does, so the DVM-level cache-hit path also works
for ST sources.

### Phase 2: Edge Case Handling

#### Step 2.1: Mixed TABLE + ST sources

When a single ST depends on both TABLE and STREAM_TABLE sources, the DVM
delta query already includes scan CTEs for both source types. The
`any_changes` check in `execute_differential_refresh` checks TABLE sources
first; the `any_st_changes` check runs when no TABLE changes exist.

However, when TABLE sources DO have changes, `any_st_changes` is
short-circuited (optimization). This is correct — the delta SQL template
always includes ALL source scans. If an ST source has no changes in its
LSN window, that scan CTE returns zero rows and contributes zero delta,
which is correct.

**Action:** Verify with a test that a mixed-source ST correctly applies
deltas from BOTH TABLE and ST changes in the same refresh cycle.

#### Step 2.2: First refresh after creation

When `prev_frontier.is_empty()`, `execute_scheduled_refresh` already falls
back to FULL. No change needed — the first refresh is always FULL to
establish the baseline.

#### Step 2.3: ST source without change buffer

If an upstream ST was created before the downstream (legacy scenario), no
change buffer may exist. `has_st_change_buffer()` returns false, and the
DVM's `scan.rs` falls through to the base-table path. This would produce
incorrect SQL.

**Action:** Add a validation check at differential-refresh time: if any
ST dependency lacks a change buffer, fall back to FULL with a warning.
This is the safe default for rare legacy/race conditions.

#### Step 2.4: Diamond consistency — concurrent ST source refreshes

In the parallel dispatch path, diamond intermediates (L1 STs) are
dispatched as separate workers. The convergence ST (join) waits for all
upstream units to complete before dispatch (via `remaining_upstreams`
counter). By the time the join ST refreshes, all L1 change buffers have
been populated. The join ST's `new_frontier` LSN (captured at refresh
time) will be ≥ the LSNs written by L1 captures.

**Action:** Add an assertion in `execute_scheduled_refresh` that for
DIFFERENTIAL refresh with ST sources, all upstream ST change buffers have
rows in the `[prev_lsn, new_lsn]` window. Log a warning if a source
appears to have zero rows (may indicate a timing issue).

### Phase 3: Testing

#### Step 3.1: Unit tests

Add unit tests in `src/scheduler.rs` and `src/dvm/mod.rs`:
- `resolve_delta_template` correctly resolves `pgt_` placeholders
- `determine_refresh_action` returns `Differential` for ST-on-ST deps
- `upstream_change_state` returns correct flags

#### Step 3.2: E2E tests — dedicated ST-on-ST differential

New test file or section in existing E2E tests:

```
test_st_to_st_differential_linear_chain
  — A → B → C, all DIFFERENTIAL. INSERT into A's source, verify B and C
    refresh via DIFFERENTIAL (check pgt_refresh_history.action = 'DIFFERENTIAL').

test_st_to_st_differential_diamond
  — dm_src → {A, B, C, D} → join, all DIFFERENTIAL. INSERT into dm_src,
    verify join refreshes via DIFFERENTIAL.

test_st_to_st_differential_mixed_sources
  — ST depends on both TABLE and another ST. Changes to the TABLE source
    produce DIFFERENTIAL refresh that includes both TABLE and ST deltas.

test_st_to_st_differential_data_correctness
  — For each topology above, verify that the ST contents after
    differential refresh match a fresh FULL refresh.

test_st_to_st_no_change_buffer_fallback
  — Create ST-on-ST dependency where upstream lacks change buffer.
    Verify graceful FULL fallback with warning.
```

#### Step 3.3: DAG benchmark validation

Re-run the DAG benchmark tests (`bench_latency_diamond_4_calc`,
`bench_latency_linear_*`, etc.) and verify:
- Per-hop latency drops significantly (DIFFERENTIAL vs FULL)
- No timeout / propagation failures
- Per-ST timing entries show `action = 'DIFFERENTIAL'`

#### Step 3.4: TPC-H validation

Run the TPC-H benchmark suite to verify correctness under realistic
workloads with multi-level ST dependencies.

### Phase 4: Cleanup

- Remove stale scheduler comments that say "STREAM_TABLE upstream sources
  have no CDC change buffer"
- Update `check_upstream_changes` comment to reflect buffer-based detection
- Update docs/ARCHITECTURE.md section on ST-on-ST refresh
- Update `PLAN_EDGE_CASES.md` to close the ST-on-ST performance gap item

---

## Risk Analysis

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Data incorrectness from LSN window misalignment | Low | Frontier already tracks ST sources; add assertion |
| Regression in TABLE-source differential | Very Low | No changes to TABLE-source path; existing tests cover |
| Performance regression from delta capture overhead | Low | Delta capture already runs for STs with downstream consumers; this just lets the downstream USE it differentially instead of discarding and doing FULL |
| ST change buffer missing (legacy/race) | Very Low | Explicit check + FULL fallback |
| Diamond timing in parallel mode | Low | EU DAG dependency edges enforce ordering |

---

## Performance Expectations

For a diamond topology (`dm_src → {A, B, C, D} → join`) with 10K rows in
the source and 100-row deltas:

| Metric | Current (FULL) | Expected (DIFFERENTIAL) | Improvement |
|--------|---------------|------------------------|-------------|
| Join ST refresh time | ~50-200ms (full recompute) | ~5-20ms (merge 100 delta rows) | 10-40× |
| Per-cycle latency | ~300-800ms | ~50-150ms | 5-10× |
| Change buffer I/O | None (wasted) | ~100 rows read per source | Net positive (avoids full scan) |

For deep linear chains (A → B → C → D → E, 5 levels):

| Metric | Current (FULL) | Expected (DIFFERENTIAL) | Improvement |
|--------|---------------|------------------------|-------------|
| Per-hop refresh | O(N) — full table | O(δ) — delta only | N/δ × |
| End-to-end latency | O(L × N) | O(L × δ) | N/δ × |

Where N = total rows, δ = delta rows, L = chain depth.

---

## Dependencies

- None — all infrastructure is implemented. This plan removes blockers
  rather than building new features.

## Related

- `plans/adrs/PLAN_ADRS.md` — ADR-001, ADR-002 (CDC architecture decisions)
- `plans/PLAN_EDGE_CASES.md` — EC-06 (keyless tables), EC-25 (DML guards)
- `plans/performance/PLAN_DAG_PERFORMANCE.md` — DAG latency formulas
- `/memories/repo/pg_trickle_st_on_st_scheduler_fix.md` — Recent force-FULL fix context
