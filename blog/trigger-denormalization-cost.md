# The Hidden Cost of Trigger-Based Denormalization

## How hand-rolled sync logic breaks — and what to do about it

---

You have a `products` table, a `categories` table, a `suppliers` table, and an `inventory` table. Your application needs to display a product listing page that combines fields from all four. You need it fast — 2ms, not 200ms. So you create a denormalized `product_listing` table with all the fields pre-joined, and you write triggers to keep it in sync.

That was eighteen months ago.

Today your denormalized table has seven triggers across four source tables, each written by a different engineer in a different sprint. Two of them have subtle race conditions. One of them has a performance regression that nobody traced back to the trigger. The data drifts by 0.3% every week in ways you can't explain. Your oncall rotation includes a step called "rerun the denorm sync script."

This is not bad engineering. This is the predictable outcome of building derived data maintenance with the wrong abstraction.

---

## Why Triggers Seem Like the Right Answer

Triggers are PostgreSQL's built-in mechanism for reacting to changes. They fire on INSERT, UPDATE, DELETE. They run in the same transaction as the change. They're fast. They're simple for the first case.

The first trigger you write is usually clean:

```sql
CREATE OR REPLACE FUNCTION sync_product_listing()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO product_listing (
    product_id, product_name, category_name, supplier_name, stock_qty
  )
  SELECT
    p.id, p.name, c.name, s.name, i.qty
  FROM products p
  JOIN categories c ON c.id = p.category_id
  JOIN suppliers s ON s.id = p.supplier_id
  LEFT JOIN inventory i ON i.product_id = p.id
  WHERE p.id = NEW.id
  ON CONFLICT (product_id) DO UPDATE SET
    product_name  = EXCLUDED.product_name,
    category_name = EXCLUDED.category_name,
    supplier_name = EXCLUDED.supplier_name,
    stock_qty     = EXCLUDED.stock_qty;
  RETURN NEW;
END;
$$;

CREATE TRIGGER trg_products_to_listing
AFTER INSERT OR UPDATE ON products
FOR EACH ROW EXECUTE FUNCTION sync_product_listing();
```

Works perfectly. Fast, transactional, no external jobs. Ship it.

Then requirements change.

---

## The Cascade of Complexity

**Month 1:** You add a trigger for `categories` changes. When a category is renamed, every product in that category needs its `category_name` updated. You write a statement-level trigger that issues a bulk UPDATE.

**Month 3:** A supplier changes their name. Same pattern — bulk UPDATE for all affected products. A third engineer adds this trigger. The bulk UPDATE takes 3 seconds on a busy day because there are 40,000 products from that supplier. The original product INSERT trigger is now occasionally blocked waiting for a row lock.

**Month 5:** Inventory levels change thousands of times a day. Someone adds a trigger. Performance immediately degrades — `product_listing` is being updated on every inventory change, and inventory changes very often. The trigger is rewritten to only update if `stock_qty` changes by more than 10 units. This works until a product goes from 5 units to 0 units in increments of 1, and the `product_listing` never reaches `stock_qty = 0`.

**Month 7:** You discover that batch imports of products (10,000 rows via `COPY`) are slow because the row-level trigger fires 10,000 times. Someone adds a `WHEN (tg_op = 'INSERT')` condition and a separate batch-import procedure that bypasses the trigger. Now there are two code paths, and they've drifted.

**Month 11:** The `categories` table gains a `parent_category_id` column. The category path for display is now `Electronics > Phones > Accessories`. Updating the trigger to compute the category path inline is painful. A `category_path` helper function is added. It does a recursive CTE on every trigger invocation.

**Month 15:** A data audit finds 847 rows in `product_listing` where `category_name` doesn't match `categories.name`. Investigation reveals a bug in the `categories` update trigger that was introduced in Month 1 and fixed in Month 8, but the 847 rows changed during the window when the bug existed. A one-time repair script is written and added to runbooks.

