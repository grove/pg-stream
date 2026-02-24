# Plan: User Trigger Replay via Post-Refresh WAL Decode

**Status:** Proposed  
**Date:** 2026-02-24  
**Depends on:**
[PLAN_USER_TRIGGERS.md](PLAN_USER_TRIGGERS.md) (Phase 1 — suppress during
refresh) · [PLAN_HYBRID_CDC.md](PLAN_HYBRID_CDC.md) (Phase 3 — WAL decoder
infrastructure)  
**Effort:** ~1–2 weeks (4 phases, incrementally deliverable)

---

## Problem

[PLAN_USER_TRIGGERS.md](PLAN_USER_TRIGGERS.md) solves the *safety* problem:
it suppresses user triggers during refresh so they don't fire on internal
MERGE/TRUNCATE operations. But suppression is not the same as support.

Users who define triggers on stream tables (STs) expect them to behave like
triggers on regular tables — firing with correct `TG_OP`, `OLD`, and `NEW`
values when real data changes occur. Today, even with suppression, triggers
never fire at all because all DML on the ST happens inside the refresh engine
(which runs with triggers suppressed).

### What users expect

```sql
CREATE TABLE regional_totals AS STREAM
  SELECT region, SUM(amount) AS total FROM orders GROUP BY region;

-- User adds an audit trigger:
CREATE TRIGGER audit_changes
  AFTER INSERT OR UPDATE OR DELETE ON regional_totals
  FOR EACH ROW EXECUTE FUNCTION log_to_audit();
```

When `orders` gets a new row and the scheduler refreshes `regional_totals`,
the user expects:

| If the region is **new** | `audit_changes` fires with `TG_OP = 'INSERT'`, `NEW = (region, total)` |
|---|---|
| If the region **existed** and the total changed | `TG_OP = 'UPDATE'`, `OLD = (region, old_total)`, `NEW = (region, new_total)` |
| If the last order for a region is deleted | `TG_OP = 'DELETE'`, `OLD = (region, old_total)` |

This plan makes that happen.

---

## Approach: Post-Refresh WAL Replay

### Key Insight

The MERGE statement that the refresh engine executes on the ST storage table
produces standard PostgreSQL WAL records:

| MERGE clause | WAL record on ST | Semantics |
|---|---|---|
| `WHEN NOT MATCHED … THEN INSERT` | `INSERT` | New row added to ST |
| `WHEN MATCHED … AND action='D' THEN DELETE` | `DELETE` | Row removed from ST |
| `WHEN MATCHED … AND action='I' AND (IS DISTINCT FROM) THEN UPDATE` | `UPDATE` | Row values changed |
| B-1: IS DISTINCT FROM guard (no change) | *No WAL record* | Skipped — no-op |

For FULL refresh (`TRUNCATE` + `INSERT`):

| Operation | WAL record | Semantics |
|---|---|---|
| `TRUNCATE` | `TRUNCATE` | All existing rows removed |
| `INSERT INTO … SELECT …` | `INSERT` per row | All rows added fresh |

A WAL decoder attached to the **stream table** (not the source table) can read
these records after the refresh completes, reconstruct the change events, and
fire user triggers with the correct `TG_OP`, `OLD`, and `NEW` values.

### Architecture

```
Refresh Engine                           Trigger Replay Worker
─────────────────────────────────────    ──────────────────────────────
1. DISABLE TRIGGER USER on ST            
2. Execute MERGE / TRUNCATE+INSERT       → WAL records written
3. ENABLE TRIGGER USER on ST             
4. COMMIT                                
                                         5. Read WAL for ST via logical slot
                                         6. For each change:
                                            - Build OLD/NEW records
                                            - Strip __pgs_row_id
                                            - Fire user triggers
                                         7. Confirm LSN
```

**Critical property:** User triggers fire *after* the refresh transaction
commits. This means triggers see a **fully consistent ST state** — no partial
MERGE visibility. This is actually *better* than standard PostgreSQL row-level
AFTER triggers, which fire before the statement completes.

---

## Design Decisions

### D-1: Why not fire triggers inline (inside the MERGE)?

Firing triggers during the MERGE (with `session_replication_role = 'origin'`)
was rejected in PLAN_USER_TRIGGERS.md for good reasons:

1. **Spurious firing** — The MERGE may touch rows that don't represent real
   user-visible changes (e.g., internal bookkeeping).
2. **Partial state** — Mid-MERGE, the ST is inconsistent.
3. **Performance** — Per-row trigger overhead on the MERGE critical path.
4. **FULL refresh asymmetry** — TRUNCATE doesn't fire row-level DELETE
   triggers; the subsequent INSERTs fire for all rows regardless of whether
   they actually changed.

WAL replay avoids all four problems.

### D-2: Why not use statement-level triggers?

Statement-level `AFTER TRUNCATE` / `AFTER INSERT` triggers could fire after
the refresh, but they don't receive `OLD`/`NEW` values. Most trigger use cases
(auditing, denormalization, notifications) need row-level data.

### D-3: Why a WAL decoder on the ST (not the source)?

The source table's WAL shows raw OLTP operations. The stream table's WAL shows
the *materialized effect* of the defining query — exactly what the user's
trigger should see. A WAL decoder on the ST gives us the "trigger input" for
free.

### D-4: DISABLE TRIGGER USER vs session_replication_role

PLAN_USER_TRIGGERS.md proposes `SET LOCAL session_replication_role = 'replica'`
to suppress triggers during refresh. However, as noted in
[REPORT_TRIGGERS_VS_REPLICATION.md §3.4](REPORT_TRIGGERS_VS_REPLICATION.md),
`session_replication_role = 'replica'` also suppresses publication of changes
to logical replication subscribers — including our own WAL decoder slot on the
ST.

Therefore, this plan uses `ALTER TABLE … DISABLE TRIGGER USER` / `ENABLE
TRIGGER USER` instead:

| Property | `session_replication_role` | `DISABLE TRIGGER USER` |
|---|---|---|
| Suppresses user triggers | ✅ Yes | ✅ Yes |
| Suppresses WAL publication | ❌ Yes (breaks our decoder) | ✅ No |
| Lock level | None | `ACCESS EXCLUSIVE` (brief) |
| Crash safety | Auto-resets (SET LOCAL) | Must re-enable on recovery |

The `ACCESS EXCLUSIVE` lock is taken only for the DDL, not for the MERGE. The
sequence is:

```sql
ALTER TABLE st DISABLE TRIGGER USER;  -- brief ACCESS EXCLUSIVE
-- MERGE runs with normal locks (ROW EXCLUSIVE)
ALTER TABLE st ENABLE TRIGGER USER;   -- brief ACCESS EXCLUSIVE
```

This is a revision of Phase 1.1 in PLAN_USER_TRIGGERS.md. The rest of that
plan (GUC, NOTIFY, DDL warning) remains valid.

### D-5: Trigger execution context

User triggers fire in a **separate transaction** from the refresh. This means:

- `TG_TABLE_NAME` = the stream table name (correct)
- `TG_OP` = `INSERT` / `UPDATE` / `DELETE` (correct)
- `OLD` / `NEW` = the actual row values, excluding `__pgs_row_id` (correct)
- The ST is fully consistent (the refresh committed)
- Any writes the trigger performs are in their own transaction

If the trigger replay worker crashes, unprocessed WAL changes remain in the
slot. On restart, it resumes from the last confirmed LSN — no data loss.

### D-6: FULL refresh replay semantics

A FULL refresh does `TRUNCATE` + `INSERT`. The WAL contains:
- 1 TRUNCATE record
- N INSERT records (one per row)

**Option A (simple):** Replay as N `INSERT` triggers. The TRUNCATE is not
replayed as row-level DELETE triggers because there's no per-row OLD data in
the TRUNCATE WAL record — matching standard PostgreSQL TRUNCATE semantics.
Users who want to react to TRUNCATE can use a statement-level `AFTER TRUNCATE`
trigger on the ST, which we allow to fire (it's not suppressed by
`DISABLE TRIGGER USER` unless specifically disabled).

**Option B (full fidelity):** Before executing the TRUNCATE, read the current
ST contents into a temporary table. After TRUNCATE + INSERT, diff the old
snapshot against the new rows to produce INSERT/UPDATE/DELETE events. This
gives users the same semantics as differential refresh, but is expensive.

**Recommendation:** Start with Option A. Add Option B behind a GUC
(`pg_stream.full_refresh_trigger_diff = false`) for users who need it.

---

## Implementation Phases

### Phase 1: ST-Side Replication Slot & Publication (~2 days)

**Goal:** Create a logical replication slot and publication on each stream
table that has user-defined triggers.

#### 1.1 Detect user triggers on STs

Add a helper to check whether a ST storage table has user-defined triggers:

```rust
/// Returns true if the stream table has any user-defined row-level
/// AFTER triggers (excluding internal pg_stream triggers).
pub fn has_user_triggers(st_relid: pg_sys::Oid) -> Result<bool, PgStreamError> {
    Spi::get_one::<bool>(&format!(
        "SELECT EXISTS(\
           SELECT 1 FROM pg_trigger \
           WHERE tgrelid = {oid}::oid \
             AND tgisinternal = false \
             AND tgname NOT LIKE 'pgs_%' \
             AND tgtype & 1 = 1 \  -- ROW trigger
         )",
        oid = st_relid.as_u32(),
    ))
    .map_err(|e| PgStreamError::SpiError(e.to_string()))
    .map(|v| v.unwrap_or(false))
}
```

This is checked:
- After each refresh cycle (in the scheduler)
- After DDL event trigger detects `CREATE TRIGGER` on a ST

#### 1.2 Create ST-side publication and slot

When user triggers are detected on a ST for the first time:

```rust
fn setup_trigger_replay(st_relid: pg_sys::Oid, pgs_id: i64)
    -> Result<(), PgStreamError>
{
    let oid = st_relid.as_u32();
    let pub_name = format!("pgstream_st_{oid}");
    let slot_name = format!("pgstream_st_{oid}");

    // Create publication for the ST storage table
    let table_name = get_qualified_table_name(st_relid)?;
    Spi::run(&format!(
        "CREATE PUBLICATION {pub_name} FOR TABLE {table_name}"
    )).map_err(|e| PgStreamError::SpiError(e.to_string()))?;

    // Note: slot creation must happen OUTSIDE the refresh transaction.
    // The scheduler creates the slot between refresh cycles.
    // Store intent in catalog; the scheduler will create the slot.
    update_trigger_replay_state(pgs_id, TriggerReplayState::Pending, Some(&slot_name))?;

    Ok(())
}
```

The replication slot is created by the scheduler between refresh cycles (not
inside a write transaction), bypassing the atomicity constraint that motivated
the original trigger-vs-replication decision.

#### 1.3 Catalog extension

Add columns to `pgstream.pgs_stream_tables`:

```sql
ALTER TABLE pgstream.pgs_stream_tables
  ADD COLUMN trigger_replay_state TEXT DEFAULT NULL
    CHECK (trigger_replay_state IN ('PENDING', 'ACTIVE', 'DISABLED')),
  ADD COLUMN trigger_replay_slot TEXT,
  ADD COLUMN trigger_replay_confirmed_lsn PG_LSN;
```

- `NULL` — No user triggers; replay not needed
- `PENDING` — User triggers detected; slot creation pending
- `ACTIVE` — Slot created; replay worker active
- `DISABLED` — Replay explicitly disabled by user (GUC)

#### 1.4 REPLICA IDENTITY on ST

The ST storage table needs `REPLICA IDENTITY` so the WAL decoder can see OLD
values for UPDATE and DELETE. Since every ST has `__pgs_row_id` as a unique
index, set:

```sql
ALTER TABLE <schema>.<st_name>
  REPLICA IDENTITY USING INDEX <st_name>___pgs_row_id_idx;
```

This provides the OLD `__pgs_row_id` in UPDATE/DELETE records, which is
sufficient to look up the old row values. For full OLD-value support in
triggers, use `REPLICA IDENTITY FULL`:

```sql
ALTER TABLE <schema>.<st_name> REPLICA IDENTITY FULL;
```

**Recommendation:** Use `REPLICA IDENTITY FULL` on STs with user triggers.
Stream tables are typically small (materialized aggregates), so the extra WAL
cost is negligible. This gives triggers complete `OLD` records.

#### 1.5 Files modified

| File | Changes |
|---|---|
| `src/cdc.rs` | Add `has_user_triggers()`, `setup_trigger_replay()`, `teardown_trigger_replay()` |
| `src/catalog.rs` | Add `TriggerReplayState` enum, CRUD for new columns on `pgs_stream_tables` |
| `src/lib.rs` | Add columns to `pgs_stream_tables` CREATE TABLE |
| `src/hooks.rs` | Detect `CREATE TRIGGER` / `DROP TRIGGER` on STs |

