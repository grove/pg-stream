# Gap Analysis: pg_trickle vs pg_ivm

> **Date:** 2026-02-28
> **pg_trickle version:** 0.1.2
> **pg_ivm version:** 1.13 (2025-10-20)

---

## Executive Summary

pg_trickle is **significantly ahead** of pg_ivm in SQL coverage, operator
support, aggregate support, and operational features. pg_ivm's two structural
advantages — **immediate (synchronous) maintenance** and **broader PostgreSQL
version support (PG 13–18)** — are both addressed by existing pg_trickle plans:

- [PLAN_TRANSACTIONAL_IVM.md](sql/PLAN_TRANSACTIONAL_IVM.md) proposes an
  `IMMEDIATE` refresh mode with statement-level AFTER triggers, transition
  tables, and a full **pg_ivm compatibility layer** (`pgivm.create_immv()`,
  `pgivm.refresh_immv()`, `pgivm.pg_ivm_immv` catalog view).
- [PLAN_PG_BACKCOMPAT.md](infra/PLAN_PG_BACKCOMPAT.md) details backporting
  pg_trickle to **PG 14–18** (recommended) or **PG 16–18** (minimum viable),
  requiring ~2.5–3 weeks of effort primarily in `#[cfg]`-gating ~435 lines
  of JSON/SQL-standard parse-tree handling.

Once these plans are implemented, **every pg_ivm advantage will be neutralized
or surpassed**, while pg_trickle retains its 24+ unique features.

The table below summarizes the comparison across every dimension. Sections that
follow provide detailed breakdowns.

| Dimension | pg_ivm | pg_trickle | Winner |
|-----------|--------|-----------|--------|
| **Maintenance timing** | Immediate (in-transaction triggers) | Deferred (scheduler/manual); **IMMEDIATE mode planned** | pg_ivm (today); **planned parity** |
| **PostgreSQL versions** | 13–18 | 18 only; **PG 14–18 planned** | pg_ivm (today); **planned parity** |
| **Language / implementation** | C (PGXS) | Rust (pgrx) | — |
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
| **pg_dump / pg_upgrade** | Must manually drop + recreate | N/A (extension upgrade migrations planned) | — |
| **Concurrency model** | ExclusiveLock on IMMV during maintenance | Advisory locks, non-blocking reads | **pg_trickle** |
| **Row Level Security** | Yes (with limitations) | Not documented | pg_ivm |
| **Data type restrictions** | Must have btree opclass (no json, xml, point) | No documented type restrictions | **pg_trickle** |
| **Maturity / ecosystem** | 4 years, 1.4k stars, PGXN, yum packages | Pre-release (0.1.2), dbt integration | pg_ivm |

---

## Detailed Comparison

### 1. Maintenance Timing

| | pg_ivm | pg_trickle |
|-|--------|-----------|
| **Model** | Immediate (synchronous, in AFTER triggers) | Deferred (background scheduler or manual) |
| **Staleness** | Zero — view is always current | Configurable (30s to weeks, or cron) |
| **Write overhead** | Every DML on base tables pays IVM cost in the same transaction | Zero write-path overhead (triggers only buffer changes as JSONB) |
| **Lock impact** | ExclusiveLock on IMMV during maintenance (serializes concurrent writes) | Advisory lock on ST only during refresh; reads never blocked |

**Gap for pg_trickle (current):** No immediate/synchronous mode. The deferred
model is better for write-heavy workloads but cannot match pg_ivm's
zero-staleness guarantee.

**Planned resolution:** [PLAN_TRANSACTIONAL_IVM.md](sql/PLAN_TRANSACTIONAL_IVM.md)
defines an `IMMEDIATE` refresh mode that uses statement-level AFTER triggers
with transition tables (the same mechanism pg_ivm uses). Key design decisions:

- **Reuses the DVM engine** — the Scan operator reads from Ephemeral Named
  Relations (transition tables) instead of change buffer tables. All other
  operators (Filter, Project, Join, Aggregate, etc.) work unchanged.
- **Phase 1** covers pg_ivm's full query subset: inner/outer joins, GROUP BY
  with COUNT/SUM/AVG/MIN/MAX, DISTINCT, simple CTEs, EXISTS subqueries,
  plus pg_trickle's auto-rewrites (DISTINCT ON, GROUPING SETS, view inlining).
- **Phase 2** adds a `pgivm.*` compatibility layer: `pgivm.create_immv()`,
  `pgivm.refresh_immv()`, `pgivm.get_immv_def()`, and a `pgivm.pg_ivm_immv`
  catalog view — enabling **drop-in migration** from pg_ivm.
