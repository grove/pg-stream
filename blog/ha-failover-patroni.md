[← Back to Blog Index](README.md)

# High Availability Failover with pg_trickle and Patroni

## How stream table state and change buffers survive a primary switchover, and what it takes to achieve zero data loss

---

Running pg_trickle in production means running it in a high-availability cluster. Nobody deploys a single PostgreSQL instance for critical workloads anymore — you have a primary, one or more standbys, and an HA controller (Patroni, Stolon, pg_auto_failover, or CloudNativePG) that handles automatic failover. The question is: what happens to your stream tables when the primary fails and a standby is promoted?

The short answer is: everything works. pg_trickle's state lives entirely in regular PostgreSQL tables (the catalog, change buffers, and materialized stream table data). All of this is replicated to standbys via standard WAL streaming. When a standby is promoted, it has a complete, consistent copy of all stream table state as of the last replicated WAL position.

The longer answer involves understanding the failure modes, the recovery semantics, and the configuration choices that determine whether you get zero data loss or merely very low data loss.

---

## What State Does pg_trickle Maintain?

pg_trickle stores all its state in PostgreSQL tables within the `pgtrickle` and `pgtrickle_changes` schemas:

1. **Catalog tables** (`pgtrickle.pgt_stream_tables`, etc.) — metadata about stream table definitions, refresh modes, DAG relationships
2. **Change buffer tables** (`pgtrickle_changes.changes_<oid>`) — pending row changes captured by CDC triggers since the last refresh
3. **Materialized data** — the stream table's result set, stored as a regular heap table
4. **Shared memory state** — scheduler bookkeeping, refresh counters, lock states

Items 1–3 are durable, WAL-logged, and replicated. Item 4 is in shared memory and is reconstructed on startup from the durable state.

---

## Synchronous vs. Asynchronous Replication

The data loss characteristics during failover depend on your replication mode:

**Synchronous replication** (`synchronous_commit = on` with a sync standby): The primary waits for the standby to acknowledge each transaction before committing. If the primary dies, the standby has every committed transaction. Zero data loss is guaranteed.

**Asynchronous replication** (the default): The primary commits immediately and streams WAL to the standby asynchronously. If the primary dies, the standby might be a few transactions behind. Those in-flight transactions are lost.

For pg_trickle, this means:

- **Synchronous mode:** After failover, the promoted standby has all committed source table changes and all committed stream table states. The change buffers accurately reflect "changes since last refresh." Resuming the scheduler produces correct results.

- **Asynchronous mode:** After failover, some recent source table changes might be lost (they were committed on the old primary but not yet replicated). This is the same data loss that affects all tables — it's not specific to pg_trickle. Stream tables might show slightly stale results (they reflect a state a few transactions behind), but they'll catch up on the next refresh.

---

## Patroni Failover Sequence

When Patroni detects the primary is unhealthy and initiates failover:

