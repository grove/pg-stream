# Frequently Asked Questions

---

## General

### What is pg_trickle?

pg_trickle is a PostgreSQL 18 extension that implements **stream tables** — declarative, automatically-refreshing materialized views with **Differential View Maintenance (DVM)**. You define a SQL query and a refresh schedule; the extension handles change capture, delta computation, and incremental refresh automatically.

It is inspired by the [DBSP](https://arxiv.org/abs/2203.16684) differential dataflow framework. See [DBSP_COMPARISON.md](research/DBSP_COMPARISON.md) for a detailed comparison.

### What is incremental view maintenance (IVM) and why does it matter?

**Incremental View Maintenance** means updating a materialized view by processing only the *changes* (deltas) to the source data, rather than re-executing the entire defining query from scratch.

Consider a stream table defined as `SELECT customer_id, SUM(amount) FROM orders GROUP BY customer_id` over a 10-million-row `orders` table. When you insert 5 new rows:

- **Without IVM (FULL refresh):** Re-scans all 10 million rows and recomputes every group. Cost: O(total rows).
- **With IVM (DIFFERENTIAL refresh):** Reads only the 5 new rows from the change buffer, identifies the affected groups, and updates just those groups. Cost: O(changed rows × affected groups).

pg_trickle's DVM engine implements IVM using differentiation rules for each SQL operator (Scan, Filter, Join, Aggregate, etc.), generating a *delta query* that computes the exact changes to the stream table from the exact changes to the source.

### What is the difference between a stream table and a regular materialized view, in practice?

| Feature | Materialized Views | Stream Tables |
|---|---|---|
| Refresh | Manual (`REFRESH MATERIALIZED VIEW`) | Automatic (scheduler) or manual |
| Incremental refresh | Not supported natively | Built-in differential mode |
| Change detection | None — always full recompute | CDC triggers track row-level changes |
| Dependency ordering | None | DAG-aware topological refresh |
| Monitoring | None | Built-in views, stats, NOTIFY alerts |
| Schedule | None | Duration strings (`5m`) or cron (`*/5 * * * *`) |
| Transactional IVM | No | Yes (IMMEDIATE mode) |

In practice, stream tables are regular PostgreSQL heap tables under the hood — you can query them, create indexes on them, join them with other tables, and reference them from views. The key difference is that pg_trickle manages their contents automatically.

### What happens behind the scenes when I INSERT a row into a table tracked by a stream table?

The full data flow for a DIFFERENTIAL-mode stream table:

1. **Your INSERT completes normally.** The row is written to the source table.
2. **A CDC trigger fires** (row-level `AFTER INSERT`). It writes a change record (action=`I`, the new row data as JSONB, the current WAL LSN) into the source's change buffer table (`pgtrickle_changes.changes_<oid>`). This happens within your transaction — if you roll back, the change record is also rolled back.
3. **You commit.** Both the source row and the change record become visible.
4. **The scheduler wakes up** (every `pg_trickle.scheduler_interval_ms`, default 1 second). It checks whether the stream table's schedule says a refresh is due.
5. **If due, the refresh engine runs.** It reads the change buffer for rows with LSN > the stream table's current frontier, generates a delta query from the DVM operator tree, and applies the result via `MERGE`.
6. **Frontier advances.** The stream table's frontier is updated to the new LSN, and the consumed change buffer rows are cleaned up.

For IMMEDIATE-mode stream tables, steps 2–6 are replaced: a statement-level AFTER trigger computes and applies the delta **within your transaction**, so the stream table is updated before your transaction commits.

### What does "differential" mean in the context of pg_trickle?

"Differential" refers to the mathematical approach of computing **differences (deltas)** rather than absolute values. Given a query Q and a set of changes ΔR to source table R, the DVM engine computes ΔQ(R, ΔR) — the *change to the query result* caused by the *change to the source*. This delta is then applied (merged) into the stream table.

Each SQL operator has its own differentiation rule. For example:
- **Filter:** ΔFilter(R, ΔR) = Filter(ΔR) — just apply the filter to the changes.
- **Join:** ΔJoin(R, S, ΔR) = Join(ΔR, S) — join the changes against the other side's current state.
- **Aggregate:** Recompute only the groups whose keys appear in the changes.

See [DVM_OPERATORS.md](DVM_OPERATORS.md) for the complete set of differentiation rules.

### What is a frontier, and why does pg_trickle track LSNs?

A **frontier** is a per-source map of `{source_oid → LSN}` that records exactly how far each stream table has consumed changes from each of its source tables. It is stored as JSONB in the `pgtrickle.pgt_stream_tables` catalog.

**Why LSNs?** PostgreSQL's Write-Ahead Log Sequence Number (LSN) provides a globally ordered, monotonically increasing position in the change stream. By recording the LSN at which each source was last consumed, the frontier ensures:

- **No missed changes.** The next refresh reads changes with LSN > frontier, ensuring contiguous, non-overlapping windows.
- **No duplicate processing.** Changes at or below the frontier are never re-read.
- **Consistent snapshots.** When a stream table depends on multiple source tables, the frontier tracks each source independently, enabling consistent multi-source delta computation.

**Lifecycle:** Created on first full refresh → Advanced on each differential refresh → Reset on reinitialize.

### What is the `__pgt_row_id` column and why does it appear in my stream tables?

Every stream table has a `__pgt_row_id BIGINT PRIMARY KEY` column. It stores a 64-bit xxHash of the row's group-by key (for aggregate queries) or all output columns (for non-aggregate queries). The refresh engine uses it to match incoming deltas against existing rows during the `MERGE` operation.

**You should ignore this column in your queries.** It is an implementation detail. If it bothers you, exclude it explicitly:

```sql
SELECT customer_id, total FROM order_totals;  -- omit __pgt_row_id
```

### What is the auto-rewrite pipeline and how does it affect my queries?

Before parsing a defining query into the DVM operator tree, pg_trickle runs five automatic rewrite passes:

| # | Pass | What it does |
|---|------|-------------|
| 0 | View inlining | Replaces view references with `(view_definition) AS alias` subqueries (fixpoint, max depth 10) |
| 1 | DISTINCT ON | Converts to `ROW_NUMBER() OVER (PARTITION BY … ORDER BY …) = 1` subquery |
| 2 | GROUPING SETS / CUBE / ROLLUP | Decomposes into `UNION ALL` of separate `GROUP BY` queries |
| 3 | Scalar subquery in WHERE | Rewrites `WHERE col > (SELECT …)` to `CROSS JOIN` |
| 4 | SubLinks in OR | Splits `WHERE a OR EXISTS (…)` into `UNION` branches |

The rewrites are transparent — your original query is preserved in the catalog (`original_query` column) while the rewritten version is stored in `defining_query`. The DVM engine only sees standard SQL operators after rewriting.

See [ARCHITECTURE.md](ARCHITECTURE.md) for details on each pass.

### How does pg_trickle compare to DBSP (the academic framework)?

pg_trickle is inspired by [DBSP](https://arxiv.org/abs/2203.16684) but is not a direct implementation. Key differences:

- **DBSP** is a general-purpose differential dataflow framework with a Rust runtime (Feldera). It models computation as circuits over Z-sets (multisets with integer weights).
- **pg_trickle** implements the same mathematical principles (delta queries, frontier tracking) but embedded inside PostgreSQL as an extension. It generates SQL delta queries rather than running a separate computation engine.
- **Trade-off:** pg_trickle leverages PostgreSQL's optimizer, indexes, and storage engine but is limited to what can be expressed as SQL queries. DBSP can implement arbitrary dataflow computations.

See [DBSP_COMPARISON.md](research/DBSP_COMPARISON.md) for a detailed comparison.

### How does pg_trickle compare to pg_ivm?

| Feature | pg_ivm | pg_trickle |
|---|---|---|
| Refresh timing | Immediate (same transaction) only | Immediate, Deferred (scheduled), or Manual |
| Incremental strategy | Transition tables + query rewriting | DVM operator tree + delta SQL generation |
| Supported SQL | Inner joins, simple outer joins, COUNT/SUM/AVG/MIN/MAX, EXISTS, DISTINCT | All of the above + window functions, recursive CTEs, LATERAL, UNION/INTERSECT/EXCEPT, 37 aggregates, TopK, GROUPING SETS |
| Cascading (view-on-view) | No | Yes (DAG-aware topological refresh) |
| Scheduling | None (always immediate) | Duration, cron, CALCULATED, or NULL |
| Monitoring | None | Built-in views, stats, NOTIFY alerts |
| PostgreSQL version | 14–17 | 18 only (until v0.4.0) |

pg_trickle's IMMEDIATE mode is designed as a migration path for pg_ivm users — it uses the same statement-level trigger approach with transition tables.

### What PostgreSQL versions are supported?

**PostgreSQL 18.x** exclusively. The extension uses features specific to PostgreSQL 18. Backward compatibility with PostgreSQL 16–17 is planned for v0.4.0.

### Does pg_trickle require `wal_level = logical`?

**No.** pg_trickle uses lightweight row-level triggers for change data capture, not logical replication. You do not need to set `wal_level = logical` or configure `max_replication_slots`.

### Is pg_trickle production-ready?

pg_trickle is under active development. It has a comprehensive test suite (700+ unit tests, 290+ end-to-end tests), but users should evaluate it against their specific workloads before deploying to production.

---

## Installation & Setup

### How do I install pg_trickle?

1. Add `pg_trickle` to `shared_preload_libraries` in `postgresql.conf`:
   ```ini
   shared_preload_libraries = 'pg_trickle'
   ```
2. Restart PostgreSQL.
3. Run:
   ```sql
   CREATE EXTENSION pg_trickle;
   ```

See [INSTALL.md](../INSTALL.md) for platform-specific instructions and pre-built release artifacts.

### What are the minimum configuration requirements?

Only `shared_preload_libraries = 'pg_trickle'` is mandatory (requires a restart). All other settings have sensible defaults. `max_worker_processes = 8` is recommended.

### Can I install pg_trickle on a managed PostgreSQL service (RDS, Cloud SQL, etc.)?

It depends on whether the service allows custom extensions and `shared_preload_libraries`. Since pg_trickle does **not** require `wal_level = logical`, it avoids one of the most common restrictions on managed services. Check your provider's documentation for custom extension support.

### How do I uninstall pg_trickle?

1. Drop all stream tables first (or they will be cascade-dropped):
   ```sql
   SELECT pgtrickle.drop_stream_table(pgt_name) FROM pgtrickle.pgt_stream_tables;
   ```
2. Drop the extension:
   ```sql
   DROP EXTENSION pg_trickle CASCADE;
   ```
3. Remove `pg_trickle` from `shared_preload_libraries` and restart PostgreSQL.

---

## Creating & Managing Stream Tables

### How do I create a stream table?

```sql
SELECT pgtrickle.create_stream_table(
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
- **IMMEDIATE** — Maintains the stream table synchronously within the same transaction as the base table DML. Uses statement-level triggers with transition tables — no change buffers, no scheduler. The stream table is always up-to-date.

### When should I use FULL vs. DIFFERENTIAL vs. IMMEDIATE?

Use **DIFFERENTIAL** (default) when:
- Source tables are large and changes between refreshes are small
- The defining query uses supported operators (most common SQL is supported)
- Some staleness (seconds to minutes) is acceptable

Use **FULL** when:
- The defining query uses unsupported aggregates (`CORR`, `COVAR_*`, `REGR_*`)
- Source tables are small and a full recompute is cheap
- You see frequent adaptive fallbacks to FULL (check refresh history)

Use **IMMEDIATE** when:
- The stream table must always reflect the latest committed data
- You need transactional consistency (reads within the same transaction see updated data)
- Write-side overhead per DML statement is acceptable
- The defining query is relatively simple (no TopK, no materialized view sources)

### What are the advantages and disadvantages of IMMEDIATE vs. deferred (FULL/DIFFERENTIAL) refresh modes?

**IMMEDIATE mode**

| | Detail |
|---|---|
| ✅ Read-your-writes consistency | The stream table is updated within the same transaction as the base table DML — always current from the writer's perspective. |
| ✅ No lag | No background worker, no schedule interval. The view is never stale. |
| ✅ No change buffers | `pgtrickle_changes.*` tables are not used, reducing write overhead on source tables. |
| ✅ pg_ivm compatibility | Drop-in migration path for existing pg_ivm / IMMV users. |
| ❌ Write amplification | Every DML statement on a base table also executes IVM trigger logic, adding latency to the original transaction. |
| ❌ Serialized concurrent writes | An `ExclusiveLock` is taken on the stream table during maintenance, serializing writers. |
| ❌ Limited SQL support | Window functions, recursive CTEs, `LATERAL` joins, scalar subqueries, and TopK (`ORDER BY … LIMIT`) are not supported — use `DIFFERENTIAL` instead. |
| ❌ No cascading | IMMEDIATE stream tables that depend on other IMMEDIATE stream tables are not supported. |
| ❌ No throttling | The refresh cannot be delayed or rate-limited. |

**Deferred mode (`FULL` / `DIFFERENTIAL`)**

| | Detail |
|---|---|
| ✅ Decoupled write path | Base table writes are fast; view maintenance runs later via the scheduler or manual refresh. |
| ✅ Broadest SQL support | Window functions, recursive CTEs, `LATERAL`, `UNION`, user-defined aggregates, TopK, cascading stream tables, and more. |
| ✅ Adaptive cost control | `DIFFERENTIAL` automatically falls back to `FULL` when the change ratio exceeds `pg_trickle.differential_max_change_ratio`. |
| ✅ Concurrency-friendly | Writers never block on view maintenance. |
| ❌ Staleness | The stream table lags by up to one schedule interval (e.g. `1m`). |
| ❌ No read-your-writes | A writer querying the stream table immediately after a write may see the pre-change data. |
| ❌ Infrastructure overhead | Requires change buffer tables, a background worker, and frontier tracking. |

**Rule of thumb:** use `IMMEDIATE` when the query is simple and freshness within the transaction matters. Use `DIFFERENTIAL` (or `FULL`) for complex queries, high concurrency, or when you want to decouple write latency from view maintenance.

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

The `pg_trickle.min_schedule_seconds` GUC (default: `60`) sets the floor. Schedules shorter than this value are rejected. Set to `1` for development/testing.

### Can a stream table reference another stream table?

**Yes.** Stream tables can depend on other stream tables. The scheduler automatically refreshes them in topological order (upstream first). Circular dependencies are detected and rejected at creation time.

```sql
-- ST1: aggregates orders
SELECT pgtrickle.create_stream_table('order_totals',
    'SELECT customer_id, SUM(amount) AS total FROM orders GROUP BY customer_id',
    '1m', 'DIFFERENTIAL');

-- ST2: filters ST1
SELECT pgtrickle.create_stream_table('big_customers',
    'SELECT customer_id, total FROM pgtrickle.order_totals WHERE total > 1000',
    '1m', 'DIFFERENTIAL');
```

### How do I change a stream table's schedule or mode?

```sql
-- Change schedule
SELECT pgtrickle.alter_stream_table('order_totals', schedule => '10m');

-- Switch refresh mode
SELECT pgtrickle.alter_stream_table('order_totals', refresh_mode => 'FULL');

-- Suspend
SELECT pgtrickle.alter_stream_table('order_totals', status => 'SUSPENDED');

-- Resume
SELECT pgtrickle.alter_stream_table('order_totals', status => 'ACTIVE');
```

### Can I change the defining query of a stream table?

Not directly. You must drop and recreate the stream table:

```sql
SELECT pgtrickle.drop_stream_table('order_totals');
SELECT pgtrickle.create_stream_table('order_totals', '<new query>', '5m', 'DIFFERENTIAL');
```

### How do I trigger a manual refresh?

```sql
SELECT pgtrickle.refresh_stream_table('order_totals');
```

This works even when `pg_trickle.enabled = false` (scheduler disabled).

---

## Data Freshness & Consistency

### How stale can a stream table be?

For **deferred** modes (FULL / DIFFERENTIAL): A stream table can be at most **one schedule interval** behind the source data, plus the time it takes to execute the refresh itself. For example, with `schedule => '1m'`, the maximum staleness is approximately 1 minute + refresh duration.

In practice, staleness is often less than the schedule interval because the scheduler continuously checks for due refreshes at `pg_trickle.scheduler_interval_ms` (default: 1 second).

For **IMMEDIATE** mode: The stream table is **always current** within the transaction that modified the source data. There is zero staleness.

Check current staleness:

```sql
SELECT pgtrickle.get_staleness('order_totals');  -- returns seconds, NULL if never refreshed

-- Or check all stream tables:
SELECT pgt_name, staleness, stale FROM pgtrickle.stream_tables_info;
```

### Can I read my own writes immediately after an INSERT?

**It depends on the refresh mode:**

- **IMMEDIATE mode:** Yes. The stream table is updated within the same transaction as your INSERT. You can query it immediately and see the updated data.
- **DIFFERENTIAL / FULL mode:** No. The stream table is updated by the background scheduler in a separate transaction. Your INSERT is captured by the CDC trigger, but the stream table won't reflect it until the next scheduled refresh (or a manual `refresh_stream_table()` call).

If read-your-writes consistency is a requirement, use `refresh_mode => 'IMMEDIATE'`.

### What consistency guarantees does pg_trickle provide?

pg_trickle provides **Delayed View Semantics (DVS):** the contents of every stream table are logically equivalent to evaluating its defining query at some past point in time — the `data_timestamp`. This means:

- The data is always **internally consistent** — it corresponds to a valid snapshot of the source data.
- The data may be **stale** — it reflects the source state at `data_timestamp`, not necessarily the current state.
- For cascading stream tables, the scheduler refreshes in **topological order** so that when ST B references upstream ST A, A has already been refreshed before B runs its delta query against A's contents.

For IMMEDIATE mode, the guarantee is stronger: the stream table always reflects the state of the source data **as of the current transaction**.

### What are "Delayed View Semantics" (DVS)?

DVS is the formal consistency guarantee: a stream table's contents are equivalent to evaluating its defining query at a specific past time (the `data_timestamp`). This is analogous to how a materialized view captured at a point in time is always internally consistent, even if the source data has since changed.

The `data_timestamp` is recorded in the catalog and advanced after each successful refresh:

```sql
SELECT pgt_name, data_timestamp FROM pgtrickle.pgt_stream_tables;
```

### What happens if the scheduler is behind — does data get lost?

**No.** Change data is never lost, even if the scheduler falls behind. Changes accumulate in the change buffer tables (`pgtrickle_changes.changes_<oid>`) until consumed by a refresh. The frontier ensures that each refresh picks up exactly where the last one left off.

However, a growing change buffer increases:
- Disk usage (change buffer tables grow)
- Refresh time (more changes to process per cycle)
- Risk of adaptive fallback to FULL (if the change ratio exceeds `pg_trickle.differential_max_change_ratio`)

The monitoring system emits a `buffer_growth_warning` NOTIFY alert if buffers grow unexpectedly.

### How does pg_trickle ensure deltas are applied in the right order across cascading stream tables?

The scheduler uses **topological ordering** from the dependency DAG. When ST B depends on ST A:

1. ST A is refreshed first — its data is brought up to date and its frontier advances.
2. ST A's refresh writes are captured by CDC triggers (since ST A is a source for ST B).
3. ST B is refreshed next — its delta query reads ST A's current (just-refreshed) data and the change buffer.

This ensures that downstream stream tables always see consistent upstream data. Circular dependencies are rejected at creation time.

---

## IMMEDIATE Mode (Transactional IVM)

### When should I use IMMEDIATE mode instead of DIFFERENTIAL?

Use IMMEDIATE when:
- Your application requires **read-your-writes consistency** — e.g., a user inserts an order and immediately queries a dashboard that must include that order.
- The defining query is relatively simple (single-table aggregation, joins, filters).
- The source table write rate is moderate (IMMEDIATE adds latency to every DML statement).

Stick with DIFFERENTIAL when:
- Staleness of a few seconds to minutes is acceptable.
- The defining query uses unsupported IMMEDIATE constructs (recursive CTEs, TopK).
- Write-side performance is critical (high-throughput OLTP).
- You need to decouple write latency from view maintenance.

### What SQL features are NOT supported in IMMEDIATE mode?

IMMEDIATE mode supports nearly all constructs that DIFFERENTIAL supports. The exceptions are:

| Feature | Why not supported | Alternative |
|---|---|---|
| Recursive CTEs (`WITH RECURSIVE`) | Not yet validated with transition tables | Use DIFFERENTIAL mode |
| TopK (`ORDER BY … LIMIT N`) | Scoped recomputation requires scheduled refresh | Use DIFFERENTIAL mode |
| Materialized views as sources | Stale-snapshot prevents trigger-based capture | Use the underlying query |
| Foreign tables as sources | No triggers on foreign tables | Use FULL mode |

Attempting to create or switch to IMMEDIATE mode with an unsupported construct produces a clear error message.

### What happens when I TRUNCATE a source table in IMMEDIATE mode?

A statement-level `AFTER TRUNCATE` trigger fires and **truncates the stream table, then re-populates it** by executing a full refresh from the defining query — all within the same transaction. The stream table remains consistent.

### Can I have cascading IMMEDIATE stream tables (ST A → ST B)?

**Yes.** When ST A is IMMEDIATE and ST B depends on ST A and is also IMMEDIATE, changes propagate through the chain within the same transaction. The IVM triggers on the base table update ST A, and since that write is visible within the transaction, ST B's triggers fire and update ST B.

### What locking does IMMEDIATE mode use?

IMMEDIATE mode acquires **statement-level locks** on the stream table during delta application:

- **Simple queries** (single-table scan/filter without aggregates or DISTINCT): `RowExclusiveLock` — allows concurrent readers, blocks other writers.
- **Complex queries** (joins, aggregates, DISTINCT, window functions): `ExclusiveLock` — blocks both readers and writers to ensure delta consistency.

This means concurrent writes to the same base table are **serialized** through the stream table lock. For high-concurrency write workloads, DIFFERENTIAL mode avoids this bottleneck.

### How do I switch an existing DIFFERENTIAL stream table to IMMEDIATE?

```sql
SELECT pgtrickle.alter_stream_table('order_totals', refresh_mode => 'IMMEDIATE');
```

This:
1. Validates the defining query against IMMEDIATE mode restrictions.
2. Removes the row-level CDC triggers from source tables.
3. Installs statement-level IVM triggers (BEFORE + AFTER with transition tables).
4. Clears the schedule (IMMEDIATE mode has no schedule).
5. Performs a full refresh to establish a consistent baseline.

To switch back:

```sql
SELECT pgtrickle.alter_stream_table('order_totals', refresh_mode => 'DIFFERENTIAL');
```

This reverses the process: removes IVM triggers, installs CDC triggers, restores the schedule (default `1m`), and performs a full refresh.

### What happens to IMMEDIATE mode during a manual `refresh_stream_table()` call?

For IMMEDIATE mode stream tables, `refresh_stream_table()` performs a **FULL refresh** — truncates and re-populates from the defining query. This is useful for recovering from edge cases or forcing a clean baseline. It is equivalent to pg_ivm's `refresh_immv(name, true)`.

### How much write-side overhead does IMMEDIATE mode add?

Each DML statement on a base table tracked by an IMMEDIATE stream table incurs:

- **BEFORE trigger:** Advisory lock acquisition + pre-state setup (~0.1–0.5 ms).
- **AFTER trigger:** Transition table copy to temp tables + delta SQL generation + delta application (~1–50 ms depending on query complexity and delta size).

For a simple single-table aggregate, expect **2–10 ms overhead per statement**. For multi-table joins or window functions, overhead is higher. The overhead scales with the number of IMMEDIATE stream tables that depend on the same source table.

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
| `TABLESAMPLE` | Stream tables materialize the full result set | Use `WHERE random() < fraction` in consuming query |
| Window functions in expressions | Cannot be differentially maintained | Move window function to a separate column |
| `LIMIT` / `OFFSET` | Stream tables materialize the full result set | Apply when querying the stream table |
| `FOR UPDATE` / `FOR SHARE` | Row-level locking not applicable | Remove the locking clause |

The following were previously rejected but are **now supported** via automatic parse-time rewrites:

| Feature | How It Works |
|---|---|
| `DISTINCT ON (…)` | Auto-rewritten to `ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...) = 1` subquery |
| `GROUPING SETS` / `CUBE` / `ROLLUP` | Auto-rewritten to `UNION ALL` of separate `GROUP BY` queries |
| `NATURAL JOIN` | Common columns resolved at parse time; explicit equi-join synthesized |
| `ALL (subquery)` | Rewritten to `NOT EXISTS` with negated condition (AntiJoin) |

Each rejected feature is explained in detail in the [Why Are These SQL Features Not Supported?](#why-are-these-sql-features-not-supported) section below.

### What happens to `ORDER BY` in defining queries?

`ORDER BY` is **accepted but silently discarded**. Row order in a stream table is undefined (consistent with PostgreSQL's `CREATE MATERIALIZED VIEW` behavior). Apply `ORDER BY` when **querying** the stream table, not in the defining query.

### Which aggregates support DIFFERENTIAL mode?

**Algebraic** (O(changes), fully incremental): `COUNT`, `SUM`, `AVG`

**Semi-algebraic** (incremental with occasional group rescan): `MIN`, `MAX`

**Group-rescan** (affected groups re-aggregated from source): `STRING_AGG`, `ARRAY_AGG`, `JSON_AGG`, `JSONB_AGG`, `BOOL_AND`, `BOOL_OR`, `BIT_AND`, `BIT_OR`, `BIT_XOR`, `JSON_OBJECT_AGG`, `JSONB_OBJECT_AGG`, `STDDEV`, `STDDEV_POP`, `STDDEV_SAMP`, `VARIANCE`, `VAR_POP`, `VAR_SAMP`, `MODE`, `PERCENTILE_CONT`, `PERCENTILE_DISC`, `CORR`, `COVAR_POP`, `COVAR_SAMP`, `REGR_AVGX`, `REGR_AVGY`, `REGR_COUNT`, `REGR_INTERCEPT`, `REGR_R2`, `REGR_SLOPE`, `REGR_SXX`, `REGR_SXY`, `REGR_SYY`

**37 aggregate function variants** are supported in total.

---

## Aggregates & Group-By

### Which aggregates are fully incremental (O(1) per change) vs. group-rescan?

pg_trickle categorizes aggregates into three tiers:

| Tier | Cost per change | Aggregates | Mechanism |
|---|---|---|---|
| **Algebraic** | O(1) | `COUNT`, `SUM`, `AVG` | Hidden auxiliary columns (`__pgt_count`, `__pgt_sum_x`) track running totals. Delta updates these columns arithmetically. |
| **Semi-algebraic** | O(1) normally, O(group) on extremum deletion | `MIN`, `MAX` | Maintained via `LEAST`/`GREATEST`. If the current MIN/MAX is deleted, the group is rescanned to find the new extremum. |
| **Group-rescan** | O(group size) per affected group | All others (35 functions) | Affected groups are re-aggregated from source data. A NULL sentinel marks stale groups for rescan. |

For most workloads, the algebraic tier (COUNT/SUM/AVG) covers the majority of aggregations and is the fastest.

### Why do some aggregates have hidden auxiliary columns?

For algebraic aggregates (COUNT, SUM, AVG), the DVM engine adds hidden `__pgt_count` and `__pgt_sum_x` columns to the stream table's storage. These store running totals that can be updated with O(1) arithmetic per change instead of rescanning the entire group.

For example, a stream table defined as `SELECT dept, AVG(salary) FROM employees GROUP BY dept` internally stores:
- `dept` — the group-by key
- `avg` — the user-visible average (computed as `__pgt_sum_x / __pgt_count`)
- `__pgt_count` — running count of rows in the group
- `__pgt_sum_x` — running sum of salary values
- `__pgt_row_id` — row identity hash

When a new employee is inserted, the refresh updates `__pgt_count += 1`, `__pgt_sum_x += new_salary`, and recomputes `avg`. No rescan of the source table is needed.

### How does HAVING work with incremental refresh?

`HAVING` is fully supported in DIFFERENTIAL mode. The DVM engine tracks **threshold transitions** — groups entering or exiting the HAVING condition:

- **Group crosses threshold upward:** A previously excluded group (e.g., `HAVING COUNT(*) > 5`) gains enough members → the group is **inserted** into the stream table.
- **Group crosses threshold downward:** A group that was included drops below the threshold → the group is **deleted** from the stream table.
- **Group stays above threshold:** Normal delta update (adjust aggregate values).

This means the stream table always reflects only the groups that satisfy the HAVING clause, even as group membership changes.

### What happens to a group when all its rows are deleted?

When the last row of a group is deleted from the source table, the DVM engine detects that `__pgt_count` drops to zero and **deletes the group row** from the stream table. The hidden auxiliary columns are cleaned up along with it.

If a new row for the same group-by key is later inserted, a fresh group row is created from scratch.

### Why are `CORR`, `COVAR_*`, and `REGR_*` limited to FULL mode?

Regression aggregates like `CORR`, `COVAR_POP`, `COVAR_SAMP`, and the `REGR_*` family require maintaining running sums of products and squares across the entire group. Unlike `COUNT`/`SUM`/`AVG` (where deltas can be computed from the change alone), regression aggregates:

1. **Lack algebraic delta rules.** There is no closed-form way to update a correlation coefficient from a single row change without access to the full group's data.
2. **Would degrade to group-rescan anyway.** Even if supported, the implementation would need to rescan the full group from source — identical to FULL mode for most practical group sizes.

These aggregates work fine in **FULL** refresh mode, which re-runs the entire query from scratch each cycle.

---

## Joins

### How does a DIFFERENTIAL refresh handle a join when both sides changed?

When both tables in a join have changes since the last refresh, the DVM engine computes the join delta using the **standard IVM join rule:**

$$\Delta(R \bowtie S) = (\Delta R \bowtie S) \cup (R \bowtie \Delta S) \cup (\Delta R \bowtie \Delta S)$$

In practice, this means:
1. Join the **changes from the left** against the **current state of the right**.
2. Join the **current state of the left** against the **changes from the right**.
3. Join the **changes from both sides** (handles simultaneous changes to matching keys).

All three parts are combined into a single CTE-based delta query that PostgreSQL executes in one pass.

### Does pg_trickle support FULL OUTER JOIN incrementally?

**Yes.** FULL OUTER JOIN is supported in DIFFERENTIAL mode with an 8-part delta computation. This handles all four cases: matched rows on both sides, left-only rows, right-only rows, and rows that transition between matched and unmatched states as data changes.

The 8 parts cover: new left matches, removed left matches, new right matches, removed right matches, newly matched from left-only, newly matched from right-only, newly unmatched to left-only, and newly unmatched to right-only.

### What happens when a join key is updated and the joined row is simultaneously deleted?

This is a known edge case. When a join key column is updated in the same refresh cycle as the joined-side row is deleted, the delta may miss the required DELETE, potentially leaving a stale row in the stream table.

**Mitigations:**
- The **adaptive FULL fallback** (triggered when the change ratio exceeds `pg_trickle.differential_max_change_ratio`) catches most high-change-rate scenarios where this is likely.
- You can stagger changes across refresh cycles.
- Use FULL mode for tables where this pattern is common.

### Why is NATURAL JOIN rejected?

`NATURAL JOIN` is **now fully supported** — it is no longer rejected. At parse time, pg_trickle resolves the common columns between the two tables and synthesizes explicit equi-join conditions. The internal `__pgt_row_id` column is excluded from common column resolution, so NATURAL JOINs between stream tables also work correctly.

---

## CTEs & Recursive Queries

### Do recursive CTEs work in DIFFERENTIAL mode?

**Yes.** pg_trickle supports `WITH RECURSIVE` in DIFFERENTIAL mode with three auto-selected strategies:

| Strategy | When used | How it works |
|---|---|---|
| **Semi-naive evaluation** | INSERT-only changes to the base case | Iteratively evaluates new derivations from the inserted rows without touching existing rows. Fastest path. |
| **Delete-and-Rederive (DRed)** | Mixed changes (INSERT + DELETE/UPDATE) | Deletes potentially affected derived rows, then rederives them from scratch to determine the true delta. |
| **Recomputation fallback** | Column mismatch or non-monotone recursive terms | Falls back to full recomputation of the recursive CTE. Used when the recursive term contains EXCEPT, Aggregate, Window, DISTINCT, AntiJoin, or INTERSECT SET operators. |

The strategy is selected automatically based on the type of changes and the recursive term's structure.

### What are the three strategies for recursive CTE maintenance?

See the table above. In brief:

- **Semi-naive** is the fast path for append-only workloads (e.g., adding nodes to a tree). It's O(new derivations) — much cheaper than a full re-evaluation.
- **DRed** handles deletions and updates correctly by first removing potentially invalidated rows and then rederiving them. More expensive than semi-naive, but still incremental.
- **Recomputation** is the safe fallback that re-executes the entire recursive CTE. Used when the recursive term's structure is too complex for incremental processing.

### What triggers a fallback from semi-naive to recomputation?

A recomputation fallback is triggered when:

1. **The recursive term contains non-monotone operators** — `EXCEPT`, `Aggregate`, `Window`, `DISTINCT`, `AntiJoin`, or `INTERSECT SET`. These operators can "un-derive" rows when inputs change, which semi-naive evaluation cannot handle.
2. **Column mismatch** — the CTE's output columns don't match the stream table's storage schema (e.g., after a schema change).
3. **Mixed DML with non-monotone terms** — DELETE or UPDATE changes combined with non-monotone recursive terms always trigger recomputation.

Check which strategy was used in the refresh history:

```sql
SELECT action, rows_inserted, rows_deleted
FROM pgtrickle.get_refresh_history('my_recursive_st', 5);
```

### What happens when a CTE is referenced multiple times in the same query?

When a non-recursive CTE is referenced more than once, pg_trickle uses **shared delta computation** — the CTE's delta is computed once and cached, then reused by each reference. This is tracked via `CteScan` operator nodes that look up the shared delta from an internal CTE registry.

For single-reference CTEs, pg_trickle simply inlines them as subqueries (no overhead).

---

## Window Functions & LATERAL

### How are window functions maintained incrementally?

pg_trickle uses **partition-based recomputation** for window functions. When source data changes, the DVM engine:

1. Identifies which **partitions** are affected by the changes (based on the `PARTITION BY` key).
2. Recomputes the window function for **only the affected partitions**.
3. Replaces the old partition results with the new ones in the stream table.

This is more efficient than a full recomputation when changes affect a small number of partitions.

### Why can't I use a window function inside a CASE or COALESCE expression?

Window functions like `ROW_NUMBER() OVER (…)` are supported as **standalone columns** but cannot be embedded in expressions (e.g., `CASE WHEN ROW_NUMBER() OVER (...) = 1 THEN ...`).

This restriction exists because the DVM engine handles window functions by recomputing entire partitions. When a window function is buried inside an expression, the engine cannot isolate the window computation from the surrounding expression.

**Rewrite:** Move the window function to a separate column in one stream table, then reference it in a second stream table:

```sql
-- ST1: compute the window function
SELECT id, dept, salary,
       ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn
FROM employees

-- ST2: use it in an expression (references ST1)
SELECT id, CASE WHEN rn = 1 THEN 'top' ELSE 'other' END AS rank_label
FROM st1
```

### What LATERAL constructs are supported?

pg_trickle supports three kinds of `LATERAL` constructs:

| Construct | Example | Delta strategy |
|---|---|---|
| **Set-returning functions** | `LATERAL jsonb_array_elements(data)` | Row-scoped recomputation — only affected parent rows are re-expanded |
| **Correlated subqueries** | `LATERAL (SELECT ... WHERE t.id = s.id)` | Row-scoped recomputation |
| **JSON_TABLE** (PG 17+) | `JSON_TABLE(data, '$.items[*]' ...)` | Modeled as `LateralFunction` |

Additional supported SRFs: `jsonb_each`, `jsonb_each_text`, `unnest`, `generate_series`, and others.

### What happens when a row moves between window partitions during a refresh?

When a row's `PARTITION BY` key changes (e.g., an employee moves departments), the DVM engine recomputes **both** the old partition (to remove the row) and the new partition (to add it). Both partitions are re-evaluated from the source data, ensuring window function results are correct.

---

## TopK (ORDER BY … LIMIT)

### How does `ORDER BY … LIMIT N` work in a stream table?

When a defining query has a top-level `ORDER BY … LIMIT N` (with a constant integer N and no OFFSET), pg_trickle recognizes it as a **TopK pattern**. The stream table stores only the top-N rows and is refreshed via a MERGE-based scoped-recomputation strategy:

1. On each refresh, the full query (with ORDER BY + LIMIT) is re-executed against the source tables.
2. The result is merged into the stream table using `MERGE` with `NOT MATCHED BY SOURCE` for deletes.
3. The catalog records `topk_limit` and `topk_order_by` for the stream table.

TopK bypasses the DVM delta pipeline — it always re-executes the bounded query. This is efficient because the result set is bounded by N.

```sql
SELECT pgtrickle.create_stream_table('top_customers',
    'SELECT customer_id, total FROM order_totals ORDER BY total DESC LIMIT 100',
    '1m', 'DIFFERENTIAL');
```

### Why is OFFSET not supported with TopK?

`OFFSET` combined with `LIMIT` creates a **sliding window** (`LIMIT 10 OFFSET 20` = rows 21–30). When source data changes, rows shift positions, causing the entire window to shift. This makes incremental maintenance impractical — every change could evict and admit different rows.

TopK without OFFSET has a stable boundary: only rows that cross the N-th position threshold change membership. `OFFSET` destroys this stability.

### What happens when a row below the top-N cutoff rises above it?

On the next refresh, the full `ORDER BY … LIMIT N` query is re-executed. The newly qualifying row appears in the result, and the row that fell out of the top-N is removed. The MERGE operation handles this by:

- **INSERT** the newly qualifying row
- **DELETE** the row that fell below the cutoff
- **UPDATE** any rows whose values changed but remained in the top-N

Since TopK always re-executes the bounded query, it correctly detects all ranking changes.

### Can I use TopK with aggregates or joins?

**Yes.** The defining query can contain any SQL that pg_trickle supports, plus `ORDER BY … LIMIT N`:

```sql
-- TopK over an aggregate
SELECT dept, SUM(salary) AS total_salary
FROM employees GROUP BY dept
ORDER BY total_salary DESC LIMIT 10

-- TopK over a join
SELECT e.name, d.name AS dept, e.salary
FROM employees e JOIN departments d ON e.dept_id = d.id
ORDER BY e.salary DESC LIMIT 20
```

The only restriction is that TopK cannot be combined with set operations (`UNION`/`INTERSECT`/`EXCEPT`), `GROUPING SETS`/`CUBE`/`ROLLUP`, or `OFFSET`.

---

## Tables Without Primary Keys

### Do source tables need a primary key?

**No, but it is strongly recommended.** When a source table has a primary key, pg_trickle uses it to generate a deterministic `__pgt_row_id` for each row. Without a primary key, pg_trickle falls back to **content-based hashing** — a hash of all column values.

### What are the risks of using tables without primary keys?

Content-based row identity has known limitations with **exact duplicate rows** (rows where every column value is identical):

1. **INSERT as no-op:** If a row identical to an existing one is inserted, both have the same `__pgt_row_id` hash, so the MERGE treats it as a no-op (the row already exists).
2. **DELETE removes all copies:** Deleting one of N identical rows generates a DELETE delta, but the MERGE removes all rows with that `__pgt_row_id`.
3. **Aggregate drift:** Over time, these mismatches can cause aggregate values to drift from the true result.

**Recommendation:** Add a primary key or unique constraint to source tables, or use FULL mode for tables with frequent exact-duplicate rows.

### How does content-based row identity work for duplicate rows?

For tables without a primary key, `__pgt_row_id` is computed as `pg_trickle_hash_multi(ARRAY[col1::text, col2::text, ...])` — an xxHash of all column values. Rows with identical content produce identical hashes.

The hash uses `\x1E` (record separator) between values and `\x00NULL\x00` for NULL values, minimizing collision risk for rows with different content. However, truly identical rows (same values in every column) will always hash to the same value — this is inherent to content-based identity.

---

## Change Data Capture (CDC)

### How does pg_trickle capture changes to source tables?

pg_trickle installs `AFTER INSERT/UPDATE/DELETE` row-level PL/pgSQL triggers on each source table. These triggers write change records (action, old/new row data as JSONB, LSN, transaction ID) into per-source buffer tables in the `pgtrickle_changes` schema.

### What is the overhead of CDC triggers?

Approximately **20–55 μs per row** (PL/pgSQL dispatch + `row_to_json()` + buffer INSERT). At typical write rates (<1000 writes/sec per source table), this adds **less than 5%** DML latency overhead.

### What happens when I `TRUNCATE` a source table?

**TRUNCATE is now captured** via a statement-level `AFTER TRUNCATE` trigger that writes a `T` marker row to the change buffer. When the differential refresh engine detects this marker, it automatically falls back to a full refresh for that cycle, ensuring the stream table stays consistent.

Previously, TRUNCATE bypassed row-level triggers entirely. This is no longer a concern — both FULL and DIFFERENTIAL mode stream tables handle TRUNCATE correctly.

### Are CDC triggers automatically cleaned up?

Yes. When the last stream table referencing a source is dropped, the trigger and its associated change buffer table are automatically removed.

### What happens if a source table is dropped or altered?

pg_trickle has DDL event triggers that detect `ALTER TABLE` and `DROP TABLE` on source tables. When detected:
- Affected stream tables are marked with `needs_reinit = true`
- The next refresh cycle performs a full reinitialization (drops and recreates the storage table)
- A `reinitialize_needed` NOTIFY alert is sent

### How do I check if a source table has switched from trigger-based CDC to WAL-based CDC?

When you enable hybrid CDC (`pg_trickle.cdc_mode = 'auto'`), pg_trickle starts capturing changes with triggers and can automatically transition to WAL-based logical replication once conditions are met. There are several ways to check the current CDC mode for each source table:

**1. Query the dependency catalog directly:**

```sql
SELECT d.source_relid, c.relname AS source_table, d.cdc_mode,
       d.slot_name, d.decoder_confirmed_lsn, d.transition_started_at
FROM pgtrickle.pgt_dependencies d
JOIN pg_class c ON c.oid = d.source_relid;
```

The `cdc_mode` column shows one of three values:
- `TRIGGER` — changes are captured via row-level triggers (the default)
- `TRANSITIONING` — the system is in the process of switching from triggers to WAL
- `WAL` — changes are captured via logical replication

**2. Use the built-in health check function:**

```sql
SELECT source_table, cdc_mode, slot_name, lag_bytes, alert
FROM pgtrickle.check_cdc_health();
```

This returns a row per source table with the current mode, replication slot lag (for WAL-mode sources), and any alert conditions such as `slot_lag_exceeds_threshold` or `replication_slot_missing`.

**3. Listen for real-time transition notifications:**

```sql
LISTEN pg_trickle_cdc_transition;
```

pg_trickle sends a `NOTIFY` with a JSON payload whenever a transition starts, completes, or is rolled back. Example payload:

```json
{
  "event": "transition_complete",
  "source_table": "public.orders",
  "old_mode": "TRANSITIONING",
  "new_mode": "WAL",
  "slot_name": "pg_trickle_slot_16384"
}
```

This lets you integrate CDC mode changes into your monitoring stack without polling.

**4. Check the global GUC setting:**

```sql
SHOW pg_trickle.cdc_mode;
```

This shows the *desired* global behavior (`trigger`, `auto`, or `wal`), not the per-table actual state. The per-table state lives in `pgt_dependencies.cdc_mode` as described above.

See [CONFIGURATION.md](CONFIGURATION.md) for details on the `pg_trickle.cdc_mode` and `pg_trickle.wal_transition_timeout` GUCs.

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

### Why does pg_trickle default to triggers instead of logical replication?

pg_trickle defaults to row-level AFTER triggers because they provide **single-transaction atomicity** — the change record is written in the same transaction as the source DML, so: 

1. **No commit-order ambiguity.** The change buffer always reflects committed data; rolled-back transactions never produce partial change records.
2. **No replication slot management.** Logical replication requires creating and monitoring replication slots, which can bloat WAL if the subscriber falls behind. Triggers have no WAL retention side effects.
3. **Works on all hosting providers.** Some managed PostgreSQL services restrict `wal_level = logical` or limit the number of replication slots. Triggers work everywhere, with no configuration changes.
4. **Simpler deployment.** No need for `wal_level = logical`, no publication/subscription setup, and no extra connections for WAL senders.

The trade-off is slightly higher per-row latency (~20–55 μs) compared to WAL decoding (~5–15 μs). For high-throughput workloads, pg_trickle supports an automatic transition to WAL-based CDC via `pg_trickle.cdc_mode = 'auto'`. See ADR-001 and ADR-002 in the architecture documentation for the full rationale.

### How does the trigger-to-WAL automatic transition work?

When `pg_trickle.cdc_mode = 'auto'`, pg_trickle monitors each source table's write rate. When the rate exceeds an internal threshold, the transition proceeds in three phases:

1. **Slot creation.** A logical replication slot is created for the source table's OID (e.g., `pg_trickle_slot_16384`).
2. **Dual capture.** For a brief period, both triggers and WAL decoding capture changes. The system uses LSN comparison to deduplicate, ensuring no changes are lost or double-counted.
3. **Trigger removal.** Once the WAL decoder has confirmed it is caught up (its confirmed LSN ≥ the frontier LSN), the row-level triggers are dropped and the source transitions fully to WAL mode.

The transition is tracked in `pgt_dependencies.cdc_mode` (values: `TRIGGER` → `TRANSITIONING` → `WAL`). If the transition times out (`pg_trickle.wal_transition_timeout`, default 5 minutes), it is rolled back and triggers are kept.

### What happens to CDC if I restore a database backup?

After restoring a backup (pg_dump, pg_basebackup, or PITR), the CDC state depends on the backup type:

| Backup type | Triggers | Change buffers | Frontier | Action needed |
|---|---|---|---|---|
| **pg_dump (logical)** | Preserved (in DDL) | Buffer rows included | Catalog restored | Usually none — next refresh detects stale frontier and does a full refresh |
| **pg_basebackup (physical)** | Preserved | Buffer rows preserved (committed at backup time) | Catalog restored | Replication slots may be invalid — WAL-mode sources may need manual transition back to TRIGGER mode |
| **PITR (point-in-time)** | Preserved | Only committed buffer rows at the recovery target | Catalog restored | Similar to pg_basebackup; frontier may point ahead of actual buffer content → first refresh does a full refresh to reconcile |

In all cases, the pg_trickle scheduler automatically detects frontier inconsistencies and falls back to a full refresh for the first cycle after restore. No manual intervention is required for trigger-mode sources.

For WAL-mode sources, replication slots created after the backup point will not exist in the restored state. Set `pg_trickle.cdc_mode = 'trigger'` temporarily, or let the auto transition recreate slots.

### Do CDC triggers fire for rows inserted via logical replication (subscribers)?

**Yes.** PostgreSQL fires row-level triggers on the subscriber side for rows applied via logical replication. This means if you have a subscriber database with pg_trickle installed, the CDC triggers will capture replicated changes into the local change buffers.

**Implication:** You can run stream tables on a subscriber database that tracks replicated tables — the change capture works transparently. However, be careful about:

- **Double-counting.** If the same table is tracked by pg_trickle on both the publisher and subscriber, changes are captured twice (once on each side). This is fine if the stream tables are independent, but confusing if you expect them to be identical.
- **Replication lag.** The stream table on the subscriber will be delayed by both the replication lag and the pg_trickle refresh schedule.

### Can I inspect the change buffer tables directly?

**Yes.** Change buffers are ordinary tables in the `pgtrickle_changes` schema, named `changes_<source_oid>`:

```sql
-- List all change buffer tables
SELECT tablename FROM pg_tables WHERE schemaname = 'pgtrickle_changes';

-- Inspect recent changes for a source table (find OID first)
SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE c.relname = 'orders' AND n.nspname = 'public';

-- Then query the buffer
SELECT action, lsn, txid, old_data, new_data
FROM pgtrickle_changes.changes_16384
ORDER BY lsn DESC LIMIT 10;
```

The `action` column contains: `I` (insert), `U` (update), `D` (delete), or `T` (truncate).

**Warning:** Do not modify buffer tables directly. The refresh engine manages buffer cleanup (truncation) after each successful refresh. Manual changes will corrupt the frontier tracking.

### How does pg_trickle prevent its own refresh writes from re-triggering CDC?

When the refresh engine writes to a stream table (via MERGE or explicit DML), it does **not** trigger CDC capture on that stream table, even if the stream table is itself a source for a downstream stream table. This is because:

1. **CDC triggers are only installed on source tables**, not on stream tables. The refresh engine writes directly to the stream table's storage without going through any change-capture mechanism.
2. **Downstream change propagation uses a different path.** When stream table A is a source for stream table B, changes to A are detected at B's refresh time by re-reading A's data (not via triggers on A). The topological ordering ensures A is refreshed before B.

This design prevents infinite loops (A triggers B triggers A) and avoids the overhead of capturing changes to materialized output that will be recomputed anyway.

---

## Diamond Dependencies & DAG Scheduling

### What is a diamond dependency and why does it matter?

A **diamond dependency** occurs when two (or more) intermediate stream tables both depend on the same source, and a downstream stream table depends on both of them:

```
       Source: orders
       /             \
  ST: totals      ST: counts
       \             /
    ST: combined_report
```

Without coordination, `combined_report` might be refreshed after `totals` is updated but before `counts` is updated (or vice versa), producing a temporarily inconsistent snapshot — `totals` reflects the latest data but `counts` is stale.

### What does `diamond_consistency = 'atomic'` do?

When `diamond_consistency = 'atomic'` is set on the downstream stream table (e.g., `combined_report`), pg_trickle ensures that **all upstream stream tables in the diamond are refreshed within the same scheduler cycle** before the downstream table is refreshed. This guarantees a consistent point-in-time snapshot.

If any upstream refresh in the atomic group fails, the downstream refresh is **skipped** for that cycle to avoid inconsistency. The failed upstream will be retried on the next cycle.

```sql
SELECT pgtrickle.alter_stream_table('combined_report',
    diamond_consistency => 'atomic');
```

### What is the difference between `'fastest'` and `'slowest'` schedule policy?

When a stream table has multiple upstream dependencies with different schedules, pg_trickle needs a policy for when to refresh the downstream table:

| Policy | Behavior | Best for |
|---|---|---|
| `fastest` | Refresh downstream whenever **any** upstream refreshes | Low-latency dashboards where partial freshness is acceptable |
| `slowest` | Refresh downstream only after **all** upstreams have refreshed | Reports requiring all-or-nothing consistency |

The default is `fastest`. Use `slowest` with `diamond_consistency = 'atomic'` for the strongest consistency guarantees.

### What happens when an atomic diamond group partially fails?

When `diamond_consistency = 'atomic'` is set and one upstream stream table in the diamond fails to refresh:

1. The **downstream** refresh is **skipped** for that cycle (it reads stale-but-consistent data from the previous successful cycle).
2. The **failed upstream** follows the normal retry logic (exponential backoff, up to `max_consecutive_errors`).
3. Other **non-failing upstreams** in the diamond are still refreshed normally — their data is fresh, but the downstream won't consume it until all upstreams succeed.
4. A `NOTIFY pg_trickle_alert` with event `diamond_partial_failure` is sent so your monitoring can detect the situation.

### How does pg_trickle determine topological refresh order?

The scheduler builds a **directed acyclic graph (DAG)** of stream table dependencies at startup and after any `create_stream_table` / `drop_stream_table` call. The algorithm:

1. **Edge discovery.** For each stream table, the defining query's source tables are extracted. If a source table is itself a stream table, a dependency edge is added.
2. **Cycle detection.** The DAG is checked for cycles. If a cycle is detected, the offending `create_stream_table` call is rejected with a clear error message listing the cycle path.
3. **Topological sort.** A Kahn's algorithm topological sort produces the refresh order — leaf nodes (no stream table dependencies) are refreshed first, then their dependents, and so on.
4. **Level assignment.** Each stream table is assigned a "level" (0 for leaves, max(parent levels) + 1 for dependents). Stream tables at the same level could theoretically be refreshed in parallel (not yet implemented).

The topological order is recalculated whenever the DAG changes. You can inspect it with:

```sql
SELECT pgt_name, depends_on, topo_level
FROM pgtrickle.stream_tables_info
ORDER BY topo_level, pgt_name;
```

---

## Schema Changes & DDL Events

### What happens when I add a column to a source table?

Adding a column to a source table is **safe and non-disruptive** if the stream table's defining query does not use `SELECT *`:

- **Named columns:** If the defining query explicitly lists columns (e.g., `SELECT id, name, amount FROM orders`), the new column is simply not captured by CDC and has no effect on the stream table.
- **`SELECT *`:** If the defining query uses `SELECT *`, pg_trickle detects the schema mismatch at the next refresh and marks the stream table with `needs_reinit = true`. The next scheduler cycle performs a full reinitialization — drops the storage table, recreates it with the new column set, and does a full refresh.

CDC triggers capture the full row as JSONB regardless of which columns the stream table uses, so no trigger changes are needed.

### What happens when I drop a column used in a stream table's query?

Dropping a column that is referenced in a stream table's defining query will cause the next refresh to **fail** because the column no longer exists in the source table. pg_trickle handles this via:

1. **DDL event trigger** detects the `ALTER TABLE ... DROP COLUMN` and marks all affected stream tables with `needs_reinit = true`.
2. On the **next refresh cycle**, the scheduler attempts reinitialization — but the defining query will fail with a PostgreSQL error (e.g., `column "amount" does not exist`).
3. The stream table moves to **ERROR** status after `max_consecutive_errors` failures.
4. A `reinitialize_needed` NOTIFY alert is sent.

**Resolution:** Drop and recreate the stream table with an updated defining query:

```sql
SELECT pgtrickle.drop_stream_table('order_totals');
SELECT pgtrickle.create_stream_table('order_totals',
    'SELECT id, name FROM orders',  -- updated query without dropped column
    '1m', 'DIFFERENTIAL');
```

### What happens when I `CREATE OR REPLACE` a view used by a stream table?

PostgreSQL event triggers fire on `CREATE OR REPLACE VIEW`, so pg_trickle detects the change and marks dependent stream tables with `needs_reinit = true`. On the next refresh:

- If the **new view definition** is compatible (same output columns, same types), reinitialization succeeds transparently — the stream table is repopulated with the new query logic.
- If the new view definition **changes the output schema** (different columns or types), the delta query will fail and the stream table enters ERROR status.

**Tip:** To avoid disruption, use `pgtrickle.alter_stream_table()` to pause the stream table before replacing the view, then resume after verifying compatibility.

### What happens when I alter or drop a function used in a stream table's query?

If a stream table's defining query calls a user-defined function (e.g., `SELECT my_func(amount) FROM orders`) and that function is altered or dropped:

- **ALTER FUNCTION** (changing the body): pg_trickle does **not** detect this automatically — PostgreSQL does not fire DDL event triggers for function body changes. The stream table continues refreshing with the new function behavior. If this is intentional, no action is needed. If you want a full rebase to the new logic, run a manual full refresh:
  ```sql
  SELECT pgtrickle.refresh_stream_table('my_st', force_full => true);
  ```
- **DROP FUNCTION**: The next refresh fails because the function no longer exists. The stream table enters ERROR status. Recreate the function or drop and recreate the stream table.

### What is reinitialize and when does it trigger?

**Reinitialize** is pg_trickle's mechanism for handling structural changes to source tables. When a stream table is marked with `needs_reinit = true`, the next scheduler cycle performs:

1. **Drop** the existing storage table (the physical heap table backing the stream table).
2. **Recreate** the storage table from the defining query's current output schema.
3. **Full refresh** — run the defining query against current source data and populate the new storage table.
4. **Reset** the frontier to the current LSN.
5. **Clear** the `needs_reinit` flag.

Reinitialize triggers automatically when:
- DDL event triggers detect `ALTER TABLE`, `DROP TABLE`, or `CREATE OR REPLACE VIEW` on source tables or intermediate views.
- A `needs_reinit` NOTIFY alert is sent.
- You can also trigger it manually:
  ```sql
  UPDATE pgtrickle.pgt_stream_tables SET needs_reinit = true WHERE pgt_name = 'my_st';
  ```

### Can I block DDL on tracked source tables?

pg_trickle does not currently block DDL on source tables — it only **reacts** to DDL changes via event triggers. If you want to prevent accidental schema changes on critical source tables, use PostgreSQL's built-in mechanisms:

```sql
-- Revoke ALTER/DROP from application roles
REVOKE ALL ON TABLE orders FROM app_user;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE orders TO app_user;
-- Only the table owner (or superuser) can now ALTER/DROP
```

Alternatively, create a custom event trigger that raises an exception when DDL targets tracked source tables:

```sql
CREATE OR REPLACE FUNCTION prevent_source_ddl() RETURNS event_trigger AS $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_event_trigger_ddl_commands() cmd
        JOIN pgtrickle.pgt_dependencies d ON d.source_relid = cmd.objid
    ) THEN
        RAISE EXCEPTION 'Cannot ALTER/DROP a table tracked by pg_trickle';
    END IF;
