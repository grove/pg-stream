# pg_trickle — Self-Refreshing Materialized Views for PostgreSQL

## What Is pg_trickle?

pg_trickle is a PostgreSQL extension that keeps your summary tables and
materialized views automatically up to date — without rebuilding them from
scratch every time something changes.

In plain PostgreSQL, refreshing a materialized view re-runs the entire query,
even if only one row changed out of millions. pg_trickle solves this by
tracking what changed and updating only the affected rows. One insert into a
million-row table? pg_trickle touches exactly one row's worth of computation.

No external services, no sidecars, no message queues. Just install the
extension and your views stay fresh.

## Why Use It?

**The problem:** Teams using PostgreSQL materialized views face a painful
trade-off — burn CPU on constant full refreshes, or live with stale data. Most
end up building custom refresh pipelines just to keep dashboards and reports
current.

**The pg_trickle approach:** You define a "stream table" with a SQL query and a
schedule. The extension captures changes to your source tables and, on each
refresh cycle, computes only what changed and merges the result. You get fresh
data with a fraction of the computation.

## How It Works (The Simple Version)

1. **You define a query and a schedule** — "keep this summary updated every
   30 seconds"
2. **pg_trickle watches your source tables** — lightweight change tracking
   captures inserts, updates, and deletes automatically
3. **Only changes are processed** — instead of re-running the full query,
   a smart "delta query" processes just the new/changed/deleted rows
4. **Your stream table stays current** — the results are merged into a real
   PostgreSQL table you can query like any other

## Key Capabilities

### Broad SQL Support

pg_trickle supports the SQL you already write:

- **JOINs** — inner, left, right, full outer, natural, nested multi-table
- **Aggregations** — COUNT, SUM, AVG, MIN, MAX, STRING_AGG, ARRAY_AGG,
  JSON_AGG, statistical aggregates, and more
- **Set operations** — UNION, INTERSECT, EXCEPT
- **Subqueries** — in FROM, WHERE, SELECT; EXISTS, IN, NOT IN, scalar subqueries
- **Common Table Expressions** — including `WITH RECURSIVE`
- **Window functions** — ROW_NUMBER, RANK, SUM OVER, etc.
- **LATERAL joins and JSON functions** — jsonb_array_elements, unnest, JSON_TABLE
- **TopK queries** — ORDER BY ... LIMIT N with efficient scoped recomputation
- **DISTINCT and DISTINCT ON**
- **GROUPING SETS, CUBE, ROLLUP**

### Four Refresh Modes

| Mode | How it works |
|------|--------------|
| **AUTO** (default) | Smart default — uses incremental delta when possible, falls back to full recomputation |
| **DIFFERENTIAL** | Processes only changed rows — the fastest option for supported queries |
| **FULL** | Complete recomputation each cycle — works with any query |
| **IMMEDIATE** | Updates the stream table *within the same transaction* as your INSERT/UPDATE/DELETE — read-your-writes consistency |

### Smart Scheduling

Stream tables can depend on each other, forming a pipeline. pg_trickle
understands these dependencies and refreshes them in the right order. You only
set an explicit schedule on the tables your application actually reads — the
system propagates timing requirements upward through the dependency chain
automatically.

Supports fixed intervals (e.g. every 30 seconds), cron expressions (e.g.
`@hourly`), and automatic demand-driven scheduling.

### Zero External Infrastructure

pg_trickle runs entirely inside PostgreSQL:

- **No WAL configuration required** — change tracking uses lightweight
  row-level triggers by default
- **No logical replication slots** — works out of the box on any PostgreSQL 18
  installation
- **Optional WAL-based capture** — when `wal_level = logical` is available,
  the system can automatically transition to WAL-based capture for even lower
  overhead. The transition is seamless and reversible.

### Built for Observability

- Monitoring views showing refresh history, timing, and row counts
- Health check function with OK/WARN/ERROR triage
- Dependency tree visualization
- Change buffer size monitoring
- NOTIFY-based alerting when refreshes complete or fail
- Staleness reporting

