# pg_trickle Roadmap

> **Audience:** Product managers, stakeholders, and technically curious readers
> who want to understand what each release delivers and why it matters —
> without needing to read Rust code or SQL specifications.

## Versions

### Foundation (v0.1.x)

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v0.1.0](roadmap/v0.1.0.md) | The complete foundation — differential engine, CDC, scheduling, monitoring | ✅ Released | Very Large | [Full details](roadmap/v0.1.x.md-full.md) |
| [v0.1.1](roadmap/v0.1.1.md) | Change capture correctness fixes (WAL decoder, UPDATE handling) | ✅ Released | Patch | [Full details](roadmap/v0.1.x.md-full.md) |
| [v0.1.2](roadmap/v0.1.2.md) | DDL tracking improvements and PgBouncer compatibility | ✅ Released | Patch | [Full details](roadmap/v0.1.x.md-full.md) |
| [v0.1.3](roadmap/v0.1.3.md) | SQL coverage completion, WAL hardening, TPC-H 22/22 | ✅ Released | Patch | [Full details](roadmap/v0.1.x.md-full.md) |

### Early Feature Development (v0.2.x – v0.5.x)

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v0.2.0](roadmap/v0.2.0.md) | Top-N views, IMMEDIATE refresh mode, diamond dependency safety | ✅ Released | Medium | [Full details](roadmap/v0.2.0.md-full.md) |
| [v0.2.1](roadmap/v0.2.1.md) | Upgrade infrastructure and documentation expansion | ✅ Released | Small | [Full details](roadmap/v0.2.1.md-full.md) |
| [v0.2.2](roadmap/v0.2.2.md) | Paginated top-N, AUTO mode default, ALTER QUERY | ✅ Released | Medium | [Full details](roadmap/v0.2.2.md-full.md) |
| [v0.2.3](roadmap/v0.2.3.md) | Non-determinism detection and operational polish | ✅ Released | Small | [Full details](roadmap/v0.2.3.md-full.md) |
| [v0.3.0](roadmap/v0.3.0.md) | Correctness for HAVING, FULL OUTER JOIN, and correlated subqueries | ✅ Released | Medium | [Full details](roadmap/v0.3.0.md-full.md) |
| [v0.4.0](roadmap/v0.4.0.md) | Parallel refresh, statement-level CDC triggers, cross-source consistency | ✅ Released | Medium | [Full details](roadmap/v0.4.0.md-full.md) |
| [v0.5.0](roadmap/v0.5.0.md) | Row-level security, ETL bootstrap gating, API polish | ✅ Released | Medium | [Full details](roadmap/v0.5.0.md-full.md) |

### Scalability and Robustness (v0.6.x – v0.9.x)

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v0.6.0](roadmap/v0.6.0.md) | Partitioned source tables, idempotent DDL, circular dependency foundation | ✅ Released | Medium | [Full details](roadmap/v0.6.0.md-full.md) |
| [v0.7.0](roadmap/v0.7.0.md) | Circular DAG execution, watermarks, Prometheus/Grafana observability | ✅ Released | Large | [Full details](roadmap/v0.7.0.md-full.md) |
| [v0.8.0](roadmap/v0.8.0.md) | pg_dump backup support and multiset invariant testing | ✅ Released | Small | [Full details](roadmap/v0.8.0.md-full.md) |
| [v0.9.0](roadmap/v0.9.0.md) | Algebraic aggregate maintenance — AVG, STDDEV, COUNT(DISTINCT) | ✅ Released | Medium | [Full details](roadmap/v0.9.0.md-full.md) |

### Production Readiness (v0.10.x – v0.14.x)

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v0.10.0](roadmap/v0.10.0.md) | DVM hardening, PgBouncer compatibility, "No Surprises" UX | ✅ Released | Medium | [Full details](roadmap/v0.10.0.md-full.md) |
| [v0.11.0](roadmap/v0.11.0.md) | Partitioned stream tables, event-driven scheduler (34× latency), circuit breaker | ✅ Released | Large | [Full details](roadmap/v0.11.0.md-full.md) |
| [v0.12.0](roadmap/v0.12.0.md) | Three-table join fix (EC-01), developer tools, SQLancer fuzzing | ✅ Released | Medium | [Full details](roadmap/v0.12.0.md-full.md) |
| [v0.13.0](roadmap/v0.13.0.md) | Columnar change tracking, shared buffers, TPC-H 22/22 DIFFERENTIAL | ✅ Released | Large | [Full details](roadmap/v0.13.0.md-full.md) |
| [v0.14.0](roadmap/v0.14.0.md) | Tiered scheduling, UNLOGGED buffers, TUI dashboard | ✅ Released | Medium | [Full details](roadmap/v0.14.0.md-full.md) |

