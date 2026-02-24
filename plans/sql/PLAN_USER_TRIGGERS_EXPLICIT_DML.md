# Plan: User Triggers on Stream Tables via Explicit DML

**Status:** Implemented (Phases 1 & 3 complete; Phase 2 deferred)  
**Date:** 2026-02-24  
**Supersedes:** [PLAN_USER_TRIGGER_REPLAY.md](PLAN_USER_TRIGGER_REPLAY.md)  
**Related:** [PLAN_USER_TRIGGERS.md](PLAN_USER_TRIGGERS.md) (Phase 1 only)  
**Effort:** ~3–5 days (3 phases)

### Implementation Status

| Phase | Status | Notes |
|---|---|---|
| Phase 1: Trigger detection + explicit DML | ✅ Complete | `has_user_triggers()` in cdc.rs, explicit DML path in refresh.rs, `CachedMergeTemplate` extended, per-step profiling |
| Phase 2: FULL refresh trigger support | ⏳ Deferred | Row-level triggers suppressed during FULL refresh via `DISABLE TRIGGER USER` + `NOTIFY pgstream_refresh`. Snapshot-diff replay deferred as documented limitation. |
| Phase 3: Documentation + GUC + DDL warning | ✅ Complete | `pg_stream.user_triggers` GUC (auto/on/off), DDL warning in hooks.rs, docs updated (SQL_REFERENCE.md, FAQ.md, CONFIGURATION.md) |

**Files modified:**
- `src/cdc.rs` — `has_user_triggers()`
- `src/config.rs` — `PGS_USER_TRIGGERS` GUC
- `src/refresh.rs` — `CachedMergeTemplate` extension, explicit DML branch, FULL refresh trigger suppression + NOTIFY, profiling
- `src/hooks.rs` — DDL warning on `CREATE TRIGGER` targeting a stream table
- `docs/SQL_REFERENCE.md` — User triggers now ✅ Supported (DIFFERENTIAL)
- `docs/FAQ.md` — Rewrote trigger FAQ entries, added GUC to reference table
- `docs/CONFIGURATION.md` — Added `pg_stream.user_triggers` section
- `tests/e2e_user_trigger_tests.rs` — 10 E2E tests (INSERT/UPDATE/DELETE triggers, no-op skip, audit trail, GUC control, FULL suppression, BEFORE trigger)
- `tests/trigger_detection_tests.rs` — 7 integration tests for `has_user_triggers()` SQL pattern

**Tests:** 841 unit tests pass, 7 new integration tests pass, 10 new E2E tests (require `just build-e2e-image`).

---

## Problem

User-defined triggers on stream table (ST) storage tables do not work today.
The refresh engine executes a single `MERGE` statement that handles INSERT,
UPDATE, and DELETE in one pass. Even if user triggers exist, they either:

1. Fire during internal MERGE operations (spurious, wrong `TG_OP`, partial
   state visible) — if no suppression is in place, or
2. Never fire at all — if suppressed by `session_replication_role` or
   `DISABLE TRIGGER USER`.

Users expect triggers on STs to behave like triggers on regular tables:
correct `TG_OP`, correct `OLD`/`NEW`, firing only on real data changes.

## Goal

Allow user-defined row-level `AFTER` triggers on ST storage tables to fire
with correct semantics during refresh, with minimal overhead for STs that do
not have user triggers.

---

## Approach: Replace MERGE with Explicit DML

### Key Insight

PostgreSQL triggers fire correctly on individual DML statements. A `DELETE`
fires `AFTER DELETE` triggers with the correct `OLD` value. An `UPDATE` fires
`AFTER UPDATE` with correct `OLD` and `NEW`. An `INSERT` fires `AFTER INSERT`
with correct `NEW`. The problem is that `MERGE` bundles all three operations
into a single statement with a single trigger invocation point.

The solution: when a ST has user-defined triggers, decompose the MERGE into
three explicit DML statements — DELETE, UPDATE, INSERT — each of which fires
its triggers natively.

### Architecture

