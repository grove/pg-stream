# Backup and Restore

Like any standard PostgreSQL extension, `pg_trickle` supports logical backups via `pg_dump` and physical backups (via tools like pgBackRest or `pg_basebackup`).

Because `pg_trickle` maintains automated states (like Change Data Capture buffers and DDL Event Triggers), specific workflows should be followed to ensure a smooth recovery.

## Physical Backups (pgBackRest / pg_basebackup)

Physical backups copy the underlying data blocks. These are the most robust backups.

**No special steps are needed** during restore. When the database comes online, `pg_trickle`'s catalogs, CDC buffers, and internal dependencies exist precisely as they did at the moment the snapshot was taken.

*Note for WAL-Mode Users: Physical backups do not export replication slot data by default. If your CDC pipeline was in `wal` mode, logical slots might not survive the recreation. The pg_trickle scheduler handles missing slots gracefully by temporarily re-enabling table triggers.*

## Logical Backups (pg_dump / pg_restore)

Logical backups dump your database schema as generic cross-compatible SQL (`CREATE TABLE`, `INSERT`, `CREATE INDEX`).

`pg_trickle` integrates with `pg_dump` natively. When restoring these backups (which typically involves sequentially recreating schemas, inserting data into those tables, and lastly applying indexes and triggers), you must restore into a database precisely, to allow the extension to rewrite its own internal triggers correctly without conflicting with plain PostgreSQL commands.

### The Recommended Multi-Stage pg_restore Strategy

The most reliable approach is to use the `--section` arguments of `pg_restore`. By breaking the restore up into pieces, we guarantee that when the schema, data, and constraints are created, all variables and configurations are actively in the database, and our custom hook `DdlEventKind::ExtensionChange` intercepts the query and automatically dials `pgtrickle.restore_stream_tables()` internally.

---

## Stream Table Snapshots (v0.27.0)

In addition to traditional backup methods, pg_trickle v0.27.0 introduces a
native **snapshot API** for stream tables. Snapshots are useful for:

- **Replica bootstrap**: Quickly populate a new replica without a full refresh
- **PITR alignment**: Export a stream table's state at a known frontier
- **Offline analysis**: Create an archival copy without impact to live queries

### Creating a snapshot

```sql
-- Creates pgtrickle.snapshot_orders_agg_<epoch_ms> (auto-named)
SELECT pgtrickle.snapshot_stream_table('public.orders_agg');

-- Or specify the target table name explicitly
SELECT pgtrickle.snapshot_stream_table('public.orders_agg',
       'pgtrickle.orders_agg_backup_2025');
```

The snapshot table contains all rows from the stream table plus three
metadata columns: `__pgt_snapshot_version`, `__pgt_frontier`, and
`__pgt_snapshotted_at`.

### Listing available snapshots

```sql
SELECT snapshot_table, created_at, row_count, size_bytes
FROM pgtrickle.list_snapshots('public.orders_agg')
ORDER BY created_at DESC;
```

### Restoring from a snapshot

```sql
-- Truncate and restore from a named snapshot
SELECT pgtrickle.restore_from_snapshot(
    'public.orders_agg',
    'pgtrickle.snapshot_orders_agg_1735000000000'
);
```

After restore, the stream table's frontier is set to the snapshot's frontier
so the next refresh cycle is **DIFFERENTIAL** (not FULL). This avoids an
expensive re-scan for the restored rows.

### Dropping a snapshot

```sql
SELECT pgtrickle.drop_snapshot('pgtrickle.snapshot_orders_agg_1735000000000');
```

This removes both the archival table and its metadata catalog row.

> **Note**: Snapshots are ordinary PostgreSQL tables; they are included in
> `pg_dump` / `pg_basebackup` automatically. No special restore procedure is
> needed — the snapshot table persists as a regular table until explicitly dropped.
