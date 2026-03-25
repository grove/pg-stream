# Differential Refresh for Stream-Table-to-Stream-Table Dependencies

> **Status:** Proposed  
> **Date:** 2026-03-25  
> **Design Goal:** Maximum performance and differential propagation — FULL refreshes never cascade as FULL to downstream STs.  
> **Related:** [PLAN_DAG_PERFORMANCE.md](../performance/PLAN_DAG_PERFORMANCE.md) ·
> [ARCHITECTURE.md](../../docs/ARCHITECTURE.md) ·
> [CONFIGURATION.md](../../docs/CONFIGURATION.md)

---

## 1. Problem

When stream table B depends on stream table A, the scheduler is forced to use
`RefreshAction::Full` every time A's data changes. This is because there is no
CDC change buffer for stream tables — only for base tables. The scheduler
detects upstream staleness by comparing `data_timestamp` values, but has no
way to know *which* rows changed. A DIFFERENTIAL refresh would be a no-op
because the DVM engine expects to read changes from a `pgtrickle_changes`
buffer table, and none exists for ST sources.

**Why this matters:**

- A FULL refresh reads the **entire** upstream table and re-executes the
  downstream's defining query against it, regardless of how many rows changed.
- For a 10M-row upstream ST where 100 rows changed, a FULL refresh does
  ~100,000× more work than necessary.
- In ST-to-ST chains (A → B → C), every hop is FULL. Each ST in the chain
  re-reads the full output of its predecessor.
- Wide DAGs with ST dependencies at every level compound this waste across
  all levels.

---

## 2. Current Architecture

### How Base-Table CDC Works

When a base table is used as a source, pg_trickle creates:

1. **A change buffer table** in the `pgtrickle_changes` schema:
   ```sql
   CREATE TABLE pgtrickle_changes.changes_{source_oid} (
       change_id   BIGSERIAL,
       lsn         PG_LSN NOT NULL,
       action      CHAR(1) NOT NULL,    -- 'I' | 'U' | 'D' | 'T'
       pk_hash     BIGINT,
       changed_cols BIGINT,
       new_{col1}  TYPE, old_{col1} TYPE,
       ...
   );
   CREATE INDEX idx_changes_{oid}_lsn_pk_cid
       ON pgtrickle_changes.changes_{oid} (lsn, pk_hash, change_id)
       INCLUDE (action);
   ```
2. **AFTER triggers** on INSERT/UPDATE/DELETE that write rows into this
   buffer with the current LSN.

During a DIFFERENTIAL refresh, the DVM reads the buffer, computes a delta
(query-specific differentiation), and applies it via MERGE:

```
[source table] → [CDC trigger] → [changes_OID buffer]
                                        ↓
[DVM delta SQL] ←── reads LSN range ────┘
        ↓
[MERGE INTO stream_table]
```

### How ST-to-ST Works Today

There is no change buffer. The scheduler detects staleness via timestamp
comparison, and forces FULL:

```
[upstream ST A refreshed] → data_timestamp updated
                                   ↓
[scheduler checks B] → A.data_timestamp > B.data_timestamp?
                                   ↓ yes
[RefreshAction::Full for B] → re-reads ALL of A's rows
```

The relevant code path in the scheduler (`refresh_single_st()`):

```rust
let (has_changes, has_stream_table_changes) = upstream_change_state(&st, ...);

let action = if has_changes && has_stream_table_changes {
    RefreshAction::Full    // ← forced FULL, no alternative
} else {
    refresh::determine_refresh_action(&st, has_changes)  // can be Differential
};
```

---

## 3. Proposed Solution: Per-ST Change Buffers

### Core Idea

After a stream table's differential (or full) refresh produces a delta and
applies it via MERGE, **capture the delta into a change buffer table** that
downstream STs can consume. This makes ST sources look exactly like base-table
sources to the DVM pipeline — the same delta SQL, frontier tracking, and
cleanup logic all work unchanged.