```
Differential refresh (no user triggers — unchanged):
  1. Build delta SQL (cached template)
  2. Execute MERGE (single pass, current behavior)

Differential refresh (with user triggers):
  1. Build delta SQL (same cached template)
  2. Materialize delta into a temp table
  3. DELETE rows where __pgs_action = 'D'
  4. UPDATE rows where __pgs_action = 'I' AND row exists    ← triggers fire
  5. INSERT rows where __pgs_action = 'I' AND row is new    ← triggers fire
     (DELETE triggers fire in step 3)

Full refresh (with user triggers):
  1. Snapshot current row IDs into a temp table
  2. TRUNCATE + INSERT (as today, triggers disabled)
  3. Enable triggers
  4. Compute diff: old snapshot vs new contents
  5. Fire INSERT/DELETE/UPDATE via explicit DML on changed rows
```

### Why This Works

Each DML statement in steps 3–5 is a standard PostgreSQL operation. The
trigger machinery provides correct:

- **`TG_OP`** — `'INSERT'`, `'UPDATE'`, or `'DELETE'`
- **`OLD`** — The pre-change row (UPDATE, DELETE)
- **`NEW`** — The post-change row (INSERT, UPDATE)
- **`TG_TABLE_NAME`** — The stream table name
- **`BEFORE` triggers** — Also supported (can modify `NEW` before write)
- **Firing order** — Standard PostgreSQL trigger ordering rules apply

---

## Design Decisions

### D-1: MERGE preserved for STs without user triggers

The explicit DML path is only used when `has_user_triggers(st_oid)` returns
true. STs without user triggers continue to use the MERGE path — zero
overhead, zero behavior change.

The check queries `pg_trigger` for non-internal, non-pg_stream row-level
triggers. This is a single index scan on `pg_trigger(tgrelid)`, cached per
refresh cycle.

### D-2: Delta materialized to temp table

The delta query is evaluated once and materialized into a temp table:

```sql
CREATE TEMP TABLE __pgs_delta_<pgs_id> ON COMMIT DROP AS (
    SELECT DISTINCT ON (__pgs_row_id) *
    FROM (<delta_sql>) __raw
    ORDER BY __pgs_row_id, __pgs_action DESC
);
```

This avoids evaluating the delta query three times (once per DML statement).
The temp table is dropped at transaction commit (`ON COMMIT DROP`), so no
cleanup is needed.

**Cost:** ~2–3 ms for temp table creation + the delta evaluation cost (same as
the MERGE path). Net overhead vs MERGE: only the temp table DDL.

### D-3: Three DML statements instead of MERGE

```sql
-- Step 1: DELETE removed rows
DELETE FROM <st> AS dt
USING __pgs_delta_<pgs_id> AS d
WHERE dt.__pgs_row_id = d.__pgs_row_id
  AND d.__pgs_action = 'D';

-- Step 2: UPDATE changed rows (row existed, new value from delta)
UPDATE <st> AS dt
SET col1 = d.col1, col2 = d.col2, ...
FROM __pgs_delta_<pgs_id> AS d
WHERE dt.__pgs_row_id = d.__pgs_row_id
  AND d.__pgs_action = 'I';

-- Step 3: INSERT new rows (row did not exist)
INSERT INTO <st> (__pgs_row_id, col1, col2, ...)
SELECT d.__pgs_row_id, d.col1, d.col2, ...
FROM __pgs_delta_<pgs_id> AS d
WHERE d.__pgs_action = 'I'
  AND NOT EXISTS (
    SELECT 1 FROM <st> AS dt
    WHERE dt.__pgs_row_id = d.__pgs_row_id
  );
```

> **Note on UPDATE vs INSERT distinction:** The delta query produces
> `__pgs_action = 'I'` for both new rows and changed rows (a changed row
> appears as a DELETE of the old version + INSERT of the new version, which
> `DISTINCT ON` collapses to action `'I'`). Step 2's `UPDATE ... FROM`
> succeeds only for rows that already exist (the JOIN filters). Step 3's
> `NOT EXISTS` catches only genuinely new rows. This separation gives
> correct `TG_OP` for each trigger.

### D-4: B-1 (IS DISTINCT FROM) preserved

The current MERGE includes a `WHEN MATCHED AND (IS DISTINCT FROM)` guard to
skip no-op updates. In the explicit DML path, the same guard is applied to
the UPDATE:

```sql
UPDATE <st> AS dt
SET col1 = d.col1, col2 = d.col2, ...
FROM __pgs_delta_<pgs_id> AS d
WHERE dt.__pgs_row_id = d.__pgs_row_id
  AND d.__pgs_action = 'I'
  AND (dt.col1 IS DISTINCT FROM d.col1
       OR dt.col2 IS DISTINCT FROM d.col2
       OR ...);
```

This ensures UPDATE triggers only fire when values actually changed — no
spurious trigger invocations for no-op updates.

### D-5: DISABLE TRIGGER USER not needed for differential

In the explicit DML path, triggers fire **correctly** during the DML
statements. There is no need to suppress them. This eliminates the
`ACCESS EXCLUSIVE` lock concern from PLAN_USER_TRIGGER_REPLAY.md.

For FULL refresh, trigger suppression is still needed during `TRUNCATE +
INSERT` (see Phase 3).

### D-6: Template caching

The explicit DML SQL templates are cached alongside the existing MERGE
template in `CachedMergeTemplate`. Three new fields:

```rust
struct CachedMergeTemplate {
    // ... existing fields ...
    /// DELETE statement for trigger-enabled DML path
    trigger_delete_template: String,
    /// UPDATE statement for trigger-enabled DML path
    trigger_update_template: String,
    /// INSERT statement for trigger-enabled DML path
    trigger_insert_template: String,
}
```

These use the same `__PGS_PREV_LSN_*` / `__PGS_NEW_LSN_*` placeholder tokens
and are resolved identically to the MERGE template.

### D-7: Interaction with existing B-3 DELETE+INSERT path

The current codebase has a `delete_insert` strategy (GUC
`pg_stream.merge_strategy = 'delete_insert'`) that uses two statements instead
of MERGE. The user-trigger path is distinct:

| Path | DELETE | UPDATE | INSERT | Use case |
|---|---|---|---|---|
| MERGE (default) | Via `WHEN MATCHED AND 'D'` | Via `WHEN MATCHED AND 'I'` | Via `WHEN NOT MATCHED` | No user triggers |
| B-3 delete_insert | All affected rows | None (implied by DELETE+INSERT) | Non-deleted rows | Large deltas (GUC override) |
| **User trigger path** | Action='D' rows | Changed existing rows | New rows | STs with user triggers |

The user trigger path is semantically different from B-3: it distinguishes
UPDATE from INSERT to fire correct trigger types.

---

## Implementation Phases

### Phase 1: Trigger Detection + Explicit DML Path (~2 days)

**Goal:** When a ST has user triggers, use explicit DML instead of MERGE.

#### 1.1 Add `has_user_triggers()` to `src/cdc.rs`

```rust
/// Returns true if the stream table has any user-defined row-level
/// triggers (excluding internal pg_stream triggers).
///
/// Cached per refresh cycle — the check runs once per ST per scheduler
/// tick, not once per DML statement.
pub fn has_user_triggers(st_relid: pg_sys::Oid) -> Result<bool, PgStreamError> {
    Spi::get_one::<bool>(&format!(
        "SELECT EXISTS(\
           SELECT 1 FROM pg_trigger \
           WHERE tgrelid = {}::oid \
             AND tgisinternal = false \
             AND tgname NOT LIKE 'pgs_%' \
         )",
        st_relid.as_u32(),
    ))
    .map_err(|e| PgStreamError::SpiError(e.to_string()))
    .map(|v| v.unwrap_or(false))
}
```

#### 1.2 Build explicit DML templates in `src/refresh.rs`

Extend the MERGE template builder (both the cache-miss path and
`prewarm_merge_cache()`) to also generate the three DML templates:

```rust
// After building merge_template and delete_insert_template:

let trigger_delete_template = format!(
    "DELETE FROM {quoted_table} AS dt \
     USING __pgs_delta_{pgs_id} AS d \
     WHERE dt.__pgs_row_id = d.__pgs_row_id \
       AND d.__pgs_action = 'D'",
    pgs_id = dt.pgs_id,
);

let trigger_update_template = format!(
    "UPDATE {quoted_table} AS dt \
     SET {update_set_clause} \
     FROM __pgs_delta_{pgs_id} AS d \
     WHERE dt.__pgs_row_id = d.__pgs_row_id \
       AND d.__pgs_action = 'I' \
       AND ({is_distinct_clause})",
    pgs_id = dt.pgs_id,
);

let trigger_insert_template = format!(
    "INSERT INTO {quoted_table} (__pgs_row_id, {user_col_list}) \
     SELECT d.__pgs_row_id, {d_user_col_list} \
     FROM __pgs_delta_{pgs_id} AS d \
     WHERE d.__pgs_action = 'I' \
       AND NOT EXISTS (\
         SELECT 1 FROM {quoted_table} AS dt \
         WHERE dt.__pgs_row_id = d.__pgs_row_id\
       )",
    pgs_id = dt.pgs_id,
);
```

#### 1.3 Add user-trigger DML path in `execute_differential_refresh()`

After the strategy selection block (`use_delete_insert` / `use_prepared` /
MERGE), add a new branch:

```rust
let has_triggers = crate::cdc::has_user_triggers(dt.pgs_relid)?;

let (merge_count, strategy_label) = if has_triggers {
    // ── User-trigger path: explicit DML ─────────────────────────
    // Step 1: Materialize delta into temp table
    let delta_sql = &resolved.merge_sql; // Contains USING clause
    let materialize_sql = format!(
        "CREATE TEMP TABLE __pgs_delta_{pgs_id} ON COMMIT DROP AS \
         SELECT * FROM {using_clause} AS d",
        pgs_id = dt.pgs_id,
    );
    Spi::run(&materialize_sql)?;

    // Step 2: DELETE removed rows (triggers fire)
    let del_count = Spi::connect_mut(|client| {
        let r = client.update(&resolved.trigger_delete_sql, None, &[])?;
        Ok::<usize, PgStreamError>(r.len())
    })?;

    // Step 3: UPDATE changed existing rows (triggers fire)
    let upd_count = Spi::connect_mut(|client| {
        let r = client.update(&resolved.trigger_update_sql, None, &[])?;
        Ok::<usize, PgStreamError>(r.len())
    })?;

    // Step 4: INSERT new rows (triggers fire)
    let ins_count = Spi::connect_mut(|client| {
        let r = client.update(&resolved.trigger_insert_sql, None, &[])?;
        Ok::<usize, PgStreamError>(r.len())
    })?;

    (del_count + upd_count + ins_count, "explicit_dml")
} else if use_delete_insert {
    // ... existing B-3 path ...
```

#### 1.4 Extend `CachedMergeTemplate` struct

Add three new fields for the trigger DML templates. Update both the
cache-miss path and `prewarm_merge_cache()`.

#### 1.5 Files modified

| File | Changes |
|---|---|
| `src/cdc.rs` | Add `has_user_triggers()` |
| `src/refresh.rs` | Extend `CachedMergeTemplate`, add explicit DML path in `execute_differential_refresh()`, extend `prewarm_merge_cache()` |

#### 1.6 Testing

| Test | Description |
|---|---|
| `test_explicit_dml_insert` | New source row → ST trigger fires with `TG_OP = 'INSERT'` and correct `NEW` |
| `test_explicit_dml_update` | Changed source row → ST trigger fires with `TG_OP = 'UPDATE'` and correct `OLD`/`NEW` |
| `test_explicit_dml_delete` | Deleted source row → ST trigger fires with `TG_OP = 'DELETE'` and correct `OLD` |
| `test_explicit_dml_no_op_skip` | Source change with same aggregate → B-1 IS DISTINCT FROM guard prevents trigger |
| `test_no_trigger_uses_merge` | ST without triggers → MERGE path, verify [PGS_PROFILE] shows `strategy=merge` |
| `test_trigger_audit_trail` | Add audit trigger on ST → verify audit table has correct entries after refresh |
| `test_before_trigger_modifies_new` | `BEFORE UPDATE` trigger modifies `NEW.col` → verify ST has trigger-modified value |

---

### Phase 2: Full Refresh Trigger Support (~2 days)

**Goal:** Fire correct triggers for changes caused by FULL refresh.

#### 2.1 Problem

Full refresh does `TRUNCATE` + `INSERT INTO st SELECT ... FROM defining_query`.
This fires:

- TRUNCATE: no row-level triggers (PostgreSQL limitation)
- INSERT: fires `AFTER INSERT` for all rows — even rows that were already
  present with the same values

Users expect: INSERT triggers for genuinely new rows, UPDATE triggers for
changed rows, DELETE triggers for removed rows, and no triggers for unchanged
rows.

#### 2.2 Solution: snapshot-diff for FULL refresh only

For FULL refresh when user triggers exist:

```rust
fn execute_full_refresh_with_triggers(
    dt: &StreamTableMeta,
) -> Result<(i64, i64), PgStreamError> {
    // Step 1: Snapshot current ST row IDs + content hash
    let snapshot_sql = format!(
        "CREATE TEMP TABLE __pgs_pre_{pgs_id} ON COMMIT DROP AS \
         SELECT __pgs_row_id, {user_col_list} \
         FROM \"{schema}\".\"{name}\"",
        pgs_id = dt.pgs_id,
        schema = dt.pgs_schema,
        name = dt.pgs_name,
    );
    Spi::run(&snapshot_sql)?;

    // Step 2: TRUNCATE + INSERT with triggers DISABLED
    Spi::run(&format!(
        "ALTER TABLE \"{}\".\"{}\" DISABLE TRIGGER USER",
        dt.pgs_schema, dt.pgs_name,
    ))?;

    let (rows, _) = execute_full_refresh(dt)?;

    Spi::run(&format!(
        "ALTER TABLE \"{}\".\"{}\" ENABLE TRIGGER USER",
        dt.pgs_schema, dt.pgs_name,
    ))?;

    // Step 3: Compute diff against pre-snapshot
    // DELETE: rows in pre but not in post
    let del_sql = format!(
        "DELETE FROM \"{schema}\".\"{name}\" AS dt \
         USING __pgs_pre_{pgs_id} AS old \
         WHERE dt.__pgs_row_id = old.__pgs_row_id \
           AND NOT EXISTS (\
             SELECT 1 FROM \"{schema}\".\"{name}\" AS curr \
             WHERE curr.__pgs_row_id = old.__pgs_row_id\
           )",
    );
    // ... but the rows were already truncated and replaced. We need a
    // different approach.

    // Actually: after TRUNCATE + INSERT, the post state IS the new truth.
    // The pre-snapshot has the old truth. We diff:

    // Deleted rows: in pre, not in post → fire DELETE triggers
    // Need to temporarily re-insert old rows, then delete them.
    // This is the same fragility concern from PLAN_USER_TRIGGER_REPLAY.md.

    // Simpler: use the same explicit DML approach as differential refresh.
    // Compute a "virtual delta" from the pre-snapshot and new state.

    // DELETES: rows in __pgs_pre that are NOT in the new ST
    // → re-insert from __pgs_pre (disabled triggers), then delete (enabled)
    //
    // UPDATES: rows in both with different values
    // → restore old values from __pgs_pre (disabled triggers), then
    //   update to new values (enabled triggers)
    //
    // INSERTS: rows in new ST that are NOT in __pgs_pre
    // → already inserted. Delete then re-insert (enabled triggers).
    //
    // This is complex. Use the simpler "NOTIFY + change event" approach
    // from PLAN_USER_TRIGGER_REPLAY Phase 2 specifically for FULL refresh.
    todo!()
}
```

**Revised approach for FULL refresh:**

After further analysis, the cleanest approach for FULL refresh is:

1. **Pre-snapshot** current ST into temp table (triggers disabled during
   TRUNCATE+INSERT — no `ACCESS EXCLUSIVE` needed if we use
   `session_replication_role` just for FULL refresh, since we don't need a
   WAL decoder on the ST for this plan).
2. **TRUNCATE + INSERT** as today (triggers suppressed).
3. **Diff** pre-snapshot vs new state:
   - `EXCEPT` for deleted row IDs
   - `EXCEPT` for inserted row IDs  
   - `INTERSECT` with value comparison for changed rows
4. **Fire triggers via explicit DML:**
   - For **deleted rows**: re-insert from snapshot (triggers disabled), then
     DELETE (triggers enabled). Wrapped in SAVEPOINT.
   - For **updated rows**: restore old values (triggers disabled), then
     UPDATE to new values (triggers enabled).
   - For **new rows**: delete then re-insert (triggers enabled). Or: leave
     as-is and accept that INSERT triggers already fired during step 2. Wait —
     triggers were suppressed in step 2.

