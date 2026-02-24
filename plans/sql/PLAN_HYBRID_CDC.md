# PLAN: Hybrid CDC — Trigger Bootstrap → Logical Replication Steady-State

**Status:** Proposed  
**Date:** 2026-02-24  
**Origin:** [REPORT_TRIGGERS_VS_REPLICATION.md](REPORT_TRIGGERS_VS_REPLICATION.md) — Recommendation 6  
**Effort:** 3–5 weeks (6 phases, incrementally deliverable)

---

## Motivation

pg_stream currently uses row-level AFTER triggers exclusively for CDC. This
works well but imposes synchronous write-side overhead (~2–15 μs per row) on
every tracked source table. As analysed in the
[Triggers vs Replication Report §3.2](REPORT_TRIGGERS_VS_REPLICATION.md#32-steady-state-triggers-vs-logical-replication-honest-comparison),
logical replication eliminates this overhead entirely, captures TRUNCATE
natively, and scales to much higher write throughput.

The hybrid approach combines the best of both worlds:
- **Triggers for creation** — zero-config, atomic, no `wal_level` requirement
- **Logical replication for steady-state** — zero write overhead, TRUNCATE
  capture, higher throughput

The transition happens transparently after the first successful refresh. If
`wal_level != logical`, the system stays on triggers permanently — no
degradation, just the current behavior.

---

## Design Principles

1. **Backward compatible** — The extension must continue to work identically
   when `wal_level != logical`. Triggers remain the default and only CDC
   mechanism unless logical replication is available.

2. **Transparent transition** — Users do not need to take any action. The
   switch from triggers to WAL-based capture happens automatically in the
   background.

3. **Same buffer table schema** — The downstream pipeline (DVM, MERGE,
   frontier) must not change. Logical replication populates the same
   `pgstream_changes.changes_<oid>` buffer tables with the same column
   layout.

4. **Graceful fallback** — If slot creation fails, or if the WAL decoder
   encounters an error, the system falls back to trigger-based CDC
   automatically.

5. **No data loss during transition** — The trigger remains active until the
   WAL decoder has caught up past the trigger's last captured LSN. Only then
   is the trigger dropped.

---

## Prerequisites

Before starting implementation, these must be established:

- [ ] Benchmark trigger overhead ([TRIGGERS_OVERHEAD.md](../performance/TRIGGERS_OVERHEAD.md))
      to validate that the migration is worth the complexity
- [ ] Verify `pgoutput` provides sufficient column-level data for the buffer
      table schema (NEW/OLD values per column, action type, LSN)
- [ ] Confirm `REPLICA IDENTITY` requirements — determine whether `DEFAULT`
      (PK-based) is sufficient or if `FULL` is needed for UPDATE old values

---

## Architecture Overview

```
┌────────────────────────────────────────────────────────────┐
│                Stream Table Lifecycle                       │
│                                                            │
│  ┌─────────────┐     ┌──────────────┐     ┌────────────┐  │
│  │  TRIGGER     │────▸│  TRANSITIONING│────▸│    WAL      │  │
│  │  (bootstrap) │     │  (both active)│     │  (steady)   │  │
│  └─────────────┘     └──────────────┘     └────────────┘  │
│                                                            │
│  • create_stream_table()   • bg worker creates slot        │
│  • trigger + buffer table  • WAL decoder starts            │
│  • first FULL refresh      • trigger still active          │
│                            • decoder catches up            │
│                            • trigger dropped               │
│                            • cdc_mode → 'WAL'             │
└────────────────────────────────────────────────────────────┘
```

### CDC Mode State Machine

Each source dependency (`pgstream.pgs_dependencies`) tracks its CDC mode:

```
TRIGGER ──▸ TRANSITIONING ──▸ WAL
   ▲                           │
   └───────── (fallback) ──────┘
```

- **TRIGGER** — Row-level AFTER trigger writes to buffer table (current behavior)
- **TRANSITIONING** — Both trigger and WAL decoder are active; decoder is
  catching up. Buffer table may contain duplicate entries (same change captured
  by both trigger and decoder). The refresh engine deduplicates by `lsn` +
  `change_id`.
- **WAL** — Only the WAL decoder populates the buffer table. Trigger has been
  dropped.

---

## Implementation Phases

### Phase 1: Catalog Schema Extension (~2 days)

**Goal:** Add the metadata columns needed to track CDC mode per source.

#### 1.1 Extend `pgstream.pgs_dependencies`

Add columns to track per-source CDC mode and WAL decoder state:

```sql
ALTER TABLE pgstream.pgs_dependencies
  ADD COLUMN cdc_mode TEXT NOT NULL DEFAULT 'TRIGGER'
    CHECK (cdc_mode IN ('TRIGGER', 'TRANSITIONING', 'WAL')),
  ADD COLUMN slot_name TEXT,
  ADD COLUMN decoder_confirmed_lsn PG_LSN,
  ADD COLUMN transition_started_at TIMESTAMPTZ;
```

- `cdc_mode` — Current CDC mechanism for this source
- `slot_name` — Name of the replication slot (NULL when using triggers)
- `decoder_confirmed_lsn` — Last LSN confirmed by the WAL decoder
- `transition_started_at` — When the transition started (for timeout detection)

#### 1.2 Extend `StreamTableMeta` / `DtDependency` structs

Update [src/catalog.rs](../../src/catalog.rs):

```rust
pub struct DtDependency {
    pub pgs_id: i64,
    pub source_relid: pg_sys::Oid,
    pub source_type: String,
    pub columns_used: Option<Vec<String>>,
    // New fields:
    pub cdc_mode: CdcMode,
    pub slot_name: Option<String>,
    pub decoder_confirmed_lsn: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdcMode {
    Trigger,
    Transitioning,
    Wal,
}
```

#### 1.3 Add GUC: `pg_stream.cdc_mode`

Add to [src/config.rs](../../src/config.rs):

```rust
/// CDC mechanism selection.
/// - "trigger" (default): always use row-level triggers
/// - "auto": use triggers for creation, transition to WAL if available
/// - "wal": require WAL-based CDC (fail if wal_level != logical)
pub static PGS_CDC_MODE: GucSetting<Option<std::ffi::CString>> =
    GucSetting::<Option<std::ffi::CString>>::new(Some(c"trigger"));
```

The `"trigger"` default ensures zero behavior change for existing users.
Setting `"auto"` enables the hybrid approach.

#### 1.4 Add GUC: `pg_stream.wal_transition_timeout`

```rust
/// Maximum time (seconds) to wait for WAL decoder to catch up during
/// transition before falling back to triggers.
pub static PGS_WAL_TRANSITION_TIMEOUT: GucSetting<i32> =
    GucSetting::<i32>::new(300); // 5 minutes
```

#### 1.5 Files modified

| File | Changes |
|---|---|
| `src/lib.rs` | Add columns to `pgs_dependencies` CREATE TABLE |
| `src/catalog.rs` | Extend `DtDependency` struct, add `CdcMode` enum, CRUD for new columns |
| `src/config.rs` | Add `PGS_CDC_MODE` and `PGS_WAL_TRANSITION_TIMEOUT` GUCs |
| `src/error.rs` | Add `WalTransitionError` variant |

#### 1.6 Testing

- Unit test: `CdcMode` serialization/deserialization
- Unit test: `DtDependency` CRUD with new columns
- E2E test: Verify catalog migration adds columns without breaking existing STs

---

### Phase 2: WAL Availability Detection (~1 day)

**Goal:** Detect at runtime whether logical replication is available.

#### 2.1 `can_use_logical_replication()`

Add to [src/cdc.rs](../../src/cdc.rs):

```rust
/// Check if the server supports logical replication for CDC.
///
/// Returns true if ALL of:
/// - `wal_level` is 'logical'
/// - `max_replication_slots` > currently used slots
/// - `pg_stream.cdc_mode` is 'auto' or 'wal'
pub fn can_use_logical_replication() -> Result<bool, PgStreamError> {
    let cdc_mode = config::pg_stream_cdc_mode();
    if cdc_mode == "trigger" {
        return Ok(false);
    }

    let wal_level = Spi::get_one::<String>(
        "SELECT current_setting('wal_level')"
    )?.unwrap_or_default();

    if wal_level != "logical" {
        if cdc_mode == "wal" {
            return Err(PgStreamError::InvalidArgument(
                "pg_stream.cdc_mode = 'wal' requires wal_level = logical".into()
            ));
        }
        return Ok(false);
    }

    // Check available replication slots
    let available = Spi::get_one::<i64>(
        "SELECT current_setting('max_replication_slots')::bigint \
         - (SELECT count(*) FROM pg_replication_slots)"
    )?.unwrap_or(0);

    Ok(available > 0)
}
```

#### 2.2 `check_replica_identity(source_oid)`

```rust
/// Check if a source table has adequate REPLICA IDENTITY for logical decoding.
///
/// Returns true if REPLICA IDENTITY is DEFAULT (has PK) or FULL.
/// Returns false if NOTHING or if using an index that doesn't cover
/// all needed columns.
pub fn check_replica_identity(source_oid: pg_sys::Oid) -> Result<bool, PgStreamError> {
    let identity = Spi::get_one_with_args::<String>(
        "SELECT CASE relreplident \
           WHEN 'd' THEN 'default' \
           WHEN 'f' THEN 'full' \
           WHEN 'n' THEN 'nothing' \
           WHEN 'i' THEN 'index' \
         END FROM pg_class WHERE oid = $1",
        &[source_oid.into()],
    )?.unwrap_or_else(|| "nothing".into());

    // 'default' works if the table has a PK (which we already check)
    // 'full' always works
    // 'nothing' doesn't provide OLD values for UPDATE/DELETE
    // 'index' may work but needs further validation
    Ok(identity == "default" || identity == "full")
}
```

#### 2.3 Files modified

| File | Changes |
|---|---|
| `src/cdc.rs` | Add `can_use_logical_replication()`, `check_replica_identity()` |

#### 2.4 Testing

- Unit test: Mock wal_level detection
- E2E test: Verify returns false when `wal_level = replica`
- E2E test: Verify returns true when `wal_level = logical` and slots available

---

### Phase 3: WAL Decoder Background Worker (~1–2 weeks)

**Goal:** Implement a background worker that decodes WAL changes and writes
them into the existing buffer table schema.

This is the most complex phase.

#### 3.1 Register a WAL decoder worker

Add to [src/scheduler.rs](../../src/scheduler.rs) or a new `src/wal_decoder.rs`:

```rust
/// Register a WAL decoder background worker for a specific source table.
///
/// The worker creates a logical replication slot, starts streaming changes,
/// and writes decoded rows into pgstream_changes.changes_<oid> using the
/// same typed-column schema as the trigger-based CDC.
pub fn register_wal_decoder_worker(source_oid: u32, slot_name: &str) {
    BackgroundWorkerBuilder::new(&format!("pg_stream WAL decoder {}", source_oid))
        .set_function("pg_stream_wal_decoder_main")
        .set_library("pg_stream")
        .enable_spi_access()
        .set_start_time(BgWorkerStartTime::RecoveryFinished)
        .set_restart_time(Some(std::time::Duration::from_secs(10)))
        .load_dynamic(source_oid, slot_name);
}
```

#### 3.2 WAL decoder main loop

New file: `src/wal_decoder.rs`

The decoder worker:

1. **Connects** to the database via SPI
2. **Creates** a logical replication slot (if not already created):
   ```sql
   SELECT pg_create_logical_replication_slot(
       'pgstream_<source_oid>', 'pgoutput'
   )
   ```
3. **Configures** the slot for the specific source table:
   ```sql
   -- pgoutput protocol parameters
   publication_names: 'pgstream_cdc_<source_oid>'
   ```
4. **Starts streaming** via `pg_logical_slot_get_changes()` in a polling loop
5. **Decodes** each change and writes to the buffer table:
   - Maps `pgoutput` tuple data to typed `new_<col>` / `old_<col>` columns
   - Computes `pk_hash` using the same hash functions as the trigger
   - Sets `lsn` from the WAL record's LSN
   - Sets `action` to `'I'` / `'U'` / `'D'` / `'T'` (TRUNCATE)
6. **Confirms** the LSN position to advance the slot
7. **Updates** `decoder_confirmed_lsn` in `pgs_dependencies`
8. **Handles SIGTERM** gracefully

#### 3.3 Publication management

Each tracked source table needs a publication for the `pgoutput` plugin:

```sql
-- Created during transition setup
CREATE PUBLICATION pgstream_cdc_<source_oid> FOR TABLE <source_table>;

-- Dropped when the source is no longer tracked
DROP PUBLICATION IF EXISTS pgstream_cdc_<source_oid>;
```

Multiple STs that share the same source table share the same publication and
replication slot (reference-counted in `pgs_change_tracking`).

#### 3.4 TRUNCATE handling

When the decoder receives a TRUNCATE message:

```rust
// TRUNCATE captured natively from WAL
Action::Truncate => {
    // Option A: Write a special 'T' action row to the buffer
    // The refresh engine interprets 'T' as "all previous rows deleted"
    insert_truncate_marker(source_oid, lsn, change_schema)?;

    // Option B: Mark downstream STs for reinit (simpler)
    mark_downstream_for_reinit(source_oid)?;
}
```

Option B (mark for reinit) is simpler and matches the proposed TRUNCATE trigger
from [REPORT §4](REPORT_TRIGGERS_VS_REPLICATION.md#4-truncate-the-gap-and-how-to-close-it).
Option A is more elegant (preserves differential mode) but requires refresh
engine changes to handle the 'T' action.

**Recommendation:** Start with Option B, add Option A as an enhancement later.

#### 3.5 REPLICA IDENTITY auto-configuration

During transition setup, if the source table has `REPLICA IDENTITY DEFAULT`
and a primary key, no action is needed — `pgoutput` will include the PK in
UPDATE/DELETE records. If the source has no PK, set `REPLICA IDENTITY FULL`:

```sql
-- Only if source has no PK and replica identity is 'nothing'
ALTER TABLE <source_table> REPLICA IDENTITY FULL;
```

This must be done carefully — it changes the source table's WAL format, which
affects other subscribers. Add a GUC `pg_stream.auto_replica_identity` (default
`false`) to control whether pg_stream is allowed to auto-set this.

#### 3.6 Error mapping: WAL decode to buffer table

The key challenge is mapping `pgoutput` protocol messages to the typed buffer
table columns. `pgoutput` sends:

| Message | Fields |
|---|---|
| `INSERT` | Relation ID, new tuple (column name → value) |
| `UPDATE` | Relation ID, old tuple (PK columns only unless FULL), new tuple |
| `DELETE` | Relation ID, old tuple (PK columns only unless FULL) |
| `TRUNCATE` | List of relation IDs |

For each column in the tuple, the decoder must:
1. Map column name to `new_<col>` / `old_<col>` buffer column
2. Convert the text representation to the column's SQL type
3. Compute `pk_hash` from the PK columns

This mapping can reuse `resolve_source_column_defs()` and
`resolve_pk_columns()` from `cdc.rs`.

#### 3.7 Polling vs streaming

Two approaches for consuming WAL changes:

| Approach | How | Pros | Cons |
|---|---|---|---|
| **Polling** | `pg_logical_slot_get_changes()` in a loop | Simple SPI-based; no protocol-level code | Latency = poll interval; slot advances on each call |
| **Streaming** | `START_REPLICATION` protocol via `libpq` | Lower latency; standard replication protocol | Requires raw libpq connection, not SPI; complex |

**Recommendation for Phase 3:** Use the **polling** approach via SPI. It's
simpler, works within the existing background worker framework, and the poll
interval can match the scheduler interval (1s default). The streaming approach
can be an optimization in a future phase.

```rust
// Polling approach
fn poll_wal_changes(source_oid: u32, slot_name: &str, change_schema: &str)
    -> Result<(), PgStreamError>
{
    let changes = Spi::connect(|client| {
        client.select(
            &format!(
                "SELECT lsn, xid, data FROM pg_logical_slot_get_changes(\
                    '{slot_name}', NULL, NULL, \
                    'proto_version', '1', \
                    'publication_names', 'pgstream_cdc_{source_oid}'\
                )"
            ),
            None, &[],
        )
    })?;

    for change in changes {
        let lsn = change.get::<String>(1)?;
        let data = change.get::<String>(3)?;
        // Parse pgoutput data and write to buffer table
        decode_and_insert(source_oid, &lsn, &data, change_schema)?;
    }

    Ok(())
}
```

#### 3.8 Files created / modified

| File | Changes |
|---|---|
| `src/wal_decoder.rs` (NEW) | WAL decoder background worker, polling loop, decode logic |
| `src/cdc.rs` | Add `create_replication_slot()`, `drop_replication_slot()`, `create_cdc_publication()`, `drop_cdc_publication()` |
| `src/lib.rs` | Add `mod wal_decoder;` |
| `src/error.rs` | Add WAL-specific error variants |

#### 3.9 Testing

- Unit test: WAL message → buffer table row mapping
- Unit test: pk_hash computation matches trigger-based pk_hash
- E2E test: Create source table, enable WAL decoder, INSERT/UPDATE/DELETE,
  verify buffer table contents match trigger-based output
- E2E test: TRUNCATE on source with WAL decoder active
- E2E test: Decoder handles schema changes gracefully (warns, pauses)

---

### Phase 4: Transition Orchestration (~3–5 days)

**Goal:** Implement the transition from trigger to WAL-based CDC.

#### 4.1 Transition flow

The transition is orchestrated by the **scheduler** (not the API function).
This is important — the transition happens asynchronously after the stream
table has been successfully created and populated.

```
Scheduler tick:
  for each source dependency where cdc_mode = 'TRIGGER':
    if can_use_logical_replication() AND check_replica_identity(source_oid):
      start_wal_transition(source_oid, pgs_id)
```

#### 4.2 `start_wal_transition()`

```rust
fn start_wal_transition(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    let oid = source_oid.to_u32();
    let slot_name = format!("pgstream_{}", oid);

    // Step 1: Create publication for this source table
    create_cdc_publication(source_oid)?;

    // Step 2: Record the current WAL LSN — this is the "handoff point"
    // The trigger captures everything up to this point.
    // The WAL decoder starts from this point.
    let handoff_lsn = get_current_wal_lsn()?;

    // Step 3: Create the replication slot at the current position
    // This captures the slot's consistent point
    create_replication_slot(&slot_name)?;

    // Step 4: Update catalog
    update_cdc_mode(source_oid, CdcMode::Transitioning, Some(&slot_name))?;

    // Step 5: Start the WAL decoder worker
    // The decoder will start reading from the slot's confirmed_flush_lsn
    register_wal_decoder_worker(oid, &slot_name);

    info!(
        "pg_stream: started WAL transition for source OID {} (slot: {}, handoff LSN: {})",
        oid, slot_name, handoff_lsn
    );

    Ok(())
}
```

#### 4.3 Deduplication during transition

During the `TRANSITIONING` phase, both the trigger and the WAL decoder write
to the same buffer table. The same change may appear twice — once from the
trigger (synchronous, immediate) and once from the WAL decoder (async, slight
delay).

**Deduplication strategy:** The buffer table has `(lsn, pk_hash, change_id)`.
Two rows with the same `lsn` and `pk_hash` but different `change_id` values
are duplicates. The refresh engine's delta query already uses:

```sql
SELECT DISTINCT ON (__pgs_row_id) * FROM (delta_sql) __raw
ORDER BY __pgs_row_id, __pgs_action DESC
```

This deduplicates by row ID. However, the raw change buffer may still have
duplicates that inflate the delta. Add an explicit dedup step:

```sql
-- During TRANSITIONING, dedup buffer before refresh
DELETE FROM pgstream_changes.changes_<oid> a
USING pgstream_changes.changes_<oid> b
WHERE a.lsn = b.lsn
  AND a.pk_hash = b.pk_hash
  AND a.action = b.action
  AND a.change_id > b.change_id  -- keep the earlier one (trigger's)
```

This is only needed during the `TRANSITIONING` phase and can be skipped once
`cdc_mode = 'WAL'`.

Alternatively, add a `source` column to the buffer table (`'T'` for trigger,
`'W'` for WAL) and filter out WAL-sourced duplicates.

**Recommendation:** Add a `cdc_source CHAR(1)` column to the buffer table
(default `'T'`). During transition, deltas are read normally — the
`DISTINCT ON` in the refresh query handles duplicates. After transition
completes, remove trigger-sourced rows.

#### 4.4 `complete_wal_transition()`

Called by the scheduler when it detects the decoder has caught up:

```rust
fn complete_wal_transition(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    let oid = source_oid.to_u32();

    // Step 1: Verify decoder has caught up past the handoff LSN
    let decoder_lsn = get_decoder_confirmed_lsn(source_oid)?;
    let current_lsn = get_current_wal_lsn()?;

    // The decoder must be within a reasonable distance of current WAL
    if !lsn_is_close(decoder_lsn, current_lsn, MAX_LAG_BYTES) {
        // Not caught up yet — check timeout
        if transition_timed_out(source_oid)? {
            warning!(
                "pg_stream: WAL transition timed out for source OID {}; \
                 falling back to triggers",
                oid
            );
            abort_wal_transition(source_oid, change_schema)?;
        }
        return Ok(());  // Try again next scheduler tick
    }

    // Step 2: Drop the trigger (WAL decoder now covers all changes)
    drop_change_trigger(source_oid, change_schema)?;

    // Step 3: Update catalog to WAL mode
    update_cdc_mode(source_oid, CdcMode::Wal, None)?;

    info!(
        "pg_stream: completed WAL transition for source OID {} — trigger dropped",
        oid
    );

    Ok(())
}
```

#### 4.5 `abort_wal_transition()`

Fallback if transition fails or times out:

```rust
fn abort_wal_transition(
    source_oid: pg_sys::Oid,
    change_schema: &str,
) -> Result<(), PgStreamError> {
    let oid = source_oid.to_u32();
    let slot_name = format!("pgstream_{}", oid);

    // Step 1: Stop the WAL decoder worker
    stop_wal_decoder_worker(oid)?;

    // Step 2: Drop the replication slot
    drop_replication_slot(&slot_name)?;

    // Step 3: Drop the publication
    drop_cdc_publication(source_oid)?;

    // Step 4: Revert catalog to trigger mode
    update_cdc_mode(source_oid, CdcMode::Trigger, None)?;

    // Step 5: Verify trigger still exists (it should — we didn't drop it)
    if !trigger_exists(source_oid)? {
        // Trigger was somehow lost — recreate it
        let pk_columns = resolve_pk_columns(source_oid)?;
        let columns = resolve_source_column_defs(source_oid)?;
        create_change_trigger(source_oid, change_schema, &pk_columns, &columns)?;
    }

    warning!(
        "pg_stream: aborted WAL transition for source OID {}; using triggers",
        oid
    );

    Ok(())
}
```

#### 4.6 Scheduler integration

Modify the scheduler tick in [src/scheduler.rs](../../src/scheduler.rs):

```rust
// After Step A (DAG rebuild) and before Step B (refresh execution):
// Step A.5: Check and advance WAL transitions
if config::pg_stream_cdc_mode() != "trigger" {
    for dep in &all_dependencies {
        match dep.cdc_mode {
            CdcMode::Trigger => {
                // Check if we should start transition
                if can_use_logical_replication()?
                    && check_replica_identity(dep.source_relid)?
                {
                    start_wal_transition(dep.source_relid, &change_schema)?;
                }
            }
            CdcMode::Transitioning => {
                // Check if transition is complete or timed out
                complete_wal_transition(dep.source_relid, &change_schema)?;
            }
            CdcMode::Wal => {
                // Check decoder health
                check_decoder_health(dep.source_relid)?;
            }
        }
    }
}
```

#### 4.7 Files modified

| File | Changes |
|---|---|
| `src/scheduler.rs` | Add transition orchestration to scheduler tick |
| `src/cdc.rs` | Add `start_wal_transition()`, `complete_wal_transition()`, `abort_wal_transition()`, publication/slot management |
| `src/catalog.rs` | Add `update_cdc_mode()`, `get_decoder_confirmed_lsn()` |

#### 4.8 Testing

- E2E test: Full lifecycle — create ST with triggers, transition to WAL, verify
  continued correctness
- E2E test: Transition timeout → fallback to triggers
- E2E test: WAL decoder error during transition → clean fallback
- E2E test: Multiple STs sharing the same source during transition
- E2E test: `ALTER TABLE` on source during transition
- E2E test: `DROP STREAM TABLE` during transition (cleanup)

---

### Phase 5: Drop & Alter Integration (~2 days)

**Goal:** Ensure `drop_stream_table()`, `alter_stream_table()`, and DDL event
hooks handle WAL-based CDC correctly.

#### 5.1 `drop_stream_table()` changes

When dropping a stream table whose sources use WAL-based CDC:

1. Check if any other STs share the same source
2. If this was the last ST using the source → stop decoder, drop slot, drop
   publication
3. If other STs share the source → just remove the dependency, leave decoder
   running

Update `pgs_change_tracking.tracked_by_pgs_ids` to remove the ST's `pgs_id`.

#### 5.2 DDL event hook changes (`hooks.rs`)

When `ALTER TABLE` is detected on a source with WAL-based CDC:

1. Mark downstream STs for reinit (same as current behavior)
2. **Additionally:** The WAL decoder may encounter column-mismatch errors after
   the ALTER. The decoder should detect this and either:
   - Pause and wait for the schema change to propagate
   - Or restart with the new column mapping

The `pgoutput` plugin sends a `Relation` message when the schema changes, so
the decoder can detect this and re-read `resolve_source_column_defs()`.

#### 5.3 Files modified

| File | Changes |
|---|---|
| `src/api.rs` | Update `drop_stream_table_impl()` for WAL cleanup |
| `src/hooks.rs` | Update `handle_alter_table()` for WAL decoder notification |
| `src/wal_decoder.rs` | Add schema-change detection in decode loop |

---

### Phase 6: Monitoring & Observability (~2 days)

**Goal:** Expose CDC mode and decoder health in monitoring views.

#### 6.1 Extend `pgstream.pg_stat_stream_tables`

Add columns:

| Column | Type | Description |
|---|---|---|
| `cdc_mode` | `TEXT` | Current CDC mode per source (`TRIGGER` / `WAL` / `TRANSITIONING`) |
| `slot_name` | `TEXT` | Replication slot name (NULL for trigger mode) |
| `slot_lag_bytes` | `BIGINT` | Bytes of WAL behind for active decoder |
| `decoder_status` | `TEXT` | `ACTIVE` / `STOPPED` / `ERROR` |

#### 6.2 Health check function

```sql
SELECT pgstream.check_cdc_health();
```

Returns a table with per-source health status:
- Source OID, table name
- CDC mode
- Estimated lag (bytes or time)
- Last confirmed LSN
- Alert if slot lag exceeds threshold

#### 6.3 `NOTIFY` integration

Emit `NOTIFY pg_stream_cdc_transition` when a source transitions between
modes, including:
- Source table name
- Old mode → New mode
- Slot name (if applicable)

#### 6.4 Files modified

| File | Changes |
|---|---|
| `src/monitor.rs` | Extend monitoring views with CDC columns |
| `src/cdc.rs` | Add health check function |

---

## Slot Naming Convention

Replication slots are named predictably for easy identification:

```
pgstream_<source_oid>
```

Example: `pgstream_16384` for source table with OID 16384.

Publications follow the same pattern:

```
pgstream_cdc_<source_oid>
```

---

## Shared Slot Management

When multiple stream tables depend on the same source table, they should share
a single replication slot and publication. The existing
`pgstream.pgs_change_tracking` table already tracks this:

```sql
-- Existing table (from lib.rs)
CREATE TABLE pgstream.pgs_change_tracking (
    source_relid        OID PRIMARY KEY,
    slot_name           TEXT NOT NULL,
    last_consumed_lsn   PG_LSN,
    tracked_by_pgs_ids  BIGINT[]
);
```

This table serves as the reference count for shared slots:
- When the first ST targeting a source enables WAL → create slot + decoder
- When additional STs target the same source → add `pgs_id` to array
- When a ST is dropped → remove `pgs_id`; if array empty → drop slot + decoder

---

## Failure Modes & Recovery

| Failure | Detection | Recovery |
|---|---|---|
| Decoder crash | bg worker restart (5s) | Restart from `confirmed_flush_lsn`; no data loss |
| Slot dropped externally | `pg_replication_slots` check | Fall back to triggers; mark `cdc_mode = 'TRIGGER'` |
| WAL disk exhaustion | `pg_stat_replication_slots` lag | Alert via NOTIFY; consider emergency slot drop |
| Source table dropped | DDL event trigger | Stop decoder; drop slot; mark ST as ERROR |
| `wal_level` changed to `replica` | Startup check | Fall back to triggers for all sources |
| Schema change on source | Decoder `Relation` message | Pause decode; re-read column defs; resume |

### Crash Recovery

On scheduler startup (already has `recover_from_crash()`):

1. Scan `pgs_dependencies` for `cdc_mode = 'TRANSITIONING'`
2. If transition timed out → `abort_wal_transition()`
3. Scan `pgs_dependencies` for `cdc_mode = 'WAL'`
4. Verify replication slots exist in `pg_replication_slots`
5. If slot missing → fall back to triggers, recreate trigger

---

## Rollout Strategy

The hybrid approach is **opt-in** via the `pg_stream.cdc_mode` GUC:

| Setting | Behavior |
|---|---|
| `'trigger'` (default) | Current behavior. No change. |
| `'auto'` | Use triggers for creation. Transparently transition to WAL when available. Fall back to triggers if WAL is unavailable or fails. |
| `'wal'` | Require WAL-based CDC. Fail `create_stream_table()` if `wal_level != logical`. |

This allows:
- Existing users: zero change, zero risk
- Adventurous users: set `pg_stream.cdc_mode = 'auto'` and benefit from reduced
  write overhead
- Environments already using logical replication: set `'wal'` for guaranteed
  WAL-based CDC

---

## Milestone Delivery Order

| Phase | Deliverable | Can ship independently? |
|---|---|---|
| Phase 1 | Catalog extension + GUCs | ✅ Yes (no behavior change with default settings) |
| Phase 2 | WAL availability detection | ✅ Yes (informational only) |
| Phase 3 | WAL decoder worker | ❌ No (needs Phase 4 to be useful) |
| Phase 4 | Transition orchestration | ❌ No (needs Phase 3) |
| Phase 5 | Drop/Alter integration | ❌ No (needs Phases 3+4) |
| Phase 6 | Monitoring | ✅ Yes (can ship after Phase 4) |

**Recommended shipping order:** Phase 1 → Phase 2 → Phase 3+4 (together) → Phase 5 → Phase 6

Phases 1 and 2 can be merged into `main` independently as preparatory work.
Phases 3 and 4 should be developed on a feature branch and merged together.
Phases 5 and 6 are follow-up work.

---

## Estimated Effort

| Phase | Effort | Complexity |
|---|---|---|
| Phase 1: Catalog + GUCs | 2 days | Low |
| Phase 2: WAL detection | 1 day | Low |
| Phase 3: WAL decoder | 1–2 weeks | **High** (protocol handling, error recovery) |
| Phase 4: Transition | 3–5 days | Medium (state machine, dedup, timeout) |
| Phase 5: Drop/Alter | 2 days | Medium |
| Phase 6: Monitoring | 2 days | Low |
| **Total** | **3–5 weeks** | |

---

## Open Questions

1. **`pgoutput` vs custom output plugin** — `pgoutput` is built-in but outputs
   text representations. A custom output plugin could emit binary, but adds a
   deployment dependency. Start with `pgoutput`.

2. **Shared vs per-source decoder workers** — One background worker per source
   is simpler but may hit `max_worker_processes` limits with many sources. A
   single decoder multiplexing multiple slots would be more scalable but also
   more complex. Start with per-source; consolidate later if needed.

3. **Buffer table `cdc_source` column** — Adding a column to track whether a
   row came from a trigger or WAL decoder aids transition dedup but slightly
   widens the buffer table. Worth it for correctness during transition.

4. **REPLICA IDENTITY FULL auto-set** — Should pg_stream automatically set
   `REPLICA IDENTITY FULL` on source tables without PKs? This has side effects
   for other WAL consumers. Default to `false`; let users opt in.

5. **Polling interval for WAL decoder** — Match the scheduler interval (1s) or
   use a separate GUC? A separate GUC allows tuning independently.
