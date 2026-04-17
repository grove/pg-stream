# Streaming Window Aggregations with pg_trickle

> **Status:** Research Report
> **Created:** 2026-04-17
> **Category:** Architecture Pattern
> **Related:** [PLAN_CQRS.md](PLAN_CQRS.md) · [PLAN_TRANSACTIONAL_OUTBOX.md](PLAN_TRANSACTIONAL_OUTBOX.md)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [What Are Streaming Window Aggregations?](#what-are-streaming-window-aggregations)
- [The Problem It Solves](#the-problem-it-solves)
- [Window Types Explained](#window-types-explained)
  - [Tumbling Windows](#tumbling-windows)
  - [Sliding / Rolling Windows](#sliding--rolling-windows)
  - [Hopping Windows](#hopping-windows)
  - [Session Windows](#session-windows)
- [How pg_trickle Enables Streaming Windows](#how-pg_trickle-enables-streaming-windows)
  - [Architecture Overview](#architecture-overview)
  - [Approach 1: Tumbling Windows via date_trunc()](#approach-1-tumbling-windows-via-date_trunc)
  - [Approach 2: Sliding Windows via SQL Window Functions](#approach-2-sliding-windows-via-sql-window-functions)
  - [Approach 3: Hopping Windows via Epoch Arithmetic](#approach-3-hopping-windows-via-epoch-arithmetic)
  - [Approach 4: Session Windows via Gap Detection](#approach-4-session-windows-via-gap-detection)
- [Handling Late-Arriving Data](#handling-late-arriving-data)
- [Worked Example: Real-Time Fraud Detection](#worked-example-real-time-fraud-detection)
- [Worked Example: IoT Sensor Analytics](#worked-example-iot-sensor-analytics)
- [Worked Example: API Performance Monitoring](#worked-example-api-performance-monitoring)
- [Advanced Patterns](#advanced-patterns)
  - [Hierarchical Windows (Minute → Hour → Day)](#hierarchical-windows-minute--hour--day)
  - [Multi-Key Windows (Partitioned)](#multi-key-windows-partitioned)
  - [Watermarks and Grace Periods](#watermarks-and-grace-periods)
- [Performance Characteristics](#performance-characteristics)
- [Complementary PostgreSQL Extensions](#complementary-postgresql-extensions)
  - [TimescaleDB — Time-Series Optimisation](#timescaledb--time-series-optimisation)
  - [pg_partman — Automatic Time Partitioning](#pg_partman--automatic-time-partitioning)
  - [pg_cron — Scheduled Window Archival](#pg_cron--scheduled-window-archival)
- [Potential pg_trickle Extensions](#potential-pg_trickle-extensions)
  - [Extension 1: Window Helper Functions](#extension-1-window-helper-functions)
  - [Extension 2: Watermark State Tracking](#extension-2-watermark-state-tracking)
- [Comparison with Dedicated Streaming Systems](#comparison-with-dedicated-streaming-systems)
- [When NOT to Use This Pattern](#when-not-to-use-this-pattern)
- [References](#references)

---

## Executive Summary

**Streaming Window Aggregations** compute aggregate metrics (sums, counts,
averages, percentiles) over a moving or fixed time window on a continuous event
stream. They are the backbone of real-time analytics: per-minute order volumes,
rolling 5-minute averages, hourly percentile latencies.

Traditionally, this pattern required a dedicated streaming system (Apache
Flink, Kafka Streams, Spark Structured Streaming). pg_trickle brings the same
capability to PostgreSQL:

1. **DIFFERENTIAL refresh** means only the time buckets touched by new events
   are recomputed — not the entire historical dataset. A burst of 500 events
   into a 1-minute bucket recomputes just that bucket, not the last 7 days of
   data.
2. **Standard SQL window functions** (`OVER (ORDER BY ts ROWS BETWEEN N
   PRECEDING AND CURRENT ROW)`) express sliding windows directly — no custom
   operator syntax required.
3. **`date_trunc()`-based grouping** creates stable, re-usable window keys
   that the DVM engine tracks efficiently across refresh cycles.
4. **`append_only => true`** on event-style sources halves CDC write overhead,
   because deletes never occur on a time-series append log.
5. **Tiered scheduling** lets high-priority windows (per-second fraud signals)
   refresh at 1s while lower-priority windows (daily rollups) refresh at 5m
   — all from the same source table.

---

## What Are Streaming Window Aggregations?

A **window** is a bounded slice of an unbounded event stream, defined by time
or row count. Aggregating within windows answers questions like:

- "How many orders arrived in the last 5 minutes?"
- "What is the 95th-percentile response time over the past hour?"
- "How much revenue did we make per minute today?"
- "Is this user's transaction rate unusually high in the last 10 minutes?"

**The key distinction from batch aggregations:**

| Batch                                     | Streaming Window                           |
|-------------------------------------------|--------------------------------------------|
| Run once over a fixed dataset             | Continuously updated as new data arrives   |
| Query re-reads all data every time        | Only changed windows recomputed            |
| Result is stale immediately               | Result is always fresh within schedule lag |
| Easy to implement                         | Complex without the right tools            |

pg_trickle bridges this gap: stream tables deliver continuously-refreshed
windowed aggregations using plain SQL.

---

## The Problem It Solves

### Problem 1: Full Table Scans on Every Dashboard Load

```sql
-- The naive approach: runs on every dashboard load
SELECT date_trunc('minute', created_at) AS minute,
       COUNT(*)                          AS orders,
       SUM(amount)                       AS revenue
FROM orders
WHERE created_at > now() - '24 hours'::interval
GROUP BY 1
ORDER BY 1 DESC;
-- With 1M rows in orders: ~80ms per query, runs 1000x/min = 80s of CPU per min
```

With a pg_trickle stream table, this query runs once per schedule cycle
(e.g., every 5s) using differential deltas. Dashboard loads hit the
pre-computed stream table at < 1ms.

### Problem 2: "Refresh Explosions" with Traditional Materialized Views

PostgreSQL materialized views support `REFRESH MATERIALIZED VIEW
CONCURRENTLY`, but:

- `CONCURRENTLY` still re-reads the entire source table.
- For a 7-day rolling window with 10M rows, every refresh is expensive
  regardless of how little changed.
- There is no native incremental/differential refresh for time-windowed views.

pg_trickle's DIFFERENTIAL mode solves this: only the delta (new/changed rows
since last refresh) flows through the aggregation engine.

### Problem 3: Operational Complexity of External Streaming Systems

Apache Flink provides world-class streaming windows — but it requires:

- A Kafka cluster for the event stream
- A Flink cluster for the processing jobs
- A sink system (Elasticsearch, ClickHouse, Redis) for results
- A separate PostgreSQL table to serve application queries

That is four additional infrastructure components, each with its own ops
burden. pg_trickle delivers the same windowed results from inside PostgreSQL.

---

## Window Types Explained

### Tumbling Windows

Non-overlapping, fixed-size windows that partition the time axis cleanly.
Every event belongs to exactly one window.

```
Time:  ─────────────────────────────────────────────────────▶
       [00:00 ─ 01:00][01:00 ─ 02:00][02:00 ─ 03:00] ...

Events: e1 e2  │  e3 e4 e5  │  e6  │
         window 1   window 2   window 3
```

**SQL:** `GROUP BY date_trunc('hour', ts)` — one row per window.

**Use cases:** Hourly revenue, daily active users, per-minute error counts.

### Sliding / Rolling Windows

Overlapping windows that continuously advance with time. Every event belongs
to multiple windows.

```
Time:   ──────────────────────────────────────────────────▶
        [═══════════════5 min═══════════════]
                    [═══════════════5 min═══════════════]
                                [═══════════════5 min═══]

At t=10m: window covers [5m–10m]
At t=11m: window covers [6m–11m]
At t=12m: window covers [7m–12m]
```

**SQL:** `SUM(amount) OVER (ORDER BY ts ROWS BETWEEN 299 PRECEDING AND CURRENT ROW)`

**Use cases:** Rolling 5-minute throughput, 1-hour moving average, "last N
transactions" per user.

### Hopping Windows

Like tumbling windows but with a step size smaller than the window size.
Events can belong to multiple windows.

```
Window size = 10 min, hop = 5 min:

[00:00─00:10]
       [00:05─00:15]
              [00:10─00:20]
                     [00:15─00:25]
```

**SQL:** Epoch arithmetic to compute window boundaries.

**Use cases:** More granular trend detection than tumbling windows; used in
fraud detection where you want overlapping anomaly windows.

### Session Windows

Windows that group events by activity gaps — a window closes when there is
no activity for longer than a timeout duration.

```
Events:  e1 e2 e3    (gap > T)    e4 e5    (gap > T)    e6
         [session 1]              [session 2]            [session 3]
```

**SQL:** `LAG(ts)` + gap detection via a cumulative sum of session breaks.

**Use cases:** User session analytics, IoT device activity windows, API
call grouping.

---

## How pg_trickle Enables Streaming Windows

### Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│                             PostgreSQL                                │
│                                                                       │
│  ┌──────────────────────────────────────┐                            │
│  │  Source: event/fact table            │                            │
│  │  (append-only preferred)             │                            │
│  │  orders, api_requests, sensor_reads  │                            │
│  └─────────────────┬────────────────────┘                            │
│                    │ CDC trigger (within same transaction)            │
│                    ▼                                                  │
│  ┌──────────────────────────────────────┐                            │
│  │  pgtrickle_changes (change buffer)   │                            │
│  │  Only new events since last refresh  │                            │
│  └─────────────────┬────────────────────┘                            │
│                    │ DIFFERENTIAL refresh                             │
│                    │ (recomputes only touched windows)                │
│                    ▼                                                  │
│  ┌─────────────────────────────────────────────────────────────┐     │
│  │  Stream Tables (Windowed Read Models)                        │     │
│  │                                                              │     │
│  │  orders_per_minute  ← tumbling, 5s, hot tier                │     │
│  │  revenue_rolling_1h ← sliding, 10s, hot tier                │     │
│  │  hourly_rollup      ← tumbling, 1m, warm tier               │     │
│  │  daily_summary      ← tumbling, 15m, cold tier              │     │
│  └─────────────────────────────────────────────────────────────┘     │
│                                                                       │
│  Dashboards, APIs, ML pipelines query stream tables — never the      │
│  source table                                                         │
└──────────────────────────────────────────────────────────────────────┘
```

### Approach 1: Tumbling Windows via date_trunc()

The most common pattern. `date_trunc()` produces stable group keys — when a
new event arrives in the `14:32` bucket, only that bucket's aggregate changes.

```sql
-- Source: order events (append-only)
CREATE TABLE orders (
    id         BIGSERIAL PRIMARY KEY,
    customer_id INT NOT NULL,
    region      TEXT NOT NULL,
    amount      NUMERIC(12, 2) NOT NULL,
    status      TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tumbling 1-minute windows: live order volume
SELECT pgtrickle.create_stream_table(
    'orders_per_minute',
    $$SELECT
        date_trunc('minute', created_at)  AS window_start,
        region,
        COUNT(*)                          AS order_count,
        SUM(amount)                       AS total_revenue,
        AVG(amount)                       AS avg_basket,
        COUNT(*) FILTER (WHERE status = 'cancelled')
                                          AS cancellations
      FROM orders
      WHERE created_at > now() - '2 hours'::interval
      GROUP BY 1, 2$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true   -- orders are never updated/deleted
);
SELECT pgtrickle.alter_stream_table('orders_per_minute', tier => 'hot');

-- Tumbling 1-hour windows: revenue rollup
SELECT pgtrickle.create_stream_table(
    'orders_per_hour',
    $$SELECT
        date_trunc('hour', created_at)    AS window_start,
        region,
        COUNT(*)                          AS order_count,
        SUM(amount)                       AS total_revenue,
        SUM(amount) FILTER (WHERE status = 'completed')
                                          AS settled_revenue
      FROM orders
      GROUP BY 1, 2$$,
    schedule => '1m',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
SELECT pgtrickle.alter_stream_table('orders_per_hour', tier => 'warm');
```

**Why DIFFERENTIAL is efficient here:**
When a new order arrives at `14:32:47`, only the `14:32` minute bucket and
the `14:00` hour bucket are touched. All other buckets remain unchanged.
The DIFFERENTIAL engine recomputes only those two group keys — not the 2
hours × 60 minutes × N regions of historical buckets.

### Approach 2: Sliding Windows via SQL Window Functions

Sliding windows compute aggregates over the N rows or N seconds preceding
each event. The result is a per-row rolling aggregate.

```sql
-- Source: API request log
CREATE TABLE api_requests (
    id            BIGSERIAL PRIMARY KEY,
    endpoint      TEXT NOT NULL,
    response_ms   INT NOT NULL,
    status_code   INT NOT NULL,
    user_id       INT,
    requested_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Sliding 5-minute rolling average response time, per endpoint
SELECT pgtrickle.create_stream_table(
    'endpoint_rolling_avg',
    $$SELECT
        id,
        endpoint,
        requested_at,
        response_ms,
        AVG(response_ms) OVER (
            PARTITION BY endpoint
            ORDER BY requested_at
            ROWS BETWEEN 299 PRECEDING AND CURRENT ROW   -- last ~300 rows ≈ 5min at 1 req/s
        )                                   AS rolling_avg_ms,
        COUNT(*) OVER (
            PARTITION BY endpoint
            ORDER BY requested_at
            ROWS BETWEEN 299 PRECEDING AND CURRENT ROW
        )                                   AS rolling_request_count
      FROM api_requests
      WHERE requested_at > now() - '6 hours'::interval$$,
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Time-range based sliding window (more precise than row count)
SELECT pgtrickle.create_stream_table(
    'user_spend_rolling_1h',
    $$SELECT
        a.user_id,
        a.requested_at,
        SUM(b.amount) AS rolling_1h_spend,
        COUNT(b.id)   AS rolling_1h_txn_count
      FROM orders a
      JOIN orders b ON b.user_id = a.user_id
        AND b.created_at BETWEEN a.created_at - '1 hour'::interval
                              AND a.created_at
      GROUP BY a.user_id, a.requested_at$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
```

**Note on performance:** Time-range sliding windows using self-joins can be
expensive for large source tables. Constrain the source with a time-bound
`WHERE` clause to limit the join surface. For very high-throughput sliding
windows (> 100K events/min), consider pre-bucketing into tumbling windows first,
then applying the slide over the buckets.

### Approach 3: Hopping Windows via Epoch Arithmetic

Hopping windows divide the timeline into overlapping windows of fixed size,
advancing by a step smaller than the window. Pure SQL arithmetic on epoch
values creates the window keys.

```sql
-- Hopping window: 10-minute windows, advancing every 5 minutes
-- Window size = 10 min, hop = 5 min
-- At t=12:07: the event falls into:
--   window starting at 12:00 (covers 12:00–12:10)
--   window starting at 12:05 (covers 12:05–12:15)

SELECT pgtrickle.create_stream_table(
    'orders_hopping_10m5m',
    $$WITH window_starts AS (
        SELECT
            o.id,
            o.amount,
            o.region,
            o.created_at,
            -- Generate the window start times this event belongs to
            -- Window size: 600s (10 min), hop: 300s (5 min)
            to_timestamp(
                (extract(epoch from o.created_at)::bigint / 300) * 300
                - s.offset_secs
            ) AS window_start
        FROM orders o
        -- Cross join with the two possible offsets (window_size / hop = 2)
        CROSS JOIN (VALUES (0), (300)) AS s(offset_secs)
        WHERE o.created_at > now() - '2 hours'::interval
          -- Only include if event falls within this window
          AND o.created_at >= to_timestamp(
                (extract(epoch from o.created_at)::bigint / 300) * 300
                - s.offset_secs
              )
          AND o.created_at < to_timestamp(
                (extract(epoch from o.created_at)::bigint / 300) * 300
                - s.offset_secs + 600
              )
    )
    SELECT
        window_start,
        window_start + '10 minutes'::interval AS window_end,
        region,
        COUNT(*)     AS order_count,
        SUM(amount)  AS total_revenue
    FROM window_starts
    GROUP BY window_start, region$$,
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
```

### Approach 4: Session Windows via Gap Detection

Session windows group events that occur within a timeout of each other.
A new session starts when the gap to the previous event exceeds the timeout.

```sql
-- User session detection: 30-minute inactivity timeout
SELECT pgtrickle.create_stream_table(
    'user_sessions',
    $$WITH gap_detection AS (
        SELECT
            user_id,
            created_at AS event_at,
            -- Is there a gap > 30 minutes before this event?
            CASE WHEN created_at - LAG(created_at) OVER (
                    PARTITION BY user_id ORDER BY created_at
                 ) > '30 minutes'::interval
                 OR LAG(created_at) OVER (
                    PARTITION BY user_id ORDER BY created_at
                 ) IS NULL
                 THEN 1 ELSE 0
            END AS is_session_start
        FROM orders
        WHERE created_at > now() - '24 hours'::interval
    ),
    session_ids AS (
        SELECT
            user_id,
            event_at,
            SUM(is_session_start) OVER (
                PARTITION BY user_id ORDER BY event_at
            ) AS session_num
        FROM gap_detection
    )
    SELECT
        user_id,
        session_num,
        MIN(event_at) AS session_start,
        MAX(event_at) AS session_end,
        COUNT(*)       AS events_in_session,
        MAX(event_at) - MIN(event_at) AS session_duration
    FROM session_ids
    GROUP BY user_id, session_num$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
```

---

## Handling Late-Arriving Data

Late-arriving events (events whose timestamp is earlier than the current
processing time) are a fundamental challenge in stream processing. An order
placed at 14:32 but recorded 5 minutes later at 14:37 belongs to the 14:32
window, not 14:37.

pg_trickle handles late arrivals naturally: DIFFERENTIAL refresh re-evaluates
the affected time bucket whenever the late row lands.

```sql
-- Strategy 1: Accept late arrivals for a grace period
-- The window covers `created_at` (event time), so late arrivals
-- automatically update the correct bucket on the next refresh

SELECT pgtrickle.create_stream_table(
    'orders_per_minute_with_grace',
    $$SELECT
        date_trunc('minute', created_at) AS window_start,
        COUNT(*)                         AS order_count,
        SUM(amount)                      AS revenue
      FROM orders
      WHERE created_at > now() - '2 hours'::interval   -- grace period: 2h
      GROUP BY 1$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
-- A late event from 1h 50min ago will update its bucket on next refresh
-- Events older than 2h are outside the window and will not appear

-- Strategy 2: Separate "settled" and "live" windows
-- Settled: > 5 minutes old (late arrivals are unlikely)
-- Live: < 5 minutes old (may still receive late arrivals)

SELECT pgtrickle.create_stream_table(
    'orders_settled',
    $$SELECT
        date_trunc('minute', created_at) AS window_start,
        COUNT(*)                         AS order_count,
        SUM(amount)                      AS revenue,
        true                             AS settled
      FROM orders
      WHERE created_at BETWEEN now() - '24 hours'::interval
                           AND now() - '5 minutes'::interval
      GROUP BY 1$$,
    schedule => '1m',   -- Slower refresh — settled windows rarely change
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);

SELECT pgtrickle.create_stream_table(
    'orders_live',
    $$SELECT
        date_trunc('minute', created_at) AS window_start,
        COUNT(*)                         AS order_count,
        SUM(amount)                      AS revenue,
        false                            AS settled
      FROM orders
      WHERE created_at > now() - '5 minutes'::interval
      GROUP BY 1$$,
    schedule => '2s',   -- Fast refresh for live window
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
```

**Rule of thumb:** Set the `WHERE` time bound to `now() - <grace_period>`.
Any late event within the grace period will update its correct window bucket.
Events older than the grace period are simply outside the window and ignored.

---

## Worked Example: Real-Time Fraud Detection

Fraud detection requires detecting unusual patterns over short time windows
with the lowest possible latency.

```sql
-- Source: transaction event table
CREATE TABLE transactions (
    id           BIGSERIAL PRIMARY KEY,
    account_id   INT NOT NULL,
    merchant_id  INT NOT NULL,
    amount       NUMERIC(12, 2) NOT NULL,
    currency     TEXT NOT NULL DEFAULT 'USD',
    country_code TEXT NOT NULL,
    txn_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Signal 1: Per-account velocity (last 10 minutes)
-- Fraud rule: > 5 transactions OR > $2000 in 10 minutes
SELECT pgtrickle.create_stream_table(
    'account_velocity_10m',
    $$SELECT
        account_id,
        COUNT(*)     AS txn_count_10m,
        SUM(amount)  AS total_amount_10m,
        COUNT(DISTINCT merchant_id)  AS unique_merchants_10m,
        COUNT(DISTINCT country_code) AS unique_countries_10m,
        MAX(amount)  AS max_single_txn
      FROM transactions
      WHERE txn_at > now() - '10 minutes'::interval
      GROUP BY account_id$$,
    schedule => 'IMMEDIATE',   -- Must update within the same transaction
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);

-- Signal 2: Unusual merchant activity (last 1 hour)
-- Fraud rule: merchant receiving more than 3x its normal hourly volume
SELECT pgtrickle.create_stream_table(
    'merchant_hourly_volume',
    $$SELECT
        merchant_id,
        date_trunc('hour', txn_at)  AS hour,
        COUNT(*)                    AS txn_count,
        SUM(amount)                 AS total_volume,
        COUNT(DISTINCT account_id)  AS unique_accounts,
        COUNT(DISTINCT country_code) AS country_spread
      FROM transactions
      WHERE txn_at > now() - '48 hours'::interval
      GROUP BY 1, 2$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);

-- Signal 3: Geographic anomaly — same account, multiple countries in 1h
SELECT pgtrickle.create_stream_table(
    'account_geo_anomaly',
    $$SELECT
        account_id,
        COUNT(DISTINCT country_code) AS countries_1h,
        array_agg(DISTINCT country_code ORDER BY country_code) AS country_list,
        MIN(txn_at)  AS first_txn,
        MAX(txn_at)  AS last_txn
      FROM transactions
      WHERE txn_at > now() - '1 hour'::interval
      GROUP BY account_id
      HAVING COUNT(DISTINCT country_code) > 1$$,
    schedule => '1m',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);

-- Fraud check at transaction time (application-level)
-- Step 1: IMMEDIATE-mode velocity check (zero lag)
SELECT txn_count_10m, total_amount_10m
FROM account_velocity_10m
WHERE account_id = $account_id;
-- If txn_count_10m > 5 OR total_amount_10m > 2000 → flag for review

-- Step 2: Geographic anomaly check
SELECT countries_1h, country_list
FROM account_geo_anomaly
WHERE account_id = $account_id;
-- If countries_1h > 1 → flag as suspicious

-- Step 3: Merchant spike check
SELECT
    txn_count,
    AVG(txn_count) OVER (ORDER BY hour ROWS BETWEEN 23 PRECEDING AND 1 PRECEDING)
        AS avg_hourly_volume
FROM merchant_hourly_volume
WHERE merchant_id = $merchant_id
ORDER BY hour DESC
LIMIT 1;
-- If txn_count > 3 * avg_hourly_volume → flag merchant
```

---

## Worked Example: IoT Sensor Analytics

IoT devices produce continuous high-volume time series. Window aggregations
reduce millions of raw readings to actionable per-minute summaries.

```sql
-- Source: raw sensor readings (very high write rate)
CREATE TABLE sensor_readings (
    id          BIGSERIAL PRIMARY KEY,
    device_id   INT NOT NULL,
    sensor_type TEXT NOT NULL,    -- temperature, pressure, humidity
    value       NUMERIC(10, 4) NOT NULL,
    unit        TEXT NOT NULL,
    quality     INT NOT NULL DEFAULT 100,  -- 0-100 signal quality
    read_at     TIMESTAMPTZ NOT NULL DEFAULT now()
) PARTITION BY RANGE (read_at);  -- Daily partitions for data retention

-- Only process good-quality readings
CREATE INDEX ON sensor_readings (device_id, read_at)
    WHERE quality >= 80;

-- Window 1: Per-minute device averages (operational view)
SELECT pgtrickle.create_stream_table(
    'sensor_1m_averages',
    $$SELECT
        device_id,
        sensor_type,
        date_trunc('minute', read_at)      AS minute,
        AVG(value)                         AS avg_value,
        MIN(value)                         AS min_value,
        MAX(value)                         AS max_value,
        STDDEV(value)                      AS stddev_value,
        COUNT(*)                           AS reading_count,
        AVG(quality)                       AS avg_quality
      FROM sensor_readings
      WHERE read_at > now() - '2 hours'::interval
        AND quality >= 80
      GROUP BY 1, 2, 3$$,
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
SELECT pgtrickle.alter_stream_table('sensor_1m_averages', tier => 'hot');

-- Window 2: Anomaly detection — readings outside 3-sigma band
SELECT pgtrickle.create_stream_table(
    'sensor_anomalies',
    $$WITH device_stats AS (
        SELECT
            device_id,
            sensor_type,
            AVG(avg_value)    AS baseline_mean,
            STDDEV(avg_value) AS baseline_stddev
        FROM sensor_1m_averages   -- reads from another stream table
        WHERE minute > now() - '24 hours'::interval
        GROUP BY device_id, sensor_type
    )
    SELECT
        r.device_id,
        r.sensor_type,
        r.read_at,
        r.value,
        s.baseline_mean,
        s.baseline_stddev,
        (r.value - s.baseline_mean) / NULLIF(s.baseline_stddev, 0) AS z_score,
        CASE
          WHEN abs((r.value - s.baseline_mean) / NULLIF(s.baseline_stddev, 0)) > 3
          THEN 'ANOMALY'
          ELSE 'NORMAL'
        END AS status
      FROM sensor_readings r
      JOIN device_stats s USING (device_id, sensor_type)
      WHERE r.read_at > now() - '1 hour'::interval
        AND r.quality >= 80$$,
    schedule => 'calculated',  -- refreshes when sensor_1m_averages updates
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);

-- Window 3: Hourly device health summary (warm tier)
SELECT pgtrickle.create_stream_table(
    'sensor_hourly_health',
    $$SELECT
        device_id,
        date_trunc('hour', minute)         AS hour,
        COUNT(DISTINCT sensor_type)        AS active_sensor_types,
        SUM(reading_count)                 AS total_readings,
        AVG(avg_quality)                   AS avg_signal_quality,
        BOOL_OR(avg_quality < 80)          AS any_low_quality_readings,
        COUNT(*) FILTER (WHERE avg_quality < 50)
                                           AS degraded_minutes
      FROM sensor_1m_averages             -- chain from the 1m stream table
      GROUP BY 1, 2$$,
    schedule => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('sensor_hourly_health', tier => 'warm');
```

---

## Worked Example: API Performance Monitoring

SRE teams need sub-minute visibility into API latency percentiles, error
rates, and throughput — across endpoints, regions, and clients.

```sql
-- Source: API access log
CREATE TABLE api_requests (
    id              BIGSERIAL PRIMARY KEY,
    endpoint        TEXT NOT NULL,
    method          TEXT NOT NULL,
    status_code     INT NOT NULL,
    response_ms     INT NOT NULL,
    client_id       INT,
    region          TEXT NOT NULL,
    requested_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tumbling 1-minute window: per-endpoint latency percentiles
SELECT pgtrickle.create_stream_table(
    'api_latency_1m',
    $$SELECT
        date_trunc('minute', requested_at)              AS minute,
        endpoint,
        region,
        COUNT(*)                                        AS request_count,
        COUNT(*) FILTER (WHERE status_code >= 500)      AS error_count,
        ROUND(
          COUNT(*) FILTER (WHERE status_code >= 500)::numeric
          / NULLIF(COUNT(*), 0) * 100, 2
        )                                               AS error_rate_pct,
        AVG(response_ms)                                AS avg_ms,
        percentile_disc(0.50) WITHIN GROUP
            (ORDER BY response_ms)                      AS p50_ms,
        percentile_disc(0.95) WITHIN GROUP
            (ORDER BY response_ms)                      AS p95_ms,
        percentile_disc(0.99) WITHIN GROUP
            (ORDER BY response_ms)                      AS p99_ms,
        MAX(response_ms)                                AS max_ms
      FROM api_requests
      WHERE requested_at > now() - '2 hours'::interval
      GROUP BY 1, 2, 3$$,
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
SELECT pgtrickle.alter_stream_table('api_latency_1m', tier => 'hot');

-- SLO burn rate: rolling 1-hour error budget consumption
SELECT pgtrickle.create_stream_table(
    'slo_burn_rate',
    $$SELECT
        endpoint,
        region,
        SUM(request_count)                              AS total_1h,
        SUM(error_count)                                AS errors_1h,
        ROUND(SUM(error_count)::numeric
              / NULLIF(SUM(request_count), 0) * 100, 4) AS error_rate_1h_pct,
        -- SLO target: 99.9% (0.1% error budget)
        ROUND((SUM(error_count)::numeric
               / NULLIF(SUM(request_count), 0)) / 0.001, 2)
                                                        AS slo_burn_rate,
        CASE
          WHEN (SUM(error_count)::numeric
                / NULLIF(SUM(request_count), 0)) > 0.014  -- 14x burn = ~5h to exhaustion
          THEN 'CRITICAL'
          WHEN (SUM(error_count)::numeric
                / NULLIF(SUM(request_count), 0)) > 0.002  -- 2x burn
          THEN 'WARNING'
          ELSE 'OK'
        END AS slo_status
      FROM api_latency_1m    -- chain from the 1m stream table
      WHERE minute > now() - '1 hour'::interval
      GROUP BY endpoint, region$$,
    schedule => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
```

---

## Advanced Patterns

### Hierarchical Windows (Minute → Hour → Day)

Chain stream tables at increasing granularities. Each level reads from the
previous one — the aggregation cost is paid incrementally.

```sql
-- Level 1: Raw → 1-minute buckets (hot tier)
SELECT pgtrickle.create_stream_table(
    'orders_1m', ..., schedule => '5s');

-- Level 2: 1-minute → 1-hour (warm tier, reads from Level 1)
SELECT pgtrickle.create_stream_table(
    'orders_1h',
    $$SELECT date_trunc('hour', window_start) AS hour,
             region,
             SUM(order_count)  AS order_count,
             SUM(total_revenue) AS total_revenue
      FROM orders_1m  -- reads from Level 1 stream table
      GROUP BY 1, 2$$,
    schedule => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('orders_1h', tier => 'warm');

-- Level 3: 1-hour → 1-day (cold tier, reads from Level 2)
SELECT pgtrickle.create_stream_table(
    'orders_daily',
    $$SELECT date_trunc('day', hour) AS day,
             region,
             SUM(order_count)        AS order_count,
             SUM(total_revenue)      AS total_revenue
      FROM orders_1h  -- reads from Level 2 stream table
      GROUP BY 1, 2$$,
    schedule => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('orders_daily', tier => 'cold');

-- The DAG: raw_events → orders_1m → orders_1h → orders_daily
-- Use pgtrickle.dag() to visualize the chain:
SELECT * FROM pgtrickle.dag();
```

**Performance benefit of hierarchical windows:**
- `orders_1m` processes raw deltas (N new rows).
- `orders_1h` processes minute-bucket deltas (at most 1 changed row per minute).
- `orders_daily` processes hour-bucket deltas (at most 1 changed row per hour).
- Each level performs far less work than computing from raw events.

### Multi-Key Windows (Partitioned)

Window aggregations partitioned across multiple dimensions simultaneously.

```sql
-- Revenue windowed by (region, product_category, customer_tier)
SELECT pgtrickle.create_stream_table(
    'revenue_by_segment',
    $$SELECT
        date_trunc('hour', o.created_at) AS hour,
        o.region,
        p.category                       AS product_category,
        c.tier                           AS customer_tier,
        COUNT(o.id)                      AS order_count,
        SUM(o.amount)                    AS revenue,
        COUNT(DISTINCT o.customer_id)    AS unique_buyers
      FROM orders o
      JOIN products p   ON p.id = o.product_id
      JOIN customers c  ON c.id = o.customer_id
      WHERE o.created_at > now() - '7 days'::interval
        AND o.status IN ('completed', 'shipped')
      GROUP BY 1, 2, 3, 4$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
-- Queries can now slice by any dimension combination without a join
SELECT * FROM revenue_by_segment
WHERE region = 'EMEA' AND product_category = 'electronics';
```

### Watermarks and Grace Periods

A watermark is the system's estimate of how far behind the stream's event
time is. In pg_trickle, you implement watermarks via time-bound `WHERE` clauses.

```sql
-- Watermark configuration table (adjustable at runtime)
CREATE TABLE stream_watermarks (
    stream_name   TEXT PRIMARY KEY,
    grace_period  INTERVAL NOT NULL DEFAULT '5 minutes',
    updated_at    TIMESTAMPTZ DEFAULT now()
);

INSERT INTO stream_watermarks VALUES
    ('orders_per_minute', '5 minutes'),
    ('sensor_1m_averages', '30 seconds'),
    ('api_latency_1m', '1 minute');

-- Application code reads the watermark at query time:
-- SELECT created_at > now() - grace_period FROM stream_watermarks WHERE stream_name = '...';

-- Or, use a function to inject the watermark dynamically
CREATE OR REPLACE FUNCTION get_watermark(stream_name TEXT)
RETURNS TIMESTAMPTZ AS $$
    SELECT now() - grace_period
    FROM stream_watermarks
    WHERE stream_name = $1;
$$ LANGUAGE sql STABLE;

SELECT pgtrickle.create_stream_table(
    'orders_watermarked',
    $$SELECT
        date_trunc('minute', created_at) AS window_start,
        COUNT(*)                         AS order_count,
        SUM(amount)                      AS revenue
      FROM orders
      WHERE created_at > get_watermark('orders_watermarked')
      GROUP BY 1$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
```

---

## Performance Characteristics

### Why DIFFERENTIAL Excels for Window Aggregations

Consider a source table with 10M rows and a 24-hour rolling window. At
100 new rows per second:

| Approach                             | Work per Refresh Cycle | Notes                                 |
|--------------------------------------|------------------------|---------------------------------------|
| PostgreSQL `REFRESH MATVIEW`         | Scan all 10M rows      | Full scan every time                  |
| Standard materialized view + trigger | N rows (via trigger)   | Complex trigger logic, error-prone    |
| pg_trickle FULL refresh              | Scan all 10M rows      | Same as matview                       |
| **pg_trickle DIFFERENTIAL**          | **~N rows (delta)**    | Only changed windows recomputed       |

For tumbling windows with `date_trunc()`, the DIFFERENTIAL engine identifies
which group keys changed (e.g., the `14:32` bucket) and recomputes only those
aggregates. The total work scales with the number of *changed* rows, not the
total dataset size.

### When to Use FULL Refresh Instead

pg_trickle's cost model will recommend FULL when:

```sql
SELECT pgt_name, recommended_mode, confidence, reason
FROM pgtrickle.recommend_refresh_mode()
WHERE pgt_name LIKE '%window%';
```

FULL is preferred for:
- **MIN/MAX aggregates over large partitions**: these require a full partition
  rescan when a minimum/maximum changes (GROUP_RESCAN). If your window has
  many rows per group key and MIN/MAX, FULL can be faster.
- **Very high change ratios**: when > 40% of the source rows change per cycle,
  the overhead of delta tracking exceeds a fresh scan.
- **Very small result sets** (< 100 rows): FULL scan + group-by is trivially
  fast and DIFFERENTIAL overhead isn't worth it.

### Scheduling Strategy

| Window Type      | Source Rate       | Recommended Schedule | Tier   |
|------------------|-------------------|----------------------|--------|
| 1-second tumbling | > 100 events/s    | `1s`                 | hot    |
| 1-minute tumbling | > 10 events/s     | `5s` – `15s`         | hot    |
| 5-minute tumbling | Any               | `30s` – `1m`         | warm   |
| 1-hour tumbling   | Any               | `calculated`         | warm   |
| Daily tumbling    | Any               | `calculated`         | cold   |
| Sliding (rows)    | Any               | `10s` – `30s`        | hot    |
| Session windows   | < 1000/s          | `30s`                | warm   |

---

## Complementary PostgreSQL Extensions

### TimescaleDB — Time-Series Optimisation

[TimescaleDB](https://www.timescale.com/) hypertables automatically partition
time-series data by time, dramatically speeding up time-range queries that
pg_trickle's DVM engine uses to identify changed buckets.

```sql
CREATE EXTENSION IF NOT EXISTS timescaledb;

-- Convert the source table to a hypertable (partitioned by hour)
SELECT create_hypertable('sensor_readings', 'read_at',
    chunk_time_interval => INTERVAL '1 hour');

-- pg_trickle stream tables on top of TimescaleDB hypertables work
-- identically — the hypertable's chunk pruning accelerates the
-- DIFFERENTIAL delta scan automatically
SELECT pgtrickle.create_stream_table(
    'sensor_1m_averages', ...,
    append_only => true
);
```

### pg_partman — Automatic Time Partitioning

[pg_partman](https://github.com/pgpartman/pg_partman) manages declarative
time-based partitioning and automatic partition creation/retention.

```sql
CREATE EXTENSION IF NOT EXISTS pg_partman;

-- Automatically manage monthly partitions on the orders table
SELECT partman.create_parent(
    p_parent_table => 'public.orders',
    p_control      => 'created_at',
    p_type         => 'range',
    p_interval     => 'monthly'
);

-- pg_partman's retention policy archives old partitions
-- pg_trickle stream tables with time-bound WHERE clauses only touch
-- recent partitions — automatic partition pruning makes this fast
```

### pg_cron — Scheduled Window Archival

Once windowed stream tables have computed their results, old time buckets
can be archived or snapshotted on a schedule.

```sql
CREATE EXTENSION IF NOT EXISTS pg_cron;

-- Archive yesterday's hourly data to a separate table at midnight
SELECT cron.schedule(
    'archive-daily-orders',
    '5 0 * * *',   -- 00:05 every day
    $$INSERT INTO orders_daily_archive
      SELECT * FROM orders_per_hour
      WHERE window_start::date = CURRENT_DATE - 1
      ON CONFLICT (window_start, region) DO NOTHING$$
);

-- Optionally disable or freeze the stream table for old time ranges
-- SELECT pgtrickle.alter_stream_table('orders_per_hour', tier => 'frozen');
```

---

## Potential pg_trickle Extensions

### Extension 1: Window Helper Functions

A set of convenience SQL functions for common window patterns, reducing
boilerplate:

```sql
-- Proposed API (not yet implemented)

-- Create a tumbling window stream table in one call
SELECT pgtrickle.create_tumbling_window(
    name        => 'orders_per_minute',
    source      => 'orders',
    window_col  => 'created_at',
    window_size => '1 minute',
    metrics     => ARRAY['COUNT(*) AS count', 'SUM(amount) AS revenue'],
    partitions  => ARRAY['region'],
    schedule    => '5s',
    lookback    => '2 hours'
);

-- Create a sliding window stream table
SELECT pgtrickle.create_sliding_window(
    name           => 'user_spend_rolling',
    source         => 'orders',
    time_col       => 'created_at',
    window_size    => '1 hour',
    partition_col  => 'customer_id',
    metric         => 'SUM(amount)',
    schedule       => '30s'
);
```

### Extension 2: Watermark State Tracking

Built-in watermark management: pg_trickle tracks the latest processed event
timestamp per stream table and exposes it as a system column.

```sql
-- Proposed: system-managed watermark per stream table
SELECT pgt_name, watermark_ts, watermark_lag_ms
FROM pgtrickle.stream_table_watermarks;

-- Late arrivals that fall before the watermark are logged
SELECT * FROM pgtrickle.late_arrivals
WHERE pgt_name = 'orders_per_minute'
  AND event_ts < watermark_ts - '1 minute'::interval;
```

---

## Comparison with Dedicated Streaming Systems

| Feature                       | Apache Flink            | Kafka Streams          | pg_trickle              |
|-------------------------------|-------------------------|------------------------|-------------------------|
| Tumbling windows              | ✅ Native               | ✅ Native              | ✅ date_trunc()         |
| Sliding windows               | ✅ Native               | ✅ Native              | ✅ SQL window functions  |
| Hopping windows               | ✅ Native               | ✅ Native              | ✅ Epoch arithmetic      |
| Session windows               | ✅ Native               | ✅ Native              | ✅ LAG() gap detection   |
| Event-time processing         | ✅ Native watermarks    | ✅ Native              | ✅ via event_time column |
| Late arrival handling         | ✅ Configurable         | ✅ Configurable        | ✅ Grace period WHERE    |
| SQL interface                 | ✅ Flink SQL            | ❌ Java/Scala API      | ✅ Standard SQL          |
| Infrastructure requirements   | Flink + Kafka cluster   | Kafka cluster          | PostgreSQL only          |
| ACID guarantees               | ❌ At-least-once        | ❌ At-least-once       | ✅ ACID (PostgreSQL)     |
| Joins with dimension tables   | Complex (async)         | Complex (KTable)       | ✅ Standard SQL JOIN     |
| Result store                  | Separate sink needed    | Separate sink needed   | ✅ Built-in (stream table)|
| Latency                       | 10ms–1s                 | 1ms–100ms              | 1s–30s (DIFF), 0ms (IMM) |
| Throughput (events/sec)       | Millions                | Millions               | ~100K (DIFF, single node)|
| Operational complexity        | Very High               | High                   | Low                      |

**When to choose pg_trickle over Flink/Kafka Streams:**
- Your event volume is < 50K events/sec per source table.
- You already use PostgreSQL and don't want additional infrastructure.
- Your team knows SQL but not the Flink/Kafka APIs.
- You need ACID guarantees and transactional correctness.
- Your windows need joins with dimension tables (trivial in SQL; complex in Flink).

**When to choose Flink/Kafka Streams over pg_trickle:**
- Your event volume exceeds PostgreSQL's write throughput ceiling.
- You need sub-100ms window latency at > 1M events/sec.
- You are already invested in a Kafka-based data platform.
- Your window operations are stateful in ways that don't map to SQL GROUP BY.

---

## When NOT to Use This Pattern

- **You need millisecond windows at very high throughput.** pg_trickle
  schedules have a minimum of ~1s. For sub-second windows at > 100K events/sec,
  a dedicated streaming engine is required.
- **Your source table is not append-only and has heavy UPDATE/DELETE traffic.**
  UPDATEs to existing event rows force GROUP_RESCAN on affected windows.
  If more than 30% of source rows are updated per cycle, DIFFERENTIAL overhead
  exceeds FULL refresh cost.
- **Your window logic is inherently stateful across sessions.** Complex
  CEP (Complex Event Processing) patterns (e.g., "find all sequences where A
  is followed by B within 5 events, then C within 30 seconds") require
  a streaming engine's native stateful operators.
- **You need exactly-once semantics end-to-end.** pg_trickle provides ACID
  within PostgreSQL, but if results are pushed to external systems, you need
  idempotent consumers.

---

## References

- Tyler Akidau et al., "The Dataflow Model" (Google, 2015) — https://research.google/pubs/the-dataflow-model/
- Tyler Akidau, "Streaming 101" — https://www.oreilly.com/radar/the-world-beyond-batch-streaming-101/
- Tyler Akidau, "Streaming 102" — https://www.oreilly.com/radar/the-world-beyond-batch-streaming-102/
- Apache Flink, "Windows" — https://nightlies.apache.org/flink/flink-docs-stable/docs/dev/datastream/operators/windows/
- Kafka Streams, "Windowing" — https://kafka.apache.org/documentation/streams/developer-guide/dsl-api.html#windowing
- PostgreSQL Window Functions — https://www.postgresql.org/docs/current/functions-window.html
- PostgreSQL `date_trunc` — https://www.postgresql.org/docs/current/functions-datetime.html
- pg_trickle SQL Reference — [SQL_REFERENCE.md](../docs/SQL_REFERENCE.md)
- pg_trickle DVM Operators — [DVM_OPERATORS.md](../docs/DVM_OPERATORS.md)
- TimescaleDB — https://www.timescale.com/
- pg_partman — https://github.com/pgpartman/pg_partman
- pg_cron — https://github.com/citusdata/pg_cron
