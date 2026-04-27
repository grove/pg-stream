# pg_trickle on CloudNativePG

## Running Incremental View Maintenance in Production Kubernetes

---

Running a stateful PostgreSQL extension in Kubernetes requires more thought than deploying a stateless web service. The extension has background workers, shared memory segments, and per-database state. Upgrades must be coordinated with the schema migration process. High availability means the extension must survive primary failover.

This post covers running pg_trickle on [CloudNativePG](https://cloudnative-pg.io/) — the CNCF-sandbox Kubernetes operator for PostgreSQL — in a production setup. The focus is on the operational mechanics: getting the extension installed, keeping it healthy across restarts and failovers, and monitoring it from outside the database.

---

## Prerequisites

- Kubernetes 1.27+
- CloudNativePG operator 1.24+ installed in the cluster
- A container image with both PostgreSQL 18 and pg_trickle installed
- The pg_trickle shared library preloaded

---

## The Container Image

CloudNativePG manages the PostgreSQL binary and data directory lifecycle. You need to provide a container image that includes the extension.

A minimal Dockerfile:

```dockerfile
FROM ghcr.io/cloudnative-pg/postgresql:18

# Install build dependencies
USER root
RUN apt-get update && apt-get install -y \
    build-essential \
    postgresql-server-dev-18 \
    git \
    curl \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Rust (required to build pg_trickle)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Install pgrx
RUN cargo install cargo-pgrx --version 0.18.0 && \
    cargo pgrx init --pg18 /usr/lib/postgresql/18/bin/pg_config

# Build and install pg_trickle
ARG PGTRICKLE_VERSION=0.36.0
RUN git clone --depth 1 --branch v${PGTRICKLE_VERSION} \
    https://github.com/grove/pg-trickle.git /tmp/pg-trickle && \
    cd /tmp/pg-trickle && \
    cargo pgrx install --release --pg-config /usr/lib/postgresql/18/bin/pg_config && \
    rm -rf /tmp/pg-trickle

USER 26
```

For production, pin to a specific digest rather than a tag. Build the image in CI and push to your internal registry.

---

## The Cluster Manifest

A production-ready CNPG `Cluster` resource:

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: pgtrickle-cluster
  namespace: production
spec:
  instances: 3
  imageName: your-registry/postgresql-pgtrickle:18-0.36.0

  postgresql:
    parameters:
      # Required: load pg_trickle's shared library
      shared_preload_libraries: "pg_trickle"

      # pg_trickle configuration
      pg_trickle.enabled: "on"
      pg_trickle.max_parallel_workers: "4"
      pg_trickle.backpressure_enabled: "on"
      pg_trickle.backpressure_max_lag_mb: "128"
      pg_trickle.log_format: "json"

      # Standard PostgreSQL tuning
      shared_buffers: "2GB"
      effective_cache_size: "6GB"
      maintenance_work_mem: "512MB"
      work_mem: "64MB"
      max_connections: "200"
      wal_level: "logical"       # required for pg_trickle's WAL features
      max_wal_senders: "10"
      max_replication_slots: "10"

  bootstrap:
    initdb:
      database: app
      owner: app
      postInitSQL:
        # Install extension in the target database
        - CREATE EXTENSION IF NOT EXISTS pg_trickle;
        # Optionally install pgvector if using vector features
        - CREATE EXTENSION IF NOT EXISTS vector;

  storage:
    size: 100Gi
    storageClass: fast-ssd  # Use NVMe-backed storage for pg_trickle workloads

  resources:
    requests:
      memory: "8Gi"
      cpu: "4"
    limits:
      memory: "16Gi"
      cpu: "8"

  # Monitoring integration
  monitoring:
    enablePodMonitor: true
    customQueriesConfigMap:
      - name: pgtrickle-metrics
        key: queries.yaml
```

The critical parameters:
- `shared_preload_libraries: "pg_trickle"` — required for the background worker to start
- `wal_level: "logical"` — required for pg_trickle's WAL decoder
- `postInitSQL` — runs once during database creation, installs the extension

---

## High Availability and Failover

CNPG runs one primary and N-1 standbys. When the primary fails, CNPG promotes a standby and updates the service endpoints. Your application reconnects to the new primary.

pg_trickle's background workers run only on the primary. On a standby:
- The extension code is present (the shared library loads fine)
- The pg_trickle catalog tables are replicated
- The background workers don't start (the standby is read-only)

On failover:
1. CNPG promotes a standby to primary
2. PostgreSQL starts up
3. pg_trickle's background workers start as part of the `shared_preload_libraries` initialization
4. The workers load the stream table catalog and begin processing any pending change buffer entries

The change buffers are regular PostgreSQL tables, replicated via streaming replication. Changes captured before the failover are in the buffers on the new primary and will be processed after startup.

**Expected staleness on failover:** Up to the maximum of:
- The change buffer accumulation during the failover window (typically 10–60 seconds)
- The stream table's configured `schedule`

For a `schedule = '5 seconds'` stream table, expect up to ~60 seconds of staleness after failover on a fast cluster.

---

## Configuration Management with ConfigMaps

Rather than hardcoding GUC values in the Cluster manifest, use a ConfigMap for pg_trickle settings and reference it:

```yaml
# configmap.yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: pgtrickle-config
data:
  pg_trickle.enabled: "on"
  pg_trickle.max_parallel_workers: "4"
  pg_trickle.backpressure_enabled: "on"
  pg_trickle.backpressure_max_lag_mb: "128"
  pg_trickle.default_schedule: "5 seconds"
  pg_trickle.log_format: "json"
```

```yaml
# In the Cluster spec
spec:
  postgresql:
    parameters:
      shared_preload_libraries: "pg_trickle"
    configMapRef:
      name: pgtrickle-config
```

This lets you update GUC values via `kubectl apply` without modifying the Cluster resource, and enables configuration review in git via normal PR workflow.

---

## Prometheus Metrics

pg_trickle exposes metrics via the PostgreSQL query interface. Export them with a custom queries ConfigMap for the CNPG PodMonitor:

```yaml
# pgtrickle-metrics.yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: pgtrickle-metrics
data:
  queries.yaml: |
    pg_trickle_stream_tables:
      query: |
        SELECT
          name,
          sla_tier,
          EXTRACT(EPOCH FROM (NOW() - last_refresh_at))::float AS staleness_seconds,
          rows_changed_last_cycle,
          avg_refresh_ms / 1000.0 AS avg_refresh_seconds,
          pending_change_rows
        FROM pgtrickle.stream_table_status()
      metrics:
        - name:
            usage: LABEL
            description: Stream table name
        - sla_tier:
            usage: LABEL
            description: SLA tier
        - staleness_seconds:
            usage: GAUGE
            description: Seconds since last refresh
        - rows_changed_last_cycle:
            usage: GAUGE
            description: Rows changed in last refresh cycle
        - avg_refresh_seconds:
            usage: GAUGE
            description: Average refresh duration in seconds
        - pending_change_rows:
            usage: GAUGE
            description: Pending rows in change buffer

    pg_trickle_change_buffers:
      query: |
        SELECT
          source_table,
          pending_rows,
          EXTRACT(EPOCH FROM (NOW() - oldest_change_at))::float AS backlog_age_seconds
        FROM pgtrickle.change_buffer_status()
      metrics:
        - source_table:
            usage: LABEL
        - pending_rows:
            usage: GAUGE
            description: Pending rows in change buffer for this source
        - backlog_age_seconds:
            usage: GAUGE
            description: Age of the oldest pending change in seconds
```

With this, Grafana can visualize stream table staleness per SLA tier, change buffer depth per source table, and refresh timing trends.

---

## Alerting Rules

Essential Prometheus alerts:

```yaml
groups:
- name: pgtrickle
  rules:
  - alert: StreamTableCriticalStaleness
    expr: |
      pg_trickle_stream_tables_staleness_seconds{sla_tier="critical"} > 30
    for: 2m
    labels:
      severity: critical
    annotations:
      summary: "Critical stream table {{ $labels.name }} is {{ $value | humanizeDuration }} stale"

  - alert: StreamTableStandardStaleness
    expr: |
      pg_trickle_stream_tables_staleness_seconds{sla_tier="standard"} > 300
    for: 5m
    labels:
      severity: warning
    annotations:
      summary: "Standard stream table {{ $labels.name }} is {{ $value | humanizeDuration }} stale"

  - alert: ChangeBufferBacklog
    expr: |
      pg_trickle_change_buffers_backlog_age_seconds > 600
    for: 5m
    labels:
      severity: warning
    annotations:
      summary: "Change buffer for {{ $labels.source_table }} has {{ $value | humanizeDuration }} backlog"
```

The critical staleness alert fires before users notice. The backlog alert fires when the refresh workers can't keep up with the write load.

---

## Upgrading pg_trickle

Upgrading pg_trickle follows the standard extension upgrade pattern, coordinated with CNPG's rolling update mechanism.

**Step 1: Build the new image**

Build a new container image with the upgraded extension version. Push to your registry.

**Step 2: Update the Cluster manifest**

```yaml
spec:
  imageName: your-registry/postgresql-pgtrickle:18-0.37.0  # new version
```

**Step 3: CNPG performs a rolling update**

CNPG stops the old primary, starts a new one with the new image, and promotes it. The standby instances update one at a time.

**Step 4: Run the extension migration**

After the cluster is running the new image, the extension schema may need to be upgraded:

```sql
ALTER EXTENSION pg_trickle UPDATE TO '0.37.0';
```

For CNPG, this is most cleanly done as a `postUpgradeSQL` hook in the Cluster spec, or as a Job that runs after the rolling update completes.

---

## Sizing Guidance

pg_trickle's resource consumption scales with:
- Number of stream tables
- Write volume on source tables (drives change buffer size and refresh frequency)
- Complexity of stream table queries (drives delta computation cost)

For a typical deployment with 10–20 stream tables and moderate write volume:

| Resource | Minimum | Recommended |
|----------|---------|-------------|
| CPU (cores) | 2 | 4–8 |
| Memory | 4GB | 8–16GB |
| Storage IOPS | 3,000 | 10,000+ (NVMe preferred) |
| `max_parallel_workers` | 2 | 4 |
| `shared_buffers` | 1GB | 25% of RAM |
| `maintenance_work_mem` | 256MB | 1–2GB (for REINDEX operations) |

The most important factor is storage latency. pg_trickle's refresh cycles are I/O-bound when processing large deltas or maintaining HNSW indexes. SSDs (NVMe preferred) make the difference between 20ms refresh cycles and 200ms ones.

---

## Production Checklist

Before going live with pg_trickle on CNPG:

- [ ] `shared_preload_libraries` includes `pg_trickle`
- [ ] `wal_level = logical` is set
- [ ] Extension installed in target database via `postInitSQL`
- [ ] Custom metrics ConfigMap deployed and referenced in Cluster manifest
- [ ] Prometheus alerts configured (staleness, backlog)
- [ ] Grafana dashboard imported
- [ ] SLA tiers configured for all stream tables
- [ ] Failover tested: `kubectl cnpg promote` to simulate primary failure, verify stream tables resume within expected window
- [ ] Upgrade path tested in staging: new image + `ALTER EXTENSION UPDATE`
- [ ] Backpressure enabled and threshold tuned for your write volume

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
