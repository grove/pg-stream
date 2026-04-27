[← Back to Blog Index](README.md)

# Declare Freshness Once: CALCULATED Scheduling

## How upstream tables derive their refresh cadence from downstream consumers

---

You have 15 stream tables. They form a DAG: raw tables → cleaned tables → aggregates → dashboards. The dashboard needs data within 10 seconds of reality. How fresh does each intermediate table need to be?

If you set every table to `schedule => '2s'`, you're over-refreshing the tables nobody queries directly. If you set them to `schedule => '30s'`, the dashboard falls behind. If you tune each one individually, you spend an afternoon doing arithmetic and then retune when the DAG changes.

pg_trickle's CALCULATED scheduling eliminates this problem. You declare the freshness requirement where it matters — on the consumer — and the system propagates it backward through the DAG.

---

## The Idea

In CALCULATED mode, a stream table doesn't have a fixed schedule. Instead, its schedule is derived from the tightest (shortest interval) schedule among all stream tables that depend on it.

```
raw_events (no consumers yet → default 1s)
    ↓
cleaned_events (consumed by summary → inherits 5s)
    ↓
event_summary (schedule => '5s')
    ↓
dashboard_metrics (schedule => '10s')
```

In this DAG:
- `dashboard_metrics` has a declared schedule of 10 seconds.
- `event_summary` has a declared schedule of 5 seconds.
- `cleaned_events` has no declared schedule. Its CALCULATED schedule is 5 seconds — inherited from `event_summary`, which is its tightest downstream consumer.
- `raw_events` has no declared schedule. Its CALCULATED schedule is also 5 seconds — inherited transitively.

If you later add a real-time alerting stream table that depends on `cleaned_events` with `schedule => '1s'`, the CALCULATED schedule for `cleaned_events` and `raw_events` automatically tightens to 1 second. You don't change anything — the DAG propagation handles it.

---

## Setting It Up

By default, a stream table uses a fixed schedule:

```sql
SELECT pgtrickle.create_stream_table(
  name     => 'event_summary',
  query    => $$ ... $$,
  schedule => '5s'
);
```

To use CALCULATED scheduling on an intermediate table, omit the schedule or explicitly set it:

```sql
SELECT pgtrickle.create_stream_table(
  name     => 'cleaned_events',
  query    => $$ SELECT ... FROM raw_events WHERE valid = true $$,
  schedule => 'CALCULATED'
);
```

When a stream table's schedule is CALCULATED, its effective refresh interval is the minimum of all downstream consumers' schedules. If no downstream consumer exists yet, it uses `pg_trickle.default_schedule_seconds` (default: 1 second).

---

## How the Propagation Works

The scheduler maintains a dependency graph. When it evaluates the refresh schedule, it walks the DAG from leaves (tables with declared schedules) backward to roots:

1. Collect all stream tables with declared (non-CALCULATED) schedules.
2. For each CALCULATED stream table, find all downstream dependents.
3. The effective schedule is `min(downstream schedules)`.
4. If the stream table has both a declared schedule and CALCULATED dependents, the declared schedule wins as a floor.

This computation happens once per scheduler cycle (every `scheduler_interval_ms`), not per refresh. The overhead is negligible — it's a graph traversal over a typically small DAG.

---

## A Realistic Example

An e-commerce analytics pipeline:

```sql
-- Layer 1: Clean raw data (CALCULATED — derived from downstream)
SELECT pgtrickle.create_stream_table(
  name     => 'valid_orders',
  query    => $$
    SELECT * FROM orders
    WHERE status != 'cancelled' AND total > 0
  $$,
  schedule => 'CALCULATED'
);

-- Layer 2: Per-customer aggregates (CALCULATED)
SELECT pgtrickle.create_stream_table(
  name     => 'customer_metrics',
  query    => $$
    SELECT
      customer_id,
      COUNT(*) AS order_count,
      SUM(total) AS lifetime_value,
      MAX(created_at) AS last_order
    FROM valid_orders
    GROUP BY customer_id
  $$,
  schedule => 'CALCULATED'
);

-- Layer 3a: Real-time dashboard (declared: 5s)
SELECT pgtrickle.create_stream_table(
  name     => 'dashboard_summary',
  query    => $$
    SELECT
      date_trunc('hour', last_order) AS hour,
      COUNT(*) AS active_customers,
      SUM(lifetime_value) AS total_ltv
    FROM customer_metrics
    GROUP BY 1
  $$,
  schedule => '5s'
);

-- Layer 3b: Weekly report (declared: 1h)
SELECT pgtrickle.create_stream_table(
  name     => 'weekly_report',
  query    => $$
    SELECT
      date_trunc('week', last_order) AS week,
      COUNT(*) FILTER (WHERE order_count >= 5) AS repeat_customers
    FROM customer_metrics
    GROUP BY 1
  $$,
  schedule => '1h'
);
```

