[← Back to Blog Index](README.md)

# LATERAL Joins in a Stream Table

## Row-scoped re-execution for the most powerful join type in PostgreSQL

---

`LATERAL` is the most powerful and least understood join in PostgreSQL. It lets the right side of a join reference columns from the left side — turning each left row into a separate subquery context. `JSON_TABLE`, `unnest()`, `generate_series()`, and correlated subqueries all use LATERAL under the hood.

Making LATERAL incremental is fundamentally different from making a regular join incremental. In a regular join, you can pre-filter the delta: changed rows on one side are joined with all rows on the other side. In a LATERAL join, the right side is *parameterized* by the left side. There's no global right-side table to join against.

pg_trickle handles LATERAL with **row-scoped re-execution**: for each changed left-side row, re-run the LATERAL subquery for that row.

---

## How Regular Join Deltas Work

Recall the delta rule for a regular join:

```
Δ(A ⋈ B) = ΔA ⋈ B  ∪  A ⋈ ΔB
```

Changed rows on the left side are joined with the *full* right side. Changed rows on the right side are joined with the full left side. This works because both sides are independent tables.

## How LATERAL Deltas Work

A LATERAL join doesn't have an independent right side. The right side is a function of each left-side row:

```sql
SELECT o.order_id, o.items_json, i.*
FROM orders o,
LATERAL json_to_recordset(o.items_json)
  AS i(product_id INT, quantity INT, price NUMERIC);
```

Here, the LATERAL subquery (`json_to_recordset`) takes `o.items_json` as input. There is no "right-side table" — the right side is computed per row.

The delta rule becomes:

**Left side changes (Δorders):**
For each changed order, execute the LATERAL subquery for that order. The result is the delta for those rows.

**Right side can't change independently.** The right side is derived from the left side. If the JSON column changes, that's a left-side change.

This simplification is the key insight: LATERAL deltas only need to consider left-side changes.

---

## Row-Scoped Re-Execution

When a row changes on the left side of a LATERAL join, pg_trickle:

1. **For deleted rows:** Remove all result rows that were produced by the old left-side row. (The previous LATERAL expansion is no longer valid.)
2. **For inserted rows:** Execute the LATERAL subquery for the new row. Insert the results.
3. **For updated rows:** Remove old results (step 1), then insert new results (step 2).

This is "re-execution" because the LATERAL subquery is literally re-run for each affected left-side row. It's "row-scoped" because only the affected rows are processed.

---

## Practical Examples

### JSON Document Unpacking

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'order_line_items',
  query => $$
    SELECT
      o.order_id,
      o.customer_id,
      i.product_id,
      i.quantity,
      i.price,
      i.quantity * i.price AS line_total
    FROM orders o,
    LATERAL jsonb_to_recordset(o.line_items)
      AS i(product_id INT, quantity INT, price NUMERIC)
  $$,
  schedule => '5s'
);
```

When a new order is inserted, pg_trickle unpacks its `line_items` JSON and inserts the resulting rows. When an order is updated (items added or removed), the old line items are deleted and the new ones are inserted.

### Unnesting Arrays

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'user_tags_flat',
  query => $$
    SELECT u.user_id, u.name, t.tag
    FROM users u,
    LATERAL unnest(u.tags) AS t(tag)
  $$,
  schedule => '10s'
);
```

Each user has an array of tags. The LATERAL `unnest()` flattens them. When a user's tag array changes, only that user's rows are re-expanded.

### Set-Returning Functions

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'date_ranges',
  query => $$
    SELECT
      e.event_id,
      e.title,
      d.day::date AS event_day
    FROM events e,
    LATERAL generate_series(e.start_date, e.end_date, '1 day') AS d(day)
  $$,
  schedule => '30s'
);
```

Each event spans a date range. The LATERAL `generate_series()` produces one row per day. When an event's dates change, only that event's rows are recalculated.

### JSON_TABLE (PostgreSQL 17+)

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'api_responses_parsed',
  query => $$
    SELECT
      r.request_id,
      r.endpoint,
      j.*
    FROM api_responses r,
    LATERAL JSON_TABLE(
      r.response_body,
      '$.results[*]'
      COLUMNS (
        item_id INT PATH '$.id',
        status TEXT PATH '$.status',
        score NUMERIC PATH '$.score'
      )
    ) AS j
  $$,
  schedule => '5s'
);
```

