# Why Your Materialized Views Are Always Stale

## (And How to Fix It in 5 Lines of SQL)

---

You have a dashboard. It runs a complex query over millions of rows. Without a materialized view it takes 8 seconds. With one, it takes 12 milliseconds. You shipped the materialized view two months ago, put a `REFRESH MATERIALIZED VIEW` in a cron job, and declared victory.

Last week a customer asked why their newly-submitted order wasn't showing in the dashboard totals. You checked. The cron job had silently failed. The view had been stale for four days.

This is the normal lifecycle of a PostgreSQL materialized view in production. Not a horror story — just the quiet, predictable friction that accumulates when you build derived data on top of a full-scan refresh model.

Here's what that friction costs, why the standard fixes don't work at scale, and how incremental view maintenance changes the equation.

---

## What Materialized Views Actually Do

PostgreSQL's `MATERIALIZED VIEW` caches the result of a query. That's it. When you write `REFRESH MATERIALIZED VIEW orders_summary`, PostgreSQL executes the underlying `SELECT` in full, writes the results to disk, and replaces the cached copy.

The critical word is *full*. Every refresh scans every row in every source table referenced by the query, regardless of what changed. If your `orders_summary` view aggregates 50 million orders and three orders were placed since the last refresh, PostgreSQL still scans all 50 million.

This is fine when:
- The underlying tables are small
- You don't care about real-time freshness (data warehouse use case)
- Refreshes run on a schedule where staleness is acceptable

It breaks down when:
- Tables grow to tens of millions of rows
- Refreshes start taking minutes
- Freshness matters (operational dashboards, real-time analytics, user-facing data)

And it fails entirely when:
- Freshness is measured in seconds, not minutes
- The cost of a full scan exceeds the cost of the query the view was meant to replace

At that point, your materialized view has stopped being a cache and started being a liability.

---

## The Three Fixes That Don't Actually Fix It

When teams hit the staleness wall, they try variations of the same three solutions.

### Fix 1: Refresh More Often

The cron job runs every hour. Make it run every minute. Make it run every 10 seconds.

The problem is that `REFRESH MATERIALIZED VIEW` locks the view while it refreshes. During the refresh, no queries can read from it. For a view that takes 500ms to refresh, running it every 10 seconds means it's locked 5% of the time. Run it every second and it's locked 50% of the time.

PostgreSQL has `REFRESH MATERIALIZED VIEW CONCURRENTLY` which avoids the lock by maintaining two copies and atomically swapping, but it requires a unique index and takes roughly twice as long. The staleness improves; the cost doubles.

At some scale, you hit a ceiling: refreshes take longer than the interval between them. You can't run a 30-second refresh every 10 seconds.

### Fix 2: Partial Refresh with Manual Change Tracking

Some teams add an `updated_at` column to source tables and write refresh logic that only processes rows changed since the last run.

```sql
-- "Incremental" refresh for orders summary
INSERT INTO orders_summary
SELECT customer_id, SUM(total), COUNT(*)
FROM orders
WHERE updated_at > last_refresh_time
GROUP BY customer_id
ON CONFLICT (customer_id) DO UPDATE
  SET total = orders_summary.total + EXCLUDED.total,
      order_count = orders_summary.order_count + EXCLUDED.order_count;
```

This is closer to the right idea. But it's brittle in practice:
- It only works for INSERTs. Deletes and updates require separate handling.
- The ON CONFLICT delta logic is hand-coded and error-prone. If the same `customer_id` appears in two separate change windows, you can double-count.
- `updated_at` columns need to be maintained on every source table. Missing one breaks the logic.
- Multi-table JOINs make the "what changed" tracking exponentially harder. If `orders` joins to `customers` and a customer's `region` changes, you need to recompute that customer's regional summary even though no orders changed.

This approach works at small scale. Teams rebuild it correctly once, and then spend the next year patching edge cases.

### Fix 3: Move to a Different System

