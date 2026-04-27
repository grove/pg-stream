[← Back to Blog Index](README.md)

# Error Budgets for Stream Tables

## SRE-style freshness monitoring with p50/p99 latency, staleness tracking, and budget consumption

---

Your team has an SLA: "the dashboard must reflect data no older than 30 seconds." You've set the stream table schedule to 5 seconds. That should leave plenty of headroom. But is it?

What if the stream table occasionally takes 20 seconds to refresh because of a complex join? What if it fails 3 times in a row and the scheduler suspends it? What if the change buffer grows large enough to cause a FULL fallback?

You won't know unless you measure. pg_trickle's `sla_summary()` function provides SRE-style metrics: percentile latencies, freshness lag, error counts, and an error budget that tells you how much headroom you have before your SLA is violated.

---

## The SLA Summary API

```sql
SELECT * FROM pgtrickle.sla_summary('dashboard_metrics');
```

```
 stream_table      | p50_refresh_ms | p99_refresh_ms | freshness_lag_s | error_count | error_budget_pct | window
-------------------+----------------+----------------+-----------------+-------------+------------------+---------
 dashboard_metrics | 4.8            | 23.1           | 2.3             | 1           | 94.2             | 1h
```

### What Each Metric Means

**p50_refresh_ms:** The median refresh duration over the measurement window. If this is close to your schedule interval, you're refreshing slower than you're scheduling.

**p99_refresh_ms:** The 99th percentile refresh duration. This is your worst-case performance (excluding true outliers). If your schedule is 5 seconds and p99 is 23 seconds, 1% of your refreshes are taking nearly 5× longer than expected.

**freshness_lag_s:** The current staleness — time since the last successful refresh completed. If this exceeds your SLA, the data is stale right now.

**error_count:** Number of failed refreshes in the measurement window. Each failure means one missed refresh cycle.

**error_budget_pct:** The remaining error budget as a percentage. This is the key metric:

$$\text{error\_budget\_pct} = 100 \times \left(1 - \frac{\text{actual\_staleness\_violations}}{\text{allowed\_staleness\_violations}}\right)$$

At 100%, you've had zero violations. At 0%, you've exhausted your budget. Below 0%, you're in breach.

---

## Defining SLAs

pg_trickle doesn't enforce SLAs — it measures compliance. The SLA is defined by your team's freshness requirement.

The `sla_summary()` function accepts an optional window parameter:

```sql
-- Last hour (default)
SELECT * FROM pgtrickle.sla_summary('dashboard_metrics');

-- Last 24 hours
SELECT * FROM pgtrickle.sla_summary('dashboard_metrics', window => '24h');

-- Last 7 days
SELECT * FROM pgtrickle.sla_summary('dashboard_metrics', window => '7d');
```

For a 30-second freshness SLA measured over 1 hour:
- There are 120 expected refresh opportunities (at 30-second intervals).
- If 6 of those opportunities were missed (refresh took too long or failed), that's a 5% violation rate → 95% error budget remaining.

---

## Alerting on Error Budget

Combine `sla_summary()` with reactive subscriptions or pg_cron to alert when the budget is low:

```sql
-- pg_cron: check error budget every 5 minutes
SELECT cron.schedule('sla-check', '*/5 * * * *', $$
  DO $$
  DECLARE
    budget NUMERIC;
  BEGIN
    SELECT error_budget_pct INTO budget
    FROM pgtrickle.sla_summary('dashboard_metrics');

    IF budget < 50 THEN
      PERFORM pg_notify('sla_warning',
        format('dashboard_metrics error budget at %s%%', budget));
    END IF;
    IF budget < 10 THEN
      PERFORM pg_notify('sla_critical',
        format('dashboard_metrics error budget at %s%%', budget));
    END IF;
  END $$;
$$);
```

A listener picks up the NOTIFY and routes it to PagerDuty, Slack, or your alerting system.

---

## Setting Meaningful SLAs

Different stream tables serve different purposes. A real-time fraud detection table and a weekly summary report shouldn't have the same SLA.

Recommended tiers:

| Tier | Freshness Target | Example |
|------|-----------------|---------|
| Critical | < 5 seconds | Fraud detection, inventory levels, live pricing |
| Standard | < 30 seconds | Dashboards, operational metrics, search indexes |
| Background | < 5 minutes | Daily reports, batch analytics, archival |

