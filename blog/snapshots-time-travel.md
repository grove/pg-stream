[← Back to Blog Index](README.md)

# Snapshots: Time Travel for Stream Tables

## Bookmark, compare, rollback, and bootstrap with point-in-time captures

---

You're about to deploy a migration that changes a stream table's defining query. If something goes wrong, you'd like to compare the new result with the old one. Or roll back entirely.

pg_trickle's snapshot system lets you do this. `snapshot_stream_table()` captures the current contents of a stream table into an ordinary PostgreSQL table. `restore_from_snapshot()` puts it back. The snapshot is a regular table — you can query it, join it, diff it, export it.

---

## Taking a Snapshot

```sql
SELECT pgtrickle.snapshot_stream_table('revenue_by_region');
```

This creates a table named `pgtrickle.snapshot_revenue_by_region_<timestamp>` containing an exact copy of the current stream table contents.

You can also name the snapshot:

```sql
SELECT pgtrickle.snapshot_stream_table('revenue_by_region', 'before_migration');
```

This creates `pgtrickle.snapshot_revenue_by_region_before_migration`.

The snapshot is a plain table. It's not a stream table — it has no CDC triggers, no refresh schedule, no change buffers. It's a frozen point-in-time copy.

---

## Listing Snapshots

```sql
SELECT * FROM pgtrickle.list_snapshots();
```

```
 stream_table       | snapshot_name     | created_at          | row_count
--------------------+-------------------+---------------------+-----------
 revenue_by_region  | before_migration  | 2026-04-27 14:30:00 | 152,847
 revenue_by_region  | 20260427_143200   | 2026-04-27 14:32:00 | 152,903
 customer_metrics   | pre_reorg         | 2026-04-26 09:15:00 | 48,291
```

Or filter by stream table:

```sql
SELECT * FROM pgtrickle.list_snapshots('revenue_by_region');
```

---

## Restoring from a Snapshot

```sql
SELECT pgtrickle.restore_from_snapshot('revenue_by_region', 'before_migration');
```

This:
1. Truncates the current stream table contents.
2. Copies the snapshot data into the stream table.
3. Resets the frontier — so the next refresh reads all changes since the snapshot was taken.

After restore, the stream table resumes normal operation. The next scheduled refresh processes the delta between the snapshot state and the current source data. If the source hasn't changed much since the snapshot, this delta is small and fast.

**Warning:** Restoring a snapshot doesn't revert the stream table's defining query or configuration. If you changed the query before restoring, the snapshot data will be refreshed using the new query. If the schemas don't match, the restore fails.

---

## Use Case 1: Pre-Migration Safety Net

The most common use case. Before changing a stream table's query, take a snapshot:

```sql
-- Before migration
SELECT pgtrickle.snapshot_stream_table('order_summary', 'pre_migration_v2');

-- Apply the migration
SELECT pgtrickle.alter_stream_table(
  name  => 'order_summary',
  query => $$ ... new query ... $$
);

-- Verify results look correct
SELECT COUNT(*) FROM pgtrickle.order_summary;  -- new data
SELECT COUNT(*) FROM pgtrickle.snapshot_order_summary_pre_migration_v2;  -- old data

-- Compare
SELECT * FROM pgtrickle.order_summary
EXCEPT
SELECT * FROM pgtrickle.snapshot_order_summary_pre_migration_v2;
```

If the new query is wrong, restore:

```sql
-- Revert the query
SELECT pgtrickle.alter_stream_table(
  name  => 'order_summary',
  query => $$ ... original query ... $$
);

-- Restore the data
SELECT pgtrickle.restore_from_snapshot('order_summary', 'pre_migration_v2');
```

---

## Use Case 2: Replica Bootstrap

When you add a new PostgreSQL replica, stream tables are empty until the first FULL refresh completes. For large stream tables, that first refresh can take minutes.

Snapshots offer a faster alternative:

1. On the primary, take a snapshot: `snapshot_stream_table('big_summary')`.
2. `pg_dump` the snapshot table and restore it on the replica.
3. On the replica, restore: `restore_from_snapshot('big_summary', 'bootstrap')`.