END;
$$ LANGUAGE plpgsql;

CREATE EVENT TRIGGER guard_source_ddl ON ddl_command_end
EXECUTE FUNCTION prevent_source_ddl();
```

---

## Performance & Tuning

### How do I tune the scheduler interval?

The `pg_trickle.scheduler_interval_ms` GUC controls how often the scheduler checks for stale stream tables (default: 1000 ms).

| Workload | Recommended Value |
|---|---|
| Low-latency (near real-time) | `100`–`500` |
| Standard | `1000` (default) |
| Low-overhead (many STs, long schedules) | `5000`–`10000` |

### What is the adaptive fallback to FULL?

When the number of pending changes exceeds `pg_trickle.differential_max_change_ratio` (default: 15%) of the source table size, DIFFERENTIAL mode automatically falls back to FULL for that refresh cycle. This prevents pathological delta queries on bulk changes.

- Set to `0.0` to always use DIFFERENTIAL (even on large change sets)
- Set to `1.0` to effectively always use FULL
- Default `0.15` (15%) is a good balance

### How many concurrent refreshes can run?

Controlled by `pg_trickle.max_concurrent_refreshes` (default: 4, range: 1–32). Each concurrent refresh uses a background worker. Increase this if you have many stream tables and available CPU/IO.

### How do I check if my stream tables are keeping up?

```sql
-- Quick overview
SELECT pgt_name, status, staleness, stale
FROM pgtrickle.stream_tables_info;

-- Detailed statistics
SELECT pgt_name, total_refreshes, avg_duration_ms, consecutive_errors, stale
FROM pgtrickle.pg_stat_stream_tables;

-- Recent refresh history for a specific ST
SELECT * FROM pgtrickle.get_refresh_history('order_totals', 10);
```

### What is `__pgt_row_id`?

Every stream table has a `__pgt_row_id BIGINT PRIMARY KEY` column. It stores a 64-bit xxHash of the row's group-by key (or all columns for non-aggregate queries). The refresh engine uses it for delta `MERGE` operations (matching DELETEs and INSERTs by row ID).

**You should ignore this column in your queries.** It is an implementation detail.

### How much disk space do change buffer tables consume?

Each change buffer table stores one row per source-table change (INSERT, UPDATE, DELETE, or TRUNCATE marker). The row size depends on the source table's column count and data types:

| Component | Approximate size |
|---|---|
| `action` column (char) | 1 byte |
| `old_data` / `new_data` (JSONB) | 1–10 KB per row (depends on source columns) |
| `lsn` (pg_lsn) | 8 bytes |
| `txid` (xid8) | 8 bytes |
| **Index** (on lsn) | ~40 bytes per row |

**Rule of thumb:** Buffer tables consume roughly **2–3× the raw row size** of the source change, because both OLD and NEW values are stored as JSONB.

Buffer tables are cleaned up (truncated or deleted) after each successful refresh. If you suspect buffer bloat, check:

```sql
SELECT relname, pg_size_pretty(pg_total_relation_size(oid)) AS size
FROM pg_class
WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'pgtrickle_changes')
ORDER BY pg_total_relation_size(oid) DESC;
```

### What determines whether DIFFERENTIAL or FULL is faster for a given workload?

The breakeven point depends on the **change ratio** — the number of changed rows relative to the total source table size:

| Change ratio | Recommended mode | Why |
|---|---|---|
| < 5% | DIFFERENTIAL | Delta query touches few rows; much cheaper than re-reading everything |
| 5–15% | DIFFERENTIAL (usually) | Still faster, but approaching the crossover |
| 15–50% | FULL | The delta query scans a large fraction of the source anyway; FULL avoids the overhead of delta computation |
| > 50% | FULL | Bulk load scenario — TRUNCATE + INSERT is simpler and faster |

Additional factors:
- **Query complexity:** Queries with many joins or window functions have more expensive delta computation. The crossover shifts lower.
- **Source table size:** For small tables (<10K rows), FULL is nearly always faster because the overhead is negligible.
- **Index presence:** DIFFERENTIAL uses indexes to look up changed rows. Missing indexes on join keys or GROUP BY columns can make delta queries slow.

The adaptive fallback (`pg_trickle.differential_max_change_ratio`, default 0.15) automates this decision per-cycle.

### What are the planner hints and when should I disable them?

Before executing a delta query, pg_trickle sets several session-level planner parameters to guide PostgreSQL toward efficient delta plans:

```sql
SET LOCAL enable_seqscan = off;     -- Prefer index scans for small deltas
SET LOCAL enable_nestloop = on;     -- Nested loops are good for small delta × large table joins
SET LOCAL enable_mergejoin = off;   -- Merge joins are worse for skewed delta sizes
```

These hints are active only during the refresh transaction and are reset afterward.

**When to disable hints:** If you notice that a particular stream table's refresh is slow (check `avg_duration_ms` in `pg_stat_stream_tables`), the planner hints may be suboptimal for that specific query. You can disable them by setting:

```sql
SET pg_trickle.planner_hints = off;
```

This allows PostgreSQL's planner to choose its own strategy. Test both settings and compare `avg_duration_ms`.

### How do prepared statements help refresh performance?

The refresh engine uses PostgreSQL prepared statements (`PREPARE` / `EXECUTE`) for the delta and MERGE queries. On the first refresh, the statement is prepared; subsequent refreshes reuse the cached plan. Benefits:

- **Reduced planning overhead.** For complex delta queries with many joins and CTEs, planning can take 5–50 ms. Prepared statements skip this on subsequent refreshes.
- **Stable plans.** The planner uses generic plans after the 5th execution (PostgreSQL default), avoiding plan instability from statistic fluctuations.

Prepared statements are stored per-session and are invalidated when:
- The stream table is reinitialized (schema change)
- The PostgreSQL connection is recycled
- The session ends

### How does the adaptive FULL fallback threshold work in practice?

The `pg_trickle.differential_max_change_ratio` GUC (default: 0.15) is evaluated **per source table, per refresh cycle:**

1. Before each differential refresh, the engine counts pending changes in the buffer table: `pending_changes = COUNT(*) FROM pgtrickle_changes.changes_<oid>`.
2. It estimates the source table size from `pg_class.reltuples`.
3. If `pending_changes / reltuples > differential_max_change_ratio`, the engine falls back to FULL for that cycle.

**Edge cases:**
- If the source table has `reltuples = 0` (freshly created, no ANALYZE yet), the engine always uses FULL until statistics are available.
- For multi-source stream tables (joins), each source is evaluated independently. If **any** source exceeds the threshold, the entire refresh falls back to FULL.
- The threshold applies to the current cycle only — the next cycle re-evaluates.

### How many stream tables can a single PostgreSQL instance handle?

There is no hard limit. Practical limits depend on:

| Factor | Guideline |
|---|---|
| **Scheduler overhead** | Each cycle iterates all STs; at 1000 STs with 1ms overhead per check, the cycle takes ~1s |
| **Background connections** | 1 per database (the scheduler) + 1 per manual refresh call |
| **Change buffer bloat** | Each source table gets its own buffer table — many sources = many tables in `pgtrickle_changes` |
| **Catalog size** | `pgt_stream_tables` and `pgt_dependencies` grow linearly |
| **Refresh throughput** | Sequential processing means total cycle time = sum of individual refresh times |

**Tested benchmarks:** Up to 500 stream tables on a single instance with <2s total cycle time for DIFFERENTIAL refreshes averaging 3ms each.

### What is the TRUNCATE vs DELETE cleanup trade-off for change buffers?

After each successful refresh, the engine cleans up processed change records from the buffer table. The `pg_trickle.cleanup_use_truncate` GUC (default: `true`) controls the method:

| Method | Pros | Cons |
|---|---|---|
| `TRUNCATE` (default) | Instant — O(1) regardless of row count. Reclaims disk space immediately. | Takes an `ACCESS EXCLUSIVE` lock on the buffer table, briefly blocking concurrent INSERTs from CDC triggers (~0.1 ms typical). |
| `DELETE` | Row-level lock only — no blocking of concurrent CDC writes. | O(N) — proportional to the number of processed rows. Dead tuples require VACUUM to reclaim space. |

**When to switch to DELETE:** If your source table has extremely high write throughput (>10K writes/sec) and you observe brief stalls in DML latency during refresh cleanup, switch to DELETE:

```sql
ALTER SYSTEM SET pg_trickle.cleanup_use_truncate = false;
SELECT pg_reload_conf();
```

For most workloads, TRUNCATE is the better choice because buffer tables are typically emptied completely after each refresh.

---

## Interoperability

### Can PostgreSQL views reference stream tables?

**Yes.** Stream tables are standard heap tables. Views work normally and reflect data as of the most recent refresh.

### Can materialized views reference stream tables?

**Yes**, though it is somewhat redundant (both are physical snapshots). The materialized view requires its own `REFRESH MATERIALIZED VIEW` — it does not auto-refresh when the stream table refreshes.

### Can I replicate stream tables with logical replication?

**Yes.** Stream tables can be published like any ordinary table:

```sql
CREATE PUBLICATION my_pub FOR TABLE pgtrickle.order_totals;
```

**Important caveats:**
- The `__pgt_row_id` column is replicated (it is the primary key)
- Subscribers receive materialized data, not the defining query
- Do **not** install pg_trickle on the subscriber and attempt to refresh the replicated table — it will have no CDC triggers or catalog entries
- Internal change buffer tables are not published by default

### Can I `INSERT`, `UPDATE`, or `DELETE` rows in a stream table directly?

**No.** Stream table contents are managed exclusively by the refresh engine. Direct DML will corrupt the internal state.

### Can I add foreign keys to or from stream tables?

**No.** The refresh engine uses bulk `MERGE` operations that do not respect foreign key ordering. Foreign key constraints on stream tables are not supported.

### Can I add my own triggers to stream tables?

**Yes, for DIFFERENTIAL mode stream tables.** When user-defined row-level triggers are detected (or `pg_trickle.user_triggers = 'on'`), the refresh engine automatically switches from `MERGE` to explicit `DELETE` + `UPDATE` + `INSERT` statements. This ensures triggers fire with the correct `TG_OP`, `OLD`, and `NEW` values.

**Limitations:**
- Row-level triggers do **not** fire during FULL refresh (they are automatically suppressed via `DISABLE TRIGGER USER`). Use `REFRESH MODE DIFFERENTIAL` for stream tables with triggers.
- The `IS DISTINCT FROM` guard prevents no-op `UPDATE` triggers when the aggregate result is unchanged.
- `BEFORE` triggers that modify `NEW` will affect the stored value — the next refresh may "correct" it back, causing oscillation.

See the `pg_trickle.user_triggers` GUC in [CONFIGURATION.md](CONFIGURATION.md) for control options.

### Can I `ALTER TABLE` a stream table directly?

**No.** Use `pgtrickle.alter_stream_table()` to modify schedule, refresh mode, or status. To change the defining query, drop and recreate the stream table.

### Does pg_trickle work with PgBouncer or other connection poolers?

**It depends on the pooling mode.** pg_trickle's background scheduler uses session-level features that are incompatible with **transaction-mode** connection pooling:

| Feature | Issue with Transaction-Mode Pooling |
|---|---|
| `pg_advisory_lock()` | Session-level lock released when connection returns to pool — concurrent refreshes possible |
| `PREPARE` / `EXECUTE` | Prepared statements are session-scoped — "does not exist" errors on different connections |
| `LISTEN` / `NOTIFY` | Notifications lost when listeners change connections |

**Recommended configurations:**

- **Session-mode pooling** (`pool_mode = session`): Fully compatible. The scheduler holds a dedicated connection.
- **Direct connection** (no pooler for the scheduler): Fully compatible. Application queries can still go through a pooler.
- **Transaction-mode pooling** (`pool_mode = transaction`): **Not supported.** The scheduler requires a persistent session.

**Tip:** If your infrastructure requires transaction-mode pooling (e.g., AWS RDS Proxy, Supabase), route the pg_trickle background worker through a direct connection while keeping application traffic on the pooler. Most connection poolers support per-database or per-user routing rules.

---

## dbt Integration

### How do I use pg_trickle with dbt?

Install the **dbt-pgtrickle** package (a pure Jinja SQL macro package — no Python dependencies):

```yaml
# packages.yml
packages:
  - package: pg_trickle/dbt_pgtrickle
    version: ">=0.2.0"
