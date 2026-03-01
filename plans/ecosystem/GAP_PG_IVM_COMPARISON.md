# pg_trickle vs pg_ivm — Comparison Report & Gap Analysis

**Date:** 2026-02-28 (merged 2026-03-01)
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

pg_trickle is **significantly ahead** of pg_ivm in SQL coverage, operator support,
aggregate support, and operational features. pg_ivm's two structural advantages —
**immediate (synchronous) maintenance** and **broader PostgreSQL version support
(PG 13–18)** — are both addressed by existing plans:

- [PLAN_TRANSACTIONAL_IVM.md](../sql/PLAN_TRANSACTIONAL_IVM.md) proposes an
  `IMMEDIATE` refresh mode with statement-level AFTER triggers, transition
  tables, and a full **pg_ivm compatibility layer** (`pgivm.create_immv()`,
  `pgivm.refresh_immv()`, `pgivm.pg_ivm_immv` catalog view).
- [PLAN_PG_BACKCOMPAT.md](../infra/PLAN_PG_BACKCOMPAT.md) details backporting
  pg_trickle to **PG 14–18** (recommended) or **PG 16–18** (minimum viable),
  requiring ~2.5–3 weeks of effort primarily in `#[cfg]`-gating ~435 lines
  of JSON/SQL-standard parse-tree handling.

Once these plans are implemented, **every pg_ivm advantage will be neutralized
or surpassed**, while pg_trickle retains its 24+ unique features.

---

