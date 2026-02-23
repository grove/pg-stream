# ADR: Row-Level Triggers Instead of Logical Replication for Change Data Capture

| Field         | Value                                                        |
|---------------|--------------------------------------------------------------|
| **Status**    | Accepted                                                     |
| **Date**      | 2025-02-15                                                   |
| **Deciders**  | pg_stream core team                                  |
| **Category**  | Architecture / Change Data Capture                           |

---

## Context

`pg_stream` is a PostgreSQL 18 extension (written in Rust via pgrx) that implements streaming tables with Incremental View Maintenance (IVM). A core requirement is **Change Data Capture (CDC)**: the system must detect and record every INSERT, UPDATE, and DELETE on source (base) tables so that downstream stream tables can be refreshed incrementally rather than recomputed from scratch.

The original design (PLAN.md Phase 3) specified **logical replication slots** with the `test_decoding` output plugin as the CDC mechanism. During implementation and live testing, a fundamental PostgreSQL limitation made this approach incompatible with the desired single-transaction API.

---

## Decision

**Use row-level PL/pgSQL triggers** (`AFTER INSERT OR UPDATE OR DELETE FOR EACH ROW`) to capture changes from source tables into per-table buffer tables (`pg_stream_changes.changes_<oid>`).

Replace all logical-replication-slot-based CDC code with trigger-based equivalents.

---

## Problem Statement

The `create_stream_table()` function must atomically perform these operations in a **single SQL transaction**:

1. Validate the defining query
2. Create the materialised storage table
3. Create catalog entries in `pg_stream.dt_catalog`, `pg_stream.pgs_dependencies`, `pg_stream.pgs_change_tracking`
4. Create change buffer tables
5. **Set up CDC** on each source table
6. Perform the initial full refresh
7. Update the catalog status to `ACTIVE`

With logical replication, step 5 requires calling `pg_create_logical_replication_slot()`. However, PostgreSQL enforces:

> **`pg_create_logical_replication_slot()` cannot execute inside a transaction that has already performed writes.**

By the time the CDC setup step is reached, the transaction has already performed DDL (creating tables) and DML (inserting catalog rows) — steps 1–4. This causes the slot creation to fail with:

```
ERROR: cannot create logical replication slot in transaction that has performed writes
```

Even restructuring the transaction to create slots *first* does not help, because step 1 (`validate_defining_query`) executes `SELECT * FROM (<query>) sub LIMIT 0` via SPI, which PostgreSQL's internal bookkeeping may classify as a write context depending on the query plan.

---

## Options Considered

### Option 1: Two-Phase API (Logical Replication)

Split `create_stream_table()` into two separate transactions:

```sql
-- Transaction 1: Create replication slot (no prior writes)
SELECT pg_stream.create_stream_table_prepare('order_totals', ...);

-- Transaction 2: Create catalog, storage table, initial refresh
SELECT pg_stream.create_stream_table_finalize('order_totals');
```

**Pros:**
- Uses PostgreSQL's native WAL-based change streaming
- Efficient for high-throughput workloads (batched WAL consumption)
- Automatically tracks schema evolution (column additions/renames/drops)

**Cons:**
- Breaks the single-function, single-transaction API contract
- Introduces a "dangling slot" problem: if `_finalize()` is never called, the slot retains WAL indefinitely, risking disk exhaustion
- Requires `wal_level = logical` in `postgresql.conf` (server restart needed)
- Idle slots still prevent WAL cleanup even when no changes are occurring
- Adds user-facing complexity (two calls instead of one)
- Partial failure between the two phases leaves the system in an inconsistent state requiring manual cleanup

### Option 2: Background Worker Slot Creation (Logical Replication)

Use a background worker process to create slots asynchronously after the main transaction commits.

**Pros:**
- Preserves a single user-facing API call
- Still uses native WAL-based CDC

**Cons:**
- Race condition: changes between transaction commit and slot creation are lost
- Significant implementation complexity (inter-process signalling, retry logic)
- The stream table is in a liminal state until the background worker completes
- Still requires `wal_level = logical`
- Error handling across process boundaries is fragile

### Option 3: Row-Level Triggers (Selected)

Create PL/pgSQL trigger functions that write change records directly to buffer tables on every INSERT, UPDATE, and DELETE.

**Pros:**
- Can be created in the same transaction as DDL/DML — preserves single-function atomic API
- No special `postgresql.conf` configuration required (`wal_level = logical` not needed)
- Simpler lifecycle: `CREATE TRIGGER` / `DROP TRIGGER`
- Changes visible immediately after commit (no separate consumption/polling step)
- No risk of WAL retention from idle slots
- Easy to reason about: standard PostgreSQL triggers with well-understood semantics

