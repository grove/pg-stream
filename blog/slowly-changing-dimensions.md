# Slowly Changing Dimensions in Real Time

## SCD Type 2 without nightly ETL, without Airflow, without leaving PostgreSQL

---

Slowly changing dimensions are a data warehousing concept with a misleading name. There's nothing slow about them in practice — customer tiers change, product prices update, employee departments shift. The "slowly" just means the change frequency is lower than transactional data.

SCD Type 2 is the version where you keep history: when a customer moves from the "gold" tier to "platinum," you don't overwrite the old row. You close it (set `valid_to = now()`) and insert a new row (with `valid_from = now()`, `valid_to = NULL`). Every historical state is preserved.

The traditional implementation involves a nightly ETL job that compares today's snapshot with yesterday's, detects changes, closes old rows, and opens new ones. It runs in Airflow. It takes 45 minutes. If it fails, your dimension table is stale until someone notices.

pg_trickle can maintain SCD Type 2 tables continuously — no ETL, no scheduler outside PostgreSQL, no batch window.

---

## The Setup

Start with a standard customer table that your application writes to:

```sql
CREATE TABLE customers (
    id       bigint PRIMARY KEY,
    name     text NOT NULL,
    email    text NOT NULL,
    tier     text NOT NULL DEFAULT 'standard',
    region   text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
```

When the application updates a customer's tier, it just runs `UPDATE customers SET tier = 'platinum' WHERE id = 42`. No SCD logic in the application.

### The SCD Type 2 Stream Table

The SCD dimension is a stream table that tracks the history of changes:

```sql
-- Event log: capture every change to customers as an event
CREATE TABLE customer_changes (
    id              bigserial PRIMARY KEY,
    customer_id     bigint NOT NULL,
    name            text NOT NULL,
    email           text NOT NULL,
    tier            text NOT NULL,
    region          text NOT NULL,
    changed_at      timestamptz NOT NULL DEFAULT now()
);

-- Trigger to capture changes into the event log
CREATE OR REPLACE FUNCTION capture_customer_change() RETURNS trigger AS $$
BEGIN
    INSERT INTO customer_changes (customer_id, name, email, tier, region, changed_at)
    VALUES (NEW.id, NEW.name, NEW.email, NEW.tier, NEW.region, now());
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_customer_changes
    AFTER INSERT OR UPDATE ON customers
    FOR EACH ROW EXECUTE FUNCTION capture_customer_change();
```

Now build the SCD Type 2 dimension as a stream table:

```sql
SELECT pgtrickle.create_stream_table(
    'dim_customer_scd2',
    $$SELECT
        c1.customer_id,
        c1.name,
        c1.tier,
        c1.region,
        c1.changed_at AS valid_from,
        c2.changed_at AS valid_to
      FROM customer_changes c1
      LEFT JOIN customer_changes c2
        ON c2.customer_id = c1.customer_id
        AND c2.changed_at = (
            SELECT MIN(c3.changed_at)
            FROM customer_changes c3
            WHERE c3.customer_id = c1.customer_id
              AND c3.changed_at > c1.changed_at
        )$$,
    schedule     => '2s',
    refresh_mode => 'DIFFERENTIAL'
);
```

This produces a standard SCD Type 2 table:

| customer_id | name | tier | region | valid_from | valid_to |
|---|---|---|---|---|---|
| 42 | Alice | standard | europe | 2026-01-15 | 2026-03-22 |
| 42 | Alice | gold | europe | 2026-03-22 | 2026-04-10 |
| 42 | Alice | platinum | europe | 2026-04-10 | NULL |

The row with `valid_to = NULL` is the current state. Historical rows have both timestamps set.

---

## What Happens When a Dimension Changes

When the application runs `UPDATE customers SET tier = 'platinum' WHERE id = 42`:

1. The `capture_customer_change` trigger fires, inserting a new row into `customer_changes`.
2. pg_trickle's CDC trigger captures the insert into the change buffer.
3. Within 2 seconds, the scheduler refreshes `dim_customer_scd2`.
4. The differential engine computes the delta:
   - The previous "current" row (tier = 'gold', valid_to = NULL) gets its `valid_to` set.
   - A new "current" row (tier = 'platinum', valid_to = NULL) is inserted.
5. The MERGE applies both changes atomically.

The SCD table is up to date within 2 seconds of the source change. No nightly batch. No Airflow DAG. No manual intervention.

---

## Querying the SCD

### Current state

```sql
SELECT * FROM dim_customer_scd2
WHERE customer_id = 42 AND valid_to IS NULL;
```

### State at a point in time

```sql
SELECT * FROM dim_customer_scd2
WHERE customer_id = 42
  AND valid_from <= '2026-03-25'
  AND (valid_to IS NULL OR valid_to > '2026-03-25');
```

This returns the "gold" row — because on March 25, Alice was in the gold tier.

### JOIN with fact tables

```sql
-- Revenue by customer tier at the time of each order
SELECT
    d.tier AS tier_at_order_time,
    SUM(o.amount) AS revenue
FROM orders o
JOIN dim_customer_scd2 d
    ON d.customer_id = o.customer_id
    AND o.created_at >= d.valid_from
    AND (d.valid_to IS NULL OR o.created_at < d.valid_to)
GROUP BY d.tier;
```

This gives you revenue grouped by the customer's tier *at the time of the order* — not their current tier. This is the whole point of SCD Type 2.

---

## SCD Type 1: Overwrite

If you don't need history — just the current state, always overwritten — that's even simpler. A stream table with the right query is already SCD Type 1:

```sql
SELECT pgtrickle.create_stream_table(
    'dim_customer_current',
    $$SELECT id AS customer_id, name, email, tier, region
      FROM customers$$,
    schedule     => '2s',
    refresh_mode => 'DIFFERENTIAL'
);
```

Every time a customer is updated, the stream table reflects the new values within 2 seconds. No history, no `valid_from`/`valid_to` — just the latest state.

---

## Why Not Just Use Triggers?

You could implement SCD Type 2 with a trigger that directly maintains the dimension table. Many teams do. The problems:

1. **Write-path coupling.** The trigger runs in the application's transaction. If the SCD logic is slow (e.g., finding the previous row, closing it, inserting a new one), it adds latency to every customer update.

2. **Correctness under concurrency.** If two updates to the same customer happen concurrently, the trigger needs to handle the race condition. Getting the `valid_to` assignment right under concurrent writes is non-trivial.

3. **No monitoring.** If the trigger fails or produces incorrect data, you won't know until someone queries the dimension and gets wrong results.

4. **Maintenance burden.** Every schema change to the source table requires updating the trigger. Adding a column? Update the trigger. Renaming a column? Update the trigger.

With pg_trickle, the SCD logic is in a SQL query. Schema changes are handled by `alter_stream_table`. Monitoring is built in. Concurrency is handled by the engine's transaction isolation. The write path has no additional overhead beyond the CDC trigger (which is minimal and standard across all stream tables).

---

## Combining with the Medallion Pattern

SCD Type 2 dimensions fit naturally into a medallion architecture:

- **Bronze:** `customers` table (mutable, application-facing)
- **Silver:** `customer_changes` event log (append-only, captured by trigger)
- **Gold:** `dim_customer_scd2` stream table (SCD Type 2, maintained by pg_trickle)

The Gold layer is queryable by BI tools, analytics pipelines, and the application itself. It's always fresh. It's always correct. And it's just a PostgreSQL table.