```

Then define a stream table model using the `stream_table` materialization:

```sql
-- models/order_totals.sql
{{ config(
    materialized='stream_table',
    schedule='1m',
    refresh_mode='DIFFERENTIAL'
) }}

SELECT customer_id, SUM(amount) AS total
FROM {{ source('public', 'orders') }}
GROUP BY customer_id
```

The `stream_table` materialization calls `pgtrickle.create_stream_table()` on the first run and `pgtrickle.alter_stream_table()` on subsequent runs (if the schedule or mode changes).

### What dbt commands work with stream tables?

| Command | Behavior |
|---|---|
| `dbt run` | Creates stream tables that don't exist; updates schedule/mode if changed; **does not** alter the defining query of existing STs |
| `dbt run --full-refresh` | Drops and recreates all stream tables from scratch (new defining query, fresh data) |
| `dbt test` | Works normally — tests query the stream table as a regular table |
| `dbt source freshness` | Works if you configure a freshness block on the stream table source |
| `dbt docs generate` | Documents stream tables like any other model |

### How does `dbt run --full-refresh` work with stream tables?

When `--full-refresh` is passed, the `stream_table` materialization:

1. Calls `pgtrickle.drop_stream_table('model_name')` to remove the existing stream table, CDC triggers, and change buffers.
2. Calls `pgtrickle.create_stream_table(...)` with the current defining query from the model file.
3. The new stream table starts in INITIALIZING status and performs its first full refresh.

This is the correct way to update a stream table's defining query in dbt. Without `--full-refresh`, dbt will **not** detect query changes (it only compares schedule and mode).

### How do I check stream table freshness in dbt?

Use dbt's built-in `source freshness` feature by adding a freshness block to your source definition:

```yaml
# models/sources.yml
sources:
  - name: pgtrickle
    schema: pgtrickle
    tables:
      - name: order_totals
        loaded_at_field: "last_refreshed_at"  # from stream_tables_info
        freshness:
          warn_after: {count: 5, period: minute}
          error_after: {count: 15, period: minute}
