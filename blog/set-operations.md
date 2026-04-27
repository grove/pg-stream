[← Back to Blog Index](README.md)

# Set Operations Done Right: UNION, INTERSECT, EXCEPT

## Incremental maintenance of set operations with multiplicity tracking

---

Set operations in SQL — `UNION`, `INTERSECT`, `EXCEPT` — combine results from multiple queries. They're straightforward to compute from scratch but surprisingly subtle to maintain incrementally. The subtlety is in the multiplicities: how many copies of each row exist on each side, and what happens when those counts change.

pg_trickle maintains all three set operations (and their `ALL` variants) incrementally using **dual-count multiplicity tracking.** Each result row tracks how many copies exist on the left side and the right side, and the set operation's semantics determine whether the row appears in the output.

---

## UNION ALL: The Simple Case

```sql
SELECT name, email FROM customers
UNION ALL
SELECT name, email FROM prospects;
```

`UNION ALL` is the simplest: no deduplication. Every row from both sides appears in the result. The delta rule is trivial:

- Insert on left → insert in result.
- Insert on right → insert in result.
- Delete on left → delete from result.
- Delete on right → delete from result.

pg_trickle handles this with standard delta propagation. No multiplicity tracking needed.

---

## UNION (Deduplicating): Where It Gets Interesting

```sql
SELECT name, email FROM customers
UNION
SELECT name, email FROM prospects;
```

`UNION` (without `ALL`) deduplicates: if the same row exists in both sides, it appears once in the result.

The delta rule requires knowing how many copies of each row exist across both sides:

```
left_count  = number of copies on the left side
right_count = number of copies on the right side
total       = left_count + right_count

Row appears in result if total > 0.
```

**Insert on left side:**
```
left_count += 1
if left_count + right_count was 0 (row was absent) → INSERT into result
else → no output change (row already present)
```

**Delete on left side:**
```
left_count -= 1
if left_count + right_count = 0 → DELETE from result
else → no output change (other copies remain)
```

This is the same reference-counting approach used for [DISTINCT](distinct-reference-counting.md), extended to track counts per side.

---

## INTERSECT: Present on Both Sides

```sql
SELECT product_id FROM warehouse_a
INTERSECT
SELECT product_id FROM warehouse_b;
```

Products available in both warehouses. The result includes a row only if it exists on both sides.

```
Row appears in result if left_count > 0 AND right_count > 0.
```

**Insert on left side (new product in warehouse A):**
```
left_count += 1
if left_count = 1 AND right_count > 0 → INSERT into result (now present on both sides)
```

**Delete on left side:**
```
left_count -= 1
if left_count = 0 AND right_count > 0 → DELETE from result (no longer on both sides)
```

**Insert on right side:** Mirror of left-side logic.

---

## EXCEPT: Present on Left, Not on Right

```sql
SELECT customer_id FROM all_customers
EXCEPT
SELECT customer_id FROM opted_out_customers;
```

Customers who haven't opted out. The result includes a row if it's on the left side and not on the right side.

```
Row appears in result if left_count > 0 AND right_count = 0.
```

**Insert on right side (customer opts out):**
```
right_count += 1
if left_count > 0 AND right_count = 1 → DELETE from result (now excluded)
```

**Delete on right side (customer opts back in):**
```
right_count -= 1
if left_count > 0 AND right_count = 0 → INSERT into result (no longer excluded)
```

This is the anti-join behavior: adding to the right side *removes* from the result. It's the set-operation analog of [NOT EXISTS](exists-not-exists-delta-rules.md).

---

## The ALL Variants

`INTERSECT ALL` and `EXCEPT ALL` preserve multiplicities:

**INTERSECT ALL:** The result contains `min(left_count, right_count)` copies of each row.

**EXCEPT ALL:** The result contains `max(left_count - right_count, 0)` copies.

The delta rules are more complex because changing a count on one side can change the output multiplicity by more than 1. pg_trickle handles this by computing the before and after output counts and emitting the difference:

```
output_before = min(left_count_before, right_count_before)  -- for INTERSECT ALL
output_after  = min(left_count_after, right_count_after)
delta = output_after - output_before

if delta > 0: emit INSERT × delta
if delta < 0: emit DELETE × |delta|
```

---

## Creating Stream Tables with Set Operations

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'available_everywhere',
  query => $$
    SELECT product_id, product_name FROM warehouse_east
    INTERSECT
    SELECT product_id, product_name FROM warehouse_west
  $$,
  schedule => '10s'
);
```

When a product is added to `warehouse_east`, pg_trickle:
1. Increments the left-side count for that product.
2. Checks the right-side count.
3. If the product is now in both warehouses → inserts into the result.

When a product is removed from `warehouse_west`:
1. Decrements the right-side count.
2. If the count drops to 0 → removes the product from the result.

---

## Building Merge Tables from Heterogeneous Sources

A practical use of UNION ALL: combining data from multiple source systems into a unified view.

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'all_contacts',
  query => $$
    SELECT 'crm' AS source, id, name, email FROM crm_contacts
    UNION ALL
    SELECT 'marketing' AS source, id, name, email FROM marketing_leads
    UNION ALL
    SELECT 'support' AS source, ticket_contact_id AS id, name, email FROM support_tickets
  $$,
  schedule => '5s'
);
```

Three different source systems, merged into one stream table. Changes to any source are reflected in the merged result within 5 seconds. Each branch maintains its own delta independently.

If you need deduplication across sources (same email from CRM and marketing → one row):

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'unique_contacts',
  query => $$
    SELECT DISTINCT email, name FROM (
      SELECT name, email FROM crm_contacts
      UNION ALL
      SELECT name, email FROM marketing_leads
      UNION ALL
      SELECT name, email FROM support_tickets
    ) all_sources
  $$,
  schedule => '10s'
);
```

The UNION ALL feeds into DISTINCT, which uses reference counting. A contact appearing in all three systems has `__pgt_dup_count = 3`. Remove them from CRM → count drops to 2. Remove from all three → count drops to 0 → removed from result.

---

## Performance

Set operation delta costs:

| Operation | Left-side change cost | Right-side change cost |
|-----------|----------------------|----------------------|
| UNION ALL | O(delta) — direct pass-through | O(delta) |
| UNION | O(delta) — count check per row | O(delta) |
| INTERSECT | O(delta) — check other side's count | O(delta) |
| EXCEPT | O(delta) — check other side's count | O(delta) |

All operations are O(delta) per refresh cycle. The constant factor is slightly higher for deduplicating operations (UNION, INTERSECT, EXCEPT) because each changed row requires a count lookup on the other side.

---

## Summary

pg_trickle maintains set operations incrementally using dual-count multiplicity tracking. Each result row knows how many copies exist on the left and right sides. The set operation's semantics (UNION: either side, INTERSECT: both sides, EXCEPT: left but not right) determine when the row appears in the output.

UNION ALL is a direct pass-through. UNION uses reference counting. INTERSECT requires presence on both sides. EXCEPT removes from the result when the right side gains a match.

For merging heterogeneous data sources, combining UNION ALL with DISTINCT gives you a continuously maintained, deduplicated merge table. All of it incremental. All of it O(delta).
