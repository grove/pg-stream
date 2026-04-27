[← Back to Blog Index](README.md)

# Drain Mode: Zero-Downtime Upgrades for Stream Tables

## Graceful quiesce before maintenance, rolling restarts, and extension upgrades

---

You need to upgrade pg_trickle. Or restart PostgreSQL for a configuration change. Or run a maintenance operation that requires no active refreshes.

If you just restart PostgreSQL while a refresh is in progress, the refresh is interrupted. The stream table is left in a partially-updated state. pg_trickle recovers on the next startup — it detects the interrupted refresh and either retries or marks the table for repair — but it's not clean.

Drain mode provides a clean shutdown path. `pgtrickle.drain()` tells the scheduler to stop dispatching new refreshes and wait for in-flight refreshes to complete. When all refreshes are done, `pgtrickle.is_drained()` returns `true`, and you can safely restart.

---

## The API

```sql
-- Signal drain
SELECT pgtrickle.drain();

-- Check status
SELECT pgtrickle.is_drained();
-- false (still waiting for in-flight refreshes)

-- Wait a moment...
SELECT pgtrickle.is_drained();
-- true (all refreshes complete, scheduler idle)
```

After drain completes:
- The scheduler is running but not dispatching new work.
- All in-flight refreshes have completed.
- Change buffers continue accumulating (CDC triggers still fire).
- Stream tables are still queryable.

---

## The Upgrade Workflow

```bash
# Step 1: Drain
psql -c "SELECT pgtrickle.drain();"

# Step 2: Wait for drain
while ! psql -qtAc "SELECT pgtrickle.is_drained();" | grep -q 't'; do
  sleep 2
done

# Step 3: Upgrade
psql -c "ALTER EXTENSION pg_trickle UPDATE;"

# Step 4: Resume
psql -c "SET pg_trickle.enabled = on;"
```

Between steps 2 and 4, no refreshes are running. The `ALTER EXTENSION UPDATE` can safely migrate schema, catalog tables, and internal state without racing against active refresh operations.

---

## What Happens During Drain

When `drain()` is called:

1. **No new refreshes are dispatched.** The scheduler's dispatch loop skips all tables.
2. **In-flight refreshes continue.** Any refresh that's already executing (running a delta query, applying a MERGE) completes normally.
3. **IMMEDIATE mode refreshes still fire.** Since they're synchronous within user transactions, they can't be deferred. Drain only affects background-scheduled refreshes.
4. **CDC continues.** Triggers keep writing to change buffers. WAL decoder keeps running. Changes accumulate.

The drain is "soft" — it doesn't kill any processes or abort any transactions. It just stops starting new work.

---

## Drain Duration

How long does drain take? It depends on the longest currently-running refresh.

In practice:
- Most refreshes complete in under 1 second.
- A complex FULL refresh on a large table might take 10–30 seconds.
- If a refresh is stuck (waiting on a lock, for example), drain waits indefinitely.

You can set a timeout:

```sql
-- Drain with 60-second timeout
SELECT pgtrickle.drain(timeout_seconds => 60);
```

If in-flight refreshes don't complete within 60 seconds, `drain()` returns `false` and the in-flight refreshes continue. You can then decide: wait longer, or proceed with the restart (accepting the interrupted-refresh recovery cost).

---

## CloudNativePG and Rolling Restarts

In Kubernetes deployments with CloudNativePG, rolling restarts are the norm. The operator restarts pods one at a time, waiting for each to be ready before restarting the next.

Drain mode integrates with this:

1. A `preStop` hook calls `pgtrickle.drain()`.
2. The readiness probe checks `pgtrickle.is_drained()`.
3. Once drained, the pod is marked unready, and the operator proceeds with the restart.

```yaml
# CloudNativePG Cluster manifest (excerpt)
spec:
  postgresql:
    preStop:
      exec:
        command:
          - psql
          - -c
          - "SELECT pgtrickle.drain();"
```

This ensures zero interrupted refreshes during rolling restarts. Change buffers accumulate during the restart window and are processed on the next scheduler cycle after the pod comes back.

---

## Drain and HA Failover

During a PostgreSQL failover (primary → standby promotion), drain mode isn't typically used — failovers are unplanned. But for planned failovers (maintenance, OS patching):

1. Drain the current primary: `SELECT pgtrickle.drain();`
2. Wait for drain: `SELECT pgtrickle.is_drained();`
3. Promote the standby.
4. pg_trickle's launcher on the new primary detects promotion via `pg_is_in_recovery()` and starts the scheduler.

The change buffers on the old primary are replicated to the standby via WAL. No changes are lost.

---

## Monitoring Drain State

```sql
SELECT * FROM pgtrickle.health_summary();
```

During drain, `health_summary()` includes:

```
 scheduler_state | drain_requested | inflight_refreshes | drain_elapsed_seconds
-----------------+-----------------+--------------------+------------------------
 draining        | t               | 2                  | 4.7
```

When `inflight_refreshes` reaches 0 and `scheduler_state` changes to `drained`, it's safe to proceed.

---

## Resuming After Drain

Drain is not permanent. To resume normal operation without restarting:

```sql
-- Cancel the drain
SET pg_trickle.enabled = on;
```

The scheduler resumes dispatching. The accumulated change buffers are processed in the next cycle. Depending on how long the drain lasted, the first post-drain refresh may be larger than usual (more accumulated changes).

---

## Summary

Drain mode is the safe shutdown path for pg_trickle. `drain()` stops new refreshes. In-flight refreshes complete. `is_drained()` confirms it's safe to proceed.

Use it before:
- `ALTER EXTENSION pg_trickle UPDATE`
- PostgreSQL restart
- Planned failover
- CloudNativePG rolling restart

The alternative — restarting mid-refresh — works (pg_trickle recovers), but it's not clean. Drain mode is one function call for a clean cutover.
