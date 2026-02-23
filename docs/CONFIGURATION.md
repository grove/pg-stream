# Configuration

Complete reference for all pg_stream GUC (Grand Unified Configuration) variables.

---

## Overview

pg_stream exposes six configuration variables in the `pgdt` namespace. All can be set in `postgresql.conf` or at runtime via `SET` / `ALTER SYSTEM`.

**Required `postgresql.conf` settings:**

```ini
shared_preload_libraries = 'pg_stream'
```

The extension **must** be loaded via `shared_preload_libraries` because it registers GUC variables and a background worker at startup.

> **Note:** `wal_level = logical` and `max_replication_slots` are **not** required. CDC uses lightweight row-level triggers, not logical replication slots.

---

## GUC Variables

### pg_stream.enabled

Enable or disable the pg_stream extension.

| Property | Value |
|---|---|
| Type | `bool` |
| Default | `true` |
| Context | `SUSET` (superuser) |
| Restart Required | No |

When set to `false`, the background scheduler stops processing refreshes. Existing stream tables remain in the catalog but are not refreshed. Manual `pgstream.refresh_stream_table()` calls still work.

```sql
-- Disable automatic refreshes
SET pg_stream.enabled = false;

-- Re-enable
SET pg_stream.enabled = true;
```

---

### pg_stream.scheduler_interval_ms

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
SET pg_stream.scheduler_interval_ms = 500;
```

---

### pg_stream.min_schedule_seconds

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
SET pg_stream.min_schedule_seconds = 10;
```

---

### pg_stream.max_consecutive_errors

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
3. Manual intervention is required: `SELECT pgstream.alter_stream_table('...', status => 'ACTIVE')`.

**Tuning Guidance:**
- **Strict** (production): `3` — fail fast to surface issues.
- **Lenient** (development): `10`–`20` — tolerate transient errors.

```sql
SET pg_stream.max_consecutive_errors = 5;
```

---

### pg_stream.change_buffer_schema

Schema where CDC change buffer tables are created.

| Property | Value |
|---|---|
| Type | `text` |
| Default | `'pgstream_changes'` |
| Context | `SUSET` |
| Restart Required | No (but existing change buffers remain in the old schema) |

Change buffer tables are named `<schema>.changes_<oid>` where `<oid>` is the source table's OID. Placing them in a dedicated schema keeps them out of the `public` namespace.

**Tuning Guidance:**
- Generally leave at the default. Change only if `pgstream_changes` conflicts with an existing schema in your database.

```sql
SET pg_stream.change_buffer_schema = 'my_change_buffers';
```

---

### pg_stream.max_concurrent_refreshes

Maximum number of stream tables that can be refreshed simultaneously.

| Property | Value |
|---|---|
| Type | `int` |
| Default | `4` |
| Range | `1` – `32` |
| Context | `SUSET` |
| Restart Required | No |

Controls concurrency in the scheduler. Each refresh acquires an advisory lock, and the scheduler skips STs that exceed this limit.

**Tuning Guidance:**
- **Small databases** (few STs): `1`–`4` is sufficient.
- **Large deployments** (50+ STs): Increase to `8`–`16` if the server has spare CPU and I/O capacity.
- **Resource-constrained**: Set to `1` for fully sequential refresh processing.

The optimal setting depends on:
- Number of CPU cores available
- I/O throughput (SSD vs HDD)
- Complexity of the defining queries
- Amount of concurrent OLTP workload

```sql
SET pg_stream.max_concurrent_refreshes = 8;
```

---

## Complete postgresql.conf Example

```ini
# Required
shared_preload_libraries = 'pg_stream'

# Optional tuning
pg_stream.enabled = true
pg_stream.scheduler_interval_ms = 1000
pg_stream.min_schedule_seconds = 60
pg_stream.max_consecutive_errors = 3
pg_stream.change_buffer_schema = 'pgstream_changes'
pg_stream.max_concurrent_refreshes = 4
```

---

## Runtime Configuration

All GUC variables can be changed at runtime by a superuser:

```sql
-- View current settings
SHOW pg_stream.enabled;
SHOW pg_stream.scheduler_interval_ms;

-- Change for current session
SET pg_stream.max_concurrent_refreshes = 8;

-- Change persistently (requires reload)
ALTER SYSTEM SET pg_stream.scheduler_interval_ms = 500;
SELECT pg_reload_conf();
```

---

## Further Reading

- [INSTALL.md](../INSTALL.md) — Installation and initial configuration
- [ARCHITECTURE.md](ARCHITECTURE.md) — System architecture overview
- [SQL_REFERENCE.md](SQL_REFERENCE.md) — Complete function reference
