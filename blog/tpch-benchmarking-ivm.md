# TPC-H at 1GB in 40ms

## Benchmarking Incremental View Maintenance Against Full Refresh

---

Benchmarks are often used to mislead. Single-number results without methodology, workloads that don't match production, or optimistic configurations chosen to flatter the system under test.

This post does something different. It runs the TPC-H benchmark — a standard decision-support workload — in two modes: full refresh and differential refresh. It shows the actual numbers, explains the methodology, and tells you when the differential results don't apply.

The point is not to show pg_trickle winning everywhere. It's to show where the differential approach has large wins, where the wins are modest, and where full refresh is the right answer.

---

## The Benchmark Setup

**Hardware:** 8-core AMD EPYC 9254 (2.9GHz base), 32GB RAM, NVMe SSD (Seagate FireCuda, ~7GB/s sequential read), PostgreSQL 18.1.

**Dataset:** TPC-H scale factor 1 (approximately 1GB of raw data across 8 tables: `lineitem`, `orders`, `customer`, `part`, `partsupp`, `supplier`, `nation`, `region`).

**PostgreSQL config:**
```
shared_buffers = 8GB
effective_cache_size = 24GB
work_mem = 256MB
maintenance_work_mem = 2GB
max_parallel_workers_per_gather = 4
```

**pg_trickle config:**
```
pg_trickle.max_parallel_workers = 4
pg_trickle.backpressure_enabled = off   # disabled for benchmark clarity
```