- **Phase 3** extends IMMEDIATE mode to window functions, set operations,
  LATERAL, recursive CTEs, and cascading IMMEDIATE stream tables.
- **Concurrency** follows pg_ivm's proven ExclusiveLock approach. The existing
  deferred mode (with advisory locks and non-blocking reads) remains the
  default.

**Gap for pg_ivm:** Synchronous maintenance adds significant write latency,
especially for joins and aggregates. Cannot be turned off per-statement — only
bulk disable/re-enable via `refresh_immv(false)` / `refresh_immv(true)`.

### 2. Aggregate Functions

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

### 3. Joins

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

**Gap for pg_ivm:** Outer joins are heavily restricted — single equijoin condition, no aggregates, no subqueries, no CASE expressions, no IS NULL in WHERE. pg_trickle has none of these restrictions.

### 4. Subqueries

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| Simple subquery in FROM | ✅ (no aggregates/DISTINCT inside) | ✅ (full support) |
| EXISTS in WHERE | ✅ (AND only, no agg/DISTINCT inside, correlated cols must be in targetlist) | ✅ (AND + OR, full SQL inside) |
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

### 5. CTEs

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| Simple non-recursive CTE | ✅ (no aggregates/DISTINCT inside) | ✅ (full SQL inside) |
| Multi-reference CTE | ? | ✅ (shared delta optimization) |
| Chained CTEs | ? | ✅ |
| WITH RECURSIVE | ❌ | ✅ (semi-naive, DRed, recomputation) |

**Gap for pg_ivm:** No recursive CTEs, no aggregates/DISTINCT inside CTEs.

### 6. Set Operations

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| UNION ALL | ❌ | ✅ |
| UNION (set) | ❌ | ✅ (via DISTINCT + UNION ALL) |
| INTERSECT | ❌ | ✅ (dual-count multiplicity) |
| INTERSECT ALL | ❌ | ✅ |
| EXCEPT | ❌ | ✅ (dual-count multiplicity) |
| EXCEPT ALL | ❌ | ✅ |

**Gap for pg_ivm:** No set operations at all.

### 7. Window Functions

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| ROW_NUMBER, RANK, DENSE_RANK | ❌ | ✅ |
| SUM/AVG/COUNT OVER () | ❌ | ✅ |
| Frame clauses (ROWS/RANGE/GROUPS) | ❌ | ✅ |
| Named WINDOW clauses | ❌ | ✅ |
| PARTITION BY recomputation | ❌ | ✅ |

**Gap for pg_ivm:** Window functions are completely unsupported.

### 8. Source Table Types

| Source type | pg_ivm | pg_trickle |
|-------------|--------|-----------|
| Simple heap tables | ✅ | ✅ |
| Views | ❌ | ✅ (auto-inlined) |
| Materialized views | ❌ | FULL mode only |
| Partitioned tables | ❌ | ✅ |
| Partitions | ❌ | ✅ (via parent) |
| Inheritance parent tables | ❌ | ? |
| Foreign tables | ❌ | FULL mode only |
| Other IMMVs / stream tables | ❌ | ✅ (DAG cascading) |

**Gap for pg_ivm:** Only simple heap tables. No views, no partitioned tables, no cascading.

### 9. Operational Features

| Feature | pg_ivm | pg_trickle |
|---------|--------|-----------|
| Automatic scheduling | ❌ (manual or in-trigger) | ✅ (cron + duration + background worker) |
| Monitoring views | ❌ | ✅ (refresh stats, staleness, CDC health) |
| Refresh history / audit | ❌ | ✅ (pgt_refresh_history) |
| NOTIFY alerting | ❌ | ✅ (pg_trickle_alert channel) |
| DDL change detection | ❌ (silent breakage) | ✅ (event triggers, auto-reinit) |
| Dependency graph | ❌ | ✅ (cycle detection, topo sort) |
| Adaptive FULL fallback | ❌ | ✅ (based on change ratio) |
| WAL-based CDC option | ❌ | ✅ (hybrid trigger→WAL) |
| dbt integration | ❌ | ✅ (dbt-pgtrickle macro package) |
| CNPG / Kubernetes | ❌ | ✅ (CNPG extension image) |
| Explain/introspection | ❌ | ✅ (explain_st) |

**Gap for pg_ivm:** No operational infrastructure at all. Users are responsible for all maintenance coordination.

### 10. Concurrency & Locking

| Aspect | pg_ivm | pg_trickle |
|--------|--------|-----------|
| Lock during maintenance | ExclusiveLock (blocks concurrent DML on IMMV) | Advisory lock (non-blocking reads) |
| REPEATABLE READ / SERIALIZABLE | Errors if concurrent maintenance detected | No issues (deferred, reads snapshot) |
| Concurrent base table writes | Serialized per IMMV | Decoupled (changes buffered) |

