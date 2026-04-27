[← Back to Blog Index](README.md)

# Window Functions Without the Full Recompute

## How pg_trickle maintains ROW_NUMBER, RANK, LAG, and LEAD incrementally

---

Window functions are the most useful SQL feature that everyone avoids putting in materialized views. The reason is obvious: `ROW_NUMBER()` depends on the ordering of the entire result set. Change one row and every subsequent row number might shift. Full recomputation seems unavoidable.

pg_trickle avoids it anyway, using a technique called **partition-scoped recomputation**. The idea: most window functions include a `PARTITION BY` clause, and a change to one partition doesn't affect other partitions. If you have 10,000 partitions and one row changes, you recompute one partition, not 10,000.

This post explains how it works, what it costs, and when it doesn't help.

---

## The Problem

Consider a sales leaderboard:

```sql
SELECT
  region,
  salesperson,
  total_sales,
  ROW_NUMBER() OVER (PARTITION BY region ORDER BY total_sales DESC) AS rank,
  LAG(total_sales) OVER (PARTITION BY region ORDER BY total_sales DESC) AS prev_sales
FROM sales_summary;
```

Without IVM, you run this query on demand or cache it in a materialized view. Either way, every evaluation scans the entire `sales_summary` table, sorts each partition, and computes row numbers and lag values.

If one salesperson in the "Northeast" region closes a deal, ideally you'd recompute only the Northeast partition. The other 49 regions haven't changed.

That's exactly what pg_trickle does.

---

## Partition-Scoped Recomputation

When pg_trickle detects a window function in a stream table query, it doesn't try to compute a row-level delta for the window output. Window functions don't have the same algebraic delta properties as `SUM` or `COUNT` — there's no closed-form expression for "how does `ROW_NUMBER()` change when row X is inserted?"

Instead, it uses a coarser but still efficient strategy:

1. **Identify affected partitions.** From the change buffer, extract the distinct partition key values that were touched. If a row in the "Northeast" region was inserted, "Northeast" is an affected partition.

2. **Delete old window results for those partitions.** Remove all rows from the stream table where `region = 'Northeast'`.

3. **Recompute the window function for those partitions.** Run the window query against the *current* source data, filtered to only the affected partitions.

4. **Insert the recomputed rows.**

The cost is proportional to the size of the affected partitions, not the size of the entire table.

---

## What This Looks Like in Practice

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'sales_leaderboard',
  query => $$
    SELECT
      region,
      salesperson,
      total_sales,
      ROW_NUMBER() OVER (
        PARTITION BY region ORDER BY total_sales DESC
      ) AS rank,
      LEAD(total_sales) OVER (
        PARTITION BY region ORDER BY total_sales DESC
      ) AS next_below
    FROM sales_summary
  $$,
  schedule => '5s'
);
```

If `sales_summary` is itself a stream table (maintained incrementally from raw `orders`), you get a two-level pipeline: orders → summary (algebraic delta) → leaderboard (partition-scoped recompute). The total latency for one new order to appear in the leaderboard is typically under 100ms.

---

## Supported Window Functions

pg_trickle supports partition-scoped recomputation for all standard window functions:

| Function | Notes |
|----------|-------|
| `ROW_NUMBER()` | Most common. Partition recompute is exact. |
| `RANK()` | Ties handled correctly — all tied rows get the same rank. |
| `DENSE_RANK()` | No gaps in ranking sequence. |
| `LAG(expr, offset)` | Looks at the previous row in the partition. |
| `LEAD(expr, offset)` | Looks at the next row. |
| `FIRST_VALUE(expr)` | First row in the window frame. |
| `LAST_VALUE(expr)` | Last row in the window frame. Requires careful frame specification. |
| `NTH_VALUE(expr, n)` | Nth row in the frame. |
| `NTILE(n)` | Divides partition into n roughly equal groups. |
| `CUME_DIST()` | Cumulative distribution. |
| `PERCENT_RANK()` | Relative rank as a fraction. |

All of these work with `ROWS`, `RANGE`, and `GROUPS` frame specifications.

---

## The No-Partition Case

What if there's no `PARTITION BY`?

```sql
SELECT
  id,
  value,
  ROW_NUMBER() OVER (ORDER BY value DESC) AS global_rank
