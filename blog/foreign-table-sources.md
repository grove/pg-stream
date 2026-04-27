[← Back to Blog Index](README.md)

# Foreign Tables as Stream Table Sources

## IVM over data that lives in another database — or in S3

---

Your data isn't all in one PostgreSQL database. Some of it is in another PostgreSQL instance across the network (`postgres_fdw`). Some of it is in Parquet files on S3 (`parquet_fdw`). Some of it comes from a CSV feed (`file_fdw`).

Can you create a stream table that aggregates across these sources? Yes — with caveats.

pg_trickle supports foreign tables as stream table sources. The CDC mechanism is different (no triggers on foreign tables, so polling-based detection is used), and the performance characteristics are different (every change detection requires a full scan of the foreign table). But it works, and for small-to-medium foreign tables, it works well.

---

## How It Works

Regular source tables use trigger-based CDC: a row-level trigger fires on every DML and writes the change to a buffer table. Foreign tables can't have triggers (in most FDW implementations), so pg_trickle uses a different approach:

**Polling-based change detection:**

1. On each refresh cycle, pg_trickle reads the current contents of the foreign table.
2. It compares the current contents with the last known state (using content hashing).
3. Rows that are new, changed, or deleted are identified and written to the change buffer.
4. The delta query proceeds as normal.

Step 2 is the expensive part. It requires a full scan of the foreign table. For a 1-million-row foreign table, this means a full network round-trip and comparison on every refresh cycle.

---

## Setting Up

```sql
-- Foreign server to another PostgreSQL instance
CREATE SERVER remote_analytics FOREIGN DATA WRAPPER postgres_fdw
  OPTIONS (host 'analytics-db', dbname 'analytics', port '5432');

CREATE USER MAPPING FOR CURRENT_USER SERVER remote_analytics
  OPTIONS (user 'reader', password 'secret');

-- Foreign table
CREATE FOREIGN TABLE remote_products (
  id INT,
  name TEXT,
  category TEXT,
  price NUMERIC
) SERVER remote_analytics OPTIONS (table_name 'products');

-- Stream table using the foreign table
SELECT pgtrickle.create_stream_table(
  name  => 'product_summary',
  query => $$
    SELECT
      rp.category,
      COUNT(*) AS product_count,
      AVG(rp.price) AS avg_price,
      MIN(rp.price) AS min_price
    FROM remote_products rp
    GROUP BY rp.category
  $$,
  schedule => '30s',
  cdc_mode => 'trigger'  -- only trigger mode works with FDW
);
```

**Note:** You must use `cdc_mode => 'trigger'` (or `'auto'` which starts with triggers). WAL-based CDC doesn't work with foreign tables because foreign table DML doesn't produce local WAL records.

pg_trickle detects that `remote_products` is a foreign table and automatically switches to polling-based change detection for that source.

---

## Mixed Sources: Foreign + Local

Stream tables can reference both foreign and local tables:

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'order_product_summary',
  query => $$
    SELECT
      o.customer_id,
      rp.category,
      SUM(o.total) AS total_spent,
      COUNT(*) AS order_count
    FROM orders o
    JOIN remote_products rp ON rp.id = o.product_id
    GROUP BY o.customer_id, rp.category
  $$,
  schedule => '10s'
);
```

Here:
- `orders` is a local table → trigger-based CDC (fast, per-row).
- `remote_products` is foreign → polling-based detection (full scan).

When `orders` changes, the delta is computed using only the changed orders (trigger CDC). When `remote_products` changes, pg_trickle detects the change via polling and recomputes the affected groups.

The practical effect: changes to local tables are reflected in 10 seconds (the schedule). Changes to foreign tables are also reflected in 10 seconds, but each check requires a full scan of the foreign table.

---

## File-Based FDWs

`file_fdw` reads from CSV/TSV files on the server's filesystem:

```sql
CREATE FOREIGN TABLE exchange_rates (
  currency TEXT,
  rate NUMERIC,
  effective_date DATE
) SERVER file_server OPTIONS (filename '/data/exchange_rates.csv', format 'csv');