**Cons:**
- Per-row function call overhead on every DML statement affecting tracked tables
- Trigger functions must be manually updated if source table schema changes
- Does not work across database boundaries (unlike logical replication)
- Buffer tables consume additional disk space (mitigated by post-refresh cleanup)
- Not suitable for extremely high write rates (>5000 writes/sec per source table)

---

## Detailed Design

### Trigger Architecture

For each source table tracked by at least one stream table:

```
Source Table (e.g., orders)
    │
    ├── AFTER INSERT OR UPDATE OR DELETE trigger: pgdt_cdc_<source_oid>
    │       │
    │       └── Calls: pg_stream_changes.pgdt_cdc_fn_<source_oid>()
    │               │
    │               └── INSERTs into: pg_stream_changes.changes_<source_oid>
    │
    └── (normal query path unaffected)
```

### Trigger Function

Each source table gets a dedicated PL/pgSQL trigger function:

```sql
CREATE OR REPLACE FUNCTION pg_stream_changes.pgdt_cdc_fn_<oid>()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO pg_stream_changes.changes_<oid>
            (lsn, xid, action, row_data)
        VALUES (pg_current_wal_lsn(), pg_current_xact_id()::text::bigint, 'I',
                row_to_json(NEW)::jsonb);
        RETURN NEW;
    ELSIF TG_OP = 'UPDATE' THEN
        INSERT INTO pg_stream_changes.changes_<oid>
            (lsn, xid, action, row_data, old_row_data)
        VALUES (pg_current_wal_lsn(), pg_current_xact_id()::text::bigint, 'U',
                row_to_json(NEW)::jsonb, row_to_json(OLD)::jsonb);
        RETURN NEW;
    ELSIF TG_OP = 'DELETE' THEN
        INSERT INTO pg_stream_changes.changes_<oid>
            (lsn, xid, action, old_row_data)
        VALUES (pg_current_wal_lsn(), pg_current_xact_id()::text::bigint, 'D',
                row_to_json(OLD)::jsonb);
        RETURN OLD;
    END IF;
    RETURN NULL;
END;
$$;
```

### Change Buffer Table

One append-only buffer table per tracked source, in the `pg_stream_changes` schema:

```sql
CREATE TABLE pg_stream_changes.changes_<source_oid> (
    change_id    BIGSERIAL PRIMARY KEY,
    lsn          PG_LSN NOT NULL,
    xid          BIGINT NOT NULL,
    action       CHAR(1) NOT NULL,   -- 'I' (insert), 'U' (update), 'D' (delete)
    row_data     JSONB,              -- New row values (INSERT, UPDATE)
    old_row_data JSONB,              -- Old row values (UPDATE, DELETE)
    captured_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_changes_<source_oid>_lsn
    ON pg_stream_changes.changes_<source_oid> (lsn);
```

### Lifecycle

| Event | Action |
|-------|--------|
| `create_stream_table()` | Create buffer table + trigger function + trigger (all in one transaction) |
| Source table DML | Trigger fires, row written to buffer table |
| `refresh_stream_table()` | Read buffer table, apply deltas, delete consumed rows |
| `drop_stream_table()` | If no other STs reference the source: drop trigger, drop trigger function, drop buffer table |

### Change Metadata

Each change record captures:

- **`lsn`** — `pg_current_wal_lsn()` at trigger execution time, used as the upper frontier bound for incremental refresh
- **`xid`** — Current transaction ID, enables grouping changes by transaction
- **`action`** — Single character: `I` (insert), `U` (update), `D` (delete)
- **`row_data`** / **`old_row_data`** — Full row contents as JSONB via `row_to_json()`
- **`captured_at`** — Timestamp for monitoring and debugging

---

## Performance Characteristics

### Trigger Overhead

The per-row cost consists of:

1. PL/pgSQL function call dispatch (~2–5 μs)
2. `row_to_json()` serialisation (~5–20 μs depending on row width)
3. Buffer table INSERT (~10–30 μs including index maintenance)

**Estimated total overhead: 20–55 μs per row.**

For typical OLTP workloads (< 1000 writes/sec per source table), this adds < 5% latency to DML operations. The overhead becomes significant only at sustained rates above ~5000 writes/sec.

### Buffer Table Growth

Between refreshes, buffer tables accumulate change rows. For a source table with:
- 100 writes/sec
- 60-second refresh interval

This produces ~6000 buffer rows per interval — trivial for PostgreSQL. The `DELETE ... WHERE lsn <= $1` cleanup after each refresh keeps buffer tables bounded.