### Performance and Integration (v0.15.x – v0.19.x)

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v0.15.0](roadmap/v0.15.0.md) | Nexmark benchmark, bulk create API, watermark hold-back, dbt Hub | ✅ Released | Medium | [Full details](roadmap/v0.15.0.md-full.md) |
| [v0.16.0](roadmap/v0.16.0.md) | Append-only fast path, algebraic aggregates, auto-indexing, benchmark CI | ✅ Released | Medium | [Full details](roadmap/v0.16.0.md-full.md) |
| [v0.17.0](roadmap/v0.17.0.md) | Cost-based refresh strategy, incremental DAG rebuild, pg_ivm migration guide | ✅ Released | Large | [Full details](roadmap/v0.17.0.md-full.md) |
| [v0.18.0](roadmap/v0.18.0.md) | Z-set delta engine, consistency enforcement, safety hardening | ✅ Released | Large | [Full details](roadmap/v0.18.0.md-full.md) |
| [v0.19.0](roadmap/v0.19.0.md) | Security hardening, packaging (PGXN, Docker Hub, apt/rpm) | ✅ Released | Medium | [Full details](roadmap/v0.19.0.md-full.md) |

### Self-Monitoring and Deep Correctness (v0.20.x – v0.27.x)

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v0.20.0](roadmap/v0.20.0.md) | pg_trickle monitors itself using its own stream tables | ✅ Released | Large | [Full details](roadmap/v0.20.0.md-full.md) |
| [v0.21.0](roadmap/v0.21.0.md) | Correctness hardening, zero-crash guarantee, shadow/canary mode | ✅ Released | Large | [Full details](roadmap/v0.21.0.md-full.md) |
| [v0.22.0](roadmap/v0.22.0.md) | Downstream CDC publication, parallel refresh pool, SLA tier auto-assignment | ✅ Released | Large | [Full details](roadmap/v0.22.0.md-full.md) |
| [v0.23.0](roadmap/v0.23.0.md) | TPC-H DVM scaling performance — all 22 queries at O(Δ) | ✅ Released | Large | [Full details](roadmap/v0.23.0.md-full.md) |
| [v0.24.0](roadmap/v0.24.0.md) | Join correctness complete fix, two-phase frontier, TOAST-aware CDC | ✅ Released | Large | [Full details](roadmap/v0.24.0.md-full.md) |
| [v0.25.0](roadmap/v0.25.0.md) | Thousands of stream tables, pooler cold-start fix, predictive model | ✅ Released | Large | [Full details](roadmap/v0.25.0.md-full.md) |
| [v0.26.0](roadmap/v0.26.0.md) | Concurrency testing, fuzz targets, refresh engine modularisation | ✅ Released | Large | [Full details](roadmap/v0.26.0.md-full.md) |
| [v0.27.0](roadmap/v0.27.0.md) | Snapshot/PITR, schedule recommendations, cluster observability | Planned | Medium | [Full details](roadmap/v0.27.0.md-full.md) |

### Toward Stable (v0.28.x – v1.0)

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v0.28.0](roadmap/v0.28.0.md) | Reliable event messaging built into PostgreSQL | ✅ Released | Large | [Full details](roadmap/v0.28.0.md-full.md) |
| [v0.29.0](roadmap/v0.29.0.md) | Off-the-shelf connector to Kafka, NATS, SQS, and more | ✅ Released | Large | [Full details](roadmap/v0.29.0.md-full.md) |
| [v0.30.0](roadmap/v0.30.0.md) | Quality gate before 1.0 — correctness, stability, and docs | Planned | Medium | [Full details](roadmap/v0.30.0.md-full.md) |
| [v0.31.0](roadmap/v0.31.0.md) | Smarter scheduling and faster hot paths | Planned | Medium | [Full details](roadmap/v0.31.0.md-full.md) |
| [v0.34.0](roadmap/v0.34.0.md) | Citus: stable object naming and per-source frontier foundation | Planned | Medium | [Full details](roadmap/v0.34.0.md-full.md) |
| [v0.32.0](roadmap/v0.32.0.md) | Live push notifications and safe live schema changes | Planned | Medium | [Full details](roadmap/v0.32.0.md-full.md) |
| [v0.33.0](roadmap/v0.33.0.md) | Time-travel queries and analytic storage | Planned | Medium | [Full details](roadmap/v0.33.0.md-full.md) |
| [v0.35.0](roadmap/v0.35.0.md) | Citus: world-class distributed source CDC and stream table support | Planned | Large | [Full details](roadmap/v0.35.0.md-full.md) |

