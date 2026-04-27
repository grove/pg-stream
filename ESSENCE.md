# What is pg_trickle?

pg_trickle is a PostgreSQL 18 extension that adds **stream tables** —
tables that are defined by a SQL query and stay up to date
automatically as the underlying data changes. No external process,
no streaming engine, no pipeline to operate. Just install the
extension and write SQL.

If you have ever wished `CREATE MATERIALIZED VIEW` would just *keep
itself fresh*, this is that.

---

## The problem

PostgreSQL's materialized views are powerful but frustrating.
`REFRESH MATERIALIZED VIEW` re-runs the entire query from scratch,
even if only one row changed in a million-row table. Your choices
are:

- **Burn CPU on full recomputation**, on a schedule, and hope you
  refresh often enough.
- **Accept stale data**, and try to explain that to the dashboard
  user.
- **Build a bespoke refresh pipeline** — Debezium, Kafka Connect, a
  streaming engine, a separate read database. Now you have *two*
  systems to operate.

Most teams pick option 3 and end up maintaining infrastructure that
is more complex than the application it supports.

---

## What pg_trickle does instead

You declare a stream table with a SQL query and a schedule:

```sql
SELECT pgtrickle.create_stream_table(
    name     => 'revenue_by_region',
    query    => $$
        SELECT c.region,
               COUNT(*)                  AS order_count,
               SUM(o.quantity * p.price) AS total_revenue
        FROM orders   o
        JOIN customers c ON c.id = o.customer_id
        JOIN products  p ON p.id = o.product_id
        GROUP BY c.region
    $$,
    schedule => '1s'
);

-- Read it like any table — always fresh.
SELECT * FROM revenue_by_region ORDER BY total_revenue DESC;
```

Behind the scenes:

1. pg_trickle parses your query into an *operator tree* (scans,
   joins, aggregates).
2. It captures every `INSERT`, `UPDATE`, and `DELETE` on the source
   tables — by default with lightweight row-level triggers, with no
   replication slots required.
3. On each refresh cycle (every `1s`, in the example above) it
   derives a *delta query* from the operator tree — the SQL that
   computes only the change since the last refresh — and merges the
   result into the stream table.

When you insert one row into a million-row source table, pg_trickle
processes one row's worth of computation. Not a million.

---

## Why people care

A few things make this materially different from "just another IVM
project":

**No external infrastructure.** No Kafka, no Flink, no Debezium, no
sidecar. The extension lives inside PostgreSQL and uses only its
built-in mechanisms.

**Stream tables can depend on stream tables.** A single write to a
base table can ripple through a graph of derived tables, each
refreshed in the right order, each doing only the work proportional
to what actually changed.

**Demand-driven scheduling.** With the default `CALCULATED` schedule
mode, you only set a refresh interval on the *consumer-facing*
stream tables — the ones your application actually reads. Upstream
stream tables inherit the tightest cadence among their downstream
dependents. You declare freshness where it matters; the system
propagates it everywhere else.

**Hybrid change capture.** It bootstraps with row-level triggers,
which always work. If `wal_level = logical` is available, it
transitions automatically to WAL-based capture for near-zero
write-side overhead. The transition is seamless. If anything goes
wrong, it falls back to triggers.

**Broad SQL coverage.** Joins (inner, left, right, full outer,
lateral), `GROUP BY` with 30+ aggregates, `DISTINCT`,
`UNION`/`INTERSECT`/`EXCEPT`, subqueries (`EXISTS`, `IN`, scalar),
CTEs including `WITH RECURSIVE`, window functions, and most of
standard SQL. See [docs/SQL_REFERENCE.md](docs/SQL_REFERENCE.md) for
the complete matrix.

**Inside the same transaction, if you want it.** `IMMEDIATE` refresh
mode maintains the stream table inside the same transaction as the
source DML, giving read-your-writes consistency without any
background worker.

---

## Where the design comes from

The mathematical foundation is the
[DBSP differential dataflow framework](https://arxiv.org/abs/2203.16684)
(Budiu et al., 2022). Delta queries are derived automatically from
your SQL's operator tree:

- joins produce the classic bilinear expansion,
- aggregates maintain auxiliary counters,
- linear operators like filters and projections pass deltas
  through unchanged.

You do not need to know any of this to use the extension; the rules
are baked in. If you are curious, the
[DVM Operators](docs/DVM_OPERATORS.md) reference and the
[DBSP Comparison](docs/research/DBSP_COMPARISON.md) explain how
pg_trickle relates to the original.

---

## Performance, briefly

Performance is a primary goal. Maximum throughput, low latency, and
minimal overhead drive every design decision. Differential refresh
is the default; full refresh is a fallback of last resort.

A few headline numbers from the
[TPC-H validation](README.md#tpc-h-validation-22-queries-sf001):

- All 22 standard analytical queries pass in DIFFERENTIAL,
  IMMEDIATE, and FULL modes, with identical results across modes.
- 5–90× measured speedup over FULL across the suite at 1% change
  rate.
- The bottleneck is PostgreSQL's own `MERGE`, not pg_trickle's
  pipeline.

For workloads with high write volume,
[hybrid CDC](docs/CDC_MODES.md) and column-level change tracking
keep the write-side overhead low (sub-microsecond per row in WAL
mode).

---

## What's it built on

- **Language:** Rust, using [pgrx](https://github.com/pgcentralfoundation/pgrx) 0.18.
- **Targets:** PostgreSQL 18.
- **License:** Apache 2.0.
- **Status:** active development, approaching 1.0. APIs may still
  change between minor versions; see [ROADMAP.md](ROADMAP.md).
- **Tests:** thousands of unit, integration, and end-to-end tests.
  TPC-H 22/22 in all modes.

---

## Try it

The fastest paths from zero to a working stream table:

```bash
# Option 1 — playground (sample data, dashboards, TUI ready)
cd playground && docker compose up -d

# Option 2 — minimal Docker image
docker run --rm -e POSTGRES_PASSWORD=secret -p 5432:5432 \
  ghcr.io/grove/pg_trickle:latest
```

Then read **[the 5-Minute Quickstart](docs/QUICKSTART_5MIN.md)**.

---

## Where to go next

| Audience | Start here |
|---|---|
| **Curious / evaluator** | [Use Cases](docs/USE_CASES.md) · [Comparisons](docs/COMPARISONS.md) |
| **Application developer** | [Quickstart (5 min)](docs/QUICKSTART_5MIN.md) → [Tutorial (in depth)](docs/GETTING_STARTED.md) → [Patterns](docs/PATTERNS.md) |
| **DBA / SRE** | [Pre-Deployment Checklist](docs/PRE_DEPLOYMENT.md) → [Configuration](docs/CONFIGURATION.md) → [Troubleshooting](docs/TROUBLESHOOTING.md) |
| **Data / analytics engineer** | [Use Cases](docs/USE_CASES.md) → [dbt integration](docs/integrations/dbt.md) → [Migrating from materialized views](docs/tutorials/MIGRATING_FROM_MATERIALIZED_VIEWS.md) |

Source: <https://github.com/grove/pg-trickle>

> **Note on terminology.** A few terms are used throughout the
> docs without further definition: *stream table*, *differential*,
> *delta query*, *DAG*, *frontier*. The
> [Glossary](docs/GLOSSARY.md) defines all of them.
