[← Back to Blog Index](README.md)

# Spill-to-Disk and the Auto-Fallback Safety Net

## What happens when your delta query exceeds work_mem — and how pg_trickle recovers

---

Your stream table has been running in DIFFERENTIAL mode for weeks. Refreshes take 5ms. Life is good.

Then marketing runs a campaign, and 50,000 orders land in 10 minutes. The delta query needs to join 50,000 changed rows with a million-row dimension table, aggregate the results, and apply the merge. The intermediate result set doesn't fit in `work_mem`. PostgreSQL spills to disk.

The refresh still completes — it just takes 2 seconds instead of 5 milliseconds. No data is lost. But if this happens every cycle, DIFFERENTIAL mode is doing more work than FULL would.

pg_trickle's spill-to-disk detection and auto-fallback handle this. When delta queries spill repeatedly, the system switches to FULL refresh until the situation stabilizes.

---

## How Delta Queries Use Memory

A DIFFERENTIAL refresh executes a delta query that processes only the changed rows. The query plan typically includes:

1. **Change buffer scan:** Read the changed rows from `pgtrickle_changes.changes_<oid>`.
2. **Join with current data:** Join changed rows with source tables to compute the delta.
3. **Aggregation:** Compute aggregate deltas (SUM, COUNT, etc.).
4. **MERGE:** Apply the delta to the stream table.

Steps 2 and 3 may use hash tables, sort buffers, or other in-memory structures. These are bounded by PostgreSQL's `work_mem` setting (default 4MB).

When the intermediate result exceeds `work_mem`, PostgreSQL's executor writes the excess to temporary files on disk. This is called "spilling" or "temp file usage." It's not an error — it's the normal overflow mechanism. But disk I/O is orders of magnitude slower than memory access.

---

## Detecting Spill

pg_trickle monitors `temp_blks_written` in the query execution statistics. After each DIFFERENTIAL refresh, it checks:

```
Did this refresh write temp blocks to disk?
```

This information is available from PostgreSQL's `pg_stat_statements` or the executor's instrumentation. pg_trickle records it in the refresh history:

```sql
SELECT
  refresh_id,
  refresh_mode,
  duration_ms,
  temp_blocks_written
FROM pgtrickle.get_refresh_history('order_summary')
ORDER BY refresh_id DESC
LIMIT 10;
```

```
 refresh_id | refresh_mode  | duration_ms | temp_blocks_written
------------+--------------+-------------+---------------------
 1042       | DIFFERENTIAL | 5.2         | 0
 1043       | DIFFERENTIAL | 5.1         | 0
 1044       | DIFFERENTIAL | 2100.0      | 4096
 1045       | DIFFERENTIAL | 1800.0      | 3584
 1046       | FULL         | 450.0       | 0
```

Refreshes 1044 and 1045 spilled to disk. Refresh 1046 was automatically switched to FULL.

---

## The Spill Threshold

pg_trickle tracks consecutive spills using two GUC settings:

**`pg_trickle.spill_threshold_blocks`** (default: 1024)
The number of temp blocks written before a refresh is flagged as "spilled." Below this threshold, minor spills are ignored — they're usually caused by transient memory pressure and don't warrant a mode switch.

**`pg_trickle.spill_consecutive_limit`** (default: 3)
The number of consecutive spilled refreshes before pg_trickle switches the stream table to FULL refresh mode. Three consecutive spills is the signal that the workload has shifted and DIFFERENTIAL is no longer efficient.

The logic:

```
if temp_blocks_written > spill_threshold_blocks:
    consecutive_spills += 1
else:
    consecutive_spills = 0

if consecutive_spills >= spill_consecutive_limit:
    switch to FULL refresh for this stream table
```

---

## The Auto-Recovery

Switching to FULL is not permanent. pg_trickle continues monitoring the change ratio (how much data changed relative to the stream table size). When the change ratio drops below `differential_max_change_ratio` (default: 0.10), pg_trickle switches back to DIFFERENTIAL.

The typical sequence:

1. Burst of changes → delta query spills → 3 consecutive spills → switch to FULL.
2. FULL refresh handles the burst cleanly.
3. Change rate returns to normal → change ratio drops below 10% → switch back to DIFFERENTIAL.
4. Normal 5ms refreshes resume.