```

Then run `dbt source freshness` to check.

Alternatively, query the pg_trickle monitoring views directly in a dbt test:

```sql
-- tests/check_freshness.sql
SELECT pgt_name FROM pgtrickle.stream_tables_info WHERE stale = true
```

### What happens when the defining query changes in dbt?

If you modify the SQL in a stream table model file and run `dbt run` **without** `--full-refresh`:

- The `stream_table` materialization detects that the stream table already exists.
- It compares the schedule and refresh mode — if either changed, it calls `alter_stream_table()` to update them.
- It does **not** compare the defining query text. The existing defining query remains in effect.

To apply a new defining query, you must run `dbt run --full-refresh`. This drops and recreates the stream table with the new query.

**Recommendation:** After changing a model's SQL, always run `dbt run --full-refresh -s model_name` to apply the change.

### Can I use `dbt snapshot` with stream tables?

**Yes, with caveats.** dbt snapshots work by tracking changes to a source table over time using `updated_at` or `check` strategies. You can snapshot a stream table like any other table.

However, keep in mind:
- Stream tables are refreshed periodically, not on every write. The snapshot will only capture changes at refresh boundaries, not at the granularity of individual source-table writes.
- The `__pgt_row_id` column will appear in the snapshot. You may want to exclude it with `check_cols` or a `select` in the snapshot configuration.
- FULL refresh mode replaces all rows each cycle, which will appear as "updates" to the snapshot strategy even if the data hasn't changed. Use DIFFERENTIAL mode for stream tables that are snapshotted.

### What dbt versions are supported?

dbt-pgtrickle is a pure Jinja SQL macro package that works with:

- **dbt-core** 1.7+ (the `stream_table` materialization uses standard Jinja patterns)
- **dbt-postgres** adapter (required for PostgreSQL connection)

There are no Python dependencies beyond dbt-core and dbt-postgres. The package is tested against dbt 1.7.x and 1.8.x in CI.

---

## Deployment & Operations

### How many background workers does pg_trickle use?

pg_trickle registers **1 background worker per database** that contains stream tables. The worker is registered during `_PG_init()` (extension load) and starts automatically when PostgreSQL starts.

| Component | Workers | Notes |
|---|---|---|
| Scheduler | 1 per database | Persistent; checks for stale STs every `scheduler_interval_ms` |
| WAL decoder | 0 (shared) | Shares the scheduler's SPI connection |
| Manual refresh | 0 | Runs in the caller's session |

Ensure `max_worker_processes` (default 8) has room for the pg_trickle worker plus any other extensions.

### How do I upgrade pg_trickle to a new version?

1. **Install the new shared library** (replace the `.so`/`.dylib` file in PostgreSQL's `lib` directory).
2. **Run the upgrade SQL:**
   ```sql
   ALTER EXTENSION pg_trickle UPDATE;
   ```
   This applies migration scripts (e.g., `pg_trickle--0.1.3--0.2.0.sql`) that update catalog tables, add new functions, and migrate data as needed.
3. **Restart PostgreSQL** if the shared library changed (required for `shared_preload_libraries` changes).
4. **Verify:**
   ```sql
   SELECT pgtrickle.version();
   ```

**Zero-downtime upgrades** are possible for minor versions (patch releases) that don't change the shared library. Just run `ALTER EXTENSION pg_trickle UPDATE` — no restart needed.

### What happens to stream tables during a PostgreSQL restart?

During a restart:
1. **The scheduler stops.** No refreshes occur while PostgreSQL is down.
2. **CDC triggers are inactive.** Source table writes during the restart window are captured when PostgreSQL comes back up (triggers are persistent DDL objects).
3. **On startup**, the scheduler background worker starts, reads the catalog, rebuilds the DAG, and resumes refresh cycles from where it left off.
4. **Frontier reconciliation.** The scheduler detects any gap between the stored frontier LSN and the current WAL position. Source changes that occurred between the last successful refresh and the restart are in the change buffers (for trigger-mode CDC) and will be processed in the first refresh cycle.

**Net effect:** Stream tables may be stale for the duration of the downtime, but no data is lost. The first refresh cycle after restart catches up automatically.

### Can I use pg_trickle on a read replica / standby?

**The scheduler does not run on standby servers.** When pg_trickle detects it is running in recovery mode (`pg_is_in_recovery() = true`), the background worker enters a sleep loop and does not attempt any refreshes.

However, stream tables replicated from the primary are **readable** on the standby — they are regular heap tables and are replicated via physical (streaming) replication like any other table.

**Pattern for read-heavy workloads:**
- Run pg_trickle on the **primary** — it performs all refreshes.
- Query stream tables on the **standby** — read replicas get the latest refreshed data via streaming replication, with replication lag as the only additional delay.

### How does pg_trickle work with CloudNativePG / Kubernetes?

pg_trickle is compatible with CloudNativePG. The `cnpg/` directory in the repository contains example manifests:

- [Dockerfile.ext](../cnpg/Dockerfile.ext) — builds a PostgreSQL image with pg_trickle pre-installed
- [cluster-example.yaml](../cnpg/cluster-example.yaml) — CloudNativePG Cluster manifest with `shared_preload_libraries = 'pg_trickle'`

Key considerations:
- Include `pg_trickle` in `shared_preload_libraries` in the Cluster's `postgresql` configuration.
- The scheduler runs on the **primary** pod only. Replica pods detect recovery mode and sleep.
- Pod restarts are handled the same way as PostgreSQL restarts (see above).
- Persistent volume claims preserve catalog and change buffers across pod rescheduling.

### Does pg_trickle work with partitioned source tables?

**Yes.** pg_trickle installs CDC triggers on the **partitioned parent table**, which PostgreSQL automatically propagates to all existing and future partitions. When a row is inserted into any partition, the trigger fires and writes the change to the buffer table.

**Caveats:**
- `TRUNCATE` on individual partitions fires the partition-level trigger, which is also captured.
- Attaching or detaching partitions (`ALTER TABLE ... ATTACH/DETACH PARTITION`) fires DDL event triggers, which may mark the stream table for reinitialization.
- Row movement between partitions (when the partition key is updated) is captured as a DELETE from the old partition and an INSERT into the new partition.

### Can I run pg_trickle in multiple databases on the same cluster?

**Yes.** Each database gets its own independent scheduler background worker, its own catalog tables, and its own change buffers. Stream tables in different databases do not interact.

**Resource planning:** Each database with stream tables requires 1 background worker slot in `max_worker_processes`. If you have 3 databases using pg_trickle, you need at least 3 worker slots.

```sql
-- On each database where you want pg_trickle:
CREATE EXTENSION pg_trickle;
```

The extension must be created separately in each database — `shared_preload_libraries` loads the shared library cluster-wide, but the SQL objects (catalog tables, functions) are per-database.

---

## Monitoring & Alerting

### What monitoring views are available?

| View | Description |
|---|---|
| `pgtrickle.stream_tables_info` | Status overview with computed staleness |
| `pgtrickle.pg_stat_stream_tables` | Comprehensive stats (refresh counts, avg duration, error streaks) |

### How do I get alerted when something goes wrong?

pg_trickle sends PostgreSQL `NOTIFY` messages on the `pg_trickle_alert` channel with JSON payloads:

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
LISTEN pg_trickle_alert;
```

