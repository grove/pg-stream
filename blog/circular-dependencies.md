[← Back to Blog Index](README.md)

# Cycles in Your Dependency Graph? That's Fine.

## Fixed-point iteration for monotone queries in pg_trickle

---

Dependency cycles are normally a fatal error in data pipelines. Airflow rejects them. dbt rejects them. Most IVM systems reject them. The logic is sound: if A depends on B and B depends on A, which one do you refresh first?

pg_trickle takes a different position. Some cycles are legitimate. A graph-reachability table that feeds a scoring table that feeds a threshold filter that feeds back into the reachability table — that's a cycle, but it has a well-defined fixed point. The system can iterate until nothing changes.

The key constraint: the queries in the cycle must be **monotone**. pg_trickle detects this at creation time and rejects non-monotone cycles. Monotone cycles converge. Non-monotone cycles can oscillate forever.

---

## What Monotone Means

A query is monotone if adding rows to its input can only add rows to its output — never remove them. In practical terms:

**Monotone operations:**
- `SELECT ... WHERE ...` (filter) — adding a row that passes the filter adds it to the output
- `JOIN` (inner, left, right, full) — adding a row that matches adds join results
- `UNION ALL` — adding to either side adds to the output
- `GROUP BY ... HAVING ...` with monotone aggregates (COUNT, SUM of non-negatives)

