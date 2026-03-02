# pg_trickle — Project Roadmap

> **Last updated:** 2026-03-02
> **Current version:** 0.1.3

For a concise description of what pg_trickle is and why it exists, read
[ESSENCE.md](ESSENCE.md) — it explains the core problem (full `REFRESH
MATERIALIZED VIEW` recomputation), how the differential dataflow approach
solves it, the hybrid trigger→WAL CDC architecture, and the broad SQL
coverage, all in plain language.

---

## Overview

pg_trickle is a PostgreSQL 18 extension that implements streaming tables with
incremental view maintenance (IVM) via differential dataflow. All 13 design
phases are complete. This roadmap tracks the path from pre-release to 1.0
and beyond.

```
 We are here
     │
     ▼
 ┌─────────┐   ┌─────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐
 │  0.1.0  │──▶│  0.2.0  │──▶│  0.3.0   │──▶│  0.4.0   │──▶│  1.0.0   │──▶│  1.x+    │
 │ Pre-    │   │ Correct-│   │ Prod-    │   │ Observ-  │   │ Stable   │   │ Scale &  │
 │ release │   │ ness    │   │ ready    │   │ ability  │   │ Release  │   │ Ecosystem│
 └─────────┘   └─────────┘   └──────────┘   └──────────┘   └──────────┘   └──────────┘
```

---

## v0.1.0 — Released (2026-02-26)

**Status: Released — all 13 design phases implemented.**

Core engine, DVM with 21 OpTree operators, trigger-based CDC, DAG-aware
scheduling, monitoring, dbt macro package, and 1,300+ tests.

See [CHANGELOG.md](CHANGELOG.md) for the full feature list.

### Late additions (pre-March 1st)

Low-risk, high-value items pulled forward from v0.2.0:

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| F4 | WAL decoder: pgoutput message parsing edge cases | 2–3h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G3.3 |
| F7 | Document JOIN key column change limitations | 1–2h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G4.2 |
| F11 | Keyless table duplicate-rows: document known behavior | 1h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G7.1 |
| F14 | CUBE explosion guard (reject oversized CUBE grouping sets) | 1h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G5.2 |

---

## v0.2.0 — Correctness & Stability

**Goal:** Close all critical and high-priority gaps to reach a provably
correct baseline. No new features — only fixes, verification, and test
coverage.

> **Status: Substantially complete.** 50/51 SQL_GAPS_7 items were completed
> in v0.1.3. The only remaining item is F40 (extension upgrade migration
> scripts), deferred to PLAN_DB_SCHEMA_STABILITY.md.

### Tier 0 — Critical (must-fix) — ✅ COMPLETE

| Item | Description | Status |
|------|-------------|--------|
| F1 | Remove `delete_insert` merge strategy | ✅ Done in v0.1.3 |
| F2 | WAL decoder: keyless-table pk_hash computation | ✅ Done in v0.1.3 |
| F3 | WAL decoder: old_* column population for UPDATEs | ✅ Done in v0.1.3 |
| F5 | JOIN key column change detection in delta SQL | ✅ Done in v0.1.0 |
| F6 | ALTER TYPE / ALTER POLICY DDL tracking | ✅ Done in v0.1.3 |

### Tier 1 — Verification — ✅ COMPLETE

| Item | Description | Status |
|------|-------------|--------|
| F8–F10, F12 | Window partition key E2E, recursive CTE monotonicity audit, ALTER DOMAIN tracking, PgBouncer compatibility docs | ✅ Done in v0.1.3 |

### Tier 2 — Robustness — ✅ COMPLETE

| Item | Description | Status |
|------|-------------|--------|
| F13, F15–F16 | LIMIT-in-subquery warning, RANGE_AGG recognition, read replica detection | ✅ Done in v0.1.3 |

### Tier 3 — Test coverage — ✅ COMPLETE

| Item | Description | Status |
|------|-------------|--------|
| F17–F26 | 62 E2E tests across 10 test files | ✅ Done in v0.1.3 |

**TPC-H-derived coverage baseline** — The 22-query correctness test suite
now achieves **22/22 queries create and pass deterministic correctness checks**
across multiple mutation cycles (`just test-tpch`, local-only, SF=0.01).
See [plans/testing/PLAN_TEST_SUITE_TPC_H.md](plans/testing/PLAN_TEST_SUITE_TPC_H.md).

