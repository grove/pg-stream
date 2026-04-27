# Temporal Stream Tables: Time-Windowed Views That Update Themselves

## Handling the "last 7 days" problem without cron

---

Here's a question that breaks most incremental view maintenance systems:

```sql
SELECT region, SUM(amount) AS revenue
FROM orders
WHERE created_at >= now() - interval '7 days'
GROUP BY region;
```

The problem isn't the query. The problem is that the result changes over time even when no data changes. At midnight, yesterday's orders cross the 7-day boundary and fall out of the window. The aggregate changes — not because a row was inserted or deleted, but because *time passed*.

A trigger-based CDC system doesn't see this. Nothing was written to the `orders` table. No trigger fired. The stream table is stale, and nobody told it.

This is the temporal IVM problem. pg_trickle solves it with temporal stream tables.

---

## What Goes Wrong Without Temporal Awareness

Consider a stream table for "revenue in the last 24 hours":

```sql
SELECT pgtrickle.create_stream_table(
    'revenue_last_24h',
    $$SELECT region, SUM(amount) AS revenue
      FROM orders
      WHERE created_at >= now() - interval '24 hours'
      GROUP BY region$$,
    schedule => '5s', refresh_mode => 'DIFFERENTIAL'
);
```

At 3:00 PM, this correctly shows revenue from 3:00 PM yesterday to now.

At 3:05 PM, if no new orders came in, the change buffer is empty. The scheduler says "nothing to do" and skips the refresh. But the correct result changed — orders from 3:00–3:05 PM yesterday should have fallen out of the window.

By midnight, the stream table might be showing "last 24 hours" revenue that actually includes data from 36 hours ago. The longer you go without an insert, the more stale the temporal window becomes.

The brute-force fix is to run a full refresh every cycle regardless of the change buffer. But that defeats the purpose of incremental maintenance — you're scanning the entire table every 5 seconds.

---

## How Temporal Stream Tables Work

pg_trickle's temporal mode adds a time-based eviction step to the refresh cycle:

```sql
SELECT pgtrickle.create_stream_table(
    'revenue_last_24h',
    $$SELECT region, SUM(amount) AS revenue
      FROM orders
      WHERE created_at >= now() - interval '24 hours'
      GROUP BY region$$,
    schedule     => '5s',
    refresh_mode => 'DIFFERENTIAL',
    temporal_mode => 'sliding_window'
);
```

With `temporal_mode => 'sliding_window'`, the refresh cycle does two things:

1. **Process the change buffer** — normal differential maintenance for new/updated/deleted rows.
2. **Evict expired rows** — identify rows in the stream table whose source data has fallen outside the time window, and compute the aggregate delta from removing them.

The eviction step doesn't scan the entire source table. It uses the stream table's own data and the known window boundary to identify expired contributions.

---

## The Eviction Delta

For a `SUM(amount)` grouped by `region`:

At 3:05 PM, the window boundary is 3:05 PM yesterday. Orders from 3:00–3:05 PM yesterday need to be subtracted from the aggregate. The eviction step:

1. Identifies orders in the change buffer with `created_at` between the old boundary (3:00 PM yesterday) and the new boundary (3:05 PM yesterday).
2. Computes the delta: subtract those orders' amounts from their respective region groups.
3. Applies the eviction delta alongside any change-buffer delta.

This is efficient: the eviction only processes the thin slice of data that crossed the window boundary since the last refresh. For a 5-second refresh cycle, that's 5 seconds' worth of source data to evict — typically a small number of rows.

---

## Use Cases

### Rolling 7-Day Revenue

```sql
SELECT pgtrickle.create_stream_table(
    'revenue_7d_rolling',
    $$SELECT
        region,
        date_trunc('hour', created_at) AS hour,
        SUM(amount) AS revenue,
        COUNT(*) AS order_count
      FROM orders
      WHERE created_at >= now() - interval '7 days'
      GROUP BY region, date_trunc('hour', created_at)$$,
    schedule      => '10s',
    refresh_mode  => 'DIFFERENTIAL',
    temporal_mode => 'sliding_window'
);
```

