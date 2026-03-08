# pg_trickle vs pg_ivm — Comparison Report & Gap Analysis

**Date:** 2026-02-28 (merged 2026-03-01, updated 2026-03-08)
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
the base-table write. `pg_trickle` is an early-stage Rust extension offering
**both deferred (scheduled) and immediate (transactional) IVM** with a richer SQL
dialect, a dependency DAG, and built-in operational tooling.

pg_trickle is **significantly ahead** of pg_ivm in SQL coverage, operator support,
aggregate support, and operational features. As of v0.2.1, pg_trickle also
matches pg_ivm's core strength — **immediate, in-transaction maintenance** — via
the `IMMEDIATE` refresh mode (all phases complete). pg_ivm's one remaining
structural advantage is **broader PostgreSQL version support (PG 13–18)**:

- **IMMEDIATE mode — fully implemented.** Statement-level AFTER triggers with
  transition tables update stream tables within the same transaction as
  base-table DML. Window functions, LATERAL, scalar subqueries, cascading
  IMMEDIATE stream tables, WITH RECURSIVE (with a stack-depth warning), and
  TopK micro-refresh are all supported. See
  [PLAN_TRANSACTIONAL_IVM.md](../sql/PLAN_TRANSACTIONAL_IVM.md).
- **AUTO refresh mode** — new default for `create_stream_table`. Selects
  DIFFERENTIAL when the query supports it and transparently falls back to
  FULL otherwise, eliminating the need to choose a mode at creation time.
- **pg_ivm compatibility layer — postponed.** The `pgivm.create_immv()` /
  `pgivm.refresh_immv()` / `pgivm.pg_ivm_immv` wrappers (Phase 2) are
  deferred to post-1.0.
- [PLAN_PG_BACKCOMPAT.md](../infra/PLAN_PG_BACKCOMPAT.md) details backporting
  pg_trickle to **PG 14–18** (recommended) or **PG 16–18** (minimum viable),
  requiring ~2.5–3 weeks of effort primarily in `#[cfg]`-gating ~435 lines
  of JSON/SQL-standard parse-tree handling.

With IMMEDIATE mode fully implemented and extension upgrade scripts shipping in
v0.2.1, **pg_ivm's only remaining advantage is PG version breadth**.
pg_trickle retains its 30+ unique features.

---

