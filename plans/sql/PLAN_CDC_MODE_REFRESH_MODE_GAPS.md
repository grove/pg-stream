# Plan: CDC Mode / Refresh Mode Interaction Gaps

Date: 2026-03-07
Status: IN PROGRESS
Last Updated: 2026-03-08

---

## 1. Problem Statement

pg_trickle has four refresh modes (`AUTO`, `FULL`, `DIFFERENTIAL`, `IMMEDIATE`)
and four CDC modes (`auto`, `trigger`, `wal`, plus the internal `transitioning`
state). Not all combinations are explicitly validated, and several edge cases
can produce surprising behavior: incorrect results, resource leaks, or opaque
errors.

This plan addresses six specific gaps, ordered by user impact.

### Gap Summary

| # | Gap | Severity | Effort |
|---|-----|----------|--------|
| G1 | No per-table `cdc_mode` override | High | Medium |
| G2 | `IMMEDIATE` + `wal` GUC — no validation | Medium | Small |
| G3 | `FULL` refresh does not advance WAL slot | Medium | Small |
| G4 | `AUTO→FULL` adaptive fallback does not flush change buffers | Medium | Medium |
| G5 | `TRANSITIONING` state lacks user-facing query ergonomics | Low | Small |
| G6 | `DIFFERENTIAL` without initialization baseline | Medium | Small |

---

## 2. Current State

### Refresh Modes

| Mode | Mechanism | CDC Path |
|------|-----------|----------|
| `AUTO` | Attempts `DIFFERENTIAL`; falls back to `FULL` when query is non-differentiable or change ratio > threshold | Standard CDC (trigger/WAL) |
| `FULL` | Truncate + reload | Standard CDC |
| `DIFFERENTIAL` | Delta query over change buffers | Standard CDC |
| `IMMEDIATE` | Statement-level IVM triggers (in-transaction) | IVM triggers, **bypasses CDC entirely** |

### CDC Modes

| Mode | Set By | Scope |
|------|--------|-------|
| `auto` (default GUC) | `pg_trickle.cdc_mode` | Cluster-wide |
| `trigger` | GUC | Cluster-wide |
| `wal` | GUC | Cluster-wide |
| `transitioning` | Internal state machine | Per-source in `pgt_dependencies.cdc_mode` |

### Key Code Locations

| Component | File | Entry Point |
|-----------|------|-------------|
| CDC mode GUC | `src/config.rs:88–106` | `PGS_CDC_MODE` |
| CDC setup | `src/api.rs:2319` | `setup_cdc_for_source()` |
| IVM vs CDC branch | `src/api.rs:452–470` | `setup_trigger_infrastructure()` |
| Full refresh | `src/refresh.rs:1011` | `execute_full_refresh()` |
| Differential refresh | `src/refresh.rs:1260` | `execute_differential_refresh()` |
| Adaptive fallback | `src/refresh.rs:1476–1554` | Change ratio check |
| Scheduler dispatch | `src/scheduler.rs:1240–1395` | `RefreshAction` match |
| Buffer cleanup | `src/refresh.rs:128` | `drain_pending_cleanups()` |
| dbt materialization | `dbt-pgtrickle/macros/materializations/stream_table.sql` | N/A |

---

## 3. Gap Details & Implementation Plans

---

### G1: Per-Table `cdc_mode` Override

**Problem.** `cdc_mode` is a cluster-wide GUC (`pg_trickle.cdc_mode`). In
mixed environments — some tables have a PK (WAL-capable), others don't — users
must choose the lowest common denominator (`trigger`) globally or manually
`ALTER TABLE ... REPLICA IDENTITY FULL` on every keyless table. There is no way
to say "use WAL for this table and triggers for that one."

**Current code path.** `setup_cdc_for_source()` (`src/api.rs:2341`) reads
`config::pg_trickle_cdc_mode()` — the global GUC — for every source. The
catalog (`pgt_dependencies.cdc_mode`) already stores a per-source CDC mode, but
only as a *result* of the state machine (never as user input).