```
[upstream ST A refreshed]
        ↓
[DVM produces delta rows: __pgt_row_id, __pgt_action, cols...]
        ↓
[MERGE INTO A's storage table]          ← existing
[INSERT delta INTO changes_pgt_{A.id}]  ← NEW: capture for downstream
        ↓
[downstream ST B: DIFFERENTIAL refresh]
        ↓
[DVM reads changes_pgt_{A.id}]          ← same DVM pipeline as base tables
[MERGE INTO B's storage table]
[INSERT delta INTO changes_pgt_{B.id}]  ← cascade continues
```

### Why This Approach

- **Reuses the entire existing DVM/MERGE pipeline.** No new query logic, no
  new delta-computation model. The change buffer schema is identical.
- **Frontier tracking works unchanged.** Each downstream ST already tracks
  per-source LSN frontiers. We assign LSNs to ST buffer rows and the existing
  min-frontier cleanup logic handles multi-consumer scenarios.
- **Opt-in per ST.** Not every ST needs a change buffer — only those with
  downstream ST dependents. This avoids storage waste.

---

## 4. Detailed Design

### 4.1 Change Buffer Table for STs

**When:** Created automatically when a stream table is first used as a source
by another stream table (i.e., when `pgt_dependencies` receives a
`source_type = 'STREAM_TABLE'` row). Also during `CREATE STREAM TABLE` if
the defining query references another ST that doesn't have a buffer yet.

**Naming convention:** `pgtrickle_changes.changes_pgt_{pgt_id}`

Using `pgt_id` (not the source OID) avoids collision with base-table buffers
(`changes_{oid}`), and the `_pgt_` infix makes the ownership unambiguous.

**Schema:**

```sql
CREATE TABLE pgtrickle_changes.changes_pgt_{pgt_id} (
    change_id   BIGSERIAL,
    lsn         PG_LSN NOT NULL,        -- assigned at capture time
    action      CHAR(1) NOT NULL,       -- 'I' | 'D'  (no 'U' — deltas are I/D pairs)
    pk_hash     BIGINT,                 -- __pgt_row_id from the delta
    new_{col1}  TYPE, ...               -- output columns of the ST
);

CREATE INDEX idx_changes_pgt_{pgt_id}_lsn_pk_cid
    ON pgtrickle_changes.changes_pgt_{pgt_id} (lsn, pk_hash, change_id)
    INCLUDE (action);
```

**Note:** No `old_*` columns are needed. The DVM delta already expresses
changes as `(row_id, 'D')` / `(row_id, 'I')` pairs. An UPDATE in the
original data appears as a delete of the old row + insert of the new row.
The downstream DVM scans read `new_*` columns for 'I' actions and ignores
payload columns for 'D' actions (it only needs `pk_hash` and `action`).

**Partitioning:** Follow the same auto-partitioning policy as base-table
buffers (`pg_trickle.buffer_partitioning` GUC).

### 4.2 Delta Capture During Refresh

After the DVM computes the delta and before/during MERGE application, capture
the delta rows into the ST's change buffer. The exact insertion point depends
on which MERGE execution path is used:

**Path 1 — Explicit DML (user-trigger path):**

The delta is already materialized into `__pgt_delta_{pgt_id}` (a temp table).
After MERGE, INSERT into the change buffer from this temp table:

```sql
INSERT INTO pgtrickle_changes.changes_pgt_{pgt_id}
    (lsn, action, pk_hash, new_col1, new_col2, ...)
SELECT pg_current_wal_lsn(), d.__pgt_action, d.__pgt_row_id,
       d.col1, d.col2, ...
FROM __pgt_delta_{pgt_id} d
WHERE d.__pgt_action IN ('I', 'D');
```

**Path 2 — Prepared statement / direct MERGE:**

The delta is not yet materialized in memory. Two options:

- **Option A:** Materialize the delta into a temp table first (same pattern
  as the explicit-DML path), then MERGE from it AND INSERT into the buffer.
  One extra `CREATE TEMP TABLE ... AS SELECT` before the MERGE.

