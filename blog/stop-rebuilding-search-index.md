# Stop Rebuilding Your Search Index at 3am

## pg_trickle's scheduler, SLA tiers, and how to tune refresh for your workload

---

If you're running periodic `REFRESH MATERIALIZED VIEW` or custom batch jobs to keep derived data fresh, you've made a decision — probably implicitly — about when to do that work and how to prioritize it.

Most teams make this decision once ("run it every 15 minutes at :00 and :15") and never revisit it. The result is a cron job that runs at 3am, takes 40 minutes on a big table, and occasionally conflicts with the morning ETL load at 7am. The on-call rotation includes "check if the refresh job finished before the standup."

pg_trickle's scheduler is designed to replace that mental model. This post explains how it works, what the tuning knobs are, and how to use SLA tiers to serve different workloads correctly.

---

## How the Scheduler Works

pg_trickle runs a background worker — a PostgreSQL background process that starts with the database and persists for its lifetime. The scheduler is one of the background worker's responsibilities.

At each tick, the scheduler checks all registered stream tables and determines which ones are due for a refresh. "Due" is determined by:
1. The stream table's `schedule` — how often it should refresh
2. Whether the change buffer has data to process
3. The current resource usage (CPU, I/O) — controlled by backpressure GUCs
4. The SLA tier's priority rules

The scheduler doesn't run all refreshes at exactly their scheduled time. It maintains a priority queue and processes refreshes in priority order, subject to concurrency limits. High-priority stream tables preempt lower-priority ones when they're due.

---

## The schedule Parameter

The simplest knob:

```sql
SELECT pgtrickle.create_stream_table(
  name     => 'live_orders',
  query    => $$ ... $$,
  schedule => '5 seconds'     -- refresh up to every 5 seconds
);

SELECT pgtrickle.create_stream_table(
  name     => 'daily_summary',
  query    => $$ ... $$,
  schedule => '1 hour'        -- refresh up to every hour
);
```

The `schedule` is a maximum interval, not a fixed interval. If the change buffer is empty (nothing changed), the refresh is skipped. This means a stream table that says "refresh every 5 seconds" but whose source data hasn't changed will actually skip most cycles.

This is important for cost. An empty refresh cycle costs almost nothing — just a check of the change buffer. You can set aggressive schedules on stream tables that are usually quiet without paying a significant overhead for the quiet periods.

---

## SLA Tiers

Different stream tables have different importance. A user-facing search corpus that drives revenue needs different treatment from a background analytics table used by an internal reporting dashboard.

pg_trickle's SLA tiers let you declare this importance explicitly:

```sql
SELECT pgtrickle.alter_stream_table(
  'live_product_search',
  sla_tier => 'critical'
);

SELECT pgtrickle.alter_stream_table(
  'daily_revenue_summary',
  sla_tier => 'standard'
);

SELECT pgtrickle.alter_stream_table(
  'historical_analytics',
  sla_tier => 'background'
);
```

Three built-in tiers, in descending priority:

| Tier | Priority | When to use |
|------|----------|-------------|
| `critical` | Highest | User-facing, latency-sensitive. Preempts everything. |
| `standard` | Default | Normal operational data. Runs when resources allow. |
| `background` | Lowest | Batch analytics, archives. Runs in idle time. |

The scheduler always processes `critical` refreshes before `standard`, and `standard` before `background`. Under load — high write volume, many stream tables competing for refresh bandwidth — background tables may be delayed significantly, but critical tables are always processed first.

---

## Concurrency: How Many Refreshes Run in Parallel

By default, pg_trickle runs two parallel refresh workers. You can tune this:

```sql
-- In postgresql.conf or SET:
pg_trickle.max_parallel_workers = 4
```

With 4 workers, up to 4 stream tables can refresh simultaneously. The scheduler assigns refreshes to idle workers and respects the SLA priority ordering.

Be careful not to set this too high. Each refresh worker opens a PostgreSQL connection and may perform index lookups, writes, and MERGE statements on stream tables. Too many concurrent refreshes can create I/O saturation or lock contention on heavily shared tables.

For most workloads, 2–4 workers is appropriate. For systems with many independent stream tables (tens to hundreds) and fast storage, more workers may help.

---

## Backpressure: Preventing Refresh from Overwhelming Your Database

The harder scheduling problem is protecting your database from refresh overhead during high-write periods.

When source tables are being written very quickly — a bulk import, a traffic spike, a viral event — the change buffers fill up rapidly. If pg_trickle tries to process all those changes immediately, the refresh workers compete with the application for I/O bandwidth and lock resources.

The backpressure mechanism limits this:

```sql
-- In postgresql.conf:
pg_trickle.backpressure_enabled = on
pg_trickle.backpressure_max_lag_mb = 128       -- pause if WAL lag exceeds 128MB
pg_trickle.backpressure_lag_check_interval = 5s -- check every 5 seconds
```

When WAL lag exceeds the threshold — a sign that the database is under write load — the scheduler pauses background-tier refreshes and slows standard-tier refreshes. Critical-tier refreshes continue at full speed.

