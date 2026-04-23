# PLAN_PGMQ.md — pg_trickle × PGMQ Integration

Date: 2026-04-22
Status: PROPOSED

---

## 1. Executive Summary

[PGMQ](https://github.com/pgmq/pgmq) is a lightweight, pure-SQL PostgreSQL
extension that implements a durable message queue with AWS-SQS-style semantics.
Queue tables (`pgmq.q_*`) and archive tables (`pgmq.a_*`) are ordinary
PostgreSQL heap tables. This means **pg_trickle can already consume them as
stream-table sources with zero code changes** — CDC triggers attach to heap
tables regardless of schema ownership.

However, PGMQ's access patterns create specific challenges (high-churn `vt` /
`read_ct` columns, visibility-timeout transience, queue tables that are
simultaneously read and written) and opportunities (event sourcing, queue
health dashboards, topic fan-out pipelines) that deserve deliberate design
choices. This document covers:

1. What PGMQ provides, how its tables are structured
2. How pg_trickle's CDC and DVM engine interact with those tables today
3. Integration patterns (read-only analytics, event sourcing, write-back,
   topic fan-out, dead-letter monitoring)
4. Concrete friction points and how to address them — some via SQL workarounds
   today, some via small, targeted code additions
5. A phased action plan

---

## 2. PGMQ Architecture Overview

### 2.1 Key Tables

| Table | Naming Pattern | Description |
|-------|---------------|-------------|
| Queue table | `pgmq.q_<name>` | Live messages; rows are inserted on `send()`, updated on `read()` (vt/read_ct bump), deleted or moved on `delete()`/`archive()` |
| Archive table | `pgmq.a_<name>` | Permanently retained messages after `archive()`; append-only in practice |
| Metadata | `pgmq.meta` | One row per queue — name, `is_partitioned`, `is_unlogged`, `created_at` |
| Notify throttle | `pgmq.notify_insert_throttle` | Tracks throttled NOTIFY payloads per queue |
| Topic bindings | `pgmq.topic_bindings` | Pattern → queue routing rules for pub-sub |

### 2.2 Queue Table Row Structure (`pgmq.message_record` type)

```sql
msg_id       BIGINT                    -- autoincrement, immutable
read_ct      INTEGER                   -- increments on every read() call
enqueued_at  TIMESTAMP WITH TIME ZONE  -- immutable, set on send()
last_read_at TIMESTAMP WITH TIME ZONE  -- updated on every read()
vt           TIMESTAMP WITH TIME ZONE  -- "visible after" timestamp; updated on read()
message      JSONB                     -- payload, immutable
headers      JSONB                     -- optional metadata/FIFO group key
```

### 2.3 Mutation Profile

This is the key characterisation for CDC planning:

| Operation | SQL DML | Columns touched |
|-----------|---------|-----------------|
| `send()` | INSERT | all |
| `read()` / `read_with_poll()` | UPDATE | `vt`, `read_ct`, `last_read_at` |
| `set_vt()` | UPDATE | `vt` |
| `delete()` | DELETE | — |
| `archive()` | DELETE (queue) + INSERT (archive) | — |
| `pop()` | DELETE | — |

The **dominant mutation type during normal operation is UPDATE** (visibility
timeout extension on each read). A busy queue may generate many UPDATE CDC
events that carry no useful business-logic change — the payload did not change,
only the in-flight status did.

### 2.4 PGMQ Notify

PGMQ optionally fires `PG_NOTIFY('pgmq.<table>.<op>', NULL)` via a constraint
trigger on inserts (throttled by `notify_insert_throttle`). pg_trickle's WAL
decoder already consumes the WAL stream; it does not need to LISTEN on
NOTIFY channels. However, NOTIFY can be used as a low-latency scheduling hint
(see §5.3).

---

## 3. How pg_trickle Interacts with PGMQ Tables Today

### 3.1 Trigger-Based CDC (default mode)

When a stream table references `pgmq.q_<name>`, pg_trickle installs a
row-level AFTER trigger on that table. Every INSERT, UPDATE, and DELETE is
captured into `pgtrickle_changes.changes_<oid>`. This works correctly with
no code changes.