## 2. Project Overview

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Repository | [sraoss/pg_ivm](https://github.com/sraoss/pg_ivm) | [grove/pg-trickle](https://github.com/grove/pg-trickle) |
| Language | C | Rust (pgrx 0.17) |
| Latest release | 1.13 (2025-10-20) | 0.2.1 (2026-03-05); 0.2.2 in dev |
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

### Implemented: pg_trickle IMMEDIATE Mode

pg_trickle now offers an `IMMEDIATE` refresh mode (Phase 1 + Phase 3 complete)
that uses statement-level AFTER triggers with transition tables — the same
mechanism pg_ivm uses. Key implementation details:

- **Reuses the DVM engine** — the Scan operator reads from transition tables
  (via temporary views) instead of change buffer tables.
- **Phase 1** (complete): core IMMEDIATE engine — INSERT/UPDATE/DELETE/TRUNCATE
  handling, advisory lock-based concurrency (`IvmLockMode`), mode switching
  via `alter_stream_table`, query restriction validation.
- **Phase 2** (postponed): `pgivm.*` compatibility layer for drop-in migration.
- **Phase 3** (complete): extended SQL support — window functions, LATERAL,
  scalar subqueries, cascading IMMEDIATE stream tables, WITH RECURSIVE
  (IM1: supported with a stack-depth warning), and TopK micro-refresh
  (IM2: recomputes top-K on every DML, gated by `pg_trickle.ivm_topk_max_limit`).
- **Phase 4** (complete): delta SQL template caching (`IVM_DELTA_CACHE`); ENR-based
  transition tables and C-level triggers deferred to post-1.0 as optimizations only.

```sql
-- Create an IMMEDIATE stream table (zero staleness)
SELECT pgtrickle.create_stream_table(
    'live_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    NULL,          -- no schedule needed
    'IMMEDIATE'
);

-- Updates propagate within the same transaction
BEGIN;
  INSERT INTO orders (region, amount) VALUES ('EU', 100);
  SELECT * FROM live_totals;  -- already includes the new row
COMMIT;
```

---

## 4. SQL Feature Coverage — Summary

| Dimension | pg_ivm | pg_trickle | Winner |
|-----------|--------|-----------|--------|
| **Maintenance timing** | Immediate (in-transaction triggers) | Deferred (scheduler/manual) **and** IMMEDIATE (in-transaction) | **pg_trickle** (offers both models) |
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
| **CTEs (recursive)** | No | Yes (semi-naive, DRed, recomputation; IMMEDIATE mode with stack-depth warning) | **pg_trickle** |
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
| **TRUNCATE handling** | Yes (auto-truncate IMMV) | IMMEDIATE mode: full refresh in same txn; DEFERRED: queued full refresh | Tie (functionally equivalent in IMMEDIATE mode) |
| **Auto-indexing** | Yes (on GROUP BY / DISTINCT / PK columns) | No (user creates indexes) | pg_ivm |
| **Row Level Security** | Yes (with limitations) | Not documented / not tested | pg_ivm |
| **Concurrency model** | ExclusiveLock on IMMV during maintenance | Advisory locks, non-blocking reads | **pg_trickle** |
| **Data type restrictions** | Must have btree opclass (no json, xml, point) | No documented type restrictions | **pg_trickle** |
| **Maturity / ecosystem** | 4 years, 1.4k stars, PGXN, yum packages | v0.2.1 released, 1,042 unit tests + 819 E2E tests, dbt integration | pg_ivm |

### 4.1 Areas Where pg_ivm Wins

Of the ~35 dimensions in the summary table above, pg_ivm holds an advantage in
only **4** (down from 6 before IMMEDIATE mode was implemented). Two are
substantive, two are temporary gaps with existing plans.

#### 1. PostgreSQL Version Support (substantive, planned resolution)

pg_ivm ships pre-built packages for **PostgreSQL 13–18** across all major
Linux distros via yum.postgresql.org and PGXN. pg_trickle currently targets
**PG 18 only**.

This is the single largest remaining structural gap. PG 13 is EOL (Nov 2025),
but PG 14–17 are widely deployed in production environments. Users on those
versions simply cannot use pg_trickle today.

**Planned resolution:** [PLAN_PG_BACKCOMPAT.md](../infra/PLAN_PG_BACKCOMPAT.md)
details backporting to PG 14–18 (~2.5–3 weeks). pgrx 0.17 already supports
PG 14–18 via feature flags; ~435 lines in `parser.rs` need `#[cfg]` gating
for JSON/SQL-standard parse-tree handling.

#### 2. Auto-Indexing (substantive, low priority)

When pg_ivm creates an IMMV, it automatically adds indexes on columns used in
`GROUP BY`, `DISTINCT`, and primary keys. This is a genuine usability advantage
— new users get reasonable read performance without manual intervention.

pg_trickle leaves index creation entirely to the user. For DIFFERENTIAL mode
stream tables, the DVM engine's MERGE-based delta application already uses the
stream table's primary key (which is auto-created), but secondary indexes for
read-side query patterns must be added manually.

**Impact:** Low — experienced users always create application-specific indexes
anyway. Auto-indexing mostly helps onboarding and simple use-cases.

**Planned resolution:** Tracked as part of the pg_ivm compatibility layer
(Phase 2, postponed to post-1.0). Could also be implemented independently as
a `CREATE INDEX IF NOT EXISTS` step in `create_stream_table`.

#### 3. Row Level Security (niche gap, untested)

pg_ivm respects Row Level Security policies during IMMV maintenance — the
trigger functions execute with the permissions of the IMMV owner, and RLS
policies on base tables are enforced. There are caveats: if an RLS policy
changes, the IMMV must be manually refreshed to reflect the new policy.

pg_trickle has not documented or tested RLS interaction. The trigger-based
CDC captures all row changes regardless of RLS policies, and the refresh
process runs as the extension owner. It is unknown whether RLS policies on
base tables are correctly enforced during delta application.

**Impact:** Niche — RLS on base tables feeding into materialized views is
uncommon in practice. Most deployments apply RLS at the application query
layer, not on the materialized summary.

**Planned resolution:** 2–3 hours to document behavior and add E2E tests.

#### 4. Maturity / Ecosystem (temporary, closing over time)

pg_ivm has **4 years of production use**, ~1,400 GitHub stars, 17 releases,
and is distributed via PGXN, yum, and apt package repositories. It has a
track record of stability and a community of users.

pg_trickle is a **v0.2.x** series release with 1,042 unit tests and 819 E2E
tests but no wide production deployments yet. It lacks the battle-testing that
comes from years of real-world usage.

**Impact:** High for risk-averse organizations considering production adoption.
Low for greenfield projects or teams willing to adopt early.

**Resolution:** This gap closes naturally with time, releases, and adoption.
The dbt integration (`dbt-pgtrickle`) and CNPG/Kubernetes deployment support
accelerate ecosystem development.

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
| WITH RECURSIVE | ❌ | ✅ (semi-naive, DRed, recomputation; IMMEDIATE mode with stack-depth warning) |

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
-- Create a stream table (AUTO mode: DIFFERENTIAL when possible, FULL fallback)
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region'
    -- refresh_mode defaults to 'AUTO', schedule defaults to 'calculated'
);

