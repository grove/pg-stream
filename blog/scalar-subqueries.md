[← Back to Blog Index](README.md)

# Scalar Subqueries in the SELECT List — Incrementally

## How pg_trickle maintains correlated subqueries without re-executing them for every row

---

Scalar subqueries in the SELECT list are a SQL convenience that hides enormous computational cost:

```sql
SELECT
  o.order_id,
  o.customer_id,
  o.total,
  (SELECT MAX(total) FROM orders o2 WHERE o2.customer_id = o.customer_id) AS customer_max
FROM orders o;
```

For each row in `orders`, PostgreSQL runs the inner query. If `orders` has 1 million rows, the subquery executes 1 million times. Materialized views don't change this — each refresh re-evaluates all subqueries.

pg_trickle maintains scalar subqueries incrementally using a **pre/post snapshot diff** technique. It doesn't re-run the subquery for every row — only for rows where the subquery result changed.

---

## The Technique

A scalar subquery in the SELECT list is correlated — it references columns from the outer query. The result depends on which outer row it's evaluating.

pg_trickle transforms this into a two-phase process:

### Phase 1: Pre-Snapshot

Before processing the delta, compute the scalar subquery result for affected rows using the *previous* state of the data:

```sql
-- Pre-snapshot: MAX(total) per customer, before changes
SELECT customer_id, MAX(total) AS customer_max
FROM orders
WHERE customer_id IN (SELECT customer_id FROM changed_rows)
GROUP BY customer_id;
```

### Phase 2: Post-Snapshot

After applying the source changes, compute the scalar subquery result again:

```sql
-- Post-snapshot: MAX(total) per customer, after changes
SELECT customer_id, MAX(total) AS customer_max
FROM orders
WHERE customer_id IN (SELECT customer_id FROM changed_rows)
GROUP BY customer_id;
```

### Phase 3: Diff

Compare pre and post snapshots. Only rows where the subquery result changed need to be updated in the stream table:

```sql
-- Rows where customer_max changed
SELECT post.customer_id, post.customer_max
FROM post_snapshot post
JOIN pre_snapshot pre USING (customer_id)
WHERE post.customer_max != pre.customer_max OR pre.customer_max IS NULL;
```

---

## Why This Is Efficient

The key insight: the scalar subquery result changes only when the *correlated group* is affected by the delta.

If 10 new orders come in across 3 customers, only those 3 customers' `MAX(total)` values could change. The pre/post snapshots are computed only for those 3 customers — not for all 1 million.

| Scenario | Full recompute (materialized view) | Pre/post diff (pg_trickle) |
|----------|-----------------------------------|---------------------------|
| 10 new orders, 3 customers | 1M subquery evaluations | 3 group evaluations |
| 100 new orders, 50 customers | 1M subquery evaluations | 50 group evaluations |
| 1 deleted order | 1M subquery evaluations | 1 group evaluation |

---

## Creating a Stream Table

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'orders_with_customer_stats',
  query => $$
    SELECT
      o.order_id,
      o.customer_id,
      o.total,
      (SELECT AVG(total) FROM orders o2
       WHERE o2.customer_id = o.customer_id) AS customer_avg,
      (SELECT COUNT(*) FROM orders o2
       WHERE o2.customer_id = o.customer_id) AS customer_order_count
    FROM orders o
  $$,
  schedule => '5s'
);
```

pg_trickle detects the two scalar subqueries, extracts their correlation keys (`customer_id`), and applies the pre/post diff technique to each.

---

## Multiple Scalar Subqueries

When a query has multiple scalar subqueries, each is processed independently:

```sql
SELECT
  p.product_id,
  p.name,
  (SELECT COUNT(*) FROM reviews r WHERE r.product_id = p.product_id) AS review_count,
  (SELECT AVG(rating) FROM reviews r WHERE r.product_id = p.product_id) AS avg_rating,
  (SELECT MIN(price) FROM inventory i WHERE i.product_id = p.product_id) AS min_price
FROM products p;
```

Three scalar subqueries, two source tables (`reviews`, `inventory`). When reviews change:
- `review_count` and `avg_rating` are re-evaluated for affected products.
- `min_price` is untouched (inventory didn't change).

When inventory changes:
- `min_price` is re-evaluated for affected products.
- `review_count` and `avg_rating` are untouched.

Each subquery tracks its own source-table dependency.

---

## When a JOIN Is Faster

The pre/post diff technique works, but a JOIN rewrite is often more efficient. The scalar subquery:

```sql
SELECT o.*, (SELECT MAX(total) FROM orders o2 WHERE o2.customer_id = o.customer_id)
FROM orders o;
```

Is equivalent to:

```sql
SELECT o.*, m.max_total
FROM orders o
JOIN (SELECT customer_id, MAX(total) AS max_total FROM orders GROUP BY customer_id) m
  USING (customer_id);
```

The JOIN version is more efficient for IVM because:
- The inner aggregate (`MAX(total)` grouped by `customer_id`) can be maintained algebraically.
- The JOIN delta is a standard equi-join delta — well-optimized.
- No pre/post snapshot comparison needed.

pg_trickle doesn't automatically rewrite scalar subqueries to JOINs (the equivalence isn't always trivial), but if performance matters, consider the manual rewrite.

---

## Limitations

**Non-correlated scalar subqueries** (no reference to the outer query):
```sql
SELECT o.*, (SELECT COUNT(*) FROM products) AS total_products FROM orders o;
```

These are simpler — the subquery result is a single value shared by all rows. pg_trickle caches it and only recomputes when the subquery's source table changes.

**Scalar subqueries with side effects:** Not supported (and shouldn't be in any query).

**Scalar subqueries returning more than one row:** PostgreSQL errors at runtime. pg_trickle inherits this behavior.

**Deeply nested scalar subqueries** (subquery within subquery within SELECT):
```sql
SELECT o.*,
  (SELECT (SELECT MAX(price) FROM products WHERE category = c.category)
   FROM customers c WHERE c.id = o.customer_id) AS category_max_price
FROM orders o;
```

pg_trickle supports one level of nesting. Deeper nesting works but with increasing overhead — each level adds a pre/post snapshot comparison. Consider rewriting deeply nested scalar subqueries as JOINs.

---

## Summary

Scalar subqueries in the SELECT list are maintained incrementally using pre/post snapshot comparison on the correlated group. Only groups affected by the delta are re-evaluated.

The cost is O(affected groups), not O(all rows). For typical workloads — small changes touching a few groups — this is orders of magnitude faster than full recomputation.

If performance is critical, consider rewriting scalar subqueries as JOINs for more efficient delta propagation. But if the scalar subquery is clearer and the performance is acceptable, leave it as is — pg_trickle handles it correctly either way.