#### 1.6 Testing

- Unit test: `has_user_triggers()` detection
- E2E test: CREATE TRIGGER on ST → `trigger_replay_state` becomes `PENDING`
- E2E test: DROP last user trigger → `trigger_replay_state` becomes `NULL`

---

### Phase 2: Trigger Replay Worker (~1 week)

**Goal:** Background worker that reads ST WAL and fires user triggers.

This is the core of the plan. It reuses infrastructure from
[PLAN_HYBRID_CDC.md Phase 3](PLAN_HYBRID_CDC.md) (WAL decoder), adapted to
read from the ST's slot rather than a source table's slot.

#### 2.1 Replay worker main loop

New file: `src/trigger_replay.rs`

```rust
/// Main loop for the trigger replay worker.
///
/// Runs as a background worker (or as part of the scheduler's post-refresh
/// step). Polls the ST's logical replication slot for changes written by
/// the MERGE/TRUNCATE+INSERT, then fires user triggers for each change.
fn trigger_replay_main(pgs_id: i64, slot_name: &str) -> Result<(), PgStreamError> {
    loop {
        if should_terminate() {
            break;
        }

        let changes = poll_st_wal_changes(pgs_id, slot_name)?;

        if changes.is_empty() {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Fire triggers in a single transaction for consistency
        Spi::connect_mut(|client| {
            for change in &changes {
                fire_user_trigger(client, pgs_id, change)?;
            }
            Ok::<(), PgStreamError>(())
        })?;

        // Confirm LSN to advance the slot
        confirm_replay_lsn(pgs_id, changes.last().unwrap().lsn)?;
    }
    Ok(())
}
```

#### 2.2 WAL change decoding

The WAL decoder reads `pgoutput` protocol messages from the ST's slot:

```rust
#[derive(Debug)]
struct StChange {
    lsn: String,
    action: ChangeAction,
    /// All column values from the NEW tuple (INSERT/UPDATE).
    new_values: Option<HashMap<String, Option<String>>>,
    /// All column values from the OLD tuple (UPDATE/DELETE).
    /// Requires REPLICA IDENTITY FULL.
    old_values: Option<HashMap<String, Option<String>>>,
}

#[derive(Debug, PartialEq)]
enum ChangeAction {
    Insert,
    Update,
    Delete,
    Truncate,
}
```

The decoder strips internal columns (`__pgs_row_id`, `__pgs_count`) from the
values before passing them to the trigger. Users see only their declared
columns.

#### 2.3 Trigger firing mechanism

PostgreSQL does not provide a public API to "fire a trigger with custom OLD/NEW
values" from SQL. Two approaches:

**Option A: Direct PL/pgSQL EXECUTE (recommended)**

Construct and execute the trigger function directly, passing OLD/NEW as record
parameters:

```sql
-- For each user trigger on the ST:
SELECT <trigger_function>(
    TG_OP   := 'UPDATE',
    TG_NAME := 'audit_changes',
    TG_TABLE_NAME := 'regional_totals',
    OLD := ROW(old_region, old_total)::regional_totals,
    NEW := ROW(new_region, new_total)::regional_totals
);
```

This doesn't work directly — PostgreSQL trigger functions expect to be called
from the trigger machinery, not from SQL.

**Option B: Synthetic DML (recommended)**

Instead of calling trigger functions directly, execute synthetic DML that
*fires the triggers naturally*:

```sql
-- For INSERT: triggers fire naturally
INSERT INTO <st_table> (__pgs_row_id, col1, col2, ...)
  VALUES (row_id, val1, val2, ...);

-- For UPDATE: triggers fire with correct OLD/NEW
UPDATE <st_table>
  SET col1 = new_val1, col2 = new_val2, ...
  WHERE __pgs_row_id = <row_id>;

-- For DELETE: triggers fire with correct OLD
DELETE FROM <st_table>
  WHERE __pgs_row_id = <row_id>;
```

**Wait — this would modify the ST data!** We need to fire triggers without
actually changing the data (which the MERGE already changed correctly).

**Option C: Replay-then-rollback (not recommended)**

Execute synthetic DML in a subtransaction, let triggers fire, then rollback
the subtransaction. Problem: trigger side effects (writes to other tables)
would also be rolled back.

**Option D: Temporary trigger capture table + NOTIFY (pragmatic)**

Instead of trying to fire triggers with full OLD/NEW fidelity, write change
events to a dedicated replay table and emit NOTIFY:

```sql
CREATE TABLE pgstream.trigger_replay_<pgs_id> (
    replay_id    BIGSERIAL PRIMARY KEY,
    action       CHAR(1) NOT NULL,  -- I/U/D/T
    old_values   JSONB,
    new_values   JSONB,
    replayed_at  TIMESTAMPTZ DEFAULT now()
);
```

User triggers read from this table (or listen for NOTIFY). This is simpler
but doesn't give real trigger semantics.

**Option E: WAL-sourced apply worker (best approach)**

The replay worker applies changes to a **shadow copy** of the ST in a
separate transaction, with user triggers enabled on the shadow. But this
adds complexity and storage overhead.

**Option F: pg_trigger low-level API (actual recommendation)**

Use `pgrx` to call PostgreSQL's internal `ExecCallTriggerFunc()` or build
trigger event data structures directly. This is the most correct approach
but requires unsafe code and deep integration with PostgreSQL internals.

**Chosen approach: Option B with idempotent replay**

The replay worker does *not* re-execute DML on the ST. Instead, it:

1. Reads the WAL change records (which describe what the MERGE wrote)
2. For each change, constructs a row-typed `OLD`/`NEW` value
3. Calls each user trigger function via a specially constructed PL/pgSQL
   wrapper:

```sql
-- Installed once per ST that has user triggers:
CREATE OR REPLACE FUNCTION pgstream.__replay_trigger_<pgs_id>(
    p_action TEXT,
    p_old    <st_type>,
    p_new    <st_type>
) RETURNS void AS $$
DECLARE
    r_old <st_type> := p_old;
    r_new <st_type> := p_new;
    trig RECORD;
BEGIN
    FOR trig IN
        SELECT tgname, proname, nspname
        FROM pg_trigger t
        JOIN pg_proc p ON p.oid = t.tgfoid
        JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE t.tgrelid = <st_oid>::oid
          AND t.tgisinternal = false
          AND t.tgname NOT LIKE 'pgs_%'
          AND t.tgtype & 1 = 1  -- ROW trigger
          AND (
            (p_action = 'INSERT' AND t.tgtype & 4 = 4) OR
            (p_action = 'DELETE' AND t.tgtype & 8 = 8) OR
            (p_action = 'UPDATE' AND t.tgtype & 16 = 16)
          )
    LOOP
        -- Cannot directly call trigger functions from PL/pgSQL.
        -- Use the NOTIFY approach as the transport mechanism.
        PERFORM pg_notify(
            'pgstream_trigger_replay',
            json_build_object(
                'stream_table', TG_TABLE_NAME,
                'action', p_action,
                'old', row_to_json(r_old),
                'new', row_to_json(r_new)
            )::text
        );
    END LOOP;
END;
$$ LANGUAGE plpgsql;
```

**Reality check:** PL/pgSQL cannot call trigger functions directly because
trigger functions have a special calling convention (`RETURNS trigger`).
PostgreSQL's trigger machinery constructs a `TriggerData` struct that includes
`tg_event`, `tg_relation`, `tg_trigtuple` (OLD), `tg_newtuple` (NEW), etc.

The only way to fire a trigger function with correct OLD/NEW is to execute
actual DML that triggers it.

### Revised approach: Replay DML with change absorption

The replay worker executes real DML against the ST, structured so the net
effect is zero data change while triggers fire correctly:

#### For INSERT (new row from MERGE):
The row already exists (MERGE inserted it). The replay worker doesn't
need to insert again. Instead, it does:
```sql
-- Temporarily delete the row, then re-insert it with triggers enabled.
-- This fires DELETE + INSERT triggers.
-- OR: skip — the row was just inserted by MERGE. To fire INSERT triggers:

-- Save the row, delete it, re-insert it inside a single statement:
WITH deleted AS (
    DELETE FROM <st> WHERE __pgs_row_id = $1 RETURNING *
)
INSERT INTO <st> SELECT * FROM deleted;
-- INSERT trigger fires on the re-insert
```

This is convoluted. Let's step back and consider a cleaner approach.

### Final approach: Two-phase refresh

The cleanest way to fire user triggers is to make the refresh itself produce
the DML that fires them. Instead of suppressing triggers and replaying:

1. **Phase A (determine changes):** Run the delta query to figure out what
   changed, but don't apply yet.
2. **Phase B (apply with triggers):** Execute individual INSERT/UPDATE/DELETE
   statements with triggers enabled.

But this defeats the purpose of MERGE (single-pass efficiency).

### Actually chosen approach: Deferred trigger queue

After significant analysis, the most practical approach that delivers real
trigger semantics without extreme complexity:

1. During refresh, with triggers suppressed, capture the effective changes
   (INSERTs, UPDATEs, DELETEs) the MERGE produced.
2. After refresh commits, replay those changes as individual DML statements
   with triggers enabled — but using a **versioned idempotent pattern** that
   ensures the ST data doesn't change.

The mechanism:

```
Refresh (triggers suppressed):
  1. Snapshot pre-MERGE state of affected rows: SELECT INTO temp_old
  2. Execute MERGE (as today)
  3. Snapshot post-MERGE state of affected rows: SELECT INTO temp_new
  4. Diff temp_old vs temp_new → change_set (INSERT/UPDATE/DELETE)
  5. Write change_set to pgstream.st_changes_<pgs_id>

Post-refresh (triggers enabled):
  6. For each INSERT in change_set:
     → The row already exists. Delete + re-insert to fire triggers:
       DELETE FROM st WHERE __pgs_row_id = X;
       INSERT INTO st VALUES (...);
       -- INSERT trigger fires 🎉
  7. For each UPDATE in change_set:
     → UPDATE st SET col1=new_val1, ... WHERE __pgs_row_id = X;
     -- UPDATE trigger fires with correct OLD/NEW 🎉
  8. For each DELETE  in change_set:
     → The row is already gone. Skip.
     -- DELETE trigger already cannot fire (row doesn't exist).
     -- Use the captured OLD values to fire manually? No — we can't.
```

**Problem with DELETE:** Once the MERGE deletes a row, it's gone. We can't
fire a DELETE trigger because there's no row to delete.

**Problem with INSERT:** Delete + re-insert works but fires DELETE *and*
INSERT triggers — users expecting just INSERT get spurious DELETE.

### Definitive approach: Pre-MERGE snapshot + post-MERGE replay

After extensive analysis, here is the approach that provides the best
trade-off between correctness, complexity, and performance:

#### Core mechanism

1. **Before MERGE** (triggers suppressed): Capture the `__pgs_row_id` set
   of rows that will be affected.
2. **Execute MERGE** (as today, triggers suppressed).
3. **After MERGE, before commit**: Compare pre/post snapshots to identify
   actual INSERTs, UPDATEs, and DELETEs.
4. **Enable triggers** on the ST.
5. **Replay** the identified changes as real DML:
   - **INSERT:** Re-insert using `INSERT ... ON CONFLICT DO UPDATE` (no-op
     update, but fires the INSERT trigger). **No** — `ON CONFLICT DO UPDATE`
     fires an UPDATE trigger, not INSERT.
   
   Actually: the only correct way is to **let the MERGE fire triggers
   directly** but with a smarter suppression mechanism that only suppresses
   during the *speculative* phase and enables during the *confirmed* phase.
   
   PostgreSQL doesn't support this — triggers are all-or-nothing per statement.

### Practical conclusion + chosen approach

After thorough analysis, there is **no way** to fire PostgreSQL row-level
triggers with correct `OLD`/`NEW` semantics without executing actual DML
that produces those rows. Any "replay" approach either:

- Requires duplicating the DML (inefficient, complex)
- Fires wrong trigger types (INSERT instead of UPDATE)
- Cannot fire DELETE triggers (row already gone)
- Requires internal PostgreSQL C API access (unsafe, version-fragile)

**The practical solution has two tiers:**

#### Tier 1: Change event table + NOTIFY (Phase 2 deliverable)

For each refresh, write a structured change log to a per-ST table:

```sql
CREATE TABLE pgstream.st_replay_<pgs_id> (
    replay_id    BIGSERIAL PRIMARY KEY,
    refresh_id   BIGINT NOT NULL,
    action       TEXT NOT NULL CHECK (action IN ('INSERT', 'UPDATE', 'DELETE')),
    old_row      JSONB,  -- NULL for INSERT
    new_row      JSONB,  -- NULL for DELETE
    created_at   TIMESTAMPTZ DEFAULT now()
);
```

Plus a `NOTIFY pgstream_trigger_replay` with the change summary.

