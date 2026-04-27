# Backup and Restore for Stream Tables

## pg_dump, PITR, and what happens when you restore a database with active stream tables

---

Stream tables are PostgreSQL tables. They have OIDs, they're in the catalog, they have indexes. `pg_dump` includes them.

But stream tables also have associated infrastructure: CDC triggers on source tables, change buffer tables in `pgtrickle_changes`, catalog entries in `pgtrickle.pgt_stream_tables`, and internal state (frontiers, refresh history, operator trees).

If you restore a `pg_dump` without understanding how these pieces interact, you can end up with stream tables that look correct but don't refresh, or CDC triggers that are missing, or change buffers that contain stale data.

This post explains what to do.

---

## What pg_dump Captures

`pg_dump` captures:

| Component | Included in pg_dump? | Notes |
|---|---|---|
| Stream table (storage table) | ✅ | Regular table, fully dumped |
| Stream table data | ✅ | Snapshot at dump time |
| pgtrickle catalog tables | ✅ | pgt_stream_tables, pgt_dependencies, etc. |
| CDC triggers on source tables | ✅ | Part of the source table definition |
| Change buffer tables (pgtrickle_changes.*) | ✅ | But data may be stale or irrelevant after restore |
| Extension registration | ✅ | `CREATE EXTENSION pg_trickle` |
| Background worker state | ❌ | In-memory, not persisted |
| Shared memory state (frontiers) | ❌ | Rebuilt on startup |

---

## The Simple Case: pg_dump + pg_restore

```bash
# Dump
pg_dump -Fc mydb > mydb.dump

# Restore to a new database
createdb mydb_restored
pg_restore -d mydb_restored mydb.dump
```

After restore:

1. **Stream table data is present** — but it's a snapshot from dump time. Any changes to source tables after the dump are not reflected.
2. **CDC triggers are present** — they'll start capturing changes as soon as the source tables are modified.
3. **Change buffers may contain stale data** — rows from before the dump that were never processed.
4. **The scheduler starts automatically** — if `pg_trickle.enabled = on`, the background worker picks up the stream tables and starts refreshing.

### The Problem: Stale Change Buffers

The change buffers captured by pg_dump contain changes that were pending at dump time. After restore, the scheduler tries to process these changes — but the stream table already has the correct data (it was dumped with its data). Processing stale change buffer rows can cause double-counting.

### The Fix: Repair After Restore

```sql
-- After restoring from pg_dump, repair all stream tables
SELECT pgtrickle.repair_stream_table(pgt_name)
FROM pgtrickle.pgt_stream_tables;
```

`repair_stream_table` does:
1. Truncates the change buffer for the stream table's sources.
2. Resets the frontier to the current state.
3. Runs a full refresh to ensure the stream table matches the current source data.
4. Re-registers CDC triggers if any are missing.

After repair, the stream table is consistent with the current source data and ready for incremental maintenance.

---

## Point-in-Time Recovery (PITR)

PITR recovers the entire database to a specific point in time using WAL archives. Everything is consistent — source tables, stream tables, change buffers, catalog entries — because the recovery applies the WAL up to the target time.

```bash
# In recovery.conf (or postgresql.conf for PG 12+)
restore_command = 'cp /archive/%f %p'
recovery_target_time = '2026-04-27 10:00:00'
```

After PITR recovery:

1. **Everything is consistent at the recovery point.** Stream tables reflect the state of source tables at that exact time.
2. **The scheduler resumes from the frontier at the recovery point.** It won't try to process changes from after the recovery point (they don't exist).
3. **No repair needed** — the WAL recovery handles all the state consistently.

PITR is the cleanest restore option for pg_trickle. If you have WAL archiving set up (and you should), this is the recommended recovery method.

---

## Selective Restore (Restoring Individual Tables)

Sometimes you need to restore a single table, not the entire database. This is trickier with stream tables.

### Restoring a source table

If you restore a source table (e.g., `orders`) from a backup:

```bash
pg_restore -d mydb -t orders mydb.dump
```

The stream tables that depend on `orders` are now inconsistent — they reflect the old state of `orders`, but `orders` has been restored to a different state.

Fix:

```sql
-- Full refresh all stream tables that depend on 'orders'
SELECT pgtrickle.refresh_stream_table(st.pgt_name, force_mode => 'FULL')
FROM pgtrickle.pgt_stream_tables st
JOIN pgtrickle.pgt_dependencies d ON d.pgt_id = st.pgt_id
WHERE d.source_table = 'public.orders';
```

### Restoring a stream table

If you restore a stream table itself:

```bash
pg_restore -d mydb -t orders_summary mydb.dump
```

The stream table now has data from the dump time, but the change buffer and frontier are from the current state. This is inconsistent.

Fix:

```sql
SELECT pgtrickle.repair_stream_table('orders_summary');
```

---

## Snapshots

For cases where you need a consistent backup of a specific stream table (without dumping the entire database), pg_trickle has a snapshot feature:

```sql
-- Create a named snapshot
SELECT pgtrickle.snapshot_stream_table('orders_summary', 'before_migration');

-- List snapshots
SELECT * FROM pgtrickle.list_snapshots();

-- Restore from snapshot
SELECT pgtrickle.restore_from_snapshot('orders_summary', 'before_migration');

-- Clean up
SELECT pgtrickle.drop_snapshot('before_migration');
```

Snapshots are full copies of the stream table data stored in a separate table. They're useful for:
- Pre-migration backups (before altering the stream table's query)
- A/B testing (compare the current state with a known-good snapshot)
- Debugging (restore to a specific state for investigation)

---

## CloudNativePG and Kubernetes

If you're running pg_trickle on CloudNativePG (the Kubernetes operator), backups use `barman` and WAL archiving to S3. The restore procedure is the same as standard PITR — the operator handles the WAL recovery, and pg_trickle's state is consistent at the recovery point.

One caveat: if you're restoring a replica that was promoted to primary, the stream table scheduler needs to be enabled on the new primary:

```sql
ALTER SYSTEM SET pg_trickle.enabled = on;
SELECT pg_reload_conf();
```

The scheduler only runs on the primary. Replicas don't run background workers.

---

## Best Practices

1. **Use PITR over pg_dump when possible.** PITR gives you a consistent point-in-time state. pg_dump requires post-restore repair.

2. **Always repair after pg_restore.** Run `repair_stream_table` on every stream table after restoring from a logical dump.

3. **Don't exclude change buffer tables from dumps.** The change buffers (`pgtrickle_changes.*`) should be included in the dump. Excluding them causes the scheduler to miss changes that were pending at dump time.

4. **Snapshot before migrations.** Before running `alter_stream_table` with a query change, take a snapshot. If the migration goes wrong, you can restore the snapshot and try again.

5. **Test your restore procedure.** Restore to a test database and verify that stream tables are refreshing correctly. Check `pgtrickle.health_check()` and `pgtrickle.pgt_status()` after restore.