**Concern:** `read()` calls generate UPDATE events on `vt`, `read_ct`, and
`last_read_at` for every message read. These updates are captured as CDC
events even if the stream table's query only cares about `msg_id`, `message`,
and `enqueued_at`. The change buffer accumulates "noise" rows that get
processed during the next differential refresh, only to produce zero net delta
after the DVM engine evaluates them against the query projection.

**Quantification:** A queue serving 1,000 reads/second generates ~1,000
UPDATE CDC rows per second in the change buffer. The DVM engine will compute
zero-delta correctly, but the buffer fills and drains at unnecessary cost.

### 3.2 WAL-Based CDC (transitioned mode)

After transition from trigger-based to WAL-based CDC, the WAL decoder reads
the replication slot. The volume problem remains — every `read()` still
generates a WAL record — but the write overhead on the source table is
eliminated (no trigger function overhead).

### 3.3 DVM Engine Processing

The DVM operator tree for a queue-sourced stream table evaluates the delta
correctly for any supported SQL. The issue is efficiency: without column-level
CDC filtering, every UPDATE (including vt-only bumps) drives a full delta
evaluation cycle for the affected `msg_id`.

---

## 4. Integration Patterns

### 4.1 Pattern A — Queue Health Dashboard (Read-Only Analytics)

**Goal:** Real-time metrics over queue state: depth, age, redelivery count.

```sql
-- Create a stream table over queue metrics
SELECT pgtrickle.create_stream_table(
    'queue_health',
    $$
    SELECT
        m.queue_name,
        COUNT(q.msg_id)                                  AS queue_depth,
        COUNT(q.msg_id) FILTER (WHERE q.vt <= NOW())     AS visible_depth,
        COUNT(q.msg_id) FILTER (WHERE q.read_ct >= 3)    AS retry_risk_count,
        MAX(EXTRACT(EPOCH FROM (NOW() - q.enqueued_at))) AS oldest_msg_age_sec,
        MIN(EXTRACT(EPOCH FROM (NOW() - q.enqueued_at))) AS newest_msg_age_sec
    FROM pgmq.meta m
    LEFT JOIN pgmq.q_events q ON true   -- replace with per-queue join
    GROUP BY m.queue_name
    $$,
    schedule => '10 seconds',
    refresh_mode => 'FULL'   -- aggregation over transient state, FULL is appropriate
);
```

**Notes:**
- Because queue depth changes continuously, `FULL` refresh mode is appropriate
  here (no stable primary-key anchor to diff against).
- A per-queue stream table allows `DIFFERENTIAL` refresh when projecting stable
  columns (e.g., archived messages analysis — see §4.3).

### 4.2 Pattern B — Event Log Stream Table (DIFFERENTIAL, archive table)

Archive tables (`pgmq.a_<name>`) are effectively append-only: rows are only
INSERTed by `archive()` and never updated or deleted in normal operation. This
makes them **ideal differential refresh sources** — the DVM delta is purely
additive.

```sql
SELECT pgtrickle.create_stream_table(
    'order_event_summary',
    $$
    SELECT
        (message ->> 'order_id')::bigint          AS order_id,
        (message ->> 'event_type')                AS event_type,
        COUNT(*)                                  AS event_count,
        MIN(enqueued_at)                          AS first_seen,
        MAX(archived_at)                          AS last_seen
    FROM pgmq.a_order_events
    GROUP BY
        (message ->> 'order_id')::bigint,
        (message ->> 'event_type')
    $$,
    schedule => '1 minute',
    refresh_mode => 'DIFFERENTIAL'
);
```

Archive tables are the primary recommended source for analytical stream tables
because:
1. No UPDATE churn — zero noise CDC events.
2. Purely monotonic INSERT stream — DBSP differential is maximally efficient.
3. The archive is the durable record; the queue is the transit vehicle.

### 4.3 Pattern C — In-Flight Message Monitoring (Selective DIFFERENTIAL)

Monitor messages currently "in-flight" (read but not yet deleted/archived):

```sql
SELECT pgtrickle.create_stream_table(
    'inflight_messages',
    $$
    SELECT
        msg_id,
        message,
        enqueued_at,
        read_ct,
        vt AS visible_after
    FROM pgmq.q_orders
    WHERE vt > NOW()   -- currently invisible = being processed
    $$,
    schedule => '5 seconds',
    refresh_mode => 'FULL'  -- vt changes constantly; FULL is pragmatic
);
```

**Why FULL here:** The `WHERE vt > NOW()` predicate is time-relative. A
`DIFFERENTIAL` refresh would need to re-evaluate every changed row against the
predicate, and `vt` changes on every `read()`. `FULL` refresh at a short
schedule (5–10 seconds) is lower overall cost for active queues.

### 4.4 Pattern D — Dead-Letter Queue Detector

```sql
SELECT pgtrickle.create_stream_table(
    'dead_letter_candidates',
    $$
    SELECT
        msg_id,
        message,
        enqueued_at,
        read_ct,
        last_read_at
    FROM pgmq.q_payments
    WHERE read_ct >= 5   -- threshold for "poison pill"
    $$,
    schedule => '1 minute',
    refresh_mode => 'DIFFERENTIAL'
);
```

`read_ct` only increases, so once a row crosses the threshold it stays in the
result — differential refresh handles this efficiently (only newly-qualifying
rows appear as deltas).

**Write-back to DLQ:** Combine with a trigger to push dead-letter candidates
into a dedicated PGMQ queue:

```sql
CREATE OR REPLACE FUNCTION pgtrickle_to_dlq()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pgmq.send(
        'dead_letter_payments',
        jsonb_build_object(
            'original_msg_id', NEW.msg_id,
            'payload',         NEW.message,
            'read_ct',         NEW.read_ct,
            'detected_at',     NOW()
        )
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER dead_letter_candidates_to_dlq
AFTER INSERT ON dead_letter_candidates
FOR EACH ROW EXECUTE FUNCTION pgtrickle_to_dlq();
```

### 4.5 Pattern E — Topic Fan-Out Pipeline

PGMQ's topic system (`pgmq.send_topic()`, `pgmq.topic_bindings`) routes
messages by wildcard pattern. pg_trickle stream tables can be layered over
the per-queue tables to compute per-topic aggregations:

```sql
-- Bind queues to topics
SELECT pgmq.bind_topic('orders.*', 'orders_us');
SELECT pgmq.bind_topic('orders.*', 'orders_eu');

-- Stream table aggregating across all order queues
SELECT pgtrickle.create_stream_table(
    'topic_order_volume',
    $$
    SELECT
        'orders_us'::text AS queue_name,
        COUNT(*)          AS message_count,
        MAX(enqueued_at)  AS latest_message
    FROM pgmq.q_orders_us
    UNION ALL
    SELECT
        'orders_eu'::text,
        COUNT(*),
        MAX(enqueued_at)
    FROM pgmq.q_orders_eu
    $$,
    schedule => '30 seconds',
    refresh_mode => 'FULL'
);
```

**Note:** UNION ALL in stream tables is supported. Each queue table is a
separate CDC source tracked independently.

### 4.6 Pattern F — Event Sourcing (PGMQ as Event Log, pg_trickle as Projection Engine)

The canonical event-sourcing pattern:

```
Application ──send()──▶ pgmq.q_domain_events
                               │
                        archive() on success
                               │
                               ▼
                    pgmq.a_domain_events  ──────────▶  Stream Tables
                    (immutable event log)               (current-state projections)
```

```sql
-- Current account balances derived from append-only event archive
SELECT pgtrickle.create_stream_table(
    'account_balances',
    $$
    SELECT
        (message ->> 'account_id')::bigint          AS account_id,
        SUM(CASE
            WHEN message ->> 'type' = 'credit'
            THEN (message ->> 'amount')::numeric
            ELSE -(message ->> 'amount')::numeric
        END)                                        AS balance,
        MAX(archived_at)                            AS as_of
    FROM pgmq.a_account_events
    GROUP BY (message ->> 'account_id')::bigint
    $$,
    schedule => '500ms',
    refresh_mode => 'DIFFERENTIAL'
);
```