- **Option B (preferred):** Use `MERGE ... RETURNING`, which is available
  since PostgreSQL 17 and therefore present in the pg_trickle target of
  PostgreSQL 18. The RETURNING clause can reference source-table alias
  columns directly, yielding exactly the delta rows needed for the buffer
  insert — no temp table required:

  ```sql
  WITH delta_rows AS (
      MERGE INTO "{schema}"."{name}" AS st
      USING ({delta_sql}) AS d
      ON st.__pgt_row_id = d.__pgt_row_id
      WHEN MATCHED AND d.__pgt_action = 'D' THEN DELETE
      WHEN MATCHED AND d.__pgt_action = 'I' ... THEN UPDATE SET ...
      WHEN NOT MATCHED AND d.__pgt_action = 'I' THEN INSERT ...
      RETURNING d.__pgt_action, d.__pgt_row_id, d.col1, d.col2, ...
  )
  INSERT INTO pgtrickle_changes.changes_pgt_{pgt_id}
      (lsn, action, pk_hash, new_col1, new_col2, ...)
  SELECT pg_current_wal_lsn(), r.__pgt_action, r.__pgt_row_id,
         r.col1, r.col2, ...
  FROM delta_rows r;
  ```

  This atomically applies the MERGE and captures the buffer rows in a
  single round-trip, with no intermediate materialisation cost.

**LSN assignment:** Use `pg_current_wal_lsn()` at capture time. This gives
each captured batch a monotonically increasing LSN that downstream STs can
use for frontier tracking, consistent with how base-table triggers assign
LSNs.

**Empty deltas:** When the MERGE produces zero changes (no-op refresh), skip
the buffer INSERT entirely. The downstream timestamp comparison will
correctly detect that nothing changed.

### 4.3 Frontier & Change Detection

**Downstream STs track the upstream ST's frontier** using the same JSONB
structure as for base tables:

```json
{
  "sources": {
    "pgt_{upstream_pgt_id}": { "lsn": "0/1A3B5C" }
  }
}
```

The key uses `pgt_{id}` instead of a raw OID to distinguish ST sources from
base-table sources in the frontier map.

**Change detection in the scheduler** (`has_stream_table_source_changes()`)
changes from timestamp comparison to buffer inspection:

```rust
// Before (current):
// Compare data_timestamp to detect staleness → force FULL

// After (proposed):
// Check if change buffer has rows beyond our frontier → allow DIFFERENTIAL
fn has_stream_table_source_changes(st: &StreamTableMeta) -> bool {
    let deps = StDependency::get_for_st(st.pgt_id).unwrap_or_default();
    for dep in &deps {
        if dep.source_type != "STREAM_TABLE" { continue; }

        let upstream_pgt_id = get_pgt_id_for_relid(dep.source_relid);
        let buffer_table = format!("changes_pgt_{}", upstream_pgt_id);

        // Check if buffer table exists and has rows beyond our frontier
        let prev_lsn = st.frontier_lsn_for_source(upstream_pgt_id);
        let has_rows = Spi::get_one::<bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM {schema}.{buffer_table} \
             WHERE lsn > '{prev_lsn}'::pg_lsn LIMIT 1)",
        )).unwrap_or(Some(false)).unwrap_or(false);

        if has_rows { return true; }
    }
    false
}
```

This is the same pattern as `has_table_source_changes()` — exactly aligned.

### 4.4 Scheduler Policy Change

In `refresh_single_st()`, the FULL override is currently triggered by:

```rust
if has_changes && has_stream_table_changes {
    RefreshAction::Full
}
```

With ST change buffers, this override is **removed** (or gated behind a
feature flag). When the upstream ST has a change buffer, the downstream ST
uses the normal `determine_refresh_action()` path, which respects the ST's
`refresh_mode` setting. If `refresh_mode = Differential`, it gets
DIFFERENTIAL.

**Fallback:** If an upstream ST does *not* have a change buffer (e.g., it has
no downstream ST dependents yet), the current FULL behavior is preserved.

### 4.5 Cleanup

The existing `drain_pending_cleanups()` logic already computes the minimum
frontier across all consumers of a given source. Extend it to include ST
change buffers:

1. In `drain_pending_cleanups()`, include `pgtrickle_changes.changes_pgt_{id}`
   tables alongside `changes_{oid}` tables.
2. The min-frontier query joins on `pgt_dependencies WHERE source_type =
   'STREAM_TABLE'` to find all downstream STs for a given upstream ST.
3. DELETE rows where `lsn <= min_frontier` (same as base-table cleanup).
4. If partitioning is enabled, reuse `detach_consumed_partitions()`.

