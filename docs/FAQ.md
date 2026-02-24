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

Each of these is explained in detail in the [Why Are These SQL Features Not Supported?](#why-are-these-sql-features-not-supported) section below.

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

### How do I check if a source table has switched from trigger-based CDC to WAL-based CDC?

When you enable hybrid CDC (`pg_stream.cdc_mode = 'auto'`), pg_stream starts capturing changes with triggers and can automatically transition to WAL-based logical replication once conditions are met. There are several ways to check the current CDC mode for each source table:

**1. Query the dependency catalog directly:**

```sql
SELECT d.source_relid, c.relname AS source_table, d.cdc_mode,
       d.slot_name, d.decoder_confirmed_lsn, d.transition_started_at
FROM pgstream.pgs_dependencies d
JOIN pg_class c ON c.oid = d.source_relid;
```

The `cdc_mode` column shows one of three values:
- `TRIGGER` — changes are captured via row-level triggers (the default)
- `TRANSITIONING` — the system is in the process of switching from triggers to WAL
- `WAL` — changes are captured via logical replication

**2. Use the built-in health check function:**

```sql
SELECT source_table, cdc_mode, slot_name, lag_bytes, alert
FROM pgstream.check_cdc_health();
```

This returns a row per source table with the current mode, replication slot lag (for WAL-mode sources), and any alert conditions such as `slot_lag_exceeds_threshold` or `replication_slot_missing`.

**3. Listen for real-time transition notifications:**

```sql
LISTEN pg_stream_cdc_transition;
```

pg_stream sends a `NOTIFY` with a JSON payload whenever a transition starts, completes, or is rolled back. Example payload:

```json
{
  "event": "transition_complete",
  "source_table": "public.orders",
  "old_mode": "TRANSITIONING",
  "new_mode": "WAL",
  "slot_name": "pg_stream_slot_16384"
}
```

This lets you integrate CDC mode changes into your monitoring stack without polling.

**4. Check the global GUC setting:**

```sql
SHOW pg_stream.cdc_mode;
```

This shows the *desired* global behavior (`trigger`, `auto`, or `wal`), not the per-table actual state. The per-table state lives in `pgs_dependencies.cdc_mode` as described above.

See [CONFIGURATION.md](CONFIGURATION.md) for details on the `pg_stream.cdc_mode` and `pg_stream.wal_transition_timeout` GUCs.

### Is it safe to add triggers to a stream table while the source table is switching CDC modes?

**Yes, this is completely safe.** CDC mode transitions and user-defined triggers operate on different tables and do not interfere with each other:

- **CDC transitions** affect how changes are captured from **source tables** (e.g., `orders`). The transition switches the capture mechanism from row-level triggers on the source table to WAL-based logical replication.
- **User-defined triggers** live on **stream tables** (e.g., `order_totals`) and control how the refresh engine *applies* changes to the materialized output.

Because these are independent concerns, you can freely add, modify, or remove triggers on a stream table at any point — including during an active CDC transition on its source tables.

**How it works in practice:**

1. The refresh engine checks for user-defined triggers on the stream table at the start of each refresh cycle (via a fast `pg_trigger` lookup, <0.1 ms).
2. If user triggers are detected, the engine uses explicit `DELETE` / `UPDATE` / `INSERT` statements instead of `MERGE`, so your triggers fire with correct `TG_OP`, `OLD`, and `NEW` values.
3. The change data consumed by the refresh engine has the same format regardless of whether it came from CDC triggers or WAL decoding — so the trigger detection and the CDC mode are fully decoupled.

A trigger added between two refresh cycles will simply be picked up on the next cycle. The only (theoretical) edge case is adding a trigger in the tiny window *during* a single refresh transaction, between the trigger-detection check and the MERGE execution — but since both happen within the same transaction, this is virtually impossible in practice.

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

**Yes, for DIFFERENTIAL mode stream tables.** When user-defined row-level triggers are detected (or `pg_stream.user_triggers = 'on'`), the refresh engine automatically switches from `MERGE` to explicit `DELETE` + `UPDATE` + `INSERT` statements. This ensures triggers fire with the correct `TG_OP`, `OLD`, and `NEW` values.

**Limitations:**
- Row-level triggers do **not** fire during FULL refresh (they are automatically suppressed via `DISABLE TRIGGER USER`). Use `REFRESH MODE DIFFERENTIAL` for stream tables with triggers.
- The `IS DISTINCT FROM` guard prevents no-op `UPDATE` triggers when the aggregate result is unchanged.
- `BEFORE` triggers that modify `NEW` will affect the stored value — the next refresh may "correct" it back, causing oscillation.

See the `pg_stream.user_triggers` GUC in [CONFIGURATION.md](CONFIGURATION.md) for control options.

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
| `pg_stream.user_triggers` | text | `auto` | User trigger handling: `auto` (detect), `on` (always explicit DML), `off` (suppress) |
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
SELECT pgstream.explain_st('order_totals');
```

This shows the DVM operator tree, source tables, and the generated delta SQL.

---

## Why Are These SQL Features Not Supported?

This section gives detailed technical explanations for each SQL limitation. pg_stream follows the principle of **"fail loudly rather than produce wrong data"** — every unsupported feature is detected at stream-table creation time and rejected with a clear error message and a suggested rewrite.

### Why is `NATURAL JOIN` rejected?

A full `NATURAL JOIN` implementation was prototyped and then **reverted** because it could silently produce wrong results.

`NATURAL JOIN` implicitly joins on **all columns with matching names** between the two tables. This creates three problems for stream tables:

1. **Hidden `__pgs_row_id` collision.** Every stream table has a `__pgs_row_id BIGINT PRIMARY KEY` column used internally for delta `MERGE` operations. If a stream table references another stream table via `NATURAL JOIN`, PostgreSQL will silently include `__pgs_row_id` in the join condition — producing wrong results without any error or warning.

2. **Schema-evolution fragility.** If a column is added to a source table that happens to share a name with a column in the other table, the join semantics silently change. In an ad-hoc query you'd notice immediately, but a stream table's defining query is stored and re-executed on every refresh, so the breakage could persist undetected across many refresh cycles.

3. **Raw parse tree limitation.** PostgreSQL's raw parser sets the `isNatural` flag on `JoinExpr` but does **not** resolve the actual join column list — the `quals` field is `NULL`. Column resolution happens later during query analysis. This means that at parse time, where pg_stream builds its operator tree, the actual join conditions are unknown. Without explicit conditions, the DVM engine cannot generate correct delta queries.

**Rewrite:**
```sql
-- Instead of:
SELECT * FROM orders NATURAL JOIN customers

-- Use explicit join conditions:
SELECT * FROM orders JOIN customers ON orders.customer_id = customers.id
```

### Why are `GROUPING SETS`, `CUBE`, and `ROLLUP` rejected?

These constructs produce **multiple grouping levels in a single query** — for example, `GROUP BY CUBE(dept, region)` yields subtotals for `(dept, region)`, `(dept)`, `(region)`, and `()` (grand total), all interleaved in one result set.

This is fundamentally incompatible with incremental maintenance for two reasons:

1. **Ambiguous row identity.** pg_stream uses a hash of the group-by key columns to identify each row (`__pgs_row_id`). With `GROUPING SETS`, the same column values can appear in multiple grouping levels (a row for `dept='Sales'` appears in both the per-department subtotal and the grand total), making row identity ambiguous. There is no way to distinguish "the subtotal for Sales" from "the grand total" using column values alone.

2. **Delta computation complexity.** Each grouping level is effectively a separate aggregate query with a different `GROUP BY` clause. A single source-row change can affect multiple grouping levels differently. Deriving a correct, efficient delta query that handles all levels simultaneously is an open research problem for incremental view maintenance.

PostgreSQL internally expands `CUBE`/`ROLLUP` during query analysis, but the raw parse tree (where pg_stream detects them) still carries `T_GroupingSet` nodes. These are detected early and rejected before any resources are allocated.

**Rewrite:**
```sql
-- Instead of:
SELECT dept, region, SUM(amount) FROM sales GROUP BY CUBE(dept, region)

-- Create separate stream tables:
SELECT dept, region, SUM(amount) FROM sales GROUP BY dept, region  -- detail
SELECT dept, SUM(amount) FROM sales GROUP BY dept                  -- by dept
SELECT region, SUM(amount) FROM sales GROUP BY region              -- by region
SELECT SUM(amount) FROM sales                                      -- grand total

-- Or combine them:
SELECT dept, region, SUM(amount) FROM sales GROUP BY dept, region
UNION ALL
SELECT dept, NULL, SUM(amount) FROM sales GROUP BY dept
UNION ALL
SELECT NULL, region, SUM(amount) FROM sales GROUP BY region
UNION ALL
SELECT NULL, NULL, SUM(amount) FROM sales
```

### Why is `DISTINCT ON (…)` rejected?

`DISTINCT ON` is a PostgreSQL-specific extension (not in the SQL standard) that returns the first row for each unique combination of the specified expressions, with "first" determined by `ORDER BY`.

It cannot be incrementally maintained because:

1. **Non-deterministic row selection.** Which row is "first" depends on the physical ordering at query time. When source data changes, the "winner" for each distinct group can change unpredictably — the delta would need to compare new and old winners for every group, which degrades to a full rescan.

2. **ORDER BY dependency.** `DISTINCT ON` semantics are tightly coupled to `ORDER BY`, but stream tables intentionally discard ordering (row storage order is undefined). This means the `ORDER BY` that `DISTINCT ON` depends on cannot be preserved.

**Rewrite:**
```sql
-- Instead of:
SELECT DISTINCT ON (dept) dept, employee, salary
FROM employees ORDER BY dept, salary DESC

-- Use a window function:
SELECT dept, employee, salary FROM (
    SELECT dept, employee, salary,
           ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn
    FROM employees
) sub WHERE rn = 1
```

### Why is `TABLESAMPLE` rejected?

`TABLESAMPLE` returns a random subset of rows from a table (e.g., `FROM orders TABLESAMPLE BERNOULLI(10)` gives ~10% of rows).

Stream tables materialize the **complete** result set of the defining query and keep it up-to-date across refreshes. Baking a random sample into the defining query is not meaningful because:

1. **Non-determinism.** Each refresh would sample different rows, making the stream table contents unstable and unpredictable. The delta between refreshes would be dominated by sampling noise, not actual data changes.

2. **CDC incompatibility.** The trigger-based change-capture system tracks specific row-level changes (inserts, updates, deletes). A `TABLESAMPLE` defining query has no stable row identity — the "changed rows" concept doesn't apply when the entire sample shifts each cycle.

**Rewrite:**
```sql
-- Instead of sampling in the defining query:
SELECT * FROM orders TABLESAMPLE BERNOULLI(10)

-- Materialize the full result and sample when querying:
SELECT * FROM order_stream_table WHERE random() < 0.1
```

### Why is `LIMIT` / `OFFSET` rejected?

Stream tables materialize the complete result set and keep it synchronized with source data. `LIMIT`/`OFFSET` would truncate the result:

1. **Undefined ordering.** `LIMIT` without `ORDER BY` returns an arbitrary subset. Even with `ORDER BY`, stream tables discard ordering — the "top N" rows concept doesn't apply to a set-based materialized result.

2. **Delta instability.** When source rows change, the boundary between "in the LIMIT" and "out of the LIMIT" shifts. A single INSERT could evict one row and admit another, requiring the refresh to track the full ordered position of every row — essentially a full rescan.

3. **Semantic mismatch.** Users who write `LIMIT 100` typically want to limit what they *read*, not what is *stored*. Since stream tables are queried separately from their definition, the `LIMIT` belongs in the consuming query.

**Rewrite:**
```sql
-- Instead of:
'SELECT * FROM orders ORDER BY created_at DESC LIMIT 100'

-- Omit LIMIT from the defining query, apply when reading:
SELECT * FROM orders_stream_table ORDER BY created_at DESC LIMIT 100
```

### Why are window functions in expressions rejected?

Window functions like `ROW_NUMBER() OVER (…)` are supported as **standalone columns** in stream tables. However, embedding a window function inside an expression — such as `CASE WHEN ROW_NUMBER() OVER (...) = 1 THEN ...` or `SUM(x) OVER (...) + 1` — is rejected.

This restriction exists because:

1. **Partition-based recomputation.** pg_stream's differential mode handles window functions by recomputing entire partitions that were affected by changes. When a window function is buried inside an expression, the DVM engine cannot isolate the window computation from the surrounding expression, making it impossible to correctly identify which partitions to recompute.

2. **Expression tree ambiguity.** The DVM parser would need to differentiate the outer expression (arithmetic, `CASE`, etc.) while treating the inner window function specially. This creates a combinatorial explosion of differentiation rules for every possible expression type × window function combination.

**Rewrite:**
```sql
-- Instead of:
SELECT id, CASE WHEN ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) = 1
                THEN 'top' ELSE 'other' END AS rank_label