The effective schedules:
- `valid_orders`: CALCULATED → 5s (from `dashboard_summary` via `customer_metrics`)
- `customer_metrics`: CALCULATED → 5s (from `dashboard_summary`)
- `dashboard_summary`: 5s (declared)
- `weekly_report`: 1h (declared)

If you remove `dashboard_summary`, the CALCULATED tables relax to 1-hour cadence — because `weekly_report` is now the tightest consumer. No manual intervention needed.

---

## The Default Schedule

When a CALCULATED stream table has no downstream consumers (it's a leaf that nobody else references), it falls back to `pg_trickle.default_schedule_seconds`. The default is 1 second.

This is intentional: CALCULATED tables are designed as intermediate nodes. If they're currently leaves, it's because the downstream consumer hasn't been created yet. Refreshing them at 1-second cadence ensures they're ready when the consumer arrives.

If you don't want this behavior — maybe it's a staging table you're building incrementally — set an explicit schedule instead:

```sql
SELECT pgtrickle.alter_stream_table(
  name     => 'staging_data',
  schedule => '30s'  -- explicit, not CALCULATED
);
```

---

## CALCULATED + Diamond Dependencies

CALCULATED scheduling composes correctly with diamond dependencies. If two branches of the DAG converge at a common node:

```
A (5s) ──→ C (CALCULATED → 5s) ──→ E (10s)
B (2s) ──→ D (CALCULATED → 2s) ──→ E
```

Here:
- `C` inherits 5s from `A` (but is also consumed by `E` at 10s; the tightest is 5s from `A`'s side — wait, let me clarify). Actually: `C`'s schedule is CALCULATED based on its consumers. `E` consumes `C` with a 10s schedule. So `C` gets 10s? No — `C` also gets its own upstream's perspective.

Let me be precise. CALCULATED propagates *downstream demand* backward. `C` is consumed by `E` (10s). `D` is consumed by `E` (10s). So both `C` and `D` get 10s.

But `A` has a declared 5s schedule. And `B` has a declared 2s schedule. These are source schedules, not CALCULATED. The scheduler refreshes `A` every 5s and `B` every 2s. `C` and `D` refresh every 10s because that's what `E` needs.

The result: `A` and `B` refresh more often than `C` and `D`. The change buffers accumulate between C/D refreshes. When C and D refresh at 10s cadence, they process all accumulated changes in one batch. This is efficient — no wasted intermediate refreshes.

---

## Monitoring Effective Schedules

You can see both declared and effective schedules:

```sql
SELECT
  name,
  schedule AS declared,
  effective_schedule
FROM pgtrickle.pgt_status()
WHERE schedule = 'CALCULATED' OR schedule != effective_schedule;
```

```
      name       | declared   | effective_schedule
-----------------+------------+--------------------
 valid_orders    | CALCULATED | 5s
 customer_metrics| CALCULATED | 5s
```

If a CALCULATED table's effective schedule seems wrong, check the dependency tree:

```sql
SELECT * FROM pgtrickle.dependency_tree('valid_orders');
```

This shows the full DAG from the table to its consumers, making it easy to trace which consumer is driving the cadence.

---

## When Not to Use CALCULATED

CALCULATED scheduling works well for intermediate pipeline tables. Don't use it for:

- **Leaf tables that are queried directly.** Give them an explicit schedule that matches your freshness requirement.
- **Tables with side effects on refresh** (e.g., stream tables feeding the outbox). These need a predictable, declared cadence.
- **Debugging.** When troubleshooting, explicit schedules are easier to reason about. Switch to CALCULATED after things stabilize.

---

## Summary

CALCULATED scheduling inverts the refresh-planning problem. Instead of figuring out how fast each intermediate table needs to refresh, you declare freshness requirements on the tables you actually consume, and the DAG propagation derives everything else.

Add a new real-time consumer? The upstream tables speed up. Remove it? They slow down. Change the SLA? The whole pipeline adjusts.

One declaration, automatic propagation, zero manual tuning.
