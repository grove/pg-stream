# What Happens When You TRUNCATE a Table?

This tutorial explains what happens when a `TRUNCATE` statement hits a base table that is referenced by a stream table. Unlike INSERT, UPDATE, and DELETE — which are fully tracked by the CDC trigger — TRUNCATE is a special case that **bypasses row-level triggers entirely**. Understanding this gap is essential for operating pg_trickle correctly.

> **Prerequisite:** Read [WHAT_HAPPENS_ON_INSERT.md](WHAT_HAPPENS_ON_INSERT.md) first — it introduces the 7-phase lifecycle. This tutorial explains why TRUNCATE breaks that lifecycle and how to recover.

## Setup

Same e-commerce example used throughout the series:

```sql
CREATE TABLE orders (
    id       SERIAL PRIMARY KEY,
    customer TEXT NOT NULL,
    amount   NUMERIC(10,2) NOT NULL
);

SELECT pgtrickle.create_stream_table(
    'customer_totals',
    $$
      SELECT customer, SUM(amount) AS total, COUNT(*) AS order_count
      FROM orders GROUP BY customer
    $$,
    '1m',
    'DIFFERENTIAL'
);

-- Seed some data
INSERT INTO orders (customer, amount) VALUES
    ('alice', 50.00),
    ('alice', 30.00),
    ('bob',   75.00),
    ('bob',   25.00);
```

After the first refresh, the stream table contains:

```
customer | total  | order_count
---------|--------|------------
alice    | 80.00  | 2
bob      | 100.00 | 2
```

---

## Case 1: TRUNCATE the Base Table

```sql
TRUNCATE orders;
```

All four rows are removed instantly.

### What Happens at the Trigger Level: Nothing

PostgreSQL's `TRUNCATE` command does **not** fire row-level AFTER triggers. This is by design in PostgreSQL — TRUNCATE is a DDL-like operation that removes all rows without scanning them individually. The per-row `AFTER INSERT OR UPDATE OR DELETE` trigger that pg_trickle installs is simply never invoked.

The change buffer remains empty:

```
pgtrickle_changes.changes_16384
┌───────────┬─────────────┬────────┬──────────┬──────────┐
│ change_id │ lsn         │ action │ ...      │ ...      │
├───────────┼─────────────┼────────┼──────────┼──────────┤
│ (empty)   │             │        │          │          │
└───────────┴─────────────┴────────┴──────────┴──────────┘
```

No rows. Zero change events captured.

### What Happens at the Scheduler: Skip

On the next refresh cycle, the scheduler:
1. Checks the change buffer for rows in the LSN window
2. Finds **zero rows**
3. Classifies the refresh action as `NO_DATA`
4. Advances the data timestamp but **does not modify the stream table**

The stream table still shows the old values:

```sql
SELECT * FROM customer_totals;
 customer | total  | order_count
----------|--------|------------
 alice    | 80.00  | 2            ← STALE! orders table is empty
 bob      | 100.00 | 2            ← STALE! orders table is empty
```

**The stream table is now stale.** It reflects data that no longer exists in the base table.

### Why This Happens

The entire incremental view maintenance pipeline depends on the change buffer to know what changed. Without change events, the DVM has no deltas to apply. The stream table's reference-counted aggregates still think there are 4 orders contributing to two groups.

This is not a bug — it's a fundamental limitation of trigger-based CDC. The trigger can only fire when PostgreSQL executes individual row operations, and TRUNCATE deliberately skips per-row processing for performance.

---

## Case 2: How to Recover — Manual Refresh

The fix is straightforward. Force a manual refresh:

```sql
SELECT pgtrickle.refresh_stream_table('customer_totals');
```

This executes a **full refresh** regardless of the stream table's configured refresh mode:

1. **TRUNCATE** the stream table itself (clearing the stale data)
2. **Re-execute** the defining query: `SELECT customer, SUM(amount) AS total, COUNT(*) AS order_count FROM orders GROUP BY customer`
3. **INSERT** the results into the stream table
4. **Update the frontier** so future differential refreshes start from the current LSN

Since the `orders` table is empty, the defining query returns zero rows. The stream table becomes empty too:

```sql
SELECT pgtrickle.refresh_stream_table('customer_totals');

SELECT * FROM customer_totals;
 customer | total | order_count
----------|-------|------------
 (0 rows)                        ← correct: orders is empty
```

The stream table is now consistent again. Future INSERT/UPDATE/DELETE operations on `orders` will be captured normally by the trigger and propagated incrementally.

---

## Case 3: TRUNCATE Then INSERT (Common ETL Pattern)

A common data loading pattern is:

```sql
BEGIN;
TRUNCATE orders;
INSERT INTO orders (customer, amount) VALUES
    ('charlie', 100.00),
    ('charlie', 200.00),
    ('dave',    150.00);
COMMIT;
```

### What the Change Buffer Sees

- TRUNCATE: **0 events** (not captured)
- INSERT charlie 100.00: **1 event** (captured)
- INSERT charlie 200.00: **1 event** (captured)
- INSERT dave 150.00: **1 event** (captured)

The change buffer has 3 INSERT events — but knows nothing about the 4 rows that were removed by TRUNCATE.

### What the Scheduler Does

The scheduler sees 3 pending changes and runs a DIFFERENTIAL refresh:

```
Aggregate delta (from INSERTs only):
  Group "charlie": ins_count = 2, ins_total = 300.00
  Group "dave":    ins_count = 1, ins_total = 150.00
```

The MERGE applies these deltas to the **existing** stream table:

```sql
SELECT * FROM customer_totals;
 customer | total  | order_count
----------|--------|------------
 alice    | 80.00  | 2            ← STALE! alice has no orders
 bob      | 100.00 | 2            ← STALE! bob has no orders
 charlie  | 300.00 | 2            ← correct
 dave     | 150.00 | 1            ← correct
```

The new data (charlie, dave) is correct. But the old data (alice, bob) persists because the TRUNCATE that removed their orders was never captured. The reference counts for alice and bob were never decremented.

### How to Fix

```sql
SELECT pgtrickle.refresh_stream_table('customer_totals');

SELECT * FROM customer_totals;
 customer | total  | order_count
----------|--------|------------
 charlie  | 300.00 | 2            ← correct
 dave     | 150.00 | 1            ← correct
```

---

## Case 4: TRUNCATE a Dimension Table in a JOIN

Consider a stream table that joins two tables:

```sql
CREATE TABLE customers (
    id   SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    tier TEXT NOT NULL DEFAULT 'standard'
);

CREATE TABLE orders (
    id          SERIAL PRIMARY KEY,
    customer_id INT REFERENCES customers(id),
    amount      NUMERIC(10,2)
);

SELECT pgtrickle.create_stream_table(
    'order_details',
    $$
      SELECT c.name, c.tier, o.amount
      FROM orders o
      JOIN customers c ON o.customer_id = c.id
    $$,
    '1m',
    'DIFFERENTIAL'
);
```

Now truncate the dimension table:

```sql
TRUNCATE customers CASCADE;
```

The `CASCADE` also truncates `orders` (due to the foreign key). Neither TRUNCATE fires row-level triggers. The change buffer is empty for both tables.

The stream table continues to show all the old joined rows as if nothing happened. The only recovery is a manual refresh:

```sql
SELECT pgtrickle.refresh_stream_table('order_details');
```

---

## Case 5: FULL Mode Stream Tables Are Immune

If the stream table uses `FULL` refresh mode instead of `DIFFERENTIAL`:

```sql
SELECT pgtrickle.create_stream_table(
    'customer_totals_full',
    $$
      SELECT customer, SUM(amount) AS total, COUNT(*) AS order_count
      FROM orders GROUP BY customer
    $$,
    '1m',
    'FULL'
);
```

A FULL-mode stream table doesn't use the change buffer at all. Every refresh cycle:
1. TRUNCATEs the stream table
2. Re-executes the defining query
3. Inserts all results

So after a TRUNCATE of the base table, the **next scheduled refresh** automatically picks up the correct state — no manual intervention needed. The trade-off is that every refresh recomputes from scratch, which is more expensive for large result sets.

---

## Why PostgreSQL Doesn't Fire Row Triggers on TRUNCATE

Understanding the PostgreSQL internals helps explain why this limitation exists:

| Operation | Mechanism | Row triggers fired? |
|-----------|-----------|-------------------|
| `DELETE FROM t` | Scans and removes rows one by one | Yes — AFTER DELETE per row |
| `TRUNCATE t` | Removes all heap files and reinitializes the table storage | No — no per-row processing |
| `DELETE FROM t WHERE true` | Same as `DELETE FROM t` (full scan) | Yes — AFTER DELETE per row |

`TRUNCATE` is fundamentally different from `DELETE`. It's an O(1) operation that replaces the table's storage files, while DELETE is O(N) — scanning every row and recording each removal in WAL.

PostgreSQL does support **statement-level** `AFTER TRUNCATE` triggers:

```sql
CREATE TRIGGER after_truncate_trigger
    AFTER TRUNCATE ON orders
    FOR EACH STATEMENT
    EXECUTE FUNCTION some_function();
```

