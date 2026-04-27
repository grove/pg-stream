# Incremental Aggregates in PostgreSQL: No ETL Required

## Running SUM, COUNT, AVG, and vector_avg over live tables without batch jobs

---

Every analytics system eventually needs pre-aggregated data. Raw tables get too big. Query latency becomes unacceptable. You start maintaining a separate summary table.

Then you need to keep it fresh. That's where things go wrong.

The standard playbook is to build an ETL pipeline: detect changes, compute new aggregates, write them back. This works. It also means you now own an ETL pipeline — a background process, a job scheduler, failure handling, backlog monitoring, and a latency SLA that's measured in minutes rather than seconds.

The question this post answers: do you actually need the ETL layer? For most SQL-expressible aggregates, the answer is no. pg_trickle can maintain running aggregates directly in PostgreSQL, incrementally, using the algebra of the aggregate functions themselves.

---

## Why Batch Aggregation Is the Default

Aggregate queries are inherently read-heavy. Computing `SUM(revenue)` over 100 million orders requires reading 100 million rows. Computing it over 1 billion rows requires reading 1 billion rows. The compute cost scales linearly with the data.

When you need that aggregate continuously — refreshed every minute, every 10 seconds — you hit a ceiling. At some table size, the scan takes longer than the interval. You can't run a 30-second aggregate every 10 seconds.

The usual response is to compute the aggregate less often (accept stale data), or to push the computation out of PostgreSQL into a streaming system better suited to continuous processing (accept operational complexity).

Both choices feel like giving up something real. Incremental aggregation avoids that tradeoff.

---

## The Algebraic Trick

The reason incremental aggregation is possible is that most aggregates have a well-defined delta function.

For `SUM(x)`:
- Insert a row with value `v`: delta = `+v`
- Delete a row with value `v`: delta = `-v`
- Update a row from `old_v` to `new_v`: delta = `+(new_v - old_v)`

For `COUNT(*)`:
- Insert: delta = `+1`
- Delete: delta = `-1`
- Update: delta = `0` (row count unchanged)

For `AVG(x)`:
- Maintain `running_sum` and `running_count` separately
- AVG = `running_sum / running_count`
- Update `running_sum` and `running_count` with the delta
- Recompute `AVG`

This is O(1) per change, regardless of table size. The key insight is that `SUM`, `COUNT`, and `AVG` are all expressible in terms of a running state that can be updated with each delta. Mathematicians call this a *semigroup* with an *inverse* — each operation has a reverse (subtraction for addition, decrement for increment) that lets you both add and remove elements from the aggregate.

This is the mathematics underlying pg_trickle's DVM (differential view maintenance) engine. The engine carries a ruleset of delta functions for each supported aggregate and applies them when changes arrive.

---

## Simple Aggregates: SUM, COUNT, AVG