SELECT pgtrickle.create_stream_table(
  name  => 'revenue_in_usd',
  query => $$
    SELECT
      o.customer_id,
      SUM(o.total * er.rate) AS revenue_usd
    FROM orders o
    JOIN exchange_rates er ON er.currency = o.currency
      AND er.effective_date = CURRENT_DATE
    GROUP BY o.customer_id
  $$,
  schedule => '1m'
);
```

When the CSV file is updated (new exchange rates), pg_trickle detects the change on the next polling cycle and recomputes the affected aggregates.

**Caveat:** `file_fdw` doesn't support transactions. If the file is updated while pg_trickle is reading it, you might get inconsistent results. Use atomic file replacement (write to a temp file, then `mv`) to avoid this.

---

## Parquet and S3

With `parquet_fdw` or `parquet_s3_fdw`:

```sql
CREATE FOREIGN TABLE s3_events (
  event_id BIGINT,
  event_type TEXT,
  payload JSONB,
  created_at TIMESTAMP
) SERVER parquet_s3 OPTIONS (
  filename 's3://my-bucket/events/*.parquet'
);

SELECT pgtrickle.create_stream_table(
  name  => 'event_type_counts',
  query => $$
    SELECT event_type, COUNT(*) AS cnt
    FROM s3_events
    GROUP BY event_type
  $$,
  schedule => '5m'
);
```

This brings S3 data into pg_trickle's IVM pipeline. The full scan of S3 data happens every 5 minutes (the schedule), and only changes are propagated to the stream table.

**Performance note:** S3 reads are slow compared to local tables. Set a longer schedule (minutes, not seconds) to amortize the scan cost.

---

## Performance Characteristics

| Source Type | CDC Method | Per-Cycle Cost | Best Schedule |
|-------------|-----------|----------------|---------------|
| Local table (with PK) | Trigger | O(delta) | 1s–10s |
| Local table (no PK) | Trigger + content hash | O(delta) | 1s–10s |
| Foreign table (postgres_fdw) | Polling | O(foreign table size) | 10s–5m |
| Foreign table (file_fdw) | Polling | O(file size) | 1m–1h |
| Foreign table (parquet_fdw/S3) | Polling | O(S3 read latency + data size) | 5m–1h |

The fundamental limitation: foreign table change detection requires a full scan because there's no trigger or WAL mechanism to capture individual changes. The schedule should be set long enough that the scan cost is acceptable.

---

## Optimization: Materialize First

For large foreign tables or frequent refreshes, consider materializing the foreign data into a local table first:

```sql
-- Local copy of foreign data
CREATE TABLE local_products AS SELECT * FROM remote_products;

-- Periodic sync (pg_cron)
SELECT cron.schedule('sync-products', '*/5 * * * *', $$
  TRUNCATE local_products;
  INSERT INTO local_products SELECT * FROM remote_products;
$$);

-- Stream table uses local copy (trigger CDC, fast)
SELECT pgtrickle.create_stream_table(
  name  => 'product_summary',
  query => $$ SELECT ... FROM local_products ... $$,
  schedule => '5s'
);
```

This separates the foreign-table sync (every 5 minutes, full scan) from the stream table refresh (every 5 seconds, trigger-based delta). The stream table gets fast incremental maintenance; the foreign data sync happens on a longer cadence.

---

## Summary

Foreign tables work as stream table sources with polling-based change detection. Every refresh cycle requires a full scan of the foreign table to detect changes. This is inherently slower than trigger-based CDC but enables IVM over data that lives outside PostgreSQL.

Use foreign tables directly when:
- The foreign table is small (<100K rows)
- The refresh schedule is long enough to amortize the scan cost
- Simplicity matters more than optimal performance

Materialize into a local table first when:
- The foreign table is large
- You need sub-second refresh latency
- The foreign table changes infrequently relative to local tables

Either way, pg_trickle handles the rest. Foreign or local, the delta rules are the same.
