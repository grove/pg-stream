# pg_stream – Incremental View Maintenance for PostgreSQL, No External Infrastructure Required

We've been building pg_stream, an open-source PostgreSQL extension that brings automatic, incremental materialized view refreshes directly inside the database. No external processes, no sidecars — just `CREATE EXTENSION pg_stream` and your views stay fresh.

**The problem:** PostgreSQL's materialized views are powerful but frustrating. `REFRESH MATERIALIZED VIEW` re-runs the entire query from scratch, even if only one row changed in a million-row table. Your choices are: burn CPU on full recomputation, or accept stale data. Most teams end up building bespoke refresh pipelines just to keep summary tables current.

**What pg_stream does differently:** You define a "stream table" with a SQL query and a schedule. The extension captures changes to your source tables and, on each refresh cycle, derives a *delta query* that processes only the changed rows and merges the result into the materialized table. One insert into a million-row source table? pg_stream touches exactly one row's worth of computation.

**Demand-driven scheduling:** Stream tables can depend on base tables or on other stream tables, forming a dependency graph. With the default `CALCULATED` schedule mode, you only set an explicit refresh interval on the leaf-level stream tables your application actually queries — the ones that matter to your users. The system propagates that cadence upward through the DAG automatically: each upstream stream table inherits the tightest schedule among its downstream dependents. You declare freshness requirements where they matter — at the consumer — and the entire pipeline adjusts. No manual coordination of refresh ordering or timing across layers of dependent views.

**Hybrid change capture:** pg_stream bootstraps with lightweight row-level triggers — no configuration needed, works out of the box. Once the first refresh succeeds and `wal_level = logical` is available, the system automatically transitions to WAL-based logical replication for lower write-side overhead (~2–15 μs per row eliminated). The transition is seamless: trigger → transitioning (both run in parallel) → WAL-only. If anything goes wrong, it falls back to triggers.

The approach is grounded in the DBSP differential dataflow framework (Budiu et al., 2022). Delta queries are derived automatically from your SQL's operator tree: joins produce the classic bilinear expansion, aggregates maintain auxiliary counters, and linear operators like filters just pass deltas through unchanged.

**Broad SQL support:** JOINs (inner, left, right, full outer), GROUP BY with all common aggregates, DISTINCT, UNION/INTERSECT/EXCEPT, subqueries, EXISTS/IN, CTEs (including WITH RECURSIVE), window functions, LATERAL joins, and most common expressions.

Other things we care about: crash-safe advisory locking, built-in monitoring views, NOTIFY-based alerting, and a dbt integration.

Written in Rust using pgrx. Targets PostgreSQL 18. Apache 2.0 licensed. Early stage — 700+ unit tests and 290+ e2e tests, but not yet production-hardened.

GitHub: https://github.com/grove/pg-stream
