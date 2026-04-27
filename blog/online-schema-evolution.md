# How to Change a Stream Table Query Without Taking It Offline

## Online schema evolution for incremental views

---

Your stream table has been running in production for three months. The business wants a new column. Or a different aggregation. Or the source table schema changed and your JOIN needs updating.

With a materialized view, you'd `DROP` the old one and `CREATE` a new one. During the window between drop and create, any query against the view fails.

With a naive stream table approach, you'd drop the stream table, losing the CDC triggers, change buffers, and refresh history, then recreate it from scratch. During the initial full refresh — which can take minutes for large tables — the stream table either doesn't exist or contains stale data.

pg_trickle's `ALTER STREAM TABLE ... QUERY` does this online. The stream table stays queryable throughout the migration.

---

## The Simple Case: Adding a Column

```sql
-- Original stream table
SELECT pgtrickle.create_stream_table(
    'order_summary',
    $$SELECT customer_id, SUM(amount) AS total, COUNT(*) AS cnt
      FROM orders GROUP BY customer_id$$,
    schedule => '3s', refresh_mode => 'DIFFERENTIAL'
);

-- Later: add average order value
SELECT pgtrickle.alter_stream_table(
    'order_summary',
    query => $$SELECT customer_id,
                      SUM(amount) AS total,
                      COUNT(*) AS cnt,
                      AVG(amount) AS avg_amount
               FROM orders GROUP BY customer_id$$
);
```

What happens internally:

1. pg_trickle parses the new query and builds a new operator tree.
2. It compares the new schema to the current storage table schema.
3. It adds the `avg_amount` column to the storage table (an `ALTER TABLE ADD COLUMN` — non-blocking in PostgreSQL).
4. It triggers a full refresh to populate the new column.
5. It updates the CDC triggers if the source table set changed.
6. It updates the catalog entry with the new query and operator tree.

During steps 1–6, the old data in the stream table is still queryable. The `avg_amount` column is NULL until the full refresh completes. After the refresh, all rows have the correct value. The switch is atomic — the refresh runs in a single transaction.

---

## Changing the Aggregation

More complex changes — altering the GROUP BY, changing JOINs, or restructuring the query — trigger a full schema migration:

```sql
-- Change grouping from per-customer to per-region
SELECT pgtrickle.alter_stream_table(
    'order_summary',
    query => $$SELECT c.region,
                      SUM(o.amount) AS total,
                      COUNT(*) AS cnt
               FROM orders o
               JOIN customers c ON c.id = o.customer_id
               GROUP BY c.region$$
);
```

This is a bigger change: the GROUP BY columns are different, a new source table (`customers`) is introduced, and the row identity changes. pg_trickle handles this by:

1. Creating a new storage table with the new schema.
2. Running a full refresh to populate it.
3. Atomically swapping the old storage table for the new one (renaming under an exclusive lock held for microseconds).
4. Dropping the old storage table.
5. Updating CDC triggers to include `customers`.

During the full refresh (step 2), the old stream table is still serving reads. The swap in step 3 is nearly instantaneous. From the application's perspective, the stream table name never changes — it just starts returning rows with the new schema.

---

## Changing the Schedule or Refresh Mode

These are lighter operations that don't require a schema migration:

```sql
-- Speed up the refresh cycle
SELECT pgtrickle.alter_stream_table('order_summary', schedule => '1 second');

-- Switch from DIFFERENTIAL to IMMEDIATE
SELECT pgtrickle.alter_stream_table('order_summary', refresh_mode => 'IMMEDIATE');
```

Schedule changes take effect on the next scheduler cycle. Refresh mode changes may trigger a one-time full refresh to ensure the stream table is consistent with the new mode's requirements.

---

## What About Downstream Dependencies?

If `order_summary` has downstream stream tables (other stream tables that reference it), the schema migration cascades correctly:

```sql
-- gold_dashboard depends on order_summary
SELECT pgtrickle.create_stream_table(
    'gold_dashboard',
    $$SELECT region, SUM(total) AS grand_total
      FROM order_summary GROUP BY region$$,
    schedule => '5s', refresh_mode => 'DIFFERENTIAL'
);
```

When you alter `order_summary`'s query, pg_trickle checks whether the schema change is compatible with downstream dependents. If the columns referenced by `gold_dashboard` still exist with compatible types, the downstream stream table continues working. If not, pg_trickle raises an error and tells you which downstream tables need updating:

```
ERROR: altering query for "order_summary" would break dependent stream table
       "gold_dashboard": column "total" would be removed.
HINT:  Drop or alter "gold_dashboard" first, or include column "total" in the new query.
```

No silent breakage. No orphaned stream tables pointing at columns that don't exist.

---

## Suspending and Resuming

If you need to make multiple changes atomically — alter the query, update the schedule, and adjust the refresh mode — you can suspend the stream table first:

```sql
-- Pause refresh cycles
SELECT pgtrickle.alter_stream_table('order_summary', status => 'SUSPENDED');

-- Make changes
SELECT pgtrickle.alter_stream_table('order_summary',
    query        => $$ ... new query ... $$,
    schedule     => '2s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Resume
SELECT pgtrickle.resume_stream_table('order_summary');
```

While suspended, the change buffers continue accumulating (CDC triggers still fire). When you resume, the next refresh cycle drains all buffered changes. No data is lost during the suspension.

---

## The Export/Import Workflow

For changes that require careful validation, you can export the stream table's definition, edit it, and reimport:

```sql
-- Export current definition as a SQL statement
SELECT pgtrickle.export_definition('order_summary');
```

This returns the full `create_stream_table` call with all current parameters. You can modify it in your migration script, test it in a staging environment, and apply it in production.

---

## When You Do Need Downtime

There's one scenario where online migration isn't possible: if you need to change the stream table's name. PostgreSQL table renames require an exclusive lock, and pg_trickle's CDC triggers reference the stream table by OID, not name. Renaming requires dropping and recreating.

For everything else — query changes, schedule changes, refresh mode changes, adding or removing columns, changing JOINs, changing GROUP BY — `alter_stream_table` handles it online.