> *Queries are derived from the TPC-H Benchmark specification; results are not
> comparable to published TPC results. TPC Benchmark™ is a trademark of TPC.*

### Tier 4 — Operational Hardening — 13/14 COMPLETE

| Item | Description | Status |
|------|-------------|--------|
| F27–F39 | Adaptive threshold, SPI retry, delta metrics, WAL hardening, etc. | ✅ Done in v0.1.3 |
| F40 | Extension upgrade migration scripts | ⬜ Deferred to PLAN_DB_SCHEMA_STABILITY.md |

### Tier 5 — Nice-to-Have — ✅ COMPLETE

| Item | Description | Status |
|------|-------------|--------|
| F41–F51 | Wide table hash, memory docs, monitoring, keyless E2E, etc. | ✅ Done in v0.1.3 |

**v0.2.0 total: ~30 hours actual** (originally estimated 66–92h)

**Exit criteria:**
- [x] Zero P0 gaps
- [x] All P1 gaps resolved or documented as known limitations
- [x] E2E test count ≥ 400 with 0 pre-existing failures (currently 460)
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
| P2 | DAG level extraction (`topological_levels()`) | 2–4h | [REPORT_PARALLELIZATION.md §B](plans/performance/REPORT_PARALLELIZATION.md) |
| P3 | Dynamic background worker dispatch per level | 12–16h | [REPORT_PARALLELIZATION.md §A+B](plans/performance/REPORT_PARALLELIZATION.md) |

### Operational

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| O1 | Extension upgrade migrations (`ALTER EXTENSION UPDATE`) | 4–6h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.2 · [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) |
| O2 | Prepared statement cleanup on cache invalidation | 3–4h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.3 |
| O3 | ~~Adaptive fallback threshold exposure via monitoring~~ | ✅ Done in v0.1.3 (F27) | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.4 |
| O4 | ~~SPI SQLSTATE error classification for retry~~ | ✅ Done in v0.1.3 (F29) | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.6 |
| O5 | Slot lag alerting thresholds (configurable) | 2–3h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G10 |

### WAL CDC Hardening

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| W1 | WAL decoder fixes (F2–F3 completed in v0.1.3) | ✅ Done | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W2 | WAL mode E2E test suite (parallel to trigger suite) | 8–12h | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W3 | WAL→trigger automatic fallback hardening | 4–6h | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W4 | Promote `pg_trickle.cdc_mode = 'auto'` to recommended | Documentation | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |

> **v0.3.0 total: ~40–58 hours** (excluding v0.2.0 prerequisites)

**Exit criteria:**
- [ ] `max_concurrent_refreshes` drives real parallel refresh
- [ ] WAL CDC mode passes full E2E suite
- [ ] Extension upgrade path tested (`0.1.0 → 0.3.0`)
- [ ] Zero P0/P1 gaps remaining

---

## v0.4.0 — Observability & Integration

**Goal:** Prometheus/Grafana observability, dbt-pgtrickle formal release,
complete documentation review, and validated upgrade path. After this
milestone the product is externally visible and monitored.

