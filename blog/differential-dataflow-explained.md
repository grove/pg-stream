# Differential Dataflow for the Rest of Us

## The mathematics behind incremental view maintenance, explained without a PhD

---

pg_trickle maintains query results incrementally. When one row changes, it updates the result without recomputing everything. That sounds simple, but it requires some careful mathematics to get right.

This post explains how that mathematics works — in plain language, without assuming a background in database theory or systems research. The goal is to build enough intuition that you can reason about when incremental maintenance is possible, when it isn't, and why.

If you just want to use pg_trickle, you don't need this. If you want to understand why it works, read on.

---

## The Problem With "Just Recompute"

Start with a concrete example. You're tracking e-commerce orders and maintaining a `revenue_by_region` table:

```sql
-- The query
SELECT region, SUM(total) AS revenue
FROM orders
JOIN customers ON customers.id = orders.customer_id
GROUP BY region;
```

This query scans every row in `orders`, joins with `customers`, and computes sums per group. If `orders` has 100 million rows, this query reads 100 million rows every time you run it. If you run it every 5 seconds to stay fresh, you're reading 100 million rows every 5 seconds. At any realistic row size, that's gigabytes per second of I/O.

The observation that makes incremental maintenance possible is: **most of the time, very little changes.**

If 10 new orders come in over 5 seconds, only those 10 orders affect the result. The other 99,999,990 orders haven't changed. Recomputing the full aggregate is wasteful by a factor of 10 million.

Incremental view maintenance answers the question: given that `orders` changed by some set of rows, what's the corresponding change to `revenue_by_region`?

---

## Collections and Multisets

Before getting to the math, a quick terminology note.

In differential dataflow, data is represented as *multisets* — collections where each element has a *weight*. A weight of `+1` means the element is present. A weight of `-1` means it was removed. A weight of `+2` means it appears twice.

For a SQL table with distinct rows, every present row has weight `+1`.

When you insert a row, you add it to the multiset with weight `+1`.
When you delete a row, you add the same row with weight `-1`. (The net effect: the row is no longer present.)
When you update a row, you add the old value with weight `-1` and the new value with weight `+1`. (The net effect: the old value is removed, the new value is added.)

This weight-based representation turns updates into insertions and deletions. It simplifies the mathematics considerably — you only need rules for how insertions and deletions propagate.

---

## The Δ Notation

In differential dataflow, we use Δ (delta) to mean "the change to."

If `T` is a table (multiset), then `ΔT` is the set of rows added and removed from `T` since some reference point. Each row in `ΔT` has a weight: `+1` for insertions, `-1` for deletions.

For a query `Q(T)` that produces result `R`, we want to compute `ΔR` (the change to the result) from `ΔT` (the change to the input), *without* touching the unchanged rows in `T`.

The question is: for each type of query operation, what's the delta rule?

---

## Delta Rules for SQL Operations

### Filter (WHERE)

```sql
-- Query
SELECT * FROM orders WHERE total > 100;

-- Delta rule
Δresult = { row ∈ ΔT | row.total > 100 }
```

If a row is inserted and it satisfies the filter, it appears in the result. If it doesn't, it's ignored. This is the simplest rule — delta of a filter is just filtering the delta.

### Projection (SELECT columns)

```sql
-- Query
SELECT customer_id, total FROM orders;

-- Delta rule
Δresult = { (row.customer_id, row.total) : row ∈ ΔT }
```

Project each delta row onto the selected columns. Also simple.

### Join

Joins are where it gets interesting.

```sql
-- Query
SELECT o.*, c.region
FROM orders o
JOIN customers c ON c.id = o.customer_id;
```

If we change `orders` (Δorders) or `customers` (Δcustomers), how does the result change?

The delta rule for a join has two parts:

**Part 1 (orders change):**
```
Δresult += Δorders ⋈ customers
```
For each changed order, join it with the *current* (unchanged) customer data. The result is the set of new/deleted joined rows contributed by the order changes.

