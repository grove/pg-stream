# pg_trickle Production Monitoring Guide

This directory contains guidance for production Prometheus/Grafana deployments
for pg_trickle. The `monitoring/` root contains a demo compose stack for local
use — **do not** use that configuration directly in production.

---

## Architecture Overview

```
PostgreSQL + pg_trickle
        │
        │  custom SQL queries (read-only role)
        ▼
postgres_exporter (least-privilege)
        │
        │  /metrics (TLS-protected)
        ▼
   Prometheus
        │
        │  data source (TLS-protected)
        ▼
    Grafana
```

---

## 1. Least-Privilege Exporter Role

Create a dedicated monitoring user with only the permissions needed to read
pg_trickle metrics:

```sql
-- Run as superuser once
CREATE ROLE pg_trickle_monitor WITH LOGIN PASSWORD 'change-me';
GRANT CONNECT ON DATABASE app TO pg_trickle_monitor;
GRANT USAGE ON SCHEMA pgtrickle TO pg_trickle_monitor;
GRANT SELECT ON ALL TABLES IN SCHEMA pgtrickle TO pg_trickle_monitor;
ALTER DEFAULT PRIVILEGES IN SCHEMA pgtrickle
    GRANT SELECT ON TABLES TO pg_trickle_monitor;

-- Allow executing monitoring SQL functions
GRANT EXECUTE ON FUNCTION pgtrickle.health_check() TO pg_trickle_monitor;
GRANT EXECUTE ON FUNCTION pgtrickle.metrics_summary() TO pg_trickle_monitor;
GRANT EXECUTE ON FUNCTION pgtrickle.preflight() TO pg_trickle_monitor;
GRANT EXECUTE ON FUNCTION pgtrickle.worker_pool_status() TO pg_trickle_monitor;
```

---

## 2. postgres_exporter Configuration

Use `POSTGRES_EXPORTER_DATA_SOURCE_NAME` with a non-superuser role:

```bash
DATA_SOURCE_NAME="postgresql://pg_trickle_monitor:password@db:5432/app?sslmode=require"
```

Recommended custom queries file (`queries.yaml`):

```yaml
pg_trickle_stream_table_lag:
  query: |
    SELECT name, last_refresh_lag_ms, status
    FROM pgtrickle.pgt_status()
  metrics:
    - name:
        usage: LABEL
        description: Stream table name
    - last_refresh_lag_ms:
        usage: GAUGE
        description: Lag since last refresh in milliseconds
    - status:
        usage: LABEL
        description: Stream table status

pg_trickle_worker_pool:
  query: |
    SELECT active_workers, max_workers, idle_workers, ring_overflow_count
    FROM pgtrickle.worker_pool_status()
  metrics:
    - active_workers:
        usage: GAUGE
        description: Active pg_trickle refresh workers
    - max_workers:
        usage: GAUGE
        description: Maximum pg_trickle refresh workers
    - idle_workers:
        usage: GAUGE
        description: Idle pg_trickle refresh workers
    - ring_overflow_count:
        usage: COUNTER
        description: Invalidation ring overflow events since startup
```

---

## 3. TLS Configuration

Enable TLS for postgres_exporter in production:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: pg_trickle
    scheme: https
    tls_config:
      ca_file: /etc/prometheus/ca.pem
      cert_file: /etc/prometheus/client.pem
      key_file: /etc/prometheus/client.key
    static_configs:
      - targets: ['postgres-exporter:9187']
```

---

## 4. Kubernetes ServiceMonitor

For clusters using the Prometheus Operator:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: pg-trickle
  namespace: monitoring
spec:
  selector:
    matchLabels:
      app: postgres-exporter
  endpoints:
    - port: metrics
      interval: 30s
      scheme: https
      tlsConfig:
        insecureSkipVerify: false
        caFile: /etc/prometheus/ca.pem
```

---

## 5. Key Alerts

Add these alert rules to Prometheus for pg_trickle:

```yaml
groups:
  - name: pg_trickle
    rules:
      - alert: PgTrickleStreamTableHighLag
        expr: pg_trickle_stream_table_lag_last_refresh_lag_ms > 30000
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Stream table {{ $labels.name }} lag > 30s"

      - alert: PgTrickleWorkerPoolExhausted
        expr: pg_trickle_worker_pool_idle_workers == 0
        for: 2m
        labels:
          severity: critical
        annotations:
          summary: "pg_trickle worker pool exhausted — increase max_dynamic_refresh_workers"

      - alert: PgTrickleInvalidationRingOverflow
        expr: increase(pg_trickle_worker_pool_ring_overflow_count[5m]) > 0
        labels:
          severity: warning
        annotations:
          summary: "pg_trickle invalidation ring overflowing — consider increasing pg_trickle.invalidation_ring_capacity"
```

---

## 6. CNPG Integration

When running with CloudNativePG, enable the pod monitor in `cluster-production.yaml`:

```yaml
monitoring:
  enablePodMonitor: true
  customQueriesConfigMap:
    - name: pg-trickle-metrics
      key: queries.yaml
```

Create the ConfigMap with the queries from section 2 above.

See also: [cnpg/cluster-production.yaml](../cnpg/cluster-production.yaml)

---

## 7. Demo vs Production

| Concern          | Demo (`monitoring/`)           | Production (`monitoring/production/`) |
|------------------|-------------------------------|----------------------------------------|
| Authentication   | No auth                       | TLS + least-privilege role             |
| Deployment       | `docker compose up -d`        | Kubernetes / systemd                   |
| Retention        | Ephemeral (volume)            | Persistent storage class               |
| Alerting         | None                          | AlertManager rules                     |
| Access control   | Open                          | RBAC + network policies                |

The demo stack in `monitoring/` is for **local development only**. Replace all
credentials and remove public exposure before any shared environment.
