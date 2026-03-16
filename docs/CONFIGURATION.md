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
    - [pg\_trickle.cleanup\_use\_truncate](#pg_tricklecleanup_use_truncate)
    - [pg\_trickle.use\_prepared\_statements](#pg_trickleuse_prepared_statements)
    - [pg\_trickle.user\_triggers](#pg_trickleuser_triggers)
  - [Guardrails & Limits](#guardrails--limits)
    - [pg\_trickle.block\_source\_ddl](#pg_trickleblock_source_ddl)
    - [pg\_trickle.buffer\_alert\_threshold](#pg_tricklebuffer_alert_threshold)
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
