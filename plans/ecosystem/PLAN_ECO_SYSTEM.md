# pg_trickle Ecosystem — Supportive Projects Plan

Date: 2026-02-24
Status: PROPOSED

---

## Overview

This document describes the ecosystem of supportive projects around the pg_trickle
PostgreSQL extension. Each project is designed to lower adoption friction, improve
operability, or integrate pg_trickle with popular tools in the modern data stack.

All projects are maintained in **separate repositories** unless noted otherwise. The
pg_trickle extension repo (`pg-trickle`) remains focused on the core Rust/pgrx extension.

### Principles

1. **SQL is the API.** Every integration wraps pg_trickle's SQL functions — no custom
   wire protocols, no binary coupling to extension internals.
2. **Separate repos, separate release cadences.** Ecosystem projects only change when
   the SQL API changes, not on every Rust refactor.
3. **Zero required dependencies.** pg_trickle works standalone. Every ecosystem project
   is optional and additive.
4. **Start small, ship fast.** Each project has a minimal viable deliverable that can
   ship in days, with expansion phases that follow.

---

## Table of Contents

- [Roadmap Summary](#roadmap-summary)
- [Project 1 — dbt Macro Package](#project-1--dbt-macro-package-dbt-pgtrickle)
- [Project 2 — Prometheus Exporter Config](#project-2--prometheus-exporter-config)
- [Project 3 — Grafana Dashboard](#project-3--grafana-dashboard)
- [Project 4 — Docker Hub Image](#project-4--docker-hub-image)
- [Project 5 — CNPG Integration](#project-5--cnpg-integration)
- [Project 6 — Airflow Provider](#project-6--airflow-provider)
- [Project 7 — CLI Tool](#project-7--cli-tool-pgtrickle)
- [Project 8 — dbt Adapter](#project-8--dbt-adapter)
- [Project 9 — PGXN & OS Packages](#project-9--pgxn--os-packages)
- [Project 10 — Flyway & Liquibase Support](#project-10--flyway--liquibase-support)
- [Project 11 — ORM Integrations](#project-11--orm-integrations)
- [Dependency Graph](#dependency-graph)
- [Cross-Cutting Concerns](#cross-cutting-concerns)

---

## Roadmap Summary

| Phase | Projects | Combined Effort | Dependencies |
|-------|----------|----------------|--------------|
| **Phase 1 — Observability** | Prometheus config, Grafana dashboard | ~8 hours | None |
| **Phase 2 — Distribution** | Docker Hub image, CNPG hardening | ~16 hours | None |
| **Phase 3 — Data Stack** | dbt macro package | ~15 hours | None |
| **Phase 4 — Orchestration** | Airflow provider, CLI tool | ~36 hours | None |
| **Phase 5 — Advanced** | dbt adapter, PGXN/apt, Flyway/Liquibase, ORMs | ~100 hours | Phase 3 |

Projects within the same phase can be developed in parallel. Cross-phase dependencies
are noted in each project section.

---

## Project 1 — dbt Macro Package (`dbt-pgtrickle`)

> Full plan: [../dbt/PLAN_DBT_MACRO.md](../dbt/PLAN_DBT_MACRO.md)

### Summary

A standalone dbt package containing a custom `stream_table` materialization that wraps
pg_trickle's SQL API. Works with the standard `dbt-postgres` adapter.

### Repository

- **Repo:** `github.com/<org>/dbt-pgtrickle` (separate)
- **Language:** Jinja SQL
- **Distribution:** Git install via `packages.yml`, later dbt Hub

### Key Deliverables

| Deliverable | Description |
|-------------|-------------|
| `stream_table` materialization | Maps `dbt run` → `create_stream_table()` / `alter_stream_table()` |
| Full-refresh support | `dbt run --full-refresh` → `drop_stream_table()` + `create_stream_table()` |
| Source freshness bridge | Maps `dbt source freshness` → `pg_stat_stream_tables` |
| Manual refresh operation | `dbt run-operation refresh --args '{"model_name": "..."}'` |
| Integration test suite | Seed data → create STs → verify → full refresh → verify |

### Effort: ~15 hours

---

## Project 2 — Prometheus Exporter Config

### Summary

A `postgres_exporter` custom queries configuration file that exposes pg_trickle metrics
as Prometheus metrics. Requires zero custom code — just a YAML config file consumed by
the standard [postgres_exporter](https://github.com/prometheus-community/postgres_exporter).

### Repository

- **Repo:** `github.com/<org>/pgtrickle-monitoring` (separate, shared with Grafana dashboard)
- **Language:** YAML + SQL
- **Distribution:** Git clone or copy the file

### Metrics Exposed

#### From `pgtrickle.pg_stat_stream_tables`

| Prometheus Metric | Source Column | Type | Labels |
|-------------------|---------------|------|--------|
| `pgtrickle_refreshes_total` | `total_refreshes` | counter | `pgt_name`, `schema` |
| `pgtrickle_refreshes_successful_total` | `successful_refreshes` | counter | `pgt_name` |
| `pgtrickle_refreshes_failed_total` | `failed_refreshes` | counter | `pgt_name` |
| `pgtrickle_rows_inserted_total` | `total_rows_inserted` | counter | `pgt_name` |
| `pgtrickle_rows_deleted_total` | `total_rows_deleted` | counter | `pgt_name` |
| `pgtrickle_avg_refresh_duration_ms` | `avg_duration_ms` | gauge | `pgt_name` |
| `pgtrickle_staleness_seconds` | `staleness` | gauge | `pgt_name` |
| `pgtrickle_stale` | `stale` | gauge (0/1) | `pgt_name` |
| `pgtrickle_consecutive_errors` | `consecutive_errors` | gauge | `pgt_name` |
| `pgtrickle_is_populated` | `is_populated` | gauge (0/1) | `pgt_name` |

#### From `pgtrickle.check_cdc_health()`

| Prometheus Metric | Source Column | Type | Labels |
|-------------------|---------------|------|--------|
| `pgtrickle_cdc_mode` | `cdc_mode` | gauge (enum) | `source_table` |
| `pgtrickle_cdc_lag_bytes` | `lag_bytes` | gauge | `source_table`, `slot_name` |
| `pgtrickle_cdc_alert` | `alert` | gauge (0/1) | `source_table`, `alert_type` |

### Deliverable Structure

```
pgtrickle-monitoring/
├── README.md
├── prometheus/
│   ├── pgtrickle_queries.yml          # postgres_exporter custom queries
│   └── alerts.yml                    # Prometheus alerting rules
├── grafana/
│   └── pgtrickle-dashboard.json       # Grafana dashboard (see Project 3)
└── docker-compose.yml                # Full observability stack demo
```

### Example: `pgtrickle_queries.yml`

```yaml
pgtrickle_stream_table_stats:
  query: |
    SELECT
      pgt_schema AS schema,
      pgt_name,
      status,
      refresh_mode,
      COALESCE(EXTRACT(EPOCH FROM staleness), 0) AS staleness_seconds,
      CASE WHEN stale THEN 1 ELSE 0 END AS is_stale,
      consecutive_errors,
      CASE WHEN is_populated THEN 1 ELSE 0 END AS is_populated,
      total_refreshes,
      successful_refreshes,
      failed_refreshes,
      total_rows_inserted,
      total_rows_deleted,
      COALESCE(avg_duration_ms, 0) AS avg_duration_ms
    FROM pgtrickle.pg_stat_stream_tables
  metrics:
    - schema:
        usage: "LABEL"
    - pgt_name:
        usage: "LABEL"
    - status:
        usage: "LABEL"
    - refresh_mode:
        usage: "LABEL"
    - staleness_seconds:
        usage: "GAUGE"
        description: "Seconds since last refresh"
    - is_stale:
        usage: "GAUGE"
        description: "1 if stream table data is stale"
    - consecutive_errors:
        usage: "GAUGE"
        description: "Current consecutive error count"
    - is_populated:
        usage: "GAUGE"
        description: "1 if stream table has been populated"
    - total_refreshes:
        usage: "COUNTER"
        description: "Total refresh operations"
    - successful_refreshes:
        usage: "COUNTER"
        description: "Total successful refreshes"
    - failed_refreshes:
        usage: "COUNTER"
        description: "Total failed refreshes"
    - total_rows_inserted:
        usage: "COUNTER"
        description: "Total rows inserted across all refreshes"
    - total_rows_deleted:
        usage: "COUNTER"
        description: "Total rows deleted across all refreshes"
    - avg_duration_ms:
        usage: "GAUGE"
        description: "Average refresh duration in milliseconds"

pgtrickle_cdc_health:
  query: |
    SELECT
      source_table,
      cdc_mode,
      COALESCE(slot_name, '') AS slot_name,
      COALESCE(lag_bytes, 0) AS lag_bytes,
      COALESCE(confirmed_lsn::text, '') AS confirmed_lsn,
      CASE WHEN alert IS NOT NULL THEN 1 ELSE 0 END AS has_alert,
      COALESCE(alert, '') AS alert_type
    FROM pgtrickle.check_cdc_health()
  metrics:
    - source_table:
        usage: "LABEL"
    - cdc_mode:
        usage: "LABEL"
    - slot_name:
        usage: "LABEL"
    - lag_bytes:
        usage: "GAUGE"
        description: "Replication slot lag in bytes"
    - has_alert:
        usage: "GAUGE"
        description: "1 if CDC source has an active alert"
    - alert_type:
        usage: "LABEL"
```

### Prometheus Alerting Rules

```yaml
# prometheus/alerts.yml
groups:
  - name: pgtrickle
    rules:
      - alert: PgTrickleTableStale
        expr: pgtrickle_stream_table_stats_is_stale == 1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Stream table {{ $labels.pgt_name }} is stale"

      - alert: PgTrickleConsecutiveErrors
        expr: pgtrickle_stream_table_stats_consecutive_errors >= 3
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "Stream table {{ $labels.pgt_name }} has {{ $value }} consecutive errors"

      - alert: PgTrickleCdcLagHigh
        expr: pgtrickle_cdc_health_lag_bytes > 1073741824
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "CDC lag for {{ $labels.source_table }} exceeds 1GB"

      - alert: PgTrickleCdcAlert
        expr: pgtrickle_cdc_health_has_alert == 1
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "CDC alert for {{ $labels.source_table }}: {{ $labels.alert_type }}"
```

### Effort: ~4 hours

---

## Project 3 — Grafana Dashboard

### Summary

A pre-built Grafana dashboard JSON that visualizes pg_trickle metrics from Prometheus
(Project 2). Importable via Grafana UI or provisioning.

### Repository

- **Repo:** Same as Project 2 (`pgtrickle-monitoring`)
- **Distribution:** JSON file, optionally published to [Grafana Dashboards](https://grafana.com/grafana/dashboards/)

### Dashboard Panels

#### Row 1 — Overview

| Panel | Type | Query |
|-------|------|-------|
| Active Stream Tables | Stat | `count(pgtrickle_..._status{status="ACTIVE"})` |
| Stale Tables | Stat (red if >0) | `count(pgtrickle_..._is_stale == 1)` |
| Error Tables | Stat (red if >0) | `count(pgtrickle_..._status{status="ERROR"})` |
| Total Refreshes/min | Stat | `rate(pgtrickle_..._total_refreshes[5m])` |

#### Row 2 — Refresh Performance

| Panel | Type | Query |
|-------|------|-------|
| Avg Refresh Duration | Time series | `pgtrickle_..._avg_duration_ms` by `pgt_name` |
| Refresh Rate | Time series | `rate(pgtrickle_..._total_refreshes[5m])` by `pgt_name` |
| Failure Rate | Time series | `rate(pgtrickle_..._failed_refreshes[5m])` by `pgt_name` |

#### Row 3 — Staleness

| Panel | Type | Query |
|-------|------|-------|
| Staleness per ST | Time series | `pgtrickle_..._staleness_seconds` by `pgt_name` |
| Rows Changed/min | Time series | `rate(pgtrickle_..._total_rows_inserted[5m]) + rate(pgtrickle_..._total_rows_deleted[5m])` |

#### Row 4 — CDC Health

| Panel | Type | Query |
|-------|------|-------|
| CDC Mode per Source | Table | `pgtrickle_cdc_health{cdc_mode}` by `source_table` |
| Replication Lag | Time series | `pgtrickle_cdc_health_lag_bytes` by `source_table` |
| CDC Alerts | Alert list | `pgtrickle_cdc_health_has_alert == 1` |

#### Row 5 — Per-Table Detail (variable: `$stream_table`)

| Panel | Type | Query |
|-------|------|-------|
| Status | Stat | Current status |
| Consecutive Errors | Stat | Error count |
| Refresh History | Time series | Insert/delete counts over time |
| Avg Duration Trend | Time series | Duration over time |

### Docker Compose Demo Stack

```yaml
# docker-compose.yml — one-command observability demo
version: '3.8'
services:
  postgres:
    image: pg_trickle:latest
    environment:
      POSTGRES_PASSWORD: postgres
    ports: ['5432:5432']

  postgres-exporter:
    image: quay.io/prometheuscommunity/postgres-exporter:latest
    environment:
      DATA_SOURCE_NAME: "postgresql://postgres:postgres@postgres:5432/postgres?sslmode=disable"
      PG_EXPORTER_EXTEND_QUERY_PATH: /etc/pgtrickle_queries.yml
    volumes:
      - ./prometheus/pgtrickle_queries.yml:/etc/pgtrickle_queries.yml:ro
    ports: ['9187:9187']

  prometheus:
    image: prom/prometheus:latest
    volumes:
      - ./prometheus/prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - ./prometheus/alerts.yml:/etc/prometheus/alerts.yml:ro
    ports: ['9090:9090']

  grafana:
    image: grafana/grafana:latest
    volumes:
      - ./grafana/pgtrickle-dashboard.json:/var/lib/grafana/dashboards/pgtrickle.json:ro
      - ./grafana/provisioning:/etc/grafana/provisioning:ro
    ports: ['3000:3000']
    environment:
      GF_SECURITY_ADMIN_PASSWORD: admin
```

### Effort: ~4 hours

---

## Project 4 — Docker Hub Image

### Summary

A production-ready, ready-to-run Docker image (`postgres:18-pgtrickle`) with pg_trickle
pre-installed and configured in `shared_preload_libraries`. Published to Docker Hub
and/or GitHub Container Registry (GHCR).

### Repository

- **Repo:** Same as `pg-trickle` main repo (Dockerfile + CI workflow)
- **Published to:** Docker Hub (`pgtrickle/postgres:18`) and GHCR (`ghcr.io/<org>/pg_trickle:latest`)

### Dockerfile

Based on the existing `cnpg/Dockerfile` but with `shared_preload_libraries` set by
default (unlike the CNPG image which defers to the operator):

```dockerfile
# Dockerfile.release (in pg-trickle repo root)
FROM pg_trickle_builder AS builder
# ... (reuse existing build stage from cnpg/Dockerfile) ...

FROM postgres:18.1
COPY --from=builder /usr/share/postgresql/18/extension/pg_trickle* \
     /usr/share/postgresql/18/extension/
COPY --from=builder /usr/lib/postgresql/18/lib/pg_trickle.so \
     /usr/lib/postgresql/18/lib/

# Pre-configure for immediate use
RUN echo "shared_preload_libraries = 'pg_trickle'" >> \
    /usr/share/postgresql/postgresql.conf.sample
```

### CI Workflow

```yaml
# .github/workflows/docker-publish.yml
name: Publish Docker Image
on:
  push:
    tags: ['v*']
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          username: ${{ secrets.DOCKERHUB_USERNAME }}
          password: ${{ secrets.DOCKERHUB_TOKEN }}
      - uses: docker/build-push-action@v5
        with:
          push: true
          tags: |
            pgtrickle/postgres:18
            pgtrickle/postgres:${{ github.ref_name }}
          file: Dockerfile.release
```

### Tags

| Tag | Meaning |
|-----|---------|
| `pgtrickle/postgres:18` | Latest pg_trickle on PG 18 |
| `pgtrickle/postgres:18-0.1.0` | Specific pg_trickle version |
| `pgtrickle/postgres:latest` | Alias for `:18` |

### Quick Start

```bash
docker run -d --name pgtrickle \
  -e POSTGRES_PASSWORD=postgres \
  -p 5432:5432 \
  pgtrickle/postgres:18

psql -h localhost -U postgres -c "CREATE EXTENSION pg_trickle;"
```

### Effort: ~8 hours

---

## Project 5 — CNPG Integration

### Summary

Harden the existing `cnpg/` directory into a production-grade CloudNativePG integration
with tested manifests, a Helm chart, and CI that validates the manifests against a real
CNPG cluster.

### Repository

- **Repo:** Same as `pg-trickle` main repo (`cnpg/` directory) — CNPG manifests are
  deployment config, not a separate product
- **Helm chart:** Optionally in a separate `pgtrickle-helm` repo if published to
  Artifact Hub

### Deliverables

#### Phase 1 — Hardened Manifests (~4 hours)

- Templatize `cnpg/cluster-example.yaml` with common variants:
  - Single-instance (dev)
  - 3-instance HA (production)
  - WAL-mode CDC enabled (with `wal_level: logical`)
- Add `Backup` and `ScheduledBackup` CRDs
- Add `Pooler` CRD (PgBouncer) for connection pooling
- Document required RBAC for the pg_trickle extension

```
cnpg/
├── Dockerfile
├── cluster-dev.yaml           # Single instance, no backup
├── cluster-production.yaml    # 3 instances, backup, pooler
├── cluster-wal-cdc.yaml       # WAL CDC enabled
├── backup.yaml                # Barman-based backup to S3
└── README.md
```

#### Phase 2 — Helm Chart (~8 hours)

```
pgtrickle-helm/
├── Chart.yaml
├── values.yaml
├── templates/
│   ├── cluster.yaml
│   ├── backup.yaml
│   ├── pooler.yaml
│   └── _helpers.tpl
└── README.md
```

Key `values.yaml` parameters:

```yaml
instances: 3
image: ghcr.io/<org>/pg_trickle:latest
pgtrickle:
  enabled: true
  schedulerIntervalMs: 1000
  minScheduleSeconds: 60
  maxConcurrentRefreshes: 4
  cdcMode: trigger             # trigger | auto | wal
  userTriggers: auto
backup:
  enabled: false
  s3Bucket: ""
  schedule: "0 0 * * *"
pooler:
  enabled: false
  instances: 2
```

### Effort: ~12 hours total

---

## Project 6 — Airflow Provider

### Summary

An Apache Airflow provider package (`airflow-provider-pgtrickle`) containing operators and
sensors for integrating pg_trickle into Airflow DAGs. Enables data teams to orchestrate
stream table refreshes alongside their existing ETL/ELT pipelines.

### Repository

- **Repo:** `github.com/<org>/airflow-provider-pgtrickle` (separate)
- **Language:** Python
- **Distribution:** PyPI (`pip install airflow-provider-pgtrickle`)

### Components

#### Operators

| Operator | Purpose | SQL Called |
|----------|---------|-----------|
| `PgTrickleCreateOperator` | Create a stream table | `pgtrickle.create_stream_table()` |
| `PgTrickleDropOperator` | Drop a stream table | `pgtrickle.drop_stream_table()` |
| `PgTrickleRefreshOperator` | Trigger a manual refresh | `pgtrickle.refresh_stream_table()` |
| `PgTrickleAlterOperator` | Alter schedule/mode/status | `pgtrickle.alter_stream_table()` |

#### Sensors

| Sensor | Purpose | SQL Polled |
|--------|---------|------------|
| `PgTrickleFreshnessSensor` | Wait until a ST is fresh (not stale) | `pgtrickle.pg_stat_stream_tables` |
| `PgTrickleHealthSensor` | Wait until CDC health is OK | `pgtrickle.check_cdc_health()` |
| `PgTrickleStatusSensor` | Wait until ST reaches a target status | `pgtrickle.pgt_stream_tables` |

#### Hooks

| Hook | Purpose |
|------|---------|
| `PgTrickleHook` | Extends `PostgresHook` with pg_trickle-specific helper methods |

### Example DAG

```python
from airflow import DAG
from airflow.utils.dates import days_ago
from airflow_provider_pgtrickle.operators import (
    PgTrickleRefreshOperator,
)
from airflow_provider_pgtrickle.sensors import (
    PgTrickleFreshnessSensor,
)

with DAG("pgtrickle_refresh", start_date=days_ago(1), schedule_interval="@hourly"):

    wait_fresh = PgTrickleFreshnessSensor(
        task_id="wait_for_orders_fresh",
        stream_table="order_totals",
        postgres_conn_id="my_pg",
        timeout=300,
    )

    refresh = PgTrickleRefreshOperator(
        task_id="refresh_order_totals",
        stream_table="order_totals",
        postgres_conn_id="my_pg",
    )

    wait_fresh >> refresh
```

### File Structure

```
airflow-provider-pgtrickle/
├── pyproject.toml
├── README.md
├── airflow_provider_pgtrickle/
│   ├── __init__.py
│   ├── hooks/
│   │   └── pgtrickle.py              # PgTrickleHook (~60 lines)
│   ├── operators/
│   │   ├── __init__.py
│   │   ├── create.py                # ~40 lines
│   │   ├── drop.py                  # ~30 lines
│   │   ├── refresh.py               # ~30 lines
│   │   └── alter.py                 # ~40 lines
│   └── sensors/
│       ├── __init__.py
│       ├── freshness.py             # ~50 lines
│       ├── health.py                # ~50 lines
│       └── status.py                # ~40 lines
└── tests/
    ├── test_hook.py
    ├── test_operators.py
    └── test_sensors.py
```

### Effort: ~16 hours

---

## Project 7 — CLI Tool (`pgtrickle`)

### Summary

A standalone command-line tool for managing pg_trickle from the terminal. Provides a
user-friendly interface to common operations without writing SQL.

### Repository

- **Repo:** `github.com/<org>/pgtrickle-cli` (separate) or as `src/bin/pgtrickle.rs`
  in the main repo if written in Rust
- **Language:** Rust (preferred — shares build infra) or Python with `click` + `psycopg`
- **Distribution:** GitHub Releases (binaries), Homebrew, `cargo install`, or PyPI

### Commands

```
pgtrickle — CLI for pg_trickle streaming tables

USAGE:
    pgtrickle [OPTIONS] <COMMAND>

CONNECTION:
    -h, --host <HOST>          PostgreSQL host [default: localhost]
    -p, --port <PORT>          PostgreSQL port [default: 5432]
    -U, --user <USER>          PostgreSQL user [default: postgres]
    -d, --dbname <DB>          Database name [default: postgres]
    --url <URL>                Connection URL (overrides host/port/user/dbname)

COMMANDS:
    list                       List all stream tables
    status <name>              Show detailed status for a stream table
    create <name> <query>      Create a stream table
    drop <name>                Drop a stream table
    refresh <name>             Trigger a manual refresh
    alter <name> [OPTIONS]     Alter schedule, mode, or status
    explain <name>             Show the DVM plan
    history <name> [--limit]   Show refresh history
    health                     Show CDC health for all sources
    stats                      Show aggregate refresh statistics
    watch [--interval]         Live-updating status display (like `watch`)
```

### Example Usage

```bash
# List all stream tables with status
$ pgtrickle list
NAME             SCHEMA   STATUS   MODE           SCHEDULE  STALE  ERRORS
order_totals     public   ACTIVE   DIFFERENTIAL   5m        no     0
big_customers    public   ACTIVE   DIFFERENTIAL   5m        no     0
daily_revenue    public   ERROR    FULL           1h        yes    3

# Detailed status
$ pgtrickle status order_totals
Name:           order_totals
Schema:         public
Status:         ACTIVE
Refresh Mode:   DIFFERENTIAL
Schedule:       5m
Last Refresh:   2026-02-24 12:34:56 UTC (26s ago)
Staleness:      26s
Stale:          no
Total Refreshes: 1,247
Avg Duration:   42ms
Source Tables:   orders (TRIGGER), customers (WAL)

# Live watch (refreshes every 2s)
$ pgtrickle watch --interval 2s
┌──────────────┬────────┬──────────────┬──────┬───────┬──────────┐
│ NAME         │ STATUS │ MODE         │ STALE│ ERRORS│ LAST     │
├──────────────┼────────┼──────────────┼──────┼───────┼──────────┤
│ order_totals │ ACTIVE │ DIFFERENTIAL │ no   │ 0     │ 4s ago   │
│ big_customers│ ACTIVE │ DIFFERENTIAL │ no   │ 0     │ 4s ago   │
│ daily_revenue│ ERROR  │ FULL         │ yes  │ 3     │ 1h ago   │
└──────────────┴────────┴──────────────┴──────┴───────┴──────────┘

# Create from file
$ pgtrickle create my_table --file my_query.sql --schedule 10m --mode DIFFERENTIAL
Created stream table: my_table

# CDC health check
$ pgtrickle health
SOURCE TABLE     CDC MODE       SLOT                  LAG      ALERT
public.orders    WAL            pg_trickle_slot_16384  512KB    none
public.events    TRIGGER        —                     —        —
public.users     TRANSITIONING  pg_trickle_slot_16400  0B       none
```

### Implementation Notes

**If Rust:** Use `clap` for argument parsing, `tokio-postgres` for DB access, `tabled`
or `comfy-table` for table formatting, `crossterm` for the `watch` TUI.

**If Python:** Use `click` for CLI, `psycopg[binary]` for DB access, `rich` for
table formatting and live display.

### Effort: ~20 hours

---

## Project 8 — dbt Adapter

> Full plan: [../dbt/PLAN_DBT_ADAPTER.md](../dbt/PLAN_DBT_ADAPTER.md)

### Summary

A full `dbt-pgtrickle` adapter extending `dbt-postgres`. Provides first-class stream
table support: custom relation types, `__pgt_row_id` column filtering, native source
freshness, and operational run-operations.

### Repository

- **Repo:** `github.com/<org>/dbt-pgtrickle` (same repo as the macro package, or
  superseding it)
- **Language:** Python + Jinja SQL
- **Distribution:** PyPI (`pip install dbt-pgtrickle`)
- **Prerequisite:** Project 1 (macro package) is the stepping stone; the adapter
  absorbs its macros

### Key Advantages Over Macro Package

| Feature | Macro Package | Full Adapter |
|---------|--------------|--------------|
| `__pgt_row_id` hidden | No | Yes |
| Relation type `stream_table` | No (shows as `table`) | Yes |
| Native source freshness | Manual macro | Adapter override |
| Connection-time extension check | No | Yes |
| Custom catalog entries | No | ST metadata in docs |

### Effort: ~54 hours

---

## Project 9 — PGXN & OS Packages

### Summary

Publish pg_trickle to [PGXN](https://pgxn.org/) (the PostgreSQL Extension Network) and
produce `.deb`/`.rpm` packages for Linux distributions. This is the standard way
PostgreSQL users discover and install extensions.

### Repository

- **Repo:** Same as `pg-trickle` main repo (packaging config + CI workflows)

### Deliverables

#### 9.1 — PGXN Publication (~4 hours)

Add a `META.json` at the repo root:

```json
{
  "name": "pg_trickle",
  "abstract": "Streaming tables with incremental view maintenance for PostgreSQL",
  "version": "0.1.0",
  "maintainer": ["Your Name <you@example.com>"],
  "license": "postgresql",
  "provides": {
    "pg_trickle": {
      "file": "pg_trickle.control",
      "version": "0.1.0"
    }
  },
  "prereqs": {
    "runtime": {
      "requires": {
        "PostgreSQL": "18.0.0"
      }
    }
  },
  "resources": {
    "repository": {
      "url": "https://github.com/<org>/pg-trickle.git",
      "type": "git"
    },
    "bugtracker": {
      "web": "https://github.com/<org>/pg-trickle/issues"
    }
  },
  "generated_by": "hand",
  "meta-spec": { "version": "1.0.0" }
}
```

Register at pgxn.org and publish with `pgxn-utils`.

#### 9.2 — Debian/Ubuntu Packages (~8 hours)

CI workflow that cross-compiles pg_trickle and produces `.deb` packages:

```yaml
# .github/workflows/deb-package.yml
name: Build .deb
on:
  push:
    tags: ['v*']
jobs:
  build:
    strategy:
      matrix:
        os: [ubuntu-22.04, ubuntu-24.04, debian-12]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build in Docker
        run: |
          docker build -t pg-trickle-builder -f packaging/Dockerfile.${{ matrix.os }} .
          docker run --rm -v $(pwd)/dist:/dist pg-trickle-builder
      - uses: softprops/action-gh-release@v1
        with:
          files: dist/*.deb
```

#### 9.3 — RPM Packages (~4 hours)

Similar workflow producing `.rpm` packages for RHEL/Rocky/Alma 9.

#### 9.4 — Homebrew Formula (~4 hours)

For macOS development:

```ruby
class PgTrickle < Formula
  desc "Streaming tables with incremental view maintenance for PostgreSQL"
  homepage "https://github.com/<org>/pg-trickle"
  url "https://github.com/<org>/pg-trickle/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "..."
  license "PostgreSQL"
  depends_on "postgresql@18"
  depends_on "rust" => :build
  # ...
end
```

### Effort: ~20 hours total

---

## Project 10 — Flyway & Liquibase Support

### Summary

Documentation and optional tooling for managing stream tables with database migration
tools. Since stream tables are created via function calls (not DDL), migration tools
need guidance on the correct patterns.

### Repository

- **Repo:** Documentation in `pg-trickle` main repo (`docs/integrations/`)
- **Optional tooling:** Liquibase extension in a separate repo

### Deliverables

#### 10.1 — Flyway Guide (~4 hours)

Document the migration pattern in `docs/integrations/FLYWAY.md`:

```sql
-- V1__create_order_totals.sql
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT customer_id, SUM(amount) AS total FROM orders GROUP BY customer_id',
    '5m',
    'DIFFERENTIAL'
);

-- V2__update_order_totals_schedule.sql
SELECT pgtrickle.alter_stream_table('order_totals', schedule => '10m');

-- V3__drop_order_totals.sql
SELECT pgtrickle.drop_stream_table('order_totals');
```

Flyway executes arbitrary SQL, so no plugin is needed — just the documentation showing
the pattern, rollback strategy, and idempotency considerations.

#### 10.2 — Liquibase Guide (~4 hours)

Document the `sql` changeset pattern in `docs/integrations/LIQUIBASE.md`:

```xml
<changeSet id="1" author="dev">
    <sql>
        SELECT pgtrickle.create_stream_table(
            'order_totals',
            'SELECT customer_id, SUM(amount) AS total FROM orders GROUP BY customer_id',
            '5m', 'DIFFERENTIAL'
        );
    </sql>
    <rollback>
        <sql>SELECT pgtrickle.drop_stream_table('order_totals');</sql>
    </rollback>
</changeSet>
```

#### 10.3 — Liquibase Custom Change Type (Optional, ~16 hours)

A Liquibase extension that adds `<createStreamTable>` and `<dropStreamTable>` change
types with proper XML schema, rollback support, and checksums.

```xml
<createStreamTable name="order_totals"
                   schedule="5m"
                   refreshMode="DIFFERENTIAL">
    <query>
        SELECT customer_id, SUM(amount) AS total
        FROM orders GROUP BY customer_id
    </query>
</createStreamTable>
```

### Effort: ~8 hours (docs only), ~24 hours (with Liquibase extension)

---

## Project 11 — ORM Integrations

### Summary

Thin integration layers for popular ORMs to make stream tables usable as read-only
models with metadata access (staleness, last refresh, status).

### Repository

- **Repos:** Separate per ORM (`django-pgtrickle`, `sqlalchemy-pgtrickle`)
- **Language:** Python
- **Distribution:** PyPI

### 11.1 — Django Integration (`django-pgtrickle`)

```python
# django_pgtrickle/models.py
from django.db import models

class StreamTableManager(models.Manager):
    """Read-only manager that exposes pg_trickle metadata."""

    def get_queryset(self):
        return super().get_queryset().defer('__pgt_row_id')

    def is_stale(self) -> bool:
        """Check if the stream table data is stale."""
        from django.db import connection
        with connection.cursor() as cursor:
            cursor.execute(
                "SELECT stale FROM pgtrickle.stream_tables_info "
                "WHERE pgt_name = %s", [self.model._meta.db_table]
            )
            row = cursor.fetchone()
            return row[0] if row else None

    def last_refresh_at(self):
        """Get the timestamp of the last refresh."""
        ...

    def refresh(self):
        """Trigger a manual refresh."""
        ...


class StreamTableModel(models.Model):
    """Base class for Django models backed by pg_trickle stream tables."""

    objects = StreamTableManager()

    class Meta:
        abstract = True
        managed = False  # Django does not manage the table schema


# Usage:
class OrderTotals(StreamTableModel):
    customer_id = models.IntegerField(primary_key=True)
    total = models.DecimalField(max_digits=10, decimal_places=2)

    class Meta(StreamTableModel.Meta):
        db_table = 'order_totals'
```

**Additional features:**
- Django management command: `python manage.py pgtrickle_status`
- Django admin integration: read-only ModelAdmin with refresh button
- Health check: Django health check backend for `django-health-check`

### 11.2 — SQLAlchemy Integration (`sqlalchemy-pgtrickle`)

```python
# sqlalchemy_pgtrickle/mixin.py
from sqlalchemy import event, inspect
from sqlalchemy.ext.hybrid import hybrid_property

class StreamTableMixin:
    """Mixin for SQLAlchemy models backed by stream tables."""

    @classmethod
    def __declare_last__(cls):
        """Make the table read-only by rejecting writes."""
        @event.listens_for(cls, "before_insert")
        @event.listens_for(cls, "before_update")
        @event.listens_for(cls, "before_delete")
        def reject_write(mapper, connection, target):
            raise RuntimeError(
                f"{cls.__name__} is a stream table and cannot be modified directly."
            )

    @classmethod
    def is_stale(cls, session) -> bool:
        result = session.execute(
            "SELECT stale FROM pgtrickle.stream_tables_info "
            "WHERE pgt_name = :name",
            {"name": cls.__tablename__}
        )
        row = result.fetchone()
        return row[0] if row else None

    @classmethod
    def refresh(cls, session):
        session.execute(
            f"SELECT pgtrickle.refresh_stream_table('{cls.__tablename__}')"
        )
        session.commit()
```

### Effort: ~16 hours per ORM (Django and SQLAlchemy)

---

## Dependency Graph

```
                          ┌──────────────────┐
                          │   pg_trickle      │
                          │   (core ext)     │
                          └────────┬─────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                    │
              ▼                    ▼                    ▼
    ┌─────────────────┐  ┌─────────────────┐  ┌──────────────┐
    │ P2: Prometheus   │  │ P4: Docker Hub  │  │ P9: PGXN/    │
    │     Config       │  │     Image       │  │    apt/rpm   │
    └───────┬─────────┘  └────────┬────────┘  └──────────────┘
            │                     │
            ▼                     ▼
    ┌─────────────────┐  ┌─────────────────┐
    │ P3: Grafana     │  │ P5: CNPG        │
    │     Dashboard   │  │     Integration │
    └─────────────────┘  └─────────────────┘

    ┌─────────────────┐  ┌─────────────────┐  ┌──────────────┐
    │ P1: dbt Macro   │──│ P8: dbt Adapter │  │ P7: CLI Tool │
    │     Package     │  │    (absorbs P1) │  │              │
    └─────────────────┘  └─────────────────┘  └──────────────┘

    ┌─────────────────┐  ┌─────────────────┐
    │ P6: Airflow     │  │ P10: Flyway/    │
    │     Provider    │  │     Liquibase   │
    └─────────────────┘  └─────────────────┘

    ┌─────────────────┐
    │ P11: Django /   │
    │      SQLAlchemy │
    └─────────────────┘
```

**Hard dependencies:**
- P3 (Grafana) requires P2 (Prometheus) — dashboard queries Prometheus metrics
- P8 (dbt Adapter) supersedes P1 (dbt Macro) — adapter absorbs macros

**Soft dependencies:**
- P5 (CNPG) works better with P4 (Docker Hub Image) published
- P6 (Airflow) can reuse patterns from P7 (CLI) or vice versa

Everything else is independent and can be built in any order.

---

## Cross-Cutting Concerns

### Documentation Standard

Every ecosystem project must include:
- **README.md** — Quick start (copy-paste in <2 minutes), prerequisites, configuration
- **CHANGELOG.md** — Semantic versioned history
- **LICENSE** — Same as pg_trickle (PostgreSQL license)
- **CI badge** — Build status in README

### Versioning

All ecosystem projects follow semantic versioning. The major version tracks compatibility
with pg_trickle's SQL API:
- pg_trickle 0.x → ecosystem projects 0.x (unstable API)
- pg_trickle 1.0 → ecosystem projects 1.0+ (stable API)

### Testing Against pg_trickle

Every project that calls pg_trickle SQL functions must have integration tests running
against a real PostgreSQL 18 instance with pg_trickle installed. Use the existing
`tests/Dockerfile.e2e` as the base test image, or the Docker Hub image (Project 4)
once published.

### Shared CI Infrastructure

Projects can share GitHub Actions workflows:
- Reusable workflow for "spin up PostgreSQL 18 + pg_trickle" as a service container
- Reusable workflow for "build pg_trickle Docker image" (needed by all integration tests)

### SQL API Stability Contract

All ecosystem projects depend on pg_trickle's SQL API surface. Changes to these functions
require coordinated updates:

| Function | Used By |
|----------|---------|
| `pgtrickle.create_stream_table()` | P1, P6, P7, P8, P10 |
| `pgtrickle.alter_stream_table()` | P1, P6, P7, P8, P10 |
| `pgtrickle.drop_stream_table()` | P1, P6, P7, P8, P10 |
| `pgtrickle.refresh_stream_table()` | P1, P6, P7, P8, P11 |
| `pgtrickle.pg_stat_stream_tables` (view) | P1, P2, P3, P6, P7, P8, P11 |
| `pgtrickle.check_cdc_health()` | P2, P3, P6, P7, P8 |
| `pgtrickle.explain_st()` | P7, P8 |
| `pgtrickle.get_refresh_history()` | P7, P8 |
| `pgtrickle.pgt_stream_tables` (table) | P1, P7, P8, P10, P11 |
| `pgtrickle.stream_tables_info` (view) | P7, P11 |

Before making breaking changes to any of these, check the "Used By" column and update
the corresponding ecosystem projects.

---

## Total Effort Summary

| Project | Effort | Priority |
|---------|--------|----------|
| P1: dbt Macro Package | 15h | Phase 3 |
| P2: Prometheus Config | 4h | Phase 1 |
| P3: Grafana Dashboard | 4h | Phase 1 |
| P4: Docker Hub Image | 8h | Phase 2 |
| P5: CNPG Integration | 12h | Phase 2 |
| P6: Airflow Provider | 16h | Phase 4 |
| P7: CLI Tool | 20h | Phase 4 |
| P8: dbt Adapter | 54h | Phase 5 |
| P9: PGXN & OS Packages | 20h | Phase 5 |
| P10: Flyway/Liquibase | 8–24h | Phase 5 |
| P11: ORM Integrations | 32h | Phase 5 |
| **Total** | **~193–209h** | |

The first two phases (Observability + Distribution) deliver the highest adoption impact
for the lowest effort: ~28 hours for Prometheus, Grafana, Docker Hub, and CNPG.