**dbt impact.** The `stream_table` materialization does not expose `cdc_mode`
as a model config key. Users of `dbt-pgtrickle` have no way to set this per
model.

#### Design

Add an optional `cdc_mode` parameter to `create_stream_table()` and
`alter_stream_table()`. When set, it overrides the global GUC for all source
tables of that stream table. When `NULL` (default), the global GUC applies
(preserving backward compatibility).

#### Implementation Steps

1. **SQL API — `create_stream_table()`** (`src/api.rs:33–41`)
   - Add parameter: `cdc_mode: default!(Option<&str>, "NULL")`
   - Pass through to `create_stream_table_impl()`.

2. **SQL API — `alter_stream_table()`** (`src/api.rs`)
   - Add parameter: `cdc_mode: default!(Option<&str>, "NULL")`
   - When non-NULL on ALTER, re-evaluate CDC setup for all sources:
     - `trigger→wal`: validate PK/REPLICA IDENTITY, create slot, begin
       transition.
     - `wal→trigger`: drop slot, recreate trigger.
     - Same mode: no-op.

3. **Catalog — `pgt_stream_tables`** (`sql/` upgrade migration)
   - Add column: `requested_cdc_mode TEXT DEFAULT NULL CHECK (requested_cdc_mode IN ('trigger', 'wal', 'auto', NULL))`
   - This stores the **user's intent**; `pgt_dependencies.cdc_mode` continues
     to store the **effective state** per source.

4. **CDC setup** (`src/api.rs:2319–2400`)
   - Modify `setup_cdc_for_source()` to accept an optional `cdc_mode_override`
     parameter. When `Some(...)`, use it instead of reading the GUC.
   - Update the PK/REPLICA IDENTITY validation to use the effective mode.

5. **dbt adapter** (`dbt-pgtrickle/macros/materializations/stream_table.sql`)
   - Add config key `cdc_mode` (default `none`).
   - Thread it into `pgtrickle_create_stream_table()` and
     `pgtrickle_alter_stream_table()` macro calls.

6. **Upgrade migration** (`sql/pg_trickle--<prev>--<next>.sql`)
   - `ALTER TABLE pgtrickle.pgt_stream_tables ADD COLUMN requested_cdc_mode TEXT DEFAULT NULL ...`
   - Update `create_stream_table` and `alter_stream_table` function signatures.

7. **Documentation**
   - `docs/SQL_REFERENCE.md`: Document new parameter.
   - `docs/CONFIGURATION.md`: Clarify GUC vs per-table precedence.

8. **Tests**
   - Unit test: `cdc_mode_override` parameter parsing.
   - Integration test: create ST with `cdc_mode => 'trigger'` while global GUC
     is `auto`; verify trigger is used, no slot created.
   - Integration test: create ST with `cdc_mode => 'wal'` on a keyless table
     without REPLICA IDENTITY FULL; verify error.
   - E2E test: alter existing ST's `cdc_mode` from `trigger` to `wal`; verify
     transition completes.
   - dbt integration test: model config `cdc_mode: 'trigger'`.

---

### G2: Explicit Validation of `IMMEDIATE` + WAL CDC

**Problem.** If a user sets `pg_trickle.cdc_mode = 'wal'` and creates a stream
table with `refresh_mode = 'IMMEDIATE'`, the system silently bypasses WAL
entirely (the `is_immediate()` branch in `setup_trigger_infrastructure()`
skips CDC setup). This is correct behavior, but confusing: the user asked for
WAL and got IVM triggers with no feedback.

With G1 (per-table `cdc_mode`), the risk increases: a user could explicitly
write `cdc_mode => 'wal', refresh_mode => 'IMMEDIATE'`, which is an
incoherent configuration.

**Current code path.** `setup_trigger_infrastructure()` (`src/api.rs:452`)
has an `if refresh_mode.is_immediate() { ... } else { ... }` branch. The
`else` branch calls `setup_cdc_for_source()`. The `if` branch calls
`ivm::setup_ivm_triggers()`. No validation rejects the combination.