### Comparison with Logical Replication

| Aspect                    | Triggers (Current)            | Logical Replication           |
|---------------------------|-------------------------------|-------------------------------|
| **API complexity**        | Single function call          | Two-phase or async init       |
| **Per-row DML overhead**  | ~20–55 μs (trigger + INSERT)  | ~0 (changes recorded in WAL)  |
| **Change consumption**    | Direct table read             | Slot polling + WAL decode     |
| **High write volume**     | Degrades >5K writes/sec       | Scales with WAL throughput    |
| **Low write volume**      | Minimal overhead              | Slot retains WAL even if idle |
| **Schema changes**        | Triggers break on schema DDL  | Automatically tracked         |
| **Transaction safety**    | Same-transaction creation     | Requires separate transaction |
| **WAL disk usage**        | No extra retention            | Slot prevents WAL cleanup     |
| **Configuration**         | None required                 | `wal_level = logical` + restart |
| **Cross-database**        | Not supported                 | Supported via streaming       |

---

## Consequences

### Positive

1. **Atomic API**: `create_stream_table()` remains a single SQL function call that the user can wrap in their own transaction with full rollback semantics.
2. **Zero configuration**: The extension works out of the box without requiring changes to `postgresql.conf` or a server restart.
3. **Operational simplicity**: No replication slot management, no risk of WAL bloat from unconsumed slots, no need to monitor `pg_replication_slots`.
4. **Easier debugging**: Buffer tables are plain SQL tables that can be queried directly with `SELECT * FROM pg_stream_changes.changes_<oid>` to inspect pending changes.
5. **Broader compatibility**: Works on any PostgreSQL 18 installation regardless of `wal_level` setting, including managed PostgreSQL services that may restrict replication configuration.

### Negative

1. **Write amplification**: Every source-table DML now performs an additional INSERT into the buffer table, doubling the write I/O for tracked tables.
2. **Schema coupling**: If a source table's schema changes (e.g., `ALTER TABLE ADD COLUMN`), the trigger function continues to work (since `row_to_json()` serializes whatever columns exist), but the stream table's defining query may need updating. Logical replication would handle this more gracefully.
3. **Scalability ceiling**: At very high write rates (>5000 writes/sec per source table), the per-row trigger overhead may become a bottleneck. Logical replication's batched WAL consumption would be more efficient.
4. **No cross-database CDC**: Triggers only fire within the same database. Future cross-database stream tables would require logical replication.

### Neutral

1. **Buffer table disk usage**: Proportional to change rate × refresh interval. Automatically cleaned after each refresh.
2. **Monitoring changes**: The `pg_stream.dt_slot_health` view was updated to report trigger health instead of replication slot health. The monitoring abstraction remains the same.

---

## Migration Path

If future requirements demand logical replication (e.g., sustained >5000 writes/sec or cross-database sources), the migration involves:

1. **Expose a two-phase API** in `src/api.rs`:
   - `create_stream_table_prepare()` — creates catalog entries and storage table
   - `create_stream_table_finalize()` — creates replication slots (separate transaction) and performs initial refresh

2. **Reactivate `src/cdc.rs` replication functions** — the original `create_replication_slot()` / `drop_replication_slot()` / `parse_test_decoding_output()` functions can be restored from git history.

3. **Update `consume_slot_changes()`** — parse `test_decoding` output and write to the same buffer table format, keeping the downstream IVM pipeline unchanged.

4. **Add dangling-slot cleanup** — a background worker or `pg_cron` job to drop slots whose corresponding stream tables were never finalized.

5. **Require `wal_level = logical`** — update documentation and installation instructions.

The change buffer table schema and the downstream IVM refresh pipeline remain identical regardless of the CDC mechanism, so the migration is isolated to the CDC layer.

---

## Related

- [PLAN.md — Phase 3: Change Data Capture via Row-Level Triggers](../PLAN.md)
- [AGENT.md — CDC Architecture: Triggers vs. Logical Replication](../AGENT.md)
- [src/cdc.rs](../src/cdc.rs) — Implementation of trigger-based CDC
- [src/api.rs](../src/api.rs) — `create_stream_table()` / `drop_stream_table()` API
- PostgreSQL documentation: [CREATE TRIGGER](https://www.postgresql.org/docs/18/sql-createtrigger.html)
- PostgreSQL documentation: [Logical Replication](https://www.postgresql.org/docs/18/logical-replication.html)
- PostgreSQL source: `pg_create_logical_replication_slot()` restriction in `src/backend/replication/slotfuncs.c`