The effect: during a bulk import or traffic spike, your user-facing search corpora stay fresh (critical tier). Your analytics summaries fall slightly behind (standard tier slows). Your background reports accumulate a backlog that drains after the spike passes. The application never sees resource contention from the refresh workers.

---

## Monitoring the Scheduler

The simplest view:

```sql
SELECT
  name,
  sla_tier,
  schedule,
  last_refresh_at,
  EXTRACT(EPOCH FROM (NOW() - last_refresh_at))::int AS staleness_secs,
  rows_changed_last_cycle,
  avg_refresh_ms,
  pending_change_rows
FROM pgtrickle.stream_table_status()
ORDER BY staleness_secs DESC;
```

When something's off:

```sql
-- Which tables have fallen behind their schedule?
SELECT name, schedule, staleness_secs
FROM pgtrickle.stream_table_status()
WHERE staleness_secs > EXTRACT(EPOCH FROM schedule::interval) * 3;

-- Is the change buffer accumulating a backlog?
SELECT source_table, pending_rows, oldest_change_at,
       EXTRACT(EPOCH FROM (NOW() - oldest_change_at))::int AS backlog_age_secs
FROM pgtrickle.change_buffer_status()
ORDER BY pending_rows DESC;
```

A high `backlog_age_secs` on a source table means the scheduler is behind. This happens during traffic spikes (expected) or when the refresh is taking too long (investigate).

---

## Diagnosing Slow Refreshes

When a stream table's refresh is consistently slower than expected:

```sql
-- Refresh history with timing breakdown
SELECT
  refreshed_at,
  duration_ms,
  rows_changed,
  delta_compute_ms,
  merge_apply_ms,
  index_update_ms
FROM pgtrickle.refresh_history('slow_stream_table')
ORDER BY refreshed_at DESC
LIMIT 20;
```

The three timing components:
- `delta_compute_ms`: Time to compute the delta (join lookups, aggregate updates)
- `merge_apply_ms`: Time to apply the delta to the stream table via MERGE
- `index_update_ms`: Time for index maintenance after the MERGE

If `delta_compute_ms` is high, look at missing indexes on source tables' join columns. If `merge_apply_ms` is high, the delta is large (many rows changed in one cycle) — consider a more frequent schedule to keep deltas smaller. If `index_update_ms` is high, the stream table has expensive indexes (large HNSW, many B-tree indexes) — this is expected and should be factored into your schedule.

---

## A Real Tuning Example

Starting configuration, before tuning:
- 15 stream tables, all with default `schedule = '1 minute'`
- 2 refresh workers
- No SLA tiers set
- User complaints: search results sometimes 3–5 minutes stale during peak hours

Post-tuning:

```sql
-- User-facing search: critical tier, aggressive schedule
SELECT pgtrickle.alter_stream_table(
  'product_search', sla_tier => 'critical', schedule => '5 seconds'
);

-- Operational dashboards: standard tier
SELECT pgtrickle.alter_stream_table(
  'support_metrics', sla_tier => 'standard', schedule => '15 seconds'
);
SELECT pgtrickle.alter_stream_table(
  'inventory_status', sla_tier => 'standard', schedule => '30 seconds'
);

-- Analytics: background tier, let them be a few minutes stale
SELECT pgtrickle.alter_stream_table(
  'daily_revenue', sla_tier => 'background', schedule => '5 minutes'
);
SELECT pgtrickle.alter_stream_table(
  'historical_funnel', sla_tier => 'background', schedule => '15 minutes'
);

-- Increase workers to handle the volume
ALTER SYSTEM SET pg_trickle.max_parallel_workers = 4;
SELECT pg_reload_conf();

-- Enable backpressure for bulk-import protection
ALTER SYSTEM SET pg_trickle.backpressure_enabled = on;
ALTER SYSTEM SET pg_trickle.backpressure_max_lag_mb = 64;
SELECT pg_reload_conf();
```

Result: `product_search` is now refreshed every 5 seconds with critical priority. During peak load, it maintains that cadence while background analytics tables fall a few minutes behind. User complaints about stale search results: zero.

---

## The 3am Problem

The reason index rebuilds and MV refreshes happen at 3am is that they're too expensive to run during the day. The solution is to make the per-cycle cost small enough that it doesn't matter when it runs.

Differential refreshes with small deltas accomplish this. A 5-second cycle that processes 50 changed rows takes 10–20ms. You can run that continuously, around the clock, and it's invisible in your I/O metrics.

The 3am batch window is a symptom of using the wrong refresh model — full scans on a schedule. When you move to incremental, the concept of "a time when it's safe to refresh" goes away. The work is continuous and small, not periodic and large.

This also means the index is always current. Not "as of midnight last night" current. Not "as of the last time someone remembered to kick off the job" current. Always current, within the configured schedule.

The 3am maintenance window goes away. The on-call step disappears from the runbook. The cron job gets deleted. The monitoring alert for "did the job finish before market open" — gone.

That's the real value proposition of continuous incremental maintenance. Not just speed. The elimination of an entire class of operational work.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
