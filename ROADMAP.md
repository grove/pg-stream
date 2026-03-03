# pg_trickle — Project Roadmap

> **Last updated:** 2026-03-04
> **Current version:** 0.2.0

For a concise description of what pg_trickle is and why it exists, read
[ESSENCE.md](ESSENCE.md) — it explains the core problem (full `REFRESH
MATERIALIZED VIEW` recomputation), how the differential dataflow approach
solves it, the hybrid trigger→WAL CDC architecture, and the broad SQL
coverage, all in plain language.

---

## Overview

pg_trickle is a PostgreSQL 18 extension that implements streaming tables with
incremental view maintenance (IVM) via differential dataflow. All 13 design
phases are complete. This roadmap tracks the path from the v0.1.x series to
1.0 and beyond.

```
                                                    We are here
                                                         │
                                                         ▼
 ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐
 │ 0.1.x  │ │ 0.2.0  │ │ 0.3.0  │ │ 0.4.0  │ │ 0.5.0  │ │ 1.0.0  │ │ 1.x+   │
 │Released│─│Complete│─│Correct│─│Compat │─│Observ-│─│Stable │─│Scale &│
 │ ✅      │ │ ✅      │ │& Secur│ │& Scale│ │ability│ │Release│ │Ecosys.│
 └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘
```

---

## v0.1.x Series — Released

### v0.1.0 — Released (2026-02-26)

**Status: Released — all 13 design phases implemented.**

Core engine, DVM with 21 OpTree operators, trigger-based CDC, DAG-aware
scheduling, monitoring, dbt macro package, and 1,300+ tests.

Key additions over pre-release:
- WAL decoder pgoutput edge cases (F4)
- JOIN key column change limitation docs (F7)
- Keyless duplicate-row behavior documented (F11)
- CUBE explosion guard (F14)

### v0.1.1 — Released (2026-02-27)

Patch release: WAL decoder keyless pk_hash fix (F2), old_* column population
for UPDATEs (F3), and `delete_insert` merge strategy removal (F1).

### v0.1.2 — Released (2026-02-28)

Patch release: ALTER TYPE/POLICY DDL tracking (F6), window partition key E2E
tests (F8), PgBouncer compatibility docs (F12), read replica detection (F16),
SPI retry with SQLSTATE classification (F29), and 40+ additional E2E tests.

### v0.1.3 — Released (2026-03-01)

Patch release: Completed 50/51 SQL_GAPS_7 items across all tiers. Highlights:
- Adaptive fallback threshold (F27), delta change metrics (F30)
- WAL decoder hardening: replay deduplication, slot lag alerting (F31–F38)
- TPC-H 22-query correctness baseline (22/22 pass, SF=0.01)
- 460 E2E tests (≥ 400 exit criterion met)
- CNPG extension image published to GHCR

See [CHANGELOG.md](CHANGELOG.md) for the full feature list.

---

## v0.2.0 — TopK, Diamond Consistency & Transactional IVM

**Status: Released (2026-03-04).**

The 51-item SQL_GAPS_7 correctness plan was completed in v0.1.x. v0.2.0 delivers
three major feature additions. The one remaining SQL_GAPS_7 item (F40 — extension
upgrade migration scripts) is deferred to v0.3.0 as O1.4

<details>
<summary>Completed items (click to expand)</summary>

| Tier | Items | Status |
|------|-------|--------|
| 0 — Critical | F1–F3, F5–F6 | ✅ Done in v0.1.1–v0.1.3 |
| 1 — Verification | F8–F10, F12 | ✅ Done in v0.1.2–v0.1.3 |
| 2 — Robustness | F13, F15–F16 | ✅ Done in v0.1.2–v0.1.3 |
| 3 — Test coverage | F17–F26 (62 E2E tests) | ✅ Done in v0.1.2–v0.1.3 |
| 4 — Operational hardening | F27–F39 | ✅ Done in v0.1.3 |
| 4 — Upgrade migrations | F40 | ⬜ Deferred → v0.3.0 O1 |
| 5 — Nice-to-have | F41–F51 | ✅ Done in v0.1.3 |

