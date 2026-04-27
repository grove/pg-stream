[← Back to Blog Index](README.md)

# Structured Logging and OpenTelemetry for Stream Tables

## When grep isn't enough: JSON events, correlation IDs, and observability integration

---

Your stream table failed. The log says:

```
ERROR: stream table 'order_summary' refresh failed: division by zero
```

OK. But which refresh cycle? What was the change ratio? How long did it run before failing? Was it DIFFERENTIAL or FULL? Which source tables had changes?

With `pg_trickle.log_format = text` (the default), these questions require correlating multiple log lines by timestamp, hoping they're adjacent, and parsing free-form text.

With `pg_trickle.log_format = json`, every log event is a structured JSON object with consistent fields. Pipe it to Loki, Datadog, Elasticsearch, or any log aggregator, and every question has an indexed answer.

---

## Enabling JSON Logging

```sql
-- In postgresql.conf or via ALTER SYSTEM
ALTER SYSTEM SET pg_trickle.log_format = 'json';
SELECT pg_reload_conf();
```

After reload, pg_trickle's log output changes from:

```
LOG: pg_trickle: refreshing 'order_summary' (DIFFERENTIAL, 42 changes)
LOG: pg_trickle: refresh complete 'order_summary' (4.7ms, 3 rows changed)
```

To:

```json
{"event":"refresh_start","pgt_name":"order_summary","pgt_id":17,"cycle_id":"c-20260427-101500-017","refresh_mode":"DIFFERENTIAL","changes_pending":42,"ts":"2026-04-27T10:15:00.123Z"}
{"event":"refresh_complete","pgt_name":"order_summary","pgt_id":17,"cycle_id":"c-20260427-101500-017","refresh_mode":"DIFFERENTIAL","duration_ms":4.7,"rows_inserted":2,"rows_updated":1,"rows_deleted":0,"ts":"2026-04-27T10:15:00.128Z"}
```

---

## Event Taxonomy

pg_trickle emits structured events for all major operations:

| Event | When | Key Fields |
|-------|------|------------|
| `refresh_start` | Refresh begins | pgt_name, cycle_id, refresh_mode, changes_pending |
| `refresh_complete` | Refresh succeeds | duration_ms, rows_inserted/updated/deleted |
| `refresh_error` | Refresh fails | error_code, error_message, duration_ms |
| `mode_fallback` | DIFFERENTIAL → FULL | reason, change_ratio, threshold |
| `cdc_transition` | Trigger → WAL (or back) | direction, source_table, reason |
| `scheduler_cycle` | Scheduler wakes | tables_checked, tables_refreshed, loop_duration_ms |
| `worker_dispatch` | Worker assigned to refresh | pgt_name, worker_id, database |
| `scc_converge` | Cycle converges | scc_id, iterations, tables |
| `scc_timeout` | Cycle hits max iterations | scc_id, iterations, remaining_changes |
| `drain_start` | Drain mode entered | inflight_count |
| `drain_complete` | Drain finished | drain_duration_ms |
| `cache_evict` | L0 cache entry evicted | pgt_id, reason, cache_size |
| `spill_detected` | Delta query spilled to disk | pgt_name, temp_blocks, consecutive_count |
| `backpressure_engaged` | WAL slot lag exceeded | source_table, lag_bytes, threshold |
| `backpressure_released` | WAL slot lag recovered | source_table, lag_bytes |

---

## Correlation via cycle_id

Every refresh cycle gets a unique `cycle_id`. All events within that cycle — start, complete/error, mode fallback, spill detection — share the same `cycle_id`.

This lets you trace the full lifecycle of a single refresh:

```bash
# In Loki/Grafana
{job="pg_trickle"} | json | cycle_id="c-20260427-101500-017"
```

```json
{"event":"refresh_start","cycle_id":"c-20260427-101500-017","refresh_mode":"DIFFERENTIAL",...}
{"event":"spill_detected","cycle_id":"c-20260427-101500-017","temp_blocks":2048,...}
{"event":"mode_fallback","cycle_id":"c-20260427-101500-017","reason":"spill_limit",...}
{"event":"refresh_complete","cycle_id":"c-20260427-101500-017","refresh_mode":"FULL","duration_ms":450,...}
```