**Simplification:** Since triggers are suppressed during step 2, no triggers
fire at all. Then in step 4, we fire explicit DML for each change type. But
this has the re-insert/delete fragility for INSERTs and DELETEs.

**Pragmatic decision:** For FULL refresh, fire **statement-level** `AFTER
INSERT` trigger (if any) after the TRUNCATE+INSERT, and emit a `NOTIFY` with
a change summary. For row-level trigger fidelity on FULL refresh, users should
use `REFRESH MODE DIFFERENTIAL` (which uses the Phase 1 explicit DML path).

This sidesteps the inherent complexity of FULL refresh trigger replay while
providing a reasonable upgrade path.

#### 2.3 Actual implementation

```rust
// In execute_full_refresh, after TRUNCATE + INSERT:
if has_user_triggers {
    // Fire statement-level AFTER INSERT triggers if any exist.
    // Row-level triggers cannot be fired with correct OLD/NEW for FULL
    // refresh without prohibitive complexity. Users who need row-level
    // triggers should use REFRESH MODE DIFFERENTIAL.

    // Emit NOTIFY with change summary
    Spi::run(&format!(
        "NOTIFY pgstream_refresh, '{{\
           \"stream_table\": \"{name}\", \
           \"schema\": \"{schema}\", \
           \"mode\": \"FULL\", \
           \"rows\": {rows}\
         }}'",
        name = dt.pgs_name,
        schema = dt.pgs_schema,
        rows = rows_inserted,
    ))?;
}
```

**Document clearly:** Row-level user triggers on STs fire correctly only for
`REFRESH MODE DIFFERENTIAL`. FULL refresh emits NOTIFY but does not fire
row-level triggers.

#### 2.4 Files modified

| File | Changes |
|---|---|
| `src/refresh.rs` | Add trigger suppression to `execute_full_refresh()`, add NOTIFY |

#### 2.5 Testing

| Test | Description |
|---|---|
| `test_full_refresh_suppresses_triggers` | FULL refresh → verify row-level triggers do NOT fire |
| `test_full_refresh_notify` | FULL refresh with triggers → verify NOTIFY received |
| `test_full_refresh_statement_trigger` | Statement-level AFTER INSERT → verify it fires after FULL refresh |

---

### Phase 3: Documentation, GUC, and DDL Warning (~1 day)

**Goal:** User-facing documentation and protective warnings.

#### 3.1 GUC: `pg_stream.user_triggers`

```rust
/// Enable user-trigger-aware refresh paths.
/// - "auto" (default): use explicit DML when user triggers are detected
/// - "off": suppress user triggers during refresh (current behavior)
/// - "on": always use explicit DML path (even without triggers, for testing)
pub static PGS_USER_TRIGGERS: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"auto"));
```

When set to `"off"`, the refresh engine uses MERGE unconditionally and
suppresses user triggers via `DISABLE TRIGGER USER` (PLAN_USER_TRIGGERS.md
Phase 1 behavior). This is the escape hatch if explicit DML causes issues.

#### 3.2 DDL event trigger warning

Extend `src/hooks.rs` to detect `CREATE TRIGGER` on a ST:

```
WARNING: pg_stream: trigger "my_trigger" on stream table "regional_totals"
will fire during differential refresh with correct TG_OP/OLD/NEW.
Note: row-level triggers do NOT fire during FULL refresh.
Use REFRESH MODE DIFFERENTIAL to ensure triggers fire on every change.
```

#### 3.3 Documentation updates

| Document | Change |
|---|---|
| `README.md` | Update restrictions table: `✅ Yes` for user triggers |
| `docs/SQL_REFERENCE.md` | Add section on user-trigger behavior |
| `docs/CONFIGURATION.md` | Add `pg_stream.user_triggers` GUC |
| `docs/FAQ.md` | Add "Can I add triggers to stream tables?" entry |

#### 3.4 Files modified

| File | Changes |
|---|---|
| `src/config.rs` | Add `PGS_USER_TRIGGERS` GUC |
| `src/hooks.rs` | Add CREATE TRIGGER detection + warning |
| `README.md`, `docs/*` | Documentation updates |