### Beyond v1.0

| Version | Theme | Status | Scope | Full details |
|---------|-------|--------|------- |---------- |
| [v1.0.0](roadmap/v1.0.0.md-full.md) | Stable release — PostgreSQL 19, package registries, zero breaking changes | Planned | Large | [Full details](roadmap/v1.0.0.md-full.md) |
| [v1.1.0](roadmap/v1.1.0.md-full.md) | PostgreSQL 17 support | Planned | Medium | [Full details](roadmap/v1.1.0.md-full.md) |
| [v1.2.0](roadmap/v1.2.0.md-full.md) | PGlite proof of concept | Planned | Medium | [Full details](roadmap/v1.2.0.md-full.md) |
| [v1.3.0](roadmap/v1.3.0.md-full.md) | Core extraction (`pg_trickle_core`) | Planned | Large | [Full details](roadmap/v1.3.0.md-full.md) |
| [v1.4.0](roadmap/v1.4.0.md-full.md) | PGlite WASM extension | Planned | Medium | [Full details](roadmap/v1.4.0.md-full.md) |
| [v1.5.0](roadmap/v1.5.0.md-full.md) | PGlite reactive integration | Planned | Medium | [Full details](roadmap/v1.5.0.md-full.md) |
| [v1.6.0](roadmap/v1.6.0.md-full.md) | TUI self-monitoring integration | Planned | Medium | [Full details](roadmap/v1.6.0.md-full.md) |

## How these versions fit together

```
v0.1.0   ─── Foundation: differential engine, CDC, scheduling, 1300+ tests
    │
v0.2–0.5 ─── TopK, IMMEDIATE mode, RLS, partitioned sources, parallel refresh
    │
v0.6–0.9 ─── Circular DAGs, watermarks, Prometheus, algebraic aggregates
    │
v0.10–14 ─── PgBouncer compat, 34× latency, partitioned outputs, tiered scheduling
    │
v0.15–19 ─── Nexmark, append-only fast path, cost model, security, packaging
    │
v0.20–23 ─── Self-monitoring, zero-crash guarantee, downstream CDC, TPC-H at scale
    │
v0.24–27 ─── Join correctness complete, thousands of STs, snapshot/PITR
    │
v0.28–29 ─── Reliable event messaging (outbox + inbox) + relay CLI
    │
v0.30    ─── Quality gate: correctness, stability, docs (required for 1.0)
    │
v0.31    ─── Scheduler intelligence and performance hot paths
    │
v0.34    ─── Citus: stable naming foundation (additive, safe for all users)
    │
v0.32–33 ─── Push notifications, zero-downtime schema changes, time-travel
    │
v0.35    ─── Citus: distributed CDC and stream table support
    │
v1.0.0   ─── Stable release, PostgreSQL 19, package registries
```

v0.1.0 through v0.27.0 build the complete core engine and harden it for
production use. v0.28.0 and v0.29.0 deliver the event-driven integration
story. v0.30.0 is a mandatory correctness and polish gate before 1.0.
v0.31.0 sharpens scheduler intelligence before new features are added.
v0.34.0 is scheduled early — before v0.32.0 and v0.33.0 — so that every
subscription object and temporal history table is created with the new
stable naming scheme from day one, reducing migration scope later.
v0.32.0 and v0.33.0 each add a distinct new capability (push notifications
and time-travel/columnar storage) while the core IVM engine remains stable.
v0.35.0 builds on v0.34.0's foundations to unlock distributed-source CDC,
distributed ST placement, and the full Citus test suite.