### 4.6 Buffer Lifecycle

| Event | Action |
|-------|--------|
| ST B is created with ST A as source | If `changes_pgt_{A.id}` doesn't exist, create it |
| ST B is dropped and was the last consumer of A | Drop `changes_pgt_{A.id}` (no consumers left) |
| ST A is dropped | Drop `changes_pgt_{A.id}` |
| ST A's schema changes (reinit) | Recreate `changes_pgt_{A.id}` with new column types |
| `refresh_mode` changed to FULL | Buffer still populated (downstream might be DIFFERENTIAL) |

The buffer is owned by the **upstream** ST, not the downstream. This is
consistent with base-table buffers being owned by the source, not the
consumer.

---

## 5. DVM Integration

### 5.1 Scan Operator for ST Sources

The DVM's scan operator (`dvm/operators/scan.rs`) currently generates delta
SQL that reads from `pgtrickle_changes.changes_{oid}`. For ST sources, it
needs to read from `pgtrickle_changes.changes_pgt_{pgt_id}` instead.

The scan CTE structure is identical:

```sql
WITH scan_upstream AS (
    -- INSERT / new rows
    SELECT c.pk_hash AS __pgt_row_id,
           'I'::TEXT AS __pgt_action,
           c.new_col1, c.new_col2, ...
    FROM pgtrickle_changes.changes_pgt_{upstream_pgt_id} c
    WHERE c.lsn > '__PGS_PREV_LSN_pgt_{id}__'::pg_lsn
      AND c.lsn <= '__PGS_NEW_LSN_pgt_{id}__'::pg_lsn
      AND c.action = 'I'

    UNION ALL

    -- DELETE rows
    SELECT c.pk_hash AS __pgt_row_id,
           'D'::TEXT AS __pgt_action,
           c.new_col1, c.new_col2, ...
    FROM pgtrickle_changes.changes_pgt_{upstream_pgt_id} c
    WHERE c.lsn > '__PGS_PREV_LSN_pgt_{id}__'::pg_lsn
      AND c.lsn <= '__PGS_NEW_LSN_pgt_{id}__'::pg_lsn
      AND c.action = 'D'
)
```

**Key difference from base-table scans:**
- Buffer name: `changes_pgt_{pgt_id}` not `changes_{oid}`
- LSN placeholder tokens: `__PGS_PREV_LSN_pgt_{id}__` (new token format)
- No `'U'` action: ST deltas only produce `'I'` and `'D'`
- No `old_*` columns: DELETE rows carry `new_*` columns (the values being
  removed) because the DVM delta format uses `(__pgt_row_id, 'D', values)`
  not `(__pgt_row_id, 'D', NULL, NULL, ...)`

### 5.2 Frontier Placeholder Resolution

The LSN placeholder resolver (in `refresh.rs`, around line 2400) needs to
handle the new `pgt_` prefix:

```rust
// Existing: replace __PGS_PREV_LSN_{oid}__ with actual LSN string
// New: also replace __PGS_PREV_LSN_pgt_{id}__ for ST sources
```

Alternatively, since `pgt_id` is always an integer, the resolver can match
on any `__PGS_PREV_LSN_\w+__` pattern and look up the corresponding
frontier entry.

---

## 6. Implementation Plan

### Phase 1: Change Buffer Infrastructure (Foundation)

| Task | File | Description |
|------|------|-------------|
| 1.1 | `cdc.rs` | Add `create_st_change_buffer_table(pgt_id, columns)` — creates `changes_pgt_{id}` with schema from the ST's output columns |
| 1.2 | `cdc.rs` | Add `drop_st_change_buffer_table(pgt_id)` |
| 1.3 | `cdc.rs` | Add `has_st_change_buffer(pgt_id) -> bool` — checks table existence |
| 1.4 | `api.rs` | In `create_stream_table_impl()`, after dependency insertion, check if any source has `source_type = 'STREAM_TABLE'` and create the upstream ST's buffer if it doesn't exist |
| 1.5 | `api.rs` | In `drop_stream_table_impl()`, check if dropped ST is the last consumer; if so, drop the upstream's buffer |
| 1.6 | `catalog.rs` | Add `count_downstream_st_consumers(pgt_id) -> i64` |