However, statement-level TRUNCATE triggers:
- Do **not** receive OLD row data (there's no `OLD` record)
- Cannot enumerate which rows were removed
- Only know that a TRUNCATE happened on a specific table

This means a TRUNCATE trigger could detect the event but cannot generate the per-row DELETE events that the DVM pipeline needs.

---

## Alternative: DELETE FROM Instead of TRUNCATE

If you need the stream table to stay consistent without manual intervention, use `DELETE FROM` instead of `TRUNCATE`:

```sql
-- Instead of: TRUNCATE orders;
DELETE FROM orders;
```

This is slower (O(N) vs O(1)) but fires the row-level DELETE trigger for every row. The change buffer captures all removals, and the next differential refresh correctly decrements all reference counts, removing groups whose count reaches zero.

| Approach | Speed | Stream table consistent? |
|----------|-------|------------------------|
| `TRUNCATE orders` | O(1) — instant | No — requires manual refresh |
| `DELETE FROM orders` | O(N) — scans all rows | Yes — triggers fire for each row |
| `TRUNCATE` + manual refresh | O(1) + O(query) | Yes — after manual refresh |

For tables with millions of rows, `DELETE FROM` can be slow and generate significant WAL. In those cases, TRUNCATE followed by a manual refresh is often the pragmatic choice.

---

## Best Practices

### 1. Always Refresh After TRUNCATE

Make it a habit: if you TRUNCATE a base table, immediately refresh all dependent stream tables:

```sql
TRUNCATE orders;
SELECT pgtrickle.refresh_stream_table('customer_totals');
```

### 2. Use DELETE FROM for Small Tables

For tables with fewer than ~100K rows, `DELETE FROM` is fast enough and keeps everything consistent automatically.

### 3. Wrap TRUNCATE + Refresh in a Function

For ETL pipelines, create a helper:

```sql
CREATE OR REPLACE FUNCTION reload_orders(data jsonb) RETURNS void AS $$
BEGIN
    TRUNCATE orders;
    INSERT INTO orders SELECT * FROM jsonb_populate_recordset(null::orders, data);
    PERFORM pgtrickle.refresh_stream_table('customer_totals');
END;
$$ LANGUAGE plpgsql;
```

### 4. Consider FULL Mode for ETL-Heavy Tables

If a table is routinely truncated and reloaded, FULL refresh mode may be simpler than DIFFERENTIAL — it naturally handles TRUNCATE because it recomputes from scratch every cycle.

### 5. Monitor for Staleness

pg_trickle emits monitoring alerts when a stream table's data deviates from expected freshness. After a TRUNCATE, the `data_timestamp` still advances (since the scheduler sees no pending changes), but the actual data is stale. The most reliable detection is comparing the stream table results against a fresh query:

```sql
-- Quick consistency check
SELECT count(*) FROM customer_totals;  -- still shows rows?
SELECT count(*) FROM orders;            -- should be 0 after TRUNCATE
```

---

## How TRUNCATE Compares to Other Operations

| Aspect | INSERT | UPDATE | DELETE | TRUNCATE |
|--------|--------|--------|--------|----------|
| **Trigger fires?** | Yes (per row) | Yes (per row) | Yes (per row) | No |
| **Change buffer** | 1 row per INSERT | 1 row per UPDATE | 1 row per DELETE | Empty |
| **Stream table updated?** | Yes (next refresh) | Yes (next refresh) | Yes (next refresh) | No — stays stale |
| **Recovery** | Automatic | Automatic | Automatic | Manual refresh required |
| **FULL mode affected?** | N/A (recomputes) | N/A (recomputes) | N/A (recomputes) | N/A (recomputes) |
| **Speed** | O(1) per row | O(1) per row | O(1) per row | O(1) total |

---

## Summary

TRUNCATE is the one common DML-like operation that falls outside pg_trickle's automatic change tracking. The trigger-based CDC architecture captures INSERT, UPDATE, and DELETE perfectly — but TRUNCATE bypasses row-level triggers by design.

The key takeaways:

1. **TRUNCATE does not fire row-level triggers** — the change buffer stays empty
2. **The stream table becomes stale** — showing data that no longer exists in the base table
3. **Manual refresh fixes it** — `SELECT pgtrickle.refresh_stream_table('name')` recomputes from scratch
4. **FULL mode is immune** — every refresh recomputes regardless of change tracking
5. **`DELETE FROM` is the trigger-safe alternative** — slower but keeps everything consistent automatically
6. **After TRUNCATE, always refresh** — make this a standard part of your ETL workflow

---

## Next in This Series

- **[What Happens When You INSERT a Row?](WHAT_HAPPENS_ON_INSERT.md)** — The full 7-phase lifecycle (start here if you haven't already)
- **[What Happens When You UPDATE a Row?](WHAT_HAPPENS_ON_UPDATE.md)** — D+I split, group key changes, net-effect for multiple UPDATEs
- **[What Happens When You DELETE a Row?](WHAT_HAPPENS_ON_DELETE.md)** — Reference counting, group deletion, INSERT+DELETE cancellation