**Progress (2026-03-08).** Phase 1 is implemented: when the cluster-wide
`pg_trickle.cdc_mode` GUC is `'wal'` and a stream table is created or altered
to `refresh_mode = 'IMMEDIATE'`, pg_trickle now emits an INFO explaining that
WAL CDC is ignored and that statement-level IVM triggers will be used instead.
The explicit rejection path is still pending because there is not yet a
per-table `cdc_mode` override surface; that arrives with G1.

#### Implementation Steps

1. **Phase 1 — INFO for implicit global-GUC mismatch** (`src/api.rs`)
   - If `refresh_mode = 'IMMEDIATE'` and the effective `cdc_mode` comes from
     the global GUC with value `'wal'`, emit:
     ```text
     INFO: cdc_mode 'wal' has no effect for IMMEDIATE refresh mode — using IVM triggers
     ```
   - Implement in both `create_stream_table_impl()` and
     `alter_stream_table_impl()`.

2. **Phase 2 — Explicit rejection once G1 exists** (`src/api.rs`)
   - After parsing `refresh_mode` and determining an explicit per-table
     `cdc_mode` override:
     ```rust
     if refresh_mode.is_immediate() && effective_cdc_mode == "wal" {
         return Err(PgTrickleError::InvalidArgument(
             "refresh_mode = 'IMMEDIATE' is incompatible with cdc_mode = 'wal'. \
              IMMEDIATE uses in-transaction IVM triggers; WAL-based CDC is async. \
              Use cdc_mode = 'trigger' or 'auto', or choose a different refresh_mode."
                 .to_string(),
         ));
     }
     ```
   - Same check in `alter_stream_table_impl()` when altering refresh mode or
     cdc mode.

3. **Tests**
   - Phase 1 E2E test: GUC `wal` + `IMMEDIATE` create path succeeds and does
     not install CDC triggers or WAL slots.
   - Phase 1 E2E test: GUC `wal` + `ALTER ... refresh_mode='IMMEDIATE'`
     succeeds, leaves no WAL slots behind, and preserves synchronous
     IMMEDIATE propagation on subsequent DML.
   - Integration test: explicit `cdc_mode => 'wal'` + `IMMEDIATE` → error.
   - Integration test: GUC `wal` + `IMMEDIATE` (no per-table override) → 
     success with INFO log.

---

### G3: FULL Refresh Does Not Advance WAL Slot

**Problem.** When `execute_full_refresh()` runs, it truncates the stream table
and reloads from the defining query. The result is correct and a new frontier
is stored (scheduler handles this at `src/scheduler.rs:1261–1273`). However,
the change buffer rows consumed during prior differential cycles — and any new
rows that accumulated during the full refresh — remain in the WAL slot's
unacknowledged window.

For trigger-based CDC this is benign: buffer rows are pruned by frontier-based
cleanup. For WAL-based CDC, the logical replication slot's `confirmed_flush_lsn`
is only advanced by the WAL decoder polling loop, **not** by the refresh
executor. If the scheduler happens to do repeated FULL refreshes (e.g., the
table is in `AUTO` mode with a high change ratio), the slot may retain WAL
segments that are never needed.

This causes:
- WAL segment bloat (`pg_wal/` grows).
- `pg_replication_slots.active_pid` shows stale lag.
- Monitoring false alarms on replication lag.

#### Implementation Steps

1. **Add helper: `advance_slot_to_current()`** (`src/wal_decoder.rs`)
   - New function:
     ```rust
     pub fn advance_slot_to_current(slot_name: &str) -> Result<(), PgTrickleError> {
         Spi::run_with_args(
             "SELECT pg_replication_slot_advance($1, pg_current_wal_lsn())",
             &[slot_name.into()],
         ).map_err(|e| PgTrickleError::SpiError(e.to_string()))
     }
     ```