### Phase 2: Delta Capture

| Task | File | Description |
|------|------|-------------|
| 2.1 | `refresh.rs` | Add `capture_delta_to_st_buffer(pgt_id, delta_temp_table, columns)` — INSERTs delta rows into `changes_pgt_{id}` with `pg_current_wal_lsn()` (used by the explicit-DML path which already has a temp table) |
| 2.2 | `refresh.rs` | In `execute_differential_refresh()`, for the prepared-statement and direct-MERGE paths: wrap the MERGE in a CTE using `MERGE ... RETURNING` to atomically capture delta rows into the buffer (PostgreSQL 17+ feature, available on the pg_trickle target of PG 18) |
| 2.3 | `refresh.rs` | In the explicit-DML path, call `capture_delta_to_st_buffer()` after the DELETE/UPDATE/INSERT sequence, reading from the already-materialized `__pgt_delta_{pgt_id}` temp table |
| 2.4 | `refresh.rs` | In `execute_full_refresh()`, after the full rewrite, compute and capture the full diff (new state minus old state) into the buffer — see Section 7 |

### Phase 3: DVM Scan for ST Sources

| Task | File | Description |
|------|------|-------------|
| 3.1 | `dvm/operators/scan.rs` | Extend scan operator: when source is a STREAM_TABLE, read from `changes_pgt_{id}` instead of `changes_{oid}`, using `pgt_`-prefixed LSN placeholders |
| 3.2 | `dvm/mod.rs` | Pass `source_type` into delta generation so the scan operator knows which buffer to reference |
| 3.3 | `refresh.rs` | Extend LSN placeholder resolver to handle `__PGS_PREV_LSN_pgt_{id}__` tokens |
| 3.4 | `refresh.rs` | Include ST source buffers in frontier computation |

### Phase 4: Scheduler Integration

| Task | File | Description |
|------|------|-------------|
| 4.1 | `scheduler.rs` | Modify `has_stream_table_source_changes()`: check buffer for rows beyond frontier instead of comparing `data_timestamp` |
| 4.2 | `scheduler.rs` | In `refresh_single_st()`, remove the `has_stream_table_changes → Full` override when the upstream has a change buffer |
| 4.3 | `scheduler.rs` | Add fallback: if upstream has no buffer, retain the existing FULL + timestamp behavior |

### Phase 5: Cleanup & Lifecycle

| Task | File | Description |
|------|------|-------------|
| 5.1 | `refresh.rs` | Extend `drain_pending_cleanups()` to include `changes_pgt_{id}` tables, using the same min-frontier logic |
| 5.2 | `refresh.rs` | Extend `cleanup_change_buffers_by_frontier()` for ST sources |
| 5.3 | `hooks.rs` | Handle ST DDL changes: if upstream ST's schema changes, drop + recreate the buffer with new column types |
| 5.4 | `api.rs` | Handle `ALTER STREAM TABLE ... SET QUERY`: recreate buffer if columns change |

### Phase 6: Testing

| Test | Tier | Description |
|------|------|-------------|
| 6.1 | Unit | `create_st_change_buffer_table` / `drop_st_change_buffer_table` |
| 6.2 | Unit | DVM scan operator generates correct SQL for ST sources |
| 6.3 | Integration | ST A → ST B chain: verify B uses DIFFERENTIAL when A has a buffer |
| 6.4 | Integration | Mixed sources: ST B depends on table T and ST A; verify correct action selection per-source |
| 6.5 | E2E | 3-level chain (table → ST₁ → ST₂ → ST₃): INSERT into base table, verify all levels update differentially |
| 6.6 | E2E | DROP ST₁ (middle of chain): verify ST₂ falls back to FULL |
| 6.7 | E2E | Buffer cleanup: verify consumed rows are drained at min frontier |
| 6.8 | E2E | Schema change propagation: ALTER source column type, verify buffer is recreated |

---

## 7. Task 2.4 Deep Dive: FULL Refresh Delta Capture

FULL refresh is the trickiest case. When an ST does a FULL refresh (truncate +
rewrite), we still need to capture the **diff** between the old and new state
so downstream STs can consume it differentially. Two approaches:

### Chosen Approach: Pre/Post Diff

**Decision:** Option A is implemented from day one. The design goal is that
FULL refreshes on an upstream ST never force downstream STs to also do FULL.
The ~2× FULL refresh cost is an accepted and bounded tradeoff: it is O(N)
with the upstream table size, exactly the same complexity class as the FULL
refresh itself, and it eliminates unbounded FULL cascading through deep chains.

Before the FULL refresh, snapshot the current `__pgt_row_id` set. After the
refresh, compare:

```sql
-- Capture pre-state
CREATE TEMP TABLE __pgt_pre_{pgt_id} ON COMMIT DROP AS
SELECT __pgt_row_id, col1, col2, ... FROM "{schema}"."{name}";

-- ... FULL refresh runs (TRUNCATE + re-INSERT) ...

-- Compute diff
-- Deleted rows: in pre but not in post
INSERT INTO pgtrickle_changes.changes_pgt_{pgt_id} (lsn, action, pk_hash, ...)
SELECT pg_current_wal_lsn(), 'D', pre.__pgt_row_id, pre.col1, ...
FROM __pgt_pre_{pgt_id} pre
LEFT JOIN "{schema}"."{name}" post ON pre.__pgt_row_id = post.__pgt_row_id
WHERE post.__pgt_row_id IS NULL;

-- Inserted rows: in post but not in pre
INSERT INTO pgtrickle_changes.changes_pgt_{pgt_id} (lsn, action, pk_hash, ...)
SELECT pg_current_wal_lsn(), 'I', post.__pgt_row_id, post.col1, ...
FROM "{schema}"."{name}" post
LEFT JOIN __pgt_pre_{pgt_id} pre ON post.__pgt_row_id = pre.__pgt_row_id
WHERE pre.__pgt_row_id IS NULL;

-- Changed rows: same row_id but different content
INSERT INTO pgtrickle_changes.changes_pgt_{pgt_id} (lsn, action, pk_hash, ...)
SELECT pg_current_wal_lsn(), 'D', pre.__pgt_row_id, pre.col1, ...
FROM __pgt_pre_{pgt_id} pre
JOIN "{schema}"."{name}" post ON pre.__pgt_row_id = post.__pgt_row_id
WHERE (pre.col1, pre.col2, ...) IS DISTINCT FROM (post.col1, post.col2, ...);

INSERT INTO pgtrickle_changes.changes_pgt_{pgt_id} (lsn, action, pk_hash, ...)
SELECT pg_current_wal_lsn(), 'I', post.__pgt_row_id, post.col1, ...
FROM "{schema}"."{name}" post
JOIN __pgt_pre_{pgt_id} pre ON post.__pgt_row_id = pre.__pgt_row_id
WHERE (pre.col1, pre.col2, ...) IS DISTINCT FROM (post.col1, post.col2, ...);
```

**Cost:** Two table scans (pre-state + diff). For large STs this is
meaningful overhead, but it's O(N) with the table size — the same as the
FULL refresh itself. The constant factor roughly doubles the refresh time.
This cost is accepted: a FULL refresh is already expensive, and protecting
all downstream STs from a cascading FULL is worth the constant overhead.

---

## 8. Performance Impact

### Storage Overhead

Each ST with downstream dependents gets a change buffer table. Buffer size
scales with **delta size per refresh**, not total ST size:

| Delta size per tick | Buffer accumulation (1 consumer) | With 5 consumers |
|--------------------|----------------------------------|-------------------|
| 100 rows | 100 rows (cleaned next tick) | 500 rows worst case |
| 10,000 rows | 10,000 rows | 50,000 rows worst case |

