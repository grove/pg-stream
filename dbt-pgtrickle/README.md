# dbt-pgtrickle

A [dbt](https://www.getdbt.com/) package that integrates
[pg_trickle](https://github.com/<org>/pg-trickle) stream tables into your dbt
project via a custom `stream_table` materialization.

No custom Python adapter required — works with the standard `dbt-postgres`
adapter. Just Jinja SQL macros that call pg_trickle's SQL API.

## Prerequisites

| Requirement | Minimum Version |
|-------------|----------------|
| dbt Core | ≥ 1.9 |
| dbt-postgres adapter | Matching dbt Core version |
| PostgreSQL | 18.x |
| pg_trickle extension | ≥ 0.1.0 (`CREATE EXTENSION pg_trickle;`) |

## Installation

Add to your `packages.yml`:

```yaml
packages:
  - git: "https://github.com/<org>/pg-trickle.git"
    revision: v0.1.0
    subdirectory: "dbt-pgtrickle"
```

Then run:

```bash
dbt deps
```

## Quick Start

Create a model with `materialized='stream_table'`:

```sql
-- models/marts/order_totals.sql
{{
  config(
    materialized='stream_table',
    schedule='5m',
    refresh_mode='DIFFERENTIAL'
  )
}}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count
FROM {{ source('raw', 'orders') }}
GROUP BY customer_id
```

```bash
dbt run --select order_totals   # Creates the stream table
dbt test --select order_totals  # Tests work normally (it's a real table)
```

## Configuration Reference

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `materialized` | string | — | Must be `'stream_table'` |
| `schedule` | string/null | `'1m'` | Refresh schedule (e.g., `'5m'`, `'1h'`, cron). `null` for pg_trickle's CALCULATED schedule. |
| `refresh_mode` | string | `'DIFFERENTIAL'` | `'FULL'`, `'DIFFERENTIAL'`, `'AUTO'`, or `'IMMEDIATE'` |
| `initialize` | bool | `true` | Populate on creation |
| `status` | string/null | `null` | `'ACTIVE'` or `'PAUSED'`. When set, applies on subsequent runs via `alter_stream_table()`. |
| `stream_table_name` | string | model name | Override stream table name |
| `stream_table_schema` | string | target schema | Override schema |
| `cdc_mode` | string/null | `null` | CDC mode override: `'auto'`, `'trigger'`, or `'wal'`. `null` uses the GUC default. |
| `partition_by` | string/null | `null` | Column name for RANGE partitioning of the storage table (v0.13.0+). Cannot be changed after creation. |
| `fuse` | string/null | `null` | Fuse circuit-breaker mode: `'off'`, `'on'`, or `'auto'` (v0.13.0+). Applied via `alter_stream_table()` on every run; no-op if unchanged. |
| `fuse_ceiling` | int/null | `null` | Change-count threshold that triggers the fuse (v0.13.0+). `null` uses the global GUC default. |
| `fuse_sensitivity` | int/null | `null` | Number of consecutive over-ceiling observations before the fuse blows (v0.13.0+). `null` means 1 (immediate). |

### `partition_by` — RANGE partitioning

Partition the stream table's storage table by a column value. pg_trickle creates a `PARTITION BY RANGE (<col>)` storage table with a default catch-all partition. Add your own date/integer range partitions via standard PostgreSQL DDL after `dbt run`.

```sql
-- models/marts/events_by_day.sql
{{ config(
    materialized='stream_table',
    schedule='1m',
    refresh_mode='DIFFERENTIAL',
    partition_by='event_day'
) }}

SELECT
    event_day,
    user_id,
    COUNT(*) AS event_count
FROM {{ source('raw', 'events') }}
GROUP BY event_day, user_id
```

> **Note:** `partition_by` is applied only at creation time. Changing it after the stream table exists has no effect. Use `dbt run --full-refresh` to recreate with a new partition key.

### `fuse` — Circuit breaker

The fuse circuit breaker suspends refreshes when the change volume exceeds a threshold, protecting against runaway refresh cycles during bulk ingestion.

```sql
-- models/marts/order_totals.sql
{{ config(
    materialized='stream_table',
    schedule='5m',
    refresh_mode='DIFFERENTIAL',
    fuse='auto',
    fuse_ceiling=50000,
    fuse_sensitivity=3
) }}

SELECT customer_id, SUM(amount) AS total
FROM {{ source('raw', 'orders') }}
GROUP BY customer_id
```

| `fuse` value | Behaviour |
|-------------|-----------|
| `'off'` | Fuse disabled (default) |
| `'on'` | Fuse always active; blows when ceiling is exceeded |
| `'auto'` | Fuse activates only when the delta is large enough to make FULL refresh cheaper than DIFFERENTIAL |

Fuse parameters are applied on every `dbt run` via `alter_stream_table()` — only calls the SQL function when the values have genuinely changed from the catalog state.

### Project-level defaults

```yaml
# dbt_project.yml
models:
  my_project:
    marts:
      +materialized: stream_table
      +schedule: '5m'
      +refresh_mode: DIFFERENTIAL
```

## Operations

### `pgtrickle_refresh` — Manual refresh

```bash
dbt run-operation pgtrickle_refresh --args '{"model_name": "order_totals"}'
```

### `refresh_all_stream_tables` — Refresh all in dependency order

Refreshes all dbt-managed stream tables in topological (dependency) order.
Upstream tables are refreshed before downstream ones. Designed for CI pipelines:
run after `dbt run` and before `dbt test` to ensure all data is current.

```bash
# Refresh all dbt-managed stream tables
dbt run-operation refresh_all_stream_tables

# Refresh only stream tables in a specific schema
dbt run-operation refresh_all_stream_tables --args '{"schema": "analytics"}'
```

### `drop_all_stream_tables` — Drop dbt-managed stream tables

Drops only stream tables defined as dbt models (safe in shared environments):

```bash
dbt run-operation drop_all_stream_tables
```

### `drop_all_stream_tables_force` — Drop ALL stream tables

Drops everything from the pg_trickle catalog, including non-dbt stream tables:

```bash
dbt run-operation drop_all_stream_tables_force
```

### `pgtrickle_check_cdc_health` — CDC pipeline health

```bash
dbt run-operation pgtrickle_check_cdc_health
```

Raises an error (non-zero exit) if any CDC source is unhealthy.

## Freshness Monitoring

Native `dbt source freshness` is not supported (the `last_refresh_at` column lives in
the catalog, not on the stream table). Use the `pgtrickle_check_freshness` run-operation
instead:

```bash
# Check all active stream tables (defaults: warn=600s, error=1800s)
dbt run-operation pgtrickle_check_freshness

# Custom thresholds
dbt run-operation pgtrickle_check_freshness \
  --args '{model_name: order_totals, warn_seconds: 300, error_seconds: 900}'
```

**Exits non-zero** when any stream table exceeds the error threshold — safe for CI.

## Useful `dbt` Commands

```bash
# List all stream table models
dbt ls --select config.materialized:stream_table

# Full refresh (drop + recreate)
dbt run --select order_totals --full-refresh

# Build models + tests in DAG order
dbt build --select order_totals
```

Note: `dbt build` runs stream table models early in the DAG. If downstream models
depend on a stream table with `initialize: false`, the table may not be populated yet.

## Testing

Stream tables are standard PostgreSQL heap tables — all dbt tests work normally:

```yaml
models:
  - name: order_totals
    columns:
      - name: customer_id
        tests:
          - not_null
          - unique
```

### Stream Table Health Test

Use the built-in `stream_table_healthy` generic test to fail your dbt test suite
when a stream table is stale, erroring, or paused:

```yaml
models:
  - name: order_totals
    tests:
      - dbt_pgtrickle.stream_table_healthy:
          warn_seconds: 300  # fail if stale for more than 5 minutes
```

The test queries `pgtrickle.pg_stat_stream_tables` and returns rows for any
unhealthy condition. An empty result set means the stream table is healthy.

### Stream Table Status Macro

For more programmatic control, use the `pgtrickle_stream_table_status()` macro
directly in custom tests or run-operations:

```sql
{%- set st = dbt_pgtrickle.pgtrickle_stream_table_status('order_totals', warn_seconds=300) -%}
{# st.status is one of: 'healthy', 'stale', 'erroring', 'paused', 'not_found' #}
{# st.staleness_seconds, st.consecutive_errors, st.total_refreshes, etc. #}
```

## `__pgt_row_id` Column

pg_trickle adds an internal `__pgt_row_id` column to stream tables for row identity
tracking. This column:

- Appears in `SELECT *` and `dbt docs generate`
- Does **not** affect `dbt test` unless you check column counts
- Can be documented to reduce confusion:

```yaml
columns:
  - name: __pgt_row_id
    description: "Internal pg_trickle row identity hash. Ignore this column."
```

## Limitations

| Limitation | Workaround |
|------------|------------|
| No in-place query alteration | Materialization auto-drops and recreates when query changes |
| `__pgt_row_id` visible | Document it; exclude in downstream `SELECT` |
| No native `dbt source freshness` | Use `pgtrickle_check_freshness` run-operation |
| No `dbt snapshot` support | Snapshot the stream table as a regular table |
| Query change detection is whitespace-sensitive | dbt compiles deterministically; unnecessary recreations are safe |
| PostgreSQL 18 required | Extension requirement |
| Shared version tags with pg_trickle extension | Pin to specific git revision |

## Contributing

See [AGENTS.md](../AGENTS.md) for development guidelines and the
[implementation plan](../plans/dbt/PLAN_DBT_MACRO.md) for design rationale.

### Running tests locally

The quickest way (requires Docker and dbt installed):

```bash
# Full run — builds Docker image, starts container, runs tests, cleans up
just test-dbt

# Fast run — reuses existing Docker image (run after first build)
just test-dbt-fast
```

Or use the script directly with options:

```bash
cd dbt-pgtrickle/integration_tests/scripts

# Default: builds image, runs tests with dbt 1.9, cleans up
./run_dbt_tests.sh

# Skip image rebuild (faster iteration)
./run_dbt_tests.sh --skip-build

# Keep the container running after tests (for debugging)
./run_dbt_tests.sh --skip-build --keep-container

# Use a custom port (avoids conflicts with local PostgreSQL)
PGPORT=25432 ./run_dbt_tests.sh
```

### Manual testing against an existing pg_trickle instance

If you already have PostgreSQL 18 + pg_trickle running locally:

```bash
export PGHOST=localhost PGPORT=5432 PGUSER=postgres PGPASSWORD=postgres PGDATABASE=postgres
cd dbt-pgtrickle/integration_tests
dbt deps
dbt seed
dbt run
./scripts/wait_for_populated.sh order_totals 30
dbt test
dbt run-operation drop_all_stream_tables
```

## License

Apache 2.0 — see [LICENSE](../LICENSE).
