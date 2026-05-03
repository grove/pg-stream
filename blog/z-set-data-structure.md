[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# The Z-Set: The Data Structure That Makes IVM Correct

## A short tour of the data structure under pg_trickle's differential engine

---

Every incremental view maintenance system needs a way to represent changes. "Row 42 was inserted." "Row 17 was deleted." "Row 99 was updated from value A to value B."

Most systems represent this as a list of operations: INSERT, DELETE, UPDATE. The operations are applied sequentially. Ordering matters. If you apply a DELETE before the corresponding INSERT, you get an error.

pg_trickle uses a different representation: the **Z-set** (integer-weighted multiset). It's simpler, more compositional, and eliminates an entire class of bugs.

---

## What a Z-Set Is

A Z-set is a collection of (element, weight) pairs, where the weight is an integer.

| Element | Weight |
|---|---|
| (alice, europe, 100) | +1 |
| (bob, asia, 200) | +1 |
| (charlie, europe, 150) | +1 |

This Z-set represents a table with three rows. Each row has weight `+1`, meaning "present."

When you **insert** a row, you add it with weight `+1`:

| Element | Weight |
|---|---|
| (dave, asia, 300) | +1 |

When you **delete** a row, you add it with weight `-1`:

| Element | Weight |
|---|---|
| (bob, asia, 200) | -1 |

When you **update** a row (bob's amount changes from 200 to 250):

| Element | Weight |
|---|---|
| (bob, asia, 200) | -1 |
| (bob, asia, 250) | +1 |

An update is a delete-then-insert. There's no special "update" operation. This simplification is the key insight.

---

## Why Weights Instead of Operations

The operation-based approach (INSERT/DELETE/UPDATE as separate types) has a problem: the order of operations matters. If you have:

```
INSERT (alice, 100)
DELETE (alice, 100)
INSERT (alice, 200)
```

Reordering these gives different results. The system has to track and preserve ordering.

With Z-sets, the order doesn't matter. You just sum the weights:

```
(alice, 100): +1 - 1 = 0   ← not present
(alice, 200): +1            ← present
```

Net result: alice has value 200. The intermediate states cancel out. You can process the changes in any order and get the same result.

This makes Z-sets **commutative** and **associative** — you can combine them freely without worrying about ordering. This property is what allows pg_trickle to batch changes and process them efficiently.

---

## Delta Rules as Weight Arithmetic

The differential engine's job is to transform a Z-set of input changes into a Z-set of output changes. Each SQL operator has a delta rule expressed as weight arithmetic.

### Filter (WHERE)

```sql
-- Query: SELECT * FROM t WHERE amount > 100
-- Delta rule: keep rows where amount > 100, preserve weights
```

Input delta:
| Element | Weight |
|---|---|
| (alice, 150) | +1 |
| (bob, 50) | +1 |

Output delta:
| Element | Weight |
|---|---|
| (alice, 150) | +1 |

Bob's row has weight +1 in the input but doesn't pass the filter, so it's not in the output. Alice's row passes, so its weight is preserved.

### Projection (SELECT columns)

If projecting causes duplicates, weights add up:

```sql
-- Query: SELECT region FROM customers
-- Input has two customers in 'europe'
```

| Element | Weight |
|---|---|
| (alice, europe) → europe | +1 |
| (bob, europe) → europe | +1 |

Result: `(europe, +2)`. The region "europe" appears twice.

### JOIN

The JOIN delta rule is where Z-sets really shine. Given tables R and S:

```
Δ(R ⋈ S) = (ΔR ⋈ S) ∪ (R ⋈ ΔS) ∪ (ΔR ⋈ ΔS)
```

In words: the change to a JOIN result is the union of:
1. New R rows joined with existing S rows
2. Existing R rows joined with new S rows
3. New R rows joined with new S rows (handles the case where both sides change simultaneously)

Because we're working with Z-sets, the union is just weight addition. If a pair appears in both term 1 and term 3, their weights add. This handles double-counting automatically.

### Aggregation (GROUP BY + SUM)

```sql
-- Query: SELECT region, SUM(amount) FROM orders GROUP BY region
```

Input delta (an order for $100 in europe is inserted):
| Element | Weight |
|---|---|
| (europe, 100) | +1 |

The delta rule for SUM:
- Find the group (europe).
- Add weight × value to the running sum: +1 × 100 = +100.
- Output: the europe group's sum increases by 100.

If a row is deleted (weight -1), the same rule applies: -1 × 100 = -100. The sum decreases.

For COUNT: it's just the sum of weights.

For AVG: it's maintained as (SUM, COUNT) internally. AVG = SUM / COUNT.

---

## Composition

The power of Z-sets is that delta rules compose. A query like:

```sql
SELECT region, SUM(amount) FROM orders
JOIN customers ON customers.id = orders.customer_id
WHERE orders.status = 'shipped'
GROUP BY region;
```

is decomposed into: Filter → Join → GroupBy+Sum. Each operator's delta rule takes a Z-set as input and produces a Z-set as output. The Z-set from Filter feeds into the Z-set for Join, which feeds into GroupBy+Sum.

Because each rule preserves the Z-set structure (input: weighted multiset → output: weighted multiset), you can chain them without special glue code. The intermediate Z-sets handle cancellations, duplicates, and concurrent changes automatically.

---

## Consolidation

After processing all the delta rules, the output Z-set might have redundant entries:

| Element | Weight |
|---|---|
| (europe, 1000) | -1 |
| (europe, 1100) | +1 |
| (europe, 1100) | +1 |

Consolidation sums weights for identical elements:

| Element | Weight |
|---|---|
| (europe, 1000) | -1 |
| (europe, 1100) | +2 |

And removes elements with weight 0:

If (europe, 1000) had weight +1 and -1, the net is 0 — it's removed from the output entirely.

The final consolidated Z-set is the MERGE operation: elements with negative weight are DELETEd from the stream table, elements with positive weight are INSERTed.

---

## What Z-Sets Can't Handle

Z-sets work for operators with well-defined inverses:
- SUM: adding is the inverse of subtracting
- COUNT: incrementing is the inverse of decrementing
- AVG: maintained as (SUM, COUNT), both invertible
- MIN/MAX: invertible with caveats (need to re-scan the group when the min/max value is deleted)

Z-sets don't work for:
- **MEDIAN:** No closed-form inverse. Deleting a row can change the median to any other value in the group.
- **PERCENTILE_CONT/DISC:** Same problem — order statistics don't have algebraic inverses.
- **DISTINCT without GROUP BY:** The weight represents multiplicity, but DISTINCT collapses it to 1. Deleting one of three duplicates changes the weight from 3 to 2, but DISTINCT still outputs 1. The delta is zero. Deleting the last duplicate changes weight from 1 to 0 — now the delta is -1.

For these cases, pg_trickle falls back to FULL refresh mode. The Z-set representation is still used internally, but the delta rule re-scans the group rather than computing a closed-form delta.

---

## Why This Matters in Practice

You don't need to think about Z-sets when using pg_trickle. The abstraction is internal.

But understanding Z-sets explains:
- **Why some aggregates support DIFFERENTIAL and others don't.** It's about whether the aggregate has an algebraic inverse in Z-set arithmetic.
- **Why UPDATEs are handled correctly without special casing.** They're just delete+insert = weight -1 and weight +1.
- **Why concurrent changes don't cause double-counting.** Weights are additive and commutative. The order of processing doesn't matter.
- **Why pg_trickle's correctness testing works.** The multiset invariant (DIFFERENTIAL result = FULL result) is a Z-set equality check.

The Z-set is the foundation that makes everything else in the differential engine compositional and correct.