1. **Fencing**: The old primary is isolated (network fence, `pg_ctl stop`, or shutdown)
2. **Promotion**: The most up-to-date standby is promoted with `pg_ctl promote`
3. **Reconnection**: Clients are redirected to the new primary (via HAProxy, DNS, or Patroni's REST API)
4. **Timeline advance**: The promoted standby starts a new WAL timeline

From pg_trickle's perspective:

- **Step 1–2**: The background worker on the old primary is killed (either by `SIGTERM` from Patroni or by the postmaster shutdown). Any in-flight refresh is aborted. This is safe — refreshes are transactional, so an interrupted refresh rolls back cleanly.

- **Step 2 (on new primary)**: PostgreSQL starts the `pg_trickle` background worker as part of the promoted standby's startup sequence. The worker reads its state from the catalog tables (which are now read-write) and resumes scheduling.

- **Step 3**: Client applications reconnect and resume writing to source tables. CDC triggers on the new primary capture changes into change buffer tables.

- **Step 4**: The DAG scheduler picks up where it left off — processing pending changes in the buffers and refreshing stream tables according to their configured schedule.

---

## The Refresh-in-Progress Problem

What if a refresh was halfway through when the primary crashed? The refresh involves:

1. Reading change buffers
2. Computing deltas
3. Applying deltas to the stream table
4. Truncating processed change buffers

All of this happens within a single transaction. If the primary crashes at any point during this transaction, the transaction rolls back. On the new primary:

- The change buffer still contains all unprocessed changes (the truncation never committed)
- The stream table still reflects the pre-refresh state (the delta application never committed)
- The next refresh processes the same changes successfully

This is the beauty of transactional refresh: crash recovery is automatic and correct. No manual intervention, no reconciliation, no "replaying from checkpoint."

---

## Shared Memory Reconstruction

pg_trickle uses shared memory for scheduler state: which stream tables need refresh, when they were last refreshed, how long the last refresh took. This state is not persisted to disk — it lives only in shared memory.

When the new primary starts the pg_trickle background worker, it reconstructs shared memory state from the catalog:

1. Reads all stream table definitions from `pgtrickle.pgt_stream_tables`
2. Reads dependency information to rebuild the DAG
3. Checks change buffer tables for pending changes
4. Initializes refresh timestamps to "never" (forcing a refresh check on the next cycle)

The first refresh cycle after failover might refresh more stream tables than strictly necessary (because it doesn't know how recently each was refreshed on the old primary). This is harmless — a redundant refresh produces correct results, just with slightly more work.

---

## Split-Brain Prevention

The most dangerous HA scenario is split-brain: both the old primary and new primary accept writes simultaneously. This can cause divergent change buffers and inconsistent stream table state.

Patroni prevents split-brain through fencing — the old primary is forcefully stopped before the new primary is promoted. But fencing can fail. To protect against split-brain at the pg_trickle level:

1. **`pg_trickle.enabled` GUC**: Set to `off` on standbys. The background worker checks this on startup and does nothing if disabled. Only the promoted primary (where Patroni sets it to `on`) runs the scheduler.

2. **Advisory locks**: The scheduler acquires a cluster-wide advisory lock before performing refreshes. If a stale primary somehow continues running, its lock acquisition will fail (or it will hold a lock that's irrelevant since clients have moved to the new primary).

3. **Timeline-aware buffers**: Change buffer entries include the WAL timeline. After a timeline fork, entries from the wrong timeline can be identified and discarded.

---

## Configuring for Zero Data Loss

For workloads where stream table consistency is critical (financial analytics, billing aggregations):

```ini
# postgresql.conf on primary
synchronous_standby_names = 'ANY 1 (standby1, standby2)'
synchronous_commit = on

# pg_trickle specific
pg_trickle.enabled = on
pg_trickle.refresh_on_promote = true   # force immediate refresh after failover
```

With synchronous replication, the promoted standby is guaranteed to have all committed data. The `refresh_on_promote` setting triggers an immediate refresh cycle on promotion, ensuring stream tables are current before client connections arrive.

For workloads where sub-second staleness is acceptable:

```ini
# Asynchronous replication (lower latency, higher throughput)
synchronous_commit = off

# pg_trickle will catch up after failover
pg_trickle.enabled = on
pg_trickle.scheduler_interval = '1s'
```

After failover, stream tables might be 1–2 seconds stale. The scheduler catches up within one interval, and the system is fully current. No manual intervention.

---

## Testing Failover

Validate your HA configuration with deliberate failover testing:

```bash
# Simulate primary failure
patronictl failover --candidate standby1 --force

# Verify pg_trickle is running on new primary
psql -h new-primary -c "SELECT pgtrickle.version();"
psql -h new-primary -c "SELECT * FROM pgtrickle.stream_table_status();"

# Verify stream tables are being refreshed
psql -h new-primary -c "
    INSERT INTO source_table (value) VALUES ('after-failover');
    SELECT pgtrickle.refresh_stream_table('my_stream');
    SELECT * FROM my_stream WHERE value = 'after-failover';
"
```

The test should confirm:
- The pg_trickle background worker is running on the new primary
- Stream table metadata is intact
- CDC triggers are active on the new primary
- Incremental refresh processes new changes correctly

---

## CloudNativePG and Kubernetes

For Kubernetes deployments using CloudNativePG (CNPG), the considerations are similar but the mechanisms differ:

- CNPG manages failover via Pod deletion and promotion
- The pg_trickle shared library is included in the container image
- GUCs are set via the `Cluster` custom resource
- Failover is typically faster (seconds) due to the container orchestration model

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: analytics-cluster
spec:
  instances: 3
  postgresql:
    parameters:
      shared_preload_libraries: "pg_trickle"
      pg_trickle.enabled: "on"
      pg_trickle.scheduler_interval: "2s"
  storage:
    size: 100Gi
```

CNPG ensures that only the primary instance has `pg_trickle.enabled = on` effective (standbys are read-only by definition). After a failover, the new primary's background worker starts automatically.

---

## Monitoring Failover Health

After a failover event, verify pg_trickle's health:

```sql
-- Check scheduler is running
SELECT * FROM pgtrickle.scheduler_status();

-- Check for stale stream tables (last_refreshed should be recent)
SELECT name, last_refreshed_at, refresh_mode
FROM pgtrickle.stream_table_status()
WHERE last_refreshed_at < now() - interval '1 minute'
ORDER BY last_refreshed_at;

-- Check change buffer sizes (should be draining, not growing)
SELECT relname, n_live_tup
FROM pg_stat_user_tables
WHERE schemaname = 'pgtrickle_changes'
ORDER BY n_live_tup DESC;
```

If change buffers are growing and stream tables aren't refreshing, the scheduler might not have started. Check the PostgreSQL log for background worker startup messages and verify the `pg_trickle.enabled` GUC is `on`.

---

*pg_trickle survives failover because its state is just PostgreSQL tables — replicated, durable, and transactional. Configure synchronous replication for zero data loss, or accept asynchronous with sub-second catch-up. Either way, your stream tables resume without manual intervention.*
