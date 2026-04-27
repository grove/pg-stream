[← Back to Blog Index](README.md)

# The CDC Mode You Never Have to Choose

## How pg_trickle's hybrid change-data-capture starts with triggers and graduates to WAL

---

Every IVM system needs to know what changed. That's the CDC (change data capture) problem: given a source table, produce a stream of inserts, updates, and deletes.

PostgreSQL gives you two mechanisms: row-level triggers and logical replication (WAL decoding). Triggers are always available but add overhead to every DML statement. WAL-based CDC has near-zero write-side overhead but requires `wal_level = logical`, a replication slot, and a decoder plugin.

Most systems make you choose. pg_trickle doesn't. Its default CDC mode — `AUTO` — starts with triggers because they always work, then silently transitions to WAL-based capture when the prerequisites are met. If the WAL decoder fails, it falls back to triggers without losing a single change.

This post explains the three CDC modes, the transition orchestration, and why you should almost certainly leave the default alone.

---

## The Three Modes

### Trigger Mode (`cdc_mode => 'trigger'`)

pg_trickle installs row-level `AFTER INSERT OR UPDATE OR DELETE` triggers on each source table. When a row changes, the trigger writes a copy of the changed row (new values for INSERT/UPDATE, old values for DELETE) into a change buffer table in the `pgtrickle_changes` schema.

```
orders (source) → AFTER trigger → pgtrickle_changes.changes_<oid>
```

**Pros:**
- Works on any PostgreSQL installation, no configuration changes
- Works with foreign tables
- Works on replicas (if they're writable, e.g., Citus workers)
- Single-transaction atomicity — the change buffer write is in the same transaction as the source DML

**Cons:**
- Every INSERT/UPDATE/DELETE on the source table does extra work (the trigger fires, the buffer row is written)
- For high-throughput tables (>10,000 rows/second), the trigger overhead is measurable: roughly 10–15% additional CPU time per DML statement

### WAL Mode (`cdc_mode => 'wal'`)

pg_trickle creates a logical replication slot and a background WAL decoder worker that reads the write-ahead log and decodes changes into the same buffer tables.

```
orders (source) → WAL → pg_trickle WAL decoder → pgtrickle_changes.changes_<oid>
```

**Pros:**
- Near-zero write-side overhead — no trigger fires during DML
- Better throughput under high write load
- The WAL decoder is a single reader, not per-statement

**Cons:**
- Requires `wal_level = logical` (which means more WAL volume)
- Requires a replication slot (which holds WAL segments until consumed)
- If the decoder falls behind, WAL accumulates on disk
- Doesn't work with foreign tables

### Auto Mode (`cdc_mode => 'auto'`) — The Default

Auto mode starts with triggers. In the background, it checks whether WAL-based CDC is available. If it is, it transitions. If the transition fails or the WAL decoder crashes, it falls back to triggers.

This is the default, and for good reason: it means you don't have to think about CDC mode during initial setup. Install pg_trickle, create stream tables, and things work immediately with triggers. Later, when you tune `wal_level = logical` for other reasons (or because you want lower write overhead), pg_trickle picks it up automatically.

---

## The Transition: How It Actually Works

The trigger-to-WAL transition is the most delicate part of the CDC subsystem. The goal: switch from trigger-based capture to WAL-based capture without missing any changes and without double-counting.

It happens in three steps:

### Step 1: Slot Creation

pg_trickle creates a logical replication slot. The slot starts capturing WAL from the current LSN (log sequence number). At this point, both triggers *and* the slot are active — triggers handle current DML, and the slot starts accumulating WAL for future use.

### Step 2: Decoder Catch-Up

The WAL decoder worker starts reading from the slot. It needs to reach the current LSN — the point where it's caught up with the live write stream. pg_trickle waits until the decoder's consumed LSN is within a configurable threshold of the current WAL position.

This step has a timeout: `pg_trickle.wal_transition_timeout` (default 300 seconds). If the decoder can't catch up in 5 minutes — maybe because write throughput is extremely high — the transition is aborted, and triggers stay active.

### Step 3: Trigger Drop

Once the decoder is caught up, pg_trickle atomically:

1. Marks the source table's CDC mode as `wal` in the catalog.
2. Records the frontier LSN — the exact point where WAL takes over.
3. Drops the row-level trigger.

From this point forward, the WAL decoder handles all change capture. The change buffer tables are the same — only the writer changes.

---

## The Fallback: WAL → Triggers

If the WAL decoder crashes, falls too far behind, or the replication slot is dropped, pg_trickle detects the failure and falls back:

1. Re-installs the row-level trigger on the source table.
2. Marks the CDC mode as `trigger` in the catalog.
3. Logs a warning:

```
WARNING: pg_trickle WAL decoder for 'orders' failed (slot_lag_exceeded),
         falling back to trigger-based CDC
```

The fallback is safe because the frontier tracking is LSN-based. pg_trickle knows exactly which changes were captured by the WAL decoder and which weren't. The trigger picks up from the current transaction, and the next refresh processes the union of WAL-captured and trigger-captured changes.

No changes are lost. No changes are double-counted.

---

## Monitoring the CDC State

You can see the current CDC mode for every source table:

```sql
SELECT * FROM pgtrickle.pgt_cdc_status;
```

```
 source_table  | cdc_mode | slot_name          | slot_lag_bytes | trigger_active
---------------+----------+--------------------+----------------+----------------
 orders        | wal      | pgt_slot_orders    |          4096  | f
 customers     | trigger  | NULL               |          NULL  | t
 products      | wal      | pgt_slot_products  |         12288  | f
 inventory     | auto     | NULL               |          NULL  | t
```

Key columns:
- `cdc_mode`: The effective mode right now.
- `slot_lag_bytes`: How far behind the WAL decoder is. Non-zero is normal; growing continuously is a problem.
- `trigger_active`: Whether the row-level trigger is installed. In WAL mode, this is `false`.

---

## When to Override the Default

Almost never. But there are cases:

**Force trigger mode** when:
- You're using foreign tables as stream table sources (WAL doesn't capture foreign table changes)
- You need the single-transaction atomicity guarantee for IMMEDIATE mode (triggers fire in the same transaction; WAL decoding is async)
- You're on a managed PostgreSQL service that doesn't allow `wal_level = logical`

```sql
SELECT pgtrickle.create_stream_table(
  name     => 'inventory_levels',
  query    => $$ ... $$,
  schedule => '5s',
  cdc_mode => 'trigger'
);
```

**Force WAL mode** when:
- Write throughput is very high (>50,000 rows/second) and trigger overhead is measurable
- You want to minimize the CPU impact on the write path

```sql
SELECT pgtrickle.create_stream_table(
  name     => 'event_aggregates',
  query    => $$ ... $$,
  schedule => '2s',
  cdc_mode => 'wal'
);
```

If you force WAL mode and the prerequisites aren't met (`wal_level != logical`), pg_trickle will error at creation time:

```
ERROR: WAL-based CDC requires wal_level = logical
HINT: Set wal_level = logical in postgresql.conf and restart,
      or use cdc_mode => 'auto' to start with triggers
```

---

## The WAL Backpressure Safety Net

Since v0.36.0, pg_trickle enforces WAL backpressure when `pg_trickle.enforce_backpressure = on`. If the replication slot lag exceeds a critical threshold, CDC is paused to prevent unbounded WAL accumulation.

The sequence:

1. Slot lag exceeds `wal_backpressure_critical_bytes` (default 1GB).
2. pg_trickle pauses the WAL decoder and emits a WARNING.
3. When lag drops below `wal_backpressure_resume_bytes` (default 512MB), decoding resumes.

This hysteresis prevents the pathological case where a slow consumer causes WAL to pile up until the disk fills. It's the same pattern as TCP flow control — back off when the receiver can't keep up, resume when it catches up.

---

## Performance: Triggers vs. WAL

Benchmarks on a 4-core PostgreSQL 18 instance, 10,000 rows/second sustained write load:

| Metric | Trigger CDC | WAL CDC |
|--------|------------|---------|
| Write latency (p50) | 0.42ms | 0.38ms |
| Write latency (p99) | 1.8ms | 1.1ms |
| CPU overhead per DML | ~12% | ~1% |
| Change capture latency | 0ms (synchronous) | 2–15ms (async) |
| WAL volume increase | None | ~20% (logical decoding) |

The trade-off is clear: triggers cost more per write but have zero capture latency. WAL costs less per write but introduces a small delay between the DML commit and the change appearing in the buffer.

For IMMEDIATE mode (synchronous IVM), triggers are mandatory — the stream table must be updated in the same transaction. For DIFFERENTIAL mode with a schedule of 1 second or more, the 2–15ms capture latency is invisible.

---

## Summary

pg_trickle's CDC subsystem has three modes, but you only need to know one: AUTO. It starts with triggers because they always work. When WAL-based capture becomes available, it transitions automatically. If WAL fails, it falls back without losing changes.

The transition is orchestrated in three steps: slot creation, decoder catch-up, trigger drop. The fallback is safe because frontier tracking is LSN-based.

Override the default only when you have a specific reason: foreign tables, IMMEDIATE mode, or measured trigger overhead on a high-write table. For everything else, let pg_trickle figure it out.