#### 3.5 Testing

| Test | Description |
|---|---|
| `test_guc_off_suppresses_triggers` | `pg_stream.user_triggers = 'off'` → triggers do not fire |
| `test_guc_auto_detects_triggers` | Default GUC + user trigger → explicit DML path used |
| `test_ddl_warning_on_create_trigger` | CREATE TRIGGER on ST → WARNING emitted |

---

## Performance Analysis

### STs without user triggers (zero overhead)

The only added cost is the `has_user_triggers()` check — a single index scan
on `pg_trigger(tgrelid)`. This returns false immediately (no rows match) and
costs <0.1 ms. The rest of the refresh is unchanged.

### STs with user triggers (explicit DML path)

| Cost component | MERGE (baseline) | Explicit DML | Delta |
|---|---|---|---|
| Delta query evaluation | 1× | 1× (same query) | 0 |
| Temp table creation | — | ~2–3 ms | +2–3 ms |
| Index scans on ST | 1 (MERGE) | 3 (DELETE + UPDATE + INSERT) | +2 scans |
| Per-row trigger overhead | 0 | ~2–5 μs per affected row | New cost |
| Temp table drop | — | Automatic (ON COMMIT DROP) | 0 |
| `has_user_triggers()` check | — | ~0.1 ms | +0.1 ms |

### Estimated overhead by scenario

| Scenario | MERGE (no triggers) | Explicit DML | Overhead |
|---|---|---|---|
| Aggregate ST, 5 groups changed | ~8 ms | ~11 ms | **~35%** |
| Aggregate ST, 50 groups changed | ~12 ms | ~17 ms | **~40%** |
| Join ST, 200 rows changed | ~15 ms | ~22 ms | **~45%** |
| Join ST, 1000 rows changed | ~25 ms | ~40 ms | **~60%** |
| No user triggers | unchanged | unchanged | **0%** |

### Comparison with PLAN_USER_TRIGGER_REPLAY.md (snapshot-diff)

| Metric | Explicit DML (this plan) | Snapshot-diff (superseded) |
|---|---|---|
| Delta evaluations | **1** | 2 (dominant hidden cost) |
| JSON serialization | **0** | 2N rows (pre + post) |
| Change event table writes | **0** | N rows |
| Per-refresh overhead | **25–60%** | 65–160% |
| Implementation complexity | **Low** (~3–5 days) | Medium (~2–3 weeks) |
| Trigger semantics | **Native PostgreSQL** | Change events only (Tier 1) |
| BEFORE triggers | **✅ Supported** | ❌ Not possible |
| Audit trail table | ❌ Not built-in | ✅ st_changes table |

The explicit DML approach is roughly **half the overhead** of snapshot-diff
while providing **better trigger semantics** with **less code**.

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| 3 index scans slower than 1 MERGE pass | Medium | Low | Only ~2–5 ms overhead for typical STs. The temp table avoids re-evaluating the delta. |
| INSERT `NOT EXISTS` subquery slow on large ST | Low | Medium | `__pgs_row_id` has a unique index; NOT EXISTS uses anti-join. |
| Temp table creation overhead | Low | Low | ~2–3 ms; `ON COMMIT DROP` avoids cleanup. |
| User trigger writes to source table (feedback loop) | Medium | High | Document clearly. Add detection + WARNING in scheduler. |
| `BEFORE` trigger modifies NEW incorrectly | Low | Medium | Valid PostgreSQL behavior; user responsibility. Document that BEFORE triggers can affect ST contents. |
| FULL refresh does not fire row-level triggers | Medium | Low | Documented limitation. Users can use `REFRESH MODE DIFFERENTIAL`. |

---

## Interaction with Other Plans

### PLAN_USER_TRIGGERS.md

This plan subsumes Phase 1 of PLAN_USER_TRIGGERS.md (`DISABLE TRIGGER USER`
suppression) — it is only needed for FULL refresh in Phase 2. The GUC
replacement is:

| PLAN_USER_TRIGGERS.md | This plan |
|---|---|
| `pg_stream.suppress_user_triggers` | `pg_stream.user_triggers = 'off'` |

