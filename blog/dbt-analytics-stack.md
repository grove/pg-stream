[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# dbt + pg_trickle: The Analytics Engineer's Stack

## Using dbt to manage stream tables that actually stay fresh

---

dbt transformed how analytics teams write SQL. You define models, dbt handles dependencies, testing, and documentation. You run `dbt run`, your warehouse updates.

But dbt has a freshness problem. `dbt run` is a batch operation. Between runs, your models are stale. Some teams run dbt every hour. Aggressive teams run it every 15 minutes. Very few run it more often than that, because the full model graph takes time to rebuild.

pg_trickle solves the freshness side. dbt solves the governance side. Together they give you continuously-fresh models that are also version-controlled, tested, and documented.

---

## What dbt-pgtrickle Does

`dbt-pgtrickle` is a dbt package that adds a `pgtrickle` materialization strategy. Instead of creating a table or view, it creates a stream table:

```yaml
# models/order_totals.sql
{{
  config(
    materialized='pgtrickle',
    schedule='5s',
    refresh_mode='DIFFERENTIAL'
  )
}}

SELECT
    customer_id,
    SUM(amount) AS total_spend,
    COUNT(*) AS order_count
FROM {{ ref('orders') }}
GROUP BY customer_id
```

When you run `dbt run`, dbt-pgtrickle calls `pgtrickle.create_stream_table()` (or `create_or_replace_stream_table()` if it already exists). The stream table is then maintained by pg_trickle's background worker — dbt doesn't need to refresh it.

---

## The Workflow

### Initial Setup

```bash
# Add to packages.yml
packages:
  - package: grove/dbt_pgtrickle
    version: ">=0.36.0"

# Install
dbt deps
```

### Defining Models

dbt models with the `pgtrickle` materialization look like regular SQL models, plus a few config parameters:

```yaml
# models/schema.yml
models:
  - name: revenue_by_region
    config:
      materialized: pgtrickle
      schedule: '3s'
      refresh_mode: DIFFERENTIAL
    columns:
      - name: region
        tests:
          - not_null
      - name: revenue
        tests:
          - not_null
```

```sql
-- models/revenue_by_region.sql
{{
  config(
    materialized='pgtrickle',
    schedule='3s',
    refresh_mode='DIFFERENTIAL'
  )
}}

SELECT
    c.region,
    date_trunc('day', o.created_at) AS order_date,
    SUM(o.amount) AS revenue,
    COUNT(*) AS order_count
FROM {{ source('app', 'orders') }} o
JOIN {{ source('app', 'customers') }} c ON c.id = o.customer_id
GROUP BY c.region, date_trunc('day', o.created_at)
```

### Running

```bash
# First run: creates all stream tables
dbt run

# Subsequent runs: recreates only models whose SQL changed
dbt run

# Full refresh: drops and recreates all stream tables
dbt run --full-refresh
```

`dbt run` is idempotent. If the model SQL hasn't changed, `create_or_replace_stream_table` detects the unchanged query and skips recreation. If the SQL has changed, it performs an online query migration — the stream table stays queryable during the rebuild.

### Testing

dbt tests work normally on stream tables. They're regular PostgreSQL tables, so `dbt test` runs SELECT queries against them:

```bash
# Run all tests
dbt test

# Test a specific model
dbt test --select revenue_by_region
```

One thing to be aware of: since stream tables update asynchronously (for DIFFERENTIAL mode), there's a small window where a test might fail because the latest source data hasn't propagated yet. The dbt-pgtrickle package adds a `pgtrickle_wait_fresh` test helper that blocks until the stream table's frontier is current:

```sql
-- tests/revenue_by_region_is_fresh.sql
SELECT * FROM pgtrickle.get_staleness('revenue_by_region')
WHERE staleness > interval '10 seconds'
```

### Documentation

`dbt docs generate` works as expected. Stream tables appear in the documentation graph with their dependencies. The dbt-pgtrickle package adds custom metadata to the docs: schedule, refresh mode, average refresh latency, and last refresh timestamp.

---

## DAG Integration

dbt manages the dependency graph between models. pg_trickle manages the refresh dependency graph between stream tables. These two DAGs align automatically when you use `{{ ref() }}`:

```sql
-- models/silver_orders.sql (stream table)
{{ config(materialized='pgtrickle', schedule='2s', refresh_mode='DIFFERENTIAL') }}

SELECT o.*, c.region, c.tier
FROM {{ source('app', 'orders') }} o
JOIN {{ source('app', 'customers') }} c ON c.id = o.customer_id

-- models/gold_revenue.sql (stream table, depends on silver)
{{ config(materialized='pgtrickle', schedule='3s', refresh_mode='DIFFERENTIAL') }}

SELECT region, SUM(amount) AS revenue, COUNT(*) AS orders
FROM {{ ref('silver_orders') }}
GROUP BY region
```

When dbt builds `gold_revenue`, it knows to build `silver_orders` first. pg_trickle's scheduler knows that `gold_revenue` depends on `silver_orders` and won't refresh it until the upstream is current.

---

## Mixing Materializations

Not every model needs to be a stream table. You can mix materializations freely:

```yaml
models:
  - name: raw_events        # materialized: table (standard dbt)
  - name: cleaned_events    # materialized: pgtrickle (stream table)
  - name: event_summary     # materialized: pgtrickle (stream table)
  - name: monthly_report    # materialized: table (batch, runs nightly)
```

The stream tables update continuously. The batch tables update when you run `dbt run`. pg_trickle only manages the stream tables — the rest are standard dbt.

---

## Freshness Checks

dbt's `source freshness` feature checks whether source tables have been updated recently. For stream tables, you can also check whether the *stream table itself* is fresh:

```yaml
sources:
  - name: stream_tables
    freshness:
      warn_after: {count: 10, period: second}
      error_after: {count: 30, period: second}
    loaded_at_field: data_timestamp
    tables:
      - name: revenue_by_region
```

`dbt source freshness` will now alert if `revenue_by_region` is more than 30 seconds stale — meaning pg_trickle's scheduler has fallen behind.

---

## When to Use Which

| Scenario | Materialization |
|---|---|
| Data changes continuously, needs to be fresh | `pgtrickle` |
| Data is loaded in bulk nightly, batch is fine | `table` |
| Lightweight derivation, no storage needed | `view` |
| Read-your-writes required in the application | `pgtrickle` with IMMEDIATE mode |
| Historical snapshots for audit trail | `snapshot` (standard dbt) |

The rule of thumb: if you're running `dbt run` more than once an hour because users complain about stale data, switch that model to `pgtrickle` materialization and let pg_trickle handle the freshness.