The entire cycle is automatic. No manual intervention.

---

## Tuning merge_work_mem_mb

The most effective way to prevent spilling is to give delta queries more memory:

```sql
-- Increase merge work memory (default: auto-calculated)
SET pg_trickle.merge_work_mem_mb = 64;
```

This sets the `work_mem` specifically for pg_trickle's delta queries, independent of the global PostgreSQL `work_mem` setting. It prevents delta queries from competing with user queries for memory.

**How to choose a value:**

1. Check the `temp_blocks_written` column in refresh history.
2. Multiply by 8KB (PostgreSQL's block size) to get the spill volume.
3. Set `merge_work_mem_mb` to at least that amount plus a 50% margin.

Example: If spills are typically 3,000 blocks (24MB), set `merge_work_mem_mb = 48`.

---

## When FULL Is Actually Better

Spilling isn't always a sign that DIFFERENTIAL mode is broken. Sometimes the delta is genuinely large enough that FULL refresh is the better strategy.

The crossover point depends on the query complexity and table size, but the general rule:

- **Change ratio < 5%:** DIFFERENTIAL is almost always faster, even if it spills slightly.
- **Change ratio 5–15%:** Gray zone. Spilling here suggests FULL might be competitive.
- **Change ratio > 15%:** FULL is usually faster. The delta query is processing so much data that it's approaching a full scan anyway, but with the overhead of the MERGE step.

pg_trickle's cost model considers all of this — change ratio, historical spill rates, and FULL refresh timing — when making the AUTO mode decision. The spill detection adds a corrective signal: "the cost model predicted DIFFERENTIAL would be fast, but it wasn't."

---

## Monitoring Spill History

For a global view of spill behavior:

```sql
SELECT
  st.name,
  COUNT(*) FILTER (WHERE rh.temp_blocks_written > 0) AS spilled_refreshes,
  COUNT(*) AS total_refreshes,
  MAX(rh.temp_blocks_written) AS max_spill_blocks,
  AVG(rh.duration_ms) FILTER (WHERE rh.temp_blocks_written > 0) AS avg_spill_duration_ms,
  AVG(rh.duration_ms) FILTER (WHERE rh.temp_blocks_written = 0) AS avg_normal_duration_ms
FROM pgtrickle.pgt_stream_tables st
JOIN pgtrickle.get_refresh_history(st.name) rh ON true
GROUP BY st.name
HAVING COUNT(*) FILTER (WHERE rh.temp_blocks_written > 0) > 0
ORDER BY spilled_refreshes DESC;
```

This shows which stream tables are spilling, how often, and how much slower spilled refreshes are compared to normal ones.

---

## Preventing Spills Proactively

Beyond increasing `merge_work_mem_mb`, there are structural approaches:

**1. Use `append_only` for insert-only tables:**
```sql
SELECT pgtrickle.create_stream_table(
  name        => 'event_counts',
  query       => $$ ... $$,
  append_only => true
);
```
Append-only mode uses INSERT instead of MERGE, which requires less memory.

**2. Shorten refresh intervals to keep deltas small:**
If a 10-second schedule accumulates 10,000 changes per cycle, a 2-second schedule accumulates 2,000. Smaller deltas are less likely to spill.

**3. Add indexes to source tables on join keys:**
The delta query joins changed rows with source tables. Without indexes, these joins may hash-join the full table, consuming work_mem.

**4. Use UNLOGGED change buffers (with caution):**
```sql
SET pg_trickle.cleanup_use_truncate = on;
```
TRUNCATE is faster than DELETE for cleaning up large change buffers, reducing the per-cycle overhead.

---

## Summary

When delta queries exceed `work_mem`, PostgreSQL spills to disk. pg_trickle detects this via `temp_blocks_written` and, after consecutive spills, automatically switches to FULL refresh until the workload stabilizes.

Tuning options:
- `merge_work_mem_mb` — give delta queries more memory.
- `spill_threshold_blocks` — ignore minor spills.
- `spill_consecutive_limit` — control how quickly the fallback triggers.

The safety net is automatic and self-healing. Bursts cause a temporary switch to FULL. Normal operations resume automatically. Your data stays correct through all of it.
