[← Back to Blog Index](README.md)

# GROUPING SETS, ROLLUP, and CUBE — Incrementally

## Multi-dimensional aggregation maintained by delta, not by full scan

---

`GROUPING SETS`, `ROLLUP`, and `CUBE` are PostgreSQL's multi-dimensional aggregation features. They let you compute subtotals, grand totals, and cross-tabulations in a single query. They're also the features most likely to make your DBA wince when you put them in a materialized view, because they multiply the work of an already-expensive `GROUP BY`.

pg_trickle maintains them incrementally. The trick is automatic decomposition: a single `CUBE` query is rewritten into multiple `UNION ALL` branches, one per grouping level. Each branch is maintained as an independent delta.

---

## Quick Refresher

If you're familiar with grouping sets, skip to the next section. If not, here's the 30-second version:

```sql
-- ROLLUP: subtotals for each prefix of the grouping columns
SELECT region, product, SUM(revenue)
FROM sales
GROUP BY ROLLUP(region, product);
```

This produces:
- Per-region, per-product totals
- Per-region subtotals (product = NULL)
- Grand total (region = NULL, product = NULL)

`CUBE` is the power set — every combination of grouping columns:

```sql
SELECT region, product, SUM(revenue)
FROM sales
GROUP BY CUBE(region, product);
```

This adds:
- Per-product subtotals (region = NULL)

`GROUPING SETS` lets you pick exactly which combinations:

```sql
SELECT region, product, channel, SUM(revenue)
FROM sales
GROUP BY GROUPING SETS (
  (region, product),
  (region, channel),
  (region),
  ()
);
```

---

## The Problem With Full Refresh

A `GROUP BY CUBE(a, b, c)` over three columns produces $2^3 = 8$ grouping levels. Over four columns: 16. Over five: 32. Each level is a separate aggregation pass.

For a table with 10 million rows, `CUBE(a, b, c)` effectively runs 8 separate `GROUP BY` queries, each scanning 10 million rows. If you're refreshing this as a materialized view every 5 seconds, you're scanning 80 million rows every 5 seconds.

With IVM, 10 new rows inserted means updating at most 8 groups per grouping level — roughly 64 group updates instead of 80 million row scans.

---

## The Rewrite

pg_trickle's query parser detects `ROLLUP`, `CUBE`, and `GROUPING SETS` and rewrites them into a `UNION ALL` of standard `GROUP BY` queries. This happens at stream table creation time — the defining query is normalized before the differential engine sees it.

For example:

```sql
-- What you write
SELECT region, product, SUM(revenue)
FROM sales
GROUP BY ROLLUP(region, product);

-- What pg_trickle sees internally
SELECT region, product, SUM(revenue)
FROM sales
GROUP BY region, product

UNION ALL

SELECT region, NULL::text AS product, SUM(revenue)
FROM sales
GROUP BY region

UNION ALL

SELECT NULL::text AS region, NULL::text AS product, SUM(revenue)
FROM sales;
```

Each branch of the `UNION ALL` is a standard `GROUP BY` that pg_trickle knows how to maintain incrementally. The delta rules for `SUM`, `COUNT`, and other algebraic aggregates apply directly.

---

## Creating a Stream Table

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'sales_cube',
  query => $$
    SELECT
      region,
      product_category,
      date_trunc('month', sale_date) AS month,
      SUM(revenue) AS total_revenue,
      COUNT(*) AS num_sales,
      AVG(revenue) AS avg_revenue
    FROM sales
    GROUP BY CUBE(region, product_category, date_trunc('month', sale_date))
  $$,
  schedule => '10s'
);
```

Internally, this is decomposed into 8 UNION ALL branches (one for each subset of {region, product_category, month}). Each branch is maintained independently.

When a new sale is recorded, pg_trickle:
1. Identifies the affected groups in each branch (e.g., region="Northeast", product="Electronics", month="2026-04").
2. Applies the algebraic delta: `new_sum = old_sum + revenue`, `new_count = old_count + 1`.
3. Updates only those groups, in all 8 branches.

---

## The GROUPING() Function

PostgreSQL's `GROUPING()` function distinguishes actual NULL values from NULL used as a "grand total" marker:

```sql
SELECT
  region,
  product,
  GROUPING(region) AS is_region_total,
  GROUPING(product) AS is_product_total,
  SUM(revenue)