**Non-monotone operations:**
- `EXCEPT` — adding to the right side *removes* from the output
- `NOT EXISTS` — adding a matching row removes the anti-joined row
- Aggregates that can decrease (`MIN`, `MAX` with deletions)
- `DISTINCT` (adding a duplicate doesn't add to the output)

pg_trickle checks monotonicity when you create a stream table that would form a cycle. If any table in the cycle uses a non-monotone operator, the creation fails:

```
ERROR: stream table 'filtered_scores' would create a non-monotone cycle
DETAIL: NOT EXISTS in query is not monotone
HINT: Remove the circular dependency or use a non-cyclic design
```

---

## Enabling Circular Dependencies

Cycles are disabled by default:

```sql
-- This fails with cycles disabled
SHOW pg_trickle.allow_circular;
-- off

SELECT pgtrickle.create_stream_table(
  name  => 'scores',
  query => $$ SELECT ... FROM reachable $$,
  schedule => '5s'
);
-- ERROR: would create circular dependency (reachable → scores → reachable)
```

To allow monotone cycles:

```sql
SET pg_trickle.allow_circular = on;
```

Or in `postgresql.conf`:

```
pg_trickle.allow_circular = on
```

---

## How Fixed-Point Iteration Works

When the scheduler encounters a cycle (a strongly connected component, or SCC, in the dependency graph), it doesn't try to topologically sort it — that's impossible for a cycle. Instead, it iterates:

1. **Refresh all tables in the SCC once**, in an arbitrary order.
2. **Check for net changes.** If any table in the SCC produced new rows, go to step 1.
3. **If no table produced new rows**, the SCC has converged. Move on.

This is fixed-point iteration. The monotonicity constraint guarantees it terminates: each iteration can only add rows (never remove them), and the total result is bounded (finite source tables, finite joins, finite query results). Eventually, no new rows are produced.

---

## A Concrete Example

Consider a fraud detection pipeline where:

1. `suspicious_accounts` flags accounts based on transaction patterns.
2. `risky_transactions` flags transactions involving suspicious accounts.
3. `suspicious_accounts` also considers accounts that have many risky transactions.

This is circular: suspicious accounts → risky transactions → suspicious accounts.

```sql
SET pg_trickle.allow_circular = on;

-- Table 1: Accounts flagged by direct pattern matching
SELECT pgtrickle.create_stream_table(
  name  => 'suspicious_accounts',
  query => $$
    SELECT account_id, 'pattern' AS reason
    FROM transactions
    GROUP BY account_id
    HAVING COUNT(*) FILTER (WHERE amount > 10000) > 5

    UNION ALL

    -- Also flag accounts with many risky transactions
    SELECT DISTINCT account_id, 'association' AS reason
    FROM risky_transactions
    GROUP BY account_id
    HAVING COUNT(*) > 10
  $$,
  schedule => '10s'
);

-- Table 2: Transactions involving suspicious accounts
SELECT pgtrickle.create_stream_table(
  name  => 'risky_transactions',
  query => $$
    SELECT t.*
    FROM transactions t
    JOIN suspicious_accounts sa ON t.account_id = sa.account_id
    WHERE t.amount > 1000
  $$,
  schedule => '10s'
);
```

On the first iteration:
- `suspicious_accounts` finds accounts with >5 high-value transactions (pattern match).
- `risky_transactions` flags transactions by those accounts.

On the second iteration:
- `suspicious_accounts` now also sees the risky-transaction counts. Some new accounts cross the >10 threshold.
- `risky_transactions` picks up transactions from the newly flagged accounts.

On the third iteration (typically):
- No new accounts are flagged. No new transactions are flagged. Convergence.

---

## The Iteration Limit

To prevent runaway iteration (in case of a bug or a degenerate data pattern), pg_trickle enforces a limit:

```sql
SHOW pg_trickle.max_fixpoint_iterations;
-- 10
```

If a cycle doesn't converge within 10 iterations, pg_trickle stops, logs a warning, and marks the SCC as "not converged." The tables are still queryable — they just contain the result after 10 iterations, which may not be the complete fixed point.

```
WARNING: SCC {suspicious_accounts, risky_transactions} did not converge
         after 10 iterations (12 new rows in last iteration)
```

You can increase the limit if your domain requires deeper iteration:

```sql
SET pg_trickle.max_fixpoint_iterations = 50;
```

---

## Monitoring SCCs

pg_trickle exposes SCC status through the monitoring API:

```sql
SELECT * FROM pgtrickle.scc_status();
```

```
 scc_id | tables                                     | last_iterations | converged | last_refresh
--------+--------------------------------------------+-----------------+-----------+-------------------
 1      | {suspicious_accounts,risky_transactions}    | 3               | t         | 2026-04-27 10:15:03
```

Key fields:
- `last_iterations`: How many rounds it took to converge.
- `converged`: Whether the last refresh reached a fixed point.
- `last_refresh`: When the SCC was last fully resolved.

If `converged` is consistently `false`, your cycle may not be monotone in practice (even if the query structure passes the static check). Check whether data patterns cause oscillation.

---

## Why Most Systems Reject Cycles

The standard argument against cycles is that they're a design smell — if your data model has circular dependencies, you should restructure it. That's usually right.

But some domains genuinely have circular relationships:

- **Graph algorithms:** Reachability, PageRank, label propagation — all defined as fixed points.
- **Constraint propagation:** Scheduling constraints that reference each other.
- **Multi-phase classification:** Where the output of one classifier feeds into another, and vice versa.
- **Supply chain:** Demand forecasts that depend on inventory, which depends on demand forecasts.

For these cases, "restructure your schema" is unhelpful. The circularity is in the domain, not in a modeling mistake.

---

## Cycles and IMMEDIATE Mode

IMMEDIATE mode (synchronous in-transaction refresh) and cycles don't mix. If A triggers a refresh of B, which triggers a refresh of A, you get an infinite loop inside a single transaction.

pg_trickle rejects this at creation time:

```
ERROR: IMMEDIATE mode is not allowed for stream tables in a cycle
HINT: Use DIFFERENTIAL or AUTO mode with a schedule
```

Cyclic stream tables must use scheduled (asynchronous) refresh. The fixed-point iteration runs in the background scheduler, not inside a user transaction.

---

## Performance Considerations

Each iteration of a cycle is a full refresh cycle for all tables in the SCC. The total cost is:

```
cost = iterations × Σ(cost per table in SCC)
```

For most practical cycles, convergence happens in 2–4 iterations. But if your cycle has 10 tables that each take 200ms to refresh, one convergence pass takes 2–8 seconds.

**Optimization tips:**
- Keep cycles small. Factor non-cyclic portions of the query out of the SCC.
- Use DIFFERENTIAL mode. Each iteration after the first typically processes only the new rows from the previous iteration.
- Set a tight `max_fixpoint_iterations` to fail fast if convergence is slow.
- Monitor `scc_status()` to track iteration counts. If they're consistently high, the cycle may need restructuring.

---

## Summary

Circular dependencies aren't always a bug. pg_trickle allows them for monotone queries — queries where adding input rows can only add output rows. The system iterates until convergence (no new rows) or until the iteration limit.

Enable with `pg_trickle.allow_circular = on`. Monitor with `scc_status()`. Keep cycles small and monotone. And if convergence takes too many iterations, that's a signal to reconsider the design — not a reason to ban cycles entirely.