---

## The Root Cause

Triggers are imperative. You describe *how* to update the denormalized table, not *what* it should contain.

This distinction matters because "how" changes as the query changes. Every time the definition of `product_listing` evolves — new join, new column, changed logic — every trigger that touches `product_listing` must be reviewed and potentially updated. Correctness requires that all triggers stay synchronized with each other and with the current definition.

In practice, they don't. Engineers add columns to the denormalized table and forget to add them to the triggers. The query logic in the trigger diverges from the query logic in the application. The triggers were written for row-level operations but the correct logic requires seeing multiple rows at once.

There are four specific failure modes that appear in almost every trigger-based denormalization system at sufficient age:

### Failure Mode 1: The Blind UPDATE

The typical pattern is:

```sql
-- In the categories trigger
UPDATE product_listing
SET category_name = NEW.name
WHERE category_id = NEW.id;
```

This works when `product_listing.category_name` should always equal `categories.name`. It breaks the instant `category_name` in `product_listing` is derived from anything other than `categories.name` directly — say, a concatenation with the parent category. Now you need the full query logic in the trigger, but the trigger was written before the concatenation existed.

### Failure Mode 2: Statement vs. Row Triggers

Row-level triggers fire once per affected row. Statement-level triggers fire once per DML statement. They have different semantics, especially for bulk operations, and most developers don't think about which they need until they hit a correctness bug.

If you update 1,000 product records in a single UPDATE statement:
- A `FOR EACH ROW` trigger fires 1,000 times, each with access to the individual row change. Safe but slow for bulk operations.
- A `FOR EACH STATEMENT` trigger fires once, with access to the set of changes via transition tables (`OLD TABLE`, `NEW TABLE`). Fast but requires set-based logic that most people get wrong.

Teams typically start with `FOR EACH ROW` (easier to write), discover performance problems, try to rewrite as `FOR EACH STATEMENT`, introduce bugs in the set logic, and revert. Or they give up and schedule a batch job.

### Failure Mode 3: The Invisible Delete

INSERTs and UPDATEs are straightforward to handle. Deletes are harder to get right.

When a supplier is deleted, you need to either delete the `product_listing` rows for their products or set the `supplier_name` to NULL. But the trigger fires *after* the row is deleted, so you can't join back to `suppliers` to find which products were affected. You need to store the old supplier ID somewhere before it disappears.

```sql
-- DELETE trigger: OLD has the deleted row, but products still have supplier_id
-- This is fine. But what if the product itself is cascade-deleted too?
-- Now your trigger runs on BOTH tables and order matters.
```

Cascade deletions, deferred constraints, and foreign key actions interact with trigger ordering in ways that are documented but rarely read. The failure mode is usually a FOREIGN KEY violation or an orphaned denormalized row.

### Failure Mode 4: The Multi-Row Race

Consider two concurrent transactions:
- Transaction A updates product 1's category from X to Y
- Transaction B updates category Y's name

Both trigger the denorm update for product 1. Which one wins? In `READ COMMITTED` isolation (the PostgreSQL default), the answer is "whichever commits last," but the trigger logic in each transaction sees a different snapshot of the data. You can end up with `product_listing.category_name` reflecting a state that never actually existed — Y's new name applied to a product that still had category X at the time of the name change.

This is a race condition. It's rare, intermittent, and produces data that's almost right — the hardest kind of bug to find.

---

## What IVM Does Differently

The key difference between trigger-based denormalization and incremental view maintenance is that IVM is *declarative*. You declare what the result should be. The engine figures out how to maintain it.