## 2. Project Overview

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Repository | [sraoss/pg_ivm](https://github.com/sraoss/pg_ivm) | [grove/pg-trickle](https://github.com/grove/pg-trickle) |
| Language | C | Rust (pgrx 0.17) |
| Latest release | 1.13 (2025-10-20) | 0.1.2 (2026-02-28) |
| Stars | ~1,400 | early stage |
| License | PostgreSQL License | Apache 2.0 |
| PG versions | 13 – 18 | 18 only; **PG 14–18 planned** |
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

### Planned: pg_trickle IMMEDIATE Mode

[PLAN_TRANSACTIONAL_IVM.md](../sql/PLAN_TRANSACTIONAL_IVM.md) defines an
`IMMEDIATE` refresh mode that uses statement-level AFTER triggers with
transition tables (the same mechanism pg_ivm uses). Key design decisions:

- **Reuses the DVM engine** — the Scan operator reads from Ephemeral Named
  Relations (transition tables) instead of change buffer tables.
- **Phase 1** covers pg_ivm's full query subset plus pg_trickle's auto-rewrites.
- **Phase 2** adds a `pgivm.*` compatibility layer for drop-in migration.
- **Phase 3** extends IMMEDIATE mode to window functions, set operations,
  LATERAL, recursive CTEs, and cascading IMMEDIATE stream tables.

---

## 4. SQL Feature Coverage — Summary

| Dimension | pg_ivm | pg_trickle | Winner |
|-----------|--------|-----------|--------|
| **Maintenance timing** | Immediate (in-transaction triggers) | Deferred (scheduler/manual); **IMMEDIATE mode planned** | pg_ivm (today); **planned parity** |
| **PostgreSQL versions** | 13–18 | 18 only; **PG 14–18 planned** | pg_ivm (today); **planned parity** |
| **Aggregate functions** | 5 (COUNT, SUM, AVG, MIN, MAX) | 39+ (all built-in aggregates) | **pg_trickle** |
| **FILTER clause on aggregates** | No | Yes | **pg_trickle** |
| **HAVING clause** | No | Yes | **pg_trickle** |
| **Inner joins** | Yes (including self-join) | Yes (including self-join, NATURAL, nested) | **pg_trickle** |
| **Outer joins** | Yes (limited — equijoin, single condition, many restrictions) | Yes (LEFT/RIGHT/FULL, nested, complex conditions) | **pg_trickle** |
| **DISTINCT** | Yes (reference-counted) | Yes (reference-counted) | Tie |
| **DISTINCT ON** | No | Yes (auto-rewritten to ROW_NUMBER) | **pg_trickle** |
| **UNION / INTERSECT / EXCEPT** | No | Yes (all 6 variants, bag + set) | **pg_trickle** |
| **Window functions** | No | Yes (partition recomputation) | **pg_trickle** |
| **CTEs (non-recursive)** | Simple only (no aggregates, no DISTINCT inside) | Full (aggregates, DISTINCT, multi-reference shared delta) | **pg_trickle** |
| **CTEs (recursive)** | No | Yes (semi-naive, DRed, recomputation) | **pg_trickle** |
| **Subqueries in FROM** | Simple only (no aggregates/DISTINCT inside) | Full support | **pg_trickle** |
| **EXISTS subqueries** | Yes (WHERE only, AND only, no agg/DISTINCT) | Yes (WHERE + targetlist, AND/OR, agg/DISTINCT inside) | **pg_trickle** |
| **NOT EXISTS / NOT IN** | No | Yes (anti-join operator) | **pg_trickle** |
| **IN (subquery)** | No | Yes (semi-join operator) | **pg_trickle** |
| **Scalar subquery in SELECT** | No | Yes (scalar subquery operator) | **pg_trickle** |
| **LATERAL subqueries** | No | Yes (row-scoped recomputation) | **pg_trickle** |
| **LATERAL SRFs** | No | Yes (jsonb_array_elements, unnest, etc.) | **pg_trickle** |
| **JSON_TABLE (PG 17+)** | No | Yes | **pg_trickle** |
| **GROUPING SETS / CUBE / ROLLUP** | No | Yes (auto-rewritten to UNION ALL) | **pg_trickle** |
| **Views as sources** | No (simple tables only) | Yes (auto-inlined, nested) | **pg_trickle** |
| **Partitioned tables** | No | Yes | **pg_trickle** |
| **Foreign tables** | No | FULL mode only | **pg_trickle** |
| **Cascading (view-on-view)** | No | Yes (DAG-aware scheduling) | **pg_trickle** |
| **Background scheduling** | No (user must trigger) | Yes (cron + duration, background worker) | **pg_trickle** |
| **Monitoring / observability** | 1 catalog table | Extensive (stats, history, staleness, CDC health, NOTIFY) | **pg_trickle** |
| **CDC mechanism** | Triggers only | Hybrid (triggers + optional WAL) | **pg_trickle** |
| **DDL tracking** | No automatic handling | Yes (event triggers, auto-reinit) | **pg_trickle** |
| **TRUNCATE handling** | Yes (auto-truncate IMMV) | Via full refresh | pg_ivm |
| **Auto-indexing** | Yes (on GROUP BY / DISTINCT / PK columns) | No (user creates indexes) | pg_ivm |
| **Row Level Security** | Yes (with limitations) | Not documented | pg_ivm |
| **Concurrency model** | ExclusiveLock on IMMV during maintenance | Advisory locks, non-blocking reads | **pg_trickle** |
| **Data type restrictions** | Must have btree opclass (no json, xml, point) | No documented type restrictions | **pg_trickle** |
| **Maturity / ecosystem** | 4 years, 1.4k stars, PGXN, yum packages | Pre-release (0.1.2), dbt integration | pg_ivm |

---

## 5. Detailed SQL Comparison

### 5.1 Aggregate Functions

| Function | pg_ivm | pg_trickle |
|----------|--------|-----------|
| COUNT(*) / COUNT(expr) | ✅ Algebraic | ✅ Algebraic |
| SUM | ✅ Algebraic | ✅ Algebraic |
| AVG | ✅ Algebraic (via SUM/COUNT) | ✅ Algebraic (via SUM/COUNT) |
| MIN | ✅ Semi-algebraic (rescan on extremum delete) | ✅ Semi-algebraic (rescan on extremum delete) |
| MAX | ✅ Semi-algebraic (rescan on extremum delete) | ✅ Semi-algebraic (rescan on extremum delete) |
| BOOL_AND / BOOL_OR | ❌ | ✅ Group-rescan |
| STRING_AGG | ❌ | ✅ Group-rescan |
| ARRAY_AGG | ❌ | ✅ Group-rescan |
| JSON_AGG / JSONB_AGG | ❌ | ✅ Group-rescan |
| BIT_AND / BIT_OR / BIT_XOR | ❌ | ✅ Group-rescan |
| JSON_OBJECT_AGG / JSONB_OBJECT_AGG | ❌ | ✅ Group-rescan |
| STDDEV / VARIANCE (all variants) | ❌ | ✅ Group-rescan |
| MODE / PERCENTILE_CONT / PERCENTILE_DISC | ❌ | ✅ Group-rescan |
| CORR / COVAR / REGR_* (11 functions) | ❌ | ✅ Group-rescan |
| ANY_VALUE (PG 16+) | ❌ | ✅ Group-rescan |
| JSON_ARRAYAGG / JSON_OBJECTAGG (PG 16+) | ❌ | ✅ Group-rescan |
| FILTER (WHERE) clause | ❌ | ✅ |
| WITHIN GROUP (ORDER BY) | ❌ | ✅ |
| **Total** | **5** | **39+** |

**Gap for pg_ivm:** Massive. Only 5 of ~40 built-in aggregate functions are supported.

### 5.2 Joins

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| Inner join | ✅ | ✅ |
| Self-join | ✅ | ✅ |
| LEFT JOIN | ✅ (restricted) | ✅ (full) |
| RIGHT JOIN | ✅ (restricted) | ✅ (normalized to LEFT) |
| FULL OUTER JOIN | ✅ (restricted) | ✅ (8-part delta) |
| NATURAL JOIN | ? | ✅ |
| Cross join | ? | ✅ |
| Nested joins (3+ tables) | ✅ | ✅ |
| Non-equi joins (theta) | ? | ✅ |
| Outer join + aggregates | ❌ | ✅ |
| Outer join + subqueries | ❌ | ✅ |
| Outer join + CASE/non-strict | ❌ | ✅ |
| Outer join multi-condition | ❌ (single equality only) | ✅ |

**Gap for pg_ivm:** Outer joins are heavily restricted — single equijoin condition, no aggregates, no subqueries, no CASE expressions, no IS NULL in WHERE.

### 5.3 Subqueries

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| Simple subquery in FROM | ✅ (no aggregates/DISTINCT inside) | ✅ (full support) |
| EXISTS in WHERE | ✅ (AND only, no agg/DISTINCT inside) | ✅ (AND + OR, full SQL inside) |
| NOT EXISTS in WHERE | ❌ | ✅ (anti-join operator) |
| IN (subquery) | ❌ | ✅ (rewritten to semi-join) |
| NOT IN (subquery) | ❌ | ✅ (rewritten to anti-join) |
| ALL (subquery) | ❌ | ✅ (rewritten to anti-join) |
| Scalar subquery in SELECT | ❌ | ✅ (scalar subquery operator) |
| Scalar subquery in WHERE | ❌ | ✅ (auto-rewritten to CROSS JOIN) |
| LATERAL subquery in FROM | ❌ | ✅ (row-scoped recomputation) |
| LATERAL SRF in FROM | ❌ | ✅ (jsonb_array_elements, unnest, etc.) |
| Subqueries in OR | ❌ | ✅ (auto-rewritten to UNION) |

**Gap for pg_ivm:** Severely limited subquery support. No anti-joins, no scalar subqueries, no LATERAL, no SRFs.

### 5.4 CTEs

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| Simple non-recursive CTE | ✅ (no aggregates/DISTINCT inside) | ✅ (full SQL inside) |
| Multi-reference CTE | ? | ✅ (shared delta optimization) |
| Chained CTEs | ? | ✅ |
| WITH RECURSIVE | ❌ | ✅ (semi-naive, DRed, recomputation) |

**Gap for pg_ivm:** No recursive CTEs, no aggregates/DISTINCT inside CTEs.

### 5.5 Set Operations

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| UNION ALL | ❌ | ✅ |
| UNION (set) | ❌ | ✅ (via DISTINCT + UNION ALL) |
| INTERSECT | ❌ | ✅ (dual-count multiplicity) |
| INTERSECT ALL | ❌ | ✅ |
| EXCEPT | ❌ | ✅ (dual-count multiplicity) |
| EXCEPT ALL | ❌ | ✅ |

**Gap for pg_ivm:** No set operations at all.

### 5.6 Window Functions

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| ROW_NUMBER, RANK, DENSE_RANK | ❌ | ✅ |
| SUM/AVG/COUNT OVER () | ❌ | ✅ |
| Frame clauses (ROWS/RANGE/GROUPS) | ❌ | ✅ |
| Named WINDOW clauses | ❌ | ✅ |
| PARTITION BY recomputation | ❌ | ✅ |

**Gap for pg_ivm:** Window functions are completely unsupported.

### 5.7 DISTINCT & Grouping

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| SELECT DISTINCT | ✅ | ✅ |
| DISTINCT ON (expr, ...) | ❌ | ✅ (auto-rewritten to ROW_NUMBER) |
| GROUP BY | ✅ | ✅ |
| GROUPING SETS | ❌ | ✅ (auto-rewritten to UNION ALL) |
| CUBE | ❌ | ✅ (auto-rewritten via GROUPING SETS) |
| ROLLUP | ❌ | ✅ (auto-rewritten via GROUPING SETS) |
| GROUPING() function | ❌ | ✅ |
| HAVING | ❌ | ✅ |

### 5.8 Source Table Types

| Source type | pg_ivm | pg_trickle |
|-------------|--------|-----------|
| Simple heap tables | ✅ | ✅ |
| Views | ❌ | ✅ (auto-inlined) |
| Materialized views | ❌ | FULL mode only |
| Partitioned tables | ❌ | ✅ |
| Partitions | ❌ | ✅ (via parent) |
| Foreign tables | ❌ | FULL mode only |
| Other IMMVs / stream tables | ❌ | ✅ (DAG cascading) |

**Gap for pg_ivm:** Only simple heap tables. No views, no partitioned tables, no cascading.

---

## 6. API Comparison

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

## 7. Scheduling and Dependency Management

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
refreshed after its upstream dependencies.

---

## 8. Change Data Capture

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Mechanism | AFTER row triggers (inline, same txn) | AFTER row triggers → change buffer |
| WAL-based CDC | ❌ | ✅ optional (`pg_trickle.cdc_mode = 'wal'`) |
| Logical replication slots | Not used | Used in WAL mode only |
| Write-side overhead | Higher (view maintenance in txn) | Lower (small trigger insert only) |
| Change buffer tables | None (applied immediately) | `pgtrickle_changes.changes_<oid>` |
| TRUNCATE handling | IMMV truncated/refreshed synchronously | Change buffer cleared; full refresh queued |

---

## 9. Concurrency and Isolation

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

## 10. Observability

| Feature | pg_ivm | pg_trickle |
|---|---|---|
| Catalog of managed views | `pgivm.pg_ivm_immv` | `pgtrickle.pgt_stream_tables` |
| Per-refresh timing/history | ❌ | ✅ `pgtrickle.pgt_refresh_history` |
| Staleness reporting | ❌ | ✅ `stale` column in monitoring views |
| Scheduler status | ❌ | ✅ `pgtrickle.pgt_status()` |
| NOTIFY-based alerting | ❌ | ✅ `pgtrickle_refresh` channel |
| Error tracking | ❌ | ✅ consecutive error counter, last error message |
| dbt integration | ❌ | ✅ `dbt-pgtrickle` macro package |
| Explain/introspection | ❌ | ✅ `explain_st` |

---

## 11. Installation and Deployment

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Pre-built packages | RPM via yum.postgresql.org | OCI image, tarball |
| CNPG / Kubernetes | ❌ (no OCI image) | ✅ OCI extension image |
| Docker local dev | Manual | ✅ documented |
| `shared_preload_libraries` | Required (or `session_preload_libraries`) | Required |
| Extension upgrade scripts | ✅ (1.0 → 1.1 → … → 1.13) | ⚠️ Planned (not yet implemented) |
| `pg_dump` / restore | Manual IMMV recreation required | Standard pg_dump supported |

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

## 13. Known Limitations

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
- Targets PostgreSQL 18 only; no backport to PG 13–17 (planned for PG 14–18).
- Early release — not yet production-hardened.

---

## 14. PostgreSQL Version Support

| | pg_ivm | pg_trickle (current) | pg_trickle (planned) |
|-|--------|---------------------|---------------------|
| PG 13 | ✅ | ❌ | ❌ (EOL Nov 2025) |
| PG 14 | ✅ | ❌ | ✅ (full plan) |
| PG 15 | ✅ | ❌ | ✅ (full plan) |
| PG 16 | ✅ | ❌ | ✅ (MVP target) |
| PG 17 | ✅ | ❌ | ✅ (MVP target) |
| PG 18 | ✅ | ✅ | ✅ |

**Planned resolution:** [PLAN_PG_BACKCOMPAT.md](../infra/PLAN_PG_BACKCOMPAT.md):

- **Minimum viable (PG 16–18):** ~1.5 weeks effort.
- **Full target (PG 14–18):** ~2.5–3 weeks effort.
- pgrx 0.17.0 already supports PG 14–18 via feature flags.
- ~435 lines in `src/dvm/parser.rs` need `#[cfg]` gating (all in
  JSON/SQL-standard sections). The remaining ~13,500 lines compile unchanged.

**Feature degradation matrix:**

| Feature | PG 14 | PG 15 | PG 16 | PG 17 | PG 18 |
|---------|:-----:|:-----:|:-----:|:-----:|:-----:|
| Core streaming tables | ✅ | ✅ | ✅ | ✅ | ✅ |
| Trigger-based CDC | ✅ | ✅ | ✅ | ✅ | ✅ |
| Differential refresh | ✅ | ✅ | ✅ | ✅ | ✅ |
| SQL/JSON constructors | — | — | ✅ | ✅ | ✅ |
| JSON_TABLE | — | — | — | ✅ | ✅ |
| WAL-based CDC | Needs test | Needs test | Likely | Likely | ✅ |

---

## 15. Features Unique to Each System

### Features Unique to pg_trickle (24 items, no pg_ivm equivalent)

1. **39+ aggregate functions** (vs 5)
2. **FILTER / HAVING / WITHIN GROUP** on aggregates
3. **Window functions** (partition recomputation)
4. **Set operations** (UNION ALL, UNION, INTERSECT, EXCEPT — all 6 variants)
5. **Recursive CTEs** (semi-naive, DRed, recomputation)
6. **LATERAL subqueries and SRFs** (jsonb_array_elements, unnest, JSON_TABLE)
7. **Anti-join / semi-join operators** (NOT EXISTS, NOT IN, IN, EXISTS with full SQL)
8. **Scalar subqueries** in SELECT list
9. **Views as sources** (auto-inlined with nested expansion)
10. **Partitioned table support**
11. **Cascading stream tables** (ST referencing other STs via DAG)
12. **Background scheduler** (cron + duration + canonical periods)
13. **GROUPING SETS / CUBE / ROLLUP** (auto-rewritten)
14. **DISTINCT ON** (auto-rewritten to ROW_NUMBER)
15. **Hybrid CDC** (trigger → WAL transition)
16. **DDL change detection** and automatic reinitialization
17. **Monitoring suite** (refresh stats, staleness tracking, CDC health, NOTIFY)
18. **Auto-rewrite pipeline** (6 transparent SQL rewrites)
19. **Volatile function detection**
20. **Adaptive FULL fallback** (change ratio threshold)
21. **dbt macro package**
22. **CNPG / Kubernetes deployment**
23. **SQL/JSON constructors** (JSON_OBJECT, JSON_ARRAY, etc.)
24. **JSON_TABLE** support (PG 17+)

### Features Unique to pg_ivm (with planned resolutions)

| # | Feature | Planned Resolution | Ref |
|---|---------|-------------------|-----|
| 1 | **Immediate (synchronous) maintenance** | `IMMEDIATE` refresh mode + pg_ivm compat layer | [PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md) |
| 2 | **Auto-index creation** on GROUP BY / DISTINCT / PK | Phase 2 of transactional IVM plan | [PLAN_TRANSACTIONAL_IVM §5.2](../sql/PLAN_TRANSACTIONAL_IVM.md) |
| 3 | **TRUNCATE propagation** (auto-truncate IMMV) | Handled by IMMEDIATE mode TRUNCATE triggers | [PLAN_TRANSACTIONAL_IVM §3.2](../sql/PLAN_TRANSACTIONAL_IVM.md) |
| 4 | **Row Level Security** respect | Document and test | — |
| 5 | **PostgreSQL 13–17 support** | PG 14–18 backcompat (~2.5–3 weeks) | [PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md) |
| 6 | **session_preload_libraries** | Not applicable (background worker needs shared_preload) | — |
| 7 | **Rename via ALTER TABLE** | Event trigger support (low effort) | — |
| 8 | **Drop via DROP TABLE** | Phase 2 of transactional IVM plan | [PLAN_TRANSACTIONAL_IVM §4.3](../sql/PLAN_TRANSACTIONAL_IVM.md) |

Of the 8 items, **5 have concrete implementation plans**. Only RLS testing,
`session_preload_libraries` (not applicable), and `ALTER TABLE RENAME` remain
unaddressed.

---

## 16. Use-Case Fit

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

## 17. Coexistence

The two extensions can be installed in the same database simultaneously — they
use different schemas (`pgivm` vs `pgtrickle`/`pgtrickle_changes`) and do not
interfere with each other. A plausible combined deployment:

- Use **pg_ivm** for small, critical lookup tables that must be perfectly
  consistent within transactions (e.g. permission caches, balance totals).
- Use **pg_trickle** for large analytical summary tables, multi-layer
  aggregation pipelines, or views with complex SQL that pg_ivm cannot handle.

---

## 18. Recommendations

### Planned work that closes pg_ivm gaps

| Priority | Item | Plan | Effort | Closes Gaps |
|----------|------|------|--------|-------------|
| **High** | IMMEDIATE refresh mode | [PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md) Phase 1 | Medium | #1 (immediate maintenance), #3 (TRUNCATE) |
| **High** | pg_ivm compatibility layer | [PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md) Phase 2 | Medium | #2 (auto-indexing), #7 (rename), #8 (DROP TABLE) |
| **High** | PG 16–18 backcompat (MVP) | [PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md) §11 | ~1.5 weeks | #5 (PG version support) |
| **Medium** | PG 14–18 backcompat (full) | [PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md) §5 | ~2.5–3 weeks | #5 (PG version support) |
| **Medium** | Extended IMMEDIATE SQL | [PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md) Phase 3 | Medium | Extends #1 beyond pg_ivm parity |

### Remaining small gaps (no existing plan)

| Priority | Item | Description | Effort |
|----------|------|-------------|--------|
| Low | RLS documentation | Document and test Row Level Security interaction | 2–3h |
| Low | ALTER TABLE RENAME | Detect rename via event trigger, update catalog | 2–4h |

### Not worth pursuing

| Item | Reason |
|------|--------|
| PG 13 support | EOL since November 2025. Incompatible `raw_parser()` API. |
| session_preload_libraries | Requires background worker, which needs shared_preload_libraries. |

---

## 19. Conclusion

pg_trickle covers **all** of pg_ivm's SQL surface and extends it dramatically
with 34+ additional aggregate functions, window functions, set operations,
recursive CTEs, LATERAL support, anti/semi-joins, and a comprehensive
operational layer.

The two remaining structural gaps — **immediate maintenance** and **PG version
support** — both have detailed implementation plans:

1. **[PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md)** adds an
   `IMMEDIATE` refresh mode that reuses the DVM engine with transition-table-
   based delta sources, plus a `pgivm.*` compatibility layer for drop-in
   pg_ivm migration.

2. **[PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md)** details backporting
   to PG 14–18 (or PG 16–18 as MVP) in ~2.5–3 weeks, primarily by `#[cfg]`-
   gating ~435 lines of JSON/SQL-standard parse-tree code.

Once both plans are implemented, **pg_trickle will be a strict superset of
pg_ivm** in every dimension: same immediate maintenance model, broader PG
version support (14–18 vs 13–18, with PG 13 EOL), dramatically wider SQL
coverage, and a complete operational layer that pg_ivm entirely lacks.

For users migrating from pg_ivm, the compatibility layer (`pgivm.create_immv`,
`pgivm.refresh_immv`, `pgivm.pg_ivm_immv`) enables **zero-change migration**.

---

## References

- pg_ivm repository: https://github.com/sraoss/pg_ivm
- pg_trickle repository: https://github.com/grove/pg-trickle
- DBSP differential dataflow paper: https://arxiv.org/abs/2203.16684
- pg_trickle ESSENCE.md: [../../ESSENCE.md](../../ESSENCE.md)
- pg_trickle DVM operators: [../../docs/DVM_OPERATORS.md](../../docs/DVM_OPERATORS.md)
- pg_trickle architecture: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