The stream table is immediately queryable with the snapshot data. The next refresh processes only the changes since the snapshot, which is much faster than a full recomputation.

For streaming replication (not logical), this isn't necessary — the replica already has the stream table data via WAL replay. But for logical replication or fresh standby setups, it's a significant time saver.

---

## Use Case 3: Forensic Comparison

Something changed in the business metrics. Revenue dropped 15% on Tuesday. Was it a data issue or a real business event?

If you have daily snapshots, you can compare:

```sql
-- Revenue by region: today vs. Monday
SELECT
  t.region,
  t.revenue AS today,
  m.revenue AS monday,
  t.revenue - m.revenue AS delta
FROM pgtrickle.revenue_by_region t
FULL OUTER JOIN pgtrickle.snapshot_revenue_by_region_monday m
  USING (region)
ORDER BY delta;
```

This is trivial with snapshots and impossible without them (or without a separate time-series history system).

---

## Use Case 4: Test Fixtures

Snapshot production data for deterministic test environments:

```sql
-- Production
SELECT pgtrickle.snapshot_stream_table('product_search', 'test_fixture_2026q2');

-- Export
pg_dump -t pgtrickle.snapshot_product_search_test_fixture_2026q2 prod_db > fixture.sql

-- Test environment
psql test_db < fixture.sql
SELECT pgtrickle.restore_from_snapshot('product_search', 'test_fixture_2026q2');
```

Your test environment now has a known-good starting state. Tests run against it, and results are reproducible.

---

## Snapshot Storage

Snapshots are ordinary heap tables. They consume the same storage as the original stream table data — roughly the same number of pages. pg_trickle doesn't compress or deduplicate snapshots.

For a stream table with 1 million rows at 200 bytes per row:
- Stream table: ~200MB
- Each snapshot: ~200MB

Plan storage accordingly. If you take daily snapshots and retain 30 days, that's 6GB for a single stream table.

---

## Retention and Cleanup

pg_trickle doesn't automatically delete old snapshots. You manage retention explicitly:

```sql
-- Drop a specific snapshot
SELECT pgtrickle.drop_snapshot('revenue_by_region', 'before_migration');

-- Drop all snapshots older than 7 days (manual query)
SELECT pgtrickle.drop_snapshot(stream_table, snapshot_name)
FROM pgtrickle.list_snapshots()
WHERE created_at < NOW() - INTERVAL '7 days';
```

For automated retention, wrap this in a cron job or a pg_cron task:

```sql
-- pg_cron: daily cleanup of snapshots older than 30 days
SELECT cron.schedule('snapshot-cleanup', '0 3 * * *', $$
  SELECT pgtrickle.drop_snapshot(stream_table, snapshot_name)
  FROM pgtrickle.list_snapshots()
  WHERE created_at < NOW() - INTERVAL '30 days'
$$);
```

---

## Snapshots vs. pg_dump

`pg_dump` backs up the entire database (or specific tables). Snapshots capture a single stream table's state within the running database.

| | Snapshots | pg_dump |
|---|---|---|
| Scope | One stream table | Whole database or selected tables |
| Speed | Instant (copy within same DB) | Depends on DB size |
| Storage | Same database | External file |
| Restore | `restore_from_snapshot()` | `pg_restore` |
| Cross-environment | No (same database) | Yes |
| Frontier reset | Yes (automatic) | Manual (`repair_stream_table`) |

Use snapshots for operational safety (pre-migration, comparison, quick rollback). Use pg_dump for disaster recovery and cross-environment transfers.

---

## Summary

Snapshots are point-in-time copies of stream table contents, stored as ordinary tables.

- `snapshot_stream_table()` to capture.
- `restore_from_snapshot()` to roll back.
- `list_snapshots()` to inventory.
- `drop_snapshot()` to clean up.

They're useful before migrations, for bootstrapping replicas, for forensic comparison, and for test fixtures. They're cheap to take (a table copy), and they compose with standard PostgreSQL tools (pg_dump, queries, joins).

If you're running stream tables in production and you're not taking snapshots before schema changes, you're flying without a parachute. It takes one function call to put one on.
