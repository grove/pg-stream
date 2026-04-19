# Transactional Inbox Helper & Dead-Letter Queue Extension

> **Status:** Implementation Plan (DESIGN REVIEW COMPLETE — 2026-04-19)
> **Created:** 2026-04-18
> **Category:** Integration Pattern — Implementation
> **Related:** [PLAN_TRANSACTIONAL_INBOX.md](PLAN_TRANSACTIONAL_INBOX.md) ·
> [PLAN_TRANSACTIONAL_OUTBOX_HELPER.md](PLAN_TRANSACTIONAL_OUTBOX_HELPER.md) ·
> [PLAN_TRANSACTIONAL_OUTBOX.md](PLAN_TRANSACTIONAL_OUTBOX.md)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Goals & Non-Goals](#goals--non-goals)
- [Background](#background)
- [Key Differences from the Outbox Helper](#key-differences-from-the-outbox-helper)
- [Part A — Inbox Helper (v0.24.0)](#part-a--inbox-helper-v0240)
  - [A.1 SQL API](#a1-sql-api)
  - [A.2 Catalog Schema](#a2-catalog-schema)
  - [A.3 Inbox Table Schema](#a3-inbox-table-schema)
  - [A.4 Auto-Created Stream Tables](#a4-auto-created-stream-tables)
  - [A.5 Poison Message Detection & DLQ Routing](#a5-poison-message-detection--dlq-routing)
  - [A.6 Adopt Existing Inbox (`enable_inbox_tracking`)](#a6-adopt-existing-inbox-enable_inbox_tracking)
  - [A.7 Retention & Garbage Collection](#a7-retention--garbage-collection)
  - [A.8 Replay Helper](#a8-replay-helper)
  - [A.9 Lifecycle & Drop Cascade](#a9-lifecycle--drop-cascade)
  - [A.10 GUCs](#a10-gucs)
  - [A.11 Failure Handling](#a11-failure-handling)
  - [A.12 Performance Considerations](#a12-performance-considerations)
  - [A.13 Migration & Upgrade](#a13-migration--upgrade)
- [Part B — Ordered Processing Extension (v0.24.0)](#part-b--ordered-processing-extension-v0240)
  - [B.1 Goals](#b1-goals)
  - [B.2 SQL API](#b2-sql-api)
  - [B.3 Aggregate-Ordered Stream Table](#b3-aggregate-ordered-stream-table)
  - [B.4 Gap Detection Stream Table](#b4-gap-detection-stream-table)
  - [B.5 Priority Queue Support](#b5-priority-queue-support)
  - [B.6 Competing Workers with Partition Affinity](#b6-competing-workers-with-partition-affinity)
- [Part C — Reference Implementations](#part-c--reference-implementations)
  - [C.1 Inbox Writer (NATS JetStream → PostgreSQL)](#c1-inbox-writer-nats-jetstream--postgresql)
  - [C.2 Inbox Processor (PostgreSQL → Business Logic)](#c2-inbox-processor-postgresql--business-logic)
  - [C.3 Webhook Receiver (HTTP → PostgreSQL)](#c3-webhook-receiver-http--postgresql)
- [Part D — Testing Strategy](#part-d--testing-strategy)
- [Part E — Documentation Plan](#part-e--documentation-plan)
- [Part F — Implementation Roadmap](#part-f--implementation-roadmap)
- [Part G — Open Questions](#part-g--open-questions)
- [Appendix — End-to-End Worked Example](#appendix--end-to-end-worked-example)

---

## Executive Summary

This plan specifies two complementary features for pg_trickle:

1. **Inbox Helper (v0.24.0):** A convenience layer that creates
   a best-practice inbox table with auto-managed stream tables for the
   pending-message queue, dead-letter queue, and processing statistics.
   It also supports *adopting* an existing hand-rolled inbox table into
   pg_trickle's monitoring infrastructure without schema changes.

2. **Ordered Processing Extension (v0.24.0):** An optional layer that adds
   per-aggregate ordered processing, sequence-gap detection, priority queues,
   and partition-affinity for competing workers.

Both features compose: Part A is shippable on its own and gives users an
immediate productivity win. Part B adds advanced processing semantics that
most teams don't need initially.

### Fundamental Difference from the Outbox Helper

The outbox helper *writes* to its outbox table automatically during
DIFFERENTIAL refresh — the user never touches the outbox table directly.

The inbox helper is the **opposite**: it *creates and configures* the inbox
table, stream tables, and monitoring, but the **application** (or an
external inbox-writer process) is responsible for INSERTing messages and
marking them as processed. pg_trickle does not own the inbox write path.
This is an important conceptual asymmetry:

| Concern | Outbox Helper | Inbox Helper |
|---------|--------------|--------------|
| Who writes to the table? | pg_trickle (during refresh) | Application / inbox-writer process |
| Who reads from the table? | External relay | Application / processor workers |
| pg_trickle's role | Produces the data | Monitors & organises the data |
| Atomicity guarantee | MERGE + outbox in same txn | Application must use `ON CONFLICT` for dedup |
| Stream table purpose | Optional (consumer offsets) | Core value (pending queue, DLQ, stats) |

---

## Goals & Non-Goals

### Goals

- **One function call** to set up a production-grade inbox pipeline with
  pending queue, dead-letter queue, statistics, and monitoring.
- **Adopt existing inboxes** without requiring table migration — map column
  names and gain pg_trickle monitoring automatically.
- **Automated DLQ routing** via stream tables that surface messages exceeding
  the retry limit, with `pg_trickle_alert` notifications.
- **Retention automation** for processed messages — bounded storage growth
  with configurable retention.
- **Replay helper** for disaster recovery and debugging — reset
  `processed_at` for selected messages to re-process them.
- **Structural parity** with the Outbox Helper — shared catalog conventions,
  GUC patterns, lifecycle rules, and documentation style.

### Non-Goals

- pg_trickle does **not** process inbox messages. Business logic remains in
  the application's processor workers.
- pg_trickle does **not** write to the inbox table. Message ingestion is the
  responsibility of the application or an inbox-writer process.
- pg_trickle does **not** own broker connections. Consuming from Kafka, NATS,
  RabbitMQ, etc. remains external.
- pg_trickle does **not** enforce message ordering in Part A — that is
  Part B of this release.
- The inbox helper does **not** replace pgmq, pgflow, or other purpose-built
  queue extensions. It is a thin orchestration layer for the common
  "table + stream table" pattern.

---

## Background

The Transactional Inbox Pattern (see
[PLAN_TRANSACTIONAL_INBOX.md](PLAN_TRANSACTIONAL_INBOX.md)) ensures reliable,
idempotent processing of inbound messages from external systems. The standard
implementation is straightforward but requires creating 3–5 objects (inbox
table, indexes, stream tables, monitoring queries) with consistent naming and
best-practice schema choices. This boilerplate is tedious and error-prone.

pg_trickle's stream tables are the natural materialisation layer for the
"pending messages" and "dead-letter" views of an inbox table. The inbox helper
automates this setup while leaving the application in full control of message
ingestion and processing.

---

## Key Differences from the Outbox Helper

Understanding the asymmetry is critical for correct implementation:

| Aspect | Outbox Helper | Inbox Helper |
|--------|--------------|--------------|
| **Table creation** | pg_trickle creates the outbox table with a fixed schema | pg_trickle creates inbox table (or adopts existing one) |
| **Data flow direction** | pg_trickle → outbox → relay → broker | broker → application → inbox → pg_trickle stream tables |
| **Refresh-path hook** | Yes — writes outbox row during DIFFERENTIAL refresh | No — inbox table is a plain source table for stream tables |
| **Deduplication** | Not needed (pg_trickle controls writes) | Required — `ON CONFLICT (event_id) DO NOTHING` in application |
| **Processing responsibility** | External relay polls outbox | Application workers poll pending stream table (or base table with `FOR UPDATE SKIP LOCKED`) |
| **Failure model** | Outbox INSERT failure → refresh rollback | Processing failure → increment `retry_count`, eventual DLQ |
| **Hot-path cost** | Additional INSERT per refresh | Zero pg_trickle overhead on the write path; standard stream table refresh cost |
| **Consumer offsets** | Needed when multiple relays share outbox | Not needed — each worker marks `processed_at` directly |

---

## Part A — Inbox Helper (v0.24.0)

### A.1 SQL API

```sql
-- Create a new inbox table with best-practice schema + stream tables
SELECT pgtrickle.create_inbox(
    inbox_name       TEXT,                      -- e.g. 'payment_inbox'
    schema           TEXT DEFAULT 'pgtrickle',  -- schema for the inbox table (default: pgtrickle)
    max_retries      INT DEFAULT 5,             -- messages with retry_count >= this go to DLQ
    schedule         INTERVAL DEFAULT '1s',     -- refresh interval for pending stream table
    with_dead_letter BOOLEAN DEFAULT true,      -- create DLQ stream table?
    with_stats       BOOLEAN DEFAULT true,      -- create stats stream table?
    retention_hours  INT DEFAULT NULL            -- override global GUC for processed retention
) RETURNS void;

-- Adopt an existing inbox table into pg_trickle monitoring
SELECT pgtrickle.enable_inbox_tracking(
    inbox_table_name       TEXT,                -- existing table, e.g. 'public.inbound_messages'
    id_column              TEXT DEFAULT 'event_id',
    processed_at_column    TEXT DEFAULT 'processed_at',
    retry_count_column     TEXT DEFAULT 'retry_count',
    error_column           TEXT DEFAULT 'error',
    received_at_column     TEXT DEFAULT 'received_at',
    event_type_column      TEXT DEFAULT 'event_type',
    max_retries            INT DEFAULT 5,
    schedule               INTERVAL DEFAULT '1s',
    with_dead_letter       BOOLEAN DEFAULT true,
    with_stats             BOOLEAN DEFAULT true
) RETURNS void;

-- Remove inbox + all associated stream tables
SELECT pgtrickle.drop_inbox(
    inbox_name TEXT,
    if_exists  BOOLEAN DEFAULT false,
    cascade    BOOLEAN DEFAULT false          -- also drop the inbox table itself?
) RETURNS void;

-- Inspect inbox status for one or all inboxes
SELECT * FROM pgtrickle.inbox_status(
    inbox_name TEXT DEFAULT NULL
);
-- Columns: inbox_name, inbox_table, pending_st, dlq_st, stats_st,
--          created_at, max_retries, retention_hours,
--          pending_count, dlq_count, processed_count,
--          oldest_pending_age_sec, total_bytes

-- Replay: reset messages for re-processing (by explicit event ID list only)
-- Note: a free-form where_clause parameter was considered and rejected due to
-- SQL injection risk. Replay by filter criteria should be done externally
-- using a parameterised SELECT to identify event IDs, then passing the list here.
SELECT pgtrickle.replay_inbox_messages(
    inbox_name TEXT,
    event_ids  TEXT[]             -- required: explicit list of event IDs to reset
) RETURNS INT;                   -- number of messages reset

-- Quick health check (returns JSONB summary)
SELECT pgtrickle.inbox_health(
    inbox_name TEXT
) RETURNS JSONB;
-- Returns:
-- {
--   "pending_count": 42,
--   "dead_letter_count": 3,
--   "avg_processing_time_sec": 0.12,
--   "oldest_pending_age_sec": 5.2,
--   "throughput_per_sec": 85.3,
--   "duplicate_rate_pct": 0.02,
--   "health_status": "healthy" | "degraded" | "critical"
-- }
```

#### Argument Validation

| Function | Validation |
|----------|------------|
| `create_inbox` | Name must not collide with existing table in `pgtrickle` schema; must not already be tracked. |
| `enable_inbox_tracking` | Table must exist; required columns must be present with compatible types; must not already be tracked. |
| `drop_inbox` | Must be tracked (or `if_exists := true`). `cascade := true` required to drop the inbox table itself. |
| `inbox_status` | Returns empty set if argument provided but no match. |
| `replay_inbox_messages` | `event_ids` must be non-NULL and non-empty. Uses a parameterised `WHERE event_id = ANY($1)` — no free-form SQL accepted; eliminates injection surface entirely. |
| `inbox_health` | Returns NULL if inbox not found. |

#### Errors

All errors use `PgTrickleError` variants:
- `InboxAlreadyExists { name }` (new variant)
- `InboxNotFound { name }` (new variant)
- `InboxTableNotFound { table }` (new variant, for `enable_inbox_tracking`)
- `InboxColumnMissing { table, column }` (new variant)

### A.2 Catalog Schema

A new metadata table tracks inbox configuration:

```sql
CREATE TABLE pgtrickle.pgt_inbox_config (
    inbox_id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    inbox_name             TEXT NOT NULL UNIQUE,
    inbox_table_schema     TEXT NOT NULL,           -- e.g. 'pgtrickle' or 'public'
    inbox_table_name       TEXT NOT NULL,           -- e.g. 'payment_inbox'
    is_managed             BOOLEAN NOT NULL,        -- true = created by create_inbox; false = adopted
    id_column              TEXT NOT NULL DEFAULT 'event_id',
    processed_at_column    TEXT NOT NULL DEFAULT 'processed_at',
    retry_count_column     TEXT NOT NULL DEFAULT 'retry_count',
    error_column           TEXT NOT NULL DEFAULT 'error',
    received_at_column     TEXT NOT NULL DEFAULT 'received_at',
    event_type_column      TEXT NOT NULL DEFAULT 'event_type',
    max_retries            INT NOT NULL DEFAULT 5,
    retention_hours        INT,                      -- NULL = use global GUC
    pending_st_name        TEXT,                     -- pgt_name of pending stream table
    dlq_st_name            TEXT,                     -- pgt_name of DLQ stream table (nullable)
    stats_st_name          TEXT,                     -- pgt_name of stats stream table (nullable)
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_drained_at        TIMESTAMPTZ,
    last_drained_count     BIGINT NOT NULL DEFAULT 0,
    UNIQUE (inbox_table_schema, inbox_table_name)
);
```

Key design choices:
- **Column mapping** (`id_column`, `processed_at_column`, etc.) enables
  `enable_inbox_tracking()` to work with arbitrary pre-existing inbox tables.
- **`is_managed`** distinguishes tables created by `create_inbox()` (which
  `drop_inbox(cascade := true)` may drop) from adopted tables (which
  `drop_inbox()` only removes stream tables and metadata for).
- **Stream table name references** allow `drop_inbox()` to clean up all
  associated stream tables without hardcoded naming conventions.

### A.3 Inbox Table Schema

`create_inbox('payment_inbox')` creates:

```sql
CREATE TABLE pgtrickle.payment_inbox (
    event_id       TEXT PRIMARY KEY,              -- globally unique; from sender
    event_type     TEXT NOT NULL,                 -- e.g. 'OrderCreated', 'PaymentReceived'
    source         TEXT,                          -- originating service/system
    aggregate_id   TEXT,                          -- entity this message belongs to (optional)
    payload        JSONB NOT NULL,                -- message body
    received_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at   TIMESTAMPTZ,                  -- NULL = not yet processed
    error          TEXT,                          -- last processing error
    retry_count    INT NOT NULL DEFAULT 0,
    trace_id       TEXT                           -- distributed tracing correlation (optional)
);

-- Partial index for pending messages (hot path for processors)
CREATE INDEX idx_<inbox>_pending
    ON pgtrickle.<inbox> (received_at)
    WHERE processed_at IS NULL;

-- Partial index for DLQ messages
CREATE INDEX idx_<inbox>_dlq
    ON pgtrickle.<inbox> (received_at)
    WHERE processed_at IS NULL AND retry_count >= <max_retries>;

-- Index for retention drain on processed messages
CREATE INDEX idx_<inbox>_processed
    ON pgtrickle.<inbox> (processed_at)
    WHERE processed_at IS NOT NULL;
```

#### Why These Columns?

| Column | Purpose |
|--------|---------|
| `event_id` | Primary key; dedup via `ON CONFLICT (event_id) DO NOTHING`. |
| `event_type` | Routing key for type-specific processors; GROUP BY in stats. |
| `source` | Origin tracking; useful for multi-source aggregation debugging. |
| `aggregate_id` | Entity key for ordered processing (Part B). Optional in Part A. |
| `payload` | The message body; JSONB for downstream flexibility. |
| `received_at` | Ingestion timestamp; used for ordering, staleness, and retention. |
| `processed_at` | Processing completion marker; NULL = pending. |
| `error` | Last error message; truncated to 1000 chars to prevent bloat. |
| `retry_count` | Tracks processing attempts; DLQ threshold = `max_retries`. |
| `trace_id` | OpenTelemetry / distributed tracing correlation. Optional. |

#### Naming Rules

- Inbox table name = `<inbox_name>` (user-chosen).
- Always lives in schema `pgtrickle` when created by `create_inbox()`.
- Adopted tables remain in their original schema.
- Stream table names: `pending_<inbox_name>`, `dlq_<inbox_name>`,
  `stats_<inbox_name>`.
- Truncated to fit PostgreSQL's 63-byte identifier limit; collision
  resolution appends `_<short_uuid>`.

### A.4 Auto-Created Stream Tables

`create_inbox()` (and `enable_inbox_tracking()`) creates up to three stream
tables, using the column mapping from `pgt_inbox_config`:

#### 1. Pending Queue Stream Table (always created)

```sql
SELECT pgtrickle.create_stream_table(
    'pending_<inbox_name>',
    format(
        $$SELECT %I AS event_id, %I AS event_type, payload,
                 %I AS received_at, %I AS retry_count
          FROM %I.%I
          WHERE %I IS NULL
            AND %I < %s$$,
        id_col, event_type_col, received_at_col, retry_count_col,
        schema, table_name,
        processed_at_col, retry_count_col, max_retries
    ),
    schedule => <schedule>,
    refresh_mode => 'DIFFERENTIAL'
);
```

This is the **primary value proposition** — a pre-filtered, always-fresh
materialised view of messages ready for processing.

#### 2. Dead-Letter Queue Stream Table (when `with_dead_letter = true`)

```sql
SELECT pgtrickle.create_stream_table(
    'dlq_<inbox_name>',
    format(
        $$SELECT %I AS event_id, %I AS event_type, payload,
                 %I AS error, %I AS retry_count, %I AS received_at
          FROM %I.%I
          WHERE %I IS NULL
            AND %I >= %s$$,
        id_col, event_type_col, error_col, retry_count_col, received_at_col,
        schema, table_name,
        processed_at_col, retry_count_col, max_retries
    ),
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
```

When new rows appear in the DLQ stream table, pg_trickle emits a
`pg_trickle_alert` event:

```json
{
  "type": "inbox_dlq_message",
  "inbox_name": "payment_inbox",
  "event_id": "evt-broken-001",
  "event_type": "PaymentReceived",
  "retry_count": 5,
  "error": "constraint violation: duplicate order_id"
}
```

**Implementation:** A post-refresh hook on the DLQ stream table checks
`rows_inserted > 0` and emits one alert per new DLQ entry. The alert
payload is bounded to 8 KB to stay within `pg_notify` limits.

#### 3. Statistics Stream Table (when `with_stats = true`)

> **Design note:** `max_pending_age_sec` has been removed from the materialised
> stats ST. It uses `now()` which changes on every refresh cycle, causing
> DIFFERENTIAL to emit a spurious delta for every row every 10 seconds. Moving
> `now()` out of the ST query allows DIFFERENTIAL mode and eliminates the O(N)
> full scan at every 10 s interval. Use `inbox_health()` for `oldest_pending_age_sec`
> on demand; wire its output to a Grafana/alerting integration via application code.

```sql
SELECT pgtrickle.create_stream_table(
    'stats_<inbox_name>',
    format(
        $$SELECT
            %I AS event_type,
            COUNT(*) FILTER (WHERE %I IS NULL AND %I < %s) AS pending,
            COUNT(*) FILTER (WHERE %I IS NOT NULL)          AS processed,
            COUNT(*) FILTER (WHERE %I IS NULL AND %I >= %s) AS dead_letter,
            AVG(EXTRACT(EPOCH FROM (%I - %I)))
                FILTER (WHERE %I IS NOT NULL)                AS avg_processing_time_sec
          FROM %I.%I
          GROUP BY %I$$,
        event_type_col,
        processed_at_col, retry_count_col, max_retries,
        processed_at_col,
        processed_at_col, retry_count_col, max_retries,
        processed_at_col, received_at_col, processed_at_col,
        schema, table_name,
        event_type_col
    ),
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL'  -- safe: no now() in query; only changes when
                                    -- message counts change between refresh cycles.
);
```

> **Migration note:** If your dashboards already use `max_pending_age_sec` from
> the stats ST, switch to `SELECT (inbox_health('...'))->>'oldest_pending_age_sec'`
> from your monitoring layer, or add a separate single-row stream table for it.

### A.5 Poison Message Detection & DLQ Routing

#### How Messages Reach the DLQ

Messages reach the DLQ **passively** — there is no active routing step.
When the application increments `retry_count` past `max_retries`, the
message disappears from `pending_<inbox>` (due to the `retry_count < max_retries`
filter) and appears in `dlq_<inbox>` on the next refresh.

This is a key simplicity benefit: the application doesn't need to know about
the DLQ at all. It just increments `retry_count` and records the error.
pg_trickle's stream tables do the rest.

#### Alert Mechanism

The DLQ alert fires via a **post-refresh callback** registered on the DLQ
stream table. Implementation:

1. After each DLQ stream table refresh, check `delta.inserted_count > 0`.
2. If yes, query the newly inserted rows (available from the diff buffer).
3. Emit one `pg_trickle_alert` per new DLQ entry (capped at 10 per refresh
   to avoid flooding the notification channel).
4. If more than 10 new DLQ entries in one refresh, emit a summary alert:
   ```json
   {
     "type": "inbox_dlq_batch",
     "inbox_name": "payment_inbox",
     "new_dlq_count": 47,
     "sample_event_ids": ["evt-1", "evt-2", "evt-3"]
   }
   ```

#### Processor-Side Circuit Breaker (Documented, Not Implemented)

pg_trickle documents but does **not** implement a processor-side circuit
breaker. This belongs in the application layer. The reference processor
(Part C.2) includes a `PoisonMessageGuard` example.

### A.6 Adopt Existing Inbox (`enable_inbox_tracking`)

Many users already have hand-rolled inbox tables. `enable_inbox_tracking()`
lets them gain pg_trickle monitoring without migrating:

```sql
-- Example: adopt an existing table with non-standard column names
SELECT pgtrickle.enable_inbox_tracking(
    inbox_table_name    => 'public.inbound_messages',
    id_column           => 'message_id',
    processed_at_column => 'completed_at',
    retry_count_column  => 'attempts',
    error_column        => 'last_failure',
    received_at_column  => 'created_at',
    event_type_column   => 'msg_type',
    max_retries         => 3,
    schedule            => '2s'
);
```

#### Validation Steps

1. Verify the table exists.
2. Verify each specified column exists and has a compatible type:
   - `id_column`: must be a text-like type (`TEXT`, `VARCHAR`, `UUID`).
   - `processed_at_column`: must be `TIMESTAMPTZ` or `TIMESTAMP`.
   - `retry_count_column`: must be an integer type.
   - `error_column`: must be a text-like type.
   - `received_at_column`: must be `TIMESTAMPTZ` or `TIMESTAMP`.
   - `event_type_column`: must be a text-like type.
3. Verify the table has a PRIMARY KEY or UNIQUE constraint on `id_column`.
4. Create stream tables using the mapped column names.
5. Insert metadata into `pgt_inbox_config` with `is_managed = false`.

#### What It Does **Not** Do

- Does not alter the existing table schema.
- Does not add indexes (user may already have appropriate ones).
- Does not create triggers or CDC infrastructure beyond what pg_trickle's
  normal stream table creation provides.

### A.7 Retention & Garbage Collection

#### Processed Message Retention

Inbox tables grow unboundedly unless processed messages are cleaned up.
The inbox helper adds a retention drain step to the scheduler:

```sql
-- PostgreSQL does not support LIMIT on DELETE directly; use a CTE.
WITH rows_to_delete AS (
    SELECT <id_column> FROM <schema>.<inbox_table>
    WHERE <processed_at_column> IS NOT NULL
      AND <processed_at_column> < now() - INTERVAL '<retention_hours> hours'
    ORDER BY <processed_at_column>
    LIMIT <drain_batch_size>
)
DELETE FROM <schema>.<inbox_table>
WHERE <id_column> IN (SELECT <id_column> FROM rows_to_delete);
```

- Runs once per cleanup cycle (controlled by
  `pg_trickle.inbox_drain_interval_seconds`).
- Batched to avoid long locks.
- Updates `pgt_inbox_config.last_drained_at` and `last_drained_count`.

#### DLQ Message Retention

DLQ messages (those with `retry_count >= max_retries` and
`processed_at IS NULL`) are **not** drained by default. They persist until:

1. An operator replays them (`replay_inbox_messages()`), or
2. An operator discards them manually
   (`UPDATE ... SET processed_at = now(), error = 'DISCARDED: ...'`), or
3. The operator explicitly sets
   `pg_trickle.inbox_dlq_retention_hours` (default `0` = keep forever) to
   auto-drain DLQ messages older than the threshold.

#### Edge Cases

- If retention is `0` hours, no drain runs (processed messages kept forever).
- Drain only touches rows where `processed_at IS NOT NULL` — never deletes
  pending or DLQ messages unless explicitly configured.

### A.8 Replay Helper

`replay_inbox_messages()` resets selected messages for re-processing by
explicit event ID list. A free-form `where_clause` parameter was considered
but rejected: `EXPLAIN`-based validation is insufficient to prevent all
injection vectors (scalar subqueries, set-returning functions), and requiring
operators to compose the filter externally via a parameterised `SELECT` is
safer and no less ergonomic.

```sql
-- Replay a specific list of event IDs
SELECT pgtrickle.replay_inbox_messages(
    'payment_inbox',
    event_ids => ARRAY['evt-001', 'evt-002']
);
-- To replay by filter, first collect the IDs externally:
-- SELECT ARRAY_AGG(event_id) FROM pgtrickle.payment_inbox
--   WHERE event_type = 'PaymentFailed' AND received_at > '2026-04-01';
-- Then pass the result to replay_inbox_messages().
```

#### Implementation

```sql
UPDATE <schema>.<inbox_table>
SET <processed_at_column> = NULL,
    <retry_count_column>   = 0,
    <error_column>         = NULL
WHERE <id_column> = ANY($1::text[])          -- parameterised; no dynamic SQL
  AND (<processed_at_column> IS NOT NULL
       OR <retry_count_column> >= <max_retries>)
RETURNING 1;
```

Returns the count of rows reset.

#### Safety

- Uses a parameterised `WHERE <id_column> = ANY($1)` — no free-form SQL
  accepted, eliminating the injection surface entirely.
- A `pg_trickle_alert` event of type `inbox_replay` is emitted with the
  count of replayed messages.

### A.9 Lifecycle & Drop Cascade

| Event | Effect |
|-------|--------|
| `pgtrickle.create_inbox(name)` | Creates inbox table + stream tables + metadata. |
| `pgtrickle.enable_inbox_tracking(table)` | Creates stream tables + metadata; existing table untouched. |
| `pgtrickle.drop_inbox(name)` | Drops stream tables + metadata. Inbox table preserved (unless `cascade := true` and `is_managed = true`). |
| `pgtrickle.drop_inbox(name, cascade := true)` | Also drops inbox table if `is_managed`. For adopted tables, only drops STs + metadata (warns user). |
| `DROP EXTENSION pg_trickle` | All managed inbox tables + STs dropped. Adopted tables survive but lose STs. |

#### `drop_inbox()` Behaviour Matrix

| `is_managed` | `cascade` | Inbox table | Stream tables | Metadata |
|--------------|-----------|-------------|---------------|----------|
| true | false | Preserved | Dropped | Dropped |
| true | true | **Dropped** | Dropped | Dropped |
| false | false | Preserved | Dropped | Dropped |
| false | true | **Preserved** (warning emitted) | Dropped | Dropped |

### A.10 GUCs

| GUC | Type | Default | Description |
|-----|------|---------|-------------|
| `pg_trickle.inbox_enabled` | bool | `true` | Master switch. When `false`, inbox stream tables are still refreshed but alerts are suppressed. |
| `pg_trickle.inbox_processed_retention_hours` | int | `72` | Default retention for processed inbox messages. |
| `pg_trickle.inbox_dlq_retention_hours` | int | `0` | Retention for DLQ messages. `0` = keep forever. |
| `pg_trickle.inbox_drain_batch_size` | int | `10000` | Max rows deleted per drain pass. |
| `pg_trickle.inbox_drain_interval_seconds` | int | `60` | Minimum interval between drain passes. |
| `pg_trickle.inbox_dlq_alert_max_per_refresh` | int | `10` | Max individual DLQ alerts per refresh; excess batched into summary. |

All GUCs are `SUSET` (require superuser to change).

### A.11 Failure Handling

| Failure | Behaviour |
|---------|-----------|
| Inbox table missing (DDL drift) | Stream table refresh fails; ST marked `ERROR`; `pg_trickle_alert` emitted. Recovery: `drop_inbox()` then `create_inbox()`. |
| `create_inbox()` partially fails | Transaction rolls back; no orphaned objects. |
| Stream table refresh fails | Standard pg_trickle error handling; retried next cycle. |
| Drain loop fails | Logged via `pgrx::warning!`; retried next cleanup cycle. Does not block stream table refresh. |
| Application INSERT fails | Not pg_trickle's responsibility; application handles. |
| Processing failure | Application increments `retry_count`; pg_trickle detects via stream table refresh. |
| **Processor crash after marking `processed_at` but before committing business logic** | pg_trickle sees the message as processed (`processed_at IS NOT NULL`). **Mitigation:** always execute business logic first, then set `processed_at` — this is the two-phase commit pattern for at-least-once processing. Recovery: `replay_inbox_messages(name, event_ids => ARRAY[$1])` re-queues specific messages. Operators should pause the processor (or set `pg_trickle.inbox_enabled = false`) before bulk replay to avoid re-processing by concurrent workers. See `PoisonMessageGuard` in `examples/inbox/inbox_processor.py` for the circuit-breaker pattern. |

### A.12 Performance Considerations

#### Stream Table Refresh Cost

The pending stream table is the primary performance concern. For an inbox
with N total rows and P pending rows:

| Refresh mode | Cost | When |
|--------------|------|------|
| DIFFERENTIAL | O(Δ) — only changed rows | Default; preferred |
| FULL | O(N) — scans entire table | Fallback for complex queries |

DIFFERENTIAL is effective because:
- CDC triggers fire only on INSERT (new message) and UPDATE
  (`processed_at` set, `retry_count` incremented).
- Between refreshes, only messages that changed state generate deltas.
- The partial index `WHERE processed_at IS NULL` keeps the hot path fast.

#### Expected Overhead

- Stream table refresh at 1 Hz with 100 messages/sec: ~2–5 ms/refresh
  (dominated by CDC buffer scan, not query complexity).
- Marginal cost of DLQ + stats stream tables: < 1 ms/refresh each (small
  result sets).
- No additional cost on the message write path (INSERT to inbox table is
  unchanged).

#### Scaling Guidelines

| Throughput | Recommendation |
|------------|---------------|
| < 1K msg/sec | Default `create_inbox()` settings work well. |
| 1K–10K msg/sec | Consider partitioning inbox table by `received_at` (via pg_partman). Increase `schedule` to `2s`. |
| > 10K msg/sec | Partition + increase `schedule` to `5s`. Use `FOR UPDATE SKIP LOCKED` directly on base table for processing (bypass pending ST). |

#### Backpressure When Ingestion Outpaces Processing

pg_trickle does not own the inbox write path, so it cannot apply
authomatic backpressure. When the message ingestion rate exceeds processor
throughput, the `pending_<inbox>` stream table grows, CDC buffer scans
widen, and refresh latency degrades. Operators should act before this
becomes critical:

| `inbox_health()` status | Signal | Recommended action |
|------------------------|--------|---------|
| `healthy` | `oldest_pending_age_sec` < 30 | None — running normally |
| `degraded` | `oldest_pending_age_sec` ≥ 30 or `pending_count` > 10K | Add processor workers; inspect slow-path aggregates |
| `critical` | `oldest_pending_age_sec` ≥ 120 or `pending_count` > 100K | Throttle ingestion at the consumer/webhook layer; scale out processors aggressively |

The `oldest_pending_age_sec` field from `inbox_health()` is the primary
pressure indicator. Wire it to a Grafana alert or a pg_trickle alert rule
to automate the response. The `stats_<inbox>` stream table exposes
`throughput_per_sec` which can be used to compare against broker ingestion
rate for a combined lag metric.

#### Stats Stream Table at High Throughput

The `stats_<inbox>` ST uses **DIFFERENTIAL** refresh mode (the `now()`-dependent
`max_pending_age_sec` column was removed from the materialised query — see §A.4
design note — enabling DIFFERENTIAL without spurious deltas) at a 10 s schedule.
At high message volumes the underlying `COUNT(*)`/`AVG()` scan is O(N) in inbox
table size:

| Inbox size | Estimated stats refresh time |
|------------|-------------------------------|
| < 100K rows | < 5 ms |
| 100K–1M rows | ~10–50 ms |
| > 1M rows | > 100 ms — consider `with_stats => false` |

For high-throughput inboxes (> 10K msg/sec or > 1M rows), set
`with_stats => false` and use `inbox_health()` on demand instead of
the materialised stats stream table. The `pending_<inbox>` and
`dlq_<inbox>` STs are DIFFERENTIAL and are not affected.

#### Bloat Mitigation

Inbox tables have a mixed UPDATE + DELETE pattern (mark processed, then
drain). Recommended autovacuum settings applied by `create_inbox()`:

```sql
ALTER TABLE pgtrickle.<inbox> SET (
    autovacuum_vacuum_scale_factor = 0.05,
    autovacuum_vacuum_cost_limit = 1000,
    autovacuum_analyze_scale_factor = 0.02
);
```

### A.13 Migration & Upgrade

#### New Installation

- Catalog table `pgtrickle.pgt_inbox_config` created via the upgrade SQL
  script.
- New SQL functions registered.
- No existing data affected.

#### Downgrade

Not supported in initial release. Users must:
1. `drop_inbox()` for all inboxes.
2. Reinstall prior version.

---

## Part B — Ordered Processing Extension (v0.24.0)

### B.1 Goals

- Support **per-aggregate ordered processing** (process messages for the
  same entity in sequence-number order).
- Detect **sequence gaps** (missing messages that block ordered processing).
- Enable **priority queues** (process critical messages before normal ones).
- Support **partition-affinity** for competing workers (each worker handles
  a subset of aggregates for cache locality).

> **Mutual Exclusion:** `enable_inbox_ordering()` and
> `enable_inbox_priority()` are **mutually exclusive** for the same inbox.
> Attempting to enable both returns `InboxOrderingPriorityConflict { name }`
> with a clear message:
> _"Per-aggregate ordering and priority tiers cannot be enabled together on
> the same inbox. Ordered processing requires processing the next message
> per aggregate regardless of priority; priority tiers would violate sequence
> ordering across tiers. Use a single tier with ordering, or model priority
> as separate inboxes."_
> If ordered processing with priority is required, the recommended pattern
> is separate inboxes per priority class, each with ordering enabled.

### B.2 SQL API

```sql
-- Enable ordered processing for an inbox
SELECT pgtrickle.enable_inbox_ordering(
    inbox_name        TEXT,
    aggregate_id_col  TEXT DEFAULT 'aggregate_id',
    sequence_num_col  TEXT DEFAULT 'sequence_num'
) RETURNS void;
-- Errors: InboxNotFound, InboxColumnMissing, InboxOrderingPriorityConflict
--         (if enable_inbox_priority was already called on this inbox)

-- Disable ordered processing (teardown)
SELECT pgtrickle.disable_inbox_ordering(
    inbox_name TEXT,
    if_exists  BOOLEAN DEFAULT false
) RETURNS void;
-- Drops the next_<inbox> stream table and the pgt_inbox_ordering_config row.
-- The original unified pending_<inbox> stream table continues to be available.
-- Returns InboxNotFound if if_exists = false and ordering is not enabled.

-- Enable priority processing for an inbox
SELECT pgtrickle.enable_inbox_priority(
    inbox_name       TEXT,
    priority_col     TEXT DEFAULT 'priority',
    tiers            JSONB DEFAULT '[
        {"name": "critical", "min": 1, "max": 2, "schedule": "1s"},
        {"name": "normal",   "min": 3, "max": 6, "schedule": "5s"},
        {"name": "background","min": 7, "max": 9, "schedule": "30s"}
    ]'
) RETURNS void;
-- Errors: InboxNotFound, InboxColumnMissing, InboxOrderingPriorityConflict
--         (if enable_inbox_ordering was already called on this inbox)

-- Disable priority processing (teardown)
SELECT pgtrickle.disable_inbox_priority(
    inbox_name TEXT,
    if_exists  BOOLEAN DEFAULT false
) RETURNS void;
-- Drops all pending_<inbox>_<tier> stream tables and the pgt_inbox_priority_config row.
-- The original unified pending_<inbox> stream table was never dropped and continues to be
-- available. Returns InboxNotFound if if_exists = false and priority is not enabled.

-- Inspect ordering gaps (on-demand query; for high-throughput inboxes
-- instead of continuous auto-refresh of the gaps_<inbox> stream table)
SELECT * FROM pgtrickle.inbox_ordering_gaps(
    inbox_name TEXT
);
-- Columns: aggregate_id, expected_seq, gap_age_sec
-- Returns empty set if no gaps detected.
```

### B.3 Aggregate-Ordered Stream Table

When `enable_inbox_ordering()` is called, a new stream table replaces (or
supplements) the default pending stream table:

```sql
SELECT pgtrickle.create_stream_table(
    'next_<inbox_name>',
    $$WITH last_processed AS (
        SELECT aggregate_id,
               COALESCE(MAX(sequence_num), 0) AS last_seq
        FROM <inbox_table>
        WHERE processed_at IS NOT NULL
        GROUP BY aggregate_id
    ),
    next_event AS (
        SELECT DISTINCT ON (i.aggregate_id)
               i.event_id, i.event_type, i.aggregate_id, i.sequence_num,
               i.payload, i.received_at
        FROM <inbox_table> i
        LEFT JOIN last_processed lp ON lp.aggregate_id = i.aggregate_id
        WHERE i.processed_at IS NULL
          AND i.retry_count < <max_retries>
          AND i.sequence_num = COALESCE(lp.last_seq, 0) + 1
        ORDER BY i.aggregate_id, i.sequence_num
    )
    SELECT * FROM next_event$$,
    schedule => <schedule>,
    refresh_mode => 'DIFFERENTIAL'
);
```

This guarantees that only the **next expected message** per aggregate is
surfaced — preventing out-of-order processing.

> **Known performance limitation — `last_processed` CTE is a full scan.**
> The `last_processed` CTE scans all processed rows in the inbox table on
> every refresh. For a long-lived inbox with millions of processed messages
> this grows without bound and will eventually degrade refresh performance.
>
> **v0.24.0 mitigation:** Add a partial index:
> ```sql
> CREATE INDEX idx_<inbox>_processed_seq
>     ON <inbox_table> (aggregate_id, sequence_num)
>     WHERE processed_at IS NOT NULL;
> ```
> The planner will use this index for the `MAX(sequence_num) GROUP BY` query.
> This makes the CTE an **index-only scan**, but it still scales with the
> number of processed rows. Concrete scaling thresholds:
>
> | Processed rows | Est. refresh time (partial index) | Recommended schedule |
> |----------------|----------------------------------|----------------------|
> | < 100K         | < 5 ms                           | `1s` (default)       |
> | 100K – 1M      | ~10–50 ms                        | `5s`                 |
> | > 1M           | > 100 ms                         | `10s` – `30s`        |
> | > 10M          | > 1 s                            | Use `inbox_ordering_gaps()` on-demand only |
>
> Operators should increase the `next_<inbox>` schedule as processed history
> grows. Document this recommendation in the inbox runbook.
>
> **Post-v0.24.0 optimization:** Introduce a `pgt_inbox_sequence_state`
> catalog table with columns `(inbox_id, aggregate_id, last_processed_seq)`
> updated atomically when processors call `advance_inbox_sequence()`.
> This makes the `last_processed` CTE O(changed aggregates) instead of
> O(all processed rows), eliminating the scaling cliff entirely.

### B.4 Gap Detection Stream Table

```sql
SELECT pgtrickle.create_stream_table(
    'gaps_<inbox_name>',
    $$SELECT aggregate_id,
             sequence_num + 1 AS missing_sequence,
             EXTRACT(EPOCH FROM (now() - next_received_at)) AS gap_age_sec
      FROM (
          SELECT aggregate_id, sequence_num,
                 received_at,
                 LEAD(sequence_num) OVER (PARTITION BY aggregate_id ORDER BY sequence_num)
                     AS next_seq,
                 LEAD(received_at) OVER (PARTITION BY aggregate_id ORDER BY sequence_num)
                     AS next_received_at
          FROM <inbox_table>
          WHERE processed_at IS NULL
      ) t
      WHERE next_seq IS NOT NULL
        AND next_seq > sequence_num + 1$$,
    schedule => '30s',
    refresh_mode => 'FULL'  -- FULL required: now() in gap_age_sec changes every refresh
);
```

> **`gap_age_sec` semantics:** The gap age is measured from the `received_at`
> of the row *after* the gap — the row whose arrival revealed the gap. This
> is the most useful metric for operators: it answers "how long have we known
> about this gap?" If the row after the gap arrived 45 seconds ago, the gap
> has been visible for at least 45 seconds.
```

> **Why LEAD() instead of generate_series?**
> The `LEAD()` window function scans the pending messages table once in O(N log N)
> and detects gaps by comparing adjacent sequence numbers. It does **not** generate
> a virtual row per expected sequence number, so it scales to any sequence density.
> By contrast, `generate_series(MIN, MAX)` over all aggregates produces a row for
> every sequence number in every aggregate’s range — 1 000 aggregates with
> max_sequence = 1 000 000 generate 1 billion virtual rows regardless of how many
> messages actually exist. The `LEAD()` approach avoids this entirely.
>
> **Scope:** Only pending messages (`processed_at IS NULL`) are scanned.
> Processed rows are excluded — a gap in processed history is not actionable.
>
> Scale guidelines:
> - Up to 1 000 000 pending messages: ✅ acceptable at 30 s schedule
> - > 1 000 000 pending messages: ⚠️ extend schedule to 60 s; monitor refresh time
> - > 10 000 000 pending messages: add `pg_trickle_alert`
>   `inbox_ordering_gap_perf` warning and consider on-demand checks only via
>   `inbox_ordering_gaps()` instead of continuous auto-refresh

When gaps are detected, a `pg_trickle_alert` event is emitted:

```json
{
  "type": "inbox_ordering_gap",
  "inbox_name": "payment_inbox",
  "aggregate_id": "customer-42",
  "missing_sequence": 7,
  "gap_age_sec": 45.2
}
```

### B.5 Priority Queue Support

When `enable_inbox_priority()` is called, multiple stream tables are created
per priority tier:

```sql
-- For each tier in the tiers array:
SELECT pgtrickle.create_stream_table(
    'pending_<inbox_name>_<tier_name>',
    $$SELECT event_id, event_type, payload, received_at, priority, retry_count
      FROM <inbox_table>
      WHERE processed_at IS NULL
        AND retry_count < <max_retries>
        AND priority BETWEEN <tier_min> AND <tier_max>$$,
    schedule => <tier_schedule>,
    refresh_mode => 'DIFFERENTIAL'
);
```

The original `pending_<inbox>` stream table is preserved as a unified view
across all priorities.

When `disable_inbox_priority()` is called:
1. All `pending_<inbox>_<tier>` stream tables are dropped.
2. The `pgt_inbox_priority_config` config row is deleted.
3. The original unified `pending_<inbox>` stream table is **already present**
   (it was never dropped by `enable_inbox_priority()`), so no restoration step
   is needed. Processors using the unified ST can resume immediately.

### B.6 Competing Workers with Partition Affinity

For high-throughput inboxes processed by multiple workers, partition affinity
improves cache locality and reduces contention:

```sql
-- Boolean-returning helper: returns true if this row belongs to the given worker.
-- Returns a BOOLEAN so it composes safely with prepared statements and ORMs
-- without requiring SQL string interpolation.
SELECT pgtrickle.inbox_is_my_partition(
    aggregate_id TEXT,
    worker_id    INT,                      -- 0-based
    total_workers INT                      -- total worker count
) RETURNS BOOLEAN;
-- Returns: abs(hashtext(aggregate_id)) % total_workers = worker_id
```

Workers use this in their polling query:

```python
rows = await conn.fetch("""
    SELECT event_id, event_type, payload
    FROM pgtrickle.payment_inbox
    WHERE processed_at IS NULL
      AND retry_count < 5
      AND pgtrickle.inbox_is_my_partition(aggregate_id, $1, $2)
    ORDER BY received_at
    LIMIT 50
    FOR UPDATE SKIP LOCKED
""", worker_id, total_workers)
```

This is advisory — workers can still process any message. The partition
condition just makes it likely that each worker focuses on its own subset.
Because the function is called inline in a WHERE clause it composes safely
with prepared statements and ORM query builders, unlike a TEXT-returning
helper that requires unsafe SQL interpolation.

---

## Part C — Reference Implementations

> **Security note on table name interpolation:** The reference examples below
> use Python f-strings to interpolate table names into SQL (e.g.
> `f"INSERT INTO {INBOX_TABLE} ..."`). This is acceptable in reference code
> because (a) the table name is read from an operator-controlled environment
> variable, not from user input, and (b) each script validates the name format
> via regex before use. In production relay code (e.g. `pgtrickle-relay`
> v0.25.0), table names should be resolved from the pg_trickle catalog at
> startup and validated against `pgt_inbox_config` / `pgt_outbox_config` —
> never accepted from request parameters or message payloads.

### C.1 Inbox Writer (NATS JetStream → PostgreSQL)

Ships as `examples/inbox/inbox_writer_nats.py`:

```python
import asyncio, asyncpg, json, os, re, signal
import nats

NATS_URL     = os.environ['NATS_URL']
PG_DSN       = os.environ['PG_DSN']
SUBJECT      = os.environ.get('NATS_SUBJECT', 'payments.>')
CONSUMER     = os.environ.get('NATS_CONSUMER', 'inbox-writer')
STREAM       = os.environ.get('NATS_STREAM', 'PAYMENTS')
INBOX_TABLE  = os.environ.get('INBOX_TABLE', 'pgtrickle.payment_inbox')

# SAFETY: INBOX_TABLE is operator-provided. Validate before use in f-strings.
if not re.match(r'^[a-z_][a-z0-9_.]*$', INBOX_TABLE):
    raise ValueError(f"INBOX_TABLE must be a valid schema-qualified identifier, got: {INBOX_TABLE!r}")

async def main():
    nc = await nats.connect(NATS_URL)
    js = nc.jetstream()
    db = await asyncpg.connect(PG_DSN)

    consumer = await js.pull_subscribe(
        SUBJECT,
        durable=CONSUMER,
        stream=STREAM,
    )

    stop = asyncio.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        asyncio.get_running_loop().add_signal_handler(sig, stop.set)

    while not stop.is_set():
        try:
            messages = await consumer.fetch(batch=50, timeout=5)
        except nats.errors.TimeoutError:
            continue

        async with db.transaction():
            for msg in messages:
                payload = json.loads(msg.data)
                headers = msg.headers or {}
                event_id = headers.get("Nats-Msg-Id", msg.reply)

                await db.execute(f"""
                    INSERT INTO {INBOX_TABLE}
                        (event_id, event_type, source, payload, trace_id)
                    VALUES ($1, $2, $3, $4, $5)
                    ON CONFLICT (event_id) DO NOTHING
                """,
                    event_id,
                    msg.subject.split(".")[-1],
                    headers.get("Source", "unknown"),
                    json.dumps(payload),
                    headers.get("X-Trace-Id"),
                )

                await msg.ack()

    await nc.close()
    await db.close()

if __name__ == '__main__':
    asyncio.run(main())
```

### C.2 Inbox Processor (PostgreSQL → Business Logic)

Ships as `examples/inbox/inbox_processor.py`:

```python
import asyncio, asyncpg, json, os, signal, time

PG_DSN       = os.environ['PG_DSN']
INBOX_TABLE  = os.environ.get('INBOX_TABLE', 'pgtrickle.payment_inbox')
BATCH_SIZE   = int(os.environ.get('BATCH_SIZE', '50'))
POLL_SECONDS = float(os.environ.get('POLL_SECONDS', '0.5'))

# SAFETY: INBOX_TABLE is read from an environment variable set by the operator,
# not from user input. Validate its format to prevent injection if this script
# is ever adapted to accept CLI arguments or config file values.
import re
if not re.match(r'^[a-z_][a-z0-9_.]*$', INBOX_TABLE):
    raise ValueError(f"INBOX_TABLE must be a valid schema-qualified identifier, got: {INBOX_TABLE!r}")

class PoisonMessageGuard:
    """Track consecutive failures; pause processing if threshold breached."""

    def __init__(self, max_consecutive=5, cooldown=30):
        self.max = max_consecutive
        self.cooldown = cooldown
        self.consecutive = 0

    def record_success(self):
        self.consecutive = 0

    def record_failure(self, event_id: str, error: str):
        self.consecutive += 1
        if self.consecutive >= self.max:
            print(f"POISON GUARD: {self.consecutive} consecutive failures, "
                  f"pausing {self.cooldown}s. Last: {event_id}: {error}")
            time.sleep(self.cooldown)
            self.consecutive = 0

async def process_event(event_type: str, payload: dict) -> None:
    """Replace with actual business logic."""
    if event_type == "OrderCreated":
        # e.g. create payment intent
        pass
    else:
        raise ValueError(f"Unknown event type: {event_type}")

async def main():
    db = await asyncpg.connect(PG_DSN)
    guard = PoisonMessageGuard()

    stop = asyncio.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        asyncio.get_running_loop().add_signal_handler(sig, stop.set)

    while not stop.is_set():
        rows = await db.fetch(f"""
            SELECT event_id, event_type, payload, retry_count
            FROM {INBOX_TABLE}
            WHERE processed_at IS NULL AND retry_count < 5
            ORDER BY received_at
            LIMIT $1
            FOR UPDATE SKIP LOCKED
        """, BATCH_SIZE)
        # NOTE: This query processes messages in receipt order (unordered with respect
        # to aggregates). For per-aggregate ordered processing (Part B), query the
        # next_<inbox> stream table instead, which guarantees sequence ordering per aggregate.

        if not rows:
            await asyncio.wait([stop.wait()], timeout=POLL_SECONDS)
            continue

        for row in rows:
            try:
                await process_event(row['event_type'], row['payload'])
                await db.execute(f"""
                    UPDATE {INBOX_TABLE}
                    SET processed_at = now()
                    WHERE event_id = $1
                """, row['event_id'])
                guard.record_success()
            except Exception as e:
                error_msg = str(e)[:1000]
                await db.execute(f"""
                    UPDATE {INBOX_TABLE}
                    SET error = $1, retry_count = retry_count + 1
                    WHERE event_id = $2
                """, error_msg, row['event_id'])
                guard.record_failure(row['event_id'], error_msg)

    await db.close()

if __name__ == '__main__':
    asyncio.run(main())
```

### C.3 Webhook Receiver (HTTP → PostgreSQL)

Ships as `examples/inbox/webhook_receiver.py`:

```python
import hashlib, hmac, json, os
from fastapi import FastAPI, Request, HTTPException
import asyncpg

app = FastAPI()
PG_DSN = os.environ['PG_DSN']
WEBHOOK_SECRET = os.environ['WEBHOOK_SECRET']
INBOX_TABLE = os.environ.get('INBOX_TABLE', 'pgtrickle.webhook_inbox')

# SAFETY: INBOX_TABLE is operator-provided. Validate before use in f-strings.
import re
if not re.match(r'^[a-z_][a-z0-9_.]*$', INBOX_TABLE):
    raise ValueError(f\"INBOX_TABLE must be a valid schema-qualified identifier, got: {INBOX_TABLE!r}\")

db_pool: asyncpg.Pool = None

@app.on_event("startup")
async def startup():
    global db_pool
    db_pool = await asyncpg.create_pool(PG_DSN, min_size=2, max_size=10)

@app.post("/webhooks/{provider}")
async def receive_webhook(provider: str, request: Request):
    body = await request.body()

    # Step 1: Verify HMAC signature BEFORE any database write
    signature = request.headers.get("X-Signature-256", "")
    expected = hmac.new(
        WEBHOOK_SECRET.encode(), body, hashlib.sha256
    ).hexdigest()
    if not hmac.compare_digest(signature, f"sha256={expected}"):
        raise HTTPException(status_code=400, detail="Invalid signature")

    # Step 2: Write to inbox (idempotent)
    payload = json.loads(body)
    event_id = payload.get("id") or request.headers.get("X-Request-Id")
    event_type = payload.get("type", "unknown")

    async with db_pool.acquire() as conn:
        await conn.execute(f"""
            INSERT INTO {INBOX_TABLE}
                (event_id, event_type, source, payload)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (event_id) DO NOTHING
        """, event_id, event_type, provider, json.dumps(payload))

    # Step 3: Return 200 immediately — processing happens async
    return {"status": "received"}
```

---

## Part D — Testing Strategy

### D.1 Unit Tests

- `create_inbox()` happy path: verify table, indexes, stream tables, metadata.
- `create_inbox()` validation: duplicate name, invalid schedule.
- `enable_inbox_tracking()` with custom column names: verify stream table SQL
  uses correct column references.
- `enable_inbox_tracking()` validation: missing columns, wrong types, no PK.
- `drop_inbox()` with `cascade` and `is_managed` matrix (4 combinations).
- `replay_inbox_messages()` with valid `event_ids` array; invalid (NULL or empty) input rejected with a clear error.
- `replay_inbox_messages()` with `event_ids` containing unknown IDs: verify only matching messages are reset, unknown IDs silently ignored.
- `inbox_health()` returns correct health status for various inbox states.
- Column mapping in `pgt_inbox_config` correctly generates stream table SQL.

### D.2 Integration Tests (Testcontainers)

- End-to-end: `create_inbox()` → INSERT messages → refresh → assert pending
  ST has correct rows → UPDATE `processed_at` → refresh → assert pending
  ST is empty.
- DLQ routing: INSERT → fail 5 times (incrementing `retry_count`) → refresh
  → assert message appears in DLQ ST and disappears from pending ST.
- DLQ alert: verify `pg_trickle_alert` fires when DLQ ST gains new rows.
- Retention drain: INSERT + process 100 messages → set `retention_hours = 0.001`
  → run drain → verify processed messages deleted, pending messages preserved.
- `enable_inbox_tracking()` on existing table with non-standard column names:
  verify stream tables work correctly.
- Stats ST: INSERT mix of event types → refresh → verify correct counts per
  event type.
- `replay_inbox_messages()`: process messages → replay by filter → verify
  they reappear in pending ST.
- Concurrent processors with `FOR UPDATE SKIP LOCKED`: 3 connections poll
  simultaneously → verify no message processed twice.
- `drop_inbox()` removes all stream tables and metadata; inbox table
  preserved when `cascade := false`.

### D.3 E2E Tests

- Full Docker stack with NATS sidecar: inbox writer consumes from NATS →
  writes to inbox → processor reads from pending ST → marks processed →
  verify exactly-once processing.
- Duplicate delivery: publish same `Nats-Msg-Id` twice → verify only one
  inbox row exists.
- Processor crash mid-batch: kill processor → verify unprocessed messages
  remain in pending ST → restart processor → verify all messages eventually
  processed.
- DLQ flow: publish message that always fails processing → verify it moves
  to DLQ after `max_retries` → replay → verify it reappears in pending ST.
- Webhook receiver: POST webhook with valid/invalid signatures → verify
  correct acceptance/rejection.

### D.4 Property-Based Tests

- `proptest` over arbitrary sequences of INSERT / process / fail / replay:
  - Pending count + processed count + DLQ count = total inbox rows.
  - No message is simultaneously in pending ST and DLQ ST.
  - After `replay_inbox_messages()`, replayed messages appear in pending ST
    on next refresh.

### D.5 Benchmark

Extend `benches/refresh_bench.rs`:
- `inbox_pending_st_refresh_100_pending` (100 pending out of 10K total)
- `inbox_pending_st_refresh_10k_pending` (10K pending out of 100K total)
- `inbox_stats_st_refresh` (aggregate query over 100K rows)
- `inbox_gaps_st_refresh_10k_aggregates` (gap detection at 10K aggregates,\
  max sequence 1 000; must complete in < 1 s at 30 s refresh schedule)

Acceptance criteria: pending ST refresh < 5 ms at 100 pending, < 50 ms at
10K pending. Gap detection < 1 s at 10K aggregates.

### D.6 Chaos Tests

- Processor crash after setting `processed_at` but before finishing business
  logic: verify `replay_inbox_messages()` re-queues the message correctly and
  the pending ST reflects it on the next refresh.
- 10 concurrent processor connections with `FOR UPDATE SKIP LOCKED`: inject
  100 000 messages and verify each processed exactly once (no duplicate
  business-logic execution).
- DLQ flood: inject 50 messages that all fail immediately; verify DLQ alert
  fires with batch summary (not 50 individual alerts) and `inbox_health()`
  reports `critical` status.
- `enable_inbox_ordering()` then inject 50 messages out-of-order per aggregate:
  verify `next_<inbox>` surfaces them strictly in sequence order; verify gap
  detection fires for each missing sequence.
- Upgrade path (v0.23.0 → v0.24.0): existing non-inbox stream tables survive;
  new inbox catalog tables present; `inbox_health()` available.

---

## Part E — Documentation Plan

| File | Sections to Add |
|------|-----------------|
| [docs/SQL_REFERENCE.md](../../docs/SQL_REFERENCE.md) | `pgtrickle.create_inbox()`, `enable_inbox_tracking()`, `drop_inbox()`, `inbox_status()`, `inbox_health()`, `replay_inbox_messages()`, `enable_inbox_ordering()`, `enable_inbox_priority()`, `inbox_is_my_partition()`; ordered+priority mutual exclusion note |
| [docs/CONFIGURATION.md](../../docs/CONFIGURATION.md) | All six inbox GUCs with defaults and effect descriptions |
| [docs/PATTERNS.md](../../docs/PATTERNS.md) | New section "Transactional Inbox" with: basic inbox setup, DLQ flow, processor crash recovery guide, per-aggregate ordering pattern, priority queue pattern, partition affinity for competing workers, "Ordered + Priority" mutual exclusion explanation and recommended alternative (separate inboxes per priority class) |
| [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md) | Diagram of inbox pipeline with stream tables |
| [docs/TROUBLESHOOTING.md](../../docs/TROUBLESHOOTING.md) | Common inbox issues: DLQ growth, stale pending messages, processor backpressure, gap detection slowdown at scale |
| [CHANGELOG.md](../../CHANGELOG.md) | v0.24.0 entry for INBOX-1 through INBOX-10 and INBOX-B1 through INBOX-B6 |
| [dbt-pgtrickle/](../../dbt-pgtrickle/) | `inbox_config` property in dbt model config; `pgtrickle_create_inbox` macro; dbt docs update |
| [examples/](../../examples/) | `inbox/inbox_writer_nats.py`, `inbox/inbox_processor.py`, `inbox/webhook_receiver.py`, `inbox/inbox_processor_ordered.py` |

---

## Part F — Implementation Roadmap

### Part A — v0.24.0 (Essential Patterns)

| Item | Description | Effort |
|------|-------------|--------|
| INBOX-1 | Catalog table `pgt_inbox_config`, `create_inbox()` SQL function, inbox table DDL generation, error variants | 1d |
| INBOX-2 | Auto-created stream tables (pending, DLQ, stats) with column-mapped SQL generation | 1d |
| INBOX-3 | `enable_inbox_tracking()` for existing tables: validation, column mapping, stream table creation | 0.5d |
| INBOX-4 | DLQ alert mechanism (post-refresh hook), `inbox_health()`, `inbox_status()` | 0.5d |
| INBOX-5 | Retention drain for processed messages, `replay_inbox_messages()` (event_ids only), `drop_inbox()` lifecycle | 0.5d |
| INBOX-6 | Unit + integration + E2E tests, benchmark, docs updates, reference examples | 1d |

**Total: ~4.5 days** (matches roadmap estimate).

### Part B — v0.24.0 (Production Patterns)

| Item | Description | Effort |
|------|-------------|--------|
| ORD-1 | `enable_inbox_ordering()`: aggregate-ordered stream table, sequence tracking, partial index for `last_processed` CTE | 1d |
| ORD-2 | Gap detection stream table + alert mechanism | 0.5d |
| ORD-3 | `enable_inbox_priority()`: per-tier stream tables, configurable schedules | 1d |
| ORD-4 | `inbox_is_my_partition()`: boolean-returning hash-based worker affinity helper. Signature: `inbox_is_my_partition(aggregate_id TEXT, worker_id INT, total_workers INT) RETURNS BOOLEAN`. Composable with prepared statements and ORMs; no SQL string interpolation required. | 0.5d |
| ORD-5 | E2E tests for ordering, gaps, priority, partition affinity | 1d |

**Total: ~4 days.**

---

## Part G — Open Questions

1. ✅ **Schema for `create_inbox()` tables.** ✅ **Resolved:** Managed inbox
   tables default to the `pgtrickle` schema. An optional `schema TEXT
   DEFAULT 'pgtrickle'` parameter is added to `create_inbox()` so users can
   place the inbox table in `public` or any other schema — useful for RLS
   policies and `pg_dump --schema=public` workflows. Adopted tables
   (`enable_inbox_tracking`) remain in their original schema. The chosen
   schema is stored in `pgt_inbox_config.inbox_table_schema`.

2. **IMMEDIATE-mode inbox stream tables.** Should the pending ST support
   `IMMEDIATE` mode for zero-lag processing? **Proposal:** Allow it \u2014
   the pending ST is a source-table-driven view, and IMMEDIATE mode on that
   ST adds latency to every inbox INSERT (same concern as outbox). Default
   to DIFFERENTIAL; document IMMEDIATE as an advanced opt-in for sub-second
   pending visibility. Unlike outbox, inbox IMMEDIATE does not have the
   atomicity concern (inbox writes are application-controlled).

3. **CloudEvents schema.** Should `create_inbox()` offer a CloudEvents-
   compatible schema variant with `spec_version`, `subject`,
   `data_content_type` columns? **Proposal:** not in v0.24.0; add as an
   optional `schema_variant => 'cloudevents'` parameter later.

4. **Payload validation on INSERT.** Should pg_trickle offer a trigger-based
   JSON Schema validator for inbox payloads? **Proposal:** out of scope \u2014
   validation belongs in the application's inbox-writer layer.

5. **Per-inbox DLQ alerts vs. global alert.** Currently, DLQ alerts include
   the `inbox_name` so a single `LISTEN pg_trickle_alert` can discriminate.
   Should there also be per-inbox channels (e.g.,
   `pg_trickle_alert_payment_inbox`)? **Proposal:** single channel with
   routing by `inbox_name` in the payload. Per-inbox channels add
   complexity for marginal benefit.

6. **Replay safety for multi-worker processors.** \u2705 **Resolved:** When
   `replay_inbox_messages()` resets `processed_at` to NULL, concurrent
   processors can pick up the message immediately. This is intentional (it
   is a re-queue). Operators must either: (a) drain the processor before
   bulk replay; or (b) set `pg_trickle.inbox_enabled = false` temporarily
   to suppress alerts while replaying; or (c) stop the processor
   explicitly. This is documented in A.11 Failure Handling, A.8 Replay
   Helper, and `docs/PATTERNS.md`.

7. **Integration with Outbox Helper.** The common inbox \u2192 business logic \u2192
   outbox pipeline (see PLAN_TRANSACTIONAL_INBOX.md \u00a7Combining Outbox and
   Inbox) should be documented as a pattern but not special-cased in code.
   **Proposal:** add a "Bidirectional Event Pipeline" section to
   `docs/PATTERNS.md` with a worked example.

8. **Multi-tenancy.** Should `create_inbox()` accept a `tenant_id` parameter
   and enable RLS? **Proposal:** out of scope for Part A. Document
   tenant-per-row and tenant-per-schema patterns in `docs/PATTERNS.md`.

9. **Inbox metrics export.** Should `inbox_health()` export Prometheus-
   compatible metrics? **Proposal:** the existing Grafana dashboard
   integration reads from stream tables directly. No additional export
   mechanism needed in Part A.

10. **`enable_inbox_tracking()` with missing optional columns.** If the
    adopted table lacks `source` or `trace_id`, should the helper skip those
    in stream table SQL? **Proposal:** yes \u2014 optional columns
    (`source`, `aggregate_id`, `trace_id`) should be gracefully omitted
    from stream table definitions if not present in the adopted table.

11. **`enable_inbox_ordering()` + `enable_inbox_priority()` together.** \u2705
    **Resolved:** mutually exclusive (see Part B, B.1 Goals and B.2 SQL
    API). Returns `InboxOrderingPriorityConflict`. Recommended alternative:
    separate inboxes per priority class, each with ordering enabled
    independently. Documented in `docs/PATTERNS.md`.

---

## Part H \u2014 Design Review Checklist

Five critical questions resolved before implementation begins:

| # | Question | Decision | Rationale |
|---|----------|----------|-----------|
| 1 | Can ordering and priority be enabled together on the same inbox? | **No \u2014 hard error** (`InboxOrderingPriorityConflict`) | Per-aggregate ordering must surface the next sequence regardless of priority. Mixing tiers violates sequence guarantees. Use separate inboxes per priority class, each with ordering. |
| 2 | Processor crash recovery \u2014 who is responsible? | **Application** (two-phase pattern) | pg_trickle documents: execute business logic first, set `processed_at` second. Replay via `replay_inbox_messages()`. Document in PATTERNS.md with code example. |
| 3 | Gap detection performance at scale | **Benchmark-gated** + scale guidelines documented | < 1 s at 10K aggregates is the v0.24.0 gate. Above 100K aggregates, auto-refresh disabled; on-demand via `inbox_ordering_gaps()` only. Delta-based optimization tracked as post-v0.24.0 follow-up. |
| 4 | Naming collision resolution for stream tables | **Truncate to 56 bytes + 7-char hex suffix** | Same algorithm as outbox. Final names stored in `pgt_inbox_config` (pending/dlq/stats ST name columns). |
| 5 | dbt integration scope | **dbt model config properties + macro** | `outbox_enabled`, `inbox_config` properties supported in dbt model config. Documented in `dbt-pgtrickle/README.md` and `docs/SQL_REFERENCE.md`. |

---

## Appendix — End-to-End Worked Example

### Setup

```sql
-- Create an inbox for payment events with all bells and whistles
SELECT pgtrickle.create_inbox(
    inbox_name       => 'payment_inbox',
    max_retries      => 5,
    schedule         => '1 second',
    with_dead_letter => true,
    with_stats       => true,
    retention_hours  => 48
);

-- This creates:
-- 1. pgtrickle.payment_inbox (table)
-- 2. pending_payment_inbox   (stream table, DIFFERENTIAL, 1s)
-- 3. dlq_payment_inbox       (stream table, DIFFERENTIAL, 30s)
-- 4. stats_payment_inbox     (stream table, DIFFERENTIAL, 10s)
-- 5. pgt_inbox_config metadata row
```

### Inbox Writer (External NATS Consumer)

```python
# Inbox writer receives from NATS and writes to inbox table
msg = await consumer.fetch(batch=1)
await db.execute("""
    INSERT INTO pgtrickle.payment_inbox
        (event_id, event_type, source, payload)
    VALUES ($1, $2, $3, $4)
    ON CONFLICT (event_id) DO NOTHING
""", msg.headers["Nats-Msg-Id"], "OrderCreated", "order-service",
     json.dumps({"order_id": 1, "amount": 99.95}))
await msg.ack()
```

### Inspect After a Few Seconds

```sql
-- Pending messages (automatically maintained)
SELECT * FROM pending_payment_inbox;
```

| event_id | event_type | payload | received_at | retry_count |
|----------|------------|---------|-------------|-------------|
| evt-001 | OrderCreated | `{"order_id": 1, "amount": 99.95}` | 10:00:01 | 0 |
| evt-002 | OrderCreated | `{"order_id": 2, "amount": 42.00}` | 10:00:02 | 0 |

### Processing

```python
# Processor reads from base table with FOR UPDATE SKIP LOCKED
rows = await db.fetch("""
    SELECT event_id, event_type, payload
    FROM pgtrickle.payment_inbox
    WHERE processed_at IS NULL AND retry_count < 5
    ORDER BY received_at
    LIMIT 50
    FOR UPDATE SKIP LOCKED
""")

for row in rows:
    try:
        await create_payment_intent(row['payload'])
        await db.execute("""
            UPDATE pgtrickle.payment_inbox
            SET processed_at = now()
            WHERE event_id = $1
        """, row['event_id'])
    except Exception as e:
        await db.execute("""
            UPDATE pgtrickle.payment_inbox
            SET error = $1, retry_count = retry_count + 1
            WHERE event_id = $2
        """, str(e)[:1000], row['event_id'])
```

### After Processing (evt-001 succeeds, evt-002 fails 5 times)

```sql
-- Pending stream table: only evt-002 remaining (until it hits DLQ)
SELECT * FROM pending_payment_inbox;
-- (empty — evt-001 processed, evt-002 now in DLQ)

-- DLQ stream table: evt-002 exceeded max_retries
SELECT * FROM dlq_payment_inbox;
```

| event_id | event_type | error | retry_count | received_at |
|----------|------------|-------|-------------|-------------|
| evt-002 | OrderCreated | "constraint violation: ..." | 5 | 10:00:02 |

```sql
-- Alert emitted on pg_trickle_alert channel:
-- {"type":"inbox_dlq_message","inbox_name":"payment_inbox",
--  "event_id":"evt-002","event_type":"OrderCreated",
--  "retry_count":5,"error":"constraint violation: ..."}
```

### Statistics

```sql
SELECT * FROM stats_payment_inbox;
```

| event_type | pending | processed | dead_letter | avg_processing_time_sec | max_pending_age_sec |
|------------|---------|-----------|-------------|------------------------|---------------------|
| OrderCreated | 0 | 1 | 1 | 0.05 | NULL |

### Health Check

```sql
SELECT pgtrickle.inbox_health('payment_inbox');
```

```json
{
  "pending_count": 0,
  "dead_letter_count": 1,
  "avg_processing_time_sec": 0.05,
  "oldest_pending_age_sec": null,
  "throughput_per_sec": 0.5,
  "duplicate_rate_pct": 0.0,
  "health_status": "degraded"
}
```

(`"degraded"` because `dead_letter_count > 0`)

### Replay After Fix

```sql
-- Operator fixes the root cause, then replays the DLQ message
SELECT pgtrickle.replay_inbox_messages(
    'payment_inbox',
    event_ids => ARRAY['evt-002']
);
-- Returns: 1 (one message reset)

-- After next refresh, evt-002 reappears in pending_payment_inbox
SELECT * FROM pending_payment_inbox;
```

| event_id | event_type | payload | received_at | retry_count |
|----------|------------|---------|-------------|-------------|
| evt-002 | OrderCreated | `{"order_id": 2, "amount": 42.00}` | 10:00:02 | 0 |

### Adopting an Existing Table

```sql
-- Existing table with non-standard column names
CREATE TABLE public.legacy_inbox (
    msg_id        UUID PRIMARY KEY,
    msg_type      TEXT NOT NULL,
    body          JSONB NOT NULL,
    created       TIMESTAMPTZ DEFAULT now(),
    completed     TIMESTAMPTZ,
    failures      INT DEFAULT 0,
    fail_reason   TEXT
);

-- Adopt it into pg_trickle monitoring
SELECT pgtrickle.enable_inbox_tracking(
    inbox_table_name    => 'public.legacy_inbox',
    id_column           => 'msg_id',
    event_type_column   => 'msg_type',
    received_at_column  => 'created',
    processed_at_column => 'completed',
    retry_count_column  => 'failures',
    error_column        => 'fail_reason',
    max_retries         => 3,
    schedule            => '2s',
    with_dead_letter    => true,
    with_stats          => true
);

-- Now you get:
-- pending_legacy_inbox   (stream table)
-- dlq_legacy_inbox       (stream table)
-- stats_legacy_inbox     (stream table)
-- All using the mapped column names in their SQL definitions.
```

---

## Cross-References

- [PLAN_TRANSACTIONAL_INBOX.md](PLAN_TRANSACTIONAL_INBOX.md) — research
  background and pattern overview.
- [PLAN_TRANSACTIONAL_OUTBOX_HELPER.md](PLAN_TRANSACTIONAL_OUTBOX_HELPER.md) —
  companion implementation plan for the outbox helper.
- [PLAN_TRANSACTIONAL_OUTBOX.md](PLAN_TRANSACTIONAL_OUTBOX.md) — outbox
  pattern research.