-- Create a stream table (explicit deferred, scheduled)
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    schedule     => '2m',
    refresh_mode => 'DIFFERENTIAL'
);

-- Create a stream table (immediate, in-transaction)
SELECT pgtrickle.create_stream_table(
    'live_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    schedule     => NULL,
    refresh_mode => 'IMMEDIATE'
);

-- Manual refresh
SELECT pgtrickle.refresh_stream_table('order_totals');

-- Alter schedule, mode, or defining query
SELECT pgtrickle.alter_stream_table('order_totals', schedule => '5m');
SELECT pgtrickle.alter_stream_table(
    'order_totals',
    query => 'SELECT region, SUM(amount) AS total FROM orders WHERE active GROUP BY region'
);

-- Drop
SELECT pgtrickle.drop_stream_table('order_totals');

-- Status and monitoring
SELECT * FROM pgtrickle.pgt_status();
SELECT * FROM pgtrickle.pg_stat_stream_tables;
SELECT * FROM pgtrickle.pgt_stream_tables;

-- DAG inspection
SELECT * FROM pgtrickle.pgt_dependencies;

-- Extended observability (added v0.2.0)
SELECT * FROM pgtrickle.change_buffer_sizes();  -- CDC buffer health
SELECT * FROM pgtrickle.list_sources('order_totals');  -- source table stats
SELECT * FROM pgtrickle.dependency_tree();  -- ASCII DAG view
SELECT * FROM pgtrickle.health_check();  -- OK/WARN/ERROR triage
SELECT * FROM pgtrickle.refresh_timeline();  -- cross-stream history
SELECT * FROM pgtrickle.trigger_inventory();  -- CDC trigger audit
SELECT * FROM pgtrickle.diamond_groups();  -- diamond consistency groups
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
| CDC buffer health | ❌ | ✅ `pgtrickle.change_buffer_sizes()` (v0.2.0) |
| Source table stats | ❌ | ✅ `pgtrickle.list_sources()` (v0.2.0) |
| Dependency tree view | ❌ | ✅ `pgtrickle.dependency_tree()` (v0.2.0) |
| Health triage | ❌ | ✅ `pgtrickle.health_check()` (v0.2.0) |
| Cross-stream refresh history | ❌ | ✅ `pgtrickle.refresh_timeline()` (v0.2.0) |
| CDC trigger audit | ❌ | ✅ `pgtrickle.trigger_inventory()` (v0.2.0) |
| Diamond group inspection | ❌ | ✅ `pgtrickle.diamond_groups()` (v0.2.0) |