This is the highest-value integration pattern. The archive table is append-only,
enabling the most efficient differential refresh. Balance adjustments are
precisely computed as deltas — no full scan required after initial population.

---

## 5. Friction Points and Mitigations

### 5.1 Problem: vt/read_ct Churn Pollutes Change Buffers

**Symptom:** A stream table over `pgmq.q_<name>` with a slow schedule
accumulates thousands of UPDATE CDC events for `vt` and `read_ct` column
changes. These events are all noise if the query only reads `msg_id`,
`message`, and `enqueued_at`.

**Workaround today (SQL-level):**
Partition the concern — use the archive table rather than the live queue table
wherever possible. For live-queue monitoring, accept FULL refresh mode.

**Medium-term mitigation (roadmap item):**
Add **column-level CDC filtering** to `pgtrickle.pgt_change_tracking`. If a
stream table's operator tree only references columns `{msg_id, message,
enqueued_at}`, the CDC trigger (or WAL decoder filter) should skip UPDATE
events where only `vt`, `read_ct`, `last_read_at` changed.

This requires:
1. Column-level lineage already tracked in `pgt_dependencies.columns_used`.
2. Extending the trigger function to check `TG_argv` column lists and exit
   early if only "ignored" columns changed.
3. WAL decoder: skip UPDATE records where the changed-column set is disjoint
   from the tracked set.

**Estimated impact:** For a queue doing 1,000 reads/sec with a stream table
refreshing every 10 seconds, column-level filtering would reduce CDC buffer
writes from ~10,000 rows per refresh cycle to near zero for archive-only
projections, and to only the INSERT/DELETE rows for payload-watching queries.

### 5.2 Problem: Unlogged Queue Tables and WAL-Based CDC

PGMQ supports `CREATE UNLOGGED TABLE` queues via `pgmq.create_unlogged()`.
Unlogged tables do not write WAL, so WAL-based CDC (logical replication)
cannot track them.

**Impact:** If a user calls `pgtrickle.create_stream_table(...)` over an
unlogged PGMQ queue, WAL mode cannot be used; the CDC system must remain in
trigger mode permanently.

**Mitigation:** pg_trickle should detect `relpersistence = 'u'` (unlogged)
during CDC registration and:
1. Block the WAL transition for that source table.
2. Log a `WARNING` explaining why.
3. Document this limitation in `docs/ERRORS.md` and `docs/FAQ.md`.

This is a small, targeted addition to `src/cdc.rs` where the WAL transition
eligibility check lives.

### 5.3 Problem: High-Frequency Refresh Scheduling for Near-Real-Time Queues

pg_trickle's scheduler uses cron or duration-based schedules. A busy queue
may need sub-second freshness for a monitoring stream table, but a 100ms
schedule wastes resources when the queue is idle.

**Opportunity:** PGMQ's `enable_notify_insert()` fires `PG_NOTIFY` on every
INSERT (throttled). pg_trickle's background worker could LISTEN on
`pgmq.<queue_table>.<op>` channels and treat an incoming NOTIFY as a
scheduling hint — triggering an immediate differential refresh rather than
waiting for the next scheduled window.

This would enable **event-driven refresh**: the stream table stays stale at
zero cost when the queue is empty and refreshes within milliseconds of new
messages being enqueued.

**Implementation sketch:**
- Add a `trigger_channel` parameter to `create_stream_table` (or a separate
  `pgtrickle.watch_notify_channel(st_name, channel)` function).
- The scheduler background worker registers a LISTEN on startup for all
  stream tables with a `trigger_channel` configured.
- On NOTIFY receipt, the stream table is added to the immediate-refresh queue
  ahead of its normal schedule.

This is a non-trivial addition but fits naturally into the existing scheduler
architecture.

### 5.4 Problem: Partitioned PGMQ Queues

PGMQ supports partitioned queues via `pg_partman`. Each partition is a child
table. pg_trickle's CDC registers on the parent (partitioned) table but
triggers fire on child partitions in PostgreSQL.

**Impact:** pg_trickle needs to ensure CDC triggers are created on each child
partition and that newly-created partitions automatically inherit the trigger.
This is handled by PostgreSQL's trigger inheritance for partitioned tables as
of PG 13+, so it should work correctly today — but this interaction should be
explicitly tested.

**Action:** Add an integration test `test_pgmq_partitioned_queue_cdc` that
creates a partitioned queue and verifies that a stream table tracking it
correctly captures changes to child partitions.

---

## 6. Required Code Changes

| Priority | Change | Location | Effort |
|----------|--------|----------|--------|
| Low (workaround available) | Column-level CDC filtering for UPDATE events | `src/cdc.rs`, WAL decoder | M |
| Low | Block WAL transition for unlogged tables + warning | `src/cdc.rs` | S |
| Medium | Event-driven refresh via NOTIFY hint | `src/scheduler.rs`, `src/api.rs` | L |
| Low | Partitioned queue CDC test | `tests/e2e_pgmq_tests.rs` (new) | S |
| Low | PGMQ section in `docs/integrations/` | Docs | S |

**S = ~1–4 hours, M = ~1–2 days, L = ~3–5 days**

No changes are required to the DVM engine or the SQL API for the core
read-only integration patterns.

---

## 7. No-Code-Change Integration (Available Today)

The following works with the current pg_trickle codebase and a standard PGMQ
installation:

```sql
-- 0. Prerequisites
CREATE EXTENSION pgmq;
CREATE EXTENSION pg_trickle;