### What happens when a stream table keeps failing?

After `pg_trickle.max_consecutive_errors` (default: 3) consecutive failures, the stream table moves to `ERROR` status and automatic refreshes stop. An `auto_suspended` NOTIFY alert is sent.

To recover:
```sql
-- Fix the underlying issue (e.g., restore a dropped source table), then:
SELECT pgtrickle.alter_stream_table('my_table', status => 'ACTIVE');
```

Retries use exponential backoff (base 1s, max 60s, ±25% jitter, up to 5 retries before counting as a real failure).

---

## Configuration Reference

| GUC | Type | Default | Description |
|---|---|---|---|
| `pg_trickle.enabled` | bool | `true` | Enable/disable the scheduler. Manual refreshes still work when `false`. |
| `pg_trickle.scheduler_interval_ms` | int | `1000` | Scheduler wake interval in milliseconds (100–60000) |
| `pg_trickle.min_schedule_seconds` | int | `60` | Minimum allowed schedule duration (1–86400) |
| `pg_trickle.max_consecutive_errors` | int | `3` | Failures before auto-suspending (1–100) |
| `pg_trickle.change_buffer_schema` | text | `pgtrickle_changes` | Schema for CDC buffer tables |
| `pg_trickle.max_concurrent_refreshes` | int | `4` | Max parallel refresh workers (1–32) |
| `pg_trickle.user_triggers` | text | `auto` | User trigger handling: `auto` (detect), `on` (always explicit DML), `off` (suppress) |
| `pg_trickle.differential_max_change_ratio` | float | `0.15` | Change ratio threshold for adaptive FULL fallback (0.0–1.0) |
| `pg_trickle.cleanup_use_truncate` | bool | `true` | Use TRUNCATE instead of DELETE for buffer cleanup |