---

## 11. Installation and Deployment

| Attribute | pg_ivm | pg_trickle |
|---|---|---|
| Pre-built packages | RPM via yum.postgresql.org | OCI image, tarball |
| CNPG / Kubernetes | ❌ (no OCI image) | ✅ OCI extension image |
| Docker local dev | Manual | ✅ documented |
| `shared_preload_libraries` | Required (or `session_preload_libraries`) | Required |
| Extension upgrade scripts | ✅ (1.0 → 1.1 → … → 1.13) | ✅ (0.1.3 → 0.2.0 → 0.2.1, CI completeness check, upgrade E2E tests) |
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
- In DIFFERENTIAL/FULL mode, data is stale between refresh cycles.
  Use **IMMEDIATE mode** for zero-staleness, in-transaction consistency.
- Recursive CTEs in IMMEDIATE mode emit a stack-depth warning; very deep
  recursion may hit PostgreSQL's stack limit.
- `LIMIT` without `ORDER BY` is not supported in defining queries.
- `OFFSET` without `ORDER BY … LIMIT` is not supported. Paged TopK
  (`ORDER BY … LIMIT N OFFSET M`) is fully supported.
- `ORDER BY` + `LIMIT` (TopK) without OFFSET uses scoped recomputation (MERGE).
- Volatile SQL functions rejected in DIFFERENTIAL mode.
- Materialized views as sources not supported in DIFFERENTIAL mode.
- `ALTER EXTENSION pg_trickle UPDATE` migration scripts ship from v0.2.1
  (0.1.3→0.2.0→0.2.1); future upgrade scripts planned for each release.
- Targets PostgreSQL 18 only; no backport to PG 13–17 (planned for PG 14–18).
- v0.2.x series — not yet production-hardened.

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

### Features Unique to pg_trickle (30 items, no pg_ivm equivalent)

1. **IMMEDIATE + deferred modes** (pg_ivm is immediate-only; pg_trickle offers both)
2. **39+ aggregate functions** (vs 5)
3. **FILTER / HAVING / WITHIN GROUP** on aggregates
4. **Window functions** (partition recomputation)
5. **Set operations** (UNION ALL, UNION, INTERSECT, EXCEPT — all 6 variants)
6. **Recursive CTEs** (semi-naive, DRed, recomputation; including IMMEDIATE mode with stack-depth warning)
7. **LATERAL subqueries and SRFs** (jsonb_array_elements, unnest, JSON_TABLE)
8. **Anti-join / semi-join operators** (NOT EXISTS, NOT IN, IN, EXISTS with full SQL)
9. **Scalar subqueries** in SELECT list
10. **Views as sources** (auto-inlined with nested expansion)
11. **Partitioned table support**
12. **Cascading stream tables** (ST referencing other STs via DAG)
13. **Background scheduler** (cron + duration + canonical periods) with **multi-database auto-discovery**
14. **GROUPING SETS / CUBE / ROLLUP** (auto-rewritten)
15. **DISTINCT ON** (auto-rewritten to ROW_NUMBER)
16. **Hybrid CDC** (trigger → WAL transition)
17. **DDL change detection** and automatic reinitialization (including ALTER FUNCTION body changes)
18. **Monitoring suite** (7 new observability functions: `change_buffer_sizes`, `list_sources`,
    `dependency_tree`, `health_check`, `refresh_timeline`, `trigger_inventory`, `diamond_groups`)