### Observability

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| M1 | Prometheus exporter configuration guide | 4–6h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §1 |
| M2 | Grafana dashboard (refresh latency, staleness, CDC lag) | 4–6h | [PLAN_ECO_SYSTEM.md §1](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

### Integration & Release prep

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R5 | dbt-pgtrickle 0.1.0 formal release (PyPI) | 2–3h | [dbt-pgtrickle/](dbt-pgtrickle/) · [PLAN_DBT_MACRO.md](plans/dbt/PLAN_DBT_MACRO.md) |
| R6 | Complete documentation review & polish | 4–6h | [docs/](docs/) |
| O1 | Extension upgrade migrations (`ALTER EXTENSION UPDATE`) | 4–6h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.2 · [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) |

> **v0.4.0 total: ~18–27 hours**

**Exit criteria:**
- [ ] Grafana dashboard published
- [ ] dbt-pgtrickle 0.1.0 on PyPI
- [ ] `ALTER EXTENSION pg_trickle UPDATE` tested (`0.3.0 → 0.4.0`)
- [ ] All public documentation current and reviewed

---

## v1.0.0 — Stable Release

**Goal:** First officially supported release. Semantic versioning locks in.
API, catalog schema, and GUC names are considered stable. Focus is
distribution — getting pg_trickle onto package registries.

### Release engineering

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R1 | Semantic versioning policy + compatibility guarantees | 2–3h | [PLAN_VERSIONING.md](plans/infra/PLAN_VERSIONING.md) |
| R2 | PGXN / apt / rpm packaging | 8–12h | [PLAN_PACKAGING.md](plans/infra/PLAN_PACKAGING.md) |
| R3 | ~~Docker Hub official image~~ → CNPG extension image | ✅ Done | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |
| R4 | CNPG operator hardening (K8s 1.33+ native ImageVolume) | 4–6h | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |

> **v1.0.0 total: ~18–27 hours**

**Exit criteria:**
- [ ] Published on PGXN and Docker Hub
- [x] CNPG extension image published to GHCR (`pg_trickle-ext`)
- [x] CNPG cluster-example.yaml validated (Image Volume approach)
- [ ] Upgrade path from v0.4.0 tested
- [ ] Semantic versioning policy in effect

---

## Post-1.0 — Scale & Ecosystem

These are not gated on 1.0 but represent the longer-term horizon.

### Ecosystem expansion

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| E1 | dbt full adapter (`dbt-pgtrickle` extending `dbt-postgres`) | 20–30h | [PLAN_DBT_ADAPTER.md](plans/dbt/PLAN_DBT_ADAPTER.md) |
| E2 | Airflow provider (`apache-airflow-providers-pgtrickle`) | 16–20h | [PLAN_ECO_SYSTEM.md §4](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| E3 | CLI tool (`pgtrickle`) for management outside SQL | 16–20h | [PLAN_ECO_SYSTEM.md §4](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| E4 | Flyway / Liquibase migration support | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| E5 | ORM integrations guide (SQLAlchemy, Django, etc.) | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

### Scale

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| S1 | External orchestrator sidecar for 100+ STs | 20–40h | [REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md) §D |
| S2 | Citus / distributed PostgreSQL compatibility | ~6 months | [plans/infra/CITUS.md](plans/infra/CITUS.md) |
| S3 | Multi-database support (beyond `postgres` DB) | TBD | [PLAN_MULTI_DATABASE.md](plans/infra/PLAN_MULTI_DATABASE.md) |

### Advanced SQL

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A1 | Circular dependency support (SCC fixpoint iteration) | ~40h | [CIRCULAR_REFERENCES.md](plans/sql/CIRCULAR_REFERENCES.md) |
| A2 | Streaming aggregation (sub-second latency path) | TBD | [PLAN_STREAMING_AGGREGATION.md](plans/sql/PLAN_STREAMING_AGGREGATION.md) |
| A3 | PostgreSQL 19 forward-compatibility | TBD | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) |

---

## Effort Summary

| Milestone | Effort estimate | Cumulative |
|-----------|-----------------|------------|
| v0.2.0 — Correctness | 66–92h | 66–92h |
| v0.3.0 — Production ready | 40–58h | 106–150h |
| v0.4.0 — Observability & Integration | 18–27h | 124–177h |
| v1.0.0 — Stable release | 18–27h | 142–204h |
| Post-1.0 (ecosystem) | 88–134h | 232–340h |
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
| [plans/infra/PLAN_VERSIONING.md](plans/infra/PLAN_VERSIONING.md) | Versioning & compatibility policy |
| [plans/infra/PLAN_PACKAGING.md](plans/infra/PLAN_PACKAGING.md) | PGXN / deb / rpm packaging |
| [plans/infra/PLAN_DOCKER_IMAGE.md](plans/infra/PLAN_DOCKER_IMAGE.md) | Official Docker image (superseded by CNPG extension image) |
| [plans/ecosystem/PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) | CNPG Image Volume extension image |
| [plans/infra/PLAN_MULTI_DATABASE.md](plans/infra/PLAN_MULTI_DATABASE.md) | Multi-database support |
| [plans/infra/PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) | PostgreSQL 19 forward-compatibility |
| [plans/sql/PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) | Extension upgrade migrations |
| [plans/sql/PLAN_STREAMING_AGGREGATION.md](plans/sql/PLAN_STREAMING_AGGREGATION.md) | Sub-second streaming aggregation |
| [plans/adrs/PLAN_ADRS.md](plans/adrs/PLAN_ADRS.md) | Architectural decisions |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture |
