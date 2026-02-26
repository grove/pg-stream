# pg_stream — Project Roadmap

> **Last updated:** 2026-02-26
> **Current version:** 0.1.0 (pre-release)

---

## Overview

pg_stream is a PostgreSQL 18 extension that implements streaming tables with
incremental view maintenance (IVM) via differential dataflow. All 13 design
phases are complete. This roadmap tracks the path from pre-release to 1.0
and beyond.

```
 We are here
     │
     ▼
 ┌─────────┐   ┌─────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐
 │  0.1.0  │──▶│  0.2.0  │──▶│  0.3.0   │──▶│  1.0.0   │──▶│  1.x+    │
 │ Pre-    │   │ Correct-│   │ Prod-    │   │ Stable   │   │ Scale &  │
 │ release │   │ ness    │   │ ready    │   │ Release  │   │ Ecosystem│
 └─────────┘   └─────────┘   └──────────┘   └──────────┘   └──────────┘
```

---

## v0.1.0 — Pre-release (current)

**Status: Complete — all 13 design phases implemented.**

Core engine, DVM with 21 OpTree operators, trigger-based CDC, DAG-aware
scheduling, monitoring, dbt macro package, and 1,300+ tests.

See [CHANGELOG.md](CHANGELOG.md) for the full feature list.

---

## v0.2.0 — Correctness & Stability

**Goal:** Close all critical and high-priority gaps to reach a provably
correct baseline. No new features — only fixes, verification, and test
coverage.

### Tier 0 — Critical (must-fix)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| F1 | DELETE+INSERT merge strategy double-evaluation guard | 3–4h | G1.1 (P0) |
| F2 | WAL decoder: keyless-table pk_hash computation | 4–6h | G3.1 (P1) |
| F3 | WAL decoder: old_* column population for UPDATEs | 4–6h | G3.2 (P1) |
| F4 | WAL decoder: pgoutput message parsing edge cases | 3–5h | G3.3 (P1) |
| F5 | JOIN key column change detection in delta SQL | 3–4h | G4.1 (P1) |
| F6 | ALTER TYPE / ALTER POLICY DDL tracking | 3–5h | G9.1 (P1) |
| F7 | Document JOIN key change limitations | 2–3h | G4.2 (P1) |

> **Subtotal: 22–33 hours**

### Tier 1 — Verification

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| F8–F12 | Window partition key E2E, recursive CTE monotonicity audit, PgBouncer compatibility docs, CDC edge cases | 17–24h | G5–G9 |

### Tier 2 — Robustness

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| F13–F16 | LIMIT-in-subquery warning, CUBE explosion guard, read replica detection, SPI error classification | 7–9h | G2, G6, G8 |

### Tier 3 — Test coverage

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| F17–F26 | 21 aggregate differential E2E, FULL JOIN E2E, INTERSECT/EXCEPT pairs, GUC variation tests, CI combined coverage | 29–38h | G7 |

> **v0.2.0 total: ~75–104 hours**

**Exit criteria:**
- [ ] Zero P0 gaps
- [ ] All P1 gaps resolved or documented as known limitations
- [ ] E2E test count ≥ 400 with 0 pre-existing failures
- [ ] Combined coverage ≥ 75%

---

## v0.3.0 — Production Readiness

**Goal:** Operational polish, parallel refresh, and production-grade
WAL-based CDC. The extension is suitable for production use after this
milestone.

### Performance & Parallelism

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| P1 | Verify PostgreSQL parallel query for delta SQL | 0h | [REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md) §E |
| P2 | DAG level extraction (`topological_levels()`) | 2–4h | §B |
| P3 | Dynamic background worker dispatch per level | 12–16h | §A+B |

### Operational

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| O1 | Extension upgrade migrations (`ALTER EXTENSION UPDATE`) | 4–6h | G8.2 |
| O2 | Prepared statement cleanup on cache invalidation | 3–4h | G8.3 |
| O3 | Adaptive fallback threshold exposure via monitoring | 2–3h | G8.4 |
| O4 | SPI SQLSTATE error classification for retry | 3–4h | G8.6 |
| O5 | Slot lag alerting thresholds (configurable) | 2–3h | G10 |

### WAL CDC Hardening

| Item | Description | Effort |
|------|-------------|--------|
| W1 | WAL decoder fixes (F2–F4 prerequisite from v0.2.0) | Done in v0.2.0 |
| W2 | WAL mode E2E test suite (parallel to trigger suite) | 8–12h |
| W3 | WAL→trigger automatic fallback hardening | 4–6h |
| W4 | Promote `pg_stream.cdc_mode = 'auto'` to recommended | Documentation |