19. **Auto-rewrite pipeline** (6 transparent SQL rewrites)
20. **Volatile function detection**
21. **AUTO refresh mode** (smart DIFFERENTIAL/FULL selection with transparent fallback)
22. **ALTER QUERY** — change the defining query of an existing stream table online,
    with schema-change classification and OID-preserving migration
23. **dbt macro package**
24. **CNPG / Kubernetes deployment**
25. **SQL/JSON constructors** (JSON_OBJECT, JSON_ARRAY, etc.)
26. **JSON_TABLE** support (PG 17+)
27. **TopK stream tables** (ORDER BY + LIMIT, including IMMEDIATE mode via micro-refresh)
28. **Paged TopK** (ORDER BY + LIMIT + OFFSET for server-side pagination)
29. **Diamond dependency consistency** (multi-path refresh atomicity with SAVEPOINT)
30. **Extension upgrade infrastructure** (SQL migration scripts, CI completeness check,
    upgrade E2E tests, per-release SQL baselines)

### Features Unique to pg_ivm (with planned resolutions)

| # | Feature | Status | Ref |
|---|---------|--------|-----|
| 1 | **Immediate (synchronous) maintenance** | ✅ **Closed** — IMMEDIATE refresh mode fully implemented (all phases) | [PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md) |
| 2 | **Auto-index creation** on GROUP BY / DISTINCT / PK | Postponed (Phase 2 of transactional IVM) | [PLAN_TRANSACTIONAL_IVM §5.2](../sql/PLAN_TRANSACTIONAL_IVM.md) |
| 3 | **TRUNCATE propagation** (auto-truncate IMMV) | ✅ **Closed** — IMMEDIATE mode fires full refresh on TRUNCATE | [PLAN_TRANSACTIONAL_IVM §3.2](../sql/PLAN_TRANSACTIONAL_IVM.md) |
| 4 | **Row Level Security** respect | Not yet addressed | — |
| 5 | **PostgreSQL 13–17 support** | PG 14–18 backcompat planned (~2.5–3 weeks) | [PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md) |
| 6 | **session_preload_libraries** | Not applicable (background worker needs shared_preload) | — |
| 7 | **Rename via ALTER TABLE** | Event trigger support (low effort) | — |
| 8 | **Drop via DROP TABLE** | Postponed (Phase 2 of transactional IVM) | [PLAN_TRANSACTIONAL_IVM §4.3](../sql/PLAN_TRANSACTIONAL_IVM.md) |
| 9 | **Extension upgrade scripts** | ✅ **Closed** — Scripts ship from v0.2.1; CI completeness check and upgrade E2E tests in place | — |

Of the 9 items, **4 are now closed** (immediate maintenance, TRUNCATE, upgrade scripts), **3
have concrete implementation plans**, and 2 are low-priority or not applicable.

---

## 16. Use-Case Fit

| Scenario | Recommended |
|---|---|
| Need views consistent within the same transaction | **Either** (pg_trickle IMMEDIATE mode or pg_ivm) |
| Application cannot tolerate any view staleness | **Either** (pg_trickle IMMEDIATE mode or pg_ivm) |
| High write throughput, views can be slightly stale | **pg_trickle** (DIFFERENTIAL mode) |
| Multi-layer summary pipelines with dependencies | **pg_trickle** |
| Time-based or cron-driven refresh schedules | **pg_trickle** |
| Views with complex SQL (window functions, CTEs, UNION) | **pg_trickle** |
| Simple aggregation with zero-staleness requirement | **Either** (pg_trickle has richer SQL coverage) |
| Kubernetes / CloudNativePG deployment | **pg_trickle** |
| dbt integration | **pg_trickle** |
| PostgreSQL 13–17 | **pg_ivm** |
| PostgreSQL 18 | **pg_trickle** (superset of pg_ivm) |
| Production-hardened, stable API | **pg_ivm** |
| Early adopter, rich SQL coverage needed | **pg_trickle** |