-- 1. Create a queue
SELECT pgmq.create('order_events');

-- 2. Stream table over the archive (best pattern — append-only)
SELECT pgtrickle.create_stream_table(
    'order_event_counts',
    $$
    SELECT
        (message ->> 'customer_id')::bigint AS customer_id,
        COUNT(*)                            AS total_events,
        MAX(archived_at)                    AS latest_event
    FROM pgmq.a_order_events
    GROUP BY (message ->> 'customer_id')::bigint
    $$,
    schedule => '1 minute',
    refresh_mode => 'DIFFERENTIAL'
);

-- 3. Stream table over live queue for dead-letter detection
SELECT pgtrickle.create_stream_table(
    'poison_messages',
    $$
    SELECT msg_id, message, read_ct, enqueued_at
    FROM pgmq.q_order_events
    WHERE read_ct >= 5
    $$,
    schedule => '2 minutes',
    refresh_mode => 'DIFFERENTIAL'
);

-- 4. Check status
SELECT * FROM pgtrickle.pgt_status();
```

---

## 8. Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                          PostgreSQL 18                              │
│                                                                     │
│   Application                                                       │
│       │                                                             │
│       ▼  pgmq.send()                                                │
│  ┌──────────────────┐     archive()     ┌──────────────────────┐   │
│  │  pgmq.q_events   │ ────────────────▶ │  pgmq.a_events       │   │
│  │  (live queue)    │                   │  (archive — RO)      │   │
│  │  msg_id          │                   │  msg_id              │   │
│  │  vt (mutable)    │                   │  archived_at         │   │
│  │  read_ct (mutable│                   │  message             │   │
│  │  message         │                   └──────────┬───────────┘   │
│  └──────────┬────────┘                             │               │
│             │                                      │               │
│   CDC (triggers/WAL)                  CDC (triggers — INSERT only) │
│             │                                      │               │
│    ┌────────▼────────┐              ┌──────────────▼────────────┐  │
│    │ change buffer   │              │ change buffer             │  │
│    │ (q_events OID)  │              │ (a_events OID)            │  │
│    └────────┬────────┘              └──────────────┬────────────┘  │
│             │  ← high UPDATE churn                 │  ← pure INSERTs│
│             │  use FULL refresh                    │  use DIFF refresh│
│             ▼                                      ▼               │
│    ┌──────────────────────────────────────────────────────────┐    │
│    │                  DVM Engine / Refresh Engine             │    │
│    └──────────────────────────────────────────────────────────┘    │
│             │                                      │               │
│             ▼                                      ▼               │
│    inflight_messages             order_event_summary               │
│    dead_letter_candidates        account_balances                  │
│    queue_health                  topic_order_volume                │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 9. Recommended Usage Guidelines

1. **Prefer archive tables as stream table sources** when the goal is
   analytics or event-sourced projections. Archive tables are append-only and
   deliver the best differential refresh performance.

2. **Use FULL refresh mode for live-queue monitoring.** Live queue tables
   have high UPDATE churn on `vt`/`read_ct`. FULL refresh at a short schedule
   (5–30 seconds) is usually more efficient than DIFFERENTIAL over noisy CDC.

3. **Avoid monitoring `is_unlogged = TRUE` queues** with WAL-based CDC. Use
   trigger mode and expect permanent trigger overhead.

4. **Use DIFFERENTIAL refresh for dead-letter detection.** Queries with
   `WHERE read_ct >= N` qualify rows monotonically — differential refresh
   is maximally efficient.

5. **Set a `schedule` that matches queue throughput.** A queue processing
   10 messages/day does not need a 5-second refresh interval. Start at
   1 minute and reduce if freshness requirements demand it.

6. **Combine PGMQ archive tables with pg_trickle stream-on-stream DAGs**
   for multi-level event projections (e.g., raw events → hourly summaries →
   daily rollups).

---

## 10. Phased Action Plan

### Phase 0 — Documentation (No Code, ~1 day)

- [ ] Add `docs/integrations/pgmq.md` with the usage guidelines from §9,
      worked examples from §4, and the table from §3.2 explaining mutation
      profile.
- [ ] Update `docs/FAQ.md` with "Can I use pg_trickle with PGMQ?" entry.
- [ ] Update `docs/ERRORS.md` with unlogged-table CDC warning message.

### Phase 1 — Testing (Light Code, ~1 day)

- [ ] Add `tests/e2e_pgmq_tests.rs`:
  - `test_pgmq_archive_differential_refresh` — verify DIFF mode works on
    archive tables.
  - `test_pgmq_live_queue_full_refresh` — verify FULL mode on live queue.
  - `test_pgmq_dead_letter_detection` — verify `WHERE read_ct >= N` pattern.
  - `test_pgmq_partitioned_queue_cdc` — verify partitioned queue CDC.
  - `test_pgmq_unlogged_queue_wal_blocked` — verify graceful WAL-block warning.

### Phase 2 — Column-Level CDC Filtering (Medium Code, ~2 days)

- [ ] Extend `pgt_change_tracking` with a `tracked_columns` `text[]` column.
- [ ] Modify trigger function generator in `src/cdc.rs` to accept a column
      list and skip UPDATE events where changed columns are disjoint from
      tracked set.
- [ ] Populate `tracked_columns` from `pgt_dependencies.columns_used` during
      stream table creation.

### Phase 3 — Event-Driven Refresh via NOTIFY (Larger Code, ~4 days)

- [ ] Add `notify_channel text` parameter to `create_stream_table`.
- [ ] Persist `notify_channel` in `pgtrickle.pgt_stream_tables` catalog.
- [ ] Extend scheduler background worker to LISTEN on registered channels.
- [ ] On NOTIFY receipt, enqueue stream table for immediate refresh.

---

## 11. Related Documents

- [PLAN_ECO_SYSTEM.md](PLAN_ECO_SYSTEM.md) — Broader ecosystem roadmap
- [REPORT_TIMESCALEDB.md](REPORT_TIMESCALEDB.md) — Analogous integration
  report for TimescaleDB
- [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md) — pg_trickle CDC and DVM
  engine internals
- [docs/SQL_REFERENCE.md](../../docs/SQL_REFERENCE.md) — `create_stream_table`
  parameter reference
- [plans/PLAN.md](../PLAN.md) — Core feature roadmap

---

*Author: GitHub Copilot (AI-assisted research + design), April 2026*