**Part 2 (customers change):**
```
Δresult += orders ⋈ Δcustomers
```
For each changed customer, join it with the *current* order data. The result is the set of rows that need to be updated because the customer's region changed.

**Part 3 (both change simultaneously):**
```
Δresult += Δorders ⋈ Δcustomers
```
If both change in the same batch, this cross-term captures rows affected by both changes simultaneously. For most workloads, this term is small (it requires a customer and one of their orders to change in the same batch).

In pg_trickle, "current data" in parts 1 and 2 is accessed from the live table, usually via an index lookup on the join key. This is why joins across tables with good indexes are handled efficiently: each delta lookup is a point query, not a scan.

---

### Aggregation (GROUP BY + aggregate functions)

This is the most important delta rule for most analytical use cases.

```sql
-- Query
SELECT region, SUM(total) AS revenue, COUNT(*) AS order_count
FROM orders o
JOIN customers c ON c.id = o.customer_id
GROUP BY region;
```

When an order is inserted for a customer in `europe`:
- `region = 'europe'`
- `delta_revenue = +order.total`
- `delta_order_count = +1`

The delta rule for `SUM`:
```
new_sum(g) = old_sum(g) + delta_sum(g)
           = old_sum(g) + SUM(weight × value for changed rows in group g)
```

For `COUNT`:
```
new_count(g) = old_count(g) + delta_count(g)
             = old_count(g) + SUM(weight for changed rows in group g)
```

For `AVG`:
```
-- AVG is maintained as (running_sum, running_count)
new_avg(g) = new_sum(g) / new_count(g)
```

The key property: these aggregates are *linear*. Their delta is a function of the delta inputs, not of the full input. The mathematical name for this is being a *monoid homomorphism* — the aggregate is a homomorphism from the monoid of multisets to the monoid of aggregate values.

`SUM` is linear: `SUM(A ∪ B) = SUM(A) + SUM(B)`.
`COUNT` is linear: `COUNT(A ∪ B) = COUNT(A) + COUNT(B)`.
`AVG` is *not* directly linear but can be decomposed into linear components (sum and count).

---

### The Non-Differentiable Cases

Not every aggregate is linear.

**`MEDIAN` (PERCENTILE_CONT(0.5)):** You can't compute the median of a union from the medians of the parts. `MEDIAN([1,3,5]) = 3`. `MEDIAN([2,4]) = 3`. `MEDIAN([1,2,3,4,5]) = 3`. But generally, `MEDIAN(A ∪ B) ≠ f(MEDIAN(A), MEDIAN(B))`. Computing the median incrementally requires knowing the *sorted order* of all elements, which requires O(n) state.

**`RANK() OVER (ORDER BY x)`:** Inserting one row can change the rank of every other row. The delta rule is `Δrank(row) = COUNT(new rows that rank higher than row)`. Computing this requires knowing where in the ordering the new row falls, which requires a sorted data structure. The update is O(log n) per changed row, but it's not O(1) per change batch.

**`DISTINCT` counting (COUNT DISTINCT):** Removing an element from a DISTINCT count requires knowing whether the element appears elsewhere. This is the "set membership" problem — you need to maintain the full set, not just a count. Approximate cardinality (HyperLogLog) is differentiable; exact DISTINCT counting is not.

**`MAX` and `MIN` with deletions:** Adding a new maximum is easy (`new_max = max(old_max, new_value)`). Removing the current maximum is hard — you need to find the *second* largest value. This requires a sorted structure.

pg_trickle is transparent about these limitations. If you create a stream table with a non-differentiable query, it either rejects the `DIFFERENTIAL` mode or falls back to `FULL` with a warning. The extension's query analyzer classifies each aggregate and operator before deciding the refresh mode.

---

## The Operator Pipeline

In differential dataflow, a query is represented as a pipeline of operators, each with its own delta rule:

```
ΔT (raw change from CDC)
    │
    ▼ Filter operator (apply WHERE clauses)
    │
    ▼ Join operator (propagate through JOINs)
    │
    ▼ Project operator (select columns)
    │
    ▼ Aggregate operator (GROUP BY + aggregate functions)
    │
    ▼ ΔR (change to apply to the stream table)
```