FROM employees

-- Move window function to a separate column, then use a wrapping stream table:
-- ST1:
SELECT id, dept, salary,
       ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn
FROM employees

-- ST2 (references ST1):
SELECT id, CASE WHEN rn = 1 THEN 'top' ELSE 'other' END AS rank_label
FROM pgstream.employees_ranked
```

### Why is `FOR UPDATE` / `FOR SHARE` rejected?

`FOR UPDATE` and related locking clauses (`FOR SHARE`, `FOR NO KEY UPDATE`, `FOR KEY SHARE`) acquire row-level locks on selected rows. This is incompatible with stream tables because:

1. **Refresh semantics.** Stream table contents are managed by the refresh engine using bulk `MERGE` operations. Row-level locks taken during the defining query would conflict with the refresh engine's own locking strategy.

2. **No direct DML.** Since users cannot directly modify stream table rows, there is no use case for locking rows inside the defining query. The locks would be held for the duration of the refresh transaction and then released, serving no purpose.

### Why is `ALL (subquery)` not supported?

`ALL (subquery)` compares a value against every row returned by a subquery (e.g., `WHERE x > ALL (SELECT y FROM t)`). It is rejected because:

1. **Negation rewrite complexity.** `x > ALL (SELECT y FROM t)` is logically equivalent to `NOT EXISTS (SELECT 1 FROM t WHERE y >= x)`, which pg_stream can handle via its anti-join operator. The rewrite is straightforward.

2. **Rare usage.** `ALL (subquery)` is uncommon in analytical queries. Supporting it directly would add operator complexity for minimal benefit.

**Rewrite:**
```sql
-- Instead of:
WHERE amount > ALL (SELECT threshold FROM limits)

