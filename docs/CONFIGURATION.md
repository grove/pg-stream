# Configuration

Complete reference for all pg_trickle GUC (Grand Unified Configuration) variables.

---

## Overview

pg_trickle exposes sixteen configuration variables in the `pg_trickle` namespace. All can be set in `postgresql.conf` or at runtime via `SET` / `ALTER SYSTEM`.

**Required `postgresql.conf` settings:**

```ini
shared_preload_libraries = 'pg_trickle'
```

The extension **must** be loaded via `shared_preload_libraries` because it registers GUC variables and a background worker at startup.

> **Note:** `wal_level = logical` and `max_replication_slots` are **not** required by default. The default CDC mode (`trigger`) uses lightweight row-level triggers. If you set `pg_trickle.cdc_mode = 'auto'` or `'wal'`, then `wal_level = logical` is needed for WAL-based capture (see [pg_trickle.cdc_mode](#pg_tricklecdc_mode)).

---

## GUC Variables

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
| Default | `60` (1 minute) |
| Range | `1` – `86400` (1 second to 24 hours) |
| Context | `SUSET` |
| Restart Required | No |

This acts as a safety guardrail to prevent users from setting impractically small schedules that would cause excessive refresh overhead.

**Tuning Guidance:**
- **Development/testing**: Set to `1` for fast iteration.
- **Production**: Keep at `60` or higher to prevent excessive WAL consumption and CPU usage.

```sql
-- Allow 10-second schedules (for testing)
SET pg_trickle.min_schedule_seconds = 10;
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

### pg_trickle.change_buffer_schema

Schema where CDC change buffer tables are created.

| Property | Value |
|---|---|
| Type | `text` |
| Default | `'pgtrickle_changes'` |
| Context | `SUSET` |
| Restart Required | No (but existing change buffers remain in the old schema) |

Change buffer tables are named `<schema>.changes_<oid>` where `<oid>` is the source table's OID. Placing them in a dedicated schema keeps them out of the `public` namespace.

**Tuning Guidance:**
- Generally leave at the default. Change only if `pgtrickle_changes` conflicts with an existing schema in your database.

```sql
SET pg_trickle.change_buffer_schema = 'my_change_buffers';
```

---

### pg_trickle.max_concurrent_refreshes

> **Reserved for future use.** This setting is accepted and stored but has no
> effect in v0.2.0. Parallel refresh is planned for v0.3.0.

Maximum number of stream tables that can be refreshed simultaneously.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `4` |
| Range | `1` – `32` |
| Context | `SUSET` |
| Restart Required | No |

The scheduler currently processes stream tables sequentially in topological
order. This GUC is reserved for v0.3.0 when parallel scheduling is
implemented. Setting it has no effect in v0.2.0.

```sql
-- Accepted but has no effect in v0.2.0
SET pg_trickle.max_concurrent_refreshes = 8;
```

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
| Values | `'auto'`, `'on'`, `'off'` |
| Context | `SUSET` |
| Restart Required | No |

When a stream table has user-defined row-level triggers, the refresh engine can decompose the `MERGE` into explicit `DELETE` + `UPDATE` + `INSERT` statements so triggers fire with correct `TG_OP`, `OLD`, and `NEW` values.

**Values:**
- **`auto`** (default): Automatically detect user triggers on the stream table. If present, use the explicit DML path; otherwise use `MERGE`.
- **`on`**: Always use the explicit DML path, even without user triggers. Useful for testing.
- **`off`**: Always use `MERGE`. User triggers are suppressed during refresh. This is the escape hatch if explicit DML causes issues.

**Notes:**
- Row-level triggers do **not** fire during FULL refresh regardless of this setting. FULL refresh uses `DISABLE TRIGGER USER` / `ENABLE TRIGGER USER` to suppress them.
- The explicit DML path adds ~25–60% overhead compared to MERGE for affected stream tables.
- Stream tables without user triggers have zero overhead when using `auto` (only a fast `pg_trigger` check).

```sql
-- Auto-detect (default)
SET pg_trickle.user_triggers = 'auto';

-- Always use explicit DML (for testing)
SET pg_trickle.user_triggers = 'on';

-- Suppress triggers, use MERGE
SET pg_trickle.user_triggers = 'off';
```

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

### pg_trickle.cdc_mode

CDC (Change Data Capture) mechanism selection.

| Value | Description |
|-------|-------------|
| `'trigger'` | **(default)** Always use row-level triggers for change capture |
| `'auto'` | Use triggers for creation; transition to WAL-based CDC if `wal_level = logical` |
| `'wal'` | Require WAL-based CDC (fails if `wal_level != logical`) |

**Default:** `'trigger'`

```sql
-- Always use triggers (default, zero-config)
SET pg_trickle.cdc_mode = 'trigger';

-- Enable automatic trigger → WAL transition
SET pg_trickle.cdc_mode = 'auto';

-- Require WAL-based CDC (error if wal_level != logical)
SET pg_trickle.cdc_mode = 'wal';
```

---

### pg_trickle.wal_transition_timeout

> **Note:** WAL-based CDC is pre-production in v0.2.0. This setting is only
> relevant when `pg_trickle.cdc_mode = 'auto'` or `'wal'`. See
> [ARCHITECTURE.md](ARCHITECTURE.md) for status.

Maximum time (seconds) to wait for the WAL decoder to catch up during
the transition from trigger-based to WAL-based CDC. If the decoder has
not caught up within this timeout, the system falls back to triggers.

**Default:** `300` (5 minutes)  
**Range:** `10` – `3600`

```sql
SET pg_trickle.wal_transition_timeout = 300;
```

---

### pg_trickle.diamond_consistency

Default diamond dependency consistency mode for new stream tables.

When stream tables form diamond-shaped dependency graphs (e.g., A → B, A → C, B → D, C → D), the scheduler can group the intermediate STs (B, C) and the convergence node (D) into an atomic refresh group. In `atomic` mode, if any member of the group fails to refresh, all members are rolled back via `SAVEPOINT`, ensuring the convergence node never reads a mix of old and new data from its upstream sources.

| Value | Description |
|-------|-------------|
| `'none'` | **(default)** Independent refresh — each ST is refreshed individually in topological order. A partial failure in a diamond may leave the convergence node reading mixed versions. |
| `'atomic'` | Atomic group refresh — all members of a diamond group are wrapped in a SAVEPOINT. On any failure, the entire group is rolled back and retried together. |

**Default:** `'none'`

This GUC sets the default for new stream tables created without an explicit `diamond_consistency` parameter. Individual stream tables can override it via `create_stream_table(... diamond_consistency => 'atomic')` or `alter_stream_table(... diamond_consistency => 'atomic')`.

```sql
-- Enable atomic diamond consistency globally
SET pg_trickle.diamond_consistency = 'atomic';

-- Disable (default — independent refresh)
SET pg_trickle.diamond_consistency = 'none';
```

> **Note:** Atomic mode only takes effect for stream tables that are part of a detected diamond group. Linear chains and standalone STs are unaffected.

---

## Complete postgresql.conf Example

```ini
# Required
shared_preload_libraries = 'pg_trickle'

# Optional tuning
pg_trickle.enabled = true
pg_trickle.scheduler_interval_ms = 1000
pg_trickle.min_schedule_seconds = 60
pg_trickle.max_consecutive_errors = 3
pg_trickle.change_buffer_schema = 'pgtrickle_changes'
pg_trickle.max_concurrent_refreshes = 4   # reserved; no effect in v0.2.0
pg_trickle.differential_max_change_ratio = 0.15
pg_trickle.cleanup_use_truncate = true
pg_trickle.merge_planner_hints = true
pg_trickle.merge_work_mem_mb = 64
# pg_trickle.merge_strategy removed in v0.2.0
pg_trickle.use_prepared_statements = true
pg_trickle.user_triggers = 'auto'
pg_trickle.block_source_ddl = false
pg_trickle.cdc_mode = 'trigger'
pg_trickle.wal_transition_timeout = 300
pg_trickle.diamond_consistency = 'none'
```

---

## Runtime Configuration

All GUC variables can be changed at runtime by a superuser:

```sql
-- View current settings
SHOW pg_trickle.enabled;
SHOW pg_trickle.scheduler_interval_ms;

-- Change for current session
SET pg_trickle.max_concurrent_refreshes = 8;  -- no effect in v0.2.0

-- Change persistently (requires reload)
ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 500;
SELECT pg_reload_conf();
```

---

## Further Reading

- [INSTALL.md](../INSTALL.md) — Installation and initial configuration
- [ARCHITECTURE.md](ARCHITECTURE.md) — System architecture overview
- [SQL_REFERENCE.md](SQL_REFERENCE.md) — Complete function reference