Each operator transforms the incoming delta using its delta rule. The output of each operator is itself a delta — a set of weighted rows to insert or remove.

The final delta `ΔR` is applied to the stream table using standard SQL `INSERT`, `UPDATE`, `DELETE` — or more efficiently, a single `MERGE` statement that handles all three cases.

---

## Why This Is Faster Than It Sounds

The pipeline is only computed for the rows in `ΔT` — the rows that actually changed. Every other row in the source tables is untouched.

For the join operator, "current customer data" is fetched via index lookups on the join key. If 10 orders changed, that's 10 index lookups. Not a table scan.

For the aggregate operator, only the affected groups are recomputed. If 10 orders changed, and they're all from `europe`, only the `europe` row in `revenue_by_region` is updated.

The cost of a refresh cycle scales with:
1. The number of rows changed in the source tables during the cycle
2. The number of distinct groups affected in each aggregate
3. The number of join hops required to propagate the change to the result

For typical OLTP workloads with hundreds of changes per cycle and stable aggregate groups, refresh cycles complete in 10–50ms regardless of total table size.

---

## The Consistency Guarantee

One subtle point: the delta computation must be consistent.

When computing `Δorders ⋈ customers` (part 1 of the join delta rule), "current customers" should be the customers table *after* any changes in the current batch have been applied.

pg_trickle handles this by processing deltas in topological order when multiple source tables change simultaneously. If both `orders` and `customers` change in the same batch, the join delta is computed with both changes applied, not with a mix of old and new values.

This is the correctness property that makes IVM hard in practice. pg_trickle's engine handles it by processing each refresh cycle as a single transaction that sees a consistent snapshot of all source changes.

---

## The MERGE Application

After computing `ΔR`, pg_trickle applies it to the stream table using a single `MERGE` statement:

```sql
MERGE INTO revenue_by_region AS target
USING delta_revenue AS source
ON target.region = source.region
WHEN MATCHED AND source.revenue != 0 THEN
  UPDATE SET revenue = target.revenue + source.revenue,
             order_count = target.order_count + source.order_count
WHEN MATCHED AND source.revenue = 0 AND source.order_count = 0 THEN
  DELETE
WHEN NOT MATCHED AND source.revenue != 0 THEN
  INSERT (region, revenue, order_count)
  VALUES (source.region, source.revenue, source.order_count);
```

A group with a positive delta gets updated. A group that drops to zero (all orders removed) gets deleted. A new group gets inserted.

The `MERGE` is atomic — it either fully applies or not at all. This is what makes pg_trickle's consistency guarantee possible: the stream table is always in a state consistent with some version of the source tables.

---

## What This Means for You

The practical implications of the differential dataflow mathematics:

**Query design:** Stick to filters, joins, projections, and linear aggregates (SUM, COUNT, AVG). These are fully differentiable. Use `DIFFERENTIAL` mode.

**Non-linear aggregates:** Use PERCENTILE, RANK, or COUNT DISTINCT? Use `FULL` mode for those tables. The stream table infrastructure is still useful — scheduling, monitoring, the catalog — but the refresh will scan the full source.

**Index design:** The join delta rules (`Δorders ⋈ customers`) require efficient index lookups on join keys. Ensure your source tables have indexes on the columns used in `JOIN ON` conditions. Missing join indexes make pg_trickle fall back to sequential scans during delta computation.

**Change volume:** The cost of a refresh cycle scales with the number of changed rows. High-frequency small changes (1–100 rows/cycle) are very cheap. High-frequency bulk operations (10k rows/cycle from batch imports) are more expensive — the delta is large. Use pg_trickle's `change_buffer_size` GUC to tune how much change is batched before a forced refresh.

The mathematics is not magic — it's a formal system with well-defined properties and limitations. Understanding those properties lets you build stream tables that are fast, correct, and predictable.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