`JSON_TABLE` is syntactic sugar for a LATERAL join over JSON path expressions. pg_trickle handles it with the same row-scoped re-execution strategy.

---

## Performance Characteristics

The cost of a LATERAL delta is:

```
cost = |changed left rows| × avg_cost(LATERAL subquery per row)
```

This is efficient when:
- **Few left-side rows change per cycle.** 5 changed orders → 5 LATERAL re-executions.
- **The LATERAL subquery is fast per row.** `unnest()` and `json_to_recordset()` are microsecond-level.

It's expensive when:
- **Many left-side rows change.** 10,000 changed orders → 10,000 re-executions. At this point, FULL refresh may be faster.
- **The LATERAL subquery is expensive.** If the subquery hits a large table or calls a slow function, the per-row cost multiplies quickly.

| Scenario | Left changes | Per-row LATERAL cost | Total delta cost |
|----------|-------------|---------------------|------------------|
| JSON unpacking, 5 new orders | 5 | 0.1ms | 0.5ms |
| Array unnest, 50 user updates | 50 | 0.05ms | 2.5ms |
| generate_series, 10 events | 10 | 0.2ms | 2ms |
| Expensive function, 1000 changes | 1000 | 5ms | 5,000ms |

For the last case, pg_trickle's AUTO mode will detect the high cost and switch to FULL refresh.

---

## LATERAL with Aggregation

A common pattern: LATERAL to expand, then GROUP BY to aggregate.

```sql
SELECT pgtrickle.create_stream_table(
  name  => 'order_item_stats',
  query => $$
    SELECT
      o.customer_id,
      COUNT(i.product_id) AS total_items,
      SUM(i.quantity * i.price) AS total_value
    FROM orders o,
    LATERAL jsonb_to_recordset(o.line_items)
      AS i(product_id INT, quantity INT, price NUMERIC)
    GROUP BY o.customer_id
  $$,
  schedule => '5s'
);
```

pg_trickle processes this as a two-stage pipeline:

1. **LATERAL stage:** Row-scoped re-execution for changed orders. Produces the expanded line items.
2. **Aggregate stage:** Algebraic delta on the aggregates. The expanded items feed into the `SUM`/`COUNT` delta rules.

Only the groups (customers) whose orders changed are updated. The algebraic delta on the aggregate side is O(affected groups), not O(all rows).

---

## Limitations

**Volatile functions in LATERAL:** If the LATERAL subquery calls a VOLATILE function (e.g., `random()`, `clock_timestamp()`), the result is non-deterministic. pg_trickle rejects VOLATILE functions in DIFFERENTIAL and IMMEDIATE mode queries:

```
ERROR: VOLATILE function random() cannot be used in DIFFERENTIAL mode
HINT: Use FULL refresh mode or replace with a STABLE/IMMUTABLE alternative
```

**Large outer tables with small LATERAL results:** If the outer table has 10 million rows and the LATERAL produces 1 row per outer row (a correlated scalar subquery in disguise), you might be better off rewriting as a regular join.

**LATERAL referencing multiple outer tables:** In multi-way joins, LATERAL can reference columns from any previously joined table. pg_trickle supports this, but the re-execution scope is determined by the outermost changed table. If the outermost table changes, all downstream LATERAL re-executions trigger.

---

## Summary

LATERAL joins are maintained incrementally via row-scoped re-execution. When a left-side row changes, the LATERAL subquery is re-run for that row. The cost is proportional to the number of changed left-side rows times the per-row LATERAL cost.

This covers `JSON_TABLE`, `unnest()`, `generate_series()`, `jsonb_to_recordset()`, and any other set-returning function used with LATERAL.

The performance sweet spot: few changed rows, cheap LATERAL subquery. The failure mode: many changed rows or expensive subquery. AUTO mode handles the transition gracefully.
