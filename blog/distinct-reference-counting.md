[← Back to Blog Index](README.md)

# DISTINCT That Doesn't Recount

## Reference counting for incremental deduplication

---

`SELECT DISTINCT` in a materialized view means a full table scan on every refresh. PostgreSQL has to see all the rows to determine which are unique. There's no shortcut — without knowing the full data set, you can't know if a row is duplicated.

pg_trickle has a shortcut: **reference counting.** Each distinct value gets a counter tracking how many source rows produce it. Insert a duplicate? Increment the counter. Delete a row? Decrement the counter. When the counter hits zero, the value is removed from the result.

No scan of existing data. O(delta) per refresh.

---

## The Problem

```sql
SELECT DISTINCT region, product_category
FROM orders
JOIN customers ON customers.id = orders.customer_id;
```

This query deduplicates (region, product_category) pairs. With 50 million orders across 200 unique pairs, a full refresh scans 50 million rows to produce 200 rows.

With IVM, 10 new orders come in. Do they create any new (region, product_category) pairs? Or are all 10 in existing pairs? To answer this without scanning, you need to know how many rows currently produce each pair.

---

## The __pgt_dup_count Column

pg_trickle maintains a hidden column `__pgt_dup_count` on stream tables that use DISTINCT. This column tracks the multiplicity of each row in the result — how many source rows produce it.

```sql
-- What the stream table actually stores (internal representation)
SELECT region, product_category, __pgt_dup_count
FROM pgtrickle.distinct_pairs;
```

```
  region    | product_category | __pgt_dup_count
------------+------------------+-----------------
 Northeast  | Electronics      | 12,847
 Northeast  | Clothing         | 8,432
 Southeast  | Electronics      | 15,291
 ...        | ...              | ...
```

The user-visible query (`SELECT * FROM distinct_pairs`) hides `__pgt_dup_count` — you just see the distinct values.

---

## Delta Rules

**INSERT a row that matches an existing distinct value:**
```
__pgt_dup_count += 1
```
No new row is added to the result. The value was already there.

**INSERT a row with a new distinct value:**
```
INSERT row with __pgt_dup_count = 1
```
New distinct value appears in the result.

**DELETE a row:**
```
__pgt_dup_count -= 1
If __pgt_dup_count = 0: DELETE the row from the result
```
The distinct value disappears only when all source rows producing it are gone.

**UPDATE a row (changes the distinct columns):**
```
Decrement old value's __pgt_dup_count (possibly remove it)
Increment new value's __pgt_dup_count (possibly insert it)
```

---

## Example

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'active_regions',
  query => $$
    SELECT DISTINCT c.region
    FROM orders o
    JOIN customers c ON c.id = o.customer_id
    WHERE o.status = 'active'
  $$,
  schedule => '5s'
);
```

Initial state: 5 regions, each with thousands of active orders.

A customer in the "Pacific" region places their first order. Before this, "Pacific" had 0 active orders — it wasn't in the result. Now it has 1.

pg_trickle:
1. Sees the INSERT in the change buffer.
2. Joins with `customers` to get region = "Pacific".
3. Checks: does "Pacific" exist in the result? No → INSERT with `__pgt_dup_count = 1`.

Later, the only Pacific order is cancelled (status changes to 'cancelled', which is filtered out by `WHERE status = 'active'`). The effective delta is a DELETE of that row.

pg_trickle:
1. Decrements Pacific's `__pgt_dup_count` from 1 to 0.
2. Removes "Pacific" from the result.

All other regions are untouched. The refresh processes 1 row, not millions.

---

## DISTINCT ON

PostgreSQL's `DISTINCT ON` is a different feature from `DISTINCT`. It returns one row per group, ordered by a specified column:

```sql
SELECT DISTINCT ON (customer_id)
  customer_id, order_id, total, created_at
FROM orders
ORDER BY customer_id, created_at DESC;
```

This returns the most recent order per customer. It's a common pattern for "latest row per group."

pg_trickle handles `DISTINCT ON` with the same reference-counting approach, but the tie-breaking logic is more complex. The stream table maintains the winning row (based on the ORDER BY) and updates it when:

- A new row with a higher sort value is inserted (it becomes the new winner).
- The current winner is deleted (the next-best row becomes the winner).

This requires knowing what the "next-best" row is, which in turn requires a lookup against the source data. The cost is O(changed groups) — for each group that was affected by the delta, one query to find the new winner.

---

## DISTINCT with Expressions

DISTINCT can appear with computed columns:

```sql
SELECT DISTINCT date_trunc('month', created_at) AS month
FROM events;
```

The reference counting applies to the *computed* value, not the raw column. Two events on different days in the same month produce the same distinct value and share a `__pgt_dup_count`.

---

## Performance

The reference-count approach makes DISTINCT maintenance O(|ΔT|) — proportional to the number of changed rows, not the table size.

| Scenario | FULL refresh | DIFFERENTIAL refresh |
|----------|-------------|---------------------|
| 10 inserts, all in existing groups | ~500ms (full scan) | <1ms (10 counter increments) |
| 10 inserts, 2 new groups | ~500ms | <1ms (8 increments + 2 inserts) |
| 10 deletes, none empties a group | ~500ms | <1ms (10 counter decrements) |
| 10 deletes, 1 group drops to zero | ~500ms | <1ms (9 decrements + 1 delete) |

The only scenario where DIFFERENTIAL doesn't help is when the delta touches every group — but that's rare for DISTINCT queries, which by definition have fewer groups than rows.

---

## When Not to Use DISTINCT in Stream Tables

DISTINCT adds the `__pgt_dup_count` overhead to every row. If your query naturally produces unique rows (e.g., `GROUP BY` with a key that guarantees uniqueness), adding DISTINCT is redundant and wasteful.

Check with `EXPLAIN`:

```sql
EXPLAIN SELECT DISTINCT region, SUM(total)
FROM orders GROUP BY region;
```

If the `GROUP BY` already produces unique (region) rows, the DISTINCT is a no-op. Remove it — pg_trickle still maintains the stream table correctly, without the reference-counting overhead.

---

## Summary

DISTINCT in stream tables uses reference counting (`__pgt_dup_count`) to avoid full-scan deduplication. Insert increments, delete decrements, and rows are removed only when the count reaches zero.

The cost is O(delta), not O(table). For the common case — many source rows, few distinct values, small changes per cycle — this is orders of magnitude faster than recomputing.

DISTINCT ON works similarly but with tie-breaking logic. Remove DISTINCT if GROUP BY already ensures uniqueness. And don't worry about the hidden counter column — it's invisible to your queries.