```sql
SELECT pgtrickle.create_stream_table(
  name         => 'product_listing',
  query        => $$
    SELECT
      p.id          AS product_id,
      p.name        AS product_name,
      CONCAT(pc.name, ' > ', c.name) AS category_path,
      s.name        AS supplier_name,
      s.lead_days,
      i.qty         AS stock_qty,
      i.updated_at  AS inventory_updated_at
    FROM products p
    JOIN categories c ON c.id = p.category_id
    JOIN categories pc ON pc.id = c.parent_id
    JOIN suppliers s ON s.id = p.supplier_id
    LEFT JOIN inventory i ON i.product_id = p.id
    WHERE p.published = true
  $$,
  schedule     => '5 seconds',
  refresh_mode => 'DIFFERENTIAL'
);
```

When `categories` is renamed, when a supplier is deleted, when inventory changes — pg_trickle's DVM engine computes the correct delta and applies it. The query definition is the ground truth. There are no separate trigger functions to keep in sync with it.

The query can be as complex as you need — multiple joins, computed columns, filters, aggregates. Change it by calling `pgtrickle.alter_stream_table()` with a new query. The engine rebuilds the stream table and starts maintaining the new definition.

---

## The Concurrency Story

IVM handles the multi-row race correctly by design. The CDC triggers capture changes as part of the source transaction, but the delta is applied by the background worker in a subsequent transaction that sees a consistent snapshot. The worker never applies a partial or inconsistent state.

The correctness guarantee is: `product_listing` is always a consistent snapshot of the query result as of some point in time. It may be up to `schedule` seconds behind, but it's never internally inconsistent.

Trigger-based denormalization doesn't offer this guarantee. Each trigger update is a separate, independently committed transaction. Between the `products` trigger and the `categories` trigger for a related change, there's a window where `product_listing` reflects one change but not the other.

---

## Performance: Triggers vs. IVM

For low-volume write workloads, row-level triggers are fast — they add microseconds to each DML. The overhead is per-row and constant.

For high-volume write workloads, triggers have two problems:

1. **Lock contention**: Every trigger that writes to `product_listing` acquires a row lock on the destination. High-concurrency writes create a serialization point at the denormalized table.

2. **Write amplification**: One change to `categories` might update thousands of rows in `product_listing`. This is hidden write amplification — the DML statement returns fast, but you've actually done an O(affected rows) write behind the scenes.

pg_trickle batches these changes. A stream table refresh cycle applies one consistent delta per affected group — one UPDATE per changed aggregate, one INSERT/DELETE per changed join result. The background worker runs at a configurable cadence, so high-frequency source writes are amortized.

For inventory-level changes (the "update thousands of times a day" problem from month 5 above), pg_trickle coalesces multiple changes to the same product within a refresh cycle. If a product's inventory changes 20 times in 5 seconds, only the net change is applied to `product_listing` in that cycle.

---

## Migration Path

If you have an existing trigger-based denormalization setup, the migration is:

1. Create the stream table with `refresh_mode => 'FULL'` and verify it produces the correct output.
2. Switch to `refresh_mode => 'DIFFERENTIAL'` once the query is verified.
3. Drop the manual triggers.
4. Drop the maintenance scripts from your runbook.

The existing triggers and the stream table can coexist during migration — `product_listing` is just a table, and pg_trickle's refreshes are transactional. As long as you reconcile the two update paths before going live, the migration is safe.

---

## What You Keep

One thing trigger-based denormalization does that pg_trickle doesn't: immediate consistency. A trigger fires in the same transaction as the change. The denormalized table is updated before the original transaction commits.

If your application relies on reading the denormalized table immediately after writing to a source table *in the same transaction*, you need immediate consistency. This is an uncommon pattern but it exists.

For everything else — dashboards, search corpora, reporting tables, user-facing aggregates — the 5-second staleness window of a scheduled IVM refresh is acceptable and the operational simplicity is worth it.

The trigger function graveyard you've been maintaining? Replace it with a SQL query. One source of truth for what the table contains.

---

*pg_trickle is an open-source PostgreSQL extension for incremental view maintenance. Source and documentation at [github.com/grove/pg-trickle](https://github.com/grove/pg-trickle).*