One `cycle_id` tells the full story: the refresh started as DIFFERENTIAL, spilled to disk, fell back to FULL, and completed in 450ms.

---

## Integration with Log Aggregators

### Loki (via promtail)

```yaml
# promtail config
scrape_configs:
  - job_name: pg_trickle
    static_configs:
      - targets: [localhost]
        labels:
          job: pg_trickle
          __path__: /var/log/postgresql/postgresql-*.log
    pipeline_stages:
      - match:
          selector: '{job="pg_trickle"}'
          stages:
            - json:
                expressions:
                  event: event
                  pgt_name: pgt_name
                  cycle_id: cycle_id
                  duration_ms: duration_ms
            - labels:
                event:
                pgt_name:
```

### Datadog

```yaml
# datadog agent config
logs:
  - type: file
    path: /var/log/postgresql/postgresql-*.log
    service: pg_trickle
    source: postgresql
    log_processing_rules:
      - type: multi_line
        name: pg_trickle_json
        pattern: '^\{"event":'
```

### Elasticsearch

```json
// Filebeat config
{
  "filebeat.inputs": [{
    "type": "log",
    "paths": ["/var/log/postgresql/postgresql-*.log"],
    "json.keys_under_root": true,
    "json.add_error_key": true,
    "fields": {"service": "pg_trickle"}
  }]
}
```

---

## Useful Queries

Once events are in your log aggregator, common queries:

**Slow refreshes (>1 second):**
```
{job="pg_trickle"} | json | event="refresh_complete" | duration_ms > 1000
```

**Failed refreshes:**
```
{job="pg_trickle"} | json | event="refresh_error"
```

**Mode fallbacks (DIFFERENTIAL → FULL):**
```
{job="pg_trickle"} | json | event="mode_fallback" | reason != ""
```

**Refresh frequency by stream table:**
```
count_over_time({job="pg_trickle"} | json | event="refresh_complete" [5m]) by (pgt_name)
```

**P99 refresh duration over time:**
```
quantile_over_time(0.99, {job="pg_trickle"} | json | event="refresh_complete" | unwrap duration_ms [5m]) by (pgt_name)
```

---

## Text Mode: Still the Default

JSON logging is opt-in. The default `text` format is human-readable and works fine for:
- Development and local testing
- Small deployments where you `tail -f` the logs
- Environments without a log aggregator

Switch to JSON when you need:
- Structured querying across thousands of refresh events
- Alerting on specific event types
- Correlation across refresh cycles
- Integration with observability platforms (Grafana, Datadog, Splunk)

---

## OpenTelemetry Compatibility

The JSON format is designed to be compatible with OpenTelemetry's log data model. The `ts` field uses ISO 8601 timestamps. The `event` field maps to the OTel event name. Custom fields map to OTel attributes.

If you're using the OpenTelemetry Collector, you can ingest pg_trickle's JSON logs directly:

```yaml
# otel-collector config
receivers:
  filelog:
    include: [/var/log/postgresql/*.log]
    operators:
      - type: json_parser
        timestamp:
          parse_from: attributes.ts
          layout: '%Y-%m-%dT%H:%M:%S.%LZ'

exporters:
  otlp:
    endpoint: "tempo:4317"
```

This feeds pg_trickle events into your tracing backend alongside application traces. The `cycle_id` can be used as a span ID for correlation.

---

## Summary

`pg_trickle.log_format = json` turns log output into structured, queryable events. Every refresh cycle gets a `cycle_id` for end-to-end correlation. Events cover the full lifecycle: refresh start/complete/error, mode fallbacks, CDC transitions, spill detection, backpressure, and scheduler cycles.

Pipe to Loki, Datadog, or Elasticsearch for structured querying. Use the event taxonomy to build dashboards and alerts. Use `cycle_id` to trace individual refresh cycles from start to finish.

The default text format is fine for development. Switch to JSON when you need real observability.