2. **Call after FULL refresh in scheduler** (`src/scheduler.rs:1261–1275`)
   - After `execute_full_refresh()` succeeds and the new frontier is stored,
     advance all WAL-mode slots for this ST's sources:
     ```rust
     // Advance WAL slots past the current LSN since full refresh
     // made all prior change-buffer data irrelevant.
     for dep in deps.iter().filter(|d| d.cdc_mode == CdcMode::Wal) {
         if let Some(ref slot) = dep.slot_name {
             if let Err(e) = wal_decoder::advance_slot_to_current(slot) {
                 log!("pg_trickle: failed to advance slot {}: {}", slot, e);
             }
         }
     }
     ```

3. **Also flush change buffer tables after FULL** (`src/refresh.rs:1011`)
   - At the end of `execute_full_refresh()`, truncate change buffers for the
     ST's sources so the next differential cycle doesn't reprocess stale rows:
     ```rust
     for oid in &source_oids {
         let _ = Spi::run(&format!(
             "TRUNCATE {change_schema}.changes_{}", oid.to_u32()
         ));
     }
     ```
   - This is safe because the FULL refresh already materialized the complete
     state. Note: if a source is shared by multiple STs, the truncate must
     be conditional (only if all co-tracking STs also received a full refresh
     or their frontier is being reset). Use a safe cleanup approach: delete
     only rows with `lsn <= new_frontier_lsn` rather than TRUNCATE.

4. **Tests**
   - Integration test: create WAL-mode ST, insert rows, trigger FULL refresh,
     check `pg_replication_slots.confirmed_flush_lsn` has advanced.
   - Integration test: verify change buffer is empty after FULL refresh.

---

### G4: AUTO→FULL Adaptive Fallback — Change Buffer Cleanup

**Problem.** When `execute_differential_refresh()` detects that the change
ratio exceeds `PGS_DIFFERENTIAL_MAX_CHANGE_RATIO` (default 15%), it falls back
to `execute_manual_full_refresh()` (`src/refresh.rs:1476–1554`). The buffer
cleanup (`drain_pending_cleanups()` + `cleanup_change_buffers_by_frontier()`)
runs **before** the fallback decision at line 1331, operating only on rows
≤ the safe frontier LSN.

When FULL runs, it recomputes the entire table from scratch, making all pending
change buffer rows irrelevant. But these rows are **not** flushed — they
persist in the buffer and will be picked up by the next scheduler tick, which
may:
1. See changes → attempt DIFFERENTIAL again.
2. Exceed threshold again → fall back to FULL again.
3. Loop indefinitely on bulk-loaded data.

This can also cause a "change ratio ping-pong" where the scheduler alternates
between DIFFERENTIAL (small delta) and FULL (accumulated stale delta pushes
ratio over threshold).

#### Implementation Steps

1. **Flush change buffers after adaptive fallback** (`src/refresh.rs`)
   - In the adaptive fallback path (after `execute_manual_full_refresh()`
     returns), delete all change buffer rows up to the new frontier LSN:
     ```rust
     if should_fallback {
         let result = execute_manual_full_refresh(st, schema, table_name, source_oids);
         // After successful FULL, clear stale deltas to prevent
         // the next DIFFERENTIAL from re-triggering fallback.
         if result.is_ok() {
             cleanup_change_buffers_by_frontier(&change_schema, &catalog_source_oids);
         }
         return result;
     }
     ```

2. **Deduplicate with G3** — The FULL-refresh cleanup logic from G3 applies
   here too. Factor out into a shared `post_full_refresh_cleanup()` helper
   called from both the scheduled FULL path and the adaptive fallback path.

3. **Tests**
   - Integration test: bulk INSERT that triggers adaptive fallback → verify
     change buffer is empty after refresh.
   - Integration test: after fallback FULL, insert one row → next cycle should
     succeed as DIFFERENTIAL without hitting the ratio threshold.

---

### G5: TRANSITIONING State User Visibility