Every 10 seconds, this stream table:
- Adds aggregates from new orders.
- Removes aggregates from orders that just passed the 7-day mark.

### Active Users (Last 15 Minutes)

```sql
SELECT pgtrickle.create_stream_table(
    'active_users',
    $$SELECT
        COUNT(DISTINCT user_id) AS active_count,
        COUNT(*) AS event_count
      FROM user_events
      WHERE occurred_at >= now() - interval '15 minutes'$$,
    schedule      => '2s',
    refresh_mode  => 'DIFFERENTIAL',
    temporal_mode => 'sliding_window'
);
```

This gives you a live "active users in the last 15 minutes" count that updates every 2 seconds. At 10:15:02, it counts events from 10:00:02 to 10:15:02. At 10:15:04, it counts events from 10:00:04 to 10:15:04. The window slides continuously.

### SLA Monitoring (Last Hour)

```sql
SELECT pgtrickle.create_stream_table(
    'sla_compliance',
    $$SELECT
        service_name,
        COUNT(*) AS total_requests,
        COUNT(*) FILTER (WHERE response_time_ms > 500) AS slow_requests,
        COUNT(*) FILTER (WHERE response_time_ms > 500)::float / COUNT(*)::float AS error_rate
      FROM request_log
      WHERE logged_at >= now() - interval '1 hour'
      GROUP BY service_name$$,
    schedule      => '5s',
    refresh_mode  => 'DIFFERENTIAL',
    temporal_mode => 'sliding_window'
);
```

---

## What Temporal Mode Costs

Temporal stream tables are slightly more expensive than non-temporal ones:

| Aspect | Non-temporal | Temporal |
|---|---|---|
| Refresh with changes | Delta only | Delta + eviction |
| Refresh with no changes | Skipped | Eviction only |
| Storage | Stream table only | Stream table + boundary index |
| CPU per cycle | Proportional to delta | Proportional to delta + evicted rows |

The key difference: non-temporal stream tables skip the refresh cycle entirely when the change buffer is empty. Temporal stream tables always run the eviction step, even with an empty change buffer — because time passing is itself a change.

For most workloads, the eviction cost is small. The number of rows crossing the window boundary per cycle is bounded by: (source table insertion rate) × (refresh interval). For a table receiving 1,000 rows/second with a 5-second refresh cycle, the eviction step processes at most 5,000 rows. Typically less, because rows don't all arrive at a uniform rate.

---

## Non-Temporal Alternatives

If you don't need a continuously-sliding window, you can use a fixed window with a simpler approach:

```sql
-- Fixed daily window: "today's orders"
SELECT pgtrickle.create_stream_table(
    'revenue_today',
    $$SELECT region, SUM(amount) AS revenue
      FROM orders
      WHERE created_at >= date_trunc('day', now())
      GROUP BY region$$,
    schedule     => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
```

This doesn't need `temporal_mode` because `date_trunc('day', now())` changes only once per day (at midnight). For the other 86,399 seconds, the window boundary is static and normal DIFFERENTIAL mode works. At midnight, a full refresh resets the window.

The distinction: sliding windows (`now() - interval '7 days'`) shift every second. Fixed windows (`date_trunc('day', now())`) shift at discrete boundaries. Sliding windows need temporal mode. Fixed windows usually don't.

---

## Temporal Mode vs. Scheduled Full Refresh

You could approximate temporal behavior by running a full refresh every N seconds:

```sql
SELECT pgtrickle.create_stream_table(
    'revenue_last_24h',
    $$SELECT region, SUM(amount) AS revenue
      FROM orders
      WHERE created_at >= now() - interval '24 hours'
      GROUP BY region$$,
    schedule     => '30s',
    refresh_mode => 'FULL'
);
```

This works, but:
- Every refresh scans the entire 24-hour window. For a table with millions of orders, this is expensive.
- The refresh takes time proportional to the window size, not the change rate.
- You can't set the schedule below a few seconds without saturating the database.

Temporal mode with DIFFERENTIAL is the efficient version: it processes only the new rows (delta) and the expired rows (eviction), not the entire window.
