# The Medallion Architecture Lives Inside PostgreSQL

## Bronze, Silver, Gold — without Spark, without Airflow, without leaving your database

---

The medallion architecture is a data engineering pattern. Raw data lands in a Bronze layer. It gets cleaned and deduplicated into Silver. Business-level aggregates live in Gold. Dashboards and applications read from Gold.

The pattern came from the Spark/Databricks world. Most implementations involve Spark jobs, Delta Lake tables, an Airflow DAG to orchestrate the pipeline, and a scheduler to run it all. The Bronze-to-Silver-to-Gold pipeline typically runs on a schedule — hourly, maybe every 15 minutes if you're aggressive.

pg_trickle implements the same architecture entirely inside PostgreSQL. No Spark. No Airflow. No external scheduler. Propagation time from Bronze to Gold: under 5 seconds.

---

## The Setup

Here's a concrete example: an e-commerce platform tracking orders, with fraud detection rules and executive-level KPIs.

### Bronze: Raw Ingest

Bronze is just your regular PostgreSQL tables. This is where your application writes.

```sql
CREATE TABLE orders (
    id          bigserial PRIMARY KEY,
    customer_id bigint NOT NULL,
    amount      numeric(12,2) NOT NULL,
    currency    text NOT NULL DEFAULT 'USD',
    status      text NOT NULL DEFAULT 'pending',
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE customers (
    id       bigint PRIMARY KEY,
    name     text NOT NULL,
    region   text NOT NULL,
    tier     text NOT NULL DEFAULT 'standard',
    email    text
);
```

Nothing special here. No CDC pipelines, no event sourcing. Just tables.

### Silver: Cleaned, Enriched, Joined

The Silver layer is a stream table that joins, cleans, and enriches the raw data:

```sql
SELECT pgtrickle.create_stream_table(
    'silver_orders',
    $$SELECT
        o.id AS order_id,
        o.customer_id,
        c.name AS customer_name,
        c.region,
        c.tier AS customer_tier,
        o.amount,
        o.currency,
        o.status,
        o.created_at,
        CASE
          WHEN o.amount > 10000 AND c.tier = 'standard' THEN true
          ELSE false
        END AS flagged_for_review
      FROM orders o
      JOIN customers c ON c.id = o.customer_id$$,
    schedule     => '2s',
    refresh_mode => 'DIFFERENTIAL'
);
```

`silver_orders` is a real table. You can index it, query it with arbitrary WHERE clauses, put a GIN index on it for full-text search if you want. It updates within 2 seconds of any change to `orders` or `customers`.

Notice the `flagged_for_review` column — Silver isn't just a raw copy, it's enriched with business logic at the SQL level.

### Gold: Business Aggregates

The Gold layer builds on Silver. It aggregates into the shape your dashboards and APIs actually need:

```sql
-- Regional revenue KPIs
SELECT pgtrickle.create_stream_table(
    'gold_revenue_by_region',
    $$SELECT
        region,
        date_trunc('day', created_at) AS day,
        COUNT(*) AS order_count,
        SUM(amount) AS revenue,
        AVG(amount) AS avg_order_value,
        COUNT(*) FILTER (WHERE flagged_for_review) AS flagged_count
      FROM silver_orders
      WHERE status != 'cancelled'
      GROUP BY region, date_trunc('day', created_at)$$,
    schedule     => '3s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Customer lifetime value
SELECT pgtrickle.create_stream_table(
    'gold_customer_ltv',
    $$SELECT
        customer_id,
        customer_name,
        region,
        customer_tier,
        SUM(amount) AS lifetime_value,
        COUNT(*) AS total_orders,
        MAX(created_at) AS last_order_at
      FROM silver_orders
      WHERE status != 'cancelled'
      GROUP BY customer_id, customer_name, region, customer_tier$$,
    schedule     => '3s',
    refresh_mode => 'DIFFERENTIAL'
);
```

---

## How the DAG Works

pg_trickle knows that `gold_revenue_by_region` depends on `silver_orders`, which depends on `orders` and `customers`. This forms a directed acyclic graph:

```
orders ──┐
          ├──→ silver_orders ──┬──→ gold_revenue_by_region
customers ┘                    └──→ gold_customer_ltv
```

The scheduler respects this ordering automatically. It won't refresh `gold_revenue_by_region` until `silver_orders` is up to date. You don't need to coordinate schedules — set the schedule on each layer and pg_trickle handles the propagation.

You can inspect the DAG:

```sql
SELECT * FROM pgtrickle.dependency_tree('gold_revenue_by_region');
```

This returns the full dependency chain, including depth level and refresh ordering.

---

## Why This Is Better Than the Spark Version

### Latency

The Spark medallion pipeline runs on a schedule — typically every 15 minutes to an hour. Between runs, your Gold layer is stale. pg_trickle's pipeline runs continuously. The end-to-end propagation time from an INSERT into `orders` to an updated row in `gold_revenue_by_region` is the sum of the schedules: 2 seconds (Silver) + 3 seconds (Gold) = 5 seconds worst case.

### Infrastructure

The Spark version requires:
- A Spark cluster (or Databricks workspace)
- Delta Lake or Iceberg for Bronze/Silver/Gold storage
- An Airflow/Dagster/Prefect DAG for orchestration
- A scheduler
- Monitoring for each step
- Permissions and credentials across systems

The pg_trickle version requires:
- PostgreSQL with the pg_trickle extension installed

That's it. The scheduler, CDC, DAG resolution, monitoring, and storage are all inside PostgreSQL.

### Consistency

In the Spark version, each layer is independently computed. If the Silver job fails halfway through, Gold may have partial data. Airflow retries help, but the state management is external.

In pg_trickle, each refresh cycle is a PostgreSQL transaction. It either fully commits or fully rolls back. There's no partial state. If a refresh fails, the stream table retains its previous consistent state and the scheduler retries on the next cycle.

### Cost

A dedicated Spark cluster costs real money — even on spot instances, the compute budget for a medallion pipeline is significant for small-to-medium teams. pg_trickle runs on your existing PostgreSQL instance. The marginal cost is CPU time during refresh cycles, which for most workloads is negligible.

---

## Adding More Layers

The DAG isn't limited to three levels. You can chain as many stream tables as your use case needs:

```sql
-- A "platinum" layer: top-10 customers per region, for the exec dashboard
SELECT pgtrickle.create_stream_table(
    'platinum_top_customers_by_region',
    $$SELECT region, customer_id, customer_name, lifetime_value
      FROM gold_customer_ltv
      ORDER BY lifetime_value DESC
      LIMIT 10$$,
    schedule     => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
```

pg_trickle supports arbitrary DAG depth. Each additional layer adds its schedule interval to the end-to-end propagation time, but the refresh cost per layer is proportional only to the delta at that layer — not the total data volume.

---

## Monitoring the Pipeline

Since every stream table has its own refresh stats, you can monitor the pipeline the same way you'd monitor any pg_trickle stream table:

```sql
-- Check freshness across all layers
SELECT
    st.pgt_name,
    st.refresh_mode,
    st.schedule,
    now() - st.data_timestamp AS staleness,
    st.consecutive_errors
FROM pgtrickle.pgt_status() st
WHERE st.pgt_name LIKE 'silver_%' OR st.pgt_name LIKE 'gold_%' OR st.pgt_name LIKE 'platinum_%'
ORDER BY st.pgt_name;
```

If you enable pg_trickle's self-monitoring (which itself uses stream tables), the extension watches its own refresh latency and alerts when a layer falls behind.

---

## When to Still Use Spark

pg_trickle's medallion architecture works inside a single PostgreSQL instance (or a Citus cluster). If your Bronze layer is petabytes of Parquet files in S3, Spark is the right tool. If your Bronze layer is a PostgreSQL table that your application writes to, the Spark pipeline was always overkill.

The dividing line is roughly: if your data fits in PostgreSQL, your medallion architecture should too.