This gives users **all the data** they need to react to changes, in a way
that's queryable, subscribable, and reliable. It covers 90% of the use
cases that motivate user triggers (auditing, notifications, denormalization).

#### Tier 2: Trigger firing via controlled DML replay (Phase 3)

For users who need actual `RETURNS trigger` functions to fire with correct
OLD/NEW, execute controlled DML replay:

**For INSERTs:** Row already exists from MERGE. Skip trigger replay for
INSERTs — instead, mark them in the change event table. (Alternatively,
temporarily delete + re-insert, but this is fragile.)

**For UPDATEs:** Execute a no-op UPDATE:
```sql
UPDATE <st> SET <all_cols> = <same_values> WHERE __pgs_row_id = $1;
```
This fires the AFTER UPDATE trigger with correct OLD = NEW = same values?
No — the B-1 `IS DISTINCT FROM` guard in the MERGE already skips no-op
updates. We need an UPDATE where old ≠ new. Since we have the pre-MERGE
snapshot, we can:
1. Before MERGE: save old row values for affected `__pgs_row_id`s
2. After MERGE (with triggers still disabled): done
3. Enable triggers
4. Re-UPDATE each changed row to its final value (triggers fire with
   OLD = pre-MERGE value, NEW = post-MERGE value)

But wait — step 4 is a **real UPDATE** that writes to WAL/heap. The values
are already correct, so it's a no-op in terms of data, but PostgreSQL
doesn't know that. It writes a new tuple version.

This is the cost of real trigger support. For most STs (small, few trigger
events per refresh), the overhead is acceptable.

**For DELETEs:** Row is already deleted by MERGE. To fire DELETE triggers:
1. Before MERGE: save rows that will be deleted
2. After MERGE (triggers disabled): they're gone
3. Enable triggers
4. Re-INSERT the old rows temporarily
5. DELETE them (triggers fire with correct OLD)
6. Commit

This is complex and has edge cases (concurrent reads see temporarily
re-inserted rows). Wrap in a subtransaction to minimize visibility.

**Recommendation for Tier 2:** Support UPDATE trigger replay (covers the
most common case — aggregate changes). INSERT and DELETE trigger replay
is deferred to a later phase due to complexity.

---

## Revised Implementation Phases

### Phase 1: Trigger Suppression Foundation (~3 hours)

**Goal:** Implement PLAN_USER_TRIGGERS.md Phase 1 with the revised
suppression mechanism (`DISABLE TRIGGER USER` instead of
`session_replication_role`).