FROM sales
GROUP BY CUBE(region, product);
```

pg_trickle preserves this in the rewrite. Each UNION ALL branch sets the appropriate `GROUPING()` bits as constants:

```sql
-- Branch for per-region subtotals
SELECT region, NULL::text AS product,
       0 AS is_region_total,   -- region is a real group
       1 AS is_product_total,  -- product is rolled up
       SUM(revenue)
FROM sales
GROUP BY region;
```

The `GROUPING()` values are deterministic per branch, so they don't need delta computation — they're constant columns.

---

## Drill-Down Dashboards

The classic use case for CUBE/ROLLUP is drill-down analytics. A dashboard shows:

1. Grand total (all regions, all products, all months)
2. User clicks a region → subtotals by product and month for that region
3. User clicks a product → per-month detail for that region+product

With a traditional materialized view, each drill level requires a query against the base table or a separate materialized view per level.

With a CUBE stream table, all levels are precomputed and maintained incrementally in a single table:

```sql
-- Grand total
SELECT total_revenue FROM sales_cube
WHERE region IS NULL AND product_category IS NULL AND month IS NULL;

-- Region subtotals
SELECT product_category, month, total_revenue FROM sales_cube
WHERE region = 'Northeast'
  AND product_category IS NOT NULL
  AND month IS NOT NULL;
```

The query hits a small, precomputed table instead of scanning millions of rows. And it's always fresh — within `schedule` seconds of reality.

---

## ROLLUP for Hierarchical Totals

ROLLUP is specifically designed for hierarchical aggregation. If your grouping columns have a natural hierarchy (year → quarter → month → day), ROLLUP produces exactly the subtotals you need:

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'revenue_hierarchy',
  query => $$
    SELECT
      date_trunc('year', sale_date)    AS year,
      date_trunc('quarter', sale_date) AS quarter,
      date_trunc('month', sale_date)   AS month,
      SUM(revenue) AS total,
      COUNT(*) AS num_sales
    FROM sales
    GROUP BY ROLLUP(
      date_trunc('year', sale_date),
      date_trunc('quarter', sale_date),
      date_trunc('month', sale_date)
    )
  $$,
  schedule => '5s'
);
```

This produces 4 levels:
- Per year + quarter + month
- Per year + quarter
- Per year
- Grand total

For a new sale in April 2026, pg_trickle updates 4 groups: (2026, Q2, April), (2026, Q2), (2026), and the grand total. Four group updates regardless of table size.

---

## Custom GROUPING SETS

When CUBE or ROLLUP generates too many combinations, use explicit GROUPING SETS:

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'targeted_summary',
  query => $$
    SELECT
      region,
      channel,
      product_category,
      SUM(revenue) AS total,
      COUNT(*) AS cnt
    FROM sales
    GROUP BY GROUPING SETS (
      (region, product_category),
      (channel, product_category),
      (region),
      ()
    )
  $$,
  schedule => '10s'
);
```

This produces exactly 4 grouping levels — not the 8 that CUBE would produce. Each is maintained as a separate UNION ALL branch with independent delta rules.

---

## Performance Characteristics

The decomposition into UNION ALL branches means the number of "virtual queries" grows with the number of grouping sets. For CUBE over N columns, that's $2^N$ branches.

| Columns | CUBE branches | ROLLUP branches |
|---------|--------------|-----------------|
| 2 | 4 | 3 |
| 3 | 8 | 4 |
| 4 | 16 | 5 |
| 5 | 32 | 6 |

Each branch has its own delta computation. The per-branch cost is small (proportional to the number of changed groups), but the constant factor matters when you have 32 branches.

**Practical limit:** CUBE over 5+ columns works but produces a lot of output rows. If you don't need all $2^N$ combinations, use explicit GROUPING SETS to include only the levels you actually query.

---

## Summary

`GROUPING SETS`, `ROLLUP`, and `CUBE` are automatically decomposed into UNION ALL branches, each maintained incrementally using standard algebraic delta rules.

The result: drill-down dashboards, hierarchical totals, and cross-tabulations that update in milliseconds instead of seconds. The precomputed table contains every grouping level you need, always fresh, always queryable.

Use ROLLUP for hierarchical data. Use CUBE when you need every combination. Use GROUPING SETS when you need exactly the levels you query. And let pg_trickle handle the math.