**Problem.** The `TRANSITIONING` CDC state is tracked in
`pgt_dependencies.cdc_mode` and is visible in
`pgtrickle.pgt_stream_table_sources`. However:

- There is no simple way to query "which stream tables are currently
  transitioning?" without joining across catalog tables.
- The `pgtrickle.pg_stat_stream_tables` monitoring view does not surface
  per-source CDC mode.
- No NOTIFY/event is emitted when a transition starts or completes.

This makes it difficult to debug slow transitions or stuck states.

#### Implementation Steps

1. **Add CDC mode to `pg_stat_stream_tables`** (`src/monitor.rs`)
   - Add a `cdc_modes` column (text array) showing the distinct CDC modes
     across all sources. Example: `{wal}`, `{trigger,wal}`,
     `{transitioning,wal}`.
   - Alternatively, add a scalar `cdc_status` column with a summary:
     `'wal'`, `'trigger'`, `'mixed'`, `'transitioning'`.

2. **Add convenience view** (`sql/` upgrade migration)
   ```sql
   CREATE VIEW pgtrickle.pgt_cdc_status AS
   SELECT
       st.pgt_schema,
       st.pgt_name,
       d.source_relid,
       c.relname AS source_name,
       d.cdc_mode,
       d.slot_name
   FROM pgtrickle.pgt_dependencies d
   JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
   JOIN pg_class c ON c.oid = d.source_relid
   WHERE d.source_type = 'TABLE';
   ```

3. **NOTIFY on transition events** (`src/wal_decoder.rs` or `src/scheduler.rs`)
   - When CDC mode changes from `TRIGGER` → `TRANSITIONING`:
     ```sql
     NOTIFY pgtrickle_cdc_transition, '{"source_oid": 12345, "from": "trigger", "to": "transitioning"}'
     ```
   - When `TRANSITIONING` → `WAL`:
     ```sql
     NOTIFY pgtrickle_cdc_transition, '{"source_oid": 12345, "from": "transitioning", "to": "wal"}'
     ```

4. **Documentation** — Update `docs/SQL_REFERENCE.md` with the new view and
   NOTIFY channel.

5. **Tests**
   - Integration test: create ST with `auto` mode on a `wal_level = logical`
     cluster → verify `pgt_cdc_status` shows `TRANSITIONING` then `WAL`.
   - Integration test: verify NOTIFY payload is emitted.

---

### G6: DIFFERENTIAL Without Initialization Baseline

**Problem.** If `execute_differential_refresh()` is called on a stream table
that has `is_populated = false` (never initialized), the frontier defaults to
`'0/0'::pg_lsn`. This means the delta query scans the *entire* change buffer
from the beginning of WAL, which:
- Is semantically wrong: a delta applied to an empty table is not the same as
  a full materialization (aggregates, JOINs, etc. produce different results).
- Is prohibitively slow on large buffers.

**Current mitigation.** The manual refresh path
(`execute_manual_differential_refresh()` at `src/api.rs:1898`) checks
`st.is_populated` and falls back to FULL. The scheduler path
(`src/scheduler.rs:1294–1340`) checks `prev_frontier.is_empty()` and falls
back to FULL. So in *practice*, this gap is mitigated.

**Residual risk.** `execute_differential_refresh()` itself has no guard — it
trusts its callers. A future caller could skip the check.

#### Implementation Steps

1. **Defensive check in `execute_differential_refresh()`** (`src/refresh.rs:1260`)
   - Add an early return at the top of the function:
     ```rust
     if !st.is_populated {
         return Err(PgTrickleError::InvalidArgument(format!(
             "Cannot run DIFFERENTIAL refresh on unpopulated stream table {}.{} — \
              a FULL refresh is required first.",
             st.pgt_schema, st.pgt_name
         )));
     }
     ```
   - Callers already handle errors by falling back to FULL or marking for
     reinit, so this is safe.