Elasticsearch, ClickHouse, Apache Flink, Materialize. These systems have proper incremental processing. They work. They also mean you're now running two data stores, with all the synchronization and consistency problems that entails.

For teams that don't need PostgreSQL-native queries, this is a legitimate choice. For the rest, it's the infrastructure equivalent of moving to a bigger house because you can't find a good plumber.

---

## What Incremental View Maintenance Actually Means

The core insight behind IVM is simple: for most queries, you don't need to recompute the full result when an input changes. You need to compute *the delta* and apply it.

For `SUM(total)`:
- Row inserted with `total = 150`: new sum = old sum + 150
- Row deleted with `total = 150`: new sum = old sum - 150
- Row updated from `total = 150` to `total = 200`: new sum = old sum + 50

For `COUNT(*)`:
- Row inserted: new count = old count + 1
- Row deleted: new count = old count - 1

These are O(1) operations. They don't depend on the size of the table. A table with 50 million rows and a table with 50 rows update in the same time if the delta has one row.

The challenge is that this algebraic property — the ability to express "how does the result change?" as a closed-form operation — doesn't hold for every query. Some aggregates (like `MEDIAN`) don't have a simple inverse. Some window functions require seeing the full neighborhood. But the aggregates that matter most in practice — SUM, COUNT, AVG, MIN/MAX with caveats, and now vector averages — all support it.

---

## pg_trickle: IVM as a PostgreSQL Extension

pg_trickle implements IVM inside PostgreSQL using a combination of trigger-based CDC and a differential dataflow engine.

The workflow is different from `REFRESH MATERIALIZED VIEW`. Instead of defining how to recompute, you define what you want maintained:

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'orders_summary',
  query        => $$
    SELECT
      c.region,
      date_trunc('day', o.created_at) AS order_date,
      SUM(o.total)                    AS revenue,
      COUNT(*)                        AS order_count,
      AVG(o.total)                    AS avg_order_value
    FROM orders o
    JOIN customers c ON c.id = o.customer_id
    GROUP BY c.region, date_trunc('day', o.created_at)
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

That's 5 lines. `orders_summary` is now a real table in your schema, queryable like any other table, with an HNSW or B-tree index if you want one.

When an order is inserted, pg_trickle's CDC triggers capture the change into a buffer. Every 5 seconds (or whatever interval you configure), the background worker drains the buffer, computes the delta using the DVM engine, and applies it to `orders_summary`. Only the affected rows change.

If 3 orders are placed in the `europe` region on a Monday, only one row in `orders_summary` is updated — the `(europe, 2026-04-27)` aggregate. The other 365×N region/day rows are untouched.

---

## What Changes

### Staleness

Your view is at most `schedule` seconds stale, continuously, without a cron job. The default schedule is 5 seconds. You can set it to 1 second for high-frequency data.

```sql
SELECT pgtrickle.alter_stream_table('orders_summary', schedule => '1 second');
```

There's no drift: every refresh cycle covers exactly the changes since the last one. There's no "catch-up" after a failure — the change buffers accumulate until the worker processes them, and the result is always consistent.

### Cost

A refresh cycle that changes 10 rows costs roughly the same whether your source tables have 1 million rows or 1 billion. The differential engine processes deltas, not the full dataset. The cost scales with the number of changes per cycle, not the size of the data.

For a table that's updated continuously by a high-write workload, expect a refresh cycle to take 10–100ms depending on the number of changed rows and the complexity of the query.

### Correctness

The CDC triggers run inside the same transaction as the change that caused them. If the original transaction rolls back, the change buffer entry is also rolled back. This means the view never reflects changes from transactions that didn't commit — a property that's surprisingly hard to guarantee with external batch pipelines.

---

## The JOIN Case: Where Batch Refreshes Fall Apart

The hardest case for batch incremental refresh is multi-table JOINs. Consider:

```sql
SELECT
  c.region,
  SUM(o.total) AS regional_revenue
FROM orders o
JOIN customers c ON c.id = o.customer_id
GROUP BY c.region;
```

If a customer changes region — `customer_id = 42` moves from `europe` to `north_america` — then:
1. Every order for `customer_id = 42` needs to move from `europe` to `north_america` in the aggregate.
2. This requires knowing the orders for that customer.
3. It also requires recomputing both the old and new regional aggregates.

A batch job keyed on `orders.updated_at` won't see this change at all — no orders were updated. A batch job keyed on `customers.updated_at` will see the customer record, but needs to then find and reprocess all their orders.

This is the join-delta problem, and it's why manual incremental refresh usually breaks down to "recompute everything for affected keys" — which is effectively a full-scan refresh for busy customers.

pg_trickle handles this through the DVM engine's join-delta rules. When `customers` row changes, the engine identifies the set of affected join keys, retrieves the relevant order aggregates using index lookups, and applies the correct delta. No full scan.

---

## When to Stick with REFRESH MATERIALIZED VIEW

pg_trickle isn't the right tool for every materialized view. Specifically:

- **Reporting/warehouse views**: If you run `REFRESH MATERIALIZED VIEW` once a day for a BI dashboard and daily staleness is acceptable, that's fine. The simplicity of the built-in mechanism beats the operational overhead of a new extension.

- **Very complex queries**: IVM requires that the query be expressible as a composition of differentiable operators. Some queries — particularly those with `DISTINCT`, `EXCEPT`, correlated subqueries, or non-monotone aggregates — require a full refresh even in pg_trickle. The extension is transparent about this: create the stream table with `refresh_mode => 'FULL'` for queries that need it, and `DIFFERENTIAL` for the ones that don't.

- **One-time or infrequent data**: If your source data is loaded in bulk once a day and doesn't change between loads, IVM overhead isn't justified. Use `REFRESH MATERIALIZED VIEW` after the bulk load.

The sweet spot for pg_trickle is operational data that changes continuously — orders, events, user actions, metrics, search corpora — where staleness causes user-visible problems and full-scan refreshes are either too slow or too expensive.

---

## A Concrete Example: A Support Team Dashboard

This is the kind of use case where the before/after is clean.

**Before:**
```sql
-- Ran as a cron job every 5 minutes
REFRESH MATERIALIZED VIEW CONCURRENTLY support_metrics;
-- Takes 45 seconds, locks the table for 90 seconds on busy days
-- Staleness: up to 5 minutes plus 45 seconds
```

**After:**
```sql
SELECT pgtrickle.create_stream_table(
  name         => 'support_metrics',
  query        => $$
    SELECT
      t.team_id,
      t.name AS team_name,
      COUNT(CASE WHEN tk.status = 'open' THEN 1 END)   AS open_tickets,
      COUNT(CASE WHEN tk.status = 'urgent' THEN 1 END) AS urgent_tickets,
      AVG(EXTRACT(EPOCH FROM (tk.resolved_at - tk.created_at)) / 3600)
        FILTER (WHERE tk.resolved_at IS NOT NULL)       AS avg_resolution_hours,
      MAX(tk.created_at)                                AS latest_ticket_at
    FROM teams t
    JOIN tickets tk ON tk.team_id = t.id
    GROUP BY t.team_id, t.name
  $$,
  schedule     => '3 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

Staleness: 3 seconds, maximum. Cost per refresh cycle: proportional to the number of new/changed tickets since last cycle — usually a handful. A dashboard page load goes from "wait 50ms for the MV query, which is sometimes stale" to "wait 2ms for the stream table read, always current."

---

## The 5 Lines

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'your_summary',
  query        => $$ /* your existing MV query here */ $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

If the DVM engine can't differentiate your query, it tells you at creation time. Fix it (usually by removing DISTINCT, rewiring a complex aggregate) or set `refresh_mode => 'FULL'` and schedule it as aggressively as the full-scan cost allows. At least the schedule is managed, monitored, and correct.

The cron job goes away. The staleness alert goes away. The view is just always fresh.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