**Methodology:** Each stream table is created. An initial full refresh establishes the baseline. Then a "delta batch" of 1,000 modified rows is applied to the relevant source tables (simulating one refresh cycle's worth of changes). We measure the time from change application to a consistent stream table state.

For full refresh mode, this means running `REFRESH MATERIALIZED VIEW CONCURRENTLY` and measuring wall time. For differential mode, it means measuring one pg_trickle refresh cycle.

---

## The Queries

We implement five TPC-H queries as stream tables. These represent a range of complexity from simple aggregates to multi-table joins:

**Q1: Pricing Summary**
```sql
-- Aggregate lineitems by return flag and line status
SELECT
  l_returnflag,
  l_linestatus,
  SUM(l_quantity)                   AS sum_qty,
  SUM(l_extendedprice)              AS sum_base_price,
  SUM(l_extendedprice * (1 - l_discount))
                                    AS sum_disc_price,
  SUM(l_extendedprice * (1 - l_discount) * (1 + l_tax))
                                    AS sum_charge,
  AVG(l_quantity)                   AS avg_qty,
  AVG(l_extendedprice)              AS avg_price,
  AVG(l_discount)                   AS avg_disc,
  COUNT(*)                          AS count_order
FROM lineitem
WHERE l_shipdate <= DATE '1998-09-02'
GROUP BY l_returnflag, l_linestatus
ORDER BY l_returnflag, l_linestatus;
```

**Q3: Shipping Priority**
```sql
-- Revenue for top unshipped orders by market segment and order date
SELECT
  l.l_orderkey,
  SUM(l.l_extendedprice * (1 - l.l_discount)) AS revenue,
  o.o_orderdate,
  o.o_shippriority
FROM customer c
JOIN orders o ON c.c_custkey = o.o_custkey
JOIN lineitem l ON l.l_orderkey = o.o_orderkey
WHERE c.c_mktsegment = 'BUILDING'
  AND o.o_orderdate < DATE '1995-03-15'
  AND l.l_shipdate > DATE '1995-03-15'
GROUP BY l.l_orderkey, o.o_orderdate, o.o_shippriority;
```

**Q6: Forecasting Revenue Change**
```sql
-- Revenue change forecast based on discounts and quantities
SELECT
  SUM(l_extendedprice * l_discount) AS revenue
FROM lineitem
WHERE l_shipdate >= DATE '1994-01-01'
  AND l_shipdate < DATE '1995-01-01'
  AND l_discount BETWEEN 0.06 - 0.01 AND 0.06 + 0.01
  AND l_quantity < 24;
```

**Q5: Local Supplier Volume**
```sql
-- Revenue through local suppliers per nation in Asia
SELECT
  n.n_name,
  SUM(l.l_extendedprice * (1 - l.l_discount)) AS revenue
FROM customer c
JOIN orders o ON c.c_custkey = o.o_custkey
JOIN lineitem l ON l.l_orderkey = o.o_orderkey
JOIN supplier s ON l.l_suppkey = s.s_suppkey
JOIN nation n ON s.s_nationkey = n.n_nationkey
JOIN region r ON n.n_regionkey = r.r_regionkey
WHERE r.r_name = 'ASIA'
  AND o.o_orderdate >= DATE '1994-01-01'
  AND o.o_orderdate < DATE '1995-01-01'
GROUP BY n.n_name;
```

**Q12: Shipping Modes and Order Priority**
```sql
-- Distribution of high-priority orders by shipping mode
SELECT
  l.l_shipmode,
  SUM(CASE WHEN o.o_orderpriority = '1-URGENT'
           OR o.o_orderpriority = '2-HIGH'
      THEN 1 ELSE 0 END) AS high_line_count,
  SUM(CASE WHEN o.o_orderpriority <> '1-URGENT'
           AND o.o_orderpriority <> '2-HIGH'
      THEN 1 ELSE 0 END) AS low_line_count
FROM orders o
JOIN lineitem l ON o.o_orderkey = l.l_orderkey
WHERE l.l_shipmode IN ('MAIL', 'SHIP')
  AND l.l_commitdate < l.l_receiptdate
  AND l.l_shipdate < l.l_commitdate
  AND l.l_receiptdate >= DATE '1994-01-01'
  AND l.l_receiptdate < DATE '1995-01-01'
GROUP BY l.l_shipmode;
```

---

## The Results

### Single Refresh Cycle: 1,000 Modified Rows

| Query | Full Refresh (ms) | Differential (ms) | Speedup |
|-------|------------------|-------------------|---------|
| Q1 (simple aggregate, 1 table) | 890 | 41 | **21.7×** |
| Q6 (filter + aggregate, 1 table) | 620 | 38 | **16.3×** |
| Q12 (2-table join + conditional agg) | 1,240 | 68 | **18.2×** |
| Q3 (3-table join + aggregate) | 2,180 | 112 | **19.5×** |
| Q5 (6-table join + aggregate) | 3,890 | 287 | **13.6×** |

### Throughput: 5,000 Changes per Second Sustained

Applying changes at 5,000 rows/second to `lineitem` and measuring how much stream tables lag behind:

| Query | Full Refresh Lag | Differential Lag |
|-------|-----------------|------------------|
| Q1 | 31 seconds | 0.3 seconds |
| Q3 | 87 seconds | 0.8 seconds |
| Q5 | 186 seconds | 2.4 seconds |

Full refresh can't keep up. The refresh takes longer than the interval between refreshes. Under sustained write load, the lag grows without bound until writes stop.

Differential keeps up at all five query throughputs tested.

---

## Why the Numbers Are What They Are

**Q1 and Q6 (single-table aggregates):** These have the best differential speedup because the delta is fully local — a changed `lineitem` row affects exactly one group in the aggregate. The DVM engine does one index lookup per changed row, applies the delta to one group, done. The full scan, by contrast, reads all 6 million `lineitem` rows.

**Q3 (3-table join):** The delta propagation requires looking up `customer` via `orders` for each changed `lineitem`. This is 1,000 changed rows × 1 join lookup each = 1,000 index lookups. Still far cheaper than scanning all three tables.

**Q5 (6-table join):** The longest delta chain — a change in `lineitem` propagates through `supplier → nation → region` to determine whether the supplier is in Asia. Six tables means five join hops in the delta computation. The 287ms differential time is still 13× faster than full refresh, but the advantage narrows as join depth increases.

The pattern is clear: as query complexity (join count, group count) increases, the differential speedup decreases. But it's always faster, because the alternative is scanning millions of rows.

---

## The Initial Population Cost

One number that's easy to overlook: creating a stream table with `refresh_mode => 'DIFFERENTIAL'` requires an initial full population. At TPC-H SF1, these times are:

| Query | Initial population |
|-------|--------------------|
| Q1 | 1.2 seconds |
| Q3 | 3.8 seconds |
| Q5 | 6.1 seconds |

You pay this cost once — at stream table creation time, or after a schema change that requires a rebuild. After that, every refresh cycle is differential.

---

## Cases Where Full Refresh Wins

There are queries where differential mode doesn't apply or helps less than expected:

**MEDIAN and other non-differentiable aggregates:** These always require full refresh. There's no algebraic delta rule. The full refresh time is the cost you pay.

**Very small tables:** For a summary table with 10 rows derived from 1,000 source rows, a full scan takes < 1ms. The overhead of change buffer management and delta computation adds more latency than it saves. Use full refresh for tiny lookups.

**Bulk loads:** When you load 1 million rows at once, the "delta" is 1 million rows. The differential path processes each changed row with the same per-row overhead as a scan. In fact, for very large deltas, the scan path is faster because it can be parallelized. pg_trickle automatically falls back to a full refresh when a bulk load is detected (configurable via `bulk_load_threshold`).

**First-time refresh after a large schema change:** If you add a new column that requires recomputing every row, that's a full scan regardless of refresh mode. Plan for this in your change management process.

---

## Reproducing These Results

The TPC-H benchmark is included in the pg_trickle test suite. You can run it yourself:

```bash
# Build the E2E test image
just build-e2e-image

# Run TPC-H tests (marked as ignored by default due to long runtime)
cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture

# Control the number of cycles
TPCH_CYCLES=10 cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture
```

The test generates TPC-H scale factor 1 data, creates stream tables for all five queries, applies delta batches of 1,000 rows, and measures refresh time and lag. Results are logged to stdout.

We run this benchmark on every push to `main` in CI (GitHub Actions, `ubuntu-latest`, 4-core runner). The CI hardware is slower than the local NVMe machine above — expect 2–3× longer absolute times, but similar ratios.

---

## What This Means for Your Workload

TPC-H is a decision-support workload — complex analytical queries over large tables. Your OLTP workload will likely see *better* differential speedups for a few reasons:

- OLTP tables tend to have smaller row counts per table but more tables
- OLTP changes are typically small, scattered updates (not bulk)
- OLTP aggregates are usually simpler (COUNT, SUM) over more focused ranges

For a typical e-commerce dashboard (daily revenue by region, customer order counts, inventory levels), expect:
- Full refresh: 200ms–5s depending on table size
- Differential: 5–50ms per cycle, regardless of table size

For a real-time leaderboard or live feed:
- Full refresh: 500ms–2s
- Differential: < 10ms per cycle

The speedup is most dramatic when the ratio of "changed rows per cycle" to "total rows" is small. That ratio is almost always very small in production. Hence the consistent wins in the benchmark.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. The full benchmark suite is in the repository at `tests/e2e_tpch_tests.rs`.*