2. **Also guard on empty frontier** (belt-and-suspenders)
   - After the `is_populated` check:
     ```rust
     if prev_frontier.is_empty() {
         return Err(PgTrickleError::InvalidArgument(format!(
             "Cannot run DIFFERENTIAL refresh on {}.{} — no previous frontier exists.",
             st.pgt_schema, st.pgt_name
         )));
     }
     ```

3. **Tests**
   - Unit test: call `execute_differential_refresh()` with
     `is_populated = false` → returns error.
   - Integration test: create ST with `initialize => false`, attempt manual
     DIFFERENTIAL refresh → verify it falls back to FULL (existing behavior,
     but now explicitly guarded).

---

## 4. Implementation Order

The gaps have interdependencies. Recommended order:

```
Phase 1 (quick wins — small, independent, high value):
├── G6: Defensive check in execute_differential_refresh()
├── G2: Validate IMMEDIATE + WAL combination
└── G3: Advance WAL slot after FULL refresh

Phase 2 (buffer hygiene):
└── G4: Flush change buffers on adaptive fallback
        (shares cleanup helper with G3)

Phase 3 (observability):
└── G5: TRANSITIONING visibility (view + NOTIFY)

Phase 4 (feature):
└── G1: Per-table cdc_mode override
        (largest change — SQL API, catalog, dbt, migration, docs)
```

### Effort Estimates

| Phase | Gaps | Complexity |
|-------|------|------------|
| 1 | G6, G2, G3 | ~3–5 files changed, no migration |
| 2 | G4 | ~2 files changed, refactor shared cleanup |
| 3 | G5 | ~3 files + migration, new view |
| 4 | G1 | ~8–10 files + migration + dbt macro changes |

---

## 5. Migration Strategy

Phases 1–2 require no SQL migration (pure Rust logic changes).

Phase 3 requires an upgrade migration for the convenience view.

Phase 4 requires an upgrade migration for the new
`pgt_stream_tables.requested_cdc_mode` column and updated function
signatures.

All changes are backward compatible: existing stream tables continue to work
without any user action. The per-table `cdc_mode` defaults to `NULL` (inherit
from GUC), and the new validations only reject configurations that were
already broken or undefined.

---

## 6. Open Questions

1. **G1 granularity**: Should per-table `cdc_mode` be on the *stream table*
   (applies to all its sources) or on individual *source dependencies*? The
   former is simpler; the latter handles the case where one ST joins a
   PK-table with a keyless table.

2. **G3 shared sources**: When a source table is tracked by multiple STs and
   only one does a FULL refresh, we cannot TRUNCATE the shared change buffer.
   Should we use per-ST frontier-based DELETE instead? Or maintain per-ST
   buffer tables (major architectural change)?

3. **G4 ping-pong prevention**: Beyond buffer cleanup, should we add a
   backoff mechanism? E.g., after an adaptive fallback, force the next N
   cycles to use FULL to let the buffer stabilize.

4. **G5 NOTIFY volume**: On a cluster with hundreds of stream tables, CDC
   transition NOTIFYs could be noisy. Should this be gated behind a GUC
   (e.g., `pg_trickle.notify_cdc_transitions = on`)?

---

## 7. Related Plans

- [PLAN_HYBRID_CDC.md](PLAN_HYBRID_CDC.md) — The original trigger→WAL
  transition design. G1 and G2 extend this.
- [PLAN_TRANSACTIONAL_IVM.md](PLAN_TRANSACTIONAL_IVM.md) — IMMEDIATE mode
  design. G2 adds validation at the boundary between IMMEDIATE and CDC.
- [PLAN_BOOTSTRAP_GATING.md](PLAN_BOOTSTRAP_GATING.md) — Bootstrap readiness.
  G6's defensive check complements this by preventing incorrect differential
  refreshes on uninitiated tables.
- [PLAN_REFRESH_MODE_DEFAULT.md](PLAN_REFRESH_MODE_DEFAULT.md) — AUTO mode
  default behavior. G4 addresses the buffer cleanup gap in AUTO's fallback.
