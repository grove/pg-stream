# pg_trickle Monitors Itself

## How the extension eats its own cooking

---

Since v0.20, pg_trickle's internal health metrics are maintained as stream tables. The extension uses itself to monitor itself.

This isn't a gimmick. It's a practical architecture decision: pg_trickle already has an engine for maintaining derived data from source tables. The extension's own operational data lives in PostgreSQL tables. Why build a separate monitoring system when you can point the same engine at its own catalog?

---

## What Self-Monitoring Tracks

When you enable self-monitoring, pg_trickle creates a set of internal stream tables that aggregate operational data:

```sql
-- Enable self-monitoring
SELECT pgtrickle.enable_self_monitoring();
```

This creates stream tables over pg_trickle's own catalog and operational tables:

### Refresh performance by stream table

```sql
-- Automatically created:
-- pgtrickle._self_refresh_stats
-- Query:
SELECT
    st.pgt_name,
    st.refresh_mode,
    COUNT(*) AS refresh_count,
    AVG(h.duration_ms) AS avg_refresh_ms,
    MAX(h.duration_ms) AS max_refresh_ms,
    SUM(h.rows_affected) AS total_rows_affected,
    MAX(h.refreshed_at) AS last_refresh
FROM pgtrickle.pgt_stream_tables st
JOIN pgtrickle.pgt_refresh_history h ON h.pgt_id = st.pgt_id
WHERE h.refreshed_at >= now() - interval '1 hour'
GROUP BY st.pgt_name, st.refresh_mode
```

This stream table tells you, in real time, how each stream table is performing: average refresh time, max refresh time, total rows processed, and when it last refreshed.

### Change buffer depth

```sql
-- pgtrickle._self_buffer_depth
-- Tracks how much unprocessed change data is queued per source table
SELECT
    source_table,
    COUNT(*) AS pending_changes,
    MIN(captured_at) AS oldest_change,
    now() - MIN(captured_at) AS max_latency
FROM pgtrickle_changes.changes_summary
GROUP BY source_table
```

If `pending_changes` is growing faster than the scheduler can drain it, this stream table surfaces the problem before it becomes a production incident.

### Error rates

```sql
-- pgtrickle._self_error_rates
-- Tracks consecutive errors and error patterns
SELECT
    st.pgt_name,
    st.consecutive_errors,
    st.status,
    h.error_message,
    h.refreshed_at AS last_error_at
FROM pgtrickle.pgt_stream_tables st
JOIN pgtrickle.pgt_refresh_history h ON h.pgt_id = st.pgt_id
WHERE h.success = false
  AND h.refreshed_at >= now() - interval '1 hour'
```

---

## Why This Matters

The typical monitoring setup for a database extension involves:

1. A Prometheus exporter that polls the extension's views
2. Grafana dashboards that visualize the metrics
3. AlertManager rules that fire when thresholds are breached

This works, but there's a lag: Prometheus scrapes every 15–30 seconds. By the time the alert fires, the problem might have been happening for a minute.

Self-monitoring stream tables are maintained continuously — every 2–5 seconds. And because they're just PostgreSQL tables, you can:

- Query them with arbitrary SQL
- Build other stream tables on top of them (meta-monitoring)
- Subscribe to them via the outbox for real-time alerts
- Expose them to any BI tool that speaks PostgreSQL

### Alerts on monitoring data

```sql
-- Alert when any stream table's refresh time exceeds 500ms
SELECT pgtrickle.create_stream_table(
    'pgtrickle._self_slow_refreshes',
    $$SELECT pgt_name, avg_refresh_ms, max_refresh_ms
      FROM pgtrickle._self_refresh_stats
      WHERE avg_refresh_ms > 500$$,
    schedule => '5s', refresh_mode => 'DIFFERENTIAL'
);

-- Enable outbox so the relay can push alerts to Slack/PagerDuty
SELECT pgtrickle.enable_outbox('pgtrickle._self_slow_refreshes');
```

Now when a stream table starts refreshing slowly, the monitoring stream table picks it up within 5 seconds, and the outbox delivers a notification to your alerting system.

---

## The Recursion Question

"If pg_trickle monitors itself with stream tables, who monitors the monitoring stream tables?"

Fair question. The self-monitoring stream tables are maintained by the same scheduler as user-defined stream tables. If the scheduler is completely dead, the monitoring tables aren't updated either.

pg_trickle handles this with a separate code path:

1. **The scheduler's heartbeat** is written directly to a catalog table (not via a stream table). External monitoring (Prometheus, health check endpoints) reads this heartbeat.

2. **Self-monitoring stream tables** track performance and trends — they're for observability, not liveness. The liveness check is the heartbeat.

3. **`pgtrickle.health_check()`** returns a summary that combines both: the heartbeat (is the scheduler running?) and the self-monitoring data (how is it performing?).

```sql
SELECT * FROM pgtrickle.health_check();
-- Returns: scheduler_running, total_stream_tables, active_count,
--          error_count, avg_refresh_ms, max_staleness, ...
```

---

## Integration with Prometheus and Grafana

Self-monitoring doesn't replace Prometheus — it complements it. pg_trickle also exposes metrics in Prometheus format:

```
# Prometheus endpoint (built-in or via pgtrickle-relay)
pgtrickle_refresh_duration_seconds{stream_table="order_totals",mode="DIFFERENTIAL"} 0.012
pgtrickle_refresh_rows_affected{stream_table="order_totals"} 47
pgtrickle_change_buffer_depth{source_table="orders"} 123
pgtrickle_scheduler_lag_seconds 0.3
pgtrickle_stream_table_staleness_seconds{stream_table="order_totals"} 2.1
```

The difference is granularity: Prometheus gives you time-series data at scrape resolution (typically 15s). Self-monitoring stream tables give you real-time aggregates at refresh resolution (1–5s). Use both.

---

## Disabling Self-Monitoring

If you don't need it — or if you're running in a resource-constrained environment — you can disable it:

```sql
SELECT pgtrickle.disable_self_monitoring();
```

This drops the internal stream tables and frees the scheduler slots they were using. The Prometheus metrics and health check endpoints continue working — they don't depend on self-monitoring.

---

## The Dogfooding Effect

Building self-monitoring on top of the same engine forces the pg_trickle team to care about edge cases that only surface under real load. If the monitoring stream tables are slow, that's a bug in the engine. If they produce incorrect results, that's a correctness issue. If they consume too many resources, that's a scheduler efficiency problem.

Every improvement to the self-monitoring stream tables is an improvement to the engine itself.