In practice, cleanup runs at the start of each downstream refresh, so buffer
tables stay small (one tick's worth of deltas per consumer lag).

### Write Overhead

Each ST refresh that has a change buffer adds one INSERT per delta row. For a
100-row delta this is ~1 ms. For a 10,000-row delta, ~10–50 ms. This is
constant overhead per refresh, amortized across all downstream consumers.

### Downstream Refresh Speedup

| Upstream ST size | Delta size | FULL refresh | DIFFERENTIAL refresh | Speedup |
|-----------------|------------|--------------|---------------------|---------|
| 100K rows | 100 rows | ~200 ms | ~5 ms | **40×** |
| 1M rows | 1,000 rows | ~2 s | ~20 ms | **100×** |
| 10M rows | 100 rows | ~20 s | ~5 ms | **4,000×** |

### Net Latency Impact (DAG Propagation)

Using the model from PLAN_DAG_PERFORMANCE.md for a wide DAG (N=500, D=10)
in CALCULATED mode with $T_r^{\text{full}} = 100\text{ms}$ and
$T_r^{\text{diff}} = 5\text{ms}$:

| Mode | Before (all FULL) | After (all DIFFERENTIAL) |
|------|-------------------|--------------------------|
| CALCULATED | $1\text{s} + 500 \times 100\text{ms} = 51\text{s}$ | $1\text{s} + 500 \times 5\text{ms} = 3.5\text{s}$ |
| Parallel C=16 | ~8 s | ~8 s (dominated by poll overhead) |

The sequential CALCULATED mode benefits enormously: **51s → 3.5s** for the
same DAG. Parallel mode doesn't benefit as much because the 200 ms poll
interval already dominates at low $T_r$.

---

## 9. Migration & Compatibility

- **Always-on.** ST change buffers are created unconditionally whenever a
  stream table is used as a source by another stream table. There is no
  opt-out GUC — the feature is always active. Deployments that want FULL
  propagation can set `refresh_mode = Full` on downstream STs explicitly.
- **Upgrade path:** On extension upgrade, automatically scan `pgt_dependencies`
  for all existing `source_type = 'STREAM_TABLE'` rows and create the missing
  `changes_pgt_{id}` buffers. Existing ST-to-ST dependencies immediately gain
  differential propagation without any user action. The upgrade step runs
  inside the extension update transaction and is idempotent (uses
  `CREATE TABLE IF NOT EXISTS`).

---

## 10. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Buffer tables increase catalog bloat | Only created for STs with downstream dependents; dropped when last consumer is removed |
| Large deltas cause buffer table bloat | Cleanup runs at min-frontier; buffer_partitioning GUC enables partition-based cleanup |
| `pg_current_wal_lsn()` not advancing on read-only workloads | **Dismissed.** The INSERT into `changes_pgt_{id}` is itself a WAL write, which always advances `pg_current_wal_lsn()`. A valid LSN is always available at capture time. |
| Pre/post diff doubles the FULL refresh time | **Accepted tradeoff.** The cost is O(N) — same complexity class as the FULL refresh itself. It eliminates unbounded FULL cascading through downstream chains. |
| No opt-out for ST change buffers | By design: always-on maximises differential propagation. Downstream STs can still be set to `refresh_mode = Full` individually if needed. |
| DVM scan template cache invalidation | Existing `get_delta_sql_template()` cache must be invalidated when a source switches from TABLE to STREAM_TABLE source type (this shouldn't happen in practice) |
| Schema changes on upstream ST | Handled by dropping/recreating the buffer; downstream STs already handle source schema changes via `needs_reinit` |

---

## 11. Alternatives Considered

### Alternative 1: Embed delta in `frontier` JSONB

Serialize delta rows into the frontier column instead of a separate table.
Avoids new tables but is impractical for deltas > ~100 rows due to JSONB
size and serialization overhead. Rejected.

### Alternative 2: Use AFTER triggers on ST storage tables

Instead of capturing the delta in `refresh.rs`, install CDC-style triggers on
the stream table's storage table to capture changes into a buffer. This would
mirror the base-table approach exactly.

**Problem:** pg_trickle already suppresses user triggers during MERGE
(`ALTER TABLE ... DISABLE TRIGGER USER`), and the internal CDC triggers would
need careful handling to not fire during manual DML vs. scheduled refresh.
The complexity is higher than the proposed approach with no benefit — we
already have the delta in memory during the refresh. Rejected.

### Alternative 3: WAL-based ST CDC

Use logical decoding on the ST's storage table to capture changes. This would
be fully transparent and handle manual DML too. However, it requires
replication slots, publications, and the full WAL-decoding machinery — massive
implementation effort for marginal benefit over the proposed approach.
Deferred to a possible future enhancement.
