# Migrating from pg_ivm to pg_trickle

## Feature gap, SQL differences, and a step-by-step migration procedure

---

If you're using pg_ivm for incremental view maintenance in PostgreSQL, you probably chose it because it was the only option. It's been around since 2021, it works for simple cases, and it proved that IVM inside PostgreSQL is viable.

But if you've hit its limits — no background scheduling, no multi-table JOIN support in some cases, no monitoring, no DAG resolution — you've probably wondered whether there's an upgrade path.

This post is that upgrade path.

---

## The Feature Gap

Here's what pg_ivm and pg_trickle each support as of mid-2026:

| Feature | pg_ivm | pg_trickle |
|---|---|---|
| Single-table aggregation (SUM, COUNT, AVG) | ✅ | ✅ |
| Multi-table JOINs with aggregation | Partial | ✅ |
| LEFT/RIGHT/FULL OUTER JOINs | ❌ | ✅ |
| HAVING clauses | ❌ | ✅ |
| Window functions (auto-rewrite) | ❌ | ✅ (via auto-rewrite to DIFFERENTIAL) |
| Subqueries in WHERE | ❌ | ✅ |
| CASE expressions in aggregates | Limited | ✅ |
| FILTER clauses on aggregates | ❌ | ✅ |
| Background scheduler | ❌ (manual refresh) | ✅ (configurable schedule, SLA tiers) |
| IMMEDIATE refresh mode | ✅ | ✅ |
| DIFFERENTIAL (deferred) refresh | ❌ | ✅ |
| DAG-aware scheduling | ❌ | ✅ (diamond-safe) |
| Change data capture | Trigger-based | Hybrid (triggers or WAL) |
| Monitoring / health checks | ❌ | ✅ (Prometheus, self-monitoring) |
| Online schema evolution | ❌ | ✅ (`alter_stream_table`) |
| Transactional outbox/inbox | ❌ | ✅ |
| Row-level security | ❌ | ✅ |
| dbt integration | ❌ | ✅ |
| Citus support | ❌ | ✅ |
| Partitioned source tables | ❌ | ✅ |
| Downstream publications | ❌ | ✅ |

The core difference: pg_ivm implements IMMEDIATE-mode refresh only. Every source table change triggers an inline delta computation. There's no deferred mode, no background worker, and no scheduling.

This means pg_ivm adds overhead to every write, with no way to amortize it. For low-write workloads, that's fine. For high-write workloads, it's a bottleneck.

---

## SQL Differences

### Creating an IVM table

**pg_ivm:**
```sql
SELECT create_immv('order_totals',
    'SELECT customer_id, SUM(amount) AS total FROM orders GROUP BY customer_id');
```

**pg_trickle:**
```sql
SELECT pgtrickle.create_stream_table(
    'order_totals',
    $$SELECT customer_id, SUM(amount) AS total
      FROM orders GROUP BY customer_id$$,
    schedule     => '3s',
    refresh_mode => 'DIFFERENTIAL'
);
```

The pg_trickle version is more verbose — you specify the schedule and refresh mode. In exchange, you get control over how and when the view is refreshed.

### Refreshing

**pg_ivm:** Automatic — triggers fire on every source change and update the IMMV inline.

**pg_trickle (IMMEDIATE):** Same behavior as pg_ivm — delta applied in the source transaction.

**pg_trickle (DIFFERENTIAL):** Background worker drains change buffers on the configured schedule. No write-path overhead.

### Dropping

**pg_ivm:**
```sql
DROP TABLE order_totals;
-- Manually clean up triggers
```

**pg_trickle:**
```sql
SELECT pgtrickle.drop_stream_table('order_totals');
-- Cleans up storage table, triggers, change buffers, catalog entries
```

---

## Migration Procedure

### Step 1: Inventory your IMMVs

```sql
-- Find all pg_ivm maintained views
SELECT relname, pg_get_viewdef(oid)
FROM pg_class
WHERE relname IN (
    SELECT immvname FROM pg_ivm_immv
);
```

Record each IMMV name and its defining query.

### Step 2: Create equivalent stream tables

For each IMMV, create a pg_trickle stream table. Start with IMMEDIATE mode to match pg_ivm's behavior:

```sql
SELECT pgtrickle.create_stream_table(
    'order_totals',
    $$SELECT customer_id, SUM(amount) AS total
      FROM orders GROUP BY customer_id$$,
    refresh_mode => 'IMMEDIATE'
);
```

If the IMMV query uses features pg_trickle supports but pg_ivm didn't (LEFT JOINs, HAVING, window functions), you can enhance the query during migration.

### Step 3: Validate

Compare the pg_ivm IMMV with the pg_trickle stream table:

```sql
-- Should return zero rows if both are correct
(SELECT * FROM old_order_totals_immv EXCEPT SELECT * FROM order_totals)
UNION ALL
(SELECT * FROM order_totals EXCEPT SELECT * FROM old_order_totals_immv);
```

### Step 4: Update application queries

If your application queries the IMMV by name, update the references to point to the stream table. If you used the same name, no changes needed.

### Step 5: Drop the old IMMVs

```sql
-- Remove pg_ivm's IMMV (this also removes the triggers)
SELECT drop_immv('old_order_totals_immv');
```

### Step 6: Optimize refresh modes

Now that you're on pg_trickle, evaluate which stream tables should be DIFFERENTIAL:

```sql
-- Switch high-write tables to deferred refresh
SELECT pgtrickle.alter_stream_table('order_totals',
    refresh_mode => 'DIFFERENTIAL',
    schedule     => '2s'
);
```

This removes the write-path overhead. Your write throughput improves, and the stream table is at most 2 seconds stale.

### Step 7: Remove pg_ivm

```sql
DROP EXTENSION pg_ivm;
```

---

## Gotchas

### Different NULL handling

pg_ivm and pg_trickle may handle NULL aggregation groups differently. Test your queries with NULL values in the GROUP BY columns to ensure the results match.

### Trigger ordering

If you have other triggers on the source tables, be aware that pg_trickle's CDC triggers execute as AFTER triggers. If your existing triggers modify the row (BEFORE triggers), the change captured by pg_trickle will reflect the modified value, which is correct. But if you have AFTER triggers that perform additional writes, check that the ordering doesn't cause issues.

### Performance profile change

Moving from IMMEDIATE to DIFFERENTIAL changes the performance profile. IMMEDIATE mode has higher write latency but zero read staleness. DIFFERENTIAL mode has lower write latency but some read staleness. Profile both modes with your actual workload.

---

## Why Switch?

If pg_ivm works for your use case today and you have no plans to:
- Scale write throughput
- Add monitoring
- Chain views (DAG)
- Use background scheduling
- Enable outbox/inbox
- Deploy on Citus

Then staying on pg_ivm is fine. It's simpler and has fewer moving parts.

But if you're hitting any of those needs — or if you're finding pg_ivm's query restriction frustrating — the migration is straightforward. You can run both extensions simultaneously during the transition, validate correctness, and cut over at your own pace.
