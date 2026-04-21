# Best-Practice Patterns for pg_trickle

This guide covers common data modeling patterns and recommended configurations
for pg_trickle stream tables. Each pattern includes worked SQL examples,
anti-patterns to avoid, and refresh mode recommendations.

> **Version:** v0.14.0+. Some features require recent versions — check
> [SQL_REFERENCE.md](SQL_REFERENCE.md) for per-feature availability.

---

## Table of Contents

- [Pattern 1: Bronze / Silver / Gold Materialization](#pattern-1-bronze--silver--gold-materialization)
- [Pattern 2: Event Sourcing with Stream Tables](#pattern-2-event-sourcing-with-stream-tables)
- [Pattern 3: Slowly Changing Dimensions (SCD)](#pattern-3-slowly-changing-dimensions-scd)
- [Pattern 4: High-Fan-Out Topology](#pattern-4-high-fan-out-topology)
- [Pattern 5: Real-Time Dashboards](#pattern-5-real-time-dashboards)
- [Pattern 6: Tiered Refresh Strategy](#pattern-6-tiered-refresh-strategy)
- [General Guidelines](#general-guidelines)

---

## Pattern 1: Bronze / Silver / Gold Materialization

A multi-layer approach where raw data flows through progressively refined
stream tables, similar to a medallion architecture.

### Architecture

```
  [raw_events]          ← Bronze: raw ingest table (regular table)
       ↓
  [events_cleaned]      ← Silver: filtered, deduplicated, typed
       ↓
  [events_aggregated]   ← Gold: business-level aggregates
```

### SQL Example

```sql
-- Bronze: regular PostgreSQL table (source of truth)
CREATE TABLE raw_events (
    event_id    BIGSERIAL PRIMARY KEY,
    user_id     INT NOT NULL,
    event_type  TEXT NOT NULL,
    payload     JSONB,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Silver: cleaned and deduplicated events
SELECT pgtrickle.create_stream_table(
    'events_cleaned',
    $$SELECT DISTINCT ON (event_id)
        event_id,
        user_id,
        event_type,
        (payload->>'amount')::numeric AS amount,
        received_at
      FROM raw_events
      WHERE event_type IN ('purchase', 'refund', 'subscription')$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Gold: per-user purchase summary
SELECT pgtrickle.create_stream_table(
    'user_purchase_summary',
    $$SELECT user_id,
             COUNT(*) AS total_purchases,
             SUM(amount) AS total_spent,
             AVG(amount) AS avg_order
      FROM events_cleaned
      WHERE event_type = 'purchase'
      GROUP BY user_id$$,
    schedule => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
```

### Recommended Configuration

| Layer  | Refresh Mode   | Schedule   | Tier |
|--------|---------------|------------|------|
| Silver | DIFFERENTIAL  | 5s – 30s   | hot  |
| Gold   | DIFFERENTIAL  | calculated | hot  |

### Anti-Patterns

- **Don't use FULL refresh for Silver.** With frequent small inserts,
  DIFFERENTIAL is 10–100x faster.
- **Don't skip the Silver layer.** Joining raw tables directly in Gold
  queries produces wider joins and slower deltas.
- **Don't use IMMEDIATE mode for Gold.** Aggregate maintenance on every
  DML row is expensive — batched DIFFERENTIAL is more efficient.

---

## Pattern 2: Event Sourcing with Stream Tables

Use stream tables as projections of an append-only event log. The source
table is the event store; stream tables materialize different read models.

### SQL Example

```sql
-- Event store (append-only source)
CREATE TABLE events (
    event_id    BIGSERIAL PRIMARY KEY,
    aggregate_id UUID NOT NULL,
    event_type   TEXT NOT NULL,
    payload      JSONB NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Projection 1: Current state per aggregate
SELECT pgtrickle.create_stream_table(
    'aggregate_state',
    $$SELECT DISTINCT ON (aggregate_id)
        aggregate_id,
        event_type AS last_event,
        payload AS current_state,
        created_at AS last_updated
      FROM events
      ORDER BY aggregate_id, created_at DESC$$,
    schedule => '2s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Projection 2: Event counts by type per hour
SELECT pgtrickle.create_stream_table(
    'hourly_event_counts',
    $$SELECT date_trunc('hour', created_at) AS hour,
             event_type,
             COUNT(*) AS event_count
      FROM events
      GROUP BY 1, 2$$,
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL'
);
```

### Recommended Configuration

| Projection          | Refresh Mode  | Why                                         |
|---------------------|--------------|---------------------------------------------|
| Current state       | DIFFERENTIAL | Small delta per cycle; DISTINCT ON supported |
| Hourly counts       | DIFFERENTIAL | Algebraic aggregate (COUNT), efficient delta |
| String aggregations | AUTO         | GROUP_RESCAN aggs may benefit from FULL      |

### Anti-Patterns

- **Don't DELETE from the event store.** pg_trickle tracks changes via
  triggers; mixing append and delete on the source creates unnecessary
  delta complexity. Archive old events to a separate table.
- **Don't use `append_only => true` with UPDATE/DELETE patterns.** The
  `append_only` flag skips DELETE tracking in the change buffer — only
  use it when the source truly never updates or deletes.

---

## Pattern 3: Slowly Changing Dimensions (SCD)

### SCD Type 1: Overwrite

The stream table always reflects the current state. Source updates
overwrite previous values.

```sql
-- Source: customer dimension table (updated in place)
CREATE TABLE customers (
    customer_id INT PRIMARY KEY,
    name        TEXT NOT NULL,
    email       TEXT,
    tier        TEXT DEFAULT 'standard',
    updated_at  TIMESTAMPTZ DEFAULT now()
);

-- SCD-1: current customer state enriched with order stats
SELECT pgtrickle.create_stream_table(
    'customer_360',
    $$SELECT c.customer_id,
             c.name,
             c.email,
             c.tier,
             COUNT(o.id) AS total_orders,
             COALESCE(SUM(o.amount), 0) AS lifetime_value
      FROM customers c
      LEFT JOIN orders o ON o.customer_id = c.customer_id
      GROUP BY c.customer_id, c.name, c.email, c.tier$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
```

### SCD Type 2: History Tracking

For SCD-2, maintain a history table with valid-from/valid-to ranges.
The stream table provides the current snapshot.

```sql
-- Source: customer history with validity ranges
CREATE TABLE customer_history (
    customer_id INT NOT NULL,
    name        TEXT NOT NULL,
    tier        TEXT NOT NULL,
    valid_from  TIMESTAMPTZ NOT NULL,
    valid_to    TIMESTAMPTZ,  -- NULL = current
    PRIMARY KEY (customer_id, valid_from)
);

-- Current active records only
SELECT pgtrickle.create_stream_table(
    'customers_current',
    $$SELECT customer_id, name, tier, valid_from
      FROM customer_history
      WHERE valid_to IS NULL$$,
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL'
);
```

### Anti-Patterns

- **Don't use FULL refresh for SCD-1 with large dimension tables.**
  Customer tables with millions of rows but few changes per cycle are
  ideal for DIFFERENTIAL.
- **Don't forget to index `valid_to IS NULL` for SCD-2 sources.** Without
  it, the delta scan touches all historical rows.

---

## Pattern 4: High-Fan-Out Topology

When a single source table feeds many downstream stream tables.

### Architecture

```
                    [orders]
                   ↙  ↓  ↓  ↘
  [daily_totals] [by_region] [by_product] [top_customers]
```

### SQL Example

```sql
-- Single source feeding multiple views
CREATE TABLE orders (
    id          SERIAL PRIMARY KEY,
    customer_id INT NOT NULL,
    region      TEXT NOT NULL,
    product_id  INT NOT NULL,
    amount      NUMERIC(10,2) NOT NULL,
    order_date  DATE NOT NULL DEFAULT CURRENT_DATE
);

-- Fan-out: 4 stream tables on 1 source
SELECT pgtrickle.create_stream_table('daily_totals',
    'SELECT order_date, SUM(amount) AS daily_total, COUNT(*) AS order_count
     FROM orders GROUP BY order_date',
    schedule => '5s', refresh_mode => 'DIFFERENTIAL');

SELECT pgtrickle.create_stream_table('by_region',
    'SELECT region, SUM(amount) AS total, COUNT(*) AS cnt
     FROM orders GROUP BY region',
    schedule => '5s', refresh_mode => 'DIFFERENTIAL');

SELECT pgtrickle.create_stream_table('by_product',
    'SELECT product_id, SUM(amount) AS total, COUNT(*) AS cnt
     FROM orders GROUP BY product_id',
    schedule => '5s', refresh_mode => 'DIFFERENTIAL');

SELECT pgtrickle.create_stream_table('top_customers',
    'SELECT customer_id, SUM(amount) AS lifetime_value, COUNT(*) AS order_count
     FROM orders GROUP BY customer_id',
    schedule => '10s', refresh_mode => 'DIFFERENTIAL');
```

### Recommended Configuration

- All fan-out targets share the same source change buffer — CDC overhead
  is paid once regardless of how many stream tables read from `orders`.
- Use `schedule => 'calculated'` on downstream STs when they chain from
  other stream tables.
- Consider `pg_trickle.max_workers` if fan-out exceeds 8 (default: 4 workers).

### Anti-Patterns

- **Don't use IMMEDIATE mode on high-fan-out sources.** Each DML row
  triggers N refreshes (one per downstream ST). Use DIFFERENTIAL with
  a batched schedule instead.
- **Don't set different schedules on STs that should be consistent.**
  If `daily_totals` and `by_region` must agree, give them the same
  schedule or use `diamond_consistency => 'atomic'`.

---

## Pattern 5: Real-Time Dashboards

For dashboards that need sub-second refresh latency.

### SQL Example

```sql
-- Live order monitor (sub-second freshness)
SELECT pgtrickle.create_stream_table(
    'order_monitor',
    $$SELECT
        date_trunc('minute', order_date) AS minute,
        region,
        COUNT(*) AS orders,
        SUM(amount) AS revenue
      FROM orders
      WHERE order_date >= CURRENT_DATE
      GROUP BY 1, 2$$,
    schedule => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- For truly real-time needs, use IMMEDIATE mode (triggers on each DML)
SELECT pgtrickle.create_stream_table(
    'live_counter',
    $$SELECT region, COUNT(*) AS cnt, SUM(amount) AS total
      FROM orders GROUP BY region$$,
    schedule => 'IMMEDIATE',
    refresh_mode => 'DIFFERENTIAL'
);
```

### When to Use IMMEDIATE vs Scheduled DIFFERENTIAL

| Scenario                      | Mode       | Why                                    |
|-------------------------------|-----------|----------------------------------------|
| Dashboard polls every 1s      | `1s`      | Batched delta amortizes overhead        |
| GraphQL subscription, < 100ms | IMMEDIATE | Triggers fire synchronously per DML    |
| Aggregate with GROUP_RESCAN   | `5s`+     | Avoid per-row full rescans             |
| High write throughput (>1K/s) | `2s`–`5s` | IMMEDIATE adds latency to each INSERT  |

### Anti-Patterns

- **Don't use IMMEDIATE for complex joins.** Each INSERT/UPDATE/DELETE
  fires the full DVM delta SQL synchronously — multi-table joins in
  IMMEDIATE mode add significant latency to writes.
- **Don't forget `pooler_compatibility_mode` with PgBouncer.** Transaction
  pooling drops temp tables between transactions; enable this flag to
  avoid stale PREPARE statements.

---

## Pattern 6: Tiered Refresh Strategy

Assign refresh importance tiers to control scheduling priority.

```sql
-- Hot: real-time operational dashboard
SELECT pgtrickle.create_stream_table('live_metrics', ...);
SELECT pgtrickle.alter_stream_table('live_metrics', tier => 'hot');

-- Warm: hourly business reports (2x interval multiplier)
SELECT pgtrickle.create_stream_table('hourly_report', ...,
    schedule => '1m');
SELECT pgtrickle.alter_stream_table('hourly_report', tier => 'warm');

-- Cold: daily analytics (10x interval multiplier)
SELECT pgtrickle.create_stream_table('daily_analytics', ...,
    schedule => '5m');
SELECT pgtrickle.alter_stream_table('daily_analytics', tier => 'cold');

-- Frozen: archive/audit (skip refresh entirely)
SELECT pgtrickle.alter_stream_table('audit_log_summary', tier => 'frozen');
```

### Tier Multipliers

| Tier   | Schedule Multiplier | Use Case                          |
|--------|-------------------:|-----------------------------------|
| hot    | 1x                 | Operational dashboards, alerts     |
| warm   | 2x                 | Hourly reports, batch pipelines    |
| cold   | 10x                | Daily analytics, low-priority STs  |
| frozen | skip               | Paused/archived, manual refresh    |

---

## General Guidelines

### Choosing a Refresh Mode

| Scenario                                     | Recommended Mode |
|----------------------------------------------|-----------------|
| Source has < 5% change ratio per cycle        | DIFFERENTIAL     |
| Source changes > 50% per cycle                | FULL             |
| Query is a simple filter/projection           | DIFFERENTIAL     |
| Query has GROUP_RESCAN aggregates (MIN, MAX)  | AUTO             |
| Query joins 4+ tables                        | DIFFERENTIAL     |
| Target table < 1000 rows                     | FULL             |
| Need per-row latency guarantee               | IMMEDIATE        |

Use `pgtrickle.recommend_refresh_mode()` (v0.14.0+) for automated
analysis:

```sql
SELECT pgt_name, recommended_mode, confidence, reason
FROM pgtrickle.recommend_refresh_mode();
```

### Monitoring Checklist

```sql
-- Check refresh efficiency across all stream tables
SELECT pgt_name, refresh_mode, diff_speedup, avg_change_ratio
FROM pgtrickle.refresh_efficiency()
ORDER BY total_refreshes DESC;

-- Find stream tables that might benefit from mode change
SELECT pgt_name, current_mode, recommended_mode, reason
FROM pgtrickle.recommend_refresh_mode()
WHERE recommended_mode != 'KEEP';

-- Check for error states
SELECT pgt_name, status, last_error_message
FROM pgtrickle.stream_tables_info
WHERE status IN ('ERROR', 'SUSPENDED');

-- Export definitions for backup
SELECT pgtrickle.export_definition(pgt_schema || '.' || pgt_name)
FROM pgtrickle.pgt_stream_tables;
```

### Common Mistakes

1. **Using FULL refresh by default.** Start with DIFFERENTIAL — it's
   correct for 80%+ of workloads. Switch to FULL only when
   `recommend_refresh_mode()` suggests it.

2. **Over-scheduling.** A 1-second schedule on a table with 1-hour
   change cycles wastes CPU. Match the schedule to actual data arrival
   rate.

3. **Ignoring `append_only`.** If the source table is truly append-only
   (no UPDATEs, no DELETEs), set `append_only => true` to halve
   change buffer writes.

4. **Not using `calculated` schedule for chained STs.** When ST-B reads
   from ST-A, use `schedule => 'calculated'` on ST-B to avoid
   unnecessary refreshes. The scheduler automatically propagates
   ST-A changes downstream.

5. **Mixing IMMEDIATE and complex joins.** IMMEDIATE mode fires delta
   SQL on every DML — an 8-table join in IMMEDIATE mode adds 50–200ms
   to each INSERT. Use scheduled DIFFERENTIAL for complex queries.

---

## Replica Bootstrap & PITR Alignment (v0.27.0)

When bootstrapping a new replica or performing point-in-time recovery,
stream tables need special handling because their state is derived from
source data at a specific frontier (LSN + timestamp).

### The problem

After a `pg_basebackup` or logical restore, stream table rows are present
but their frontiers may be stale. The next refresh would trigger a FULL
re-scan of all source data, which is expensive for large stream tables.

### Solution: use snapshots for replica bootstrap

```sql
-- On the primary: export the stream table state
SELECT pgtrickle.snapshot_stream_table(
    'public.orders_agg',
    'pgtrickle.orders_agg_replica_init'
);

-- Dump only the snapshot table to the replica
pg_dump -t 'pgtrickle.orders_agg_replica_init' mydb | psql replica_db

-- On the replica: restore and align the frontier
SELECT pgtrickle.restore_from_snapshot(
    'public.orders_agg',
    'pgtrickle.orders_agg_replica_init'
);

-- Clean up the bootstrap snapshot
SELECT pgtrickle.drop_snapshot('pgtrickle.orders_agg_replica_init');
```

After `restore_from_snapshot()`, the frontier is set to the snapshot's
frontier and the next refresh is DIFFERENTIAL — only changes after the
snapshot creation time are fetched.

### PITR alignment workflow

When performing PITR to a specific LSN:

1. Take a snapshot immediately before the target LSN
2. Restore the database to the target LSN using `pg_basebackup` + WAL replay
3. Run `restore_from_snapshot()` on each stream table to align frontiers

```sql
-- Step 1: snapshot all stream tables (before PITR)
SELECT pgtrickle.snapshot_stream_table(
    pgt_schema || '.' || pgt_name,
    'pgtrickle.pitr_snapshot_' || pgt_name || '_' || extract(epoch from now())::bigint
)
FROM pgtrickle.pgt_stream_tables
WHERE status = 'ACTIVE';

-- Step 3 (after PITR): restore all snapshots
SELECT pgtrickle.restore_from_snapshot(
    pgt_schema || '.' || pgt_name,
    'pgtrickle.pitr_snapshot_' || pgt_name || '_<epoch>'
)
FROM pgtrickle.pgt_stream_tables;
```

> **Performance**: Restoring a 1M-row stream table from a snapshot completes
> in < 5 seconds (bulk INSERT from local table). The frontier alignment ensures
> the first differential refresh fetches only new changes, not all rows.