-- Use NOT EXISTS:
WHERE NOT EXISTS (SELECT 1 FROM limits WHERE threshold >= amount)
```

### Why is `ORDER BY` silently discarded?

`ORDER BY` in the defining query is **accepted but ignored**. This is consistent with how PostgreSQL treats `CREATE MATERIALIZED VIEW AS SELECT ... ORDER BY ...` — the ordering is not preserved in the stored data.

Stream tables are heap tables with no guaranteed row order. The `ORDER BY` in the defining query would only affect the order of the initial `INSERT`, which has no lasting effect. Apply ordering when **querying** the stream table:

```sql
-- This ORDER BY is meaningless in the defining query:
'SELECT region, SUM(amount) FROM orders GROUP BY region ORDER BY total DESC'

-- Instead, order when reading:
SELECT * FROM regional_totals ORDER BY total DESC
```

### Why are unsupported aggregates (`CORR`, `COVAR_*`, `REGR_*`) limited to FULL mode?

Regression aggregates like `CORR`, `COVAR_POP`, `COVAR_SAMP`, and the `REGR_*` family require maintaining running sums of products and squares across the entire group. Unlike `COUNT`/`SUM`/`AVG` (where deltas can be computed from the change alone) or group-rescan aggregates (where only affected groups are re-read), regression aggregates:

1. **Lack algebraic delta rules.** There is no closed-form way to update a correlation coefficient from a single row change without access to the full group's data.

2. **Would degrade to group-rescan anyway.** Even if supported, the implementation would need to rescan the full group from source — identical to FULL mode for most practical group sizes.

These aggregates work fine in **FULL** refresh mode, which re-runs the entire query from scratch each cycle.

---

## Why Are These Stream Table Operations Restricted?

Stream tables are regular PostgreSQL heap tables under the hood, but their contents are managed exclusively by the refresh engine. This section explains why certain operations that work on ordinary tables are disallowed or unsupported on stream tables.

### Why can't I `INSERT`, `UPDATE`, or `DELETE` rows in a stream table?

Stream table contents are the **output** of the refresh engine — they represent the materialized result of the defining query at a specific point in time. Direct DML would corrupt this contract in several ways:

1. **Row ID integrity.** Every row has a `__pgs_row_id` (a 64-bit xxHash of the group-by key or all columns). The refresh engine uses this for delta `MERGE` — matching incoming deltas against existing rows. A manually inserted row with an incorrect or duplicate `__pgs_row_id` would cause the next differential refresh to produce wrong results (double-counting, missed deletes, or merge conflicts).

2. **Frontier inconsistency.** Each refresh records a *frontier* — a set of per-source LSN positions that represent "data up to this point has been materialized." A manual DML change is not tracked by any frontier. The next differential refresh would either overwrite the change (if the delta touches the same row) or leave the stream table in a state that doesn't match any consistent point-in-time snapshot of the source data.

3. **Change buffer desync.** The CDC triggers on source tables write changes to buffer tables. The refresh engine reads these buffers and advances the frontier. Manual DML on the stream table bypasses this pipeline entirely — the buffer and frontier have no record of the change, so future refreshes cannot account for it.

If you need to post-process stream table data, create a **view** or a **second stream table** that references the first one.

### Why can't I add foreign keys to or from a stream table?

Foreign key constraints require that referenced/referencing rows exist at the time of each DML statement. The refresh engine violates this assumption:

1. **Bulk `MERGE` ordering.** A differential refresh executes a single `MERGE INTO` statement that applies all deltas (inserts and deletes) atomically. PostgreSQL evaluates FK constraints row-by-row within this `MERGE`. If a parent row is deleted and a new parent inserted in the same delta batch, the child FK check may fail because it sees the delete before the insert — even though the final state would be consistent.

2. **Full refresh uses `TRUNCATE` + `INSERT`.** In FULL mode, the refresh engine truncates the stream table and re-inserts all rows. `TRUNCATE` does not fire individual `DELETE` triggers and bypasses FK cascade logic, which would leave referencing tables with dangling references.

3. **Cross-table refresh ordering.** If stream table A has an FK referencing stream table B, both tables refresh independently (in topological order, but in separate transactions). There is no guarantee that A's refresh sees B's latest data — the FK constraint could transiently fail between refreshes.

**Workaround:** Enforce referential integrity in the consuming application or use a view that joins the stream tables and validates the relationship.

### How do user-defined triggers work on stream tables?

When a DIFFERENTIAL mode stream table has user-defined row-level triggers (or `pg_stream.user_triggers = 'on'`), the refresh engine uses **explicit DML decomposition** instead of `MERGE`:

1. **Delta materialized once.** The delta query result is stored in a temporary table (`__pgs_delta_<id>`) to avoid evaluating it three times.

2. **DELETE removed rows.** Rows in the stream table whose `__pgs_row_id` is absent from the delta are deleted. `AFTER DELETE` triggers fire with correct `OLD` values.

3. **UPDATE changed rows.** Rows whose `__pgs_row_id` exists in both the stream table and delta but whose values differ (checked via `IS DISTINCT FROM`) are updated. `AFTER UPDATE` triggers fire with correct `OLD` and `NEW`. No-op updates (where values are identical) are skipped, preventing spurious triggers.

4. **INSERT new rows.** Rows in the delta whose `__pgs_row_id` is absent from the stream table are inserted. `AFTER INSERT` triggers fire with correct `NEW` values.

**FULL refresh behavior:** Row-level user triggers are automatically suppressed during FULL refresh via `DISABLE TRIGGER USER` / `ENABLE TRIGGER USER`. A `NOTIFY pgstream_refresh` is emitted so listeners know a FULL refresh occurred. Use `REFRESH MODE DIFFERENTIAL` for stream tables that need per-row trigger semantics.

**Performance:** The explicit DML path adds ~25–60% overhead compared to MERGE for triggered stream tables. Stream tables without user triggers have zero overhead (only a fast `pg_trigger` check, <0.1 ms).

**Control:** The `pg_stream.user_triggers` GUC controls this behavior:
- `auto` (default): detect user triggers automatically
- `on`: always use explicit DML (useful for testing)
- `off`: always use MERGE, suppressing triggers

### Why can't I `ALTER TABLE` a stream table directly?

Stream table metadata (defining query, schedule, refresh mode) is stored in the pg_stream catalog (`pgstream.pgs_stream_tables`). A direct `ALTER TABLE` would change the physical table without updating the catalog, causing:

1. **Column mismatch.** If you add or remove columns, the refresh engine's cached delta query and `MERGE` statement would reference columns that no longer exist (or miss new ones), causing runtime errors.

2. **`__pgs_row_id` invalidation.** The row ID hash is computed from the defining query's output columns. Altering the table schema without updating the defining query would make existing row IDs inconsistent with the new column set.

Use `pgstream.alter_stream_table()` to change schedule, refresh mode, or status. To change the defining query or column structure, drop and recreate the stream table.

### Why can't I `TRUNCATE` a stream table?

`TRUNCATE` removes all rows instantly but does not update the pg_stream frontier or change buffers. After a `TRUNCATE`:

1. **Differential refresh sees no changes.** The frontier still records the last-processed LSN. No new source changes may have occurred, so the next differential refresh produces an empty delta — leaving the stream table empty even though the source still has data.

2. **No recovery path for differential mode.** The refresh engine has no way to detect that the stream table was externally truncated. It assumes the current contents match the frontier.

Use `pgstream.refresh_stream_table('my_table')` to force a full re-materialization, or drop and recreate the stream table if you need a clean slate.