All GUCs are `SUSET` context (superuser SET) and take effect without restart, except `shared_preload_libraries` which requires a PostgreSQL restart.

---

## Troubleshooting

### Unit tests crash with `symbol not found in flat namespace` on macOS 26+

macOS 26 (Tahoe) changed the dynamic linker (`dyld`) to eagerly resolve all
flat-namespace symbols at binary load time. pgrx extensions link PostgreSQL
server symbols (e.g. `CacheMemoryContext`, `SPI_connect`) with
`-Wl,-undefined,dynamic_lookup`, which previously resolved lazily. Since
`cargo test --lib` runs outside the postgres process, those symbols are
missing and the test binary aborts:

```
dyld[66617]: symbol not found in flat namespace '_CacheMemoryContext'
```

**Use `just test-unit`** — it automatically detects macOS 26+ and injects a
stub library (`libpg_stub.dylib`) via `DYLD_INSERT_LIBRARIES`. The stub
provides NULL/no-op definitions for the ~28 PostgreSQL symbols; they are never
called during unit tests (pure Rust logic only).

This does **not** affect integration tests, E2E tests, `just lint`,
`just build`, or the extension running inside PostgreSQL.

See the [Installation Guide](../INSTALL.md#unit-tests-crash-on-macos-26-symbol-not-found-in-flat-namespace) for details and manual usage.

### My stream table is stuck in INITIALIZING status

The initial full refresh may have failed. Check:
```sql
SELECT * FROM pgtrickle.get_refresh_history('my_table', 5);
```
If the error is transient, retry with:
```sql
SELECT pgtrickle.refresh_stream_table('my_table');
```

### My stream table shows stale data but the scheduler is running

Common causes:
1. **TRUNCATE on source table** — bypasses CDC triggers. Manual refresh needed.
2. **Too many errors** — check `consecutive_errors` in `pgtrickle.pg_stat_stream_tables`. Resume with `ALTER ... status => 'ACTIVE'`.
3. **Long-running refresh** — check for lock contention or slow defining queries.
4. **Scheduler disabled** — verify `SHOW pg_trickle.enabled;` returns `on`.

### I get "cycle detected" when creating a stream table

Stream tables cannot have circular dependencies. If ST-A depends on ST-B and ST-B depends on ST-A (directly or transitively), creation is rejected. Restructure your queries to break the cycle.

### A source table was altered and my stream table stopped refreshing

pg_trickle detects DDL changes via event triggers and marks affected stream tables with `needs_reinit = true`. The next scheduler cycle will reinitialize (full drop + recreate of storage) the stream table automatically. If the schema change breaks the defining query, the reinitialization will fail — check refresh history for the error and recreate the stream table with an updated query.

### How do I see the delta query generated for a stream table?

```sql
SELECT pgtrickle.explain_st('order_totals');
```

This shows the DVM operator tree, source tables, and the generated delta SQL.

### How do I interpret the refresh history?

The `pgtrickle.get_refresh_history()` function returns the most recent refresh records for a stream table:

```sql
SELECT * FROM pgtrickle.get_refresh_history('order_totals', 10);
```

Key columns:

| Column | Meaning |
|---|---|
| `action` | Refresh type: `FULL`, `DIFFERENTIAL`, `TOPK`, `IMMEDIATE`, or `REINITIALIZE` |
| `rows_inserted` | Rows added to the stream table in this cycle |
| `rows_deleted` | Rows removed from the stream table in this cycle |
| `rows_updated` | Rows modified in the stream table (for explicit DML path) |
| `duration_ms` | Wall-clock time for the refresh |
| `error_message` | NULL for success; error text for failures |
| `source_changes` | Number of pending change records processed |
| `fallback_reason` | If DIFFERENTIAL fell back to FULL: `change_ratio_exceeded`, `truncate_detected`, or `reinitialize` |

**Patterns to look for:**
- High `rows_inserted` + `rows_deleted` with low `source_changes` → possible duplicate rows (keyless source tables)
- `fallback_reason = 'change_ratio_exceeded'` frequently → consider lowering the threshold or switching to FULL mode
- Increasing `duration_ms` over time → index maintenance or buffer bloat; consider VACUUM or checking for missing indexes

### How can I tell if the scheduler is running?

Several ways to verify:

**1. Check the background worker:**
```sql
SELECT pid, datname, backend_type, state, query
FROM pg_stat_activity
WHERE backend_type = 'pg_trickle scheduler';
```

If no rows are returned, the scheduler is not running. Common causes: `pg_trickle.enabled = false`, extension not in `shared_preload_libraries`, or `max_worker_processes` exhausted.

**2. Check recent refresh activity:**
```sql
SELECT MAX(refreshed_at) AS last_refresh
FROM pgtrickle.pgt_stream_tables
WHERE status = 'ACTIVE';
```

If the last refresh was long ago relative to the shortest schedule, the scheduler may be stuck.

**3. Check PostgreSQL logs:**
The scheduler logs startup and shutdown messages at `LOG` level:
```
LOG:  pg_trickle scheduler started for database "mydb"
LOG:  pg_trickle scheduler shutting down (SIGTERM)
```

### How do I debug a stream table that shows stale data?

Follow this diagnostic checklist:

1. **Is the scheduler running?** (See above)
2. **Is the stream table active?**
   ```sql
   SELECT pgt_name, status, consecutive_errors FROM pgtrickle.pg_stat_stream_tables
   WHERE pgt_name = 'my_st';
   ```
   If status is `ERROR` or `SUSPENDED`, the stream table has been auto-suspended after repeated failures.
3. **Are there pending changes?**
   ```sql
   SELECT COUNT(*) FROM pgtrickle_changes.changes_<source_oid>;
   ```
   If zero, the source table may not have CDC triggers (check `SELECT tgname FROM pg_trigger WHERE tgrelid = '<source_oid>'`).
4. **Is the refresh failing silently?**
   ```sql
   SELECT * FROM pgtrickle.get_refresh_history('my_st', 5);
   ```
   Check for error messages.
5. **Is there lock contention?** Long-running transactions holding locks on the source or stream table can block refreshes. Check `pg_locks` and `pg_stat_activity`.

### What does the `needs_reinit` flag mean and how do I clear it?

The `needs_reinit` flag in `pgtrickle.pgt_stream_tables` indicates that the stream table's physical storage needs to be rebuilt — typically because a source table's schema changed.

When `needs_reinit = true`:
- The scheduler **skips** normal differential/full refresh.
- Instead, it performs a **reinitialize**: drop the storage table, recreate it from the current defining query schema, and populate with a full refresh.
- If reinitialization succeeds, `needs_reinit` is cleared automatically.

If reinitialization keeps failing (e.g., the defining query references a dropped column):
```sql
-- Fix the underlying issue first, then clear manually:
UPDATE pgtrickle.pgt_stream_tables SET needs_reinit = false WHERE pgt_name = 'my_st';
-- Or drop and recreate:
SELECT pgtrickle.drop_stream_table('my_st');
SELECT pgtrickle.create_stream_table('my_st', 'SELECT ...', '1m', 'DIFFERENTIAL');
```

---

## Why Are These SQL Features Not Supported?

This section gives detailed technical explanations for each SQL limitation. pg_trickle follows the principle of **"fail loudly rather than produce wrong data"** — every unsupported feature is detected at stream-table creation time and rejected with a clear error message and a suggested rewrite.

### How does `NATURAL JOIN` work?

`NATURAL JOIN` is **now fully supported**. At parse time, pg_trickle resolves the common columns between the two tables (using `OpTree::output_columns()`) and synthesizes explicit equi-join conditions. This supports `INNER`, `LEFT`, `RIGHT`, and `FULL` NATURAL JOIN variants.

Internally, `NATURAL JOIN` is converted to an explicit `JOIN ... ON` before the DVM engine builds its operator tree, so delta computation works identically to a manually specified equi-join.

**Note:** The internal `__pgt_row_id` column is excluded from common column resolution, so NATURAL JOINs between stream tables work correctly.

### How do `GROUPING SETS`, `CUBE`, and `ROLLUP` work?

`GROUPING SETS`, `CUBE`, and `ROLLUP` are **now fully supported** via an automatic parse-time rewrite. pg_trickle decomposes these constructs into a `UNION ALL` of separate `GROUP BY` queries before the DVM engine processes the query.

> **Explosion guard:** `CUBE(N)` generates $2^N$ branches. pg_trickle rejects
> CUBE/ROLLUP combinations that would produce more than **64 branches** to
> prevent runaway memory usage. Use explicit `GROUPING SETS(...)` instead.

For example:
```sql
-- This defining query:
SELECT dept, region, SUM(amount) FROM sales GROUP BY CUBE(dept, region)

-- Is automatically rewritten to:
SELECT dept, region, SUM(amount) FROM sales GROUP BY dept, region
UNION ALL
SELECT dept, NULL::text, SUM(amount) FROM sales GROUP BY dept
UNION ALL
SELECT NULL::text, region, SUM(amount) FROM sales GROUP BY region
UNION ALL
SELECT NULL::text, NULL::text, SUM(amount) FROM sales
```

`GROUPING()` function calls are replaced with integer literal constants corresponding to the grouping level. The rewrite is transparent — the DVM engine sees only standard `GROUP BY` + `UNION ALL` operators and can apply incremental delta computation to each branch independently.

### How does `DISTINCT ON (…)` work?

`DISTINCT ON` is **now fully supported** via an automatic parse-time rewrite. pg_trickle transparently transforms `DISTINCT ON` into a `ROW_NUMBER()` window function subquery:

```sql
-- This defining query:
SELECT DISTINCT ON (dept) dept, employee, salary
FROM employees ORDER BY dept, salary DESC

-- Is automatically rewritten to:
SELECT dept, employee, salary FROM (
    SELECT dept, employee, salary,
           ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn
    FROM employees
) sub WHERE rn = 1
```

The rewrite happens before DVM parsing, so the operator tree sees a standard window function query and can apply partition-based recomputation for incremental delta maintenance.

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

Stream tables materialize the complete result set and keep it synchronized with source data. Bare `LIMIT`/`OFFSET` (without a recognized pattern) would truncate the result:

1. **Undefined ordering.** `LIMIT` without `ORDER BY` returns an arbitrary subset.

2. **Delta instability.** When source rows change, the boundary between "in the LIMIT" and "out of the LIMIT" shifts. A single INSERT could evict one row and admit another, requiring the refresh to track the full ordered position of every row.

3. **Semantic mismatch.** Users who write `LIMIT 100` typically want to limit what they *read*, not what is *stored*.

**Exception — TopK pattern:** When the defining query has a top-level `ORDER BY … LIMIT N` (constant integer, no OFFSET), pg_trickle recognizes this as a "TopK" query and accepts it. The stream table stores only the top-N rows and is refreshed via a MERGE-based scoped-recomputation strategy. See the SQL Reference for details.

**Rewrite (when TopK doesn't apply):**
```sql
-- Instead of:
'SELECT * FROM orders ORDER BY created_at DESC LIMIT 100'

-- Omit LIMIT from the defining query, apply when reading:
SELECT * FROM orders_stream_table ORDER BY created_at DESC LIMIT 100
```

### Why are window functions in expressions rejected?

Window functions like `ROW_NUMBER() OVER (…)` are supported as **standalone columns** in stream tables. However, embedding a window function inside an expression — such as `CASE WHEN ROW_NUMBER() OVER (...) = 1 THEN ...` or `SUM(x) OVER (...) + 1` — is rejected.

This restriction exists because:

1. **Partition-based recomputation.** pg_trickle's differential mode handles window functions by recomputing entire partitions that were affected by changes. When a window function is buried inside an expression, the DVM engine cannot isolate the window computation from the surrounding expression, making it impossible to correctly identify which partitions to recompute.

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
FROM pgtrickle.employees_ranked
```

### Why is `FOR UPDATE` / `FOR SHARE` rejected?

`FOR UPDATE` and related locking clauses (`FOR SHARE`, `FOR NO KEY UPDATE`, `FOR KEY SHARE`) acquire row-level locks on selected rows. This is incompatible with stream tables because:

1. **Refresh semantics.** Stream table contents are managed by the refresh engine using bulk `MERGE` operations. Row-level locks taken during the defining query would conflict with the refresh engine's own locking strategy.

2. **No direct DML.** Since users cannot directly modify stream table rows, there is no use case for locking rows inside the defining query. The locks would be held for the duration of the refresh transaction and then released, serving no purpose.

### Why is `ALL (subquery)` not supported?

`ALL (subquery)` compares a value against every row returned by a subquery (e.g., `WHERE x > ALL (SELECT y FROM t)`). It is rejected because:

1. **Negation rewrite complexity.** `x > ALL (SELECT y FROM t)` is logically equivalent to `NOT EXISTS (SELECT 1 FROM t WHERE y >= x)`, which pg_trickle can handle via its anti-join operator. The rewrite is straightforward.

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

1. **Row ID integrity.** Every row has a `__pgt_row_id` (a 64-bit xxHash of the group-by key or all columns). The refresh engine uses this for delta `MERGE` — matching incoming deltas against existing rows. A manually inserted row with an incorrect or duplicate `__pgt_row_id` would cause the next differential refresh to produce wrong results (double-counting, missed deletes, or merge conflicts).

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

When a DIFFERENTIAL mode stream table has user-defined row-level triggers (or `pg_trickle.user_triggers = 'on'`), the refresh engine uses **explicit DML decomposition** instead of `MERGE`:

1. **Delta materialized once.** The delta query result is stored in a temporary table (`__pgt_delta_<id>`) to avoid evaluating it three times.

2. **DELETE removed rows.** Rows in the stream table whose `__pgt_row_id` is absent from the delta are deleted. `AFTER DELETE` triggers fire with correct `OLD` values.

3. **UPDATE changed rows.** Rows whose `__pgt_row_id` exists in both the stream table and delta but whose values differ (checked via `IS DISTINCT FROM`) are updated. `AFTER UPDATE` triggers fire with correct `OLD` and `NEW`. No-op updates (where values are identical) are skipped, preventing spurious triggers.

4. **INSERT new rows.** Rows in the delta whose `__pgt_row_id` is absent from the stream table are inserted. `AFTER INSERT` triggers fire with correct `NEW` values.

**FULL refresh behavior:** Row-level user triggers are automatically suppressed during FULL refresh via `DISABLE TRIGGER USER` / `ENABLE TRIGGER USER`. A `NOTIFY pgtrickle_refresh` is emitted so listeners know a FULL refresh occurred. Use `REFRESH MODE DIFFERENTIAL` for stream tables that need per-row trigger semantics.

**Performance:** The explicit DML path adds ~25–60% overhead compared to MERGE for triggered stream tables. Stream tables without user triggers have zero overhead (only a fast `pg_trigger` check, <0.1 ms).

**Control:** The `pg_trickle.user_triggers` GUC controls this behavior:
- `auto` (default): detect user triggers automatically
- `on`: always use explicit DML (useful for testing)
- `off`: always use MERGE, suppressing triggers

### Why can't I `ALTER TABLE` a stream table directly?

Stream table metadata (defining query, schedule, refresh mode) is stored in the pg_trickle catalog (`pgtrickle.pgt_stream_tables`). A direct `ALTER TABLE` would change the physical table without updating the catalog, causing:

1. **Column mismatch.** If you add or remove columns, the refresh engine's cached delta query and `MERGE` statement would reference columns that no longer exist (or miss new ones), causing runtime errors.

2. **`__pgt_row_id` invalidation.** The row ID hash is computed from the defining query's output columns. Altering the table schema without updating the defining query would make existing row IDs inconsistent with the new column set.

Use `pgtrickle.alter_stream_table()` to change schedule, refresh mode, or status. To change the defining query or column structure, drop and recreate the stream table.

### Why can't I `TRUNCATE` a stream table?

`TRUNCATE` removes all rows instantly but does not update the pg_trickle frontier or change buffers. After a `TRUNCATE`:

1. **Differential refresh sees no changes.** The frontier still records the last-processed LSN. No new source changes may have occurred, so the next differential refresh produces an empty delta — leaving the stream table empty even though the source still has data.

2. **No recovery path for differential mode.** The refresh engine has no way to detect that the stream table was externally truncated. It assumes the current contents match the frontier.

Use `pgtrickle.refresh_stream_table('my_table')` to force a full re-materialization, or drop and recreate the stream table if you need a clean slate.

### What are the memory limits for delta processing?

The differential refresh path executes the delta query as a **single SQL statement**. For large batches (e.g., a bulk UPDATE of 10M rows), PostgreSQL may attempt to materialize the entire delta result set in memory. If the delta exceeds `work_mem`, PostgreSQL will spill to temporary files on disk, which is slower but safe. In extreme cases, OOM (out of memory) can occur if `work_mem` is set very high and the delta is enormous.

**Mitigations:**

1. **Adaptive fallback.** The `pg_trickle.differential_max_change_ratio` GUC (default 0.15) automatically triggers a FULL refresh when the ratio of pending changes to total rows exceeds the threshold. This prevents large deltas from consuming excessive memory.

2. **`work_mem` tuning.** PostgreSQL's `work_mem` setting controls how much memory each sort/hash operation uses before spilling to disk. For pg_trickle workloads, 64–256 MB is typical. Monitor `temp_blks_written` in `pg_stat_statements` to detect spilling.

3. **`pg_trickle.merge_work_mem_mb` GUC.** Sets a session-level `work_mem` override during the MERGE execution (default: 0 = use global `work_mem`). This allows higher memory for refresh without affecting other queries.

4. **Monitoring.** If `pg_stat_statements` is installed, pg_trickle logs a warning when the MERGE query writes temporary blocks to disk.

### Why are refreshes processed sequentially?

The scheduler processes stream tables **sequentially** in topological (dependency) order within a single background worker process. Even though `pg_trickle.max_concurrent_refreshes` can be set up to 32, **parallel refresh of independent branches is not yet implemented**.

**Why sequential?**

- **Correctness.** Topological ordering guarantees that upstream stream tables are refreshed before downstream ones. Parallel execution of independent DAG branches requires careful lock management to avoid read/write conflicts.
- **Simplicity.** A single-worker model avoids connection pool exhaustion, advisory lock races, and prepared statement conflicts.

**Impact:**

- For most deployments with <100 stream tables, sequential processing adds negligible latency (each differential refresh typically takes 5–50ms).
- For large deployments with many independent stream tables, total cycle time = sum of all individual refresh times.

**Future:** Parallel refresh via multiple background workers is planned for a future release.

### How many connections does pg_trickle use?

pg_trickle uses the following PostgreSQL connections:

| Component | Connections | When |
|-----------|-------------|------|
| Background scheduler | 1 | Always (per database with STs) |
| WAL decoder polling | 0 (shared) | Uses the scheduler's SPI connection |
| Manual refresh | 1 | Per-call (uses caller's session) |

**Total**: 1 persistent connection per database. WAL decoder polling shares the scheduler's SPI connection rather than opening separate connections.

**`max_worker_processes`**: pg_trickle registers 1 background worker per database during `_PG_init()`. Ensure `max_worker_processes` (default 8) has room for the pg_trickle worker plus any other extensions.

**Advisory locks**: The scheduler holds a session-level advisory lock per actively-refreshing ST. These are released immediately after each refresh completes.
