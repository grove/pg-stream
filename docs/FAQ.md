# Frequently Asked Questions

---

## General

### What is pg_stream?

pg_stream is a PostgreSQL 18 extension that implements **stream tables** — declarative, automatically-refreshing materialized views with **Differential View Maintenance (DVM)**. You define a SQL query and a refresh schedule; the extension handles change capture, delta computation, and incremental refresh automatically.

It is inspired by the [DBSP](https://arxiv.org/abs/2203.16684) differential dataflow framework. See [DBSP_COMPARISON.md](research/DBSP_COMPARISON.md) for a detailed comparison.

### How is this different from PostgreSQL materialized views?

| Feature | Materialized Views | Stream Tables |
|---|---|---|
| Refresh | Manual (`REFRESH MATERIALIZED VIEW`) | Automatic (scheduler) or manual |
| Incremental refresh | Not supported natively | Built-in differential mode |
| Change detection | None — always full recompute | CDC triggers track row-level changes |
| Dependency ordering | None | DAG-aware topological refresh |
| Monitoring | None | Built-in views, stats, NOTIFY alerts |
| Schedule | None | Duration strings (`5m`) or cron (`*/5 * * * *`) |

### What PostgreSQL versions are supported?

**PostgreSQL 18.x** exclusively. The extension uses features specific to PostgreSQL 18.

### Does pg_stream require `wal_level = logical`?

**No.** pg_stream uses lightweight row-level triggers for change data capture, not logical replication. You do not need to set `wal_level = logical` or configure `max_replication_slots`.

### Is pg_stream production-ready?

pg_stream is under active development. It has a comprehensive test suite (700+ unit tests, 290+ end-to-end tests), but users should evaluate it against their specific workloads before deploying to production.

---

## Installation & Setup

### How do I install pg_stream?

1. Add `pg_stream` to `shared_preload_libraries` in `postgresql.conf`:
   ```ini
   shared_preload_libraries = 'pg_stream'
   ```
2. Restart PostgreSQL.
3. Run:
   ```sql
   CREATE EXTENSION pg_stream;
   ```

See [INSTALL.md](../INSTALL.md) for platform-specific instructions and pre-built release artifacts.

### What are the minimum configuration requirements?

Only `shared_preload_libraries = 'pg_stream'` is mandatory (requires a restart). All other settings have sensible defaults. `max_worker_processes = 8` is recommended.

### Can I install pg_stream on a managed PostgreSQL service (RDS, Cloud SQL, etc.)?

It depends on whether the service allows custom extensions and `shared_preload_libraries`. Since pg_stream does **not** require `wal_level = logical`, it avoids one of the most common restrictions on managed services. Check your provider's documentation for custom extension support.

### How do I uninstall pg_stream?

1. Drop all stream tables first (or they will be cascade-dropped):
   ```sql
   SELECT pgstream.drop_stream_table(pgs_name) FROM pgstream.pgs_stream_tables;
   ```
2. Drop the extension:
   ```sql
   DROP EXTENSION pg_stream CASCADE;
   ```
3. Remove `pg_stream` from `shared_preload_libraries` and restart PostgreSQL.

---

## Creating & Managing Stream Tables

### How do I create a stream table?

```sql
SELECT pgstream.create_stream_table(
    'order_totals',                                           -- name
    'SELECT customer_id, SUM(amount) AS total
     FROM orders GROUP BY customer_id',                       -- defining query
    '5m',                                                     -- refresh schedule
    'DIFFERENTIAL'                                            -- refresh mode
);
```

### What is the difference between FULL and DIFFERENTIAL refresh mode?

- **FULL** — Truncates the stream table and re-runs the entire defining query every refresh cycle. Simple but expensive for large result sets.
- **DIFFERENTIAL** — Computes only the delta (changes since the last refresh) using the DVM engine and applies it via a `MERGE` statement. Much faster when only a small fraction of source data changes between refreshes.

### When should I use FULL vs. DIFFERENTIAL?

Use **DIFFERENTIAL** (default) when:
- Source tables are large and changes between refreshes are small
- The defining query uses supported operators (most common SQL is supported)

Use **FULL** when:
- The defining query uses unsupported aggregates (`CORR`, `COVAR_*`, `REGR_*`)
- Source tables are small and a full recompute is cheap
- You see frequent adaptive fallbacks to FULL (check refresh history)

### What schedule formats are supported?

**Duration strings:**

| Unit | Suffix | Example |
|---|---|---|
| Seconds | `s` | `30s` |
| Minutes | `m` | `5m` |
| Hours | `h` | `2h` |
| Days | `d` | `1d` |
| Weeks | `w` | `1w` |
| Compound | — | `1h30m` |

**Cron expressions:**

| Format | Example | Description |
|---|---|---|
| 5-field | `*/5 * * * *` | Every 5 minutes |
| Aliases | `@hourly`, `@daily` | Built-in shortcuts |

**CALCULATED mode:** Pass `NULL` as the schedule to inherit the schedule from downstream dependents.

### What is the minimum allowed schedule?

The `pg_stream.min_schedule_seconds` GUC (default: `60`) sets the floor. Schedules shorter than this value are rejected. Set to `1` for development/testing.

### Can a stream table reference another stream table?

**Yes.** Stream tables can depend on other stream tables. The scheduler automatically refreshes them in topological order (upstream first). Circular dependencies are detected and rejected at creation time.

```sql
-- ST1: aggregates orders
SELECT pgstream.create_stream_table('order_totals',
    'SELECT customer_id, SUM(amount) AS total FROM orders GROUP BY customer_id',
    '1m', 'DIFFERENTIAL');

-- ST2: filters ST1
SELECT pgstream.create_stream_table('big_customers',
    'SELECT customer_id, total FROM pgstream.order_totals WHERE total > 1000',
    '1m', 'DIFFERENTIAL');
```

### How do I change a stream table's schedule or mode?

```sql
-- Change schedule
SELECT pgstream.alter_stream_table('order_totals', schedule => '10m');

-- Switch refresh mode
SELECT pgstream.alter_stream_table('order_totals', refresh_mode => 'FULL');

-- Suspend
SELECT pgstream.alter_stream_table('order_totals', status => 'SUSPENDED');

-- Resume
SELECT pgstream.alter_stream_table('order_totals', status => 'ACTIVE');
```

### Can I change the defining query of a stream table?

Not directly. You must drop and recreate the stream table:

```sql
SELECT pgstream.drop_stream_table('order_totals');
SELECT pgstream.create_stream_table('order_totals', '<new query>', '5m', 'DIFFERENTIAL');
```

### How do I trigger a manual refresh?

```sql
SELECT pgstream.refresh_stream_table('order_totals');
```

This works even when `pg_stream.enabled = false` (scheduler disabled).

---

## SQL Support

### What SQL features are supported in defining queries?

Most common SQL is supported in both FULL and DIFFERENTIAL modes:

- Table scans, projections, `WHERE`/`HAVING` filters
- `INNER`, `LEFT`, `RIGHT`, `FULL OUTER JOIN` (including multi-table joins)
- `GROUP BY` with 25+ aggregate functions (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `BOOL_AND`/`OR`, `STRING_AGG`, `ARRAY_AGG`, `JSON_AGG`, `JSONB_AGG`, `BIT_AND`/`OR`/`XOR`, `STDDEV`, `VARIANCE`, `MODE`, `PERCENTILE_CONT`/`DISC`, and more)
- `FILTER (WHERE ...)` on aggregates
- `DISTINCT`
- Set operations: `UNION ALL`, `UNION`, `INTERSECT`, `INTERSECT ALL`, `EXCEPT`, `EXCEPT ALL`
- Subqueries: `EXISTS`, `NOT EXISTS`, `IN (subquery)`, `NOT IN (subquery)`, scalar subqueries
- Non-recursive and recursive CTEs
- Window functions (`ROW_NUMBER`, `RANK`, `SUM OVER`, etc.)
- `LATERAL` joins with set-returning functions and correlated subqueries
- `CASE`, `COALESCE`, `NULLIF`, `GREATEST`, `LEAST`, `BETWEEN`, `IS DISTINCT FROM`

See [DVM Operators](DVM_OPERATORS.md) for the complete list.

### What SQL features are NOT supported?

The following are rejected with clear error messages and suggested rewrites:

| Feature | Reason | Suggested Rewrite |
|---|---|---|
| `DISTINCT ON (…)` | Not supported for incremental maintenance | Use `DISTINCT` or `ROW_NUMBER()` |
| `GROUPING SETS` / `CUBE` / `ROLLUP` | Multiple grouping levels not supported | Separate stream tables or `UNION ALL` |
| `TABLESAMPLE` | Stream tables materialize the full result set | Use `WHERE random() < fraction` in consuming query |
| `NATURAL JOIN` | Rejected to prevent silent wrong results | Use explicit `JOIN ... ON` |
| Window functions in expressions | Cannot be differentially maintained | Move window function to a separate column |
| `LIMIT` / `OFFSET` | Stream tables materialize the full result set | Apply when querying the stream table |
| `FOR UPDATE` / `FOR SHARE` | Row-level locking not applicable | Remove the locking clause |
| `ALL (subquery)` | Not supported | Use `NOT EXISTS` with negated condition |

### What happens to `ORDER BY` in defining queries?

`ORDER BY` is **accepted but silently discarded**. Row order in a stream table is undefined (consistent with PostgreSQL's `CREATE MATERIALIZED VIEW` behavior). Apply `ORDER BY` when **querying** the stream table, not in the defining query.

### Which aggregates support DIFFERENTIAL mode?

**Algebraic** (O(changes), fully incremental): `COUNT`, `SUM`, `AVG`

**Semi-algebraic** (incremental with occasional group rescan): `MIN`, `MAX`

**Group-rescan** (affected groups re-aggregated from source): `STRING_AGG`, `ARRAY_AGG`, `JSON_AGG`, `JSONB_AGG`, `BOOL_AND`, `BOOL_OR`, `BIT_AND`, `BIT_OR`, `BIT_XOR`, `JSON_OBJECT_AGG`, `JSONB_OBJECT_AGG`, `STDDEV`, `STDDEV_POP`, `STDDEV_SAMP`, `VARIANCE`, `VAR_POP`, `VAR_SAMP`, `MODE`, `PERCENTILE_CONT`, `PERCENTILE_DISC`

**Not supported** (use FULL mode): `CORR`, `COVAR_POP`, `COVAR_SAMP`, `REGR_*`

---

## Change Data Capture (CDC)

### How does pg_stream capture changes to source tables?

pg_stream installs `AFTER INSERT/UPDATE/DELETE` row-level PL/pgSQL triggers on each source table. These triggers write change records (action, old/new row data as JSONB, LSN, transaction ID) into per-source buffer tables in the `pgstream_changes` schema.

### What is the overhead of CDC triggers?

Approximately **20–55 μs per row** (PL/pgSQL dispatch + `row_to_json()` + buffer INSERT). At typical write rates (<1000 writes/sec per source table), this adds **less than 5%** DML latency overhead.

### What happens when I `TRUNCATE` a source table?

**TRUNCATE bypasses row-level triggers entirely.** The stream table will become stale without the system detecting it. This is a known PostgreSQL limitation — TRUNCATE does not fire `AFTER DELETE` triggers.

**Recovery options:**
1. Manually refresh: `SELECT pgstream.refresh_stream_table('my_table');`
2. Use `DELETE FROM source_table` instead of `TRUNCATE` when CDC matters.
3. FULL-mode stream tables are immune since they recompute from scratch every cycle.

### Are CDC triggers automatically cleaned up?

Yes. When the last stream table referencing a source is dropped, the trigger and its associated change buffer table are automatically removed.

### What happens if a source table is dropped or altered?

pg_stream has DDL event triggers that detect `ALTER TABLE` and `DROP TABLE` on source tables. When detected:
- Affected stream tables are marked with `needs_reinit = true`
- The next refresh cycle performs a full reinitialization (drops and recreates the storage table)
- A `reinitialize_needed` NOTIFY alert is sent

---

## Performance & Tuning

### How do I tune the scheduler interval?

The `pg_stream.scheduler_interval_ms` GUC controls how often the scheduler checks for stale stream tables (default: 1000 ms).

| Workload | Recommended Value |
|---|---|
| Low-latency (near real-time) | `100`–`500` |
| Standard | `1000` (default) |
| Low-overhead (many STs, long schedules) | `5000`–`10000` |

### What is the adaptive fallback to FULL?

When the number of pending changes exceeds `pg_stream.differential_max_change_ratio` (default: 15%) of the source table size, DIFFERENTIAL mode automatically falls back to FULL for that refresh cycle. This prevents pathological delta queries on bulk changes.

- Set to `0.0` to always use DIFFERENTIAL (even on large change sets)
- Set to `1.0` to effectively always use FULL
- Default `0.15` (15%) is a good balance

### How many concurrent refreshes can run?

Controlled by `pg_stream.max_concurrent_refreshes` (default: 4, range: 1–32). Each concurrent refresh uses a background worker. Increase this if you have many stream tables and available CPU/IO.

### How do I check if my stream tables are keeping up?

```sql
-- Quick overview
SELECT pgs_name, status, staleness, stale
FROM pgstream.stream_tables_info;

-- Detailed statistics
SELECT pgs_name, total_refreshes, avg_duration_ms, consecutive_errors, stale
FROM pgstream.pg_stat_stream_tables;

-- Recent refresh history for a specific ST
SELECT * FROM pgstream.get_refresh_history('order_totals', 10);
```

### What is `__pgs_row_id`?

Every stream table has a `__pgs_row_id BIGINT PRIMARY KEY` column. It stores a 64-bit xxHash of the row's group-by key (or all columns for non-aggregate queries). The refresh engine uses it for delta `MERGE` operations (matching DELETEs and INSERTs by row ID).

**You should ignore this column in your queries.** It is an implementation detail.

---

## Interoperability

### Can PostgreSQL views reference stream tables?

**Yes.** Stream tables are standard heap tables. Views work normally and reflect data as of the most recent refresh.

### Can materialized views reference stream tables?

**Yes**, though it is somewhat redundant (both are physical snapshots). The materialized view requires its own `REFRESH MATERIALIZED VIEW` — it does not auto-refresh when the stream table refreshes.

### Can I replicate stream tables with logical replication?

**Yes.** Stream tables can be published like any ordinary table:

```sql
CREATE PUBLICATION my_pub FOR TABLE pgstream.order_totals;
```

**Important caveats:**
- The `__pgs_row_id` column is replicated (it is the primary key)
- Subscribers receive materialized data, not the defining query
- Do **not** install pg_stream on the subscriber and attempt to refresh the replicated table — it will have no CDC triggers or catalog entries
- Internal change buffer tables are not published by default

### Can I `INSERT`, `UPDATE`, or `DELETE` rows in a stream table directly?

**No.** Stream table contents are managed exclusively by the refresh engine. Direct DML will corrupt the internal state.

### Can I add foreign keys to or from stream tables?

**No.** The refresh engine uses bulk `MERGE` operations that do not respect foreign key ordering. Foreign key constraints on stream tables are not supported.

### Can I add my own triggers to stream tables?

**Not recommended.** While PostgreSQL will allow it, the `MERGE` statement used during refresh may fire your triggers in unexpected ways (e.g., a differential refresh issues DELETE + INSERT pairs, not UPDATEs).

### Can I `ALTER TABLE` a stream table directly?

**No.** Use `pgstream.alter_stream_table()` to modify schedule, refresh mode, or status. To change the defining query, drop and recreate the stream table.

---

## Monitoring & Alerting

### What monitoring views are available?

| View | Description |
|---|---|
| `pgstream.stream_tables_info` | Status overview with computed staleness |
| `pgstream.pg_stat_stream_tables` | Comprehensive stats (refresh counts, avg duration, error streaks) |

### How do I get alerted when something goes wrong?

pg_stream sends PostgreSQL `NOTIFY` messages on the `pg_stream_alert` channel with JSON payloads:

| Event | When |
|---|---|
| `stale_data` | Staleness exceeds 2× the schedule |
| `auto_suspended` | Stream table suspended after max consecutive errors |
| `reinitialize_needed` | Upstream DDL change detected |
| `buffer_growth_warning` | Change buffer growing unexpectedly |
| `refresh_completed` | Refresh completed successfully |
| `refresh_failed` | Refresh failed |

Listen with:
```sql
LISTEN pg_stream_alert;
```

### What happens when a stream table keeps failing?

After `pg_stream.max_consecutive_errors` (default: 3) consecutive failures, the stream table moves to `ERROR` status and automatic refreshes stop. An `auto_suspended` NOTIFY alert is sent.

To recover:
```sql
-- Fix the underlying issue (e.g., restore a dropped source table), then:
SELECT pgstream.alter_stream_table('my_table', status => 'ACTIVE');
```

Retries use exponential backoff (base 1s, max 60s, ±25% jitter, up to 5 retries before counting as a real failure).

---

## Configuration Reference

| GUC | Type | Default | Description |
|---|---|---|---|
| `pg_stream.enabled` | bool | `true` | Enable/disable the scheduler. Manual refreshes still work when `false`. |
| `pg_stream.scheduler_interval_ms` | int | `1000` | Scheduler wake interval in milliseconds (100–60000) |
| `pg_stream.min_schedule_seconds` | int | `60` | Minimum allowed schedule duration (1–86400) |
| `pg_stream.max_consecutive_errors` | int | `3` | Failures before auto-suspending (1–100) |
| `pg_stream.change_buffer_schema` | text | `pgstream_changes` | Schema for CDC buffer tables |
| `pg_stream.max_concurrent_refreshes` | int | `4` | Max parallel refresh workers (1–32) |
| `pg_stream.differential_max_change_ratio` | float | `0.15` | Change ratio threshold for adaptive FULL fallback (0.0–1.0) |
| `pg_stream.cleanup_use_truncate` | bool | `true` | Use TRUNCATE instead of DELETE for buffer cleanup |

All GUCs are `SUSET` context (superuser SET) and take effect without restart, except `shared_preload_libraries` which requires a PostgreSQL restart.

---

## Troubleshooting

### My stream table is stuck in INITIALIZING status

The initial full refresh may have failed. Check:
```sql
SELECT * FROM pgstream.get_refresh_history('my_table', 5);
```
If the error is transient, retry with:
```sql
SELECT pgstream.refresh_stream_table('my_table');
```

### My stream table shows stale data but the scheduler is running

Common causes:
1. **TRUNCATE on source table** — bypasses CDC triggers. Manual refresh needed.
2. **Too many errors** — check `consecutive_errors` in `pgstream.pg_stat_stream_tables`. Resume with `ALTER ... status => 'ACTIVE'`.
3. **Long-running refresh** — check for lock contention or slow defining queries.
4. **Scheduler disabled** — verify `SHOW pg_stream.enabled;` returns `on`.

### I get "cycle detected" when creating a stream table

Stream tables cannot have circular dependencies. If ST-A depends on ST-B and ST-B depends on ST-A (directly or transitively), creation is rejected. Restructure your queries to break the cycle.

### A source table was altered and my stream table stopped refreshing

pg_stream detects DDL changes via event triggers and marks affected stream tables with `needs_reinit = true`. The next scheduler cycle will reinitialize (full drop + recreate of storage) the stream table automatically. If the schema change breaks the defining query, the reinitialization will fail — check refresh history for the error and recreate the stream table with an updated query.

### How do I see the delta query generated for a stream table?

```sql
SELECT pgstream.explain_dt('order_totals');
```

This shows the DVM operator tree, source tables, and the generated delta SQL.
