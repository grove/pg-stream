[← Back to Blog Index](README.md)

# Hot, Warm, Cold, Frozen: Tiered Scheduling at Scale

## How pg_trickle's scheduler stays efficient at 50, 500, and 5,000 stream tables

---

At 5 stream tables, the scheduler is invisible. It wakes up every second, checks if anything is due, refreshes what needs refreshing, and goes back to sleep. CPU cost: immeasurable.

At 50 stream tables, the scheduler loop takes a few milliseconds per cycle. Still invisible.

At 500 stream tables, the scheduler is checking 500 tables every cycle. Most of them haven't changed. Most of them aren't due for refresh. But the scheduler doesn't know that until it checks. The loop time starts to matter.

pg_trickle's tiered scheduling solves this. Stream tables are classified by change frequency — hot, warm, cold, frozen — and checked at different cadences. A frozen table that hasn't changed in a week isn't checked every second.

---

## The Tiers

| Tier | Change Frequency | Check Cadence | Example |
|------|-----------------|---------------|---------|
| **Hot** | Changes every cycle or nearly | Every scheduler cycle (1s) | Real-time dashboards, event counters |
| **Warm** | Changes every few cycles | Every 5 cycles | Hourly aggregates, session summaries |
| **Cold** | Changes infrequently | Every 30 cycles | Weekly reports, monthly rollups |
| **Frozen** | No changes in extended period | Every 60 cycles | Archived data, one-time imports |

"Check cadence" means how often the scheduler looks at the stream table's change buffer to determine if a refresh is needed. A cold table with a 10-second schedule is still refreshed every 10 seconds *if it has changes* — but the scheduler only checks for changes every 30 cycles instead of every cycle.

---

## Automatic Classification

Tier assignment is automatic. pg_trickle tracks a rolling window of "did this stream table have changes in the last N scheduler cycles?" and classifies based on the ratio:

```
change_ratio = cycles_with_changes / total_cycles (over last 100 cycles)
```

| change_ratio | Tier |
|-------------|------|
| > 0.8 | Hot |
| 0.2 – 0.8 | Warm |
| 0.01 – 0.2 | Cold |
| < 0.01 | Frozen |

Promotion is immediate: if a frozen table starts receiving changes, it's promoted to hot on the next scheduler cycle. Demotion is gradual: a table must be consistently quiet to move from hot to warm, warm to cold, etc. This prevents flapping.

---

## Why It Matters

The scheduler loop has a per-table cost. For each table, it:

1. Reads the catalog entry (schedule, status, last refresh time).
2. Checks the change buffer for pending changes.
3. Evaluates the dependency graph (is the table due? are upstream tables current?).
4. Decides whether to dispatch a refresh.

Steps 2 and 3 are the expensive parts. Step 2 requires a query against the change buffer table. Step 3 requires walking the DAG.

Without tiering, 500 tables means 500 change-buffer checks per second. With tiering, if 50 are hot, 100 are warm, 150 are cold, and 200 are frozen:

```
Checks per cycle:
  Hot:    50 × 1.0      = 50
  Warm:   100 × 0.2     = 20
  Cold:   150 × 0.033   = 5
  Frozen: 200 × 0.017   = 3.4
  Total: ~78 per cycle (vs. 500 without tiering)
```

That's an 84% reduction in scheduler overhead. The savings compound because the skipped checks are precisely the tables that don't have changes — checking them would have been wasted work anyway.

---

## Enabling Tiered Scheduling

Tiered scheduling is enabled by default since v0.12.0:

```sql
SHOW pg_trickle.tiered_scheduling;
-- on
```

To disable (not recommended, but available for debugging):

```sql
SET pg_trickle.tiered_scheduling = off;
```

With tiering off, every table is checked every cycle. This is fine for small deployments (<50 tables) but wasteful at scale.

---

## Monitoring Tiers

```sql
SELECT
  name,
  schedule,
  tier,
  change_ratio,
  last_refresh
FROM pgtrickle.pgt_status()
ORDER BY tier, change_ratio DESC;
```