For each tier, set the stream table schedule to ~⅓ of the freshness target. This gives 3 refresh attempts before the SLA is breached:

| Tier | Freshness Target | Schedule |
|------|-----------------|----------|
| Critical | 5s | 1–2s |
| Standard | 30s | 10s |
| Background | 5m | 1–2m |

---

## Diagnosing Budget Consumption

When the error budget is dropping, diagnose the cause:

### 1. Check refresh history

```sql
SELECT
  refresh_id,
  refresh_mode,
  duration_ms,
  rows_changed,
  status,
  error_message
FROM pgtrickle.get_refresh_history('dashboard_metrics')
WHERE status != 'success' OR duration_ms > 10000
ORDER BY refresh_id DESC
LIMIT 20;
```

Look for:
- `status = 'error'` — failed refreshes. Check `error_message`.
- `duration_ms` spikes — slow refreshes that may exceed the SLA window.
- `refresh_mode = 'FULL'` — fallbacks from DIFFERENTIAL to FULL, which are usually slower.

### 2. Check for mode fallbacks

```sql
SELECT
  COUNT(*) FILTER (WHERE refresh_mode = 'DIFFERENTIAL') AS differential_count,
  COUNT(*) FILTER (WHERE refresh_mode = 'FULL') AS full_count,
  AVG(duration_ms) FILTER (WHERE refresh_mode = 'DIFFERENTIAL') AS avg_diff_ms,
  AVG(duration_ms) FILTER (WHERE refresh_mode = 'FULL') AS avg_full_ms
FROM pgtrickle.get_refresh_history('dashboard_metrics')
WHERE created_at > NOW() - INTERVAL '1 hour';
```

If FULL refreshes are frequent and slow, investigate why DIFFERENTIAL is falling back (change ratio too high, spilling, etc.).

### 3. Check change buffer sizes

```sql
SELECT * FROM pgtrickle.pgt_cdc_status
WHERE pgt_name = 'dashboard_metrics';
```

Large change buffers → larger deltas → slower DIFFERENTIAL refreshes → potential SLA violations.

---

## Error Budget Across All Stream Tables

For a system-wide view:

```sql
SELECT
  s.name,
  s.schedule,
  sla.p50_refresh_ms,
  sla.p99_refresh_ms,
  sla.freshness_lag_s,
  sla.error_budget_pct,
  CASE
    WHEN sla.error_budget_pct >= 95 THEN 'healthy'
    WHEN sla.error_budget_pct >= 50 THEN 'warning'
    ELSE 'critical'
  END AS status
FROM pgtrickle.pgt_stream_tables s
CROSS JOIN LATERAL pgtrickle.sla_summary(s.name) sla
ORDER BY sla.error_budget_pct ASC;
```

This ranks all stream tables by error budget health. Tables at the top of the list need attention.

---

## Prometheus Integration

pg_trickle exports SLA metrics as Prometheus gauges:

```
pg_trickle_refresh_p50_ms{stream_table="dashboard_metrics"} 4.8
pg_trickle_refresh_p99_ms{stream_table="dashboard_metrics"} 23.1
pg_trickle_freshness_lag_seconds{stream_table="dashboard_metrics"} 2.3
pg_trickle_error_budget_pct{stream_table="dashboard_metrics"} 94.2
```

Use these in Grafana dashboards and alerting rules:

```yaml
# Prometheus alert rule
- alert: StreamTableSLABreach
  expr: pg_trickle_error_budget_pct < 20
  for: 5m
  labels:
    severity: critical
  annotations:
    summary: "Stream table {{ $labels.stream_table }} error budget below 20%"
```

---

## Summary

`sla_summary()` gives you SRE-style visibility into stream table health: percentile latencies, freshness lag, error counts, and error budget.

Set SLAs per tier (critical/standard/background). Monitor the error budget. Alert when it drops below a threshold. Diagnose with refresh history, mode fallback analysis, and change buffer checks.

The goal: know you're meeting your freshness SLA *before* a customer asks why the dashboard is stale. The error budget tells you exactly how much headroom you have left.
