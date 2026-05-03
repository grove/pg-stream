[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# Testing Stream Tables: Shadow Mode and Correctness Fuzzing

## How pg_trickle validates that differential refresh matches full refresh

---

Incremental view maintenance has a correctness invariant: the result of applying deltas incrementally must be identical to recomputing the query from scratch.

This sounds obvious, but it's surprisingly easy to violate. Edge cases in JOIN delta rules, NULL handling in aggregates, concurrent modifications during refresh, boundary conditions in GROUP BY with zero-count groups — each of these can produce a result that's silently wrong.

pg_trickle tests this invariant aggressively, with two complementary techniques: shadow mode (production validation) and SQLancer-based fuzzing (pre-release testing).

---

## Shadow Mode

Shadow mode runs DIFFERENTIAL and FULL refresh in parallel on the same stream table and compares the results. If they diverge, it raises an alert.

### Enabling Shadow Mode

```sql
SELECT pgtrickle.alter_stream_table(
    'revenue_by_region',
    shadow_mode => true
);
```

With shadow mode enabled, every refresh cycle:

1. Runs the normal DIFFERENTIAL refresh (applies the delta).
2. Runs a FULL refresh (recomputes from scratch) into a shadow table.
3. Compares the two results row-by-row.
4. If they match: normal operation continues.
5. If they diverge: logs a warning with the differing rows and optionally raises an alert.

The DIFFERENTIAL result is the one that's committed to the stream table — shadow mode doesn't affect the data your application sees. The FULL refresh runs in a separate transaction and is discarded after comparison.

### What Divergence Looks Like

```
WARNING: shadow mode divergence detected for "revenue_by_region"
  Rows only in DIFFERENTIAL result:
    (region='europe', revenue=150200.50, order_count=1203)
  Rows only in FULL result:
    (region='europe', revenue=150200.00, order_count=1203)
  Divergence: 1 row(s), max delta: revenue differs by 0.50
```

This tells you that the differential engine computed a revenue of $150,200.50 for Europe, but the full recomputation says it should be $150,200.00. There's a 50-cent discrepancy — probably a rounding issue in the delta rule for a specific aggregation path.

### When to Use Shadow Mode

- **After deploying a new pg_trickle version.** Run shadow mode for a few hours or days on your most complex stream tables to validate that the new version's delta engine produces correct results.

- **On complex queries.** Multi-table JOINs with nested aggregations and CASE expressions are where delta bugs are most likely to hide. Shadow mode catches them before users do.

- **As a canary.** Enable shadow mode on one representative stream table permanently. If the differential engine ever regresses, you'll know immediately.

### Performance Impact

Shadow mode roughly doubles the refresh cost — you're running both a DIFFERENTIAL and a FULL refresh every cycle. For a stream table that normally refreshes in 10ms, shadow mode takes about 20ms. For production use, pick a few representative tables rather than enabling it on everything.

---

## SQLancer Fuzzing

SQLancer is a database testing tool that generates random SQL queries and checks them for correctness. pg_trickle's test suite uses SQLancer-based fuzzing to find delta engine bugs before they reach production.

### How It Works

The fuzzer:

1. Generates a random schema (tables with various column types).
2. Generates a random query that's valid for IVM (JOINs, GROUP BYs, aggregates, filters).
3. Creates a stream table with DIFFERENTIAL mode.
4. Generates random DML (INSERTs, UPDATEs, DELETEs) against the source tables.
5. Refreshes the stream table.
6. Runs the defining query from scratch (FULL refresh) and compares.
7. Repeats with more random DML.

If the DIFFERENTIAL result ever diverges from the FULL result, the fuzzer reports the schema, query, DML sequence, and the divergent rows. This is a minimal reproduction case that the team can investigate and fix.

### What It's Found

Over the development of pg_trickle, SQLancer fuzzing has found:

- **NULL group handling:** `GROUP BY` on a nullable column where the group key transitions from NULL to non-NULL in an UPDATE. The delta rule was computing the old group's aggregate incorrectly.

- **Empty group cleanup:** When all rows in a GROUP BY group are deleted, the aggregate should be removed from the stream table. The delta engine was leaving zero-count groups in some JOIN configurations.

- **Multi-column update ordering:** When an UPDATE changes both the JOIN key and a aggregated value in the same statement, the delta engine needs to process the key change before the value change. A specific three-table JOIN configuration triggered the wrong ordering.

- **CASE expression with NULL:** `SUM(CASE WHEN x IS NULL THEN 0 ELSE x END)` had a delta rule that didn't handle the transition from NULL to non-NULL correctly.

Each of these bugs was caught by the fuzzer in automated testing, before any release. The fixes are in pg_trickle's test suite as regression tests.

---

## The Multiset Invariant

The correctness invariant that both shadow mode and fuzzing check is the **multiset invariant**:

```
DIFFERENTIAL_RESULT = FULL_RESULT
```

Where both sides are compared as multisets (bags, not sets). Row ordering doesn't matter. But duplicates do — if the differential result has two copies of a row and the full result has one, that's a divergence.

The comparison is done column-by-column with type-aware equality:
- Numeric columns are compared with configurable tolerance (default: exact).
- Timestamps are compared with microsecond precision.
- NULL values follow SQL NULL semantics (NULL = NULL for comparison purposes in this context).
- Array columns are compared as sorted sets.

---

## Running the Fuzz Tests

pg_trickle's fuzz targets are in the `fuzz/` directory:

```bash
# Run the differential correctness fuzzer
cargo +nightly fuzz run fuzz_differential -- -max_total_time=300
```

This runs for 5 minutes, generating random schemas, queries, and DML sequences. Any divergence is reported as a crash with a reproducer in `fuzz/artifacts/`.

The CI pipeline runs fuzzing on a daily schedule. If a new commit introduces a delta engine regression, the next day's fuzz run catches it.

---

## Writing Custom Correctness Tests

If you have a specific query pattern you're concerned about, you can write a targeted correctness test:

```sql
-- Create the stream table
SELECT pgtrickle.create_stream_table(
    'test_target',
    $$SELECT region, SUM(amount) AS total
      FROM orders JOIN customers ON customers.id = orders.customer_id
      GROUP BY region$$,
    schedule => '1s', refresh_mode => 'DIFFERENTIAL'
);

-- Make some changes
INSERT INTO orders (customer_id, amount) VALUES (1, 100);
UPDATE customers SET region = 'asia' WHERE id = 1;
DELETE FROM orders WHERE amount < 10;

-- Wait for refresh
SELECT pg_sleep(2);

-- Compare DIFFERENTIAL result with FULL recomputation
SELECT * FROM test_target
EXCEPT
SELECT region, SUM(amount) AS total
FROM orders JOIN customers ON customers.id = orders.customer_id
GROUP BY region;
-- Should return zero rows
```

If this returns any rows, the differential engine has a bug for your specific query pattern.

---

## The Cost of Correctness

Shadow mode costs CPU (double refresh). Fuzzing costs CI time. Regression tests cost maintenance.

The alternative is deploying a delta engine that might silently produce wrong results. For a system whose entire value proposition is "your data is always correct and up to date," this isn't an option.
