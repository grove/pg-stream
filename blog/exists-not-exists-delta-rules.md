[← Back to Blog Index](README.md)

# EXISTS and NOT EXISTS: The Delta Rules Nobody Talks About

## Semi-joins and anti-joins, maintained incrementally

---

`EXISTS` and `NOT EXISTS` subqueries appear in almost every non-trivial SQL codebase. "Show me orders that have at least one item over $100." "Show me customers who haven't placed an order this month." They're the SQL equivalent of set membership and set complement.

Making them incremental is trickier than it looks. A semi-join (`EXISTS`) has binary output per row — the row either qualifies or it doesn't. A single insert into the subquery table can flip that binary for multiple outer rows. An anti-join (`NOT EXISTS`) is even worse: adding a matching row *removes* outer rows from the result.

pg_trickle handles both, using delta-key pre-filtering and inverted weight semantics. This post explains the rules.

---

## EXISTS as a Semi-Join

```sql
SELECT c.customer_id, c.name
FROM customers c
WHERE EXISTS (
  SELECT 1 FROM orders o
  WHERE o.customer_id = c.customer_id
    AND o.total > 1000
);
```

This returns customers who have at least one high-value order. From a set perspective, it's the semi-join: `customers ⋉ (orders WHERE total > 1000)`.

The key property: **duplicates in the subquery don't matter.** Whether a customer has 1 or 100 high-value orders, they appear in the result exactly once.

### Delta Rule for EXISTS

When orders change (insert/delete), the stream table delta is:

**Insert into orders (new high-value order):**
1. Check: does the customer already have a qualifying order? (Was the EXISTS already true?)
2. If no → the customer is newly qualifying. Add them to the result (+1 weight).
3. If yes → no change. The EXISTS was already satisfied.

**Delete from orders (removed high-value order):**
1. Check: does the customer still have any qualifying orders?
2. If no → the customer no longer qualifies. Remove them from the result (-1 weight).
3. If yes → no change. Other orders still satisfy the EXISTS.

This "before/after" check is the core of the semi-join delta. pg_trickle implements it using a **reference count** on the subquery match:

```
refcount(customer_id) = number of matching orders for that customer
```

- INSERT: increment refcount. If it went from 0 → 1, emit +1 to result.
- DELETE: decrement refcount. If it went from 1 → 0, emit -1 to result.
- If refcount was >1 before and is still >1 after, no result change.

---

## NOT EXISTS as an Anti-Join

```sql
SELECT c.customer_id, c.name
FROM customers c
WHERE NOT EXISTS (
  SELECT 1 FROM orders o
  WHERE o.customer_id = c.customer_id
    AND o.created_at > NOW() - INTERVAL '30 days'
);
```

Customers who haven't ordered in the last 30 days. This is the anti-join: `customers ▷ (recent orders)`.

### Delta Rule for NOT EXISTS

The delta is the inverse of EXISTS:

**Insert into orders (new recent order):**
1. Check: did the customer previously have zero matching orders? (Was NOT EXISTS true?)
2. If yes → the customer now has a matching order. Remove them from the result (-1 weight).
3. If no → no change. The NOT EXISTS was already false.

**Delete from orders (removed recent order):**
1. Check: does the customer now have zero matching orders?
2. If yes → the customer re-qualifies. Add them to the result (+1 weight).
3. If no → no change. Other recent orders remain.

Same reference counting, inverted logic. The cost is identical.

---

## Delta-Key Pre-Filtering

The critical optimization is **pre-filtering by the join key**. When orders change, pg_trickle doesn't scan all customers to check the EXISTS condition. It filters by the join key from the change buffer.

If 5 new orders come in for customer IDs {12, 34, 56}, pg_trickle:

1. Extracts the distinct `customer_id` values from the change buffer: {12, 34, 56}.
2. Checks the reference count only for those 3 customers.
3. Updates the result only if the reference count crossed the 0/1 boundary.

The cost is proportional to the number of distinct join keys in the change buffer, not the size of the customer table.

```sql
-- Internal logic (simplified)
WITH changed_keys AS (
  SELECT DISTINCT customer_id
  FROM pgtrickle_changes.changes_orders
  WHERE __pgt_op IN ('I', 'D')
),
new_counts AS (
  SELECT c.customer_id, COUNT(o.id) AS cnt
  FROM customers c
  LEFT JOIN orders o ON o.customer_id = c.customer_id
    AND o.total > 1000
  WHERE c.customer_id IN (SELECT customer_id FROM changed_keys)
  GROUP BY c.customer_id
)
-- Emit delta based on cnt crossing 0 boundary
...
```

---

## Correlated Subqueries

EXISTS subqueries are often correlated — they reference columns from the outer query:

```sql
SELECT p.product_id, p.name
FROM products p
WHERE EXISTS (
  SELECT 1 FROM inventory i
  WHERE i.product_id = p.product_id
    AND i.warehouse_id = p.default_warehouse_id
    AND i.quantity > 0
);
```

The correlation here is on two columns: `product_id` and `default_warehouse_id`. pg_trickle extracts both as the join key for pre-filtering.

When inventory changes for a specific (product_id, warehouse_id) pair, only that pair is checked against the reference count. Products in other warehouses are untouched.

---

## SubLink Extraction

PostgreSQL's internal representation of EXISTS subqueries is called a "SubLink." pg_trickle's parser extracts SubLinks from the WHERE clause and converts them to semi-join or anti-join operators in the OpTree.

The extraction handles:

- Simple `WHERE EXISTS (...)` — direct semi-join.
- `WHERE NOT EXISTS (...)` — direct anti-join.
- `WHERE EXISTS (...) AND other_condition` — semi-join followed by filter.
- `WHERE EXISTS (...) OR other_condition` — requires special handling (covered below).

### The OR Case

```sql
WHERE EXISTS (SELECT 1 FROM orders WHERE ...) OR status = 'VIP'
```

An `OR` with an EXISTS is harder because you can't simply convert to a semi-join — the row might qualify from the non-EXISTS branch. pg_trickle rewrites this as a `UNION ALL`:

```sql
-- Branch 1: rows qualifying via EXISTS
SELECT ... WHERE EXISTS (...)

UNION ALL

-- Branch 2: rows qualifying via the other condition (minus those already in Branch 1)
SELECT ... WHERE status = 'VIP' AND NOT EXISTS (...)
```

Each branch is then maintained independently with the rules described above.

---

## IN and NOT IN

`IN` with a subquery is semantically equivalent to `EXISTS`:

```sql
-- These are equivalent
WHERE customer_id IN (SELECT customer_id FROM vip_list)
WHERE EXISTS (SELECT 1 FROM vip_list WHERE vip_list.customer_id = customers.customer_id)
```

pg_trickle normalizes `IN (subquery)` to `EXISTS` during parsing. The delta rules are the same.

`NOT IN` is equivalent to `NOT EXISTS`, with one important difference: `NOT IN` has tricky NULL semantics. If the subquery returns any NULL, `NOT IN` returns FALSE for all outer rows. pg_trickle handles this correctly by tracking whether the subquery contains NULLs and short-circuiting the anti-join logic when it does.

---

## Nested EXISTS

EXISTS subqueries can be nested:

```sql
SELECT d.department_id
FROM departments d
WHERE EXISTS (
  SELECT 1 FROM employees e
  WHERE e.department_id = d.department_id
    AND EXISTS (
      SELECT 1 FROM certifications c
      WHERE c.employee_id = e.employee_id
        AND c.type = 'security_clearance'
    )
);
```

"Departments that have at least one employee with a security clearance."

pg_trickle processes nested EXISTS by flattening the SubLinks bottom-up:

1. Inner EXISTS: certifications → employees (semi-join on employee_id)
2. Outer EXISTS: filtered employees → departments (semi-join on department_id)

Each level uses its own reference count and delta-key pre-filtering. A new certification insert triggers a check on the employee, which may trigger a check on the department.

---

## Performance

The cost of maintaining EXISTS/NOT EXISTS is dominated by the reference-count lookup. For each changed key, pg_trickle needs to know the current count of matching rows.

This requires either:
- A maintained count (stored alongside the stream table data), or
- A query against the current source data for the affected keys.

pg_trickle uses the latter — it queries the source tables filtered to the changed keys. This is efficient when the number of changed keys is small (typical case). It can be expensive when a bulk operation affects many keys.

| Scenario | Changed keys | Cost |
|----------|-------------|------|
| 1 new order | 1 customer lookup | <1ms |
| 100 new orders, 30 distinct customers | 30 customer lookups | ~5ms |
| Bulk import: 100K orders, 10K customers | 10K customer lookups | ~200ms |
| FULL refresh (fallback) | All rows | Same as non-IVM |

For the bulk import case, pg_trickle's AUTO mode may decide that FULL refresh is faster than 10,000 individual lookups. The cost model accounts for the number of distinct join keys, not just the number of changed rows.

---

## Summary

EXISTS and NOT EXISTS are maintained incrementally via reference counting on the join key. A match count tracks how many subquery rows qualify for each outer row. When the count crosses the 0/1 boundary, the outer row is added to or removed from the result.

Delta-key pre-filtering ensures the cost is proportional to the number of changed join keys, not the table size. Correlated subqueries, nested EXISTS, and OR conditions are all handled through SubLink extraction and rewrite.

The result: subqueries that would force a full scan in a materialized view are maintained incrementally in a stream table. Write the EXISTS you need. pg_trickle figures out the delta.