**Gap for pg_ivm:** ExclusiveLock is a significant concurrency bottleneck. pg_trickle's advisory-lock approach allows reads to continue during refresh.

### 11. Auto-Indexing

pg_ivm automatically creates unique indexes on GROUP BY columns, DISTINCT columns, or primary key columns. pg_trickle creates a primary key on `__pgt_row_id` but does not create additional indexes on user-facing columns.

**Gap for pg_trickle:** Consider auto-creating indexes on GROUP BY / DISTINCT columns for efficient delta application, similar to pg_ivm.

### 12. TRUNCATE Propagation

pg_ivm automatically truncates the IMMV when a base table is truncated. pg_trickle detects TRUNCATE via CDC (the trigger fires per-row if rows exist, or not at all) and handles it through the normal refresh cycle.

**Gap for pg_trickle:** Minor — TRUNCATE of an empty table with subsequent inserts will be handled at next refresh, not immediately.

### 13. Row Level Security

pg_ivm respects RLS policies on base tables. pg_trickle does not document RLS behavior.

**Gap for pg_trickle:** RLS interaction should be documented and tested.

### 14. pg_dump / pg_upgrade

pg_ivm requires manually dropping and recreating all IMMVs after pg_dump restore or pg_upgrade. pg_trickle's upgrade story is in progress (planned for v0.3.0).

**Gap for both:** Neither has a seamless upgrade path.

### 15. PostgreSQL Version Support

| | pg_ivm | pg_trickle (current) | pg_trickle (planned) |
|-|--------|---------------------|---------------------|
| PG 13 | ✅ | ❌ | ❌ (EOL Nov 2025) |
| PG 14 | ✅ | ❌ | ✅ (full plan) |
| PG 15 | ✅ | ❌ | ✅ (full plan) |
| PG 16 | ✅ | ❌ | ✅ (MVP target) |
| PG 17 | ✅ | ❌ | ✅ (MVP target) |
| PG 18 | ✅ | ✅ | ✅ |

**Gap for pg_trickle (current):** PG 18-only. Can't be used on PG 14–17.

**Planned resolution:** [PLAN_PG_BACKCOMPAT.md](infra/PLAN_PG_BACKCOMPAT.md)
details two target scopes:

- **Minimum viable (PG 16–18):** ~1.5 weeks effort. Only JSON_TABLE (~250
  lines) needs `#[cfg]` gating for PG 17+. SQL/JSON constructors (~150 lines)
  need gating for PG 16+.
- **Full target (PG 14–18):** ~2.5–3 weeks effort. `raw_parser()` API is
  identical on PG 14+. PG 13 is intentionally dropped (EOL, incompatible
  parser API).

Key findings from the backcompat analysis:
- **pgrx 0.17.0 already supports PG 14–18** via feature flags (bindgen
  regenerates all `pg_sys::` types per version).
- **~435 lines** in `src/dvm/parser.rs` need `#[cfg]` gating (all in
  JSON/SQL-standard sections). The remaining ~13,500 lines compile unchanged.
- **Shared memory, SPI, triggers, event triggers, background workers** all
  work identically on PG 14–18.
- **WAL decoder** uses pure SPI (zero unsafe blocks) — API available since
  PG 10. Trigger-based CDC works on all versions. WAL CDC can be progressively
  enabled on PG 16–17 after `pgoutput` format validation.

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

## Features Unique to pg_trickle (No pg_ivm Equivalent)

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

## Features Unique to pg_ivm (No pg_trickle Equivalent Today)

| # | Feature | Planned Resolution | Ref |
|---|---------|-------------------|-----|
| 1 | **Immediate (synchronous) maintenance** | `IMMEDIATE` refresh mode + pg_ivm compat layer | [PLAN_TRANSACTIONAL_IVM](sql/PLAN_TRANSACTIONAL_IVM.md) |
| 2 | **Auto-index creation** on GROUP BY / DISTINCT / PK | Phase 2 of transactional IVM plan | [PLAN_TRANSACTIONAL_IVM §5.2](sql/PLAN_TRANSACTIONAL_IVM.md) |
| 3 | **TRUNCATE propagation** (auto-truncate IMMV) | Handled by IMMEDIATE mode TRUNCATE triggers | [PLAN_TRANSACTIONAL_IVM §3.2](sql/PLAN_TRANSACTIONAL_IVM.md) |
| 4 | **Row Level Security** respect | Document and test | — |
| 5 | **PostgreSQL 13–17 support** | PG 14–18 backcompat (~2.5–3 weeks) | [PLAN_PG_BACKCOMPAT](infra/PLAN_PG_BACKCOMPAT.md) |
| 6 | **session_preload_libraries** | Not applicable (background worker needs shared_preload) | — |
| 7 | **Rename via ALTER TABLE** | Event trigger support (low effort) | — |
| 8 | **Drop via DROP TABLE** | Phase 2 of transactional IVM plan (object_access_hook or event trigger) | [PLAN_TRANSACTIONAL_IVM §4.3](sql/PLAN_TRANSACTIONAL_IVM.md) |