```sql
-- Maintain real-time revenue metrics by region and day
SELECT pgtrickle.create_stream_table(
  name         => 'daily_revenue',
  query        => $$
    SELECT
      c.region,
      date_trunc('day', o.created_at) AS day,
      SUM(o.total)                    AS revenue,
      COUNT(*)                        AS order_count,
      AVG(o.total)                    AS avg_order_value,
      COUNT(DISTINCT o.customer_id)   AS unique_customers
    FROM orders o
    JOIN customers c ON c.id = o.customer_id
    GROUP BY c.region, date_trunc('day', o.created_at)
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

When an order is inserted:
1. pg_trickle identifies the affected group `(region='europe', day='2026-04-27')`.
2. The DVM engine computes `delta_revenue = +order.total`, `delta_count = +1`, `new_avg = (old_sum + order.total) / (old_count + 1)`.
3. A single UPDATE is applied to `daily_revenue` for that one row.

An order placed at 14:32 is reflected in `daily_revenue` within 5 seconds. No batch job. No ETL. No Kafka.

Note the `COUNT(DISTINCT customer_id)` — this is a case where the delta is more complex. DISTINCT counts require tracking membership, not just a running tally. pg_trickle handles this with a set-based approach, but for very high-cardinality distinct counts, `HyperLogLog` approximation is often more practical and is on the roadmap.

---

## Conditional Aggregates

Real summary tables always have conditional logic — active vs. inactive, successful vs. failed, above/below threshold.

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'support_dashboard',
  query        => $$
    SELECT
      team_id,
      COUNT(*)                                             AS total_tickets,
      COUNT(*) FILTER (WHERE status = 'open')             AS open_tickets,
      COUNT(*) FILTER (WHERE status = 'urgent')           AS urgent_tickets,
      COUNT(*) FILTER (WHERE status = 'resolved'
                       AND resolved_at > NOW() - INTERVAL '24 hours')
                                                          AS resolved_today,
      AVG(EXTRACT(EPOCH FROM (resolved_at - created_at)))
        FILTER (WHERE resolved_at IS NOT NULL)            AS avg_resolution_secs,
      MAX(created_at)                                     AS latest_ticket_at
    FROM tickets
    GROUP BY team_id
  $$,
  schedule     => '3 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

The `FILTER (WHERE ...)` construct is a standard SQL conditional aggregate. pg_trickle handles it by carrying the condition into the delta computation. A ticket transitioning from `open` to `resolved` generates:
- `delta_open_tickets = -1`
- `delta_resolved_today = +1` (if within the 24-hour window)
- A recomputation of `avg_resolution_secs`

The time-window filter (`resolved_at > NOW() - INTERVAL '24 hours'`) introduces a subtlety: rows can age out of the window without any DML. This is handled by pg_trickle's time-window eviction logic, which runs during each refresh cycle to evict rows that have moved outside their window bounds.

---

## Multi-Table Aggregates With Joins

The place where batch ETL is hardest to replace is multi-table aggregation — computing aggregates over data that spans multiple source tables.

```sql
-- Per-author engagement metrics: articles + reactions + comments
SELECT pgtrickle.create_stream_table(
  name         => 'author_engagement',
  query        => $$
    SELECT
      u.id            AS author_id,
      u.display_name,
      COUNT(DISTINCT a.id)                        AS article_count,
      SUM(r.count)                                AS total_reactions,
      COUNT(DISTINCT c.id)                        AS total_comments,
      ROUND(SUM(r.count)::numeric /
        NULLIF(COUNT(DISTINCT a.id), 0), 2)       AS avg_reactions_per_article
    FROM users u
    LEFT JOIN articles a ON a.author_id = u.id
      AND a.published = true
    LEFT JOIN article_reactions r ON r.article_id = a.id
    LEFT JOIN comments c ON c.article_id = a.id
    GROUP BY u.id, u.display_name
  $$,
  schedule     => '10 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

When a reaction is added to an article by author 42:
1. The CDC trigger on `article_reactions` fires, recording the INSERT.
2. The DVM engine evaluates the join delta: this reaction belongs to an article by author 42.
3. The engine updates the `author_id = 42` row in `author_engagement`: `total_reactions += 1`, recompute `avg_reactions_per_article`.

No other rows are touched. Author 43's metrics are unaffected.

This is the join-propagation problem that makes hand-rolled incremental ETL so fragile: the change in `article_reactions` affects a row in the aggregate keyed by `author_id`, through an intermediate join on `articles`. The DVM engine handles this join-delta propagation automatically.

---

## Vector Aggregates: vector_avg

Here's where things get interesting for ML workloads.

The same algebraic principle that applies to `AVG(price)` applies to `AVG(embedding)` — a vector average is just an element-wise sum divided by a count. It's the same delta function applied to each dimension.

Arriving in v0.37:

```sql
-- Per-user taste vector: average embedding of liked items
SELECT pgtrickle.create_stream_table(
  name         => 'user_taste',
  query        => $$
    SELECT
      ul.user_id,
      vector_avg(i.embedding)   AS taste_vec,
      COUNT(*)                  AS like_count,
      MAX(ul.liked_at)          AS last_interaction
    FROM user_likes ul
    JOIN items i ON i.id = ul.item_id
    GROUP BY ul.user_id
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);

CREATE INDEX ON user_taste USING hnsw (taste_vec vector_cosine_ops);
```

When user 42 likes item 1701:
1. CDC captures the INSERT on `user_likes`.
2. The DVM engine retrieves `item 1701's embedding`.
3. Delta: `new_sum_vec = old_sum_vec + item_1701.embedding`, `new_count = old_count + 1`, `new_taste_vec = new_sum_vec / new_count`.
4. One UPDATE to `user_taste` for `user_id = 42`.

One million users, thousands of likes per second. Each refresh cycle touches only the users whose likes changed in that cycle. The HNSW index on `taste_vec` receives targeted updates, not a full rebuild.

This replaces the "recompute all user taste vectors every night" batch job with a continuously maintained table. Users get personalization that reflects their most recent action, not last night's.

---

## Percentile and Rank Aggregates: The Hard Cases

Not every aggregate has a clean delta function. Some require knowing the full distribution.

**`PERCENTILE_CONT` / `PERCENTILE_DISC`**: Median and other percentiles require sorted access to the full set of values. There's no O(1) way to update a percentile when a single value changes. These always require a full recomputation.

**`RANK()` / `DENSE_RANK()`**: Inserting one row can change the rank of every other row. This is an O(n) update in the worst case.

**`NTILE()`**: Same problem as rank.

For these, pg_trickle falls back to `refresh_mode => 'FULL'` — which is still useful if you want scheduled maintenance, monitoring, and the other benefits of stream tables, just without the differential speedup.

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'customer_revenue_ranks',
  query        => $$
    SELECT
      customer_id,
      total_revenue,
      RANK() OVER (ORDER BY total_revenue DESC) AS revenue_rank
    FROM customer_totals
  $$,
  schedule     => '1 minute',
  refresh_mode => 'FULL'   -- RANK() requires full recompute
);
```

This is still better than an unmanaged materialized view: the refresh is scheduled, monitored, and the table's freshness is visible via `pgtrickle.stream_table_status()`.

---

## Monitoring What You've Built

Once stream tables exist, pg_trickle exposes their state through a monitoring view:

```sql
SELECT
  name,
  refresh_mode,
  last_refresh_at,
  EXTRACT(EPOCH FROM (NOW() - last_refresh_at))::int AS staleness_secs,
  rows_changed_last_cycle,
  avg_refresh_ms,
  schedule
FROM pgtrickle.stream_table_status()
ORDER BY staleness_secs DESC;
```

This is what replaces the ETL job monitoring dashboard. You're not watching a Celery worker queue or a Kafka consumer lag counter — you're watching a PostgreSQL view that reports directly from the extension's catalog. One `SELECT` tells you everything.

---

## The Tradeoff

Incremental aggregation inside PostgreSQL is not free. The DVM engine adds overhead to the source write path — CDC triggers add roughly 50–200 microseconds per modified row, depending on how many stream tables reference that table.

For high-write workloads (thousands of writes per second), this overhead is worth measuring. pg_trickle's backpressure mechanism can be configured to shed load by falling back to full refresh when the write rate exceeds a configurable threshold.

But for the vast majority of OLTP workloads — hundreds to low thousands of writes per second — the overhead is negligible, and the elimination of the external ETL layer more than compensates.

The ETL pipeline that takes 40 minutes of engineering per incident and lives in a separate repository, separate monitoring system, and separate deployment pipeline? That's gone. The aggregate data is where it should have been all along: in the database, maintained by the database, queryable with SQL.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