FROM measurements;
```

Without a partition clause, the entire result set is one partition. A change to any row triggers a full recomputation of the window function. In this case, DIFFERENTIAL mode degrades to something close to FULL refresh for the window step.

pg_trickle still applies differential maintenance to the steps *before* the window function (filters, joins, aggregates). Only the window recomputation itself is full. If the query is `SELECT ... FROM big_table WHERE active = true` and 1% of rows are active, the window recompute runs over 1% of the data, not 100%.

**Recommendation:** If you need global ranking over a large table, add an artificial partition key or accept that the window step will be a full recompute. For tables under ~100K rows, the full recompute is fast enough that it doesn't matter.

---

## Multiple Window Clauses

Queries can have multiple `OVER` clauses with different partitioning:

```sql
SELECT
  department,
  team,
  employee,
  salary,
  RANK() OVER (PARTITION BY department ORDER BY salary DESC) AS dept_rank,
  RANK() OVER (PARTITION BY team ORDER BY salary DESC) AS team_rank
FROM employees;
```

pg_trickle handles this through its automatic rewrite pipeline. The query is decomposed into two window passes:

1. First pass: compute `dept_rank` partitioned by `department`.
2. Second pass: compute `team_rank` partitioned by `team`.

Each pass uses its own partition key for scoped recomputation. If an employee in "Engineering/Backend" gets a raise, pg_trickle recomputes:
- The "Engineering" partition for `dept_rank`
- The "Backend" partition for `team_rank`

Other departments and teams are untouched.

---

## Window Functions After GROUP BY

A common pattern: aggregate first, then rank.

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'top_customers_by_region',
  query => $$
    SELECT
      region,
      customer_id,
      total_revenue,
      RANK() OVER (PARTITION BY region ORDER BY total_revenue DESC) AS rank
    FROM (
      SELECT
        c.region,
        o.customer_id,
        SUM(o.total) AS total_revenue
      FROM orders o
      JOIN customers c ON c.id = o.customer_id
      GROUP BY c.region, o.customer_id
    ) sub
  $$,
  schedule => '10s'
);
```

pg_trickle processes this as a two-stage pipeline:

1. **Inner query** (join + GROUP BY): maintained incrementally using algebraic delta rules. Only the groups affected by changed orders are updated.
2. **Outer query** (window function): partition-scoped recompute on the groups that changed.

The amplification factor is low. If 5 orders come in across 3 customers in 2 regions, the inner stage updates 3 groups. The outer stage recomputes 2 partitions.

---

## Frame Specifications

Window frames control which rows the function can see:

```sql
-- Running total (default frame)
SUM(amount) OVER (PARTITION BY account ORDER BY date
                  ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)

-- 7-row moving average
AVG(value) OVER (PARTITION BY sensor ORDER BY ts
                 ROWS BETWEEN 3 PRECEDING AND 3 FOLLOWING)

-- Range-based: all rows with same date
COUNT(*) OVER (PARTITION BY category ORDER BY date
               RANGE BETWEEN '0 days' PRECEDING AND '0 days' FOLLOWING)
```

All frame types work with partition-scoped recomputation. The frame specification doesn't change the partitioning strategy — it only affects what the window function computes *within* the partition.

---

## Performance: When It Helps, When It Doesn't

The speedup from partition-scoped recomputation depends on two factors:

1. **Number of partitions.** More partitions = smaller per-partition recompute = bigger speedup.
2. **Number of affected partitions per refresh.** If every refresh touches every partition, you're doing a full recompute with extra overhead.

| Scenario | Partitions | Affected/cycle | Speedup vs. FULL |
|----------|-----------|----------------|-------------------|
| Regional leaderboard | 50 | 2–3 | ~20× |
| Per-customer ranking | 10,000 | 10–50 | ~200× |
| Per-sensor percentile | 1,000 | 5–20 | ~50× |
| Global ranking (no PARTITION BY) | 1 | 1 | ~1× (no benefit) |
| High-churn table (all partitions touched) | 100 | 100 | ~1× (no benefit) |

The break-even point: if more than ~30% of partitions are affected in a single refresh cycle, pg_trickle's AUTO mode will likely choose FULL refresh instead. The partition-scoped approach has overhead (identifying affected partitions, deleting and re-inserting) that isn't worth it when most partitions are changing anyway.

---

## Summary

Window functions in stream tables work via partition-scoped recomputation: identify which partitions were affected by source changes, recompute only those partitions, leave the rest untouched.

It's not a row-level delta — it's a partition-level delta. But for the common case of many partitions with localized changes, the performance difference is dramatic. A leaderboard over 50 regions with 100,000 salespeople refreshes the same as a leaderboard over one region with 2,000.

The rule of thumb: if your window function has `PARTITION BY` and changes are spread across a small fraction of partitions, use DIFFERENTIAL mode. If changes hit most partitions or there's no `PARTITION BY`, AUTO mode will choose FULL refresh, which is the right call.