---

## 17. Coexistence

The two extensions can be installed in the same database simultaneously — they
use different schemas (`pgivm` vs `pgtrickle`/`pgtrickle_changes`) and do not
interfere with each other. However, with pg_trickle's `IMMEDIATE` mode now
available, there is less reason to use both:

- Use **pg_trickle IMMEDIATE** for small, critical lookup tables that must be
  perfectly consistent within transactions (the use-case that previously
  required pg_ivm).
- Use **pg_trickle DIFFERENTIAL/FULL** for large analytical summary tables,
  multi-layer aggregation pipelines, or views where slight staleness is
  acceptable.
- Use **pg_ivm** only if you need PostgreSQL 13–17 support or prefer its
  mature, battle-tested codebase.

---

## 18. Recommendations

### Planned work that closes pg_ivm gaps

| Priority | Item | Plan | Effort | Closes Gaps |
|----------|------|------|--------|-------------|
| ✅ Done | IMMEDIATE refresh mode (all phases) | [PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md) | Complete | #1 (immediate maintenance), #3 (TRUNCATE) |
| ✅ Done | Extension upgrade scripts | v0.2.1 release | Complete | #9 (upgrade scripts) |
| Postponed | pg_ivm compatibility layer | [PLAN_TRANSACTIONAL_IVM](../sql/PLAN_TRANSACTIONAL_IVM.md) Phase 2 | Deferred to post-1.0 | #2 (auto-indexing), #7 (rename), #8 (DROP TABLE) |
| **High** | PG 16–18 backcompat (MVP) | [PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md) §11 | ~1.5 weeks | #5 (PG version support) |
| **Medium** | PG 14–18 backcompat (full) | [PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md) §5 | ~2.5–3 weeks | #5 (PG version support) |

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

The **immediate maintenance** gap is now fully closed: pg_trickle's `IMMEDIATE`
refresh mode provides the same in-transaction consistency as pg_ivm, while also
supporting window functions, LATERAL, scalar subqueries, WITH RECURSIVE (IM1),
TopK micro-refresh (IM2), and cascading stream tables in IMMEDIATE mode — all
of which pg_ivm cannot do.

The **upgrade infrastructure** gap is also closed: v0.2.1 ships SQL migration
scripts (0.1.3→0.2.0→0.2.1), a CI completeness checker, and upgrade E2E tests,
matching pg_ivm's upgrade path story.

The one remaining structural gap is **PG version support**:

- **[PLAN_PG_BACKCOMPAT](../infra/PLAN_PG_BACKCOMPAT.md)** details backporting
  to PG 14–18 (or PG 16–18 as MVP) in ~2.5–3 weeks, primarily by `#[cfg]`-
  gating ~435 lines of JSON/SQL-standard parse-tree code.

Once backcompat is implemented, **pg_trickle will be a strict superset of
pg_ivm** in every dimension: same immediate maintenance model, comparable PG
version support (14–18 vs 13–18, with PG 13 EOL), dramatically wider SQL
coverage, and a complete operational layer that pg_ivm entirely lacks.

For users migrating from pg_ivm, the `IMMEDIATE` refresh mode already provides
the same zero-staleness guarantee. A full compatibility layer (`pgivm.create_immv`,
`pgivm.refresh_immv`, `pgivm.pg_ivm_immv`) is planned for post-1.0 to enable
**zero-change migration**.

---

## References

- pg_ivm repository: https://github.com/sraoss/pg_ivm
- pg_trickle repository: https://github.com/grove/pg-trickle
- DBSP differential dataflow paper: https://arxiv.org/abs/2203.16684
- pg_trickle ESSENCE.md: [../../ESSENCE.md](../../ESSENCE.md)
- pg_trickle DVM operators: [../../docs/DVM_OPERATORS.md](../../docs/DVM_OPERATORS.md)
- pg_trickle architecture: [../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