Of the 8 items, **5 have concrete implementation plans**. Only RLS testing,
`session_preload_libraries` (not applicable), and `ALTER TABLE RENAME` remain
unaddressed.

---

## Recommendations

### Planned work that closes pg_ivm gaps:

| Priority | Item | Plan | Effort | Closes Gaps |
|----------|------|------|--------|-------------|
| **High** | IMMEDIATE refresh mode | [PLAN_TRANSACTIONAL_IVM](sql/PLAN_TRANSACTIONAL_IVM.md) Phase 1 | Medium | #1 (immediate maintenance), #3 (TRUNCATE) |
| **High** | pg_ivm compatibility layer | [PLAN_TRANSACTIONAL_IVM](sql/PLAN_TRANSACTIONAL_IVM.md) Phase 2 | Medium | #2 (auto-indexing), #7 (rename), #8 (DROP TABLE) |
| **High** | PG 16–18 backcompat (MVP) | [PLAN_PG_BACKCOMPAT](infra/PLAN_PG_BACKCOMPAT.md) §11 | ~1.5 weeks | #5 (PG version support) |
| **Medium** | PG 14–18 backcompat (full) | [PLAN_PG_BACKCOMPAT](infra/PLAN_PG_BACKCOMPAT.md) §5 | ~2.5–3 weeks | #5 (PG version support) |
| **Medium** | Extended IMMEDIATE SQL | [PLAN_TRANSACTIONAL_IVM](sql/PLAN_TRANSACTIONAL_IVM.md) Phase 3 | Medium | Extends #1 beyond pg_ivm parity |

### Remaining small gaps (no existing plan):

| Priority | Item | Description | Effort |
|----------|------|-------------|--------|
| Low | RLS documentation | Document and test Row Level Security interaction | 2–3h |
| Low | ALTER TABLE RENAME | Detect rename via event trigger, update catalog | 2–4h |

### Things NOT worth pursuing:

| Item | Reason |
|------|--------|
| PG 13 support | EOL since November 2025. Incompatible `raw_parser()` API. pg_ivm supports it but the user base is negligible. |
| session_preload_libraries | Requires background worker, which needs shared_preload_libraries. |

---

## Conclusion

pg_trickle covers **all** of pg_ivm's SQL surface and extends it dramatically
with 34+ additional aggregate functions, window functions, set operations,
recursive CTEs, LATERAL support, anti/semi-joins, and a comprehensive
operational layer.

The two remaining structural gaps — **immediate maintenance** and **PG version
support** — both have detailed implementation plans:

1. **[PLAN_TRANSACTIONAL_IVM](sql/PLAN_TRANSACTIONAL_IVM.md)** adds an
   `IMMEDIATE` refresh mode that reuses the DVM engine with transition-table-
   based delta sources, plus a `pgivm.*` compatibility layer for drop-in
   pg_ivm migration. The plan covers 4 phases from MVP (pg_ivm SQL parity)
   through extended SQL support and performance optimization.

2. **[PLAN_PG_BACKCOMPAT](infra/PLAN_PG_BACKCOMPAT.md)** details backporting
   to PG 14–18 (or PG 16–18 as MVP) in ~2.5–3 weeks, primarily by `#[cfg]`-
   gating ~435 lines of JSON/SQL-standard parse-tree code. pgrx 0.17.0
   already provides the framework-level multi-version support.

Once both plans are implemented, **pg_trickle will be a strict superset of
pg_ivm** in every dimension: same immediate maintenance model, broader PG
version support (14–18 vs 13–18, with PG 13 EOL), dramatically wider SQL
coverage, and a complete operational layer that pg_ivm entirely lacks.

For users migrating from pg_ivm, the compatibility layer (`pgivm.create_immv`,
`pgivm.refresh_immv`, `pgivm.pg_ivm_immv`) enables **zero-change migration**.
Once migrated, users can optionally switch to `DIFFERENTIAL` mode for better
write-path performance, or keep `IMMEDIATE` for read-your-writes consistency
while gaining access to pg_trickle's 24+ unique SQL features.
