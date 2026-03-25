# Configuration

Complete reference for all pg_trickle GUC (Grand Unified Configuration) variables.

---

## Table of Contents

- [Overview](#overview)
- [GUC Variables](#guc-variables)
  - [Essential](#essential)
    - [pg\_trickle.enabled](#pg_trickleenabled)
    - [pg\_trickle.cdc\_mode](#pg_tricklecdc_mode)
    - [pg\_trickle.scheduler\_interval\_ms](#pg_tricklescheduler_interval_ms)
    - [pg\_trickle.event\_driven\_wake](#pg_trickleevent_driven_wake)
    - [pg\_trickle.wake\_debounce\_ms](#pg_tricklewake_debounce_ms)
    - [pg\_trickle.min\_schedule\_seconds](#pg_tricklemin_schedule_seconds)
    - [pg\_trickle.default\_schedule\_seconds](#pg_trickledefault_schedule_seconds)
    - [pg\_trickle.max\_consecutive\_errors](#pg_tricklemax_consecutive_errors)
  - [WAL CDC](#wal-cdc)
    - [pg\_trickle.wal\_transition\_timeout](#pg_tricklewal_transition_timeout)
    - [pg\_trickle.slot\_lag\_warning\_threshold\_mb](#pg_trickleslot_lag_warning_threshold_mb)
    - [pg\_trickle.slot\_lag\_critical\_threshold\_mb](#pg_trickleslot_lag_critical_threshold_mb)
  - [Refresh Performance](#refresh-performance)
    - [pg\_trickle.differential\_max\_change\_ratio](#pg_trickledifferential_max_change_ratio)
    - [pg\_trickle.merge\_planner\_hints](#pg_tricklemerge_planner_hints)
    - [pg\_trickle.merge\_work\_mem\_mb](#pg_tricklemerge_work_mem_mb)
    - [pg\_trickle.merge\_seqscan\_threshold](#pg_tricklemerge_seqscan_threshold)
    - [pg\_trickle.auto\_backoff](#pg_trickleauto_backoff)
    - [pg\_trickle.tiered\_scheduling](#pg_trickletiered_scheduling)
    - [pg\_trickle.cleanup\_use\_truncate](#pg_tricklecleanup_use_truncate)
    - [pg\_trickle.use\_prepared\_statements](#pg_trickleuse_prepared_statements)
    - [pg\_trickle.user\_triggers](#pg_trickleuser_triggers)
  - [Guardrails & Limits](#guardrails--limits)
    - [pg\_trickle.block\_source\_ddl](#pg_trickleblock_source_ddl)
    - [pg\_trickle.buffer\_alert\_threshold](#pg_tricklebuffer_alert_threshold)
    - [pg\_trickle.compact\_threshold](#pg_tricklecompact_threshold)
    - [pg\_trickle.max\_grouping\_set\_branches](#pg_tricklemax_grouping_set_branches)
    - [pg\_trickle.ivm\_topk\_max\_limit](#pg_trickleivm_topk_max_limit)
    - [pg\_trickle.ivm\_recursive\_max\_depth](#pg_trickleivm_recursive_max_depth)
  - [Parallel Refresh](#parallel-refresh)
    - [pg\_trickle.parallel\_refresh\_mode](#pg_trickleparallel_refresh_mode)
    - [pg\_trickle.max\_dynamic\_refresh\_workers](#pg_tricklemax_dynamic_refresh_workers)
    - [pg\_trickle.max\_concurrent\_refreshes](#pg_tricklemax_concurrent_refreshes)
  - [Advanced / Internal](#advanced--internal)
    - [pg\_trickle.change\_buffer\_schema](#pg_tricklechange_buffer_schema)
    - [pg\_trickle.foreign\_table\_polling](#pg_trickleforeign_table_polling)
  - [Circular Dependencies](#circular-dependencies)
    - [pg\_trickle.allow\_circular](#pg_trickleallow_circular)
    - [pg\_trickle.max\_fixpoint\_iterations](#pg_tricklemax_fixpoint_iterations)
- [GUC Interaction Matrix](#guc-interaction-matrix)
- [Tuning Profiles](#tuning-profiles)
  - [Low-Latency Profile](#low-latency-profile)
  - [High-Throughput Profile](#high-throughput-profile)
  - [Resource-Constrained Profile](#resource-constrained-profile)
- [Complete postgresql.conf Example](#complete-postgresqlconf-example)
- [Runtime Configuration](#runtime-configuration)
- [Further Reading](#further-reading)

---

## Overview

pg_trickle exposes twenty-three configuration variables in the `pg_trickle` namespace. All can be set in `postgresql.conf` or at runtime via `SET` / `ALTER SYSTEM`.

**Required `postgresql.conf` settings:**

```ini
shared_preload_libraries = 'pg_trickle'
```

The extension **must** be loaded via `shared_preload_libraries` because it registers GUC variables and a background worker at startup.

> **Note:** `wal_level = logical` and `max_replication_slots` are recommended but **not** required. The default CDC mode (`auto`) uses lightweight row-level triggers initially and transparently transitions to WAL-based capture if `wal_level = logical` is available. If `wal_level` is not `logical`, pg_trickle stays on triggers permanently — no degradation, no errors. Set `pg_trickle.cdc_mode = 'trigger'` to disable WAL transitions entirely (see [pg_trickle.cdc_mode](#pg_tricklecdc_mode)).

---

## GUC Variables

### Essential

The settings most users configure at install time.

---

### pg_trickle.enabled

Enable or disable the pg_trickle extension.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `true` |
| Context | `SUSET` (superuser) |
| Restart Required | No |

When set to `false`, the background scheduler stops processing refreshes. Existing stream tables remain in the catalog but are not refreshed. Manual `pgtrickle.refresh_stream_table()` calls still work.

```sql
-- Disable automatic refreshes
SET pg_trickle.enabled = false;

-- Re-enable
SET pg_trickle.enabled = true;
```

---

### pg_trickle.cdc_mode

CDC (Change Data Capture) mechanism selection.

| Value | Description |
|-------|-------------|
| `'auto'` | **(default)** Use triggers for creation; transition to WAL-based CDC if `wal_level = logical`. Falls back to triggers automatically on error. |
| `'trigger'` | Always use row-level triggers for change capture |
| `'wal'` | Require WAL-based CDC (fails if `wal_level != logical`) |

**Default:** `'auto'`

`pg_trickle.cdc_mode` only affects deferred refresh modes (`'AUTO'`, `'FULL'`,
and `'DIFFERENTIAL'`). `refresh_mode = 'IMMEDIATE'` bypasses CDC entirely and
always uses statement-level IVM triggers. If the GUC is set to `'wal'` when a
stream table is created or altered to `IMMEDIATE`, pg_trickle logs an INFO and
continues with IVM triggers instead of creating CDC triggers or WAL slots.

Per-stream-table overrides take precedence over the GUC when you pass
`cdc_mode => 'auto' | 'trigger' | 'wal'` to
`pgtrickle.create_stream_table(...)` or `pgtrickle.alter_stream_table(...)`.
The override is stored in `pgtrickle.pgt_stream_tables.requested_cdc_mode`.
For shared source tables, pg_trickle resolves the effective source-level CDC
mechanism conservatively: any dependent stream table that requests `'trigger'`
keeps the source on trigger CDC; otherwise `'wal'` wins over `'auto'`.

```sql
-- Enable automatic trigger → WAL transition (default)
SET pg_trickle.cdc_mode = 'auto';

-- Force trigger-only CDC (disable WAL transitions)
SET pg_trickle.cdc_mode = 'trigger';

-- Require WAL-based CDC (error if wal_level != logical)
SET pg_trickle.cdc_mode = 'wal';
```

---

### pg_trickle.scheduler_interval_ms

How often the background scheduler checks for stream tables that need refreshing.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `1000` (1 second) |
| Range | `100` – `60000` (100ms to 60s) |
| Context | `SUSET` |
| Restart Required | No |

**Tuning Guidance:**
- **Low-latency workloads** (sub-second schedule): Set to `100`–`500`.
- **Standard workloads** (minutes of schedule): Default `1000` is appropriate.
- **Low-overhead workloads** (many STs with long schedules): Increase to `5000`–`10000` to reduce scheduler overhead.

The scheduler interval does **not** determine refresh frequency — it determines how often the scheduler *checks* whether any ST's staleness exceeds its schedule (or whether a cron expression has fired). The actual refresh frequency is governed by `schedule` (duration or cron) and canonical period alignment.

```sql
SET pg_trickle.scheduler_interval_ms = 500;
```

---

### pg_trickle.event_driven_wake

Enable event-driven scheduler wake via LISTEN/NOTIFY. When enabled, CDC triggers emit `pg_notify('pgtrickle_wake', '')` after writing to the change buffer, and the scheduler LISTENs on that channel, waking immediately instead of waiting for the full `scheduler_interval_ms` poll. This reduces median end-to-end latency from ~500 ms to ~15 ms for low-volume workloads.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `true` |
| Context | `SUSET` |
| Restart Required | No |

**Tuning Guidance:**
- **Low-latency workloads**: Leave enabled (default) for the best latency.
- **Extreme write throughput** (>100K DML/s): Consider disabling if the per-statement NOTIFY overhead is measurable. The NOTIFY is coalesced by PostgreSQL (one notification per transaction), so the actual overhead is negligible for most workloads.

```sql
-- Disable event-driven wake (fall back to poll-only)
SET pg_trickle.event_driven_wake = off;
```

---

### pg_trickle.wake_debounce_ms

After the scheduler receives the first `pgtrickle_wake` notification, it waits this many milliseconds to coalesce rapidly arriving notifications before starting a refresh tick. Lower values reduce latency; higher values reduce wake overhead during bulk DML.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `10` (10 milliseconds) |
| Range | `1` – `5000` |
| Context | `SUSET` |
| Restart Required | No |

**Tuning Guidance:**
- **Single-statement latency-sensitive**: Use `1`–`5` ms.
- **Bulk DML workloads**: Use `50`–`200` ms to coalesce more notifications per tick.
- **Default** (`10` ms) balances sub-20 ms latency with reasonable coalescing.

```sql
SET pg_trickle.wake_debounce_ms = 50;
```

---

### pg_trickle.min_schedule_seconds

Minimum allowed `schedule` value (in seconds) when creating or altering a stream table with a duration-based schedule. This limit does **not** apply to cron expressions.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `1` (1 second) |
| Range | `1` – `86400` (1 second to 24 hours) |
| Context | `SUSET` |
| Restart Required | No |

This acts as a safety guardrail to prevent users from setting impractically small schedules that would cause excessive refresh overhead.

**Tuning Guidance:**
- **Development/testing**: Default `1` allows sub-second testing.
- **Production**: Raise to `60` or higher to prevent excessive WAL consumption and CPU usage.

```sql
-- Restrict to 10-second minimum schedules
SET pg_trickle.min_schedule_seconds = 10;
```

---

### pg_trickle.default_schedule_seconds

Default effective schedule (in seconds) for isolated CALCULATED stream tables that have no downstream dependents.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `1` (1 second) |
| Range | `1` – `86400` (1 second to 24 hours) |
| Context | `SUSET` |
| Restart Required | No |

When a CALCULATED stream table (scheduled with `'calculated'`) has no downstream dependents to derive a schedule from, this value is used as its effective refresh interval. This is distinct from `min_schedule_seconds`, which is the validation **floor** for duration-based schedules.

**Tuning Guidance:**
- **Development/testing**: Default `1` allows rapid iteration.
- **Production standalone CALCULATED tables**: Raise to match your desired update cadence (e.g., `60` for once-per-minute).

```sql
-- Set default for isolated CALCULATED tables to 30 seconds
SET pg_trickle.default_schedule_seconds = 30;
```

---

### pg_trickle.max_consecutive_errors

Maximum consecutive refresh failures before a stream table is moved to `ERROR` status.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `3` |
| Range | `1` – `100` |
| Context | `SUSET` |
| Restart Required | No |

When a ST's `consecutive_errors` reaches this threshold:
1. The ST status changes to `ERROR`.
2. Automatic refreshes stop for this ST.
3. Manual intervention is required: `SELECT pgtrickle.alter_stream_table('...', status => 'ACTIVE')`.

**Tuning Guidance:**
- **Strict** (production): `3` — fail fast to surface issues.
- **Lenient** (development): `10`–`20` — tolerate transient errors.

```sql
SET pg_trickle.max_consecutive_errors = 5;
```

---

### WAL CDC

Settings specific to WAL-based CDC. Only relevant when `pg_trickle.cdc_mode = 'auto'` or `'wal'`.

---

### pg_trickle.wal_transition_timeout

> **Note:** This setting is only relevant when `pg_trickle.cdc_mode = 'auto'` or `'wal'`. See
> [ARCHITECTURE.md](ARCHITECTURE.md) for the full CDC transition lifecycle.

Maximum time (seconds) to wait for the WAL decoder to catch up during
the transition from trigger-based to WAL-based CDC. If the decoder has
not caught up within this timeout, the system falls back to triggers.

**Default:** `300` (5 minutes)  
**Range:** `10` – `3600`

```sql
SET pg_trickle.wal_transition_timeout = 300;
```

---

### pg_trickle.slot_lag_warning_threshold_mb

Warning threshold for retained WAL on pg_trickle replication slots.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `100` (MB) |
| Range | `1` – `1048576` |
| Context | `SUSET` |
| Restart Required | No |

When retained WAL for a pg_trickle replication slot exceeds this threshold:
- The scheduler emits a `slot_lag_warning` event on `LISTEN pg_trickle_alert`
- `pgtrickle.health_check()` reports `WARN` for the `slot_lag` check

Raise this on high-throughput systems that intentionally tolerate larger WAL retention. Lower it if you want earlier warning before slots risk invalidation.

```sql
SET pg_trickle.slot_lag_warning_threshold_mb = 256;
```

---

### pg_trickle.slot_lag_critical_threshold_mb

Critical threshold for retained WAL on pg_trickle replication slots.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `1024` (MB) |
| Range | `1` – `1048576` |
| Context | `SUSET` |
| Restart Required | No |

When retained WAL for a pg_trickle replication slot exceeds this threshold,
`pgtrickle.check_cdc_health()` returns a per-source
`slot_lag_exceeds_threshold` alert.

This threshold is intentionally higher than the warning threshold so operators can separate early warning from source-level unhealthy state.

```sql
SET pg_trickle.slot_lag_critical_threshold_mb = 2048;
```

---

### Refresh Performance

Fine-grained tuning for the differential refresh engine.

---

### pg_trickle.differential_max_change_ratio

Maximum change-to-table ratio before DIFFERENTIAL refresh falls back to FULL refresh.

| Property | Value |
|---|---|
| Type | `float` |
| Default | `0.15` (15%) |
| Range | `0.0` – `1.0` |
| Context | `SUSET` |
| Restart Required | No |

When the number of pending change buffer rows exceeds this fraction of the source table's estimated row count, the refresh engine switches from DIFFERENTIAL (which uses JSONB parsing and window functions) to FULL refresh. At high change rates FULL refresh is cheaper because it avoids the per-row JSONB overhead.

**Special Values:**
- **`0.0`**: Disable adaptive fallback — always use DIFFERENTIAL.
- **`1.0`**: Always fall back to FULL (effectively forces FULL mode).

**Tuning Guidance:**
- **OLTP with low change rates** (< 5%): Default `0.15` is appropriate.
- **Batch-load workloads** (bulk inserts): Lower to `0.05`–`0.10` so large batches trigger FULL refresh sooner.
- **Latency-sensitive** (want deterministic refresh time): Set to `0.0` to always use DIFFERENTIAL.

```sql
-- Lower threshold for batch-heavy workloads
SET pg_trickle.differential_max_change_ratio = 0.10;

-- Disable adaptive fallback
SET pg_trickle.differential_max_change_ratio = 0.0;
```

---

### pg_trickle.cleanup_use_truncate

Use `TRUNCATE` instead of per-row `DELETE` for change buffer cleanup when the entire buffer is consumed by a refresh.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `true` |
| Context | `SUSET` |
| Restart Required | No |

After a differential refresh consumes all rows from the change buffer, the engine must clean up the buffer table. `TRUNCATE` is O(1) regardless of row count, versus `DELETE` which must update indexes row-by-row. This saves 3–5 ms per refresh at 10%+ change rates.

**Trade-off:** `TRUNCATE` acquires an `AccessExclusiveLock` on the change buffer table. If concurrent DML on the source table is actively inserting into the same change buffer via triggers, this lock can cause brief contention.

**Tuning Guidance:**
- **Most workloads**: Leave at `true` — the performance benefit outweighs the brief lock.
- **High-concurrency OLTP** with continuous writes during refresh: Set to `false` if you observe lock-wait timeouts on the change buffer.

```sql
-- Use per-row DELETE for change buffer cleanup
SET pg_trickle.cleanup_use_truncate = false;
```

---

### pg_trickle.merge_planner_hints

Inject `SET LOCAL` planner hints before MERGE execution during differential refresh.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `true` |
| Context | `SUSET` |
| Restart Required | No |

When enabled, the refresh executor estimates the delta size and applies optimizer hints within the transaction:
- **Delta ≥ 100 rows**: `SET LOCAL enable_nestloop = off` — forces hash joins instead of nested-loop joins.
- **Delta ≥ 10,000 rows**: additionally `SET LOCAL work_mem = '<N>MB'` (see [pg_trickle.merge_work_mem_mb](#pg_tricklemerge_work_mem_mb)).

This reduces P95 latency spikes caused by PostgreSQL choosing nested-loop plans for medium/large delta sizes.

**Tuning Guidance:**
- **Most workloads**: Leave at `true` — the hints improve tail latency without affecting small deltas.
- **Custom plan overrides**: Set to `false` if you manage planner settings yourself or if the hints conflict with your `pg_hint_plan` configuration.

```sql
-- Disable planner hints
SET pg_trickle.merge_planner_hints = false;
```

---

### pg_trickle.merge_work_mem_mb

`work_mem` value (in MB) applied via `SET LOCAL` when the delta exceeds 10,000 rows and [planner hints](#pg_tricklemerge_planner_hints) are enabled.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `64` (64 MB) |
| Range | `8` – `4096` (8 MB to 4 GB) |
| Context | `SUSET` |
| Restart Required | No |

A higher value lets PostgreSQL use larger in-memory hash tables for the MERGE join, avoiding disk-spilling sort/merge strategies on large deltas. This setting is only applied when both `merge_planner_hints = true` and the delta exceeds 10,000 rows.

**Tuning Guidance:**
- **Servers with ample RAM** (32+ GB): Increase to `128`–`256` for faster large-delta refreshes.
- **Memory-constrained**: Lower to `16`–`32` or disable planner hints entirely.
- **Very large deltas** (100K+ rows): Consider `256`–`512` if refresh latency matters.

```sql
SET pg_trickle.merge_work_mem_mb = 128;
```

---

### pg_trickle.merge_seqscan_threshold

Delta-to-ST row ratio below which sequential scans are disabled for the MERGE transaction. Requires [planner hints](#pg_tricklemerge_planner_hints) to be enabled.

| Property | Value |
|---|---|
| Type | `real` |
| Default | `0.001` |
| Range | `0.0` – `1.0` |
| Context | `SUSET` |
| Restart Required | No |

When the estimated delta row count divided by the stream table's `reltuples` falls below this threshold, the refresh executor issues `SET LOCAL enable_seqscan = off`, coercing PostgreSQL into using the `__pgt_row_id` B-tree index instead of a full sequential scan.

Set to `0.0` to disable the feature entirely.

**Tuning Guidance:**
- **Default (`0.001`)**: Suitable for most workloads. A 10M-row ST with fewer than 10K delta rows triggers the hint.
- **High-throughput / small STs**: Increase to `0.01` if your STs are small and you want more aggressive index usage.
- **Disable**: Set to `0.0` if index-only scans are not beneficial for your access pattern.

```sql
SET pg_trickle.merge_seqscan_threshold = 0.01;
```

---

### pg_trickle.auto_backoff

Automatically back off the refresh schedule when a stream table is consistently falling behind.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `on` |
| Context | `SUSET` |
| Restart Required | No |

When enabled (the default), the scheduler tracks a per-stream-table backoff factor. If a
refresh cycle takes more than **95%** of the scheduled interval, the backoff factor doubles
(capped at **8×**), effectively stretching the schedule to avoid runaway refresh storms.
The factor resets to 1× on the first on-time completion, and a `WARNING` is emitted whenever
the factor changes so you always know why a stream table is refreshing more slowly than expected.

The 95% trigger threshold means that brief jitter on developer machines (e.g. a 950 ms refresh
on a 1-second schedule) will correctly engage backoff, while a 900 ms refresh on the same
schedule will not. The EC-11 operator alert (`scheduler_falling_behind` NOTIFY) continues to
fire at the lower 80% threshold, giving you advance warning before the scheduler is actually stuck.

This is a safety net for overloaded systems — it prevents a single slow stream table from
monopolizing the background worker when operators are not available to intervene.

**Tuning Guidance:**
- **Leave on** (the default) for both production and development environments.
- **Disable** only if you are deliberately running stream tables at the limit of their schedule
  budget and want the scheduler to keep trying at full speed regardless.

```sql
-- Disable if you want no backoff (not recommended for production)
SET pg_trickle.auto_backoff = off;
```

---

### pg_trickle.tiered_scheduling

Enable tiered refresh scheduling (Hot/Warm/Cold/Frozen) for stream tables.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `off` |
| Context | `SUSET` |
| Restart Required | No |

When enabled, the scheduler applies a per-stream-table refresh tier multiplier
to duration-based schedules. Each stream table has a `refresh_tier` column
(default `'hot'`) that controls how often it is refreshed relative to its
configured schedule:

| Tier | Multiplier | Effect |
|------|-----------|--------|
| `hot` | 1× | Refresh at configured schedule (default) |
| `warm` | 2× | Refresh at 2× the configured interval |
| `cold` | 10× | Refresh at 10× the configured interval |
| `frozen` | skip | Never refreshed until manually promoted |

Cron-based schedules are not affected by the tier multiplier.

Set the tier via:
```sql
SELECT pgtrickle.alter_stream_table('my_table', tier => 'warm');
SELECT pgtrickle.alter_stream_table('my_table', tier => 'frozen');
```

**Design note:** Tiers are user-assigned only. Automatic classification from
`pg_stat_user_tables` was rejected because pg_trickle's own MERGE scans
pollute the read counters, making auto-classification unreliable.

---

### Diamond Schedule Policy (per-stream-table)

Controls how the scheduler fires diamond consistency groups — sets of stream
tables that share upstream sources through a diamond-shaped DAG topology.

| Property | Value |
|---|---|
| Column | `diamond_schedule_policy` in `pgt_stream_tables` |
| Values | `'fastest'` (default), `'slowest'` |
| Set via | `create_stream_table(..., diamond_schedule_policy => 'slowest')` |
| Alter via | `alter_stream_table('name', diamond_schedule_policy => 'slowest')` |

Only meaningful when `diamond_consistency = 'atomic'` is also set.

**`fastest` (default):** The atomic group fires when **any** member is due.
This maximizes freshness but can cause CPU multiplication. In an asymmetric
diamond where stream table B refreshes every 1 s and stream table C every 5 s,
both feeding D with `diamond_consistency = 'atomic'`: C refreshes **5× more
often than its schedule** because B triggers the group every second. For N
members with schedules S₁ < S₂ < … < Sₙ, the total refresh count is
N × (cycle_time / S₁), meaning slower members do up to Sₙ/S₁ times more work
than their schedule implies.

**`slowest`:** The atomic group fires only when **all** members are due.
This minimizes CPU cost at the expense of freshness — faster members are held
back until the slowest member's schedule fires.

**Tuning Guidance:**
- Use `'fastest'` when freshness of the diamond tip matters and the cost of
  extra refreshes is acceptable.
- Use `'slowest'` when CPU budget is tight or members have very different
  schedules (e.g., 1 s vs 60 s) and the multiplication would be excessive.

```sql
-- Create with slowest policy to avoid CPU multiplication
SELECT pgtrickle.create_stream_table(
    'my_diamond_tip',
    'SELECT ... FROM a JOIN b ...',
    diamond_consistency => 'atomic',
    diamond_schedule_policy => 'slowest'
);
```

---

### pg_trickle.use_prepared_statements

Use SQL `PREPARE` / `EXECUTE` for MERGE statements during differential refresh.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `true` |
| Context | `SUSET` |
| Restart Required | No |

When enabled, the refresh executor issues `PREPARE __pgt_merge_{id}` on the first cache-hit cycle, then uses `EXECUTE` on subsequent cycles. After approximately 5 executions, PostgreSQL switches from a custom plan to a generic plan, saving 1–2 ms of parse/plan overhead per refresh.

**Tuning Guidance:**
- **Most workloads**: Leave at `true` — the cumulative parse/plan savings are significant for frequently-refreshed stream tables.
- **Highly skewed data**: Set to `false` if prepared-statement parameter sniffing produces poor plans (e.g., highly skewed LSN distributions causing bad join estimates).

```sql
-- Disable prepared statements
SET pg_trickle.use_prepared_statements = false;
```

---

### pg_trickle.user_triggers

Control how user-defined triggers on stream tables are handled during refresh.

| Property | Value |
|---|---|
| Type | `text` |
| Default | `'auto'` |
| Values | `'auto'`, `'off'` (`'on'` accepted as deprecated alias for `'auto'`) |
| Context | `SUSET` |
| Restart Required | No |

When a stream table has user-defined row-level triggers, the refresh engine can decompose the `MERGE` into explicit `DELETE` + `UPDATE` + `INSERT` statements so triggers fire with correct `TG_OP`, `OLD`, and `NEW` values.

**Values:**
- **`auto`** (default): Automatically detect user triggers on the stream table. If present, use the explicit DML path; otherwise use `MERGE`.
- **`off`**: Always use `MERGE`. User triggers are suppressed during refresh. This is the escape hatch if explicit DML causes issues.
- **`on`**: Deprecated compatibility alias for `auto`. Existing configs continue to work, but new configs should use `auto`.

**Notes:**
- Row-level triggers do **not** fire during FULL refresh regardless of this setting. FULL refresh uses `DISABLE TRIGGER USER` / `ENABLE TRIGGER USER` to suppress them.
- The explicit DML path adds ~25–60% overhead compared to MERGE for affected stream tables.
- Stream tables without user triggers have zero overhead when using `auto` (only a fast `pg_trigger` check).

```sql
-- Auto-detect (default)
SET pg_trickle.user_triggers = 'auto';

-- Suppress triggers, use MERGE
SET pg_trickle.user_triggers = 'off';

-- Backward-compatible legacy setting (treated the same as 'auto')
SET pg_trickle.user_triggers = 'on';
```

---

### Guardrails & Limits

Safety controls and hard limits.

---

### pg_trickle.block_source_ddl

When enabled, column-affecting DDL (e.g., `ALTER TABLE ... DROP COLUMN`,
`ALTER TABLE ... ALTER COLUMN ... TYPE`) on source tables tracked by stream
tables is **blocked** with an ERROR instead of silently marking stream tables
for reinitialization.

This is useful in production environments where you want to prevent accidental
schema changes that would trigger expensive full recomputation of downstream
stream tables.

**Default:** `false`  
**Context:** Superuser

```sql
-- Block column-affecting DDL on tracked source tables
SET pg_trickle.block_source_ddl = true;

-- Allow DDL (stream tables will be marked for reinit instead)
SET pg_trickle.block_source_ddl = false;
```

> **Note:** Only column-affecting changes are blocked. Benign DDL (adding
> indexes, comments, constraints) is always allowed regardless of this setting.

---

### pg_trickle.buffer_alert_threshold

When any source table's change buffer exceeds this number of rows, a
`BufferGrowthWarning` alert is emitted. Raise for high-throughput workloads,
lower for small tables.

**Default:** `1000000` (1 million rows)  
**Range:** `1000` – `100000000`

```sql
SET pg_trickle.buffer_alert_threshold = 500000;
```

---

### pg_trickle.compact_threshold

When a source table's pending change buffer exceeds this many rows,
compaction is triggered before the next refresh cycle. Compaction eliminates
net-zero INSERT+DELETE pairs (rows inserted then deleted within the same
refresh window) and collapses multi-change groups to first+last rows per
`pk_hash`, reducing delta scan overhead by 50–90% for high-churn tables.

Set to `0` to disable compaction.

**Default:** `100000` (100K rows)  
**Range:** `0` – `100000000`

```sql
-- Trigger compaction at 50K pending rows
SET pg_trickle.compact_threshold = 50000;

-- Disable compaction
SET pg_trickle.compact_threshold = 0;
```

---

### pg_trickle.max_grouping_set_branches

Maximum allowed grouping set branches in `CUBE`/`ROLLUP` queries.
`CUBE(n)` produces $2^n$ branches — without a limit, large cubes cause
memory exhaustion during parsing. Users who genuinely need more than
64 branches can raise this GUC.

**Default:** `64`  
**Range:** `1` – `65536`

```sql
-- Allow up to 128 grouping set branches
SET pg_trickle.max_grouping_set_branches = 128;
```

---

### pg_trickle.ivm_topk_max_limit

Maximum `LIMIT` value for TopK stream tables in **IMMEDIATE** mode.
TopK queries exceeding this threshold are rejected because the inline
micro-refresh (recomputing top-K rows on every DML statement) adds
latency proportional to `LIMIT`. Set to `0` to disable TopK in
IMMEDIATE mode entirely.

**Default:** `1000`  
**Range:** `0` – `1000000`

```sql
-- Allow TopK up to LIMIT 5000 in IMMEDIATE mode
SET pg_trickle.ivm_topk_max_limit = 5000;
```

---

### pg_trickle.ivm_recursive_max_depth

Maximum recursion depth for `WITH RECURSIVE` queries in **IMMEDIATE** mode.
The semi-naive evaluation injects a `__pgt_depth` counter column into the
recursive SQL; iteration stops when the counter reaches this limit. Protects
against infinite recursion in pathological graphs.

**Default:** `100`  
**Range:** `1` – `10000`

```sql
-- Allow deeper recursion for large hierarchies
SET pg_trickle.ivm_recursive_max_depth = 500;
```

---

## Parallel Refresh

These settings control whether and how the scheduler dispatches refresh
work to multiple dynamic background workers instead of processing
stream tables sequentially. See
[PLAN_PARALLELISM.md](../plans/sql/PLAN_PARALLELISM.md) for the design.

> **Note:** Parallel refresh is new in v0.4.0 and defaults to `off`. Enable
> it via `pg_trickle.parallel_refresh_mode` after validating your workload.

### pg_trickle.parallel_refresh_mode

Controls whether the scheduler dispatches refresh work to dynamic
background workers.

| Property | Value |
|---|---|
| Type | `text` |
| Default | `'off'` |
| Values | `'off'`, `'dry_run'`, `'on'` |
| Context | `SUSET` |
| Restart Required | No |

- **`off`** (default): Sequential execution. All stream tables are
  refreshed one at a time in topological order by the single scheduler
  background worker. This is the proven, stable default.
- **`dry_run`**: The scheduler computes execution units, logs dispatch
  decisions (unit keys, ready-queue contents, budget), but still executes
  refreshes inline. Useful for previewing parallel behaviour without
  actually spawning workers.
- **`on`**: True parallel refresh. The coordinator builds an execution-unit
  DAG, dispatches ready units to dynamic background workers, and respects
  both the per-database cap (`max_concurrent_refreshes`) and the
  cluster-wide cap (`max_dynamic_refresh_workers`).

```sql
-- Preview parallel dispatch decisions without changing runtime behaviour
SET pg_trickle.parallel_refresh_mode = 'dry_run';

-- Enable parallel refresh
SET pg_trickle.parallel_refresh_mode = 'on';
```

---

### pg_trickle.max_dynamic_refresh_workers

Cluster-wide cap on concurrently active pg_trickle dynamic refresh workers.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `4` |
| Range | `0` – `64` |
| Context | `SUSET` |
| Restart Required | No |

This is distinct from `pg_trickle.max_concurrent_refreshes` (per-database
cap). When multiple databases each have their own scheduler, this GUC
prevents them from overcommitting the shared PostgreSQL
`max_worker_processes` budget.

**Worker-budget planning:** Each dynamic refresh worker consumes one
`max_worker_processes` slot. In addition, pg_trickle uses one slot for
the launcher and one per-database scheduler. Ensure:

```
max_worker_processes >= pg_trickle launchers (1)
                      + pg_trickle schedulers (1 per database)
                      + max_dynamic_refresh_workers
                      + autovacuum workers
                      + parallel query workers
                      + other extensions
```

A typical small deployment (1–2 databases, 4 parallel workers) needs at
least `max_worker_processes = 16`. The E2E test Docker image uses 128.

```sql
-- Allow up to 8 concurrent refresh workers cluster-wide
SET pg_trickle.max_dynamic_refresh_workers = 8;
```

---

### pg_trickle.max_concurrent_refreshes

Per-database dispatch cap for parallel refresh workers.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `4` |
| Range | `1` – `32` |
| Context | `SUSET` |
| Restart Required | No |

When `parallel_refresh_mode = 'on'`, this limits how many execution units
a single database coordinator may have in-flight at the same time. In
sequential mode (`parallel_refresh_mode = 'off'`), this setting has no
effect.

The effective concurrent refreshes for a database is:

```
min(max_concurrent_refreshes, max_dynamic_refresh_workers - workers_used_by_other_dbs)
```

```sql
-- Allow up to 8 concurrent refreshes in this database
SET pg_trickle.max_concurrent_refreshes = 8;
```

---

## Advanced / Internal

### pg_trickle.change_buffer_schema

Schema name for change-buffer tables created by the trigger-based CDC
pipeline.

**Default:** `'pgtrickle_changes'`

Change buffer tables are named `<schema>.changes_<oid>` where `<oid>` is
the source table's OID. Placing them in a dedicated schema keeps them out
of the `public` namespace.

```sql
SET pg_trickle.change_buffer_schema = 'my_change_buffers';
```

---

### pg_trickle.foreign_table_polling

Enable polling-based change detection for foreign table sources. When
enabled, the scheduler periodically re-executes the foreign table query
and computes deltas via snapshot comparison (`EXCEPT ALL`). Foreign tables
cannot use trigger or WAL-based CDC, so this is the only mechanism for
incremental maintenance.

**Default:** `false`

```sql
SET pg_trickle.foreign_table_polling = true;
```

---

### Circular Dependencies

> **v0.7.0+** — Circular dependency support is now fully available for safe
> monotone cycles in DIFFERENTIAL mode. These settings control whether cycles
> are allowed at all and how many fixpoint iterations the scheduler will try
> before surfacing a non-convergence error.

### pg_trickle.allow_circular

Master switch for circular (cyclic) stream table dependencies. When `false`
(default), creating a stream table that would introduce a cycle in the
dependency graph is rejected with a `CycleDetected` error. When `true`,
monotone cycles — those containing only safe operators (joins, filters,
projections, UNION ALL, INTERSECT, EXISTS) — are allowed.

Non-monotone operators (Aggregate, EXCEPT, Window functions, NOT EXISTS)
always block cycle creation regardless of this setting, because they cannot
guarantee convergence to a fixed point.

**Default:** `false`

```sql
SET pg_trickle.allow_circular = true;
```

### pg_trickle.max_fixpoint_iterations

Maximum number of iterations per strongly connected component (SCC) before
the scheduler declares non-convergence and marks all SCC members as ERROR.
Prevents runaway loops in circular dependency chains.

For most practical use cases (transitive closure, graph reachability),
convergence happens in 2–5 iterations. The default of 100 provides ample
headroom.

**Default:** `100`  
**Range:** `1` – `10000`

```sql
SET pg_trickle.max_fixpoint_iterations = 50;
```

---

## GUC Interaction Matrix

Some GUC variables interact with or depend on each other. The table below
documents these cross-dependencies to help avoid misconfiguration.

| GUC A | GUC B | Interaction |
|-------|-------|-------------|
| `event_driven_wake` | `scheduler_interval_ms` | When `event_driven_wake = true`, the scheduler wakes on NOTIFY and `scheduler_interval_ms` serves only as the poll-based fallback interval. Lowering `scheduler_interval_ms` below 100 ms with event-driven wake enabled adds little value and wastes CPU. |
| `event_driven_wake` | `wake_debounce_ms` | `wake_debounce_ms` only takes effect when `event_driven_wake = true`. It coalesces rapid-fire notifications during bulk DML. Set higher (50–100 ms) for write-heavy workloads, lower (5–10 ms) for latency-sensitive workloads. |
| `auto_backoff` | `min_schedule_seconds` | `auto_backoff` stretches the effective interval up to 8× the configured schedule, but never below `min_schedule_seconds`. If `min_schedule_seconds` is high, backoff has limited room to operate. |
| `auto_backoff` | `default_schedule_seconds` | The backoff multiplier is applied to `default_schedule_seconds` (or the per-ST override); raising this value gives backoff a wider range. |
| `parallel_refresh_mode` | `max_concurrent_refreshes` | `parallel_refresh_mode = 'on'` dispatches independent STs to parallel workers, up to `max_concurrent_refreshes` per database. Setting `max_concurrent_refreshes = 1` effectively disables parallelism even when the mode is `'on'`. |
| `parallel_refresh_mode` | `max_dynamic_refresh_workers` | `max_dynamic_refresh_workers` is a cluster-wide cap across all databases. If you have 4 databases each wanting 4 concurrent refreshes, set this to ≥16 (or accept queuing). |
| `differential_max_change_ratio` | `fuse_default_ceiling` | Both guard against large change batches but at different levels: `differential_max_change_ratio` triggers a FULL refresh fallback (proportional to table size), while `fuse_default_ceiling` halts refresh entirely (absolute row count). The fuse fires first if the change count exceeds it, regardless of the ratio. |
| `block_source_ddl` | DDL operations | When `true`, DDL on source tables (ALTER TABLE, DROP COLUMN) is blocked by an event trigger. Disable temporarily with `SET pg_trickle.block_source_ddl = false` before schema migrations, then re-enable. |
| `cdc_mode` | `cdc_trigger_mode` | `cdc_trigger_mode` (`'statement'` / `'row'`) only applies when CDC is trigger-based. When `cdc_mode = 'wal'` (or after auto-transition to WAL), `cdc_trigger_mode` is irrelevant. |
| `cdc_mode` | `wal_transition_timeout` | `wal_transition_timeout` only applies when `cdc_mode = 'auto'`. It controls how many seconds to wait for the first WAL-based refresh to succeed before falling back to triggers. |
| `cleanup_use_truncate` | `compact_threshold` | `cleanup_use_truncate = true` uses TRUNCATE to clear consumed change buffers (fastest, acquires AccessExclusiveLock briefly). `compact_threshold` controls when fully-consumed buffers are compacted via DELETE — only relevant when TRUNCATE is disabled. |
| `allow_circular` | `max_fixpoint_iterations` | `max_fixpoint_iterations` is only evaluated when `allow_circular = true`. It caps the number of convergence iterations for circular dependency chains. |
| `ivm_topk_max_limit` | TopK queries | Queries with `LIMIT > ivm_topk_max_limit` fall back to FULL refresh instead of the optimized TopK path. Raise this if you have legitimate large TopK queries. |
| `ivm_recursive_max_depth` | Recursive CTEs | Recursive expansion beyond `ivm_recursive_max_depth` iterations is terminated with a warning and falls back to FULL refresh. Set to 0 to disable the guard (not recommended). |

---

## Tuning Profiles

Three named profiles for common deployment patterns. Copy the relevant
settings into your `postgresql.conf` and adjust to taste.

### Low-Latency Profile

**Goal:** Minimize end-to-end latency from base table write to stream table
update. Best for dashboards, real-time analytics, and operational monitoring.

```ini
# Event-driven wake — sub-50ms median latency
pg_trickle.event_driven_wake = true
pg_trickle.wake_debounce_ms = 5              # aggressive: 5ms coalesce

# Fast scheduling
pg_trickle.scheduler_interval_ms = 200       # poll fallback (rarely used)
pg_trickle.min_schedule_seconds = 1
pg_trickle.default_schedule_seconds = 1

# Parallel refresh for independent STs
pg_trickle.parallel_refresh_mode = 'on'
pg_trickle.max_concurrent_refreshes = 4

# Lean merge
pg_trickle.merge_planner_hints = true
pg_trickle.merge_work_mem_mb = 128           # more memory = fewer disk sorts
pg_trickle.cleanup_use_truncate = true
pg_trickle.use_prepared_statements = true

# Guardrails
pg_trickle.auto_backoff = true               # prevent CPU runaway
pg_trickle.fuse_default_ceiling = 0          # disabled — latency over safety
pg_trickle.block_source_ddl = true
```

### High-Throughput Profile

**Goal:** Maximize rows-per-second processed across many stream tables under
heavy write load. Accepts slightly higher latency in exchange for better
batching and resource efficiency.

```ini
# Batched wake — coalesce writes into larger deltas
pg_trickle.event_driven_wake = true
pg_trickle.wake_debounce_ms = 50             # 50ms coalesce window

# Relaxed scheduling
pg_trickle.scheduler_interval_ms = 2000      # 2-second poll fallback
pg_trickle.min_schedule_seconds = 2
pg_trickle.default_schedule_seconds = 5

# Heavy parallelism
pg_trickle.parallel_refresh_mode = 'on'
pg_trickle.max_concurrent_refreshes = 8
pg_trickle.max_dynamic_refresh_workers = 8

# Aggressive performance
pg_trickle.merge_planner_hints = true
pg_trickle.merge_work_mem_mb = 256           # large work_mem for big deltas
pg_trickle.merge_seqscan_threshold = 0.01    # allow seq scans for >1% changes
pg_trickle.cleanup_use_truncate = true
pg_trickle.use_prepared_statements = true
pg_trickle.auto_backoff = true

# Safety for bulk workloads
pg_trickle.fuse_default_ceiling = 500000     # pause on >500K changes
pg_trickle.differential_max_change_ratio = 0.25  # FULL fallback at 25%
pg_trickle.block_source_ddl = true
```

### Resource-Constrained Profile

**Goal:** Minimize CPU and memory footprint for small instances, shared
hosting, or development environments. Accepts higher latency and slower
throughput.

```ini
# Poll-based only — no NOTIFY overhead
pg_trickle.event_driven_wake = false
pg_trickle.scheduler_interval_ms = 5000      # 5-second poll

# Conservative scheduling
pg_trickle.min_schedule_seconds = 5
pg_trickle.default_schedule_seconds = 10

# Minimal parallelism
pg_trickle.parallel_refresh_mode = 'off'     # single-threaded refresh
pg_trickle.max_concurrent_refreshes = 1
pg_trickle.max_dynamic_refresh_workers = 1

# Conservative memory
pg_trickle.merge_work_mem_mb = 32
pg_trickle.merge_planner_hints = true
pg_trickle.cleanup_use_truncate = true

# Tight guardrails
pg_trickle.auto_backoff = true
pg_trickle.fuse_default_ceiling = 100000
pg_trickle.differential_max_change_ratio = 0.10
pg_trickle.block_source_ddl = true
pg_trickle.buffer_alert_threshold = 500000
```

---

## Complete postgresql.conf Example

```ini
# Required
shared_preload_libraries = 'pg_trickle'

# Essential
pg_trickle.enabled = true
pg_trickle.cdc_mode = 'auto'
pg_trickle.scheduler_interval_ms = 1000
pg_trickle.min_schedule_seconds = 1
pg_trickle.default_schedule_seconds = 1
pg_trickle.max_consecutive_errors = 3

# WAL CDC
pg_trickle.wal_transition_timeout = 300
pg_trickle.slot_lag_warning_threshold_mb = 100
pg_trickle.slot_lag_critical_threshold_mb = 1024

# Refresh performance
pg_trickle.differential_max_change_ratio = 0.15
pg_trickle.merge_planner_hints = true
pg_trickle.merge_work_mem_mb = 64
pg_trickle.cleanup_use_truncate = true
pg_trickle.use_prepared_statements = true
pg_trickle.user_triggers = 'auto'

# Guardrails & limits
pg_trickle.block_source_ddl = false
pg_trickle.buffer_alert_threshold = 1000000
pg_trickle.compact_threshold = 100000
pg_trickle.max_grouping_set_branches = 64
pg_trickle.ivm_topk_max_limit = 1000
pg_trickle.ivm_recursive_max_depth = 100

# Circular dependencies (v0.7.0+)
pg_trickle.allow_circular = false                # master switch
pg_trickle.max_fixpoint_iterations = 100         # convergence limit

# Parallel refresh (v0.4.0+, default off)
pg_trickle.parallel_refresh_mode = 'off'        # 'off' | 'dry_run' | 'on'
pg_trickle.max_dynamic_refresh_workers = 4       # cluster-wide worker cap
pg_trickle.max_concurrent_refreshes = 4          # per-database dispatch cap

# Advanced / internal
pg_trickle.change_buffer_schema = 'pgtrickle_changes'
pg_trickle.foreign_table_polling = false
```

---

## Runtime Configuration

All GUC variables can be changed at runtime by a superuser:

```sql
-- View current settings
SHOW pg_trickle.enabled;
SHOW pg_trickle.parallel_refresh_mode;

-- Enable parallel refresh for current session
SET pg_trickle.parallel_refresh_mode = 'on';

-- Change persistently (requires reload)
ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 500;
SELECT pg_reload_conf();
```

---

## Further Reading

- [INSTALL.md](../INSTALL.md) — Installation and initial configuration
- [ARCHITECTURE.md](ARCHITECTURE.md) — System architecture overview
- [SQL_REFERENCE.md](SQL_REFERENCE.md) — Complete function reference
