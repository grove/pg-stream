[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# How pg_trickle Handles Diamond Dependencies

## The refresh ordering problem that nobody talks about until it causes double-counting

---

If you've used pg_trickle for more than a few stream tables, you've probably built a DAG. Stream table C depends on A and B. A and B each depend on the same source table. You refresh, and everything works.

But there's a subtle correctness problem hiding in that topology. It's called a diamond dependency, and if your IVM engine doesn't handle it, your aggregates will be wrong.

---

## The Diamond

Here's the shape:

```
                source_table
               /            \
              ▼              ▼
          st_A (agg1)    st_B (agg2)
              \              /
               ▼            ▼
              st_C (combines A and B)
```

`st_A` and `st_B` are both stream tables that depend on `source_table`. `st_C` is a stream table that JOINs or UNIONs the output of `st_A` and `st_B`.

The problem: when `source_table` changes, the delta needs to propagate through both A and B before C can safely refresh. If C refreshes after A but before B, it sees a partially-updated state. Depending on the query, this can cause double-counting, missing rows, or incorrect aggregates.

---

## A Concrete Example

Consider a sales analytics platform:

```sql
-- Source table
CREATE TABLE sales (
    id          bigserial PRIMARY KEY,
    region      text NOT NULL,
    product_id  bigint NOT NULL,
    amount      numeric(12,2) NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- Branch A: revenue by region
SELECT pgtrickle.create_stream_table(
    'revenue_by_region',
    $$SELECT region, SUM(amount) AS revenue, COUNT(*) AS sale_count
      FROM sales GROUP BY region$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);

-- Branch B: revenue by product
SELECT pgtrickle.create_stream_table(
    'revenue_by_product',
    $$SELECT product_id, SUM(amount) AS revenue, COUNT(*) AS sale_count
      FROM sales GROUP BY product_id$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);

-- Diamond tip: executive summary combining both
SELECT pgtrickle.create_stream_table(
    'exec_summary',
    $$SELECT
        'total' AS label,
        (SELECT SUM(revenue) FROM revenue_by_region) AS regional_total,
        (SELECT SUM(revenue) FROM revenue_by_product) AS product_total
    $$,
    schedule => '3s', refresh_mode => 'DIFFERENTIAL'
);
```

In a correct state, `regional_total` and `product_total` should always be equal — they're both `SUM(amount)` over the same source data, just grouped differently.

Without diamond-aware scheduling, here's what can happen:

1. A new sale for $1,000 is inserted into `sales`.
2. The scheduler refreshes `revenue_by_region` — it picks up the new sale.
3. The scheduler refreshes `exec_summary` — it sees the updated `revenue_by_region` but the old `revenue_by_product`.
4. `regional_total = $101,000`, `product_total = $100,000`. They disagree.
5. The scheduler refreshes `revenue_by_product` — now it catches up.
6. Next refresh of `exec_summary` fixes the inconsistency.

For a few seconds, your executive dashboard showed inconsistent numbers. In a financial system, this is a bug. In an audit, this is a finding.

---

## How pg_trickle Solves This

pg_trickle's DAG resolver identifies diamond dependencies at stream table creation time. When you create `exec_summary`, the engine discovers the diamond:

```
sales → revenue_by_region  → exec_summary
sales → revenue_by_product → exec_summary
```

Both paths originate from `sales` and converge at `exec_summary`. pg_trickle records this as a **diamond group**.

You can see it:

```sql
SELECT * FROM pgtrickle.diamond_groups();
```

This returns the set of stream tables that share a common ancestor and converge at a common descendant.

### The Scheduling Rule

The scheduler enforces a simple invariant: **all members of a diamond group must be refreshed to the same frontier before the convergence point is refreshed.**

In practice, this means:
1. `sales` changes.
2. The scheduler refreshes `revenue_by_region` from the change buffer.
3. The scheduler refreshes `revenue_by_product` from the same change buffer epoch.
4. Only after both have been refreshed to the same frontier does `exec_summary` become eligible for refresh.

Steps 2 and 3 can happen in any order, or even in parallel (if you have multiple worker slots). But step 4 is blocked until both are complete.

This is the **frontier tracker** at work. Each stream table has a frontier — a version vector that tracks which changes it has incorporated. The scheduler won't refresh a downstream table unless all its upstream dependencies have frontiers that are at least as advanced as the change epoch being processed.

---

## What This Costs

Diamond-aware scheduling adds a small amount of latency to the convergence point. Instead of refreshing `exec_summary` as soon as any upstream changes, it waits for all upstreams in the diamond group to catch up.

In practice, this wait is bounded by the slowest branch of the diamond — typically a few hundred milliseconds. For the correctness guarantee you get in return, this is a good trade.

If your topology doesn't have diamonds, there's zero overhead. The scheduler only applies the diamond constraint when `diamond_groups()` identifies a non-empty set of convergence points.

---

## Deeper Diamonds

Diamonds can be nested. Consider:

```
source_1 ──→ st_A ──→ st_C ──→ st_E
source_1 ──→ st_B ──→ st_C
source_2 ──→ st_B ──→ st_D ──→ st_E
source_2 ──→ ──────→ st_D
```

There are two diamonds here: one at `st_C` (from `source_1` via A and B) and one at `st_E` (from `source_2` via B/C and D). pg_trickle handles arbitrarily nested diamonds — the frontier tracker works at every level of the DAG.

---

## Detecting Diamonds in Existing Deployments

If you're adding a new stream table to an existing DAG and want to check whether it creates a diamond:

```sql
-- Before creating the new stream table, check the current DAG
SELECT * FROM pgtrickle.dependency_tree('your_new_stream_table');

-- After creating it, check for diamond groups
SELECT * FROM pgtrickle.diamond_groups();
```

pg_trickle also logs a notice when a diamond is detected at creation time:

```
NOTICE: stream table "exec_summary" creates a diamond dependency via
        "sales" → ["revenue_by_region", "revenue_by_product"].
        Refresh scheduling will ensure frontier consistency.
```

---

## Why Other Systems Get This Wrong

Most materialized view systems don't handle diamonds at all, because they don't have a concept of incremental refresh ordering. `REFRESH MATERIALIZED VIEW` is a full recomputation — there's no delta to propagate incorrectly, so there's no diamond problem. (There's also no performance, but that's a different post.)

External IVM systems that do handle incremental deltas often punt on diamonds by documenting it as a known limitation: "avoid creating cyclic or diamond dependency topologies." pg_trickle treats it as a core correctness requirement.
