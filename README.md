# pg_stream

[![Build](https://github.com/grove/pg-stream/actions/workflows/build.yml/badge.svg)](https://github.com/grove/pg-stream/actions/workflows/build.yml)
[![CI](https://github.com/grove/pg-stream/actions/workflows/ci.yml/badge.svg)](https://github.com/grove/pg-stream/actions/workflows/ci.yml)
[![Release](https://github.com/grove/pg-stream/actions/workflows/release.yml/badge.svg)](https://github.com/grove/pg-stream/actions/workflows/release.yml)
[![Coverage](https://codecov.io/gh/grove/pg-stream/branch/main/graph/badge.svg)](https://codecov.io/gh/grove/pg-stream)
[![Benchmarks](https://img.shields.io/badge/Benchmarks-view-blue)](docs/BENCHMARK.md)
[![Roadmap](https://img.shields.io/badge/Roadmap-view-informational)](ROADMAP.md)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![PostgreSQL 18](https://img.shields.io/badge/PostgreSQL-18-blue?logo=postgresql&logoColor=white)](https://www.postgresql.org/)
[![pgrx 0.17](https://img.shields.io/badge/pgrx-0.17-orange)](https://github.com/pgcentralfoundation/pgrx)

> **Early Release Notice:** This project is in an early stage of development and is **not yet production ready**. APIs, configuration options, and internal behavior may change without notice. Use at your own risk and please report any issues you encounter.

> For a plain-language description of the problem pg_stream solves, the differential dataflow approach, and the hybrid CDC architecture, read **[ESSENCE.md](ESSENCE.md)**.

**Stream Tables for PostgreSQL 18**

pg_stream brings declarative, automatically-refreshing materialized views to PostgreSQL, inspired by the [DBSP](https://arxiv.org/abs/2203.16684) differential dataflow framework ([comparison](docs/research/DBSP_COMPARISON.md)). Define a SQL query and a schedule bound (or cron schedule); the extension handles the rest.

## Key Features

- **Declarative** — define a query and a schedule bound (or cron expression); the extension schedules and executes refreshes automatically.
- **Differential View Maintenance (DVM)** — only processes changed rows, not the entire base table. Delta queries are derived automatically from the defining query's operator tree.
- **CTE Support** — full support for Common Table Expressions. Non-recursive CTEs are inlined and differentiated algebraically. Multi-reference CTEs share delta computation. Recursive CTEs (`WITH RECURSIVE`) work in both FULL and DIFFERENTIAL modes.
- **Trigger-based CDC** — lightweight `AFTER` row-level triggers capture changes into buffer tables. No logical replication slots or `wal_level = logical` required. Triggers are created and dropped automatically.
- **Hybrid CDC (optional)** — when `wal_level = logical` is available, the system can automatically transition from triggers to WAL-based (logical replication) capture for lower write-side overhead. Controlled by the `pg_stream.cdc_mode` GUC (`trigger` / `auto` / `wal`).
- **DAG-aware scheduling** — stream tables that depend on other stream tables are refreshed in topological order. `CALCULATED` schedule propagation is supported.
- **Crash-safe** — advisory locks prevent concurrent refreshes; crash recovery marks in-flight refreshes as failed and resumes normal operation.
- **Observable** — built-in monitoring views (`pgstream.pg_stat_stream_tables`), refresh history, slot health checks, staleness reporting, and `NOTIFY`-based alerting.

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
| **Aggregation** | `GROUP BY` + `MIN`, `MAX` | ✅ Full | Semi-algebraic — uses `LEAST`/`GREATEST` merge; per-group rescan when extremum is deleted |
| **Aggregation** | `GROUP BY` + `BOOL_AND`, `BOOL_OR` | ✅ Full | Group-rescan — affected groups are re-aggregated from source |
| **Aggregation** | `GROUP BY` + `STRING_AGG`, `ARRAY_AGG` | ✅ Full | Group-rescan — affected groups are re-aggregated from source |
| **Aggregation** | `GROUP BY` + `JSON_AGG`, `JSONB_AGG` | ✅ Full | Group-rescan — affected groups are re-aggregated from source |
| **Aggregation** | `GROUP BY` + `BIT_AND`, `BIT_OR`, `BIT_XOR` | ✅ Full | Group-rescan — affected groups are re-aggregated from source |
| **Aggregation** | `GROUP BY` + `JSON_OBJECT_AGG`, `JSONB_OBJECT_AGG` | ✅ Full | Group-rescan — affected groups are re-aggregated from source |
| **Aggregation** | `GROUP BY` + `STDDEV`, `STDDEV_POP`, `STDDEV_SAMP` | ✅ Full | Group-rescan — affected groups are re-aggregated from source |
| **Aggregation** | `GROUP BY` + `VARIANCE`, `VAR_POP`, `VAR_SAMP` | ✅ Full | Group-rescan — affected groups are re-aggregated from source |
| **Aggregation** | `MODE() WITHIN GROUP (ORDER BY …)` | ✅ Full | Ordered-set aggregate — group-rescan strategy |
| **Aggregation** | `PERCENTILE_CONT` / `PERCENTILE_DISC` `WITHIN GROUP (ORDER BY …)` | ✅ Full | Ordered-set aggregate — group-rescan strategy |
| **Aggregation** | `CORR`, `COVAR_*`, `REGR_*` (12 regression aggregates) | ✅ Full | Group-rescan strategy |
| **Aggregation** | `JSON_ARRAYAGG`, `JSON_OBJECTAGG` (SQL standard) | ✅ Full | Group-rescan strategy; PostgreSQL 16+ |
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
| **Window functions** | Multiple `PARTITION BY` clauses | ✅ Full | Auto-split into joined subqueries |
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
| **Safety** | Volatile function/operator detection | ✅ Full | Rejected in DIFFERENTIAL; warned for STABLE functions |
| **Source tables** | Tables without primary key | ✅ Full | Content-hash row identity via all columns |
| **Source tables** | Views as sources | ✅ Full | Auto-inlined as subqueries; CDC triggers land on base tables |
| **Source tables** | Materialized views | ❌ DIFF / ✅ FULL | Rejected in DIFFERENTIAL (stale snapshot); allowed in FULL |
| **Ordering** | `ORDER BY` | ⚠️ Ignored | Accepted but silently discarded; storage row order is undefined |
| **Ordering** | `LIMIT` / `OFFSET` | ❌ Rejected | Not supported — stream tables materialize the full result set |

See [docs/DVM_OPERATORS.md](docs/DVM_OPERATORS.md) for the full differentiation rules and CTE tiers.

## Quick Start

### Prerequisites

- PostgreSQL 18.x
- Rust 1.82+ with [pgrx](https://github.com/pgcentralfoundation/pgrx) 0.17.x

### Install

```bash
# Build and install the extension
cargo pgrx install --release --pg-config $(pg_config --bindir)/pg_config

# Or package for deployment
cargo pgrx package --pg-config $(pg_config --bindir)/pg_config
```

Add to `postgresql.conf`:

```ini
shared_preload_libraries = 'pg_stream'
max_worker_processes = 8
```

> **Note:** `wal_level = logical` and `max_replication_slots` are **not** required by default. CDC uses lightweight row-level triggers unless you opt in to WAL-based capture via `pg_stream.cdc_mode = 'auto'` (see [CONFIGURATION.md](docs/CONFIGURATION.md)).

Restart PostgreSQL, then:

```sql
CREATE EXTENSION pg_stream;
```

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

-- Create a stream table with 1-minute schedule
SELECT pgstream.create_stream_table(
    'regional_totals',
    'SELECT region, SUM(amount) AS total, COUNT(*) AS cnt
     FROM orders GROUP BY region',
    '1m',
    'DIFFERENTIAL'
);

-- Or use a cron schedule (e.g., every hour)
SELECT pgstream.create_stream_table(
    'hourly_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    '@hourly',
    'FULL'
);

-- Query the stream table like any regular table
SELECT * FROM regional_totals;

-- Manual refresh (scheduler also refreshes automatically)
SELECT pgstream.refresh_stream_table('regional_totals');

-- Check status
SELECT * FROM pgstream.pgs_status();

-- View monitoring stats
SELECT * FROM pgstream.pg_stat_stream_tables;

-- Drop when no longer needed
SELECT pgstream.drop_stream_table('regional_totals');
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
| [ROADMAP.md](ROADMAP.md) | Release milestones and future plans (v0.2.0 → v1.0.0 → post-1.0) |

### Research & Plans

| Document | Description |
|---|---|
| [plans/sql/PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) | Plan for `CREATE MATERIALIZED VIEW ... WITH (pgstream.stream)` syntax |
| [docs/research/CUSTOM_SQL_SYNTAX.md](docs/research/CUSTOM_SQL_SYNTAX.md) | Research: PostgreSQL extension syntax mechanisms |

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
| `LIMIT` / `OFFSET` | Stream tables materialize the full result set | Apply when querying the stream table |
| `FOR UPDATE` / `FOR SHARE` | Row-level locking not applicable | Remove the locking clause |

### Stream Table Restrictions

Stream tables are regular PostgreSQL heap tables, but their contents are managed exclusively by the refresh engine. See [FAQ — Why Are These Stream Table Operations Restricted?](docs/FAQ.md#why-are-these-stream-table-operations-restricted) for detailed explanations.

| Operation | Allowed? | Notes |
|---|---|---|
| ST references other STs | ✅ Yes | DAG-ordered refresh; cycles are rejected |
| Views reference STs | ✅ Yes | Standard PostgreSQL views work normally |
| Materialized views reference STs | ✅ Yes | Requires separate `REFRESH MATERIALIZED VIEW` |
| Logical replication of STs | ✅ Yes | `__pgs_row_id` column is replicated; subscribers receive materialized data only |
| Direct DML on STs | ❌ No | Contents managed by the refresh engine |
| Foreign keys on STs | ❌ No | Bulk `MERGE` during refresh does not respect FK ordering |
| User triggers on STs | ✅ Supported | Supported in DIFFERENTIAL mode; suppressed during FULL refresh (see `pg_stream.user_triggers` GUC) |

See [SQL Reference — Restrictions & Interoperability](docs/SQL_REFERENCE.md#restrictions--interoperability) for details and examples.

## How It Works

1. **Create** — `pgstream.create_stream_table()` parses the defining query into an operator tree, creates a storage table, installs lightweight CDC triggers on source tables, and registers the ST in the catalog.

2. **Capture** — Changes to base tables are captured via the hybrid CDC layer. By default, `AFTER INSERT/UPDATE/DELETE` row-level triggers write to per-source change buffer tables in the `pgstream_changes` schema. With `pg_stream.cdc_mode = 'auto'`, the system transitions to WAL-based capture (logical replication) after the first successful refresh for lower write-side overhead.

3. **Schedule** — A background worker wakes periodically (default: 1s) and checks which STs have exceeded their schedule (or whose cron schedule has fired). STs are scheduled for refresh in topological order.

4. **Differentiate** — The DVM engine differentiates the defining query's operator tree to produce a delta query (ΔQ) that computes only the changes since the last refresh.

5. **Apply** — The delta query is executed and its results (INSERT/DELETE actions with row IDs) are merged into the storage table.

6. **Version** — Each refresh records a frontier (per-source LSN positions) and a data timestamp, implementing a simplified Data Versioning System (DVS).

## Architecture

```
┌─────────────────────────────────────────────┐
│              PostgreSQL 18                  │
│                                             │
│  ┌─────────┐    ┌──────────┐   ┌─────────┐  │
│  │  Source │    │  Stream  │   │pg_stream│  │
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
│  │     pgstream_changes schema      │       │
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
│  │     Full / Differential          │       │
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
# Unit tests (no database required)
cargo test --lib

# Integration tests (requires Docker for Testcontainers)
cargo test --test '*'

# Property-based tests
cargo test --test property_tests

# All tests
cargo test

# Benchmarks
cargo bench
```

**Test counts:** ~910 unit tests + 74 integration tests + 23 E2E test suites (~384 E2E tests), 0 failures.

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