```
     name            | schedule |  tier  | change_ratio | last_refresh
---------------------+----------+--------+--------------+---------------------
 live_dashboard      | 2s       | hot    | 0.95         | 2026-04-27 10:15:01
 event_counter       | 1s       | hot    | 0.88         | 2026-04-27 10:15:01
 session_summary     | 10s      | warm   | 0.45         | 2026-04-27 10:14:55
 daily_revenue       | 30s      | warm   | 0.22         | 2026-04-27 10:14:40
 monthly_report      | 5m       | cold   | 0.03         | 2026-04-27 10:10:00
 archived_metrics    | 1h       | frozen | 0.00         | 2026-04-27 09:00:00
```

If a table you expect to be hot shows up as cold, check the change buffer — maybe the source table isn't receiving DML as expected.

---

## Interaction with Event-Driven Wake

pg_trickle also supports event-driven wake via LISTEN/NOTIFY. When CDC triggers fire, they emit a NOTIFY that wakes the scheduler immediately instead of waiting for the next polling cycle.

Tiered scheduling and event-driven wake are complementary:

- **Event-driven wake** reduces latency: the scheduler doesn't wait up to 1 second to notice a change.
- **Tiered scheduling** reduces overhead: the scheduler doesn't waste cycles checking tables that haven't changed.

Both are enabled by default. Together, they handle the "many tables, sporadic changes" workload efficiently: most tables are checked infrequently (tiering), and the ones that do change are refreshed immediately (event-driven wake).

---

## Tuning for Large Deployments

For deployments with 1,000+ stream tables:

**Increase scheduler interval slightly:**
```sql
SET pg_trickle.scheduler_interval_ms = 2000;  -- 2 seconds instead of 1
```

This halves the number of scheduler cycles per second. With tiered scheduling, the per-cycle cost is already low, so the impact on freshness is minimal (hot tables are still checked every 2 seconds).

**Ensure event-driven wake is on:**
```sql
SET pg_trickle.event_driven_wake = on;
```

This ensures that hot tables are refreshed immediately on change, regardless of the scheduler interval. The scheduler interval only affects the polling fallback.

**Monitor scheduler loop time:**
```sql
SELECT
  avg_loop_ms,
  max_loop_ms,
  tables_checked_per_cycle,
  tables_refreshed_per_cycle
FROM pgtrickle.health_summary();
```

If `avg_loop_ms` exceeds 100ms, the scheduler is doing too much work per cycle. This typically means too many tables are classified as hot (because they all have continuous changes). Consider:
- Increasing the schedule for tables that don't need sub-second freshness.
- Using CALCULATED scheduling to let intermediate tables inherit longer cadences.
- Checking whether bulk imports are keeping change buffers permanently active.

---

## The Frozen-to-Hot Promotion Path

When a frozen table starts receiving changes (e.g., a monthly batch import), the promotion is immediate:

1. CDC trigger fires, emitting NOTIFY.
2. Scheduler wakes, checks the table (even though it's in the frozen tier, NOTIFY overrides the tier check cadence).
3. Table is promoted to hot.
4. Normal scheduling resumes.

The promotion happens within one scheduler cycle — there's no delay from being in the frozen tier. The NOTIFY acts as an interrupt, bypassing the tier-based check cadence.

Demotion back to frozen takes longer: the table must have zero changes for ~100 consecutive cycles. This prevents flapping during sporadic-but-recurring workloads.

---

## Summary

Tiered scheduling classifies stream tables as hot, warm, cold, or frozen based on change frequency. The scheduler checks hot tables every cycle and frozen tables every ~60 cycles, reducing per-cycle overhead by 80%+ at scale.

Classification is automatic. Promotion is immediate (via NOTIFY interrupt). Demotion is gradual (prevents flapping). It's enabled by default and requires no configuration for most deployments.

At 5 tables, you don't need it. At 500, you can't live without it.
