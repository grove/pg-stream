[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# IMMEDIATE Mode: When "Good Enough Freshness" Isn't Good Enough

## Synchronous IVM inside your transaction

---

Most of the conversation about incremental view maintenance focuses on latency: how fast can you refresh? A second? 500 milliseconds? 100?

But there's a class of problems where any refresh lag is too much. If you're computing an account balance, a running inventory count, or a double-entry bookkeeping ledger, reading the stream table and getting a result that's 200ms behind the write that just happened in the same request — that's a bug.

This is what `refresh_mode => 'IMMEDIATE'` does: it applies the delta inside the same transaction that caused the change. No background worker. No schedule. No lag. When `INSERT INTO orders (...)` commits, the stream table already reflects that order.

---

## How DIFFERENTIAL and IMMEDIATE Differ

DIFFERENTIAL mode (the default) works like this:

```
Transaction 1: INSERT INTO orders (...) → trigger fires → change buffer row written
Transaction 1: COMMIT

  ... 1–5 seconds later ...

Background worker: drain change buffer → compute delta → MERGE into stream table
```

There's a window between commit and refresh. Your application inserts an order and then immediately queries the stream table — the order isn't there yet. For dashboards that refresh every few seconds, this is fine. For a checkout flow that needs to show the updated total on the next page load, it's not.

IMMEDIATE mode eliminates the window:

```
Transaction 1: INSERT INTO orders (...) → trigger fires → delta computed inline → MERGE applied
Transaction 1: COMMIT
```

The delta computation runs as part of the trigger execution. By the time the transaction commits, the stream table is updated. There's no window. The next `SELECT` from the stream table — even in the same transaction — sees the new data.

---

## The Trade-Off

IMMEDIATE mode isn't free. The delta computation happens on the write path. Every INSERT, UPDATE, or DELETE on a source table does more work — the trigger has to compute the delta and apply it before control returns to your application.

For a simple aggregation over one table, this overhead is small — a few hundred microseconds per row. For a multi-table JOIN with several aggregation groups, it can add single-digit milliseconds per write.

The decision matrix:

| Situation | Mode |
|---|---|
| Dashboard queries, analytics, reporting | DIFFERENTIAL (1–5s schedule) |
| Read-your-writes required, low write throughput | IMMEDIATE |
| High write throughput, some lag acceptable | DIFFERENTIAL |
| Financial calculations, inventory, bookkeeping | IMMEDIATE |
| Write-heavy event logging | DIFFERENTIAL |

If you're unsure, start with DIFFERENTIAL. Switch to IMMEDIATE when you hit a case where read-your-writes consistency matters.

---

## A Concrete Example: Account Balances

Double-entry bookkeeping is the textbook use case. Every financial transaction creates two journal entries — a debit and a credit. The account balance is the sum of all entries for that account.

```sql
-- Journal entries table
CREATE TABLE journal_entries (
    id          bigserial PRIMARY KEY,
    account_id  bigint NOT NULL REFERENCES accounts(id),
    entry_type  text NOT NULL CHECK (entry_type IN ('debit', 'credit')),
    amount      numeric(15,2) NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- Account balance: always consistent, always current
SELECT pgtrickle.create_stream_table(
    'account_balances',
    $$SELECT
        account_id,
        SUM(CASE WHEN entry_type = 'credit' THEN amount ELSE -amount END) AS balance,
        COUNT(*) AS entry_count,
        MAX(created_at) AS last_entry_at
      FROM journal_entries
      GROUP BY account_id$$,
    refresh_mode => 'IMMEDIATE'
);
```

Now when your application writes a journal entry and reads the balance in the same transaction:

```sql
BEGIN;
INSERT INTO journal_entries (account_id, entry_type, amount)
VALUES (42, 'credit', 150.00);

-- This returns the updated balance, including the $150 credit
SELECT balance FROM account_balances WHERE account_id = 42;
COMMIT;
```

No race condition. No eventual consistency. No background worker to wait for.

---

## Inventory Tracking

Same pattern, different domain. An e-commerce warehouse tracks stock with an events table:

```sql
CREATE TABLE stock_events (
    id          bigserial PRIMARY KEY,
    sku         text NOT NULL,
    warehouse   text NOT NULL,
    quantity    int NOT NULL,  -- positive = received, negative = shipped
    event_type  text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

SELECT pgtrickle.create_stream_table(
    'inventory_levels',
    $$SELECT
        sku,
        warehouse,
        SUM(quantity) AS on_hand,
        SUM(CASE WHEN quantity < 0 THEN -quantity ELSE 0 END) AS total_shipped,
        SUM(CASE WHEN quantity > 0 THEN quantity ELSE 0 END) AS total_received
      FROM stock_events
      GROUP BY sku, warehouse$$,
    refresh_mode => 'IMMEDIATE'
);
```

When the checkout service writes a shipment event, `inventory_levels.on_hand` is decremented in the same transaction. The next availability check — even in the same request — sees the correct count. No overselling because a background worker hadn't caught up.

---

## What IMMEDIATE Mode Restricts

Not every query supports IMMEDIATE mode. The restriction exists because the trigger must compute the delta synchronously, and some operators require access to data that isn't available in the trigger context.

Queries that work in IMMEDIATE mode:
- Single-table or multi-table JOINs with aggregates (SUM, COUNT, AVG, MIN, MAX)
- WHERE filters on source columns
- CASE expressions in aggregates
- GROUP BY on any column or expression

Queries that require DIFFERENTIAL or FULL mode:
- Window functions (RANK, ROW_NUMBER, LAG, LEAD) — these need the full partition
- HAVING clauses that reference aggregate results from other groups
- Queries referencing `now()` or other volatile functions in the defining query
- DISTINCT without GROUP BY
- LIMIT / OFFSET

pg_trickle tells you if your query isn't compatible:

```sql
SELECT pgtrickle.create_stream_table(
    'bad_example',
    $$SELECT *, ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn
      FROM employees$$,
    refresh_mode => 'IMMEDIATE'
);
-- ERROR: query uses window functions, which are not supported in IMMEDIATE mode.
-- HINT: Use refresh_mode => 'DIFFERENTIAL' or 'FULL' instead.
```

---

## Mixing Modes in a DAG

A common pattern is to use IMMEDIATE for the leaf stream tables that face the application, and DIFFERENTIAL for upstream aggregation layers:

```sql
-- Silver layer: cleaned, enriched orders (DIFFERENTIAL, 2s schedule)
SELECT pgtrickle.create_stream_table(
    'orders_enriched',
    $$SELECT o.id, o.amount, o.created_at,
            c.name, c.region, c.tier
      FROM orders o
      JOIN customers c ON c.id = o.customer_id$$,
    schedule     => '2s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Gold layer: per-customer balance (IMMEDIATE, no schedule needed)
SELECT pgtrickle.create_stream_table(
    'customer_totals',
    $$SELECT customer_id, SUM(amount) AS total_spend, COUNT(*) AS order_count
      FROM orders
      GROUP BY customer_id$$,
    refresh_mode => 'IMMEDIATE'
);
```

The dashboard reads from `orders_enriched` (2-second lag is fine). The checkout flow reads from `customer_totals` (zero lag, because the application just wrote the order).

---

## When to Move From IMMEDIATE to DIFFERENTIAL

If your write throughput grows and the per-write overhead of IMMEDIATE mode starts showing up in your P99 latency, switch:

```sql
SELECT pgtrickle.alter_stream_table(
    'account_balances',
    refresh_mode => 'DIFFERENTIAL',
    schedule     => '1 second'
);
```

One-second staleness is usually acceptable for everything except the strictest consistency requirements. And for those, you can keep the critical tables on IMMEDIATE while moving the less critical ones to DIFFERENTIAL.

The ALTER is online. No downtime. The stream table continues serving reads during the switch.