This is a prerequisite. See PLAN_USER_TRIGGERS.md for details, with the
modification described in [D-4](#d-4-disable-trigger-user-vs-session_replication_role).

#### 1.1 Changes to PLAN_USER_TRIGGERS.md Phase 1

Replace `SET LOCAL session_replication_role = 'replica'` with:

```rust
// Before MERGE / TRUNCATE+INSERT:
let quoted = format!(
    "\"{}\".\"{}\"",
    schema.replace('"', "\"\""),
    name.replace('"', "\"\""),
);
Spi::run(&format!("ALTER TABLE {quoted} DISABLE TRIGGER USER"))
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;

// ... MERGE / TRUNCATE+INSERT ...

Spi::run(&format!("ALTER TABLE {quoted} ENABLE TRIGGER USER"))
    .map_err(|e| PgStreamError::SpiError(e.to_string()))?;
```

Add crash recovery in `recover_from_crash()`: scan for STs where triggers
are stuck in the disabled state and re-enable them.

**Files changed:** `src/refresh.rs`, `src/scheduler.rs`, `src/config.rs`

---

### Phase 2: Change Event Table + NOTIFY (~3–5 days)

**Goal:** After each refresh, write a structured change log showing what
the refresh actually changed, and emit a NOTIFY.

This is the **primary deliverable** — it covers the vast majority of user
trigger use cases without the complexity of actual trigger firing.

#### 2.1 Pre/Post snapshot capture

Modify the refresh engine to capture changes:

```rust
// Before MERGE:
let affected_row_ids = get_affected_row_ids(dt, prev_frontier, new_frontier)?;
let pre_snapshot = if has_user_triggers && !affected_row_ids.is_empty() {
    Some(snapshot_rows(dt, &affected_row_ids)?)
} else {
    None
};

// Execute MERGE (as today, with triggers disabled)
let (merge_count, strategy) = execute_merge(...)?;

// After MERGE:
if let Some(pre) = pre_snapshot {
    let post_snapshot = snapshot_rows(dt, &affected_row_ids)?;
    let changes = diff_snapshots(&pre, &post_snapshot)?;
    write_change_events(dt.pgs_id, &changes)?;
    emit_replay_notify(dt, &changes)?;
}
```

#### 2.2 Snapshot helpers

```rust
/// IDs of rows that will be affected by this refresh's delta.
fn get_affected_row_ids(
    dt: &StreamTableMeta,
    prev_frontier: &Frontier,
    new_frontier: &Frontier,
) -> Result<Vec<i64>, PgStreamError> {
    // Run the delta query and extract __pgs_row_id values
    let delta_sql = get_or_build_delta_sql(dt, prev_frontier, new_frontier)?;
    Spi::connect(|client| {
        let rows = client.select(
            &format!("SELECT DISTINCT __pgs_row_id FROM ({delta_sql}) d"),
            None, &[],
        )?;
        let ids: Vec<i64> = rows.map(|r| r.get::<i64>(1).unwrap().unwrap()).collect();
        Ok(ids)
    })
}

/// Snapshot current row values for the given row IDs.
fn snapshot_rows(
    dt: &StreamTableMeta,
    row_ids: &[i64],
) -> Result<HashMap<i64, JsonValue>, PgStreamError> {
    let quoted = format!("\"{}\".\"{}\"", dt.pgs_schema, dt.pgs_name);
    let id_list = row_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
    Spi::connect(|client| {
        let rows = client.select(
            &format!(
                "SELECT __pgs_row_id, row_to_json(t.*) \
                 FROM {quoted} t \
                 WHERE __pgs_row_id IN ({id_list})"
            ),
            None, &[],
        )?;
        let mut map = HashMap::new();
        for row in rows {
            let id = row.get::<i64>(1)?.unwrap();
            let json = row.get::<JsonValue>(2)?.unwrap();
            map.insert(id, json);
        }
        Ok(map)
    })
}
```

#### 2.3 Diff computation

```rust
#[derive(Debug)]
struct ChangeEvent {
    action: String,         // "INSERT", "UPDATE", "DELETE"
    old_row: Option<JsonValue>,
    new_row: Option<JsonValue>,
}

fn diff_snapshots(
    pre: &HashMap<i64, JsonValue>,
    post: &HashMap<i64, JsonValue>,
) -> Vec<ChangeEvent> {
    let mut changes = Vec::new();

    // Rows in post but not in pre → INSERT
    for (id, new_val) in post {
        if !pre.contains_key(id) {
            changes.push(ChangeEvent {
                action: "INSERT".into(),
                old_row: None,
                new_row: Some(strip_internal_columns(new_val)),
            });
        }
    }

    // Rows in both → check for UPDATE
    for (id, old_val) in pre {
        if let Some(new_val) = post.get(id) {
            if old_val != new_val {
                changes.push(ChangeEvent {
                    action: "UPDATE".into(),
                    old_row: Some(strip_internal_columns(old_val)),
                    new_row: Some(strip_internal_columns(new_val)),
                });
            }
        }
    }

    // Rows in pre but not in post → DELETE
    for (id, old_val) in pre {
        if !post.contains_key(id) {
            changes.push(ChangeEvent {
                action: "DELETE".into(),
                old_row: Some(strip_internal_columns(old_val)),
                new_row: None,
            });
        }
    }

    changes
}
```

#### 2.4 Change event table

Per-ST table created when the first user trigger is detected:

```sql
CREATE TABLE pgstream.st_changes_<pgs_id> (
    event_id     BIGSERIAL PRIMARY KEY,
    refresh_seq  BIGINT NOT NULL,
    action       TEXT NOT NULL,
    old_row      JSONB,
    new_row      JSONB,
    created_at   TIMESTAMPTZ DEFAULT now()
);

-- Auto-cleanup: retain only last N refreshes
-- (controlled by GUC pg_stream.trigger_replay_retention)
```

#### 2.5 NOTIFY emission

```rust
fn emit_replay_notify(dt: &StreamTableMeta, changes: &[ChangeEvent]) -> Result<(), PgStreamError> {
    let summary = serde_json::json!({
        "stream_table": dt.pgs_name,
        "schema": dt.pgs_schema,
        "inserts": changes.iter().filter(|c| c.action == "INSERT").count(),
        "updates": changes.iter().filter(|c| c.action == "UPDATE").count(),
        "deletes": changes.iter().filter(|c| c.action == "DELETE").count(),
    });
    Spi::run(&format!(
        "NOTIFY pgstream_trigger_replay, '{}'",
        summary.to_string().replace('\'', "''"),
    ))?;
    Ok(())
}
```

#### 2.6 GUCs

| GUC | Type | Default | Description |
|---|---|---|---|
| `pg_stream.trigger_replay_enabled` | `bool` | `false` | Enable change event capture for STs with user triggers |
| `pg_stream.trigger_replay_retention` | `int` | `1000` | Max change events to retain per ST (older events auto-deleted) |

#### 2.7 FULL refresh handling

For FULL refresh, the snapshot approach works the same:

1. Pre-snapshot: `SELECT row_to_json(t.*) FROM st t` (all current rows)
2. TRUNCATE + INSERT
3. Post-snapshot: `SELECT row_to_json(t.*) FROM st t` (all new rows)
4. Diff: rows only in pre → DELETE, rows only in post → INSERT, rows in
   both with changed values → UPDATE, rows in both unchanged → skip

This gives correct INSERT/UPDATE/DELETE semantics even for FULL refresh,
at the cost of reading the entire ST twice. For typical ST sizes (aggregated
output, hundreds to low thousands of rows), this is negligible.

#### 2.8 Files created/modified

| File | Changes |
|---|---|
| `src/trigger_replay.rs` (NEW) | Snapshot capture, diff, change event writing, NOTIFY |
| `src/refresh.rs` | Inject pre/post snapshot around MERGE and FULL refresh |
| `src/config.rs` | Add `trigger_replay_enabled`, `trigger_replay_retention` GUCs |
| `src/lib.rs` | Add `mod trigger_replay;` |
| `src/scheduler.rs` | Periodic cleanup of old change events |

#### 2.9 Testing

| Test | Description |
|---|---|
| `test_change_events_insert` | Add source row → refresh → verify INSERT event in st_changes |
| `test_change_events_update` | Update source row → refresh → verify UPDATE event with old/new |
| `test_change_events_delete` | Delete source row → refresh → verify DELETE event with old |
| `test_change_events_full_refresh` | FULL refresh → verify correct INSERT/UPDATE/DELETE events |
| `test_change_events_no_op` | No-op refresh → verify no events written |
| `test_change_events_notify` | LISTEN → refresh → verify NOTIFY received |
| `test_change_events_retention` | Insert > retention limit events → verify oldest are pruned |
| `test_change_events_disabled` | GUC off → no events written |

---

### Phase 3: UPDATE Trigger Replay (~3–5 days)

**Goal:** For UPDATE changes, fire actual user triggers with correct
OLD/NEW values.

This phase is **optional** — Phase 2's change event table covers most use
cases. Phase 3 is for users who have existing `RETURNS trigger` functions
that they cannot rewrite.

#### 3.1 Mechanism

For each UPDATE change identified in Phase 2's diff:

1. After the MERGE (with triggers still disabled), the row has its
   post-MERGE value.
2. Temporarily write the pre-MERGE value back:
   ```sql
   UPDATE <st> SET col1=old_val1, col2=old_val2, ...
     WHERE __pgs_row_id = $1;
   ```
3. Enable user triggers.
4. Update to the post-MERGE value:
   ```sql
   UPDATE <st> SET col1=new_val1, col2=new_val2, ...
     WHERE __pgs_row_id = $1;
   ```
   This fires the AFTER UPDATE trigger with correct OLD (pre-MERGE) and
   NEW (post-MERGE).
5. Disable user triggers again for the next replay.

All of this happens **within the refresh transaction**, after the MERGE.

#### 3.2 Optimization: batch replay

For STs with few changes per refresh (typical for aggregate STs), the
per-row overhead is negligible. For larger change sets, batch the
pre-restore and post-replay UPDATEs:

```sql
-- Step 1: Restore old values (triggers disabled)
UPDATE <st> AS t
SET col1 = v.old_col1, col2 = v.old_col2, ...
FROM (VALUES
    (row_id_1, old_col1_1, old_col2_1, ...),
    (row_id_2, old_col1_2, old_col2_2, ...),
    ...
) AS v(__pgs_row_id, old_col1, old_col2, ...)
WHERE t.__pgs_row_id = v.__pgs_row_id;

-- Step 2: Enable triggers
ALTER TABLE <st> ENABLE TRIGGER USER;

-- Step 3: Apply new values (triggers fire!)
UPDATE <st> AS t
SET col1 = v.new_col1, col2 = v.new_col2, ...
FROM (VALUES
    (row_id_1, new_col1_1, new_col2_1, ...),
    (row_id_2, new_col1_2, new_col2_2, ...),
    ...
) AS v(__pgs_row_id, new_col1, new_col2, ...)
WHERE t.__pgs_row_id = v.__pgs_row_id;

-- Step 4: Re-disable triggers (for safety)
ALTER TABLE <st> DISABLE TRIGGER USER;
```

#### 3.3 INSERT trigger replay

For INSERT changes, the row was just created by the MERGE and doesn't have
a pre-existing OLD value. To fire an INSERT trigger:

1. After MERGE, the row exists (triggers disabled)
2. DELETE the row (triggers disabled)
3. ENABLE triggers
4. Re-INSERT the row → INSERT trigger fires with correct NEW values
5. DISABLE triggers

This is fragile: if the replay crashes between DELETE and INSERT, the row
is lost. Wrap in a subtransaction (SAVEPOINT) for safety.

#### 3.4 DELETE trigger replay

The MERGE already deleted the row. To fire a DELETE trigger:

1. After MERGE, row is gone (triggers disabled)
2. Re-INSERT the old row values (from pre-snapshot) — triggers disabled
3. ENABLE triggers
4. DELETE the re-inserted row → DELETE trigger fires with correct OLD
5. DISABLE triggers

Same fragility concern — use a SAVEPOINT.

#### 3.5 GUC control

```
pg_stream.trigger_replay_mode = 'events'    -- Phase 2 only (default)
pg_stream.trigger_replay_mode = 'update'     -- Phase 3: replay UPDATEs
pg_stream.trigger_replay_mode = 'full'       -- Phase 3: replay all DML
```

#### 3.6 Files modified

| File | Changes |
|---|---|
| `src/trigger_replay.rs` | Add DML replay functions for UPDATE, INSERT, DELETE |
| `src/refresh.rs` | Call trigger replay after MERGE in the same transaction |
| `src/config.rs` | Add `trigger_replay_mode` GUC |

#### 3.7 Testing

| Test | Description |
|---|---|
| `test_trigger_replay_update` | Pre-existing row changes → AFTER UPDATE trigger fires with correct OLD/NEW |
| `test_trigger_replay_insert` | New row → AFTER INSERT trigger fires |
| `test_trigger_replay_delete` | Row removed → AFTER DELETE trigger fires |
| `test_trigger_replay_crash_recovery` | Simulate crash mid-replay → verify SAVEPOINT rollback preserves data |
| `test_trigger_replay_multiple_triggers` | Multiple triggers on same ST |

---

### Phase 4: Full Refresh Trigger Replay (~2 days)

**Goal:** Fire triggers for changes caused by FULL refresh.

Uses Phase 2's snapshot-diff approach:

1. Pre-snapshot all ST rows
2. TRUNCATE + INSERT (triggers disabled)
3. Post-snapshot all ST rows
4. Diff → change events
5. (Phase 3 mode) Replay UPDATEs/INSERTs/DELETEs

For statement-level AFTER TRUNCATE triggers: allow these to fire naturally
by excluding them from `DISABLE TRIGGER USER`:

```sql
-- Only disable ROW-level user triggers:
-- Unfortunately, DISABLE TRIGGER USER disables ALL user triggers.
-- No way to selectively disable only row-level triggers.
```

**Workaround:** After TRUNCATE + INSERT, if the ST had a TRUNCATE trigger:
```sql
-- Fire TRUNCATE trigger by executing a synthetic TRUNCATE on a
-- temporary table with the same triggers. This is a hack.
```

**Recommendation:** Document that statement-level TRUNCATE triggers fire
from the change event table (Phase 2), not as real PostgreSQL triggers.

#### 4.1 Files modified

| File | Changes |
|---|---|
| `src/trigger_replay.rs` | FULL refresh snapshot + diff |
| `src/refresh.rs` | Inject snapshot around TRUNCATE + INSERT |

---

## Performance Consequences of Tier 1 (Change Event Table)

The snapshot-diff mechanism adds overhead at three points in each refresh
cycle. This cost is **only incurred when `trigger_replay_enabled = true` AND
the ST has user-defined triggers**. STs without user triggers pay zero cost.

### P-1: Delta query evaluated twice

`get_affected_row_ids()` executes the delta SQL to extract `__pgs_row_id`
values, then the MERGE re-executes the same delta SQL internally. For a delta
query that takes 5 ms today, this adds ~5 ms. This is the **dominant cost** —
the delta involves reading change buffers, joining source tables, and computing
hashes.

### P-2: Two snapshot reads of the stream table

Pre-MERGE and post-MERGE each execute:

```sql
SELECT __pgs_row_id, row_to_json(t.*)
FROM <st> t
WHERE __pgs_row_id IN (<affected_ids>)
```

This is an index scan per affected row plus JSON serialization. For a typical
aggregate ST where a refresh touches 5–50 groups, this is sub-millisecond. For
a wide ST with 1 000+ changed rows, expect 2–10 ms per snapshot.

### P-3: Change event writes

One `INSERT` per change into `pgstream.st_changes_<pgs_id>` with two JSONB
columns. For 10 change events: ~0.5 ms. Negligible.

### P-4: FULL refresh amplification

FULL refresh is the worst case. Both snapshots read the **entire** ST
(`SELECT row_to_json(t.*) FROM st`). A 100K-row ST means ~200K `row_to_json`
calls — potentially 50–200 ms of added overhead.

### Estimated overhead by scenario

| Scenario | Current refresh | With Tier 1 | Overhead |
|---|---|---|---|
| Aggregate ST, 5 groups changed | ~8 ms | ~14 ms | ~75% (P-1 dominates) |
| Join ST, 200 rows changed | ~15 ms | ~25 ms | ~65% |
| FULL refresh, 10K-row ST | ~50 ms | ~90 ms | ~80% (P-4, full scan ×2) |
| No user triggers on ST | unchanged | unchanged | **0%** (skipped entirely) |

### Mitigation: temp-table delta materialization

The double delta evaluation (P-1) can be eliminated by materializing the delta
into a temporary table and using it for both the row-ID extraction and as the
MERGE source:

```sql
CREATE TEMP TABLE __pgs_delta_<pgs_id> AS (<delta_sql>);
-- Use __pgs_delta_<pgs_id> for both snapshot IDs and MERGE USING clause
```

This adds temp-table creation/drop overhead (~2–3 ms) but removes the full
delta re-evaluation. Net win for deltas that take >5 ms to compute. Should be
added as an optimization when `trigger_replay_enabled = true`.

### Tier 2 (Phase 3) additional cost

Phase 3's DML replay adds further overhead on top of Tier 1:

| Operation | Cost per row | Notes |
|---|---|---|
| UPDATE replay (restore old + apply new) | ~0.1–0.3 ms | Two UPDATEs per changed row; fires trigger |
| INSERT replay (delete + re-insert) | ~0.1–0.2 ms | Wrapped in SAVEPOINT for crash safety |
| DELETE replay (insert old + delete) | ~0.1–0.2 ms | Wrapped in SAVEPOINT for crash safety |
| `DISABLE/ENABLE TRIGGER USER` per batch | ~0.5–1 ms | ACCESS EXCLUSIVE lock, taken twice |

For a typical aggregate ST with 5–10 changes per refresh, Phase 3 adds
~1–3 ms. For large change sets (1 000+ rows), Phase 3 could add 100–300 ms —
at that scale, Tier 1's change event table is the better choice.

> **Note:** These estimates are analytical. Empirical benchmarks should be
> added to the test suite once Phase 2 is implemented.

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Snapshot overhead slows refresh | Medium | Medium | Only snapshot when `trigger_replay_enabled = true` AND user triggers exist. Skip for STs with no triggers. |
| Large ST snapshot OOM | Low | High | Use cursors for large STs. Add GUC for max snapshot size. |
| `DISABLE TRIGGER USER` lock contention | Low | Medium | Lock is brief (DDL only, not MERGE). Monitor `pg_stat_activity` for waits. |
| Crash between DISABLE and ENABLE | Low | High | `recover_from_crash()` re-enables triggers on all STs. |
| Trigger replay causes cascade to source tables | Medium | High | Document clearly. CDC triggers are on source tables, so this creates a feedback loop. Add circuit breaker. |
| Double-fire on retry after crash | Low | Medium | Idempotent change events (dedup by `refresh_seq` + `__pgs_row_id`). |

---

## Interaction with Other Plans

### PLAN_USER_TRIGGERS.md

This plan **supersedes** PLAN_USER_TRIGGERS.md in the following ways:

| PLAN_USER_TRIGGERS.md | This plan |
|---|---|
| Phase 1: `session_replication_role` | Phase 1: `DISABLE TRIGGER USER` (supports WAL decoder) |
| Phase 2: NOTIFY after refresh | Phase 2: Change event table + NOTIFY (richer) |
| Phase 3: DDL warning | Unchanged — still needed |

PLAN_USER_TRIGGERS.md Phase 1 GUC (`pg_stream.suppress_user_triggers`)
becomes `pg_stream.trigger_replay_enabled` (when true, triggers are
suppressed during MERGE and changes are captured; when false, triggers
fire during MERGE as today — not recommended).

### PLAN_HYBRID_CDC.md

The two plans are **orthogonal** but share infrastructure concepts:

| Concern | PLAN_HYBRID_CDC.md | This plan |
|---|---|---|
| WAL decoder target | **Source** tables | **Stream** tables (Phase 1 only if WAL approach is used) |
| Publication | `pgstream_cdc_<source_oid>` | `pgstream_st_<pgs_id>` |
| Slot | `pgstream_<source_oid>` | `pgstream_st_<pgs_id>` |
| Purpose | Replace triggers with WAL for source CDC | Fire user triggers after refresh |

If PLAN_HYBRID_CDC.md is implemented first, the WAL decoder infrastructure
(slot management, `pgoutput` parsing) can be reused by this plan's Phase 1
(the WAL-based approach abandoned in favor of snapshot-diff in Phase 2).

**Recommended implementation order:**
1. PLAN_USER_TRIGGERS.md Phase 1 (trigger suppression)  — **~3 hours**
2. This plan Phase 2 (change event table + NOTIFY) — **~3–5 days**
3. PLAN_HYBRID_CDC.md (source-side WAL CDC) — **~3–5 weeks** (independent)
4. This plan Phase 3 (trigger replay DML) — **~3–5 days** (optional)

---

## Effort Estimate

| Phase | Effort | Priority | Dependency |
|---|---|---|---|
| Phase 1: Trigger suppression | ~3 hours | **High** | None |
| Phase 2: Change event table + NOTIFY | ~3–5 days | **High** | Phase 1 |
| Phase 3: DML trigger replay | ~3–5 days | Low | Phase 2 |
| Phase 4: FULL refresh replay | ~2 days | Low | Phase 2 |
| **Total** | **~2–3 weeks** | | |

---

## Open Questions

1. **Change event retention:** How long should change events be retained?
   Default 1000 events per ST, or time-based (e.g., 7 days)?

2. **Trigger cascade circuit breaker:** If a user trigger writes to a
   source table (creating a feedback loop), should we detect and block
   this? Or document it as user responsibility?

3. **Phase 3 transaction boundaries:** Should trigger replay happen in the
   same transaction as the MERGE (sees pre-commit state) or in a separate
   transaction (sees committed state)?

4. **BEFORE triggers:** This plan focuses on AFTER triggers. Should BEFORE
   triggers be supported? They could modify NEW values, which would
   conflict with the MERGE's intended output.

5. **TRUNCATE trigger support:** PostgreSQL TRUNCATE fires statement-level
   triggers, not row-level. Should we support AFTER TRUNCATE on STs?

---

## Commit Plan

1. `feat: use DISABLE TRIGGER USER for trigger suppression during refresh`
2. `feat: add trigger_replay_enabled GUC and has_user_triggers() detection`
3. `feat: capture pre/post MERGE snapshots for change event generation`
4. `feat: write change events to pgstream.st_changes_<pgs_id> table`
5. `feat: emit NOTIFY pgstream_trigger_replay after refresh`
6. `test: add E2E tests for change event capture and NOTIFY`
7. `docs: document user trigger support via change events`
8. `feat: UPDATE trigger replay via controlled DML` (Phase 3)
9. `feat: INSERT/DELETE trigger replay` (Phase 3)
10. `feat: FULL refresh change event capture` (Phase 4)
