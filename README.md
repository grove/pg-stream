# pg_trickle

[![Build](https://github.com/grove/pg-trickle/actions/workflows/build.yml/badge.svg)](https://github.com/grove/pg-trickle/actions/workflows/build.yml)
[![CI](https://github.com/grove/pg-trickle/actions/workflows/ci.yml/badge.svg)](https://github.com/grove/pg-trickle/actions/workflows/ci.yml)
[![Release](https://github.com/grove/pg-trickle/actions/workflows/release.yml/badge.svg)](https://github.com/grove/pg-trickle/actions/workflows/release.yml)
[![Coverage](https://codecov.io/gh/grove/pg-trickle/branch/main/graph/badge.svg)](https://codecov.io/gh/grove/pg-trickle)
[![Benchmarks](https://img.shields.io/badge/Benchmarks-view-blue)](docs/BENCHMARK.md)
[![Roadmap](https://img.shields.io/badge/Roadmap-view-informational)](ROADMAP.md)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![PostgreSQL 18](https://img.shields.io/badge/PostgreSQL-18-blue?logo=postgresql&logoColor=white)](https://www.postgresql.org/)
[![pgrx 0.17](https://img.shields.io/badge/pgrx-0.17-orange)](https://github.com/pgcentralfoundation/pgrx)

> **Pre-1.0 Notice:** pg_trickle is under active development and has not yet reached a stable 1.0 release. The core feature set is extensive and thoroughly tested, but APIs and configuration options may still change between minor versions. See [ROADMAP.md](ROADMAP.md) for the planned path to 1.0.

> For a plain-language description of the problem pg_trickle solves, the differential dataflow approach, and the hybrid CDC architecture, read **[ESSENCE.md](ESSENCE.md)**.

**Stream Tables for PostgreSQL 18**

pg_trickle brings declarative, automatically-refreshing materialized views to PostgreSQL, inspired by the [DBSP](https://arxiv.org/abs/2203.16684) differential dataflow framework ([comparison](docs/research/DBSP_COMPARISON.md)). Define a SQL query and a schedule bound (or cron schedule); the extension handles the rest.

## History and Motivation

This project started with a practical goal. We were inspired by existing data platforms built around pipelines that keep themselves incrementally up to date, and we wanted to bring that same style of self-maintaining data flow directly into PostgreSQL. In particular, we needed support for recursive CTEs, which were essential to the kinds of pipelines we had in mind. We could not find an open-source incremental view maintenance system that matched that requirement, so pg_trickle began as an attempt to close that gap.

It also became an experiment in what coding agents could realistically help build. We set out to develop pg_trickle without editing code by hand, while still holding it to the same bar we would expect from any other systems project: broad feature coverage, strong code quality, extensive tests, and thorough documentation. Skepticism toward AI-written software is reasonable; the right way to evaluate pg_trickle is by the codebase, the tests, and the docs.

That constraint changed how we worked. Agents can produce a lot of surface area quickly, but database systems are unforgiving of vague assumptions and hidden edge cases. To make the project hold together, we had to be unusually explicit about architecture, operator semantics, failure handling, and test coverage. In practice, that pushed us toward more written design, more reviewable behavior, and more verification than a quick prototype would normally get.

The result is a **spec-driven** development process, not vibe-coding. Every feature starts as a written plan — an architecture decision record, a gap analysis, or a phased implementation spec — before any code is generated. The [`plans/`](plans/) directory contains over 110 documents (~72,500 lines) covering operator semantics, CDC tradeoffs, performance strategies, ecosystem comparisons, and edge case catalogues. Agents work from these specs; the specs are reviewed and revised by humans. This is what makes it possible to maintain coherence across a large codebase without manually editing every line: the design is explicit, the invariants are written down, and the tests verify both.

We also do not think the use of AI should lower the standard for trust. If anything, it raises it. The point of the experiment was not to ask people to trust the toolchain; it was to see whether disciplined use of coding agents could help produce a serious, inspectable PostgreSQL extension. Whether that worked is for readers and users to judge, but the intent is simple: make the code, the tests, the documentation, and the tradeoffs visible enough that the project can stand on its own merits.

## Key Features

- **Declarative** — define a query and a schedule bound (or cron expression); the extension schedules and executes refreshes automatically.
- **Four refresh modes** — `AUTO` (smart default: DIFFERENTIAL when possible, FULL fallback), `DIFFERENTIAL` (incremental delta), `FULL` (complete recomputation), and `IMMEDIATE` (synchronous in-transaction maintenance via statement-level triggers with transition tables).
- **Differential View Maintenance (DVM)** — only processes changed rows, not the entire base table. Delta queries are derived automatically from the defining query's operator tree.
- **Transactional IVM (IMMEDIATE mode)** — stream tables can be maintained **within the same transaction** as the base table DML, providing read-your-writes consistency. Supports all DVM operators including window functions, LATERAL joins, scalar subqueries, cascading IMMEDIATE stream tables, WITH RECURSIVE, and TopK micro-refresh.
- **CTE Support** — full support for Common Table Expressions. Non-recursive CTEs are inlined and differentiated algebraically. Multi-reference CTEs share delta computation. Recursive CTEs (`WITH RECURSIVE`) work in FULL, DIFFERENTIAL, and IMMEDIATE modes.
- **TopK support** — `ORDER BY ... LIMIT N [OFFSET M]` queries are accepted and refreshed correctly via scoped recomputation. Paged TopK (`OFFSET M`) supports server-side pagination.
- **ALTER QUERY** — change the defining query of an existing stream table online. The engine classifies schema changes (same / compatible / incompatible), migrates the storage table, updates the dependency graph, and runs a full refresh. Compatible changes preserve the storage table OID so views and publications remain valid.
- **Trigger-based CDC** — lightweight `AFTER` row-level triggers capture changes into buffer tables. No logical replication slots or `wal_level = logical` required. Triggers are created and dropped automatically.
- **Hybrid CDC (optional)** — when `wal_level = logical` is available, the system can automatically transition from triggers to WAL-based (logical replication) capture for lower write-side overhead. Controlled by the `pg_trickle.cdc_mode` GUC (`trigger` / `auto` / `wal`).
- **DAG-aware scheduling** — stream tables that depend on other stream tables are refreshed in topological order. `CALCULATED` schedule propagation is supported.
- **Diamond dependency consistency** — diamond-shaped DAGs (A→B→D, A→C→D) can be refreshed atomically to prevent split-version reads.
- **Circular dependency support** — monotone dependency cycles can be enabled explicitly and are refreshed to a fixed point with convergence guardrails and monitoring.
- **Watermark gating** — external loaders can publish per-source watermarks so downstream refreshes wait until related sources are aligned.
- **Multi-database auto-discovery** — a single launcher worker automatically spawns per-database scheduler workers for every database that has the extension installed. No manual per-database configuration needed.
- **PgBouncer compatible** — works behind PgBouncer in transaction-pool mode (the default on Supabase, Railway, Neon, and similar managed platforms). Session locks have been replaced with row-level locking. Per-table `pooler_compatibility_mode` disables prepared statements and NOTIFY for connection-pooler deployments.
- **Tiered scheduling** — assign stream tables to Hot / Warm / Cold / Frozen tiers to control effective refresh rates in large deployments (`pg_trickle.tiered_scheduling = on`).
- **Change buffer compaction** — automatically collapses cancelling INSERT/DELETE pairs and sequential changes to the same row, reducing delta scan overhead 50–90 % for high-churn tables.
- **Crash-safe** — row-level locks prevent concurrent refreshes; crash recovery marks in-flight refreshes as failed and resumes normal operation.
- **Observable** — built-in monitoring views, refresh history, slot health checks, staleness reporting, SCC status, watermark status, `NOTIFY`-based alerting, and dedicated helper functions such as `health_check`, `change_buffer_sizes`, `dependency_tree`, `refresh_timeline`, `trigger_inventory`, `list_sources`, `diamond_groups`, and `pgt_scc_status`.

## SQL Support

Every operator listed here works in `DIFFERENTIAL` mode (incremental delta computation) unless noted otherwise. `FULL` mode always works — it re-runs the entire query on each refresh.

| Category | Feature | Support | Notes |
|---|---|---|---|
| **Core** | Table scan | ✅ Full | |
| **Core** | Projection (`SELECT` expressions) | ✅ Full | |
| **Filtering** | `WHERE` clause | ✅ Full | |
| **Filtering** | `HAVING` clause | ✅ Full | Applied as a filter on top of the aggregate delta |
| **Joins** | `INNER JOIN` | ✅ Full | Equi-joins are optimal; non-equi-joins also work |
| **Joins** | `LEFT OUTER JOIN` | ✅ Full | |
| **Joins** | `RIGHT OUTER JOIN` | ✅ Full | Automatically converted to `LEFT JOIN` with swapped operands |
| **Joins** | `FULL OUTER JOIN` | ✅ Full | 8-part delta; may be slower than inner joins on high-churn data |
| **Joins** | `NATURAL JOIN` | ✅ Full | Resolved at parse time; explicit equi-join synthesized |
| **Joins** | Nested joins (3+ tables) | ✅ Full | `a JOIN b JOIN c` — snapshot subqueries with disambiguated columns |
| **Aggregation** | `GROUP BY` + `COUNT`, `SUM`, `AVG` | ✅ Full | Fully algebraic — no rescan needed |
| **Aggregation** | `GROUP BY` + `MIN`, `MAX` | ✅ Full | Semi-algebraic — `LEAST`/`GREATEST` merge; per-group rescan on extremum deletion |
| **Aggregation** | 30+ other aggregates (`STRING_AGG`, `ARRAY_AGG`, `BOOL_AND/OR`, `JSON[B]_AGG`, `STDDEV`, `VARIANCE`, `MODE`, `PERCENTILE_*`, `CORR`, `COVAR_*`, `REGR_*`, etc.) | ✅ Full | Group-rescan — affected groups re-aggregated from source |
| **Aggregation** | `FILTER (WHERE …)` on aggregates | ✅ Full | Filter predicate applied within delta computation |
| **Deduplication** | `DISTINCT` | ✅ Full | Reference-counted multiplicity tracking |
| **Deduplication** | `DISTINCT ON (…)` | ✅ Full | Auto-rewritten to `ROW_NUMBER()` window subquery |
| **Set operations** | `UNION ALL` | ✅ Full | |
| **Set operations** | `UNION` (deduplicated) | ✅ Full | Composed as `UNION ALL` + `DISTINCT` |
| **Set operations** | `INTERSECT` / `EXCEPT` | ✅ Full | Dual-count multiplicity tracking with LEAST / GREATEST boundary crossing |
| **Subqueries** | Subquery in `FROM` | ✅ Full | `(SELECT …) AS alias` |
| **Subqueries** | `EXISTS` / `NOT EXISTS` in `WHERE` | ✅ Full | Semi-join / anti-join delta operators |
| **Subqueries** | `IN` / `NOT IN` (subquery) in `WHERE` | ✅ Full | Rewritten to semi-join / anti-join |
| **Subqueries** | `ALL` (subquery) in `WHERE` | ✅ Full | Rewritten to anti-join via `NOT EXISTS` |
| **Subqueries** | Scalar subquery in `SELECT` | ✅ Full | `(SELECT max(x) FROM t)` in target list |
| **Subqueries** | Scalar subquery in `WHERE` | ✅ Full | Auto-rewritten to `CROSS JOIN` |
| **CTEs** | Non-recursive `WITH` | ✅ Full | Single & multi-reference; shared delta computation |
| **CTEs** | Recursive `WITH RECURSIVE` | ✅ Full | Both `FULL` and `DIFFERENTIAL` modes (semi-naive + DRed) |
| **Window functions** | `ROW_NUMBER`, `RANK`, `SUM OVER`, etc. | ✅ Full | Partition-based recomputation |
| **Window functions** | Window frame clauses | ✅ Full | `ROWS`, `RANGE`, `GROUPS` with `BETWEEN` bounds and `EXCLUDE` |
| **Window functions** | Named `WINDOW` clauses | ✅ Full | `WINDOW w AS (...)` resolved from query-level window definitions |
| **Window functions** | Multiple `PARTITION BY` clauses | ✅ Full | Same partition key used directly; different keys fall back to full recomputation |
| **LATERAL SRFs** | `jsonb_array_elements`, `unnest`, `jsonb_each`, etc. | ✅ Full | Row-scoped recomputation in DIFFERENTIAL mode |
| **JSON_TABLE** | `JSON_TABLE(expr, path COLUMNS (...))` | ✅ Full | PostgreSQL 17+; modeled as lateral function |
| **Expressions** | `CASE WHEN … THEN … ELSE … END` | ✅ Full | Both searched and simple CASE |
| **Expressions** | `COALESCE`, `NULLIF`, `GREATEST`, `LEAST` | ✅ Full | |
| **Expressions** | `IN (list)`, `BETWEEN`, `IS DISTINCT FROM` | ✅ Full | Including `NOT IN`, `NOT BETWEEN`, `IS NOT DISTINCT FROM` |
| **Expressions** | `IS TRUE/FALSE/UNKNOWN` | ✅ Full | All boolean test variants |
| **Expressions** | `SIMILAR TO`, `ANY(array)`, `ALL(array)` | ✅ Full | |
| **Expressions** | `ARRAY[…]`, `ROW(…)` | ✅ Full | |
| **Expressions** | `CURRENT_DATE`, `CURRENT_TIMESTAMP`, etc. | ✅ Full | All SQL value functions |
| **Expressions** | Array subscript, field access | ✅ Full | `arr[1]`, `(rec).field`, `(data).*` |
| **Grouping** | `GROUPING SETS` / `CUBE` / `ROLLUP` | ✅ Full | Auto-rewritten to `UNION ALL` of `GROUP BY` queries |
| **Safety** | Volatile function/operator detection | ✅ Full | VOLATILE expressions rejected in DIFFERENTIAL and IMMEDIATE; STABLE functions warned |
| **Source tables** | Tables without primary key | ✅ Full | Content-hash row identity via all columns |
| **Source tables** | Views as sources | ✅ Full | Auto-inlined as subqueries; CDC triggers land on base tables |
| **Source tables** | Materialized views | ⚠️ DIFF / ✅ FULL | DIFFERENTIAL requires `pg_trickle.matview_polling = on` (snapshot comparison); FULL always works |
| **TopK** | `ORDER BY ... LIMIT N [OFFSET M]` | ✅ Full | TopK stream tables store only N rows (optionally from position M+1); scoped recomputation via MERGE |
| **Ordering** | `ORDER BY` (without LIMIT) | ⚠️ Ignored | Accepted but silently discarded; storage row order is undefined |
| **Ordering** | `LIMIT` / `OFFSET` (without ORDER BY) | ❌ Rejected | Not supported — stream tables materialize the full result set |

See [docs/DVM_OPERATORS.md](docs/DVM_OPERATORS.md) for the full differentiation rules and CTE tiers.

## Performance

pg_trickle is designed for low-latency, high-throughput incremental view maintenance. Differential refresh processes only changed rows — not the entire base table — yielding significant speedups over full recomputation.

### Differential vs Full Refresh

Benchmarked at 1% change rate (10 cycles, Docker-hosted PostgreSQL 18.3):

| Query Type | Rows | FULL (ms) | DIFFERENTIAL (ms) | Speedup |
|---|---|---|---|---|
| Table scan | 100K | 461 | 40 | **11.6x** |
| Filter (WHERE) | 10K | 26 | 9 | **2.9x** |
| Aggregate (GROUP BY) | 100K | 31 | 52 | 0.6x |
| Join (INNER JOIN) | 100K | 673 | 42 | **15.9x** |
| Join + Aggregate | 100K | 52 | 63 | 0.8x |

Aggregate queries with few output groups (5 regions from 100K rows) are faster with FULL refresh because the aggregate re-scan is cheap. Differential shines on queries that produce many output rows (scans, joins, filtered projections), where FULL must TRUNCATE + re-insert the entire result set. Speedup scales with table size and inversely with change rate — at 50% churn, the two modes converge.

**Tip:** For aggregate queries where FULL is consistently faster, set `refresh_mode = 'full'` explicitly (`ALTER STREAM TABLE ... SET refresh_mode = 'full'`). Alternatively, rely on the adaptive fallback — when the change ratio exceeds `pg_trickle.adaptive_full_threshold` (default: 50%), the engine switches to FULL automatically for that cycle.

### Zero-Change Latency

When no data has changed, the scheduler's per-cycle overhead is minimal:

| Metric | Value |
|---|---|
| Average | 3.2 ms |
| Max | 5.1 ms |
| Target | < 10 ms |

**Tip:** For tables that change rarely, assign them to the **Cold** or **Frozen** tier (`pg_trickle.tiered_scheduling = on`) to reduce polling frequency without removing the stream table. Frozen tables are skipped by the scheduler entirely until manually refreshed or explicitly thawed.

### Where Time Is Spent

Rust-side delta SQL generation takes **< 1%** of total refresh time (sub-microsecond to ~50 µs depending on operator complexity). PostgreSQL's MERGE execution dominates at **70–97%** of wall-clock time — the extension gets out of the way and lets the database do what it does best.

**Tip:** Since MERGE dominates, standard PostgreSQL tuning applies: ensure the stream table has an index on its primary key (created automatically) and that source tables have indexes on any JOIN or WHERE columns in the defining query. Increasing `work_mem` and `maintenance_work_mem` can also reduce sort and hash costs within the MERGE plan.

### DAG Propagation

Changes propagate through multi-level stream table DAGs efficiently:

| Topology | Stream Tables | Propagation Time |
|---|---|---|
| Linear chain (depth 10) | 10 | ~820 ms |
| Wide DAG (3 levels × 20 wide) | 60 | ~2,430 ms |
| Diamond (4-way fan-out + join) | 5 | ~200 ms |

PARALLEL refresh mode processes independent branches concurrently, reducing wall-clock time for wide DAGs.

**Tips:** Enable `PARALLEL` refresh mode (`ALTER STREAM TABLE ... SET refresh_mode = 'parallel'`) to process independent DAG branches concurrently. For deep linear chains (> 5 levels), consider consolidating intermediate steps into a single defining query to reduce propagation hops and transaction overhead.

### IMMEDIATE Mode

For use cases that require read-your-writes consistency, IMMEDIATE mode maintains the stream table **within the same transaction** as the DML — no scheduler, no change buffers, no additional latency. The delta is computed from PostgreSQL's transition tables and applied inline.

### Change Buffer Compaction

High-churn tables benefit from automatic compaction of CDC change buffers, which collapses cancelling INSERT/DELETE pairs and sequential changes to the same row, reducing delta scan overhead by **50–90%**.

**Tip:** Compaction is most effective when refreshes are batched. Increasing `refresh_interval` gives the compactor more time to accumulate and collapse changes between cycles, amplifying the reduction. For workloads with periodic bulk writes (e.g., hourly ETL), align `refresh_interval` with the load cadence so the compactor runs after each batch settles.

### Write-Path Overhead

Incremental view maintenance is not free on the write side. CDC triggers add overhead to every INSERT, UPDATE, and DELETE on source tables. The trade-off is intentional: atomic, transactional change tracking with zero data loss, in exchange for modest write-side cost.

**Per-row trigger overhead: 20–55 µs.** At typical OLTP write rates (< 1,000 writes/sec per source), this adds **< 5%** to DML latency — well below network round-trip time.

**Write amplification** (measured via [E2E CDC overhead benchmarks](docs/BENCHMARK.md)):

| Operation | Write Amplification |
|---|---|
| Single-row INSERT | ~2.0x |
| Bulk INSERT (10K rows) | ~2.1x |
| Bulk UPDATE (10K rows) | ~2.2x |
| Bulk DELETE (10K rows) | ~2.3x |

Several layers reduce this cost automatically:

- **Hybrid CDC** — triggers bootstrap change capture with zero config; when `wal_level = logical` is available, the system transitions to WAL-based capture for lower write-side overhead (~5 µs/row). Controlled by `pg_trickle.cdc_mode` (default: `auto`).
- **Columnar change tracking** — CDC records only the columns referenced by the defining query, using a VARBIT bitmask. UPDATEs that touch only unreferenced columns are skipped entirely, reducing delta volume by **50–90%** for wide tables.
- **Delta predicate pushdown** — WHERE predicates from the defining query are injected into change buffer scans, filtering irrelevant changes at read time (**5–10x** delta volume reduction for selective queries).
- **Event-driven scheduler wake** — CDC triggers emit `pg_notify()` to wake the scheduler immediately instead of polling, reducing propagation latency from ~515 ms to ~15 ms median.
- **Adaptive FULL fallback** — when the change ratio exceeds a threshold (default: 50%), the engine automatically switches to FULL refresh for that cycle, avoiding the case where differential is slower than recomputation.

For write-heavy workloads where trigger overhead is a concern, FULL refresh mode bypasses CDC entirely — no triggers are installed, and each refresh re-executes the full query.

**If overhead is still a concern:**

- **Force WAL-based CDC** — if `wal_level = logical` is confirmed available on your instance, set `pg_trickle.cdc_mode = 'wal'` to bypass trigger overhead entirely, dropping per-row cost from 20–55 µs to ~5 µs.
- **Batch writes** — prefer multi-row `INSERT` or `COPY` over single-row statements. Per-row trigger cost is constant, so batching amortizes it across fewer transactions and reduces change buffer pressure.
- **Narrow the defining query** — referencing fewer source columns lets columnar filtering discard more UPDATE events at capture time. UPDATEs that touch only unreferenced columns are skipped entirely, with no entry written to the change buffer.
- **Increase `refresh_interval`** — less frequent refreshes allow the compactor to collapse more cancelling changes per cycle, reducing the total delta volume the engine must process.
- **Switch to FULL refresh** — `ALTER STREAM TABLE ... SET refresh_mode = 'full'` removes all CDC triggers on the source table. Each refresh re-executes the full query; no change buffers, no trigger overhead.

### TPC-H Validation (22 queries, SF=0.01)

pg_trickle is validated against the full TPC-H benchmark suite — all 22 standard queries across all three refresh modes. The test suite runs 15 test scenarios and passes completely:

| Test | Queries | Result |
|---|---|---|
| Differential correctness | 22/22 | ✅ pass |
| Immediate correctness | 22/22 | ✅ pass |
| Full correctness | 22/22 | ✅ pass |
| Differential == Immediate | 22/22 | ✅ identical results |
| Full == Differential | 22/22 | ✅ identical results |
| Rollback correctness | INSERT / UPDATE / DELETE | ✅ pass |
| Savepoint rollback | INSERT + DELETE | ✅ pass |
| Single-row mutations | 3 queries | ✅ pass |
| Combined-delete regression (q07, q08, q09) | 3 queries | ✅ pass |

**Sustained churn (T1-C, 7 stream tables, 50 cycles, SF=0.01):**

| Metric | Value |
|---|---|
| Avg cycle time | 150.7 ms |
| Min / Max | 115.9 / 537.9 ms |
| Drift detected | 0 |
| Refresh failures | 0 |

**Per-query FULL vs DIFFERENTIAL latency (SF=0.01, T1-B, avg over 3 cycles):**

| Query | Tier | FULL (ms) | DIFF (ms) | Speedup |
|---|---|---|---|---|
| q02 | T1 | 24.6 | 16.9 | 1.45x |
| q21 | T1 | 12.4 | 12.1 | 1.02x |
| q13 | T1 | 9.4 | 18.3 | 0.51x |
| q11 | T1 | 11.0 | 9.0 | 1.22x |
| q08 | T1 | 18.0 | 43.7 | 0.41x |
| q01 | T2 | 13.3 | 19.4 | 0.69x |
| q05 | T2 | 15.0 | 202.5 | 0.07x |
| q07 | T2 | 15.2 | 33.5 | 0.45x |
| q09 | T2 | 15.6 | 129.5 | 0.12x |
| q16 | T2 | 11.5 | 8.6 | 1.33x |
| q22 | T2 | 10.4 | 17.0 | 0.61x |
| q03 | T3 | 8.2 | 7.6 | 1.08x |
| q04 | T3 | 12.0 | 20.5 | 0.58x |
| q06 | T3 | 9.0 | 11.1 | 0.81x |
| q10 | T3 | 9.6 | 8.4 | 1.13x |
| q12 | T3 | 11.0 | 19.6 | 0.56x |
| q14 | T3 | 10.6 | 21.2 | 0.50x |
| q15 | T3 | 14.9 | 18.3 | 0.82x |
| q17 | T3 | 11.5 | 51.4 | 0.22x |
| q18 | T3 | 10.0 | 8.4 | 1.19x |
| q19 | T3 | 12.3 | 29.8 | 0.41x |
| q20 | T3 | 13.4 | 5663.0 | 0.00x |

> **Note:** SF=0.01 is a very small dataset (1,500 orders). At this scale, FULL refresh is competitive because base table scans are cheap. q20 (a correlated subquery) is a known differential outlier — at higher scale factors where table scans dominate, differential speedups grow substantially. The `ADAPTIVE` fallback mode automatically selects FULL when the change ratio exceeds the threshold, covering the q20-class case.

For full benchmark methodology and how to run your own benchmarks, see [docs/BENCHMARK.md](docs/BENCHMARK.md).

## Quick Start

### Prerequisites

- PostgreSQL 18.x
- Rust 1.85+ (edition 2024) with [pgrx](https://github.com/pgcentralfoundation/pgrx) 0.17.x

### Install

```bash
# Build and install the extension
cargo pgrx install --release

# Or package for deployment
cargo pgrx package
```

Add to `postgresql.conf`:

```ini
shared_preload_libraries = 'pg_trickle'
max_worker_processes = 8
```

> **Note:** `wal_level = logical` and `max_replication_slots` are **not** required by default. CDC uses lightweight row-level triggers unless you opt in to WAL-based capture via `pg_trickle.cdc_mode = 'auto'` (see [CONFIGURATION.md](docs/CONFIGURATION.md)).

Restart PostgreSQL, then:

```sql
CREATE EXTENSION pg_trickle;
```

### Kubernetes (CloudNativePG)

pg_trickle is distributed as a minimal OCI extension image for [CloudNativePG Image Volume Extensions](https://cloudnative-pg.io/docs/1.28/imagevolume_extensions/). The image is `scratch`-based (< 10 MB) and contains only the extension files — no PostgreSQL server, no OS.

```bash
docker pull ghcr.io/grove/pg_trickle-ext:0.12.0
```

Deploy with the official CNPG PostgreSQL 18 operand image:

```yaml
# In your Cluster resource
spec:
  imageName: ghcr.io/cloudnative-pg/postgresql:18
  postgresql:
    shared_preload_libraries: [pg_trickle]
    extensions:
      - name: pg-trickle
        image:
          reference: ghcr.io/grove/pg_trickle-ext:0.12.0
```

See [cnpg/cluster-example.yaml](cnpg/cluster-example.yaml) and [cnpg/database-example.yaml](cnpg/database-example.yaml) for complete examples. Requires Kubernetes 1.33+ and CNPG 1.28+.

### Usage

```sql
-- Create base tables
CREATE TABLE orders (
    id    INT PRIMARY KEY,
    region TEXT,
    amount NUMERIC
);

INSERT INTO orders VALUES
    (1, 'US', 100), (2, 'EU', 200),
    (3, 'US', 300), (4, 'APAC', 50);

-- Create a stream table — AUTO mode (default): DIFFERENTIAL when possible, FULL fallback.
-- Schedule defaults to 'calculated' (derived from consumer refresh cycles).
SELECT pgtrickle.create_stream_table(
    'regional_totals',
    'SELECT region, SUM(amount) AS total, COUNT(*) AS cnt
     FROM orders GROUP BY region'
);

-- Explicit IMMEDIATE mode: updated within the same transaction as DML
SELECT pgtrickle.create_stream_table(
    'regional_totals_live',
    'SELECT region, SUM(amount) AS total, COUNT(*) AS cnt
     FROM orders GROUP BY region',
    schedule     => NULL,
    refresh_mode => 'IMMEDIATE'
);

-- Explicit schedule and mode
SELECT pgtrickle.create_stream_table(
    'hourly_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    schedule     => '@hourly',
    refresh_mode => 'FULL'
);

-- Query the stream table like any regular table
SELECT * FROM regional_totals;

-- Manual refresh (scheduler also refreshes automatically)
SELECT pgtrickle.refresh_stream_table('regional_totals');

-- Change the defining query online (ALTER QUERY)
SELECT pgtrickle.alter_stream_table(
    'regional_totals',
    query => 'SELECT region, SUM(amount) AS total, COUNT(*) AS cnt
              FROM orders WHERE active GROUP BY region'
);

-- Check status and overall health
SELECT * FROM pgtrickle.pgt_status();
SELECT * FROM pgtrickle.health_check();  -- OK/WARN/ERROR triage

-- View monitoring stats
SELECT * FROM pgtrickle.pg_stat_stream_tables;
SELECT * FROM pgtrickle.dependency_tree();  -- ASCII DAG view
SELECT * FROM pgtrickle.change_buffer_sizes();  -- CDC buffer health

-- Drop when no longer needed
SELECT pgtrickle.drop_stream_table('regional_totals');
```

## Documentation

| Document | Description |
|---|---|
| [GETTING_STARTED.md](docs/GETTING_STARTED.md) | Hands-on tutorial building an org-chart with stream tables |
| [INSTALL.md](INSTALL.md) | Detailed installation and configuration guide |
| [docs/SQL_REFERENCE.md](docs/SQL_REFERENCE.md) | Complete SQL function reference |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture and data flow |
| [docs/DVM_OPERATORS.md](docs/DVM_OPERATORS.md) | Supported operators and differentiation rules |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | GUC variables and tuning guide |
| [ROADMAP.md](ROADMAP.md) | Release milestones and future plans (current milestone: v0.13.0) |

### Research & Plans

| Document | Description |
|---|---|
| [plans/sql/PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) | Plan for `CREATE MATERIALIZED VIEW ... WITH (pgtrickle.stream)` syntax |
| [plans/sql/REPORT_CUSTOM_SQL_SYNTAX.md](plans/sql/REPORT_CUSTOM_SQL_SYNTAX.md) | Research: PostgreSQL extension syntax mechanisms |

### Deep-Dive Tutorials

| Tutorial | Description |
|---|---|
| [What Happens on INSERT](docs/tutorials/WHAT_HAPPENS_ON_INSERT.md) | Full 7-phase lifecycle of a single INSERT through the pipeline |
| [What Happens on UPDATE](docs/tutorials/WHAT_HAPPENS_ON_UPDATE.md) | D+I split, group key changes, net-effect for multiple UPDATEs |
| [What Happens on DELETE](docs/tutorials/WHAT_HAPPENS_ON_DELETE.md) | Reference counting, group deletion, INSERT+DELETE cancellation |
| [What Happens on TRUNCATE](docs/tutorials/WHAT_HAPPENS_ON_TRUNCATE.md) | Why TRUNCATE bypasses triggers and recovery strategies |

## Known Limitations

The following SQL features are **rejected with clear error messages** explaining the limitation and suggesting rewrites. See [FAQ — Why Are These SQL Features Not Supported?](docs/FAQ.md#why-are-these-sql-features-not-supported) for detailed technical explanations.

| Feature | Reason | Suggested Rewrite |
|---|---|---|
| Materialized views in DIFFERENTIAL | CDC triggers cannot track `REFRESH MATERIALIZED VIEW` | Use the underlying query directly, or use FULL mode |
| `TABLESAMPLE` | Stream tables materialize the complete result set; sampling at define-time is not meaningful | Use `WHERE random() < fraction` in the consuming query |
| Window functions in expressions | Window functions inside `CASE`, `COALESCE`, arithmetic, etc. cannot be differentially maintained | Move window function to a separate column |
| `LIMIT` / `OFFSET` without `ORDER BY` | Stream tables materialize the full result set; arbitrary row subsets are non-deterministic | Use `ORDER BY ... LIMIT N` for TopK, or apply LIMIT when querying the stream table |
| `FOR UPDATE` / `FOR SHARE` | Row-level locking not applicable | Remove the locking clause |

### Stream Table Restrictions

Stream tables are regular PostgreSQL heap tables, but their contents are managed exclusively by the refresh engine. See [FAQ — Why Are These Stream Table Operations Restricted?](docs/FAQ.md#why-are-these-stream-table-operations-restricted) for detailed explanations.

| Operation | Allowed? | Notes |
|---|---|---|
| ST references other STs | ✅ Yes | DAG-ordered refresh; monotone cycles supported via `pg_trickle.allow_circular` |
| Views reference STs | ✅ Yes | Standard PostgreSQL views work normally |
| Materialized views reference STs | ✅ Yes | Requires separate `REFRESH MATERIALIZED VIEW` |
| Logical replication of STs | ✅ Yes | `__pgt_row_id` column is replicated; subscribers receive materialized data only |
| Direct DML on STs | ❌ No | Contents managed by the refresh engine |
| Foreign keys on STs | ❌ No | Bulk `MERGE` during refresh does not respect FK ordering |
| User triggers on STs | ✅ Supported | Supported in DIFFERENTIAL mode; suppressed during FULL refresh (see `pg_trickle.user_triggers` GUC) |

See [SQL Reference — Restrictions & Interoperability](docs/SQL_REFERENCE.md#restrictions--interoperability) for details and examples.

## How It Works

### DIFFERENTIAL / FULL Mode (scheduled)

1. **Create** — `pgtrickle.create_stream_table()` parses the defining query into an operator tree, creates a storage table, installs lightweight CDC triggers on source tables, and registers the ST in the catalog.

2. **Capture** — Changes to base tables are captured via the hybrid CDC layer. By default, `AFTER INSERT/UPDATE/DELETE` row-level triggers write to per-source change buffer tables in the `pgtrickle_changes` schema. With `pg_trickle.cdc_mode = 'auto'`, the system transitions to WAL-based capture (logical replication) after the first successful refresh for lower write-side overhead.

3. **Schedule** — A background worker wakes periodically (default: 1s) and checks which STs have exceeded their schedule (or whose cron schedule has fired). STs are scheduled for refresh in topological order.

4. **Differentiate** — The DVM engine differentiates the defining query's operator tree to produce a delta query (ΔQ) that computes only the changes since the last refresh.

5. **Apply** — The delta query is executed and its results (INSERT/DELETE actions with row IDs) are merged into the storage table.

6. **Version** — Each refresh records a frontier (per-source LSN positions) and a data timestamp, implementing a simplified Data Versioning System (DVS).

### IMMEDIATE Mode (transactional)

When `refresh_mode = 'IMMEDIATE'`, the stream table is maintained **within the same transaction** as the base table DML:

1. **BEFORE triggers** acquire an advisory lock on the stream table.
2. **AFTER triggers** (with `REFERENCING NEW TABLE AS ... OLD TABLE AS ...`) copy transition tables to temp tables.
3. The DVM engine computes the delta from the transition tables (via `DeltaSource::TransitionTable`).
4. The delta is applied to the stream table via DELETE + INSERT ON CONFLICT.

No change buffer tables, no scheduler, no WAL infrastructure. The stream table is always up-to-date within the current transaction.

Supported SQL features include all DIFFERENTIAL-mode constructs: JOINs, aggregates, window functions, LATERAL, scalar subqueries, `WITH RECURSIVE`, TopK, and more. `WITH RECURSIVE` in IMMEDIATE mode uses semi-naive evaluation (INSERT-only) or Delete-and-Rederive (DELETE/UPDATE), bounded by `pg_trickle.ivm_recursive_max_depth` (default 100). TopK stream tables in IMMEDIATE mode use micro-refresh (recompute top-K on every DML), gated by `pg_trickle.ivm_topk_max_limit`.

## Architecture

```
┌─────────────────────────────────────────────┐
│              PostgreSQL 18                  │
│                                             │
│  ┌─────────┐    ┌──────────┐   ┌─────────┐  │
│  │  Source │    │  Stream  │   │pg_trickle│  │
│  │  Tables │───▸│  Tables  │   │ Catalog │  │
│  └────┬────┘    └──────────┘   └─────────┘  │
│       │                                     │
│  ┌────▼─────────────────────────────┐       │
│  │     Hybrid CDC Layer              │       │
│  │     Triggers (default) or WAL     │       │
│  └────┬─────────────────────────────┘       │
│       │                                     │
│  ┌────▼─────────────────────────────┐       │
│  │     Change Buffer Tables         │       │
│  │     pgtrickle_changes schema      │       │
│  └────┬─────────────────────────────┘       │
│       │                                     │
│  ┌────▼─────────────────────────────┐       │
│  │     DVM Engine                   │       │
│  │  ┌────────┐ ┌─────────────────┐  │       │
│  │  │ Parser │ │ Differentiation │  │       │
│  │  │ OpTree │ │  Delta Query ΔQ │  │       │
│  │  └────────┘ └─────────────────┘  │       │
│  └────┬─────────────────────────────┘       │
│       │                                     │
│  ┌────▼─────────────────────────────┐       │
│  │     Refresh Executor             │       │
│  │  Full / Differential / Immediate │       │
│  └────┬─────────────────────────────┘       │
│       │                                     │
│  ┌────▼─────────────────────────────┐       │
│  │     Background Scheduler         │       │
│  │     DAG-aware, schedule-based    │       │
│  └──────────────────────────────────┘       │
└─────────────────────────────────────────────┘
```

## Testing

```bash
just test-unit           # Pure Rust unit tests (no Docker needed)
just test-integration    # Testcontainers-based integration tests
just test-light-e2e      # Light E2E (stock postgres container, fast)
just test-e2e            # Full E2E (custom Docker image)
just test-all            # All of the above + pgrx tests
just bench               # Criterion benchmarks
```

**Test counts:** ~1,660 unit tests + integration tests + ~1,130 E2E tests.

### Code Coverage

Generate coverage reports for unit tests using [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov):

```bash
# Full report (HTML + LCOV)
just coverage

# Or use the script directly
./scripts/coverage.sh

# LCOV only (for CI / Codecov upload)
./scripts/coverage.sh --lcov

# Quick terminal summary
./scripts/coverage.sh --text
```

Reports are written to `coverage/`:
- `coverage/html/index.html` — browsable HTML report
- `coverage/lcov.info` — LCOV data for upload to [Codecov](https://codecov.io)

Coverage is automatically collected and uploaded to Codecov on every push to `main` and on pull requests via the **Coverage** GitHub Actions workflow.

## Contributors

- [Geir O. Grønmo](https://github.com/grove)
- [Baard H. Rehn Johansen](https://github.com/BaardBouvet)
- [GitHub Copilot](https://github.com/features/copilot) (AI pair programmer)

## License

[Apache License 2.0](LICENSE)
