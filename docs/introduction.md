# pg_stream

**pg_stream** is a PostgreSQL 18 extension that turns ordinary SQL views into
self-maintaining stream tables — no external processes, no sidecars, no
bespoke refresh pipelines. Just `CREATE EXTENSION pg_stream` and your views
stay fresh.

```sql
-- Declare a stream table — a view that maintains itself
SELECT pgstream.create_stream_table(
    'active_orders',
    'SELECT * FROM orders WHERE status = ''active''',
    '30s'
);

-- Insert a row — the stream table updates automatically on the next refresh
INSERT INTO orders (id, status) VALUES (42, 'active');
SELECT count(*) FROM active_orders;  -- 1
```

## The problem with materialized views

PostgreSQL's materialized views are powerful but frustrating.
`REFRESH MATERIALIZED VIEW` re-runs the entire query from scratch, even if
only one row changed in a million-row table. Your choices are: burn CPU on
full recomputation, or accept stale data. Most teams end up building bespoke
refresh pipelines just to keep summary tables current.

## What pg_stream does differently

pg_stream captures changes to your source tables and — on each refresh cycle
— derives a *delta query* that processes only the changed rows and merges the
result into the materialized table. One insert into a million-row source table?
pg_stream touches exactly one row's worth of computation.

The approach is grounded in the [DBSP differential dataflow framework](https://arxiv.org/abs/2203.16684)
(Budiu et al., 2022). Delta queries are derived automatically from your SQL's
operator tree: joins produce the classic bilinear expansion, aggregates
maintain auxiliary counters, and linear operators like filters pass deltas
through unchanged.

## Key capabilities

| Feature | Description |
|---------|-------------|
| **Incremental refresh** | Only changed rows are recomputed — never a full table scan |
| **Cascading DAG** | Stream tables that depend on stream tables propagate deltas downstream automatically |
| **Demand-driven scheduling** | Set a freshness interval on the views your app queries; upstream layers inherit the tightest schedule automatically |
| **Hybrid CDC** | Starts with lightweight row-level triggers; seamlessly transitions to WAL-based logical replication once available |
| **Broad SQL support** | JOINs, GROUP BY, DISTINCT, UNION/INTERSECT/EXCEPT, subqueries, CTEs (including WITH RECURSIVE), window functions, LATERAL, and more |
| **Built-in observability** | Monitoring views, refresh history, NOTIFY-based alerting |
| **CloudNativePG-ready** | Ships as a Docker image for Kubernetes deployments |

## Demand-driven scheduling

With the default `CALCULATED` schedule mode, you only set an explicit refresh
interval on the stream tables your application actually queries. The system
propagates that cadence upward through the dependency graph: each upstream
stream table inherits the tightest schedule among its downstream dependents.
You declare freshness requirements where they matter — at the consumer — and
the entire pipeline adjusts without manual coordination.

## Hybrid change capture

pg_stream bootstraps with lightweight row-level triggers — no configuration
needed, works out of the box. Once the first refresh succeeds and
`wal_level = logical` is available, the system automatically transitions to
WAL-based logical replication for lower write-side overhead. The transition
is seamless: `trigger → transitioning → WAL-only`. If anything goes wrong, it
falls back to triggers.

---

## Explore this documentation

- **[Getting Started](GETTING_STARTED.md)** — build a three-layer DAG from scratch in minutes
- **[SQL Reference](SQL_REFERENCE.md)** — every function and option
- **[Architecture](ARCHITECTURE.md)** — how the engine works internally
- **[Configuration](CONFIGURATION.md)** — GUC variables and tuning

---

## Source & releases

Written in Rust using [pgrx](https://github.com/pgcentralfoundation/pgrx).
Targets PostgreSQL 18. Apache 2.0 licensed.

- Repository: [github.com/grove/pg-stream](https://github.com/grove/pg-stream)
- Install instructions: [Installation](installation.md)
- Changelog: [Changelog](changelog.md)
- Roadmap: [Roadmap](roadmap.md)