> **v0.3.0 total: ~40–58 hours** (excluding v0.2.0 prerequisites)

**Exit criteria:**
- [ ] `max_concurrent_refreshes` drives real parallel refresh
- [ ] WAL CDC mode passes full E2E suite
- [ ] Extension upgrade path tested (`0.1.0 → 0.3.0`)
- [ ] Zero P0/P1 gaps remaining

---

## v1.0.0 — Stable Release

**Goal:** First officially supported release. Semantic versioning begins.
API and catalog schema are considered stable.

### Release engineering

| Item | Description | Effort |
|------|-------------|--------|
| R1 | Semantic versioning policy + compatibility guarantees | 2–3h |
| R2 | PGXN / apt / rpm packaging | 8–12h |
| R3 | Docker Hub official image (PostgreSQL 18 + pg_stream) | 4–6h |
| R4 | CNPG operator hardening | 4–6h |
| R5 | dbt-pgstream 0.1.0 formal release (PyPI) | 2–3h |
| R6 | Complete documentation review & polish | 4–6h |

### Observability

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| M1 | Prometheus exporter configuration guide | 4–6h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §1 |
| M2 | Grafana dashboard (refresh latency, staleness, CDC lag) | 4–6h | §1 |

> **v1.0.0 total: ~32–48 hours**

**Exit criteria:**
- [ ] Published on PGXN and Docker Hub
- [ ] dbt-pgstream 0.1.0 on PyPI
- [ ] Grafana dashboard available
- [ ] CNPG cluster-example.yaml validated
- [ ] Upgrade path from v0.3.0 tested
- [ ] All documentation current

---

## Post-1.0 — Scale & Ecosystem

These are not gated on 1.0 but represent the longer-term horizon.

### Ecosystem expansion

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| E1 | dbt full adapter (`dbt-pgstream` extending `dbt-postgres`) | 20–30h | [PLAN_DBT_ADAPTER.md](plans/dbt/PLAN_DBT_ADAPTER.md) |
| E2 | Airflow provider (`apache-airflow-providers-pgstream`) | 16–20h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §4 |
| E3 | CLI tool (`pgstream`) for management outside SQL | 16–20h | §4 |
| E4 | Flyway / Liquibase migration support | 8–12h | §5 |
| E5 | ORM integrations guide (SQLAlchemy, Django, etc.) | 8–12h | §5 |

### Scale

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| S1 | External orchestrator sidecar for 100+ STs | 20–40h | [REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md) §D |
| S2 | Citus / distributed PostgreSQL compatibility | ~6 months | [plans/infra/CITUS.md](plans/infra/CITUS.md) |
| S3 | Multi-database support (beyond `postgres` DB) | TBD | |

### Advanced SQL

| Item | Description | Effort |
|------|-------------|--------|
| A1 | Circular dependency support (SCC fixpoint iteration) | ~40h |
| A2 | Streaming aggregation (sub-second latency path) | TBD |
| A3 | PostgreSQL 19 forward-compatibility | TBD |

---

## Effort Summary

| Milestone | Effort estimate | Cumulative |
|-----------|-----------------|------------|
| v0.2.0 — Correctness | 75–104h | 75–104h |
| v0.3.0 — Production ready | 40–58h | 115–162h |
| v1.0.0 — Stable release | 32–48h | 147–210h |
| Post-1.0 (ecosystem) | 88–134h | 235–344h |
| Post-1.0 (scale) | 6+ months | — |

---

## References

| Document | Purpose |
|----------|---------|
| [CHANGELOG.md](CHANGELOG.md) | What's been built |
| [plans/PLAN.md](plans/PLAN.md) | Original 13-phase design plan |
| [plans/sql/SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) | 53 known gaps, prioritized |
| [plans/performance/REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md) | Parallelization options analysis |
| [plans/performance/STATUS_PERFORMANCE.md](plans/performance/STATUS_PERFORMANCE.md) | Benchmark results |
| [plans/ecosystem/PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) | Ecosystem project catalog |
| [plans/dbt/PLAN_DBT_ADAPTER.md](plans/dbt/PLAN_DBT_ADAPTER.md) | Full dbt adapter plan |
| [plans/infra/CITUS.md](plans/infra/CITUS.md) | Citus compatibility plan |
| [plans/adrs/PLAN_ADRS.md](plans/adrs/PLAN_ADRS.md) | Architectural decisions |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture |
incremental view maintenance (IVM) via differential dataflow. All 13 design
phases are complete. This roadmap tracks the path from pre-release to 1.0
and beyond.