**TPC-H baseline:** 22/22 queries pass deterministic correctness checks across
multiple mutation cycles (`just test-tpch`, SF=0.01).

> *Queries are derived from the TPC-H Benchmark specification; results are not
> comparable to published TPC results. TPC Benchmark™ is a trademark of TPC.*

</details>

### ORDER BY / LIMIT / OFFSET — TopK Support ✅

`ORDER BY ... LIMIT N` defining queries are accepted and refreshed correctly.
All 9 plan items (TK1–TK9) implemented, including 5 TPC-H queries with ORDER BY
restored (Q2, Q3, Q10, Q18, Q21).

| Item | Description | Status |
|------|-------------|--------|
| TK1 | E2E tests for `FETCH FIRST` / `FETCH NEXT` rejection | ✅ Done |
| TK2 | OFFSET without ORDER BY warning in subqueries | ✅ Done |
| TK3 | `detect_topk_pattern()` + `TopKInfo` struct in `parser.rs` | ✅ Done |
| TK4 | Catalog columns: `pgt_topk_limit`, `pgt_topk_order_by` | ✅ Done |
| TK5 | TopK-aware refresh path (scoped recomputation via MERGE) | ✅ Done |
| TK6 | DVM pipeline bypass for TopK tables in `api.rs` | ✅ Done |
| TK7 | E2E + unit tests (`e2e_topk_tests.rs`, 18 tests) | ✅ Done |
| TK8 | Documentation (SQL Reference, FAQ, CHANGELOG) | ✅ Done |
| TK9 | TPC-H: restored ORDER BY + LIMIT in Q2, Q3, Q10, Q18, Q21 | ✅ Done |

See [PLAN_ORDER_BY_LIMIT_OFFSET.md](plans/sql/PLAN_ORDER_BY_LIMIT_OFFSET.md).

### Diamond Dependency Consistency ✅

Atomic refresh groups eliminate the inconsistency window in diamond DAGs
(A→B→D, A→C→D). All 8 plan items (D1–D8) implemented.

| Item | Description | Status |
|------|-------------|--------|
| D1 | Data structures (`Diamond`, `ConsistencyGroup`) in `dag.rs` | ✅ Done |
| D2 | Diamond detection algorithm in `dag.rs` | ✅ Done |
| D3 | Consistency group computation in `dag.rs` | ✅ Done |
| D4 | Catalog columns + GUCs (`diamond_consistency`, `diamond_schedule_policy`) | ✅ Done |
| D5 | Scheduler wiring with SAVEPOINT loop | ✅ Done |
| D6 | Monitoring function `pgtrickle.diamond_groups()` | ✅ Done |
| D7 | E2E test suite (`tests/e2e_diamond_tests.rs`) | ✅ Done |
| D8 | Documentation (`SQL_REFERENCE.md`, `CONFIGURATION.md`, `ARCHITECTURE.md`) | ✅ Done |

See [PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](plans/sql/PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md).

### Transactional IVM — IMMEDIATE Mode ✅

New `IMMEDIATE` refresh mode that updates stream tables **within the same
transaction** as base table DML, using statement-level AFTER triggers with
transition tables. Phase 1 (core engine) and Phase 3 (extended SQL support)
are complete. Phase 2 (pg_ivm compatibility layer) is postponed. Phase 4
(performance optimizations) has partial completion (delta SQL template caching).

| Item | Description | Status |
|------|-------------|--------|
| TI1 | `RefreshMode::Immediate` enum, catalog CHECK, API validation | ✅ Done |
| TI2 | Statement-level IVM trigger functions with transition tables | ✅ Done |
| TI3 | `DeltaSource::TransitionTable` — Scan operator dual-path | ✅ Done |
| TI4 | Delta application (DELETE + INSERT ON CONFLICT) | ✅ Done |
| TI5 | Advisory lock-based concurrency (`IvmLockMode`) | ✅ Done |
| TI6 | TRUNCATE handling (full refresh of stream table) | ✅ Done |
| TI7 | `alter_stream_table` mode switching (DIFFERENTIAL↔IMMEDIATE, FULL↔IMMEDIATE) | ✅ Done |
| TI8 | Query restriction validation (`validate_immediate_mode_support`) | ✅ Done |
| TI9 | Delta SQL template caching (thread-local `IVM_DELTA_CACHE`) | ✅ Done |
| TI10 | Window functions, LATERAL, scalar subqueries in IMMEDIATE mode | ✅ Done |
| TI11 | Cascading IMMEDIATE stream tables (ST_A → ST_B) | ✅ Done |
| TI12 | 29 E2E tests + 8 unit tests | ✅ Done |
| TI13 | Documentation (SQL Reference, Architecture, FAQ, CHANGELOG) | ✅ Done |

> Remaining performance optimizations (ENR-based transition table access,
> aggregate fast-path, C-level trigger functions, prepared statement reuse)
> are tracked under post-1.0 A2.

See [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md).

**Exit criteria:**
- [x] `ORDER BY ... LIMIT N` (TopK) defining queries accepted and refreshed correctly
- [x] TPC-H queries Q2, Q3, Q10, Q18, Q21 pass with original LIMIT restored
- [x] Diamond dependency consistency (D1–D8) implemented and E2E-tested
- [x] IMMEDIATE refresh mode: INSERT/UPDATE/DELETE on base table updates stream table within the same transaction
- [x] Window functions, LATERAL, scalar subqueries work in IMMEDIATE mode
- [x] Cascading IMMEDIATE stream tables (ST_A → ST_B) propagate correctly
- [x] Concurrent transaction tests pass

---

## v0.3.0 — Correctness, Security & Operations

**Goal:** Fix correctness gaps, harden security (RLS), validate partitioned
sources, improve CDC reliability, and polish operational tooling. The extension
is safe for pre-production use after this milestone.

### Non-Deterministic Function Handling

Volatile functions (`random()`, `gen_random_uuid()`, `clock_timestamp()`) break
delta computation in DIFFERENTIAL mode — values change on each evaluation,
causing phantom changes and corrupted row identity hashes. This is a silent
correctness gap.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ND1 | Volatility lookup via `pg_proc.provolatile` + recursive `Expr` scanner | 1–2h | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §Part 1 |
| ND2 | OpTree volatility walker + enforcement policy (reject volatile in DIFFERENTIAL, warn for stable) | 1h | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §Part 2 |
| ND3 | E2E tests (volatile rejected, stable warned, immutable allowed, nested volatile in WHERE) | 1–2h | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §E2E Tests |
| ND4 | Documentation (`SQL_REFERENCE.md`, `DVM_OPERATORS.md`) | 0.5h | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §Files |

> **Non-determinism subtotal: ~4–6 hours**

### Row-Level Security (RLS) Support

Stream tables materialize the full result set (like `MATERIALIZED VIEW`). RLS
is applied on the stream table itself for read-side filtering. Phase 1
hardens the security context; Phase 2 adds a tutorial; Phase 3 completes DDL
tracking. Phase 4 (per-role `security_invoker`) is deferred to post-1.0.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R1 | Document RLS semantics in SQL_REFERENCE.md and FAQ.md | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 |
| R2 | Disable RLS on change buffer tables (`ALTER TABLE ... DISABLE ROW LEVEL SECURITY`) | 30min | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R2 |
| R3 | Force superuser context for manual `refresh_stream_table()` (prevent "who refreshed it?" hazard) | 2h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R3 |
| R4 | Force SECURITY DEFINER on IVM trigger functions (IMMEDIATE mode delta queries must see all rows) | 2h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R4 |
| R5 | E2E test: RLS on source table does not affect stream table content | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R5 |
| R6 | Tutorial: RLS on stream tables (enable RLS, per-tenant policies, verify filtering) | 1.5h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.2 R6 |
| R7 | E2E test: RLS on stream table filters reads per role | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.2 R7 |
| R8 | E2E test: IMMEDIATE mode + RLS on stream table | 30min | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.2 R8 |
| R9 | Track ENABLE/DISABLE RLS DDL on source tables (AT_EnableRowSecurity et al.) in hooks.rs | 2h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.3 R9 |
| R10 | E2E test: ENABLE RLS on source table triggers reinit | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.3 R10 |

> **RLS subtotal: ~8–12 hours** (Phase 4 `security_invoker` deferred to post-1.0)

### Partitioning Support (Source Tables)

Partitioned source tables already work with trigger-based CDC (PG 13+ trigger
propagation), but there are validation gaps, missing tests, and an ATTACH
PARTITION detection hole. This section addresses the near-term items only;
partitioned storage tables are deferred to a future release.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PT1 | E2E tests for partitioned source tables (RANGE, basic CRUD, differential refresh) | 8–12h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §7 |
| PT2 | ATTACH PARTITION detection in DDL hook → force `needs_reinit` | 4–8h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §3.3 |
| PT3 | WAL publication: set `publish_via_partition_root = true` for partitioned sources | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §3.4 |
| PT4 | Foreign table source detection (`relkind = 'f'`) → restrict to FULL mode | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §6.3 |
| PT5 | Documentation: partitioned source table support & caveats | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §8 |

> **Partitioning subtotal: ~18–32 hours**

### Performance

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| P1 | Verify PostgreSQL parallel query for delta SQL | 0h | [REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md) §E |

### Operational

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| O1 | Extension upgrade migrations (`ALTER EXTENSION UPDATE`) | 4–6h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.2 · [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) |
| O2 | Prepared statement cleanup on cache invalidation | 3–4h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.3 |
| O3 | ~~Adaptive fallback threshold exposure via monitoring~~ | ✅ Done in v0.1.3 (F27) | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.4 |
| O4 | ~~SPI SQLSTATE error classification for retry~~ | ✅ Done in v0.1.3 (F29) | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G8.6 |
| O5 | Slot lag alerting thresholds (configurable) | 2–3h | [SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) G10 |
| O6 | Simplify `pg_trickle.user_triggers` GUC (remove redundant `on` value) | 1h | [PLAN_FEATURE_CLEANUP.md](plans/PLAN_FEATURE_CLEANUP.md) C5 |

### WAL CDC Hardening

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| W1 | WAL decoder fixes (F2–F3 completed in v0.1.3) | ✅ Done | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W2 | WAL mode E2E test suite (parallel to trigger suite) | 8–12h | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W3 | WAL→trigger automatic fallback hardening | 4–6h | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W4 | Promote `pg_trickle.cdc_mode = 'auto'` to recommended | Documentation | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |

> **v0.3.0 total: ~55–85 hours**

**Exit criteria:**
- [ ] Volatile functions rejected in DIFFERENTIAL mode; stable functions warned
- [ ] RLS semantics documented; change buffers RLS-hardened; IVM triggers SECURITY DEFINER
- [ ] RLS on stream table E2E-tested (DIFFERENTIAL + IMMEDIATE)
- [ ] Partitioned source tables E2E-tested; ATTACH PARTITION detected
- [ ] WAL CDC mode passes full E2E suite
- [ ] Extension upgrade path tested (`0.1.x → 0.3.0`)
- [ ] Zero P0/P1 gaps remaining

---

## v0.4.0 — Backward Compatibility & Parallel Refresh

**Goal:** Widen the deployment target from PG 18-only to PG 16–18, and enable
parallel refresh across DAG levels. After this milestone the extension is
suitable for production use on mainstream PostgreSQL versions.

### PostgreSQL Backward Compatibility (PG 16–18)

pg_trickle currently targets PG 18 only. pgrx 0.17.0 supports PG 13–18 via
feature flags. Starting with PG 16–18 minimizes scope (only JSON_TABLE gating
needed) while widening the deployment target for the production-ready release.
PG 14–15 support can follow in a later release.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| BC1 | Cargo.toml feature flags (`pg16`, `pg17`, `pg18`) + `cfg_aliases` | 4–8h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 1 |
| BC2 | `#[cfg]` gate JSON_TABLE nodes in `parser.rs` (~250 lines, PG 17+) | 12–16h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 2 |
| BC3 | `pg_get_viewdef()` trailing-semicolon behavior verification | 2–4h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 3 |
| BC4 | CI matrix expansion (PG 16, 17, 18) + parameterized Dockerfiles | 12–16h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phases 4–5 |
| BC5 | WAL decoder validation against PG 16–17 `pgoutput` format | 8–12h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §6A |

> **Backward compatibility subtotal: ~38–56 hours**

### Parallel Refresh

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| P2 | DAG level extraction (`topological_levels()`) | 2–4h | [REPORT_PARALLELIZATION.md §B](plans/performance/REPORT_PARALLELIZATION.md) |
| P3 | Dynamic background worker dispatch per level | 12–16h | [REPORT_PARALLELIZATION.md §A+B](plans/performance/REPORT_PARALLELIZATION.md) |

> **v0.4.0 total: ~52–76 hours**

**Exit criteria:**
- [ ] PG 16 and PG 17 pass full E2E suite (trigger CDC mode)
- [ ] `max_concurrent_refreshes` drives real parallel refresh via DAG levels
- [ ] WAL decoder validated against PG 16–17 `pgoutput` format
- [ ] CI matrix covers PG 16, 17, 18

---

## v0.5.0 — Observability & Integration

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

> **v0.5.0 total: ~18–27 hours**

**Exit criteria:**
- [ ] Grafana dashboard published
- [ ] dbt-pgtrickle 0.1.0 on PyPI
- [ ] `ALTER EXTENSION pg_trickle UPDATE` tested (`0.4.0 → 0.5.0`)
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
- [ ] Upgrade path from v0.5.0 tested
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
| A2 | Transactional IVM Phase 4 remaining (ENR-based transition tables, aggregate fast-path, C-level triggers, prepared stmt reuse) | ~36–54h | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) |
| A3 | PostgreSQL 19 forward-compatibility | TBD | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) |
| A4 | PostgreSQL 14–15 backward compatibility | ~40h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) |
| A5 | Partitioned stream table storage (opt-in) | ~60–80h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §4 |

---

## Effort Summary

| Milestone | Effort estimate | Cumulative | Status |
|-----------|-----------------|------------|--------|
| v0.1.x — Core engine + correctness | ~30h actual | 30h | ✅ Released |
| v0.2.0 — TopK, Diamond & Transactional IVM | ✔️ Complete | 62–78h | ✅ Done |
| v0.3.0 — Correctness, Security & Operations | 55–85h | 117–163h | |
| v0.4.0 — Backward Compat & Parallel Refresh | 52–76h | 169–239h | |
| v0.5.0 — Observability & Integration | 18–27h | 187–266h | |
| v1.0.0 — Stable release | 18–27h | 205–293h | |
| Post-1.0 (ecosystem) | 88–134h | 293–427h | |
| Post-1.0 (scale) | 6+ months | — | |

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
| [plans/sql/PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) | Transactional IVM (immediate, same-transaction refresh) |
| [plans/sql/PLAN_ORDER_BY_LIMIT_OFFSET.md](plans/sql/PLAN_ORDER_BY_LIMIT_OFFSET.md) | ORDER BY / LIMIT / OFFSET gaps & TopK support |
| [plans/sql/PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) | Non-deterministic function handling |
| [plans/sql/PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) | Row-Level Security support plan (Phases 1–4) |
| [plans/infra/PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) | PostgreSQL partitioning & sharding compatibility |
| [plans/infra/PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) | Supporting older PostgreSQL versions (13–17) |
| [plans/sql/PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](plans/sql/PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md) | Diamond dependency consistency (multi-path refresh atomicity) |
| [plans/adrs/PLAN_ADRS.md](plans/adrs/PLAN_ADRS.md) | Architectural decisions |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System architecture |
