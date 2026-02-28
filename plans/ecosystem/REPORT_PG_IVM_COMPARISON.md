# pg_trickle vs pg_ivm — Comparison Report

**Date:** 2026-02-28  
**Author:** Internal research  
**Status:** Reference document

---

## 1. Executive Summary

Both `pg_trickle` and `pg_ivm` implement Incremental View Maintenance (IVM) as
PostgreSQL extensions — the goal of keeping materialized query results up-to-date
without full recomputation. Despite the shared objective they differ fundamentally
in design philosophy, maintenance model, SQL coverage, operational model, and
target audience.

`pg_ivm` is a mature, widely-deployed C extension (1.4k GitHub stars, 17 releases)
focused on **immediate**, synchronous IVM that runs inside the same transaction as
the base-table write. `pg_trickle` is an early-stage Rust extension targeting
**deferred, scheduled** IVM with a richer SQL dialect, a dependency DAG, and
built-in operational tooling.

The two projects are **complementary rather than directly competing**: pg_ivm is
the right choice when you need sub-millisecond view consistency within a
transaction; pg_trickle is the right choice when you want a declarative,
independently-scheduled summary layer decoupled from write latency.

---

## 2. Project Overview

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Repository | [sraoss/pg_ivm](https://github.com/sraoss/pg_ivm) | [grove/pg-trickle](https://github.com/grove/pg-trickle) |
| Language | C | Rust (pgrx 0.17) |
| Latest release | 1.13 (2025-10-20) | 0.1.1 (2026-02-26) |
| Stars | ~1,400 | early stage |
| License | PostgreSQL License | Apache 2.0 |
| PG versions | 13 – 18 | 18 only |
| Schema | `pgivm` | `pgtrickle` / `pgtrickle_changes` |
| Shared library required | Yes (`shared_preload_libraries` or `session_preload_libraries`) | Yes (`shared_preload_libraries`, required for background worker) |
| Background worker | No | Yes (scheduler + optional WAL decoder) |

---

## 3. Maintenance Model

This is the most important design difference between the two extensions.

### pg_ivm — Immediate Maintenance

pg_ivm updates its views **synchronously inside the same transaction** that
modified the base table. When a row is inserted/updated/deleted, `AFTER` row
triggers fire and update the IMMV before the transaction commits.

```
BEGIN;
  UPDATE base_table ...;   -- triggers fire here
  -- IMMV is updated before COMMIT
COMMIT;
```

**Consequences:**

- The IMMV is always exactly consistent with the committed state of the base
  table — zero staleness.
- Write latency increases by the cost of view maintenance. For large joins or
  aggregates on popular tables this can be significant.
- Locking: `ExclusiveLock` is held on the IMMV during maintenance to prevent
  concurrent anomalies. In `REPEATABLE READ` or `SERIALIZABLE` isolation,
  errors are raised when conflicts are detected.
- `TRUNCATE` on a base table triggers full IMMV refresh (for most view types).
- Not compatible with logical replication (subscriber nodes are not updated).

### pg_trickle — Deferred, Scheduled Maintenance

pg_trickle updates its stream tables **asynchronously**, driven by a background
worker scheduler. Changes are captured by row-level triggers (or optionally by
WAL decoding) into change-buffer tables and are applied in batch on the next
refresh cycle.

```
-- Write path: only a trigger INSERT into change buffer
BEGIN;
  UPDATE base_table ...;   -- trigger captures delta into pgtrickle_changes.*
COMMIT;

-- Separate refresh cycle (background worker):
  apply_delta_to_stream_table(...)
```

**Consequences:**

- Write latency is minimized — the trigger write into the change buffer is
  ~2–50 μs regardless of view complexity.
- Stream tables are stale between refresh cycles. The staleness bound is
  configurable (e.g. `'30s'`, `'5m'`, `'@hourly'`, or cron expressions).
- Refresh can be triggered manually: `pgtrickle.refresh_stream_table(...)`.
- Multiple stream tables can share a refresh pipeline ordered by dependency
  (topological DAG scheduling).
- The WAL-based CDC mode (`pg_trickle.cdc_mode = 'wal'`) eliminates trigger
  overhead entirely when `wal_level = logical` is available.

---

## 4. SQL Feature Coverage

### pg_ivm — Supported Features

| Feature | pg_ivm |
|---|---|
| Simple SELECT / projection | ✅ |
| WHERE | ✅ |
| INNER JOIN | ✅ |
| OUTER JOIN (left/right/full) | ✅ (v1.13, added recently) |
| GROUP BY + COUNT, SUM, AVG, MIN, MAX | ✅ |
| DISTINCT | ✅ |
| Simple subqueries in FROM | ✅ |
| EXISTS subqueries in WHERE | ✅ |
| Simple CTEs (WITH) | ✅ |
| WINDOW functions | ❌ |
| HAVING | ❌ |
| ORDER BY / LIMIT / OFFSET | ❌ |
| UNION / INTERSECT / EXCEPT | ❌ |
| WITH RECURSIVE | ❌ |
| LATERAL | ❌ |
| User-defined aggregates | ❌ |
| JSON / array aggregates | ❌ |
| Partitioned tables as sources | ❌ |
| Views / materialized views as sources | ❌ |
| Volatile functions in query | ❌ |

**Notable restrictions:**
- Target-list columns must have a btree operator class — `json`, `xml`, `point`
  types cannot appear in the target list.
- Column names must not start with `__ivm_` (reserved for internal counters).
- GROUP BY expressions must appear in the target list.
- `pg_dump` / `pg_upgrade` require manual IMMV recreation.

### pg_trickle — Supported Features

| Feature | pg_trickle |
|---|---|
| Simple SELECT / projection | ✅ |
| WHERE | ✅ |
| HAVING | ✅ |
| INNER JOIN | ✅ |
| LEFT / RIGHT / FULL OUTER JOIN | ✅ |
| NATURAL JOIN | ✅ |
| GROUP BY + COUNT, SUM, AVG, MIN, MAX | ✅ |
| GROUP BY + STRING_AGG, ARRAY_AGG, BOOL_AND/OR | ✅ |
| GROUP BY + JSON_AGG, JSONB_AGG, STDDEV, VARIANCE, regression | ✅ |
| DISTINCT | ✅ (reference-counted multiplicity) |
| DISTINCT ON | ✅ (rewritten to ROW_NUMBER window) |
| UNION ALL / UNION | ✅ |
| INTERSECT / EXCEPT | ✅ |
| Subqueries in FROM | ✅ |
| EXISTS / NOT EXISTS | ✅ |
| IN / NOT IN (subquery) | ✅ |
| Scalar subqueries | ✅ |
| Non-recursive CTEs | ✅ |
| WITH RECURSIVE | ✅ (semi-naive + DRed in DIFFERENTIAL) |
| WINDOW functions + frames | ✅ |
| LATERAL / SRFs (unnest, jsonb_array_elements, …) | ✅ |
| JSON_TABLE (PG 17+) | ✅ |
| GROUPING SETS / CUBE / ROLLUP | ✅ |
| Views as sources | ✅ (auto-inlined) |
| Materialized views as sources | ❌ DIFF / ✅ FULL |
| Volatile functions | ❌ (rejected in DIFFERENTIAL) |
| ORDER BY | ⚠️ (accepted, silently ignored) |
| LIMIT / OFFSET | ❌ |
| Partitioned tables | ✅ |
| Tables without primary keys | ✅ (hash-based row identity) |

---

## 5. API Comparison

### pg_ivm API

```sql
-- Create an IMMV
SELECT pgivm.create_immv('myview', 'SELECT * FROM mytab');

-- Full refresh (emergency)
SELECT pgivm.refresh_immv('myview', true);   -- with data
SELECT pgivm.refresh_immv('myview', false);  -- disable maintenance

-- Inspect
SELECT immvrelid, pgivm.get_immv_def(immvrelid)
FROM pgivm.pg_ivm_immv;

-- Drop
DROP TABLE myview;

-- Rename
ALTER TABLE myview RENAME TO myview2;
```

pg_ivm IMMVs are standard PostgreSQL tables. They can be dropped with
`DROP TABLE` and renamed with `ALTER TABLE`.

### pg_trickle API

```sql
-- Create a stream table
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    '2m',           -- refresh schedule
    'DIFFERENTIAL'  -- or 'FULL'
);

-- Manual refresh
SELECT pgtrickle.refresh_stream_table('order_totals');

-- Alter schedule or mode
SELECT pgtrickle.alter_stream_table('order_totals', schedule => '5m');

-- Drop
SELECT pgtrickle.drop_stream_table('order_totals');

-- Status and monitoring
SELECT * FROM pgtrickle.pgt_status();
SELECT * FROM pgtrickle.pg_stat_stream_tables;
SELECT * FROM pgtrickle.pgt_stream_tables;

-- DAG inspection
SELECT * FROM pgtrickle.pgt_dependencies;
```

pg_trickle stream tables are regular PostgreSQL tables but managed through the
`pgtrickle` schema's API functions. They cannot be renamed with `ALTER TABLE`
(use `alter_stream_table`).

---

## 6. Scheduling and Dependency Management

| Capability | pg_ivm | pg_trickle |
|---|---|---|
| Automatic scheduling | ❌ (immediate only, no scheduler) | ✅ background worker |
| Manual refresh | ✅ `refresh_immv()` | ✅ `refresh_stream_table()` |
| Cron schedules | ❌ | ✅ (standard 5/6-field cron + aliases) |
| Duration-based staleness bounds | ❌ | ✅ (`'30s'`, `'5m'`, `'1h'`, …) |
| Dependency DAG | ❌ | ✅ (stream tables can reference other stream tables) |
| Topological refresh ordering | ❌ | ✅ (upstream refreshes before downstream) |
| CALCULATED schedule propagation | ❌ | ✅ (consumers drive upstream schedules) |

pg_trickle's DAG scheduling is a significant differentiator: you can build
multi-layer pipelines where each downstream stream table is automatically
refreshed after its upstream dependencies, with the refresh schedule derived
from the leaf-level freshness requirement rather than manually coordinated.

---

## 7. Change Data Capture

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Mechanism | AFTER row triggers (inline, same txn) | AFTER row triggers → change buffer |
| WAL-based CDC | ❌ | ✅ optional (`pg_trickle.cdc_mode = 'wal'`) |
| Logical replication slots | Not used | Used in WAL mode only |
| Write-side overhead | Higher (view maintenance in txn) | Lower (small trigger insert only) |
| Change buffer tables | None (applied immediately) | `pgtrickle_changes.changes_<oid>` |
| TRUNCATE handling | IMMV truncated/refreshed synchronously | Change buffer cleared; full refresh queued |

---

## 8. Concurrency and Isolation

### pg_ivm
- Holds `ExclusiveLock` on the IMMV during incremental update.
- In `READ COMMITTED`: serializes concurrent updates to the same IMMV.
- In `REPEATABLE READ` / `SERIALIZABLE`: raises an error when a concurrent
  transaction has already updated the IMMV.
- Single-table INSERT-only IMMVs use the lighter `RowExclusiveLock`.

### pg_trickle
- Refresh operations acquire an advisory lock per stream table so only one
  refresh can run at a time.
- Base table writes are never blocked by refresh operations.
- `pg_trickle.max_concurrent_refreshes` controls parallelism across the DAG.
- Crash recovery: in-flight refreshes are marked failed on restart; the
  scheduler retries on the next cycle.

---

## 9. Observability

| Feature | pg_ivm | pg_trickle |
|---|---|---|
| Catalog of managed views | `pgivm.pg_ivm_immv` | `pgtrickle.pgt_stream_tables` |
| Per-refresh timing/history | ❌ | ✅ `pgtrickle.pgt_refresh_history` |
| Staleness reporting | ❌ | ✅ `stale` column in monitoring views |
| Scheduler status | ❌ | ✅ `pgtrickle.pgt_status()` |
| NOTIFY-based alerting | ❌ | ✅ `pgtrickle_refresh` channel |
| Error tracking | ❌ | ✅ consecutive error counter, last error message |
| dbt integration | ❌ | ✅ `dbt-pgtrickle` macro package |

---

## 10. Installation and Deployment

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Pre-built packages | RPM via yum.postgresql.org | OCI image, tarball |
| CNPG / Kubernetes | ❌ (no OCI image) | ✅ OCI extension image |
| Docker local dev | Manual | ✅ documented |
| `shared_preload_libraries` | Required (or `session_preload_libraries`) | Required |
| Extension upgrade scripts | ✅ (1.0 → 1.1 → … → 1.13) | ⚠️ Planned (not yet implemented) |
| `pg_dump` / restore | Manual IMMV recreation required | Standard pg_dump supported |

---

## 11. Known Limitations

### pg_ivm Limitations
- Adds latency to every write on tracked base tables.
- Cannot track tables modified via logical replication (subscriber nodes are
  not updated).
- `pg_dump` / `pg_upgrade` require manual recreation of all IMMVs.
- Limited aggregate support (no user-defined aggregates, no window functions).
- Column type restrictions (btree operator class required in target list).
- No scheduler or background worker — refresh is immediate only.
- On high-churn tables, `min`/`max` aggregates can trigger expensive rescans.

### pg_trickle Limitations
- Data is stale between refresh cycles — not suitable for applications
  requiring sub-second consistency.
- `LIMIT` / `OFFSET` not supported in DIFFERENTIAL mode.
- Volatile SQL functions rejected in DIFFERENTIAL mode.
- Materialized views as sources not supported in DIFFERENTIAL mode.
- `ALTER EXTENSION pg_trickle UPDATE` migration scripts not yet implemented
  (planned for v0.2.0+).
- Targets PostgreSQL 18 only; no backport to PG 13–17.
- Early release — not yet production-hardened.

---

## 12. Performance Characteristics

### pg_ivm
- **Write path:** slower — every DML statement triggers inline view maintenance.
  From the README example: a single row update on a 10M-row join IMMV takes
  ~15 ms vs ~9 ms for a plain table update.
- **Read path:** instant — IMMV is always current, no refresh needed on read.
- **Refresh (full):** comparable to `REFRESH MATERIALIZED VIEW` (~20 seconds
  for a 10M-row join in the example).

### pg_trickle
- **Write path:** minimal overhead — only a small trigger INSERT into the
  change buffer (~2–50 μs per row). In WAL mode, zero trigger overhead.
- **Read path:** instant from the materialized table (potentially stale).
- **Refresh (differential):** proportional to the number of changed rows, not
  the total table size. A single-row change on a million-row aggregate touches
  one row's worth of computation.
- **Refresh (full):** re-runs the entire query; comparable to
  `REFRESH MATERIALIZED VIEW`.

---

## 13. Use-Case Fit

| Scenario | Recommended |
|---|---|
| Need views consistent within the same transaction | **pg_ivm** |
| Application cannot tolerate any view staleness | **pg_ivm** |
| High write throughput, views can be slightly stale | **pg_trickle** |
| Multi-layer summary pipelines with dependencies | **pg_trickle** |
| Time-based or cron-driven refresh schedules | **pg_trickle** |
| Views with complex SQL (window functions, CTEs, UNION) | **pg_trickle** |
| Simple aggregation with zero-staleness requirement | **pg_ivm** |
| Kubernetes / CloudNativePG deployment | **pg_trickle** |
| dbt integration | **pg_trickle** |
| PostgreSQL 13–17 | **pg_ivm** |
| PostgreSQL 18 | Either (pg_trickle preferred for new projects) |
| Production-hardened, stable API | **pg_ivm** |
| Early adopter, rich SQL coverage needed | **pg_trickle** |

---

## 14. Coexistence

The two extensions can be installed in the same database simultaneously — they
use different schemas (`pgivm` vs `pgtrickle`/`pgtrickle_changes`) and do not
interfere with each other. A plausible combined deployment:

- Use **pg_ivm** for small, critical lookup tables that must be perfectly
  consistent within transactions (e.g. permission caches, balance totals).
- Use **pg_trickle** for large analytical summary tables, multi-layer
  aggregation pipelines, or views with complex SQL that pg_ivm cannot handle.

---

## 15. Summary Table

| Dimension | pg_ivm | pg_trickle |
|---|---|---|
| Maintenance timing | **Immediate** (same transaction) | **Deferred** (scheduled) |
| Write latency impact | Higher | Minimal |
| View staleness | Zero | Configurable (seconds to hours) |
| SQL coverage | Moderate | Broad |
| Aggregate support | count/sum/avg/min/max only | All built-in aggregates |
| WINDOW functions | ❌ | ✅ |
| WITH RECURSIVE | ❌ | ✅ |
| OUTER JOINs | ✅ (v1.13) | ✅ |
| Multi-view DAG | ❌ | ✅ |
| Scheduler / background worker | ❌ | ✅ |
| WAL-based CDC | ❌ | ✅ (optional) |
| Observability | Minimal | Rich |
| dbt integration | ❌ | ✅ |
| Extension upgrade path | ✅ (1.0–1.13) | ⚠️ Planned |
| PostgreSQL versions | 13–18 | 18 only |
| Maturity | Production-ready | Early release |
| Language | C | Rust |
| License | PostgreSQL License | Apache 2.0 |

---

## References

- pg_ivm repository: https://github.com/sraoss/pg_ivm
- pg_trickle repository: https://github.com/grove/pg-trickle
- DBSP differential dataflow paper: https://arxiv.org/abs/2203.16684
- pg_trickle ESSENCE.md: [../../ESSENCE.md](../../ESSENCE.md)
- pg_trickle DVM operators: [../../docs/DVM_OPERATORS.md](../../docs/DVM_OPERATORS.md)
- pg_trickle architecture: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
