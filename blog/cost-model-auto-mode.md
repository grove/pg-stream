# The Cost Model: How pg_trickle Decides Whether to Refresh Differentially

## Inside the AUTO mode decision engine

---

pg_trickle supports three refresh modes: FULL (recompute everything), DIFFERENTIAL (apply only the delta), and IMMEDIATE (apply the delta in the source transaction).

There's a fourth option: `AUTO`. With AUTO mode, pg_trickle decides on each refresh cycle whether to use DIFFERENTIAL or FULL, based on a learned cost model.

---

## Why Not Always Use DIFFERENTIAL?

DIFFERENTIAL refresh is faster when the delta is small relative to the total data. If 10 rows changed out of 10 million, DIFFERENTIAL processes 10 rows. FULL scans 10 million.

But DIFFERENTIAL has overhead that FULL doesn't:
- **Change buffer management.** Reading from the change buffer, deduplicating, ordering.
- **Delta computation.** Running the delta rules through the operator tree.
- **Merge complexity.** The MERGE statement for DIFFERENTIAL is more complex than a simple INSERT.
- **State maintenance.** The engine maintains auxiliary data structures (group state for aggregates, join indexes for delta joins).

When the delta is large — say, 60% of the table was updated in a bulk operation — the DIFFERENTIAL overhead can exceed the cost of just recomputing from scratch. At that point, FULL refresh is faster.

The crossover point depends on the query, the data distribution, the table size, and the hardware. There's no universal threshold.

---

## The AUTO Mode Cost Model

When you set `refresh_mode => 'AUTO'`, pg_trickle evaluates a cost estimate at the beginning of each refresh cycle:

```sql
SELECT pgtrickle.create_stream_table(
    'orders_summary',
    $$SELECT region, SUM(amount), COUNT(*)
      FROM orders GROUP BY region$$,
    schedule     => '5s',
    refresh_mode => 'AUTO'
);
```

### The Decision Inputs

1. **Delta size.** The number of rows in the change buffer since the last refresh.
2. **Source table size.** The estimated row count of each source table (from `pg_class.reltuples`).
3. **Delta ratio.** `delta_size / source_table_size` — what fraction of the source changed.
4. **Query complexity.** Number of JOINs, aggregation groups, subqueries.
5. **Historical refresh times.** How long FULL and DIFFERENTIAL refreshes have actually taken for this stream table (learned from `pgt_refresh_history`).

### The Decision

The cost model estimates:

```
estimated_diff_cost = f(delta_size, query_complexity, join_count) + overhead
estimated_full_cost = g(source_table_size, query_complexity)
```

If `estimated_diff_cost < estimated_full_cost × safety_margin`, use DIFFERENTIAL. Otherwise, use FULL.

The `safety_margin` (default: 0.8) biases toward DIFFERENTIAL — it's preferred unless FULL is clearly cheaper. This is because DIFFERENTIAL has lower I/O impact (it doesn't scan the entire source table) and doesn't hold locks as long.

### The Learning Component

The cost model starts with conservative estimates based on table statistics. After each refresh, it records the actual cost (wall-clock time, rows processed, I/O). Over time, the model learns the actual FULL and DIFFERENTIAL costs for each stream table and refines its estimates.

You can see the model's current state:

```sql
SELECT * FROM pgtrickle.explain_refresh_mode('orders_summary');
```

This returns:

```
 stream_table   | current_mode | last_full_ms | last_diff_ms | delta_ratio | threshold | recommendation
----------------+--------------+--------------+--------------+-------------+-----------+---------------
 orders_summary | AUTO(DIFF)   | 450.2        | 3.1          | 0.0001      | 0.35      | DIFFERENTIAL
```

`threshold` is the delta ratio above which the model switches to FULL. In this case, if more than 35% of the source table changes in a single cycle, the model will choose FULL refresh.

---

## When AUTO Switches to FULL

### Bulk loads

```sql
-- A bulk import inserts 500,000 rows into a 1,000,000-row table
COPY orders FROM '/data/import.csv';
```