## Quick Start

```sql
-- 1. Enable the extension
CREATE EXTENSION pg_trickle;

-- 2. Create your source table
CREATE TABLE orders (
    id     INT PRIMARY KEY,
    region TEXT,
    amount NUMERIC
);

-- 3. Create a stream table — it stays up to date automatically
SELECT pgtrickle.create_stream_table(
    'regional_totals',
    'SELECT region, SUM(amount) AS total, COUNT(*) AS cnt
     FROM orders GROUP BY region'
);

-- 4. Query it like any regular table
SELECT * FROM regional_totals;

-- 5. Insert data into your source table...
INSERT INTO orders VALUES (1, 'US', 100), (2, 'EU', 200);

-- 6. After the next refresh cycle, regional_totals reflects the changes
SELECT * FROM regional_totals;

-- For instant updates within the same transaction:
SELECT pgtrickle.create_stream_table(
    'regional_totals_live',
    'SELECT region, SUM(amount) AS total, COUNT(*) AS cnt
     FROM orders GROUP BY region',
    refresh_mode => 'IMMEDIATE'
);
```

## Installation

### Prerequisites

- PostgreSQL 18.x
- Rust 1.82+ with [pgrx](https://github.com/pgcentralfoundation/pgrx) 0.17.x

### Build and Install

```bash
cargo pgrx install --release --pg-config $(pg_config --bindir)/pg_config
```

Add to `postgresql.conf`:

```ini
shared_preload_libraries = 'pg_trickle'
max_worker_processes = 8
```

Restart PostgreSQL, then run `CREATE EXTENSION pg_trickle;` in your database.

### Kubernetes (CloudNativePG)

pg_trickle is available as an OCI extension image for CloudNativePG:

```bash
docker pull ghcr.io/grove/pg_trickle-ext:0.10.0
```

### Docker Hub

Pre-built Docker images with PostgreSQL 18 and pg_trickle are available:

```bash
docker pull grove/pg_trickle:latest
```

## Additional Features

- **ALTER QUERY** — change the defining query of a stream table online without
  dropping and recreating it
- **Diamond dependency consistency** — pipelines with shared upstream sources
  are refreshed atomically to prevent inconsistent reads
- **Circular dependency support** — monotone cycles can be refreshed to a fixed
  point with convergence guardrails
- **Watermark gating** — external data loaders can publish watermarks so
  downstream refreshes wait until sources are aligned
- **PgBouncer compatible** — works behind connection poolers in transaction-pool
  mode (Supabase, Railway, Neon, and similar managed platforms)
- **Crash-safe** — row-level locks prevent concurrent refreshes; crash recovery
  resumes normal operation automatically
- **dbt integration** — the `dbt-pgtrickle` package lets you manage stream
  tables as dbt models

## Technical Foundation

pg_trickle is grounded in the
[DBSP](https://arxiv.org/abs/2203.16684) differential dataflow framework
(Budiu et al., 2022). Delta queries are derived automatically from the SQL
operator tree: joins produce the classic bilinear expansion, aggregates maintain
auxiliary counters, and linear operators like filters pass deltas through
unchanged.

Written in Rust using [pgrx](https://github.com/pgcentralfoundation/pgrx).
Apache 2.0 licensed. 1,100+ unit tests and 800+ end-to-end tests.

## Links

- **Source code:** https://github.com/grove/pg-trickle
- **Documentation:** https://grove.github.io/pg-trickle/
- **Issue tracker:** https://github.com/grove/pg-trickle/issues
- **Getting started tutorial:** https://github.com/grove/pg-trickle/blob/main/docs/GETTING_STARTED.md
- **SQL reference:** https://github.com/grove/pg-trickle/blob/main/docs/SQL_REFERENCE.md
- **Configuration guide:** https://github.com/grove/pg-trickle/blob/main/docs/CONFIGURATION.md
- **Roadmap:** https://github.com/grove/pg-trickle/blob/main/ROADMAP.md