PLAN_USER_TRIGGERS.md Phases 2–3 (NOTIFY, DDL warning) are absorbed into
Phase 3 of this plan.

### PLAN_HYBRID_CDC.md

Fully independent. PLAN_HYBRID_CDC.md operates on source tables (improving
write performance and adding TRUNCATE capture). This plan operates on stream
tables (enabling user triggers). No interaction or dependency.

### PLAN_USER_TRIGGER_REPLAY.md

**Superseded.** The snapshot-diff + change event + DML replay approach is
replaced by the simpler explicit DML decomposition. The change event table
(Tier 1 of the old plan) can be implemented as a separate optional feature
later if users want queryable audit trails — but it is no longer the primary
mechanism for user trigger support.

---

## Effort Estimate

| Phase | Effort | Priority | Status |
|---|---|---|---|
| Phase 1: Trigger detection + explicit DML | ~2 days | **High** | ✅ Complete |
| Phase 2: FULL refresh support | ~2 days | Medium | ⏳ Deferred (suppression implemented, snapshot-diff deferred) |
| Phase 3: Documentation + GUC + DDL warning | ~1 day | Medium | ✅ Complete |
| **Total** | **~3–5 days** | | Phases 1 & 3 done |

---

## Open Questions

1. **Statement-level triggers:** Should statement-level `AFTER INSERT/UPDATE/
   DELETE` triggers fire? The explicit DML path fires them naturally (one per
   DML statement). With 3 statements, users get three trigger invocations.
   This is correct but may surprise users who expect one logical "refresh
   event." Consider adding a `NOTIFY pgstream_refresh` for the logical event.
   > **Resolved:** Statement-level triggers fire naturally. A `NOTIFY pgstream_refresh` is emitted for FULL refresh. The 3-invocation behavior is documented.

2. **Trigger execution order:** With DELETE → UPDATE → INSERT execution order,
   triggers fire in that order. If a user has dependencies between trigger
   types, this could matter. Document the execution order.
   > **Resolved:** Execution order is DELETE → UPDATE → INSERT. Documented in FAQ.md.

3. **FULL refresh row-level triggers:** The current plan does not fire
   row-level triggers for FULL refresh. Is this acceptable? The alternative
   (snapshot-diff replay) adds significant complexity. Recommendation: defer
   to a future enhancement; document as a known limitation.
   > **Resolved:** Accepted as documented limitation. FULL refresh suppresses row-level triggers via `DISABLE TRIGGER USER` / `ENABLE TRIGGER USER` and emits `NOTIFY pgstream_refresh`. Users who need per-row triggers should use `REFRESH MODE DIFFERENTIAL`. Tested in `test_full_refresh_suppresses_triggers`.

4. **B-1 guard and UPDATE triggers:** The IS DISTINCT FROM guard skips no-op
   UPDATEs. This means UPDATE triggers only fire when values actually change.
   Confirm this is the desired behavior (almost certainly yes).
   > **Resolved:** Confirmed. The IS DISTINCT FROM guard prevents spurious UPDATE triggers. Tested in `test_explicit_dml_no_op_skip`.

5. **`BEFORE` trigger changing `NEW`:** If a `BEFORE UPDATE` trigger modifies
   `NEW.col`, the ST will contain the trigger-modified value, which may differ
   from the defining query's output. The next differential refresh will see
   this as a change and try to "correct" it, creating an oscillation. Add
   detection + WARNING for this case.
   > **Resolved:** BEFORE triggers that modify NEW work correctly (tested in `test_before_trigger_modifies_new`). Oscillation risk is documented in FAQ.md. Detection + WARNING deferred to a future enhancement.

---

## Commit Plan

1. ✅ `feat: add has_user_triggers() detection in cdc.rs`
2. ✅ `feat: explicit DML path for differential refresh with user triggers`
3. ✅ `feat: extend CachedMergeTemplate with trigger DML templates`
4. ✅ `test: E2E tests for user triggers on stream tables`
5. ✅ `feat: FULL refresh trigger suppression + NOTIFY`
6. ✅ `feat: add pg_stream.user_triggers GUC`
7. ✅ `feat: DDL warning on CREATE TRIGGER targeting a stream table`
8. ✅ `docs: document user trigger support and limitations`