The change buffer now has 500,000 rows. Delta ratio: 50%. The cost model says: this is more than the threshold (35%). FULL refresh will be faster.

The scheduler runs a FULL refresh for this cycle, then switches back to DIFFERENTIAL for subsequent cycles (which presumably have normal-sized deltas).

### Initial population

When a stream table is first created, the initial population is always a FULL refresh — there's no delta to apply. After the first cycle, AUTO mode begins evaluating.

### Periodic catch-up

If the scheduler falls behind (e.g., the database was under heavy load and skipped several cycles), the accumulated change buffer might exceed the threshold. AUTO mode will catch up with a FULL refresh rather than processing a massive delta incrementally.

---

## Overriding AUTO

You can always override the model's decision with a manual refresh:

```sql
-- Force a FULL refresh regardless of the cost model
SELECT pgtrickle.refresh_stream_table('orders_summary', force_mode => 'FULL');

-- Force a DIFFERENTIAL refresh
SELECT pgtrickle.refresh_stream_table('orders_summary', force_mode => 'DIFFERENTIAL');
```

And you can adjust the threshold:

```sql
-- More aggressive DIFFERENTIAL (switch to FULL only above 50% delta ratio)
SELECT pgtrickle.alter_stream_table('orders_summary', auto_threshold => 0.5);

-- More aggressive FULL (switch to FULL above 10% delta ratio)
SELECT pgtrickle.alter_stream_table('orders_summary', auto_threshold => 0.1);
```

---

## Monitoring AUTO Decisions

The refresh history records which mode was chosen for each cycle:

```sql
SELECT
    refreshed_at,
    refresh_mode,
    duration_ms,
    rows_affected,
    delta_size
FROM pgtrickle.get_refresh_history('orders_summary')
ORDER BY refreshed_at DESC
LIMIT 20;
```

```
      refreshed_at       | refresh_mode | duration_ms | rows_affected | delta_size
-------------------------+--------------+-------------+---------------+-----------
 2026-04-27 10:15:03.412 | DIFFERENTIAL |         3.1 |            12 |         45
 2026-04-27 10:14:58.301 | DIFFERENTIAL |         2.8 |             8 |         30
 2026-04-27 10:14:53.198 | FULL         |       452.1 |        10200  |     505000
 2026-04-27 10:14:48.100 | DIFFERENTIAL |         3.5 |            15 |         52
```

You can see the bulk import at 10:14:53 triggered a FULL refresh (delta_size = 505,000). The subsequent cycles returned to DIFFERENTIAL.

---

## The Recommendation Engine

If you're not sure which mode to use, pg_trickle can recommend:

```sql
SELECT * FROM pgtrickle.recommend_refresh_mode('orders_summary');
```

This analyzes the query structure, current table sizes, and historical refresh patterns to suggest DIFFERENTIAL, FULL, IMMEDIATE, or AUTO.

It also tells you the refresh efficiency — how much work DIFFERENTIAL saves compared to FULL:

```sql
SELECT * FROM pgtrickle.refresh_efficiency('orders_summary');
```

```
 stream_table   | avg_diff_ms | avg_full_ms | efficiency_ratio | recommendation
----------------+-------------+-------------+------------------+---------------
 orders_summary |         3.2 |       450.0 |           140.6x | DIFFERENTIAL
```

An efficiency ratio of 140× means DIFFERENTIAL is 140 times faster than FULL on average. For this stream table, there's no reason to use FULL except during bulk loads — which AUTO handles automatically.

---

## When to Use Each Mode

| Mode | Best for |
|---|---|
| DIFFERENTIAL | Most workloads: continuous changes, small deltas |
| FULL | Bulk loads, complex non-differentiable queries |
| IMMEDIATE | Read-your-writes consistency, low write throughput |
| AUTO | Mixed workloads: normal traffic + periodic bulk imports |

If you're unsure, start with AUTO. It'll do the right thing for most workloads and you can always switch to a fixed mode if you want more predictability.
